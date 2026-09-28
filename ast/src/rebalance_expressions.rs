//! Recover statement-shaped construction from unreadable scalar expressions.
//!
//! Luau's SSA lowering turns repeated `text = text .. part` updates into one
//! deeply left-associated concat tree. Re-emitting that tree as a single return
//! obscures both intent and evaluation order. This pass splits only long concat
//! spines; other expression kinds remain owned by their original collapse pass.

use rustc_hash::FxHashSet;

use crate::{
    Assign, Binary, BinaryOperation, Block, If, LValue, Literal, Local, LocalRw, RValue, RcLocal,
    Statement, Traverse, UnaryOperation,
};

const MIN_CONCAT_OPERATORS: usize = 4;
const MAX_ACTIVE_LOCALS: usize = 200;

pub fn rebalance_expressions(block: &mut Block) {
    let mut context = FunctionContext::new(block, 0);
    rebalance_block(block, &mut context);
}

struct FunctionContext {
    local_headroom: usize,
    reserved: FxHashSet<String>,
}

impl FunctionContext {
    /// Only a split `return` mints a local, and no rewrite creates one. The
    /// name snapshot precedes nested closure rewrites, so it is taken eagerly,
    /// but only for a function that owns such a `return`.
    fn new(block: &mut Block, parameter_count: usize) -> Self {
        let mut reserved = FxHashSet::default();
        let mut local_headroom = 0;
        if block.any_statement(&mut |statement| return_concat_parts_spine(statement)) {
            crate::rehoist_constants::collect_reserved_identifiers(block, &mut reserved);
            let declared = count_declared_locals(block);
            local_headroom = MAX_ACTIVE_LOCALS.saturating_sub(parameter_count + declared);
        }
        Self { local_headroom, reserved }
    }

    fn fresh_text_local(&mut self) -> Option<RcLocal> {
        if self.local_headroom == 0 {
            return None;
        }
        self.local_headroom -= 1;
        let name = crate::rehoist_constants::unique_name("text", &mut self.reserved);
        Some(RcLocal::new(Local::new(Some(name))))
    }
}

fn count_declared_locals(block: &Block) -> usize {
    block
        .0
        .iter()
        .map(|statement| {
            let direct = match statement {
                Statement::Assign(assign) if assign.prefix => assign
                    .left
                    .iter()
                    .filter(|left| matches!(left, LValue::Local(_)))
                    .count(),
                Statement::NumericFor(_) => 1,
                Statement::GenericFor(node) => node.res_locals.len(),
                _ => 0,
            };
            direct
                + match statement {
                    Statement::If(node) => {
                        count_declared_locals(&node.then_block.lock())
                            + count_declared_locals(&node.else_block.lock())
                    }
                    Statement::While(node) => count_declared_locals(&node.block.lock()),
                    Statement::Repeat(node) => count_declared_locals(&node.block.lock()),
                    Statement::NumericFor(node) => count_declared_locals(&node.block.lock()),
                    Statement::GenericFor(node) => count_declared_locals(&node.block.lock()),
                    _ => 0,
                }
        })
        .sum()
}

