//! Argument snapshots for call reconstruction. Local spelling and type hints
//! cannot prove that a cell stays unchanged during a reconstructed helper body.
use rustc_hash::FxHashSet;
use crate::{Block, RValue, Statement, Traverse, Upvalue};

#[derive(Default)]
pub(crate) struct CaptureSafety {
    references: FxHashSet<u64>,
    captured: FxHashSet<u64>,
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
    pub(crate) fn nodes(&self) -> usize { self.nodes }

    pub(crate) fn stable(&self, value: &RValue) -> bool {
        match value {
            RValue::Literal(crate::Literal::Nil | crate::Literal::Boolean(_) | crate::Literal::String(_)) => true,
            RValue::Literal(crate::Literal::Number(n)) => n.is_finite()
                && n.abs().to_bits() != std::f64::consts::PI.to_bits(),
            RValue::Local(local) => !self.exhausted && !self.references.contains(&local.stable_id()),
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
            for value in crate::deinline::stmt_rvalues(statement) {
                self.value(value, depth + 1);
                if self.exhausted { return; }
            }
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
            for child in value.rvalues() {
                self.value(child, depth + 1);
                if self.exhausted { break; }
            }
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
}
