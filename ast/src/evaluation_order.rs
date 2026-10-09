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
    statement_with_registers(statement, capture, &|_| false)
}

/// As [`statement`], `register` telling the locals the statement's function
/// holds in registers: a store reads such a base or key when it runs, after
/// every value the statement assigns (`t.x = f()` reads `t` after `f`); an
/// upvalue or any other address is evaluated before them.
pub fn statement_with_registers(statement: &Statement, capture: &impl Fn(&RcLocal) -> bool,
    register: &impl Fn(&RcLocal) -> bool) -> Order {
    let mut out = Order::default();
    match statement {
        Statement::Assign(assign) => {
            let late = |value: &RValue| matches!(value, RValue::Local(local) if register(local));
            for (i, lhs) in assign.left.iter().enumerate() {
                if let LValue::Index(index) = lhs {
                    if !late(&index.left) { out.value(&index.left, Position::LhsBase(i), false, 0, capture); }
                    if !late(&index.right) { out.value(&index.right, Position::LhsKey(i), false, 0, capture); }
                }
            }
            for (i, rhs) in assign.right.iter().enumerate() { out.value(rhs, Position::Rhs(i), false, 0, capture); }
            // Other addresses precede all RHS evaluations; stores follow
            // them in order, each reading its register base and key first.
            // No replacement is attempted inside a store event.
            for (i, lhs) in assign.left.iter().enumerate() {
                if let LValue::Index(index) = lhs {
                    if late(&index.left) { out.value(&index.left, Position::LhsBase(i), false, 0, capture); }
                    if late(&index.right) { out.value(&index.right, Position::LhsKey(i), false, 0, capture); }
                }
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

/// What [`reads_first`] knows of the body it reads.
pub(crate) struct Body<'a> {
    /// The locals the body's function holds in registers, which an operation
    /// reads when it runs; any other is an upvalue, fetched where it stands.
    pub registers: &'a dyn Fn(&RcLocal) -> bool,
    /// Reads no code can change: a stable local, a constant import.
    pub unchanged: &'a dyn Fn(&RValue) -> bool,
}

impl Body<'_> {
    fn in_register(&self, local: &RcLocal) -> bool { (self.registers)(local) }
    fn unchanged(&self, value: &RValue) -> bool { (self.unchanged)(value) }
}

/// The single-local question [`LeadingReads`] answers for every parameter
/// at once, kept as its specification: checked against it in debug builds
/// and tests.
///
/// In Lua's evaluation order: `Some(true)` when the first observable event of
/// `value` is reading `local`, `Some(false)` when something observable (a
/// call, an index, an operator that may dispatch, a skippable operand) comes
/// first, `None` when `value` does nothing observable and never reads it.
/// Literals are not observable, nor reads `body.unchanged` says no code can
/// change (a stable local, a constant import): a value computed just before
/// may run code, so any other read it would now precede decides. Unlike the
/// event order above, this answers one question cheaply: may a value computed
/// just before `value` move into that read? `register`: the value standing for
/// `local` is itself a register local, which an operation reads only when it
/// runs ([`late_operands`]); any other value is evaluated where it stands.
#[cfg(any(test, debug_assertions))]
pub(crate) fn reads_first(value: &RValue, local: &RcLocal, register: bool, body: &Body) -> Option<bool> {
    match value {
        RValue::Literal(_) => None,
        RValue::Local(read) if read == local => Some(true),
        // A read the code moved ahead cannot change may be passed over.
        RValue::Local(_) => (!body.unchanged(value)).then_some(false),
        _ if is_import_path(value) => (!body.unchanged(value)).then_some(false),
        RValue::Binary(binary) if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) => {
            reads_first(&binary.left, local, register, body).or(Some(false))
        }
        RValue::IfExpression(select) => reads_first(&select.condition, local, register, body).or(Some(false)),
        RValue::Call(call) | RValue::Select(Select::Call(call)) => call_reads_first(call, local, register, body),
        RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
            method_call_reads_first(call, local, register, body)
        }
        // Luau hands a register local straight to an arithmetic or comparison
        // instruction and to GETTABLE: it is read when the operation runs,
        // after the other operands (`a * (b * 2)` runs `b * 2` before reading
        // `a`).
        _ if late_operands(value).is_some() => {
            late_operands_read_first(&late_operands(value).unwrap(), true, local, register, body)
        }
        // A constructor's SETTABLE reads a register key after the field's value.
        RValue::Table(table) if register => {
            let first = table.0.iter().find_map(|(key, item)| match key {
                Some(RValue::Local(read)) if read == local => reads_first(item, local, register, body).or(Some(true)),
                Some(key) => reads_first(key, local, register, body).or_else(|| reads_first(item, local, register, body)),
                None => reads_first(item, local, register, body),
            });
            first.or(Some(false))
        }
        _ => {
            let mut first = None;
            value.visit_rvalues(&mut |child| {
                first = reads_first(child, local, register, body);
                first.is_none()
            });
            first.or(Some(false))
        }
    }
}

