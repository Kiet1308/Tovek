use super::*;
use crate::{Assign, Binary, BinaryOperation, Closure, Global, Literal, Return};
use std::cell::Cell;

thread_local! { static BUILDS: Cell<usize> = const { Cell::new(0) }; }
pub(super) fn record_build() { BUILDS.with(|count| count.set(count.get() + 1)); }
pub(super) fn reset_builds() { BUILDS.with(|count| count.set(0)); }
pub(super) fn builds() -> usize { BUILDS.with(Cell::get) }

fn targets() -> Vec<ExprTarget> {
    let mut body = Block::default();
    for index in 0..3 {
        let parameter = RcLocal::default();
        let condition = Binary::new(
            Call::new(Global::from("typeof").into(), vec![parameter.clone().into()]).into(),
            Literal::String(format!("kind{index}").into_bytes()).into(), BinaryOperation::Equal).into();
        let function = Function { parameters: vec![parameter], body: Block(vec![Return::new(vec![condition]).into()]), ..Default::default() };
        let closure = Closure { node_origin: Default::default(), upvalues: vec![],
            function: by_address::ByAddress(Arc::new(Mutex::new(function))) };
        let mut declaration = Assign::new(vec![RcLocal::default().into()], vec![closure.into()]);
        declaration.prefix = true;
        body.0.push(declaration.into());
    }
    collect_expr_targets(&body, false)
}

#[test]
fn bounded_membership_preserves_scope_order_and_duplicate_indices() {
    let mut active = ActiveTargets::default();
    for index in [0, 63, 64, 127, 128, 191, 192, 255, 255] { active.push(index); }
    assert_eq!(active.order, [0, 63, 64, 127, 128, 191, 192, 255, 255]);
    for index in 0..256 { assert_eq!(active.contains(&index), active.order.contains(&index)); }
    let mut inner = active.clone();
    inner.push(7);
    assert!(inner.contains(&7));
    assert!(!active.contains(&7), "nested activation must not escape its scope");
}

#[test]
fn caller_root_priority_is_lazy_and_built_once_for_repeated_queries() {
    let targets = targets();
    assert_eq!(targets.len(), 3);
    let _scope = crate::reconstruction_search::enter(vec![vec![Some(9)], vec![Some(8)], vec![Some(9)], vec![Some(8)]]);
    let caller = 12345usize as FnPtr;
    crate::reconstruction_search::register_function(caller as usize, 0);
    for (index, target) in targets.iter().enumerate() {
        crate::reconstruction_search::register_function(target.func_ptr as usize, index + 1);
    }
    let root = std::mem::discriminant(&targets[0].expr);
    let mut orders = CandidateOrders::default();
    orders.roots.insert(root, vec![2, 1, 0]);
    reset_builds();
    assert!(orders.get(std::mem::discriminant(&RValue::Literal(Literal::Nil)), Some(caller), &targets).is_none());
    assert!(crate::reconstruction_search::hints_deferred_for_test());
    assert_eq!(orders.get(root, None, &targets), Some(&[2, 1, 0][..]));
    assert!(crate::reconstruction_search::hints_deferred_for_test(), "an unknown caller cannot force hints");
    assert_eq!(orders.get(root, Some(caller), &targets), Some(&[1, 2, 0][..]));
    assert!(!crate::reconstruction_search::hints_deferred_for_test());
    for _ in 0..1024 {
        assert_eq!(orders.get(root, Some(caller), &targets), Some(&[1, 2, 0][..]));
        assert_eq!(orders.get(root, None, &targets), Some(&[2, 1, 0][..]));
    }
    assert_eq!(builds(), 2, "2,050 logical queries need two priority builds");
}
