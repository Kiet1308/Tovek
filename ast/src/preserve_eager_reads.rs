//! Keep incoming upvalue reads eager when recompilation inlines their function
//! and turns them into registers borrowed by an operation after its operands.
use rustc_hash::FxHashSet;

use crate::{deinline_safety::CaptureSafety, effects::may_write_capture, evaluation_order,
    Binary, BinaryOperation, Block, LValue, RValue, Statement, Traverse};

/// Run after every expression simplifier and before the final register check.
/// A hazardous read with incomplete or ambiguous capture facts refuses output.
pub fn preserve_eager_upvalue_reads(block: &mut Block) -> Result<usize, &'static str> {
    let safety = CaptureSafety::captures_for_read_timing(block);
    let mut state = State { safety: &safety, visited: FxHashSet::default(), changed: 0, failure: None };
    state.block(block, None);
    match state.failure { Some(reason) => Err(reason), None => Ok(state.changed) }
}

struct State<'a> {
    safety: &'a CaptureSafety,
    visited: FxHashSet<usize>,
    changed: usize,
    failure: Option<&'static str>,
}

impl State<'_> {
    fn preserve(&mut self, value: &mut RValue, function: Option<usize>) {
        if self.failure.is_some() { return; }
        let RValue::Local(local) = value else { return };
        // The chunk owns its registers even when unrelated code exceeds the capture census budget.
        if function.is_none() { return; }
        if !self.safety.complete() {
            self.failure = Some("incoming read timing exceeds the capture analysis budget");
            return;
        }
        if self.safety.register_of(local, function) { return; }
        if !self.safety.upvalue_of(local, function) {
            self.failure = Some("ambiguous closure context for incoming read timing");
            return;
        }
        // `x or x` preserves every scalar value and forces a temporary before the later effect, even after O2 inlining.
        *value = Binary::new(local.clone().into(), local.clone().into(), BinaryOperation::Or).into();
        self.changed += 1;
    }

    fn sequence(&mut self, values: Vec<&mut RValue>, function: Option<usize>) {
        let mut later_effect = false;
        for value in values.into_iter().rev() {
            if self.failure.is_some() { return; }
            let effect = may_write_capture(value);
            if later_effect { self.preserve(value, function); }
            later_effect |= effect;
        }
    }

    fn value(&mut self, value: &mut RValue, function: Option<usize>) {
        if self.failure.is_some() { return; }
        if let RValue::Closure(closure) = value {
            let identity = (&*closure.function.0 as *const _) as usize;
            if self.visited.insert(identity) {
                self.block(&mut closure.function.lock().body, Some(identity));
            }
            return;
        }
        match value {
            RValue::Table(table) => {
                for (key, item) in &mut table.0 {
                    if may_write_capture(item) && let Some(key) = key { self.preserve(key, function); }
                }
            }
            RValue::MethodCall(call) | RValue::Select(crate::Select::MethodCall(call)) => {
                if call.arguments.iter().any(may_write_capture) { self.preserve(&mut call.value, function); }
            }
            RValue::Call(call) | RValue::Select(crate::Select::Call(call)) => {
                // Builtins can arrive through local aliases; ordinary CALL already reads arguments eagerly.
                self.sequence(call.arguments.iter_mut().collect(), function);
            }
            _ if evaluation_order::late_operands(value).is_some() => {
                self.sequence(value.rvalues_mut(), function);
            }
            _ => {}
        }
        value.visit_rvalues_mut(&mut |child| { self.value(child, function); self.failure.is_none() });
    }

    fn block(&mut self, block: &mut Block, function: Option<usize>) {
        for statement in &mut block.0 {
            if self.failure.is_some() { return; }
            if let Statement::Assign(assign) = &*statement {
                let mut addresses = Vec::new();
                for (position, lhs) in assign.left.iter().enumerate() {
                    if let LValue::Index(index) = lhs {
                        for (key, value) in [(false, &*index.left), (true, &*index.right)] {
                            if let RValue::Local(local) = value
                                && evaluation_order::late_store_conflict(statement, local, &may_write_capture)
                            { addresses.push((position, key)); }
                        }
                    }
                }
                let assign = statement.as_assign_mut().unwrap();
                for (position, key) in addresses {
                    let index = assign.left[position].as_index_mut().unwrap();
                    self.preserve(if key { &mut index.right } else { &mut index.left }, function);
                }
            }
            match statement {
                Statement::Call(call) => {
                    self.sequence(call.arguments.iter_mut().collect(), function);
                }
                Statement::MethodCall(call) if call.arguments.iter().any(may_write_capture) => {
                    self.preserve(&mut call.value, function);
                }
                _ => {}
            }
            statement.visit_lvalues_mut(&mut |lhs| {
                lhs.visit_rvalues_mut(&mut |value| { self.value(value, function); self.failure.is_none() }); self.failure.is_none()
            });
            statement.visit_rvalues_mut(&mut |value| { self.value(value, function); self.failure.is_none() });
            match statement {
                Statement::If(node) => {
                    self.block(&mut node.then_block.lock(), function);
                    self.block(&mut node.else_block.lock(), function);
                }
                Statement::While(node) => self.block(&mut node.block.lock(), function),
                Statement::Repeat(node) => self.block(&mut node.block.lock(), function),
                Statement::NumericFor(node) => self.block(&mut node.block.lock(), function),
                Statement::GenericFor(node) => self.block(&mut node.block.lock(), function),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Call, Closure, Function, Global, If, IfExpression, Index, Literal,
        Local, MethodCall, RcLocal, Return, Table, Upvalue};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
    fn effect() -> RValue { Call::new(Global::from("change").into(), vec![]).into() }
    fn number(n: f64) -> RValue { Literal::Number(n).into() }
    fn enclosed(body: Block, captures: Vec<Upvalue>) -> Block {
        Block(vec![Return::new(vec![Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function { body, ..Default::default() }))),
            upvalues: captures,
        }.into()]).into()])
    }

    #[test]
    fn stabilizes_only_borrowed_operand_positions_and_is_idempotent() {
        for shape in 0..12 {
            let cell = local("cell");
            let value: RValue = match shape {
                0 => Table::new(vec![(Some(cell.clone().into()), effect())]).into(),
                1 => Binary::new(cell.clone().into(), effect(), BinaryOperation::Add).into(),
                2 => Binary::new(cell.clone().into(), effect(), BinaryOperation::LessThan).into(),
                3 => Index::new(cell.clone().into(), effect()).into(),
                4 => MethodCall::new(cell.clone().into(), "method".into(), vec![effect()]).into(),
                10 | 11 => Call::new(local("max").into(), vec![cell.clone().into(),
                    crate::Select::Call(Call::new(Global::from("change").into(), vec![])).into()]).into(),
                _ => Call::new(Index::new(Global::from("math").into(), Literal::String(b"max".to_vec()).into()).into(),
                    vec![cell.clone().into(), effect()]).into(),
            };
            let statement = match shape {
                6 => Assign::new(vec![Index::new(local("object").into(), cell.clone().into()).into()], vec![effect()]).into(),
                7 => Assign::new(vec![Index::new(cell.clone().into(), Literal::String(b"field".to_vec()).into()).into()], vec![effect()]).into(),
                8 | 11 => value.as_call().unwrap().clone().into(),
                9 => MethodCall::new(cell.clone().into(), "method".into(), vec![effect()]).into(),
                _ => Return::new(vec![value]).into(),
            };
            let mut body = enclosed(Block(vec![statement]), vec![Upvalue::Ref(cell)]);
            assert_eq!(preserve_eager_upvalue_reads(&mut body), Ok(1), "shape {shape}");
            let after = body.to_string();
            assert!(after.contains("cell or cell"), "{after}");
            assert_eq!(preserve_eager_upvalue_reads(&mut body), Ok(0));
            assert_eq!(body.to_string(), after);
        }
    }

    #[test]
    fn keeps_owner_pure_concat_and_return_tuple_reads_unchanged() {
        for incoming in [false, true] {
            let cell = local("cell");
            let body = Block(vec![
                Return::new(vec![Binary::new(cell.clone().into(), number(1.0), BinaryOperation::Add).into()]).into(),
                Return::new(vec![Binary::new(cell.clone().into(), effect(), BinaryOperation::Concat).into()]).into(),
                Return::new(vec![cell.clone().into(), effect()]).into(),
                Return::new(vec![Table::new(vec![(Some(cell.clone().into()), number(2.0))]).into()]).into(),
                Return::new(vec![Binary::new(effect(), cell.clone().into(), BinaryOperation::Add).into()]).into(),
            ]);
            let mut body = if incoming { enclosed(body, vec![Upvalue::Ref(cell.clone())]) } else { body };
            if !incoming {
                body.0.push(Return::new(vec![Table::new(vec![(Some(cell.clone().into()), effect())]).into()]).into());
                body.0.push(Return::new(vec![Closure {
                    node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(cell)],
                }.into()]).into());
            }
            let before = body.to_string();
            assert_eq!(preserve_eager_upvalue_reads(&mut body), Ok(0));
            assert_eq!(body.to_string(), before);
        }
    }

    #[test]
    fn leaves_nested_selection_and_earlier_key_errors_at_their_original_sites() {
        let cell = local("cell");
        let fields = vec![(Some(Literal::Nil.into()), effect()), (Some(cell.clone().into()), effect())];
        let selected: RValue = IfExpression::new(local("flag").into(), Table::new(fields).into(), number(0.0)).into();
        let conditional = If::new(local("outer").into(),
            Block(vec![Return::new(vec![Call::new(Global::from("consume").into(), vec![effect(), selected]).into()]).into()]),
            Block::default());
        let mut body = enclosed(Block(vec![conditional.into()]), vec![Upvalue::Ref(cell)]);
        let before = body.to_string();
        assert_eq!(preserve_eager_upvalue_reads(&mut body), Ok(1));
        // Added syntax can change table wrapping; this fixture contains no string literals.
        let compact = |text: &str| text.split_whitespace().collect::<String>();
        assert_eq!(compact(&body.to_string()), compact(&before.replace("[cell]", "[cell or cell]")));
    }

    #[test]
    fn preserves_addresses_before_earlier_local_stores_and_later_address_effects() {
        for later_address in [false, true] {
            let cell = local("cell");
            let store = Index::new(local("object").into(), cell.clone().into()).into();
            let left = if later_address {
                vec![store, Index::new(effect(), number(1.0)).into()]
            } else { vec![cell.clone().into(), store] };
            let mut body = enclosed(Block(vec![Assign::new(left, vec![number(1.0), number(2.0)]).into()]),
                vec![Upvalue::Ref(cell)]);
            assert_eq!(preserve_eager_upvalue_reads(&mut body), Ok(1));
            assert!(body.to_string().contains("[cell or cell]"));
        }
    }

    #[test]
    fn large_literal_contents_do_not_limit_read_timing_analysis() {
        for hazard in [false, true] {
            let literal = Literal::String(vec![b'x'; 8 * 1024 * 1024 + 1]);
            let mut body = if hazard {
                let cell = local("cell");
                let mut body = enclosed(Block(vec![Return::new(vec![
                    Table::new(vec![(Some(cell.clone().into()), effect())]).into(),
                ]).into()]), vec![Upvalue::Ref(cell)]);
                let mut declaration = Assign::new(vec![local("payload").into()], vec![literal.into()]);
                declaration.prefix = true;
                body.0.insert(0, declaration.into());
                body
            } else { Block(vec![Return::new(vec![literal.into()]).into()]) };
            assert_eq!(preserve_eager_upvalue_reads(&mut body), Ok(usize::from(hazard)));
            assert_eq!(preserve_eager_upvalue_reads(&mut body), Ok(0));
        }
    }

    #[test]
    fn deep_expressions_do_not_hide_owned_or_incoming_hazard_facts() {
        for incoming in [false, true] {
            let cell = local("cell");
            let mut deep = number(0.0);
            for _ in 0..256 {
                deep = Binary::new(deep, number(1.0), BinaryOperation::Add).into();
            }
            let mut declaration = Assign::new(vec![cell.clone().into()], vec![number(1.0)]);
            declaration.prefix = true;
            let mut statements = vec![Return::new(vec![deep,
                Table::new(vec![(Some(cell.clone().into()), effect())]).into(),
            ]).into()];
            if !incoming { statements.insert(0, declaration.into()); }
            let captures = if incoming { vec![Upvalue::Ref(cell)] } else { vec![] };
            let mut body = enclosed(Block(statements), captures);
            assert!(!CaptureSafety::captures(&body).complete());
            assert!(CaptureSafety::captures_for_read_timing(&body).complete());
            assert_eq!(preserve_eager_upvalue_reads(&mut body), Ok(usize::from(incoming)));
            assert_eq!(preserve_eager_upvalue_reads(&mut body), Ok(0));
        }
    }

    #[test]
    fn ambiguous_context_and_exhausted_census_refuse_instead_of_guessing() {
        let cell = local("cell");
        let function = ByAddress(Arc::new(Mutex::new(Function {
            body: Block(vec![Return::new(vec![Table::new(vec![(Some(cell.clone().into()), effect())]).into()]).into()]),
            ..Default::default()
        })));
        let make = |upvalues| -> RValue { Closure { node_origin: Default::default(), function: function.clone(), upvalues }.into() };
        for reverse in [false, true] {
            let mut values = vec![make(vec![Upvalue::Ref(cell.clone())]), make(vec![])];
            if reverse { values.reverse(); }
            let mut body = Block(vec![Return::new(values).into()]);
            let before = body.to_string();
            assert_eq!(preserve_eager_upvalue_reads(&mut body), Err("ambiguous closure context for incoming read timing"));
            assert_eq!(body.to_string(), before);
        }
        let mut large = Block(vec![Statement::Empty(crate::Empty {}); 200_001]);
        let before = large.0.len();
        assert_eq!(preserve_eager_upvalue_reads(&mut large), Ok(0));
        assert_eq!(large.0.len(), before);
        large.0.extend(enclosed(Block(vec![Return::new(vec![
            Table::new(vec![(Some(cell.clone().into()), effect())]).into(),
        ]).into()]), vec![Upvalue::Ref(cell)]).0);
        assert_eq!(preserve_eager_upvalue_reads(&mut large), Err("incoming read timing exceeds the capture analysis budget"));
        assert_eq!(large.0.len(), before + 1);
    }
}