/// [`reads_first`] of a call: a builtin's FASTCALL takes a register-local
/// argument as it is, when it runs ([`fastcall_arguments`]); any other call
/// fetches its callee and copies its arguments in order.
#[cfg(any(test, debug_assertions))]
fn call_reads_first(call: &crate::Call, local: &RcLocal, register: bool, body: &Body) -> Option<bool> {
    match fastcall_arguments(call) {
        // Luau may or may not compile it to FASTCALL: assume the order that
        // refuses more, early for other locals, late for a register argument
        // in place of `local`.
        Some(arguments) => late_operands_read_first(&arguments.iter().collect::<Vec<_>>(), false, local, register, body),
        None => std::iter::once(&*call.value).chain(&call.arguments)
            .find_map(|value| reads_first(value, local, register, body)).or(Some(false)),
    }
}

/// [`reads_first`] of a method call: NAMECALL reads a register receiver after
/// the arguments, statement or expression alike.
#[cfg(any(test, debug_assertions))]
fn method_call_reads_first(call: &crate::MethodCall, local: &RcLocal, register: bool, body: &Body) -> Option<bool> {
    if register && matches!(call.value.as_ref(), RValue::Local(read) if read == local) {
        return call.arguments.iter().find_map(|argument| reads_first(argument, local, register, body)).or(Some(true));
    }
    std::iter::once(&*call.value).chain(&call.arguments)
        .find_map(|value| reads_first(value, local, register, body)).or(Some(false))
}

/// [`reads_first`] of an operation reading its register-local operands when
/// it runs. `definite`: it surely does; otherwise only a register argument in
/// place of `local` counts as read late.
#[cfg(any(test, debug_assertions))]
fn late_operands_read_first(operands: &[&RValue], definite: bool, local: &RcLocal, register: bool,
    body: &Body) -> Option<bool> {
    let late = |operand: &RValue| match operand {
        RValue::Local(read) if read == local => register,
        RValue::Local(read) => definite && body.in_register(read),
        _ => false,
    };
    if let Some(first) = operands.iter().filter(|operand| !late(operand))
        .find_map(|operand| reads_first(operand, local, register, body))
    {
        return Some(first);
    }
    for operand in operands {
        match operand {
            RValue::Local(read) if read == local => return Some(true),
            RValue::Local(_) if !body.unchanged(operand) => return Some(false),
            _ => {}
        }
    }
    Some(false)
}

/// The operands of an operation that reads its register-local operands only
/// when it runs, in evaluation order: arithmetic and comparison, indexing, and
/// a call Luau may compile to a builtin's FASTCALL ([`fastcall_arguments`]).
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
        RValue::Call(call) | RValue::Select(crate::Select::Call(call)) => {
            fastcall_arguments(call).map(|arguments| arguments.iter().collect())
        }
        _ => None,
    }
}

/// The arguments of a call Luau may compile to a builtin's FASTCALL1/2/3, which
/// takes a register-local argument as it is: a global path given one to three
/// arguments, the last giving one value. A last call or `...` passes as one
/// value only at -O2, to a fixed-arity numeric builtin (math, bit32, buffer,
/// vector, integer) or when it is a builtin call known to give one result.
/// Any other call copies its arguments into place in order.
fn fastcall_arguments(call: &crate::Call) -> Option<&[RValue]> {
    fn fixed_arity_library(path: &RValue) -> bool {
        match path {
            RValue::Index(index) => fixed_arity_library(&index.left),
            RValue::Global(global) => matches!(global.0.as_slice(), b"math" | b"bit32" | b"buffer" | b"vector" | b"integer"),
            _ => false,
        }
    }
    let arguments = call.arguments.as_slice();
    let direct = is_import_path(&call.value)
        && arguments.len() <= 3
        && match arguments.last() {
            Some(RValue::Call(last)) => is_import_path(&last.value) || fixed_arity_library(&call.value),
            Some(RValue::MethodCall(_) | RValue::VarArg(_)) => fixed_arity_library(&call.value),
            _ => true,
        };
    direct.then_some(arguments)
}

