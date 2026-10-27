use crate::{
    Literal, LocalRw, RValue, RcLocal, Reduce, SideEffects, Traverse, formatter::Formatter,
};

use std::{fmt, iter};

// shortcut: bound extra allocation syntax to 4096 entries; extend with large-allocation controls.
pub(crate) const MAX_ALLOCATION_PADDING: usize = 4096;

#[derive(Clone, PartialEq, Default)]
pub struct Table(pub Vec<(Option<RValue>, RValue)>, pub crate::node_origins::Origin, pub TableShape);

impl Table {
    pub fn new(fields: Vec<(Option<RValue>, RValue)>) -> Self { Self(fields, Default::default(), Default::default()) }

    /// An explicit key that addresses the originally allocated array part.
    pub fn array_index(&self, key: &RValue) -> Option<usize> {
        if let TableShape::NewTable { array, .. } = self.2
            && let RValue::Literal(Literal::Number(index)) = key
            && *index >= 1.0 && *index <= f64::from(array) && index.fract() == 0.0
        { Some(*index as usize) } else { None }
    }

    /// Extra keyed syntax needed to preserve the original hash allocation.
    /// None as the key means array slot 1, which already exists and is nil.
    pub(crate) fn hash_padding(&self) -> Result<(usize, Option<&RValue>), &'static str> {
        let TableShape::NewTable { hash, array } = self.2 else { return Ok((0, None)); };
        // shortcut: preserve legacy empty-table output across compiler allocation
        // prediction; upgrade when empty hash hints have an exact source encoding.
        if hash == 0 || self.0.is_empty() { return Ok((0, None)); }
        let listed = self.0.iter().filter(|(key, _)| key.is_some()).count();
        if listed > hash as usize {
            return Err("constructor fields exceed the original NEWTABLE hash allocation");
        }
        let padding = (hash as usize / 2 + 1).saturating_sub(listed);
        if padding == 0 { return Ok((0, None)); }
        if padding <= MAX_ALLOCATION_PADDING {
            if array > 0 { return Ok((padding, None)); }
            if let Some((Some(key), _)) = self.0.first()
                && (matches!(key, RValue::Literal(Literal::String(_) | Literal::Boolean(_)))
                    || matches!(key, RValue::Literal(Literal::Number(number)) if !number.is_nan()))
            {
                return Ok((padding, Some(key)));
            }
        }
        Err("a NEWTABLE hash allocation without enough reproducible constructor fields")
    }
}

/// How the bytecode built a table, which decides the order `next` and
/// `pairs` visit its keys in: the size of its hash part, and whether a store
/// of `nil` leaves a key in it. A DUPTABLE copies a template whose keys all
/// exist before any store; in a NEWTABLE of hash size `hash`, a node whose
/// value was stored `nil` can be reused by a later colliding insertion.
/// A table grown past its hash part rehashes, which moves its keys. Layout
/// is observable, so unlike source locations it participates in AST equality.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TableShape {
    #[default]
    Unknown,
    NewTable { hash: u32, array: u32 },
    Template { keys: u32 },
}

impl TableShape {
    /// The nodes Luau gives a hash part sized for `keys` keys.
    pub fn nodes(keys: u32) -> u32 {
        if keys == 0 { 0 } else { keys.next_power_of_two() }
    }
}

