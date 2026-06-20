use crate::{type_system::Infer, SideEffects, Traverse, Type, TypeSystem};
use by_address::ByAddress;
use derive_more::From;
use enum_dispatch::enum_dispatch;
use nohash_hasher::NoHashHasher;
use parking_lot::Mutex;
use std::{
    fmt::{self, Display},
    hash::{Hash, Hasher},
};
use triomphe::Arc;

#[derive(Debug, Default, From, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct Local(pub Option<String>);

impl Local {
    pub fn new(name: Option<String>) -> Self {
        Self(name)
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
        match &self.0 .0.lock().0 {
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
    pub fn new(local: Local) -> Self {
        Self(ByAddress(Arc::new(Mutex::new(local))), next_local_id())
    }
}

impl LocalRw for RcLocal {
    fn values_read(&self) -> Vec<&RcLocal> {
        vec![self]
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        vec![self]
    }
}

#[enum_dispatch]
pub trait LocalRw {
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
        self.values_read()
            .into_iter()
            .chain(self.values_written())
            .collect()
    }

    fn replace_values_read(&mut self, old: &RcLocal, new: &RcLocal) {
        for value in self.values_read_mut() {
            if value == old {
                *value = new.clone();
            }
        }
    }

    fn replace_values_written(&mut self, old: &RcLocal, new: &RcLocal) {
        for value in self.values_written_mut() {
            if value == old {
                *value = new.clone();
            }
        }
    }

    fn replace_values(&mut self, old: &RcLocal, new: &RcLocal) {
        self.replace_values_read(old, new);
        self.replace_values_written(old, new);
    }
}
