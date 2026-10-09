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

/// A DUPTABLE template field. The VM loader creates every template key with the
/// value `0` before any store runs (older lifts modelled it as `nil`), so the
/// entry evaluates nothing and a store that follows may take its slot.
pub fn is_template_placeholder(value: &RValue) -> bool {
    matches!(value, RValue::Literal(Literal::Nil | Literal::Number(0.0)))
}

/// Whether a store to `key` may fold into the constructor entry `listed =
/// value` for its slot, keeping the listed key. Keys naming one slot by
/// value can still differ under `pairs` (`[0]`, `[-0]`): a slot holding a
/// value keeps the key it was created with, but a `nil` slot may be taken
/// over by a later insertion or dropped by a rehash, and the store then
/// creates the key it spells. Which one happens depends on the hash layout.
pub fn keeps_listed_key(listed: Option<&RValue>, value: &RValue, key: &RValue) -> bool {
    matches!((listed, key), (Some(RValue::Literal(listed)), RValue::Literal(key)) if listed == key)
        || !matches!(value, RValue::Literal(Literal::Nil))
}

/// A constructor entry whose value evaluates nothing: a template placeholder or
/// a constant the template carries. A later store may take its slot, and a
/// value may be moved ahead of it, without reordering any evaluation.
pub fn is_inert_entry_value(value: &RValue) -> bool {
    matches!(value, RValue::Literal(_))
}

/// Literal keys a constructor already lists. A later store to one of them
/// mutates a finished table; folding it would print the key twice, which is
/// never how a table is written. Hashes keep the check O(1) on long field runs,
/// and a hit is confirmed against the entries.
#[derive(Default)]
pub struct ListedKeys(rustc_hash::FxHashSet<u64>);

impl ListedKeys {
    pub fn new(table: &Table) -> Self {
        Self(table.0.iter().filter_map(|(key, _)| key.as_ref().and_then(literal_key_hash)).collect())
    }

    pub fn lists(&self, table: &Table, key: &RValue) -> bool {
        literal_key_hash(key).is_some_and(|hash| {
            self.0.contains(&hash)
                && table.0.iter().any(|(listed, _)| listed.as_ref().is_some_and(|listed| crate::same_table_key(listed, key)))
        })
    }

    pub fn add(&mut self, key: &RValue) {
        if let Some(hash) = literal_key_hash(key) {
            self.0.insert(hash);
        }
    }
}

fn literal_key_hash(key: &RValue) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    match key {
        RValue::Literal(Literal::String(bytes)) => (0u8, bytes).hash(&mut hasher),
        // `t[0]` and `t[-0]` are the same slot.
        RValue::Literal(Literal::Number(number)) => {
            (1u8, if *number == 0.0 { 0 } else { number.to_bits() }).hash(&mut hasher)
        }
        _ => return None,
    }
    Some(hasher.finish())
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
            inferred_calls: Default::default(),
        }
        .format_table(self)
    }
}
