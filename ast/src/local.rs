use crate::{SideEffects, Traverse, Type, TypeSystem, type_system::Infer};
use by_address::ByAddress;
use enum_dispatch::enum_dispatch;
use nohash_hasher::NoHashHasher;
use parking_lot::Mutex;
use std::{
    fmt::{self, Display},
    hash::{Hash, Hasher},
};
use triomphe::Arc;

/// A local variable: its (eventual) source name, plus an optional naming hint
/// derived from the compiler's bytecode type information (`vector`, `buffer`,
/// `cframe`, ...).  The hint is attached by SSA construction from the lifter's
/// typed-register ranges and survives local coalescing (`apply_local_map`),
/// so the AST namer can consult it as the lowest-priority evidence once every
/// usage-based hint has had its chance.
#[derive(Debug, Default, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct Local(pub Option<String>, pub Option<String>, pub Vec<SourceBinding>, pub Option<Box<BindingLineage>>, pub BindingRoles);

/// Source presentation constraints, independent of storage ancestry and cell
/// ownership. A conditional result is inferred, never a recovered source local.
#[derive(Debug, Default, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct BindingRoles {
    pub parameter: bool,
    pub conditional_result: bool,
    pub separate_from_parameter: bool,
    /// When the namer had to count past a taken name, the byte length of the
    /// base it counted from (`part` of `part2`, `bit32` of `bit32_2`); 0
    /// otherwise. A later pass may rename the local, so the final namer reads
    /// it only through [`Local::namer_stem`], which checks it still applies.
    pub suffix_stem: u16,
}

/// Diagnostic ancestry of storage/SSA identities, not an equality or lifetime
/// certificate. IDs are scoped to one decompilation. An incomplete set may be
/// displayed, but must never be used to justify a rewrite.
#[derive(Debug, Default, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct BindingLineage {
    pub definitions: Vec<u64>,
    pub incomplete: bool,
    /// A new storage local was constructed from a cloned Local metadata value.
    /// This is an ancestry fact, not proof that an emitted expression was cloned.
    pub copied_local_metadata: bool,
}

impl BindingLineage {
    pub const LIMIT: usize = 256;

    pub fn add(&mut self, id: u64) {
        if let Err(index) = self.definitions.binary_search(&id) {
            if self.definitions.len() < Self::LIMIT {
                self.definitions.insert(index, id);
            } else {
                self.incomplete = true;
                // A bounded union must be independent of local-map iteration.
                if index < Self::LIMIT {
                    self.definitions.insert(index, id);
                    self.definitions.pop();
                }
            }
        }
    }

    fn inherit(&mut self, other: &Self) {
        self.incomplete |= other.incomplete;
        self.copied_local_metadata |= other.copied_local_metadata;
        for &id in &other.definitions { self.add(id); }
    }
}

/// Compiler-recorded identity, independent of spelling and SSA/storage identity.
/// Several origins are retained when a mandatory local map merges evidence.
#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum BindingOrigin {
    DebugLocal { prototype: usize, register: u8, start_pc: usize, end_pc: usize },
    DebugUpvalue { prototype: usize, slot: usize },
    Function { prototype: usize },
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct SourceBinding {
    pub origin: BindingOrigin,
    pub name: String,
}

/// Debug metadata is untrusted input. Never repair spelling and label it recovered.
pub fn valid_source_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !matches!(name, "and" | "break" | "do" | "else" | "elseif" | "end" | "false"
            | "for" | "function" | "goto" | "if" | "in" | "local" | "nil" | "not"
            | "or" | "repeat" | "return" | "then" | "true" | "until" | "while")
}

