//! Argument snapshots for call reconstruction. Local spelling and type hints
//! cannot prove that a cell stays unchanged during a reconstructed helper body.
use rustc_hash::{FxHashMap, FxHashSet};
use crate::{Block, LValue, Literal, RValue, Statement, Traverse, Upvalue};

#[derive(Default)]
pub(crate) struct CaptureSafety {
    references: FxHashSet<u64>,
    /// Locals assigned after their declaration, anywhere in the module.
    rebound: FxHashSet<u64>,
    /// Locals a function assigns as its upvalue: only those can change
    /// while their declaring function runs a call.
    closure_written: FxHashSet<u64>,
    captured: FxHashSet<u64>,
    /// The module names `getfenv` or `setfenv` (`_G.setfenv` too).
    dynamic_environment: bool,
    /// How the module reaches `debug.info`, which reads call frames.
    frames: FrameReads,
    /// The locals each function reads as upvalues, by its identity.
    upvalues: FxHashMap<usize, FxHashSet<u64>>,
    /// Shared function bodies whose closure occurrences disagree on incoming locals.
    ambiguous_upvalues: FxHashSet<usize>,
    /// Value captures read a snapshot, not a later value of the same SSA-coalesced local.
    copied_upvalues: FxHashMap<usize, FxHashSet<u64>>,
    /// Who may run a function that assigns a cell (see [`Self::private_writers`]).
    calls: CallGraph,
    visited: FxHashSet<usize>,
    nodes: usize,
    literal_bytes: usize,
    /// Read timing ignores literal contents and walks expression children iteratively.
    read_timing: bool,
    exhausted: bool,
}

/// Reads of the `debug` library, against those that only call `debug.info`
/// or fetch another member by name: any other use lets `debug.info` run
/// under another name.
#[derive(Default)]
struct FrameReads {
    library: u32,
    members: u32,
    info: u32,
    info_calls: u32,
    computed: bool,
    /// Some `debug.info` call reads which frames run ([`reads_frame_identity`]).
    read: bool,
    /// Some `debug.info` call may hand out a function (`f`), which can then
    /// run without its name.
    functions: bool,
    /// The functions making such a call that one more frame below them may
    /// change ([`reads_beyond_caller`]), by identity.
    callers: FxHashSet<usize>,
    /// The locals naming every function that reads call frames, itself or
    /// through another such call; `None` when one also runs otherwise.
    observers: Option<FxHashSet<u64>>,
}

/// How the module's functions reach each other, by function identity: what
/// tells whether code other than a named call may run a function.
#[derive(Default)]
struct CallGraph {
    /// Reads of each local, and those calling it by name (`f(...)`).
    reads: FxHashMap<u64, u32>,
    callee_reads: FxHashMap<u64, u32>,
    /// Each function's closure expressions, and the one local declarations
    /// bind them to (`local function f`), when every one of them does.
    closures: FxHashMap<usize, u32>,
    bound: FxHashMap<usize, (u32, Option<u64>)>,
    /// The functions assigning each cell, and those calling each local.
    writers: FxHashMap<u64, FxHashSet<usize>>,
    callers: FxHashMap<u64, FxHashSet<usize>>,
    /// The function declaring each local (`None`: the chunk). A call it
    /// makes runs on its own activation's cells, never on another's.
    declared_in: FxHashMap<u64, Option<usize>>,
    private_writers: std::cell::RefCell<FxHashMap<u64, Option<std::rc::Rc<FxHashSet<u64>>>>>,
}

/// How much a census traversal records: [`SHAPE`] only its size, depth and
/// literal bounds; [`CAPTURES`] also captured locals and upvalues; [`FULL`]
/// everything [`CaptureSafety::new`] answers.
const SHAPE: u8 = 0;
const CAPTURES: u8 = 1;
const FULL: u8 = 2;

impl CaptureSafety {
    pub(crate) fn new(body: &Block) -> Self {
        let mut result = Self::default();
        result.block::<FULL, 200_000>(&body.0, 0, &FxHashSet::default(), None);
        result.visited.clear();
        result.frames.observers = result.frame_observers();
        result
    }

    /// The captures alone: which locals closures capture, and each function's
    /// upvalues. Only [`Self::uncaptured`], [`Self::register_of`] and [`Self::upvalue_of`] are
    /// answered; the traversal and its budget are the full census's, so
    /// [`Self::complete`] agrees with [`Self::new`] on the same body.
    pub(crate) fn captures(body: &Block) -> Self {
        let mut result = Self::default();
        result.block::<CAPTURES, 200_000>(&body.0, 0, &FxHashSet::default(), None);
        result.visited.clear();
        result
    }

    /// The capture identities needed by final read timing, without helper
    /// literal/expression bounds; node and block/closure depth limits still apply.
    pub(crate) fn captures_for_read_timing(body: &Block) -> Self {
        let mut result = Self { read_timing: true, ..Default::default() };
        result.block::<CAPTURES, 200_000>(&body.0, 0, &FxHashSet::default(), None);
        result.visited.clear();
        result
    }

    /// Capture identities and writes with the same bounds as read timing.
    pub(crate) fn writes_for_read_timing(body: &Block) -> Self {
        let mut result = Self { read_timing: true, ..Default::default() };
        result.block::<FULL, 200_000>(&body.0, 0, &FxHashSet::default(), None);
        result.visited.clear();
        result
    }

    /// Helper admission only needs the census's size/depth/literal bounds.
    /// Reuse the exact traversal without building capture or call-graph facts,
    /// and stop as soon as the helper's smaller node limit is exceeded.
    pub(crate) fn helper_within_shape_budget(body: &Block) -> bool {
        let mut result = Self::default();
        result.block::<SHAPE, 2048>(&body.0, 0, &FxHashSet::default(), None);
        !result.exhausted
    }

    /// [`FrameReads::observers`]: from each function calling `debug.info`,
    /// to the functions calling it by name, as long as each is only ever
    /// called by its one name (see [`Self::private_writers`]).
    fn frame_observers(&self) -> Option<FxHashSet<u64>> {
        let calls = &self.calls;
        let mut pending: Vec<usize> = self.frames.callers.iter().copied().collect();
        let mut seen = FxHashSet::default();
        let mut names = FxHashSet::default();
        while let Some(function) = pending.pop() {
            if !seen.insert(function) {
                continue;
            }
            let &(declarations, name) = calls.bound.get(&function)?;
            let name = name?;
            let called_only = calls.reads.get(&name).copied().unwrap_or(0)
                == calls.callee_reads.get(&name).copied().unwrap_or(0);
            if declarations != calls.closures.get(&function).copied().unwrap_or(0)
                || self.rebound.contains(&name)
                || !called_only
            {
                return None;
            }
            names.insert(name);
            pending.extend(calls.callers.get(&name).into_iter().flatten().copied());
        }
        Some(names)
    }