/// [`reads_first`] over a block: a statement that only binds locals to
/// unobservable values is passed over, any other statement decides.
#[cfg(any(test, debug_assertions))]
pub(crate) fn block_reads_first(stmts: &[Statement], local: &RcLocal, register: bool, body: &Body) -> bool {
    fn first_of<'a>(
        values: impl IntoIterator<Item = &'a RValue>,
        local: &RcLocal,
        register: bool,
        body: &Body,
    ) -> Option<bool> {
        values.into_iter().find_map(|value| reads_first(value, local, register, body))
    }
    for statement in stmts {
        let first = match statement {
            // Addresses evaluated where they stand, then the values, then the
            // register bases and keys SETTABLE reads (see `statement`).
            Statement::Assign(assign) => {
                let late = |address: &RValue| match address {
                    RValue::Local(read) if read == local => register,
                    RValue::Local(read) => body.in_register(read),
                    _ => false,
                };
                let addresses = || assign.left.iter().filter_map(LValue::as_index).flat_map(|index| [&*index.left, &*index.right]);
                let ordered = addresses().filter(|address| !late(address))
                    .chain(&assign.right)
                    .chain(addresses().filter(|address| late(address)));
                let first = first_of(ordered, local, register, body);
                // A store into a table or a global can run `__newindex`; one
                // into a cell the moved value may read changes what it reads.
                let stores_observably = assign.left.iter().any(|lhs| match lhs {
                    LValue::Local(stored) => !body.unchanged(&RValue::Local(stored.clone())),
                    _ => true,
                });
                if stores_observably { first.or(Some(false)) } else { first }
            }
            // The same order as the call as an expression (a register
            // receiver after the arguments, a FASTCALL argument when it runs).
            Statement::Call(call) => call_reads_first(call, local, register, body),
            Statement::MethodCall(call) => method_call_reads_first(call, local, register, body),
            Statement::If(branch) => reads_first(&branch.condition, local, register, body).or(Some(false)),
            Statement::Return(ret) => first_of(&ret.values, local, register, body).or(Some(false)),
            Statement::Empty(_) | Statement::Comment(_) => None,
            _ => Some(false),
        };
        if let Some(found) = first {
            return found;
        }
    }
    false
}

/// The parameters a helper body reads before its first observable event, in
/// the order it reads them: the ordered leading reads. Luau evaluates every
/// argument of an inlined call into a register, in parameter order, before
/// the body (`compileInlinedCall`); a site that evaluates each argument at
/// its parameter's one read instead keeps that order when those reads come
/// in parameter order with nothing observable before or between them. So the
/// arguments that may run code or change can all move back into the call
/// when they are a subsequence of these reads ([`LeadingReads::admits`]).
///
/// Computed once per helper, in one walk of its body per argument kind: an
/// argument evaluated where it stands, and a register local, which an
/// operation reads only when it runs ([`reads_first`]'s `register`).
#[derive(Clone, Default)]
pub(crate) struct LeadingReads {
    values: Vec<RcLocal>,
    registers: Vec<RcLocal>,
}

impl LeadingReads {
    /// `read_once`: the parameters the body reads exactly once; only those
    /// may stand for an argument evaluated at its read.
    pub(crate) fn new(stmts: &[Statement], params: &[RcLocal], read_once: impl Fn(&RcLocal) -> bool, body: &Body) -> Self {
        let walk = |register: bool| {
            let mut reads = Vec::new();
            block_leading_reads(stmts, &|local: &RcLocal| params.contains(local), register, body, &mut reads);
            // Each parameter's first read; a later one is never leading.
            let mut seen = Vec::with_capacity(reads.len());
            reads.retain(|param| !seen.contains(param) && { seen.push(param.clone()); true });
            reads.retain(|param| read_once(param));
            reads
        };
        let leading = Self { values: walk(false), registers: walk(true) };
        // A parameter leads exactly when it is the body's first read in the
        // single-local question; pinned so the two never drift apart. (A
        // parameter no code can change is passed over by both; one that may,
        // only possible with an exhausted census, is ordered more precisely
        // here.)
        #[cfg(debug_assertions)]
        if params.iter().all(|param| body.unchanged(&RValue::Local(param.clone()))) {
            for param in params.iter().filter(|param| read_once(param)) {
                for (register, list) in [(false, &leading.values), (true, &leading.registers)] {
                    debug_assert_eq!(list.contains(param), block_reads_first(stmts, param, register, body));
                }
            }
        }
        leading
    }

    /// Whether the arguments that may run code or change, given in parameter
    /// order with whether each is a register local, may be evaluated before
    /// the body: each is a leading read, read once, and they come in this
    /// order. All of one kind, as the two lists order reads differently.
    pub(crate) fn admits<'a>(&self, unstable: impl IntoIterator<Item = (&'a RcLocal, bool)>) -> bool {
        let mut kind = None;
        let mut next = 0;
        for (param, register) in unstable {
            if *kind.get_or_insert(register) != register {
                return false;
            }
            let list = if register { &self.registers } else { &self.values };
            match list[next..].iter().position(|read| read == param) {
                Some(at) => next += at + 1,
                None => return false,
            }
        }
        true
    }
}

