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
    /// Lifted from a v14 `FASTPCALL` fallback: the compiler evaluated every
    /// argument first and fetched the importable `pcall`/`xpcall` global last
    /// (it emits FASTPCALL only for a global it may import, never for a local
    /// or an assigned `pcall`). Source `pcall(f, ...)` compiles back to exactly
    /// that order, so a pass may treat this call's global callee as read after
    /// its arguments. Not part of equality; rebuilt calls start without it.
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
    fn values_read(&self) -> Vec<&RcLocal> {
        self.value
            .values_read()
            .into_iter()
            .chain(self.arguments.iter().flat_map(|r| r.values_read()))
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.value
            .values_read_mut()
            .into_iter()
            .chain(self.arguments.iter_mut().flat_map(|r| r.values_read_mut()))
            .collect()
    }
}

impl fmt::Display for Call {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Vec::new(),
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
    fn values_read(&self) -> Vec<&RcLocal> {
        self.value
            .values_read()
            .into_iter()
            .chain(self.arguments.iter().flat_map(|r| r.values_read()))
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.value
            .values_read_mut()
            .into_iter()
            .chain(self.arguments.iter_mut().flat_map(|r| r.values_read_mut()))
            .collect()
    }
}

impl fmt::Display for MethodCall {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Vec::new(),
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
