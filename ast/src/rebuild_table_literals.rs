use crate::{Assign, Block, Index, LValue, LocalRw, RValue, RcLocal, Statement, Table, Traverse};

/// Rebuild table literals that were lowered to `local t = {}` followed by
/// contiguous field assignments.
///
/// This pass deliberately only consumes assignments before the first non-field
/// statement, so the table has no opportunity to be read or aliased before the
/// folded writes.
pub fn rebuild_table_literals(block: &mut Block) -> bool {
    let usage = crate::inline_temps::collect_usage(block);
    let single_write = usage.iter().filter(|(_, usage)| usage.writes == 1)
        .map(|(local, _)| local.clone()).collect();
    let captured = usage.iter()
        .filter(|(_, usage)| usage.captured)
        .map(|(local, _)| local.clone())
        .collect::<rustc_hash::FxHashSet<_>>();
    rebuild_with_captured(block, &captured, &single_write)
}

pub(crate) fn rebuild_with_captured(
    block: &mut Block,
    captured: &rustc_hash::FxHashSet<RcLocal>,
    single_write: &rustc_hash::FxHashSet<RcLocal>,
) -> bool {
    let nested_changed = rebuild_nested_blocks(block, captured, single_write);
    let sunk_changed = sink_total_table_declarations(block, captured);
    let regions_changed = sink_private_constructor_regions(block, captured);
    let drained_changed = extract_drained_constructor_fields(block);
    let folded = rebuild_current_block(block, captured, single_write);
    nil_unread_placeholders(block, captured) | folded | sunk_changed | regions_changed | drained_changed | nested_changed
}

/// A template's loader value need not appear in source when every path fills
/// the slot before observation, rehash or GC can turn a nil-valued key dead.
fn nil_unread_placeholders(block: &mut Block, captured: &rustc_hash::FxHashSet<RcLocal>) -> bool {
    let mut changed = false;
    for index in 0..block.0.len() {
        let Some(object) = table_constructor_local(&block.0[index]) else { continue; };
        if captured.contains(&object) { continue; }
        let (prefix, rest) = block.0.split_at_mut(index + 1);
        let table = prefix[index].as_assign_mut().unwrap().right[0].as_table_mut().unwrap();
        // Keep a DUPTABLE on recompilation: NEWTABLE nil nodes can be reused
        // by the later constructor fields before the proved store is reached.
        if !matches!(table.2, crate::TableShape::Template { .. }) || table.0.len() > 32
            || !table.0.iter().all(|(key, _)| matches!(key,
                Some(RValue::Literal(crate::Literal::String(name))) if crate::formatter::Formatter::<String>::is_valid_name(name)))
            // DUPTABLE can preload literal values before earlier effectful
            // fields run. An earlier collection can kill this nil key, or a
            // different nil key whose later refill rehashes this slot.
            || !table.0.iter().all(|(_, value)| cannot_collect(value))
        { continue; }
        for field in 0..table.0.len() {
            if matches!(table.0[field].1, RValue::Literal(crate::Literal::Number(0.0)))
                && first_private_store(rest, &object, table.0[field].0.as_ref().unwrap(), table, &mut 256, 0) == Ok(true)
            {
                table.0[field].1 = crate::Literal::Nil.into();
                changed = true;
            }
        }
    }
    changed
}

fn cannot_collect(value: &RValue) -> bool {
    let summary = crate::effects::summarize(value, &|_| false);
    !summary.exhausted && summary.effects.is_total_pure()
        && !summary.effects.contains(crate::effects::Effects::ALLOCATION)
}

fn first_private_store(
    statements: &[Statement], object: &RcLocal, key: &RValue, table: &Table,
    budget: &mut usize, depth: usize,
) -> Result<bool, ()> {
    if depth > 16 { return Err(()); }
    for statement in statements {
        if *budget == 0 { return Err(()); }
        *budget -= 1;
        if let Statement::If(branch) = statement {
            if !cannot_collect(&branch.condition)
                || branch.condition.any_local_read(&mut |read| read == object) { return Err(()); }
            let yes = first_private_store(&branch.then_block.lock(), object, key, table, budget, depth + 1)?;
            let no = first_private_store(&branch.else_block.lock(), object, key, table, budget, depth + 1)?;
            if yes && no { return Ok(true); }
            continue;
        }
        if let Some((written_key, value)) = statement.as_assign()
            .and_then(|assign| field_assignment_parts(assign, object))
        {
            if !cannot_collect(value) || value.any_local_read(&mut |read| read == object) { return Err(()); }
            if crate::same_table_key(written_key, key) { return Ok(true); }
            // Only an existing template key can change without a possible rehash.
            if !table.0.iter().any(|(listed, _)| listed.as_ref().is_some_and(|listed| crate::same_table_key(listed, written_key))) {
                return Err(());
            }
        } else if !match statement {
            Statement::Assign(assign) => assign.left.iter().all(|value| matches!(value, LValue::Local(_)))
                && assign.right.iter().all(cannot_collect),
            Statement::Comment(_) | Statement::Empty(_) => true,
            _ => false,
        }
            || statement.any_local_read(&mut |read| read == object)
            || statement.any_local_write(&mut |written| written == object)
        {
            return Err(());
        }
    }
    Ok(false)
}

/// Merge repeated string or boolean keys when their layout permits it
/// ([`mergeable_key`]). A later entry for a key already listed takes the
/// first entry's place when that entry is a literal and nothing between them
/// could observe the move: a DUPTABLE
/// placeholder filled by a store whose key was still a temp when it was
/// folded (`{ Velocity = 0, SmoothTime = smoothTime, Velocity = velocity }`).
pub(crate) fn merge_repeated_keys(block: &mut Block) -> bool {
    let mut changed = false;
    for statement in &mut block.0 {
        for value in statement.rvalues_mut() {
            changed |= merge_repeated_keys_in_value(value);
        }
        changed |= match statement {
            Statement::If(branch) => {
                merge_repeated_keys(&mut branch.then_block.lock()) | merge_repeated_keys(&mut branch.else_block.lock())
            }
            Statement::While(node) => merge_repeated_keys(&mut node.block.lock()),
            Statement::Repeat(node) => merge_repeated_keys(&mut node.block.lock()),
            Statement::NumericFor(node) => merge_repeated_keys(&mut node.block.lock()),
            Statement::GenericFor(node) => merge_repeated_keys(&mut node.block.lock()),
            _ => false,
        };
    }
    changed
}

fn merge_repeated_keys_in_value(value: &mut RValue) -> bool {
    let mut changed = match value {
        RValue::Table(table) => merge_repeated_keys_in(table),
        RValue::Closure(closure) => return merge_repeated_keys(&mut closure.function.lock().body),
        _ => false,
    };
    for child in value.rvalues_mut() {
        changed |= merge_repeated_keys_in_value(child);
    }
    changed
}

/// Only string/boolean keys have no aliases among differently spelled literal
/// or implicit array keys. Numeric/vector keys can alias signed zeros or array
/// entries; nil/NaN keys can raise before the later value is evaluated. Keep
/// those entries in place unless a separate runtime-slot proof is available.
fn mergeable_key(key: &RValue) -> bool {
    matches!(key, RValue::Literal(crate::Literal::String(_) | crate::Literal::Boolean(_)))
}

fn merge_repeated_keys_in(table: &mut Table) -> bool {
    // Avoid building an index for the common tiny constructor.
    if table.0.len() < 16 {
        return merge_repeated_keys_scanning(table);
    }
    merge_repeated_keys_indexed(table, literal_fingerprint)
}

/// The index slot of a [`mergeable_key`] literal; a hit is confirmed against
/// the key itself.
fn literal_fingerprint(literal: &crate::Literal) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = rustc_hash::FxHasher::default();
    match literal {
        crate::Literal::String(value) => value.hash(&mut hash),
        crate::Literal::Boolean(value) => value.hash(&mut hash),
        other => unreachable!("only string and boolean keys merge, not {other:?}"),
    }
    hash.finish()
}

fn merge_repeated_keys_indexed(
    table: &mut Table,
    fingerprint: impl Fn(&crate::Literal) -> u64,
) -> bool {
    let mut first_by_hash: rustc_hash::FxHashMap<u64, Vec<usize>> = Default::default();
    let mut removed = vec![false; table.0.len()];
    let mut changed = false;
    // A surviving entry can only change from literal to nonliteral. Thus the
    // latest barrier of each kind never retreats, even when an earlier value
    // is replaced; a suffix/range scan is unnecessary.
    let mut last_nonliteral: Option<usize> = None;
    let mut last_nonread: Option<usize> = None;
    for later in 0..table.0.len() {
        let first = match &table.0[later].0 {
            Some(key @ RValue::Literal(literal)) if mergeable_key(key) => {
                let candidates = first_by_hash.entry(fingerprint(literal)).or_default();
                let first = candidates.iter().copied().find(|&first| {
                    #[cfg(test)]
                    tests::MERGE_KEY_COMPARISONS.with(|count| count.set(count.get() + 1));
                    table.0[first].0.as_ref() == Some(key)
                });
                if first.is_none() { candidates.push(later); }
                first
            }
            _ => None,
        };
        let reads_only = matches!(table.0[later].1, RValue::Local(_) | RValue::Literal(_));
        let barrier = if reads_only { last_nonread } else { last_nonliteral };
        let movable = first.is_some_and(|first| {
            matches!(table.0[first].1, RValue::Literal(_))
                && crate::table_replaces_entry(table, first, &table.0[later].1)
                && barrier.is_none_or(|barrier| barrier <= first)
        });
        let position = if let (Some(first), true) = (first, movable) {
            table.0[first].1 = std::mem::replace(&mut table.0[later].1, crate::Literal::Nil.into());
            removed[later] = true;
            changed = true;
            first
        } else {
            later
        };
        let (key, value) = &table.0[position];
        let total_key = key.as_ref().is_none_or(|key| {
            matches!(key, RValue::Literal(_)) && crate::is_total_table_key(key)
        });
        if !total_key || !matches!(value, RValue::Literal(_)) {
            last_nonliteral = Some(last_nonliteral.map_or(position, |old| old.max(position)));
        }
        if !total_key || !matches!(value, RValue::Literal(_) | RValue::Local(_)) {
            last_nonread = Some(last_nonread.map_or(position, |old| old.max(position)));
        }
    }
    if changed {
        let mut removed = removed.into_iter();
        table.0.retain(|_| !removed.next().expect("one marker per stable entry"));
    }
    changed
}

fn merge_repeated_keys_scanning(table: &mut Table) -> bool {
    let mut changed = false;
    let mut later = 1;
    while later < table.0.len() {
        let first = match &table.0[later].0 {
            Some(key @ RValue::Literal(_)) if mergeable_key(key) => table.0[..later].iter().position(|(existing, _)| existing.as_ref() == Some(key)),
            _ => None,
        };
        // Moving the value ahead crosses the entries between: literals, or
        // local reads when the value is one too (reads do not interfere).
        let moved_reads_only = matches!(table.0[later].1, RValue::Local(_) | RValue::Literal(_));
        let movable = first.is_some_and(|first| {
            matches!(table.0[first].1, RValue::Literal(_))
                && crate::table_replaces_entry(table, first, &table.0[later].1)
                && table.0[first + 1..later].iter().all(|(key, value)| {
                    key.as_ref().is_none_or(|key| matches!(key, RValue::Literal(_)) && crate::is_total_table_key(key))
                        && (matches!(value, RValue::Literal(_)) || (moved_reads_only && matches!(value, RValue::Local(_))))
                })
        });
        if let (Some(first), true) = (first, movable) {
            let (_, value) = table.0.remove(later);
            table.0[first].1 = value;
            changed = true;
            continue;
        }
        later += 1;
    }
    changed
}

fn rebuild_nested_blocks(block: &mut Block, captured: &rustc_hash::FxHashSet<RcLocal>, single_write: &rustc_hash::FxHashSet<RcLocal>) -> bool {
    let mut changed = false;
    for statement in &mut block.0 {
        changed |= rebuild_nested_in_statement(statement, captured, single_write);
    }
    changed
}

