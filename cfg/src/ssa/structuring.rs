use ast::{LocalRw, Reduce, SideEffects, Traverse, UnaryOperation};

use itertools::Itertools;
use petgraph::{
    algo::dominators::Dominators,
    stable_graph::{EdgeIndex, NodeIndex},
    visit::{DfsPostOrder, EdgeRef},
    Direction,
};
use rustc_hash::FxHashMap;
use tuple::Map;

use crate::{
    block::{BlockEdge, BranchType},
    function::Function,
};

#[derive(Debug)]
pub enum PatternOperator {
    And,
    Or,
}

impl From<PatternOperator> for ast::BinaryOperation {
    fn from(val: PatternOperator) -> Self {
        match val {
            PatternOperator::And => ast::BinaryOperation::And,
            PatternOperator::Or => ast::BinaryOperation::Or,
        }
    }
}

#[derive(Debug)]
pub struct ConditionalAssignmentPattern {
    assigner: NodeIndex,
    next: NodeIndex,
    tested_local: ast::RcLocal,
    assigned_local: ast::RcLocal,
    assigned_value: ast::RValue,
    parameter: ast::RcLocal,
    operator: PatternOperator,
}

type ConditionalSequenceConfiguration = (bool, bool);

#[derive(Debug)]
pub struct ConditionalSequencePattern {
    first_node: NodeIndex,
    second_node: NodeIndex,
    short_circuit: NodeIndex,
    assign: bool,
    inverted: bool,
    final_condition: ast::RValue,
}

#[derive(Debug)]
pub struct GenericForNextPattern {
    body_node: NodeIndex,
    res_locals: Vec<ast::RcLocal>,
    // generator is not necessarily a local in Luau
    // it is commonly something like: `generator or __get_builtin("iter")`
    generator: ast::RValue,
    state: ast::RcLocal,
    internal_control: ast::RcLocal,
}

fn simplify_condition(function: &mut Function, node: NodeIndex) -> bool {
    let block = function.block_mut(node).unwrap();
    if let Some(if_stat) = block.last_mut().and_then(|s| s.as_if_mut()) {
        if let Some(unary) = if_stat.condition.as_unary()
            && unary.operation == UnaryOperation::Not
        {
            if_stat.condition = *unary.value.clone();
            let (then_edge, else_edge) = function.conditional_edges(node).unwrap().map(|e| e.id());
            let (then_edge, else_edge) = function.graph_mut().index_twice_mut(then_edge, else_edge);
            then_edge.branch_type = BranchType::Else;
            else_edge.branch_type = BranchType::Then;
            return true;
        } else if let Some(binary) = if_stat.condition.as_binary() {
            if binary.left.as_literal().is_some() && binary.right.as_literal().is_none() {
                if_stat.condition = ast::Binary::new(
                    *binary.right.clone(),
                    *binary.left.clone(),
                    match binary.operation {
                        ast::BinaryOperation::Equal => ast::BinaryOperation::Equal,
                        ast::BinaryOperation::NotEqual => ast::BinaryOperation::NotEqual,
                        ast::BinaryOperation::LessThan => ast::BinaryOperation::GreaterThan,
                        ast::BinaryOperation::LessThanOrEqual => {
                            ast::BinaryOperation::GreaterThanOrEqual
                        }
                        ast::BinaryOperation::GreaterThan => ast::BinaryOperation::LessThan,
                        ast::BinaryOperation::GreaterThanOrEqual => {
                            ast::BinaryOperation::LessThanOrEqual
                        }
                        _ => return false,
                    },
                )
                .into();
                return true;
            }
        }
    }
    false
}

fn single_assign(block: &ast::Block) -> Option<&ast::Assign> {
    if block.len() == 1
        && let Some(assign) = block.last().unwrap().as_assign()
        && assign.left.len() == 1
    {
        Some(assign)
    } else {
        None
    }
}

