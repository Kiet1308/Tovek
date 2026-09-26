//! Explicit local introductions by emitter passes, separate from input ancestry.
//! Numeric identity is serialized as the same bN key used by the emission map.
//! Records contain no RcLocal owners and confer no source/effect/lifetime proof.
use serde::Serialize;

use crate::RcLocal;

pub const RECORD_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    ScalarSelectResult,
    ShortCircuitResult,
    EvaluationSnapshot,
    ConstructorPropertyValue,
    ConstructorInitializerSnapshot,
    VectorConstructor,
}

#[derive(Clone, Debug, Serialize)]
pub struct Record {
    pub binding_id: String,
    pub role: Role,
}

#[derive(Clone, Default, Debug, Serialize)]
pub struct Ledger {
    pub records: Vec<Record>,
    pub omitted_records: usize,
}

#[derive(Debug, Serialize)]
pub struct Pass {
    pub pass: &'static str,
    pub rewrite_model: &'static str,
    pub introduced_locals: usize,
    #[serde(flatten)]
    pub ledger: Ledger,
}

impl Ledger {
    pub fn record(&mut self, local: &RcLocal, role: Role) {
        if self.records.len() == RECORD_LIMIT {
            self.omitted_records += 1;
        } else {
            self.records.push(Record { binding_id: format!("b{}", local.stable_id()), role });
        }
    }

    /// Commit only a successful rewrite attempt. A discarded attempt's ledger
    /// must not be appended, even though it consumed monotonically assigned IDs.
    pub fn append(&mut self, other: Self) {
        let available = RECORD_LIMIT.saturating_sub(self.records.len());
        self.omitted_records += other.omitted_records + other.records.len().saturating_sub(available);
        self.records.extend(other.records.into_iter().take(available));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_records_do_not_copy_source_lineage_or_keep_local_owners() {
        let local = RcLocal::default();
        let owners = triomphe::Arc::strong_count(&local.0.0);
        let mut ledger = Ledger::default();
        for _ in 0..RECORD_LIMIT + 2 { ledger.record(&local, Role::EvaluationSnapshot); }
        assert_eq!(ledger.records.len(), RECORD_LIMIT);
        assert_eq!(ledger.omitted_records, 2);
        let mut merged = Ledger::default();
        merged.record(&local, Role::ScalarSelectResult);
        merged.append(ledger);
        assert_eq!(merged.records.len(), RECORD_LIMIT);
        assert_eq!(merged.omitted_records, 3);
        assert_eq!(triomphe::Arc::strong_count(&local.0.0), owners);
        assert_eq!(merged.records[0].binding_id, format!("b{}", local.stable_id()));
        assert!(!local.has_source_binding());
        assert!(local.0.lock().3.is_none());
    }
}
