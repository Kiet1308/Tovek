//! Preserve path-exclusive normal-exhaustion copies after a source `for`.
//!
//! The legacy structurer can represent a FORGLOOP as a normal Luau `for`,
//! but it historically emitted the CFG's normal-exhaustion adapter directly
//! after that loop.  A body-side `break` bypasses the adapter in the CFG, so
//! an unconditional copy can overwrite the value selected by the break path.
//! This pass recognizes only the narrow, proof-backed AST shape produced by
//! that lowering and makes the adapter exhaustion-only with a readable flag.

use rustc_hash::FxHashSet;

use crate::{Assign, Block, If, LValue, Literal, LocalRw, RValue, RcLocal, Statement};

/// Guard immediate post-loop assignments that reset a value also written by a
/// break-capable generic-for body.  The pass is intentionally conservative:
/// assignments are considered adapters only when every assignment in the
/// contiguous suffix writes a local written by the loop body and the body has
/// an outer-loop `break` (breaks nested inside another loop do not count).
///
/// `allow_legacy_heuristic` must be true only for a block produced by the
/// legacy structurer.  A normal source-level `for` followed by `x = y` is
/// indistinguishable from a compiler exhaustion copy once lowered to this AST;
/// the source-like structurer therefore never opts into this heuristic.
pub fn guard_generic_for_adapters(block: &mut Block, allow_legacy_heuristic: bool) {
    if !allow_legacy_heuristic {
        return;
    }
    // Recurse first so nested loops are normalized before the enclosing block
    // examines its own immediate suffix.
    for statement in &mut block.0 {
        guard_nested_blocks(statement, allow_legacy_heuristic);
    }

    let mut index = 0;
    while index < block.0.len() {
        let (body_writes, has_break) = match &block.0[index] {
            // Production generic-for nodes retain the lifter's immutable
            // provenance marker.  An origin-less node can equally be an
            // ordinary hand-built/source AST, where the suffix assignment is
            // observable after `break`; do not apply this AST-only heuristic
            // without that compiler marker.
            Statement::GenericFor(for_loop) if for_loop.origin.is_some() => {
                let body = for_loop.block.lock();
                let mut writes = FxHashSet::default();
                collect_writes(&body, &mut writes);
                (writes, contains_outer_break(&body))
            }
            _ => {
                index += 1;
                continue;
            }
        };
        if !has_break {
            index += 1;
            continue;
        }
        let result_locals: FxHashSet<RcLocal> = match &block.0[index] {
            Statement::GenericFor(for_loop) => for_loop.res_locals.iter().cloned().collect(),
            _ => unreachable!(),
        };
        let mut adapter_len = 0;
        while index + 1 + adapter_len < block.0.len() {
            let statement = &block.0[index + 1 + adapter_len];
            let is_adapter = match statement {
                Statement::Assign(assign)
                    if assign.left.len() == 1
                        && assign.right.len() == 1
                        && !assign.left[0]
                            .as_local()
                            .is_some_and(|local| result_locals.contains(local)) =>
                {
                    let target = assign.left[0].as_local();
                    let source = match &assign.right[0] {
                        RValue::Local(source) => Some(source),
                        _ => None,
                    };
                    match (target, source) {
                        (Some(target), Some(source)) => {
                            // All known compiler exhaustion adapters copy a
                            // value from a nil-seeded temporary/result.  The
                            // seed check prevents wrapping an ordinary literal
                            // assignment that happens to follow a break-capable
                            // loop (which would change explicit-break behavior).
                            let both_nil_seeded = has_nil_seed(block, index, target)
                                && has_nil_seed(block, index, source);
                            both_nil_seeded && body_writes.contains(target)
                        }
                        _ => false,
                    }
                }
                _ => false,
            };
            if !is_adapter {
                break;
            }
            adapter_len += 1;
        }
        if adapter_len == 0 {
            index += 1;
            continue;
        }
        let flag = RcLocal::default();
        if let Statement::GenericFor(for_loop) = &mut block.0[index] {
            insert_false_before_outer_break(&mut for_loop.block.lock(), &flag);
        }
        block.0.insert(
            index,
            Assign::new(
                vec![LValue::Local(flag.clone())],
                vec![RValue::Literal(Literal::Boolean(true))],
            )
            .into(),
        );
        // The generic-for shifted one position after the initialization.
        let adapter_start = index + 2;
        let adapters = Block::from(
            block
                .0
                .drain(adapter_start..adapter_start + adapter_len)
                .collect::<Vec<_>>(),
        );
        block.0.insert(
            adapter_start,
            If::new(RValue::Local(flag), adapters, Block::default()).into(),
        );
        index = adapter_start + 1;
    }
}