/// A recorded function name is already visible in a same-named field/global
/// definition. Keeping a temporary here would obscure method syntax. A real
/// debug local remains protected even if its spelling matches the field.
pub fn assignment_preserves_function_name(statement: &crate::Statement, local: &RcLocal) -> bool {
    let crate::Statement::Assign(assign) = statement else { return false; };
    if assign.left.len() != 1 || assign.right.len() != 1 || assign.right[0].as_local() != Some(local) {
        return false;
    }
    let evidence = local.0.lock();
    if evidence.2.is_empty() || evidence.2.iter().any(|b| !matches!(b.origin, BindingOrigin::Function { .. })) {
        return false;
    }
    let Some(name) = evidence.source_name() else { return false; };
    match &assign.left[0] {
        crate::LValue::Global(global) => global.0 == name.as_bytes(),
        crate::LValue::Index(index) => matches!(&*index.right,
            crate::RValue::Literal(crate::Literal::String(key)) if key == name.as_bytes()),
        _ => false,
    }
}

/// Late AST presentation exception only. The field already displays the
/// compiler's function-prototype name; a debug local/upvalue is still distinct
/// source evidence. This grants no motion/capture/arity permission, and is not
/// used by SSA inlining before closure bodies and captures have been linked.
pub(crate) fn constructor_preserves_function_name(statement: &crate::Statement, local: &RcLocal) -> bool {
    let name = {
        let evidence = local.0.lock();
        if evidence.2.is_empty() || evidence.2.iter().any(|b| !matches!(b.origin, BindingOrigin::Function { .. })) {
            return false;
        }
        let Some(name) = evidence.source_name() else { return false; };
        name.to_owned()
    };
    fn field(value: &crate::RValue, local: &RcLocal, name: &[u8], budget: &mut usize, depth: usize) -> bool {
        if depth >= 32 || *budget == 0 { return false; }
        let crate::RValue::Table(table) = value else { return false; };
        // Keep a uniform list of named helpers (and a lone helper) as source
        // structure. This exception repairs a mixed constructor that already
        // contains inline callbacks; it does not start nesting every function.
        let mixed_callbacks = table.0.iter().any(|(_, value)| matches!(value, crate::RValue::Closure(_)));
        for (key, value) in &table.0 {
            if *budget == 0 { return false; }
            *budget -= 1;
            if mixed_callbacks && value.as_local() == Some(local)
                && matches!(key, Some(crate::RValue::Literal(crate::Literal::String(key))) if key == name)
            { return true; }
            if field(value, local, name, budget, depth + 1) { return true; }
        }
        false
    }
    let values = match statement {
        crate::Statement::Assign(assign) => &assign.right,
        crate::Statement::Return(ret) => &ret.values,
        _ => return false,
    };
    let mut budget = 512;
    values.iter().any(|value| field(value, local, name.as_bytes(), &mut budget, 0))
}

impl From<Option<String>> for Local {
    fn from(name: Option<String>) -> Self {
        Self(name, None, Vec::new(), None, BindingRoles::default())
    }
}

impl Local {
    pub fn new(name: Option<String>) -> Self {
        Self(name, None, Vec::new(), None, BindingRoles::default())
    }

    /// An unnamed local carrying a bytecode-type naming hint.
    pub fn with_type_hint(hint: String) -> Self {
        Self(None, Some(hint), Vec::new(), None, BindingRoles::default())
    }

    /// The bytecode-type naming hint, if any.
    pub fn type_hint(&self) -> Option<&str> {
        self.1.as_deref()
    }

    /// Record the name the namer gave this local, counted from `base` when
    /// `base` itself was taken (see [`BindingRoles::suffix_stem`]).
    pub fn set_counted_name(&mut self, name: String, base: &str) {
        self.4.suffix_stem = if name != base && name.starts_with(base) { base.len().min(u16::MAX as usize) as u16 } else { 0 };
        self.0 = Some(name);
    }

    /// The base the namer counted from to spell this local's current name, and
    /// the counter: (`part`, 2) for `part2`. `None` once the name no longer has
    /// that shape (a later pass renamed the local).
    pub fn namer_stem(&self) -> Option<(&str, usize)> {
        let name = self.0.as_deref()?;
        let stem = name.get(..self.4.suffix_stem as usize).filter(|stem| !stem.is_empty())?;
        Some((stem, crate::name_spelling::suffix_of(name, stem)?))
    }

    pub fn add_source_binding(&mut self, binding: SourceBinding) {
        if valid_source_name(&binding.name) && !self.2.contains(&binding) {
            self.2.push(binding);
            self.2.sort();
        }
    }

