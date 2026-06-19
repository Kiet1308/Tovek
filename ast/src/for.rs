use crate::{
    has_side_effects, Assign, Block, LValue, LocalRw, RValue, RcLocal, SideEffects, Traverse,
};
use itertools::Itertools;
use parking_lot::Mutex;
use std::fmt;
use triomphe::Arc;

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
    fn values_read(&self) -> Vec<&RcLocal> {
        self.counter
            .1
            .values_read()
            .into_iter()
            .chain(self.limit.1.values_read())
            .chain(self.step.1.values_read().into_iter())
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.counter
            .1
            .values_read_mut()
            .into_iter()
            .chain(self.limit.1.values_read_mut())
            .chain(self.step.1.values_read_mut().into_iter())
            .collect()
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
    fn values_read(&self) -> Vec<&RcLocal> {
        self.counter
            .1
            .values_read()
            .into_iter()
            .chain(self.step.values_read().into_iter())
            .chain(self.limit.values_read())
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.counter
            .1
            .values_read_mut()
            .into_iter()
            .chain(self.step.values_read_mut().into_iter())
            .chain(self.limit.values_read_mut())
            .collect()
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
    fn values_read(&self) -> Vec<&RcLocal> {
        self.initial
            .values_read()
            .into_iter()
            .chain(self.limit.values_read())
            .chain(self.step.values_read())
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.initial
            .values_read_mut()
            .into_iter()
            .chain(self.limit.values_read_mut())
            .chain(self.step.values_read_mut())
            .collect()
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        vec![&self.counter]
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        vec![&mut self.counter]
    }
}

impl Traverse for NumericFor {
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
pub struct GenericForInit(pub Assign);

impl GenericForInit {
    pub fn new(generator: RcLocal, state: RcLocal, initial_control: RcLocal) -> Self {
        Self(Assign::new(
            vec![
                generator.clone().into(),
                state.clone().into(),
                initial_control.clone().into(),
            ],
            vec![generator.into(), state.into(), initial_control.into()],
        ))
    }
}

impl SideEffects for GenericForInit {
    fn has_side_effects(&self) -> bool {
        self.0.has_side_effects()
    }
}

impl Traverse for GenericForInit {
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
    fn values_read(&self) -> Vec<&RcLocal> {
        self.0.values_read()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.0.values_read_mut()
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
}

impl GenericForNext {
    pub fn new(res_locals: Vec<RcLocal>, generator: RValue, state: RcLocal) -> Self {
        assert!(!res_locals.is_empty());
        Self {
            res_locals: res_locals.into_iter().map(LValue::Local).collect(),
            generator,
            state: RValue::Local(state),
        }
    }
}

// GenericForNext can error
has_side_effects!(GenericForNext);

impl Traverse for GenericForNext {
    fn lvalues_mut(&mut self) -> Vec<&mut LValue> {
        self.res_locals.iter_mut().collect()
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.generator, &mut self.state]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.generator, &self.state]
    }
}

impl LocalRw for GenericForNext {
    fn values_read(&self) -> Vec<&RcLocal> {
        self.generator
            .values_read()
            .into_iter()
            .chain(self.state.values_read().into_iter())
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.generator
            .values_read_mut()
            .into_iter()
            .chain(self.state.values_read_mut().into_iter())
            .collect()
    }

    fn values_written(&self) -> Vec<&RcLocal> {
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
            "-- GenericForNext\n{} = {}({}, [internal control])\nif {} ~= nil\n[internal control] = {}\n-- end GenericForNext",
            self.res_locals.iter().join(", "),
            self.generator,
            self.state,
            self.res_locals[0],
            self.res_locals[0],
        )
    }
}

#[derive(Debug, Clone)]
pub struct GenericFor {
    pub res_locals: Vec<RcLocal>,
    pub right: Vec<RValue>,
    pub block: Arc<Mutex<Block>>,
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
        }
    }
}

has_side_effects!(GenericFor);

impl LocalRw for GenericFor {
    fn values_read(&self) -> Vec<&RcLocal> {
        self.right.iter().flat_map(|r| r.values_read()).collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.right
            .iter_mut()
            .flat_map(|r| r.values_read_mut())
            .collect()
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        self.res_locals.iter().collect()
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        self.res_locals.iter_mut().collect()
    }
}

impl Traverse for GenericFor {
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
