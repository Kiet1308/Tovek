//! Argument snapshots for call reconstruction. Local spelling and type hints
//! cannot prove that a cell stays unchanged during a reconstructed helper body.
use rustc_hash::{FxHashMap, FxHashSet};
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
    /// The locals each function reads as upvalues, by its identity.
    upvalues: FxHashMap<usize, FxHashSet<u64>>,
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

    /// A global path no code the script runs can change: a builtin global
    /// (`print`, `workspace`) or a field of a library whose members are fixed
    /// (`math.clamp`, `Enum.KeyCode.E`), up to three names, never assigned in
    /// the module, in a module with a fixed environment. Luau's own builtin
    /// calls assume the same. Any other path is a read of a table someone may
    /// write (`Shared.x`, `Foo.new`, `script.Parent`), or of a global another
    /// chunk may set.
    pub(crate) fn constant_import(&self, value: &RValue) -> bool {
        !self.dynamic_environment()
            && import_root(value).is_some_and(|(name, _)| !self.written_globals.contains(name))
            && library_import(value)
    }
    pub(crate) fn nodes(&self) -> usize { self.nodes }

    /// Whether `local`, read in `function` (`None`: the chunk), is one of its
    /// registers, which Luau reads where an operation uses it; an upvalue is
    /// fetched (GETUPVAL) before.
    pub(crate) fn register_of(&self, local: &crate::RcLocal, function: Option<usize>) -> bool {
        !self.exhausted
            && function.is_none_or(|function| {
                self.upvalues.get(&function).is_some_and(|upvalues| !upvalues.contains(&local.stable_id()))
            })
    }

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

    /// A read the code a call runs cannot change: a literal, a local [`stable`]
    /// here, or a [`constant_import`].
    pub(crate) fn unchanged_by_calls(&self, value: &RValue) -> bool {
        matches!(value, RValue::Literal(_)) || self.stable(value) || self.constant_import(value)
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
                        // `math.foo = f` replaces a library member.
                        LValue::Index(index) => {
                            let mut base = &*index.left;
                            while let RValue::Index(inner) = base {
                                base = &inner.left;
                            }
                            if let RValue::Global(global) = base {
                                self.written_globals.insert(global.0.clone());
                            }
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
            let identity = triomphe::Arc::as_ptr(&closure.function.0) as usize;
            let upvalues = self.upvalues.entry(identity).or_default();
            upvalues.extend(closure.upvalues.iter().map(|capture| {
                let (Upvalue::Ref(local) | Upvalue::Copy(local)) = capture;
                local.stable_id()
            }));
            for capture in &closure.upvalues {
                if !self.spend(depth) { return; }
                let (Upvalue::Ref(local) | Upvalue::Copy(local)) = capture;
                self.captured.insert(local.stable_id());
                if let Upvalue::Ref(local) = capture { self.references.insert(local.stable_id()); }
            }
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

/// The root global of an import path (up to three names) and its field count.
fn import_root(value: &RValue) -> Option<(&[u8], usize)> {
    fn root(value: &RValue, depth: usize) -> Option<(&[u8], usize)> {
        match value {
            RValue::Global(global) => Some((&global.0, depth)),
            RValue::Index(index) if depth < 2 && matches!(*index.right, RValue::Literal(Literal::String(_))) => {
                root(&index.left, depth + 1)
            }
            _ => None,
        }
    }
    root(value, 0)
}

/// A builtin global or a member of a fixed library (`print`, `math.max`,
/// `Enum.KeyCode.E`): fetching it runs no code and, unless the script
/// replaces the library, always gives the same value.
pub fn library_import(value: &RValue) -> bool {
    import_root(value).is_some_and(|(name, fields)| if fields == 0 { builtin_global(name) } else { fixed_library(name) })
}

/// Globals Luau and Roblox provide, whose binding a script does not change.
fn builtin_global(name: &[u8]) -> bool {
    fixed_library(name)
        || matches!(
            name,
            b"assert" | b"error" | b"getmetatable" | b"setmetatable" | b"ipairs" | b"pairs" | b"next"
                | b"pcall" | b"xpcall" | b"print" | b"rawequal" | b"rawget" | b"rawset" | b"rawlen"
                | b"select" | b"tonumber" | b"tostring" | b"type" | b"typeof" | b"unpack" | b"require"
                | b"newproxy" | b"gcinfo" | b"game" | b"workspace" | b"Workspace" | b"script" | b"plugin"
                | b"shared" | b"_G" | b"tick" | b"time" | b"wait" | b"spawn" | b"delay" | b"warn"
                | b"elapsedTime" | b"settings" | b"UserSettings"
        )
}

/// Library tables whose members are fixed functions and constants.
fn fixed_library(name: &[u8]) -> bool {
    matches!(
        name,
        b"math" | b"string" | b"table" | b"bit32" | b"utf8" | b"os" | b"coroutine" | b"buffer" | b"vector"
            | b"debug" | b"task" | b"Enum" | b"Instance" | b"Vector3" | b"Vector2" | b"Vector3int16"
            | b"Vector2int16" | b"CFrame" | b"Color3" | b"ColorSequence" | b"ColorSequenceKeypoint"
            | b"NumberSequence" | b"NumberSequenceKeypoint" | b"NumberRange" | b"UDim" | b"UDim2" | b"Rect"
            | b"Ray" | b"Region3" | b"Region3int16" | b"BrickColor" | b"TweenInfo" | b"Random" | b"DateTime"
            | b"PhysicalProperties" | b"Font" | b"Faces" | b"Axes" | b"PathWaypoint" | b"RaycastParams"
            | b"OverlapParams" | b"SharedTable" | b"Content"
    )
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