fn rebuild_nested_in_statement(
    statement: &mut Statement,
    captured: &rustc_hash::FxHashSet<RcLocal>,
    single_write: &rustc_hash::FxHashSet<RcLocal>,
) -> bool {
    let closures_changed = rebuild_closures_in_statement(statement, captured, single_write);
    let blocks_changed = match statement {
        Statement::If(r#if) => {
            rebuild_with_captured(&mut r#if.then_block.lock(), captured, single_write)
                | rebuild_with_captured(&mut r#if.else_block.lock(), captured, single_write)
        }
        Statement::While(r#while) => rebuild_with_captured(&mut r#while.block.lock(), captured, single_write),
        Statement::Repeat(repeat) => rebuild_with_captured(&mut repeat.block.lock(), captured, single_write),
        Statement::NumericFor(numeric_for) => {
            rebuild_with_captured(&mut numeric_for.block.lock(), captured, single_write)
        }
        Statement::GenericFor(generic_for) => {
            rebuild_with_captured(&mut generic_for.block.lock(), captured, single_write)
        }
        _ => false,
    };
    closures_changed | blocks_changed
}

fn rebuild_closures_in_statement(
    statement: &mut Statement,
    captured: &rustc_hash::FxHashSet<RcLocal>,
    single_write: &rustc_hash::FxHashSet<RcLocal>,
) -> bool {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    functions.into_iter().fold(false, |changed, function| {
        rebuild_with_captured(&mut function.lock().body, captured, single_write) | changed
    })
}

fn rebuild_current_block(block: &mut Block, captured: &rustc_hash::FxHashSet<RcLocal>, single_write: &rustc_hash::FxHashSet<RcLocal>) -> bool {
    // Keep a compacted prefix and consume the untouched suffix in source order.
    // Accepted stores become empty slots; each surviving statement moves at
    // most once, instead of shifting the suffix after every field assignment.
    let mut read = 0;
    let mut write = 0;
    let mut changed = false;
    while read < block.0.len() {
        if read != write {
            debug_assert!(matches!(block.0[write], Statement::Empty(_)));
            block.0.swap(write, read);
        }
        let index = write;
        write += 1;
        read += 1;
        if read == block.0.len() { break; }
        let Some(object_local) = table_constructor_local(&block.0[index]) else {
            continue;
        };

        let initial_len = block.0[index]
            .as_assign()
            .and_then(|assign| assign.right[0].as_table())
            .map(|table| table.0.len())
            .unwrap_or(0);
        // A fresh single-write declaration cannot already be reachable through
        // a closure. Contiguous stores keep it private until their first escape:
        // every key/value below is checked for a read or capture of this object.
        // Later closure captures do not retroactively observe initialization.
        let private = !captured.contains(&object_local)
            || (single_write.contains(&object_local)
                && !observed_before(&block.0[..index], &object_local, &mut 8192, 0));
        if private && captured.contains(&object_local) && initial_len == 0
            && lone_callback_field(&block.0[read..], &object_local)
        {
            // A one-field wrapper around a callback adds nesting without
            // grouping a registry. Retain the readable statement layout.
            continue;
        }
        if !private { continue; }
        let (prefix, remaining) = block.0.split_at_mut(read);
        let table = prefix[index].as_assign_mut().unwrap().right[0]
            .as_table_mut().unwrap();
        let mut array_len = table.0.iter().filter(|(key, _)| key.is_none()).count();
        let mut entries = PlaceholderEntries::new(table);
        let mut listed = None;
        let mut appended_slot_keys = false;
        for position in 0..remaining.len() {
            let (statement, rest) = remaining[position..].split_first_mut().unwrap();
            if let Statement::SetList(set_list) = statement {
                if let Some(entries) = entries.take() { entries.finish(table); }
                if !can_append_set_list_at(table, array_len, set_list, &object_local) {
                    break;
                }
                let Statement::SetList(set_list) = std::mem::replace(statement, crate::Empty {}.into()) else {
                    unreachable!()
                };
                array_len += set_list.values.len() + usize::from(set_list.tail.is_some());
                table.0.extend(
                    set_list
                        .values
                        .into_iter()
                        .map(|value| (None, single_value(value))),
                );
                if let Some(tail) = set_list.tail {
                    table.0.push((None, tail));
                }
                changed = true;
                read += 1;
                continue;
            }
            let Some((key, value)) = statement
                .as_assign()
                .and_then(|assign| field_assignment_parts(assign, &object_local))
            else {
                break;
            };
            if table
                    .0
                    .last()
                    .is_some_and(|(key, value)| key.is_none() && expands(value))
                || !can_fold_table_field_assignment(key, value, &object_local)
            {
                break;
            }
            if !matches!(key, RValue::Literal(crate::Literal::String(_))) {
                if let Some(entries) = entries.take() { entries.finish(table); }
            }
            let placement = match &entries {
                Some(entries) => entries.placement(table, key),
                None => placement(table, initial_len, key,
                    listed.get_or_insert_with(|| crate::ListedKeys::new(table)), appended_slot_keys),
            };
            let mut placement = match placement {
                Some(placement) => placement,
                // Luau emits SETLIST for a constructor's list items only: a
                // store before one was an entry of the constructor itself,
                // listed key included (`{[0] = nil, [-0] = v, item}`).
                None if set_list_follows(rest, &object_local) => {
                    appended_slot_keys = true;
                    Placement::Append
                }
                None => break,
            };
            if !keeps_template_layout(table, placement, key, value) {
                // A constructor may repeat a nil key: retain the later store
                // when replacing the earlier one would change collision history.
                if !set_list_follows(rest, &object_local) || !crate::table_takes_another_key(table, key) { break; }
                appended_slot_keys = true;
                placement = Placement::Append;
            }

            let field_assign = std::mem::replace(statement, crate::Empty {}.into()).into_assign().unwrap();
            let (key, value) = field_assignment_key_value(field_assign);
            match &mut entries {
                Some(entries) => entries.insert(table, placement, key, value),
                None => insert_table_entry(table, placement, key, value, listed.as_mut().unwrap()),
            }
            changed = true;
            read += 1;
        }
        if let Some(entries) = entries { entries.finish(table); }
    }
    block.0.truncate(write);
    changed
}

/// A list store whose index skips far past the array its constructor fills:
/// Luau writes a constructor's list items in batches that continue each
/// other from index 1, so only a hand-made chunk leaves a gap. A short gap
/// still prints as `nil` items (the VM sizes the array part to the store's
/// last index); past [`MAX_LIST_GAP`] slots no stores fill, no bounded
/// constructor stands for it.
pub const LIST_INDEX_GAP: &str = "a SETLIST index far past the array its constructor fills";

/// Per-slot source writes cannot reproduce an unknown bulk array resize.
pub const OPEN_LIST_TAIL: &str = "an open SETLIST without a proved native constructor";

/// The most `nil` items a hand-made list store's gap may print as.
const MAX_LIST_GAP: usize = 255;

/// The slots a constructor allocates for list items no pass could fold into
/// it (a branch or a call came first): Luau sizes the array part from the
/// list items, and list items keep their place in it, first under `next`.
/// `nil` items hold the slots until the list's own stores fill them. Runs
/// once, on the finished tree, every closure included. Also preserves an
/// underfilled hash allocation. Refuses an unrepresentable allocation or
/// ([`LIST_INDEX_GAP`]) a store that skips past the slots the stores fill.
pub fn reserve_unfolded_table_slots(block: &mut Block) -> Result<(), &'static str> {
    // Leave an unfinished constructor intact for the bounded snapshot recovery.
    if block.0.iter().any(|statement| matches!(statement, Statement::SetList(list) if list.tail.is_some())) {
        return Err(OPEN_LIST_TAIL);
    }
    unfold_store_sized_fields(block);
    for index in 0..block.0.len() {
        let (head, rest) = block.0.split_at_mut(index + 1);
        // The constructor the compiler sizes from the stores keeps no padding.
        let mut sized = false;
        if let Some(object) = table_constructor_local(&head[index])
            && let Statement::Assign(assign) = &mut head[index]
            && let RValue::Table(table) = &mut assign.right[0]
        {
            sized = sized_by_stores(table, rest, &object);
            if !sized {
                reserve_allocation_slots(table)?;
            }
            reserve_list_slots(table, rest, &object)?;
        }
        let statement = &mut block.0[index];
        let mut nested = Ok(());
        statement.traverse_rvalues(&mut |value| {
            if nested.is_ok() {
                nested = match value {
                    RValue::Closure(closure) => reserve_unfolded_table_slots(&mut closure.function.lock().body),
                    // The first table is the constructor itself.
                    RValue::Table(_) if std::mem::take(&mut sized) => Ok(()),
                    RValue::Table(table) => reserve_allocation_slots(table),
                    _ => Ok(()),
                };
            }
        });
        nested?;
        match statement {
            Statement::If(node) => {
                reserve_unfolded_table_slots(&mut node.then_block.lock())?;
                reserve_unfolded_table_slots(&mut node.else_block.lock())?;
            }
            Statement::While(node) => reserve_unfolded_table_slots(&mut node.block.lock())?,
            Statement::Repeat(node) => reserve_unfolded_table_slots(&mut node.block.lock())?,
            Statement::NumericFor(node) => reserve_unfolded_table_slots(&mut node.block.lock())?,
            Statement::GenericFor(node) => reserve_unfolded_table_slots(&mut node.block.lock())?,
            _ => {}
        }
    }
    Ok(())
}

/// A constructor holding named fields of a table its later stores also
/// fill, smaller than the allocation those stores size (`local M = { A =
/// function() end }; function M.B() end ...`): as stores again, right after
/// `local M = {}`, the fields let Luau size the table from all of them, as
/// the source did; kept, the constructor needs `nil` fields to hold the
/// allocation ([`Table::hash_padding`]).
fn unfold_store_sized_fields(block: &mut Block) {
    let mut index = 0;
    while index < block.0.len() {
        if let Some(object) = table_constructor_local(&block.0[index])
            && let Statement::Assign(assign) = &block.0[index]
            && let RValue::Table(table) = &assign.right[0]
            && let crate::TableShape::NewTable { hash, array } = table.2
            && table.0.len().next_power_of_two() != hash as usize
            && let Some(name) = object.0.0.lock().0.clone()
            && let Some(fields) = table.0.iter().map(|(key, value)| match key {
                // Moved past the declaration, a value would read the new
                // local where it read another `name`.
                Some(RValue::Literal(crate::Literal::String(field)))
                    if crate::formatter::Formatter::<String>::is_valid_name(field) && !mentions(value, &name) =>
                {
                    Some(field.clone())
                }
                _ => None,
            }).collect::<Option<rustc_hash::FxHashSet<_>>>()
        {
            let mut prediction = ArrayPrediction { object: &object, size: 0, loops: Default::default(), fields };
            prediction.block(&block.0[index + 1..]);
            if prediction.size == array && prediction.fields.len().next_power_of_two() == hash as usize {
                let Statement::Assign(assign) = &mut block.0[index] else { unreachable!() };
                let RValue::Table(table) = &mut assign.right[0] else { unreachable!() };
                let stores: Vec<Statement> = std::mem::take(&mut table.0)
                    .into_iter()
                    .map(|(key, value)| Assign::new(vec![Index::new(object.clone().into(), key.unwrap()).into()], vec![value]).into())
                    .collect();
                let after = index + 1;
                index += stores.len();
                block.0.splice(after..after, stores);
            }
        }
        index += 1;
    }
}

/// Whether `value` prints `name` as a word.
fn mentions(value: &RValue, name: &str) -> bool {
    let text = value.to_string();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    text.match_indices(name).any(|(at, _)| !word(text[..at].chars().next_back()) && !word(text[at + name.len()..].chars().next()))
}

/// An array allocation no `nil` items spell ([`create_large_allocations`]).
pub const LARGE_ALLOCATION: &str = "a NEWTABLE array allocation without enough reproducible constructor items";

/// `table.create(n)` for an empty constructor whose array part is past what
/// `nil` items may spell: Luau allocates the same table (`lua_createtable(L,
/// n, 0)`). Only where `table` is the builtin library: the chunk never
/// assigns it and has a fixed environment. The hash part of an empty
/// constructor is not kept elsewhere either ([`Table::hash_padding`]).
pub fn create_large_allocations(block: &mut Block) {
    for index in 0..block.0.len() {
        // A constructor its stores size keeps `{}`.
        let (head, rest) = block.0.split_at(index + 1);
        let mut sized = table_constructor_local(&head[index]).is_some_and(|object| {
            matches!(&head[index], Statement::Assign(assign)
                if matches!(&assign.right[0], RValue::Table(table) if sized_by_stores(table, rest, &object)))
        });
        let statement = &mut block.0[index];
        statement.traverse_rvalues(&mut |value| match value {
            RValue::Closure(closure) => create_large_allocations(&mut closure.function.lock().body),
            RValue::Table(_) if std::mem::take(&mut sized) => {}
            RValue::Table(table)
                if table.0.is_empty()
                    && matches!(table.2, crate::TableShape::NewTable { array, .. }
                        if array as usize > crate::table::MAX_ALLOCATION_PADDING) =>
            {
                let crate::TableShape::NewTable { array, .. } = table.2 else { unreachable!() };
                let create = Index::new(
                    crate::Global::new(b"table".to_vec()).into(),
                    crate::Literal::String(b"create".to_vec()).into(),
                );
                *value = crate::Call::new(create.into(), vec![crate::Literal::Number(f64::from(array)).into()]).into();
            }
            _ => {}
        });
        match statement {
            Statement::If(node) => {
                create_large_allocations(&mut node.then_block.lock());
                create_large_allocations(&mut node.else_block.lock());
            }
            Statement::While(node) => create_large_allocations(&mut node.block.lock()),
            Statement::Repeat(node) => create_large_allocations(&mut node.block.lock()),
            Statement::NumericFor(node) => create_large_allocations(&mut node.block.lock()),
            Statement::GenericFor(node) => create_large_allocations(&mut node.block.lock()),
            _ => {}
        }
    }
}

/// Late recovery for effectful prefix fields. The pipeline's final local and
/// register checks also cover these introduced bindings; no cleanup may move them.
// shortcut: bound recovery to 64 regions with 32 fields; expand with headroom and motion controls.
pub fn snapshot_open_list_constructors(block: &mut Block) -> Option<crate::local_producers::Pass> {
    let inventory = crate::lower_conditionals::prepare_local_rewrite(block, |block|
        block.0.iter().any(|statement| matches!(statement, Statement::SetList(list) if list.tail.is_some()))
    ).ok()??;
    let mut state = OpenListSnapshots { safety: crate::deinline_safety::CaptureSafety::captures(block),
        inventory, visited: Default::default(), regions: 0,
        pass: crate::local_producers::Pass { pass: "table_open_list_snapshots",
            rewrite_model: "private-prefix-snapshots-v1", introduced_locals: 0, ledger: Default::default() } };
    state.block(block, None);
    (state.regions > 0).then_some(state.pass)
}

struct OpenListSnapshots {
    safety: crate::deinline_safety::CaptureSafety,
    inventory: crate::lower_conditionals::RewriteInventory,
    visited: rustc_hash::FxHashSet<usize>,
    regions: usize,
    pass: crate::local_producers::Pass,
}

impl OpenListSnapshots {
    fn can_snapshot_key(&self, key: &RValue, value: &RValue, function: Option<usize>) -> bool {
        // shortcut: ambiguous capture contexts stay refused until read timing is proved for every occurrence.
        !matches!(key, RValue::Local(local) if self.inventory.captured.contains(&local.stable_id())
            && !crate::is_total_pure(value) && !self.safety.register_of(local, function)
            && !self.safety.upvalue_of(local, function))
    }

    fn snapshots(&mut self, fields: &mut [(Option<RValue>, RValue)], function: Option<usize>) -> Vec<Statement> {
        let mut prefix = Vec::new();
        for (key, value) in fields {
            // An owning register key is read by SETTABLE after the value callback.
            let key_last = matches!(key, Some(RValue::Local(local))
                if self.inventory.captured.contains(&local.stable_id())
                    && !crate::is_total_pure(value) && self.safety.register_of(local, function));
            let mut values = [(key.as_mut(), "constructorKey"), (Some(value), "constructorValue")];
            if key_last { values.swap(0, 1); }
            for (value, name) in values {
                let Some(value) = value else { continue; };
                if matches!(value, RValue::Literal(_)) { continue; }
                let local = crate::rehoist_constants::unique_local(name, &mut self.inventory.reserved);
                let mut declaration = Assign::new(vec![local.clone().into()],
                    vec![std::mem::replace(value, local.clone().into())]);
                declaration.prefix = true;
                prefix.push(declaration.into());
                self.pass.introduced_locals += 1;
                self.pass.ledger.record(&local, crate::local_producers::Role::ConstructorInitializerSnapshot);
            }
        }
        prefix
    }

    fn value(&mut self, value: &mut RValue) {
        if let RValue::Closure(closure) = value {
            let function = (&*closure.function.0 as *const _) as usize;
            if self.visited.insert(function) {
                self.block(&mut closure.function.lock().body, Some(function));
            }
        } else {
            value.visit_rvalues_mut(&mut |child| { self.value(child); true });
        }
    }

    fn block(&mut self, block: &mut Block, function: Option<usize>) {
        for statement in &mut block.0 {
            if let Statement::Assign(assign) = statement {
                for left in &mut assign.left {
                    if let LValue::Index(index) = left {
                        self.value(&mut index.left);
                        self.value(&mut index.right);
                    }
                }
            }
            statement.visit_rvalues_mut(&mut |value| { self.value(value); true });
            match statement {
                Statement::If(node) => { self.block(&mut node.then_block.lock(), function); self.block(&mut node.else_block.lock(), function); }
                Statement::While(node) => self.block(&mut node.block.lock(), function),
                Statement::Repeat(node) => self.block(&mut node.block.lock(), function),
                Statement::NumericFor(node) => self.block(&mut node.block.lock(), function),
                Statement::GenericFor(node) => self.block(&mut node.block.lock(), function),
                _ => {}
            }
        }
        let mut index = 0;
        while index < block.0.len() && self.regions < 64 {
            let Some(object) = table_constructor_local(&block.0[index]) else { index += 1; continue; };
            if self.inventory.captured.contains(&object.stable_id()) { index += 1; continue; }
            let original = block.0[index].as_assign().unwrap().right[0].as_table().unwrap();
            let crate::TableShape::NewTable { hash, array } = original.2 else { index += 1; continue; };
            let positional = original.0.iter().filter(|(key, _)| key.is_none()).count();
            let mut keyed = original.0.len() - positional;
            if original.0.len() > 32 || keyed > hash as usize
                || original.0.last().is_some_and(|(key, value)| key.is_none() && expands(value))
                || original.0.iter().any(|(key, value)| key.as_ref().is_some_and(|key| !self.can_snapshot_key(key, value, function)))
            {
                index += 1; continue;
            }
            let mut can_raise = false;
            let mut interleave_checks = false;
            for (key, value) in &original.0 {
                // Once a store can fail, no later expression may run early.
                interleave_checks |= can_raise && (key.as_ref().is_some_and(|key| !crate::is_total_pure(key))
                    || !crate::is_total_pure(value));
                can_raise |= key.as_ref().is_some_and(|key| !crate::is_total_table_key(key));
            }
            let initial_fields = original.0.len();
            let mut table = original.clone();
            let mut stores = Vec::new();
            let mut endpoint = None;
            let mut proof = ConstructorMotion { object: object.stable_id(), dependencies: Default::default(), remaining: 4096 };
            for next in index + 1..block.0.len().min(index + 65) {
                if let Statement::SetList(list) = &block.0[next] {
                    if list.tail.is_some() && can_append_set_list(&table, list, &object) { endpoint = Some(next); }
                    break;
                }
                if let Some((key, value)) = block.0[next].as_assign()
                    .and_then(|assign| field_assignment_parts(assign, &object))
                {
                    // These keys remain explicit beside the tail, so each consumes hash syntax.
                    if table.0.len() >= 32 || keyed >= hash as usize
                        || !self.can_snapshot_key(key, value, function)
                        || !crate::table_takes_another_key(&table, key)
                        || !proof.value(key, 0) || !proof.value(value, 0) { break; }
                    can_raise |= !crate::is_total_table_key(key);
                    stores.push((next, table.0.len()));
                    // Repeat the store, not its final value: nil can change collision history.
                    table.0.push((Some(key.clone()), value.clone()));
                    keyed += 1;
                } else if !proof.statement(&block.0[next], 0) { break; }
            }
            let Some(end) = endpoint else { index += 1; continue; };
            let list = block.0[end].as_set_list().unwrap();
            let tail = list.tail.as_ref().unwrap();
            // Retaining positional fields must reproduce the original static array allocation, not an earlier resize.
            if positional > 0 && (!expands(tail) || array as usize != positional + list.values.len()
                + usize::from(!matches!(tail, RValue::VarArg(_))))
            { index += 1; continue; }
            table.0.extend(list.values.iter().cloned().map(|value| (None, single_value(value))));
            table.0.push((None, list.tail.clone().unwrap()));
            if table.hash_padding().is_err() { index += 1; continue; }
            // A private key checker needs no array padding that might overwrite a checked numeric key.
            interleave_checks |= positional > 0 && can_raise;
            let mut prefix = if interleave_checks {
                let mut check = block.0[index].as_assign().unwrap().clone();
                // This private table only checks keys; the final constructor retains allocation and layout.
                check.right[0] = Table::default().into();
                let mut prefix = vec![check.into()];
                for field in &mut table.0[..initial_fields] {
                    prefix.extend(self.snapshots(std::slice::from_mut(field), function));
                    if let Some(key) = &field.0 {
                        prefix.push(Assign::new(vec![Index::new(object.clone().into(), key.clone()).into()],
                            vec![field.1.clone()]).into());
                    }
                }
                prefix
            } else {
                self.snapshots(&mut table.0[..initial_fields], function)
            };
            let mut replacements = Vec::new();
            for (position, field) in stores {
                let mut prefix = self.snapshots(&mut table.0[field..field + 1], function);
                if can_raise {
                    let mut store = block.0[position].as_assign().unwrap().clone();
                    store.left[0].as_index_mut().unwrap().right = Box::new(table.0[field].0.clone().unwrap());
                    store.right[0] = table.0[field].1.clone();
                    prefix.push(store.into());
                }
                replacements.push((position, prefix));
            }
            let mut initializer = block.0[index].as_assign().unwrap().clone();
            initializer.right[0].as_table_mut().unwrap().0.clone_from_slice(&table.0[..initial_fields]);
            let mut final_store = initializer.clone();
            final_store.prefix = !can_raise;
            final_store.node_origin.merge(&list.node_origin);
            final_store.right[0] = table.into();
            block.0[end] = final_store.into();
            if can_raise && !interleave_checks { prefix.push(initializer.into()); }
            let inserted = prefix.len();
            for (position, replacement) in replacements.into_iter().rev() {
                block.0.splice(position..=position, replacement);
            }
            block.0.splice(index..=index, prefix);
            self.regions += 1;
            index += inserted;
        }
    }
}

fn lower_array_keys(table: &mut Table) -> Result<usize, &'static str> {
    let crate::TableShape::NewTable { hash, array } = table.2 else { return Ok(0); };
    let positional = table.0.iter().filter(|(key, _)| key.is_none()).count();
    // Positional fields have their own numbering: converting `[2]` in
    // `{1, [2] = nil, 3}` would shift the final store from index 2 to 3.
    if positional > 0 { return Ok(positional); }
    let sequential = table.0.iter().filter_map(|(key, _)| key.as_ref().and_then(|key| table.array_index(key)))
        .enumerate().all(|(slot, index)| index == slot + 1);
    if !sequential {
        // Pad the array before these stores, retaining their explicit keys and
        // order. Their keyed syntax must still fit the original hash allocation.
        return if table.0.len() <= hash as usize { Ok(0) }
            else { Err("nonsequential constructor stores into a preallocated array") };
    }
    let mut slots = 0;
    for (key, value) in &mut table.0 {
        if let Some(RValue::Literal(crate::Literal::Number(index))) = key
            && *index >= 1.0 && *index <= f64::from(array) && index.fract() == 0.0
        {
            if *index != (slots + 1) as f64 {
                return Err("nonsequential constructor stores into a preallocated array");
            }
            *key = None;
            *value = single_value(std::mem::replace(value, crate::Literal::Nil.into()));
        }
        slots += usize::from(key.is_none());
    }
    Ok(slots)
}

/// Whether Luau gives the printed `local object = {}` the array part the
/// bytecode allocated without any `nil` items: it sizes an empty constructor
/// declared alone from the stores to the local that follow (`TableShape.cpp`).
fn sized_by_stores(table: &Table, rest: &[Statement], object: &RcLocal) -> bool {
    let crate::TableShape::NewTable { array, .. } = table.2 else { return false };
    array > 0
        && table.0.is_empty()
        && !rest.iter().any(|statement| matches!(statement, Statement::SetList(list) if &list.object_local == object))
        && {
            let mut prediction = ArrayPrediction { object, size: 0, loops: Default::default(), fields: Default::default() };
            prediction.block(rest);
            prediction.size == array
        }
}

/// Luau's `predictTableShapes` for one table, over the printed statements in
/// source order, nested blocks and closures included: `object[n] = v` grows
/// the array part when `n` is the next index; `object[i] = v` in a
/// `for i = 1, k` loop (`k <= 16`, no step) sizes a still-empty one to `k`;
/// each distinct `object.name = v` adds a hash node (`fields`). Compound
/// assignments and declarations never count.
struct ArrayPrediction<'a> {
    object: &'a RcLocal,
    size: u32,
    loops: rustc_hash::FxHashMap<RcLocal, u32>,
    fields: rustc_hash::FxHashSet<Vec<u8>>,
}

