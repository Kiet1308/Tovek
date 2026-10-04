//! Preserve whole-request deduplication while CPU admission runs in quanta.
//! The core owns context equivalence; this layer retains only ordinal maps and
//! bounded immutable output text, releasing each result after its last use.
use crate::{BatchInput, BatchResultItem, ParsedItem, MAX_RESPONSE_LEN};
use axum::body::Bytes;

struct Saved {
    source: Option<Bytes>,
    error: Option<String>,
}
impl Saved {
    fn bytes(&self) -> usize {
        self.source.as_ref().map_or(0, Bytes::len) + self.error.as_ref().map_or(0, String::len)
    }
    fn apply(&self, row: &mut BatchResultItem) {
        row.ok = self.source.is_some();
        row.decompilation = self.source.clone();
        row.error = self.error.clone();
    }
}

pub(super) struct Plan {
    slots: Vec<Option<usize>>,
    remaining: Vec<usize>,
    saved: Vec<Option<Saved>>,
    retained: usize,
    maximum: usize,
}

impl Plan {
    pub fn new(items: &[ParsedItem]) -> Self {
        Self::with_policy(items, luau_lifter::requires_fresh_decompilation(), MAX_RESPONSE_LEN)
    }

    fn with_policy(items: &[ParsedItem], fresh: bool, maximum: usize) -> Self {
        let mut slots = vec![None; items.len()];
        let remaining = if fresh || items.len() < 2 { Vec::new() } else {
            let mut indices = Vec::new();
            let inputs = items.iter().enumerate().filter_map(|(index, item)| {
                match item {
                    ParsedItem::Ready { bytecode, key, script_name, .. } => {
                        indices.push(index);
                        Some(BatchInput { bytecode, encode_key: *key, script_name: script_name.as_deref() })
                    }
                    ParsedItem::Failed { .. } => None,
                }
            }).collect::<Vec<_>>();
            // Options and semantic environment are constant across one parsed
            // request, exactly as in the core batch API.
            let (_, ready_slots, remaining) = luau_lifter::batch_layout(&inputs);
            for (index, slot) in indices.into_iter().zip(ready_slots) { slots[index] = Some(slot); }
            remaining
        };
        let saved = (0..remaining.len()).map(|_| None).collect();
        Self { slots, remaining, saved, retained: 0, maximum }
    }

    pub fn slot(&self, index: usize) -> Option<usize> { self.slots[index] }

    pub fn apply(&self, row: &mut BatchResultItem) -> bool {
        let Some(saved) = self.slot(row.index).and_then(|slot| self.saved[slot].as_ref()) else { return false; };
        saved.apply(row);
        true
    }

    pub fn finish_row(&mut self, row: &mut BatchResultItem, reusable: bool) {
        let Some(slot) = self.slot(row.index) else { return; };
        if self.saved[slot].is_none() && self.remaining[slot] > 1 && reusable {
            let saved = Saved { source: row.decompilation.clone(), error: row.error.clone() };
            if saved.bytes() <= self.maximum.saturating_sub(self.retained) {
                self.retained += saved.bytes();
                self.saved[slot] = Some(saved);
            }
            // Exhausting an optimization's budget must never turn a valid
            // output into an error. A later duplicate can recompute instead;
            // only the response writer decides whether its JSON will fit.
        }
        self.remaining[slot] -= 1;
        if self.remaining[slot] == 0 {
            if let Some(saved) = self.saved[slot].take() { self.retained -= saved.bytes(); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DecompileOptions;

    fn item(name: &str) -> ParsedItem {
        ParsedItem::Ready { bytecode: Bytes::from_static(b"code"), key: 203,
            options: DecompileOptions::default(), id: None, script_name: Some(name.into()) }
    }
    fn row(index: usize, source: &str) -> BatchResultItem {
        BatchResultItem { index, id: Some(index.to_string()), script_name: Some(format!("path{index}.Widget")),
            ok: true, decompilation: Some(Bytes::copy_from_slice(source.as_bytes())), error: None }
    }

    #[test]
    fn normalized_contexts_share_ordinals_but_diagnostics_execute_every_item() {
        let inputs = vec![item("one.Widget"), item("two.Widget"), item("Other")];
        let mut plan = Plan::with_policy(&inputs, false, MAX_RESPONSE_LEN);
        assert_eq!(plan.slot(0), plan.slot(1));
        assert_ne!(plan.slot(0), plan.slot(2));
        let mut first = row(0, "return 7");
        plan.finish_row(&mut first, true);
        assert_eq!(plan.retained, 8);
        let mut second = row(1, "unused");
        assert!(plan.apply(&mut second));
        assert_eq!(second.id.as_deref(), Some("1"));
        assert_eq!(second.script_name.as_deref(), Some("path1.Widget"));
        assert_eq!(second.decompilation.as_deref(), Some(&b"return 7"[..]));
        plan.finish_row(&mut second, true);
        assert_eq!(plan.retained, 0);
        let fresh = Plan::with_policy(&inputs, true, MAX_RESPONSE_LEN);
        assert!(fresh.slots.iter().all(Option::is_none));
    }

    #[test]
    fn memo_exhaustion_preserves_output_and_allows_later_recomputation() {
        let inputs = vec![item("Widget"), item("Other"), item("Widget"), item("Other")];
        let maximum = 32;
        let mut plan = Plan::with_policy(&inputs, false, maximum);
        // Escaped source may be rejected by the response writer despite
        // fitting the memo, while a later ordinary source still fits output.
        let mut first = row(0, &"\n".repeat(32));
        plan.finish_row(&mut first, true);
        assert_eq!(plan.retained, maximum);
        let mut second = row(1, "return 7");
        plan.finish_row(&mut second, true);
        assert!(second.ok);
        assert_eq!(second.decompilation.as_deref(), Some(&b"return 7"[..]));
        assert_eq!(plan.retained, maximum);
        let mut third = row(2, "unused");
        assert!(plan.apply(&mut third));
        plan.finish_row(&mut third, true);
        assert_eq!(plan.retained, 0);
        let mut fourth = row(3, "return 7");
        assert!(!plan.apply(&mut fourth), "memo exhaustion must allow recomputation");
        plan.finish_row(&mut fourth, true);
        assert!(fourth.ok);
        assert_eq!(plan.retained, 0);
    }

    #[test]
    fn transient_failures_are_not_reused() {
        let inputs = vec![item("Widget"), item("Widget")];
        let mut plan = Plan::with_policy(&inputs, false, MAX_RESPONSE_LEN);
        let mut failed = row(0, "");
        failed.ok = false; failed.decompilation = None; failed.error = Some("CPU admission timed out".into());
        plan.finish_row(&mut failed, false);
        assert!(!plan.apply(&mut row(1, "unused")));
        assert_eq!(plan.retained, 0);
    }
}
