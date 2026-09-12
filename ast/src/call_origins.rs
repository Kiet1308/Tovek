//! Bounded producer events for reconstructed calls. Diagnostic only: IDs do not
//! participate in AST equality, naming, ownership, matching or semantic proofs.
use std::{cell::RefCell, collections::BTreeMap, marker::PhantomData, rc::Rc};
use serde::Serialize;

pub const EVENT_LIMIT: usize = 4096;
pub const CALLEE_LIMIT: usize = 50_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind { StatementDeinline, ExpressionDeinline, ArithmeticDeinline, TerminalSynthesis }

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub event_id: u32,
    pub producer: Kind,
    pub callee_binding_at_creation: String,
    pub callee_prototype: Option<usize>,
    /// Equivalent output structure, never a certificate of an original call.
    pub evidence: &'static str,
}

#[derive(Default, Debug, Serialize)]
pub struct Report {
    pub events: Vec<Event>,
    pub omitted_events: usize,
    pub omitted_callee_registrations: usize,
}

#[derive(Default)]
struct State {
    report: Report,
    callees: BTreeMap<u64, Option<usize>>,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

/// Restore the calling worker on success, nested calls and panic recovery.
pub struct Scope(Option<State>, PhantomData<Rc<()>>);
pub fn enter(enabled: bool) -> Scope {
    Scope(STATE.with(|s| s.replace(enabled.then(State::default))), PhantomData)
}
impl Scope {
    pub fn take_report(self) -> Report {
        STATE.with(|s| s.borrow_mut().as_mut().map(|s| std::mem::take(&mut s.report)).unwrap_or_default())
    }
}
impl Drop for Scope {
    fn drop(&mut self) { STATE.with(|s| { s.replace(self.0.take()); }); }
}

pub(crate) fn register_callee(binding: u64, prototype: Option<usize>) {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let Some(state) = state.as_mut() else { return; };
        if let Some(old) = state.callees.get_mut(&binding) {
            if *old != prototype { *old = None; }
        } else if state.callees.len() < CALLEE_LIMIT {
            state.callees.insert(binding, prototype);
        } else {
            state.report.omitted_callee_registrations += 1;
        }
    });
}

pub(crate) fn record(producer: Kind, binding: u64) -> u32 {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        let Some(state) = state.as_mut() else { return 0; };
        if state.report.events.len() == EVENT_LIMIT {
            state.report.omitted_events += 1;
            return 0;
        }
        let event_id = state.report.events.len() as u32 + 1;
        state.report.events.push(Event {
            event_id, producer, callee_binding_at_creation: format!("b{binding}"),
            callee_prototype: if producer == Kind::TerminalSynthesis { None }
                else { state.callees.get(&binding).copied().flatten() },
            evidence: if producer == Kind::TerminalSynthesis { "synthesis" } else { "equivalent_call_inference" },
        });
        event_id
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Call, Local, RcLocal, Statement};
    #[test]
    fn budget_and_nested_scope_do_not_leak_into_later_files() {
        let scope = enter(true);
        register_callee(7, Some(3));
        assert_eq!(record(Kind::StatementDeinline, 7), 1);
        {
            let _off = enter(false);
            assert_eq!(record(Kind::StatementDeinline, 7), 0);
        }
        for _ in 1..EVENT_LIMIT { record(Kind::ExpressionDeinline, 7); }
        assert_eq!(record(Kind::ExpressionDeinline, 7), 0);
        let report = scope.take_report();
        assert_eq!(report.events[0].callee_prototype, Some(3));
        assert_eq!(report.events.len(), EVENT_LIMIT);
        assert_eq!(report.omitted_events, 1);
        assert_eq!(record(Kind::StatementDeinline, 7), 0);
    }

    #[test]
    fn diagnostics_do_not_change_equality_debug_text_or_local_ownership() {
        let scope = enter(true);
        let callee = RcLocal::new(Local::new(Some("helper".into())));
        let ordinary = Call::new(callee.clone().into(), vec![]);
        let reconstructed = ordinary.clone().reconstructed(Kind::ExpressionDeinline);
        assert_ne!(ordinary.reconstruction_event, reconstructed.reconstruction_event);
        assert_eq!(ordinary, reconstructed);
        assert_eq!(format!("{ordinary:?}"), format!("{reconstructed:?}"));
        assert_eq!(ordinary.to_string(), reconstructed.to_string());
        let owners = triomphe::Arc::strong_count(&callee.0.0);
        let _report = scope.take_report();
        assert_eq!(triomphe::Arc::strong_count(&callee.0.0), owners);
    }

    #[test]
    fn deep_copies_keep_creation_id_and_emission_counts_actual_occurrences() {
        let scope = enter(true);
        let callee = RcLocal::new(Local::new(Some("helper".into())));
        register_callee(callee.stable_id(), Some(4));
        let call = Call::new(callee.clone().into(), vec![]).reconstructed(Kind::StatementDeinline);
        let id = call.reconstruction_event;
        let body = Block(vec![Statement::Call(call)]);
        let mut copy = crate::simplify_gotos::deep_clone_block(&body);
        copy.0.extend(body.0);
        let (source, _, map) = crate::formatter::format_with_emission_map(&copy, Default::default(), true).unwrap();
        assert_eq!(map.reconstructed_calls.len(), 2);
        for item in map.reconstructed_calls {
            assert_eq!(item.event_id, id);
            assert_eq!(item.current_callee_binding, Some(callee.stable_id()));
            assert_eq!(&source[item.span.start.byte_offset..item.span.end.byte_offset], "helper()");
        }
        let report = scope.take_report();
        assert_eq!(report.events.len(), 1);
        assert_eq!(report.events[0].callee_prototype, Some(4));
    }

    #[test]
    fn compact_display_preserves_full_annotation_and_unrecognized_comments() {
        let body = Block(vec![
            crate::Comment::new("inlined by Luau -O2 (UNHOOKABLE)".into()).into(),
            crate::Comment::new("retain this diagnostic verbatim".into()).into(),
        ]);
        let (full, _, _) = crate::formatter::format_with_emission_map(&body, Default::default(), true).unwrap();
        let (compact, _, map) = crate::formatter::format_with_emission_map_options(&body, Default::default(), true, true).unwrap();
        assert!(full.contains("UNHOOKABLE"));
        assert!(!compact.contains("UNHOOKABLE"));
        assert!(compact.contains("-- inferred call"));
        assert!(compact.contains("-- retain this diagnostic verbatim"));
        assert_eq!(map.annotations[0].text, "inlined by Luau -O2 (UNHOOKABLE)");
        assert_eq!(map.annotations[0].displayed_text.as_deref(), Some("inferred call"));
        let span = map.annotations[0].span;
        assert_eq!(&compact[span.start.byte_offset..span.end.byte_offset], "-- inferred call");
    }

    #[test]
    fn compact_display_keeps_text_when_metadata_cannot_retain_it() {
        let text = format!("[DEDUP] synthesized from {}", "x".repeat(crate::emission_map::ANNOTATION_BYTE_LIMIT));
        let body = Block(vec![crate::Comment::new(text.clone()).into()]);
        let (source, _, map) = crate::formatter::format_with_emission_map_options(&body, Default::default(), true, true).unwrap();
        assert!(source.contains(&text));
        assert!(map.annotations[0].truncated);
        assert!(map.annotations[0].displayed_text.is_none());
        let (without_map, _, _) = crate::formatter::format_with_emission_map_options(&body, Default::default(), false, true).unwrap();
        assert_eq!(source, without_map);
    }
}