impl ArrayPrediction<'_> {
    fn block(&mut self, statements: &[Statement]) {
        for statement in statements {
            self.statement(statement);
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Assign(assign) if !assign.prefix && !crate::formatter::prints_as_compound(assign) => {
                for left in &assign.left {
                    if let LValue::Index(Index { left: box RValue::Local(base), right: key, .. }) = left
                        && base == self.object
                    {
                        match &**key {
                            RValue::Literal(crate::Literal::Number(index)) if *index == f64::from(self.size) + 1.0 => {
                                self.size += 1;
                            }
                            RValue::Local(counter) if self.size == 0 => {
                                if let Some(&bound) = self.loops.get(counter) {
                                    self.size = bound;
                                }
                            }
                            RValue::Literal(crate::Literal::String(name)) if crate::formatter::Formatter::<String>::is_valid_name(name) => {
                                self.fields.insert(name.clone());
                            }
                            _ => {}
                        }
                    }
                }
                for right in &assign.right {
                    self.closures(right);
                }
                return;
            }
            Statement::NumericFor(node) => {
                for value in [&node.initial, &node.limit, &node.step] {
                    self.closures(value);
                }
                if let (
                    RValue::Literal(crate::Literal::Number(initial)),
                    RValue::Literal(crate::Literal::Number(limit)),
                    RValue::Literal(crate::Literal::Number(step)),
                ) = (&node.initial, &node.limit, &node.step)
                    && *initial == 1.0
                    && *step == 1.0
                    && (1.0..=16.0).contains(limit)
                {
                    self.loops.insert(node.counter.clone(), *limit as u32);
                }
                self.block(&node.block.lock());
                return;
            }
            _ => {}
        }
        statement.traverse_rvalues_ref(&mut |value| {
            if let RValue::Closure(closure) = value {
                self.block(&closure.function.lock().body);
            }
        });
        match statement {
            Statement::If(node) => {
                self.block(&node.then_block.lock());
                self.block(&node.else_block.lock());
            }
            Statement::While(node) => self.block(&node.block.lock()),
            Statement::Repeat(node) => self.block(&node.block.lock()),
            Statement::GenericFor(node) => self.block(&node.block.lock()),
            _ => {}
        }
    }

    fn closures(&mut self, value: &RValue) {
        let mut visit = |value: &RValue| {
            if let RValue::Closure(closure) = value {
                self.block(&closure.function.lock().body);
            }
        };
        visit(value);
        value.traverse_rvalues_ref(&mut visit);
    }
}

fn reserve_allocation_slots(table: &mut Table) -> Result<(), &'static str> {
    let crate::TableShape::NewTable { array, .. } = table.2 else { return Ok(()); };
    let slots = lower_array_keys(table)?;
    let missing = (array as usize).saturating_sub(slots);
    if missing > 0 {
        if missing > crate::table::MAX_ALLOCATION_PADDING || table.0.last().is_some_and(|(key, value)| key.is_none() && expands(value)) {
            return Err(LARGE_ALLOCATION);
        }
        pad_array_slots(table, slots, array as usize)?;
    }
    table.hash_padding().map(|_| ())
}

fn reserve_list_slots(table: &mut Table, statements: &[Statement], object: &RcLocal) -> Result<bool, &'static str> {
    let mut slots = 0;
    // The slots the stores write whatever a trailing call returns.
    let mut written = 0;
    // The slots the stores fill between them: compiler output continues each
    // batch where the last one (or the folded items) ended.
    let mut stored = 0usize;
    for statement in statements {
        if let Statement::SetList(list) = statement
            && &list.object_local == object
        {
            // A trailing `...` takes no slot of its own; a call takes one.
            let tail = list.tail.as_ref().is_some_and(|tail| !matches!(tail, RValue::VarArg(_)));
            let end = list.index.saturating_sub(1).saturating_add(list.values.len());
            slots = slots.max(end.saturating_add(usize::from(tail)));
            written = written.max(end);
            stored = stored.saturating_add(list.values.len() + usize::from(tail));
        }
        // Assigned again, the local names another table.
        if writes_local(statement, object) {
            break;
        }
    }
    // A `nil` item stores after every keyed entry: it must not clear one the
    // stores may leave alone (`{[2] = x, f()}` keeps `x` when `f()` returns
    // nothing).
    for (key, _) in &table.0 {
        match key {
            None => {}
            Some(RValue::Literal(crate::Literal::Number(index))) => {
                if index.fract() == 0.0 && *index > written as f64 && *index <= slots as f64 {
                    slots = *index as usize - 1;
                }
            }
            Some(RValue::Literal(_)) => {}
            Some(_) => slots = slots.min(written),
        }
    }
    let listed = table.0.iter().filter(|(key, _)| key.is_none()).count();
    if slots <= listed || table.0.last().is_some_and(|(key, value)| key.is_none() && expands(value)) {
        return Ok(false);
    }
    // Compiler output fills every slot past the folded items with a store,
    // so the pad below stays bounded by the stores the output prints anyway.
    if slots - listed > stored.saturating_add(MAX_LIST_GAP) {
        return Err(LIST_INDEX_GAP);
    }
    pad_array_slots(table, listed, slots)?;
    Ok(true)
}

fn pad_array_slots(table: &mut Table, listed: usize, slots: usize) -> Result<(), &'static str> {
    // With no positional fields, reserve before keyed stores: a computed key
    // may address the array. Luau flushes these nil items before the first key.
    let position = if listed == 0 { 0 } else { table.0.len() };
    // An explicit key could already hold a value in a slot that padding clears.
    if table.0[..position].iter().any(|(key, value)| {
        let aliases = match key {
            Some(RValue::Literal(crate::Literal::Number(index))) => {
                index.fract() == 0.0 && *index > listed as f64 && *index <= slots as f64
            }
            None | Some(RValue::Literal(_)) => false,
            Some(_) => true,
        };
        aliases && !matches!(value, RValue::Literal(crate::Literal::Nil))
    }) {
        return Err("array allocation padding that could overwrite an explicit key");
    }
    table.0.splice(position..position, std::iter::repeat_n((None, crate::Literal::Nil.into()), slots - listed));
    Ok(())
}

/// Whether `statement`, its nested blocks included, assigns `local`.
fn writes_local(statement: &Statement, local: &RcLocal) -> bool {
    statement.values_written().contains(&local)
        || match statement {
            Statement::If(node) => {
                node.then_block.lock().0.iter().any(|statement| writes_local(statement, local))
                    || node.else_block.lock().0.iter().any(|statement| writes_local(statement, local))
            }
            Statement::While(node) => node.block.lock().0.iter().any(|statement| writes_local(statement, local)),
            Statement::Repeat(node) => node.block.lock().0.iter().any(|statement| writes_local(statement, local)),
            Statement::NumericFor(node) => node.block.lock().0.iter().any(|statement| writes_local(statement, local)),
            Statement::GenericFor(node) => node.block.lock().0.iter().any(|statement| writes_local(statement, local)),
            _ => false,
        }
}

/// Whether the run of stores to `object` that `statements` starts with ends
/// at a SETLIST of it.
fn set_list_follows(statements: &[Statement], object: &RcLocal) -> bool {
    for statement in statements {
        match statement {
            Statement::SetList(list) => return &list.object_local == object,
            statement if statement.as_assign().and_then(|assign| field_assignment_parts(assign, object)).is_some() => {}
            _ => return false,
        }
    }
    false
}

fn lone_callback_field(statements: &[Statement], object: &RcLocal) -> bool {
    let Some(first) = statements.first() else { return false; };
    if first.as_assign().and_then(|assign| field_assignment_parts(assign, object)).is_none() { return false; }
    let mut callback = false;
    crate::inline_temps::collect_closures_in_statement(first, &mut |_| callback = true);
    if !callback { return false; }
    !statements.get(1).is_some_and(|next| {
        next.as_assign().and_then(|assign| field_assignment_parts(assign, object))
            .is_some_and(|(key, value)| can_fold_table_field_assignment(key, value, object))
            || matches!(next, Statement::SetList(list) if can_append_set_list(&Table::default(), list, object))
    })
}

fn observed_before(statements: &[Statement], local: &RcLocal, remaining: &mut usize, depth: usize) -> bool {
    if depth >= 64 { return true; }
    for statement in statements {
        if *remaining == 0 { return true; }
        *remaining -= 1;
        if statement.any_local_read(&mut |read| read == local)
            || statement.values_written().into_iter().any(|written| written == local) {
            return true;
        }
        let nested = match statement {
            Statement::If(branch) => observed_before(&branch.then_block.lock().0, local, remaining, depth + 1)
                || observed_before(&branch.else_block.lock().0, local, remaining, depth + 1),
            Statement::While(loop_) => observed_before(&loop_.block.lock().0, local, remaining, depth + 1),
            Statement::Repeat(loop_) => observed_before(&loop_.block.lock().0, local, remaining, depth + 1),
            Statement::NumericFor(loop_) => observed_before(&loop_.block.lock().0, local, remaining, depth + 1),
            Statement::GenericFor(loop_) => observed_before(&loop_.block.lock().0, local, remaining, depth + 1),
            _ => false,
        };
        if nested { return true; }
    }
    false
}

fn expands(value: &RValue) -> bool {
    matches!(
        value,
        RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_)
    )
}

fn single_value(value: RValue) -> RValue {
    match value {
        RValue::Call(call) => crate::Select::Call(call).into(),
        RValue::MethodCall(call) => crate::Select::MethodCall(call).into(),
        RValue::VarArg(vararg) => crate::Select::VarArg(vararg).into(),
        value => value,
    }
}

fn can_append_set_list(table: &Table, list: &crate::SetList, object: &RcLocal) -> bool {
    can_append_set_list_at(table, table.0.iter().filter(|(key, _)| key.is_none()).count(), list, object)
}

fn can_append_set_list_at(table: &Table, array_len: usize, list: &crate::SetList, object: &RcLocal) -> bool {
    list.object_local == *object
        && list.index == 1 + array_len
        && !table.0.last().is_some_and(|(key, value)| key.is_none() && expands(value))
        // Luau flushes array entries before every keyed field. Appending at
        // the exact next list index therefore retains numeric-key overwrites,
        // nil stores and dynamic-key errors in their original order.
        && !list.values.iter().chain(list.tail.iter())
            .any(|value| value.any_local_read(&mut |read| read == object))
}

/// Sink an unobservable, total table allocation across intervening local
/// declarations so its first field writes become contiguous. This recovers
/// shapes such as PortalAnimator's `Model/Center/Parts` return without moving
/// the intervening call into the constructor.
fn sink_total_table_declarations(
    block: &mut Block,
    captured: &rustc_hash::FxHashSet<RcLocal>,
) -> bool {
    let mut changed = false;
    let mut index = 0;
    // A successful move only permutes local declarations within their run, so
    // its terminating non-declaration stays at the same index. Find each run's
    // endpoint once before examining expensive dependency/read predicates.
    let mut declarations_end = 0;
    while index + 2 < block.0.len() {
        let Some(object) = table_constructor_local(&block.0[index]) else {
            index += 1;
            continue;
        };
        if captured.contains(&object) {
            index += 1;
            continue;
        }
        if declarations_end <= index {
            declarations_end = index + 1;
            while declarations_end < block.0.len()
                && is_intervening_local_declaration(&block.0[declarations_end])
            {
                declarations_end += 1;
            }
        }
        if declarations_end == index + 1
            || declarations_end >= block.0.len()
            || block.0[declarations_end].as_assign()
                .and_then(|assign| field_assignment_parts(assign, &object)).is_none()
        {
            index += 1;
            continue;
        }
        let table = block.0[index].as_assign().unwrap().right[0]
            .as_table()
            .unwrap();
        if !crate::side_effects::is_total_table(table) {
            index += 1;
            continue;
        }
        let dependencies = table
            .values_read()
            .into_iter()
            .cloned()
            .collect::<rustc_hash::FxHashSet<_>>();
        if dependencies.iter().any(|local| captured.contains(local)) {
            index += 1;
            continue;
        }

        let mut field_index = index + 1;
        while field_index < block.0.len()
            && is_intervening_local_declaration(&block.0[field_index])
            && !block.0[field_index].any_local_read(&mut |read| read == &object)
            && !crate::inline_temps::statement_writes_any_local(
                &block.0[field_index],
                &dependencies,
            )
        {
            field_index += 1;
        }
        if field_index == index + 1
            || field_index >= block.0.len()
            || !block.0[field_index]
                .as_assign()
                .and_then(|assign| field_assignment_parts(assign, &object))
                .is_some_and(|(key, value)| absorbs_store(table, key, value))
        {
            index += 1;
            continue;
        }

        block.0[index..field_index].rotate_left(1);
        changed = true;
        index = field_index;
    }
    changed
}

#[cfg(test)]
fn sink_total_table_declarations_reference(
    block: &mut Block,
    captured: &rustc_hash::FxHashSet<RcLocal>,
) -> bool {
    let mut changed = false;
    let mut index = 0;
    while index + 2 < block.0.len() {
        let Some(object) = table_constructor_local(&block.0[index]) else {
            index += 1;
            continue;
        };
        if captured.contains(&object) {
            index += 1;
            continue;
        }
        let table = block.0[index].as_assign().unwrap().right[0]
            .as_table()
            .unwrap();
        if !crate::side_effects::is_total_table(table) {
            index += 1;
            continue;
        }
        let dependencies = table
            .values_read()
            .into_iter()
            .cloned()
            .collect::<rustc_hash::FxHashSet<_>>();
        if dependencies.iter().any(|local| captured.contains(local)) {
            index += 1;
            continue;
        }

        let mut field_index = index + 1;
        while field_index < block.0.len()
            && is_intervening_local_declaration(&block.0[field_index])
            && !block.0[field_index]
                .values_read()
                .iter()
                .any(|read| *read == &object)
            && !crate::inline_temps::statement_writes_any_local(
                &block.0[field_index],
                &dependencies,
            )
        {
            field_index += 1;
        }
        if field_index == index + 1
            || field_index >= block.0.len()
            || block.0[field_index]
                .as_assign()
                .and_then(|assign| field_assignment_parts(assign, &object))
                .is_none()
        {
            index += 1;
            continue;
        }

        let declaration = block.0.remove(index);
        block.0.insert(field_index - 1, declaration);
        changed = true;
        index = field_index;
    }
    changed
}


