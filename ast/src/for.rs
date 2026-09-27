use crate::{
    Assign, Block, LValue, LocalRw, RValue, RcLocal, SideEffects, Traverse, has_side_effects,
};
use itertools::Itertools;
use parking_lot::Mutex;
use std::fmt;
use triomphe::Arc;

/// The bytecode preparation strategy used by a generic-for loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForPrepKind {
    Generic,
    Next,
    Inext,
}

/// VM semantics profile used to interpret the protocol metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VmProfileId {
    Luau,
}

/// Stable identity for one generic-for protocol pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ForId {
    pub prep_pc: usize,
    pub step_pc: usize,
}

/// Provenance for a compiler-emitted generic-for protocol.
///
/// The pair `(prep_pc, step_pc)` is a stable loop identity.  The remaining
/// fields preserve the exact control-flow and result shape that produced the
/// internal marker, allowing structuring passes to reject ambiguous pairs.
/// Metadata is optional only for compatibility with hand-built AST fixtures;
/// production bytecode lifting always attaches it, and the typed source-like
/// structurer rejects a reachable marker when the identity is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ForOrigin {
    pub prep_pc: usize,
    pub step_pc: usize,
    pub body_pc: usize,
    pub follow_pc: usize,
    pub prep_kind: ForPrepKind,
    pub base_register: u8,
    pub result_count: u8,
    pub aux: u32,
    pub bytecode_version: u8,
    pub vm_profile: VmProfileId,
    /// True when the source supplied an explicit single-result call followed
    /// by nil iterator arguments.  The VM tuple is otherwise padded with
    /// implicit nils for ordinary one-expression generic loops.
    pub explicit_nil_args: bool,
}

impl ForOrigin {
    pub fn id(self) -> ForId {
        ForId {
            prep_pc: self.prep_pc,
            step_pc: self.step_pc,
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct NumForInit {
    // TODO: REFACTOR: store 3 `Assign`s instead
    // TODO: STYLE: rename to `control`? that's what lua calls it
    pub counter: (LValue, RValue),
    pub limit: (LValue, RValue),
    pub step: (LValue, RValue),
}

impl NumForInit {
    pub fn new(counter: RcLocal, limit: RcLocal, step: RcLocal) -> Self {
        Self {
            counter: (LValue::Local(counter.clone()), RValue::Local(counter)),
            limit: (LValue::Local(limit.clone()), RValue::Local(limit)),
            step: (LValue::Local(step.clone()), RValue::Local(step)),
        }
    }
}

// NumForInit checks if counter, limit and step are numbers
// this can result in an error, so it has side effects.
has_side_effects!(NumForInit);

impl Traverse for NumForInit {
    fn visit_lvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::LValue) -> bool) -> bool {
        visit(&self.counter.0) && visit(&self.limit.0) && visit(&self.step.0)
    }

    fn visit_lvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::LValue) -> bool) -> bool {
        visit(&mut self.counter.0) && visit(&mut self.limit.0) && visit(&mut self.step.0)
    }

    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        visit(&self.counter.1) && visit(&self.limit.1) && visit(&self.step.1)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        visit(&mut self.counter.1) && visit(&mut self.limit.1) && visit(&mut self.step.1)
    }

    fn lvalues(&self) -> Vec<&LValue> {
        vec![&self.counter.0, &self.limit.0, &self.step.0]
    }

    fn lvalues_mut(&mut self) -> Vec<&mut LValue> {
        vec![&mut self.counter.0, &mut self.limit.0, &mut self.step.0]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.counter.1, &self.limit.1, &self.step.1]
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.counter.1, &mut self.limit.1, &mut self.step.1]
    }
}

impl LocalRw for NumForInit {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.counter.1.visit_local_reads(visit)
            && self.limit.1.visit_local_reads(visit)
            && self.step.1.visit_local_reads(visit)
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.counter.1.visit_local_reads_mut(visit)
            && self.limit.1.visit_local_reads_mut(visit)
            && self.step.1.visit_local_reads_mut(visit)
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        self.counter
            .0
            .values_written()
            .into_iter()
            .chain(self.limit.0.values_written())
            .chain(self.step.0.values_written().into_iter())
            .collect()
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        self.counter
            .0
            .values_written_mut()
            .into_iter()
            .chain(self.limit.0.values_written_mut())
            .chain(self.step.0.values_written_mut().into_iter())
            .collect()
    }
}