    pub fn source_name(&self) -> Option<&str> {
        // A local's own debug interval outranks names recorded at capture sites
        // and the weaker function-prototype name. Intervals spelling different
        // names refuse; several of one name (a variable reused across the
        // source's `do` blocks) agree.
        let mut locals = self.2.iter().filter(|b| matches!(b.origin, BindingOrigin::DebugLocal { .. }));
        if let Some(local) = locals.next() {
            return locals.all(|other| other.name == local.name).then_some(local.name.as_str());
        }
        let first = self.2.first()?;
        self.2.iter().all(|b| b.name == first.name).then_some(first.name.as_str())
    }
}

impl fmt::Display for Local {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.0 {
            Some(name) => write!(f, "{}", name),
            None => write!(f, "UNNAMED_LOCAL"),
        }
    }
}

thread_local! {
    /// Per-thread monotonic id sequence, counting up once per `RcLocal::new` on
    /// this thread, reset to 0 at the start of every top-level decompilation by
    /// [`reset_local_ids`].
    ///
    /// Why per-thread + reset rather than one process-global atomic counter:
    /// `RcLocal`'s `Eq`/`Ord`/`Hash` are keyed on this id, and crucially
    /// `FxHashMap<RcLocal, _>` / `FxHashSet<RcLocal>` iteration order (used in
    /// several ordering-significant passes) depends on the *absolute* id value
    /// (the hash), not merely the relative creation order. The `decompile-folder`
    /// driver decompiles files concurrently on a rayon pool; a single shared
    /// counter let the `RcLocal::new` calls of concurrently-running files
    /// interleave, so a given file's locals got different absolute ids run-to-run,
    /// permuting their hashed iteration order and therefore their generated names.
    ///
    /// Each file is an independent decompilation unit: it `reset_local_ids()` at
    /// entry, lifts its functions sequentially on the calling thread (minting the
    /// monotonic high-water mark), then decompiles those functions *in parallel*.
    /// Each per-function task re-bases this counter to a disjoint, stride-spaced
    /// range keyed by the function's lift-order index (see [`set_local_id_base`])
    /// before it mints any `RcLocal`, so the ids a file assigns depend only on its
    /// own lift order — never on which rayon worker runs a function, how many ran
    /// before it, or what other files run concurrently on the shared pool. That
    /// makes every file's output byte-identical to decompiling it alone (`-e`),
    /// the determinism the `decompile-folder`/batch drivers rely on. Ids only ever
    /// need to be unique *within* one decompilation unit (locals from different
    /// files are never compared), which the per-file monotonic sequence (plus the
    /// strided per-function bases) guarantees.
    static NEXT_LOCAL_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Reset the per-thread local-id counter. Call once at the start of each
/// top-level decompilation so the ids it assigns are independent of any work the
/// thread did for previous files. See [`NEXT_LOCAL_ID`].
pub fn reset_local_ids() {
    NEXT_LOCAL_ID.with(|c| c.set(0));
}

/// The next id this thread would assign. Used by the per-function decompile loop
/// to capture the high-water mark after lifting, so each function can be given a
/// disjoint id range (see [`set_local_id_base`]).
pub fn current_local_id() -> u64 {
    NEXT_LOCAL_ID.with(|c| c.get())
}

/// Set this thread's local-id counter to `base`. The parallel per-function
/// decompile loop calls this at the start of each function with a stride-spaced
/// base derived from the function's index, so the ids a function mints depend
/// only on its position in the (deterministic) lift order — never on which rayon
/// worker runs it or how many functions ran on that worker before. The decompiled
/// output is independent of the absolute id values (it depends only on each
/// function's internal creation ORDER, which is thread-independent), so the
/// strided bases alone make the pipeline deterministic and byte-identical to the
/// old serial path — no renumber is required. The ranges must stay disjoint
/// (stride ≫ ids-per-function) and above the lifting high-water mark, both
/// guaranteed by the caller.
pub fn set_local_id_base(base: u64) {
    NEXT_LOCAL_ID.with(|c| c.set(base));
}

fn next_local_id() -> u64 {
    NEXT_LOCAL_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    })
}