/// Whether a store adding a key may fold into `table`'s constructor without
/// changing the order `pairs` visits its keys in. A NEWTABLE must keep its
/// initial hash capacity, including zero; folding stores past it erases growth
/// and rehash steps. A DUPTABLE's hash part has
/// nodes for its template keys rounded up to a power of two; a store adding a
/// key past that rehashes the table, and a constructor printing the key would
/// be a larger template, laid out otherwise: `{a = 1, b = 2}; t.c = 3` is not
/// `{a = 1, b = 2, c = 3}` under `pairs` (fr-t1110 F2).
pub fn table_takes_another_key(table: &Table, key: &RValue) -> bool {
    let capacity = match table.2 {
        TableShape::Unknown => return true,
        TableShape::NewTable { hash, .. } => {
            if let Some(index) = table.array_index(key) {
                let slots = table.0.iter().filter(|(key, _)| key.as_ref().is_none_or(|key| table.array_index(key).is_some())).count();
                let has_list = table.0.iter().any(|(key, _)| key.is_none());
                return index == slots + 1 && (!has_list
                    || table.0.iter().filter(|(key, _)| key.is_some()).count() < hash as usize);
            }
            hash
        }
        TableShape::Template { keys } => {
            // A larger template pre-seeds the added key before any nil store.
            if table.0.iter().any(|(_, value)| !crate::return_types::is_proven_non_nil(value)) {
                return false;
            }
            TableShape::nodes(keys)
        }
    };
    // Mixed constructors cannot lower explicit array keys to list items:
    // doing so shifts the later list stores. Luau counts those keys as hash fields.
    let has_list = table.0.iter().any(|(key, _)| key.is_none());
    let listed = table.0.iter().filter(|(key, _)| key.as_ref()
        .is_some_and(|key| has_list || table.array_index(key).is_none())).count();
    u32::try_from(listed + 1).is_ok_and(|after| after <= capacity)
}

/// Replacing an earlier NEWTABLE value must not change whether its node can
/// be reused by a later insertion. Templates already have every listed key.
pub fn table_replaces_entry(table: &Table, position: usize, value: &RValue) -> bool {
    !matches!(table.2, TableShape::NewTable { .. })
        || position + 1 == table.0.len()
        || (crate::return_types::is_proven_non_nil(&table.0[position].1)
            && crate::return_types::is_proven_non_nil(value))
}

/// A nil NEWTABLE store can still create a key/node; dropping it before later
/// insertions changes collision chains even though it contributes no live value.
pub fn table_moves_entry_to_end(table: &Table, position: usize) -> bool {
    match table.2 {
        TableShape::Unknown => true,
        TableShape::NewTable { .. } => position + 1 == table.0.len(),
        TableShape::Template { .. } => false,
    }
}

/// After field unification, the site's values also prove the helper's values.
/// A NEWTABLE can match a template only if allocation and collision placement
/// agree; matching keys and values alone is insufficient.
pub fn compatible_table_layouts(pattern: &Table, site: &Table) -> bool {
    match (pattern.2, site.2) {
        (TableShape::Unknown, TableShape::Unknown) => true,
        (TableShape::NewTable { hash: a, array: x }, TableShape::NewTable { hash: b, array: y }) => a == b && x == y,
        (TableShape::Template { keys: a }, TableShape::Template { keys: b }) => {
            TableShape::nodes(a) == TableShape::nodes(b)
        }
        (TableShape::NewTable { hash, array: 0 }, TableShape::Template { keys })
        | (TableShape::Template { keys }, TableShape::NewTable { hash, array: 0 }) => {
            hash == TableShape::nodes(keys)
                && site.0.iter().all(|(key, _)| matches!(key, Some(RValue::Literal(Literal::String(_)))))
                && site.0.iter().take(site.0.len().saturating_sub(1))
                    .all(|(_, value)| crate::return_types::is_proven_non_nil(value))
        }
        _ => false,
    }
}