fn match_conditional_sequence(
    function: &Function,
    node: NodeIndex,
) -> Option<ConditionalSequencePattern> {
    // TODO: check if len() == 1?
    let block = function.block(node).unwrap();
    if let Some(r#if) = block.last().and_then(|s| s.as_if()) {
        let first_condition = r#if.condition.clone();
        let test_pattern = |second_conditional, other, other_args: FxHashMap<_, _>| {
            let second_conditional_successors = function.edges(second_conditional).collect_vec();
            let second_block = function.block(second_conditional).unwrap();
            if let Some(second_conditional_if) = second_block.last().and_then(|s| s.as_if()) {
                if second_conditional_successors.len() == 2
                    && let Ok(edge_to_other) = second_conditional_successors
                        .iter()
                        .filter(|&s| {
                            s.target() == other
                                && s.weight()
                                    .arguments
                                    .iter()
                                    .all(|(p, _)| other_args.contains_key(p))
                        })
                        .exactly_one()
                {
                    if second_block.len() == 2 {
                        if let ast::Statement::Assign(assign) = &second_block[0] {
                            // TODO: make sure this variable isnt used anywhere but this block
                            // and the args passed to other.
                            let values_written = assign.values_written();
                            if values_written.len() == 1
                                && second_conditional_if.condition
                                    == values_written[0].clone().into()
                            {
                                let valid = if other_args.len() == 1
                                    && let Ok((_, ast::RValue::Local(local))) =
                                        edge_to_other.weight().arguments.iter().exactly_one()
                                    && local == values_written[0]
                                {
                                    true
                                } else {
                                    other_args.is_empty()
                                };
                                if valid {
                                    assert!(assign.right.len() == 1);
                                    return Some((assign.right[0].clone(), true));
                                }
                            }
                        }
                        return None;
                    } else if second_block.len() == 1
                        && edge_to_other
                            .weight()
                            .arguments
                            .iter()
                            .all(|(k, v)| other_args.get(k).is_some_and(|rv| rv == v))
                    {
                        return Some((second_conditional_if.condition.clone(), false));
                    }
                }
            }
            None
        };
        let first_terminator = function.conditional_edges(node).unwrap();
        let (then_edge, else_edge) = first_terminator;
        if function.predecessor_blocks(then_edge.target()).count() == 1
            && then_edge.weight().arguments.is_empty()
            && let else_args = else_edge
                .weight()
                .arguments
                .iter()
                .cloned()
                .collect::<FxHashMap<_, _>>()
            && let Some((second_condition, assign)) =
                test_pattern(then_edge.target(), else_edge.target(), else_args)
        {
            let second_terminator = function.conditional_edges(then_edge.target()).unwrap();
            if second_terminator.0.target() == else_edge.target() {
                Some(ConditionalSequencePattern {
                    first_node: node,
                    second_node: then_edge.target(),
                    short_circuit: else_edge.target(),
                    assign,
                    inverted: true,
                    final_condition: ast::Binary::new(
                        ast::Unary::new(first_condition, ast::UnaryOperation::Not).into(),
                        second_condition,
                        ast::BinaryOperation::Or,
                    )
                    .into(),
                })
            } else {
                Some(ConditionalSequencePattern {
                    first_node: node,
                    second_node: then_edge.target(),
                    short_circuit: else_edge.target(),
                    assign,
                    inverted: false,
                    final_condition: ast::Binary::new(
                        first_condition,
                        second_condition,
                        ast::BinaryOperation::And,
                    )
                    .into(),
                })
            }
        } else if function.predecessor_blocks(else_edge.target()).count() == 1
            && else_edge.weight().arguments.is_empty()
            && let then_args = then_edge
                .weight()
                .arguments
                .iter()
                .cloned()
                .collect::<FxHashMap<_, _>>()
            && let Some((second_condition, assign)) =
                test_pattern(else_edge.target(), then_edge.target(), then_args)
        {
            let second_terminator = function.conditional_edges(else_edge.target()).unwrap();
            if first_terminator.0.target() == second_terminator.0.target() {
                Some(ConditionalSequencePattern {
                    first_node: node,
                    second_node: else_edge.target(),
                    short_circuit: then_edge.target(),
                    assign,
                    inverted: false,
                    final_condition: ast::Binary::new(
                        first_condition,
                        second_condition,
                        ast::BinaryOperation::Or,
                    )
                    .into(),
                })
            } else {
                Some(ConditionalSequencePattern {
                    first_node: node,
                    second_node: else_edge.target(),
                    short_circuit: then_edge.target(),
                    assign,
                    inverted: true,
                    final_condition: ast::Binary::new(
                        ast::Unary::new(first_condition, ast::UnaryOperation::Not).into(),
                        second_condition,
                        ast::BinaryOperation::And,
                    )
                    .into(),
                })
            }
        } else {
            None
        }
    } else {
        None
    }
}