fn rebalance_block(block: &mut Block, context: &mut FunctionContext) {
    for statement in &mut block.0 {
        let mut functions = Vec::new();
        statement.post_traverse_rvalues(&mut |value| -> Option<()> {
            if let RValue::Closure(closure) = value {
                functions.push(closure.function.clone());
            }
            None
        });
        for function in functions {
            let mut function = function.lock();
            let parameter_count = function.parameters.len();
            let mut nested_context = FunctionContext::new(&mut function.body, parameter_count);
            rebalance_block(&mut function.body, &mut nested_context);
        }

        match statement {
            Statement::If(node) => {
                rebalance_block(&mut node.then_block.lock(), context);
                rebalance_block(&mut node.else_block.lock(), context);
            }
            Statement::While(node) => rebalance_block(&mut node.block.lock(), context),
            Statement::Repeat(node) => rebalance_block(&mut node.block.lock(), context),
            Statement::NumericFor(node) => rebalance_block(&mut node.block.lock(), context),
            Statement::GenericFor(node) => rebalance_block(&mut node.block.lock(), context),
            _ => {}
        }
    }

    let mut index = 0;
    while index < block.0.len() {
        if let Some((local, prefix, arms, fallback)) = conditional_assignment_parts(&block.0[index])
        {
            let replacement = conditional_assignment_statements(local, prefix, arms, fallback);
            let replacement_len = replacement.len();
            block.0.splice(index..=index, replacement);
            index += replacement_len;
            continue;
        }

        if let Some((local, parts)) = declaration_concat_parts(&block.0[index]) {
            let replacement = concat_statements(local, parts, true);
            let replacement_len = replacement.len();
            block.0.splice(index..=index, replacement);
            index += replacement_len;
            continue;
        }

        let Some(parts) = return_concat_parts(&block.0[index]) else {
            index += 1;
            continue;
        };
        let Some(local) = context.fresh_text_local() else {
            index += 1;
            continue;
        };
        let mut replacement = concat_statements(local.clone(), parts, true);
        replacement.push(crate::Return::new(vec![RValue::Local(local)]).into());
        let replacement_len = replacement.len();
        block.0.splice(index..=index, replacement);
        index += replacement_len;
    }
}

type ConditionalArms = Vec<(RValue, RValue)>;

fn conditional_assignment_parts(
    statement: &Statement,
) -> Option<(RcLocal, bool, ConditionalArms, RValue)> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    let LValue::Local(local) = &assign.left[0] else {
        return None;
    };
    // At least two selected arms plus a fallback: the root must be `or`.
    if !matches!(&assign.right[0], RValue::Binary(binary) if binary.operation == BinaryOperation::Or) {
        return None;
    }
    if crate::expression_budget::collapse_allowed(&assign.right[0]) {
        return None;
    }

    let mut terms = Vec::new();
    collect_or_terms(&assign.right[0], &mut terms);
    let fallback = terms.pop()?;
    // A single `condition and value or fallback` is still compact and familiar.
    // The readability cliff starts at a chained selection with at least two
    // conditions (the nested-if shape this recovery targets).
    if terms.len() < 2 {
        return None;
    }
    for &term in &terms {
        let RValue::Binary(binary) = term else {
            return None;
        };
        if binary.operation != BinaryOperation::And || !statically_truthy(&binary.right) {
            return None;
        }
    }
    if terms.len() < 3 && crate::expression_budget::expression_cost(fallback) < 10 {
        return None;
    }
    if assign.prefix && reads_rendered_name(&assign.right[0], local) {
        return None;
    }
    // Clone only after every refusal gate has passed. Keep the original
    // accepted-path copy order: fallback, then each condition/value pair.
    let fallback = clone_rebalance_part(fallback);
    let arms = terms.into_iter().map(|term| {
        let RValue::Binary(binary) = term else { unreachable!() };
        (clone_rebalance_part(&binary.left), clone_rebalance_part(&binary.right))
    }).collect();
    Some((local.clone(), assign.prefix, arms, fallback))
}

fn collect_or_terms<'a>(value: &'a RValue, terms: &mut Vec<&'a RValue>) {
    if let RValue::Binary(binary) = value
        && binary.operation == BinaryOperation::Or
    {
        collect_or_terms(&binary.left, terms);
        collect_or_terms(&binary.right, terms);
    } else {
        terms.push(value);
    }
}

fn statically_truthy(value: &RValue) -> bool {
    matches!(
        value,
        RValue::Literal(Literal::Boolean(true) | Literal::Number(_) | Literal::String(_))
            | RValue::Table(_)
            | RValue::Closure(_)
            | RValue::Unary(crate::Unary {
                operation: UnaryOperation::Length,
                ..
            })
    )
}