/// A reference-counted local. Its identity (`Eq`/`Ord`/`Hash`) is keyed on a
/// stable, monotonically-assigned `id` (field `.1`) rather than the `Arc`'s
/// memory address.
///
/// Previously these traits were derived through `ByAddress`, i.e. keyed on the
/// heap address of the `Arc`. Addresses are randomized per process (ASLR /
/// allocator), so address-ordered phi-parameter sorting (`destruct::sort_params`)
/// and `FxHashMap<RcLocal, _>` / `FxHashSet<RcLocal>` iteration order varied
/// run-to-run, permuting the generated local names in the output. Keying on a
/// creation-order id makes decompilation deterministic without changing any
/// semantics: the id is assigned once at construction and copied by `Clone`
/// (clones share the same `Arc`), so id-equality is *exactly* the old
/// pointer-identity equality.
#[derive(Debug, Clone)]
pub struct RcLocal(pub ByAddress<Arc<Mutex<Local>>>, u64);

impl PartialEq for RcLocal {
    fn eq(&self, other: &Self) -> bool {
        self.1 == other.1
    }
}
impl Eq for RcLocal {}

impl PartialOrd for RcLocal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RcLocal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.1.cmp(&other.1)
    }
}

impl Hash for RcLocal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.1.hash(state);
    }
}

impl Default for RcLocal {
    fn default() -> Self {
        Self::new(Local::default())
    }
}

impl Infer for RcLocal {
    fn infer<'a: 'b, 'b>(&'a mut self, system: &mut TypeSystem<'b>) -> Type {
        system.type_of(self).clone()
    }
}

impl Display for RcLocal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0.0.lock().0 {
            Some(name) => write!(f, "{}", name),
            None => {
                let mut hasher = NoHashHasher::<u8>::default();
                self.hash(&mut hasher);
                write!(f, "UNNAMED_{}", hasher.finish())
            }
        }
    }
}

impl SideEffects for RcLocal {}

impl Traverse for RcLocal {}

impl RcLocal {
    pub fn new(mut local: Local) -> Self {
        if let Some(lineage) = &mut local.3 { lineage.copied_local_metadata = true; }
        Self(ByAddress(Arc::new(Mutex::new(local))), next_local_id())
    }

    /// Deterministic identity within one decompilation artifact.
    pub fn stable_id(&self) -> u64 {
        self.1
    }

    pub fn has_source_binding(&self) -> bool {
        !self.0.lock().2.is_empty()
    }

    pub fn preserve_binding(&self) -> bool {
        let local = self.0.lock();
        !local.2.is_empty() || local.4.conditional_result
    }

    pub fn inherit_source_bindings(&self, other: &Self) {
        if self == other { return; }
        let (evidence, lineage, roles) = {
            let local = other.0.lock();
            (local.2.clone(), local.3.clone(), local.4)
        };
        if evidence.is_empty() && lineage.is_none() && roles == BindingRoles::default() { return; }
        let mut local = self.0.lock();
        local.4.parameter |= roles.parameter;
        local.4.conditional_result |= roles.conditional_result;
        local.4.separate_from_parameter |= roles.separate_from_parameter;
        for binding in evidence { local.add_source_binding(binding); }
        if let Some(lineage) = lineage {
            local.3.get_or_insert_with(Default::default).inherit(&lineage);
        }
    }

    /// Enable diagnostic lineage for a newly recorded definition. This does
    /// not allocate another RcLocal or promote its source/capture evidence.
    pub fn record_definition_lineage(&self) {
        self.0.lock().3.get_or_insert_with(Default::default).add(self.stable_id());
    }

    pub fn mark_lineage_incomplete(&self) {
        self.0.lock().3.get_or_insert_with(Default::default).incomplete = true;
    }