impl fmt::Display for NumForInit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "-- NumForInit\nlocal {}, {}, {} = {}, {}, {}\n-- end NumForInit",
            self.counter.0, self.limit.0, self.step.0, self.counter.1, self.limit.1, self.step.1
        )
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct NumForNext {
    // TODO: REFACTOR: store an `Assign` and an `If` instead?
    // TODO: REFACTOR: this is the worst s$H##()WT ever literally
    // TODO: STYLE: rename to `control`? that's what lua calls it
    pub counter: (LValue, RValue), // RcLocal, // cant be of type RcLocal because Traverse
    pub limit: RValue,
    pub step: RValue,
}

// NumForNext can error if the types of counter, limit and step are wrong
has_side_effects!(NumForNext);

impl NumForNext {
    pub fn new(counter: RcLocal, limit: RValue, step: RValue) -> Self {
        Self {
            counter: (LValue::Local(counter.clone()), RValue::Local(counter)),
            limit,
            step,
        }
    }
}

impl Traverse for NumForNext {
    fn visit_lvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::LValue) -> bool) -> bool {
        visit(&self.counter.0)
    }

    fn visit_lvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::LValue) -> bool) -> bool {
        visit(&mut self.counter.0)
    }

    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        visit(&self.counter.1) && visit(&self.step) && visit(&self.limit)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        visit(&mut self.counter.1) && visit(&mut self.step) && visit(&mut self.limit)
    }

    fn lvalues(&self) -> Vec<&LValue> {
        vec![&self.counter.0]
    }

    fn lvalues_mut(&mut self) -> Vec<&mut LValue> {
        vec![&mut self.counter.0]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.counter.1, &self.step, &self.limit]
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.counter.1, &mut self.step, &mut self.limit]
    }
}

impl LocalRw for NumForNext {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.counter.1.visit_local_reads(visit)
            && self.step.visit_local_reads(visit)
            && self.limit.visit_local_reads(visit)
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.counter.1.visit_local_reads_mut(visit)
            && self.step.visit_local_reads_mut(visit)
            && self.limit.visit_local_reads_mut(visit)
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        self.counter.0.values_written()
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        self.counter.0.values_written_mut()
    }
}

impl fmt::Display for NumForNext {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "-- NumForNext\n{} = {} + {};\nif {} <= {}\n-- end NumForNext",
            self.counter.0, self.counter.1, self.step, self.counter.0, self.limit
        )
    }
}

// TODO: STYLE: this should probably be named "NumFor"
#[derive(Debug, Clone)]
pub struct NumericFor {
    pub initial: RValue,
    pub limit: RValue,
    pub step: RValue,
    // TODO: STYLE: rename to `control`? (thats what lua calls it)
    pub counter: RcLocal,
    pub block: Arc<Mutex<Block>>,
}

impl PartialEq for NumericFor {
    fn eq(&self, _other: &Self) -> bool {
        // TODO: compare block
        false
    }
}

has_side_effects!(NumericFor);

impl NumericFor {
    pub fn new(
        initial: RValue,
        limit: RValue,
        step: RValue,
        counter: RcLocal,
        block: Block,
    ) -> Self {
        Self {
            initial,
            limit,
            step,
            counter,
            block: Arc::new(block.into()),
        }
    }
}

impl LocalRw for NumericFor {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.initial.visit_local_reads(visit)
            && self.limit.visit_local_reads(visit)
            && self.step.visit_local_reads(visit)
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.initial.visit_local_reads_mut(visit)
            && self.limit.visit_local_reads_mut(visit)
            && self.step.visit_local_reads_mut(visit)
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        vec![&self.counter]
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        vec![&mut self.counter]
    }
}