fn guard_nested_blocks(statement: &mut Statement, allow_legacy_heuristic: bool) {
    match statement {
        Statement::If(node) => {
            guard_generic_for_adapters(&mut node.then_block.lock(), allow_legacy_heuristic);
            guard_generic_for_adapters(&mut node.else_block.lock(), allow_legacy_heuristic);
        }
        Statement::While(node) => {
            guard_generic_for_adapters(&mut node.block.lock(), allow_legacy_heuristic)
        }
        Statement::Repeat(node) => {
            guard_generic_for_adapters(&mut node.block.lock(), allow_legacy_heuristic)
        }
        Statement::NumericFor(node) => {
            guard_generic_for_adapters(&mut node.block.lock(), allow_legacy_heuristic)
        }
        Statement::GenericFor(node) => {
            guard_generic_for_adapters(&mut node.block.lock(), allow_legacy_heuristic)
        }
        _ => {}
    }
}

fn collect_writes(block: &Block, writes: &mut FxHashSet<RcLocal>) {
    for statement in &block.0 {
        writes.extend(statement.values_written().into_iter().cloned());
        match statement {
            Statement::If(node) => {
                collect_writes(&node.then_block.lock(), writes);
                collect_writes(&node.else_block.lock(), writes);
            }
            Statement::While(node) => collect_writes(&node.block.lock(), writes),
            Statement::Repeat(node) => collect_writes(&node.block.lock(), writes),
            Statement::NumericFor(node) => collect_writes(&node.block.lock(), writes),
            Statement::GenericFor(node) => collect_writes(&node.block.lock(), writes),
            _ => {}
        }
    }
}

fn has_nil_seed(block: &Block, loop_index: usize, local: &RcLocal) -> bool {
    block.0.iter().take(loop_index).any(|statement| {
        matches!(
            statement,
            Statement::Assign(assign)
                if assign.left.len() == 1
                    && assign.right.len() == 1
                    && assign.left[0].as_local() == Some(local)
                    && matches!(assign.right[0], RValue::Literal(Literal::Nil))
        )
    })
}

fn contains_outer_break(block: &Block) -> bool {
    block.0.iter().any(|statement| match statement {
        Statement::Break(_) => true,
        Statement::If(node) => {
            contains_outer_break(&node.then_block.lock())
                || contains_outer_break(&node.else_block.lock())
        }
        // A nested loop owns its own break statements.
        Statement::While(_)
        | Statement::Repeat(_)
        | Statement::NumericFor(_)
        | Statement::GenericFor(_) => false,
        _ => false,
    })
}