    pub fn source_bindings_compatible(&self, other: &Self) -> bool {
        if self == other { return true; }
        // Compare borrowed evidence without allocating or cloning its strings.
        // Lock by stable identity to keep the order consistent across workers.
        let (first, second) = if self < other { (self, other) } else { (other, self) };
        let first = first.0.lock();
        let second = second.0.lock();
        if (first.4.parameter && second.4.separate_from_parameter)
            || (second.4.parameter && first.4.separate_from_parameter) {
            return false;
        }
        // Keep separately recorded locals distinct, even for equal spelling.
        let own = |b: &&SourceBinding| matches!(b.origin, BindingOrigin::DebugLocal { .. });
        let mut left = first.2.iter().filter(own).peekable();
        let mut right = second.2.iter().filter(own).peekable();
        left.peek().is_none() || right.peek().is_none() || left.eq(right)
    }
}

impl LocalRw for RcLocal {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        visit(self)
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        visit(self)
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }
}

#[enum_dispatch]
pub trait LocalRw {
    /// Visit reads in evaluation order, stopping when `visit` returns false.
    /// Closure captures are reads; nested block and closure bodies are separate.
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.values_read().into_iter().all(visit)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.values_read_mut().into_iter().all(visit)
    }

    fn any_local_read(&self, predicate: &mut dyn FnMut(&RcLocal) -> bool) -> bool {
        !self.visit_local_reads(&mut |local| !predicate(local))
    }

    /// Visit definite local-write slots in the same order as `values_written`,
    /// including duplicate destinations. Nested bodies are separate. Returning
    /// false stops the traversal immediately; the default preserves compatibility
    /// with implementations that only override the original vector API.
    fn visit_local_writes<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.values_written().into_iter().all(visit)
    }

    fn visit_local_writes_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.values_written_mut().into_iter().all(visit)
    }

    fn any_local_write(&self, predicate: &mut dyn FnMut(&RcLocal) -> bool) -> bool {
        !self.visit_local_writes(&mut |local| !predicate(local))
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        Vec::new()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        Vec::new()
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        Vec::new()
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        Vec::new()
    }

    fn values(&self) -> Vec<&RcLocal> {
        let mut values = Vec::new();
        self.visit_local_reads(&mut |local| { values.push(local); true });
        self.visit_local_writes(&mut |local| { values.push(local); true });
        values
    }

    fn replace_values_read(&mut self, old: &RcLocal, new: &RcLocal) {
        self.visit_local_reads_mut(&mut |value| {
            if value == old { *value = new.clone(); }
            true
        });
    }

    fn replace_values_written(&mut self, old: &RcLocal, new: &RcLocal) {
        self.visit_local_writes_mut(&mut |value| {
            if value == old {
                *value = new.clone();
            }
            true
        });
    }

    fn replace_values(&mut self, old: &RcLocal, new: &RcLocal) {
        self.replace_values_read(old, new);
        self.replace_values_written(old, new);
    }
}

#[cfg(test)]
mod source_binding_tests {
    use super::*;

    fn binding(start_pc: usize, name: &str) -> SourceBinding {
        SourceBinding { origin: BindingOrigin::DebugLocal {
            prototype: 2, register: 3, start_pc, end_pc: start_pc + 4,
        }, name: name.to_string() }
    }

    #[test]
    fn register_reuse_and_same_spelling_are_distinct_bindings() {
        let a = RcLocal::default();
        let b = RcLocal::default();
        a.0.lock().add_source_binding(binding(0, "value"));
        b.0.lock().add_source_binding(binding(8, "value"));
        assert!(!a.source_bindings_compatible(&b));
        b.inherit_source_bindings(&a);
        assert_eq!(b.0.lock().2.len(), 2);
        // Once joined (storage shared past the local limit), intervals of one
        // spelling still name the local; different spellings do not.
        assert_eq!(b.0.lock().source_name(), Some("value"));
        let c = RcLocal::default();
        c.0.lock().add_source_binding(binding(16, "other"));
        c.inherit_source_bindings(&b);
        assert_eq!(c.0.lock().source_name(), None);
    }

    #[test]
    fn debug_binding_outranks_type_and_capture_hints() {
        let mut local = Local::with_type_hint("number".to_string());
        local.add_source_binding(SourceBinding { origin: BindingOrigin::DebugUpvalue {
            prototype: 8, slot: 0,
        }, name: "capturedAlias".to_string() });
        local.add_source_binding(binding(2, "discountAmount"));
        assert_eq!(local.source_name(), Some("discountAmount"));
        assert_eq!(local.type_hint(), Some("number"));
    }