fn conditional_assignment_statements(
    local: RcLocal,
    prefix: bool,
    arms: ConditionalArms,
    fallback: RValue,
) -> Vec<Statement> {
    let assign = |value| Assign::new(vec![LValue::Local(local.clone())], vec![value]).into();
    let mut else_block = Block(vec![assign(fallback)]);
    for (condition, value) in arms.into_iter().rev() {
        else_block = Block(vec![If::new(
            condition,
            Block(vec![assign(value)]),
            else_block,
        )
        .into()]);
    }
    let branch = else_block.0.pop().unwrap();
    if prefix {
        let mut declaration = Assign::new(vec![LValue::Local(local)], vec![]);
        declaration.prefix = true;
        vec![declaration.into(), branch]
    } else {
        vec![branch]
    }
}

fn declaration_concat_parts(statement: &Statement) -> Option<(RcLocal, Vec<RValue>)> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    let LValue::Local(local) = &assign.left[0] else {
        return None;
    };
    if !long_concat_spine(&assign.right[0]) || reads_rendered_name(&assign.right[0], local) {
        return None;
    }
    long_concat_parts(&assign.right[0]).map(|parts| (local.clone(), parts))
}

fn reads_rendered_name(value: &RValue, destination: &RcLocal) -> bool {
    let Some(destination_name) = destination.0 .0.lock().0.clone() else {
        return false;
    };
    value.values_read().into_iter().any(|read| {
        read.0
             .0
            .lock()
            .0
            .as_ref()
            .is_some_and(|name| name == &destination_name)
    }) || contains_global_rendered_name(value, &destination_name)
}

fn contains_global_rendered_name(value: &RValue, name: &str) -> bool {
    match value {
        RValue::Global(global) => global.0 == name.as_bytes(),
        // Moving a closure initializer below `local name` changes lexical
        // resolution inside its body too: an AST `Global("name")` would render
        // as a capture of the newly introduced local. Traverse function bodies
        // explicitly because `Closure::Traverse` intentionally stops there.
        RValue::Closure(closure) => {
            block_contains_global_rendered_name(&closure.function.lock().body, name)
        }
        _ => value
            .rvalues()
            .into_iter()
            .any(|child| contains_global_rendered_name(child, name)),
    }
}

fn block_contains_global_rendered_name(block: &Block, name: &str) -> bool {
    block.0.iter().any(|statement| {
        matches!(statement, Statement::Assign(assign) if assign.left.iter().any(|left| {
            matches!(left, LValue::Global(global) if global.0 == name.as_bytes())
        })) || crate::deinline::stmt_rvalues(statement)
            .into_iter()
            .any(|value| contains_global_rendered_name(value, name))
            || match statement {
                Statement::If(node) => {
                    block_contains_global_rendered_name(&node.then_block.lock(), name)
                        || block_contains_global_rendered_name(&node.else_block.lock(), name)
                }
                Statement::While(node) => {
                    block_contains_global_rendered_name(&node.block.lock(), name)
                }
                Statement::Repeat(node) => {
                    block_contains_global_rendered_name(&node.block.lock(), name)
                }
                Statement::NumericFor(node) => {
                    block_contains_global_rendered_name(&node.block.lock(), name)
                }
                Statement::GenericFor(node) => {
                    block_contains_global_rendered_name(&node.block.lock(), name)
                }
                _ => false,
            }
    })
}

fn return_concat_parts(statement: &Statement) -> Option<Vec<RValue>> {
    let Statement::Return(return_) = statement else {
        return None;
    };
    let [value] = return_.values.as_slice() else {
        return None;
    };
    long_concat_parts(value)
}

fn return_concat_parts_spine(statement: &Statement) -> bool {
    matches!(statement, Statement::Return(return_)
        if matches!(return_.values.as_slice(), [value] if long_concat_spine(value)))
}

/// The collector yields one part plus one per left-spine concat. Most
/// callers have an unrelated or short RHS: prove eligibility without
/// cloning any of their subtrees, stopping after four operators.
fn long_concat_spine(value: &RValue) -> bool {
    let mut spine = value;
    for _ in 0..MIN_CONCAT_OPERATORS {
        let RValue::Binary(binary) = spine else { return false; };
        if binary.operation != BinaryOperation::Concat { return false; }
        spine = &binary.left;
    }
    true
}

