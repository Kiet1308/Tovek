// Frozen pre-visitor and pre-borrowed-name reference; never delegates to the new selectors.
use super::*;
use crate::{Closure, Function, GenericFor, If, NumericFor, Return, Table};
use by_address::ByAddress;
use parking_lot::Mutex;
use triomphe::Arc;
fn global_name(global: &Global) -> Option<String> { std::str::from_utf8(&global.0).ok().map(str::to_owned) }

fn named(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }

fn fixture() -> Block {
    let shared = named("WAIT_INTERVAL");
    let inner = Closure { node_origin: Default::default(),
        function: ByAddress(Arc::new(Mutex::new(Function {
            parameters: vec![named("nestedParameter")],
            body: Block(vec![Return::new(vec![Global::from("nestedGlobal").into(), shared.clone().into()]).into()]),
            ..Function::default()
        }))), upvalues: vec![] };
    let outer = Closure { node_origin: Default::default(),
        function: ByAddress(Arc::new(Mutex::new(Function {
            parameters: vec![named("WAIT_INTERVAL"), named("outerParameter")],
            body: Block(vec![Return::new(vec![inner.clone().into()]).into()]),
            ..Function::default()
        }))), upvalues: vec![] };
    let repeated = (0..32).map(|_| (None, RValue::Local(shared.clone()))).collect();
    Block(vec![
        Assign::new(vec![Index::new(outer.clone().into(), inner.clone().into()).into(),
            LValue::Global(Global::from("lhsGlobal"))],
            vec![Table::new(vec![(Some(inner.clone().into()), outer.clone().into())]).into(), Table::new(repeated).into()]).into(),
        If::new(inner.clone().into(), Block(vec![Return::new(vec![outer.clone().into()]).into()]),
            Block(vec![Return::new(vec![inner.clone().into()]).into()])).into(),
        NumericFor::new(outer.into(), inner.clone().into(), Literal::Number(1.0).into(), named("counter"),
            Block(vec![Return::new(vec![shared.into()]).into()])).into(),
        GenericFor::new(vec![named("key"), named("value")], vec![inner.into()],
            Block(vec![Return::new(vec![RValue::Global(Global(vec![0xff, 0, b'x'])),
                Global::from("\0validUtf8").into(), Global::from("unicode_λ").into()]).into()])).into(),
    ])
}

#[test]
fn borrowed_reserved_names_match_legacy_membership_and_generated_names() {
    for seed_count in [0, 1, 3, 7, 15, 31, 63] {
        let mut expected: FxHashSet<_> = (0..seed_count).map(|n| format!("reserved{n}")).collect();
        expected.insert("WAIT_INTERVAL".into());
        let mut actual = expected.clone();
        let mut body = fixture();
        let before = body.to_string();
        legacy_collect_reserved_identifiers(&mut body, &mut expected);
        collect_reserved_identifiers(&mut body, &mut actual);
        assert_eq!(actual, expected);
        assert_eq!(body.to_string(), before, "reservation must not mutate AST owners or names");
        assert!(actual.contains("nestedParameter") && actual.contains("nestedGlobal"));
        assert!(actual.contains("counter") && actual.contains("key"));
        assert!(actual.contains("\0validUtf8") && actual.contains("unicode_λ"));
        for name in ["WAIT_INTERVAL", "WAIT_INTERVAL", "counter", "key", "nestedGlobal", "fresh"] {
            assert_eq!(unique_name(name, &mut actual), unique_name(name, &mut expected));
        }
    }
}