pub fn structure_conditionals(function: &mut Function) -> bool {
    let mut did_structure = false;
    // TODO: does this need to be in dfs post order?
    let mut dfs = DfsPostOrder::new(function.graph(), function.entry().unwrap());
    while let Some(node) = dfs.next(function.graph()) {
        if simplify_condition(function, node) {
            did_structure = true;
        }
        if structure_bool_conditional(function, node) {
            did_structure = true;
        }

        if let Some(pattern) = match_conditional_sequence(function, node)
            // TODO: can we continue?
            && &Some(pattern.second_node) != function.entry()
        {
            let second_to_sc_edges = function
                .edges(pattern.second_node)
                .filter(|e| e.target() == pattern.short_circuit)
                .collect::<Vec<_>>();
            assert!(second_to_sc_edges.len() == 1);
            let second_to_sc_args = second_to_sc_edges[0].weight().arguments.clone();
            let first_to_sc_edges = function
                .edges(pattern.first_node)
                .filter(|e| e.target() == pattern.short_circuit)
                .collect::<Vec<_>>();
            assert!(first_to_sc_edges.len() == 1);
            let first_to_sc_edge = first_to_sc_edges[0].id();
            // Merging `first` and `second` collapses both of their edges to the
            // short-circuit node into a single edge, so the merged edge can only
            // carry ONE set of arguments. The rewrite below overwrites `first`'s
            // arguments with `second`'s. That is only sound when `first`'s value
            // for a shared parameter is the same as `second`'s (a genuine
            // short-circuit) — otherwise `first` carried a *distinct* early-exit
            // result that would be silently destroyed.
            //
            // The signature of such a distinct early exit is a constant literal
            // on `first`'s edge that differs from `second`'s value, e.g.
            // `if (A or B) then return true` lifts to `first -> sc(X = true)`
            // while the fall-through computes `X = CanCollide and ...`. Clobbering
            // `X = true` turns `(A or B) or rest` into `not (A or B) and rest`
            // (observed: QuickTeleport `isGroundHit` excluding floor-hitbox parts).
            // When that happens, bail out and leave the conditional for
            // `structure_bool_conditional` / `make_bool_conditional`, which
            // reconstruct it as a correct `or`/`and`.
            let clobber_destroys_value = function
                .graph()
                .edge_weight(first_to_sc_edge)
                .unwrap()
                .arguments
                .iter()
                .any(|(k, v)| {
                    matches!(v, ast::RValue::Literal(_))
                        && second_to_sc_args
                            .iter()
                            .find(|(k2, _)| k2 == k)
                            .is_some_and(|(_, v2)| v2 != v)
                });
            if !clobber_destroys_value {
                for arg in &mut function
                    .graph_mut()
                    .edge_weight_mut(first_to_sc_edge)
                    .unwrap()
                    .arguments
                {
                    if let Some(new_arg) = second_to_sc_args.iter().find(|(k, _)| k == &arg.0) {
                        *arg = new_arg.clone();
                    }
                }

                let second_terminator = function.conditional_edges(pattern.second_node).unwrap();
                let other_edge = if second_terminator.0.target() == pattern.short_circuit {
                    second_terminator.1
                } else {
                    second_terminator.0
                };
                let other_edge = other_edge.id();
                assert!(skip_over_node(function, pattern.first_node, other_edge));

                let mut removed_block = function.remove_block(pattern.second_node).unwrap();
                let first_node = pattern.first_node;
                if pattern.assign {
                    let assign = removed_block.first_mut().unwrap().as_assign_mut().unwrap();
                    assign.right = vec![pattern.final_condition.reduce()];
                } else {
                    let removed_if = removed_block.last_mut().unwrap().as_if_mut().unwrap();
                    removed_if.condition = pattern.final_condition.reduce_condition();
                }
                if pattern.inverted {
                    let removed_if = removed_block.last_mut().unwrap().as_if_mut().unwrap();
                    // TODO: unnecessary clone?
                    removed_if.condition =
                        ast::Unary::new(removed_if.condition.clone(), UnaryOperation::Not)
                            .reduce_condition();
                }
                let first_block = function.block_mut(first_node).unwrap();
                first_block.pop();
                first_block.extend(removed_block.0);
                did_structure = true;
            }
        }

        did_structure |= try_remove_unnecessary_condition(function, node);
    }

    did_structure
}

// TODO: REFACTOR: move to ast
// None = unknown
fn is_truthy(rvalue: ast::RValue) -> Option<bool> {
    match rvalue.reduce_condition() {
        // __len has to return number, but __unm can return any value
        ast::RValue::Unary(ast::Unary {
            operation: ast::UnaryOperation::Length,
            ..
        }) => Some(true),
        ast::RValue::Literal(
            ast::Literal::Boolean(true) | ast::Literal::Number(_) | ast::Literal::String(_),
        )
        | ast::RValue::Table(_)
        | ast::RValue::Closure(_) => Some(true),
        ast::RValue::Literal(ast::Literal::Nil | ast::Literal::Boolean(_)) => Some(false),
        _ => None,
    }
}

