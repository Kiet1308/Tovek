use crate::{
    Assign, Block, Index, LValue, LocalRw, RValue, RcLocal, SideEffects, Statement, Table, Traverse,
};

/// Rebuild table literals that were lowered to `local t = {}` followed by
/// contiguous field assignments.
///
/// This pass deliberately only consumes assignments before the first non-field
/// statement, so the table has no opportunity to be read or aliased before the
/// folded writes.
pub fn rebuild_table_literals(block: &mut Block) {
    rebuild_nested_blocks(block);
    rebuild_current_block(block);
}

fn rebuild_nested_blocks(block: &mut Block) {
    for statement in &mut block.0 {
        rebuild_nested_in_statement(statement);
    }
}

fn rebuild_nested_in_statement(statement: &mut Statement) {
    rebuild_closures_in_statement(statement);
    match statement {
        Statement::If(r#if) => {
            rebuild_table_literals(&mut r#if.then_block.lock());
            rebuild_table_literals(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => rebuild_table_literals(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => rebuild_table_literals(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => rebuild_table_literals(&mut numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => rebuild_table_literals(&mut generic_for.block.lock()),
        _ => {}
    }
}

fn rebuild_closures_in_statement(statement: &mut Statement) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    for function in functions {
        rebuild_table_literals(&mut function.lock().body);
    }
}

fn rebuild_current_block(block: &mut Block) {
    let mut index = 0;
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
        }

        index += 1;
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
    !key.has_side_effects()
        && !key.values_read().iter().any(|read| *read == object_local)
        && !value.values_read().iter().any(|read| *read == object_local)
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
        Some(position) if !table.0[position].1.has_side_effects() => {
            table.0[position].1 = value;
        }
        _ => table.0.push((Some(key), value)),
    }
}

#[cfg(test)]
mod tests {
    use super::rebuild_table_literals;
    use crate::{
        Assign, Block, Call, Closure, Function, Global, If, Index, LValue, Literal, Local, RValue,
        RcLocal, Return, Table, Upvalue,
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
        let mut block = Block(vec![
            If::new(
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
            .into(),
        ]);

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
            Return::new(vec![
                Call::new(local_value(&create_element), vec![local_value(&temp)]).into(),
            ])
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
