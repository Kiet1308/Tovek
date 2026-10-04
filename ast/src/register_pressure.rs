//! The registers Luau's compiler gives a function of the output. Luau has 255
//! per function and refuses a source that needs more, so a rewrite that adds
//! registers (a loop around a block, `do` blocks flattened into one scope)
//! must check, and a finished output that still needs more is not one.
//!
//! The count follows the allocator in Luau's `Compiler.cpp` at `-O0`, the
//! level that takes the most: no constant operands (`x + 1` loads the `1`),
//! no imports, no folding, no register reuse for `local a = b`. Higher levels
//! only take fewer (`-O2` inlines only below 128 registers), so the count
//! bounds every level. Every register local is one register; the cases that
//! depend on what the compiler proves (numbered register runs in `return`,
//! assignment conflicts) are counted as if the proof failed.

use rustc_hash::FxHashSet;

use crate::{
    BinaryOperation, Block, Closure, LValue, Literal, RValue, RcLocal, Select, Statement, UnaryOperation,
    formatter::Formatter,
};

/// Luau's registers per function.
pub const REGISTER_LIMIT: usize = 255;

/// Whether some function in `block` (the chunk, or any closure in it) needs
/// more registers than Luau has.
pub fn registers_exceed_limit(block: &Block) -> bool {
    function_exceeds_limit(&block.0, 0, FxHashSet::default())
}

/// The most registers `statements` hold at once, starting with `active` held,
/// in a function whose upvalues are `upvalues`. Nested closures are separate
/// functions and are not counted.
pub fn block_registers(statements: &[Statement], active: usize, upvalues: &[RcLocal]) -> usize {
    Model::new(upvalue_ids(upvalues.iter()), None).block(statements, active)
}

/// The most registers one statement of `statements` takes above the locals
/// in scope, with the hidden registers of the loops around it (a loop's
/// variables are locals). Locals sharing storage leave room for it.
pub fn widest_statement(statements: &[Statement], upvalues: &[RcLocal]) -> usize {
    Model::new(upvalue_ids(upvalues.iter()), None).widest(statements, 0)
}

/// The ids of the locals a closure's function reads as upvalues.
fn closure_upvalue_ids(closure: &Closure) -> FxHashSet<u64> {
    upvalue_ids(closure.upvalues.iter().map(|upvalue| match upvalue {
        crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local) => local,
    }))
}

fn upvalue_ids<'a>(locals: impl Iterator<Item = &'a RcLocal>) -> FxHashSet<u64> {
    locals.map(RcLocal::stable_id).collect()
}

fn function_exceeds_limit(statements: &[Statement], parameters: usize, upvalues: FxHashSet<u64>) -> bool {
    let nested = std::cell::Cell::new(false);
    let model = Model::new(upvalues, Some(&nested));
    model.block(statements, parameters) > REGISTER_LIMIT || nested.get()
}

struct Model<'a> {
    /// Locals the function reads as upvalues: an operand among them is
    /// loaded into a register first.
    upvalues: FxHashSet<u64>,
    /// Set when a closure met on the way needs too many registers; `None`
    /// leaves closures uncounted.
    nested_exceeded: Option<&'a std::cell::Cell<bool>>,
}

impl<'a> Model<'a> {
    fn new(upvalues: FxHashSet<u64>, nested_exceeded: Option<&'a std::cell::Cell<bool>>) -> Self {
        Self { upvalues, nested_exceeded }
    }

    /// The most registers held while `statements` run, from `active`.
    fn block(&self, statements: &[Statement], mut active: usize) -> usize {
        let mut peak = active;
        for statement in statements {
            let reached = match statement {
                Statement::If(node) => (active + self.condition(&node.condition))
                    .max(self.block(&node.then_block.lock().0, active))
                    .max(self.block(&node.else_block.lock().0, active)),
                Statement::While(node) => {
                    (active + self.condition(&node.condition)).max(self.block(&node.block.lock().0, active))
                }
                // The condition sees the body's locals.
                Statement::Repeat(node) => {
                    let block = node.block.lock();
                    let declared = declared_in(&block.0);
                    self.block(&block.0, active).max(active + declared + self.condition(&node.condition))
                }
                // A counter the body assigns takes a register of its own,
                // which only counts when it decides the limit.
                Statement::NumericFor(node) => {
                    let block = node.block.lock();
                    let inner = self.block(&block.0, active + 3);
                    let written = (inner + 1).max(active + 3 + self.numeric_for_init(node, true));
                    if written <= REGISTER_LIMIT || assigns_local(&block, &node.counter) {
                        written
                    } else {
                        inner.max(active + 3 + self.numeric_for_init(node, false))
                    }
                }
                Statement::GenericFor(node) => {
                    let variables = node.res_locals.len().max(2);
                    (active + self.list(&node.right, 3)).max(self.block(&node.block.lock().0, active + 3 + variables))
                }
                statement => active + self.statement(statement),
            };
            peak = peak.max(reached);
            if let Statement::Assign(assign) = statement
                && assign.prefix
            {
                active += assign.left.len();
            }
        }
        peak
    }