fn insert_false_before_outer_break(block: &mut Block, flag: &RcLocal) {
    let mut index = 0;
    while index < block.0.len() {
        match &mut block.0[index] {
            Statement::Break(_) => {
                block.0.insert(
                    index,
                    Assign::new(
                        vec![LValue::Local(flag.clone())],
                        vec![RValue::Literal(Literal::Boolean(false))],
                    )
                    .into(),
                );
                index += 2;
            }
            Statement::If(node) => {
                insert_false_before_outer_break(&mut node.then_block.lock(), flag);
                insert_false_before_outer_break(&mut node.else_block.lock(), flag);
                index += 1;
            }
            _ => index += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Binary, BinaryOperation, Break, ForOrigin, ForPrepKind, GenericFor, Global, VmProfileId,
    };

    fn test_origin() -> ForOrigin {
        ForOrigin {
            prep_pc: 0,
            step_pc: 1,
            body_pc: 2,
            follow_pc: 2,
            prep_kind: ForPrepKind::Generic,
            base_register: 0,
            result_count: 1,
            aux: 0,
            bytecode_version: 1,
            vm_profile: VmProfileId::Luau,
            explicit_nil_args: false,
        }
    }

    fn nil_seed(local: RcLocal) -> Statement {
        Assign::new(
            vec![LValue::Local(local)],
            vec![RValue::Literal(Literal::Nil)],
        )
        .into()
    }

    #[test]
    fn guards_only_local_copy_adapters() {
        let target = RcLocal::default();
        let source = RcLocal::default();
        let result = RcLocal::default();
        let mut generic = GenericFor::new(
            vec![result],
            vec![RValue::Global(Global::from("items"))],
            Block::from(vec![
                Assign::new(
                    vec![LValue::Local(target.clone())],
                    vec![RValue::Literal(Literal::Number(1.0))],
                )
                .into(),
                Break {}.into(),
            ]),
        );
        generic.origin = Some(test_origin());
        let mut block = Block::from(vec![
            nil_seed(target.clone()),
            nil_seed(source.clone()),
            generic.into(),
            Assign::new(
                vec![LValue::Local(target.clone())],
                vec![RValue::Local(source)],
            )
            .into(),
        ]);

        guard_generic_for_adapters(&mut block, true);

        assert!(matches!(block.0[2], Statement::Assign(_)));
        assert!(matches!(block.0[3], Statement::GenericFor(_)));
        assert!(matches!(block.0[4], Statement::If(_)));
    }

    #[test]
    fn does_not_guard_arithmetic_after_breakable_loop() {
        let target = RcLocal::default();
        let source = RcLocal::default();
        let result = RcLocal::default();
        let mut generic = GenericFor::new(
            vec![result],
            vec![RValue::Global(Global::from("items"))],
            Block::from(vec![
                Assign::new(
                    vec![LValue::Local(target.clone())],
                    vec![RValue::Literal(Literal::Number(1.0))],
                )
                .into(),
                Break {}.into(),
            ]),
        );
        generic.origin = Some(test_origin());
        let mut block = Block::from(vec![
            nil_seed(target.clone()),
            nil_seed(source),
            generic.into(),
            Assign::new(
                vec![LValue::Local(target.clone())],
                vec![Binary::new(
                    RValue::Local(target),
                    RValue::Literal(Literal::Number(1.0)),
                    BinaryOperation::Add,
                )
                .into()],
            )
            .into(),
        ]);

        guard_generic_for_adapters(&mut block, true);

        assert!(matches!(block.0[2], Statement::GenericFor(_)));
        assert!(matches!(block.0[3], Statement::Assign(_)));
    }

    #[test]
    fn source_like_path_does_not_apply_ambiguous_copy_heuristic() {
        let target = RcLocal::default();
        let source = RcLocal::default();
        let result = RcLocal::default();
        let generic = GenericFor::new(
            vec![result],
            vec![RValue::Global(Global::from("items"))],
            Block::from(vec![Break {}.into()]),
        );
        let mut block = Block::from(vec![
            nil_seed(target.clone()),
            nil_seed(source.clone()),
            generic.into(),
            Assign::new(
                vec![LValue::Local(target)],
                vec![RValue::Local(source)],
            )
            .into(),
        ]);

        guard_generic_for_adapters(&mut block, false);

        assert!(matches!(block.0[2], Statement::GenericFor(_)));
        assert!(matches!(block.0[3], Statement::Assign(_)));
    }

    #[test]
    fn originless_ordinary_copy_is_not_guarded() {
        // This is a valid source-level shape, not a proven compiler adapter:
        // the assignment after the loop must still run when the body breaks.
        // Without provenance the AST pass cannot distinguish the two cases,
        // so it must leave the suffix untouched.
        let target = RcLocal::default();
        let source = RcLocal::default();
        let result = RcLocal::default();
        let generic = GenericFor::new(
            vec![result],
            vec![RValue::Global(Global::from("items"))],
            Block::from(vec![
                Assign::new(
                    vec![LValue::Local(target.clone())],
                    vec![RValue::Literal(Literal::Number(1.0))],
                )
                .into(),
                Break {}.into(),
            ]),
        );
        let mut block = Block::from(vec![
            nil_seed(target.clone()),
            nil_seed(source.clone()),
            generic.into(),
            Assign::new(
                vec![LValue::Local(target)],
                vec![RValue::Local(source)],
            )
            .into(),
        ]);

        guard_generic_for_adapters(&mut block, true);

        assert!(matches!(block.0[2], Statement::GenericFor(_)));
        assert!(matches!(block.0[3], Statement::Assign(_)));
    }
}