/// [`reads_first`] for a set of locals at once: appends each read of one of
/// `params` to `reads`, in Lua's evaluation order, until the first
/// observable event, and returns whether `value` had none. The cases mirror
/// [`reads_first`] one for one (`LeadingReads::new` checks they agree).
fn leading_reads(value: &RValue, params: &dyn Fn(&RcLocal) -> bool, register: bool, body: &Body,
    reads: &mut Vec<RcLocal>) -> bool {
    match value {
        RValue::Literal(_) => true,
        RValue::Local(read) if params(read) => {
            reads.push(read.clone());
            body.unchanged(value)
        }
        RValue::Local(_) => body.unchanged(value),
        _ if is_import_path(value) => body.unchanged(value),
        RValue::Binary(binary) if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) => {
            leading_reads(&binary.left, params, register, body, reads);
            false
        }
        RValue::IfExpression(select) => {
            leading_reads(&select.condition, params, register, body, reads);
            false
        }
        RValue::Call(call) | RValue::Select(Select::Call(call)) => call_leading_reads(call, params, register, body, reads),
        RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
            method_call_leading_reads(call, params, register, body, reads)
        }
        _ if late_operands(value).is_some() => {
            late_operands_leading_reads(&late_operands(value).unwrap(), true, params, register, body, reads)
        }
        RValue::Table(table) if register => {
            for (key, item) in &table.0 {
                let clear = match key {
                    Some(RValue::Local(read)) if params(read) => {
                        leading_reads(item, params, register, body, reads) && {
                            reads.push(read.clone());
                            body.unchanged(key.as_ref().unwrap())
                        }
                    }
                    Some(key) => leading_reads(key, params, register, body, reads)
                        && leading_reads(item, params, register, body, reads),
                    None => leading_reads(item, params, register, body, reads),
                };
                if !clear {
                    return false;
                }
            }
            false
        }
        _ => {
            value.visit_rvalues(&mut |child| leading_reads(child, params, register, body, reads));
            false
        }
    }
}

/// [`call_reads_first`] for a set of locals.
fn call_leading_reads(call: &crate::Call, params: &dyn Fn(&RcLocal) -> bool, register: bool, body: &Body,
    reads: &mut Vec<RcLocal>) -> bool {
    match fastcall_arguments(call) {
        Some(arguments) => {
            late_operands_leading_reads(&arguments.iter().collect::<Vec<_>>(), false, params, register, body, reads)
        }
        None => {
            std::iter::once(&*call.value).chain(&call.arguments).all(|value| leading_reads(value, params, register, body, reads));
            false
        }
    }
}

/// [`method_call_reads_first`] for a set of locals.
fn method_call_leading_reads(call: &crate::MethodCall, params: &dyn Fn(&RcLocal) -> bool, register: bool, body: &Body,
    reads: &mut Vec<RcLocal>) -> bool {
    if register && let RValue::Local(receiver) = call.value.as_ref() && params(receiver) {
        if call.arguments.iter().all(|argument| leading_reads(argument, params, register, body, reads)) {
            reads.push(receiver.clone());
        }
        return false;
    }
    std::iter::once(&*call.value).chain(&call.arguments).all(|value| leading_reads(value, params, register, body, reads));
    false
}

/// [`late_operands_read_first`] for a set of locals: the operands evaluated
/// where they stand first, then those read when the operation runs.
fn late_operands_leading_reads(operands: &[&RValue], definite: bool, params: &dyn Fn(&RcLocal) -> bool, register: bool,
    body: &Body, reads: &mut Vec<RcLocal>) -> bool {
    let late = |operand: &RValue| match operand {
        RValue::Local(read) if params(read) => register,
        RValue::Local(read) => definite && body.in_register(read),
        _ => false,
    };
    if !operands.iter().filter(|operand| !late(operand)).all(|operand| leading_reads(operand, params, register, body, reads)) {
        return false;
    }
    for operand in operands.iter().filter(|operand| late(operand)) {
        if let RValue::Local(read) = operand
            && params(read)
        {
            reads.push(read.clone());
        }
        if !body.unchanged(operand) {
            return false;
        }
    }
    false
}

