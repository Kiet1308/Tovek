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
                closure.visit_local_reads(&mut |local| {
                    self.event(position, if capture(local) { Effects::CAPTURE_READ } else { Effects::default() }, Some(local.stable_id()), None, conditional);
                    !self.exhausted
                });
                if self.exhausted { return; }
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
                value.visit_rvalues(&mut |child| { self.value(child, position, conditional, depth + 1, capture); !self.exhausted });
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

/// In Lua's evaluation order: `Some(true)` when the first observable event of
/// `value` is reading `local`, `Some(false)` when something observable (a
/// call, an index, an operator that may dispatch, a skippable operand) comes
/// first, `None` when `value` does nothing observable and never reads it.
/// Literals are not observable, nor reads `unchanged` says no code can
/// change (a stable local, a constant import): a value computed just before
/// may run code, so any other read it would now precede decides. Unlike the
/// event order above, this answers one question cheaply: may a value computed
/// just before `value` move into that read? `register`: the value standing for
/// `local` is itself a register local, which an operation reads only when it
/// runs ([`late_operands`]); any other value is evaluated where it stands.
pub(crate) fn reads_first(value: &RValue, local: &RcLocal, register: bool, unchanged: &impl Fn(&RValue) -> bool) -> Option<bool> {
    match value {
        RValue::Literal(_) => None,
        RValue::Local(read) if read == local => Some(true),
        // A read the code moved ahead cannot change may be passed over.
        RValue::Local(_) => (!unchanged(value)).then_some(false),
        _ if is_import_path(value) => (!unchanged(value)).then_some(false),
        RValue::Binary(binary) if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) => {
            reads_first(&binary.left, local, register, unchanged).or(Some(false))
        }
        RValue::IfExpression(select) => reads_first(&select.condition, local, register, unchanged).or(Some(false)),
        // Luau hands a register local straight to an arithmetic or comparison
        // instruction, to GETTABLE, and to a builtin's FASTCALL: it is read
        // when the operation runs, after the other operands (`a * (b * 2)`
        // runs `b * 2` before reading `a`).
        _ if late_operands(value).is_some() => {
            let operands = late_operands(value).unwrap();
            // A call Luau may or may not compile to FASTCALL: assume the order
            // that refuses more, early for other locals, late for a register
            // argument in place of `local`.
            let definite = !matches!(value, RValue::Call(_) | RValue::Select(_));
            let late = |operand: &RValue| match operand {
                RValue::Local(read) if read == local => register,
                RValue::Local(_) => definite,
                _ => false,
            };
            let early = operands.iter().filter(|operand| !late(operand));
            if let Some(first) = early.into_iter().find_map(|operand| reads_first(operand, local, register, unchanged)) {
                return Some(first);
            }
            for operand in &operands {
                match operand {
                    RValue::Local(read) if read == local => return Some(true),
                    RValue::Local(_) if !unchanged(operand) => return Some(false),
                    _ => {}
                }
            }
            Some(false)
        }
        _ => {
            let mut first = None;
            value.visit_rvalues(&mut |child| {
                first = reads_first(child, local, register, unchanged);
                first.is_none()
            });
            first.or(Some(false))
        }
    }
}

/// The operands of an operation that reads its register-local operands only
/// when it runs, in evaluation order: arithmetic and comparison, indexing, and
/// a call of a global path Luau may compile to a builtin's FASTCALL.
pub fn late_operands(value: &RValue) -> Option<Vec<&RValue>> {
    match value {
        RValue::Binary(binary) if matches!(
            binary.operation,
            BinaryOperation::Add | BinaryOperation::Sub | BinaryOperation::Mul | BinaryOperation::Div
                | BinaryOperation::IDiv | BinaryOperation::Mod | BinaryOperation::Pow | BinaryOperation::Equal
                | BinaryOperation::NotEqual | BinaryOperation::LessThan | BinaryOperation::LessThanOrEqual
                | BinaryOperation::GreaterThan | BinaryOperation::GreaterThanOrEqual
        ) => Some(vec![&binary.left, &binary.right]),
        RValue::Index(index) => Some(vec![&index.left, &index.right]),
        RValue::Call(call) | RValue::Select(crate::Select::Call(call)) if is_import_path(&call.value) => {
            Some(call.arguments.iter().collect())
        }
        _ => None,
    }
}

