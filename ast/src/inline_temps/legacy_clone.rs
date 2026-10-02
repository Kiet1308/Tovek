//! Independent pre-transfer destination walker and clone commit for tests.
//! Keep its traversal, guard order and origin behavior as the W7 reference.
use super::*;
use std::cell::Cell;

thread_local! { static ENABLED: Cell<bool> = const { Cell::new(false) }; }

pub(super) fn enabled() -> bool { ENABLED.with(Cell::get) }

pub(super) fn run<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) { ENABLED.with(|enabled| enabled.set(self.0)); }
    }
    let _restore = Restore(ENABLED.with(|enabled| enabled.replace(true)));
    run()
}

pub(super) fn replace_direct_rvalue_use(
    statement: &mut Statement,
    local: &RcLocal,
    replacement: &RValue,
    facts: &MotionFacts,
) -> bool {
    let mut before_side_effects = match &*statement {
        Statement::Assign(assign) => assign
            .left
            .iter()
            .any(|left| lvalue_evaluation_order_barrier(left, facts, &|_| false)),
        _ => false,
    };
    let mut replaced = false;
    for_each_inlineable_direct_rvalue_mut(statement, &mut |rvalue| {
        if replaced {
            return;
        }
        if replace_first_rvalue_use(
            rvalue,
            local,
            replacement,
            facts,
            &mut before_side_effects,
            false,
        ) {
            replaced = true;
        } else if rvalue_evaluation_order_barrier(rvalue, facts) {
            before_side_effects = true;
        }
    });
    replaced
}

fn replace_first_rvalue_use(
    rvalue: &mut RValue,
    local: &RcLocal,
    replacement: &RValue,
    facts: &MotionFacts,
    before_side_effects: &mut bool,
    conditionally_evaluated: bool,
) -> bool {
    #[cfg(test)]
    chain_probe::record_destination_visit();
    if matches!(rvalue, RValue::Local(read) if read == local) {
        // The caller has already proved this exact use with can_sink_with_summary.
        // Closure construction does not execute its body: captures can commute
        // with earlier capture reads. The older boolean barrier below cannot
        // distinguish those reads from callbacks/writes, which the position
        // proof still rejects. Keep the independent conditional-use refusal.
        if (!matches!(&replacement, RValue::Closure(_))
            && !can_replace_after_prior_effects(&replacement, *before_side_effects, facts))
            || (conditionally_evaluated && rvalue_evaluation_order_barrier(&replacement, facts))
        {
            return false;
        }
        #[cfg(test)]
        chain_probe::record_copy(replacement);
        *rvalue = replacement.clone();
        crate::node_origins::inlined(rvalue);
        return true;
    }

    // A declaration initializer is evaluated unconditionally. Moving it into
    // the right arm of `and`/`or`, or either value arm of an if-expression,
    // must not make calls/global reads/captured-cell reads conditional. Keep the
    // ordinary left-to-right barrier accounting, but explicitly carry whether
    // the current subtree may be skipped.
    if let RValue::Binary(binary) = rvalue
        && matches!(
            binary.operation,
            crate::BinaryOperation::And | crate::BinaryOperation::Or
        )
    {
        if replace_first_rvalue_use(
            &mut binary.left,
            local,
            replacement,
            facts,
            before_side_effects,
            conditionally_evaluated,
        ) {
            return true;
        }
        if rvalue_evaluation_order_barrier(&binary.left, facts) {
            *before_side_effects = true;
        }
        return replace_first_rvalue_use(
            &mut binary.right,
            local,
            replacement,
            facts,
            before_side_effects,
            true,
        );
    }

    if let RValue::IfExpression(if_expression) = rvalue {
        if replace_first_rvalue_use(
            &mut if_expression.condition,
            local,
            replacement,
            facts,
            before_side_effects,
            conditionally_evaluated,
        ) {
            return true;
        }
        if rvalue_evaluation_order_barrier(&if_expression.condition, facts) {
            *before_side_effects = true;
        }

        // At most one read exists globally for an inline candidate. Scanning
        // the then arm first can therefore only make the else-arm check more
        // conservative; it cannot move two copies of the initializer.
        if replace_first_rvalue_use(
            &mut if_expression.then_value,
            local,
            replacement,
            facts,
            before_side_effects,
            true,
        ) {
            return true;
        }
        if rvalue_evaluation_order_barrier(&if_expression.then_value, facts) {
            *before_side_effects = true;
        }
        return replace_first_rvalue_use(
            &mut if_expression.else_value,
            local,
            replacement,
            facts,
            before_side_effects,
            true,
        );
    }

    for child in rvalue.rvalues_mut() {
        if replace_first_rvalue_use(
            child,
            local,
            replacement,
            facts,
            before_side_effects,
            conditionally_evaluated,
        ) {
            return true;
        }
        if rvalue_evaluation_order_barrier(child, facts) {
            *before_side_effects = true;
        }
    }
    false
}