impl fmt::Debug for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.debug_tuple("Table").field(&self.0).finish() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(shape: TableShape, values: Vec<RValue>) -> Table {
        Table(values.into_iter().enumerate().map(|(index, value)| {
            (Some(Literal::String(vec![b'a' + index as u8]).into()), value)
        }).collect(), Default::default(), shape)
    }

    fn number() -> RValue { Literal::Number(1.0).into() }

    #[test]
    fn array_slots_are_separate_from_hash_capacity() {
        let mut t = table(TableShape::NewTable { hash: 2, array: 3 }, vec![number()]);
        assert!(table_takes_another_key(&t, &number()));
        assert!(!table_takes_another_key(&t, &Literal::Number(2.0).into()));
        assert!(table_takes_another_key(&t, &RcLocal::default().into()));
        t.0.push((Some(number()), Literal::Nil.into()));
        assert!(table_takes_another_key(&t, &Literal::Number(2.0).into()));
        assert!(table_takes_another_key(&t, &Literal::String(b"b".to_vec()).into()));
        let mut other = t.clone();
        other.2 = TableShape::NewTable { hash: 2, array: 4 };
        assert!(!compatible_table_layouts(&t, &other));
    }

    #[test]
    fn mixed_list_fields_keep_explicit_array_keys_in_the_hash_census() {
        let mut t = Table(vec![
            (Some(number()), number()),
            (Some(Literal::Boolean(true).into()), number()),
            (None, number()),
        ], Default::default(), TableShape::NewTable { hash: 2, array: 1 });
        assert!(!table_takes_another_key(&t, &Literal::String(b"extra".to_vec()).into()));
        t.0.pop();
        assert!(table_takes_another_key(&t, &Literal::String(b"extra".to_vec()).into()));

        let mut t = Table(vec![(None, number()), (Some(Literal::Boolean(true).into()), number())],
            Default::default(), TableShape::NewTable { hash: 1, array: 3 });
        assert!(!table_takes_another_key(&t, &Literal::Number(2.0).into()));
        assert!(!table_takes_another_key(&t, &number()));
        t.2 = TableShape::NewTable { hash: 2, array: 3 };
        assert!(table_takes_another_key(&t, &Literal::Number(2.0).into()));
        assert!(!table_takes_another_key(&t, &Literal::Number(3.0).into()));
    }

    #[test]
    fn empty_hash_hint_keeps_the_selected_compatibility_policy() {
        let t = table(TableShape::NewTable { hash: 8, array: 0 }, vec![]);
        assert_eq!(t.hash_padding().unwrap().0, 0);
        assert_eq!(t.to_string(), "{}");
    }

    #[test]
    fn append_preserves_capacity_and_template_nil_nodes() {
        assert!(!table_takes_another_key(&table(TableShape::NewTable { hash: 0, array: 0 }, vec![]), &Literal::String(b"next".to_vec()).into()));
        assert!(table_takes_another_key(&table(TableShape::NewTable { hash: 2, array: 0 }, vec![number()]), &Literal::String(b"next".to_vec()).into()));
        assert!(!table_takes_another_key(&table(TableShape::NewTable { hash: 2, array: 0 }, vec![number(), number()]), &Literal::String(b"next".to_vec()).into()));
        assert!(table_takes_another_key(&table(TableShape::Template { keys: 3 }, vec![number(), number(), number()]), &Literal::String(b"next".to_vec()).into()));
        assert!(!table_takes_another_key(&table(TableShape::Template { keys: 3 }, vec![number(), number(), RcLocal::default().into()]), &Literal::String(b"next".to_vec()).into()));
    }

    #[test]
    fn replacement_and_movement_do_not_change_nil_collision_history() {
        let mut t = table(TableShape::NewTable { hash: 2, array: 0 }, vec![Literal::Nil.into(), number()]);
        assert!(!table_replaces_entry(&t, 0, &number()));
        assert!(!table_moves_entry_to_end(&t, 0));
        assert!(table_replaces_entry(&t, 1, &Literal::Nil.into()));
        t.0[0].1 = number();
        assert!(table_replaces_entry(&t, 0, &number()));
        assert!(!table_replaces_entry(&t, 0, &Literal::Nil.into()));
        t.2 = TableShape::Template { keys: 2 };
        assert!(table_replaces_entry(&t, 0, &Literal::Nil.into()));
        assert!(!table_moves_entry_to_end(&t, 1));
    }

    #[test]
    fn equality_and_helper_matching_keep_allocation_semantics() {
        let a = table(TableShape::NewTable { hash: 2, array: 0 }, vec![number(), Literal::Nil.into()]);
        let mut b = a.clone();
        b.2 = TableShape::Template { keys: 2 };
        assert_ne!(a, b);
        assert!(compatible_table_layouts(&a, &b));
        b.0[0].1 = RcLocal::default().into();
        assert!(!compatible_table_layouts(&a, &b));
        b.2 = TableShape::NewTable { hash: 4, array: 0 };
        assert!(!compatible_table_layouts(&a, &b));
    }
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