    /// The locals naming every function that may assign `cell`, directly or
    /// by calling one that does, when each such function is only ever called
    /// by that name: then nothing but a call `f(...)` of one of them changes
    /// `cell` while an expression runs. `None` when one may run otherwise
    /// (passed as a value, stored, returned, rebound), so any call or
    /// metamethod might.
    fn private_writers(&self, cell: u64) -> Option<std::rc::Rc<FxHashSet<u64>>> {
        // `debug.info(1, "f")` hands out the running function itself.
        if self.exhausted || self.frames.functions || self.call_frames_untracked() {
            return None;
        }
        let calls = &self.calls;
        if let Some(known) = calls.private_writers.borrow().get(&cell) {
            return known.clone();
        }
        let home = calls.declared_in.get(&cell).copied().flatten();
        let result = (|| {
            let mut functions: FxHashSet<usize> = calls.writers.get(&cell).cloned().unwrap_or_default();
            let mut pending: Vec<usize> = functions.iter().copied().collect();
            let mut names = FxHashSet::default();
            while let Some(function) = pending.pop() {
                let &(declarations, name) = calls.bound.get(&function)?;
                let name = name?;
                let called_only = calls.reads.get(&name).copied().unwrap_or(0)
                    == calls.callee_reads.get(&name).copied().unwrap_or(0);
                if declarations != calls.closures.get(&function).copied().unwrap_or(0)
                    || self.rebound.contains(&name)
                    || !called_only
                {
                    return None;
                }
                if names.insert(name) {
                    for &caller in calls.callers.get(&name).into_iter().flatten() {
                        if Some(caller) != home && functions.insert(caller) {
                            pending.push(caller);
                        }
                    }
                }
            }
            Some(std::rc::Rc::new(names))
        })();
        calls.private_writers.borrow_mut().insert(cell, result.clone());
        result
    }

