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
        // Evaluate every value before writing any slot. The explicit count is
        // essential: nil results must overwrite existing entries, and a tail
        // callback must still observe the table before the fixed-value stores.
        if let Some(tail) = &self.tail {
            let object_name = self.object_local.to_string();
            let values_name = if object_name == "_values" {
                "_values2"
            } else {
                "_values"
            };
            let key_name = if object_name == "_k" { "_k2" } else { "_k" };
            let mut arguments = self.values.clone();
            arguments.push(tail.clone());
            write!(
                f,
                "do local {values_name} = table.pack({}); for {key_name} = 1, {values_name}.n do {}[",
                formatter::format_arg_list(&arguments),
                self.object_local
            )?;
            if self.index > 1 {
                write!(f, "{} + ", self.index - 1)?;
            }
            return write!(f, "{key_name}] = {values_name}[{key_name}] end end");
        }
        if !self.values.is_empty() {
            for i in 0..self.values.len() {
                if i != 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}[{}]", self.object_local, self.index + i)?;
            }
            write!(f, " = {}", formatter::format_arg_list(&self.values))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_fallback_names_do_not_shadow_target_or_table_builtin() {
        for name in ["_values", "_k"] {
            let target = RcLocal::new(crate::Local::new(Some(name.into())));
            let list = SetList::new(target, 2, vec![], Some(crate::VarArg {}.into()));
            let output = list.to_string();
            assert!(!output.contains(&format!("local {name} =")), "{output}");
            assert!(!output.contains(&format!("for {name} =")), "{output}");
        }
        let target = RcLocal::new(crate::Local::new(Some("table".into())));
        let mut declaration = crate::Assign::new(vec![target.clone().into()], vec![
            crate::Table::default().into(),
        ]);
        declaration.prefix = true;
        let mut block = crate::Block(vec![
            declaration.into(),
            SetList::new(target.clone(), 1, vec![], Some(crate::VarArg {}.into())).into(),
        ]);
        crate::name_locals::name_locals(&mut block, true);
        assert_ne!(target.to_string(), "table");
        assert!(block.to_string().contains("table.pack(...)"));
    }
}
