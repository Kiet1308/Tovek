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
        }
    }

    pub(crate) fn reconstructed(mut self, producer: crate::call_origins::Kind) -> Self {
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
        }
        .format_method_call(self)
    }
}

crate::node_origins::semantic_debug!(MethodCall; value,method,arguments);
