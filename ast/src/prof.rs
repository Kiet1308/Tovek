//! Env-gated (`MEDAL_PROF`) phase counters in nanoseconds, shared by every
//! crate of the pipeline. Disabled counters cost one cached flag check.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
pub fn on() -> bool {
    *ENABLED.get_or_init(|| std::env::var("MEDAL_PROF").is_ok())
}

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        $(pub static $name: AtomicU64 = AtomicU64::new(0);)*
        /// Accumulated nanoseconds per phase since the last reset.
        pub fn snapshot() -> Vec<(&'static str, u64)> {
            vec![$((stringify!($name), $name.load(Ordering::Relaxed)),)*]
        }
        pub fn reset() {
            $($name.store(0, Ordering::Relaxed);)*
        }
    };
}
counters!(
    DESER_LIFT,
    PAR_LOOP_WALL,
    F_SSA_CONSTRUCT,
    F_SIMPLE_FAST,
    F_STRUCTURE_JUMPS,
    F_SSA_INLINE,
    F_STRUCTURE_CONDS,
    F_REMOVE_PARAMS,
    F_APPLY_MAP,
    F_DESTRUCT,
    F_RESTRUCTURE,
    F_SIMPLIFY_GOTOS,
    F_FLATTEN_GUARDS,
    F_DECLARE_LOCALS,
    F_HOIST,
    S_LINK_UPVALUES,
    S_DEINLINE,
    S_FACTOR_INITIAL,
    S_FACTOR_FIXEDPOINT,
    S_CLEANUP_RETURNS,
    S_MATERIALIZE,
    S_REROLL_ARITHMETIC,
    S_REHOIST_CONSTANTS,
    S_NAME_LOCALS,
    S_RECOVER_METHODS,
    S_INLINE_TEMPS_1,
    S_COND_EXPRS,
    S_REBUILD_TABLES,
    S_MATERIALIZE_CALL_RECEIVERS,
    S_COPY_CLEANUP,
    S_REBALANCE_EXPRS,
    S_CLEANUP_FINAL,
    S_ELIMINATE_NIL,
    S_RECOVER_CONN,
    S_EXPR_DEINLINE,
    S_NORMALIZE_CONDS,
    S_GUARD_CONTINUE,
    S_FORMAT,
    TOTAL,
    SETUP,
    F_PRESERVE,
    F_FACTOR_TAILS,
    F_COALESCE,
    F_CHECKS,
    S_ARITH_DEINLINE,
    S_SYNTH_HELPERS,
    S_BRANCH_CONSTRUCTORS,
    S_LOWER_SELECTS,
    S_LATE,
    S_REFINE_NAMES,
    F_TOTAL,
    I_CENSUS,
    I_INLINE,
    I_DEAD,
    I_TABLES,
    C_RENAME,
    C_APPLY_MAP,
    C_MARK_UPVALUES,
    C_PROPAGATE,
    C_REMOVE_PARAMS,
    C_SETUP,
);

pub struct Timer(Option<(Instant, &'static AtomicU64)>);
impl Timer {
    pub fn new(c: &'static AtomicU64) -> Self {
        Timer(on().then(|| (Instant::now(), c)))
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        if let Some((s, c)) = self.0.take() {
            c.fetch_add(s.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
    }
}
