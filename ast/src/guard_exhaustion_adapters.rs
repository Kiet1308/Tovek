//! Preserve path-exclusive normal-exhaustion copies after a source `for`.
//!
//! The legacy structurer can represent a FORGLOOP as a normal Luau `for`,
//! but it historically emitted the CFG's normal-exhaustion adapter directly
//! after that loop.  A body-side `break` bypasses the adapter in the CFG, so
//! an unconditional copy can overwrite the value selected by the break path.
//! The old implementation tried to recognize that shape from an untyped AST.
//! Because ordinary source statements have the same shape, the compatibility
//! entry point below is intentionally disabled; only CFG-backed provenance may
//! make an adapter exhaustion-only.

use crate::Block;

/// Legacy AST output does not carry the CFG edge identity needed to prove that
/// a post-loop copy belongs exclusively to the normal-exhaustion port.  In
/// particular, `ForOrigin` and historical `= nil` seeds also occur on ordinary
/// source-visible paths.  Keep this compatibility entry point, but leave the
/// AST untouched until an explicit `ExhaustionAdapterPlan` is attached by the
/// structurer.
pub fn guard_generic_for_adapters(_block: &mut Block, _allow_legacy_heuristic: bool) {
    // Intentionally disabled: see the proof obligation above.
}

#[cfg(test)]
mod tests {
    use super::guard_generic_for_adapters;
    use crate::{
        Assign, Binary, BinaryOperation, Block, Break, ForOrigin, ForPrepKind, GenericFor, Global,
        LValue, Literal, RValue, RcLocal, Statement, VmProfileId,
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
    fn leaves_ambiguous_local_copy_untouched() {
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
            // A later non-nil write invalidates any historical nil seed.  The
            // AST pass still cannot use this shape as exhaustion provenance.
            Assign::new(
                vec![LValue::Local(source.clone())],
                vec![RValue::Literal(Literal::Number(42.0))],
            )
            .into(),
            generic.into(),
            Assign::new(
                vec![LValue::Local(target.clone())],
                vec![RValue::Local(source)],
            )
            .into(),
        ]);

        guard_generic_for_adapters(&mut block, true);

        assert!(matches!(block.0[3], Statement::GenericFor(_)));
        assert!(matches!(block.0[4], Statement::Assign(_)));
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
