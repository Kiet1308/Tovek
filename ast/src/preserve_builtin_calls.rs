//! Keep actual builtin calls when a different output optimization level could
//! fold them and bypass a replacement supplied by the external environment.
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{deinline_safety::CaptureSafety, effects, Block, Call, LValue, Literal,
    RValue, Select, Statement, Traverse};

/// Run before constant spelling, which intentionally creates
/// foldable constructor calls, and after callee/argument motion has finished.
pub fn preserve_builtin_calls(block: &mut Block, assume_standard_libraries: bool) -> Result<usize, &'static str> {
    let safety = CaptureSafety::writes_for_read_timing(block);
    let mut state = State { safety: &safety, aliases: FxHashMap::default(),
        visited: FxHashSet::default(), changed: 0, assume_standard_libraries, failure: None };
    state.block(block);
    match state.failure { Some(reason) => Err(reason), None => Ok(state.changed) }
}

#[derive(Clone, Copy, PartialEq)]
enum Builtin { Library, Function }

struct State<'a> {
    safety: &'a CaptureSafety,
    aliases: FxHashMap<u64, Builtin>,
    visited: FxHashSet<usize>,
    changed: usize,
    assume_standard_libraries: bool,
    failure: Option<&'static str>,
}

impl State<'_> {
    fn builtin(&self, value: &RValue) -> Option<Builtin> {
        match value {
            RValue::Global(global) => match global.0.as_slice() {
                b"type" | b"typeof" => Some(Builtin::Function),
                b"math" | b"bit32" | b"string" | b"vector" | b"Vector3" => Some(Builtin::Library),
                _ => None,
            },
            RValue::Local(local) => self.aliases.get(&local.stable_id()).copied(),
            RValue::Index(index) if matches!(&*index.right, RValue::Literal(Literal::String(_))) => match self.builtin(&index.left) {
                Some(Builtin::Library) => Some(Builtin::Function),
                _ => None,
            },
            // Luau also recognizes `local math = math or replacement` as a library alias.
            RValue::Binary(binary) if binary.operation == crate::BinaryOperation::Or
                && self.builtin(&binary.left) == Some(Builtin::Library) => Some(Builtin::Library),
            _ => None,
        }
    }

    fn call(&mut self, call: &mut Call) {
        // A configured vector FASTCALL can bypass a replacement in its mutable library even in a safe environment.
        if self.failure.is_some() || call.group_callee || call.vector_constructor_fastcall
            || self.builtin(&call.value) != Some(Builtin::Function) { return; }
        let mut group = true;
        let mut pure = true;
        for argument in &call.arguments {
            if !self.assume_standard_libraries && matches!(argument, RValue::Literal(Literal::Number(_))) { continue; }
            let summary = effects::summarize(argument, &|local|
                !self.safety.uncaptured(local) && !self.safety.never_reassigned(local));
            if summary.exhausted {
                self.failure = Some("builtin call timing exceeds the operand analysis budget");
                return;
            }
            pure &= summary.effects.is_total_pure();
            group &= matches!(&*call.value, RValue::Local(_)) || !summary.effects.has_order_dependency();
        }
        if !pure {
            let RValue::Local(callee) = &*call.value else { return };
            if !self.safety.complete() {
                self.failure = Some("builtin call timing exceeds the capture analysis budget");
                return;
            }
            if !self.safety.uncaptured(callee) && !self.safety.never_reassigned(callee) { return; }
            let mut later_effect = false;
            for argument in call.arguments.iter().rev() {
                if later_effect && let RValue::Local(local) = argument
                    && !self.safety.uncaptured(local) && !self.safety.never_reassigned(local) { return; }
                later_effect |= effects::may_write_capture(argument);
            }
            // The callee snapshot is fixed; grouping preserves argument order unless FASTCALL borrowed a mutable early argument.
            group = true;
        }
        if group {
            call.group_callee = true;
            self.changed += 1;
        } else if !self.safety.complete() {
            self.failure = Some("builtin call timing exceeds the capture analysis budget");
        }
    }

    fn value(&mut self, value: &mut RValue) {
        if self.failure.is_some() { return; }
        if let RValue::Closure(closure) = value {
            if self.visited.insert((&*closure.function.0 as *const _) as usize) {
                self.block(&mut closure.function.lock().body);
            }
            return;
        }
        if let RValue::Call(call) | RValue::Select(Select::Call(call)) = value { self.call(call); }
        value.visit_rvalues_mut(&mut |child| { self.value(child); true });
    }

    fn block(&mut self, block: &mut Block) {
        for statement in &mut block.0 {
            if self.failure.is_some() { return; }
            if let Statement::Assign(assign) = &*statement {
                for (left, right) in assign.left.iter().zip(&assign.right) {
                    if let LValue::Local(local) = left && let Some(builtin) = self.builtin(right) {
                        self.aliases.insert(local.stable_id(), builtin);
                    }
                }
            }
            if let Statement::Call(call) = statement { self.call(call); }
            statement.visit_lvalues_mut(&mut |left| {
                left.visit_rvalues_mut(&mut |value| { self.value(value); true }); true
            });
            statement.visit_rvalues_mut(&mut |value| { self.value(value); true });
            match statement {
                Statement::If(node) => {
                    self.block(&mut node.then_block.lock());
                    self.block(&mut node.else_block.lock());
                }
                Statement::While(node) => self.block(&mut node.block.lock()),
                Statement::Repeat(node) => self.block(&mut node.block.lock()),
                Statement::NumericFor(node) => self.block(&mut node.block.lock()),
                Statement::GenericFor(node) => self.block(&mut node.block.lock()),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Binary, BinaryOperation, Closure, Global, Index, Local, RcLocal, Return, Table, Unary, UnaryOperation, Upvalue};

    fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
    fn number(n: f64) -> RValue { Literal::Number(n).into() }
    fn maximum() -> RValue {
        Index::new(Global::from("math").into(), Literal::String(b"max".to_vec()).into()).into()
    }
    fn declaration(local: &RcLocal, value: RValue) -> Statement {
        let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    #[test]
    fn groups_direct_alias_and_named_constant_calls_without_grouping_other_helpers() {
        let first = local("first");
        let maximum_alias = local("maximum");
        let library = local("library");
        let mut body = Block(vec![
            declaration(&first, number(1.0)), declaration(&maximum_alias, maximum()),
            declaration(&library, Global::from("math").into()),
            Return::new(vec![
                Call::new(maximum(), vec![number(1.0), number(2.0)]).into(),
                Call::new(maximum_alias.into(), vec![first.into(), number(2.0)]).into(),
                Call::new(Index::new(library.into(), Literal::String(b"max".to_vec()).into()).into(), vec![number(1.0), number(2.0)]).into(),
                Call::new(Global::from("print").into(), vec![number(1.0)]).into(),
                Call::new(local("helper").into(), vec![number(1.0)]).into(),
            ]).into(),
        ]);
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(3));
        let output = body.to_string();
        assert!(output.contains("(math.max)(1, 2)"), "{output}");
        assert!(output.contains("(maximum)(first, 2)"), "{output}");
        assert!(output.contains("(library.max)(1, 2)"), "{output}");
        assert!(output.contains("print(1)"), "{output}");
        assert!(output.contains("helper(1)"), "{output}");
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(0));
    }

    #[test]
    fn does_not_move_captured_reads_or_observable_arguments_before_the_callee() {
        let cell = local("cell");
        let maximum_alias = local("maximum");
        let writer = Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(cell.clone())] };
        writer.function.lock().body.0.push(Assign::new(vec![cell.clone().into()], vec![number(20.0)]).into());
        let mut body = Block(vec![declaration(&maximum_alias, maximum()), Return::new(vec![
            Call::new(maximum(), vec![cell.clone().into(), number(2.0)]).into(),
            Call::new(maximum(), vec![Call::new(Global::from("change").into(), vec![]).into(), number(2.0)]).into(),
            Call::new(maximum(), vec![Global::from("value").into(), number(2.0)]).into(),
            writer.into(),
        ]).into()]);
        let before = body.to_string();
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(0));
        assert_eq!(body.to_string(), before);
        body.0.push(Return::new(vec![Call::new(maximum_alias.into(), vec![cell.into(), number(2.0)]).into()]).into());
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1));
        assert!(body.to_string().ends_with("return (maximum)(cell, 2)"));
    }

    #[test]
    fn readonly_captures_allow_grouping_without_changing_their_read_value() {
        let cell = local("cell");
        let maximum_alias = local("maximum");
        let mut body = Block(vec![declaration(&cell, number(1.0)), declaration(&maximum_alias, maximum()), Return::new(vec![
            Call::new(maximum(), vec![cell.clone().into(), number(2.0)]).into(),
            Call::new(maximum_alias.into(), vec![cell.clone().into(), number(2.0)]).into(),
            Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(cell)] }.into(),
        ]).into()]);
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(2));
        assert!(body.to_string().contains("(maximum)(cell, 2)"));
    }

    #[test]
    fn exact_numeric_literals_respect_later_library_spelling() {
        for assume_standard_libraries in [false, true] {
            let mut body = Block(vec![Return::new([std::f64::consts::PI, f64::INFINITY, f64::NAN]
                .into_iter().map(|n| Call::new(maximum(), vec![number(n), number(2.0)]).into()).collect()).into()]);
            assert_eq!(preserve_builtin_calls(&mut body, assume_standard_libraries), Ok(if assume_standard_libraries { 0 } else { 3 }));
        }
    }

    #[test]
    fn saved_callees_preserve_arithmetic_string_and_effectful_arguments() {
        for shape in 0..6 {
            let first = local("first");
            let maximum_alias = local("maximum");
            let argument = match shape {
                0 => Unary::new(first.clone().into(), UnaryOperation::Negate).into(),
                1 => Binary::new(first.clone().into(), number(1.0), BinaryOperation::Add).into(),
                2 => Binary::new(number(1.0), number(0.0), BinaryOperation::Div).into(),
                3 => Binary::new(Literal::String(b"a".to_vec()).into(), Literal::String(b"b".to_vec()).into(), BinaryOperation::Concat).into(),
                4 => Unary::new(Literal::String(b"abc".to_vec()).into(), UnaryOperation::Length).into(),
                _ => Select::Call(Call::new(Global::from("change").into(), vec![])).into(),
            };
            let mut call = Call::new(maximum_alias.clone().into(), vec![argument, number(2.0)]);
            call.callee_after_arguments = true;
            let mut body = Block(vec![declaration(&first, number(1.0)), declaration(&maximum_alias, maximum()),
                Return::new(vec![call.into()]).into()]);
            assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1), "shape {shape}");
            assert!(body.to_string().contains("return (maximum)("));
        }
    }

    #[test]
    fn saved_callees_still_reject_argument_effects_that_change_borrowed_reads() {
        for changes_callee in [false, true] {
            let first = local("first");
            let maximum_alias = local("maximum");
            let changed = if changes_callee { maximum_alias.clone() } else { first.clone() };
            let writer = Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(changed.clone())] };
            writer.function.lock().body.0.push(Assign::new(vec![changed.into()], vec![Literal::Nil.into()]).into());
            let arguments = vec![first.clone().into(), Select::Call(Call::new(Global::from("change").into(), vec![])).into()];
            let mut call = Call::new(maximum_alias.clone().into(), arguments);
            call.callee_after_arguments = true;
            let mut body = Block(vec![declaration(&first, number(1.0)), declaration(&maximum_alias, maximum()),
                Return::new(vec![call.into(), writer.into()]).into()]);
            assert_eq!(preserve_builtin_calls(&mut body, false), Ok(0));
        }
    }

    #[test]
    fn configured_vector_fastcalls_keep_their_native_path_through_aliases() {
        for (fast, name) in [(false, "new"), (true, "new"), (true, "customConstructor")] {
            let library = local("library");
            let constructor = local("constructor");
            let index = |value| Index::new(value, Literal::String(name.as_bytes().to_vec()).into()).into();
            let make = |value| {
                let mut call = Call::new(value, vec![number(1.0), number(2.0), number(3.0)]);
                call.callee_after_arguments = fast;
                call.vector_constructor_fastcall = fast;
                let copied = call.with_parts((*call.value).clone(), call.arguments.clone());
                assert_eq!(copied.vector_constructor_fastcall, fast);
                assert_eq!(copied, call);
                if fast {
                    let mut ordinary = call.clone();
                    ordinary.vector_constructor_fastcall = false;
                    assert_ne!(ordinary, call);
                    assert!(format!("{call:?}").contains("vector_constructor_fastcall: true"));
                }
                call.into()
            };
            let mut body = Block(vec![declaration(&library, Global::from("Vector3").into()),
                declaration(&constructor, index(library.into())),
                Return::new(vec![make(index(Global::from("Vector3").into())), make(constructor.into())]).into()]);
            assert_eq!(preserve_builtin_calls(&mut body, false), Ok(if fast { 0 } else { 2 }));
        }
    }

    #[test]
    fn standard_vector_member_fastcalls_can_keep_their_fallback_calls() {
        for member in ["magnitude", "normalize", "floor"] {
            let path = Index::new(Global::from("vector").into(), Literal::String(member.as_bytes().to_vec()).into()).into();
            let mut call = Call::new(path, vec![local("value").into()]);
            call.callee_after_arguments = true;
            let mut body = Block(vec![Return::new(vec![call.into()]).into()]);
            assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1));
            assert_eq!(body.to_string(), format!("return (vector.{member})(value)"));
        }
    }

    #[test]
    fn large_literal_contents_do_not_hide_readonly_captured_arguments() {
        let cell = local("cell");
        let mut body = Block(vec![
            declaration(&local("payload"), Literal::String(vec![b'x'; 8 * 1024 * 1024 + 1]).into()),
            declaration(&cell, number(1.0)),
            Return::new(vec![Call::new(maximum(), vec![cell.clone().into(), number(2.0)]).into(),
                Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(cell)] }.into(),
            ]).into(),
        ]);
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1));
    }

    #[test]
    fn exhausted_capture_facts_refuse_only_calls_that_need_them() {
        for shape in 0..4 {
            let cell = local("cell");
            let maximum_alias = local("maximum");
            let mut body = Block(vec![Statement::Empty(crate::Empty {}); 200_001]);
            body.0.push(declaration(&cell, number(1.0)));
            body.0.push(declaration(&maximum_alias, maximum()));
            let callee = if shape >= 2 { maximum_alias.into() } else { maximum() };
            let argument = match shape {
                1 => number(1.0),
                3 => Unary::new(cell.clone().into(), UnaryOperation::Negate).into(),
                _ => cell.clone().into(),
            };
            body.0.push(Return::new(vec![Call::new(callee, vec![argument, number(2.0)]).into(),
                Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(cell)] }.into(),
            ]).into());
            assert_eq!(preserve_builtin_calls(&mut body, false), if shape == 0 || shape == 3 {
                Err("builtin call timing exceeds the capture analysis budget")
            } else { Ok(1) });
        }
    }

    #[test]
    fn grouped_calls_keep_semantics_through_copying_and_statement_boundaries() {
        let mut call = Call::new(maximum(), vec![number(1.0), number(2.0)]);
        let original = call.clone();
        call.group_callee = true;
        assert_ne!(call, original);
        let copied = call.with_parts((*call.value).clone(), call.arguments.clone());
        assert_eq!(copied, call);
        assert!(format!("{call:?}").contains("group_callee: true"));
        let body = Block(vec![
            Call::new(Global::from("consume").into(), vec![]).into(), copied.clone().into(),
            Assign::new(vec![Index::new(Select::Call(copied).into(), number(1.0)).into()], vec![Table::default().into()]).into(),
        ]);
        let output = body.to_string();
        assert!(output.contains("consume();\n(math.max)(1, 2);\n(math.max)(1, 2)[1] = {}"), "{output}");
    }
}
