//! Bounded evaluation positions used by late expression motion. The event order
//! is a dependency chain, with conditional events conservatively kept on it.
//! No event owns AST nodes or serves as proof after the statement is mutated.
use crate::{effects::{self, Effects}, BinaryOperation, LValue, LocalRw, RValue, RcLocal,
    Select, Statement, Traverse};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Callee, Receiver, Argument(usize), LhsBase(usize), LhsKey(usize),
    Rhs(usize), Store(usize), Value, Operation,
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub position: Position,
    pub effects: Effects,
    pub read: Option<u64>,
    pub write: Option<u64>,
    pub conditional: bool,
}

#[derive(Default)]
pub struct Order {
    pub events: Vec<Event>,
    pub exhausted: bool,
    nodes: usize,
}

impl Order {
    fn event(&mut self, position: Position, effects: Effects, read: Option<u64>, write: Option<u64>, conditional: bool) {
        if self.events.len() >= 8192 { self.exhausted = true; return; }
        self.events.push(Event { position, effects, read, write, conditional });
    }

    fn value(&mut self, value: &RValue, position: Position, conditional: bool, depth: usize, capture: &impl Fn(&RcLocal) -> bool) {
        self.nodes += 1;
        if self.exhausted || self.nodes > 4096 || depth >= 128 { self.exhausted = true; return; }
        match value {
            RValue::Local(local) => self.event(position, effects::intrinsic(value, capture), Some(local.stable_id()), None, conditional),
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                self.value(&call.value, Position::Callee, conditional, depth + 1, capture);
                for (i, arg) in call.arguments.iter().enumerate() {
                    self.value(arg, Position::Argument(i), conditional, depth + 1, capture);
                    if self.exhausted { return; }
                }
                self.event(Position::Operation, Effects::DYNAMIC_CALL, None, None, conditional);
            }
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
                self.value(&call.value, Position::Receiver, conditional, depth + 1, capture);
                for (i, arg) in call.arguments.iter().enumerate() {
                    self.value(arg, Position::Argument(i), conditional, depth + 1, capture);
                    if self.exhausted { return; }
                }
                // Luau evaluates arguments before NAMECALL lookup. Dot-call
                // lookup instead belongs to the callee expression above.
                self.event(Position::Callee, Effects::DYNAMIC_CALL, None, None, conditional);
                self.event(Position::Operation, Effects::DYNAMIC_CALL, None, None, conditional);
            }
            RValue::Binary(binary) => {
                self.value(&binary.left, position, conditional, depth + 1, capture);
                let short = matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or);
                self.value(&binary.right, position, conditional || short, depth + 1, capture);
                self.event(Position::Operation, effects::intrinsic(value, capture), None, None, conditional);
            }
            RValue::IfExpression(branch) => {
                self.value(&branch.condition, position, conditional, depth + 1, capture);
                self.value(&branch.then_value, position, true, depth + 1, capture);
                self.value(&branch.else_value, position, true, depth + 1, capture);
            }
            RValue::Closure(closure) => {
                for local in closure.values_read() {
                    self.event(position, if capture(local) { Effects::CAPTURE_READ } else { Effects::default() }, Some(local.stable_id()), None, conditional);
                    if self.exhausted { return; }
                }
                self.event(position, Effects::ALLOCATION, None, None, conditional);
            }
            RValue::Table(table) => {
                self.event(position, Effects::ALLOCATION, None, None, conditional);
                for (key, item) in &table.0 {
                    if let Some(key) = key { self.value(key, position, conditional, depth + 1, capture); }
                    self.value(item, position, conditional, depth + 1, capture);
                    // A private constructor store cannot call __newindex, but
                    // invalid keys can raise before the next field is evaluated.
                    if key.as_ref().is_some_and(|k| !crate::is_total_table_key(k)) {
                        self.event(Position::Operation, Effects::MAY_THROW, None, None, conditional);
                    }
                    if self.exhausted { return; }
                }
            }
            _ => {
                for child in value.rvalues() { self.value(child, position, conditional, depth + 1, capture); }
                self.event(position, effects::intrinsic(value, capture), None, None, conditional);
            }
        }
    }
}

