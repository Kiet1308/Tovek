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
    /// Globals the module assigns somewhere.
    written_globals: FxHashSet<Vec<u8>>,
    /// The module names `getfenv` or `setfenv` (`_G.setfenv` too).
    dynamic_environment: bool,
    /// How the module reaches `debug.info`, which reads call frames.
    frames: FrameReads,
    /// The locals each function reads as upvalues, by its identity.
    upvalues: FxHashMap<usize, FxHashSet<u64>>,
    /// Who may run a function that assigns a cell (see [`Self::private_writers`]).
    calls: CallGraph,
    visited: FxHashSet<usize>,
    nodes: usize,
    literal_bytes: usize,
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
    /// The functions making such a call themselves, by identity.
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

impl CaptureSafety {
    pub(crate) fn new(body: &Block) -> Self {
        let mut result = Self::default();
        result.block(&body.0, 0, &FxHashSet::default(), None);
        result.visited.clear();
        result.frames.observers = result.frame_observers();
        result
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
        if self.exhausted || self.reads_call_frames() {
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

    pub(crate) fn uncaptured(&self, local: &crate::RcLocal) -> bool {
        !self.exhausted && !self.captured.contains(&local.stable_id())
    }

    pub(crate) fn complete(&self) -> bool { !self.exhausted }

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
    /// through others. Moved into a helper, that code runs a frame deeper.
    /// A closure's body runs in a frame of its own wherever it is created.
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

    /// A global path no code the script runs can change and whose read
    /// cannot raise, so it may be read at any point: a builtin global
    /// (`print`, `workspace`) or a member of a fixed library table
    /// (`math.clamp`, `Vector2.new`), never assigned in the module, in a
    /// module with a fixed environment. Luau's own builtin calls assume the
    /// same. Any other path is a read of a table someone may write
    /// (`Shared.x`, `Foo.new`, `script.Parent`), of a global another chunk
    /// may set, or one that may raise ([`import_cannot_raise`]).
    pub(crate) fn constant_import(&self, value: &RValue) -> bool {
        !self.dynamic_environment()
            && import_root(value).is_some_and(|(name, _)| !self.written_globals.contains(name))
            && import_cannot_raise(value)
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

    /// `upvalues`: the locals the function owning `statements` reads as
    /// upvalues; `owner`: its identity (`None`: the chunk).
    fn block(&mut self, statements: &[Statement], depth: usize, upvalues: &FxHashSet<u64>, owner: Option<usize>) {
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
                    self.block(&s.then_block.lock().0, depth + 1, upvalues, owner);
                    self.block(&s.else_block.lock().0, depth + 1, upvalues, owner);
                }
                Statement::While(s) => self.block(&s.block.lock().0, depth + 1, upvalues, owner),
                Statement::Repeat(s) => self.block(&s.block.lock().0, depth + 1, upvalues, owner),
                Statement::NumericFor(s) => self.block(&s.block.lock().0, depth + 1, upvalues, owner),
                Statement::GenericFor(s) => self.block(&s.block.lock().0, depth + 1, upvalues, owner),
                _ => {}
            }
            if self.exhausted { return; }
            crate::deinline::visit_stmt_rvalues(statement, &mut |value| {
                self.value(value, depth + 1, owner);
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

    fn value(&mut self, value: &RValue, depth: usize, owner: Option<usize>) {
        if !self.spend(depth) { return; }
        let width = match value {
            RValue::Table(s) => s.0.len().saturating_mul(2),
            RValue::Call(s) | RValue::Select(crate::Select::Call(s)) => s.arguments.len(),
            RValue::MethodCall(s) | RValue::Select(crate::Select::MethodCall(s)) => s.arguments.len(),
            RValue::Closure(s) => s.upvalues.len(),
            _ => 0,
        };
        if width > 200_000usize.saturating_sub(self.nodes) { self.exhausted = true; return; }
        // The name read as a global or as a key (`_G.setfenv`), as Luau's
        // compiler and the chunk's string table see it.
        if let RValue::Global(crate::Global(name)) | RValue::Literal(crate::Literal::String(name)) = value
            && matches!(name.as_slice(), b"getfenv" | b"setfenv")
        {
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
                        self.frames.callers.extend(owner);
                    }
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
        if let RValue::Literal(crate::Literal::String(bytes)) = value {
            self.literal_bytes = self.literal_bytes.saturating_add(bytes.len());
            if self.literal_bytes > 8 * 1024 * 1024 { self.exhausted = true; return; }
        }
        if let RValue::Closure(closure) = value {
            let identity = triomphe::Arc::as_ptr(&closure.function.0) as usize;
            *self.calls.closures.entry(identity).or_default() += 1;
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
                let upvalues = self.upvalues[&identity].clone();
                self.block(&closure.function.0.lock().body.0, depth + 1, &upvalues, Some(identity));
            }
        } else {
            value.visit_rvalues(&mut |child| {
                self.value(child, depth + 1, owner);
                !self.exhausted
            });
        }
    }
}

/// `debug`, or `_G.debug`.
fn is_debug_library(value: &RValue) -> bool {
    match value {
        RValue::Global(global) => global.0 == b"debug",
        RValue::Index(index) => matches!(index.left.as_ref(), RValue::Global(global) if global.0 == b"_G")
            && matches!(index.right.as_ref(), RValue::Literal(Literal::String(name)) if name == b"debug"),
        _ => false,
    }
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

/// Whether a `debug.info` call reads which frames run. One frame more, a
/// helper's, changes no answer of `debug.info(1 or 2, "s"/"l")`: the frame
/// at those levels stays in this script (`s`), and no decompiled line keeps
/// its number anyway (`l`). A function (`f`), name (`n`) or arity (`a`), or
/// a deeper level, may name another frame.
fn reads_frame_identity(call: &crate::Call) -> bool {
    !matches!(call.arguments.as_slice(), [
        RValue::Literal(Literal::Number(level)),
        RValue::Literal(Literal::String(options)),
    ] if (*level == 1.0 || *level == 2.0) && options.iter().all(|option| matches!(option, b's' | b'l')))
}

/// Whether `value` calls `debug.info` reading which frames run, closure
/// bodies aside.
fn calls_debug_info(value: &RValue) -> bool {
    match value {
        RValue::Closure(_) => false,
        RValue::Call(call) | RValue::Select(crate::Select::Call(call))
            if debug_member(&call.value) == Some(Some(b"info".as_slice())) && reads_frame_identity(call) => true,
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

/// A builtin global or a member of a fixed library (`print`, `math.max`,
/// `Enum.KeyCode.E`): fetching it runs no code and, unless the script
/// replaces the library, always gives the same value.
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
    /// A [`library_import`] this chunk cannot have replaced: fetching it runs
    /// no script code.
    pub fn fixed_library_import(&self, value: &RValue) -> bool {
        !self.dynamic_environment
            && library_import(value)
            && import_root(value).is_some_and(|(name, _)| !self.written.contains(name))
    }
}

/// A builtin global or a member of a fixed library table (`print`,
/// `math.max`): GETIMPORT reads it without raising. A member's own field
/// may not exist (`math.abs.missing` indexes a function), and `Enum`
/// raises for a name it lacks (`Enum.KeyCode.E` included).
fn import_cannot_raise(value: &RValue) -> bool {
    import_root(value).is_some_and(|(name, fields)| match fields {
        0 => builtin_global(name),
        1 => fixed_library(name) && name != b"Enum",
        _ => false,
    })
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
    /// library may call it under another name.
    #[test]
    fn debug_info_is_tracked_by_its_calls() {
        let call = |callee: RValue| -> RValue { crate::Call::new(callee, vec![]).into() };
        let traceback = census(vec![call(member(global("debug"), "traceback"))]);
        assert!(!traceback.reads_call_frames());
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
}