fn is_intervening_local_declaration(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Assign(assign)
            if assign.prefix
                && !assign.parallel
                && assign.left.iter().all(|left| matches!(left, LValue::Local(_)))
    ) || matches!(statement, Statement::Comment(_) | Statement::Empty(_))
}

/// Delay only an unobserved allocation and stable local/literal reads. Calls,
/// branch selection and stores to other objects stay at their original sites.
/// The target's first use must be a field/list write that the existing ordered
/// constructor builder can consume. This is independent of any UI API name.
fn sink_private_constructor_regions(
    block: &mut Block,
    captured: &rustc_hash::FxHashSet<RcLocal>,
) -> bool {
    let mut changed = false;
    let mut index = 0;
    while index + 2 < block.0.len() {
        let Some(object) = table_constructor_local(&block.0[index]) else { index += 1; continue; };
        if object.has_source_binding() || captured.contains(&object)
            || matches!(&block.0[index + 1], Statement::Comment(comment) if comment.trailing) {
            index += 1;
            continue;
        }
        let mut proof = ConstructorMotion { object: object.stable_id(), dependencies: Default::default(), remaining: 4096 };
        let initializer = &block.0[index].as_assign().unwrap().right[0];
        if !proof.initializer(initializer, captured, 0) { index += 1; continue; }
        let table = initializer.as_table().unwrap();
        let mut destination = None;
        // Bound every proposal independently; failed proof leaves the region
        // untouched. No wall-clock or worker-dependent acceptance budget.
        for next in index + 1..block.0.len().min(index + 65) {
            let statement = &block.0[next];
            let endpoint = match statement {
                Statement::Assign(assign) => field_assignment_parts(assign, &object)
                    // Preserve statement function definitions and callback
                    // property layout; motion must improve the output tree.
                    .is_some_and(|(key, value)| !matches!(value, RValue::Closure(_))
                        && can_fold_table_field_assignment(key, value, &object)
                        && absorbs_store(table, key, value)),
                Statement::SetList(list) => can_append_set_list(table, list, &object),
                _ => false,
            };
            if endpoint {
                if next > index + 1 { destination = Some(next); }
                break;
            }
            if !proof.statement(statement, 0) { break; }
        }
        if let Some(next) = destination {
            block.0[index..next].rotate_left(1);
            crate::telemetry::count("constructor_regions_sunk", 1);
            changed = true;
            index = next;
        } else {
            index += 1;
        }
    }
    changed
}

struct ConstructorMotion {
    object: u64,
    dependencies: rustc_hash::FxHashSet<u64>,
    remaining: usize,
}

impl ConstructorMotion {
    fn tick(&mut self, depth: usize, width: usize) -> bool {
        if depth > 32 || self.remaining == 0 || width >= self.remaining { return false; }
        self.remaining -= 1;
        true
    }

    fn initializer(&mut self, value: &RValue, captured: &rustc_hash::FxHashSet<RcLocal>, depth: usize) -> bool {
        if !self.tick(depth, 0) { return false; }
        match value {
            RValue::Literal(_) => true,
            RValue::Local(local) => {
                if local.stable_id() == self.object || captured.contains(local) { return false; }
                self.dependencies.insert(local.stable_id());
                true
            }
            RValue::Table(table) => table.0.len().saturating_mul(2) < self.remaining
                && table.0.iter().all(|(key, value)| key.as_ref().is_none_or(|key|
                    self.tick(depth + 1, 0) && crate::is_total_table_key(key))
                    && self.initializer(value, captured, depth + 1)),
            // Keep callback layout, snapshots, dynamic-key errors and open
            // result tails. Type/API naming evidence is never a motion proof.
            _ => false,
        }
    }

    fn value(&mut self, value: &RValue, depth: usize) -> bool {
        let width = match value {
            RValue::Table(table) => table.0.len().saturating_mul(2),
            RValue::Call(call) | RValue::Select(crate::Select::Call(call)) => call.arguments.len() + 1,
            RValue::MethodCall(call) | RValue::Select(crate::Select::MethodCall(call)) => call.arguments.len() + 1,
            RValue::Closure(closure) => closure.upvalues.len(),
            _ => 3,
        };
        if !self.tick(depth, width) { return false; }
        match value {
            RValue::Local(local) if local.stable_id() == self.object => return false,
            RValue::Closure(closure) => {
                self.remaining -= closure.upvalues.len();
                return closure.upvalues.iter().all(|upvalue| {
                    let (crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local)) = upvalue;
                    local.stable_id() != self.object
                });
            }
            _ => {}
        }
        value.visit_rvalues(&mut |child| self.value(child, depth + 1))
    }

    fn statement(&mut self, statement: &Statement, depth: usize) -> bool {
        if !self.tick(depth, 0) { return false; }
        match statement {
            Statement::Assign(assign) => {
                if assign.parallel || assign.left.len().saturating_add(assign.right.len()) >= self.remaining {
                    return false;
                }
                for left in &assign.left {
                    if !self.tick(depth + 1, 0) { return false; }
                    match left {
                        LValue::Local(local) if local.stable_id() == self.object
                            || self.dependencies.contains(&local.stable_id()) => return false,
                        LValue::Index(index) if !self.value(&index.left, depth + 1)
                            || !self.value(&index.right, depth + 1) => return false,
                        _ => {}
                    }
                }
                assign.right.iter().all(|value| self.value(value, depth + 1))
            }
            Statement::If(branch) => self.value(&branch.condition, depth + 1)
                && self.block(&branch.then_block.lock(), depth + 1)
                && self.block(&branch.else_block.lock(), depth + 1),
            Statement::Call(call) => call.arguments.len() < self.remaining
                && self.value(&call.value, depth + 1)
                && call.arguments.iter().all(|value| self.value(value, depth + 1)),
            Statement::MethodCall(call) => call.arguments.len() < self.remaining
                && self.value(&call.value, depth + 1)
                && call.arguments.iter().all(|value| self.value(value, depth + 1)),
            Statement::Empty(_) | Statement::Comment(_) => true,
            // No control transfer, loop, close boundary or opaque SETLIST is
            // crossed. Both if arms must satisfy the same dependency proof.
            _ => false,
        }
    }

    fn block(&mut self, block: &Block, depth: usize) -> bool {
        block.0.len() < self.remaining && block.0.iter().all(|statement| self.statement(statement, depth))
    }
}

/// Recover a constructor field that an inlined helper immediately drains:
///
/// `local p = { ..., children = C }; local c = p.children; p.children = nil`
/// becomes `local p = { ... }; local c = C`.
///
/// The field must be the last constructor entry and no earlier entry may name
/// the same slot (`{99, [1] = 42}` keeps `99` in slot 1 once `[1] = 42` goes),
/// so evaluation order and the final raw-table contents are identical. The
/// regular table/tree inliner can then consume `p` and `c` from the leaves
/// upward.
fn extract_drained_constructor_fields(block: &mut Block) -> bool {
    let mut changed = false;
    let mut index = 0;
    while index + 2 < block.0.len() {
        let Some((object, key)) =
            drained_field_pattern(&block.0[index], &block.0[index + 1], &block.0[index + 2])
        else {
            index += 1;
            continue;
        };

        let table = block.0[index].as_assign_mut().unwrap().right[0]
            .as_table_mut()
            .unwrap();
        let Some((Some(last_key), _)) = table.0.last() else {
            index += 1;
            continue;
        };
        if last_key != &key
            || table
                .0
                .iter()
                .take(table.0.len() - 1)
                .any(|(existing, _)| may_name_slot(existing.as_ref(), &key))
        {
            index += 1;
            continue;
        }
        let (_, value) = table.0.pop().unwrap();
        if value.any_local_read(&mut |read| read == &object) {
            table.0.push((Some(key), value));
            index += 1;
            continue;
        }

        block.0[index + 1].as_assign_mut().unwrap().right[0] = value;
        block.0.remove(index + 2);
        changed = true;
        index += 1;
    }
    changed
}

fn drained_field_pattern(
    constructor: &Statement,
    alias: &Statement,
    clear: &Statement,
) -> Option<(RcLocal, RValue)> {
    let object = table_constructor_local(constructor)?;

    let Statement::Assign(alias) = alias else {
        return None;
    };
    if !alias.prefix || alias.parallel || alias.left.len() != 1 || alias.right.len() != 1 {
        return None;
    }
    let LValue::Local(alias_local) = &alias.left[0] else {
        return None;
    };
    if alias_local == &object {
        return None;
    }
    let RValue::Index(alias_index) = &alias.right[0] else {
        return None;
    };
    if !matches!(alias_index.left.as_ref(), RValue::Local(local) if local == &object)
        || !stable_drained_key(&alias_index.right)
    {
        return None;
    }
    let key = (*alias_index.right).clone();

    let Statement::Assign(clear) = clear else {
        return None;
    };
    if clear.prefix
        || clear.parallel
        || clear.left.len() != 1
        || !matches!(
            clear.right.as_slice(),
            [RValue::Literal(crate::Literal::Nil)]
        )
    {
        return None;
    }
    let LValue::Index(clear_index) = &clear.left[0] else {
        return None;
    };
    if !matches!(clear_index.left.as_ref(), RValue::Local(local) if local == &object)
        || clear_index.right.as_ref() != &key
    {
        return None;
    }
    Some((object, key))
}

fn stable_drained_key(key: &RValue) -> bool {
    crate::side_effects::is_total_table_key(key)
}

/// Whether a constructor entry keyed `existing` (`None`: positional) may
/// store into the slot of the literal `key`. Positional entries fill 1, 2, ...
/// (a call or `...` last, any number of them), a computed key any slot;
/// numbers name slots by value (`[0]` and `[-0]` are one slot).
fn may_name_slot(existing: Option<&RValue>, key: &RValue) -> bool {
    use crate::Literal;
    let RValue::Literal(literal) = key else { return true };
    match existing {
        None => matches!(literal, Literal::Number(_)),
        Some(existing @ RValue::Literal(_)) => crate::same_table_key(existing, key),
        Some(_) => true,
    }
}

fn table_constructor_local(statement: &Statement) -> Option<RcLocal> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    let LValue::Local(local) = &assign.left[0] else {
        return None;
    };
    let RValue::Table(table) = &assign.right[0] else {
        return None;
    };
    if table.any_local_read(&mut |read| read == local) {
        return None;
    }
    Some(local.clone())
}

fn field_assignment_parts<'a>(
    assign: &'a Assign,
    object_local: &RcLocal,
) -> Option<(&'a RValue, &'a RValue)> {
    if assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    let LValue::Index(Index { left, right, .. }) = &assign.left[0] else {
        return None;
    };
    let RValue::Local(local) = left.as_ref() else {
        return None;
    };
    if local != object_local {
        return None;
    }
    Some((right.as_ref(), &assign.right[0]))
}

fn can_fold_table_field_assignment(key: &RValue, value: &RValue, object_local: &RcLocal) -> bool {
    !key.any_local_read(&mut |read| read == object_local)
        && !value.any_local_read(&mut |read| read == object_local)
}

// A contiguous field keeps key evaluation, then value evaluation, then the
// store at the same position. Keys may call or raise; no other evaluation is
// crossed and the fresh table cannot be observed by a captured reference.

fn field_assignment_key_value(assign: Assign) -> (RValue, RValue) {
    let key = *assign
        .left
        .into_iter()
        .next()
        .unwrap()
        .into_index()
        .unwrap()
        .right;
    let value = assign.right.into_iter().next().unwrap();
    (key, value)
}

/// Where a contiguous field store lands in the constructor being rebuilt.
#[derive(Clone, Copy)]
enum Placement {
    /// Take the key's slot. Replacing an entry in place moves the new value
    /// ahead of every entry after it, so this requires the slot and every
    /// crossed entry to evaluate nothing (template placeholders or constants).
    Replace(usize),
    /// A store after a fresh `nil` slot: drop the slot and append, keeping the
    /// value after every intervening evaluation.
    MoveToEnd(usize),
    Append,
}

/// The placement for a store to `key`, or `None` when the constructor already
/// lists `key` and cannot absorb the store. The store then stays a statement:
/// `{ value = a, value = b }` is never how a table is written, and the
/// statement form evaluates in exactly the same order.
/// `appended_slot_keys`: the run appended a key whose slot the constructor
/// already lists; a store to such a slot lands after that entry.
fn placement(
    table: &Table,
    initial_len: usize,
    key: &RValue,
    listed: &crate::ListedKeys,
    appended_slot_keys: bool,
) -> Option<Placement> {
    let window = initial_len.min(table.0.len());
    match last_entry_for(&table.0[..window], key) {
        Some(_) if appended_slot_keys && last_entry_for(&table.0[window..], key).is_some() => None,
        Some(position) if !keeps_listed_key(table, position, key) => None,
        // The value takes the listed key's place, ahead of every entry after
        // it, appended stores included (`{a = 1, b = 2}; t.c = g(); t.a = h()`
        // must not become `{a = h(), b = 2, c = g()}`).
        Some(position) if inert_suffix(table, position, table.0.len()) => {
            Some(Placement::Replace(position))
        }
        Some(position)
            if matches!(&table.0[position].1, RValue::Literal(crate::Literal::Nil))
                && crate::is_total_table_key(key) =>
        {
            Some(Placement::MoveToEnd(position))
        }
        _ if listed.lists(table, key) => None,
        _ => Some(Placement::Append),
    }
}

/// Whether folding a store into a DUPTABLE constructor keeps the order
/// `pairs` visits its keys in. The template's hash part has nodes for its
/// keys rounded up to a power of two; a store adding a key past that
/// rehashes the table, and a constructor printing the key would be a larger
/// template, laid out otherwise: `{a = 1, b = 2}; t.c = 3` is not `{a = 1,
/// b = 2, c = 3}` under `pairs` (fr-t1110 F2). A template's keys all exist
/// before its stores, so moving one to the end (a `nil` slot) changes the
/// order of insertion too.
fn keeps_template_layout(table: &Table, placement: Placement, key: &RValue, value: &RValue) -> bool {
    match placement {
        Placement::Replace(position) => crate::table_replaces_entry(table, position, value),
        Placement::MoveToEnd(position) => crate::table_moves_entry_to_end(table, position),
        Placement::Append => crate::table_takes_another_key(table, key),
    }
}

/// Whether the constructor could take a store to `key` right now. Sinking a
/// declaration toward a store it cannot absorb only moves the table.
fn absorbs_store(table: &Table, key: &RValue, value: &RValue) -> bool {
    match last_entry_for(&table.0, key) {
        None => crate::table_takes_another_key(table, key),
        Some(position) => keeps_listed_key(table, position, key)
            && ((inert_suffix(table, position, table.0.len())
                    && crate::table_replaces_entry(table, position, value))
                || (matches!(&table.0[position].1, RValue::Literal(crate::Literal::Nil))
                    && crate::is_total_table_key(key)
                    && crate::table_moves_entry_to_end(table, position))),
    }
}

/// Whether the entry at `position` keeps its key under a store to `key`
/// ([`crate::keeps_listed_key`]: a `nil` slot does not keep `[0]` under
/// `[-0]`).
fn keeps_listed_key(table: &Table, position: usize, key: &RValue) -> bool {
    let (listed, value) = &table.0[position];
    crate::keeps_listed_key(listed.as_ref(), value, key)
}

fn insert_table_entry(
    table: &mut Table,
    placement: Placement,
    key: RValue,
    value: RValue,
    listed: &mut crate::ListedKeys,
) {
    match placement {
        Placement::Replace(position) => table.0[position].1 = value,
        // The slot keeps its key ([`keeps_listed_key`]).
        Placement::MoveToEnd(position) => {
            let (listed_key, _) = table.0.remove(position);
            table.0.push((listed_key, value));
        }
        Placement::Append => {
            listed.add(&key);
            table.0.push((Some(key), value));
        }
    }
}

/// DUPTABLE templates are keyed placeholders. Keep their logical order
/// while replacing/removing entries, without repeatedly scanning or shifting
/// the prefix. [`placement`] searches exactly the first `initial_len` *live*
/// entries, including appended entries that enter that window after a
/// removal; `boundary` tracks that same moving window in stable slot indices.
/// Uncommon dynamic keys and SETLIST flush back to the general implementation.
#[cfg_attr(test, derive(Clone))]
struct PlaceholderEntries {
    /// Every live slot of each key, in slot order.
    positions: rustc_hash::FxHashMap<Vec<u8>, Vec<usize>>,
    non_inert: std::collections::BTreeSet<usize>,
    removed: Vec<bool>,
    boundary: usize,
}

impl PlaceholderEntries {
    fn new(table: &Table) -> Option<Self> {
        if table.0.len() < 16 || !table.0.iter().all(|(key, value)| {
            matches!(key, Some(RValue::Literal(crate::Literal::String(_))))
                && crate::is_inert_entry_value(value)
        }) { return None; }
        let mut positions: rustc_hash::FxHashMap<_, Vec<_>> = Default::default();
        for (index, (key, _)) in table.0.iter().enumerate() {
            let Some(RValue::Literal(crate::Literal::String(key))) = key else { unreachable!() };
            positions.entry(key.clone()).or_default().push(index);
        }
        Some(Self { positions, non_inert: Default::default(),
            removed: vec![false; table.0.len()], boundary: table.0.len() - 1 })
    }