    /// The most registers one statement takes above the locals in scope,
    /// with the hidden registers of enclosing loops (`held`): a numeric
    /// loop's three, a generic loop's three and room for two variables.
    fn widest(&self, statements: &[Statement], held: usize) -> usize {
        statements
            .iter()
            .map(|statement| {
                let nested = match statement {
                    Statement::If(node) => self
                        .widest(&node.then_block.lock().0, held)
                        .max(self.widest(&node.else_block.lock().0, held)),
                    Statement::While(node) => self.widest(&node.block.lock().0, held),
                    Statement::Repeat(node) => self.widest(&node.block.lock().0, held),
                    Statement::NumericFor(node) => self.widest(&node.block.lock().0, held + 3),
                    Statement::GenericFor(node) => {
                        self.widest(&node.block.lock().0, held + 3 + 2usize.saturating_sub(node.res_locals.len()))
                    }
                    _ => 0,
                };
                (held + self.statement(statement)).max(nested)
            })
            .max()
            .unwrap_or(0)
    }

    /// The registers a statement takes above the locals in scope while it
    /// evaluates; a loop's body is counted by [`Model::block`].
    fn statement(&self, statement: &Statement) -> usize {
        match statement {
            Statement::Assign(assign) if assign.prefix => self.list(&assign.right, assign.left.len()),
            Statement::Assign(assign) => self.assignment(&assign.left, &assign.right),
            Statement::Call(call) => self.call(self.fresh(&call.value), 1, &call.arguments, 0),
            Statement::MethodCall(call) => self.call(self.operand(&call.value), 2, &call.arguments, 0),
            Statement::Return(r#return) => self.consecutive(&r#return.values),
            Statement::If(node) => self.condition(&node.condition),
            Statement::While(node) => self.condition(&node.condition),
            Statement::NumericFor(node) => 3 + self.numeric_for_init(node, true),
            // The generator, state and control, from the list of values.
            Statement::GenericFor(node) => self.list(&node.right, 3),
            _ => 0,
        }
    }

    /// Above a numeric loop's three registers: the start goes into the
    /// counter (the topmost unless the body assigns it), the limit and step
    /// below it.
    fn numeric_for_init(&self, node: &crate::NumericFor, counter_written: bool) -> usize {
        usize::from(counter_written)
            + self
                .above(&node.initial, !counter_written)
                .max(self.above(&node.limit, false))
                .max(self.above(&node.step, false))
    }

    /// `count` registers filled from `values` (a declaration's locals, a
    /// generic loop's three): each value is evaluated into its own register
    /// with the whole run held, a call last filling the rest itself.
    fn list(&self, values: &[RValue], count: usize) -> usize {
        let Some((last, rest)) = values.split_last() else { return count };
        if values.len() >= count {
            let assigned = values.iter().take(count).enumerate().map(|(index, value)| self.above(value, index + 1 == count));
            let extra = values.iter().skip(count).map(|value| self.operand(value));
            return count + assigned.chain(extra).max().unwrap_or(0);
        }
        let leading = rest.iter().map(|value| count + self.above(value, false)).max().unwrap_or(0);
        let filled = count - rest.len();
        let last = match last {
            RValue::Call(call) => rest.len() + self.call(self.fresh(&call.value), 1, &call.arguments, filled),
            RValue::MethodCall(call) => rest.len() + self.call(self.operand(&call.value), 2, &call.arguments, filled),
            RValue::VarArg(_) => count,
            value => count + self.above(value, false),
        };
        leading.max(last).max(count)
    }

    /// An assignment: targets' tables and keys first, held while the values
    /// are evaluated; a local takes its value in place (or in a register of
    /// its own when another target reads it), anything else from a register.
    fn assignment(&self, left: &[LValue], right: &[RValue]) -> usize {
        if let ([target], [value]) = (left, right) {
            return match target {
                LValue::Local(local) if self.in_register(local) => self.above(value, false),
                LValue::Local(_) => self.operand(value),
                LValue::Global(global) if spellable(global) => self.operand(value),
                target => {
                    let (held, peak) = self.target(target);
                    peak.max(held + self.operand(value))
                }
            };
        }
        let (mut held, mut peak) = (0, 0);
        for target in left {
            let (target_held, target_peak) = self.target(target);
            peak = peak.max(held + target_peak);
            held += target_held;
            if matches!(target, LValue::Local(local) if self.in_register(local)) {
                held += 1;
            }
        }
        peak = peak.max(held);
        for (index, value) in right.iter().enumerate() {
            let Some(target) = left.get(index) else {
                peak = peak.max(held + self.operand(value));
                continue;
            };
            if index + 1 == right.len() && left.len() > right.len() {
                let count = left.len() - index;
                peak = peak.max(held + match value {
                    RValue::Call(call) => self.call(self.fresh(&call.value), 1, &call.arguments, count),
                    RValue::MethodCall(call) => self.call(self.operand(&call.value), 2, &call.arguments, count),
                    RValue::VarArg(_) => count,
                    value => count + self.above(value, false),
                });
            } else if matches!(target, LValue::Local(local) if self.in_register(local)) {
                peak = peak.max(held + self.above(value, false));
            } else {
                let registers = self.operand(value);
                peak = peak.max(held + registers);
                held += usize::from(registers > 0);
            }
        }
        peak
    }

    /// The registers an assignment target holds (its table and key) and the
    /// most it takes while they are evaluated.
    fn target(&self, target: &LValue) -> (usize, usize) {
        match target {
            LValue::Local(_) => (0, 0),
            LValue::Global(global) if spellable(global) => (0, 0),
            // `getfenv(1)["name"]`: the environment, then the key.
            LValue::Global(_) => (2, 2),
            LValue::Index(index) => {
                let table = self.operand(&index.left);
                let table_held = usize::from(table > 0);
                if is_field_name(&index.right) {
                    (table_held, table)
                } else {
                    let key = self.operand(&index.right);
                    (table_held + usize::from(key > 0), table.max(table_held + key))
                }
            }
        }
    }

    /// Values evaluated into consecutive registers held together: returned
    /// values, from a run as long as the list.
    fn consecutive(&self, values: &[RValue]) -> usize {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| index + self.fresh(value))
            .max()
            .unwrap_or(0)
            .max(values.len())
    }