    /// Whether evaluating a value may change `local`: a local no function
    /// assigns as its upvalue never does; one only named functions assign
    /// does by a call of one of them ([`Self::private_writers`]); any other
    /// by any call or metamethod ([`crate::effects::may_write_capture`]).
    pub(crate) fn may_change(&self, local: &crate::RcLocal) -> impl Fn(&RValue) -> bool + '_ {
        let writers = if self.closure_written(local) {
            self.private_writers(local.stable_id())
        } else {
            Some(Default::default())
        };
        move |value: &RValue| match &writers {
            Some(names) => !names.is_empty() && calls_any(value, names),
            None => crate::effects::may_write_capture(value),
        }
    }

    /// Whether no code assigns `local` after its declaration: every read of
    /// it, by value or through a cell, sees the value it was declared with.
    pub(crate) fn never_reassigned(&self, local: &crate::RcLocal) -> bool {
        !self.exhausted && !self.rebound.contains(&local.stable_id())
    }

    /// The helper's by-value captures still agree with reads at a call site.
    /// shortcut: reject a rebound copy even before its write; refine only with
    /// a proof covering every path from closure creation to the matched site.
    pub(crate) fn copies_unchanged(&self, function: usize) -> bool {
        !self.exhausted && self.copied_upvalues.get(&function)
            .is_none_or(|locals| locals.is_disjoint(&self.rebound))
    }

    pub(crate) fn uncaptured(&self, local: &crate::RcLocal) -> bool {
        !self.exhausted && !self.captured.contains(&local.stable_id())
    }

    pub(crate) fn complete(&self) -> bool { !self.exhausted }

    /// The full census saw each function body once. Required before using
    /// ordinary tree walkers that do not track repeated closure identities.
    pub(crate) fn unique_function_bodies(&self) -> bool {
        !self.exhausted && self.calls.closures.values().all(|&occurrences| occurrences == 1)
    }

    /// A census that knows nothing, as one past its budget.
    #[cfg(test)]
    pub(crate) fn exhausted_for_tests() -> Self {
        Self { exhausted: true, ..Self::default() }
    }

    /// The module calls `debug.info`, whose answer depends on the call
    /// frames running: the same code in a helper sees another frame.
    pub(crate) fn reads_call_frames(&self) -> bool {
        self.exhausted || self.frames.read || self.call_frames_untracked()
    }

    /// `debug.info` (or the whole library, or a function calling it) may run
    /// under another name, so no code can be shown free of it.
    pub(crate) fn call_frames_untracked(&self) -> bool {
        let frames = &self.frames;
        self.exhausted
            || frames.computed
            || frames.library > frames.members
            || frames.info > frames.info_calls
            || frames.observers.is_none()
    }

    /// Whether running `statements` reads call frames: a `debug.info` call
    /// in them, or a call of a local function that makes one, itself or
    /// through others. Moved into a helper, that code runs a frame deeper. A
    /// closure's body runs in a frame of its own wherever it is created.
    ///
    /// `error(message, level)` reads no frame that matters: its level only
    /// picks the position prefixed to a string message, and a position is
    /// not part of what a decompile keeps (no output line keeps its number
    /// either). A helper raising with level 2 is rebuilt like any other.
    pub(crate) fn reads_frames(&self, statements: &[Statement]) -> bool {
        statements.iter().any(|statement| {
            crate::deinline::stmt_rvalues(statement).into_iter().any(|value| self.value_reads_frames(value))
                || match statement {
                    Statement::If(node) => {
                        self.reads_frames(&node.then_block.lock().0) || self.reads_frames(&node.else_block.lock().0)
                    }
                    Statement::While(node) => self.reads_frames(&node.block.lock().0),
                    Statement::Repeat(node) => self.reads_frames(&node.block.lock().0),
                    Statement::NumericFor(node) => self.reads_frames(&node.block.lock().0),
                    Statement::GenericFor(node) => self.reads_frames(&node.block.lock().0),
                    _ => false,
                }
        })
    }

    /// [`Self::reads_frames`] of one value.
    pub(crate) fn value_reads_frames(&self, value: &RValue) -> bool {
        calls_debug_info(value)
            || self.frames.observers.as_ref().is_some_and(|names| !names.is_empty() && calls_any(value, names))
    }

    /// Whether some function assigns `local` as its upvalue: then a call
    /// made while its declaring function runs may change it.
    pub(crate) fn closure_written(&self, local: &crate::RcLocal) -> bool {
        self.exhausted || self.closure_written.contains(&local.stable_id())
    }

    /// A module that reads `getfenv`/`setfenv` can give a function its own
    /// globals, so the same code in a helper and in its caller may mean
    /// different things. Luau neither inlines nor imports in such a module.
    pub(crate) fn dynamic_environment(&self) -> bool { self.exhausted || self.dynamic_environment }

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

    /// A known incoming local with the same capture identities at every closure
    /// occurrence; union membership alone cannot prove an eager GETUPVAL read.
    pub(crate) fn upvalue_of(&self, local: &crate::RcLocal, function: Option<usize>) -> bool {
        !self.exhausted && function.is_some_and(|function| {
            !self.ambiguous_upvalues.contains(&function)
                && self.upvalues.get(&function).is_some_and(|upvalues| upvalues.contains(&local.stable_id()))
        })
    }

    pub(crate) fn stable(&self, value: &RValue) -> bool {
        match value {
            RValue::Literal(crate::Literal::Nil | crate::Literal::Boolean(_) | crate::Literal::String(_)) => true,
            RValue::Literal(crate::Literal::Number(n)) => n.is_finite()
                && n.abs().to_bits() != std::f64::consts::PI.to_bits(),
            // Bytecode vector constants are values, not constructor lookups.
            RValue::Literal(crate::Literal::Vector(x, y, z)) => [x, y, z].iter().all(|n| n.is_finite()),
            RValue::Literal(crate::Literal::VectorD(x, y, z)) => [x, y, z].iter().all(|n| n.is_finite()),
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

    /// Whether reading `value` in `function` (`None`: the chunk) gives the
    /// same value across any call: it is [`stable`], or a register of that
    /// function no closure assigns. A call does not run the activation it
    /// is made from, the only code writing such a local; read as an upvalue
    /// it may change, its declaring activation resumed as a coroutine.
    pub(crate) fn stable_at(&self, value: &RValue, function: Option<usize>) -> bool {
        self.stable(value)
            || matches!(value, RValue::Local(local)
                if !self.closure_written.contains(&local.stable_id()) && self.register_of(local, function))
    }

    /// A read the code a call runs cannot change: a literal or a local [`stable`]
    /// here. Even builtin lookups may run code in an externally supplied environment.
    pub(crate) fn unchanged_by_calls(&self, value: &RValue) -> bool {
        matches!(value, RValue::Literal(_)) || self.stable(value)
    }

    fn spend<const NODE_LIMIT: usize>(&mut self, depth: usize) -> bool {
        self.nodes += 1;
        if depth >= 128 || self.nodes > NODE_LIMIT { self.exhausted = true; }
        !self.exhausted
    }

    /// `upvalues`: the locals the function owning `statements` reads as
    /// upvalues; `owner`: its identity (`None`: the chunk).
    fn block<const FACTS: u8, const NODE_LIMIT: usize>(&mut self, statements: &[Statement], depth: usize, upvalues: &FxHashSet<u64>, owner: Option<usize>) {
        for statement in statements {
            if !self.spend::<NODE_LIMIT>(depth) { return; }
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
            if FACTS >= FULL {
                match statement {
                    Statement::NumericFor(node) => { self.rebound.insert(node.counter.stable_id()); }
                    Statement::GenericFor(node) => {
                        self.rebound.extend(node.res_locals.iter().map(crate::RcLocal::stable_id));
                    }
                    Statement::Call(call) => self.call(call, owner),
                    // `local function f` / `local f = function`: the closure's name.
                    Statement::Assign(assign) if assign.prefix => {
                        for left in &assign.left {
                            if let LValue::Local(local) = left {
                                self.calls.declared_in.insert(local.stable_id(), owner);
                            }
                        }
                        if let ([LValue::Local(name)], [RValue::Closure(closure)]) = (assign.left.as_slice(), assign.right.as_slice()) {
                            let identity = triomphe::Arc::as_ptr(&closure.function.0) as usize;
                            let entry = self.calls.bound.entry(identity).or_insert((0, Some(name.stable_id())));
                            entry.0 += 1;
                            if entry.1 != Some(name.stable_id()) {
                                entry.1 = None;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if FACTS >= FULL && let Statement::Assign(assign) = statement {
                for left in &assign.left {
                    match left {
                        LValue::Local(local) if !assign.prefix => {
                            self.rebound.insert(local.stable_id());
                            if upvalues.contains(&local.stable_id()) {
                                self.closure_written.insert(local.stable_id());
                                if let Some(owner) = owner {
                                    self.calls.writers.entry(local.stable_id()).or_default().insert(owner);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            match statement {
                Statement::If(s) => {
                    self.block::<FACTS, NODE_LIMIT>(&s.then_block.lock().0, depth + 1, upvalues, owner);
                    self.block::<FACTS, NODE_LIMIT>(&s.else_block.lock().0, depth + 1, upvalues, owner);
                }
                Statement::While(s) => self.block::<FACTS, NODE_LIMIT>(&s.block.lock().0, depth + 1, upvalues, owner),
                Statement::Repeat(s) => self.block::<FACTS, NODE_LIMIT>(&s.block.lock().0, depth + 1, upvalues, owner),
                Statement::NumericFor(s) => self.block::<FACTS, NODE_LIMIT>(&s.block.lock().0, depth + 1, upvalues, owner),
                Statement::GenericFor(s) => self.block::<FACTS, NODE_LIMIT>(&s.block.lock().0, depth + 1, upvalues, owner),
                _ => {}
            }
            if self.exhausted { return; }
            crate::deinline::visit_stmt_rvalues(statement, &mut |value| {
                self.value::<FACTS, NODE_LIMIT>(value, depth + 1, owner);
                !self.exhausted
            });
            if self.exhausted { return; }
        }
    }

    /// A call by a local's name, made in `owner`.
    fn call(&mut self, call: &crate::Call, owner: Option<usize>) {
        if let RValue::Local(callee) = call.value.as_ref() {
            *self.calls.callee_reads.entry(callee.stable_id()).or_default() += 1;
            if let Some(owner) = owner {
                self.calls.callers.entry(callee.stable_id()).or_default().insert(owner);
            }
        }
    }

    fn value<const FACTS: u8, const NODE_LIMIT: usize>(&mut self, value: &RValue, depth: usize, owner: Option<usize>) {
        if !self.read_timing {
            self.value_node::<FACTS, NODE_LIMIT>(value, depth, owner);
            if !self.exhausted && !matches!(value, RValue::Closure(_)) {
                value.visit_rvalues(&mut |child| {
                    self.value::<FACTS, NODE_LIMIT>(child, depth + 1, owner);
                    !self.exhausted
                });
            }
            return;
        }
        let mut pending = vec![value];
        while let Some(value) = pending.pop() {
            self.value_node::<FACTS, NODE_LIMIT>(value, depth, owner);
            if self.exhausted { return; }
            if matches!(value, RValue::Closure(_)) { continue; }
            let start = pending.len();
            value.visit_rvalues(&mut |child| {
                if pending.len() >= NODE_LIMIT.saturating_sub(self.nodes) {
                    self.exhausted = true;
                    return false;
                }
                pending.push(child);
                true
            });
            if self.exhausted { return; }
            // Keep the original census order, including shared closure occurrences.
            pending[start..].reverse();
        }
    }

    fn value_node<const FACTS: u8, const NODE_LIMIT: usize>(&mut self, value: &RValue, depth: usize, owner: Option<usize>) {
        if !self.spend::<NODE_LIMIT>(depth) { return; }
        let width = match value {
            RValue::Table(s) => s.0.len().saturating_mul(2),
            RValue::Call(s) | RValue::Select(crate::Select::Call(s)) => s.arguments.len(),
            RValue::MethodCall(s) | RValue::Select(crate::Select::MethodCall(s)) => s.arguments.len(),
            RValue::Closure(s) => s.upvalues.len(),
            _ => 0,
        };
        if width > 200_000usize.saturating_sub(self.nodes) { self.exhausted = true; return; }
        if FACTS >= FULL {
            if names_environment_function(value) {
                self.dynamic_environment = true;
            }
            match value {
                RValue::Local(local) => *self.calls.reads.entry(local.stable_id()).or_default() += 1,
                RValue::Call(call) | RValue::Select(crate::Select::Call(call)) => {
                    self.call(call, owner);
                    if debug_member(&call.value) == Some(Some(b"info".as_slice())) {
                        self.frames.info_calls += 1;
                        if reads_frame_identity(call) {
                            self.frames.read = true;
                        }
                        if reads_beyond_caller(call) {
                            self.frames.callers.extend(owner);
                        }
                        // The options come last: `debug.info(level, options)`,
                        // `debug.info(thread, level, options)`, `debug.info(f, options)`.
                        self.frames.functions |= !matches!(call.arguments.last(),
                            Some(RValue::Literal(Literal::String(options))) if !options.contains(&b'f'));
                    }
                }
                _ if is_debug_library(value) => self.frames.library += 1,
                RValue::Index(_) => match debug_member(value) {
                    Some(Some(name)) => {
                        self.frames.members += 1;
                        self.frames.info += u32::from(name == b"info");
                    }
                    Some(None) => self.frames.computed = true,
                    None => {}
                },
                _ => {}
            }
        }
        if !self.read_timing && let RValue::Literal(crate::Literal::String(bytes)) = value {
            self.literal_bytes = self.literal_bytes.saturating_add(bytes.len());
            if self.literal_bytes > 8 * 1024 * 1024 { self.exhausted = true; return; }
        }
        if let RValue::Closure(closure) = value {
            let identity = triomphe::Arc::as_ptr(&closure.function.0) as usize;
            if FACTS >= FULL {
                *self.calls.closures.entry(identity).or_default() += 1;
            }
            if FACTS >= CAPTURES {
                let incoming = closure.upvalues.iter().map(|capture| {
                    let (Upvalue::Ref(local) | Upvalue::Copy(local)) = capture;
                    local.stable_id()
                }).collect::<FxHashSet<_>>();
                match self.upvalues.entry(identity) {
                    std::collections::hash_map::Entry::Vacant(entry) => { entry.insert(incoming); }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if entry.get() != &incoming { self.ambiguous_upvalues.insert(identity); }
                        entry.get_mut().extend(incoming);
                    }
                }
            }
            for capture in &closure.upvalues {
                if !self.spend::<NODE_LIMIT>(depth) { return; }
                if FACTS >= CAPTURES {
                    let (Upvalue::Ref(local) | Upvalue::Copy(local)) = capture;
                    self.captured.insert(local.stable_id());
                    if let Upvalue::Ref(local) = capture { self.references.insert(local.stable_id()); }
                }
                if FACTS >= FULL && let Upvalue::Copy(local) = capture {
                    self.copied_upvalues.entry(identity).or_default().insert(local.stable_id());
                }
            }
            if self.visited.insert(identity) {
                // Only the full census reads a function's upvalues (writes to them).
                let upvalues = if FACTS >= FULL { self.upvalues[&identity].clone() } else { FxHashSet::default() };
                self.block::<FACTS, NODE_LIMIT>(&closure.function.0.lock().body.0, depth + 1, &upvalues, Some(identity));
            }
        }
    }
}

/// The global `name`, or `_G.name` (`_G["name"]`).
fn names_global(value: &RValue, name: &[u8]) -> bool {
    match value {
        RValue::Global(global) => global.0 == name,
        RValue::Index(index) => matches!(index.left.as_ref(), RValue::Global(global) if global.0 == b"_G")
            && matches!(index.right.as_ref(), RValue::Literal(Literal::String(key)) if key == name),
        _ => false,
    }
}

/// `debug`, or `_G.debug`.
fn is_debug_library(value: &RValue) -> bool {
    names_global(value, b"debug")
}

/// For `debug.<name>`, `Some(Some(name))`; for a computed member of
/// `debug`, `Some(None)`; anything else `None`.
fn debug_member(value: &RValue) -> Option<Option<&[u8]>> {
    let RValue::Index(index) = value else { return None };
    if !is_debug_library(&index.left) {
        return None;
    }
    Some(match index.right.as_ref() {
        RValue::Literal(Literal::String(name)) => Some(name.as_slice()),
        _ => None,
    })
}

/// Whether `value` names getfenv or setfenv, as a global or as a key
/// (`_G.setfenv`), as Luau's compiler and the chunk's string table see it.
pub(crate) fn names_environment_function(value: &RValue) -> bool {
    matches!(value, RValue::Global(crate::Global(name)) | RValue::Literal(Literal::String(name))
        if matches!(name.as_slice(), b"getfenv" | b"setfenv"))
}

/// Whether a `debug.info` call reads which frames run. One frame more, a
/// helper's, changes no answer of `debug.info(1, "s"/"l")`: the running
/// function stays in this script (`s`), and no decompiled line keeps its
/// number anyway (`l`). A function (`f`), name (`n`) or arity (`a`), or a
/// caller (level 2 or deeper, which may be in another script), may name
/// another frame.
fn reads_frame_identity(call: &crate::Call) -> bool {
    !matches!(call.arguments.as_slice(), [
        RValue::Literal(Literal::Number(level)),
        RValue::Literal(Literal::String(options)),
    ] if *level == 1.0 && options.iter().all(|option| matches!(option, b's' | b'l')))
}

/// Whether a `debug.info` call in a function that code in a helper calls may
/// see the frame the helper adds, or one it shifts. The function's own frame
/// (level 1) stays; its caller (level 2) becomes the helper, in the same
/// script (`s`) and on no line the output keeps anyway (`l`). Its name (`n`),
/// identity (`f`) or arity (`a`), or a frame above (level 3 or deeper), may
/// change.
fn reads_beyond_caller(call: &crate::Call) -> bool {
    !matches!(call.arguments.as_slice(), [
        RValue::Literal(Literal::Number(level)),
        RValue::Literal(Literal::String(options)),
    ] if *level == 1.0 || (*level == 2.0 && options.iter().all(|option| matches!(option, b's' | b'l'))))
}

/// Whether `value` calls `debug.info` reading which frames run, or
/// `debug.traceback`, which lists them, closure bodies aside: a helper taking
/// a trace itself lists one more frame once its copies are calls. Unlike a
/// `debug.info` call, a trace taken elsewhere (in a function the code calls,
/// in an error handler, `xpcall(f, debug.traceback)`) marks nothing for the
/// module: it is the diagnostic of an error, like the position `error`
/// prefixes, and tracking its callers closed every Promise library, whose
/// error handler (an anonymous closure) takes one.
fn calls_debug_info(value: &RValue) -> bool {
    match value {
        RValue::Closure(_) => false,
        RValue::Call(call) | RValue::Select(crate::Select::Call(call))
            if (debug_member(&call.value) == Some(Some(b"info".as_slice())) && reads_frame_identity(call))
                || debug_member(&call.value) == Some(Some(b"traceback".as_slice())) =>
        {
            true
        }
        _ => !value.visit_rvalues(&mut |child| !calls_debug_info(child)),
    }
}

/// Whether `value` calls one of `names` (`f(...)`), closure bodies aside.
fn calls_any(value: &RValue, names: &FxHashSet<u64>) -> bool {
    match value {
        RValue::Closure(_) => return false,
        RValue::Call(call) | RValue::Select(crate::Select::Call(call))
            if matches!(call.value.as_ref(), RValue::Local(callee) if names.contains(&callee.stable_id())) =>
        {
            return true;
        }
        _ => {}
    }
    !value.visit_rvalues(&mut |child| !calls_any(child, names))
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

/// Classify builtin/library names (`print`, `math.max`, `Enum.KeyCode.E`);
/// an external environment can still make their lookup observable.
pub fn library_import(value: &RValue) -> bool {
    import_root(value).is_some_and(|(name, fields)| if fields == 0 { builtin_global(name) } else { fixed_library(name) })
}

/// What a chunk's bytecode shows about its globals, for passes that run
/// before an AST census ([`CaptureSafety`]) exists. The default knows of no
/// assignment and no environment change.
#[derive(Clone, Debug, Default)]
pub struct ChunkGlobals {
    /// The chunk names `getfenv` or `setfenv`: a function may get globals of
    /// its own, any of which may be a table with an `__index`.
    pub dynamic_environment: bool,
    /// Globals the chunk assigns (SETGLOBAL): `math = setmetatable(...)`.
    pub written: FxHashSet<Vec<u8>>,
    /// Some global the chunk reads or assigns has a name no identifier
    /// spells (patched bytecode only): source reaches it as
    /// `getfenv(1)["name"]`.
    pub unspellable: bool,
}

impl ChunkGlobals {
    /// A library name with no recorded environment/root write in this chunk;
    /// this does not prove lookup purity or exclude replacement by outside code.
    pub fn fixed_library_import(&self, value: &RValue) -> bool {
        !self.dynamic_environment
            && library_import(value)
            && import_root(value).is_some_and(|(name, _)| !self.written.contains(name))
    }
}

/// Names of globals Luau and Roblox provide.
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

/// Names commonly used for library tables.
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
/// Node units one module's de-inline search may spend. The largest real
/// module (GardenVisualRegistry, 2,300 lines, 60 helpers) spends about 23M
/// with the value-site result modes and discard variants; past the budget
/// every remaining site is refused, so it bounds pathological inputs only.
const SEARCH_BUDGET: usize = 32_000_000;

impl Default for SearchBudget {
    fn default() -> Self {
        Self { remaining: std::cell::Cell::new(SEARCH_BUDGET), exhausted: std::cell::Cell::new(false) }
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
    fn absent_environment_writes_do_not_make_import_lookups_inert() {
        let local = crate::RcLocal::default();
        let facts = CaptureSafety::new(&Block::default());
        let global = RValue::from(crate::Global::from("math"));
        let member = RValue::from(crate::Index::new(global.clone(), Literal::String(b"max".to_vec()).into()));
        assert!(!facts.dynamic_environment());
        for value in [global, member, crate::Global::from("print").into()] {
            assert!(!facts.unchanged_by_calls(&value));
        }
        assert!(facts.unchanged_by_calls(&local.into()));
        assert!(facts.unchanged_by_calls(&Literal::Number(2.0).into()));
    }
    #[test]
    fn copied_upvalues_need_the_original_value_even_with_shared_closures() {
        let cell = crate::RcLocal::default();
        let function = by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function::default())));
        let identity = triomphe::Arc::as_ptr(&function.0) as usize;
        let capture = |copy| crate::Return::new(vec![crate::Closure {
            node_origin: Default::default(), function: function.clone(),
            upvalues: vec![if copy { Upvalue::Copy(cell.clone()) } else { Upvalue::Ref(cell.clone()) }],
        }.into()]).into();
        let write = crate::Assign::new(vec![cell.clone().into()], vec![Literal::Number(2.0).into()]);
        assert!(CaptureSafety::new(&Block(vec![capture(true)])).copies_unchanged(identity));
        assert!(CaptureSafety::new(&Block(vec![capture(false), write.clone().into()])).copies_unchanged(identity));
        for copies_first in [true, false] {
            let body = Block(vec![capture(copies_first), capture(!copies_first), write.clone().into()]);
            assert!(!CaptureSafety::new(&body).copies_unchanged(identity));
        }
        let loop_body = Block(vec![capture(true)]);
        let numeric = crate::NumericFor::new(Literal::Number(1.0).into(), Literal::Number(3.0).into(),
            Literal::Number(1.0).into(), cell.clone(), loop_body.clone());
        let generic = crate::GenericFor::new(vec![cell], vec![crate::Global::from("iterator").into()], loop_body);
        for node in [Statement::from(numeric), Statement::from(generic)] {
            assert!(!CaptureSafety::new(&Block(vec![node])).copies_unchanged(identity));
        }
        assert!(!CaptureSafety::exhausted_for_tests().copies_unchanged(identity));
    }

    #[test]
    fn incoming_upvalues_require_consistent_complete_capture_contexts() {
        let [key, other, owned] = std::array::from_fn(|_| crate::RcLocal::default());
        let function = by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function::default())));
        let identity = triomphe::Arc::as_ptr(&function.0) as usize;
        let closure = |upvalues| -> RValue { crate::Closure {
            node_origin: Default::default(), function: function.clone(), upvalues,
        }.into() };
        for mismatch in [false, true] {
            for reverse in [false, true] {
                let mut values = vec![
                    closure(vec![Upvalue::Copy(key.clone()), Upvalue::Ref(other.clone())]),
                    closure(if mismatch { vec![Upvalue::Ref(key.clone())] } else {
                        // Capture order, duplicates and Copy/Ref modes do not change read timing.
                        vec![Upvalue::Copy(other.clone()), Upvalue::Ref(key.clone()), Upvalue::Copy(key.clone())]
                    }),
                ];
                if reverse { values.reverse(); }
                let body = Block(vec![crate::Return::new(values).into()]);
                for mut safety in [CaptureSafety::captures(&body), CaptureSafety::new(&body),
                    CaptureSafety::captures_for_read_timing(&body), CaptureSafety::writes_for_read_timing(&body)] {
                    assert!(safety.complete());
                    for incoming in [&key, &other] {
                        assert_eq!(safety.upvalue_of(incoming, Some(identity)), !mismatch);
                        assert!(!safety.register_of(incoming, Some(identity)));
                    }
                    assert!(safety.register_of(&owned, Some(identity)));
                    assert!(!safety.upvalue_of(&owned, Some(identity)));
                    assert!(!safety.upvalue_of(&key, None));
                    assert!(!safety.upvalue_of(&key, Some(usize::MAX)));
                    safety.exhausted = true;
                    assert!(!safety.upvalue_of(&key, Some(identity)));
                    assert!(!safety.register_of(&owned, Some(identity)));
                }
            }
        }
    }

    #[test]
    fn helper_shape_budget_matches_full_census_at_boundaries_and_shared_closures() {
        let check = |body: &Block| {
            let full = CaptureSafety::new(body);
            assert_eq!(CaptureSafety::helper_within_shape_budget(body), full.complete() && full.nodes() <= 2048);
        };
        for count in [0, 1, 2047, 2048, 2049, 4096] {
            check(&Block(vec![Statement::Empty(crate::Empty {}); count]));
        }
        // Width is a conservative allocation guard, not the node count: local
        // assignment destinations are not values visited by this census.
        check(&Block(vec![crate::Assign::new(
            (0..2048).map(|_| crate::RcLocal::default().into()).collect(), vec![]).into()]));
        for bytes in [8 * 1024 * 1024, 8 * 1024 * 1024 + 1] {
            check(&Block(vec![crate::Return::new(vec![Literal::String(vec![0; bytes]).into()]).into()]));
        }
        let mut nested = Block(vec![crate::Return::new(vec![Literal::Nil.into()]).into()]);
        for _ in 0..130 {
            check(&nested);
            nested = Block(vec![crate::If::new(Literal::Boolean(true).into(), nested, Block::default()).into()]);
        }
        let local = crate::RcLocal::default();
        let closure = crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
                body: Block(vec![crate::Return::new(vec![local.clone().into()]).into(); 600]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Copy(local.clone()), Upvalue::Ref(local)],
        };
        // Each occurrence charges its captures; the shared function body is
        // visited only once, including when reached through an LHS operand.
        check(&Block(vec![crate::Assign::new(
            vec![crate::Index::new(closure.clone().into(), Literal::Nil.into()).into()],
            vec![closure.clone().into(), closure.into()],
        ).into()]));
    }

    #[test]
    fn capture_census_answers_captures_as_the_full_census() {
        let locals: Vec<crate::RcLocal> = (0..4).map(|_| crate::RcLocal::default()).collect();
        let function = |body: Vec<Statement>| by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
            body: Block(body),
            ..Default::default()
        })));
        let inner = crate::Closure {
            node_origin: Default::default(),
            function: function(vec![crate::Return::new(vec![locals[2].clone().into()]).into()]),
            upvalues: vec![Upvalue::Ref(locals[2].clone())],
        };
        let outer_function = function(vec![
            crate::Assign::new(vec![locals[1].clone().into()], vec![inner.clone().into()]).into(),
            crate::Return::new(vec![locals[0].clone().into()]).into(),
        ]);
        let outer = crate::Closure { node_origin: Default::default(), function: outer_function.clone(), upvalues: vec![Upvalue::Copy(locals[0].clone())] };
        let identities = [None, Some(triomphe::Arc::as_ptr(&outer_function.0) as usize),
            Some(triomphe::Arc::as_ptr(&inner.function.0) as usize)];
        let mut bodies = vec![
            Block::default(),
            Block(vec![crate::Assign::new(vec![locals[3].clone().into()], vec![outer.clone().into()]).into()]),
            Block(vec![crate::Return::new(vec![outer.clone().into(), outer.into()]).into()]),
        ];
        // Past the node budget both censuses know nothing.
        bodies.push(Block(vec![Statement::Empty(crate::Empty {}); 200_001]));
        for body in &bodies {
            let full = CaptureSafety::new(body);
            let captures = CaptureSafety::captures(body);
            assert_eq!(full.complete(), captures.complete());
            for local in &locals {
                assert_eq!(full.uncaptured(local), captures.uncaptured(local));
                for function in identities {
                    assert_eq!(full.register_of(local, function), captures.register_of(local, function));
                    assert_eq!(full.upvalue_of(local, function), captures.upvalue_of(local, function));
                }
            }
        }
    }

    #[test]
    fn read_timing_census_keeps_facts_inside_deep_expressions_and_shared_cycles() {
        let cell = crate::RcLocal::default();
        let closure = crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
                body: Block(vec![crate::Assign::new(vec![cell.clone().into()], vec![Literal::Nil.into()]).into()]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Ref(cell.clone())],
        };
        let identity = triomphe::Arc::as_ptr(&closure.function.0) as usize;
        let mut value: RValue = closure.clone().into();
        for _ in 0..256 {
            value = crate::Binary::new(Literal::Number(1.0).into(), value, crate::BinaryOperation::Add).into();
        }
        let body = Block(vec![crate::Return::new(vec![value, closure.clone().into()]).into()]);
        assert!(!CaptureSafety::new(&body).complete());
        assert!(!CaptureSafety::captures(&body).complete());
        assert!(!CaptureSafety::helper_within_shape_budget(&body));
        for safety in [CaptureSafety::captures_for_read_timing(&body), CaptureSafety::writes_for_read_timing(&body)] {
            assert!(safety.complete());
            assert!(safety.upvalue_of(&cell, Some(identity)));
            assert!(!safety.uncaptured(&cell));
        }
        let safety = CaptureSafety::writes_for_read_timing(&body);
        assert!(!safety.never_reassigned(&cell));
        assert!(safety.closure_written(&cell));
        assert_eq!(safety.calls.closures[&identity], 2);

        closure.function.lock().body.0.push(crate::Return::new(vec![closure.clone().into()]).into());
        for safety in [CaptureSafety::captures_for_read_timing(&body), CaptureSafety::writes_for_read_timing(&body)] {
            assert!(safety.complete());
            assert!(safety.upvalue_of(&cell, Some(identity)));
        }
        // Break the deliberately cyclic fixture's Arc ownership after checking the visited set.
        closure.function.lock().body.0.pop();
    }

    #[test]
    fn read_timing_census_retains_control_closure_and_node_limits() {
        for closures in [false, true] {
            let mut body = Block(vec![crate::Return::new(vec![Literal::Nil.into()]).into()]);
            for _ in 0..130 {
                body = if closures {
                    Block(vec![crate::Return::new(vec![crate::Closure {
                        node_origin: Default::default(),
                        function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
                            body, ..Default::default()
                        }))),
                        upvalues: vec![],
                    }.into()]).into()])
                } else {
                    Block(vec![crate::If::new(Literal::Boolean(true).into(), body, Block::default()).into()])
                };
            }
            assert!(!CaptureSafety::captures_for_read_timing(&body).complete());
            assert!(!CaptureSafety::writes_for_read_timing(&body).complete());
        }
        for count in [199_999, 200_000] {
            let body = Block(vec![crate::Return::new(vec![Literal::Nil.into(); count]).into()]);
            for safety in [CaptureSafety::captures_for_read_timing(&body), CaptureSafety::writes_for_read_timing(&body)] {
                assert_eq!(safety.complete(), count == 199_999);
            }
        }
        // A tiny instance of the same node bound also limits pending children.
        let value = crate::Table::new(vec![(None, Literal::Nil.into()); 4]);
        let body = Block(vec![crate::Return::new(vec![value.into()]).into()]);
        let mut safety = CaptureSafety { read_timing: true, ..Default::default() };
        safety.block::<CAPTURES, 5>(&body.0, 0, &FxHashSet::default(), None);
        assert!(!safety.complete());
    }

    #[test]
    fn budgets_fail_closed_and_literals_do_not_hide_environment_lookups() {
        let safety = CaptureSafety::new(&Block::default());
        for n in [f64::INFINITY, f64::NAN, std::f64::consts::PI, -std::f64::consts::PI] {
            assert!(!safety.stable(&crate::Literal::Number(n).into()));
        }
        for n in [0.0, -0.0, 2.0] { assert!(safety.stable(&crate::Literal::Number(n).into())); }
        let budget = SearchBudget::default();
        assert!(budget.spend(SEARCH_BUDGET));
        assert!(!budget.exhausted());
        assert!(!budget.spend(1));
        assert!(budget.exhausted());
        assert!(!budget.spend(0));
        let huge = Block(vec![crate::Return::new(vec![crate::Literal::String(vec![0; 8 * 1024 * 1024 + 1]).into()]).into()]);
        assert!(!CaptureSafety::new(&huge).complete());
        assert!(!CaptureSafety::captures(&huge).complete());
        assert!(CaptureSafety::captures_for_read_timing(&huge).complete());
    }

    /// `local total; local function bump() total += 1 end`: while `bump` is
    /// only called by name, nothing but `bump()` changes `total`; once it is
    /// passed as a value, any call or metamethod might.
    #[test]
    fn only_calls_of_named_writers_change_a_cell_until_one_escapes() {
        let (total, bump, object) = (crate::RcLocal::default(), crate::RcLocal::default(), crate::RcLocal::default());
        let writer = crate::Function {
            body: Block(vec![crate::Assign::new(vec![total.clone().into()], vec![crate::Literal::Number(1.0).into()]).into()]),
            ..Default::default()
        };
        let declare = |local: &crate::RcLocal, value: RValue| -> Statement {
            let mut assign = crate::Assign::new(vec![local.clone().into()], vec![value]);
            assign.prefix = true;
            assign.into()
        };
        let closure = crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(writer))),
            upvalues: vec![Upvalue::Ref(total.clone())],
        };
        let mut statements = vec![
            declare(&total, crate::Literal::Number(0.0).into()),
            declare(&bump, closure.into()),
            crate::Call::new(bump.clone().into(), vec![]).into(),
        ];
        let field: RValue = crate::Index::new(object.into(), crate::Literal::String(b"x".to_vec()).into()).into();
        let call: RValue = crate::Call::new(bump.clone().into(), vec![]).into();
        let safety = CaptureSafety::new(&Block(statements.clone()));
        let may_change = safety.may_change(&total);
        assert!(may_change(&call));
        assert!(!may_change(&field));
        statements.push(crate::Call::new(RValue::Global(crate::Global::from("register")), vec![bump.into()]).into());
        let safety = CaptureSafety::new(&Block(statements));
        let may_change = safety.may_change(&total);
        assert!(may_change(&field));
    }

    #[test]
    fn vector_constants_are_stable_but_constructors_are_not() {
        let safety = CaptureSafety::default();
        assert!(safety.stable(&crate::Literal::Vector(0.0, -0.0, 1.0).into()));
        assert!(safety.stable(&crate::Literal::VectorD(0.0, -0.0, 1.0).into()));
        assert!(!safety.stable(&crate::Literal::Vector(f32::NAN, 0.0, 1.0).into()));
        assert!(!safety.stable(&crate::Call::new(member(global("vector"), "create"),
            vec![crate::Literal::Number(1.0).into()]).into()));
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

    fn global(name: &str) -> RValue { RValue::Global(crate::Global::from(name)) }
    fn member(left: RValue, name: &str) -> RValue {
        crate::Index::new(left, crate::Literal::String(name.as_bytes().to_vec()).into()).into()
    }
    fn census(values: Vec<RValue>) -> CaptureSafety {
        CaptureSafety::new(&Block(values.into_iter().map(|value| crate::Return::new(vec![value]).into()).collect()))
    }

    /// `setfenv` reached through `_G` changes the environment as much as the
    /// bare global does.
    #[test]
    fn environment_functions_count_however_they_are_named() {
        assert!(!census(vec![global("print")]).dynamic_environment());
        assert!(census(vec![global("setfenv")]).dynamic_environment());
        assert!(census(vec![member(global("_G"), "setfenv")]).dynamic_environment());
        assert!(census(vec![member(global("_G"), "getfenv")]).dynamic_environment());
    }

    /// A direct `debug.info(...)` call reads frames; any other use of the
    /// library may call it under another name. Code calling
    /// `debug.traceback(...)` itself reads frames too, but marks nothing for
    /// the module.
    #[test]
    fn debug_info_is_tracked_by_its_calls() {
        let call = |callee: RValue| -> RValue { crate::Call::new(callee, vec![]).into() };
        let traceback = census(vec![call(member(global("debug"), "traceback"))]);
        assert!(!traceback.reads_call_frames() && !traceback.call_frames_untracked());
        let traced = crate::Call::new(member(global("debug"), "traceback"), vec![]);
        assert!(traceback.reads_frames(&[crate::Return::new(vec![traced.into()]).into()]));
        let handler = census(vec![member(global("debug"), "traceback")]);
        assert!(!handler.reads_call_frames());
        for direct in [member(global("debug"), "info"), member(member(global("_G"), "debug"), "info")] {
            let safety = census(vec![call(direct)]);
            assert!(safety.reads_call_frames() && !safety.call_frames_untracked());
        }
        for escaped in [
            global("debug"),
            member(global("_G"), "debug"),
            member(global("debug"), "info"),
            crate::Index::new(global("debug"), global("key")).into(),
        ] {
            assert!(census(vec![escaped]).call_frames_untracked());
        }
    }

    /// One frame more keeps the running function's source and moves no
    /// line that survives anyway; a caller's (level 2) may be in another
    /// script, and only `f` hands out a function.
    #[test]
    fn debug_info_levels_and_options() {
        let info = |arguments: Vec<RValue>| -> RValue {
            crate::Call::new(member(global("debug"), "info"), arguments).into()
        };
        let number = |n: f64| RValue::Literal(Literal::Number(n));
        let string = |s: &str| RValue::Literal(Literal::String(s.as_bytes().to_vec()));
        let running = census(vec![info(vec![number(1.0), string("sl")])]);
        assert!(!running.reads_call_frames() && !running.frames.functions);
        let caller = census(vec![info(vec![number(2.0), string("sl")])]);
        assert!(caller.reads_call_frames() && !caller.frames.functions);
        assert!(census(vec![info(vec![number(1.0), string("f")])]).frames.functions);
        assert!(census(vec![info(vec![number(1.0), global("options")])]).frames.functions);
    }

    /// `function Text:Render() local source = debug.info(2, "s") end`: called
    /// from a helper, the method sees the helper, in the same script, where it
    /// saw the helper's caller; its callers need no tracking, so a method may
    /// read it. Its caller's name, or the frame above, still may change.
    #[test]
    fn a_method_reading_its_callers_source_keeps_the_module_open() {
        let info = |arguments: Vec<RValue>| -> RValue {
            crate::Call::new(member(global("debug"), "info"), arguments).into()
        };
        let number = |n: f64| RValue::Literal(Literal::Number(n));
        let string = |s: &str| RValue::Literal(Literal::String(s.as_bytes().to_vec()));
        let method = |reader: RValue| -> Block {
            let function = crate::Function {
                body: Block(vec![crate::Return::new(vec![reader]).into()]),
                ..Default::default()
            };
            let closure = crate::Closure {
                node_origin: Default::default(),
                function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(function))),
                upvalues: vec![],
            };
            let render = crate::Index::new(global("Text"), string("Render"));
            Block(vec![crate::Assign::new(vec![render.into()], vec![closure.into()]).into()])
        };
        assert!(!CaptureSafety::new(&method(info(vec![number(2.0), string("sl")]))).call_frames_untracked());
        assert!(!CaptureSafety::new(&method(info(vec![number(1.0), string("n")]))).call_frames_untracked());
        assert!(CaptureSafety::new(&method(info(vec![number(2.0), string("n")]))).call_frames_untracked());
        assert!(CaptureSafety::new(&method(info(vec![number(3.0), string("s")]))).call_frames_untracked());
        assert!(CaptureSafety::new(&method(info(vec![global("level"), string("s")]))).call_frames_untracked());
    }

    /// `error(message, level)`, at any level, reads no frame: the level only
    /// picks the position prefixed to a string message, which a decompile
    /// does not keep. `error` passed on as a value is no frame read either.
    #[test]
    fn error_levels_read_no_frames() {
        let number = |n: f64| RValue::Literal(Literal::Number(n));
        let string = RValue::Literal(Literal::String(b"bad".to_vec()));
        let error = |arguments: Vec<RValue>| crate::Call::new(global("error"), arguments);
        let through_g = |arguments: Vec<RValue>| crate::Call::new(member(global("_G"), "error"), arguments);
        for arguments in [
            vec![string.clone()],
            vec![string.clone(), number(0.0)],
            vec![string.clone(), number(2.0)],
            vec![string.clone(), number(3.0)],
            vec![string.clone(), global("level")],
            vec![crate::Call::new(global("reason"), vec![]).into()],
        ] {
            for call in [error(arguments.clone()), through_g(arguments)] {
                let safety = CaptureSafety::new(&Block(vec![call.clone().into()]));
                assert!(!safety.reads_call_frames() && !safety.call_frames_untracked());
                assert!(!safety.reads_frames(&[call.clone().into()]));
                assert!(!safety.reads_frames(&[crate::Return::new(vec![call.into()]).into()]));
            }
        }
        assert!(!census(vec![global("error")]).call_frames_untracked());
        assert!(!census(vec![member(global("_G"), "error")]).call_frames_untracked());
    }
}
