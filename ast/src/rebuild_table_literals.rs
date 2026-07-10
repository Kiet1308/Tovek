use crate::{Assign, Block, Index, LValue, LocalRw, RValue, RcLocal, Statement, Table, Traverse};

/// Rebuild table literals that were lowered to `local t = {}` followed by
/// contiguous field assignments.
///
/// This pass deliberately only consumes assignments before the first non-field
/// statement, so the table has no opportunity to be read or aliased before the
/// folded writes.
pub fn rebuild_table_literals(block: &mut Block) -> bool {
    let captured = crate::inline_temps::collect_usage(block)
        .into_iter()
        .filter(|(_, usage)| usage.captured)
        .map(|(local, _)| local)
        .collect::<rustc_hash::FxHashSet<_>>();
    rebuild_with_captured(block, &captured)
}

pub(crate) fn rebuild_with_captured(
    block: &mut Block,
    captured: &rustc_hash::FxHashSet<RcLocal>,
) -> bool {
    let nested_changed = rebuild_nested_blocks(block, captured);
    let sunk_changed = sink_total_table_declarations(block, captured);
    let drained_changed = extract_drained_constructor_fields(block);
    rebuild_current_block(block) | sunk_changed | drained_changed | nested_changed
}

fn rebuild_nested_blocks(block: &mut Block, captured: &rustc_hash::FxHashSet<RcLocal>) -> bool {
    let mut changed = false;
    for statement in &mut block.0 {
        changed |= rebuild_nested_in_statement(statement, captured);
    }
    changed
}

fn rebuild_nested_in_statement(
    statement: &mut Statement,
    captured: &rustc_hash::FxHashSet<RcLocal>,
) -> bool {
    let closures_changed = rebuild_closures_in_statement(statement, captured);
    let blocks_changed = match statement {
        Statement::If(r#if) => {
            rebuild_with_captured(&mut r#if.then_block.lock(), captured)
                | rebuild_with_captured(&mut r#if.else_block.lock(), captured)
        }
        Statement::While(r#while) => rebuild_with_captured(&mut r#while.block.lock(), captured),
        Statement::Repeat(repeat) => rebuild_with_captured(&mut repeat.block.lock(), captured),
        Statement::NumericFor(numeric_for) => {
            rebuild_with_captured(&mut numeric_for.block.lock(), captured)
        }
        Statement::GenericFor(generic_for) => {
            rebuild_with_captured(&mut generic_for.block.lock(), captured)
        }
        _ => false,
    };
    closures_changed | blocks_changed
}

fn rebuild_closures_in_statement(
    statement: &mut Statement,
    captured: &rustc_hash::FxHashSet<RcLocal>,
) -> bool {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    functions.into_iter().fold(false, |changed, function| {
        rebuild_with_captured(&mut function.lock().body, captured) | changed
    })
}

fn rebuild_current_block(block: &mut Block) -> bool {
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

        while index + 1 < block.0.len() {
            let Some((key, value)) = block.0[index + 1]
                .as_assign()
                .and_then(|assign| field_assignment_parts(assign, &object_local))
            else {
                break;
            };

            if !can_fold_table_field_assignment(key, value, &object_local) {
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
        if !table.0.iter().all(|(key, value)| {
            key.iter().all(total_stable_component) && total_stable_component(value)
        }) {
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

fn total_stable_component(value: &RValue) -> bool {
    matches!(value, RValue::Local(_) | RValue::Literal(_))
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
    matches!(
        key,
        RValue::Literal(
            crate::Literal::String(_) | crate::Literal::Number(_) | crate::Literal::Boolean(_)
        )
    )
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
    let LValue::Index(Index { left, right }) = &assign.left[0] else {
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
    stable_dynamic_key(key)
        && !key.values_read().iter().any(|read| *read == object_local)
        && !value.values_read().iter().any(|read| *read == object_local)
}

/// Keys materialised by Luau's `SETTABLE` lowering are safe to put back at the
/// same position in the constructor when they are stable lookup chains. `Index`
/// is conservatively side-effectful in the general AST model (metamethods can
/// run), but here no statement or field evaluation is crossed: the lookup still
/// occurs after all existing constructor entries and before the same value.
/// Calls and operator expressions remain refused.
fn stable_dynamic_key(key: &RValue) -> bool {
    match key {
        RValue::Local(_) | RValue::Global(_) | RValue::Literal(_) => true,
        RValue::Index(index) => stable_dynamic_key(&index.left) && stable_dynamic_key(&index.right),
        _ => false,
    }
}

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
        _ => table.0.push((Some(key), value)),
    }
}

fn inert_nil_placeholder_suffix(table: &Table, position: usize, initial_len: usize) -> bool {
    table.0[position..initial_len].iter().all(|(key, value)| {
        matches!(
            key,
            Some(RValue::Literal(
                crate::Literal::String(_) | crate::Literal::Number(_) | crate::Literal::Boolean(_)
            ))
        ) && matches!(value, RValue::Literal(crate::Literal::Nil))
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
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(local.clone())],
        })
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
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![],
        });
        let mut block = Block(vec![
            declare(
                &props,
                RValue::Table(Table(vec![(Some(string("Name")), string("Button"))])),
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
                RValue::Table(Table(vec![
                    (Some(string("Name")), string("Panel")),
                    (
                        Some(string("children")),
                        RValue::Table(Table(vec![(Some(string("Label")), string("Child"))])),
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
                RValue::Table(Table(vec![(Some(string("Model")), local_value(&model))])),
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
    fn does_not_fold_side_effectful_dynamic_key() {
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
            "local table = {}\ntable[makeKey()] = \"Value\"\ntable.Name = \"After\""
        );
    }

    #[test]
    fn overwrites_initial_placeholder_fields_without_duplicate_keys() {
        let props = local("props");
        let mut block = Block(vec![
            declare(
                &props,
                RValue::Table(Table(vec![
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
                RValue::Table(Table(vec![
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
        assert_eq!(output.matches("A =").count(), 2, "{output}");
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