    /// The registers a call takes from its first: the callee (`slots` = 1)
    /// or method and object (`slots` = 2) then the arguments, each evaluated
    /// on top of the ones before, and at least room for the results.
    fn call(&self, callee: usize, slots: usize, arguments: &[RValue], results: usize) -> usize {
        let arguments_peak = arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| slots + index + self.fresh(argument))
            .max()
            .unwrap_or(0);
        (slots + arguments.len()).max(results).max(callee).max(arguments_peak)
    }

    /// The registers `value` takes evaluated into a fresh register on top,
    /// that one included.
    fn fresh(&self, value: &RValue) -> usize {
        1 + self.above(value, true)
    }

    /// The registers an instruction reading `value` in place takes: none for
    /// a local in a register, else a fresh one.
    fn operand(&self, value: &RValue) -> usize {
        match value {
            RValue::Local(local) if self.in_register(local) => 0,
            value => self.fresh(value),
        }
    }

    /// The registers above the top `value` takes, evaluated into a register
    /// already held. `top`: that register is the topmost and free to
    /// clobber, so a call returns into it and a table is built in it.
    fn above(&self, value: &RValue, top: bool) -> usize {
        match value {
            RValue::Local(_) | RValue::VarArg(_) | RValue::Select(Select::VarArg(_)) => 0,
            RValue::Closure(closure) => {
                if let Some(nested) = self.nested_exceeded
                    && !nested.get()
                {
                    let function = closure.function.lock();
                    if function_exceeds_limit(&function.body.0, function.parameters.len(), closure_upvalue_ids(closure)) {
                        nested.set(true);
                    }
                }
                0
            }
            RValue::Global(global) if spellable(global) => 0,
            // `getfenv(1)["name"]`: the call, then the key.
            RValue::Global(_) => 2,
            RValue::Literal(literal) => literal_registers(literal) - 1,
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                self.call(self.fresh(&call.value), 1, &call.arguments, 1) - usize::from(top)
            }
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
                self.call(self.operand(&call.value), 2, &call.arguments, 1) - usize::from(top)
            }
            RValue::Table(table) => usize::from(!top) + self.table(table),
            RValue::Index(index) => {
                if is_field_name(&index.right) {
                    match index.left.as_ref() {
                        RValue::Local(local) if self.in_register(local) => 0,
                        table if top => self.above(table, true),
                        table => self.fresh(table),
                    }
                } else {
                    pair(self.operand(&index.left), self.operand(&index.right))
                }
            }
            RValue::Unary(unary) => self.operand(&unary.value),
            RValue::Binary(binary) => match binary.operation {
                BinaryOperation::Concat => {
                    let mut parts = vec![binary.left.as_ref()];
                    let mut rest = binary.right.as_ref();
                    while let RValue::Binary(next) = rest
                        && next.operation == BinaryOperation::Concat
                    {
                        parts.push(&next.left);
                        rest = &next.right;
                    }
                    parts.push(rest);
                    parts
                        .iter()
                        .enumerate()
                        .map(|(index, part)| index + self.fresh(part))
                        .max()
                        .unwrap_or(0)
                        .max(parts.len())
                }
                BinaryOperation::And | BinaryOperation::Or => {
                    let condition_like = matches!(&*binary.left, RValue::Binary(left)
                        if left.operation.is_comparator()
                            || matches!(left.operation, BinaryOperation::And | BinaryOperation::Or));
                    match binary.right.as_ref() {
                        RValue::Local(local) if !condition_like && self.in_register(local) => {
                            self.operand(&binary.left)
                        }
                        right => usize::from(!top) + self.condition(&binary.left).max(self.above(right, true)),
                    }
                }
                _ => pair(self.operand(&binary.left), self.operand(&binary.right)),
            },
            RValue::IfExpression(select) => self
                .condition(&select.condition)
                .max(self.above(&select.then_value, top))
                .max(self.above(&select.else_value, top)),
        }
    }

    /// The registers testing `value` takes (an `if`, a loop, an `and`): a
    /// comparison reads both operands in place, `and`/`or` test each side.
    /// It is never more than evaluating the value itself, so `not` counts its
    /// operand evaluated.
    fn condition(&self, value: &RValue) -> usize {
        match value {
            RValue::Binary(binary) if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) => {
                self.condition(&binary.left).max(self.condition(&binary.right))
            }
            RValue::Binary(binary) if binary.operation.is_comparator() => {
                pair(self.operand(&binary.left), self.operand(&binary.right))
            }
            RValue::Unary(unary) if unary.operation == UnaryOperation::Not => self.operand(&unary.value),
            value => self.operand(value),
        }
    }

    /// A constructor above its register: list items wait in a run of up to
    /// 16 registers (each evaluated on top of the ones before it), a keyed
    /// entry's key and value come above the whole run.
    fn table(&self, table: &crate::Table) -> usize {
        let items = table.0.iter().filter(|(key, _)| key.is_none()).count();
        let run = items.min(16);
        let mut peak = run;
        let mut item = 0;
        for (key, value) in &table.0 {
            match key {
                None => {
                    peak = peak.max(item % 16 + self.fresh(value));
                    item += 1;
                }
                Some(key) => peak = peak.max(run + pair(self.operand(key), self.operand(value))),
            }
        }
        peak
    }

    fn in_register(&self, local: &RcLocal) -> bool {
        !self.upvalues.contains(&local.stable_id())
    }
}