/// [`block_reads_first`] for a set of locals.
fn block_leading_reads(stmts: &[Statement], params: &dyn Fn(&RcLocal) -> bool, register: bool, body: &Body,
    reads: &mut Vec<RcLocal>) {
    for statement in stmts {
        let clear = match statement {
            Statement::Assign(assign) => {
                let late = |address: &RValue| match address {
                    RValue::Local(read) if params(read) => register,
                    RValue::Local(read) => body.in_register(read),
                    _ => false,
                };
                let addresses = || assign.left.iter().filter_map(LValue::as_index).flat_map(|index| [&*index.left, &*index.right]);
                let clear = addresses().filter(|address| !late(address))
                    .chain(&assign.right)
                    .chain(addresses().filter(|address| late(address)))
                    .all(|value| leading_reads(value, params, register, body, reads));
                let stores_observably = assign.left.iter().any(|lhs| match lhs {
                    LValue::Local(stored) => !body.unchanged(&RValue::Local(stored.clone())),
                    _ => true,
                });
                clear && !stores_observably
            }
            Statement::Call(call) => call_leading_reads(call, params, register, body, reads),
            Statement::MethodCall(call) => method_call_leading_reads(call, params, register, body, reads),
            Statement::If(branch) => {
                leading_reads(&branch.condition, params, register, body, reads);
                false
            }
            Statement::Return(ret) => {
                ret.values.iter().all(|value| leading_reads(value, params, register, body, reads));
                false
            }
            Statement::Empty(_) | Statement::Comment(_) => true,
            _ => false,
        };
        if !clear {
            return;
        }
    }
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
    can_sink_with_summary(statement_, local, replacement, capture, &|_| false, candidate)
}

