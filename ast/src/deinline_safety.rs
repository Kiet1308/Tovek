//! Argument snapshots for call reconstruction. Local spelling and type hints
//! cannot prove that a cell stays unchanged during a reconstructed helper body.
use rustc_hash::FxHashSet;
use crate::{Block, LValue, Literal, RValue, Statement, Traverse, Upvalue};

#[derive(Default)]
pub(crate) struct CaptureSafety {
    references: FxHashSet<u64>,
    /// Locals assigned after their declaration, anywhere in the module.
    rebound: FxHashSet<u64>,
    captured: FxHashSet<u64>,
    /// Globals the module assigns somewhere.
    written_globals: FxHashSet<Vec<u8>>,
    /// The module reads `getfenv` or `setfenv`.
    dynamic_environment: bool,
    visited: FxHashSet<usize>,
    nodes: usize,
    literal_bytes: usize,
    exhausted: bool,
}

impl CaptureSafety {
    pub(crate) fn new(body: &Block) -> Self {
        let mut result = Self::default();
        result.block(&body.0, 0);
        result.visited.clear();
        result
    }

    pub(crate) fn uncaptured(&self, local: &crate::RcLocal) -> bool {
        !self.exhausted && !self.captured.contains(&local.stable_id())
    }

    pub(crate) fn complete(&self) -> bool { !self.exhausted }

    /// A module that reads `getfenv`/`setfenv` can give a function its own
    /// globals, so the same code in a helper and in its caller may mean
    /// different things. Luau neither inlines nor imports in such a module.
    pub(crate) fn dynamic_environment(&self) -> bool { self.exhausted || self.dynamic_environment }

    /// A global path (`math.clamp`, `workspace`) Luau resolves once, when the
    /// script loads: up to three names, the first never assigned in the
    /// module, in a module with a fixed environment. No code the script runs
    /// later can change what it reads.
    pub(crate) fn constant_import(&self, value: &RValue) -> bool {
        fn root(value: &RValue, depth: usize) -> Option<&[u8]> {
            match value {
                RValue::Global(global) => Some(&global.0),
                RValue::Index(index) if depth < 2 && matches!(*index.right, RValue::Literal(Literal::String(_))) => {
                    root(&index.left, depth + 1)
                }
                _ => None,
            }
        }
        !self.dynamic_environment() && root(value, 0).is_some_and(|name| !self.written_globals.contains(name))
    }
    pub(crate) fn nodes(&self) -> usize { self.nodes }

    pub(crate) fn stable(&self, value: &RValue) -> bool {
        match value {
            RValue::Literal(crate::Literal::Nil | crate::Literal::Boolean(_) | crate::Literal::String(_)) => true,
            RValue::Literal(crate::Literal::Number(n)) => n.is_finite()
                && n.abs().to_bits() != std::f64::consts::PI.to_bits(),
            // A cell shared by reference changes only through an assignment
            // after its declaration. One a nested closure re-captures from
            // its parent (`LCT_UPVAL`) is a reference to a cell that, written
            // once, never changes: each activation and loop iteration
            // declares a fresh one.
            RValue::Local(local) => {
                !self.exhausted
                    && !(self.references.contains(&local.stable_id()) && self.rebound.contains(&local.stable_id()))
            }
            _ => false,
        }
    }

    fn spend(&mut self, depth: usize) -> bool {
        self.nodes += 1;
        if depth >= 128 || self.nodes > 200_000 { self.exhausted = true; }
        !self.exhausted
    }

    fn block(&mut self, statements: &[Statement], depth: usize) {
        for statement in statements {
            if !self.spend(depth) { return; }
            // Check wide containers before Traverse allocates their root list.
            let width = match statement {
                Statement::Assign(s) => s.left.len().saturating_mul(2).saturating_add(s.right.len()),
                Statement::Return(s) => s.values.len(),
                Statement::Call(s) => s.arguments.len(),
                Statement::MethodCall(s) => s.arguments.len(),
                Statement::SetList(s) => s.values.len(),
                Statement::GenericFor(s) => s.res_locals.len().saturating_add(s.right.len()),
                _ => 0,
            };
            if width > 200_000usize.saturating_sub(self.nodes) { self.exhausted = true; return; }
            if let Statement::Assign(assign) = statement {
                for left in &assign.left {
                    match left {
                        LValue::Global(global) => {
                            self.written_globals.insert(global.0.clone());
                        }
                        LValue::Local(local) if !assign.prefix => {
                            self.rebound.insert(local.stable_id());
                        }
                        _ => {}
                    }
                }
            }
            match statement {
                Statement::If(s) => {
                    self.block(&s.then_block.lock().0, depth + 1);
                    self.block(&s.else_block.lock().0, depth + 1);
                }
                Statement::While(s) => self.block(&s.block.lock().0, depth + 1),
                Statement::Repeat(s) => self.block(&s.block.lock().0, depth + 1),
                Statement::NumericFor(s) => self.block(&s.block.lock().0, depth + 1),
                Statement::GenericFor(s) => self.block(&s.block.lock().0, depth + 1),
                _ => {}
            }
            if self.exhausted { return; }
            crate::deinline::visit_stmt_rvalues(statement, &mut |value| {
                self.value(value, depth + 1);
                !self.exhausted
            });
            if self.exhausted { return; }
        }
    }