/// Two operands evaluated in turn: the first one's register stays held.
fn pair(first: usize, second: usize) -> usize {
    if first == 0 { second } else { first.max(1 + second) }
}

/// The registers a literal takes as printed: `-1` negates a loaded `1`,
/// `(0 / 0)` divides, `vector.create(...)` calls.
fn literal_registers(literal: &Literal) -> usize {
    fn number(value: f64) -> usize {
        if value.is_nan() {
            // `(0 / 0)`, or `-(0 / 0)` negating it.
            if value.is_sign_negative() { 3 } else { 4 }
        } else {
            1 + usize::from(value.is_sign_negative())
        }
    }
    let components = |x: f64, y: f64, z: f64| {
        // `vector.create`, then the components on top of one another.
        [x, y, z].iter().enumerate().map(|(index, &component)| 1 + index + number(component)).max().unwrap().max(4)
    };
    match *literal {
        Literal::Number(value) => number(value),
        Literal::Vector(x, y, z) => components(x.into(), y.into(), z.into()),
        Literal::VectorD(x, y, z) => components(x, y, z),
        _ => 1 + usize::from(literal.prints_negated()),
    }
}

/// Whether an index prints as `.name`, which takes no key register.
fn is_field_name(key: &RValue) -> bool {
    matches!(key, RValue::Literal(Literal::String(name)) if Formatter::<std::fmt::Formatter>::is_valid_name(name))
}

