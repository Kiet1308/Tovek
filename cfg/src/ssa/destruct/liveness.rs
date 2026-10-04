use ast::{LocalRw, RcLocal};
use rustc_hash::FxHashMap;

use petgraph::stable_graph::NodeIndex;

use crate::function::Function;

/// SSA liveness with block parameters:
///
/// * LiveOut(B) = arg_out_uses(B) union (union over S in successors(B) of
///   [live_in(S) minus params(S)])
/// * LiveIn(B) = [uses(B) union live_out(B)] minus defs(B)
///
/// `uses` are all reads of the block, `arg_out_uses` the reads of the
/// arguments passed to its successors and `params` the destinations those
/// arguments define. The least fixed point is unique, so this equals the
/// former per-variable path exploration. Small functions retain the dense
/// word-at-a-time solver. Large products of blocks and locals use a sparse
/// delta solver: only newly live bits cross predecessor edges. Each result row
/// independently becomes dense when packed words would cost more than a bitset.
#[derive(Debug, Default)]
pub struct Liveness {
    ids: FxHashMap<u64, u32>,
    slots: FxHashMap<NodeIndex, usize>,
    storage: Storage,
    #[cfg(test)]
    locals: Vec<RcLocal>,
}

// The original solver holds six B * ceil(L / 64) matrices simultaneously.
// Prefer sparse rows above this size unless even their mandatory row metadata
// would cost more. This is a representation threshold, never an analysis or
// process-memory budget: both solvers compute the full fixed point.
const PREFERRED_DENSE_SCRATCH_BYTES: usize = 8 * 1024 * 1024;