fn long_concat_parts(value: &RValue) -> Option<Vec<RValue>> {
    if !long_concat_spine(value) {
        return None;
    }
    let mut parts = Vec::new();
    collect_left_concat_parts(value, &mut parts);
    (parts.len() > MIN_CONCAT_OPERATORS).then_some(parts)
}

#[cfg(test)]
thread_local! { static PART_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[inline]
fn clone_rebalance_part(value: &RValue) -> RValue {
    #[cfg(test)]
    PART_CLONES.with(|count| count.set(count.get() + 1));
    value.clone()
}

fn collect_left_concat_parts(value: &RValue, parts: &mut Vec<RValue>) {
    if let RValue::Binary(binary) = value
        && binary.operation == BinaryOperation::Concat
    {
        collect_left_concat_parts(&binary.left, parts);
        parts.push(clone_rebalance_part(&binary.right));
    } else {
        parts.push(clone_rebalance_part(value));
    }
}

fn concat_statements(local: RcLocal, mut parts: Vec<RValue>, prefix: bool) -> Vec<Statement> {
    debug_assert!(parts.len() > MIN_CONCAT_OPERATORS);
    let first = parts.remove(0);
    let mut declaration = Assign::new(vec![LValue::Local(local.clone())], vec![first]);
    declaration.prefix = prefix;

    let mut statements = Vec::with_capacity(parts.len() + 1);
    statements.push(declaration.into());
    statements.extend(parts.into_iter().map(|part| {
        Assign::new(
            vec![LValue::Local(local.clone())],
            vec![Binary::new(RValue::Local(local.clone()), part, BinaryOperation::Concat).into()],
        )
        .into()
    }));
    statements
}

#[cfg(test)]
mod tests {
    use super::{rebalance_expressions, ConditionalArms, MIN_CONCAT_OPERATORS,
        collect_left_concat_parts, collect_or_terms, reads_rendered_name, statically_truthy};
    use crate::{
        Assign, Binary, BinaryOperation, Block, Closure, Function, Global, LValue, Literal, Local,
        RValue, RcLocal, Return, Statement,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    // Original eager-copy paths, including every former refusal gate.
    fn conditional_assignment_parts_reference(
        statement: &Statement,
    ) -> Option<(RcLocal, bool, ConditionalArms, RValue)> {
        let Statement::Assign(assign) = statement else {
            return None;
        };
        if assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
            return None;
        }
        let LValue::Local(local) = &assign.left[0] else {
            return None;
        };
        if assign.prefix && reads_rendered_name(&assign.right[0], local) {
            return None;
        }
        if crate::expression_budget::collapse_allowed(&assign.right[0]) {
            return None;
        }

        let mut terms = Vec::new();
        collect_or_terms(&assign.right[0], &mut terms);
        let fallback = super::clone_rebalance_part(terms.pop()?);
        // A single `condition and value or fallback` is still compact and familiar.
        // The readability cliff starts at a chained selection with at least two
        // conditions (the nested-if shape this recovery targets).
        if terms.len() < 2 {
            return None;
        }
        let mut arms = Vec::with_capacity(terms.len());
        for term in terms {
            let RValue::Binary(binary) = term else {
                return None;
            };
            if binary.operation != BinaryOperation::And || !statically_truthy(&binary.right) {
                return None;
            }
            arms.push((super::clone_rebalance_part(&binary.left), super::clone_rebalance_part(&binary.right)));
        }
        if arms.len() < 3 && crate::expression_budget::expression_cost(&fallback) < 10 {
            return None;
        }
        Some((local.clone(), assign.prefix, arms, fallback))
    }


    fn long_concat_parts_reference(value: &RValue) -> Option<Vec<RValue>> {
        let mut parts = Vec::new();
        collect_left_concat_parts(value, &mut parts);
        (parts.len() > MIN_CONCAT_OPERATORS).then_some(parts)
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn concat_chain(count: usize) -> RValue {
        (1..count).fold(string("a"), |left, index| {
            Binary::new(left, string(&index.to_string()), BinaryOperation::Concat).into()
        })
    }

    type OriginSnapshot = Option<(Vec<std::sync::Arc<crate::node_origins::Input>>, bool, bool, bool, Option<&'static str>)>;

    fn value_snapshot(value: &RValue, origins: &mut Vec<OriginSnapshot>, numbers: &mut Vec<u64>) {
        use crate::{node_origins, Traverse};
        if let Some(origin) = node_origins::value(value) {
            origins.push(origin.0.as_ref().map(|data| (data.inputs.clone(), data.inlined,
                data.cloned, data.incomplete, data.synthesized)));
        }
        if let RValue::Literal(Literal::Number(number)) = value { numbers.push(number.to_bits()); }
        value.visit_rvalues(&mut |child| { value_snapshot(child, origins, numbers); true });
    }

    fn annotate_value(value: &mut RValue, index: &mut usize) {
        use crate::{node_origins, Traverse};
        *index += 1;
        if let Some(origin) = node_origins::value_mut(value) {
            *origin = node_origins::Origin::input(node_origins::Input {
                function: "rebalance_differential".into(), block: *index / 4,
                statement: *index, value: Some(*index % 4),
            });
            let data = origin.0.as_mut().unwrap();
            data.inlined = *index & 1 != 0;
            data.cloned = *index & 2 != 0;
            data.incomplete = *index & 4 != 0;
            if *index & 8 != 0 { data.synthesized = Some("test_origin"); }
        }
        value.visit_rvalues_mut(&mut |child| { annotate_value(child, index); true });
    }

    fn measured<T>(run: impl FnOnce() -> T) -> (T, usize) {
        super::PART_CLONES.with(|count| count.set(0));
        let result = run();
        (result, super::PART_CLONES.with(|count| count.get()))
    }

    fn compare_parts(actual: &[RValue], expected: &[RValue]) {
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        let (mut actual_origins, mut actual_numbers) = (Vec::new(), Vec::new());
        let (mut expected_origins, mut expected_numbers) = (Vec::new(), Vec::new());
        for value in actual { value_snapshot(value, &mut actual_origins, &mut actual_numbers); }
        for value in expected { value_snapshot(value, &mut expected_origins, &mut expected_numbers); }
        assert_eq!(actual_origins, expected_origins, "complete origin metadata");
        assert_eq!(actual_numbers, expected_numbers, "signed zero / NaN payload bits");
    }

    #[test]
    fn borrowed_concat_gate_matches_eager_reference_shape_origins_and_clone_counts() {
        for length in 0..=9 {
            for right_associated in [false, true] {
                let mut value: RValue = crate::Call::new(Global::from("initial").into(),
                    vec![Literal::Number(f64::from_bits(0x7ff8_0000_0000_1234)).into()]).into();
                for index in 0..length {
                    let part: RValue = crate::Call::new(Global::from("part").into(),
                        vec![Literal::Number(-0.0).into(), string(&index.to_string())]).into();
                    value = if right_associated { Binary::new(part, value, BinaryOperation::Concat).into() }
                        else { Binary::new(value, part, BinaryOperation::Concat).into() };
                }
                annotate_value(&mut value, &mut 0);
                let (actual, actual_copies) = measured(|| super::long_concat_parts(&value));
                let (expected, expected_copies) = measured(|| long_concat_parts_reference(&value));
                assert_eq!(actual.is_some(), expected.is_some(), "length {length}, right {right_associated}");
                if let (Some(actual), Some(expected)) = (actual, expected) {
                    compare_parts(&actual, &expected);
                    assert_eq!(actual_copies, expected_copies, "accepted copy sequence is unchanged");
                } else {
                    assert_eq!(actual_copies, 0, "refused spines borrow their operands");
                    assert!(expected_copies > 0);
                }
            }
        }
        for width in [64, 256, 1024] {
            // This was formerly cloned in full even though its root is not a concat.
            let value: RValue = crate::Call::new(Global::from("observe").into(),
                (0..width).map(|_| Literal::String(vec![0xff; 128]).into()).collect()).into();
            let (actual, clones) = measured(|| super::long_concat_parts(&value));
            assert!(actual.is_none());
            assert_eq!(clones, 0);
            assert!(measured(|| long_concat_parts_reference(&value)).1 > 0);
        }
    }

    #[test]
    fn borrowed_conditional_gate_matches_eager_reference_and_retains_copy_origins() {
        let local = RcLocal::new(Local::new(Some("result".into())));
        let mut accepted = 0;
        let mut avoided_copies = 0;
        for seed in 0..512usize {
            let arm_count = 1 + seed % 4;
            let arms: Vec<RValue> = (0..arm_count).map(|index| {
                let condition: RValue = crate::Call::new(Global::from("choose").into(),
                    (0..8).map(|argument| string(&argument.to_string())).collect()).into();
                let value = match (seed / 4 + index) % 6 {
                    0 => Literal::Number(f64::from_bits(0x7ff8_0000_0000_1234)).into(),
                    1 => crate::Call::new(Global::from("unsupportedTruthiness").into(), vec![]).into(),
                    2 => Literal::Boolean(false).into(),
                    3 => crate::Table::new(vec![(None, Literal::Number(-0.0).into())]).into(),
                    4 => crate::Unary::new(RValue::Local(local.clone()), crate::UnaryOperation::Length).into(),
                    _ => string("selected"),
                };
                if seed % 19 == 0 && index + 1 == arm_count { condition }
                else { Binary::new(condition, value, BinaryOperation::And).into() }
            }).collect();
            let mut terms = arms.into_iter();
            let mut value = terms.next().unwrap();
            for term in terms { value = Binary::new(value, term, BinaryOperation::Or).into(); }
            let fallback = if seed % 3 == 0 { concat_chain(7) } else { string("fallback") };
            value = Binary::new(value, fallback, BinaryOperation::Or).into();
            annotate_value(&mut value, &mut 0);
            let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
            assign.prefix = seed % 2 == 0;
            let statement = Statement::Assign(assign);
            let (actual, actual_copies) = measured(|| super::conditional_assignment_parts(&statement));
            let (expected, expected_copies) = measured(|| conditional_assignment_parts_reference(&statement));
            assert_eq!(actual.is_some(), expected.is_some(), "seed {seed}");
            match (actual, expected) {
                (Some((actual_local, actual_prefix, actual_arms, actual_fallback)),
                    Some((expected_local, expected_prefix, expected_arms, expected_fallback))) => {
                    accepted += 1;
                    assert_eq!(actual_local, expected_local);
                    assert_eq!(actual_prefix, expected_prefix);
                    assert_eq!(actual_copies, expected_copies, "seed {seed}: accepted clones");
                    let actual: Vec<_> = actual_arms.into_iter().flat_map(|(condition, value)| [condition, value])
                        .chain(std::iter::once(actual_fallback)).collect();
                    let expected: Vec<_> = expected_arms.into_iter().flat_map(|(condition, value)| [condition, value])
                        .chain(std::iter::once(expected_fallback)).collect();
                    compare_parts(&actual, &expected);
                }
                (None, None) => {
                    assert_eq!(actual_copies, 0, "seed {seed}: no speculative copies");
                    avoided_copies += expected_copies;
                }
                _ => unreachable!(),
            }
        }
        assert!(accepted > 0);
        assert!(avoided_copies > 0);
    }

    #[test]
    fn long_return_concat_becomes_named_compound_updates() {
        let mut block = Block(vec![Return::new(vec![concat_chain(6)]).into()]);

        rebalance_expressions(&mut block);

        assert_eq!(
            block.to_string(),
            "local text = \"a\"\ntext ..= \"1\"\ntext ..= \"2\"\ntext ..= \"3\"\ntext ..= \"4\"\ntext ..= \"5\"\nreturn text"
        );
        let once = block.to_string();
        rebalance_expressions(&mut block);
        assert_eq!(block.to_string(), once);
    }

    #[test]
    fn long_decl_concat_reuses_existing_local() {
        let text = RcLocal::new(Local::new(Some("summary".into())));
        let mut declaration = Assign::new(vec![LValue::Local(text)], vec![concat_chain(5)]);
        declaration.prefix = true;
        let mut block = Block(vec![declaration.into()]);

        rebalance_expressions(&mut block);

        assert_eq!(
            block.to_string(),
            "local summary = \"a\"\nsummary ..= \"1\"\nsummary ..= \"2\"\nsummary ..= \"3\"\nsummary ..= \"4\""
        );
    }

    #[test]
    fn short_concat_stays_inline() {
        let mut block = Block(vec![Return::new(vec![concat_chain(4)]).into()]);

        rebalance_expressions(&mut block);

        assert_eq!(block.0.len(), 1);
    }

    #[test]
    fn oversized_truthy_select_chain_becomes_if_elseif() {
        let result = RcLocal::new(Local::new(Some("sizeFactor".into())));
        let first_condition = RValue::Global(crate::Global::from("invalidAverage"));
        let second_condition = RValue::Global(crate::Global::from("belowAverage"));
        let mut fallback = string("fallback0");
        for index in 1..=12 {
            fallback = Binary::new(
                fallback,
                string(&format!("fallback{index}")),
                BinaryOperation::Concat,
            )
            .into();
        }
        let expression = Binary::new(
            Binary::new(
                Binary::new(
                    first_condition,
                    RValue::Literal(Literal::Number(3.0)),
                    BinaryOperation::And,
                )
                .into(),
                Binary::new(
                    second_condition,
                    RValue::Literal(Literal::Number(1.0)),
                    BinaryOperation::And,
                )
                .into(),
                BinaryOperation::Or,
            )
            .into(),
            fallback,
            BinaryOperation::Or,
        )
        .into();
        let mut declaration = Assign::new(vec![LValue::Local(result)], vec![expression]);
        declaration.prefix = true;
        let mut block = Block(vec![declaration.into()]);

        rebalance_expressions(&mut block);

        let output = block.to_string();
        assert!(
            output.starts_with("local sizeFactor\n\nif invalidAverage then"),
            "{output}"
        );
        assert!(output.contains("elseif belowAverage then"), "{output}");
        assert!(output.contains("sizeFactor = 3"), "{output}");
        assert!(output.contains("sizeFactor = 1"), "{output}");
    }

    #[test]
    fn conditional_chain_with_falsy_selected_arm_stays_expression() {
        let result = RcLocal::new(Local::new(Some("value".into())));
        let mut fallback = string("fallback0");
        for index in 1..=20 {
            fallback = Binary::new(
                fallback,
                string(&format!("fallback{index}")),
                BinaryOperation::Concat,
            )
            .into();
        }
        let expression = Binary::new(
            Binary::new(
                RValue::Global(crate::Global::from("condition")),
                RValue::Literal(Literal::Boolean(false)),
                BinaryOperation::And,
            )
            .into(),
            fallback,
            BinaryOperation::Or,
        )
        .into();
        let mut declaration = Assign::new(vec![LValue::Local(result)], vec![expression]);
        declaration.prefix = true;
        let mut block = Block(vec![declaration.into()]);

        rebalance_expressions(&mut block);

        assert_eq!(block.0.len(), 1);
    }

    #[test]
    fn concat_split_refuses_shadowed_outer_name_read() {
        let destination = RcLocal::new(Local::new(Some("text".into())));
        let outer = RcLocal::new(Local::new(Some("text".into())));
        let mut expression = string("prefix");
        for _ in 0..4 {
            expression = Binary::new(
                expression,
                RValue::Local(outer.clone()),
                BinaryOperation::Concat,
            )
            .into();
        }
        let mut declaration = Assign::new(vec![LValue::Local(destination)], vec![expression]);
        declaration.prefix = true;
        let mut block = Block(vec![declaration.into()]);

        rebalance_expressions(&mut block);

        assert_eq!(block.0.len(), 1);
    }

    #[test]
    fn concat_split_refuses_shadowed_global_name_read() {
        let destination = RcLocal::new(Local::new(Some("text".into())));
        let mut expression = string("prefix");
        for _ in 0..4 {
            expression = Binary::new(
                expression,
                RValue::Global(crate::Global::from("text")),
                BinaryOperation::Concat,
            )
            .into();
        }
        let mut declaration = Assign::new(vec![LValue::Local(destination)], vec![expression]);
        declaration.prefix = true;
        let mut block = Block(vec![declaration.into()]);

        rebalance_expressions(&mut block);

        assert_eq!(block.0.len(), 1);
        assert!(block.to_string().starts_with("local text ="));
    }

    #[test]
    fn conditional_split_refuses_shadowed_global_name_read() {
        let destination = RcLocal::new(Local::new(Some("result".into())));
        let arm = |condition: &str, value: f64| {
            Binary::new(
                RValue::Global(crate::Global::from(condition)),
                RValue::Literal(Literal::Number(value)),
                BinaryOperation::And,
            )
            .into()
        };
        let mut fallback = RValue::Global(crate::Global::from("result"));
        for index in 0..12 {
            fallback = Binary::new(
                fallback,
                string(&index.to_string()),
                BinaryOperation::Concat,
            )
            .into();
        }
        let expression = Binary::new(
            Binary::new(arm("first", 1.0), arm("second", 2.0), BinaryOperation::Or).into(),
            fallback,
            BinaryOperation::Or,
        )
        .into();
        let mut declaration = Assign::new(vec![LValue::Local(destination)], vec![expression]);
        declaration.prefix = true;
        let mut block = Block(vec![declaration.into()]);

        rebalance_expressions(&mut block);

        assert_eq!(block.0.len(), 1);
    }

    #[test]
    fn conditional_split_refuses_global_write_inside_selected_closure() {
        let destination = RcLocal::new(Local::new(Some("result".into())));
        let closure = RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function {
                body: Block(vec![Assign::new(
                    vec![LValue::Global(Global::from("result"))],
                    vec![RValue::Literal(Literal::Number(1.0))],
                )
                .into()]),
                ..Function::default()
            }))),
            upvalues: vec![],
        });
        let first = Binary::new(
            RValue::Global(Global::from("first")),
            closure.clone(),
            BinaryOperation::And,
        )
        .into();
        let second = Binary::new(
            RValue::Global(Global::from("second")),
            closure,
            BinaryOperation::And,
        )
        .into();
        let mut fallback = string("fallback");
        for index in 0..12 {
            fallback = Binary::new(
                fallback,
                string(&index.to_string()),
                BinaryOperation::Concat,
            )
            .into();
        }
        let expression = Binary::new(
            Binary::new(first, second, BinaryOperation::Or).into(),
            fallback,
            BinaryOperation::Or,
        )
        .into();
        let mut declaration = Assign::new(vec![LValue::Local(destination)], vec![expression]);
        declaration.prefix = true;
        let mut block = Block(vec![declaration.into()]);

        rebalance_expressions(&mut block);

        assert_eq!(block.0.len(), 1);
    }

    #[test]
    fn return_concat_refuses_new_local_without_register_headroom() {
        let mut statements = (0..200)
            .map(|index| {
                let local = RcLocal::new(Local::new(Some(format!("value{index}"))));
                let mut declaration =
                    Assign::new(vec![LValue::Local(local)], vec![string("value")]);
                declaration.prefix = true;
                declaration.into()
            })
            .collect::<Vec<_>>();
        statements.push(Return::new(vec![concat_chain(6)]).into());
        let mut block = Block(statements);

        rebalance_expressions(&mut block);

        assert_eq!(block.0.len(), 201);
        assert!(matches!(block.0.last(), Some(crate::Statement::Return(_))));
    }
}