// TODO: STYLE: rename
fn make_bool_conditional(
    function: &mut Function,
    node: NodeIndex,
    mut then_value: ast::RValue,
    mut else_value: ast::RValue,
) -> Option<ast::RValue> {
    let block = function.block_mut(node).unwrap();
    let r#if = block.last_mut().unwrap().as_if_mut().unwrap();
    if let ast::RValue::Literal(ast::Literal::Boolean(then_value)) = then_value
        && let ast::RValue::Literal(ast::Literal::Boolean(else_value)) = else_value
        && then_value != else_value
    {
        let cond = ast::Unary::new(
            std::mem::replace(&mut r#if.condition, ast::Literal::Nil.into()),
            ast::UnaryOperation::Not,
        );
        let cond = if then_value {
            ast::Unary::new(cond.into(), ast::UnaryOperation::Not)
        } else {
            cond
        };
        Some(cond.reduce())
    } else {
        // If the then-value is exactly the condition, the whole `if` collapses to
        // `condition or else_value`: when the condition is truthy it evaluates to
        // then_value (which *is* the condition), so the result is then_value;
        // otherwise it is else_value. This is the clean form for
        // `x = (a and b and c) or d`-style code, and it handles whole `and`-chains
        // that the right-operand-only check below misses.
        //
        // The condition is used here in value position (exactly as the original
        // then-branch used it), so it stays a plain value-preserving `reduce()`,
        // never `reduce_condition()`. Emitting `condition or else_value` directly
        // (rather than routing it through the `then_truthy` path, which would build
        // `not(cond) and X or cond` and lean on the `X and X` collapse rule) keeps
        // De Morgan chains clean: `(not a and not b) or d` reduces to
        // `not (a or b) or d`, not the verbose `not (a or b or (a or b)) or d`.
        //
        // The cheap discriminant/id `==` is tested before the recursive
        // `has_side_effects` walk so the common non-matching call short-circuits
        // without walking. The guard ensures the condition (== then_value) is
        // effect-free, so evaluating it once here matches the original.
        if r#if.condition == then_value && !then_value.has_side_effects() {
            let condition = std::mem::replace(&mut r#if.condition, ast::Literal::Nil.into());
            return Some(
                ast::Binary::new(condition, else_value, ast::BinaryOperation::Or).reduce(),
            );
        }
        // Symmetric case: when the else-value is the condition, `if c then X else c`
        // collapses to `c and X` (c truthy -> X; c falsy -> short-circuits to c).
        // Unlike the then-case, X need not be truthy — `and` yields its right
        // operand verbatim when c is truthy. Same value-position / effect-free
        // reasoning as above (the original evaluates c twice on the falsy path, this
        // once), and the cheap `==` gates the recursive side-effect walk.
        if r#if.condition == else_value && !else_value.has_side_effects() {
            let condition = std::mem::replace(&mut r#if.condition, ast::Literal::Nil.into());
            return Some(
                ast::Binary::new(condition, then_value, ast::BinaryOperation::And).reduce(),
            );
        }
        // TODO: for `v0 and v1 and v2` only the right operand v2 (and the whole
        // chain, handled above) is recognised as truthy; inner left operands are
        // intentionally not — `(a and b) and a or c` would not collapse and reads
        // worse than the original if/else.
        let then_truthy = match is_truthy(then_value.clone()) {
            Some(truthy) => truthy,
            None if !then_value.has_side_effects() => {
                let value = match &r#if.condition {
                    ast::RValue::Binary(ast::Binary {
                        right: box ref value,
                        operation: ast::BinaryOperation::And,
                        ..
                    }) => value,
                    value => value,
                };
                !value.has_side_effects() && *value == then_value
            }
            None => false,
        };
        // TODO: if condition is `and not else_value` or `not else_value` then truthy?
        let else_truthy = is_truthy(else_value.clone()).is_some_and(|v| v);
        let cond = if !then_truthy && !else_truthy {
            return None;
        } else if !then_truthy {
            std::mem::swap(&mut then_value, &mut else_value);
            ast::Unary::new(
                std::mem::replace(&mut r#if.condition, ast::Literal::Nil.into()),
                ast::UnaryOperation::Not,
            )
            .reduce_condition()
        } else if !else_truthy {
            std::mem::replace(&mut r#if.condition, ast::Literal::Nil.into()).reduce_condition()
        } else {
            let cond =
                std::mem::replace(&mut r#if.condition, ast::Literal::Nil.into()).reduce_condition();
            if let ast::RValue::Unary(ast::Unary {
                box value,
                operation: ast::UnaryOperation::Not,
            }) = cond
            {
                std::mem::swap(&mut then_value, &mut else_value);
                value
            } else {
                cond
            }
        };

        Some(
            ast::Binary::new(
                ast::Binary::new(cond, then_value, ast::BinaryOperation::And).into(),
                else_value,
                ast::BinaryOperation::Or,
            )
            .reduce(),
        )
    }
}

// TODO: `return if g then true else false` in luau?
// local a; if g then a = true else a = false end; return a -> return g and true or false
// local a; if g then a = false else a = true end; return a -> return not g
// local a; if g == 1 then a = true else a = false end; return a -> return g == 1
fn structure_bool_conditional(function: &mut Function, node: NodeIndex) -> bool {
    let match_triangle = |assigner, next, next_args: FxHashMap<ast::RcLocal, ast::RValue>| {
        if let Some(edge_to_next) = function.unconditional_edge(assigner)
            && edge_to_next.target() == next
            && edge_to_next.weight().arguments.iter().all(|(p, _)| next_args.contains_key(p))
            && let Some(assign) = single_assign(function.block(assigner).unwrap())
            // TODO: allow multiple unused (excl. first) locals in left
            && assign.left.len() == 1 && assign.right.len() == 1
            && let ast::LValue::Local(assigned_local) = &assign.left[0]
            && next_args.len() == 1 && let Ok((param, ast::RValue::Local(arg))) = edge_to_next.weight().arguments.iter().exactly_one()
            && arg == assigned_local
        {
            // TODO: make sure assigned_local is only used in the assigner and it's params to next
            // TODO: unnecessary clone
            Some((param, assign.right[0].clone(), next_args[param].clone()))
        } else {
            None
        }
    };

    if let Some(ast::Statement::If(_)) = function.block(node).unwrap().last() {
        let (then_edge, else_edge) = function.conditional_edges(node).unwrap();
        if then_edge.target() == else_edge.target() {
            if let Ok((res_local, then_value, else_value)) = then_edge
                .weight()
                .arguments
                .iter()
                .filter_map(|(p, a)| {
                    else_edge
                        .weight()
                        .arguments
                        .iter()
                        .find(|(p1, _)| p == p1)
                        .map(|(_, a1)| (p, a, a1))
                })
                .exactly_one()
            {
                let (then_edge, else_edge) = (then_edge.id(), else_edge.id());
                // TODO: unnecessary clones
                let res_local = res_local.clone();
                let then_value = then_value.clone();
                let else_value = else_value.clone();

                if let Some(res) = make_bool_conditional(function, node, then_value, else_value) {
                    function
                        .graph_mut()
                        .edge_weight_mut(then_edge)
                        .unwrap()
                        .arguments[0]
                        .1 = res_local.clone().into();
                    function
                        .graph_mut()
                        .edge_weight_mut(else_edge)
                        .unwrap()
                        .arguments[0]
                        .1 = res_local.clone().into();
                    let block = function.block_mut(node).unwrap();
                    let r#if = block.last_mut().unwrap().as_if_mut().unwrap();
                    r#if.condition = res_local.clone().into();
                    let pos = block.len() - 1;
                    block.insert(
                        pos,
                        ast::Assign::new(vec![res_local.into()], vec![res]).into(),
                    );
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else if then_edge.weight().arguments.is_empty()
            && function
                .predecessor_blocks(then_edge.target())
                .exactly_one()
                .is_ok()
            && let else_args = else_edge
                .weight()
                .arguments
                .iter()
                .cloned()
                .collect::<FxHashMap<_, _>>()
            && let Some((res_local, then_value, else_value)) =
                match_triangle(then_edge.target(), else_edge.target(), else_args)
        {
            let then_block = then_edge.target();
            let (then_edge, else_edge) = (
                function.unconditional_edge(then_block).unwrap().id(),
                else_edge.id(),
            );
            let res_local = res_local.clone();
            if let Some(res) = make_bool_conditional(function, node, then_value, else_value) {
                function
                    .graph_mut()
                    .edge_weight_mut(then_edge)
                    .unwrap()
                    .arguments[0]
                    .1 = res_local.clone().into();
                function
                    .graph_mut()
                    .edge_weight_mut(else_edge)
                    .unwrap()
                    .arguments[0]
                    .1 = res_local.clone().into();
                skip_over_node(function, node, then_edge);
                if function.predecessor_blocks(then_block).next().is_none() {
                    function.remove_block(then_block);
                }
                let block = function.block_mut(node).unwrap();
                let r#if = block.last_mut().unwrap().as_if_mut().unwrap();
                r#if.condition = res_local.clone().into();
                let pos = block.len() - 1;
                block.insert(
                    pos,
                    ast::Assign::new(vec![res_local.into()], vec![res]).into(),
                );
                true
            } else {
                false
            }
        } else if else_edge.weight().arguments.is_empty()
            && function
                .predecessor_blocks(else_edge.target())
                .exactly_one()
                .is_ok()
            && let then_args = then_edge
                .weight()
                .arguments
                .iter()
                .cloned()
                .collect::<FxHashMap<_, _>>()
            && let Some((res_local, else_value, then_value)) =
                match_triangle(else_edge.target(), then_edge.target(), then_args)
        {
            let else_block = else_edge.target();
            let (then_edge, else_edge) = (
                then_edge.id(),
                function.unconditional_edge(else_block).unwrap().id(),
            );
            let res_local = res_local.clone();
            if let Some(res) = make_bool_conditional(function, node, then_value, else_value) {
                function
                    .graph_mut()
                    .edge_weight_mut(then_edge)
                    .unwrap()
                    .arguments[0]
                    .1 = res_local.clone().into();
                function
                    .graph_mut()
                    .edge_weight_mut(else_edge)
                    .unwrap()
                    .arguments[0]
                    .1 = res_local.clone().into();
                skip_over_node(function, node, else_edge);
                if function.predecessor_blocks(else_block).next().is_none() {
                    function.remove_block(else_block);
                }
                let block = function.block_mut(node).unwrap();
                let r#if = block.last_mut().unwrap().as_if_mut().unwrap();
                r#if.condition = res_local.clone().into();
                let pos = block.len() - 1;
                block.insert(
                    pos,
                    ast::Assign::new(vec![res_local.into()], vec![res]).into(),
                );
                true
            } else {
                false
            }
        } else if function.predecessor_blocks(then_edge.target()).exactly_one().is_ok()
            && let Ok(then_next) = function.successor_blocks(then_edge.target()).exactly_one()
            && function.predecessor_blocks(else_edge.target()).exactly_one().is_ok()
            && let Ok(else_next) = function.successor_blocks(else_edge.target()).exactly_one()
            && then_next == else_next
            && let Some(then_assign) = single_assign(function.block(then_edge.target()).unwrap())
            // TODO: allow multiple unused (excl. first) locals in left
            && then_assign.left.len() == 1 && then_assign.right.len() == 1
            && let Some(else_assign) = single_assign(function.block(else_edge.target()).unwrap())
            // TODO: allow multiple unused (excl. first) locals in left
            && else_assign.left.len() == 1 && else_assign.right.len() == 1
            && let Ok((then_param, ast::RValue::Local(then_arg))) = then_edge.weight().arguments.iter().exactly_one()
            && let Ok((else_param, ast::RValue::Local(else_arg))) = else_edge.weight().arguments.iter().exactly_one()
            && then_param == else_param
            && then_assign.left[0].as_local() == Some(then_arg)
            && else_assign.left[0].as_local() == Some(else_arg)
        {
            // TODO: make sure then_arg and else_arg arent used outside their respective assigner blocks
            // and the arguments passed to next
            let res_local = then_param.clone();
            let then_value = then_assign.right[0].clone();
            let else_value = else_assign.right[0].clone();
            let then_block = then_edge.target();
            let else_block = else_edge.target();
            let (then_edge, else_edge) = (
                function.unconditional_edge(then_block).unwrap().id(),
                function.unconditional_edge(else_block).unwrap().id(),
            );
            if let Some(res) = make_bool_conditional(function, node, then_value, else_value) {
                function
                    .graph_mut()
                    .edge_weight_mut(then_edge)
                    .unwrap()
                    .arguments[0]
                    .1 = res_local.clone().into();
                function
                    .graph_mut()
                    .edge_weight_mut(else_edge)
                    .unwrap()
                    .arguments[0]
                    .1 = res_local.clone().into();
                skip_over_node(function, node, then_edge);
                if function.predecessor_blocks(then_block).next().is_none() {
                    function.remove_block(then_block);
                }
                skip_over_node(function, node, else_edge);
                if function.predecessor_blocks(else_block).next().is_none() {
                    function.remove_block(else_block);
                }
                let block = function.block_mut(node).unwrap();
                let r#if = block.last_mut().unwrap().as_if_mut().unwrap();
                r#if.condition = res_local.clone().into();
                let pos = block.len() - 1;
                block.insert(
                    pos,
                    ast::Assign::new(vec![res_local.into()], vec![res]).into(),
                );
                true
            } else {
                false
            }
        } else if let (then_target, else_target) = (then_edge.target(), else_edge.target())
            && function
                .predecessor_blocks(then_target)
                .exactly_one()
                .is_ok()
            && function
                .predecessor_blocks(else_target)
                .exactly_one()
                .is_ok()
            && function.successor_blocks(then_target).next().is_none()
            && function.successor_blocks(else_target).next().is_none()
            && let Ok(ast::Statement::Return(ast::Return {
                values: then_values,
            })) = function.block(then_target).unwrap().iter().exactly_one()
            && let Ok(then_value) = then_values.iter().exactly_one()
            && let Ok(ast::Statement::Return(ast::Return {
                values: else_values,
            })) = function.block(else_target).unwrap().iter().exactly_one()
            && let Ok(else_value) = else_values.iter().exactly_one()
        {
            // TODO: unnecessary clones
            let then_value = then_value.clone();
            let else_value = else_value.clone();

            if let Some(res) = make_bool_conditional(function, node, then_value, else_value) {
                function.remove_block(then_target);
                function.remove_block(else_target);
                let block = function.block_mut(node).unwrap();
                block.pop();
                block.push(ast::Return::new(vec![res]).into());
                true
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    }
    //todo!();
}

fn match_method_call(call: &ast::Call) -> Option<(&ast::RValue, &str)> {
    // TODO: make sure `a:method with space()` doesnt happen
    if !call.arguments.is_empty()
        && !call.arguments[0].has_side_effects()
        && let Some(ast::Index {
            box left,
            right: box ast::RValue::Literal(ast::Literal::String(index)),
        }) = call.value.as_index()
        && left == &call.arguments[0]
    {
        if let Ok(index) = std::str::from_utf8(index) {
            Some((left, index))
        } else {
            None
        }
    } else {
        None
    }
}

// This code does not apply to Luau
pub fn structure_method_calls(function: &mut Function) -> bool {
    let mut did_structure = false;
    for block in function.blocks_mut() {
        for stat in &mut block.0 {
            if let ast::Statement::Call(call) = stat {
                if let Some((value, method)) = match_method_call(call) {
                    *stat = ast::MethodCall::new(
                        value.clone(),
                        method.to_string(),
                        call.arguments.drain(1..).collect(),
                    )
                    .into();
                    did_structure = true;
                }
            }
            stat.traverse_rvalues(&mut |rvalue| {
                if let ast::RValue::Call(call) = rvalue {
                    if let Some((value, method)) = match_method_call(call) {
                        *rvalue = ast::MethodCall::new(
                            value.clone(),
                            method.to_string(),
                            call.arguments.drain(1..).collect(),
                        )
                        .into();
                        did_structure = true;
                    }
                } else if let ast::RValue::Select(select) = rvalue {
                    if let ast::Select::Call(call) = select {
                        if let Some((value, method)) = match_method_call(call) {
                            *select = ast::MethodCall::new(
                                value.clone(),
                                method.to_string(),
                                call.arguments.drain(1..).collect(),
                            )
                            .into();
                            did_structure = true;
                        }
                    }
                }
            });
        }
    }
    did_structure
}

// TODO: STYLE: better argument names
// `before -> skip -> after` to `before -> after`.
// multiple `before -> skip` edges can exist.
// multiple `skip -> after` edges can exist, but we decide which to use based on
// the edge index
// updates edges
fn skip_over_node(
    function: &mut Function,
    before_node: NodeIndex,
    skip_to_after: EdgeIndex,
    // params from (skip_node, after_node)
    // parameters: &[(ast::RcLocal, ast::RValue)],
) -> bool {
    let (skip_node, after_node) = function.graph().edge_endpoints(skip_to_after).unwrap();
    let mut did_structure = false;
    let skip_to_after_args = function
        .graph()
        .edge_weight(skip_to_after)
        .unwrap()
        .arguments
        .clone();
    for edge in function
        .graph()
        .edges_directed(before_node, Direction::Outgoing)
        .filter(|e| e.target() == skip_node)
        .map(|e| e.id())
        .collect::<Vec<_>>()
    {
        let mut new_arguments = function
            .graph()
            .edge_weight(edge)
            .unwrap()
            .arguments
            .clone();
        new_arguments.extend(skip_to_after_args.iter().cloned());
        // TODO: eliminate duplicate arguments where possible

        // all arguments in edges to a block must have the same parameters
        // TODO: make arguments a map so order doesnt matter
        if !new_arguments
            .iter()
            .map(|(p, _)| p)
            .eq(skip_to_after_args.iter().map(|(p, _)| p))
        {
            continue;
        }

        let mut edge = function.graph_mut().remove_edge(edge).unwrap();
        edge.arguments = new_arguments.into_iter().collect();
        function.graph_mut().add_edge(before_node, after_node, edge);
        did_structure = true;
    }

    did_structure
}

fn try_remove_unnecessary_condition(function: &mut Function, node: NodeIndex) -> bool {
    let block = function.block(node).unwrap();
    if !block.is_empty()
        && block.last().unwrap().as_if().is_some()
        && let Some((then_edge, else_edge)) = function.conditional_edges(node)
        && then_edge.target() == else_edge.target()
        && then_edge.weight().arguments == else_edge.weight().arguments
    {
        let target = then_edge.target();
        // TODO: check if this works (+ restructuring/src/jump.rs)
        let cond = function
            .block_mut(node)
            .unwrap()
            .pop()
            .unwrap()
            .into_if()
            .unwrap()
            .condition;
        let new_stat = match cond {
            ast::RValue::Call(call) => Some(call.into()),
            ast::RValue::MethodCall(method_call) => Some(method_call.into()),
            // Keep the condition as `local _ = cond` unless it is provably total:
            // a relational/arithmetic/index/length condition still RAISES on a type
            // mismatch even with no side effect, and dropping it loses that error
            // (C11). `is_total_pure` is stricter than `!has_side_effects()`.
            cond if !ast::is_total_pure(&cond) => Some(
                ast::Assign {
                    left: vec![ast::RcLocal::default().into()],
                    right: vec![cond],
                    prefix: true,
                    parallel: false,
                }
                .into(),
            ),
            _ => None,
        };
        function.block_mut(node).unwrap().extend(new_stat);
        let arguments = function
            .remove_edges(node)
            .into_iter()
            .next()
            .unwrap()
            .1
            .arguments;
        let mut new_edge = BlockEdge::new(BranchType::Unconditional);
        new_edge.arguments = arguments;
        function.set_edges(node, vec![(target, new_edge)]);
        true
    } else {
        false
    }
}

// TODO: same as in structurer
fn is_for_next(function: &Function, node: NodeIndex) -> bool {
    function
        .block(node)
        .unwrap()
        .first()
        .map(|s| {
            matches!(
                s,
                ast::Statement::GenericForNext(_) | ast::Statement::NumForNext(_)
            )
        })
        .unwrap_or(false)
}

// TODO: REFACTOR: same as match_jump in restructure, maybe can use some common code?
// TODO: STYLE: rename to merge_blocks or something
pub fn structure_jumps(function: &mut Function, dominators: &Dominators<NodeIndex>) -> bool {
    let mut did_structure = false;
    for node in function.graph().node_indices().collect_vec() {
        // we call function.remove_block, that might've resulted in node being removed
        if function.block(node).is_some()
            && let Some(jump) = function.unconditional_edge(node)
            && let jump_target = jump.target()
            && jump_target != node
            && !is_for_next(function, jump_target)
        {
            let jump_edge = jump.id();
            let block = function.block(node).unwrap();
            // TODO: block_is_no_op?
            if block.is_empty() {
                let mut remove = true;
                for pred in function.predecessor_blocks(node).collect_vec() {
                    let did = skip_over_node(function, pred, jump_edge)
                        | try_remove_unnecessary_condition(function, pred);
                    if did {
                        did_structure = true;
                    }
                    remove &= did;
                }
                if remove && function.entry() != &Some(node) {
                    function.remove_block(node);
                    continue;
                }
            }
            if function.predecessor_blocks(jump_target).count() == 1
                && dominators
                    .dominators(jump_target)
                    .map(|mut d| d.contains(&node))
                    .unwrap_or(false)
                // TODO: remove args or smthn idk
                && function.graph().edge_weight(jump_edge).unwrap().arguments.is_empty()
            {
                // assert!(function.graph().edge_weight(jump_edge).unwrap().arguments.is_empty());
                let edges = function.remove_edges(jump_target);
                let body = function.remove_block(jump_target).unwrap();
                if &Some(jump_target) == function.entry() {
                    function.set_entry(node);
                }
                function.block_mut(node).unwrap().extend(body.0);
                function.set_edges(node, edges);
                did_structure = true;
            }
        }
    }
    did_structure
}

#[cfg(test)]
mod tests {
    use super::make_bool_conditional;
    use crate::function::Function;
    use ast::{
        Binary, BinaryOperation, Block, If, Literal, Local, RValue, RcLocal, Unary, UnaryOperation,
    };

    fn rclocal(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    // RcLocal identity is keyed on a creation-order id copied by Clone, so the
    // condition and the then-value must reference the *same* RcLocal instances
    // (as they do in real SSA) for structural equality to hold — hence `lv`
    // clones a shared local rather than minting a fresh one.
    fn lv(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn and(left: RValue, right: RValue) -> RValue {
        Binary::new(left, right, BinaryOperation::And).into()
    }

    fn not(value: RValue) -> RValue {
        Unary::new(value, UnaryOperation::Not).into()
    }

    // Build a one-block function whose block ends in `if <condition>`, run
    // make_bool_conditional, and return the rendered result expression (if any).
    fn run(condition: RValue, then_value: RValue, else_value: RValue) -> Option<String> {
        let mut function = Function::new(0);
        let node = function.new_block();
        *function.block_mut(node).unwrap() =
            Block(vec![If::new(condition, Block::default(), Block::default()).into()]);
        function.set_entry(node);
        make_bool_conditional(&mut function, node, then_value, else_value).map(|r| r.to_string())
    }

    // The boolean-reduction bug this fix targets: when the then-value is the whole
    // `and`-chain condition, the legacy path only matched the chain's right operand
    // (v2), so this produced `not (v0 and v1 and v2) and X or (v0 and v1 and v2)`
    // (or, with a non-truthy else, was not transformed at all). It must now collapse
    // to the clean `(v0 and v1 and v2) or else`.
    #[test]
    fn whole_and_chain_collapses_with_non_truthy_else() {
        let (v0, v1, v2, d) = (rclocal("v0"), rclocal("v1"), rclocal("v2"), rclocal("d"));
        let chain = || and(and(lv(&v0), lv(&v1)), lv(&v2));
        assert_eq!(
            run(chain(), chain(), lv(&d)),
            Some("v0 and v1 and v2 or d".to_string())
        );
    }

    #[test]
    fn whole_and_chain_collapses_with_truthy_else() {
        let (v0, v1, v2) = (rclocal("v0"), rclocal("v1"), rclocal("v2"));
        let chain = || and(and(lv(&v0), lv(&v1)), lv(&v2));
        let x: RValue = Literal::String(b"X".to_vec()).into();
        assert_eq!(
            run(chain(), chain(), x),
            Some("v0 and v1 and v2 or \"X\"".to_string())
        );
    }

    // Using the raw condition (not reduce_condition) keeps De Morgan chains clean:
    // `(not a and not b) or d` reduces to `not (a or b) or d`, not the verbose
    // `not (a or b or (a or b)) or d` an `X and X` collapse would leave behind.
    #[test]
    fn de_morgan_chain_stays_clean() {
        let (a, b, d) = (rclocal("a"), rclocal("b"), rclocal("d"));
        let chain = || and(not(lv(&a)), not(lv(&b)));
        assert_eq!(
            run(chain(), chain(), lv(&d)),
            Some("not (a or b) or d".to_string())
        );
    }

    // GUARD: a side-effecting condition (a global read can fire __index) must NOT
    // collapse even when it equals the then-value — `if g then g else d` is kept,
    // because `g or d` would evaluate `g` once instead of twice.
    #[test]
    fn side_effecting_condition_does_not_collapse() {
        let d = rclocal("d");
        let glob = || RValue::Global(ast::Global::from("g"));
        assert_eq!(run(glob(), glob(), lv(&d)), None);
    }

    // The then-value unrelated to the condition (and a non-truthy else) is left
    // untransformed — the early-return must not misfire.
    #[test]
    fn unrelated_then_value_is_not_transformed() {
        let (v0, v1, v2, e, d) = (
            rclocal("v0"),
            rclocal("v1"),
            rclocal("v2"),
            rclocal("e"),
            rclocal("d"),
        );
        let chain = and(and(lv(&v0), lv(&v1)), lv(&v2));
        assert_eq!(run(chain, lv(&e), lv(&d)), None);
    }

    // The collapse is not restricted to and-chains: single-local and comparison
    // conditions collapse cleanly too.
    #[test]
    fn single_local_and_comparison_conditions_collapse() {
        let (v, d) = (rclocal("v"), rclocal("d"));
        assert_eq!(run(lv(&v), lv(&v), lv(&d)), Some("v or d".to_string()));

        let (a, b, e) = (rclocal("a"), rclocal("b"), rclocal("e"));
        let eq = || Binary::new(lv(&a), lv(&b), BinaryOperation::Equal).into();
        assert_eq!(run(eq(), eq(), lv(&e)), Some("a == b or e".to_string()));
    }

    // then == condition == else collapses to just the condition (via `X or X`).
    #[test]
    fn then_condition_and_else_all_equal_collapse_to_condition() {
        let v = rclocal("v");
        assert_eq!(run(lv(&v), lv(&v), lv(&v)), Some("v".to_string()));
    }

    // No-regression boundary: matching only the right operand (v2, not the whole
    // chain) must still go through the legacy path, not be swallowed by the
    // early-return (which fires only on whole-condition equality).
    #[test]
    fn right_operand_match_uses_legacy_path() {
        let (v0, v1, v2) = (rclocal("v0"), rclocal("v1"), rclocal("v2"));
        let chain = and(and(lv(&v0), lv(&v1)), lv(&v2));
        let x: RValue = Literal::String(b"X".to_vec()).into();
        let out = run(chain, lv(&v2), x).unwrap();
        // still references the full chain, i.e. not wrongly collapsed to `v2 or "X"`
        assert!(out.contains("v0") && out.contains("v1"));
    }

    // Symmetric case: else-value == condition -> `condition and then_value`.
    // `if (a and b) then c else (a and b)` => `a and b and c`.
    #[test]
    fn else_equals_condition_collapses_to_and() {
        let (a, b, c) = (rclocal("a"), rclocal("b"), rclocal("c"));
        let chain = || and(lv(&a), lv(&b));
        assert_eq!(
            run(chain(), lv(&c), chain()),
            Some("a and b and c".to_string())
        );
    }

    #[test]
    fn else_equals_condition_single_local() {
        let (a, x) = (rclocal("a"), rclocal("x"));
        // `if a then x else a` => `a and x` (x need not be truthy)
        assert_eq!(run(lv(&a), lv(&x), lv(&a)), Some("a and x".to_string()));
    }

    // GUARD (symmetric): a side-effecting condition equal to the else-value must
    // NOT collapse (`g and x` would read `g` once instead of twice).
    #[test]
    fn side_effecting_else_condition_does_not_collapse() {
        let x = rclocal("x");
        let glob = || RValue::Global(ast::Global::from("g"));
        assert_eq!(run(glob(), lv(&x), glob()), None);
    }
}