    #[test]
    fn debug_names_are_validated_without_repairing_the_spelling() {
        for invalid in ["", "for", "123name", "a.b", "a b", "a\0b", "đẹp"] {
            let mut local = Local::default();
            local.add_source_binding(binding(0, invalid));
            assert_eq!(local.source_name(), None, "{invalid:?}");
        }
        for valid in ["self", "_", "UpperCamel", "CONSTANT_NAME", "v1"] {
            assert!(valid_source_name(valid));
        }
    }
}

/// Built-in compatibility collectors share a single growing output buffer.
pub(crate) fn collect_reads(node: &impl LocalRw) -> Vec<&RcLocal> {
    let mut reads = Vec::new();
    node.visit_local_reads(&mut |local| { reads.push(local); true });
    reads
}

pub(crate) fn collect_reads_mut(node: &mut impl LocalRw) -> Vec<&mut RcLocal> {
    let mut reads = Vec::new();
    node.visit_local_reads_mut(&mut |local| { reads.push(local); true });
    reads
}

#[cfg(test)]
mod write_visitor_tests {
    use super::{LocalRw, RcLocal};
    use crate::{Assign, Block, LValue, RValue, Statement};

    // The original vector implementations remain independent of the new write
    // visitors, so they serve as an ordering and mutation oracle here.
    fn assert_conformance<T: LocalRw + Clone + std::fmt::Debug>(node: T) {
        let expected: Vec<_> = node.values_written().into_iter().map(RcLocal::stable_id).collect();
        let expected_values: Vec<_> = node.values_read().into_iter()
            .chain(node.values_written()).map(RcLocal::stable_id).collect();
        assert_eq!(node.values().into_iter().map(RcLocal::stable_id).collect::<Vec<_>>(), expected_values);
        let replacements: Vec<_> = (0..expected.len()).map(|_| RcLocal::default()).collect();
        for limit in 1..=expected.len() + 1 {
            let mut actual = Vec::new();
            let completed = node.visit_local_writes(&mut |local| {
                actual.push(local.stable_id());
                actual.len() < limit
            });
            assert_eq!(actual, expected[..expected.len().min(limit)]);
            assert_eq!(completed, expected.len() < limit);

            let mut reference = node.clone();
            for (slot, replacement) in reference.values_written_mut().into_iter()
                .zip(&replacements).take(limit)
            {
                *slot = replacement.clone();
            }
            let mut actual = node.clone();
            let mut count = 0;
            let completed = actual.visit_local_writes_mut(&mut |slot| {
                assert_eq!(slot.stable_id(), expected[count]);
                *slot = replacements[count].clone();
                count += 1;
                count < limit
            });
            assert_eq!(count, expected.len().min(limit));
            assert_eq!(completed, expected.len() < limit);
            // Some structured nodes intentionally have non-reflexive PartialEq.
            // Semantic Debug also checks the unchanged read slots/nested bodies.
            assert_eq!(format!("{actual:?}"), format!("{reference:?}"));
        }
        let absent = RcLocal::default();
        for id in expected.iter().copied().chain(std::iter::once(absent.stable_id())) {
            let mut visited = 0;
            let found = node.any_local_write(&mut |local| {
                visited += 1;
                local.stable_id() == id
            });
            let first = expected.iter().position(|&candidate| candidate == id);
            assert_eq!(found, first.is_some());
            assert_eq!(visited, first.map_or(expected.len(), |index| index + 1));
        }
    }