#[test]
fn nested_function_collection_preserves_legacy_order_duplicates_and_shallow_bodies() {
    let mut body = fixture();
    let mut expected = Vec::new();
    let mut actual = Vec::new();
    legacy_collect_nested_functions(&mut body.0, &mut expected);
    collect_nested_functions(&mut body.0, &mut actual);
    assert_eq!(actual, expected);
    assert!(actual.len() > 2, "duplicate closure occurrences remain visited");
    // Direct collectors stop at each closure; nested bodies are processed only
    // by their separate scope, preserving the original owner/locking boundary.
    let mut child_expected = Vec::new();
    let mut child_actual = Vec::new();
    for function in actual {
        let mut function = function.lock();
        legacy_collect_nested_functions(&mut function.body.0, &mut child_expected);
        collect_nested_functions(&mut function.body.0, &mut child_actual);
    }
    assert_eq!(child_actual, child_expected);
}

pub(crate) fn legacy_collect_reserved_identifiers(body: &mut Block, reserved: &mut FxHashSet<String>) {
    for statement in &mut body.0 {
        let mut functions = Vec::new();
        statement.post_traverse_values(&mut |value| -> Option<()> {
            match value {
                Either::Right(RValue::Local(local)) | Either::Left(LValue::Local(local)) => {
                    if let Some(name) = local_name(&local) {
                        reserved.insert(name);
                    }
                }
                Either::Right(RValue::Global(global)) | Either::Left(LValue::Global(global)) => {
                    if let Some(name) = global_name(&global) {
                        reserved.insert(name);
                    }
                }
                Either::Right(RValue::Closure(closure)) => {
                    functions.push(closure.function.clone());
                }
                _ => {}
            }
            None
        });
        for function in functions {
            let mut function = function.lock();
            reserved.extend(function.parameters.iter().filter_map(local_name));
            legacy_collect_reserved_identifiers(&mut function.body, reserved);
        }
        match statement {
            Statement::If(node) => {
                legacy_collect_reserved_identifiers(&mut node.then_block.lock(), reserved);
                legacy_collect_reserved_identifiers(&mut node.else_block.lock(), reserved);
            }
            Statement::While(node) => {
                legacy_collect_reserved_identifiers(&mut node.block.lock(), reserved)
            }
            Statement::Repeat(node) => {
                legacy_collect_reserved_identifiers(&mut node.block.lock(), reserved)
            }
            Statement::NumericFor(node) => {
                if let Some(name) = local_name(&node.counter) {
                    reserved.insert(name);
                }
                legacy_collect_reserved_identifiers(&mut node.block.lock(), reserved);
            }
            Statement::GenericFor(node) => {
                reserved.extend(node.res_locals.iter().filter_map(local_name));
                legacy_collect_reserved_identifiers(&mut node.block.lock(), reserved);
            }
            _ => {}
        }
    }
}

fn legacy_collect_nested_functions(
    stmts: &mut [Statement],
    functions: &mut Vec<by_address::ByAddress<triomphe::Arc<parking_lot::Mutex<crate::Function>>>>,
) {
    for statement in stmts {
        for value in crate::deinline::stmt_rvalues_mut(statement) {
            legacy_collect_functions_in_rvalue(value, functions);
        }
        match statement {
            Statement::If(node) => {
                legacy_collect_nested_functions(&mut node.then_block.lock().0, functions);
                legacy_collect_nested_functions(&mut node.else_block.lock().0, functions);
            }
            Statement::While(node) => legacy_collect_nested_functions(&mut node.block.lock().0, functions),
            Statement::Repeat(node) => {
                legacy_collect_nested_functions(&mut node.block.lock().0, functions)
            }
            Statement::NumericFor(node) => {
                legacy_collect_nested_functions(&mut node.block.lock().0, functions)
            }
            Statement::GenericFor(node) => {
                legacy_collect_nested_functions(&mut node.block.lock().0, functions)
            }
            _ => {}
        }
    }
}

fn legacy_collect_functions_in_rvalue(
    value: &mut RValue,
    functions: &mut Vec<by_address::ByAddress<triomphe::Arc<parking_lot::Mutex<crate::Function>>>>,
) {
    if let RValue::Closure(closure) = value {
        functions.push(closure.function.clone());
        return;
    }
    for child in value.rvalues_mut() {
        legacy_collect_functions_in_rvalue(child, functions);
    }
}
