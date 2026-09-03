use crate::{formatter, LocalRw, RValue, RcLocal, SideEffects, Traverse};

#[derive(Debug, Clone, PartialEq)]
pub struct SetList {
    pub object_local: RcLocal,
    pub index: usize,
    pub values: Vec<RValue>,
    pub tail: Option<RValue>,
}

impl SetList {
    pub fn new(
        object_local: RcLocal,
        index: usize,
        values: Vec<RValue>,
        tail: Option<RValue>,
    ) -> Self {
        Self {
            object_local,
            index,
            values,
            tail,
        }
    }
}

impl LocalRw for SetList {
    fn values_read(&self) -> Vec<&RcLocal> {
        let tail_locals = self
            .tail
            .as_ref()
            .map(|t| t.values_read())
            .unwrap_or_default();
        std::iter::once(&self.object_local)
            .chain(self.values.iter().flat_map(|rvalue| rvalue.values_read()))
            .chain(tail_locals)
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        let tail_locals = self
            .tail
            .as_mut()
            .map(|t| t.values_read_mut())
            .unwrap_or_default();
        std::iter::once(&mut self.object_local)
            .chain(
                self.values
                    .iter_mut()
                    .flat_map(|rvalue| rvalue.values_read_mut()),
            )
            .chain(tail_locals)
            .collect()
    }
}

impl SideEffects for SetList {
    fn has_side_effects(&self) -> bool {
        self.values
            .iter()
            .chain(self.tail.as_ref())
            .any(|rvalue| rvalue.has_side_effects())
    }
}

impl Traverse for SetList {
    fn rvalues(&self) -> Vec<&RValue> {
        self.values.iter().chain(self.tail.as_ref()).collect()
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        self.values.iter_mut().chain(self.tail.as_mut()).collect()
    }
}

impl std::fmt::Display for SetList {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        // A SETLIST that couldn't be folded back into a table constructor.
        // Lower it to plain, valid index assignments:
        //   obj[i], obj[i + 1], ... = v0, v1, ...
        // A multret tail (`f()` / `...`) must keep every value it produces, so
        // it is stored through a packing constructor instead of being
        // truncated to one value by the multiple assignment:
        //   for _k, _v in next, { f() } do obj[i + n - 1 + _k] = _v end
        // (`next` visits every non-nil packed value with its index, which is
        // exactly what SETLIST stores; a nil is a no-op on a fresh slot.)
        if !self.values.is_empty() {
            for i in 0..self.values.len() {
                if i != 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}[{}]", self.object_local, self.index + i)?;
            }
            write!(f, " = {}", formatter::format_arg_list(&self.values))?;
        }
        if let Some(tail) = &self.tail {
            if !self.values.is_empty() {
                write!(f, "; ")?;
            }
            let base = self.index + self.values.len() - 1;
            write!(f, "for _k, _v in next, {{ {} }} do {}[", tail, self.object_local)?;
            if base == 0 {
                write!(f, "_k")?;
            } else {
                write!(f, "{} + _k", base)?;
            }
            write!(f, "] = _v end")?;
        }
        Ok(())
    }
}