pub fn statement(statement: &Statement, capture: &impl Fn(&RcLocal) -> bool) -> Order {
    let mut out = Order::default();
    match statement {
        Statement::Assign(assign) => {
            for (i, lhs) in assign.left.iter().enumerate() {
                if let LValue::Index(index) = lhs {
                    out.value(&index.left, Position::LhsBase(i), false, 0, capture);
                    out.value(&index.right, Position::LhsKey(i), false, 0, capture);
                }
            }
            for (i, rhs) in assign.right.iter().enumerate() { out.value(rhs, Position::Rhs(i), false, 0, capture); }
            // All addresses precede all RHS evaluations; stores follow them.
            // No replacement is attempted inside a store event.
            for (i, lhs) in assign.left.iter().enumerate() {
                match lhs {
                    LValue::Local(local) => out.event(Position::Store(i), if capture(local) { Effects::CAPTURE_WRITE } else { Effects::default() }, None, Some(local.stable_id()), false),
                    _ => out.event(Position::Store(i), Effects::DYNAMIC_CALL, None, None, false),
                }
            }
        }
        Statement::Call(call) => {
            out.value(&call.value, Position::Callee, false, 0, capture);
            for (i, arg) in call.arguments.iter().enumerate() { out.value(arg, Position::Argument(i), false, 0, capture); }
            out.event(Position::Operation, Effects::DYNAMIC_CALL, None, None, false);
        }
        Statement::MethodCall(call) => {
            out.value(&call.value, Position::Receiver, false, 0, capture);
            for (i, arg) in call.arguments.iter().enumerate() { out.value(arg, Position::Argument(i), false, 0, capture); }
            out.event(Position::Callee, Effects::DYNAMIC_CALL, None, None, false);
            out.event(Position::Operation, Effects::DYNAMIC_CALL, None, None, false);
        }
        Statement::If(branch) => out.value(&branch.condition, Position::Value, false, 0, capture),
        Statement::Return(ret) => {
            for (i, value) in ret.values.iter().enumerate() { out.value(value, Position::Rhs(i), false, 0, capture); }
        }
        Statement::NumericFor(loop_) => {
            for (i, value) in [&loop_.initial, &loop_.limit, &loop_.step].into_iter().enumerate() {
                out.value(value, Position::Rhs(i), false, 0, capture);
            }
        }
        Statement::GenericFor(loop_) => {
            for (i, value) in loop_.right.iter().enumerate() { out.value(value, Position::Rhs(i), false, 0, capture); }
        }
        Statement::SetList(list) => {
            out.event(Position::LhsBase(0), if capture(&list.object_local) { Effects::CAPTURE_READ } else { Effects::default() }, Some(list.object_local.stable_id()), None, false);
            for (i, value) in list.values.iter().enumerate() { out.value(value, Position::Rhs(i), false, 0, capture); }
            if let Some(value) = &list.tail { out.value(value, Position::Rhs(list.values.len()), false, 0, capture); }
            out.event(Position::Store(0), Effects::DYNAMIC_CALL, None, None, false);
        }
        Statement::Empty(_) | Statement::Comment(_) => {}
        // Repeated evaluation and unstructured control require a region proof.
        _ => out.exhausted = true,
    }
    out
}

/// Can an earlier scalar initializer occupy this local's evaluation position?
/// The caller separately proves one use, scope, intervening statements and arity.
pub fn can_sink(statement_: &Statement, local: &RcLocal, replacement: &RValue, capture: &impl Fn(&RcLocal) -> bool) -> bool {
    let candidate = effects::summarize(replacement, capture);
    can_sink_with_summary(statement_, local, replacement, capture, candidate)
}

