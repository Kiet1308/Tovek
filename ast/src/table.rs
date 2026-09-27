use crate::{
    Literal, LocalRw, RValue, RcLocal, Reduce, SideEffects, Traverse, formatter::Formatter,
};

use std::{fmt, iter};

#[derive(Clone, PartialEq, Default)]
pub struct Table(pub Vec<(Option<RValue>, RValue)>, pub crate::node_origins::Origin);

impl Table {
    pub fn new(fields: Vec<(Option<RValue>, RValue)>) -> Self { Self(fields, Default::default()) }
}
impl fmt::Debug for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.debug_tuple("Table").field(&self.0).finish() }
}

impl Reduce for Table {
    fn reduce(self) -> RValue {
        self.into()
    }

    fn reduce_condition(self) -> RValue {
        let table: RValue = self.into();
        if crate::is_total_pure(&table) {
            Literal::Boolean(true).into()
        } else {
            // TODO: remove all members w/o side effects
            table
        }
    }
}

/*impl Infer for Table {
    fn infer<'a: 'b, 'b>(&'a mut self, system: &mut TypeSystem<'b>) -> Type {
        let elements: BTreeSet<_> = self
            .0
            .iter_mut()
            .map(|(f, v)| (f.clone(), v.infer(system)))
            .collect();
        let elements: BTreeSet<_> = elements
            .iter()
            .filter(|(f, t)| {
                f.is_some() || !elements.iter().any(|(_, x)| t != x && t.is_subtype_of(x))
            })
            .cloned()
            .collect();
        let (elements, fields): (BTreeSet<_>, BTreeMap<_, _>) =
            elements.into_iter().partition_map(|(f, t)| match f {
                None => Either::Left(t),
                Some(f) => Either::Right((f, t)),
            });

        Type::Table {
            indexer: Box::new((
                Type::Any,
                if elements.len() > 1 {
                    Type::Union(elements)
                } else {
                    elements.into_iter().next().unwrap_or(Type::Any)
                },
            )),
            fields,
        }
    }
}*/

impl LocalRw for Table {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.0.iter().all(|(key, value)| {
            key.as_ref().is_none_or(|key| key.visit_local_reads(visit))
                && value.visit_local_reads(visit)
        })
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.0.iter_mut().all(|(key, value)| {
            key.as_mut().is_none_or(|key| key.visit_local_reads_mut(visit))
                && value.visit_local_reads_mut(visit)
        })
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }
}

impl Traverse for Table {
    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        self.0.iter().all(|(key, value)| {
            key.as_ref().is_none_or(|key| visit(key)) && visit(value)
        })
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        self.0.iter_mut().all(|(key, value)| {
            key.as_mut().is_none_or(|key| visit(key)) && visit(value)
        })
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        self.0
            .iter_mut()
            .flat_map(|(k, v)| k.iter_mut().chain(iter::once(v)))
            .collect()
    }

    fn rvalues(&self) -> Vec<&RValue> {
        self.0
            .iter()
            .flat_map(|(k, v)| k.iter().chain(iter::once(v)))
            .collect()
    }
}

impl SideEffects for Table {
    fn has_side_effects(&self) -> bool {
        self.0
            .iter()
            .flat_map(|(k, v)| k.iter().chain(iter::once(v)))
            .any(|r| r.has_side_effects())
    }
}

/*impl fmt::Display for Table {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{{{}}}",
            self.0
                .iter()
                .map(|(key, value)| match key {
                    Some(key) => format!("{} = {}", key, value),
                    None => value.to_string(),
                })
                .join(", ")
        )
    }
}*/

impl fmt::Display for Table {
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
        .format_table(self)
    }
}