fn spellable(global: &crate::Global) -> bool {
    Formatter::<std::fmt::Formatter>::is_valid_name(&global.0)
}

/// The locals a block's own statements declare.
fn declared_in(statements: &[Statement]) -> usize {
    statements
        .iter()
        .map(|statement| match statement {
            Statement::Assign(assign) if assign.prefix => assign.left.len(),
            _ => 0,
        })
        .sum()
}

/// Whether anything in `block`, closures included, assigns `local`.
fn assigns_local(block: &Block, local: &RcLocal) -> bool {
    block.any_statement_deep(&mut |statement| {
        matches!(statement, Statement::Assign(assign)
            if !assign.prefix && assign.left.iter().any(|left| matches!(left, LValue::Local(target) if target == local)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Binary, Call, GenericFor, Global, Index, Local};

    fn global(name: &str) -> RValue {
        Global::from(name).into()
    }

    fn field(table: RValue, name: &str) -> RValue {
        Index::new(table, Literal::String(name.as_bytes().to_vec()).into()).into()
    }

    fn declare(value: RValue) -> Statement {
        let mut assign = Assign::new(vec![RcLocal::new(Local::default()).into()], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    /// Each operand Luau does not read in place takes a register of its own,
    /// the result another: `local a = g.x + g.y` holds three.
    #[test]
    fn operands_take_registers_of_their_own() {
        let sum = Binary::new(field(global("g"), "x"), field(global("g"), "y"), BinaryOperation::Add);
        assert_eq!(block_registers(&[declare(sum.into())], 0, &[]), 3);
        // `x + 1` loads the `1` at -O0.
        let local = RcLocal::new(Local::default());
        let increment = Binary::new(local.clone().into(), Literal::Number(1.0).into(), BinaryOperation::Add);
        assert_eq!(block_registers(&[declare(increment.into())], 1, &[]), 3);
        // An upvalue is fetched first.
        let doubled = Binary::new(local.clone().into(), local.clone().into(), BinaryOperation::Add);
        assert_eq!(block_registers(&[declare(doubled.clone().into())], 0, &[]), 1);
        assert_eq!(block_registers(&[declare(doubled.into())], 0, &[local]), 3);
    }

    /// A generic loop holds its generator, state and control, and room for
    /// at least two variables, however few it names.
    #[test]
    fn generic_loops_hold_room_for_two_variables() {
        let mut body = Block::default();
        for _ in 0..6 {
            let variable = RcLocal::new(Local::default());
            body = Block(vec![GenericFor::new(vec![variable], vec![global("items")], body).into()]);
        }
        assert_eq!(block_registers(&body.0, 0, &[]), 30);
    }

    /// A call takes its callee and arguments in consecutive registers, each
    /// argument evaluated on top of the ones before.
    #[test]
    fn calls_take_consecutive_registers() {
        let arguments = (0..54).map(|_| RcLocal::new(Local::default()).into()).collect();
        let call: Statement = Call::new(global("f"), arguments).into();
        assert_eq!(block_registers(std::slice::from_ref(&call), 0, &[]), 55);
        let nested = Call::new(global("f"), vec![global("a"), field(global("t"), "x"), Call::new(global("g"), vec![global("b")]).into()]);
        // `f`, `a`, `t.x`, then `g(b)` from the fourth register: `g`, `b`.
        assert_eq!(block_registers(&[nested.into()], 0, &[]), 5);
    }
}
