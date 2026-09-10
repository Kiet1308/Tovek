#![feature(let_chains)]

mod fallback;
mod region;

/// Build a semantics-preserving state-machine AST when no source-shaped
/// region proof is available and the caller permits synthetic control.
pub use fallback::lift as lift_fallback;
pub use fallback::lift_with_ignored_locals as lift_fallback_with_ignored_locals;
pub use fallback::{
    CertifiedFallback, SyntheticLocal, SyntheticRole,
    lift_certified_with_ignored_locals as lift_certified_fallback_with_ignored_locals,
};
/// Attempt a conservative, source-shaped structuring pass.
///
/// This compatibility wrapper discards the distinction between `Unsupported`
/// and `Unsafe`; callers must not use its `None` result to select a weaker
/// matcher.  Prefer [`lift_source_like_attempt_with_ignored_locals`] for all
/// routing decisions.
#[deprecated(note = "use lift_source_like_attempt_with_ignored_locals; this API erases unsafe rejection reasons")]
pub use region::lift as lift_source_like;
/// Compatibility name for callers that explicitly accept the loss of a typed
/// rejection reason.  Production structurer selection must use the typed API.
pub use region::lift as lift_source_like_discarding_rejection_reason;
#[deprecated(note = "use lift_source_like_attempt_with_ignored_locals; this API erases unsafe rejection reasons")]
pub use region::lift_with_ignored_locals as lift_source_like_with_ignored_locals;
/// Explicit compatibility name for the untyped protected-locals wrapper.
pub use region::lift_with_ignored_locals as lift_source_like_discarding_rejection_reason_with_ignored_locals;
pub use region::{
    StructureAttempt, UnsafeStructureReason,
    lift_attempt_with_ignored_locals as lift_source_like_attempt_with_ignored_locals,
};