    /// [`placement`] over stable slot indices.
    fn placement(&self, table: &Table, key: &RValue) -> Option<Placement> {
        let RValue::Literal(crate::Literal::String(bytes)) = key else { unreachable!() };
        let Some(positions) = self.positions.get(bytes).filter(|positions| !positions.is_empty()) else {
            return Some(Placement::Append);
        };
        let Some(position) = positions.iter().rev().copied().find(|position| *position <= self.boundary) else {
            // Only an appended copy lists the key.
            return None;
        };
        if self.non_inert.range(position..).next().is_none() {
            // Ahead of every later entry, appended stores included.
            Some(Placement::Replace(position))
        } else if matches!(&table.0[position].1, RValue::Literal(crate::Literal::Nil)) {
            Some(Placement::MoveToEnd(position))
        } else {
            None
        }
    }

    fn insert(&mut self, table: &mut Table, placement: Placement, key: RValue, value: RValue) {
        let RValue::Literal(crate::Literal::String(bytes)) = &key else { unreachable!() };
        let inert_value = crate::is_inert_entry_value(&value);
        match placement {
            Placement::Replace(position) => {
                table.0[position].1 = value;
                if !inert_value { self.non_inert.insert(position); }
                return;
            }
            Placement::MoveToEnd(position) => {
                self.removed[position] = true;
                let positions = self.positions.get_mut(bytes).unwrap();
                let slot = positions.iter().rposition(|slot| *slot == position).unwrap();
                positions.remove(slot);
                // Removed slots are always inside the window. The immediately
                // following slot therefore remains live, or is appended below.
                self.boundary += 1;
            }
            Placement::Append => {}
        }
        let index = table.0.len();
        self.positions.entry(bytes.clone()).or_default().push(index);
        self.removed.push(false);
        if !inert_value { self.non_inert.insert(index); }
        table.0.push((Some(key), value));
    }

    fn finish(self, table: &mut Table) {
        let mut removed = self.removed.into_iter();
        table.0.retain(|_| !removed.next().expect("one marker per stable entry"));
    }
}

/// The last of `entries` storing into `key`'s slot: the one a store to it
/// overwrites (`{[0] = 0, [-0] = 10}` keeps 10 in slot 0).
fn last_entry_for(entries: &[(Option<RValue>, RValue)], key: &RValue) -> Option<usize> {
    entries.iter().rposition(|(existing, _)| existing.as_ref().is_some_and(|existing| crate::same_table_key(existing, key)))
}

fn inert_suffix(table: &Table, position: usize, initial_len: usize) -> bool {
    table.0[position..initial_len].iter().all(|(key, value)| {
        key.as_ref()
            .is_some_and(crate::side_effects::is_total_table_key)
            && crate::is_inert_entry_value(value)
    })
}

