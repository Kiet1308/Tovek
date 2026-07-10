//! Shared readability budget for expression-collapsing passes.
//!
//! A raw node count underestimates boolean tangles: mixed `and`/`or` trees and
//! comparisons embedded in short-circuit logic are substantially harder to
//! scan than arithmetic of the same size.  Keep the metric structural (and
//! therefore allocation-free) instead of rendering an expression just to
//! measure its text.

use crate::{BinaryOperation, RValue, Select, Traverse};

/// Largest scalar expression a collapse pass may manufacture.
///
/// Tables and closures are laid out as blocks by the formatter and should be
/// handled by their owning pass instead of charging their entire UI subtree to
/// this scalar budget.
pub const MAX_COLLAPSED_EXPRESSION_COST: usize = 25;

/// Return whether a newly collapsed scalar expression remains readable.
pub fn collapse_allowed(value: &RValue) -> bool {
    expression_cost(value) <= MAX_COLLAPSED_EXPRESSION_COST
}

/// Structural readability cost:
///
/// * one point per expression node;
/// * two extra points when an `and`/`or` edge changes operator;
/// * three extra points for a comparison nested under short-circuit logic.
pub fn expression_cost(value: &RValue) -> usize {
    cost(value, None, false)
}

fn cost(value: &RValue, parent_logic: Option<BinaryOperation>, inside_logic: bool) -> usize {
    let (logic, comparison) = match value {
        RValue::Binary(binary) => (
            matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or)
                .then_some(binary.operation),
            matches!(
                binary.operation,
                BinaryOperation::Equal
                    | BinaryOperation::NotEqual
                    | BinaryOperation::LessThan
                    | BinaryOperation::LessThanOrEqual
                    | BinaryOperation::GreaterThan
                    | BinaryOperation::GreaterThanOrEqual
            ),
        ),
        _ => (None, false),
    };

    let mixed_logic =
        usize::from(parent_logic.is_some() && logic.is_some() && parent_logic != logic) * 2;
    let nested_comparison = usize::from(inside_logic && comparison) * 3;
    let child_parent = logic.or(parent_logic.filter(|_| inside_logic));
    let child_inside_logic = inside_logic || logic.is_some();

    1 + mixed_logic
        + nested_comparison
        + value
            .rvalues()
            .into_iter()
            .map(|child| cost(child, child_parent, child_inside_logic))
            .sum::<usize>()
}

/// Heavier operational cost retained for guard-merging. Calls and closures are
/// expensive there because merging several of them onto one line is noisy even
/// when the raw scalar tree is shallow.
pub fn guard_expression_cost(value: &RValue) -> usize {
    match value {
        RValue::Local(_) | RValue::Global(_) | RValue::Literal(_) | RValue::VarArg(_) => 1,
        RValue::Unary(unary) => 1 + guard_expression_cost(&unary.value),
        RValue::Binary(binary) => {
            1 + guard_expression_cost(&binary.left) + guard_expression_cost(&binary.right)
        }
        RValue::Index(index) => {
            1 + guard_expression_cost(&index.left) + guard_expression_cost(&index.right)
        }
        RValue::Call(call) => {
            8 + guard_expression_cost(&call.value)
                + call
                    .arguments
                    .iter()
                    .map(guard_expression_cost)
                    .sum::<usize>()
        }
        RValue::MethodCall(method_call) => {
            8 + guard_expression_cost(&method_call.value)
                + method_call
                    .arguments
                    .iter()
                    .map(guard_expression_cost)
                    .sum::<usize>()
        }
        RValue::Table(table) => {
            4 + table
                .0
                .iter()
                .map(|(key, value)| {
                    key.as_ref().map_or(0, guard_expression_cost) + guard_expression_cost(value)
                })
                .sum::<usize>()
        }
        RValue::IfExpression(if_expression) => {
            4 + guard_expression_cost(&if_expression.condition)
                + guard_expression_cost(&if_expression.then_value)
                + guard_expression_cost(&if_expression.else_value)
        }
        RValue::Closure(_) => 100,
        RValue::Select(select) => match select {
            Select::VarArg(var_arg) => guard_expression_cost(&RValue::VarArg(var_arg.clone())),
            Select::Call(call) => guard_expression_cost(&RValue::Call(call.clone())),
            Select::MethodCall(method_call) => {
                guard_expression_cost(&RValue::MethodCall(method_call.clone()))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{expression_cost, MAX_COLLAPSED_EXPRESSION_COST};
    use crate::{Binary, BinaryOperation, Global, Literal, RValue};

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn binary(left: RValue, operation: BinaryOperation, right: RValue) -> RValue {
        Binary::new(left, right, operation).into()
    }

    #[test]
    fn mixed_logic_and_nested_comparison_pay_readability_penalties() {
        let comparison = binary(
            global("value"),
            BinaryOperation::GreaterThan,
            RValue::Literal(Literal::Number(0.0)),
        );
        let expression = binary(
            binary(global("enabled"), BinaryOperation::And, comparison),
            BinaryOperation::Or,
            global("fallback"),
        );

        // Seven nodes + two for the and/or transition + three for the nested
        // comparison.
        assert_eq!(expression_cost(&expression), 12);
        assert!(expression_cost(&expression) < MAX_COLLAPSED_EXPRESSION_COST);
    }
}
