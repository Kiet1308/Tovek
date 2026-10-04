//! A block's SSA definitions and ordered eligible use sites, without AST owners.
//!
//! This is an analysis view, not a second source AST. `RcLocal::stable_id` is
//! the value identity; captures remain ordinary parent operand uses and child
//! function bodies are excluded by `visit_local_reads`. Consumers may empty a
//! definition or replace expression operands, but must rebuild after statement
//! positions change. Stale emptied definitions are conservative search bounds.

use ast::{LocalRw, RcLocal, Statement};
use rustc_hash::FxHashMap;

#[derive(Default)]
pub(super) struct Definitions(FxHashMap<u64, usize>);

impl Definitions {
    fn record(&mut self, statement: &Statement, index: usize) {
        if let Statement::Assign(assign) = statement
            && assign.right.len() == 1
        {
            for local in assign.left.iter().filter_map(|left| left.as_local()) {
                // Preserve the first definition, including hand-built non-SSA
                // blocks and parallel result packs with repeated destinations.
                self.0.entry(local.stable_id()).or_insert(index);
            }
        }
    }

    #[cfg(test)]
    pub fn new(block: &ast::Block) -> Self {
        let mut result = Self::default();
        for (index, statement) in block.iter().enumerate() { result.record(statement, index); }
        result
    }

    pub fn first(&self, reads: &[Option<u64>], before: usize) -> Option<usize> {
        reads.iter().flatten().filter_map(|local| self.0.get(local).copied())
            .filter(|&index| index < before).min()
    }
}

pub(super) struct BlockValues {
    pub definitions: Definitions,
    pub reads: Vec<Vec<Option<u64>>>,
}

impl BlockValues {
    /// Discovery of producer positions and the old eligibility snapshot shares
    /// one block visit. No RcLocal clone is retained per operand occurrence.
    pub fn new(block: &ast::Block, mut eligible: impl FnMut(&Statement, &RcLocal) -> bool) -> Self {
        let mut definitions = Definitions::default();
        let mut reads = Vec::with_capacity(block.len());
        for (index, statement) in block.iter().enumerate() {
            definitions.record(statement, index);
            reads.push(eligible_reads(statement, |local| eligible(statement, local)));
        }
        Self { definitions, reads }
    }
}

/// Preserve operand order and duplicate reads. A consumed site becomes None;
/// newly exposed operands are deliberately not admitted into this snapshot.
pub(super) fn eligible_reads(value: &impl LocalRw, mut eligible: impl FnMut(&RcLocal) -> bool) -> Vec<Option<u64>> {
    let mut reads = Vec::new();
    value.visit_local_reads(&mut |local| {
        if eligible(local) { reads.push(Some(local.stable_id())); }
        true
    });
    #[cfg(test)]
    assert_eq!(reads, value.values_read().into_iter().filter(|local| eligible(local))
        .map(|local| Some(local.stable_id())).collect::<Vec<_>>());
    reads
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_and_operand_occurrences_preserve_packs_duplicate_reads_and_capture_scope() {
        let [a, b, c, child_only] = std::array::from_fn(|_| RcLocal::default());
        let closure = ast::Closure { node_origin: Default::default(), function: Default::default(),
            upvalues: vec![ast::Upvalue::Ref(a.clone()), ast::Upvalue::Copy(a.clone())] };
        closure.function.lock().body.push(ast::Return::new(vec![child_only.clone().into()]).into());
        let block = ast::Block(vec![
            ast::Assign::new(vec![a.clone().into(), b.clone().into()], vec![ast::Call::new(
                ast::Global::from("pack").into(), vec![]).into()]).into(),
            ast::Assign::new(vec![a.clone().into()], vec![b.clone().into()]).into(),
            ast::Assign::new(vec![c.clone().into()], vec![closure.into()]).into(),
            ast::Return::new(vec![a.clone().into(), b.clone().into(), a.clone().into()]).into(),
        ]);
        let before = [&a, &b, &c, &child_only].map(|local| triomphe::Arc::count(&local.0.0));
        let mut values = BlockValues::new(&block, |_, _| true);
        assert_eq!([&a, &b, &c, &child_only].map(|local| triomphe::Arc::count(&local.0.0)), before);
        assert_eq!(values.reads[2], [Some(a.stable_id()), Some(a.stable_id())]);
        assert_eq!(values.reads[3], [Some(a.stable_id()), Some(b.stable_id()), Some(a.stable_id())]);
        assert_eq!(values.definitions.first(&values.reads[3], 3), Some(0));
        values.reads[3][0] = None;
        assert_eq!(values.definitions.first(&values.reads[3], 3), Some(0));
        assert_eq!(values.definitions.first(&[Some(child_only.stable_id())], block.len()), None);
        assert_eq!(values.definitions.first(&[Some(c.stable_id())], 2), None);
    }
}