#[cfg(test)]
mod tests {
    thread_local! {
        pub(super) static MERGE_KEY_COMPARISONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    use super::rebuild_table_literals;
    use crate::{
        Assign, Block, Call, Closure, Comment, Function, Global, If, Index, LValue, Literal, Local,
        RValue, RcLocal, Return, Table, Upvalue,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global(name.as_bytes().to_vec()))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    fn nil() -> RValue {
        RValue::Literal(Literal::Nil)
    }

    fn local_value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn declare(local: &RcLocal, value: RValue) -> crate::Statement {
        let mut assign = Assign::new(vec![LValue::Local(local.clone())], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    fn assign_field(object: &RcLocal, key: RValue, value: RValue) -> crate::Statement {
        Assign::new(
            vec![LValue::Index(Index::new(local_value(object), key))],
            vec![value],
        )
        .into()
    }

    fn print(value: RValue) -> crate::Statement {
        Call::new(global("print"), vec![value]).into()
    }

    #[test]
    fn template_placeholder_becomes_nil_only_after_both_paths_fill_it() {
        let object = local("object");
        let table = Table(vec![(Some(string("Selected")), number(0.0)),
            (Some(string("Other")), number(0.0))], Default::default(), crate::TableShape::Template { keys: 2 });
        let mut block = Block(vec![declare(&object, table.into()),
            If::new(local_value(&local("condition")),
                Block(vec![assign_field(&object, string("Selected"), number(1.0))]),
                Block(vec![assign_field(&object, string("Selected"), number(2.0))])).into(),
            print(local_value(&object))]);
        assert!(super::nil_unread_placeholders(&mut block, &Default::default()));
        let table = block.0[0].as_assign().unwrap().right[0].as_table().unwrap();
        assert_eq!(table.0[0].1, nil());
        assert_eq!(table.0[1].1, number(0.0));
        assert!(!super::nil_unread_placeholders(&mut block, &Default::default()));
    }

    #[test]
    fn template_placeholder_keeps_zero_before_observation_or_possible_rehash() {
        let object = local("object");
        let table = Table(vec![(Some(string("Selected")), number(0.0))],
            Default::default(), crate::TableShape::Template { keys: 1 });
        for kind in 0..8 {
            let write = assign_field(&object, string("Selected"), number(1.0));
            let hazard = match kind {
                0 => print(local_value(&object)),
                1 => assign_field(&object, string("Added"), number(2.0)),
                2 => assign_field(&object, global("key"), number(2.0)),
                3 => print(closure_capturing(&object)),
                4 => If::new(global("condition"), Block(vec![write.clone()]), Block::default()).into(),
                5 => Return::new(vec![local_value(&object)]).into(),
                6 => crate::While::new(global("condition"), Block(vec![write.clone()])).into(),
                _ => assign_field(&object, string("Selected"), Index::new(local_value(&object), string("Selected")).into()),
            };
            let mut block = Block(vec![declare(&object, table.clone().into()), hazard]);
            if kind != 4 { block.0.push(write); }
            assert!(!super::nil_unread_placeholders(&mut block, &Default::default()), "hazard {kind}");
        }
        let mut block = Block(vec![declare(&object, table.into()),
            assign_field(&object, string("Selected"), number(1.0))]);
        assert!(!super::nil_unread_placeholders(&mut block, &[object].into_iter().collect()));
    }

    #[test]
    fn template_placeholder_keeps_zero_across_possible_collection() {
        let object = local("object");
        let temporary = local("temporary");
        let table = Table(vec![(Some(string("Selected")), number(0.0))],
            Default::default(), crate::TableShape::Template { keys: 1 });
        let call = || RValue::Call(Call::new(global("collect"), vec![]));
        let write = || assign_field(&object, string("Selected"), number(1.0));
        let hazards = vec![
            print(number(1.0)),
            declare(&temporary, call()),
            declare(&temporary, Table::new(vec![]).into()),
            declare(&temporary, closure_capturing(&temporary)),
            assign_field(&object, string("Selected"), call()),
            If::new(call(), Block(vec![write()]), Block(vec![write()])).into(),
            If::new(global("condition"), Block(vec![write()]), Block(vec![write()])).into(),
        ];
        for (index, hazard) in hazards.into_iter().enumerate() {
            let mut block = Block(vec![declare(&object, table.clone().into()), hazard, write()]);
            assert!(!super::nil_unread_placeholders(&mut block, &Default::default()), "hazard {index}");
            assert_eq!(block[0].as_assign().unwrap().right[0].as_table().unwrap().0[0].1, number(0.0));
        }
    }

    #[test]
    fn template_placeholder_checks_all_constructor_values_and_keeps_safe_slots() {
        let object = local("object");
        for value in [RValue::Call(Call::new(global("collect"), vec![])), Table::new(vec![]).into()] {
            for duplicate in [false, true] {
                for earlier in [false, true] {
                    let mut fields = vec![(Some(string("Selected")), number(0.0)),
                        (Some(string(if duplicate { "Selected" } else { "Other" })), value.clone())];
                    if earlier { fields.reverse(); }
                    let table = Table(fields, Default::default(), crate::TableShape::Template { keys: 2 });
                    let mut block = Block(vec![declare(&object, table.into()),
                        assign_field(&object, string("Selected"), number(1.0))]);
                    assert!(!super::nil_unread_placeholders(&mut block, &Default::default()));
                }
            }
        }
        let table = Table(vec![(Some(string("Before")), number(7.0)),
            (Some(string("Selected")), number(0.0))], Default::default(), crate::TableShape::Template { keys: 2 });
        let mut block = Block(vec![declare(&object, table.into()),
            assign_field(&object, string("Selected"), number(1.0))]);
        assert!(super::nil_unread_placeholders(&mut block, &Default::default()));
        assert_eq!(block[0].as_assign().unwrap().right[0].as_table().unwrap().0[1].1, nil());
    }

    #[test]
    fn template_placeholder_keeps_zero_when_an_earlier_dead_key_can_rehash() {
        let object = local("object");
        for value in [RValue::Call(Call::new(global("collect"), vec![])),
            Table::new(vec![]).into(), closure_capturing(&local("captured"))]
        {
            let table = Table(vec![(Some(string("Earlier")), nil()),
                (Some(string("Collector")), value),
                (Some(string("Selected")), number(0.0))], Default::default(),
                crate::TableShape::Template { keys: 3 });
            let mut block = Block(vec![declare(&object, table.into()),
                If::new(local_value(&local("condition")),
                    Block(vec![assign_field(&object, string("Earlier"), number(1.0))]),
                    Block(vec![assign_field(&object, string("Earlier"), number(2.0))])).into(),
                assign_field(&object, string("Selected"), number(3.0))]);
            assert!(!super::nil_unread_placeholders(&mut block, &Default::default()));
            assert_eq!(block[0].as_assign().unwrap().right[0].as_table().unwrap().0[2].1, number(0.0));
            block.0.remove(1);
            assert!(!super::nil_unread_placeholders(&mut block, &Default::default()));
        }
    }

    #[test]
    fn hash_padding_repeats_only_the_first_key_and_is_bounded() {
        let mut table = Table(vec![(Some(string("first")), number(1.0)),
            (Some(string("second")), nil())], Default::default(), crate::TableShape::NewTable { hash: 8, array: 0 });
        let original = table.0.clone();
        super::reserve_allocation_slots(&mut table).unwrap();
        assert_eq!(table.0, original);
        assert_eq!(table.to_string().matches("[\"first\"] = nil").count(), 3);
        let padded = table.clone();
        super::reserve_allocation_slots(&mut table).unwrap();
        assert_eq!(table, padded);
        // No field can carry the padding: the legacy constructor prints unchanged.
        for fields in [vec![(None, number(1.0))], vec![(Some(global("key")), number(1.0))]] {
            let mut table = Table(fields, Default::default(), crate::TableShape::NewTable { hash: 8, array: 0 });
            let before = table.0.clone();
            super::reserve_allocation_slots(&mut table).unwrap();
            assert_eq!(table.0, before);
        }
        table.2 = crate::TableShape::NewTable { hash: 1 << 30, array: 0 };
        super::reserve_allocation_slots(&mut table).unwrap();
        table.2 = crate::TableShape::NewTable { hash: 0, array: (crate::table::MAX_ALLOCATION_PADDING + 1) as u32 };
        assert!(super::reserve_allocation_slots(&mut table).is_err());
    }

    #[test]
    fn preallocated_array_keys_keep_positions_capacity_and_single_values() {
        let mut table = Table(vec![(Some(number(1.0)), nil()),
            (Some(number(2.0)), Call::new(global("many"), vec![]).into())],
            Default::default(), crate::TableShape::NewTable { hash: 0, array: 2 });
        super::reserve_allocation_slots(&mut table).unwrap();
        assert_eq!(table.to_string(), "{ nil, (many()) }");
        table.2 = crate::TableShape::NewTable { hash: 0, array: 3 };
        super::reserve_allocation_slots(&mut table).unwrap();
        assert_eq!(table.0.len(), 3);
        assert_eq!(table.0[2], (None, nil()));
        table.2 = crate::TableShape::NewTable { hash: 1, array: 3 };
        let fields = table.0.clone();
        for _ in 0..2 {
            super::reserve_allocation_slots(&mut table).unwrap();
            assert!(table.to_string().contains("[1] = nil"));
            assert_eq!(table.0, fields);
        }
        let mut gap = Table(vec![(Some(number(2.0)), number(1.0))],
            Default::default(), crate::TableShape::NewTable { hash: 0, array: 3 });
        assert!(super::reserve_allocation_slots(&mut gap).is_err());
        let mut mixed = Table(vec![(None, number(1.0)), (Some(number(2.0)), nil()), (None, number(3.0))],
            Default::default(), crate::TableShape::NewTable { hash: 1, array: 2 });
        super::reserve_allocation_slots(&mut mixed).unwrap();
        assert!(mixed.to_string().contains("[2] = nil"));
        assert_eq!(mixed.0.len(), 3);
        mixed.0.pop();
        mixed.0[1].1 = number(9.0);
        assert!(super::reserve_allocation_slots(&mut mixed).is_err());
    }

    #[test]
    fn nonsequential_array_stores_keep_keys_after_initial_nil_reservation() {
        for keys in [vec![2.0], vec![1.0, 3.0], vec![3.0, 1.0]] {
            let fields: Vec<_> = keys.into_iter().map(|key| (Some(number(key)), number(key * 10.0))).collect();
            let mut table = Table(fields.clone(), Default::default(), crate::TableShape::NewTable { hash: 4, array: 3 });
            super::reserve_allocation_slots(&mut table).unwrap();
            assert_eq!(&table.0[..3], &[(None, nil()), (None, nil()), (None, nil())]);
            assert_eq!(&table.0[3..], &fields);
            let padded = table.clone();
            super::reserve_allocation_slots(&mut table).unwrap();
            assert_eq!(table, padded);
        }
        let fields = vec![(Some(number(1.0)), number(1.0)), (Some(number(3.0)), number(3.0))];
        let mut table = Table(fields.clone(), Default::default(), crate::TableShape::NewTable { hash: 1, array: 3 });
        assert!(super::reserve_allocation_slots(&mut table).is_err());
        assert_eq!(table.0, fields);
    }

    #[test]
    fn array_reservation_precedes_computed_keys_that_can_address_it() {
        let original = vec![(Some(global("key")), number(9.0))];
        let mut table = Table(original.clone(), Default::default(),
            crate::TableShape::NewTable { hash: 1, array: 2 });
        let object = local("object");
        let mut block = Block(vec![declare(&object, table.clone().into()),
            crate::SetList::new(object.clone(), 1, vec![number(1.0)],
                Some(Call::new(global("tail"), vec![]).into())).into()]);
        super::reserve_allocation_slots(&mut table).unwrap();
        assert_eq!(table.0[..2], [(None, nil()), (None, nil())]);
        assert_eq!(table.0[2..], original);
        let reserved = table.clone();
        super::reserve_allocation_slots(&mut table).unwrap();
        assert_eq!(table, reserved);
        // An open tail is refused before mutating the constructor, so the late
        // snapshot recovery can still see the original fields.
        assert_eq!(super::reserve_unfolded_table_slots(&mut block), Err(super::OPEN_LIST_TAIL));
        assert_eq!(block.0[0].as_assign().unwrap().right[0].as_table().unwrap().0, original);
    }

    #[test]
    fn final_reservation_refuses_open_tails_in_nested_blocks() {
        for tail in [Some(Call::new(global("tail"), vec![]).into()), Some(crate::VarArg {}.into()), None] {
            let open = tail.is_some();
            let object = local("object");
            let body = Block(vec![declare(&object, Table::default().into()),
                crate::SetList::new(object, 1, vec![number(1.0)], tail).into()]);
            let mut block = Block(vec![If::new(global("condition"), body, Block::default()).into()]);
            assert_eq!(super::reserve_unfolded_table_slots(&mut block),
                if open { Err(super::OPEN_LIST_TAIL) } else { Ok(()) });
        }
    }

    #[test]
    fn late_list_snapshots_keep_evaluation_order_and_record_fresh_bindings() {
        for dynamic_key in [false, true] {
            let object = local("object");
            let key = if dynamic_key { Call::new(global("key"), vec![]).into() } else { string("second") };
            let table = Table(vec![(Some(string("first")), Call::new(global("first"), vec![]).into()),
                (Some(key), Call::new(global("last"), vec![]).into())], Default::default(),
                crate::TableShape::NewTable { hash: 2, array: 2 });
            let mut block = Block(vec![declare(&object, table.into()), Call::new(global("middle"), vec![]).into(),
                crate::SetList::new(object, 1, vec![number(1.0)], Some(Call::new(global("tail"), vec![]).into())).into()]);
            let pass = super::snapshot_open_list_constructors(&mut block).unwrap();
            assert_eq!(pass.introduced_locals, if dynamic_key { 3 } else { 2 });
            assert_eq!(pass.ledger.records.len(), pass.introduced_locals);
            let text = block.to_string();
            assert!(text.find("first()").unwrap() < text.find("last()").unwrap(), "{text}");
            assert!(text.find("last()").unwrap() < text.find("middle()").unwrap(), "{text}");
            assert!(text.find("middle()").unwrap() < text.find("tail()").unwrap(), "{text}");
            for call in ["first()", "last()", "middle()", "tail()"] { assert_eq!(text.matches(call).count(), 1); }
            if dynamic_key { assert!(text.find("first()").unwrap() < text.find("key()").unwrap()); }
            assert_eq!(block.0.iter().filter(|statement| statement.as_assign()
                .is_some_and(|assign| assign.right.first().is_some_and(|value| value.as_table().is_some()))).count(),
                if dynamic_key { 2 } else { 1 });
            assert!(super::reserve_unfolded_table_slots(&mut block).is_ok());
            assert!(super::snapshot_open_list_constructors(&mut block).is_none());
        }
    }

    #[test]
    fn late_list_snapshots_keep_split_and_repeated_field_stores() {
        for empty in [false, true] {
            for dynamic in [false, true] {
                let object = local("object");
                let fields = if empty { vec![] } else {
                    vec![(Some(string("first")), Call::new(global("initial"), vec![]).into())]
                };
                let key = if dynamic { Call::new(global("key"), vec![]).into() } else { string("first") };
                let store = |key, value| Assign::new(
                    vec![crate::Index::new(object.clone().into(), key).into()], vec![value]).into();
                let mut block = Block(vec![declare(&object, Table(fields, Default::default(),
                    crate::TableShape::NewTable { hash: 4, array: 2 }).into()),
                    Call::new(global("before"), vec![]).into(),
                    store(key, Call::new(global("stored"), vec![]).into()),
                    store(string("first"), nil()),
                    Call::new(global("middle"), vec![]).into(),
                    crate::SetList::new(object.clone(), 1, vec![number(1.0)],
                        Some(Call::new(global("tail"), vec![]).into())).into()]);
                let pass = super::snapshot_open_list_constructors(&mut block).unwrap();
                assert_eq!(pass.introduced_locals, usize::from(!empty) + usize::from(dynamic) + 1);
                let text = block.to_string();
                let calls = ["before()", "stored()", "middle()", "tail()"];
                for call in calls { assert_eq!(text.matches(call).count(), 1, "{text}"); }
                for pair in calls.windows(2) {
                    assert!(text.find(pair[0]).unwrap() < text.find(pair[1]).unwrap(), "{text}");
                }
                if dynamic {
                    assert!(text.find("key()").unwrap() < text.find("stored()").unwrap());
                    assert!(text.find("object[constructorKey]").unwrap() < text.find("middle()").unwrap(), "{text}");
                }
                if !empty { assert!(text.find("initial()").unwrap() < text.find("before()").unwrap()); }
                let constructors: Vec<_> = block.0.iter().filter_map(|statement| statement.as_assign())
                    .filter_map(|assign| assign.right.first().and_then(RValue::as_table)).collect();
                assert_eq!(constructors.len(), if dynamic { 2 } else { 1 });
                let final_table = constructors.last().unwrap();
                assert_eq!(final_table.0.iter().filter(|(key, _)| key.is_some()).count(), if empty { 2 } else { 3 });
                assert!(super::reserve_unfolded_table_slots(&mut block).is_ok());
            }
        }
    }

    #[test]
    fn late_list_snapshots_read_owned_captured_keys_after_effectful_values() {
        for nested in [false, true] {
            for split in [false, true] {
                for value in [number(9.0), local("unchanged").into(),
                    Call::new(global("change"), vec![]).into(), global("lookup")]
                {
                    let key = local("key");
                    let object = local("object");
                    let pure = crate::is_total_pure(&value);
                    let literal = matches!(value, RValue::Literal(_));
                    let fields = if split { vec![] } else { vec![(Some(key.clone().into()), value.clone())] };
                    let mut block = Block(vec![declare(&local("change"), closure_capturing(&key)),
                        declare(&object, Table(fields, Default::default(),
                            crate::TableShape::NewTable { hash: 1, array: 1 }).into())]);
                    if split { block.0.push(assign_field(&object, key.clone().into(), value)); }
                    block.0.push(Call::new(global("middle"), vec![]).into());
                    block.0.push(crate::SetList::new(object, 1, vec![],
                        Some(Call::new(global("tail"), vec![]).into())).into());
                    if nested {
                        block = Block(vec![declare(&local("build"), Closure {
                            node_origin: Default::default(),
                            function: ByAddress(Arc::new(Mutex::new(Function {
                                parameters: vec![key], body: block, ..Default::default()
                            }))), upvalues: vec![],
                        }.into())]);
                    }
                    let pass = super::snapshot_open_list_constructors(&mut block).unwrap();
                    assert_eq!(pass.introduced_locals, if literal { 1 } else { 2 });
                    assert_eq!(pass.ledger.records.len(), pass.introduced_locals);
                    let text = block.to_string();
                    if !literal {
                        assert_eq!(text.find("local constructorKey = key").unwrap()
                            < text.find("local constructorValue = ").unwrap(), pure, "{text}");
                    }
                    assert!(super::reserve_unfolded_table_slots(&mut block).is_ok(), "{text}");
                }
            }
        }
    }

    #[test]
    fn late_list_snapshots_read_incoming_keys_before_effectful_values() {
        for copy in [false, true] {
            for grandchild in [false, true] {
                for split in [false, true] {
                    for value in [Call::new(global("change"), vec![]).into(), global("lookup")] {
                        let key = local("key");
                        let object = local("object");
                        let fields = if split { vec![] } else { vec![(Some(key.clone().into()), value.clone())] };
                        let mut body = Block(vec![declare(&object, Table(fields, Default::default(),
                            crate::TableShape::NewTable { hash: 1, array: 1 }).into())]);
                        if split { body.0.push(assign_field(&object, key.clone().into(), value)); }
                        body.0.push(Call::new(global("middle"), vec![]).into());
                        body.0.push(crate::SetList::new(object, 1, vec![],
                            Some(Call::new(global("tail"), vec![]).into())).into());
                        let capture = if copy { Upvalue::Copy(key) } else { Upvalue::Ref(key) };
                        let mut block = Block(vec![declare(&local("build"), Closure {
                            node_origin: Default::default(),
                            function: ByAddress(Arc::new(Mutex::new(Function { body, ..Default::default() }))),
                            upvalues: vec![capture.clone()],
                        }.into())]);
                        if grandchild {
                            block = Block(vec![declare(&local("outer"), Closure {
                                node_origin: Default::default(),
                                function: ByAddress(Arc::new(Mutex::new(Function { body: block, ..Default::default() }))),
                                upvalues: vec![capture],
                            }.into())]);
                        }
                        let pass = super::snapshot_open_list_constructors(&mut block).unwrap();
                        assert_eq!(pass.introduced_locals, 2);
                        assert_eq!(pass.ledger.records.len(), 2);
                        let text = block.to_string();
                        assert!(text.find("local constructorKey = key").unwrap()
                            < text.find("local constructorValue = ").unwrap(), "{text}");
                        assert!(super::reserve_unfolded_table_slots(&mut block).is_ok(), "{text}");
                    }
                }
            }
        }
    }

    #[test]
    fn late_list_snapshots_refuse_ambiguous_captured_keys() {
        for reverse in [false, true] {
            for split in [false, true] {
                for value in [Call::new(global("change"), vec![]).into(), global("lookup")] {
                    let key = local("key");
                    let object = local("object");
                    let fields = if split { vec![] } else { vec![(Some(key.clone().into()), value.clone())] };
                    let mut body = Block(vec![declare(&object, Table(fields, Default::default(),
                        crate::TableShape::NewTable { hash: 1, array: 1 }).into())]);
                    if split { body.0.push(assign_field(&object, key.clone().into(), value)); }
                    body.0.push(Call::new(global("middle"), vec![]).into());
                    body.0.push(crate::SetList::new(object, 1, vec![],
                        Some(Call::new(global("tail"), vec![]).into())).into());
                    let function = ByAddress(Arc::new(Mutex::new(Function { body, ..Default::default() })));
                    let closure = Closure { node_origin: Default::default(), function,
                        upvalues: vec![Upvalue::Ref(key)] };
                    // One occurrence supplies captures absent from the other shared body.
                    let mut first = closure.clone();
                    first.upvalues.clear();
                    let mut block = Block(vec![declare(&local("first"), first.into()),
                        declare(&local("build"), closure.into())]);
                    if reverse { block.0.reverse(); }
                    let before = block.to_string();
                    assert!(super::snapshot_open_list_constructors(&mut block).is_none());
                    assert_eq!(block.to_string(), before);
                }
            }
        }
    }

    #[test]
    fn late_list_snapshots_check_initial_keys_before_later_effects() {
        for later in [Call::new(global("effect"), vec![]).into(), global("observable_lookup")] {
            let object = local("object");
            let shape = crate::TableShape::NewTable { hash: 2, array: 2 };
            let fields = vec![(Some(Call::new(global("key"), vec![]).into()), Call::new(global("first"), vec![]).into()),
                (Some(string("second")), later.clone())];
            let mut block = Block(vec![declare(&object, Table(fields, Default::default(), shape).into()),
                Call::new(global("middle"), vec![]).into(),
                crate::SetList::new(object, 1, vec![number(1.0)], Some(Call::new(global("tail"), vec![]).into())).into()]);
            let pass = super::snapshot_open_list_constructors(&mut block).unwrap();
            assert_eq!(pass.introduced_locals, 3);
            assert_eq!(pass.ledger.records.len(), 3);
            let text = block.to_string();
            let later = later.to_string();
            let ordered = ["key()", "first()", "object[constructorKey] = constructorValue", &later, "middle()", "tail()"];
            for pair in ordered.windows(2) {
                assert!(text.find(pair[0]).unwrap() < text.find(pair[1]).unwrap(), "{text}");
            }
            for expression in ["key()", "first()", &later, "middle()", "tail()"] {
                assert_eq!(text.matches(expression).count(), 1, "{text}");
            }
            let tables: Vec<_> = block.0.iter().filter_map(|statement| statement.as_assign())
                .filter_map(|assign| assign.right.first().and_then(RValue::as_table)).collect();
            assert_eq!(tables.len(), 2);
            assert!(tables[0].0.is_empty());
            assert_eq!(tables[1].2, shape);
            assert!(super::reserve_unfolded_table_slots(&mut block).is_ok());
            assert!(super::snapshot_open_list_constructors(&mut block).is_none());
        }
    }

    #[test]
    fn late_list_snapshots_keep_positional_prefixes_and_static_allocation() {
        for call_tail in [false, true] {
            for keyed_first in [false, true] {
                for dynamic_key in [false, true] {
                    for split in [false, true] {
                        let object = local("object");
                        let key = if dynamic_key { Call::new(global("key"), vec![]).into() } else { string("key") };
                        let mut fields = vec![(None, super::single_value(Call::new(global("first"), vec![]).into())),
                            (Some(key), Call::new(global("keyed"), vec![]).into())];
                        if keyed_first { fields.swap(0, 1); }
                        let shape = crate::TableShape::NewTable { hash: if split { 2 } else { 1 },
                            array: if call_tail { 3 } else { 2 } };
                        let mut block = Block(vec![declare(&object, Table(fields, Default::default(), shape).into())]);
                        if split { block.0.push(assign_field(&object, string("added"), Call::new(global("split"), vec![]).into())); }
                        block.0.push(Call::new(global("middle"), vec![]).into());
                        let tail = if call_tail { Call::new(global("tail"), vec![]).into() } else { crate::VarArg {}.into() };
                        block.0.push(crate::SetList::new(object, 2, vec![number(1.0)], Some(tail)).into());
                        let pass = super::snapshot_open_list_constructors(&mut block).unwrap();
                        assert_eq!(pass.introduced_locals, 2 + usize::from(dynamic_key) + usize::from(split));
                        let text = block.to_string();
                        assert_eq!(text.matches("first()").count(), 1, "{text}");
                        assert_eq!(text.matches("keyed()").count(), 1, "{text}");
                        assert_eq!(text.find("keyed()").unwrap() < text.find("first()").unwrap(), keyed_first, "{text}");
                        assert!(text.find("first()").unwrap() < text.find("middle()").unwrap(), "{text}");
                        let final_table = block.0.last().unwrap().as_assign().unwrap().right[0].as_table().unwrap();
                        assert_eq!(final_table.2, shape);
                        assert_eq!(final_table.0[usize::from(keyed_first)].0, None);
                        assert_eq!(final_table.0.iter().filter(|(key, _)| key.is_some()).count(), 1 + usize::from(split));
                        assert!(super::reserve_unfolded_table_slots(&mut block).is_ok(), "{text}");
                    }
                }
            }
        }
    }

    #[test]
    fn late_list_snapshots_reject_positional_resize_and_open_prefixes() {
        for hazard in 0..5 {
            let object = local("object");
            let mut fields = vec![(Some(string("key")), number(1.0)), (None, number(2.0))];
            let mut array = 2;
            let mut index = 2;
            let mut tail = crate::VarArg {}.into();
            match hazard {
                0 => array = 1,
                1 => array = 3,
                2 => fields[1].1 = Call::new(global("many"), vec![]).into(),
                3 => tail = number(3.0),
                4 => index = 3,
                _ => unreachable!(),
            }
            let mut block = Block(vec![declare(&object, Table(fields, Default::default(),
                crate::TableShape::NewTable { hash: 1, array }).into()), Call::new(global("middle"), vec![]).into(),
                crate::SetList::new(object, index, vec![number(3.0)], Some(tail)).into()]);
            let before = block.to_string();
            assert!(super::snapshot_open_list_constructors(&mut block).is_none(), "hazard {hazard}");
            assert_eq!(block.to_string(), before);
        }
    }

    #[test]
    fn late_list_snapshots_refuse_self_reads_escape_and_capacity_changes() {
        for hazard in 0..8 {
            let object = local("object");
            let mut fields = vec![(Some(local("key").into()), number(9.0))];
            let mut middle: crate::Statement = Call::new(global("middle"), vec![]).into();
            let mut tail = Call::new(global("tail"), vec![]).into();
            let mut hash = 2;
            match hazard {
                0 => fields[0].0 = Some(Index::new(object.clone().into(), string("key")).into()),
                1 => middle = Call::new(global("observe"), vec![object.clone().into()]).into(),
                2 => fields.push((Some(string("second")), closure_capturing(&object))),
                3 => hash = 0,
                4 => tail = Call::new(global("observe"), vec![object.clone().into()]).into(),
                5 => middle = Assign::new(vec![crate::Index::new(object.clone().into(), string("second")).into()],
                    vec![Call::new(global("observe"), vec![object.clone().into()]).into()]).into(),
                6 => {
                    hash = 1;
                    middle = Assign::new(vec![crate::Index::new(object.clone().into(), string("second")).into()],
                        vec![number(1.0)]).into();
                }
                7 => {
                    fields.clear();
                    hash = 0;
                    middle = Assign::new(vec![crate::Index::new(object.clone().into(), number(1.0)).into()],
                        vec![number(1.0)]).into();
                }
                _ => unreachable!(),
            }
            let mut block = Block(vec![declare(&object, Table(fields, Default::default(),
                crate::TableShape::NewTable { hash, array: 2 }).into()), middle,
                crate::SetList::new(object, 1, vec![number(1.0)], Some(tail)).into()]);
            let before = block.to_string();
            assert!(super::snapshot_open_list_constructors(&mut block).is_none(), "hazard {hazard}");
            assert_eq!(block.to_string(), before);
        }
    }

    #[test]
    fn repeated_keys_preserve_newtable_nil_history_in_both_paths() {
        for first_value in [nil(), number(0.0)] {
            let original = Table(vec![(Some(string("first")), first_value.clone()),
                (Some(string("second")), number(2.0)), (Some(string("first")), number(1.0))],
                Default::default(), crate::TableShape::NewTable { hash: 4, array: 0 });
            let mut indexed = original.clone();
            let mut scanning = original.clone();
            let changed = super::merge_repeated_keys_indexed(&mut indexed, super::literal_fingerprint);
            assert_eq!(changed, super::merge_repeated_keys_scanning(&mut scanning));
            assert_eq!(indexed, scanning);
            assert_eq!(changed, first_value != nil());
            if !changed { assert_eq!(indexed, original); }
            super::reserve_allocation_slots(&mut indexed).unwrap();
            assert_eq!(indexed.0.len() + indexed.hash_padding().unwrap().0, 3);
        }
    }

    #[test]
    fn indexed_repeated_keys_match_original_with_collisions_and_barriers() {
        let read = local("captured");
        let keys = [
            Some(string("a")), Some(string("b")), Some(string("c")),
            Some(number(0.0)), Some(number(-0.0)),
            Some(number(f64::from_bits(0x7ff8_0000_0000_0001))),
            Some(number(f64::from_bits(0x7ff8_0000_0000_0002))),
            Some(Literal::Boolean(true).into()), Some(Literal::Integer(0).into()),
            Some(Literal::Vector(0.0, -0.0, 1.0).into()),
            Some(Literal::VectorD(-0.0, 0.0, f64::NAN).into()),
            Some(nil()), Some(local_value(&read)), None,
        ];
        for seed in 0..2_048u64 {
            let mut random = seed + 1;
            let mut entries = Vec::new();
            for index in 0..(16 + seed as usize % 113) {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let key = keys[(random >> 32) as usize % keys.len()].clone();
                let value = match (random >> 16) % 6 {
                    0..=2 => number(index as f64),
                    3 => local_value(&read),
                    4 => Call::new(global("effect"), vec![number(index as f64)]).into(),
                    _ => Table::new(vec![(Some(local_value(&read)), nil())]).into(),
                };
                entries.push((key, value));
            }
            let original = Table::new(entries);
            let mut actual = original.clone();
            let mut expected = original.clone();
            let changed = super::merge_repeated_keys_indexed(&mut actual, |literal| {
                // Force all kinds into one bucket to exercise equality checks.
                if seed % 4 == 0 { 0 } else { super::literal_fingerprint(literal) }
            });
            let reference_changed = super::merge_repeated_keys_scanning(&mut expected);
            assert_eq!(changed, reference_changed, "seed={seed}");
            assert_eq!(actual, expected, "seed={seed}");
        }
    }

    #[test]
    fn indexed_repeated_keys_preserve_first_occurrence_float_bits_and_origins() {
        let mut effect = Call::new(global("effect"), vec![]);
        effect.node_origin = crate::node_origins::Origin::input(crate::node_origins::Input {
            function: "repeated_keys".into(), block: 2, statement: 7, value: Some(1),
        });
        let mut table = Table::new(vec![
            (Some(number(0.0)), number(1.0)),
            (Some(number(-0.0)), number(2.0)),
            (Some(number(0.0)), number(3.0)),
            (Some(string("effect")), nil()),
            (Some(string("effect")), effect.into()),
            // The first occurrence now holds an effect, so the later literal
            // remains even though a newer duplicate could absorb it.
            (Some(string("effect")), number(4.0)),
            (Some(string("effect")), number(5.0)),
        ]);
        assert!(super::merge_repeated_keys_indexed(&mut table, super::literal_fingerprint));
        assert_eq!(table.0.len(), 6);
        assert_eq!(table.0[0], (Some(number(0.0)), number(1.0)));
        assert_eq!(table.0[1], (Some(number(-0.0)), number(2.0)));
        assert_eq!(table.0[2], (Some(number(0.0)), number(3.0)));
        let origin = crate::node_origins::value(&table.0[3].1).unwrap().0.as_ref().unwrap();
        assert_eq!(origin.inputs[0].statement, 7);
        assert!(!origin.cloned && !origin.inlined);
        assert_eq!(table.0[4].1, number(4.0));
        assert_eq!(table.0[5].1, number(5.0));
    }

    #[test]
    fn repeated_keys_keep_numeric_aliases_and_invalid_key_errors_in_place() {
        for key in [number(1.0), number(f64::NAN), nil(), Literal::Vector(-0.0, 0.0, 1.0).into()] {
            let original = Table::new(vec![
                (Some(key.clone()), number(0.0)),
                (None, number(9.0)),
                (Some(key), Call::new(global("effect"), vec![]).into()),
            ]);
            let mut actual = original.clone();
            assert!(!super::merge_repeated_keys_indexed(&mut actual, super::literal_fingerprint));
            assert_eq!(actual, original);
            assert!(!super::merge_repeated_keys_scanning(&mut actual));
            assert_eq!(actual, original);
        }
    }

    #[test]
    fn repeated_key_lookup_is_linear_for_large_unique_and_duplicate_tables() {
        for size in [4_000, 8_000] {
            for duplicate in [false, true] {
                let mut table = Table::new((0..size).map(|index| {
                    (Some(string(&format!("field{}", if duplicate { index / 2 } else { index }))), nil())
                }).collect());
                MERGE_KEY_COMPARISONS.with(|count| count.set(0));
                let changed = super::merge_repeated_keys_in(&mut table);
                let comparisons = MERGE_KEY_COMPARISONS.with(std::cell::Cell::get);
                assert_eq!(changed, duplicate);
                assert_eq!(table.0.len(), if duplicate { size / 2 } else { size });
                assert!(comparisons <= size, "{size} entries used {comparisons} key comparisons");
            }
        }
    }

    #[test]
    #[ignore = "manual release scaling benchmark"]
    fn benchmark_repeated_key_scaling() {
        for size in [1_000, 2_000, 4_000, 8_000] {
            for duplicate in [false, true] {
                let original = Table::new((0..size).map(|index| {
                    (Some(string(&format!("field{}", if duplicate { index / 2 } else { index }))), nil())
                }).collect());
                let mut actual = original.clone();
                let start = std::time::Instant::now();
                super::merge_repeated_keys_in(&mut actual);
                let indexed = start.elapsed();
                let mut reference = original.clone();
                let start = std::time::Instant::now();
                super::merge_repeated_keys_scanning(&mut reference);
                let scanning = start.elapsed();
                assert_eq!(actual, reference);
                eprintln!("repeated_keys size={size} duplicate={duplicate} indexed_us={} scanning_us={}",
                    indexed.as_micros(), scanning.as_micros());
            }
        }
    }

    #[test]
    fn indexed_constructor_runs_match_original_motion_and_barriers() {
        for seed in 0..512u64 {
            let locals: Vec<_> = (0..12).map(|i| local(&format!("local{i}"))).collect();
            let mut random = seed + 1;
            let mut statements = Vec::new();
            for index in 0..160 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let target = &locals[(random >> 32) as usize % locals.len()];
                let source = &locals[(random >> 16) as usize % locals.len()];
                let value = if random & 1 == 0 { number(index as f64) } else { local_value(source) };
                statements.push(match random % 12 {
                    0..=3 => declare(target, Table::new(vec![(Some(string("initial")), value)]).into()),
                    4..=6 => declare(target, value),
                    7 => declare(target, Call::new(global("effect"), vec![value]).into()),
                    8 => assign_field(target, string("field"), value),
                    9 => print(value),
                    10 => crate::Empty {}.into(),
                    _ => Assign::new(vec![target.clone().into()], vec![value]).into(),
                });
            }
            let captured = locals.iter().enumerate().filter(|(index, _)| (*index + seed as usize) % 7 == 0)
                .map(|(_, local)| local.clone()).collect();
            let mut expected = Block(statements.clone());
            let mut actual = Block(statements);
            assert_eq!(super::sink_total_table_declarations(&mut actual, &captured),
                super::sink_total_table_declarations_reference(&mut expected, &captured));
            assert_eq!(actual, expected, "seed {seed}");
        }
    }

    #[test]
    fn constructor_run_index_survives_successful_moves_and_long_irrelevant_runs() {
        let mut statements = Vec::new();
        for group in 0..4 {
            let object = local(&format!("object{group}"));
            statements.push(declare(&object, Table::default().into()));
            for index in 0..256 {
                statements.push(declare(&local(&format!("other{group}_{index}")), Table::default().into()));
            }
            statements.push(assign_field(&object, string("field"), number(group as f64)));
        }
        for index in 0..256 {
            statements.push(declare(&local(&format!("unused{index}")), Table::default().into()));
        }
        let mut expected = Block(statements.clone());
        let mut actual = Block(statements);
        assert!(super::sink_total_table_declarations(&mut actual, &Default::default()));
        assert!(super::sink_total_table_declarations_reference(&mut expected, &Default::default()));
        assert_eq!(actual, expected);
    }

    /// The general (scanning) placement, applied when it accepts the store.
    fn general_insert(table: &mut Table, initial_len: usize, key: RValue, value: RValue) -> bool {
        let mut listed = crate::ListedKeys::new(table);
        match super::placement(table, initial_len, &key, &listed, false) {
            Some(placement) => {
                super::insert_table_entry(table, placement, key, value, &mut listed);
                true
            }
            None => false,
        }
    }

    #[test]
    fn indexed_placeholders_preserve_the_moving_prefix_and_evaluation_order() {
        for seed in 0..256usize {
            let initial_len = 16 + seed % 33;
            let initial = Table((0..initial_len).map(|index| {
                // Include duplicate template keys: the last matching live
                // entry in the window must stay the selected one (it is the
                // one a later store overwrites) even after removals.
                (Some(string(&format!("field{}", index % (initial_len - seed % 7)))), nil())
            }).collect(), Default::default(), Default::default());
            let mut expected = initial.clone();
            let mut actual = initial;
            let mut index = super::PlaceholderEntries::new(&actual).unwrap();
            let mut state = seed as u64 + 1;
            for step in 0..128 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let key = string(&format!("field{}", (state >> 32) as usize % (initial_len + 8)));
                let value = match step % 5 {
                    0 | 1 => nil(),
                    2 => Call::new(global("effect"), vec![number(step as f64)]).into(),
                    _ => number(step as f64),
                };
                let placement = index.placement(&actual, &key);
                let accepted = general_insert(&mut expected, initial_len, key.clone(), value.clone());
                assert_eq!(placement.is_some(), accepted, "seed={seed}, step={step}");
                if let Some(placement) = placement {
                    index.insert(&mut actual, placement, key, value);
                }
                let mut compacted = actual.clone();
                index.clone().finish(&mut compacted);
                assert_eq!(compacted, expected, "seed={seed}, step={step}");
            }
        }
    }

    #[test]
    fn indexed_placeholders_flush_before_dynamic_keys_and_setlist() {
        for with_setlist in [false, true] {
            let object = local("record");
            let initial = Table((0..32).map(|i| (Some(string(&format!("f{i}"))), nil())).collect(), Default::default(), Default::default());
            let mut expected = initial.clone();
            let mut statements = vec![declare(&object, initial.into())];
            for i in (0..32).rev() {
                let key = string(&format!("f{i}"));
                let value: RValue = Call::new(global("effect"), vec![number(i as f64)]).into();
                assert!(general_insert(&mut expected, 32, key.clone(), value.clone()));
                statements.push(assign_field(&object, key, value));
            }
            if with_setlist {
                let list = crate::SetList { object_local: object.clone(), index: 1,
                    values: vec![number(7.0)], tail: None, node_origin: Default::default() };
                statements.push(list.into());
                expected.0.push((None, number(7.0)));
            } else {
                let key: RValue = Call::new(global("key"), vec![]).into();
                assert!(general_insert(&mut expected, 32, key.clone(), number(7.0)));
                statements.push(assign_field(&object, key, number(7.0)));
            }
            statements.push(Return::new(vec![object.into()]).into());
            let mut block = Block(statements);
            assert!(rebuild_table_literals(&mut block));
            assert_eq!(block[0].as_assign().unwrap().right[0].as_table().unwrap(), &expected);
            assert_eq!(block.len(), 2);
        }
    }

    fn closure_capturing(local: &RcLocal) -> RValue {
        RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(local.clone())],
        })
    }

    #[test]
    fn compacts_multiple_constructors_without_reordering_effects() {
        let first = local("first");
        let second = local("second");
        let call = |name: &str| Call::new(global(name), vec![]).into();
        let mut block = Block(vec![
            declare(&first, Table::default().into()),
            assign_field(&first, string("a"), call("a")),
            assign_field(&first, string("b"), call("b")),
            print(local_value(&first)),
            declare(&second, Table::default().into()),
            assign_field(&second, string("c"), call("c")),
            assign_field(&second, string("self"), local_value(&second)),
            assign_field(&second, string("d"), call("d")),
            Return::new(vec![local_value(&first), local_value(&second)]).into(),
        ]);
        assert!(rebuild_table_literals(&mut block));
        let expected = Block(vec![
            declare(&first, Table::new(vec![
                (Some(string("a")), call("a")), (Some(string("b")), call("b")),
            ]).into()),
            print(local_value(&first)),
            declare(&second, Table::new(vec![(Some(string("c")), call("c"))]).into()),
            assign_field(&second, string("self"), local_value(&second)),
            assign_field(&second, string("d"), call("d")),
            Return::new(vec![local_value(&first), local_value(&second)]).into(),
        ]);
        assert_eq!(block, expected);
        assert!(!rebuild_table_literals(&mut block));
        assert_eq!(block, expected);
    }

    #[test]
    fn many_setlist_batches_keep_array_indices_and_keyed_overwrites() {
        let object = local("values");
        let mut block = Block(vec![declare(&object, Table::default().into())]);
        let n = 2048;
        for index in 1..=n {
            block.push(crate::SetList::new(object.clone(), index, vec![number(index as f64)], None).into());
            if index % 127 == 0 {
                block.push(assign_field(&object, number(index as f64), number(-1.0)));
            }
        }
        block.push(Return::new(vec![local_value(&object)]).into());
        assert!(rebuild_table_literals(&mut block));
        assert_eq!(block.len(), 2);
        let table = block[0].as_assign().unwrap().right[0].as_table().unwrap();
        let mut entries = table.0.iter();
        for index in 1..=n {
            assert_eq!(entries.next(), Some(&(None, number(index as f64))));
            if index % 127 == 0 {
                assert_eq!(entries.next(), Some(&(Some(number(index as f64)), number(-1.0))));
            }
        }
        assert!(entries.next().is_none());
    }

    #[test]
    fn appends_setlist_with_fixed_single_value_and_expanding_tail() {
        let object = local("children");
        let mut block = Block(vec![
            declare(
                &object,
                Table::new(vec![
                    (None, number(1.0)),
                    (Some(string("Header")), number(9.0)),
                ])
                .into(),
            ),
            crate::SetList::new(
                object.clone(),
                2,
                vec![Call::new(global("fixed"), vec![]).into()],
                Some(Call::new(global("tail"), vec![]).into()),
            )
            .into(),
            Return::new(vec![local_value(&object)]).into(),
        ]);
        assert!(rebuild_table_literals(&mut block));
        assert_eq!(block.0.len(), 2);
        let table = block.0[0].as_assign().unwrap().right[0].as_table().unwrap();
        assert!(matches!(
            &table.0[2].1,
            RValue::Select(crate::Select::Call(_))
        ));
        assert!(matches!(&table.0[3].1, RValue::Call(_)));
    }

    #[test]
    fn setlist_requires_valid_slots_and_no_capture_during_initialization() {
        for kind in 0..6 {
            let object = local("children");
            let entry = match kind {
                0 => (None, Call::new(global("old_tail"), vec![]).into()),
                1 => (Some(number(1.0)), number(8.0)),
                2 => (Some(local_value(&local("key"))), number(8.0)),
                _ => (None, number(1.0)),
            };
            let mut block = Block(vec![
                declare(&object, Table::new(vec![entry]).into()),
                crate::SetList::new(
                    object.clone(),
                    if kind == 3 { 4 } else { 2 },
                    vec![],
                    Some(if kind == 4 {
                        closure_capturing(&object)
                    } else {
                        Call::new(global("tail"), vec![]).into()
                    }),
                )
                .into(),
                Return::new(vec![if kind == 5 {
                    closure_capturing(&object)
                } else {
                    local_value(&object)
                }])
                .into(),
            ]);
            let before = block.to_string();
            rebuild_table_literals(&mut block);
            if kind == 5 {
                // This closure is only created after the complete constructor.
                assert_eq!(block.0.len(), 2);
                let table = block.0[0].as_assign().unwrap().right[0].as_table().unwrap();
                assert_eq!(table.0.len(), 2);
                assert!(matches!(&table.0[1].1, RValue::Call(_)));
            } else { assert_eq!(block.to_string(), before, "case {kind}"); }
        }
    }

    #[test]
    fn folds_fields_only_before_the_first_capture_or_alias_observation() {
        for barrier in 0..5 {
            let object = local("components");
            let mut block = Block(vec![]);
            if barrier == 1 { block.0.push(print(closure_capturing(&object))); }
            block.0.push(declare(&object, Table::default().into()));
            if barrier == 2 { block.0.push(print(closure_capturing(&object))); }
            if barrier == 3 { block.0.push(declare(&local("alias"), local_value(&object))); }
            block.0.push(assign_field(&object, string("Widget"), Call::new(global("make"), vec![]).into()));
            if barrier == 4 { block.0.push(Assign::new(vec![object.clone().into()], vec![global("replacement")]).into()); }
            block.0.push(Return::new(vec![closure_capturing(&object)]).into());
            let before = block.to_string();
            assert_eq!(rebuild_table_literals(&mut block), barrier == 0);
            if barrier == 0 {
                assert_eq!(block.0.len(), 2);
                assert_eq!(block.0[0].as_assign().unwrap().right[0].as_table().unwrap().0.len(), 1);
            } else { assert_eq!(block.to_string(), before, "barrier {barrier}"); }
        }
    }

    #[test]
    fn captured_single_callback_field_keeps_layout_until_another_field_joins() {
        for multiple in [false, true] {
            let object = local("settings");
            let callback_dependency = local("key");
            let mut block = Block(vec![
                declare(&object, Table::default().into()),
                assign_field(&object, string("OnChanged"), closure_capturing(&callback_dependency)),
            ]);
            if multiple { block.0.push(assign_field(&object, string("Enabled"), number(1.0))); }
            block.0.push(Return::new(vec![closure_capturing(&object)]).into());
            let before = block.to_string();
            assert_eq!(rebuild_table_literals(&mut block), multiple);
            if multiple { assert_eq!(block.0.len(), 2); }
            else { assert_eq!(block.to_string(), before); }
        }
    }

    #[test]
    fn rebuilds_contiguous_string_field_assignments() {
        let props = local("props");
        let layout_order = local("layoutOrder");
        let mut block = Block(vec![
            declare(&props, RValue::Table(Table::default())),
            assign_field(&props, string("Name"), string("TextButton")),
            assign_field(&props, string("LayoutOrder"), local_value(&layout_order)),
            Return::new(vec![local_value(&props)]).into(),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local props = { Name = \"TextButton\", LayoutOrder = layoutOrder }\nreturn props"
        );
    }

    #[test]
    fn rebuilds_dynamic_effect_free_keys() {
        let table = local("table");
        let key = local("key");
        let value = local("value");
        let mut block = Block(vec![
            declare(&table, RValue::Table(Table::default())),
            assign_field(&table, local_value(&key), local_value(&value)),
            Return::new(vec![local_value(&table)]).into(),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local table = { [key] = value }\nreturn table"
        );
    }

    #[test]
    fn rebuilds_stable_index_chain_key_and_inlines_callback_table() {
        let props = local("v3");
        let react = local("react");
        let create_element = local("createElement");
        let event_key = RValue::Index(Index::new(
            RValue::Index(Index::new(local_value(&react), string("Event"))),
            string("Activated"),
        ));
        let callback = RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![],
        });
        let mut block = Block(vec![
            declare(
                &props,
                RValue::Table(Table::new(vec![(Some(string("Name")), string("Button"))])),
            ),
            assign_field(&props, event_key, callback),
            Return::new(vec![Call::new(
                local_value(&create_element),
                vec![string("TextButton"), local_value(&props)],
            )
            .into()])
            .into(),
        ]);

        rebuild_table_literals(&mut block);
        crate::inline_temps::inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 1);
        let output = block.to_string();
        assert!(output.starts_with("return createElement(\"TextButton\", {"));
        assert!(
            output.contains("[react.Event.Activated] = function()"),
            "{output}"
        );
    }

    #[test]
    fn leaf_to_root_fixpoint_rebuilds_three_level_table_tree() {
        let outer = local("v3");
        let middle = local("v4");
        let leaf = local("v5");
        let mut block = Block(vec![
            declare(&outer, RValue::Table(Table::default())),
            declare(&middle, RValue::Table(Table::default())),
            declare(&leaf, RValue::Table(Table::default())),
            assign_field(&leaf, string("Name"), string("Leaf")),
            assign_field(&middle, string("Leaf"), local_value(&leaf)),
            assign_field(&outer, string("Middle"), local_value(&middle)),
            Return::new(vec![local_value(&outer)]).into(),
        ]);

        loop {
            let rebuilt = rebuild_table_literals(&mut block);
            let inlined = crate::inline_temps::inline_single_use_temps(&mut block);
            if !rebuilt && !inlined {
                break;
            }
        }

        assert_eq!(block.0.len(), 1);
        assert_eq!(
            block.to_string(),
            "return {\n\tMiddle = {\n\t\tLeaf = { Name = \"Leaf\" }\n\t}\n}"
        );
        assert!(!crate::inline_temps::rebuild_ui_expression_trees(
            &mut block
        ));
    }

    #[test]
    fn drained_last_field_rejoins_props_and_children_arguments() {
        let props = local("v7");
        let children = local("children");
        let create_element = local("createElement");
        let mut block = Block(vec![
            declare(
                &props,
                RValue::Table(Table::new(vec![
                    (Some(string("Name")), string("Panel")),
                    (
                        Some(string("children")),
                        RValue::Table(Table::new(vec![(Some(string("Label")), string("Child"))])),
                    ),
                ])),
            ),
            declare(
                &children,
                RValue::Index(Index::new(local_value(&props), string("children"))),
            ),
            assign_field(&props, string("children"), nil()),
            Return::new(vec![Call::new(
                local_value(&create_element),
                vec![string("Frame"), local_value(&props), local_value(&children)],
            )
            .into()])
            .into(),
        ]);

        loop {
            let rebuilt = rebuild_table_literals(&mut block);
            let inlined = crate::inline_temps::inline_single_use_temps(&mut block);
            if !rebuilt && !inlined {
                break;
            }
        }

        assert_eq!(
            block.to_string(),
            "return createElement(\"Frame\", { Name = \"Panel\" }, { Label = \"Child\" })"
        );
    }

    #[test]
    fn sinks_total_constructor_past_local_call_then_rebuilds_return() {
        let entry = local("v6");
        let model = local("spinModel");
        let center = local("modelCenter");
        let parts = local("parts");
        let mut block = Block(vec![
            declare(
                &entry,
                RValue::Table(Table::new(vec![(Some(string("Model")), local_value(&model))])),
            ),
            declare(
                &center,
                Call::new(global("getModelCenter"), vec![local_value(&model)]).into(),
            ),
            Comment::trailing("inlined helper".into()).into(),
            assign_field(&entry, string("Center"), local_value(&center)),
            assign_field(&entry, string("Parts"), local_value(&parts)),
            Return::new(vec![local_value(&entry)]).into(),
        ]);

        crate::inline_temps::rebuild_ui_expression_trees(&mut block);

        assert_eq!(
            block.to_string(),
            "local modelCenter = getModelCenter(spinModel) -- inlined helper\nreturn { Model = spinModel, Center = modelCenter, Parts = parts }"
        );
    }

    #[test]
    fn rebuilds_children_after_selected_local_and_interleaved_store() {
        let children = local("v2");
        let selected = local("selected");
        let props = local("props");
        let mut block = Block(vec![
            declare(&children, Table::default().into()),
            declare(&selected, nil()),
            If::new(global("condition"), Block(vec![
                Assign::new(vec![selected.clone().into()], vec![Call::new(global("choose"), vec![]).into()]).into(),
            ]), Block::default()).into(),
            assign_field(&props, string("Selected"), local_value(&selected)),
            crate::SetList::new(children.clone(), 1, vec![local_value(&selected)],
                Some(Call::new(global("tail"), vec![]).into())).into(),
            Return::new(vec![local_value(&children)]).into(),
        ]);
        assert!(rebuild_table_literals(&mut block));
        let text = block.to_string();
        assert!(text.find("if condition").unwrap() < text.find("props.Selected").unwrap());
        assert!(text.find("props.Selected").unwrap() < text.find("local v2 =").unwrap());
        assert!(text.contains("{ selected, tail() }"), "{text}");
        assert!(!text.contains("table.pack"));
        assert!(!rebuild_table_literals(&mut block));
    }

    #[test]
    fn constructor_region_refuses_observation_and_dependency_writes_in_either_arm() {
        for kind in 0..9 {
            let object = local("v2");
            let dependency = local("dependency");
            let receiver = local("receiver");
            let hazard = match kind {
                0 => print(local_value(&object)),
                1 => Assign::new(vec![object.clone().into()], vec![Table::default().into()]).into(),
                2 => Assign::new(vec![dependency.clone().into()], vec![number(9.0)]).into(),
                3 => assign_field(&receiver, local_value(&object), number(1.0)),
                4 => Assign::new(vec![Index::new(Index::new(local_value(&object), string("child")).into(),
                    string("value")).into()], vec![number(1.0)]).into(),
                5 => declare(&receiver, closure_capturing(&object)),
                6 => Return::new(vec![]).into(),
                7 => crate::Close { locals: vec![dependency.clone()] }.into(),
                _ => print(closure_capturing(&dependency)),
            };
            let mut block = Block(vec![
                declare(&object, Table::new(vec![(Some(string("Snapshot")), local_value(&dependency))]).into()),
                If::new(global("condition"), Block::default(), Block(vec![hazard])).into(),
                crate::SetList::new(object.clone(), 1, vec![number(4.0)], None).into(),
                Return::new(vec![local_value(&object)]).into(),
            ]);
            let before = block.to_string();
            rebuild_table_literals(&mut block);
            assert_eq!(block.to_string(), before, "hazard {kind}");
        }
    }

    #[test]
    fn constructor_region_keeps_recorded_binding_initialization_and_budget_fallback() {
        for kind in 0..8 {
            let object = local("children");
            let dependency = local("dependency");
            if kind == 0 {
                object.0.lock().add_source_binding(crate::SourceBinding {
                    name: "children".into(), origin: crate::BindingOrigin::DebugLocal {
                        prototype: 0, register: 1, start_pc: 0, end_pc: 20,
                    },
                });
            }
            let initializer = match kind {
                1 => Table::new(vec![(Some(local_value(&dependency)), number(1.0))]),
                2 => Table::new(vec![(Some(number(f64::NAN)), number(1.0))]),
                3 => Table::new(vec![(Some(string("Value")), Call::new(global("before"), vec![]).into())]),
                4 => Table::new(vec![(None, crate::VarArg.into())]),
                5 => Table::new(vec![(Some(string("Callback")), closure_capturing(&dependency))]),
                _ => Table::default(),
            };
            let mut region = vec![print(string("work"))];
            if kind == 6 { region = vec![print(string("work")); 64]; }
            if kind == 7 {
                for _ in 0..34 { region = vec![If::new(global("condition"), Block(region), Block::default()).into()]; }
            }
            let mut block = Block(vec![declare(&object, initializer.into())]);
            block.0.extend(region);
            block.0.extend([
                crate::SetList::new(object.clone(), 1, vec![number(4.0)], None).into(),
                Return::new(vec![local_value(&object)]).into(),
            ]);
            let before = block.to_string();
            rebuild_table_literals(&mut block);
            assert_eq!(block.to_string(), before, "refusal {kind}");
        }
    }

    #[test]
    fn constructor_region_retains_lhs_and_callback_evaluations_before_list() {
        let object = local("children");
        let receiver = local("receiver");
        let field = Assign::new(vec![Index::new(
            Call::new(global("base"), vec![]).into(),
            Call::new(global("key"), vec![]).into()).into()],
            vec![Call::new(global("value"), vec![]).into()]);
        let mut block = Block(vec![
            declare(&object, Table::new(vec![(Some(number(1.0)), number(99.0))]).into()),
            print(closure_capturing(&receiver)),
            field.into(),
            crate::SetList::new(object.clone(), 1, vec![nil()],
                Some(Call::new(global("tail"), vec![]).into())).into(),
            Return::new(vec![local_value(&object)]).into(),
        ]);
        let prefix = block.0[1..3].iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(rebuild_table_literals(&mut block));
        assert_eq!(block.0[..2].iter().map(ToString::to_string).collect::<Vec<_>>(), prefix);
        let table = block.0[2].as_assign().unwrap().right[0].as_table().unwrap();
        assert_eq!(table.0.len(), 3);
        assert_eq!(table.0[0], (Some(number(1.0)), number(99.0)));
        assert_eq!(table.0[1], (None, nil()));
        assert!(matches!(table.0[2].1, RValue::Call(_)));
    }

    #[test]
    fn constructor_region_keeps_module_function_definitions_as_statements() {
        let object = local("Module");
        let callback = RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function::default()))), upvalues: vec![],
        });
        let mut block = Block(vec![declare(&object, Table::default().into()),
            print(string("initialization")),
            assign_field(&object, string("run"), callback),
            Return::new(vec![local_value(&object)]).into()]);
        let before = block.to_string();
        assert!(before.contains("function Module.run"));
        rebuild_table_literals(&mut block);
        assert_eq!(block.to_string(), before);
    }

    #[test]
    fn preserves_statement_order_around_first_read_barrier() {
        let table = local("table");
        let mut block = Block(vec![
            declare(&table, RValue::Table(Table::default())),
            assign_field(&table, string("Name"), string("First")),
            print(local_value(&table)),
            assign_field(&table, string("AfterRead"), string("Second")),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local table = { Name = \"First\" }\nprint(table)\ntable.AfterRead = \"Second\""
        );
    }

    #[test]
    fn does_not_fold_assignment_that_reads_constructed_table() {
        let table = local("table");
        let mut block = Block(vec![
            declare(&table, RValue::Table(Table::default())),
            assign_field(&table, string("Self"), local_value(&table)),
            assign_field(&table, string("Name"), string("After")),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local table = {}\ntable.Self = table\ntable.Name = \"After\""
        );
    }

    #[test]
    fn does_not_fold_closure_that_captures_constructed_table() {
        let table = local("table");
        let mut block = Block(vec![
            declare(&table, RValue::Table(Table::default())),
            assign_field(&table, string("Getter"), closure_capturing(&table)),
            assign_field(&table, string("Name"), string("After")),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(block.0.len(), 3);
        assert!(matches!(&block.0[0], crate::Statement::Assign(assign)
            if assign.prefix && matches!(assign.right.as_slice(), [RValue::Table(table_value)] if table_value.0.is_empty())));
        assert!(matches!(&block.0[1], crate::Statement::Assign(assign)
            if !assign.prefix
                && matches!(assign.left.as_slice(), [LValue::Index(index)]
                    if matches!(index.left.as_ref(), RValue::Local(local) if local == &table))));
    }

    #[test]
    fn folds_side_effectful_values_without_moving_them_past_barriers() {
        let table = local("table");
        let mut block = Block(vec![
            declare(&table, RValue::Table(Table::default())),
            assign_field(
                &table,
                string("Name"),
                Call::new(global("makeName"), vec![]).into(),
            ),
            assign_field(&table, string("Order"), number(1.0)),
            print(local_value(&table)),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local table = { Name = makeName(), Order = 1 }\nprint(table)"
        );
    }

    #[test]
    fn folds_side_effectful_dynamic_key_at_its_original_position() {
        let table = local("table");
        let mut block = Block(vec![
            declare(&table, RValue::Table(Table::default())),
            assign_field(
                &table,
                Call::new(global("makeKey"), vec![]).into(),
                string("Value"),
            ),
            assign_field(&table, string("Name"), string("After")),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local table = { [makeKey()] = \"Value\", Name = \"After\" }"
        );
    }

    #[test]
    fn overwrites_initial_placeholder_fields_without_duplicate_keys() {
        let props = local("props");
        let mut block = Block(vec![
            declare(
                &props,
                RValue::Table(Table::new(vec![
                    (Some(string("Name")), nil()),
                    (Some(string("LayoutOrder")), nil()),
                ])),
            ),
            assign_field(&props, string("Name"), string("Button")),
            assign_field(&props, string("LayoutOrder"), number(1.0)),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local props = { Name = \"Button\", LayoutOrder = 1 }"
        );
    }

    #[test]
    fn later_placeholder_write_stays_after_effectful_constructor_fields() {
        let props = local("props");
        let mut block = Block(vec![
            declare(
                &props,
                RValue::Table(Table::new(vec![
                    (Some(string("A")), nil()),
                    (
                        Some(string("B")),
                        Call::new(global("mark"), vec![string("B")]).into(),
                    ),
                ])),
            ),
            assign_field(
                &props,
                string("A"),
                Call::new(global("mark"), vec![string("A")]).into(),
            ),
        ]);

        rebuild_table_literals(&mut block);

        let output = block.to_string();
        let mark_b = output.find("B = mark(\"B\")").unwrap();
        let mark_a = output.rfind("A = mark(\"A\")").unwrap();
        assert!(mark_b < mark_a, "{output}");
        assert_eq!(output.matches("A =").count(), 1, "{output}");
    }

    #[test]
    fn keeps_a_second_store_to_a_listed_key_as_a_statement() {
        let props = local("props");
        let mut block = Block(vec![
            declare(&props, RValue::Table(Table::default())),
            assign_field(&props, string("Name"), string("First")),
            assign_field(&props, string("Name"), string("Second")),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local props = { Name = \"First\" }\nprops.Name = \"Second\""
        );
    }

    #[test]
    fn stores_take_zero_template_slots_across_constants() {
        // DUPTABLE creates template keys with the value 0; a constant the
        // template carries evaluates nothing, so later stores take their slots.
        let part = local("part");
        let mut block = Block(vec![
            declare(&part, RValue::Table(Table::new(vec![
                (Some(string("Size")), number(0.0)),
                (Some(string("Anchored")), RValue::Literal(Literal::Boolean(true))),
                (Some(string("Parent")), number(0.0)),
            ]))),
            assign_field(&part, string("Size"), Call::new(global("size"), vec![]).into()),
            assign_field(&part, string("Parent"), global("workspace")),
            Return::new(vec![part.clone().into()]).into(),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local part = { Size = size(), Anchored = true, Parent = workspace }\nreturn part"
        );
    }

    #[test]
    fn rebuilds_inside_nested_blocks_and_closures() {
        let outer_table = local("outerTable");
        let closure_table = local("closureTable");
        let function = Arc::new(Mutex::new(Function {
            body: Block(vec![
                declare(&closure_table, RValue::Table(Table::default())),
                assign_field(&closure_table, string("Name"), string("InsideClosure")),
            ]),
            ..Function::default()
        }));
        let mut block = Block(vec![If::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![
                declare(&outer_table, RValue::Table(Table::default())),
                assign_field(&outer_table, string("Name"), string("InsideIf")),
                print(RValue::Closure(Closure {
                    node_origin: Default::default(),
                    function: ByAddress(function.clone()),
                    upvalues: vec![],
                })),
            ]),
            Block(vec![]),
        )
        .into()]);

        rebuild_table_literals(&mut block);

        assert!(block.to_string().contains("Name = \"InsideIf\""));
        assert_eq!(
            function.lock().body.to_string(),
            "local closureTable = { Name = \"InsideClosure\" }"
        );
    }

    #[test]
    fn does_not_fold_nonlocal_constructor_assignment() {
        let table = local("table");
        let mut block = Block(vec![
            Assign::new(
                vec![LValue::Local(table.clone())],
                vec![RValue::Table(Table::default())],
            )
            .into(),
            assign_field(&table, string("Name"), string("Value")),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(block.to_string(), "table = {}\ntable.Name = \"Value\"");
    }

    #[test]
    fn rebuilt_pure_table_can_inline_into_single_call_use() {
        let create_element = local("createElement");
        let temp = local("v");
        let mut block = Block(vec![
            declare(&temp, RValue::Table(Table::default())),
            assign_field(&temp, string("Name"), string("Button")),
            Return::new(vec![Call::new(
                local_value(&create_element),
                vec![local_value(&temp)],
            )
            .into()])
            .into(),
        ]);

        rebuild_table_literals(&mut block);
        crate::inline_temps::inline_single_use_temps(&mut block);

        assert_eq!(
            block.to_string(),
            "return createElement({ Name = \"Button\" })"
        );
    }
}
