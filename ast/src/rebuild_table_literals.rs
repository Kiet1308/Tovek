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
    rebuild_current_block(block, captured, single_write) | sunk_changed | regions_changed | drained_changed | nested_changed
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
    let mut index = 0;
    let mut changed = false;
    while index + 1 < block.0.len() {
        let Some(object_local) = table_constructor_local(&block.0[index]) else {
            index += 1;
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
            && lone_callback_field(&block.0[index + 1..], &object_local)
        {
            // A one-field wrapper around a callback adds nesting without
            // grouping a registry. Retain the readable statement layout.
            index += 1;
            continue;
        }

        while index + 1 < block.0.len() {
            if let Statement::SetList(set_list) = &block.0[index + 1] {
                let table = block.0[index].as_assign().unwrap().right[0]
                    .as_table()
                    .unwrap();
                if !private
                    || !can_append_set_list(table, set_list, &object_local)
                {
                    break;
                }
                let Statement::SetList(set_list) = block.0.remove(index + 1) else {
                    unreachable!()
                };
                let table = block.0[index].as_assign_mut().unwrap().right[0]
                    .as_table_mut()
                    .unwrap();
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
                continue;
            }
            let Some((key, value)) = block.0[index + 1]
                .as_assign()
                .and_then(|assign| field_assignment_parts(assign, &object_local))
            else {
                break;
            };

            let table = block.0[index].as_assign().unwrap().right[0]
                .as_table()
                .unwrap();
            if !private
                || table
                    .0
                    .last()
                    .is_some_and(|(key, value)| key.is_none() && expands(value))
                || !can_fold_table_field_assignment(key, value, &object_local)
            {
                break;
            }

            let field_assign = block.0.remove(index + 1).into_assign().unwrap();
            let (key, value) = field_assignment_key_value(field_assign);
            let table = block.0[index].as_assign_mut().unwrap().right[0]
                .as_table_mut()
                .unwrap();
            insert_table_entry(table, initial_len, key, value);
            changed = true;
        }

        index += 1;
    }
    changed
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
        if statement.values_read().into_iter().chain(statement.values_written()).any(|read| read == local) {
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
    list.object_local == *object
        && list.index == 1 + table.0.iter().filter(|(key, _)| key.is_none()).count()
        && !table.0.last().is_some_and(|(key, value)| key.is_none() && expands(value))
        // Luau flushes array entries before every keyed field. Appending at
        // the exact next list index therefore retains numeric-key overwrites,
        // nil stores and dynamic-key errors in their original order.
        && !list.values.iter().chain(list.tail.iter())
            .any(|value| value.values_read().iter().any(|read| *read == object))
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
                        && can_fold_table_field_assignment(key, value, &object)),
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
            let declaration = block.0.remove(index);
            block.0.insert(next - 1, declaration);
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
        value.rvalues().into_iter().all(|child| self.value(child, depth + 1))
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
/// The field must be the last constructor entry and unique, so evaluation order
/// and the final raw-table contents are identical. The regular table/tree
/// inliner can then consume `p` and `c` from the leaves upward.
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
                .any(|(existing, _)| existing.as_ref() == Some(&key))
        {
            index += 1;
            continue;
        }
        let (_, value) = table.0.pop().unwrap();
        if value.values_read().iter().any(|read| *read == &object) {
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
    if table.values_read().iter().any(|read| *read == local) {
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
    !key.values_read().iter().any(|read| *read == object_local)
        && !value.values_read().iter().any(|read| *read == object_local)
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

fn insert_table_entry(table: &mut Table, initial_len: usize, key: RValue, value: RValue) {
    match table
        .0
        .iter()
        .take(initial_len)
        .position(|(existing_key, _)| existing_key.as_ref() == Some(&key))
    {
        // Replacing an old placeholder in place moves the new value ahead of
        // every constructor entry after it. That is only order-preserving for
        // the common lowering shape where the old entry and every crossed entry
        // are inert `literalKey = nil` placeholders. Otherwise retain the old
        // entry and append the later write as a duplicate constructor field;
        // Luau evaluates duplicate fields in order, exactly like the original
        // constructor followed by `t[key] = value`.
        Some(position) if inert_nil_placeholder_suffix(table, position, initial_len) => {
            table.0[position].1 = value;
        }
        Some(position)
            if matches!(&table.0[position].1, RValue::Literal(crate::Literal::Nil))
                && crate::is_total_table_key(&key) =>
        {
            // A nil store to a fresh literal-key slot does nothing. Remove
            // just that placeholder, keeping the later value after every
            // intervening evaluation instead of moving it to the old slot.
            table.0.remove(position);
            table.0.push((Some(key), value));
        }
        _ => table.0.push((Some(key), value)),
    }
}

fn inert_nil_placeholder_suffix(table: &Table, position: usize, initial_len: usize) -> bool {
    table.0[position..initial_len].iter().all(|(key, value)| {
        key.as_ref()
            .is_some_and(crate::side_effects::is_total_table_key)
            && matches!(value, RValue::Literal(crate::Literal::Nil))
    })
}

#[cfg(test)]
mod tests {
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

    fn closure_capturing(local: &RcLocal) -> RValue {
        RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(local.clone())],
        })
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
            "local props = {\n\tName = \"TextButton\",\n\tLayoutOrder = layoutOrder\n}\nreturn props"
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
            "local table = {\n\t[key] = value\n}\nreturn table"
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
            "return {\n\tMiddle = {\n\t\tLeaf = {\n\t\t\tName = \"Leaf\"\n\t\t}\n\t}\n}"
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
            "return createElement(\"Frame\", {\n\tName = \"Panel\"\n}, {\n\tLabel = \"Child\"\n})"
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
            "local modelCenter = getModelCenter(spinModel) -- inlined helper\nreturn {\n\tModel = spinModel,\n\tCenter = modelCenter,\n\tParts = parts\n}"
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
            "local table = {\n\tName = \"First\"\n}\nprint(table)\ntable.AfterRead = \"Second\""
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
            "local table = {\n\tName = makeName(),\n\tOrder = 1\n}\nprint(table)"
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
            "local table = {\n\t[makeKey()] = \"Value\",\n\tName = \"After\"\n}"
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
            "local props = {\n\tName = \"Button\",\n\tLayoutOrder = 1\n}"
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
    fn does_not_overwrite_fields_folded_from_earlier_assignments() {
        let props = local("props");
        let mut block = Block(vec![
            declare(&props, RValue::Table(Table::default())),
            assign_field(&props, string("Name"), string("First")),
            assign_field(&props, string("Name"), string("Second")),
        ]);

        rebuild_table_literals(&mut block);

        assert_eq!(
            block.to_string(),
            "local props = {\n\tName = \"First\",\n\tName = \"Second\"\n}"
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
            "local closureTable = {\n\tName = \"InsideClosure\"\n}"
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
            "return createElement({\n\tName = \"Button\"\n})"
        );
    }
}