#[cfg(test)]
thread_local! { static FORCE_SPARSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[derive(Debug)]
enum Storage {
    Dense { words: usize, live_in: Vec<u64>, live_out: Vec<u64> },
    Sparse { live_in: Vec<LiveRow>, live_out: Vec<LiveRow> },
}

impl Default for Storage {
    fn default() -> Self {
        Self::Dense { words: 0, live_in: Vec::new(), live_out: Vec::new() }
    }
}

#[derive(Clone, Copy, Debug)]
struct Word {
    index: u32,
    bits: u64,
}

/// Sorted nonzero words. Used for immutable definition/parameter masks and for
/// low-density live sets; no array is sized by the largest stable local ID.
fn packed_words(mut ids: Vec<u32>) -> Vec<Word> {
    ids.sort_unstable();
    let mut result: Vec<Word> = Vec::new();
    for id in ids {
        let index = id / 64;
        let bit = 1 << (id % 64);
        if let Some(word) = result.last_mut().filter(|word| word.index == index) {
            word.bits |= bit;
        } else {
            result.push(Word { index, bits: bit });
        }
    }
    result
}

fn word_bits(words: &[Word], index: u32) -> u64 {
    words.binary_search_by_key(&index, |word| word.index)
        .map_or(0, |at| words[at].bits)
}

#[derive(Debug)]
enum LiveRow {
    Sparse(Vec<Word>),
    Dense(Box<[u64]>),
}

impl Default for LiveRow {
    fn default() -> Self { Self::Sparse(Vec::new()) }
}

impl LiveRow {
    fn bits(&self, index: u32) -> u64 {
        match self {
            Self::Sparse(words) => word_bits(words, index),
            Self::Dense(words) => words[index as usize],
        }
    }

    /// Return precisely the new bits. Propagating a previously seen bit again
    /// is unnecessary even on cycles, parallel edges or irreducible graphs.
    fn insert(&mut self, index: u32, bits: u64, word_count: usize) -> u64 {
        if bits == 0 { return 0; }
        match self {
            Self::Dense(words) => {
                let word = &mut words[index as usize];
                let added = bits & !*word;
                *word |= bits;
                added
            }
            Self::Sparse(words) => match words.binary_search_by_key(&index, |word| word.index) {
                Ok(at) => {
                    let added = bits & !words[at].bits;
                    words[at].bits |= bits;
                    added
                }
                Err(at) => {
                    // Vec's next geometric allocation must also fit below a
                    // dense row. This matters for many tiny nonempty rows.
                    let next_capacity = if words.len() == words.capacity() {
                        (words.capacity() * 2).max(4)
                    } else { words.capacity() };
                    if (words.len() + 1) * std::mem::size_of::<Word>() >= word_count * 8
                        || next_capacity * std::mem::size_of::<Word>() > word_count * 8
                    {
                        let mut dense = vec![0; word_count].into_boxed_slice();
                        for word in words.iter() { dense[word.index as usize] = word.bits; }
                        dense[index as usize] = bits;
                        *self = Self::Dense(dense);
                    } else {
                        words.insert(at, Word { index, bits });
                        // Vec may reserve more than requested on a different
                        // standard library/allocator. Enforce the actual row
                        // bound as well, not only the expected growth above.
                        if words.capacity() * std::mem::size_of::<Word>() > word_count * 8 {
                            let mut dense = vec![0; word_count].into_boxed_slice();
                            for word in words.iter() { dense[word.index as usize] = word.bits; }
                            *self = Self::Dense(dense);
                        }
                    }
                    bits
                }
            },
        }
    }

    fn allocated_bytes(&self) -> usize {
        match self {
            Self::Sparse(words) => words.capacity() * std::mem::size_of::<Word>(),
            Self::Dense(words) => std::mem::size_of_val(&**words),
        }
    }

    fn for_each(self, mut visit: impl FnMut(u32, u64)) {
        match self {
            Self::Sparse(words) => {
                for word in words { visit(word.index, word.bits); }
            }
            Self::Dense(words) => {
                for (index, &bits) in words.iter().enumerate() {
                    if bits != 0 { visit(index as u32, bits); }
                }
            }
        }
    }
}

/// Coalesce pending deltas by block and word. A bit-event queue could retain
/// O(B * L) events under dense fan-in. Here at most B block IDs are queued,
/// and pending payload has the same sparse/dense word bound as result rows.
struct Pending {
    rows: Vec<LiveRow>,
    queued: Vec<bool>,
    work: Vec<usize>,
    words: usize,
}

impl Pending {
    fn new(blocks: usize, words: usize) -> Self {
        Self { rows: (0..blocks).map(|_| LiveRow::default()).collect(),
            queued: vec![false; blocks], work: Vec::new(), words }
    }

    fn insert(&mut self, slot: usize, index: u32, bits: u64) {
        if bits == 0 { return; }
        self.rows[slot].insert(index, bits, self.words);
        if !self.queued[slot] {
            self.queued[slot] = true;
            self.work.push(slot);
        }
    }

    fn pop(&mut self) -> Option<(usize, LiveRow)> {
        let slot = self.work.pop()?;
        self.queued[slot] = false;
        Some((slot, std::mem::take(&mut self.rows[slot])))
    }
}

type Facts = (Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>);

fn dense_scratch_bytes(blocks: usize, words: usize) -> Option<usize> {
    blocks.checked_mul(words)?.checked_mul(6 * std::mem::size_of::<u64>())
}

fn prefer_dense(dense_bytes: Option<usize>, words: usize, dense_limit: usize) -> bool {
    // A tall CFG with very few locals can exceed the preferred total size,
    // but replacing contiguous words with per-block row headers makes it
    // larger. Compare the six dense rows with only the unavoidable sparse
    // live-in/out/pending headers and definition/parameter mask headers. This
    // deliberately excludes their word payload and allocation overhead.
    let sparse_metadata = 3 * std::mem::size_of::<LiveRow>()
        + 2 * std::mem::size_of::<Vec<Word>>();
    let narrow = words <= sparse_metadata / (6 * std::mem::size_of::<u64>());
    // Zero is reserved for the differential test's forced sparse backend.
    dense_bytes.is_some_and(|bytes| bytes <= dense_limit || (dense_limit != 0 && narrow))
}

fn set(bits: &mut [u64], id: u32) {
    bits[id as usize / 64] |= 1 << (id % 64);
}

impl Liveness {
    /// No block was analysed (the terminal single-block fast path).
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn live_in(&self, node: NodeIndex, local: &RcLocal) -> bool {
        self.contains(node, local, true)
    }

    pub fn live_out(&self, node: NodeIndex, local: &RcLocal) -> bool {
        self.contains(node, local, false)
    }

    fn contains(&self, node: NodeIndex, local: &RcLocal, incoming: bool) -> bool {
        let (Some(&slot), Some(&id)) = (self.slots.get(&node), self.ids.get(&local.stable_id())) else {
            return false;
        };
        let bits = match &self.storage {
            Storage::Dense { words, live_in, live_out } => {
                let rows = if incoming { live_in } else { live_out };
                rows[slot * words + id as usize / 64]
            }
            Storage::Sparse { live_in, live_out } => {
                let rows = if incoming { live_in } else { live_out };
                rows[slot].bits(id / 64)
            }
        };
        bits & (1 << (id % 64)) != 0
    }

    pub fn calculate(function: &Function) -> Self {
        #[cfg(test)]
        if FORCE_SPARSE.with(std::cell::Cell::get) { return Self::calculate_with_limit(function, 0); }
        Self::calculate_with_limit(function, PREFERRED_DENSE_SCRATCH_BYTES)
    }

    #[cfg(test)]
    pub(super) fn with_sparse_solver<T>(run: impl FnOnce() -> T) -> T {
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) { FORCE_SPARSE.with(|flag| flag.set(self.0)); }
        }
        let _restore = Restore(FORCE_SPARSE.with(|flag| flag.replace(true)));
        run()
    }

    fn calculate_with_limit(function: &Function, dense_limit: usize) -> Self {
        let nodes = function.graph().node_indices().collect::<Vec<_>>();
        let slots = nodes.iter().enumerate().map(|(slot, &node)| (node, slot)).collect::<FxHashMap<_, _>>();
        let mut ids = FxHashMap::<u64, u32>::default();
        #[cfg(test)]
        let mut locals = Vec::new();
        let mut id = |local: &RcLocal| {
            let next = ids.len() as u32;
            *ids.entry(local.stable_id()).or_insert_with(|| {
                #[cfg(test)]
                locals.push(local.clone());
                next
            })
        };
        // (uses, defs, params, arg_out_uses) as id lists per block.
        let mut facts: Vec<Facts> = vec![(Vec::new(), Vec::new(), Vec::new(), Vec::new()); nodes.len()];
        for (slot, &node) in nodes.iter().enumerate() {
            for statement in function.block(node).unwrap().iter() {
                statement.visit_local_reads(&mut |local| {
                    facts[slot].0.push(id(local));
                    true
                });
                statement.visit_local_writes(&mut |local| {
                    facts[slot].1.push(id(local));
                    true
                });
            }
            for (predecessor, edge) in function.edges_to_block(node) {
                facts[slot].2.extend(edge.arguments.iter().map(|(parameter, _)| id(parameter)));
                let predecessor = slots[&predecessor];
                for (_, argument) in &edge.arguments {
                    argument.visit_local_reads(&mut |local| {
                        facts[predecessor].3.push(id(local));
                        true
                    });
                }
            }
        }
        // With no operand identities every query is false. In particular a
        // large graph of constant branches must not allocate empty bit rows.
        // Keep slots so is_empty still distinguishes an analysed graph from
        // the terminal fast path, just as before.
        if ids.is_empty() {
            return Self { ids, slots, storage: Storage::default(), #[cfg(test)] locals };
        }
        let words = ids.len().div_ceil(64).max(1);
        let dense_bytes = dense_scratch_bytes(nodes.len(), words);
        let storage = if prefer_dense(dense_bytes, words, dense_limit) {
            Self::solve_dense(function, &nodes, &slots, &facts, words)
        } else {
            Self::solve_sparse(function, &nodes, &slots, facts, words)
        };
        if ast::telemetry::enabled() {
            ast::telemetry::count("ssa_liveness_dense_scratch_estimate", dense_bytes.unwrap_or(usize::MAX) as u64);
            ast::telemetry::count("ssa_liveness_sparse_functions", u64::from(matches!(storage, Storage::Sparse { .. })));
            ast::telemetry::count("ssa_liveness_retained_bytes", storage.allocated_bytes() as u64);
        }
        Self { ids, slots, storage, #[cfg(test)] locals }
    }

    fn solve_dense(
        function: &Function, nodes: &[NodeIndex], slots: &FxHashMap<NodeIndex, usize>,
        facts: &[Facts], words: usize,
    ) -> Storage {
        let rows = nodes.len() * words;
        let (mut uses, mut keep, mut params, mut arg_out) =
            (vec![0u64; rows], vec![!0u64; rows], vec![0u64; rows], vec![0u64; rows]);
        for (slot, (block_uses, block_defs, block_params, block_arg_out)) in facts.iter().enumerate() {
            let row = slot * words..(slot + 1) * words;
            for &read in block_uses { set(&mut uses[row.clone()], read); }
            for &written in block_defs { keep[row.start + written as usize / 64] &= !(1 << (written % 64)); }
            for &parameter in block_params { set(&mut params[row.clone()], parameter); }
            for &read in block_arg_out { set(&mut arg_out[row.clone()], read); }
        }
        let successors = nodes.iter()
            .map(|&node| function.successor_blocks(node).map(|next| slots[&next]).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let predecessors = nodes.iter()
            .map(|&node| function.predecessor_blocks(node).map(|previous| slots[&previous]).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let mut live_in = vec![0u64; rows];
        let mut live_out = vec![0u64; rows];
        let mut work = (0..nodes.len()).rev().collect::<Vec<_>>();
        let mut queued = vec![true; nodes.len()];
        let mut next_out = vec![0u64; words];
        while let Some(slot) = work.pop() {
            queued[slot] = false;
            let row = slot * words..(slot + 1) * words;
            next_out.copy_from_slice(&arg_out[row.clone()]);
            for &successor in &successors[slot] {
                let successor = successor * words..(successor + 1) * words;
                for word in 0..words {
                    next_out[word] |= live_in[successor.start + word] & !params[successor.start + word];
                }
            }
            let mut changed = false;
            for word in 0..words {
                let at = row.start + word;
                let value = (uses[at] | next_out[word]) & keep[at];
                changed |= live_in[at] != value;
                live_in[at] = value;
                live_out[at] = next_out[word];
            }
            if changed {
                for &predecessor in &predecessors[slot] {
                    if !queued[predecessor] {
                        queued[predecessor] = true;
                        work.push(predecessor);
                    }
                }
            }
        }
        Storage::Dense { words, live_in, live_out }
    }

    fn solve_sparse(
        function: &Function, nodes: &[NodeIndex], slots: &FxHashMap<NodeIndex, usize>,
        mut facts: Vec<Facts>, words: usize,
    ) -> Storage {
        let masks = facts.iter_mut().map(|(_, defs, params, _)| {
            (packed_words(std::mem::take(defs)), packed_words(std::mem::take(params)))
        }).collect::<Vec<_>>();
        let predecessors = nodes.iter().map(|&node| {
            function.predecessor_blocks(node).map(|previous| slots[&previous]).collect::<Vec<_>>()
        }).collect::<Vec<_>>();
        let mut live_in = (0..nodes.len()).map(|_| LiveRow::default()).collect::<Vec<_>>();
        let mut live_out = (0..nodes.len()).map(|_| LiveRow::default()).collect::<Vec<_>>();
        let mut work = Pending::new(nodes.len(), words);
        for (slot, (uses, _, _, arg_out)) in facts.into_iter().enumerate() {
            for word in packed_words(uses) {
                let bits = word.bits & !word_bits(&masks[slot].0, word.index);
                let added = live_in[slot].insert(word.index, bits, words);
                work.insert(slot, word.index, added);
            }
            for word in packed_words(arg_out) {
                live_out[slot].insert(word.index, word.bits, words);
                let bits = word.bits & !word_bits(&masks[slot].0, word.index);
                let added = live_in[slot].insert(word.index, bits, words);
                work.insert(slot, word.index, added);
            }
        }
        // The equations are distributive over individual bits. Seed their
        // constant terms above, then propagate only new LiveIn bits backward;
        // parameters kill them at the successor and definitions at the source.
        while let Some((successor, changes)) = work.pop() {
            changes.for_each(|index, bits| {
                let bits = bits & !word_bits(&masks[successor].1, index);
                if bits == 0 { return; }
                for &slot in &predecessors[successor] {
                    let added_out = live_out[slot].insert(index, bits, words);
                    let bits = added_out & !word_bits(&masks[slot].0, index);
                    let added_in = live_in[slot].insert(index, bits, words);
                    work.insert(slot, index, added_in);
                }
            });
        }
        Storage::Sparse { live_in, live_out }
    }

    #[cfg(test)]
    fn live_in_set(&self, node: NodeIndex) -> rustc_hash::FxHashSet<RcLocal> {
        self.locals.iter().filter(|local| self.live_in(node, local)).cloned().collect()
    }

    #[cfg(test)]
    fn live_out_set(&self, node: NodeIndex) -> rustc_hash::FxHashSet<RcLocal> {
        self.locals.iter().filter(|local| self.live_out(node, local)).cloned().collect()
    }
}

impl Storage {
    fn allocated_bytes(&self) -> usize {
        match self {
            Self::Dense { live_in, live_out, .. } => (live_in.capacity() + live_out.capacity()) * 8,
            Self::Sparse { live_in, live_out } => {
                (live_in.capacity() + live_out.capacity()) * std::mem::size_of::<LiveRow>()
                    + live_in.iter().chain(live_out).map(LiveRow::allocated_bytes).sum::<usize>()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashSet;
    use crate::block::BlockEdge;

    /// Deliberately independent set equations and vector-based operand queries.
    /// It neither packs words nor shares the production transfer implementation.
    fn reference(function: &Function) -> FxHashMap<NodeIndex, (FxHashSet<u64>, FxHashSet<u64>)> {
        let mut facts = FxHashMap::default();
        for (node, block) in function.blocks() {
            let uses = block.iter().flat_map(|statement| statement.values_read())
                .map(RcLocal::stable_id).collect::<FxHashSet<_>>();
            let defs = block.iter().flat_map(|statement| statement.values_written())
                .map(RcLocal::stable_id).collect::<FxHashSet<_>>();
            let params = function.edges_to_block(node).flat_map(|(_, edge)| edge.arguments.iter())
                .map(|(local, _)| local.stable_id()).collect::<FxHashSet<_>>();
            let args = function.edges(node).flat_map(|edge| &edge.weight().arguments)
                .flat_map(|(_, value)| value.values_read()).map(RcLocal::stable_id).collect::<FxHashSet<_>>();
            facts.insert(node, (uses, defs, params, args));
        }
        let mut result = function.graph().node_indices()
            .map(|node| (node, (FxHashSet::default(), FxHashSet::default()))).collect::<FxHashMap<_, _>>();
        loop {
            let mut changed = false;
            for node in function.graph().node_indices() {
                let (uses, defs, _, args) = &facts[&node];
                let mut out = args.clone();
                for successor in function.successor_blocks(node) {
                    out.extend(result[&successor].0.iter().filter(|local| !facts[&successor].2.contains(*local)).copied());
                }
                let incoming = uses.union(&out).filter(|local| !defs.contains(*local)).copied().collect::<FxHashSet<_>>();
                changed |= result[&node] != (incoming.clone(), out.clone());
                result.insert(node, (incoming, out));
            }
            if !changed { return result; }
        }
    }

    #[test]
    fn sparse_and_dense_match_set_equations_on_cycles_captures_and_sparse_node_indices() {
        for seed in 1..=192u64 {
            let mut state = seed;
            let mut random = |limit: usize| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((state >> 32) as usize) % limit
            };
            let mut function = Function::new(0);
            let nodes = (0..2 + random(10)).map(|_| function.new_block()).collect::<Vec<_>>();
            function.set_entry(nodes[0]);
            let hole = function.new_block();
            let last = function.new_block();
            function.remove_block(hole);
            let mut nodes = nodes;
            nodes.push(last);
            let locals = (0..1 + random(140)).map(|_| RcLocal::default()).collect::<Vec<_>>();
            for &node in &nodes {
                for _ in 0..1 + random(9) {
                    let read = locals[random(locals.len())].clone();
                    let written = locals[random(locals.len())].clone();
                    let value = match random(4) {
                        0 => ast::Binary::new(read.clone().into(), read.into(), ast::BinaryOperation::Add).into(),
                        1 => ast::Closure { node_origin: Default::default(), function: Default::default(),
                            upvalues: vec![ast::Upvalue::Copy(read.clone()), ast::Upvalue::Ref(read)] }.into(),
                        _ => read.into(),
                    };
                    function.block_mut(node).unwrap().push(ast::Assign::new(vec![written.into()], vec![value]).into());
                }
                for _ in 0..random(5) {
                    let target = nodes[random(nodes.len())];
                    let parameter = locals[random(locals.len())].clone();
                    let argument = locals[random(locals.len())].clone();
                    let edge = BlockEdge { arguments: vec![(parameter, argument.into())], ..Default::default() };
                    function.graph_mut().add_edge(node, target, edge.clone());
                    if random(3) == 0 { function.graph_mut().add_edge(node, target, edge); }
                }
            }
            let expected = reference(&function);
            let dense = Liveness::calculate_with_limit(&function, usize::MAX);
            let sparse = Liveness::calculate_with_limit(&function, 0);
            for &node in &nodes {
                for local in &locals {
                    let id = local.stable_id();
                    assert_eq!(dense.live_in(node, local), expected[&node].0.contains(&id), "dense in seed={seed}");
                    assert_eq!(dense.live_out(node, local), expected[&node].1.contains(&id), "dense out seed={seed}");
                    assert_eq!(sparse.live_in(node, local), expected[&node].0.contains(&id), "sparse in seed={seed}");
                    assert_eq!(sparse.live_out(node, local), expected[&node].1.contains(&id), "sparse out seed={seed}");
                }
            }
            let unknown = RcLocal::default();
            assert!(!sparse.live_in(last, &unknown));
            assert!(!sparse.live_out(hole, &locals[0]));
        }
    }

    #[test]
    fn sparse_rows_promote_by_storage_cost_and_propagate_only_new_bits() {
        let mut row = LiveRow::default();
        assert_eq!(row.insert(3, 0b101, 16), 0b101);
        assert_eq!(row.insert(3, 0b111, 16), 0b010);
        assert!(matches!(row, LiveRow::Sparse(_)));
        for index in 0..16 { row.insert(index, 1 << index, 16); }
        assert!(matches!(row, LiveRow::Dense(_)));
        for index in 0..16 {
            assert_eq!(row.bits(index), (1 << index) | if index == 3 { 0b111 } else { 0 });
            assert_eq!(row.insert(index, 1 << index, 16), 0);
        }
        assert_eq!(row.allocated_bytes(), 16 * 8);
        assert_eq!(dense_scratch_bytes(usize::MAX, 2), None);
        assert_eq!(dense_scratch_bytes(100, 20), Some(96_000));
    }

    #[test]
    fn tall_narrow_cfg_keeps_dense_words_when_sparse_headers_cost_more() {
        let tall_narrow = dense_scratch_bytes(1_000_000, 1);
        assert!(tall_narrow.unwrap() > PREFERRED_DENSE_SCRATCH_BYTES);
        assert!(prefer_dense(tall_narrow, 1, PREFERRED_DENSE_SCRATCH_BYTES));
        assert!(!prefer_dense(dense_scratch_bytes(4096, 64), 64, PREFERRED_DENSE_SCRATCH_BYTES));
        assert!(!prefer_dense(None, 1, PREFERRED_DENSE_SCRATCH_BYTES));
        assert!(!prefer_dense(tall_narrow, 1, 0));

        // Lower the preferred size so a small executable graph exercises the
        // same selection branch without allocating a million test blocks.
        let mut function = Function::new(0);
        let nodes = (0..8).map(|_| function.new_block()).collect::<Vec<_>>();
        let local = RcLocal::default();
        function.set_entry(nodes[0]);
        function.parameters.push(local.clone());
        for pair in nodes.windows(2) {
            function.graph_mut().add_edge(pair[0], pair[1], BlockEdge::default());
        }
        function.block_mut(nodes[7]).unwrap().push(ast::Return::new(vec![local.clone().into()]).into());
        let dense = Liveness::calculate_with_limit(&function, 1);
        let sparse = Liveness::calculate_with_limit(&function, 0);
        assert!(matches!(dense.storage, Storage::Dense { .. }));
        assert!(matches!(sparse.storage, Storage::Sparse { .. }));
        for node in nodes {
            assert!(dense.live_in(node, &local));
            assert_eq!(dense.live_out(node, &local), sparse.live_out(node, &local));
            assert_eq!(dense.live_in(node, &local), sparse.live_in(node, &local));
        }
    }

    #[test]
    fn pending_deltas_coalesce_words_and_queue_each_block_once() {
        let mut pending = Pending::new(4, 16);
        for bit in 0..64 {
            for slot in 0..4 {
                for index in 0..16 { pending.insert(slot, index, 1 << bit); }
            }
        }
        assert_eq!(pending.work.len(), 4);
        assert!(pending.rows.iter().all(|row| row.allocated_bytes() <= 16 * 8));
        let mut visited = 0;
        while let Some((slot, row)) = pending.pop() {
            visited += 1;
            assert!(!pending.queued[slot]);
            let mut count = 0;
            row.for_each(|_, bits| { assert_eq!(bits, u64::MAX); count += 1; });
            assert_eq!(count, 16);
        }
        assert_eq!(visited, 4);
        pending.insert(0, 3, 1);
        assert_eq!(pending.work, [0]);
    }

    #[test]
    fn graphs_without_local_operands_allocate_no_liveness_rows() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);
        function.graph_mut().add_edge(entry, exit, BlockEdge::default());
        function.block_mut(exit).unwrap().push(ast::Return::new(vec![ast::Literal::Nil.into()]).into());
        let live = Liveness::calculate_with_limit(&function, 0);
        assert!(!live.is_empty());
        assert_eq!(live.storage.allocated_bytes(), 0);
        let unknown = RcLocal::default();
        assert!(!live.live_in(entry, &unknown) && !live.live_out(exit, &unknown));
    }

    #[test]
    fn large_sparse_cfg_avoids_quadratic_dense_scratch_without_losing_cross_block_uses() {
        const COUNT: usize = 4096;
        let mut function = Function::new(0);
        let nodes = (0..COUNT).map(|_| function.new_block()).collect::<Vec<_>>();
        let locals = (0..COUNT).map(|_| RcLocal::default()).collect::<Vec<_>>();
        function.set_entry(nodes[0]);
        for index in 0..COUNT {
            function.block_mut(nodes[index]).unwrap().push(ast::Assign::new(
                vec![locals[index].clone().into()], vec![ast::Literal::Number(index as f64).into()]).into());
            if index > 0 {
                function.graph_mut().add_edge(nodes[index - 1], nodes[index], BlockEdge::default());
                function.block_mut(nodes[index]).unwrap().push(ast::Call::new(
                    ast::Global::from("observe").into(), vec![locals[index - 1].clone().into()]).into());
            }
        }
        let live = Liveness::calculate(&function);
        assert!(matches!(live.storage, Storage::Sparse { .. }));
        let dense_scratch = dense_scratch_bytes(COUNT, COUNT / 64).unwrap();
        assert!(dense_scratch > PREFERRED_DENSE_SCRATCH_BYTES);
        assert!(live.storage.allocated_bytes() < dense_scratch / 8);
        for index in 0..COUNT {
            assert_eq!(live.live_out(nodes[index], &locals[index]), index + 1 < COUNT);
            assert!(!live.live_in(nodes[index], &locals[index]));
            if index > 0 { assert!(live.live_in(nodes[index], &locals[index - 1])); }
            if index > 1 { assert!(!live.live_in(nodes[index], &locals[index - 2])); }
        }
        let dense = Liveness::calculate_with_limit(&function, usize::MAX);
        for index in 1..COUNT {
            assert_eq!(live.live_in(nodes[index], &locals[index - 1]), dense.live_in(nodes[index], &locals[index - 1]));
        }
    }

    #[test]
    fn visitor_liveness_keeps_phi_transport_and_generic_control_live() {
        for backedge in [false, true] {
            let mut function = Function::new(0);
            let entry = function.new_block();
            let step = function.new_block();
            let exhausted = function.new_block();
            let body = function.new_block();
            function.set_entry(entry);
            let [generator, state, control, result, phi] = std::array::from_fn(|_| RcLocal::default());
            function.parameters = vec![generator.clone(), state.clone(), control.clone()];
            function.graph_mut().add_edge(entry, step, BlockEdge::default());
            function.graph_mut().add_edge(step, exhausted, BlockEdge::default());
            let argument = ast::Binary::new(result.clone().into(), result.clone().into(),
                ast::BinaryOperation::Add).into();
            let edge = BlockEdge { arguments: vec![(phi.clone(), argument)], ..Default::default() };
            // Parallel edges and duplicate reads in an argument must not lose
            // the transport's use at its source or its definition at its target.
            function.graph_mut().add_edge(step, body, edge.clone());
            function.graph_mut().add_edge(step, body, edge);
            if backedge { function.graph_mut().add_edge(step, step, BlockEdge::default()); }
            function.block_mut(step).unwrap().push(ast::GenericForNext::new(
                vec![result.clone(), result.clone()], generator.clone().into(),
                state.clone(), control.clone(),
            ).into());
            function.block_mut(exhausted).unwrap().push(ast::Return::new(vec![control.clone().into()]).into());
            function.block_mut(body).unwrap().push(ast::Return::new(vec![phi.clone().into(), control.clone().into()]).into());

            let live = Liveness::calculate(&function);
            let sparse = Liveness::calculate_with_limit(&function, 0);
            for node in [entry, step, exhausted, body] {
                assert_eq!(sparse.live_in_set(node), live.live_in_set(node));
                assert_eq!(sparse.live_out_set(node), live.live_out_set(node));
            }
            let inputs: FxHashSet<_> = [generator, state, control.clone()].into_iter().collect();
            assert_eq!(live.live_in_set(entry), inputs);
            assert_eq!(live.live_out_set(entry), inputs);
            assert_eq!(live.live_in_set(step), inputs);
            let mut outputs: FxHashSet<_> = [result, control.clone()].into_iter().collect();
            if backedge { outputs.extend(inputs); }
            assert_eq!(live.live_out_set(step), outputs);
            assert_eq!(live.live_in_set(exhausted), [control.clone()].into_iter().collect());
            assert_eq!(live.live_in_set(body), [phi, control].into_iter().collect());
            assert!(live.live_out_set(exhausted).is_empty());
            assert!(live.live_out_set(body).is_empty());
        }
    }
}
