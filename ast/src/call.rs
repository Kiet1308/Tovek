use std::fmt;

use crate::{LocalRw, RcLocal, Traverse, formatter::Formatter, has_side_effects};

use super::RValue;

#[derive(Clone)]
pub struct Call {
    pub node_origin: crate::node_origins::Origin,
    pub value: Box<RValue>,
    pub arguments: Vec<RValue>,
    /// Creation event only. Copies retain the event; a newly built Call starts
    /// unattributed. Zero means no retained diagnostic record, not original code.
    pub reconstruction_event: u32,
    /// Lifted from a `FASTCALL` (builtin) or v14 `FASTPCALL` fallback: the
    /// compiler evaluated every argument first and fetched the callee last.
    /// Source `table.insert(t, f(x))` compiles back to exactly that order, so
    /// a pass may treat an importable callee (`tostring`, `table.insert`) as
    /// read after its arguments. Not part of equality; rebuilt calls start
    /// without it.
    pub callee_after_arguments: bool,
    /// Rebuilt by a pass, and which kind: a de-inliner's equivalent call
    /// inferred from a copy Luau inlined (the uses that named its arguments
    /// now sit in the callee), or a call to a synthesized helper. The
    /// formatter prints a de-inlined call's site comment and its helper's
    /// call count from this attribute, and `--stats-json` counts calls by
    /// kind from it. Not part of equality.
    pub rebuilt: Option<crate::call_origins::Kind>,
    /// A rebuilt call of a helper that returns exactly one value on every
    /// path: where all of a call's results are taken it still gives one,
    /// so `("...%*"):format(x, f())` prints as a backtick string. Not part
    /// of equality.
    pub one_result: bool,
}

impl PartialEq for Call {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.arguments == other.arguments
    }
}
impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Preserve semantic debug fingerprints used by existing diagnostics.
        f.debug_struct("Call").field("value", &self.value).field("arguments", &self.arguments).finish()
    }
}

impl Call {
    pub fn new(value: RValue, arguments: Vec<RValue>) -> Self {
        Self {
            node_origin: Default::default(),
            value: Box::new(value),
            arguments,
            reconstruction_event: 0,
            callee_after_arguments: false,
            rebuilt: None,
            one_result: false,
        }
    }

    /// This call with its callee and arguments rewritten by a pass (lowered,
    /// copied): still the same call, so it keeps every attribute, `rebuilt`
    /// above all (its site comment, its helper's count and `--stats-json` all
    /// read it). A pass rewriting a call's parts builds the result here, never
    /// through [`Call::new`], which starts a new, unattributed call.
    pub fn with_parts(&self, value: RValue, arguments: Vec<RValue>) -> Self {
        Self {
            node_origin: self.node_origin.clone(),
            value: Box::new(value),
            arguments,
            reconstruction_event: self.reconstruction_event,
            callee_after_arguments: self.callee_after_arguments,
            rebuilt: self.rebuilt,
            one_result: self.one_result,
        }
    }

    pub(crate) fn reconstructed(mut self, producer: crate::call_origins::Kind) -> Self {
        self.rebuilt = Some(producer);
        if crate::call_origins::enabled() {
            self.node_origin = crate::node_origins::Origin::synthesized(match producer {
            crate::call_origins::Kind::StatementDeinline => "statement_deinline",
            crate::call_origins::Kind::ExpressionDeinline => "expression_deinline",
            crate::call_origins::Kind::ArithmeticDeinline => "arithmetic_deinline",
            crate::call_origins::Kind::TerminalSynthesis => "terminal_synthesis",
            });
        }
        if let RValue::Local(local) = &*self.value {
            self.reconstruction_event = crate::call_origins::record(producer, local.stable_id());
        }
        self
    }

    /// An equivalent call a de-inliner inferred: what the formatter marks
    /// with a site comment and counts on its helper's definition line.
    pub fn is_inferred(&self) -> bool {
        self.rebuilt.is_some_and(crate::call_origins::Kind::is_inference)
    }
}

// call can error
has_side_effects!(Call);
// impl SideEffects for Call {
//     fn has_side_effects(&self) -> bool {
//         matches!(self.value, box RValue::Local(_))
//             || self.value.has_side_effects()
//             || self.arguments.iter().any(|arg| arg.has_side_effects())
//     }
// }

impl Traverse for Call {
    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        visit(&self.value) && self.arguments.iter().all(visit)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        visit(&mut self.value) && self.arguments.iter_mut().all(visit)
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        std::iter::once(self.value.as_mut())
            .chain(self.arguments.iter_mut())
            .collect()
    }

    fn rvalues(&self) -> Vec<&RValue> {
        std::iter::once(self.value.as_ref())
            .chain(self.arguments.iter())
            .collect()
    }
}

impl LocalRw for Call {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.value.visit_local_reads(visit)
            && self.arguments.iter().all(|value| value.visit_local_reads(visit))
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.value.visit_local_reads_mut(visit)
            && self.arguments.iter_mut().all(|value| value.visit_local_reads_mut(visit))
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }
}

impl fmt::Display for Call {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Default::default(),
            position_query: None,
            closure_observer: None,
            emission_map: None,
            layout_budget: None,
            compact_annotations: false,
            inferred_calls: Default::default(),
        }
        .format_call(self)
    }
}

#[derive(Clone, PartialEq)]
pub struct MethodCall {
    pub node_origin: crate::node_origins::Origin,
    // TODO: STYLE: rename to object?
    pub value: Box<RValue>,
    pub method: String,
    pub arguments: Vec<RValue>,
}

impl MethodCall {
    pub fn new(value: RValue, method: String, arguments: Vec<RValue>) -> Self {
        Self {
            node_origin: Default::default(),
            value: Box::new(value),
            method,
            arguments,
        }
    }
}

// this should reflect Index
has_side_effects!(MethodCall);

impl Traverse for MethodCall {
    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        visit(&self.value) && self.arguments.iter().all(visit)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        visit(&mut self.value) && self.arguments.iter_mut().all(visit)
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        std::iter::once(self.value.as_mut())
            .chain(self.arguments.iter_mut())
            .collect()
    }

    fn rvalues(&self) -> Vec<&RValue> {
        std::iter::once(self.value.as_ref())
            .chain(self.arguments.iter())
            .collect()
    }
}

impl LocalRw for MethodCall {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.value.visit_local_reads(visit)
            && self.arguments.iter().all(|value| value.visit_local_reads(visit))
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.value.visit_local_reads_mut(visit)
            && self.arguments.iter_mut().all(|value| value.visit_local_reads_mut(visit))
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }
}

impl fmt::Display for MethodCall {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Default::default(),
            position_query: None,
            closure_observer: None,
            emission_map: None,
            layout_budget: None,
            compact_annotations: false,
            inferred_calls: Default::default(),
        }
        .format_method_call(self)
    }
}

crate::node_origins::semantic_debug!(MethodCall; value,method,arguments);
