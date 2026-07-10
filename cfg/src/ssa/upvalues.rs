use std::collections::VecDeque;

use ast::LocalRw;
use indexmap::IndexSet;
use petgraph::stable_graph::NodeIndex;
use rangemap::RangeInclusiveMap;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::function::Function;

/// The by-reference upvalues a statement captures — closures sitting directly on
/// an assignment's right-hand side, matching the forward pass's detection in
/// `UpvaluesOpen::new`. (At SSA-construction time the lifter emits every
/// `NEWCLOSURE` as its own `tmp = function … end`, so a closure passed as a call
/// argument is still a top-level RHS here.)
fn ref_upvalues(statement: &ast::Statement) -> impl Iterator<Item = &ast::RcLocal> {
    statement
        .as_assign()
        .into_iter()
        .flat_map(|assign| assign.right.iter())
        .filter_map(|r| r.as_closure())
        .flat_map(|c| c.upvalues.iter())
        .filter_map(|u| match u {
            ast::Upvalue::Ref(l) => Some(l),
            ast::Upvalue::Copy(_) => None,
        })
}

/// How a statement defines a given (old) local — see `UpvaluesOpen::def_kind`.
enum DefKind {
    /// A `nil`-literal assignment (declaration shape) writing this SSA version.
    Nil(ast::RcLocal),
    /// A definition by any other means.
    Other,
    /// Not a definition of the local.
    NotDef,
}

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

        this.extend_open_backward(function);
        this
    }

    /// Pull a by-reference-captured local's *cross-block `nil` initializer* into
    /// the same open region as its captures, so every version of the cell is
    /// grouped into one variable by `construct::mark_upvalues`.
    ///
    /// The forward pass above marks a captured local open only from the
    /// closure-creating statement onward (and into successors). When the local
    /// is declared (and `nil`-initialized) in a block that *dominates* the block
    /// where the connection is assigned — the classic
    /// ```text
    /// local conn            -- entry block
    /// if cond then
    ///     conn = sig:Connect(function() conn:Disconnect() end)  -- successor block
    /// end
    /// ```
    /// pattern — that `nil` version is never seen as open. `mark_upvalues` then
    /// leaves it out of the upvalue group: it survives as a separate dead
    /// `local conn = nil`, the reassignment is re-declared as a fresh `local`,
    /// and a final captured write whose result no surviving reader references
    /// collapses to `local _ = ...`. That is a correctness bug — the closures
    /// call `:Disconnect()` on the still-`nil` declaration.
    ///
    /// The fix walks backward from each capture to the reaching definition,
    /// carrying the *same* open-location set so the group key
    /// (`open_locations.first()`, the only thing `mark_upvalues` reads) is
    /// preserved and the declaration lands in the capture's group. It is
    /// deliberately conservative to avoid absorbing unrelated values that merely
    /// share a bytecode register (the lifter maps one register to one original
    /// local for the whole function):
    ///   * It never scans the capture's *own* block. A same-block reaching
    ///     definition is already handled correctly by the forward pass, and is
    ///     usually the `:Connect` receiver temp (`conn = sig:Connect(...)`
    ///     reuses `conn`'s register for `sig`) — absorbing it would wrongly
    ///     forbid inlining `sig`.
    ///   * It only groups a cross-block definition that is a `nil` literal — the
    ///     shape of a `local x`/`local x = nil` declaration. A non-`nil`
    ///     reaching definition is a distinct value (or a parameter), so the walk
    ///     stops without grouping it.
    ///   * It stops at a `Close` of the local (a reused register's previous
    ///     cell boundary) and never marks live-through blocks, so it only ever
    ///     adds coverage at the one declaration site.
    fn extend_open_backward(&mut self, function: &Function) {
        // Seed: one entry per (block, captured local) — the lowest open
        // statement index there and its location set. Collected into a Vec and
        // sorted so processing order, and therefore the result, is independent
        // of `FxHashMap`/`IndexSet` iteration order (determinism).
        let mut starts: Vec<(NodeIndex, usize, ast::RcLocal, IndexSet<(NodeIndex, usize)>)> =
            Vec::new();
        for (&node, locals) in &self.open {
            for (local, ranges) in locals {
                if let Some((range, locs)) = ranges.iter().next() {
                    starts.push((node, *range.start(), local.clone(), locs.clone()));
                }
            }
        }
        starts.sort_by(|a, b| (a.0.index(), a.1, &a.2).cmp(&(b.0.index(), b.1, &b.2)));

        if starts.is_empty() {
            return;
        }

        // SSA versions whose value is consumed by *real* code — read by some
        // statement other than as a closure's by-reference upvalue, then
        // propagated backward through block-parameters (phis), since a phi whose
        // result is consumed also consumes its incoming arguments. (A `Close`
        // reads nothing, so it never marks anything consumed.) We only pull a
        // `nil` declaration into a captured cell when that exact `nil` version is
        // NOT consumed, i.e. it is purely a captured handle whose only readers
        // are the closures (the connection pattern). When the value is also used
        // by ordinary code — e.g. `local x; if c then x = ... end; if not x then
        // ... end` with `x` captured elsewhere — the regular phi/copy coalescing
        // already unifies the `nil` default with the assigned versions;
        // force-merging it into the upvalue cell would only make the out-of-SSA
        // pass materialize the default in every branch (a readability
        // regression). Working at version (not original-local) granularity also
        // keeps an unrelated temp that merely reuses the register — e.g. the
        // `game:GetService(...)` receiver — from being mistaken for a read of
        // the captured cell.
        let consumed = self.consumed_versions(function);

        // The set of blocks in which each original local is captured by
        // reference. We only pull a declaration into a cell whose captures are
        // confined to a *single* block. When a local is captured by closures in
        // more than one block, its cell spans a control-flow merge (e.g. a task
        // handle captured by a worker closure in one branch and by a cleanup
        // closure after the merge): the forward pass groups each capture site
        // separately, so force-merging the shared `nil` declaration into one of
        // them would desynchronize the others — an unsound coalescing. Confining
        // to one capture block keeps the cell free of such merges.
        let mut capture_blocks: FxHashMap<ast::RcLocal, FxHashSet<NodeIndex>> =
            FxHashMap::default();
        for (node, block) in function.blocks() {
            for statement in block.iter() {
                for upvalue in ref_upvalues(statement) {
                    if let Some(old) = self.old_locals.get(upvalue) {
                        capture_blocks.entry(old.clone()).or_default().insert(node);
                    }
                }
            }
        }

        // Predecessor blocks left to scan, from their end: (block, local,
        // carried location set). `visited` makes each (block, local) processed
        // at most once, which both bounds the work (no exponential re-walk over
        // loop back-edges) and keeps the outcome deterministic.
        let mut work: VecDeque<(NodeIndex, ast::RcLocal, IndexSet<(NodeIndex, usize)>)> =
            VecDeque::new();
        let mut visited: FxHashSet<(NodeIndex, ast::RcLocal)> = FxHashSet::default();

        // Only walk back when the reaching definition is *not* in the capture's
        // own block (the cross-block case is the bug). A same-block definition
        // or `Close` means the cell originates here and the forward pass already
        // covers it.
        for (node, sc, local, locs) in &starts {
            // Skip locals captured across more than one block (cell may span a
            // merge — see `capture_blocks`).
            if capture_blocks.get(local).map_or(0, |b| b.len()) != 1 {
                continue;
            }
            if self.block_defines_or_closes(function, *node, *sc, local) {
                continue;
            }
            Self::enqueue_predecessors(function, *node, local, locs, &mut work);
        }
        for (node, _, local, _) in &starts {
            visited.insert((*node, local.clone()));
        }

        while let Some((node, local, locs)) = work.pop_front() {
            if !visited.insert((node, local.clone())) {
                continue;
            }
            let block = function.block(node).unwrap();
            let len = block.len();
            let mut decided = false;
            for i in (0..len).rev() {
                match block.get(i).unwrap() {
                    ast::Statement::Close(close) => {
                        if close.locals.contains(&local) {
                            decided = true; // cell boundary: stop, do not group
                            break;
                        }
                    }
                    statement => match self.def_kind(statement, &local) {
                        // Cross-block `nil` declaration whose value no real code
                        // consumes: the cell's initializer — group it.
                        DefKind::Nil(ref version) if !consumed.contains(version) => {
                            self.mark_open(node, &local, i, len, &locs);
                            decided = true;
                            break;
                        }
                        // A non-`nil` (re)definition, or a `nil` default whose
                        // value ordinary code also consumes (already unified by
                        // phi coalescing): a distinct value — stop, don't group.
                        DefKind::Nil(_) | DefKind::Other => {
                            decided = true;
                            break;
                        }
                        DefKind::NotDef => {}
                    },
                }
            }
            if decided {
                continue;
            }
            // No statement defines the local here. If a block-parameter (phi)
            // does, that merge IS the reaching definition — stop, and leave it to
            // the regular phi coalescing. Recursing past it would reach the
            // individual phi arms and group only the `nil` one (e.g. the `else`
            // of `if c then x = v else x = nil end`), splitting the merge and
            // making the out-of-SSA pass materialize the default.
            if self.block_has_phi_def(function, node, &local) {
                continue;
            }
            // Truly live-through: keep walking back toward the declaration.
            Self::enqueue_predecessors(function, node, &local, &locs, &mut work);
        }
    }

    /// True if `block` contains a definition or a `Close` of `local` that the
    /// backward walk must respect: a block-parameter (phi) at the block entry, or
    /// a statement def / `Close` in `[0, scan_upto)` (scanned back-to-front).
    fn block_defines_or_closes(
        &self,
        function: &Function,
        node: NodeIndex,
        scan_upto: usize,
        local: &ast::RcLocal,
    ) -> bool {
        if self.block_has_phi_def(function, node, local) {
            return true;
        }
        let block = function.block(node).unwrap();
        for i in (0..scan_upto).rev() {
            match block.get(i).unwrap() {
                ast::Statement::Close(close) => {
                    if close.locals.contains(local) {
                        return true;
                    }
                }
                statement => {
                    if !matches!(self.def_kind(statement, local), DefKind::NotDef) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// True if `node` has a *real-merge* incoming block-parameter (phi) that
    /// defines `local` — i.e. its incoming arguments are not all the same SSA
    /// version. The backward walk must stop at such a merge (it is the reaching
    /// definition, and recursing into the arms would group only the `nil` arm),
    /// but should pass *through* a phi that is merely a copy of one version:
    ///   * a single-predecessor block (trivial rename), or
    ///   * a *degenerate* phi whose argument is the same version on every edge
    ///     (the local was not actually written on any path through the merge —
    ///     e.g. an unrelated `if` sits between the declaration and the capture).
    /// Passing through the degenerate case lets the walk reach the real
    /// declaration; a degenerate phi is a pure copy, so this stays sound.
    fn block_has_phi_def(
        &self,
        function: &Function,
        node: NodeIndex,
        local: &ast::RcLocal,
    ) -> bool {
        let mut seen: Option<Option<&ast::RcLocal>> = None;
        for (_, edge) in function.edges_to_block(node) {
            let Some((_, arg)) = edge
                .arguments
                .iter()
                .find(|(param, _)| self.old_locals.get(param) == Some(local))
            else {
                continue;
            };
            let arg = arg.as_local();
            match seen {
                None => seen = Some(arg),
                // A different incoming version, or a non-local argument we can't
                // prove identical, means a genuine merge.
                Some(prev) if prev != arg || arg.is_none() => return true,
                Some(_) => {}
            }
        }
        false
    }

    /// Classify how `statement` defines (the old) `local`:
    ///   * `Nil(v)` — a `nil`-literal assignment writing SSA version `v` (the
    ///     shape of a `local x`/`local x = nil` declaration).
    ///   * `Other` — a definition by any other means (a real value, a for-loop
    ///     counter, …).
    ///   * `NotDef` — does not define `local`.
    fn def_kind(&self, statement: &ast::Statement, local: &ast::RcLocal) -> DefKind {
        if let ast::Statement::Assign(assign) = statement {
            for (j, lhs) in assign.left.iter().enumerate() {
                if let Some(version) = lhs
                    .as_local()
                    .filter(|w| self.old_locals.get(w) == Some(local))
                {
                    return if matches!(
                        assign.right.get(j),
                        Some(ast::RValue::Literal(ast::Literal::Nil))
                    ) {
                        DefKind::Nil(version.clone())
                    } else {
                        DefKind::Other
                    };
                }
            }
        }
        // Non-`Assign` writers (e.g. for-loop counters) still count as a def.
        if statement
            .values_written()
            .into_iter()
            .any(|w| self.old_locals.get(w) == Some(local))
        {
            return DefKind::Other;
        }
        DefKind::NotDef
    }

    /// SSA versions whose value is consumed by *real* code: those read by a
    /// statement otherwise than as a closure's by-reference upvalue, then closed
    /// backward through block-parameters (a phi whose result is consumed
    /// consumes each of its incoming arguments). A `Close` reads nothing, so it
    /// never marks anything consumed. This distinguishes a purely-captured handle
    /// (whose `nil` initializer flows only to the closures and the cell's
    /// `Close`) from a local that ordinary code also uses — at version
    /// granularity, so an unrelated temp reusing the same register does not
    /// count.
    fn consumed_versions(&self, function: &Function) -> FxHashSet<ast::RcLocal> {
        let mut consumed: FxHashSet<ast::RcLocal> = FxHashSet::default();
        for (_, block) in function.blocks() {
            for statement in block.iter() {
                // By-ref upvalues captured directly on this statement (a
                // `temp = function() ... end` assignment) are captures, not real
                // reads — exclude them.
                let captured: FxHashSet<&ast::RcLocal> = ref_upvalues(statement).collect();
                for read in statement.values_read() {
                    if !captured.contains(read) {
                        consumed.insert(read.clone());
                    }
                }
            }
        }

        // Block-parameter (phi) back-propagation: result consumed ⇒ arguments
        // consumed.
        let mut param_args: FxHashMap<ast::RcLocal, Vec<ast::RcLocal>> = FxHashMap::default();
        for edge in function.graph().edge_weights() {
            for (param, arg) in &edge.arguments {
                if let ast::RValue::Local(a) = arg {
                    param_args.entry(param.clone()).or_default().push(a.clone());
                }
            }
        }
        let mut worklist: Vec<ast::RcLocal> = consumed.iter().cloned().collect();
        while let Some(version) = worklist.pop() {
            if let Some(args) = param_args.get(&version) {
                for arg in args {
                    if consumed.insert(arg.clone()) {
                        worklist.push(arg.clone());
                    }
                }
            }
        }
        consumed
    }

    /// Mark `local` open over `[from, block_len - 1]` in `node`, carrying `locs`
    /// (whose first element is the cell's first capture, the grouping key). Any
    /// pre-existing locations at `from` are appended after, so the carried
    /// first-open location stays the consumer's `.first()`.
    fn mark_open(
        &mut self,
        node: NodeIndex,
        local: &ast::RcLocal,
        from: usize,
        block_len: usize,
        locs: &IndexSet<(NodeIndex, usize)>,
    ) {
        if block_len == 0 {
            return;
        }
        let ranges = self
            .open
            .entry(node)
            .or_default()
            .entry(local.clone())
            .or_default();
        let mut new_locs = locs.clone();
        if let Some(prev) = ranges.get(&from) {
            new_locs.extend(prev.iter().copied());
        }
        ranges.insert(from..=block_len - 1, new_locs);
    }

    fn enqueue_predecessors(
        function: &Function,
        node: NodeIndex,
        local: &ast::RcLocal,
        locs: &IndexSet<(NodeIndex, usize)>,
        work: &mut VecDeque<(NodeIndex, ast::RcLocal, IndexSet<(NodeIndex, usize)>)>,
    ) {
        let mut preds: Vec<NodeIndex> = function.predecessor_blocks(node).collect();
        // Stable order so the worklist (and therefore the result) is deterministic.
        preds.sort_by_key(|n| n.index());
        for pred in preds {
            work.push_back((pred, local.clone(), locs.clone()));
        }
    }
}
