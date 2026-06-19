use indexmap::IndexSet;
use petgraph::stable_graph::NodeIndex;
use rangemap::RangeInclusiveMap;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::function::Function;

#[derive(Debug)]
pub(crate) struct UpvaluesOpen {
    // Per range, the SET of `(block, statement)` sites where this local was
    // captured ("opened") as an upvalue along the paths reaching that point. It
    // is an insertion-ordered `IndexSet` (first-occurrence preserved) rather
    // than a `Vec`: as a `Vec` these were accumulated across CFG diamonds/loops
    // with NO deduplication, so the vector doubled at every merge and grew
    // exponentially (measured 63M entries on a 97-block function, ~17-20s and
    // hundreds of MB). The ONLY consumer (`construct.rs::mark_upvalues`) reads
    // `.first()` of this collection, so deduplicating while preserving the first
    // inserted element is output-exact.
    // WARNING: keep that invariant — only `.first()` may be relied upon; do not
    // start iterating/counting these sets without revisiting the dedup-safety.
    pub open: FxHashMap<
        NodeIndex,
        FxHashMap<ast::RcLocal, RangeInclusiveMap<usize, IndexSet<(NodeIndex, usize)>>>,
    >,
    old_locals: FxHashMap<ast::RcLocal, ast::RcLocal>,
}

impl UpvaluesOpen {
    pub fn new(function: &Function, old_locals: FxHashMap<ast::RcLocal, ast::RcLocal>) -> Self {
        let mut this = Self {
            open: Default::default(),
            old_locals,
        };
        let entry = function.entry().unwrap();
        let mut stack = vec![entry];
        let mut visited = FxHashSet::default();
        while let Some(node) = stack.pop() {
            visited.insert(node);
            let block = function.block(node).unwrap();
            let block_opened = this.open.entry(node).or_default();
            for (stat_index, statement) in block.iter().enumerate() {
                // TODO: use traverse rvalues instead
                // this is because the lifter isnt guaranteed to be lifting bytecode
                // it could be lifting lua source code for deobfuscation purposes
                if let ast::Statement::Assign(assign) = statement {
                    for opened in assign
                        .right
                        .iter()
                        .filter_map(|r| r.as_closure())
                        .flat_map(|c| c.upvalues.iter())
                        .filter_map(|u| match u {
                            ast::Upvalue::Copy(_) => None,
                            ast::Upvalue::Ref(l) => Some(l),
                        })
                        .map(|l| this.old_locals[l].clone())
                    {
                        let open_ranges = block_opened.entry(opened).or_default();
                        let mut open_locations = IndexSet::default();
                        if let Some((_prev_range, prev_locations)) =
                            open_ranges.get_key_value(&stat_index)
                        {
                            // TODO: this assert fails in Luau with the below code,
                            // but i dont know why. it appears to work fine with the
                            // assert commented out, but we should double check it.
                            /*
                            local u = a

                            if u then
                                print'hi'
                            end

                            function f()
                                return u
                            end
                            */
                            // assert!(prev_range.contains(&(block.len() - 1)));
                            open_locations.extend(prev_locations.iter().copied());
                        }
                        open_locations.insert((node, stat_index));
                        open_ranges.insert(stat_index..=block.len() - 1, open_locations);
                    }
                } else if let ast::Statement::Close(close) = statement {
                    for closed in &close.locals {
                        if let Some(open_ranges) = block_opened.get_mut(closed) {
                            open_ranges.remove(stat_index..=block.len() - 1);
                        }
                    }
                }
            }
            for successor in function.successor_blocks(node) {
                // TODO: is there any case where successor is visited but has open stuff
                // that wasnt already discovered?
                // maybe possible with multiple opens
                if !visited.contains(&successor) {
                    let successor_block = function.block(successor).unwrap();
                    let open_at_end = this.open[&node]
                        .iter()
                        .filter_map(|(l, m)| {
                            Some((l.clone(), m.get(&(block.len().saturating_sub(1)))?.clone()))
                        })
                        .collect::<Vec<_>>();
                    let successor_open = this.open.entry(successor).or_default();
                    for (open, mut locations) in open_at_end {
                        let open_ranges = successor_open.entry(open).or_default();
                        // TODO: sorta ugly doing a saturating subtraction, use uninclusive ranges instead?
                        let range = 0..=successor_block.len().saturating_sub(1);
                        if let Some((prev_range, prev_locations)) = open_ranges.get_key_value(&0) {
                            assert_eq!(prev_range, &range);
                            locations.extend(prev_locations.iter().copied());
                        }
                        open_ranges.insert(range, locations);
                    }

                    stack.push(successor);
                }
            }
        }
        this
    }
}