/// [`reads_first`] over a block: a statement that only binds locals to
/// unobservable values is passed over, any other statement decides.
pub(crate) fn block_reads_first(stmts: &[Statement], local: &RcLocal, register: bool, unchanged: &impl Fn(&RValue) -> bool) -> bool {
    fn first_of<'a>(
        values: impl IntoIterator<Item = &'a RValue>,
        local: &RcLocal,
        register: bool,
        unchanged: &impl Fn(&RValue) -> bool,
    ) -> Option<bool> {
        values.into_iter().find_map(|value| reads_first(value, local, register, unchanged))
    }
    for statement in stmts {
        let first = match statement {
            // Store addresses, then values, then the stores (see `statement`).
            Statement::Assign(assign) => {
                let addresses = assign.left.iter().flat_map(|lhs| match lhs {
                    LValue::Index(index) => [Some(&*index.left), Some(&*index.right)],
                    _ => [None, None],
                });
                let first = first_of(addresses.flatten().chain(&assign.right), local, register, unchanged);
                // A store into a table or a global can run `__newindex`; one
                // into a cell the moved value may read changes what it reads.
                let stores_observably = assign.left.iter().any(|lhs| match lhs {
                    LValue::Local(stored) => !unchanged(&RValue::Local(stored.clone())),
                    _ => true,
                });
                if stores_observably { first.or(Some(false)) } else { first }
            }
            Statement::Call(call) => first_of(std::iter::once(&*call.value).chain(&call.arguments), local, register, unchanged).or(Some(false)),
            Statement::MethodCall(call) => {
                first_of(std::iter::once(&*call.value).chain(&call.arguments), local, register, unchanged).or(Some(false))
            }
            Statement::If(branch) => reads_first(&branch.condition, local, register, unchanged).or(Some(false)),
            Statement::Return(ret) => first_of(&ret.values, local, register, unchanged).or(Some(false)),
            Statement::Empty(_) | Statement::Comment(_) => None,
            _ => Some(false),
        };
        if let Some(found) = first {
            return found;
        }
    }
    false
}

fn is_import_path(value: &RValue) -> bool {
    match value {
        RValue::Global(_) => true,
        RValue::Index(index) => {
            matches!(index.right.as_ref(), RValue::Literal(crate::Literal::String(_))) && is_import_path(&index.left)
        }
        _ => false,
    }
}

/// Can an earlier scalar initializer occupy this local's evaluation position?
/// The caller separately proves one use, scope, intervening statements and arity.
pub fn can_sink(statement_: &Statement, local: &RcLocal, replacement: &RValue, capture: &impl Fn(&RcLocal) -> bool) -> bool {
    let candidate = effects::summarize(replacement, capture);
    can_sink_with_summary(statement_, local, replacement, capture, candidate)
}

/// A captured-cell snapshot can replace every direct read only when no earlier
/// operation can change that cell. Unlike `can_sink`, aliases may have multiple
/// reads in one statement. Nested control flow is checked by the caller.
pub fn can_reuse_capture(statement_: &Statement, local: &RcLocal) -> bool {
    let order = statement(statement_, &|_| false);
    if order.exhausted { return false; }
    // Read where its operation runs, the cell is read after every other
    // operand of it: `v + touch()` with `v` standing for the cell.
    if late_operand_conflict(statement_, local, &effects::may_write_capture) { return false; }
    let mut may_write = false;
    let mut found = false;
    for event in &order.events {
        if event.read == Some(local.stable_id()) {
            if may_write { return false; }
            found = true;
        }
        may_write |= event.effects.contains(Effects::CAPTURE_WRITE);
    }
    found
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
    // A local read sunk into an operand Luau reads only when its operation
    // runs happens after the operation's other operands, whichever side they
    // stand on: `local v = total; v + touch()` must not read `total` after
    // `touch` writes it.
    if found
        && matches!(replacement, RValue::Local(_))
        && candidate.effects.contains(Effects::CAPTURE_READ)
        && late_operand_conflict(statement_, local, &|operand| {
            effects::summarize(operand, capture).effects.contains(Effects::CAPTURE_WRITE)
        })
    {
        return false;
    }
    found
}