    fn value(&mut self, value: &RValue, depth: usize) {
        if !self.spend(depth) { return; }
        let width = match value {
            RValue::Table(s) => s.0.len().saturating_mul(2),
            RValue::Call(s) | RValue::Select(crate::Select::Call(s)) => s.arguments.len(),
            RValue::MethodCall(s) | RValue::Select(crate::Select::MethodCall(s)) => s.arguments.len(),
            RValue::Closure(s) => s.upvalues.len(),
            _ => 0,
        };
        if width > 200_000usize.saturating_sub(self.nodes) { self.exhausted = true; return; }
        if let RValue::Global(global) = value
            && matches!(global.0.as_slice(), b"getfenv" | b"setfenv")
        {
            self.dynamic_environment = true;
        }
        if let RValue::Literal(crate::Literal::String(bytes)) = value {
            self.literal_bytes = self.literal_bytes.saturating_add(bytes.len());
            if self.literal_bytes > 8 * 1024 * 1024 { self.exhausted = true; return; }
        }
        if let RValue::Closure(closure) = value {
            for capture in &closure.upvalues {
                if !self.spend(depth) { return; }
                let (Upvalue::Ref(local) | Upvalue::Copy(local)) = capture;
                self.captured.insert(local.stable_id());
                if let Upvalue::Ref(local) = capture { self.references.insert(local.stable_id()); }
            }
            let identity = triomphe::Arc::as_ptr(&closure.function.0) as usize;
            if self.visited.insert(identity) {
                self.block(&closure.function.0.lock().body.0, depth + 1);
            }
        } else {
            value.visit_rvalues(&mut |child| {
                self.value(child, depth + 1);
                !self.exhausted
            });
        }
    }
}

/// Deterministic work fuel bounds candidate comparisons independently of host
/// speed/thread scheduling. Never commit a match if its ambiguity scan runs out.
pub(crate) struct SearchBudget {
    remaining: std::cell::Cell<usize>,
    exhausted: std::cell::Cell<bool>,
}
impl Default for SearchBudget {
    fn default() -> Self {
        Self { remaining: std::cell::Cell::new(20_000_000), exhausted: std::cell::Cell::new(false) }
    }
}
impl SearchBudget {
    pub(crate) fn spend(&self, nodes: usize) -> bool {
        if self.exhausted.get() { return false; }
        let Some(left) = self.remaining.get().checked_sub(nodes.max(1)) else {
            self.exhausted.set(true);
            crate::telemetry::count("deinline_search_budget_refused", 1);
            return false;
        };
        self.remaining.set(left);
        true
    }
    pub(crate) fn exhausted(&self) -> bool { self.exhausted.get() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budgets_fail_closed_and_literals_do_not_hide_environment_lookups() {
        let safety = CaptureSafety::new(&Block::default());
        for n in [f64::INFINITY, f64::NAN, std::f64::consts::PI, -std::f64::consts::PI] {
            assert!(!safety.stable(&crate::Literal::Number(n).into()));
        }
        for n in [0.0, -0.0, 2.0] { assert!(safety.stable(&crate::Literal::Number(n).into())); }
        let budget = SearchBudget::default();
        assert!(budget.spend(20_000_000));
        assert!(!budget.exhausted());
        assert!(!budget.spend(1));
        assert!(budget.exhausted());
        assert!(!budget.spend(0));
        let huge = Block(vec![crate::Return::new(vec![crate::Literal::String(vec![0; 8 * 1024 * 1024 + 1]).into()]).into()]);
        assert!(!CaptureSafety::new(&huge).complete());
    }

    /// A reference capture changes a cell only if something assigns it after
    /// its declaration; a nested closure's re-capture of a parent upvalue is
    /// such a reference to a cell written once.
    #[test]
    fn a_reference_capture_of_a_once_written_cell_is_stable() {
        let (once, rebound) = (crate::RcLocal::default(), crate::RcLocal::default());
        let reader = triomphe::Arc::new(parking_lot::Mutex::new(crate::Function::default()));
        let capture = crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(reader),
            upvalues: vec![Upvalue::Ref(once.clone()), Upvalue::Ref(rebound.clone())],
        };
        let mut declare_once = crate::Assign::new(vec![once.clone().into()], vec![crate::Literal::Number(1.0).into()]);
        declare_once.prefix = true;
        let mut declare_rebound = crate::Assign::new(vec![rebound.clone().into()], vec![crate::Literal::Number(1.0).into()]);
        declare_rebound.prefix = true;
        let block = Block(vec![
            declare_once.into(),
            declare_rebound.into(),
            crate::Call::new(RValue::Global(crate::Global::from("keep")), vec![capture.into()]).into(),
            crate::Assign::new(vec![rebound.clone().into()], vec![crate::Literal::Number(2.0).into()]).into(),
        ]);
        let safety = CaptureSafety::new(&block);
        assert!(safety.stable(&RValue::Local(once)));
        assert!(!safety.stable(&RValue::Local(rebound)));
    }
}