    #[test]
    fn write_visitors_match_legacy_slots_stopping_and_mutation() {
        let locals: Vec<_> = (0..7).map(|_| RcLocal::default()).collect();
        let left = vec![
            locals[0].clone().into(),
            crate::Index::new(locals[1].clone().into(), locals[2].clone().into()).into(),
            crate::Global::from("global").into(),
            locals[0].clone().into(), // repeated identity, separate write slot
            locals[3].clone().into(),
        ];
        let assign = Assign::new(left, vec![locals[0].clone().into(), locals[4].clone().into()]);
        for value in &assign.left { assert_conformance(value.clone()); }
        assert_conformance(locals[0].clone()); // An RValue local is a read, not a write.
        let nested: Block = vec![assign.clone().into()].into();
        let mut next = crate::GenericForNext::new(
            vec![locals[0].clone(), locals[0].clone(), locals[3].clone()],
            locals[4].clone().into(), locals[5].clone(), locals[6].clone(),
        );
        next.res_locals.insert(1, crate::Index::new(locals[1].clone().into(), locals[2].clone().into()).into());
        let mut writes = Vec::new();
        next.visit_local_writes(&mut |local| { writes.push(local.stable_id()); true });
        assert_eq!(writes, vec![locals[0].stable_id(), locals[0].stable_id(), locals[3].stable_id()]);
        assert!(!writes.contains(&next.control.stable_id()));
        let statements: Vec<Statement> = vec![
            assign.clone().into(),
            Assign::new(Vec::new(), vec![locals[0].clone().into()]).into(),
            crate::NumForInit::new(locals[0].clone(), locals[0].clone(), locals[3].clone()).into(),
            crate::NumForNext::new(locals[0].clone(), locals[1].clone().into(), locals[2].clone().into()).into(),
            crate::NumericFor::new(locals[1].clone().into(), locals[2].clone().into(),
                locals[3].clone().into(), locals[0].clone(), nested.clone()).into(),
            crate::GenericForInit(assign, None).into(),
            next.into(),
            crate::GenericFor::new(vec![locals[0].clone(), locals[0].clone(), locals[3].clone()],
                vec![locals[4].clone().into()], nested.clone()).into(),
            crate::If::new(locals[0].clone().into(), nested.clone(), nested.clone()).into(),
            crate::While::new(locals[0].clone().into(), nested.clone()).into(),
            crate::Repeat::new(locals[0].clone().into(), nested.clone()).into(),
            crate::Call::new(locals[0].clone().into(), vec![locals[1].clone().into()]).into(),
            crate::MethodCall::new(locals[0].clone().into(), "method".into(), vec![locals[1].clone().into()]).into(),
            crate::Return::new(vec![locals[0].clone().into()]).into(),
            crate::SetList::new(locals[0].clone(), 1, vec![locals[1].clone().into()], Some(locals[2].clone().into())).into(),
            crate::Close { locals: locals.clone() }.into(),
            crate::Empty {}.into(), crate::Break {}.into(), crate::Continue {}.into(),
            crate::Comment::new("comment".into()).into(),
            crate::Label::from("label").into(), crate::Goto::new(crate::Label::from("label")).into(),
        ];
        for statement in statements { assert_conformance(statement); }
        let closure = crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(
                crate::Function { body: nested, ..Default::default() }))),
            upvalues: vec![crate::Upvalue::Copy(locals[0].clone()), crate::Upvalue::Ref(locals[1].clone())],
        };
        assert_conformance(RValue::from(closure));
        assert_conformance(LValue::from(locals[0].clone()));
    }

    #[test]
    fn write_visitor_defaults_support_existing_vector_only_implementations() {
        #[derive(Clone, Debug)]
        struct VectorOnly(Vec<RcLocal>, Vec<RcLocal>);
        impl LocalRw for VectorOnly {
            fn values_read(&self) -> Vec<&RcLocal> { self.0.iter().collect() }
            fn values_read_mut(&mut self) -> Vec<&mut RcLocal> { self.0.iter_mut().collect() }
            fn values_written(&self) -> Vec<&RcLocal> { self.1.iter().collect() }
            fn values_written_mut(&mut self) -> Vec<&mut RcLocal> { self.1.iter_mut().collect() }
        }
        let original = RcLocal::default();
        let other = RcLocal::default();
        let mut value = VectorOnly(vec![original.clone()], vec![original.clone(), other.clone(), original.clone()]);
        assert_conformance(value.clone());
        let replacement = RcLocal::default();
        value.replace_values_written(&original, &replacement);
        assert_eq!(value.0, vec![original]);
        assert_eq!(value.1, vec![replacement.clone(), other, replacement]);
    }
}