/// The supplied summary must describe the current candidate under caller-owned
/// runtime facts. Capture/write/conditional and destination order gates remain.
pub(crate) fn can_sink_with_summary(statement_: &Statement, local: &RcLocal, replacement: &RValue,
    capture: &impl Fn(&RcLocal) -> bool, candidate: effects::Summary) -> bool {
    let order = statement(statement_, capture);
    if order.exhausted || candidate.exhausted { return false; }
    let reads = replacement.values_read();
    let mut found = false;
    for event in &order.events {
        if event.read == Some(local.stable_id()) {
            if found || (event.conditional && candidate.effects.has_order_dependency()) { return false; }
            found = true;
        } else if !found {
            if event.write.is_some_and(|id| reads.iter().any(|l| l.stable_id() == id)) { return false; }
            let effect_conflict = !candidate.effects.is_total_pure() && !event.effects.is_total_pure();
            let capture_conflict = (candidate.effects.contains(Effects::CAPTURE_WRITE) && event.effects.contains(Effects::CAPTURE_READ))
                || (candidate.effects.contains(Effects::CAPTURE_READ) && event.effects.contains(Effects::CAPTURE_WRITE));
            if effect_conflict || capture_conflict { return false; }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Call, Index, Literal, Local, MethodCall, Return, Table};
    fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
    fn field(base: &RcLocal) -> RValue { Index::new(base.clone().into(), Literal::String(b"field".to_vec()).into()).into() }

    #[test]
    fn address_reads_precede_rhs_but_terminal_stores_follow_it() {
        let object = local("object"); let value = local("value");
        let rhs = Call::new(value.clone().into(), vec![]).into();
        for nested in [false, true] {
            let base = if nested { field(&object) } else { object.clone().into() };
            let stmt = Assign::new(vec![Index::new(base, Literal::String(b"key".to_vec()).into()).into()], vec![value.clone().into()]).into();
            let order = statement(&stmt, &|_| false);
            let read = order.events.iter().position(|e| e.read == Some(value.stable_id())).unwrap();
            let store = order.events.iter().position(|e| e.position == Position::Store(0)).unwrap();
            assert!(read < store);
            assert_eq!(can_sink(&stmt, &value, &rhs, &|_| false), !nested);
        }
    }

    #[test]
    fn dot_lookup_and_namecall_have_distinct_argument_positions() {
        let object = local("object"); let value = local("value");
        let dot = Call::new(field(&object), vec![value.clone().into()]).into();
        let method = Statement::MethodCall(MethodCall { node_origin: Default::default(), value: Box::new(object.clone().into()), method: "field".into(), arguments: vec![value.clone().into()] });
        assert!(!can_sink(&dot, &value, &field(&object), &|_| false));
        assert!(can_sink(&method, &value, &field(&object), &|_| false));
        // A receiver snapshot must still precede an argument that can change it.
        assert!(!can_sink(&method, &value, &field(&object), &|l| l == &object));
        let ret = Return::new(vec![Literal::Number(std::f64::consts::PI).into(), value.clone().into()]).into();
        assert!(!can_sink(&ret, &value, &field(&object), &|_| false));
    }

    #[test]
    fn conditional_reads_invalid_keys_capture_order_and_budget_refuse() {
        let target = local("v"); let flag = local("flag");
        let replacement = field(&flag);
        let condition = crate::Binary::new(flag.clone().into(), target.clone().into(), BinaryOperation::And).into();
        assert!(!can_sink(&Return::new(vec![condition]).into(), &target, &replacement, &|_| false));
        let table = Table::new(vec![(Some(Literal::Nil.into()), Literal::Nil.into()), (None, target.clone().into())]).into();
        assert!(!can_sink(&Return::new(vec![table]).into(), &target, &replacement, &|_| false));
        let ret = Return::new(vec![flag.clone().into(), target.clone().into()]).into();
        assert!(!can_sink(&ret, &target, &replacement, &|l| l == &flag));
        let wide = Return::new(vec![Literal::Nil.into(); 4097]).into();
        assert!(statement(&wide, &|_| false).exhausted);
    }
}