impl Traverse for NumericFor {
    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        visit(&self.initial) && visit(&self.limit) && visit(&self.step)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        visit(&mut self.initial) && visit(&mut self.limit) && visit(&mut self.step)
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.initial, &self.limit, &self.step]
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.initial, &mut self.limit, &mut self.step]
    }
}

impl fmt::Display for NumericFor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "for {} = {}, {}, {} do\n{}\nend",
            self.counter,
            self.initial,
            self.limit,
            self.step,
            self.block
                .lock()
                .iter()
                .map(|n| n.to_string().replace('\n', "\n\t"))
                .join("\n\t")
        )
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct GenericForInit(pub Assign, pub Option<ForOrigin>);

impl GenericForInit {
    pub fn new(generator: RcLocal, state: RcLocal, initial_control: RcLocal) -> Self {
        Self(
            Assign::new(
                vec![
                    generator.clone().into(),
                    state.clone().into(),
                    initial_control.clone().into(),
                ],
                vec![generator.into(), state.into(), initial_control.into()],
            ),
            None,
        )
    }

    pub fn new_with_origin(
        generator: RcLocal,
        state: RcLocal,
        initial_control: RcLocal,
        origin: ForOrigin,
    ) -> Self {
        let mut init = Self::new(generator, state, initial_control);
        init.1 = Some(origin);
        init
    }

    pub fn origin(&self) -> Option<ForOrigin> {
        self.1
    }
}

impl SideEffects for GenericForInit {
    fn has_side_effects(&self) -> bool {
        self.0.has_side_effects()
    }
}

impl Traverse for GenericForInit {
    fn visit_lvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::LValue) -> bool) -> bool {
        self.0.visit_lvalues(visit)
    }

    fn visit_lvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::LValue) -> bool) -> bool {
        self.0.visit_lvalues_mut(visit)
    }

    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        self.0.visit_rvalues(visit)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        self.0.visit_rvalues_mut(visit)
    }

    fn lvalues(&self) -> Vec<&LValue> {
        self.0.lvalues()
    }

    fn lvalues_mut(&mut self) -> Vec<&mut LValue> {
        self.0.lvalues_mut()
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        self.0.rvalues_mut()
    }

    fn rvalues(&self) -> Vec<&RValue> {
        self.0.rvalues()
    }
}

impl LocalRw for GenericForInit {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.0.visit_local_reads(visit)
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.0.visit_local_reads_mut(visit)
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        self.0.values_written()
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        self.0.values_written_mut()
    }
}

impl fmt::Display for GenericForInit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "-- GenericForInit\n{}\n[internal control] = {}\n-- end GenericForInit",
            self.0, self.0.left[2]
        )
    }
}

// TODO: STYLE: i think GenericFor is a bad name, lua calls iterators "generators",
// so maybe uh GenerativeFor? LOL
// or GenFor?
#[derive(Debug, PartialEq, Clone)]
pub struct GenericForNext {
    // TODO: REFACTOR: store an `Assign` with a `Call` and an `If` instead?
    pub res_locals: Vec<LValue>,
    pub generator: RValue,
    pub state: RValue,
    /// The hidden iterator control value (the third register in Luau's
    /// generic-for protocol).  It used to be implicit in this AST node, which
    /// made it impossible for a CFG fallback to lower a residual FORGLOOP
    /// without guessing which local carries the control value.
    pub control: RcLocal,
    pub origin: Option<ForOrigin>,
}

impl GenericForNext {
    pub fn new(
        res_locals: Vec<RcLocal>,
        generator: RValue,
        state: RcLocal,
        control: RcLocal,
    ) -> Self {
        assert!(!res_locals.is_empty());
        Self {
            res_locals: res_locals.into_iter().map(LValue::Local).collect(),
            generator,
            state: RValue::Local(state),
            control,
            origin: None,
        }
    }

    pub fn new_with_origin(
        res_locals: Vec<RcLocal>,
        generator: RValue,
        state: RcLocal,
        control: RcLocal,
        origin: ForOrigin,
    ) -> Self {
        let mut next = Self::new(res_locals, generator, state, control);
        next.origin = Some(origin);
        next
    }

    pub fn origin(&self) -> Option<ForOrigin> {
        self.origin
    }
}