/// A captured-cell snapshot can replace every direct read only when no earlier
/// operation can change that cell. Unlike `can_sink`, aliases may have multiple
/// reads in one statement. Nested control flow is checked by the caller.
/// `register`: the cell is a register of the statement's function, which Luau
/// reads where the instruction consuming it runs; an upvalue is fetched where
/// it stands.
pub fn can_reuse_capture(statement_: &Statement, local: &RcLocal, register: bool) -> bool {
    let order = statement_with_registers(statement_, &|_| false, &|read| register && read == local);
    if order.exhausted { return false; }
    // Read where its operation runs, the cell is read after every other
    // operand of it: `v + touch()` with `v` standing for the cell.
    if register && late_operand_conflict(statement_, local, &effects::may_write_capture) { return false; }
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
/// `register` tells the locals of the statement's function held in registers
/// ([`statement_with_registers`]); `local` is read where `replacement` is,
/// late only when that is a register too.
pub(crate) fn can_sink_with_summary(statement_: &Statement, local: &RcLocal, replacement: &RValue,
    capture: &impl Fn(&RcLocal) -> bool, register: &impl Fn(&RcLocal) -> bool, candidate: effects::Summary) -> bool {
    let replacement_register = matches!(replacement, RValue::Local(read) if register(read));
    let order = statement_with_registers(statement_, capture,
        &|read| if read == local { replacement_register } else { register(read) });
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

/// Whether `local`, standing where Luau hands a register local straight to the
/// instruction consuming it, is read there after a value `conflicts` holds
/// for. That instruction runs once the values around it are evaluated: an
/// operation of [`late_operands`] after its other operands, NAMECALL after
/// the arguments of a method call on `local`, a constructor's SETTABLE after
/// the value of the field `local` keys. A store address is the event order's
/// ([`statement_with_registers`], [`late_store_conflict`]). Closure bodies are
/// not entered.
pub fn late_operand_conflict(statement: &Statement, local: &RcLocal, conflicts: &impl Fn(&RValue) -> bool) -> bool {
    let is_local = |value: &RValue| matches!(value, RValue::Local(read) if read == local);
    let at_statement = match statement {
        Statement::Call(call) => fastcall_arguments(call).is_some_and(|arguments| operands_conflict(arguments.iter(), &is_local, conflicts)),
        Statement::MethodCall(call) => is_local(&call.value) && call.arguments.iter().any(conflicts),
        _ => false,
    };
    at_statement
        || !statement.visit_lvalues(&mut |lhs| lhs.visit_rvalues(&mut |value| !late_operand_conflict_in(value, &is_local, conflicts)))
        || !statement.visit_rvalues(&mut |value| !late_operand_conflict_in(value, &is_local, conflicts))
}

/// [`late_operand_conflict`] within one expression, `value` included.
pub fn value_late_operand_conflict(value: &RValue, local: &RcLocal, conflicts: &impl Fn(&RValue) -> bool) -> bool {
    late_operand_conflict_in(value, &|value: &RValue| matches!(value, RValue::Local(read) if read == local), conflicts)
}

fn late_operand_conflict_in(value: &RValue, is_local: &impl Fn(&RValue) -> bool, conflicts: &impl Fn(&RValue) -> bool) -> bool {
    let here = match value {
        RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
            is_local(&call.value) && call.arguments.iter().any(conflicts)
        }
        RValue::Table(table) => table.0.iter().any(|(key, item)| key.as_ref().is_some_and(is_local) && conflicts(item)),
        RValue::Closure(_) => return false,
        _ => late_operands(value).is_some_and(|operands| operands_conflict(operands.into_iter(), is_local, conflicts)),
    };
    here || !value.visit_rvalues(&mut |child| !late_operand_conflict_in(child, is_local, conflicts))
}

/// Whether `local`, a register local standing as the base or key of a store,
/// is read after a value `conflicts` holds for: SETTABLE runs after every
/// other value of its assignment and after the stores before it, which may
/// run `__newindex` ([`statement_with_registers`]).
pub fn late_store_conflict(statement: &Statement, local: &RcLocal, conflicts: &impl Fn(&RValue) -> bool) -> bool {
    let Statement::Assign(assign) = statement else { return false };
    let is_local = |value: &RValue| matches!(value, RValue::Local(read) if read == local);
    let addresses = || assign.left.iter().filter_map(LValue::as_index).flat_map(|index| [index.left.as_ref(), index.right.as_ref()]);
    addresses().any(is_local)
        && (assign.left.iter().filter(|lhs| !matches!(lhs, LValue::Local(_))).count() > 1
            || addresses().chain(&assign.right).any(|value| !is_local(value) && conflicts(value)))
}

/// Whether code reading `local` from an upvalue would see another value than
/// these statements reading it from their function's register: some
/// operation of theirs, nested control flow included (closure bodies run
/// elsewhere), reads the register `local` when it runs, after a value
/// `may_change` says may write it ([`late_operand_conflict`],
/// [`late_store_conflict`]). An upvalue is fetched (GETUPVAL) where it
/// stands, before that value.
pub fn region_late_read_conflict(stmts: &[Statement], local: &RcLocal, may_change: &impl Fn(&RValue) -> bool) -> bool {
    stmts.iter().any(|statement| {
        late_operand_conflict(statement, local, may_change)
            || late_store_conflict(statement, local, may_change)
            || match statement {
                Statement::If(node) => {
                    region_late_read_conflict(&node.then_block.lock().0, local, may_change)
                        || region_late_read_conflict(&node.else_block.lock().0, local, may_change)
                }
                Statement::While(node) => region_late_read_conflict(&node.block.lock().0, local, may_change),
                Statement::Repeat(node) => region_late_read_conflict(&node.block.lock().0, local, may_change),
                Statement::NumericFor(node) => region_late_read_conflict(&node.block.lock().0, local, may_change),
                Statement::GenericFor(node) => region_late_read_conflict(&node.block.lock().0, local, may_change),
                _ => false,
            }
    })
}

/// One of `operands` is `local` and another one `conflicts` holds for.
fn operands_conflict<'a>(operands: impl Iterator<Item = &'a RValue> + Clone, is_local: &impl Fn(&RValue) -> bool,
    conflicts: &impl Fn(&RValue) -> bool) -> bool {
    operands.clone().any(is_local) && operands.filter(|operand| !is_local(operand)).any(conflicts)
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
        assert!(can_reuse_capture(&Return::new(vec![read.clone(), call.clone()]).into(), &snapshot, true));
        assert!(!can_reuse_capture(&Return::new(vec![call.clone(), read.clone()]).into(), &snapshot, true));
        assert!(!can_reuse_capture(&Return::new(vec![read.clone(), call, read]).into(), &snapshot, true));
        assert!(can_reuse_capture(&Return::new(vec![field(&snapshot)]).into(), &snapshot, true));
        // A global callee lookup may dispatch __index before its argument read.
        let global = crate::Global(b"print".to_vec()).into();
        assert!(!can_reuse_capture(&Call::new(global, vec![snapshot.clone().into()]).into(), &snapshot, true));
    }

    #[test]
    fn register_operands_wait_for_their_instruction() {
        let (cell, other) = (local("cell"), local("other"));
        let call = |name: &str, args: Vec<RValue>| -> RValue { Call::new(crate::Global(name.as_bytes().to_vec()).into(), args).into() };
        let path = |library: &str, name: &str| -> RValue {
            Index::new(crate::Global(library.as_bytes().to_vec()).into(), Literal::String(name.as_bytes().to_vec()).into()).into()
        };
        let method = |object: &RcLocal, args: Vec<RValue>| -> RValue {
            MethodCall { node_origin: Default::default(), value: Box::new(object.clone().into()), method: "m".into(), arguments: args }.into()
        };
        let change = || call("change", vec![]);
        let writes = &effects::may_write_capture;
        let returns = |value: RValue| -> Statement { Return::new(vec![value]).into() };
        // NAMECALL runs after the arguments, which are copied in order.
        assert!(late_operand_conflict(&returns(method(&cell, vec![change()])), &cell, writes));
        assert!(!late_operand_conflict(&returns(method(&other, vec![cell.clone().into(), change()])), &cell, writes));
        // A constructor's SETTABLE runs after the field's value.
        let keyed = Table::new(vec![(Some(cell.clone().into()), change())]).into();
        assert!(late_operand_conflict(&returns(keyed), &cell, writes));
        // FASTCALL2 takes a register argument as is; a last argument giving
        // all its results leaves the copies, but for a fixed-arity numeric
        // builtin or a builtin call giving one result at -O2.
        let insert = |last: RValue| -> Statement {
            Statement::Call(Call::new(path("table", "insert"), vec![cell.clone().into(), last]))
        };
        assert!(late_operand_conflict(&insert(change()), &cell, writes));
        assert!(!late_operand_conflict(&insert(method(&other, vec![])), &cell, writes));
        let fmod = Call::new(path("math", "fmod"), vec![cell.clone().into(), method(&other, vec![])]).into();
        assert!(late_operand_conflict(&returns(fmod), &cell, writes));
        let wide = Call::new(path("math", "max"), vec![cell.clone().into(), change(), Literal::Number(1.0).into(), Literal::Number(2.0).into()]).into();
        assert!(!late_operand_conflict(&returns(wide), &cell, writes));
        // As the base or key of a store, after the values and earlier stores.
        let store = |lhs: Vec<LValue>, rhs: Vec<RValue>| -> Statement { Assign::new(lhs, rhs).into() };
        let at = |base: &RcLocal, key: RValue| -> LValue { Index::new(base.clone().into(), key).into() };
        assert!(late_store_conflict(&store(vec![at(&other, cell.clone().into())], vec![change()]), &cell, writes));
        assert!(late_store_conflict(&store(vec![at(&cell, Literal::String(b"x".to_vec()).into())], vec![change()]), &cell, writes));
        assert!(!late_store_conflict(&store(vec![at(&other, cell.clone().into())], vec![Literal::Number(1.0).into()]), &cell, writes));
        let two = store(vec![at(&other, Literal::Number(1.0).into()), at(&cell, Literal::Number(1.0).into())],
            vec![Literal::Number(1.0).into(), Literal::Number(2.0).into()]);
        assert!(late_store_conflict(&two, &cell, writes));
        // `reads_first`: a register receiver is read after the arguments.
        let unchanged = Body { registers: &|_| true, unchanged: &|_| true };
        let receiver = method(&cell, vec![change()]);
        assert_eq!(reads_first(&receiver, &cell, true, &unchanged), Some(false));
        assert_eq!(reads_first(&receiver, &cell, false, &unchanged), Some(true));
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
        let unchanged = Body { registers: &|_| true, unchanged: &|_| true };
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
        let changed = Body { registers: &|_| true, unchanged: &|_| false };
        let sum = RValue::Binary(crate::Binary::new(other.clone().into(), param.clone().into(), crate::BinaryOperation::Add));
        assert!(block_reads_first(&[Return::new(vec![sum.clone()]).into()], &param, false, &changed));
        assert!(!block_reads_first(&[Return::new(vec![sum.clone()]).into()], &param, true, &changed));
        assert!(block_reads_first(&[Return::new(vec![sum.clone()]).into()], &param, true, &unchanged));
        // The same `x + p` with `x` the body's upvalue: GETUPVAL fetches it
        // first, so an argument moved ahead of it could change what it reads.
        let upvalue = Body { registers: &|local| *local == param, unchanged: &|_| false };
        assert!(!block_reads_first(&[Return::new(vec![sum]).into()], &param, false, &upvalue));
        // `p.key = f()`: SETTABLE reads the register `p` after the value; an
        // argument expression in place of `p` is evaluated first.
        let store_call = store(param.clone().into(), call(vec![]));
        assert!(!block_reads_first(std::slice::from_ref(&store_call), &param, true, &unchanged));
        assert!(block_reads_first(std::slice::from_ref(&store_call), &param, false, &unchanged));
        // `p:report(change())` as a statement: NAMECALL reads a register
        // receiver after the argument, an argument expression before it.
        let report = Statement::MethodCall(MethodCall { node_origin: Default::default(), value: Box::new(param.clone().into()),
            method: "report".into(), arguments: vec![call(vec![])] });
        assert!(!block_reads_first(std::slice::from_ref(&report), &param, true, &unchanged));
        assert!(block_reads_first(std::slice::from_ref(&report), &param, false, &unchanged));
        // `table.insert(p, f())` as a statement: FASTCALL2 takes it as it is.
        let path = Index::new(crate::Global(b"table".to_vec()).into(), Literal::String(b"insert".to_vec()).into()).into();
        let insert = Statement::Call(Call::new(path, vec![param.clone().into(), call(vec![])]));
        assert!(!block_reads_first(std::slice::from_ref(&insert), &param, true, &unchanged));
        // `return f(x) * p`: the call runs before `p` either way.
        let call_first = RValue::Binary(crate::Binary::new(call(vec![other.clone().into()]), param.clone().into(), crate::BinaryOperation::Mul));
        assert!(!block_reads_first(&[Return::new(vec![call_first]).into()], &param, false, &unchanged));
    }

    #[test]
    fn leading_reads_list_parameters_in_read_order_until_something_observable() {
        let (a, b, c, x) = (local("a"), local("b"), local("c"), local("x"));
        let params = [a.clone(), b.clone(), c.clone()];
        let call = |args: Vec<RValue>| -> RValue { Call::new(crate::Global(b"f".to_vec()).into(), args).into() };
        let unchanged = Body { registers: &|_| true, unchanged: &|_| true };
        let leading = |stmts: &[Statement]| LeadingReads::new(stmts, &params, |_| true, &unchanged);
        let reads = |leading: &LeadingReads| leading.values.clone();
        // `return f(a, b, c)`: all three, in order.
        let all = [Return::new(vec![call(vec![a.clone().into(), b.clone().into(), c.clone().into()])]).into()];
        assert_eq!(reads(&leading(&all)), params.to_vec());
        // `f(b, a); return c`: the call is observable, `c` comes after it.
        let swapped: [Statement; 2] = [Statement::Call(Call::new(crate::Global(b"f".to_vec()).into(), vec![b.clone().into(), a.clone().into()])),
            Return::new(vec![c.clone().into()]).into()];
        assert_eq!(reads(&leading(&swapped)), vec![b.clone(), a.clone()]);
        // `return x.k + f(a)`: the index runs first.
        let indexed = [Return::new(vec![crate::Binary::new(field(&x), call(vec![a.clone().into()]), BinaryOperation::Add).into()]).into()];
        assert!(reads(&leading(&indexed)).is_empty());
        // `return x and a or b`: only the left operand is evaluated on every path.
        let short: RValue = crate::Binary::new(crate::Binary::new(x.clone().into(), a.clone().into(), BinaryOperation::And).into(), b.clone().into(), BinaryOperation::Or).into();
        assert!(reads(&leading(&[Return::new(vec![short]).into()])).is_empty());
        // A parameter read twice never leads; the next one still may.
        let twice = [Return::new(vec![call(vec![a.clone().into(), a.clone().into(), b.clone().into()])]).into()];
        let once = LeadingReads::new(&twice, &params, |p| *p != a, &unchanged);
        assert_eq!(once.values, vec![b.clone()]);
        // Admission: the unstable arguments in parameter order, a subsequence
        // of one kind's reads.
        let listed = leading(&swapped);
        assert!(listed.admits([(&b, false)]));
        assert!(listed.admits([(&a, false)]));
        assert!(!listed.admits([(&a, false), (&b, false)]), "parameter order must be the read order");
        assert!(!listed.admits([(&c, false)]));
        let ordered = leading(&all);
        assert!(ordered.admits([(&a, false), (&c, false)]));
        assert!(!ordered.admits([(&a, false), (&b, true)]), "one kind at a time");
    }

    #[test]
    fn leading_reads_agree_with_the_single_local_question() {
        let (a, b, x) = (local("a"), local("b"), local("x"));
        let params = [a.clone(), b.clone()];
        let call = |args: Vec<RValue>| -> RValue { Call::new(crate::Global(b"f".to_vec()).into(), args).into() };
        let method = |object: &RcLocal, args: Vec<RValue>| -> RValue {
            MethodCall { node_origin: Default::default(), value: Box::new(object.clone().into()), method: "m".into(), arguments: args }.into()
        };
        let store = |base: RValue, value: RValue| -> Statement {
            Assign::new(vec![Index::new(base, Literal::String(b"key".to_vec()).into()).into()], vec![value]).into()
        };
        let bodies: Vec<Vec<Statement>> = vec![
            vec![Return::new(vec![crate::Binary::new(x.clone().into(), b.clone().into(), BinaryOperation::Mul).into()]).into()],
            vec![Return::new(vec![method(&a, vec![b.clone().into(), call(vec![])])]).into()],
            vec![store(a.clone().into(), call(vec![b.clone().into()]))],
            vec![Return::new(vec![Table::new(vec![(Some(a.clone().into()), b.clone().into())]).into()]).into()],
            vec![store(field(&x), a.clone().into()), Return::new(vec![b.clone().into()]).into()],
            vec![Return::new(vec![crate::Binary::new(b.clone().into(), a.clone().into(), BinaryOperation::LessThan).into()]).into()],
        ];
        for changed in [false, true] {
            let unchanged = |value: &RValue| !changed || !matches!(value, RValue::Local(local) if *local == x);
            let facts = Body { registers: &|_| true, unchanged: &unchanged };
            for body in &bodies {
                let leading = LeadingReads::new(body, &params, |_| true, &facts);
                for param in &params {
                    assert_eq!(leading.values.contains(param), block_reads_first(body, param, false, &facts));
                    assert_eq!(leading.registers.contains(param), block_reads_first(body, param, true, &facts));
                }
            }
        }
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