/// Whether `local` stands as a register operand of an operation that reads it
/// only when it runs ([`late_operands`]) beside another operand `conflicts`
/// holds for. Closure bodies are not entered.
fn late_operand_conflict(statement: &Statement, local: &RcLocal, conflicts: &impl Fn(&RValue) -> bool) -> bool {
    fn in_value(value: &RValue, local: &RcLocal, conflicts: &impl Fn(&RValue) -> bool) -> bool {
        if let Some(operands) = late_operands(value)
            && operands.iter().any(|operand| matches!(operand, RValue::Local(read) if read == local))
            && operands.iter().any(|operand| !matches!(operand, RValue::Local(read) if read == local) && conflicts(operand))
        {
            return true;
        }
        if matches!(value, RValue::Closure(_)) {
            return false;
        }
        let mut found = false;
        value.visit_rvalues(&mut |child| {
            found = in_value(child, local, conflicts);
            !found
        });
        found
    }
    let mut found = false;
    statement.visit_rvalues(&mut |value| {
        found = in_value(value, local, conflicts);
        !found
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Call, Index, Literal, Local, MethodCall, Return, Table};
    fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
    fn field(base: &RcLocal) -> RValue { Index::new(base.clone().into(), Literal::String(b"field".to_vec()).into()).into() }

    #[test]
    fn capture_alias_checks_every_read_but_not_effects_after_the_last() {
        let snapshot = local("snapshot");
        let mutate = local("mutate");
        let call: RValue = Call::new(mutate.into(), vec![]).into();
        let read: RValue = snapshot.clone().into();
        assert!(can_reuse_capture(&Return::new(vec![read.clone(), call.clone()]).into(), &snapshot));
        assert!(!can_reuse_capture(&Return::new(vec![call.clone(), read.clone()]).into(), &snapshot));
        assert!(!can_reuse_capture(&Return::new(vec![read.clone(), call, read]).into(), &snapshot));
        assert!(can_reuse_capture(&Return::new(vec![field(&snapshot)]).into(), &snapshot));
        // A global callee lookup may dispatch __index before its argument read.
        let global = crate::Global(b"print".to_vec()).into();
        assert!(!can_reuse_capture(&Call::new(global, vec![snapshot.clone().into()]).into(), &snapshot));
    }

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
    fn a_block_reads_first_through_local_bindings_and_store_addresses() {
        let (param, object, other) = (local("param"), local("object"), local("other"));
        let call = |args: Vec<RValue>| -> RValue { Call::new(crate::Global(b"f".to_vec()).into(), args).into() };
        let store = |base: RValue, value: RValue| -> Statement {
            Assign::new(vec![Index::new(base, Literal::String(b"key".to_vec()).into()).into()], vec![value]).into()
        };
        let bind = |value: RValue| -> Statement { Assign::new(vec![other.clone().into()], vec![value]).into() };
        // `local other = 1; object.key = param`: the address is only a local.
        let unchanged = |_: &RValue| true;
        assert!(block_reads_first(&[bind(Literal::Number(1.0).into()), store(object.clone().into(), param.clone().into())], &param, false, &unchanged));
        // `object.field.key = param`: looking up the address can run code.
        assert!(!block_reads_first(&[store(field(&object), param.clone().into())], &param, false, &unchanged));
        // `f(); return param` and `local other = f(param)`.
        assert!(!block_reads_first(&[Statement::Call(Call::new(crate::Global(b"f".to_vec()).into(), vec![])), Return::new(vec![param.clone().into()]).into()], &param, false, &unchanged));
        assert!(block_reads_first(&[bind(call(vec![param.clone().into()]))], &param, false, &unchanged));
        // A value that never reads it decides nothing, a store does.
        assert!(!block_reads_first(&[store(object.clone().into(), other.clone().into()), Return::new(vec![param.clone().into()]).into()], &param, false, &unchanged));
        // `return x + p` with `x` a cell a call may write: Luau reads the
        // register `x` when `+` runs. An argument expression in place of `p`
        // is evaluated before that read; a register argument is read with it,
        // after `x`.
        let changed = |_: &RValue| false;
        let sum = RValue::Binary(crate::Binary::new(other.clone().into(), param.clone().into(), crate::BinaryOperation::Add));
        assert!(block_reads_first(&[Return::new(vec![sum.clone()]).into()], &param, false, &changed));
        assert!(!block_reads_first(&[Return::new(vec![sum.clone()]).into()], &param, true, &changed));
        assert!(block_reads_first(&[Return::new(vec![sum]).into()], &param, true, &unchanged));
        // `return f(x) * p`: the call runs before `p` either way.
        let call_first = RValue::Binary(crate::Binary::new(call(vec![other.clone().into()]), param.clone().into(), crate::BinaryOperation::Mul));
        assert!(!block_reads_first(&[Return::new(vec![call_first]).into()], &param, false, &unchanged));
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