// GenericForNext can error
has_side_effects!(GenericForNext);

impl Traverse for GenericForNext {
    fn visit_lvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::LValue) -> bool) -> bool {
        self.res_locals.iter().all(visit)
    }

    fn visit_lvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::LValue) -> bool) -> bool {
        self.res_locals.iter_mut().all(visit)
    }

    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        visit(&self.generator) && visit(&self.state)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        visit(&mut self.generator) && visit(&mut self.state)
    }

    fn lvalues(&self) -> Vec<&LValue> {
        self.res_locals.iter().collect()
    }

    fn lvalues_mut(&mut self) -> Vec<&mut LValue> {
        self.res_locals.iter_mut().collect()
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        // `control` is a hidden register represented as an RcLocal rather than
        // an expression.  It is still exposed through `LocalRw::values_read`
        // below so SSA/local-renaming passes cannot miss it.
        vec![&mut self.generator, &mut self.state]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.generator, &self.state]
    }
}

impl LocalRw for GenericForNext {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.generator.visit_local_reads(visit)
            && self.state.visit_local_reads(visit)
            && visit(&self.control)
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.generator.visit_local_reads_mut(visit)
            && self.state.visit_local_reads_mut(visit)
            && visit(&mut self.control)
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        // `control` is updated only on the Then/non-nil edge.  LocalRw has no
        // edge-sensitive write representation, and its callers treat this
        // list as definite block writes, so including `control` here would be
        // unsound on the exhaustion edge.  The CFG fallback materializes the
        // conditional `control = first_result` explicitly; the source-shaped
        // structurer separately validates that the hidden protocol local is
        // not observable outside the marker pair.
        self.res_locals
            .iter()
            .flat_map(|l| l.values_written())
            .collect()
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        self.res_locals
            .iter_mut()
            .flat_map(|l| l.values_written_mut())
            .collect()
    }
}

impl fmt::Display for GenericForNext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "-- GenericForNext\n{} = {}({}, {})\nif {} ~= nil\n{} = {}\n-- end GenericForNext",
            self.res_locals.iter().join(", "),
            self.generator,
            self.state,
            self.control,
            self.res_locals[0],
            self.control,
            self.res_locals[0],
        )
    }
}

#[derive(Debug, Clone)]
pub struct GenericFor {
    pub res_locals: Vec<RcLocal>,
    pub right: Vec<RValue>,
    pub block: Arc<Mutex<Block>>,
    pub origin: Option<ForOrigin>,
}

impl PartialEq for GenericFor {
    fn eq(&self, _other: &Self) -> bool {
        // TODO: compare block
        false
    }
}

impl GenericFor {
    pub fn new(res_locals: Vec<RcLocal>, right: Vec<RValue>, block: Block) -> Self {
        Self {
            res_locals,
            right,
            block: Arc::new(block.into()),
            origin: None,
        }
    }

    pub fn new_with_origin(
        res_locals: Vec<RcLocal>,
        right: Vec<RValue>,
        block: Block,
        origin: ForOrigin,
    ) -> Self {
        let mut generic_for = Self::new(res_locals, right, block);
        generic_for.origin = Some(origin);
        generic_for
    }
}

has_side_effects!(GenericFor);

impl LocalRw for GenericFor {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.right.iter().all(|value| value.visit_local_reads(visit))
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.right.iter_mut().all(|value| value.visit_local_reads_mut(visit))
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        self.res_locals.iter().collect()
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        self.res_locals.iter_mut().collect()
    }
}

impl Traverse for GenericFor {
    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        self.right.iter().all(visit)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        self.right.iter_mut().all(visit)
    }

    fn rvalues(&self) -> Vec<&RValue> {
        self.right.iter().collect()
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        self.right.iter_mut().collect()
    }
}

impl fmt::Display for GenericFor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "for {} in {} do\n{}\nend",
            self.res_locals.iter().join(", "),
            self.right.iter().join(", "),
            self.block
                .lock()
                .iter()
                .map(|n| n.to_string().replace('\n', "\n\t"))
                .join("\n\t")
        )
    }
}
