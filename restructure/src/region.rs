//! A conservative, source-shaped CFG structurer.
//!
//! The regular matcher is intentionally permissive because it is useful for
//! partially reduced graphs.  This pass is the opposite: it never mutates the
//! CFG and only returns an AST after proving that all reachable blocks have a
//! unique source-level owner.  Any uncertainty returns `None`, leaving the
//! existing structurer and its semantics-preserving dispatcher as fallbacks.

use ast::{
    Assign, Block, GenericFor, If, LValue, Literal, LocalRw, RValue, RcLocal, Reduce, Statement,
    Traverse, Unary, UnaryOperation,
};
use cfg::block::{BlockEdge, BranchType};
use cfg::function::Function;
use itertools::Itertools;
use petgraph::{
    algo::dominators::{Dominators, simple_fast},
    stable_graph::NodeIndex,
    visit::EdgeRef,
};
use rustc_hash::{FxHashMap, FxHashSet};
use std::{collections::VecDeque, fmt};

/// A proof obligation whose failure makes it unsafe to try a weaker
/// source-shaping matcher on the same CFG.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsafeStructureReason {
    CapturedCellReorder,
    CapturedLoopResultRef,
    LiveBranchRewrite,
    ForInitSuffixOrder,
    ForOriginMissing,
    ForOriginMismatch,
    ForOriginDuplicate,
    ForOriginPrepKindUnsupported,
    ForProtocolEdgeTransfer,
    ForInitEdgeTransferOrder,
    UnmodeledClose,
    UnmodeledControl,
}

impl fmt::Display for UnsafeStructureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::CapturedCellReorder => "captured-cell reorder across iterator preparation",
            Self::CapturedLoopResultRef => {
                "loop result is captured by reference without a proven iteration cell"
            }
            Self::LiveBranchRewrite => "live branch rewrite across a conditional join",
            Self::ForInitSuffixOrder => "observable FORGPREP suffix reorder",
            Self::ForOriginMissing => "generic-for provenance is missing",
            Self::ForOriginMismatch => "generic-for prep/step provenance mismatch",
            Self::ForOriginDuplicate => "duplicate generic-for provenance identity",
            Self::ForOriginPrepKindUnsupported => {
                "generic-for fast-path prep kind is not source-proven"
            }
            Self::ForProtocolEdgeTransfer => {
                "generic-for edge transfer touches hidden iterator protocol"
            }
            Self::ForInitEdgeTransferOrder => {
                "generic-for init edge transfer cannot preserve iterator evaluation order"
            }
            Self::UnmodeledClose => "explicit close event has no source-level representation",
            Self::UnmodeledControl => "VM loop marker has no source-level representation",
        };
        f.write_str(name)
    }
}

/// Result of the proof-driven source-like structurer.  `Unsupported` means
/// this pass has no representation for the graph; `Unsafe` means it found a
/// concrete semantic obstruction that weaker matchers must not bypass.
#[derive(Debug)]
pub enum StructureAttempt {
    Structured(Block),
    Unsupported,
    Unsafe(UnsafeStructureReason),
}

#[derive(Clone)]
struct LoopInfo {
    header: NodeIndex,
    init: NodeIndex,
    body_entry: NodeIndex,
    normal_exit: NodeIndex,
    join: NodeIndex,
    nodes: FxHashSet<NodeIndex>,
    res_locals: Vec<RcLocal>,
    right: Vec<RValue>,
    origin: Option<ast::ForOrigin>,
}

struct Analysis {
    reachable: FxHashSet<NodeIndex>,
    nodes: Vec<NodeIndex>,
    post_dominators: FxHashMap<NodeIndex, FxHashSet<NodeIndex>>,
    live_in: FxHashMap<NodeIndex, FxHashSet<RcLocal>>,
    live_out: FxHashMap<NodeIndex, FxHashSet<RcLocal>>,
    loops_by_init: FxHashMap<NodeIndex, LoopInfo>,
    loops_by_header: FxHashMap<NodeIndex, LoopInfo>,
}

fn collect_closure_captures(
    closure: &ast::Closure,
    captured: &mut FxHashSet<RcLocal>,
    seen_closures: &mut FxHashSet<usize>,
) {
    // The same function body may be shared by multiple closure sites while
    // each site carries a distinct upvalue vector.  Account for this
    // occurrence's captures before using the body identity as a recursion
    // guard.
    captured.extend(closure.upvalues.iter().map(|upvalue| match upvalue {
        ast::Upvalue::Copy(local) | ast::Upvalue::Ref(local) => local.clone(),
    }));
    let identity = closure.function.0.as_ptr() as usize;
    if !seen_closures.insert(identity) {
        return;
    }
    let body = closure.function.lock().body.clone();
    collect_block_captures_with_seen(&body, captured, seen_closures);
}

fn collect_block_captures_with_seen(
    block: &Block,
    captured: &mut FxHashSet<RcLocal>,
    seen_closures: &mut FxHashSet<usize>,
) {
    for statement in block.iter() {
        collect_statement_captures_with_seen(statement, captured, seen_closures);
    }
}

fn collect_rvalue_captures(value: &RValue, captured: &mut FxHashSet<RcLocal>) {
    let mut seen_closures = FxHashSet::default();
    collect_rvalue_captures_with_seen(value, captured, &mut seen_closures);
}

fn collect_rvalue_captures_with_seen(
    value: &RValue,
    captured: &mut FxHashSet<RcLocal>,
    seen_closures: &mut FxHashSet<usize>,
) {
    // The traversal visits nested expressions but not the root value.
    if let RValue::Closure(closure) = value {
        collect_closure_captures(closure, captured, seen_closures);
    }
    let mut value_copy = value.clone();
    value_copy.traverse_rvalues(&mut |nested| {
        if let RValue::Closure(closure) = nested {
            collect_closure_captures(closure, captured, seen_closures);
        }
    });
}

fn collect_statement_captures(statement: &Statement, captured: &mut FxHashSet<RcLocal>) {
    let mut seen_closures = FxHashSet::default();
    collect_statement_captures_with_seen(statement, captured, &mut seen_closures);
}

fn collect_statement_captures_with_seen(
    statement: &Statement,
    captured: &mut FxHashSet<RcLocal>,
    seen_closures: &mut FxHashSet<usize>,
) {
    let mut statement_copy = statement.clone();
    statement_copy.traverse_rvalues(&mut |value| {
        if let RValue::Closure(closure) = value {
            collect_closure_captures(closure, captured, seen_closures);
        }
    });
    match statement {
        // `Traverse` intentionally stops at structured statement bodies, so
        // inspect those blocks explicitly to find closures hidden behind an
        // `If`, loop, or already-structured generic-for node.
        Statement::If(node) => {
            collect_block_captures_with_seen(&node.then_block.lock(), captured, seen_closures);
            collect_block_captures_with_seen(&node.else_block.lock(), captured, seen_closures);
        }
        Statement::While(node) => {
            collect_block_captures_with_seen(&node.block.lock(), captured, seen_closures);
        }
        Statement::Repeat(node) => {
            collect_block_captures_with_seen(&node.block.lock(), captured, seen_closures);
        }
        Statement::NumericFor(node) => {
            collect_block_captures_with_seen(&node.block.lock(), captured, seen_closures);
        }
        Statement::GenericFor(node) => {
            collect_block_captures_with_seen(&node.block.lock(), captured, seen_closures);
        }
        _ => {}
    }
}

fn block_has_rewritten_closure(
    block: &Block,
    rewrite: &FxHashMap<RcLocal, RcLocal>,
) -> bool {
    !rewrite.is_empty()
        && block.iter().any(|statement| {
            let mut captures = FxHashSet::default();
            collect_statement_captures(statement, &mut captures);
            captures.iter().any(|local| rewrite.contains_key(local))
        })
}

fn statement_captures_any(statement: &Statement, locals: &[RcLocal]) -> bool {
    let mut captures = FxHashSet::default();
    collect_statement_captures(statement, &mut captures);
    captures
        .iter()
        .any(|captured| locals.iter().any(|local| local == captured))
}

fn rvalue_captures_any(value: &RValue, locals: &[RcLocal]) -> bool {
    let mut captures = FxHashSet::default();
    collect_rvalue_captures(value, &mut captures);
    captures
        .iter()
        .any(|captured| locals.iter().any(|local| local == captured))
}

fn collect_closure_ref_captures(
    closure: &ast::Closure,
    captured: &mut FxHashSet<RcLocal>,
    seen_closures: &mut FxHashSet<usize>,
) {
    captured.extend(closure.upvalues.iter().filter_map(|upvalue| match upvalue {
        ast::Upvalue::Ref(local) => Some(local.clone()),
        ast::Upvalue::Copy(_) => None,
    }));
    let identity = closure.function.0.as_ptr() as usize;
    if !seen_closures.insert(identity) {
        return;
    }
    let body = closure.function.lock().body.clone();
    collect_block_ref_captures_with_seen(&body, captured, seen_closures);
}

fn collect_block_ref_captures_with_seen(
    block: &Block,
    captured: &mut FxHashSet<RcLocal>,
    seen_closures: &mut FxHashSet<usize>,
) {
    for statement in block.iter() {
        collect_statement_ref_captures_with_seen(statement, captured, seen_closures);
    }
}

fn collect_statement_ref_captures_with_seen(
    statement: &Statement,
    captured: &mut FxHashSet<RcLocal>,
    seen_closures: &mut FxHashSet<usize>,
) {
    let mut statement_copy = statement.clone();
    statement_copy.traverse_rvalues(&mut |value| {
        if let RValue::Closure(closure) = value {
            collect_closure_ref_captures(closure, captured, seen_closures);
        }
    });
    match statement {
        Statement::If(node) => {
            collect_block_ref_captures_with_seen(&node.then_block.lock(), captured, seen_closures);
            collect_block_ref_captures_with_seen(&node.else_block.lock(), captured, seen_closures);
        }
        Statement::While(node) => {
            collect_block_ref_captures_with_seen(&node.block.lock(), captured, seen_closures);
        }
        Statement::Repeat(node) => {
            collect_block_ref_captures_with_seen(&node.block.lock(), captured, seen_closures);
        }
        Statement::NumericFor(node) => {
            collect_block_ref_captures_with_seen(&node.block.lock(), captured, seen_closures);
        }
        Statement::GenericFor(node) => {
            collect_block_ref_captures_with_seen(&node.block.lock(), captured, seen_closures);
        }
        _ => {}
    }
}

fn collect_rvalue_ref_captures(value: &RValue, captured: &mut FxHashSet<RcLocal>) {
    let mut seen_closures = FxHashSet::default();
    if let RValue::Closure(closure) = value {
        collect_closure_ref_captures(closure, captured, &mut seen_closures);
    }
    let mut value_copy = value.clone();
    value_copy.traverse_rvalues(&mut |nested| {
        if let RValue::Closure(closure) = nested {
            collect_closure_ref_captures(closure, captured, &mut seen_closures);
        }
    });
}

fn statement_has_ref_capture_of(statement: &Statement, locals: &[RcLocal]) -> bool {
    let mut captured = FxHashSet::default();
    collect_statement_ref_captures_with_seen(statement, &mut captured, &mut FxHashSet::default());
    captured
        .iter()
        .any(|captured| locals.iter().any(|local| local == captured))
}

fn rvalue_has_ref_capture_of(value: &RValue, locals: &[RcLocal]) -> bool {
    let mut captured = FxHashSet::default();
    collect_rvalue_ref_captures(value, &mut captured);
    captured
        .iter()
        .any(|captured| locals.iter().any(|local| local == captured))
}

fn closure_contains_close(closure: &ast::Closure, seen_closures: &mut FxHashSet<usize>) -> bool {
    let identity = closure.function.0.as_ptr() as usize;
    if !seen_closures.insert(identity) {
        return false;
    }
    let body = closure.function.lock().body.clone();
    block_contains_close_with_seen(&body, seen_closures)
}

fn block_contains_close(block: &Block) -> bool {
    block_contains_close_with_seen(block, &mut FxHashSet::default())
}

fn rvalue_contains_close(value: &RValue) -> bool {
    let mut seen_closures = FxHashSet::default();
    rvalue_contains_close_with_seen(value, &mut seen_closures)
}

fn rvalue_contains_close_with_seen(
    value: &RValue,
    seen_closures: &mut FxHashSet<usize>,
) -> bool {
    if let RValue::Closure(closure) = value {
        if closure_contains_close(closure, seen_closures) {
            return true;
        }
    }
    let mut value_copy = value.clone();
    let mut nested_close = false;
    value_copy.traverse_rvalues(&mut |nested| {
        if !nested_close {
            if let RValue::Closure(closure) = nested {
                nested_close = closure_contains_close(closure, seen_closures);
            }
        }
    });
    nested_close
}

fn block_contains_close_with_seen(
    block: &Block,
    seen_closures: &mut FxHashSet<usize>,
) -> bool {
    block
        .iter()
        .any(|statement| statement_contains_close_with_seen(statement, seen_closures))
}

fn statement_contains_close_with_seen(
    statement: &Statement,
    seen_closures: &mut FxHashSet<usize>,
) -> bool {
    if matches!(statement, Statement::Close(_)) {
        return true;
    }
    let mut statement_copy = statement.clone();
    let mut nested_close = false;
    statement_copy.traverse_rvalues(&mut |value| {
        if !nested_close {
            if let RValue::Closure(closure) = value {
                nested_close = closure_contains_close(closure, seen_closures);
            }
        }
    });
    if nested_close {
        return true;
    }
    match statement {
        Statement::If(node) => {
            block_contains_close_with_seen(&node.then_block.lock(), seen_closures)
                || block_contains_close_with_seen(&node.else_block.lock(), seen_closures)
        }
        Statement::While(node) => {
            block_contains_close_with_seen(&node.block.lock(), seen_closures)
        }
        Statement::Repeat(node) => {
            block_contains_close_with_seen(&node.block.lock(), seen_closures)
        }
        Statement::NumericFor(node) => {
            block_contains_close_with_seen(&node.block.lock(), seen_closures)
        }
        Statement::GenericFor(node) => {
            block_contains_close_with_seen(&node.block.lock(), seen_closures)
        }
        _ => false,
    }
}

fn closure_contains_unlowered_control(
    closure: &ast::Closure,
    seen_closures: &mut FxHashSet<usize>,
) -> bool {
    let identity = closure.function.0.as_ptr() as usize;
    if !seen_closures.insert(identity) {
        return false;
    }
    let body = closure.function.lock().body.clone();
    block_contains_unlowered_control_with_seen(&body, seen_closures)
}

pub(crate) fn rvalue_contains_unlowered_control(value: &RValue) -> bool {
    let mut seen_closures = FxHashSet::default();
    rvalue_contains_unlowered_control_with_seen(value, &mut seen_closures)
}

fn rvalue_contains_unlowered_control_with_seen(
    value: &RValue,
    seen_closures: &mut FxHashSet<usize>,
) -> bool {
    if let RValue::Closure(closure) = value {
        if closure_contains_unlowered_control(closure, seen_closures) {
            return true;
        }
    }
    let mut value_copy = value.clone();
    let mut nested_control = false;
    value_copy.traverse_rvalues(&mut |nested| {
        if !nested_control {
            if let RValue::Closure(closure) = nested {
                nested_control = closure_contains_unlowered_control(closure, seen_closures);
            }
        }
    });
    nested_control
}

fn block_contains_unlowered_control_with_seen(
    block: &Block,
    seen_closures: &mut FxHashSet<usize>,
) -> bool {
    block
        .iter()
        .any(|statement| statement_contains_unlowered_control_with_seen(statement, seen_closures))
}

/// Recursively detect VM loop markers in an arbitrary AST block.  This is
/// shared with the certified fallback, which must reject markers hidden in
/// closure bodies and edge expressions just as the source-like preflight does.
pub(crate) fn block_contains_unlowered_control(block: &Block) -> bool {
    let mut seen_closures = FxHashSet::default();
    block_contains_unlowered_control_with_seen(block, &mut seen_closures)
}

fn statement_contains_unlowered_control_with_seen(
    statement: &Statement,
    seen_closures: &mut FxHashSet<usize>,
) -> bool {
    statement_contains_unlowered_control_with_seen_mode(statement, seen_closures, true)
}

fn statement_contains_unlowered_control_with_seen_mode(
    statement: &Statement,
    seen_closures: &mut FxHashSet<usize>,
    include_root_marker: bool,
) -> bool {
    let is_root_marker = matches!(
        statement,
        Statement::NumForInit(_)
            | Statement::NumForNext(_)
            | Statement::GenericForInit(_)
            | Statement::GenericForNext(_)
    );
    // Even when the marker itself is consumed by the outer CFG pass, its RHS
    // may contain a closure.  Scan nested values before applying the
    // root-marker exception so hidden protocol markers cannot bypass the
    // preflight.
    let mut statement_copy = statement.clone();
    let mut nested_control = false;
    statement_copy.traverse_rvalues(&mut |value| {
        if !nested_control {
            if let RValue::Closure(closure) = value {
                nested_control = closure_contains_unlowered_control(closure, seen_closures);
            }
        }
    });
    if nested_control || (include_root_marker && is_root_marker) {
        return true;
    }
    match statement {
        Statement::If(node) => {
            block_contains_unlowered_control_with_seen(&node.then_block.lock(), seen_closures)
                || block_contains_unlowered_control_with_seen(&node.else_block.lock(), seen_closures)
        }
        Statement::While(node) => {
            block_contains_unlowered_control_with_seen(&node.block.lock(), seen_closures)
        }
        Statement::Repeat(node) => {
            block_contains_unlowered_control_with_seen(&node.block.lock(), seen_closures)
        }
        Statement::NumericFor(node) => {
            block_contains_unlowered_control_with_seen(&node.block.lock(), seen_closures)
        }
        Statement::GenericFor(node) => {
            block_contains_unlowered_control_with_seen(&node.block.lock(), seen_closures)
        }
        _ => false,
    }
}

fn block_contains_hidden_unlowered_control(block: &Block) -> bool {
    let mut seen_closures = FxHashSet::default();
    block.iter().any(|statement| {
        // The top-level CFG markers are consumed by this pass; only markers
        // hidden in an already-structured child block or closure body make the
        // resulting source-like AST incomplete.
        let is_root_marker = matches!(
            statement,
            Statement::NumForInit(_)
                | Statement::NumForNext(_)
                | Statement::GenericForInit(_)
                | Statement::GenericForNext(_)
        );
        statement_contains_unlowered_control_with_seen_mode(
            statement,
            &mut seen_closures,
            !is_root_marker,
        )
    })
}

impl Analysis {
    fn new(function: &Function) -> Option<Self> {
        let entry = function.entry().as_ref().copied()?;
        let mut reachable = FxHashSet::default();
        let mut work = vec![entry];
        while let Some(node) = work.pop() {
            if !reachable.insert(node) {
                continue;
            }
            work.extend(function.successor_blocks(node));
        }
        let mut nodes = reachable.iter().copied().collect_vec();
        nodes.sort_by_key(|node| node.index());
        if nodes.is_empty() {
            return None;
        }

        // Edge arguments are SSA phi copies.  They are sourceable when the
        // destination is a local and the source block actually branches; a
        // parallel assignment is emitted on the edge below.  Reject malformed
        // transfers (duplicate destinations or a transfer after a terminal
        // statement) instead of silently dropping a value.
        for node in &nodes {
            let has_terminal = function.block(*node).is_some_and(|block| {
                block.last().is_some_and(|statement| {
                    matches!(
                        statement,
                        Statement::Return(_)
                            | Statement::Break(_)
                            | Statement::Continue(_)
                            | Statement::Goto(_)
                    )
                })
            });
            for edge in function.edges(*node) {
                if edge.weight().arguments.is_empty() {
                    continue;
                }
                if has_terminal {
                    return None;
                }
                let mut destinations = FxHashSet::default();
                if edge
                    .weight()
                    .arguments
                    .iter()
                    .any(|(destination, _)| !destinations.insert(destination.clone()))
                {
                    return None;
                }
            }
        }

        let dominators = simple_fast(function.graph(), entry);
        let post_dominators = Self::post_dominators(function, &nodes, &reachable);
        let (live_in, live_out) = Self::liveness(function, &nodes, &reachable);
        let (loops_by_init, loops_by_header) =
            Self::find_generic_loops(function, &nodes, &reachable, &dominators, &post_dominators)?;
        Some(Self {
            reachable,
            nodes,
            post_dominators,
            live_in,
            live_out,
            loops_by_init,
            loops_by_header,
        })
    }

    /// Standard backwards liveness over the complete reachable CFG.  Unlike
    /// the old `visited` heuristic, this fixed point follows loop backedges.
    /// Edge arguments are parallel transfers: their destinations are defined
    /// on the edge and their right-hand sides are read after branch selection.
    fn liveness(
        function: &Function,
        nodes: &[NodeIndex],
        reachable: &FxHashSet<NodeIndex>,
    ) -> (
        FxHashMap<NodeIndex, FxHashSet<RcLocal>>,
        FxHashMap<NodeIndex, FxHashSet<RcLocal>>,
    ) {
        let mut uses = FxHashMap::default();
        let mut defs = FxHashMap::default();
        for node in nodes {
            let mut node_uses = FxHashSet::default();
            let mut node_defs = FxHashSet::default();
            if let Some(block) = function.block(*node) {
                for statement in block.iter() {
                    for read in statement.values_read() {
                        if !node_defs.contains(read) {
                            node_uses.insert(read.clone());
                        }
                    }
                    node_defs.extend(statement.values_written().into_iter().cloned());
                }
            }
            uses.insert(*node, node_uses);
            defs.insert(*node, node_defs);
        }

        let mut live_in = nodes
            .iter()
            .copied()
            .map(|node| (node, FxHashSet::default()))
            .collect::<FxHashMap<_, _>>();
        let mut live_out = live_in.clone();
        let mut work = VecDeque::from(nodes.iter().rev().copied().collect_vec());
        let mut queued = nodes.iter().copied().collect::<FxHashSet<_>>();
        while let Some(node) = work.pop_front() {
            queued.remove(&node);
            let mut next_out = FxHashSet::default();
            for edge in function
                .edges(node)
                .filter(|edge| reachable.contains(&edge.target()))
            {
                let mut edge_live = live_in[&edge.target()].clone();
                // Edge arguments are simultaneous parallel copies.  Remove
                // every destination before adding any source, otherwise a
                // swap (`a <- b, b <- a`) would erase `b` while processing
                // the second pair and under-approximate edge liveness.
                for (destination, _) in &edge.weight().arguments {
                    edge_live.remove(destination);
                }
                for (_, value) in &edge.weight().arguments {
                    edge_live.extend(value.values_read().into_iter().cloned());
                }
                next_out.extend(edge_live);
            }
            let mut next_in = uses[&node].clone();
            next_in.extend(
                next_out
                    .iter()
                    .filter(|local| !defs[&node].contains(*local))
                    .cloned(),
            );
            if next_out != live_out[&node] || next_in != live_in[&node] {
                live_out.insert(node, next_out);
                live_in.insert(node, next_in);
                for predecessor in function
                    .predecessor_blocks(node)
                    .filter(|predecessor| reachable.contains(predecessor))
                {
                    if queued.insert(predecessor) {
                        work.push_back(predecessor);
                    }
                }
            }
        }
        (live_in, live_out)
    }

    /// Collect closure captures that can be established before a particular
    /// generic-for preparation executes.  A whole-function capture set is too
    /// broad: a closure created only in the loop body or after the loop cannot
    /// observe the iterator preparation.  Reverse reachability from the init
    /// block keeps the guard precise while still covering an indirect local
    /// callable initialized on any predecessor path.
    fn captured_locals_before_init(
        &self,
        function: &Function,
        init: NodeIndex,
    ) -> FxHashSet<RcLocal> {
        let mut pre_nodes = FxHashSet::default();
        let mut work = vec![init];
        while let Some(node) = work.pop() {
            if !self.reachable.contains(&node) || !pre_nodes.insert(node) {
                continue;
            }
            work.extend(
                function
                    .predecessor_blocks(node)
                    .filter(|predecessor| self.reachable.contains(predecessor)),
            );
        }

        let mut captured = FxHashSet::default();
        for node in &pre_nodes {
            let Some(block) = function.block(*node) else {
                continue;
            };
            let statements = if *node == init {
                let marker = block
                    .iter()
                    .position(|statement| statement.as_generic_for_init().is_some())
                    .unwrap_or(block.len());
                block.iter().take(marker).collect_vec()
            } else {
                block.iter().collect_vec()
            };
            for statement in statements {
                collect_statement_captures(statement, &mut captured);
            }
            for edge in function.edges(*node) {
                if pre_nodes.contains(&edge.target()) {
                    for (_, value) in &edge.weight().arguments {
                        collect_rvalue_captures(value, &mut captured);
                    }
                }
            }
        }
        captured
    }

    fn post_dominators(
        function: &Function,
        nodes: &[NodeIndex],
        reachable: &FxHashSet<NodeIndex>,
    ) -> FxHashMap<NodeIndex, FxHashSet<NodeIndex>> {
        let universe = nodes.iter().copied().collect::<FxHashSet<_>>();
        // A post-dominator is useful to this structurer only when the node
        // can reach a real terminal block.  Without this filter, a closed
        // SCC with no exit can retain the initial universe forever and look
        // like a valid join (there is no synthetic exit node in the CFG).
        let mut can_reach_terminal = FxHashSet::default();
        let terminals = nodes
            .iter()
            .copied()
            .filter(|node| {
                !function
                    .successor_blocks(*node)
                    .any(|successor| reachable.contains(&successor))
            })
            .collect_vec();
        let mut reverse_work = terminals.clone();
        while let Some(node) = reverse_work.pop() {
            if !can_reach_terminal.insert(node) {
                continue;
            }
            reverse_work.extend(
                function
                    .predecessor_blocks(node)
                    .filter(|predecessor| reachable.contains(predecessor)),
            );
        }
        let mut result = nodes
            .iter()
            .copied()
            .map(|node| {
                (
                    node,
                    can_reach_terminal
                        .contains(&node)
                        .then(|| universe.clone())
                        .unwrap_or_default(),
                )
            })
            .collect::<FxHashMap<_, _>>();
        // Fixed point with an implicit terminal.  A node with a successor in
        // a non-terminating SCC has no useful post-dominator: all paths do
        // not converge at a real exit, so its set is empty and the candidate
        // is rejected fail-closed by common_postdominator().
        // Re-evaluate only predecessors of a changed node.  The old full
        // graph sweep was O(V) rounds in the worst case; this worklist keeps
        // the same exact fixed point while avoiding repeated scans of large
        // unrelated regions.
        let mut work = VecDeque::from(nodes.to_vec());
        let mut queued = nodes.iter().copied().collect::<FxHashSet<_>>();
        while let Some(node) = work.pop_front() {
            queued.remove(&node);
            let successors = function
                .successor_blocks(node)
                .filter(|successor| reachable.contains(successor))
                .collect_vec();
            let mut next = if successors.is_empty() {
                // Real terminal: the implicit exit is not materialized in the
                // returned map, so the node post-dominates itself.
                [node].into_iter().collect()
            } else if successors
                .iter()
                .any(|successor| result[successor].is_empty())
            {
                FxHashSet::default()
            } else {
                let mut intersection = result[&successors[0]].clone();
                for successor in successors.iter().skip(1) {
                    intersection.retain(|candidate| result[successor].contains(candidate));
                }
                intersection.insert(node);
                intersection
            };
            // Nodes which cannot reach a terminal remain empty even if a
            // malformed graph presents them as a terminal through an
            // unreachable edge.
            if !can_reach_terminal.contains(&node) {
                next.clear();
            }
            if next != result[&node] {
                result.insert(node, next);
                for predecessor in function
                    .predecessor_blocks(node)
                    .filter(|predecessor| reachable.contains(predecessor))
                {
                    if queued.insert(predecessor) {
                        work.push_back(predecessor);
                    }
                }
            }
        }
        result
    }

    fn find_generic_loops(
        function: &Function,
        nodes: &[NodeIndex],
        reachable: &FxHashSet<NodeIndex>,
        dominators: &Dominators<NodeIndex>,
        post_dominators: &FxHashMap<NodeIndex, FxHashSet<NodeIndex>>,
    ) -> Option<(
        FxHashMap<NodeIndex, LoopInfo>,
        FxHashMap<NodeIndex, LoopInfo>,
    )> {
        let mut natural = FxHashMap::<NodeIndex, FxHashSet<NodeIndex>>::default();
        for source in nodes {
            for edge in function.edges(*source) {
                let header = edge.target();
                if !reachable.contains(&header)
                    || !dominators
                        .dominators(*source)
                        .is_some_and(|mut ds| ds.any(|candidate| candidate == header))
                {
                    continue;
                }
                let set = natural.entry(header).or_default();
                set.insert(header);
                set.insert(*source);
                // An empty generic-for body is represented by a self-looping
                // FORGLOOP header.  There is no natural-loop predecessor to
                // walk in that case; walking the header's ordinary
                // predecessors would incorrectly absorb the init/tail chain
                // into the loop and make the candidate appear multi-entry.
                if *source == header {
                    continue;
                }
                let mut reverse = vec![*source];
                while let Some(node) = reverse.pop() {
                    for predecessor in function.predecessor_blocks(node) {
                        if !reachable.contains(&predecessor) || !set.insert(predecessor) {
                            continue;
                        }
                        if predecessor != header {
                            reverse.push(predecessor);
                        }
                    }
                }
            }
        }

        // A compiler-emitted generic-for is a semantic loop even when every
        // body path exits before reaching FORGLOOP again (for example an
        // always-break or always-return body).  Such a loop has no dominance
        // backedge and therefore does not occur in `natural`.  Seed those
        // candidates from the identity-bearing provenance as well.  The
        // ownership walk is deliberately conservative: every body node must
        // be dominated by the body entry, and traversal stops at the
        // exhaustion/follow edge or at an outer target.  This prevents a
        // straight-line tail after the loop from being swallowed by the body.
        let mut candidates = natural;
        let semantic_headers = nodes
            .iter()
            .copied()
            .filter_map(|header| {
                let next = function
                    .block(header)
                    .and_then(|block| block.last())
                    .and_then(|statement| statement.as_generic_for_next())?;
                let origin = next.origin()?;
                let (then_edge, else_edge) = function.conditional_edges(header)?;
                let body_entry = then_edge.target();
                let normal_exit = else_edge.target();
                let direct_break = body_entry == normal_exit;
                if !reachable.contains(&body_entry) {
                    return None;
                }
                // Luau can compile an unconditional `break` without a
                // separate body block: both FORGLOOP arms target the follow
                // block, so the provenance body target aliases follow.  This
                // is still a source-level loop whose body is exactly
                // `break`; only accept the alias when the PC envelope agrees.
                if direct_break && origin.body_pc != origin.follow_pc {
                    return None;
                }
                // Production lifter output carries exact PC ranges.  Require
                // the CFG branch targets to agree with the provenance envelope
                // whenever ranges are available; this is what distinguishes a
                // direct outer exit from the source loop's own follow block.
                if function
                    .block_at_pc(origin.step_pc)
                    .is_some_and(|node| node != header)
                    || function
                        .block_at_pc(origin.body_pc)
                        .is_some_and(|node| {
                            node != body_entry
                                && !(origin.body_pc == origin.step_pc
                                    && function
                                        .block(body_entry)
                                        .is_some_and(|block| block.is_empty()))
                        })
                    || function
                        .block_at_pc(origin.follow_pc)
                        .is_some_and(|node| node != normal_exit)
                {
                    return None;
                }
                let mut owned = FxHashSet::default();
                owned.insert(header);
                if direct_break {
                    return Some((header, owned, origin));
                }
                let mut work = vec![body_entry];
                while let Some(node) = work.pop() {
                    if node == header || node == normal_exit {
                        continue;
                    }
                    if !reachable.contains(&node) || !owned.insert(node) {
                        continue;
                    }
                    // A body node with an incoming path that bypasses the
                    // FORGLOOP header is a shared/multi-entry region, not a
                    // single source-level loop body.
                    if !dominators
                        .dominators(node)
                        .is_some_and(|mut ds| ds.any(|candidate| candidate == body_entry))
                    {
                        return None;
                    }
                    if let Some(range) = function.block_pc_range(node)
                        && (range.start < origin.body_pc || range.start >= origin.follow_pc)
                    {
                        // A block outside the compiler-emitted body envelope
                        // belongs to an ancestor/tail region even when the
                        // whole-function dominator tree says it is reachable
                        // only through this body path.
                        return None;
                    }
                    for edge in function.edges(node) {
                        let target = edge.target();
                        if !reachable.contains(&target)
                            || target == header
                            || target == normal_exit
                        {
                            continue;
                        }
                        if dominators
                            .dominators(target)
                            .is_some_and(|mut ds| ds.any(|candidate| candidate == body_entry))
                        {
                            work.push(target);
                        }
                        // Otherwise this is an outer/terminal target.  It is
                        // intentionally left outside the owned set so the
                        // normal path builder can classify it as break/return.
                    }
                }
                owned.contains(&body_entry).then_some((header, owned, origin))
            })
            .collect_vec();
        for (header, owned, _origin) in semantic_headers {
            // Natural-loop discovery sees only paths that return to the
            // FORGLOOP header.  A body arm that terminates with `return` (or
            // another terminal transfer) is still owned by the source-level
            // loop, but is absent from that reverse backedge walk.  Merge the
            // provenance-seeded ownership into an existing natural candidate
            // so those terminal arms are represented instead of becoming
            // spurious external exits with no common post-dominator.
            candidates
                .entry(header)
                .and_modify(|existing| existing.extend(owned.iter().copied()))
                .or_insert(owned);
        }

        let mut infos = Vec::new();
        let mut seen_origins = FxHashSet::default();
        for (header, nodes_in_loop) in candidates {
            let Some(next) = function
                .block(header)
                .and_then(|block| block.last())
                .and_then(|statement| statement.as_generic_for_next())
            else {
                // Unmodelled numeric/irreducible cycles are handled by the
                // existing path and dispatcher structurers.
                continue;
            };
            let (then_edge, else_edge) = function.conditional_edges(header)?;
            let body_entry = then_edge.target();
            let normal_exit = else_edge.target();
            let direct_break = body_entry == normal_exit;
            if direct_break {
                // The direct-break form has no body node: the follow block is
                // shared by both FORGLOOP arms and must remain outside the
                // loop so the surrounding path can consume it once.
                if nodes_in_loop.len() != 1 {
                    return None;
                }
            } else if !nodes_in_loop.contains(&body_entry) || nodes_in_loop.contains(&normal_exit) {
                return None;
            }
            // A natural loop with an entry other than its header cannot be
            // represented by a single source `for`.
            for node in &nodes_in_loop {
                if *node != header
                    && function
                        .predecessor_blocks(*node)
                        .any(|predecessor| !nodes_in_loop.contains(&predecessor))
                {
                    return None;
                }
            }
            let inits = function
                .predecessor_blocks(header)
                .filter(|predecessor| !nodes_in_loop.contains(predecessor))
                .filter(|predecessor| {
                    function.block(*predecessor).is_some_and(|block| {
                        block
                            .iter()
                            .any(|statement| statement.as_generic_for_init().is_some())
                    })
                })
                .collect_vec();
            if inits.len() != 1
                || function.predecessor_blocks(header).any(|predecessor| {
                    !nodes_in_loop.contains(&predecessor) && predecessor != inits[0]
                })
            {
                return None;
            }
            let init = inits[0];
            let init_statement = function
                .block(init)
                .and_then(|block| block.iter().find_map(|s| s.as_generic_for_init()))?;
            let init_locals = init_statement
                .0
                .left
                .iter()
                .map(LValue::as_local)
                .collect::<Option<Vec<_>>>()?;
            if init_locals.len() != 3
                || init_locals.iter().enumerate().any(|(index, local)| {
                    init_locals
                        .iter()
                        .skip(index + 1)
                        .any(|other| other == local)
                })
                || next.generator != RValue::Local(init_locals[0].clone())
                || next.state != RValue::Local(init_locals[1].clone())
                || next.control != init_locals[2].clone()
            {
                return None;
            }
            let res_locals = next
                .res_locals
                .iter()
                .map(|lvalue| lvalue.as_local().cloned())
                .collect::<Option<Vec<_>>>()?;
            if res_locals.is_empty() || init_statement.0.right.is_empty() {
                return None;
            }
            // Every production generic-for marker must carry an exact
            // prep/step pair.  A missing pair is indistinguishable from an
            // older/custom protocol encoding and is therefore fail-closed;
            // tests that construct markers by hand attach an explicit test
            // origin before entering this proof.
            let Some(origin) = (match (init_statement.origin(), next.origin()) {
                (Some(init_origin), Some(next_origin))
                    if init_origin == next_origin
                        && init_origin.result_count as usize == res_locals.len()
                        && (direct_break == (init_origin.body_pc == init_origin.follow_pc))
                        && init_origin.step_pc != init_origin.prep_pc =>
                {
                    if !seen_origins.insert(init_origin) {
                        return None;
                    }
                    Some(init_origin)
                }
                _ => None,
            }) else {
                return None;
            };
            // When the lifter supplied PC envelopes, every non-header node
            // owned by this candidate must lie between the compiler's body
            // target and the instruction after FORGLOOP.  Apply this check to
            // natural candidates too: otherwise a malformed/transformed CFG
            // whose semantic ownership proof failed could still be accepted
            // by the older backedge-only set and swallow an outer tail.
            if function
                .block_at_pc(origin.step_pc)
                .is_some_and(|node| node != header)
                || function
                    .block_at_pc(origin.body_pc)
                    .is_some_and(|node| {
                        node != body_entry
                            && !(origin.body_pc == origin.step_pc
                                && function
                                    .block(body_entry)
                                    .is_some_and(|block| block.is_empty()))
                    })
                || function
                    .block_at_pc(origin.follow_pc)
                    .is_some_and(|node| node != normal_exit)
                || nodes_in_loop.iter().any(|node| {
                    *node != header
                        && function.block_pc_range(*node).is_some_and(|range| {
                            range.start < origin.body_pc || range.start >= origin.follow_pc
                        })
                })
            {
                return None;
            }
            let external_targets = nodes_in_loop
                .iter()
                .flat_map(|node| function.successor_blocks(*node))
                .filter(|target| !nodes_in_loop.contains(target))
                .unique()
                .collect_vec();
            let join = common_postdominator(&external_targets, post_dominators)?;
            if nodes_in_loop.contains(&join) {
                return None;
            }
            infos.push(LoopInfo {
                header,
                init,
                body_entry,
                normal_exit,
                join,
                nodes: nodes_in_loop,
                res_locals,
                right: init_statement.0.right.clone(),
                origin: Some(origin),
            });
        }
        for (index, left) in infos.iter().enumerate() {
            for right in infos.iter().skip(index + 1) {
                let overlap = left.nodes.intersection(&right.nodes).count();
                if overlap != 0
                    && !(left.nodes.is_subset(&right.nodes) || right.nodes.is_subset(&left.nodes))
                {
                    return None;
                }
            }
        }
        let mut by_init = FxHashMap::default();
        let mut by_header = FxHashMap::default();
        for info in infos {
            if by_init.insert(info.init, info.clone()).is_some()
                || by_header.insert(info.header, info).is_some()
            {
                return None;
            }
        }
        Some((by_init, by_header))
    }
}

fn common_postdominator(
    targets: &[NodeIndex],
    post_dominators: &FxHashMap<NodeIndex, FxHashSet<NodeIndex>>,
) -> Option<NodeIndex> {
    let first = *targets.first()?;
    let mut common = post_dominators.get(&first)?.clone();
    for target in targets.iter().skip(1) {
        common.retain(|candidate| post_dominators[target].contains(candidate));
    }
    // Common post-dominators form a chain when a unique structured join
    // exists.  The closest join is the candidate whose own post-dominator set
    // contains every other common candidate.  This avoids a BFS from every
    // target to every candidate (which made nested large CFGs unnecessarily
    // expensive); incomparable candidates are ambiguous and fail closed.
    let mut candidates = common.iter().copied().collect_vec();
    candidates.sort_by_key(|candidate| candidate.index());
    candidates.into_iter().find(|candidate| {
        post_dominators
            .get(candidate)
            .is_some_and(|post_dominators_of_candidate| {
                common.iter().all(|other| {
                    *other == *candidate || post_dominators_of_candidate.contains(other)
                })
            })
    })
}

struct LoopContext<'a> {
    info: &'a LoopInfo,
    exports: &'a [(RcLocal, RcLocal)],
}

struct PathResult {
    block: Block,
    next: Option<NodeIndex>,
}

struct Builder<'a> {
    function: &'a Function,
    analysis: Analysis,
    visited: FxHashSet<NodeIndex>,
    rewrite: FxHashMap<RcLocal, RcLocal>,
    protected_locals: FxHashSet<RcLocal>,
    unsafe_reason: Option<UnsafeStructureReason>,
}

impl<'a> Builder<'a> {
    fn new(
        function: &'a Function,
        analysis: Analysis,
        protected_locals: FxHashSet<RcLocal>,
    ) -> Self {
        let suffix_unsafe_reason = analysis.loops_by_init.values().find_map(|info| {
            let block = function.block(info.init)?;
            let marker = block
                .iter()
                .position(|statement| statement.as_generic_for_init().is_some())?;
            let suffix = block
                .iter()
                .skip(marker + 1)
                .filter(|statement| !is_ignorable(statement));
            if suffix
                .clone()
                .any(|statement| !is_reorderable_for_init_suffix(statement))
            {
                return Some(UnsafeStructureReason::ForInitSuffixOrder);
            }
            // A callable local on the iterator RHS may invoke any closure
            // reachable from this function.  Without a value-flow summary,
            // writes to a captured cell established before this preparation
            // must remain fail-closed even when the RHS contains no inline
            // closure node.  Captures created only after the loop are outside
            // the iterator's observation window.
            let pre_init_captures = analysis.captured_locals_before_init(function, info.init);
            if suffix.clone().any(|statement| {
                statement
                    .values_written()
                    .into_iter()
                    .any(|written| pre_init_captures.contains(written))
            }) {
                return Some(UnsafeStructureReason::CapturedCellReorder);
            }
            let right_captures = info.right.iter().fold(
                FxHashSet::default(),
                |mut captures, value| {
                    collect_rvalue_captures(value, &mut captures);
                    captures
                },
            );
            if suffix.clone().any(|statement| {
                statement
                    .values_written()
                    .into_iter()
                    .any(|written| right_captures.contains(written))
            }) {
                return Some(UnsafeStructureReason::CapturedCellReorder);
            }
            None
        });
        let unsafe_reason = suffix_unsafe_reason
            .or_else(|| {
                analysis.nodes.iter().any(|node| {
                    function
                        .block(*node)
                        .is_some_and(block_contains_hidden_unlowered_control)
                        || function.edges(*node).any(|edge| {
                            edge.weight().arguments.iter().any(|(_, value)| {
                                rvalue_contains_unlowered_control(value)
                            })
                        })
                })
                .then_some(UnsafeStructureReason::UnmodeledControl)
            })
            .or_else(|| {
            analysis.nodes.iter().any(|node| {
                function.block(*node).is_some_and(block_contains_close)
                    || function.edges(*node).any(|edge| {
                        edge.weight().arguments.iter().any(|(_, value)| {
                            rvalue_contains_close(value)
                        })
                })
            })
            .then_some(UnsafeStructureReason::UnmodeledClose)
        });
        Self {
            function,
            analysis,
            visited: FxHashSet::default(),
            rewrite: FxHashMap::default(),
            protected_locals,
            unsafe_reason,
        }
    }

    fn reject_unsafe<T>(&mut self, reason: UnsafeStructureReason) -> Option<T> {
        self.unsafe_reason.get_or_insert(reason);
        None
    }

    fn rewrite_statement(&self, mut statement: Statement) -> Statement {
        for local in statement.values_read_mut() {
            if let Some(replacement) = self.rewrite.get(local) {
                *local = replacement.clone();
            }
        }
        for local in statement.values_written_mut() {
            if let Some(replacement) = self.rewrite.get(local) {
                *local = replacement.clone();
            }
        }
        statement
    }

    fn rewrite_rvalue(&self, mut value: RValue) -> RValue {
        for local in value.values_read_mut() {
            if let Some(replacement) = self.rewrite.get(local) {
                *local = replacement.clone();
            }
        }
        value
    }

    /// Reconcile rewrite maps after two conditional arms.  A nested loop may
    /// introduce an export only on one arm; that mapping is branch-local unless
    /// the continuation actually reads the exported SSA local.  The previous
    /// implementation rejected every such difference, which sent otherwise
    /// structured optimizer diamonds (including Pet's refresh path) to the
    /// synthetic dispatcher.  Keep common mappings, retain the incoming map
    /// for branch-local differences, and fail closed when a differing mapping
    /// is live at the actual continuation.  `Analysis::live_in` is a complete
    /// CFG fixed point, so enclosing-loop backedges are included even when the
    /// corresponding block was already emitted by this recursive traversal.
    fn reconcile_rewrite(
        &mut self,
        base: &FxHashMap<RcLocal, RcLocal>,
        then_map: &FxHashMap<RcLocal, RcLocal>,
        else_map: &FxHashMap<RcLocal, RcLocal>,
        continuation: Option<NodeIndex>,
    ) -> Option<FxHashMap<RcLocal, RcLocal>> {
        let mut keys = base.keys().cloned().collect::<FxHashSet<_>>();
        keys.extend(then_map.keys().cloned());
        keys.extend(else_map.keys().cloned());
        let mut merged = base.clone();
        for key in keys {
            let then_value = then_map.get(&key);
            let else_value = else_map.get(&key);
            if then_value == else_value {
                match then_value {
                    Some(value) => {
                        merged.insert(key, value.clone());
                    }
                    None => {
                        merged.remove(&key);
                    }
                }
                continue;
            }
            let used_after = continuation.is_some_and(|node| {
                self.analysis
                    .live_in
                    .get(&node)
                    .is_some_and(|live| live.contains(&key))
            });
            if used_after {
                return self.reject_unsafe(UnsafeStructureReason::LiveBranchRewrite);
            }
            // No continuation observes this branch-local export.  Restore the
            // incoming mapping (if any), never leaking a child-only rewrite.
            match base.get(&key) {
                Some(value) => {
                    merged.insert(key, value.clone());
                }
                None => {
                    merged.remove(&key);
                }
            }
        }
        Some(merged)
    }

    /// A loop that is conditionally entered can introduce an export mapping
    /// on only one arm.  The loop arm initializes its fresh export to nil;
    /// materialize the bypass arm's incoming SSA value before the common
    /// continuation.  This preserves both the loop-result exhaustion value
    /// and the pre-existing value when the loop is skipped, without requiring
    /// the loop init block to dominate the join.  Existing mappings are
    /// intentionally left fail-closed: copying an outer mapping into the
    /// bypass arm would conflate two lexical loop-result cells.
    fn materialize_optional_export_gaps(
        &self,
        base: &FxHashMap<RcLocal, RcLocal>,
        then_map: &mut FxHashMap<RcLocal, RcLocal>,
        else_map: &mut FxHashMap<RcLocal, RcLocal>,
        continuation: Option<NodeIndex>,
        both_arms_reach_continuation: bool,
        then_block: &mut Block,
        else_block: &mut Block,
    ) -> Option<()> {
        let Some(join) = continuation else {
            return Some(());
        };
        let mut keys = base.keys().cloned().collect::<FxHashSet<_>>();
        keys.extend(then_map.keys().cloned());
        keys.extend(else_map.keys().cloned());
        for key in keys {
            let then_value = then_map.get(&key).cloned();
            let else_value = else_map.get(&key).cloned();
            if then_value == else_value {
                continue;
            }
            // A single `PathResult` successor cannot represent a conditional
            // whose arms leave through different ports (for example,
            // `continue` versus `break`).  Never synthesize a copy into the
            // non-reaching arm: doing so would publish a rewrite for a path
            // that cannot execute the assignment, and could append it after a
            // terminal statement.  The caller rejects this mixed-port rewrite
            // before constructing the conditional.
            if !both_arms_reach_continuation {
                return None;
            }
            let used_after = self
                .analysis
                .live_in
                .get(&join)
                .is_some_and(|live| live.contains(&key));
            if !used_after {
                continue;
            }
            match (then_value, else_value) {
                (Some(export), None) if !base.contains_key(&key) => {
                    if !then_block
                        .iter()
                        .any(|statement| Self::is_nil_assignment(statement, &export))
                    {
                        return None;
                    }
                    insert_before_terminal(
                        else_block,
                        Assign::new(
                            vec![LValue::Local(export.clone())],
                            vec![RValue::Local(key.clone())],
                        )
                        .into(),
                    );
                    else_map.insert(key, export);
                }
                (None, Some(export)) if !base.contains_key(&key) => {
                    if !else_block
                        .iter()
                        .any(|statement| Self::is_nil_assignment(statement, &export))
                    {
                        return None;
                    }
                    insert_before_terminal(
                        then_block,
                        Assign::new(
                            vec![LValue::Local(export.clone())],
                            vec![RValue::Local(key.clone())],
                        )
                        .into(),
                    );
                    then_map.insert(key, export);
                }
                _ => return None,
            }
        }
        Some(())
    }

    fn exports_for(&self, info: &LoopInfo) -> Vec<(RcLocal, RcLocal)> {
        info.res_locals
            .iter()
            .filter(|local| {
                self.analysis.nodes.iter().any(|node| {
                    if info.nodes.contains(node) {
                        return false;
                    }
                    let read_in_block = self.function.block(*node).is_some_and(|block| {
                        block.iter().any(|statement| {
                            statement
                                .values_read()
                                .into_iter()
                                .any(|read| read == *local)
                        })
                    });
                    let read_on_edge = self.function.edges(*node).any(|edge| {
                        edge.weight().arguments.iter().any(|(_, value)| {
                            value.values_read().into_iter().any(|read| read == *local)
                        })
                    });
                    read_in_block || read_on_edge
                })
            })
            .cloned()
            .map(|local| (local, RcLocal::default()))
            .collect()
    }

    fn is_nil_assignment(statement: &Statement, local: &RcLocal) -> bool {
        matches!(
            statement,
            Statement::Assign(assign)
                if assign.left.len() == 1
                    && assign.right.len() == 1
                    && assign.left[0].as_local() == Some(local)
                    && matches!(assign.right[0], RValue::Literal(Literal::Nil))
        )
    }

    fn normal_adapter_nodes(
        &self,
        info: &LoopInfo,
        exports: &[(RcLocal, RcLocal)],
    ) -> Option<Vec<NodeIndex>> {
        let mut nil_writes = FxHashSet::default();
        let mut nodes = Vec::new();
        let mut current = info.normal_exit;
        let mut seen = FxHashSet::default();
        while current != info.join {
            if !seen.insert(current) || info.nodes.contains(&current) {
                return None;
            }
            let block = self.function.block(current)?;
            for statement in block.iter() {
                for local in &info.res_locals {
                    if Self::is_nil_assignment(statement, local) {
                        nil_writes.insert(local.clone());
                    }
                }
                if !is_ignorable(statement)
                    && !info
                        .res_locals
                        .iter()
                        .any(|local| Self::is_nil_assignment(statement, local))
                {
                    return None;
                }
            }
            nodes.push(current);
            let edges = self.function.edges(current).collect_vec();
            if edges.len() != 1 || edges[0].weight().branch_type != BranchType::Unconditional {
                return None;
            }
            // This path represents the implicit exhaustion edge of the
            // source-level `for`.  Its outgoing edge is currently consumed
            // by the loop as a boundary, so there is no emitted statement
            // slot for an SSA transfer before the post-loop join.  Dropping
            // such a transfer would lose observable writes (for example
            // `sink = 42` on exhaustion); reject until the exhaustion port
            // models edge effects explicitly.
            if !edges[0].weight().arguments.is_empty() {
                return None;
            }
            current = edges[0].target();
        }
        // Every live result must receive the VM's exhaustion value before the
        // post-loop join.  A direct exhaustion edge has no separate adapter
        // block, so account for an explicit nil write in the normal-exit/join
        // block because the traversal above stops at the join boundary.
        // Otherwise a live result would be exported from the source-level
        // loop's nil-initialized outer binding even though FORGLOOP may retain
        // its last value.  Adapter paths are held to the same requirement;
        // the VM does not guarantee that even the first result slot is cleared
        // when iteration exhausts.
        let direct_exit_nil_writes = nodes.is_empty().then(|| {
            let mut writes = FxHashSet::default();
            if let Some(block) = self.function.block(info.normal_exit) {
                for statement in block.iter() {
                    if is_ignorable(statement) {
                        continue;
                    }
                    let Some(local) = info
                        .res_locals
                        .iter()
                        .find(|local| Self::is_nil_assignment(statement, local))
                    else {
                        // A later nil store cannot prove what an earlier read,
                        // call, or other observable statement saw.  Require a
                        // contiguous exhaustion-write prefix before exposing
                        // the rewritten export to the rest of the join.
                        break;
                    };
                    writes.insert(local.clone());
                }
            }
            writes
        });
        if exports.iter().any(|(local, _)| {
            let proven = if nodes.is_empty() {
                direct_exit_nil_writes
                    .as_ref()
                    .is_some_and(|writes| writes.contains(local))
            } else {
                nil_writes.contains(local)
            };
            !proven
        }) {
            return None;
        }
        Some(nodes)
    }

    fn has_unsafe_export_write(
        &self,
        info: &LoopInfo,
        exports: &[(RcLocal, RcLocal)],
        adapters: &[NodeIndex],
    ) -> bool {
        let adapters = adapters.iter().copied().collect::<FxHashSet<_>>();
        let direct_normal_exit = adapters.is_empty() && info.normal_exit == info.join;
        let mut pre_init_nodes = FxHashSet::default();
        let mut work = vec![info.init];
        while let Some(node) = work.pop() {
            if !self.analysis.reachable.contains(&node)
                || info.nodes.contains(&node)
                || !pre_init_nodes.insert(node)
            {
                continue;
            }
            work.extend(
                self.function
                    .predecessor_blocks(node)
                    .filter(|predecessor| self.analysis.reachable.contains(predecessor)),
            );
        }
        exports.iter().any(|(local, _)| {
            self.analysis.nodes.iter().any(|node| {
                // A value written before the preparation is the incoming
                // value that a bypass arm must preserve.  Writes after the
                // loop (other than the proven nil exhaustion adapter) remain
                // unsafe because they would alter the exported iteration
                // result before the common continuation.
                if pre_init_nodes.contains(node) {
                    return false;
                }
                self.function.block(*node).is_some_and(|block| {
                    block.iter().any(|statement| {
                        if !statement
                            .values_written()
                            .into_iter()
                            .any(|written| written == local)
                        {
                            return false;
                        }
                        // The header marker is the VM write that creates the
                        // iterator result.  A normal-exhaustion adapter may
                        // write nil, and a body break can share that adapter;
                        // every other write would change the value observed
                        // by the post-loop export and is therefore rejected.
                        (*node != info.header || !matches!(statement, Statement::GenericForNext(_)))
                            && !((adapters.contains(node)
                                || (direct_normal_exit && *node == info.normal_exit))
                                && Self::is_nil_assignment(statement, local))
                    })
                })
            })
        })
    }

    fn has_unsafe_captured_result_write(&self, info: &LoopInfo) -> bool {
        info.res_locals.iter().any(|result| {
            let captured_in_loop = info.nodes.iter().any(|node| {
                self.function.block(*node).is_some_and(|block| {
                    block.iter().any(|statement| {
                        let mut captures = FxHashSet::default();
                        collect_statement_captures(statement, &mut captures);
                        captures.contains(result)
                    })
                }) || self.function.edges(*node).any(|edge| {
                    edge.weight().arguments.iter().any(|(_, value)| {
                        let mut captures = FxHashSet::default();
                        collect_rvalue_captures(value, &mut captures);
                        captures.contains(result)
                    })
                })
            });
            captured_in_loop
                && self.analysis.nodes.iter().any(|node| {
                    if info.nodes.contains(node) {
                        return false;
                    }
                    let block_write = self.function.block(*node).is_some_and(|block| {
                        block.iter().any(|statement| {
                            statement
                                .values_written()
                                .into_iter()
                                .any(|written| written == result)
                        })
                    });
                    let edge_write = self.function.edges(*node).any(|edge| {
                        edge.weight()
                            .arguments
                            .iter()
                            .any(|(destination, _)| destination == result)
                    });
                    block_write || edge_write
                })
        })
    }

    fn has_unsafe_ref_captured_result(&self, info: &LoopInfo) -> bool {
        info.nodes.iter().any(|node| {
            self.function.block(*node).is_some_and(|block| {
                block
                    .iter()
                    .any(|statement| statement_has_ref_capture_of(statement, &info.res_locals))
            }) || self.function.edges(*node).any(|edge| {
                edge.weight().arguments.iter().any(|(_, value)| {
                    rvalue_has_ref_capture_of(value, &info.res_locals)
                })
            })
        })
    }

    fn has_unsafe_captured_result_escape(&self, info: &LoopInfo) -> bool {
        self.analysis.nodes.iter().any(|node| {
            if info.nodes.contains(node) {
                return false;
            }
            self.function.block(*node).is_some_and(|block| {
                block
                    .iter()
                    .any(|statement| statement_captures_any(statement, &info.res_locals))
            }) || self.function.edges(*node).any(|edge| {
                edge.weight().arguments.iter().any(|(_, value)| {
                    rvalue_captures_any(value, &info.res_locals)
                })
            })
        })
    }

    fn append_export(&self, block: &mut Block, exports: &[(RcLocal, RcLocal)]) {
        if exports.is_empty() {
            return;
        }
        let mut assignment = Assign::new(
            exports
                .iter()
                .map(|(_, export)| LValue::Local(export.clone()))
                .collect(),
            exports
                .iter()
                .map(|(local, _)| {
                    RValue::Local(
                        self.rewrite
                            .get(local)
                            .cloned()
                            .unwrap_or_else(|| local.clone()),
                    )
                })
                .collect(),
        );
        assignment.parallel = exports.len() > 1;
        block.push(assignment.into());
    }

    /// Materialize an SSA phi transfer on the selected CFG edge.  Luau
    /// evaluates every right-hand side before assigning any destination, so a
    /// single `parallel` assignment preserves swaps and cycles without
    /// inventing an ordering between the copies.
    fn edge_transfer(
        &self,
        edge: &cfg::block::BlockEdge,
        rewrite: &FxHashMap<RcLocal, RcLocal>,
    ) -> Option<Block> {
        if edge.arguments.is_empty() {
            return Some(Block::default());
        }
        // Rewriting a closure's upvalue vector does not rewrite references in
        // its child function body (the traversal intentionally stops at the
        // closure boundary).  Emitting such an edge transfer would therefore
        // make the closure's metadata and body refer to different cells.
        // Reject the edge until closure-body rewriting has a scope-aware
        // implementation.
        if edge.arguments.iter().any(|(_, value)| {
            let mut captures = FxHashSet::default();
            collect_rvalue_captures(value, &mut captures);
            captures.iter().any(|local| rewrite.contains_key(local))
        }) {
            return None;
        }
        let left = edge
            .arguments
            .iter()
            .map(|(destination, _)| {
                LValue::Local(
                    rewrite
                        .get(destination)
                        .cloned()
                        .unwrap_or_else(|| destination.clone()),
                )
            })
            .collect_vec();
        let right = edge
            .arguments
            .iter()
            .map(|(_, value)| self.rewrite_rvalue_with(value.clone(), rewrite))
            .collect_vec();
        let mut assignment = Assign::new(left, right);
        assignment.parallel = true;
        Some(Block::from(vec![assignment.into()]))
    }

    fn rewrite_rvalue_with(
        &self,
        mut value: RValue,
        rewrite: &FxHashMap<RcLocal, RcLocal>,
    ) -> RValue {
        for local in value.values_read_mut() {
            if let Some(replacement) = rewrite.get(local) {
                *local = replacement.clone();
            }
        }
        value
    }

    fn build_loop(&mut self, info: &LoopInfo) -> Option<PathResult> {
        if !self.visited.insert(info.init) {
            return None;
        }
        let init_block = self.function.block(info.init)?;
        if block_has_rewritten_closure(init_block, &self.rewrite) {
            return None;
        }
        let init_index = init_block
            .iter()
            .position(|statement| statement.as_generic_for_init().is_some())?;
        let init_edges = self.function.edges(info.init).collect_vec();
        if init_edges.len() != 1
            || init_edges[0].target() != info.header
            || !matches!(
                init_edges[0].weight().branch_type,
                cfg::block::BranchType::Unconditional
            )
        {
            return None;
        }
        // The two FORGLOOP exits are consumed by the source-level `for`
        // node itself.  Their edge-local phi copies would have to be placed
        // before either the body entry or the exhaustion adapter; emitting
        // them in the surrounding path would be too late.  Reject this shape
        // until those two protocol ports have explicit transfer slots.
        if self
            .function
            .edges(info.header)
            .any(|edge| !edge.weight().arguments.is_empty())
        {
            return None;
        }
        // Optimized Luau may leave a small, pure setup suffix in the same
        // FORGPREP block after the marker (for example `local seen = {}`).
        // The source `for` evaluates its iterator expression before this
        // suffix in the bytecode, so moving the suffix before the emitted
        // `for` is only legal when it is total/pure and independent of the
        // iterator RHS. Calls, metamethod-sensitive expressions, dynamic
        // table keys, and data dependencies remain fail-closed.
        let init_suffix = init_block
            .iter()
            .skip(init_index + 1)
            .filter(|statement| !is_ignorable(statement))
            .cloned()
            .collect_vec();
        // A suffix is emitted before the source-level `for`, so accept it only
        // after the explicit purity/dependency/cell checks below prove that
        // this tuple-staging commute cannot change an observable event.  The
        // checks reject calls, reads, protocol/result aliases, protected or
        // captured writes, and every non-local assignment.
        if init_suffix
            .iter()
            .any(|statement| !is_reorderable_for_init_suffix(statement))
        {
            return self.reject_unsafe(UnsafeStructureReason::ForInitSuffixOrder);
        }
        if init_block
            .iter()
            .take(init_index)
            .any(|statement| !is_linear_statement(statement))
        {
            return None;
        }
        let mut output: Block = init_block
            .iter()
            .take(init_index)
            .cloned()
            .map(|statement| self.rewrite_statement(statement))
            .collect_vec()
            .into();
        let right_reads = info
            .right
            .iter()
            .flat_map(|value| value.values_read())
            .cloned()
            .collect::<FxHashSet<_>>();
        let right_captures = info.right.iter().fold(
            FxHashSet::default(),
            |mut captures, value| {
                collect_rvalue_captures(value, &mut captures);
                captures
            },
        );
        // A total/pure suffix expression can still read a local whose value is
        // changed indirectly by an observable iterator RHS (for example, a
        // call through a closure).  The IR has no effect summary precise
        // enough to prove that such a read is stable, so keep the commute
        // fail-closed unless the suffix is read-free.
        if init_suffix
            .iter()
            .any(|statement| !statement.values_read().is_empty())
        {
            return None;
        }
        if init_suffix.iter().any(|statement| {
            statement.values_written().into_iter().any(|written| {
                right_reads.contains(written) || self.protected_locals.contains(written)
            })
        }) {
            return None;
        }
        if init_suffix.iter().any(|statement| {
            statement
                .values_written()
                .into_iter()
                .any(|written| right_captures.contains(written))
        }) {
            // A closure in the iterator RHS may observe this cell while the
            // iterator is prepared.  Moving the suffix write before that call
            // would change the captured value.  Captures that occur only in
            // the loop body are safe: the suffix already executes before the
            // first FORGLOOP iteration, so moving it before the source-level
            // `for` preserves the value seen when those closures are created.
            return self.reject_unsafe(UnsafeStructureReason::CapturedCellReorder);
        }
        // The RHS can call through an existing local closure.  Since this
        // structurer cannot summarize indirect callable effects precisely,
        // keep any write to a closure-captured cell established before this
        // preparation fail-closed.  Closures created only in the loop body or
        // after it cannot observe the iterator call and are safe to ignore.
        let pre_init_captures = self
            .analysis
            .captured_locals_before_init(self.function, info.init);
        if init_suffix.iter().any(|statement| {
            statement
                .values_written()
                .into_iter()
                .any(|written| pre_init_captures.contains(written))
        }) {
            return self.reject_unsafe(UnsafeStructureReason::CapturedCellReorder);
        }
        output.extend(
            init_suffix
                .iter()
                .cloned()
                .map(|statement| self.rewrite_statement(statement)),
        );
        let exports = self.exports_for(info);
        let adapters = self.normal_adapter_nodes(info, &exports)?;
        if self.has_unsafe_ref_captured_result(info) {
            return self.reject_unsafe(UnsafeStructureReason::CapturedLoopResultRef);
        }
        if self.has_unsafe_export_write(info, &exports, &adapters)
            || exports
                .iter()
                .any(|(local, _)| self.rewrite.contains_key(local))
            || self.has_unsafe_captured_result_write(info)
            || self.has_unsafe_captured_result_escape(info)
        {
            return None;
        }
        for (_, export) in &exports {
            output.push(
                Assign::new(vec![LValue::Local(export.clone())], vec![RValue::Literal(
                    Literal::Nil,
                )])
                .into(),
            );
        }
        if !self.visited.insert(info.header) {
            return None;
        }
        let header = self.function.block(info.header)?;
        if header
            .iter()
            .take(header.len().saturating_sub(1))
            .any(|statement| !is_ignorable(statement))
        {
            return None;
        }
        let next = header.last()?.as_generic_for_next()?;
        let init_locals = init_block
            .get(init_index)?
            .as_generic_for_init()?
            .0
            .left
            .iter()
            .map(LValue::as_local)
            .collect::<Option<Vec<_>>>()?;
        if next.res_locals.len() != info.res_locals.len()
            || init_locals.len() != 3
            || next.generator != RValue::Local(init_locals[0].clone())
            || next.state != RValue::Local(init_locals[1].clone())
            || next.control != init_locals[2].clone()
        {
            return None;
        }
        let protocol_locals = [
            init_locals[0].clone(),
            init_locals[1].clone(),
            init_locals[2].clone(),
        ];
        // Edge-local SSA transfers are ordinary source assignments only when
        // their values are visible to the source-level construct.  A
        // FORGLOOP updates the hidden control register as part of the VM
        // iterator protocol; materialising an edge argument that reads or
        // writes any protocol local would turn that hidden update into a
        // visible assignment (or feed a stale visible value back into the
        // next iterator call).  In particular, a body -> header backedge can
        // carry a phi copy into `control`; there is no source `for` transfer
        // slot at that point, so reject the candidate until the protocol edge
        // effects are modelled explicitly.
        let edge_touches_protocol = |edge: &BlockEdge| {
            edge.arguments.iter().any(|(destination, value)| {
                protocol_locals.iter().any(|protocol| protocol == destination)
                    || value
                        .values_read()
                        .into_iter()
                        .any(|read| protocol_locals.iter().any(|protocol| protocol == read))
                    || rvalue_captures_any(value, &protocol_locals)
            })
        };
        // The init marker evaluates the iterator RHS in its block.  An
        // init -> header phi copy executes only after that evaluation, but a
        // source-shaped `for` has no pre-loop transfer slot: emitting it in
        // the surrounding path would run it before the RHS, while emitting it
        // in the body would run it once per iteration.  Reject every such
        // transfer until tuple staging gives it an exact placement.
        if !init_edges[0].weight().arguments.is_empty() {
            return self.reject_unsafe(UnsafeStructureReason::ForInitEdgeTransferOrder);
        }
        if self.analysis.nodes.iter().any(|node| {
            self.function
                .edges(*node)
                .any(|edge| edge_touches_protocol(edge.weight()))
        }) {
            return self.reject_unsafe(UnsafeStructureReason::ForProtocolEdgeTransfer);
        }
        if init_suffix.iter().any(|statement| {
            statement
                .values_read()
                .into_iter()
                .chain(statement.values_written())
                .any(|local| {
                    protocol_locals.iter().any(|protocol| protocol == local)
                        || info.res_locals.iter().any(|result| result == local)
                })
                || statement_captures_any(statement, &protocol_locals)
        }) {
            return None;
        }
        // Export rewrites are intentionally persistent after a loop so later
        // straight-line code observes the live-out binding.  A subsequent
        // loop must nevertheless never reuse one of those SSA identities:
        // doing so would rewrite its own iterator/result references to the
        // previous loop's export and silently merge two distinct lexical
        // cells (especially visible through a closure in the second body).
        if protocol_locals
            .iter()
            .chain(info.res_locals.iter())
            .any(|local| self.rewrite.contains_key(local))
        {
            return None;
        }
        // Function parameters and already-linked upvalue cells have
        // function/closure scope.  A source `for` introduces fresh loop
        // bindings for its result tuple and hides the VM protocol locals;
        // accepting an SSA register that aliases one of those protected
        // cells would turn a parameter/upvalue mutation into a shadowed local
        // and change what callers or closures observe after the loop.
        if protocol_locals
            .iter()
            .chain(info.res_locals.iter())
            .any(|local| self.protected_locals.contains(local))
        {
            return None;
        }
        // Result registers are distinct VM slots from the iterator protocol
        // tuple.  If malformed SSA/local rewriting aliases one of them, a
        // source `for` would introduce a child-scoped binding that shadows a
        // live generator/state/control value; rejecting the candidate is the
        // only semantics-preserving choice.
        if next.res_locals.iter().any(|lvalue| {
            lvalue
                .as_local()
                .is_none_or(|result| protocol_locals.iter().any(|protocol| protocol == result))
        }) || info.res_locals.iter().enumerate().any(|(index, result)| {
            info.res_locals
                .iter()
                .skip(index + 1)
                .any(|other| other == result)
        }) {
            return None;
        }
        // `GenericFor` intentionally hides the VM iterator protocol.  The
        // FORGLOOP instruction nevertheless updates its hidden control
        // register on every iteration, so a read/capture of generator/state/
        // control anywhere outside the two marker statements cannot be
        // represented by ordinary source syntax without changing its value.
        // Reject the whole shape instead of silently emitting a stale local.
        let touches_protocol = |statement: &Statement| {
            statement
                .values_read()
                .into_iter()
                .chain(statement.values_written())
                .any(|local| protocol_locals.iter().any(|protocol| protocol == local))
                || statement_captures_any(statement, &protocol_locals)
        };
        if self.analysis.nodes.iter().any(|node| {
            *node != info.header
                && *node != info.init
                && self
                    .function
                    .block(*node)
                    .is_some_and(|block| block.iter().any(touches_protocol))
        }) {
            return None;
        }
        if init_block.iter().take(init_index).any(touches_protocol)
            || init_block.iter().any(|statement| {
                statement_captures_any(statement, &protocol_locals)
            })
            || init_block
                .iter()
                .find_map(|statement| statement.as_generic_for_init())
                .is_some_and(|init| {
                    init.values_read()
                        .into_iter()
                        .any(|local| protocol_locals.iter().any(|protocol| protocol == local))
                })
        {
            return None;
        }
        if info.nodes.iter().any(|node| {
            *node != info.header
                && self.function.block(*node).is_some_and(|block| {
                    block.iter().any(|statement| {
                        statement.values_written().into_iter().any(|written| {
                            protocol_locals.iter().any(|protocol| written == protocol)
                        })
                    })
                })
        }) {
            return None;
        }
        // A result register must not be captured by a closure before loop
        // entry: a post-loop rewrite would then change which cell that closure
        // observes.  Ordinary writes before init are allowed when the loop is
        // conditionally bypassed; the bypass arm copies that incoming value to
        // the fresh export.  Writes in the init suffix remain rejected by the
        // suffix protocol checks below.
        // Walk backwards from the init rather than relying only on dominance:
        // a closure created on one branch of a preheader need not dominate the
        // init, but it is still able to retain the old register.
        let mut pre_init_nodes = FxHashSet::default();
        pre_init_nodes.insert(info.init);
        let mut pre_init_work = vec![info.init];
        while let Some(node) = pre_init_work.pop() {
            for predecessor in self
                .function
                .predecessor_blocks(node)
                .filter(|predecessor| self.analysis.reachable.contains(predecessor))
            {
                if info.nodes.contains(&predecessor) || !pre_init_nodes.insert(predecessor) {
                    continue;
                }
                pre_init_work.push(predecessor);
            }
        }
        if pre_init_nodes.iter().any(|node| {
            // Reverse reachability from an inner init can pass through an
            // enclosing loop's back edge and revisit the normal-exhaustion
            // adapter.  Those nodes are validated separately above; their
            // intentional nil writes are not pre-entry aliases.
            if adapters.contains(node)
                || (*node == info.normal_exit
                    && info.normal_exit == info.join
                    && self.function.block(*node).is_some_and(|block| {
                        block.iter().any(|statement| {
                            info.res_locals
                                .iter()
                                .any(|local| Self::is_nil_assignment(statement, local))
                        })
                    }))
            {
                return false;
            }
            self.function.block(*node).is_some_and(|block| {
                block.iter().any(|statement| {
                    let writes_result = *node == info.init
                        && block
                            .iter()
                            .skip(init_index + 1)
                            .any(|statement| {
                                statement.values_written().into_iter().any(|local| {
                                    info.res_locals.iter().any(|result| result == local)
                                })
                            });
                    let mut captures = FxHashSet::default();
                    collect_statement_captures(statement, &mut captures);
                    let captures_result = captures
                        .iter()
                        .any(|captured| info.res_locals.iter().any(|result| result == captured));
                    writes_result || captures_result
                })
            })
            || (node != &info.init
                && self.function.edges(*node).any(|edge| {
                    edge.weight().arguments.iter().any(|(_, value)| {
                        let mut captures = FxHashSet::default();
                        collect_rvalue_captures(value, &mut captures);
                        captures
                            .iter()
                            .any(|captured| info.res_locals.iter().any(|result| result == captured))
                    })
                }))
        }) {
            return None;
        }
        let context = LoopContext {
            info,
            exports: &exports,
        };
        // The iterator RHS is evaluated before the loop body.  Capture its
        // rewrite environment now; nested loops in the body may introduce
        // exports for locals that happen to share an SSA identity, but those
        // exports must not retroactively rewrite the outer iterator setup.
        let right = info
            .right
            .iter()
            .cloned()
            .map(|value| self.rewrite_rvalue(value))
            .collect();
        let body_result = if info.body_entry == info.normal_exit {
            // The compiler's unconditional-break shape aliases the body and
            // follow targets.  Do not walk the follow block into the loop;
            // materialise the source-level break and let the outer path visit
            // that block after the loop.
            PathResult {
                block: Block::from(vec![Statement::Break(ast::Break {}).into()]),
                next: Some(info.join),
            }
        } else {
            self.build_path(info.body_entry, Some(info.header), Some(&context))?
        };
        // A terminal body path (e.g. `return value`) has no successor.  It is
        // still a valid source-level loop body; the outer path resumes at the
        // exhaustion join for any remaining CFG path.
        if body_result.next != Some(info.header)
            && body_result.next != Some(info.join)
            && body_result.next.is_some()
        {
            return None;
        }
        let mut generic_for = GenericFor::new(info.res_locals.clone(), right, body_result.block);
        generic_for.origin = info.origin;
        output.push(generic_for.into());
        for node in adapters {
            // A body-side break may legally share the normal-exhaustion
            // adapter (for example, a compiler-generated `result = nil`
            // block can be the target of both the header's Else edge and a
            // nested condition).  The break path has already consumed that
            // node and emitted its statements; consuming it a second time
            // would reject a semantically valid CFG.  Any other duplicate is
            // still rejected by build_path/build_exit_adapter before this
            // point.
            self.visited.insert(node);
        }
        for (local, export) in &exports {
            self.rewrite.insert(local.clone(), export.clone());
        }
        Some(PathResult {
            block: output,
            next: Some(info.join),
        })
    }

    fn build_path(
        &mut self,
        start: NodeIndex,
        stop: Option<NodeIndex>,
        context: Option<&LoopContext<'_>>,
    ) -> Option<PathResult> {
        let mut output = Block::default();
        let mut current = start;
        loop {
            if Some(current) == stop || context.is_some_and(|ctx| current == ctx.info.header) {
                return Some(PathResult {
                    block: output,
                    next: Some(current),
                });
            }
            if self.analysis.loops_by_header.contains_key(&current) {
                return None;
            }
            if !self.visited.insert(current) {
                return None;
            }
            if let Some(info) = self.analysis.loops_by_init.get(&current).cloned() {
                if let Some(ctx) = context {
                    // A source-level nested loop may only join back inside
                    // its enclosing loop.  If the inferred join is outside
                    // the enclosing region (for example, a body edge that
                    // jumps directly to the parent's exit), emitting an
                    // inner `break` would execute only one Luau `break` and
                    // accidentally continue the parent.  Reject the shape so
                    // the semantics-preserving fallback handles the outer
                    // escape explicitly.
                    if !info.nodes.is_subset(&ctx.info.nodes)
                        || !ctx.info.nodes.contains(&info.join)
                    {
                        return None;
                    }
                }
                self.visited.remove(&current);
                let nested = self.build_loop(&info)?;
                output.extend(nested.block.0);
                current = nested.next?;
                continue;
            }
            let block = self.function.block(current)?;
            if block_has_rewritten_closure(block, &self.rewrite) {
                return None;
            }
            if block.iter().any(|statement| {
                matches!(
                    statement,
                    Statement::GenericForInit(_) | Statement::GenericForNext(_)
                )
            }) {
                return None;
            }
            let successors = self.function.successor_blocks(current).collect_vec();
            match successors.as_slice() {
                [] => {
                    if block.iter().enumerate().any(|(index, statement)| {
                        !is_linear_statement(statement)
                            && !(index + 1 == block.len()
                                && matches!(statement, Statement::Return(_)))
                    }) {
                        // A structured statement here owns mutable nested
                        // blocks, but this path has no CFG branch metadata with
                        // which to rebuild those blocks.  In particular,
                        // `If::values_read/values_written` intentionally only
                        // expose the condition, so copying it would make loop
                        // export/liveness analysis unsound.  Let the existing
                        // semantics-preserving structurer handle the shape.
                        return None;
                    }
                    output.extend(
                        block
                            .iter()
                            .cloned()
                            .map(|statement| self.rewrite_statement(statement)),
                    );
                    return Some(PathResult {
                        block: output,
                        next: None,
                    });
                }
                [target] => {
                    let edges = self.function.edges(current).collect_vec();
                    if edges.len() != 1
                        || edges[0].target() != *target
                        || edges[0].weight().branch_type != BranchType::Unconditional
                    {
                        // A one-successor block is only a straight-line
                        // region when its edge is explicitly unconditional;
                        // accepting a malformed Then/Else edge would erase
                        // branch semantics while still looking readable.
                        return None;
                    }
                    if block
                        .iter()
                        .any(|statement| !is_linear_statement(statement))
                    {
                        // Do not pass through a pre-structured If/loop when
                        // the CFG has only one successor.  Its nested body is
                        // not represented by this edge and cannot safely be
                        // rewritten under a loop-result export map.
                        return None;
                    }
                    output.extend(
                        block
                            .iter()
                            .cloned()
                            .map(|statement| self.rewrite_statement(statement)),
                    );
                    output.extend(self.edge_transfer(edges[0].weight(), &self.rewrite)?.0);
                    if let Some(ctx) = context {
                        // A legitimate inner `break` may first land in a
                        // linear adapter block that belongs to an enclosing
                        // loop.  Ownership is proven by `build_exit_adapter`
                        // below (single successor, no unsafe statements, and
                        // the exact inner join); rejecting every node inside
                        // an ancestor would reject the Pet-shaped CFG.
                        if *target == ctx.info.header {
                            // A straight-line backedge is ordinary loop
                            // fall-through, not an explicit source-level
                            // `continue`.  Conditional transfer arms still
                            // materialize `Continue` below, where it carries
                            // real branch-control information.
                            return Some(PathResult {
                                block: output,
                                next: Some(*target),
                            });
                        }
                        if !ctx.info.nodes.contains(target) {
                            // This edge originates in the loop body, not in
                            // the FORGLOOP header. Even when it happens to
                            // target the header's normal-exhaustion adapter,
                            // that adapter is part of the body break path and
                            // must execute before the break.
                            let adapter = self.build_exit_adapter(*target, ctx.info.join, ctx)?;
                            output.extend(adapter.block.0);
                            self.append_export(&mut output, ctx.exports);
                            output.push(Statement::Break(ast::Break {}).into());
                            return Some(PathResult {
                                block: output,
                                next: Some(ctx.info.join),
                            });
                        }
                    }
                    current = *target;
                }
                [_, _] => {
                    let statement = block.last()?.clone();
                    let if_statement = statement.as_if()?.clone();
                    // The CFG owns the branch bodies.  A pre-populated AST
                    // body here would be discarded when we rebuild the If,
                    // so let the existing structurer handle that shape.
                    if !if_statement.then_block.lock().is_empty()
                        || !if_statement.else_block.lock().is_empty()
                    {
                        return None;
                    }
                    // Branch order in a StableDiGraph is not semantic.  Always
                    // use the explicit Then/Else tags from the CFG edge.
                    let (then_edge, else_edge) = self.function.conditional_edges(current)?;
                    let then_target = then_edge.target();
                    let else_target = else_edge.target();
                    let prefix = block.iter().take(block.len() - 1).cloned().collect_vec();
                    if prefix
                        .iter()
                        .any(|statement| !is_linear_statement(statement))
                    {
                        return None;
                    }
                    let prefix = prefix
                        .into_iter()
                        .map(|statement| self.rewrite_statement(statement))
                        .collect_vec();
                    output.extend(prefix);
                    let conditional = self.build_conditional(
                        current,
                        if_statement,
                        then_target,
                        else_target,
                        context,
                    )?;
                    output.extend(conditional.block.0);
                    // A conditional consumes both arms up to their common
                    // post-dominator.  Continue at that join so a normal
                    // top-level diamond can be followed by its tail, and so a
                    // nested conditional can be followed by the rest of its
                    // enclosing loop body.  The loop/header stop check at the
                    // top of this loop turns the next boundary into the
                    // appropriate `PathResult` for the caller.
                    let Some(next) = conditional.next else {
                        return Some(PathResult {
                            block: output,
                            next: None,
                        });
                    };
                    current = next;
                }
                _ => return None,
            }
        }
    }

    fn build_exit_adapter(
        &mut self,
        start: NodeIndex,
        join: NodeIndex,
        context: &LoopContext<'_>,
    ) -> Option<PathResult> {
        let mut output = Block::default();
        let mut current = start;
        while current != join {
            if context.info.nodes.contains(&current) || !self.visited.insert(current) {
                return None;
            }
            let block = self.function.block(current)?;
            if block_has_rewritten_closure(block, &self.rewrite) {
                return None;
            }
            if block.iter().any(|statement| {
                !is_ignorable(statement)
                    && !context
                        .info
                        .res_locals
                        .iter()
                        .any(|local| Self::is_nil_assignment(statement, local))
            }) {
                return None;
            }
            output.extend(
                block
                    .iter()
                    .cloned()
                    .map(|statement| self.rewrite_statement(statement)),
            );
            let successors = self.function.successor_blocks(current).collect_vec();
            if successors.len() != 1 {
                return None;
            }
            let edges = self.function.edges(current).collect_vec();
            if edges.len() != 1
                || edges[0].target() != successors[0]
                || edges[0].weight().branch_type != BranchType::Unconditional
            {
                return None;
            }
            output.extend(self.edge_transfer(edges[0].weight(), &self.rewrite)?.0);
            current = successors[0];
        }
        Some(PathResult {
            block: output,
            next: Some(join),
        })
    }

    fn build_conditional(
        &mut self,
        source: NodeIndex,
        statement: If,
        then_target: NodeIndex,
        else_target: NodeIndex,
        context: Option<&LoopContext<'_>>,
    ) -> Option<PathResult> {
        if let Some(ctx) = context {
            let inside_join =
                common_postdominator(&[then_target, else_target], &self.analysis.post_dominators)
                    .filter(|join| *join != ctx.info.header && ctx.info.nodes.contains(join));
            if let Some(join) = inside_join {
                // Rewrites created by a nested loop are path-sensitive.  Do
                // not let a loop that is only present in one arm leak its
                // export mapping into the other arm (or into the join).
                let base_rewrite = self.rewrite.clone();
                let then_transfer = self.edge_transfer(
                    self.function
                        .edges(source)
                        .find(|edge| {
                            edge.target() == then_target
                                && edge.weight().branch_type == BranchType::Then
                        })?
                        .weight(),
                    &base_rewrite,
                )?;
                let then_result = self.build_path(then_target, Some(join), Some(ctx))?;
                let mut then_rewrite = self.rewrite.clone();
                self.rewrite = base_rewrite.clone();
                let else_transfer = self.edge_transfer(
                    self.function
                        .edges(source)
                        .find(|edge| {
                            edge.target() == else_target
                                && edge.weight().branch_type == BranchType::Else
                        })?
                        .weight(),
                    &base_rewrite,
                )?;
                let else_result = self.build_path(else_target, Some(join), Some(ctx))?;
                // A terminal arm has no path to the inside join.  Do not let
                // optional-export materialization mutate that arm before we
                // reject the mixed-port conditional: doing so would either
                // append a copy after `break`/`return` or make an unreachable
                // arm look as if it contributed to the join environment.
                if then_result.next != Some(join) || else_result.next != Some(join) {
                    return None;
                }
                let mut else_rewrite = self.rewrite.clone();
                let mut then_block = then_transfer;
                then_block.extend(then_result.block.0);
                let mut else_block = else_transfer;
                else_block.extend(else_result.block.0);
                self.materialize_optional_export_gaps(
                    &base_rewrite,
                    &mut then_rewrite,
                    &mut else_rewrite,
                    Some(join),
                    true,
                    &mut then_block,
                    &mut else_block,
                )?;
                self.rewrite = self.reconcile_rewrite(
                    &base_rewrite,
                    &then_rewrite,
                    &else_rewrite,
                    Some(join),
                )?;
                let mut condition = statement.condition.clone();
                for local in condition.values_read_mut() {
                    if let Some(replacement) = base_rewrite.get(local) {
                        *local = replacement.clone();
                    }
                }
                simplify_conditional(&mut condition, &mut then_block, &mut else_block);
                return Some(PathResult {
                    block: Block::from(vec![If::new(condition, then_block, else_block).into()]),
                    next: Some(join),
                });
            }
            let base_rewrite = self.rewrite.clone();
            let then_edge = self
                .function
                .edges(source)
                .find(|edge| {
                    edge.target() == then_target && edge.weight().branch_type == BranchType::Then
                })?
                .weight()
                .clone();
            let then_transfer = self.edge_transfer(&then_edge, &base_rewrite)?;
            let then_result = self.build_transfer_arm(then_target, ctx)?;
            let mut then_rewrite = self.rewrite.clone();
            self.rewrite = base_rewrite.clone();
            let else_edge = self
                .function
                .edges(source)
                .find(|edge| {
                    edge.target() == else_target && edge.weight().branch_type == BranchType::Else
                })?
                .weight()
                .clone();
            let else_transfer = self.edge_transfer(&else_edge, &base_rewrite)?;
            let else_result = self.build_transfer_arm(else_target, ctx)?;
            let mut else_rewrite = self.rewrite.clone();
            let continuation = (then_result.next == Some(ctx.info.header)
                || else_result.next == Some(ctx.info.header))
                .then_some(ctx.info.header);
            let both_reach_continuation = continuation.is_some_and(|join| {
                then_result.next == Some(join) && else_result.next == Some(join)
            });
            // `PathResult` carries one successor, while a loop conditional can
            // in reality have multiple terminal ports (for example,
            // `continue` on one arm and `break` on the other).  A rewrite
            // created by only one such arm cannot be published globally: the
            // non-reaching arm has no continuation environment to receive the
            // export, and the post-loop path is distinct from the header.
            // Reject the mixed-port rewrite before optional-gap materialization
            // so no assignment can be appended after a terminal statement.
            if !both_reach_continuation && then_rewrite != else_rewrite {
                return self.reject_unsafe(UnsafeStructureReason::LiveBranchRewrite);
            }
            let mut then_block = then_transfer;
            then_block.extend(then_result.block.0);
            let mut else_block = else_transfer;
            else_block.extend(else_result.block.0);
            self.materialize_optional_export_gaps(
                &base_rewrite,
                &mut then_rewrite,
                &mut else_rewrite,
                continuation,
                both_reach_continuation,
                &mut then_block,
                &mut else_block,
            )?;
            self.rewrite =
                self.reconcile_rewrite(&base_rewrite, &then_rewrite, &else_rewrite, continuation)?;
            let mut condition = statement.condition.clone();
            for local in condition.values_read_mut() {
                if let Some(replacement) = base_rewrite.get(local) {
                    *local = replacement.clone();
                }
            }
            strip_terminal_continue(&mut then_block);
            strip_terminal_continue(&mut else_block);
            simplify_conditional(&mut condition, &mut then_block, &mut else_block);
            Some(PathResult {
                block: Block::from(vec![If::new(condition, then_block, else_block).into()]),
                next: Some(ctx.info.header),
            })
        } else {
            let join =
                common_postdominator(&[then_target, else_target], &self.analysis.post_dominators);
            let base_rewrite = self.rewrite.clone();
            let then_transfer = self.edge_transfer(
                self.function
                    .edges(source)
                    .find(|edge| {
                        edge.target() == then_target
                            && edge.weight().branch_type == BranchType::Then
                    })?
                    .weight(),
                &base_rewrite,
            )?;
            let then_result = self.build_path(then_target, join, None)?;
            let mut then_rewrite = self.rewrite.clone();
            self.rewrite = base_rewrite.clone();
            let else_transfer = self.edge_transfer(
                self.function
                    .edges(source)
                    .find(|edge| {
                        edge.target() == else_target
                            && edge.weight().branch_type == BranchType::Else
                    })?
                    .weight(),
                &base_rewrite,
            )?;
            let else_result = self.build_path(else_target, join, None)?;
            let mut else_rewrite = self.rewrite.clone();
            let mut then_block = then_transfer;
            then_block.extend(then_result.block.0);
            let mut else_block = else_transfer;
            else_block.extend(else_result.block.0);
            self.materialize_optional_export_gaps(
                &base_rewrite,
                &mut then_rewrite,
                &mut else_rewrite,
                join,
                join.is_some_and(|join| {
                    then_result.next == Some(join) && else_result.next == Some(join)
                }),
                &mut then_block,
                &mut else_block,
            )?;
            self.rewrite =
                self.reconcile_rewrite(&base_rewrite, &then_rewrite, &else_rewrite, join)?;
            if then_result.next != join || else_result.next != join {
                return None;
            }
            let mut condition = statement.condition.clone();
            for local in condition.values_read_mut() {
                if let Some(replacement) = base_rewrite.get(local) {
                    *local = replacement.clone();
                }
            }
            simplify_conditional(&mut condition, &mut then_block, &mut else_block);
            Some(PathResult {
                block: Block::from(vec![If::new(condition, then_block, else_block).into()]),
                next: join,
            })
        }
    }

    fn build_transfer_arm(
        &mut self,
        target: NodeIndex,
        context: &LoopContext<'_>,
    ) -> Option<PathResult> {
        if target == context.info.header {
            return Some(PathResult {
                block: Block::from(vec![Statement::Continue(ast::Continue {}).into()]),
                next: Some(target),
            });
        }
        if context.info.nodes.contains(&target) {
            return self.build_path(target, Some(context.info.header), Some(context));
        }
        // Do not reject an ancestor-owned adapter solely by set membership:
        // `build_exit_adapter` proves that it is the unique path from this
        // arm to the current loop join.  A direct ancestor escape cannot pass
        // that proof because its header has already been visited (and any
        // cycle/ambiguous path is rejected there).
        let mut block = Block::default();
        // This is a transfer from inside the loop body. A target equal to
        // `normal_exit` is still a body-side break and must run that adapter;
        // only the header's Else edge represents implicit exhaustion.
        let adapter = self.build_exit_adapter(target, context.info.join, context)?;
        block.extend(adapter.block.0);
        self.append_export(&mut block, context.exports);
        block.push(Statement::Break(ast::Break {}).into());
        Some(PathResult {
            block,
            next: Some(context.info.join),
        })
    }
}

fn is_ignorable(statement: &Statement) -> bool {
    matches!(statement, Statement::Comment(_) | Statement::Empty(_))
}

fn is_linear_statement(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Call(_)
            | Statement::MethodCall(_)
            | Statement::Assign(_)
            | Statement::Close(_)
            | Statement::SetList(_)
            | Statement::Comment(_)
            | Statement::Empty(_)
    )
}

/// Positive allow-list for statements that may be commuted across a generic
/// iterator preparation marker.  In particular, `Close` is intentionally not
/// included: it changes upvalue lifetime even though its `LocalRw` summary is
/// empty.  Calls, metamethod-sensitive writes, and indexed/global stores are
/// likewise rejected until iterator provenance/effects are available.
fn is_reorderable_for_init_suffix(statement: &Statement) -> bool {
    let Statement::Assign(assign) = statement else {
        return false;
    };
    !assign.left.is_empty()
        && assign
            .left
            .iter()
            .all(|left| matches!(left, LValue::Local(_)))
        && !assign.right.is_empty()
        && assign.right.iter().all(ast::is_total_pure)
}

fn strip_terminal_continue(block: &mut Block) {
    let Some(index) = block
        .0
        .iter()
        .rposition(|statement| !is_ignorable(statement))
    else {
        return;
    };
    if matches!(block.0[index], Statement::Continue(_)) {
        block.0.remove(index);
    }
}

/// Place an edge-value copy before an explicit transfer.  `break`, `continue`
/// and `return` terminate a Luau block, so appending a copy after one would
/// either produce invalid source or rely on cleanup to delete a write that the
/// value-flow analysis already considered observable.
fn insert_before_terminal(block: &mut Block, statement: Statement) {
    let index = block
        .0
        .iter()
        .rposition(|candidate| !is_ignorable(candidate))
        .filter(|&index| {
            matches!(
                block.0[index],
                Statement::Break(_) | Statement::Continue(_) | Statement::Return(_)
            )
        })
        .unwrap_or(block.len());
    block.insert(index, statement);
}

fn simplify_conditional(condition: &mut RValue, then_block: &mut Block, else_block: &mut Block) {
    if then_block.is_empty() && !else_block.is_empty() {
        *condition = Unary::new(condition.clone(), UnaryOperation::Not).reduce_condition();
        std::mem::swap(then_block, else_block);
    }
}

fn global_is_named(global: &ast::Global, name: &str) -> bool {
    global.0 == name.as_bytes()
}

fn call_is_named(value: &RValue, name: &str) -> bool {
    let call = match value {
        RValue::Call(call) => call,
        RValue::Select(ast::Select::Call(call)) => call,
        _ => return false,
    };
    matches!(call.value.as_ref(), RValue::Global(global) if global_is_named(global, name))
}

/// Prove that a specialized prep opcode has the source expression whose VM
/// fast path it assumes.  `FORGPREP_NEXT` is emitted for `pairs(...)` (or the
/// explicit `next, table` tuple), while `FORGPREP_INEXT` assumes `ipairs(...)`.
/// Arbitrary RHS values with those opcodes are malformed/custom bytecode and
/// must remain on the certified fallback path.
fn source_proves_for_prep_kind(
    init: &ast::GenericForInit,
    origin: ast::ForOrigin,
) -> bool {
    let ipairs_aux = origin.aux & 0x8000_0000 != 0;
    let canonical_aux = (if ipairs_aux { 0x8000_0000 } else { 0 })
        | origin.result_count as u32;
    if origin.result_count == 0 || origin.aux != canonical_aux {
        return false;
    }
    match origin.prep_kind {
        // The high AUX bit selects the ipairs-style FORGLOOP write/exit
        // behavior.  A generic prep with that bit set is not equivalent to an
        // ordinary source iterator, so keep malformed/custom metadata out of
        // the source-shaped path.
        ast::ForPrepKind::Generic => !ipairs_aux,
        ast::ForPrepKind::Next => {
            !ipairs_aux
                && origin.result_count <= 2
                && match init.0.right.as_slice() {
                    [value] => call_is_named(value, "pairs"),
                    [RValue::Global(global), _state] => global_is_named(global, "next"),
                    [RValue::Global(global), _state, RValue::Literal(Literal::Nil)] => {
                        global_is_named(global, "next")
                    }
                    _ => false,
                }
        }
        ast::ForPrepKind::Inext => {
            ipairs_aux
                && origin.result_count <= 2
                && init.0.right.len() == 1
                && call_is_named(&init.0.right[0], "ipairs")
        }
    }
}

/// Validate the identity-bearing half of the generic-for protocol before any
/// region discovery mutates or consumes the CFG.  Production output always
/// carries provenance; a reachable marker without it is an explicit unsafe
/// metadata-loss condition rather than a request to guess the VM protocol.
fn validate_for_origins(function: &Function) -> Result<(), UnsafeStructureReason> {
    let Some(entry) = function.entry().as_ref().copied() else {
        return Ok(());
    };
    let mut reachable = FxHashSet::default();
    let mut work = vec![entry];
    while let Some(node) = work.pop() {
        if !reachable.insert(node) {
            continue;
        }
        work.extend(function.successor_blocks(node));
    }

    let mut init_origins = FxHashMap::default();
    let mut init_source_proven = FxHashMap::default();
    let mut next_origins = FxHashMap::default();
    let mut saw_marker = false;
    let mut saw_missing = false;
    for node in reachable {
        let Some(block) = function.block(node) else {
            continue;
        };
        for statement in block.iter() {
            match statement {
                Statement::GenericForInit(init) => {
                    saw_marker = true;
                    let Some(origin) = init.origin() else {
                        saw_missing = true;
                        continue;
                    };
                    if init_origins.insert(origin.id(), origin).is_some() {
                        return Err(UnsafeStructureReason::ForOriginDuplicate);
                    }
                    init_source_proven.insert(origin.id(), source_proves_for_prep_kind(init, origin));
                }
                Statement::GenericForNext(next) => {
                    saw_marker = true;
                    let Some(origin) = next.origin() else {
                        saw_missing = true;
                        continue;
                    };
                    let result_count = next
                        .res_locals
                        .iter()
                        .filter(|lvalue| lvalue.as_local().is_some())
                        .count();
                    if result_count != origin.result_count as usize {
                        return Err(UnsafeStructureReason::ForOriginMismatch);
                    }
                    if next_origins.insert(origin.id(), origin).is_some() {
                        return Err(UnsafeStructureReason::ForOriginDuplicate);
                    }
                }
                _ => {}
            }
        }
    }
    if !saw_marker {
        return Ok(());
    }
    if saw_missing {
        return Err(UnsafeStructureReason::ForOriginMissing);
    }
    if init_origins.len() != next_origins.len()
        || init_origins.keys().any(|id| !next_origins.contains_key(id))
    {
        return Err(UnsafeStructureReason::ForOriginMismatch);
    }
    for (id, init_origin) in init_origins {
        let Some(next_origin) = next_origins.get(&id) else {
            return Err(UnsafeStructureReason::ForOriginMismatch);
        };
        if init_origin != *next_origin {
            return Err(UnsafeStructureReason::ForOriginMismatch);
        }
        if !init_source_proven.get(&id).copied().unwrap_or(false) {
            return Err(UnsafeStructureReason::ForOriginPrepKindUnsupported);
        }
    }
    Ok(())
}

/// Return a complete source-shaped AST only when all reachable nodes can be
/// consumed exactly once.  The caller owns the fallback policy.
pub fn lift(function: Function) -> Option<Block> {
    let protected_locals = function.parameters.iter().cloned().collect();
    lift_with_ignored_locals(function, &protected_locals)
}

/// Run the source-like pass while retaining whether a rejection is a concrete
/// semantic safety finding.  Production selection must route `Unsafe` directly
/// to the certified fallback rather than handing the same CFG to the legacy
/// matcher.
pub fn lift_attempt_with_ignored_locals(
    function: Function,
    protected_locals: &FxHashSet<RcLocal>,
) -> StructureAttempt {
    struct LocalIdTransaction {
        base: u64,
        committed: bool,
    }

    impl Drop for LocalIdTransaction {
        fn drop(&mut self) {
            if !self.committed {
                ast::set_local_id_base(self.base);
            }
        }
    }

    // Loop live-out proofs may temporarily mint export locals.  Roll the
    // thread-local allocator back on every rejected path (including unwinding)
    // so a speculative pretty-print attempt cannot perturb later fallback
    // identities or direct callers of this API.
    let mut local_ids = LocalIdTransaction {
        base: ast::current_local_id(),
        committed: false,
    };
    if let Err(reason) = validate_for_origins(&function) {
        return StructureAttempt::Unsafe(reason);
    }
    let Some(analysis) = Analysis::new(&function) else {
        return StructureAttempt::Unsupported;
    };
    let Some(entry) = function.entry().as_ref().copied() else {
        return StructureAttempt::Unsupported;
    };
    let mut builder = Builder::new(&function, analysis, protected_locals.clone());
    let Some(result) = builder.build_path(entry, None, None) else {
        return builder
            .unsafe_reason
            .map(StructureAttempt::Unsafe)
            .unwrap_or(StructureAttempt::Unsupported);
    };
    if let Some(reason) = builder.unsafe_reason {
        return StructureAttempt::Unsafe(reason);
    }
    if result.next.is_some() || builder.visited != builder.analysis.reachable {
        return builder
            .unsafe_reason
            .map(StructureAttempt::Unsafe)
            .unwrap_or(StructureAttempt::Unsupported);
    }
    local_ids.committed = true;
    StructureAttempt::Structured(result.block)
}

/// Source-shaped structuring with an explicit set of function/closure-scope
/// locals that must not be reused as hidden iterator or loop-result registers.
/// The set is copied into the immutable builder so callers can reuse their
/// liveness set without coupling its lifetime to the CFG traversal.
pub fn lift_with_ignored_locals(
    function: Function,
    protected_locals: &FxHashSet<RcLocal>,
) -> Option<Block> {
    match lift_attempt_with_ignored_locals(function, protected_locals) {
        StructureAttempt::Structured(block) => Some(block),
        StructureAttempt::Unsupported | StructureAttempt::Unsafe(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Analysis, Builder, StructureAttempt, UnsafeStructureReason,
        lift as production_lift,
        lift_attempt_with_ignored_locals as production_lift_attempt,
    };
    use ast::{
        Assign, Block, Call, Close, Closure, ForOrigin, ForPrepKind, GenericForInit,
        GenericForNext, Global, If, LValue, Literal, Local, RValue, RcLocal, Statement, Table,
        Upvalue, VmProfileId,
    };
    use by_address::ByAddress;
    use cfg::{
        block::{BlockEdge, BranchType},
        function::Function,
    };
    use parking_lot::Mutex;
    use rustc_hash::{FxHashMap, FxHashSet};
    use triomphe::Arc;

    #[test]
    fn optional_export_copy_precedes_terminal_through_trailing_trivia() {
        let value = RcLocal::new(Local::new(Some("value".into())));
        let export = RcLocal::new(Local::new(Some("export".into())));
        let mut block = Block::from(vec![
            Statement::Break(ast::Break {}).into(),
            Statement::Comment(ast::Comment::new("trivia".into())).into(),
        ]);
        let copy = Assign::new(vec![LValue::Local(export)], vec![RValue::Local(value)]).into();

        super::insert_before_terminal(&mut block, copy);

        assert!(matches!(block.0[0], Statement::Assign(_)));
        assert!(matches!(block.0[1], Statement::Break(_)));
        assert!(matches!(block.0[2], Statement::Comment(_)));

        let mut continue_block = Block::from(vec![
            Statement::Continue(ast::Continue {}).into(),
            Statement::Comment(ast::Comment::new("trivia".into())).into(),
        ]);
        super::strip_terminal_continue(&mut continue_block);
        assert!(continue_block
            .0
            .iter()
            .all(|statement| !matches!(statement, Statement::Continue(_))));
    }

    // Hand-built CFG fixtures predate provenance-bearing marker constructors.
    // Attach deterministic, source-proven test origins at the test boundary so
    // those fixtures exercise the same proof path as real lifter output. Tests
    // that specifically validate metadata loss call the production entry point
    // through `super::` and therefore bypass this compatibility helper.
    fn attach_test_origins(function: &mut Function) {
        let nodes = function
            .blocks()
            .map(|(node, _)| node)
            .collect::<Vec<_>>();
        let init_nodes = nodes
            .iter()
            .copied()
            .filter(|node| {
                function
                    .block(*node)
                    .is_some_and(|block| block.iter().any(|s| s.as_generic_for_init().is_some()))
            })
            .collect::<Vec<_>>();
        let next_nodes = nodes
            .iter()
            .copied()
            .filter(|node| {
                function
                    .block(*node)
                    .is_some_and(|block| block.iter().any(|s| s.as_generic_for_next().is_some()))
            })
            .collect::<Vec<_>>();
        for (index, (init_node, next_node)) in init_nodes
            .into_iter()
            .zip(next_nodes.into_iter())
            .enumerate()
        {
            let Some(init) = function
                .block(init_node)
                .and_then(|block| block.iter().find_map(|s| s.as_generic_for_init()))
                .cloned()
            else {
                continue;
            };
            let Some(next) = function
                .block(next_node)
                .and_then(|block| block.iter().find_map(|s| s.as_generic_for_next()))
                .cloned()
            else {
                continue;
            };
            if init.origin().is_some() || next.origin().is_some() {
                continue;
            }
            let result_count = next.res_locals.len() as u8;
            let prep_kind = match init.0.right.as_slice() {
                [RValue::Call(call)]
                    if matches!(call.value.as_ref(), RValue::Global(global) if global.0 == b"pairs") =>
                {
                    ForPrepKind::Next
                }
                [RValue::Call(call)]
                    if matches!(call.value.as_ref(), RValue::Global(global) if global.0 == b"ipairs") =>
                {
                    ForPrepKind::Inext
                }
                [RValue::Global(global), ..] if global.0 == b"next" => ForPrepKind::Next,
                _ => ForPrepKind::Generic,
            };
            let aux = if prep_kind == ForPrepKind::Inext {
                0x8000_0000 | result_count as u32
            } else {
                result_count as u32
            };
            let origin = ForOrigin {
                prep_pc: index * 3 + 1,
                step_pc: index * 3 + 2,
                body_pc: index * 3 + 3,
                follow_pc: index * 3 + 4,
                prep_kind,
                base_register: 0,
                result_count,
                aux,
                bytecode_version: 6,
                vm_profile: VmProfileId::Luau,
            };
            if let Some(block) = function.block_mut(init_node) {
                if let Some(marker) = block
                    .iter_mut()
                    .find_map(|s| s.as_generic_for_init_mut())
                {
                    marker.1 = Some(origin);
                }
            }
            if let Some(block) = function.block_mut(next_node) {
                if let Some(marker) = block
                    .iter_mut()
                    .find_map(|s| s.as_generic_for_next_mut())
                {
                    marker.origin = Some(origin);
                }
            }
        }
    }

    fn lift(mut function: Function) -> Option<Block> {
        attach_test_origins(&mut function);
        production_lift(function)
    }

    fn lift_attempt_with_ignored_locals(
        mut function: Function,
        protected_locals: &FxHashSet<RcLocal>,
    ) -> StructureAttempt {
        attach_test_origins(&mut function);
        production_lift_attempt(function, protected_locals)
    }

    #[test]
    fn refuses_edge_arguments_after_terminal_statement() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);
        *function.block_mut(entry).unwrap() =
            Block::from(vec![Statement::Return(Default::default()).into()]);
        function.set_edges(entry, vec![(exit, BlockEdge {
            branch_type: BranchType::Unconditional,
            arguments: vec![(
                RcLocal::new(Local::new(Some("x".into()))),
                RValue::Literal(Literal::Nil),
            )],
        })]);
        *function.block_mut(exit).unwrap() = Block::default();
        assert!(lift(function).is_none());
    }

    #[test]
    fn materializes_parallel_edge_arguments_in_branch_arms() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let then_node = function.new_block();
        let else_node = function.new_block();
        let join = function.new_block();
        function.set_entry(entry);

        let incoming = RcLocal::new(Local::new(Some("incoming".into())));
        function.block_mut(entry).unwrap().push(
            If::new(
                RValue::Global(Global::from("condition")),
                Block::default(),
                Block::default(),
            )
            .into(),
        );
        function
            .block_mut(join)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(entry, vec![
            (then_node, BlockEdge {
                branch_type: BranchType::Then,
                arguments: vec![(incoming.clone(), Literal::Number(1.0).into())],
            }),
            (else_node, BlockEdge {
                branch_type: BranchType::Else,
                arguments: vec![(incoming.clone(), Literal::Number(2.0).into())],
            }),
        ]);
        function.set_edges(then_node, vec![(
            join,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(else_node, vec![(
            join,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        let output = lift(function)
            .expect("branch-local phi transfers should be sourceable")
            .to_string();
        assert!(output.contains("incoming = 1"), "{output}");
        assert!(output.contains("incoming = 2"), "{output}");
    }

    #[test]
    fn refuses_malformed_branch_tags_without_panicking() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let left = function.new_block();
        let right = function.new_block();
        function.set_entry(entry);
        function.block_mut(entry).unwrap().push(
            ast::If::new(
                RValue::Global(Global::from("condition")),
                Block::default(),
                Block::default(),
            )
            .into(),
        );
        function
            .block_mut(left)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function
            .block_mut(right)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(entry, vec![
            (left, BlockEdge::new(BranchType::Unconditional)),
            (right, BlockEdge::new(BranchType::Unconditional)),
        ]);
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_prestructured_body_on_single_successor_path() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);
        let value = RcLocal::new(Local::new(Some("value".into())));
        function.block_mut(entry).unwrap().push(
            If::new(
                RValue::Global(Global::from("condition")),
                Block::from(vec![
                    Assign::new(vec![LValue::Local(value.clone())], vec![
                        Literal::Number(1.0).into(),
                    ])
                    .into(),
                ]),
                Block::default(),
            )
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(entry, vec![(
            exit,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        // The one-edge CFG does not describe the nested If body.  Treating it
        // as a flat statement would lose the branch and make rewrites of its
        // locals incomplete, so the source-like pass must fail closed.
        assert!(lift(function).is_none());
    }

    #[test]
    fn continues_after_top_level_diamond_join() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let then_node = function.new_block();
        let else_node = function.new_block();
        let join = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);

        let result = RcLocal::new(Local::new(Some("result".into())));
        function.block_mut(entry).unwrap().push(
            If::new(
                RValue::Global(Global::from("condition")),
                Block::default(),
                Block::default(),
            )
            .into(),
        );
        function.block_mut(then_node).unwrap().push(
            Assign::new(vec![LValue::Local(result.clone())], vec![
                Literal::Number(1.0).into(),
            ])
            .into(),
        );
        function.block_mut(else_node).unwrap().push(
            Assign::new(vec![LValue::Local(result.clone())], vec![
                Literal::Number(2.0).into(),
            ])
            .into(),
        );
        function.block_mut(join).unwrap().push(
            Assign::new(vec![LValue::Local(result.clone())], vec![RValue::Local(
                result.clone(),
            )])
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(entry, vec![
            (else_node, BlockEdge::new(BranchType::Else)),
            (then_node, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(then_node, vec![(
            join,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(else_node, vec![(
            join,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(join, vec![(
            exit,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        let output = lift(function)
            .expect("a plain diamond followed by a tail should be source-shaped")
            .to_string();
        assert!(output.contains("if condition then"), "{output}");
        assert!(output.contains("result = result"), "{output}");
    }

    #[test]
    fn structures_generic_for_with_explicit_then_else_edges() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let origin = ForOrigin {
            prep_pc: 10,
            step_pc: 20,
            body_pc: 21,
            follow_pc: 22,
            prep_kind: ForPrepKind::Generic,
            base_register: 0,
            result_count: 1,
            aux: 1,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        // The lifter normally replaces these internal iterator registers with
        // the original iterator expression during SSA cleanup.
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        for_init.1 = Some(origin);
        function.block_mut(init).unwrap().push(for_init.into());
        let mut for_next = GenericForNext::new(
            vec![value.clone()],
            generator.clone().into(),
            state.clone(),
            control.clone(),
        );
        for_next.origin = Some(origin);
        function.block_mut(header).unwrap().push(for_next.into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        // Deliberately insert Else first: graph insertion order is not branch
        // semantics, so the structurer must honor the edge tags.
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        let output = lift(function).expect("simple generic-for should be source-shaped");
        let generic_for = output
            .iter()
            .find_map(|statement| statement.as_generic_for())
            .expect("source-shaped output should retain loop provenance");
        assert_eq!(generic_for.origin, Some(origin));
        let output = output.to_string();
        assert!(output.contains("for value in items do"), "{output}");
        assert!(!output.contains("continue"), "{output}");
        assert!(!output.contains("GenericFor"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn rejects_non_generic_for_prep_kind_without_source_proof() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let value2 = RcLocal::new(Local::new(Some("value2".into())));
        let origin = ForOrigin {
            prep_pc: 10,
            step_pc: 20,
            body_pc: 21,
            follow_pc: 22,
            prep_kind: ForPrepKind::Next,
            base_register: 0,
            result_count: 2,
            aux: 2,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![
            RValue::Global(Global::from("next")),
            RValue::Global(Global::from("items")),
            RValue::Global(Global::from("extra")),
        ];
        for_init.1 = Some(origin);
        function.block_mut(init).unwrap().push(for_init.into());
        let mut for_next = GenericForNext::new(
            vec![value, value2],
            generator.into(),
            state,
            control,
        );
        for_next.origin = Some(origin);
        function.block_mut(header).unwrap().push(for_next.into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (exit, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::ForOriginPrepKindUnsupported)
        ));
    }

    #[test]
    fn accepts_specialized_prep_kinds_with_one_result() {
        let attempt_for = |prep_kind, right, aux| {
            let mut function = Function::new(0);
            let init = function.new_block();
            let header = function.new_block();
            let body = function.new_block();
            let exit = function.new_block();
            function.set_entry(init);

            let generator = RcLocal::new(Local::new(Some("generator".into())));
            let state = RcLocal::new(Local::new(Some("state".into())));
            let control = RcLocal::new(Local::new(Some("control".into())));
            let value = RcLocal::new(Local::new(Some("value".into())));
            let origin = ForOrigin {
                prep_pc: 10,
                step_pc: 20,
                body_pc: 21,
                follow_pc: 22,
                prep_kind,
                base_register: 0,
                result_count: 1,
                aux,
                bytecode_version: 6,
                vm_profile: VmProfileId::Luau,
            };
            let mut for_init =
                GenericForInit::new(generator.clone(), state.clone(), control.clone());
            for_init.0.right = vec![right];
            for_init.1 = Some(origin);
            function.block_mut(init).unwrap().push(for_init.into());
            let mut for_next = GenericForNext::new(
                vec![value],
                generator.into(),
                state,
                control,
            );
            for_next.origin = Some(origin);
            function.block_mut(header).unwrap().push(for_next.into());
            function
                .block_mut(exit)
                .unwrap()
                .push(Statement::Return(Default::default()).into());
            function.set_edges(init, vec![
                (header, BlockEdge::new(BranchType::Unconditional)),
            ]);
            function.set_edges(header, vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ]);
            function.set_edges(body, vec![
                (header, BlockEdge::new(BranchType::Unconditional)),
            ]);
            lift_attempt_with_ignored_locals(function, &FxHashSet::default())
        };

        assert!(matches!(
            attempt_for(
                ForPrepKind::Next,
                RValue::Call(Call::new(
                    RValue::Global(Global::from("pairs")),
                    vec![RValue::Global(Global::from("items"))],
                )),
                1,
            ),
            StructureAttempt::Structured(_)
        ));
        assert!(matches!(
            attempt_for(
                ForPrepKind::Inext,
                RValue::Call(Call::new(
                    RValue::Global(Global::from("ipairs")),
                    vec![RValue::Global(Global::from("items"))],
                )),
                0x8000_0001,
            ),
            StructureAttempt::Structured(_)
        ));
    }

    #[test]
    fn rejects_generic_for_with_ipairs_aux_flag() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let value2 = RcLocal::new(Local::new(Some("value2".into())));
        let origin = ForOrigin {
            prep_pc: 10,
            step_pc: 20,
            body_pc: 21,
            follow_pc: 22,
            prep_kind: ForPrepKind::Generic,
            base_register: 0,
            result_count: 2,
            aux: 0x8000_0002,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        for_init.1 = Some(origin);
        function.block_mut(init).unwrap().push(for_init.into());
        let mut for_next = GenericForNext::new(
            vec![value, value2],
            generator.into(),
            state,
            control,
        );
        for_next.origin = Some(origin);
        function.block_mut(header).unwrap().push(for_next.into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (exit, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::ForOriginPrepKindUnsupported)
        ));
    }

    #[test]
    fn refuses_pre_init_edge_closure_capture_of_result() {
        let mut function = Function::new(0);
        let pre = function.new_block();
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        function.set_entry(pre);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let callback = RcLocal::new(Local::new(Some("callback".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function::default()))),
            upvalues: vec![Upvalue::Ref(result.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(join).unwrap().push(
            ast::Return::new(vec![RValue::Call(Call::new(
                RValue::Local(callback.clone()),
                Vec::new(),
            ))])
            .into(),
        );
        function.set_edges(pre, vec![(
            init,
            BlockEdge {
                branch_type: BranchType::Unconditional,
                arguments: vec![(callback, RValue::Closure(closure))],
            },
        )]);
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(
            lift(function).is_none(),
            "an edge-created closure must not retain the shadowed pre-loop result cell"
        );
    }

    #[test]
    fn refuses_protocol_capture_on_pre_init_edge() {
        let mut function = Function::new(0);
        let pre = function.new_block();
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        function.set_entry(pre);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let callback = RcLocal::new(Local::new(Some("callback".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function::default()))),
            upvalues: vec![Upvalue::Ref(generator.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(join).unwrap().push(
            ast::Return::new(vec![RValue::Call(Call::new(
                RValue::Local(callback.clone()),
                Vec::new(),
            ))])
            .into(),
        );
        function.set_edges(pre, vec![(
            init,
            BlockEdge {
                branch_type: BranchType::Unconditional,
                arguments: vec![(
                    callback,
                    RValue::Closure(closure),
                )],
            },
        )]);
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(
            lift(function).is_none(),
            "edge closures must not capture a hidden iterator protocol local"
        );
    }

    #[test]
    fn refuses_post_loop_result_read_on_edge_without_exhaustion_proof() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        let tail = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function
            .block_mut(tail)
            .unwrap()
            .push(ast::Return::new(vec![RValue::Local(sink.clone())]).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(join, vec![(
            tail,
            BlockEdge {
                branch_type: BranchType::Unconditional,
                arguments: vec![(sink, RValue::Local(result))],
            },
        )]);

        // The edge is a post-loop read just like a statement read.  Without
        // an exhaustion-value proof the result cannot be exported safely.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_captured_result_written_by_post_loop_edge() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        let tail = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let callback = RcLocal::new(Local::new(Some("callback".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(result.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Ref(result.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(vec![LValue::Local(callback.clone())], vec![RValue::Closure(closure)])
                .into(),
        );
        function.block_mut(tail).unwrap().push(
            ast::Return::new(vec![RValue::Call(Call::new(
                RValue::Local(callback),
                Vec::new(),
            ))])
            .into(),
        );
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(join, vec![
            (
                tail,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(result, Literal::Number(42.0).into())],
                },
            ),
        ]);

        // The closure retains the loop result cell.  A post-loop edge write
        // to that SSA identity cannot be represented by a source-level loop
        // binding without changing which cell the closure observes.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_captured_result_written_by_direct_exhaustion() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let callback = RcLocal::new(Local::new(Some("callback".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(result.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Ref(result.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(vec![LValue::Local(callback.clone())], vec![RValue::Closure(closure)])
                .into(),
        );
        function.block_mut(join).unwrap().extend([
            Assign::new(
                vec![LValue::Local(result)],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
            ast::Return::new(vec![RValue::Call(Call::new(
                RValue::Local(callback),
                Vec::new(),
            ))])
            .into(),
        ]);
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        // Even an explicit nil write on direct exhaustion targets the shared
        // VM result cell. A source-level `for` binding would instead leave the
        // closure observing its last loop-local value.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_ref_capture_of_loop_result_without_cell_proof() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(result.clone())]).into(),
                ]),
                ..Default::default()
            })) ),
            upvalues: vec![Upvalue::Ref(result.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![result], generator.into(), state, control).into(),
        );
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Call(Call::new(
                RValue::Global(Global::from("collect")),
                vec![RValue::Closure(closure)],
            )));
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (exit, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);

        let attempt = lift_attempt_with_ignored_locals(function, &FxHashSet::default());
        assert!(matches!(
            attempt,
            StructureAttempt::Unsafe(UnsafeStructureReason::CapturedLoopResultRef)
        ));
    }

    #[test]
    fn refuses_edge_closure_when_result_rewrite_would_split_cells() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        let tail = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let callback = RcLocal::new(Local::new(Some("callback".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(result.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Copy(result.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(join).unwrap().push(
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        function
            .block_mut(tail)
            .unwrap()
            .push(ast::Return::new(vec![RValue::Local(callback.clone())]).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(join, vec![
            (
                tail,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(callback, RValue::Closure(closure))],
                },
            ),
        ]);

        // The export rewrite changes the closure upvalue, but the closure body
        // still contains the original local identity.  Keep this unsupported
        // until both sides can be rewritten together.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_statement_closure_when_result_rewrite_would_split_cells() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        let tail = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let callback = RcLocal::new(Local::new(Some("callback".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(result.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Copy(result.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(join).unwrap().push(
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        function.block_mut(tail).unwrap().extend([
            Assign::new(
                vec![LValue::Local(callback.clone())],
                vec![RValue::Closure(closure)],
            )
            .into(),
            ast::Return::new(vec![RValue::Call(Call::new(
                RValue::Local(callback),
                Vec::new(),
            ))])
            .into(),
        ]);
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(join, vec![
            (tail, BlockEdge::new(BranchType::Unconditional)),
        ]);

        // The statement rewrite updates the closure upvalue metadata, but its
        // child function body still reads the original local identity.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_nested_closure_when_result_rewrite_would_split_cells() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        let tail = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        let outer = RcLocal::new(Local::new(Some("outer".into())));
        let inner = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(result.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Copy(result.clone())],
        });
        let outer_value = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![ast::Return::new(vec![inner]).into()]),
                ..Default::default()
            }))),
            upvalues: Vec::new(),
        });
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(join).unwrap().push(
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        let call_outer = RValue::Call(Call::new(RValue::Local(outer.clone()), Vec::new()));
        function.block_mut(tail).unwrap().extend([
            Assign::new(
                vec![LValue::Local(sink)],
                vec![RValue::Local(result)],
            )
            .into(),
            Assign::new(vec![LValue::Local(outer)], vec![outer_value]).into(),
            ast::Return::new(vec![RValue::Call(Call::new(call_outer, Vec::new()))]).into(),
        ]);
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(join, vec![
            (tail, BlockEdge::new(BranchType::Unconditional)),
        ]);

        // The outer closure hides the inner closure's capture from a shallow
        // scan; recursively inspect child bodies before applying the rewrite.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_nested_closure_capture_without_result_export() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        let tail = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let outer = RcLocal::new(Local::new(Some("outer".into())));
        let inner = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(result.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Copy(result.clone())],
        });
        let outer_value = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![ast::Return::new(vec![inner]).into()]),
                ..Default::default()
            }))),
            upvalues: Vec::new(),
        });
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(join).unwrap().push(
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        let call_outer = RValue::Call(Call::new(RValue::Local(outer.clone()), Vec::new()));
        function.block_mut(tail).unwrap().extend([
            Assign::new(vec![LValue::Local(outer)], vec![outer_value]).into(),
            ast::Return::new(vec![RValue::Call(Call::new(call_outer, Vec::new()))]).into(),
        ]);
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(join, vec![
            (tail, BlockEdge::new(BranchType::Unconditional)),
        ]);

        // No post-loop read forces an export rewrite, but the nested closure
        // still captures the VM result cell outside the source loop scope.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_nested_closure_capture_of_loop_protocol_local() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        let tail = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let outer = RcLocal::new(Local::new(Some("outer".into())));
        let inner = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(control.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Copy(control.clone())],
        });
        let outer_value = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![ast::Return::new(vec![inner]).into()]),
                ..Default::default()
            }))),
            upvalues: Vec::new(),
        });
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(vec![LValue::Local(outer.clone())], vec![outer_value]).into(),
        );
        let call_outer = RValue::Call(Call::new(RValue::Local(outer), Vec::new()));
        function.block_mut(tail).unwrap().push(
            ast::Return::new(vec![RValue::Call(Call::new(call_outer, Vec::new()))]).into(),
        );
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(join, vec![
            (tail, BlockEdge::new(BranchType::Unconditional)),
        ]);

        // The inner closure captures the hidden control register through an
        // outer closure, so a source-level loop would expose a stale cell.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_nested_if_closure_capture_after_loop_rewrite() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        let tail = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        let outer = RcLocal::new(Local::new(Some("outer".into())));
        let inner = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(result.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Copy(result.clone())],
        });
        let conditional = If::new(
            Global::from("condition").into(),
            Block::from(vec![ast::Return::new(vec![inner]).into()]),
            Block::default(),
        );
        let outer_value = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![conditional.into()]),
                ..Default::default()
            }))),
            upvalues: Vec::new(),
        });
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(join).unwrap().push(
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        function.block_mut(tail).unwrap().extend([
            Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(result)]).into(),
            Assign::new(vec![LValue::Local(outer.clone())], vec![outer_value]).into(),
            ast::Return::new(vec![RValue::Call(Call::new(
                RValue::Local(outer),
                Vec::new(),
            ))])
            .into(),
        ]);
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (join, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(join, vec![
            (tail, BlockEdge::new(BranchType::Unconditional)),
        ]);

        // Structured statement bodies are opaque to `Traverse`; the capture
        // scan must still find the inner closure under this prestructured If.
        assert!(lift(function).is_none());
    }

    #[test]
    fn rejects_protocol_edge_arguments_on_loop_backedge() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let mut for_init = GenericForInit::new(
            generator.clone(),
            state.clone(),
            control.clone(),
        );
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![value],
                generator.clone().into(),
                state,
                control.clone(),
            )
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        // This is a phi copy on the backedge into the hidden FORGLOOP control
        // register.  Emitting it as a visible assignment in a source `for`
        // body would not affect the VM's hidden iterator state.
        function.set_edges(body, vec![
            (
                header,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(control, Literal::Number(42.0).into())],
                },
            ),
        ]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::ForProtocolEdgeTransfer)
        ));
    }

    #[test]
    fn rejects_init_edge_arguments_that_cross_iterator_evaluation() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let incoming = RcLocal::new(Local::new(Some("incoming".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![value], generator.into(), state, control).into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(init, vec![
            (
                header,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(incoming, Literal::Number(1.0).into())],
                },
            ),
        ]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::ForInitEdgeTransferOrder)
        ));
    }

    #[test]
    fn rejects_partially_annotated_generic_for_with_typed_reason() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let origin = ForOrigin {
            prep_pc: 1,
            step_pc: 2,
            body_pc: 3,
            follow_pc: 4,
            prep_kind: ForPrepKind::Generic,
            base_register: 0,
            result_count: 1,
            aux: 1,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
        };
        let mut init_marker =
            GenericForInit::new(generator.clone(), state.clone(), control.clone());
        init_marker.1 = Some(origin);
        init_marker.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(init_marker.into());
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![result], generator.into(), state, control).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::ForOriginMissing)
        ));
    }

    #[test]
    fn rejects_effectful_for_init_suffix_without_tuple_staging_proof() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        // An effectful suffix cannot be moved across iterator evaluation.  A
        // pure, read-free local assignment is accepted only after the explicit
        // tuple-staging proof in `build_loop`; this call remains fail-closed.
        function
            .block_mut(init)
            .unwrap()
            .push(Call::new(RValue::Global(Global::from("prepare")), Vec::new()).into());
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value], generator.into(), state, control).into());
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("body".into())).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::ForInitSuffixOrder)
        ));
    }

    #[test]
    fn refuses_for_init_suffix_write_to_captured_local() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let captured = RcLocal::new(Local::new(Some("captured".into())));
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function::default()))),
            upvalues: vec![Upvalue::Ref(captured.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        // The iterator call can observe `captured` through the closure.  The
        // suffix write must therefore stay after this call, not move before
        // the emitted source-level `for`.
        for_init.0.right = vec![RValue::Call(Call::new(
            RValue::Closure(closure),
            Vec::new(),
        ))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(init).unwrap().push(
            Assign::new(vec![LValue::Local(captured)], vec![RValue::Table(
                Table::default(),
            )])
            .into(),
        );
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value], generator.into(), state, control).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        let attempt = lift_attempt_with_ignored_locals(function.clone(), &FxHashSet::default());
        assert!(matches!(
            attempt,
            StructureAttempt::Unsafe(UnsafeStructureReason::CapturedCellReorder)
        ));
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_indirect_callable_for_init_suffix_write_to_captured_local() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let captured = RcLocal::new(Local::new(Some("captured".into())));
        let iterator_factory = RcLocal::new(Local::new(Some("iterator_factory".into())));
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    ast::Return::new(vec![RValue::Local(captured.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Ref(captured.clone())],
        };
        // The RHS invokes a pre-existing local closure.  Its capture is not
        // represented inline in the call expression, so direct RHS capture
        // collection alone would miss this reorder hazard.
        function.block_mut(init).unwrap().push(
            Assign::new(
                vec![LValue::Local(iterator_factory.clone())],
                vec![RValue::Closure(closure)],
            )
            .into(),
        );
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Call(Call::new(
            RValue::Local(iterator_factory),
            Vec::new(),
        ))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(init).unwrap().push(
            Assign::new(
                vec![LValue::Local(captured)],
                vec![RValue::Table(Table::default())],
            )
            .into(),
        );
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value], generator.into(), state, control).into());
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("body".into())).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::CapturedCellReorder)
        ));
    }

    #[test]
    fn classifies_close_in_for_init_suffix_as_unsafe() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let captured = RcLocal::new(Local::new(Some("captured".into())));
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        // `Close` has an intentionally empty LocalRw summary, so a generic
        // `is_linear_statement` gate would incorrectly commute it before the
        // iterator preparation.  The suffix allow-list must reject it.
        function.block_mut(init).unwrap().push(
            Close {
                locals: vec![captured],
            }
            .into(),
        );
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value], generator.into(), state, control).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function.clone(), &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::ForInitSuffixOrder)
        ));
        assert!(lift(function).is_none());
    }

    #[test]
    fn classifies_close_in_for_body_as_unmodeled() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value.clone()], generator.into(), state, control).into());
        function
            .block_mut(body)
            .unwrap()
            .push(Close { locals: vec![value] }.into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (exit, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::UnmodeledClose)
        ));
    }

    #[test]
    fn classifies_close_in_closure_body_as_unmodeled() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let callback = RcLocal::new(Local::new(Some("callback".into())));
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    Close {
                        locals: vec![result.clone()],
                    }
                    .into(),
                    ast::Return::new(vec![RValue::Local(result.clone())]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Ref(result.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(
                vec![LValue::Local(callback)],
                vec![RValue::Closure(closure)],
            )
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (exit, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::UnmodeledClose)
        ));
    }

    #[test]
    fn classifies_close_in_edge_closure_as_unmodeled() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exhausted = function.new_block();
        let tail = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let callback = RcLocal::new(Local::new(Some("callback".into())));
        let child_local = RcLocal::new(Local::new(Some("child_local".into())));
        let closure = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    Close {
                        locals: vec![child_local],
                    }
                    .into(),
                    ast::Return::new(Vec::new()).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: Vec::new(),
        });
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![result],
                generator.into(),
                state,
                control,
            )
            .into(),
        );
        function
            .block_mut(exhausted)
            .unwrap()
            .push(Assign::new(vec![LValue::Local(callback.clone())], vec![RValue::Literal(Literal::Nil)]).into());
        function
            .block_mut(tail)
            .unwrap()
            .push(ast::Return::new(vec![RValue::Local(callback.clone())]).into());
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (exhausted, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(exhausted, vec![
            (
                tail,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(callback, closure)],
                },
            ),
        ]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::UnmodeledClose)
        ));
    }

    #[test]
    fn classifies_markers_in_closure_body_as_unmodeled() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let callback = RcLocal::new(Local::new(Some("callback".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        let closure = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: Block::from(vec![
                    for_init.into(),
                    GenericForNext::new(vec![value], generator.into(), state, control).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: Vec::new(),
        });
        function.block_mut(entry).unwrap().extend([
            Assign::new(vec![LValue::Local(callback.clone())], vec![closure]).into(),
            ast::Return::new(vec![RValue::Local(callback)]).into(),
        ]);

        // The outer CFG has no protocol marker of its own, but formatting the
        // returned closure would still expose its hidden child markers.
        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::UnmodeledControl)
        ));
    }

    #[test]
    fn classifies_markers_hidden_in_root_protocol_rhs_closure_as_unmodeled() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let child_generator = RcLocal::new(Local::new(Some("child_generator".into())));
        let child_state = RcLocal::new(Local::new(Some("child_state".into())));
        let child_control = RcLocal::new(Local::new(Some("child_control".into())));
        let child_value = RcLocal::new(Local::new(Some("child_value".into())));

        let mut child_init =
            GenericForInit::new(child_generator.clone(), child_state.clone(), child_control.clone());
        child_init.0.right = vec![RValue::Global(Global::from("child_items"))];
        let child_body = Block::from(vec![
            child_init.into(),
            GenericForNext::new(
                vec![child_value],
                child_generator.into(),
                child_state,
                child_control,
            )
            .into(),
        ]);
        let hidden_protocol = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(ast::Function {
                body: child_body,
                ..Default::default()
            }))),
            upvalues: Vec::new(),
        });

        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        // The outer protocol marker is consumed by this pass, but its RHS
        // closure must still be scanned for hidden child markers.
        for_init.0.right = vec![hidden_protocol];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![value], generator.into(), state, control).into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (exit, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::UnmodeledControl)
        ));
    }

    #[test]
    fn branch_rewrite_liveness_includes_loop_backedges() {
        let mut function = Function::new(0);
        let join = function.new_block();
        let read = function.new_block();
        function.set_entry(join);
        let last = RcLocal::new(Local::new(Some("last".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        function
            .block_mut(read)
            .unwrap()
            .push(Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(last.clone())]).into());
        function.set_edges(join, vec![(
            read,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(read, vec![(
            join,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        let analysis = Analysis::new(&function).expect("cyclic CFG is analyzable");
        assert!(analysis.live_in[&join].contains(&last));
        assert!(analysis.live_out[&read].contains(&last));
        let mut builder = Builder::new(&function, analysis, FxHashSet::default());
        builder.visited.insert(read);
        let exported = RcLocal::new(Local::new(Some("exported".into())));
        let then_map = [(last.clone(), exported)].into_iter().collect();
        let else_map = FxHashMap::default();
        assert!(
            builder
                .reconcile_rewrite(&FxHashMap::default(), &then_map, &else_map, Some(join))
                .is_none()
        );
    }

    #[test]
    fn optional_export_copy_is_inserted_before_terminal_transfer() {
        let mut function = Function::new(0);
        let join = function.new_block();
        let exit = function.new_block();
        function.set_entry(join);

        let raw = RcLocal::new(Local::new(Some("raw_result".into())));
        let export = RcLocal::new(Local::new(Some("exported_result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        function.block_mut(join).unwrap().push(
            Assign::new(
                vec![LValue::Local(sink)],
                vec![RValue::Local(raw.clone())],
            )
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(join, vec![(
            exit,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        let analysis = Analysis::new(&function).expect("linear CFG is analyzable");
        assert!(analysis.live_in[&join].contains(&raw));
        let builder = Builder::new(&function, analysis, FxHashSet::default());
        let base = FxHashMap::default();
        let mut then_map = [(raw.clone(), export.clone())].into_iter().collect();
        let mut else_map = FxHashMap::default();
        let mut then_block = Block::from(vec![
            Assign::new(
                vec![LValue::Local(export.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        ]);
        let mut else_block = Block::from(vec![Statement::Continue(ast::Continue {}).into()]);

        builder
            .materialize_optional_export_gaps(
                &base,
                &mut then_map,
                &mut else_map,
                Some(join),
                true,
                &mut then_block,
                &mut else_block,
            )
            .expect("both arms reach the continuation");

        assert!(matches!(else_block.last(), Some(Statement::Continue(_))));
        assert!(matches!(
            else_block.get(else_block.len() - 2),
            Some(Statement::Assign(_))
        ));
        assert_eq!(else_map.get(&raw), Some(&export));
    }

    #[test]
    fn mixed_exit_optional_export_fails_closed_without_mutating_terminal_arm() {
        let mut function = Function::new(0);
        let join = function.new_block();
        let exit = function.new_block();
        function.set_entry(join);

        let raw = RcLocal::new(Local::new(Some("raw_result".into())));
        let export = RcLocal::new(Local::new(Some("exported_result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        function.block_mut(join).unwrap().push(
            Assign::new(
                vec![LValue::Local(sink)],
                vec![RValue::Local(raw.clone())],
            )
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(join, vec![(
            exit,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        let analysis = Analysis::new(&function).expect("linear CFG is analyzable");
        let builder = Builder::new(&function, analysis, FxHashSet::default());
        let base = FxHashMap::default();
        let mut then_map = [(raw.clone(), export.clone())].into_iter().collect();
        let mut else_map = FxHashMap::default();
        let mut then_block = Block::from(vec![
            Assign::new(
                vec![LValue::Local(export)],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        ]);
        let mut else_block = Block::from(vec![Statement::Break(ast::Break {}).into()]);

        assert!(builder
            .materialize_optional_export_gaps(
                &base,
                &mut then_map,
                &mut else_map,
                Some(join),
                false,
                &mut then_block,
                &mut else_block,
            )
            .is_none());
        assert_eq!(else_block.len(), 1);
        assert!(matches!(else_block.last(), Some(Statement::Break(_))));
        assert!(else_map.is_empty());
    }

    #[test]
    fn liveness_accounts_for_edge_argument_sources_and_destinations() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);
        let source = RcLocal::new(Local::new(Some("source".into())));
        let destination = RcLocal::new(Local::new(Some("destination".into())));
        function.block_mut(exit).unwrap().push(
            Assign::new(
                vec![LValue::Local(RcLocal::new(Local::new(Some("sink".into()))))],
                vec![RValue::Local(destination.clone())],
            )
            .into(),
        );
        function.set_edges(entry, vec![(exit, BlockEdge {
            branch_type: BranchType::Unconditional,
            arguments: vec![(destination, RValue::Local(source.clone()))],
        })]);
        let nodes = vec![entry, exit];
        let reachable = nodes.iter().copied().collect::<FxHashSet<_>>();
        let (live_in, live_out) = Analysis::liveness(&function, &nodes, &reachable);
        assert!(live_in[&entry].contains(&source));
        assert!(live_out[&entry].contains(&source));
    }

    #[test]
    fn repro_parallel_edge_swap_liveness() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);
        let a = RcLocal::new(Local::new(Some("a".into())));
        let b = RcLocal::new(Local::new(Some("b".into())));
        let sink_a = RcLocal::new(Local::new(Some("sink_a".into())));
        let sink_b = RcLocal::new(Local::new(Some("sink_b".into())));
        function.block_mut(exit).unwrap().extend([
            Assign::new(vec![LValue::Local(sink_a)], vec![RValue::Local(a.clone())]).into(),
            Assign::new(vec![LValue::Local(sink_b)], vec![RValue::Local(b.clone())]).into(),
        ]);
        function.set_edges(entry, vec![(exit, BlockEdge {
            branch_type: BranchType::Unconditional,
            arguments: vec![
                (a.clone(), RValue::Local(b.clone())),
                (b.clone(), RValue::Local(a.clone())),
            ],
        })]);
        let nodes = vec![entry, exit];
        let reachable = nodes.iter().copied().collect::<FxHashSet<_>>();
        let (_, live_out) = Analysis::liveness(&function, &nodes, &reachable);
        assert!(live_out[&entry].contains(&a));
        assert!(
            live_out[&entry].contains(&b),
            "parallel swap loses b: {:?}",
            live_out[&entry]
        );
    }

    #[test]
    fn refuses_generic_for_result_aliasing_parameter() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        function.parameters.push(value.clone());

        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value], generator.into(), state, control).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        // Reusing a function parameter as a source `for` result would create
        // a fresh loop-local binding and lose the parameter cell's identity.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_sequential_loop_result_reuse_after_export_rewrite() {
        let mut function = Function::new(0);
        let first_init = function.new_block();
        let first_header = function.new_block();
        let first_body = function.new_block();
        let between = function.new_block();
        let second_init = function.new_block();
        let second_header = function.new_block();
        let second_body = function.new_block();
        let exit = function.new_block();
        function.set_entry(first_init);

        let first_generator = RcLocal::new(Local::new(Some("first_generator".into())));
        let first_state = RcLocal::new(Local::new(Some("first_state".into())));
        let first_control = RcLocal::new(Local::new(Some("first_control".into())));
        let second_generator = RcLocal::new(Local::new(Some("second_generator".into())));
        let second_state = RcLocal::new(Local::new(Some("second_state".into())));
        let second_control = RcLocal::new(Local::new(Some("second_control".into())));
        // Deliberately reuse the same SSA identity for both loop results.  The
        // first loop exports it because the second loop body reads it; the
        // second loop must then be rejected rather than rewritten to that
        // earlier export cell.
        let reused_result = RcLocal::new(Local::new(Some("reused_result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));

        let mut first_for_init = GenericForInit::new(
            first_generator.clone(),
            first_state.clone(),
            first_control.clone(),
        );
        first_for_init.0.right = vec![RValue::Global(Global::from("first_items"))];
        function
            .block_mut(first_init)
            .unwrap()
            .push(first_for_init.into());
        function.block_mut(first_header).unwrap().push(
            GenericForNext::new(
                vec![reused_result.clone()],
                first_generator.into(),
                first_state,
                first_control,
            )
            .into(),
        );
        let mut second_for_init = GenericForInit::new(
            second_generator.clone(),
            second_state.clone(),
            second_control.clone(),
        );
        second_for_init.0.right = vec![RValue::Global(Global::from("second_items"))];
        function
            .block_mut(second_init)
            .unwrap()
            .push(second_for_init.into());
        function.block_mut(second_header).unwrap().push(
            GenericForNext::new(
                vec![reused_result.clone()],
                second_generator.into(),
                second_state,
                second_control,
            )
            .into(),
        );
        function.block_mut(second_body).unwrap().push(
            Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(
                reused_result,
            )])
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(first_init, vec![(
            first_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(first_header, vec![
            (between, BlockEdge::new(BranchType::Else)),
            (first_body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(first_body, vec![(
            first_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(between, vec![(
            second_init,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(second_init, vec![(
            second_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(second_header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (second_body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(second_body, vec![(
            second_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(lift(function).is_none());
    }

    #[test]
    fn structures_empty_generic_for_self_loop() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![value.clone()], generator.into(), state, control).into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        // An empty loop body is a legal compiler shape: FORGLOOP's Then edge
        // points back to its own header and Else leaves the loop.
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (header, BlockEdge::new(BranchType::Then)),
        ]);

        let output = lift(function)
            .expect("an empty generic-for self-loop should be source-shaped")
            .to_string();
        assert!(output.contains("for value in items do"), "{output}");
        assert!(!output.contains("GenericFor"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn structures_compiler_empty_generic_for_with_pc_alias_body() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let empty_body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let origin = ForOrigin {
            prep_pc: 10,
            step_pc: 11,
            // Luau emits an empty body target that aliases the FORGLOOP PC;
            // the CFG still keeps a distinct empty block for the body edge.
            body_pc: 11,
            follow_pc: 12,
            prep_kind: ForPrepKind::Generic,
            base_register: 0,
            result_count: 1,
            aux: 1,
            bytecode_version: 13,
            vm_profile: VmProfileId::Luau,
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        for_init.1 = Some(origin);
        function.block_mut(init).unwrap().push(for_init.into());
        let mut for_next = GenericForNext::new(vec![value], generator.into(), state, control);
        for_next.origin = Some(origin);
        function.block_mut(header).unwrap().push(for_next.into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![(header, BlockEdge::new(BranchType::Unconditional))]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (empty_body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(empty_body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_block_pc_range(header, 11, 11);
        function.set_block_pc_range(exit, 12, 13);

        let output = lift(function)
            .expect("the compiler's empty-body PC alias should remain source-shaped")
            .to_string();
        assert!(output.contains("for value in items do"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn structures_generic_for_whose_body_always_breaks_without_backedge() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![value], generator.into(), state, control).into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(init, vec![(header, BlockEdge::new(BranchType::Unconditional))]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        // Source `break` is patched directly to the follow block; there is no
        // body-to-header dominance backedge to seed a natural loop.
        function.set_edges(body, vec![(exit, BlockEdge::new(BranchType::Unconditional))]);

        let output = lift(function)
            .expect("an always-break generic-for should be source-shaped")
            .to_string();
        assert!(output.contains("for value in items do"), "{output}");
        assert!(output.contains("break"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn structures_compiler_direct_break_with_body_follow_alias() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let follow = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let origin = ForOrigin {
            prep_pc: 1,
            step_pc: 2,
            // An unconditional compiler `break` can make FORGLOOP's body
            // target equal the follow target (no separate body block).
            body_pc: 3,
            follow_pc: 3,
            prep_kind: ForPrepKind::Generic,
            base_register: 0,
            result_count: 1,
            aux: 1,
            bytecode_version: 13,
            vm_profile: VmProfileId::Luau,
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        for_init.1 = Some(origin);
        function.block_mut(init).unwrap().push(for_init.into());
        let mut for_next = GenericForNext::new(vec![value], generator.into(), state, control);
        for_next.origin = Some(origin);
        function.block_mut(header).unwrap().push(for_next.into());
        function
            .block_mut(follow)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(init, vec![(header, BlockEdge::new(BranchType::Unconditional))]);
        // Both FORGLOOP arms target the follow block.  The Then arm is the
        // source-level unconditional break; the Else arm is exhaustion.
        function.set_edges(header, vec![
            (follow, BlockEdge::new(BranchType::Else)),
            (follow, BlockEdge::new(BranchType::Then)),
        ]);

        let output = production_lift(function)
            .expect("a compiler direct-break alias should be source-shaped")
            .to_string();
        assert!(output.contains("for value in items do"), "{output}");
        assert!(output.contains("break"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn structures_generic_for_whose_body_always_returns_without_backedge() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![value.clone()], generator.into(), state, control).into(),
        );
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Return(ast::Return { values: vec![value.into()] }).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(init, vec![(header, BlockEdge::new(BranchType::Unconditional))]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        // A return terminates the body and likewise leaves no natural latch.
        function.set_edges(body, Vec::new());

        let output = lift(function)
            .expect("an always-return generic-for should be source-shaped")
            .to_string();
        assert!(output.contains("for value in items do"), "{output}");
        assert!(output.contains("return value"), "{output}");
    }

    #[test]
    fn structures_generic_for_with_conditional_terminal_return() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body_if = function.new_block();
        let return_block = function.new_block();
        let continue_block = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![value.clone()], generator.into(), state, control).into(),
        );
        function.block_mut(body_if).unwrap().push(
            If::new(
                RValue::Global(Global::from("should_return")),
                Block::default(),
                Block::default(),
            )
            .into(),
        );
        function
            .block_mut(return_block)
            .unwrap()
            .push(Statement::Return(ast::Return { values: vec![value.into()] }).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(init, vec![(header, BlockEdge::new(BranchType::Unconditional))]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body_if, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body_if, vec![
            (return_block, BlockEdge::new(BranchType::Then)),
            (continue_block, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(continue_block, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(return_block, Vec::new());

        let output = lift(function)
            .expect("a conditional terminal return inside a generic-for should be source-shaped")
            .to_string();
        assert!(output.contains("for value in items do"), "{output}");
        assert!(output.contains("if should_return then"), "{output}");
        assert!(output.contains("return value"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn complete_generic_for_origin_loss_is_unsafe() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![value], generator.into(), state, control).into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![(header, BlockEdge::new(BranchType::Unconditional))]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(header, BlockEdge::new(BranchType::Unconditional))]);

        assert!(matches!(
            production_lift_attempt(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::ForOriginMissing)
        ));
    }

    #[test]
    fn accepts_explicit_next_tuple_with_compiler_nil_control() {
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let mut init = GenericForInit::new(generator, state, control);
        init.0.right = vec![
            RValue::Global(Global::from("next")),
            RValue::Global(Global::from("items")),
            RValue::Literal(Literal::Nil),
        ];
        let origin = ForOrigin {
            prep_pc: 1,
            step_pc: 2,
            body_pc: 3,
            follow_pc: 4,
            prep_kind: ForPrepKind::Next,
            base_register: 0,
            result_count: 2,
            aux: 2,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
        };
        assert!(super::source_proves_for_prep_kind(&init, origin));
        init.0.right[2] = RValue::Global(Global::from("extra"));
        assert!(!super::source_proves_for_prep_kind(&init, origin));
    }

    #[test]
    fn refuses_tagged_edge_on_straight_line_path() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);
        function
            .block_mut(entry)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("entry".into())).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        // A single Then edge is malformed CFG metadata, not a straight-line
        // transfer.  The region pass must reject it instead of discarding the
        // tag and silently changing control flow.
        function.set_edges(entry, vec![(exit, BlockEdge::new(BranchType::Then))]);
        assert!(lift(function).is_none());
    }

    #[test]
    fn non_terminating_scc_has_no_usable_postdominator() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let terminal = function.new_block();
        let infinite = function.new_block();
        function.set_entry(entry);
        function.block_mut(entry).unwrap().push(
            If::new(
                RValue::Global(Global::from("condition")),
                Block::default(),
                Block::default(),
            )
            .into(),
        );
        function
            .block_mut(terminal)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(entry, vec![
            (terminal, BlockEdge::new(BranchType::Then)),
            (infinite, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(infinite, vec![(
            infinite,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        let analysis = Analysis::new(&function).expect("analysis itself should be total");
        assert!(analysis.post_dominators[&infinite].is_empty());
        assert!(analysis.post_dominators[&entry].is_empty());
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_tagged_normal_exhaustion_adapter_edge() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let normal_exit = function.new_block();
        let join = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("body".into())).into());
        function.block_mut(normal_exit).unwrap().push(
            Assign::new(vec![LValue::Local(result)], vec![RValue::Literal(
                Literal::Nil,
            )])
            .into(),
        );
        function
            .block_mut(join)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (normal_exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        // The adapter has one outgoing edge, but it is malformed metadata.
        function.set_edges(normal_exit, vec![(join, BlockEdge::new(BranchType::Then))]);

        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_unmodeled_normal_exhaustion_edge_transfer() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let normal_exit = function.new_block();
        let break_adapter = function.new_block();
        let join = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        let keep = RcLocal::new(Local::new(Some("keep".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![result], generator.into(), state, control).into(),
        );
        function.block_mut(body).unwrap().push(
            If::new(RValue::Local(keep), Block::default(), Block::default()).into(),
        );
        function.block_mut(normal_exit).unwrap().push(
            Assign::new(
                vec![LValue::Local(sink.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        function
            .block_mut(join)
            .unwrap()
            .push(ast::Return::new(vec![RValue::Local(sink.clone())]).into());

        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (normal_exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![
            (break_adapter, BlockEdge::new(BranchType::Else)),
            (header, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(break_adapter, vec![
            (join, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(normal_exit, vec![
            (
                join,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(sink, Literal::Number(42.0).into())],
                },
            ),
        ]);

        let output = lift(function).map(|block| block.to_string());
        assert!(output.is_none(), "unexpected source-shaped output: {:?}", output);
    }

    #[test]
    fn refuses_live_result_export_without_exhaustion_value() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        function
            .block_mut(join)
            .unwrap()
            .push(ast::Return::new(vec![RValue::Local(result)]).into());

        function.set_edges(init, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);
        function.set_edges(header, vec![
            (join, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![
            (header, BlockEdge::new(BranchType::Unconditional)),
        ]);

        // FORGLOOP does not necessarily clear its first result on exhaustion;
        // exporting that live register as an outer source variable would
        // incorrectly force it to the pre-loop nil initialization.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_direct_exhaustion_nil_written_after_result_read() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let join = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        // The result is observed before the later nil write.  On exhaustion,
        // FORGLOOP may retain its last value, so that later write cannot prove
        // that the earlier read saw nil.
        function.block_mut(join).unwrap().extend([
            Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(result.clone())]).into(),
            Assign::new(vec![LValue::Local(result)], vec![RValue::Literal(Literal::Nil)]).into(),
            Statement::Return(Default::default()).into(),
        ]);

        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (join, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_post_loop_write_to_exported_result() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let normal_exit = function.new_block();
        let join = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("body".into())).into());
        function.block_mut(normal_exit).unwrap().push(
            Assign::new(vec![LValue::Local(result.clone())], vec![RValue::Literal(
                Literal::Nil,
            )])
            .into(),
        );
        // This write is outside the loop/normal adapter but the result is
        // exported below.  Rewriting it to the export would leave closures or
        // aliases of the original result register observing the wrong cell.
        function.block_mut(join).unwrap().extend(
            vec![
                Assign::new(vec![LValue::Local(result.clone())], vec![RValue::Literal(
                    Literal::Nil,
                )])
                .into(),
                Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(result)]).into(),
                Statement::Return(Default::default()).into(),
            ]
            .into_iter(),
        );
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (normal_exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(normal_exit, vec![(
            join,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(lift(function).is_none());
    }

    #[test]
    fn rejected_candidate_restores_local_id_allocator() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        // This malformed edge forces a fail-closed result.  The public region
        // API must be transactional even when called outside luau-lifter.
        function.set_edges(entry, vec![(exit, BlockEdge::new(BranchType::Then))]);
        let before = ast::current_local_id();
        assert!(lift(function).is_none());
        assert_eq!(ast::current_local_id(), before);
    }

    #[test]
    fn refuses_generic_for_protocol_reads_outside_markers() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        let after = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let value = RcLocal::new(Local::new(Some("value".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(
                vec![value],
                generator.clone().into(),
                state.clone(),
                control.clone(),
            )
            .into(),
        );
        // The hidden control register is read in the loop body and after the
        // loop.  A source `for` cannot expose the updated VM value, so this
        // must use the semantics-preserving fallback.
        function.block_mut(body).unwrap().push(
            Assign::new(vec![LValue::Local(sink.clone())], vec![RValue::Local(
                control.clone(),
            )])
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function
            .block_mut(after)
            .unwrap()
            .push(Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(control)]).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(exit, vec![(
            after,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(lift(function).is_none());
    }

    #[test]
    fn exports_nested_loop_result_before_outer_body_use() {
        let mut function = Function::new(0);
        let outer_init = function.new_block();
        let outer_header = function.new_block();
        let outer_body = function.new_block();
        let inner_init = function.new_block();
        let inner_header = function.new_block();
        let inner_body = function.new_block();
        let inner_exit = function.new_block();
        let after_inner = function.new_block();
        let outer_exit = function.new_block();
        function.set_entry(outer_init);

        let outer_generator = RcLocal::new(Local::new(Some("outer_generator".into())));
        let outer_state = RcLocal::new(Local::new(Some("outer_state".into())));
        let outer_control = RcLocal::new(Local::new(Some("outer_control".into())));
        let outer_value = RcLocal::new(Local::new(Some("outer_value".into())));
        let inner_generator = RcLocal::new(Local::new(Some("inner_generator".into())));
        let inner_state = RcLocal::new(Local::new(Some("inner_state".into())));
        let inner_control = RcLocal::new(Local::new(Some("inner_control".into())));
        let inner_value = RcLocal::new(Local::new(Some("inner_value".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));

        let mut outer_for_init = GenericForInit::new(
            outer_generator.clone(),
            outer_state.clone(),
            outer_control.clone(),
        );
        // Deliberately alias the outer iterator RHS with the inner result.  A
        // body-created export must not rewrite this pre-body expression.
        outer_for_init.0.right = vec![RValue::Local(inner_value.clone())];
        function
            .block_mut(outer_init)
            .unwrap()
            .push(outer_for_init.into());
        function.block_mut(outer_header).unwrap().push(
            GenericForNext::new(
                vec![outer_value],
                outer_generator.clone().into(),
                outer_state.clone(),
                outer_control.clone(),
            )
            .into(),
        );
        function.block_mut(inner_init).unwrap().push({
            let mut init = GenericForInit::new(
                inner_generator.clone(),
                inner_state.clone(),
                inner_control.clone(),
            );
            init.0.right = vec![RValue::Global(Global::from("inner_items"))];
            init.into()
        });
        function.block_mut(inner_header).unwrap().push(
            GenericForNext::new(
                vec![inner_value.clone()],
                inner_generator.clone().into(),
                inner_state.clone(),
                inner_control.clone(),
            )
            .into(),
        );
        function
            .block_mut(outer_body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("outer body".into())).into());
        function
            .block_mut(inner_body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("inner body".into())).into());
        function.block_mut(inner_exit).unwrap().push(
            Assign::new(
                vec![LValue::Local(inner_value.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        function.block_mut(after_inner).unwrap().push(
            Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(
                inner_value.clone(),
            )])
            .into(),
        );
        function
            .block_mut(outer_exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(outer_init, vec![(
            outer_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(outer_header, vec![
            (outer_exit, BlockEdge::new(BranchType::Else)),
            (outer_body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(outer_body, vec![(
            inner_init,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(inner_init, vec![(
            inner_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(inner_header, vec![
            (inner_exit, BlockEdge::new(BranchType::Else)),
            (inner_body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(inner_body, vec![(
            inner_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(inner_exit, vec![(
            after_inner,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(after_inner, vec![(
            outer_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        // The assignment is after the nested loop and must be rewritten to its
        // explicit live-out export rather than referencing a loop-local value.
        let output = lift(function).expect("nested generic-for should be source-shaped");
        let output = output.to_string();
        assert!(output.matches("for ").count() >= 2, "{output}");
        assert!(
            output.contains("for outer_value in inner_value do"),
            "{output}"
        );
        assert!(!output.contains("sink = inner_value"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn structures_pet_shaped_nested_loop_break_through_outer_region() {
        let mut function = Function::new(0);
        let outer_init = function.new_block();
        let outer_header = function.new_block();
        let inner_init = function.new_block();
        let inner_header = function.new_block();
        let inner_if = function.new_block();
        let inner_continue = function.new_block();
        let inner_break_adapter = function.new_block();
        let inner_exhaustion = function.new_block();
        let after_inner = function.new_block();
        let keep = function.new_block();
        let remove = function.new_block();
        let outer_exit = function.new_block();
        function.set_entry(outer_init);

        let outer_generator = RcLocal::new(Local::new(Some("outer_generator".into())));
        let outer_state = RcLocal::new(Local::new(Some("outer_state".into())));
        let outer_control = RcLocal::new(Local::new(Some("outer_control".into())));
        let key = RcLocal::new(Local::new(Some("key".into())));
        let inner_generator = RcLocal::new(Local::new(Some("inner_generator".into())));
        let inner_state = RcLocal::new(Local::new(Some("inner_state".into())));
        let inner_control = RcLocal::new(Local::new(Some("inner_control".into())));
        let pet = RcLocal::new(Local::new(Some("pet".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));

        let mut outer_for_init = GenericForInit::new(
            outer_generator.clone(),
            outer_state.clone(),
            outer_control.clone(),
        );
        outer_for_init.0.right = vec![RValue::Global(Global::from("pets"))];
        function
            .block_mut(outer_init)
            .unwrap()
            .push(outer_for_init.into());
        function.block_mut(outer_header).unwrap().push(
            GenericForNext::new(
                vec![key],
                outer_generator.clone().into(),
                outer_state,
                outer_control,
            )
            .into(),
        );

        let mut inner_for_init = GenericForInit::new(
            inner_generator.clone(),
            inner_state.clone(),
            inner_control.clone(),
        );
        inner_for_init.0.right = vec![RValue::Global(Global::from("all_pets"))];
        function
            .block_mut(inner_init)
            .unwrap()
            .push(inner_for_init.into());
        function.block_mut(inner_header).unwrap().push(
            GenericForNext::new(
                vec![pet.clone()],
                inner_generator.clone().into(),
                inner_state,
                inner_control,
            )
            .into(),
        );
        function.block_mut(inner_if).unwrap().push(
            If::new(
                RValue::Global(Global::from("matches")),
                Block::default(),
                Block::default(),
            )
            .into(),
        );
        function.block_mut(inner_exhaustion).unwrap().push({
            let mut exhaustion_nil =
                Assign::new(vec![LValue::Local(pet.clone())], vec![RValue::Literal(
                    Literal::Nil,
                )]);
            // SSA destruction may leave a one-value parallel copy here.  It
            // is semantically the same nil write and must not prevent the
            // source-shaped loop from recognizing the exhaustion adapter.
            exhaustion_nil.parallel = true;
            exhaustion_nil.into()
        });
        function
            .block_mut(after_inner)
            .unwrap()
            .push(If::new(RValue::Local(pet), Block::default(), Block::default()).into());
        function.block_mut(remove).unwrap().push(
            Assign::new(vec![LValue::Local(sink)], vec![RValue::Literal(
                Literal::Nil,
            )])
            .into(),
        );
        function
            .block_mut(outer_exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(outer_init, vec![(
            outer_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(outer_header, vec![
            (outer_exit, BlockEdge::new(BranchType::Else)),
            (inner_init, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(inner_init, vec![(
            inner_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(inner_header, vec![
            (inner_exhaustion, BlockEdge::new(BranchType::Else)),
            (inner_if, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(inner_if, vec![
            (inner_continue, BlockEdge::new(BranchType::Else)),
            (inner_break_adapter, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(inner_continue, vec![(
            inner_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(inner_break_adapter, vec![(
            after_inner,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(inner_exhaustion, vec![(
            after_inner,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(after_inner, vec![
            (keep, BlockEdge::new(BranchType::Then)),
            (remove, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(keep, vec![(
            outer_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(remove, vec![(
            outer_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        let output = lift(function)
            .expect("a Pet-shaped nested generic-for with an inner break should be source-shaped")
            .to_string();
        assert_eq!(output.matches("for ").count(), 2, "{output}");
        assert!(output.contains("break"), "{output}");
        assert!(output.contains("if matches then"), "{output}");
        assert!(!output.contains("GenericFor"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn rejects_nested_loop_escape_to_enclosing_join() {
        let mut function = Function::new(0);
        let outer_init = function.new_block();
        let outer_header = function.new_block();
        let inner_init = function.new_block();
        let inner_header = function.new_block();
        let inner_if = function.new_block();
        let inner_continue = function.new_block();
        let after_inner = function.new_block();
        let outer_exit = function.new_block();
        function.set_entry(outer_init);

        let outer_generator = RcLocal::new(Local::new(Some("outer_generator".into())));
        let outer_state = RcLocal::new(Local::new(Some("outer_state".into())));
        let outer_control = RcLocal::new(Local::new(Some("outer_control".into())));
        let outer_value = RcLocal::new(Local::new(Some("outer_value".into())));
        let inner_generator = RcLocal::new(Local::new(Some("inner_generator".into())));
        let inner_state = RcLocal::new(Local::new(Some("inner_state".into())));
        let inner_control = RcLocal::new(Local::new(Some("inner_control".into())));
        let inner_value = RcLocal::new(Local::new(Some("inner_value".into())));

        let mut outer_for_init = GenericForInit::new(
            outer_generator.clone(),
            outer_state.clone(),
            outer_control.clone(),
        );
        outer_for_init.0.right = vec![RValue::Global(Global::from("outer_items"))];
        function
            .block_mut(outer_init)
            .unwrap()
            .push(outer_for_init.into());
        function.block_mut(outer_header).unwrap().push(
            GenericForNext::new(
                vec![outer_value],
                outer_generator.clone().into(),
                outer_state,
                outer_control,
            )
            .into(),
        );

        let mut inner_for_init = GenericForInit::new(
            inner_generator.clone(),
            inner_state.clone(),
            inner_control.clone(),
        );
        inner_for_init.0.right = vec![RValue::Global(Global::from("inner_items"))];
        function
            .block_mut(inner_init)
            .unwrap()
            .push(inner_for_init.into());
        function.block_mut(inner_header).unwrap().push(
            GenericForNext::new(
                vec![inner_value],
                inner_generator.clone().into(),
                inner_state,
                inner_control,
            )
            .into(),
        );
        function.block_mut(inner_if).unwrap().push(
            If::new(
                RValue::Global(Global::from("leave_outer")),
                Block::default(),
                Block::default(),
            )
            .into(),
        );
        function
            .block_mut(outer_exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(outer_init, vec![(
            outer_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(outer_header, vec![
            (outer_exit, BlockEdge::new(BranchType::Else)),
            (inner_init, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(inner_init, vec![(
            inner_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(inner_header, vec![
            (after_inner, BlockEdge::new(BranchType::Else)),
            (inner_if, BlockEdge::new(BranchType::Then)),
        ]);
        // This branch skips the enclosing loop's body tail and exits the
        // parent directly.  A nested source `break` cannot represent that
        // transfer, so source-like structuring must decline the graph.
        function.set_edges(inner_if, vec![
            (outer_exit, BlockEdge::new(BranchType::Then)),
            (inner_continue, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(inner_continue, vec![(
            inner_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(after_inner, vec![(
            outer_header,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        assert!(lift(function).is_none());
    }

    #[test]
    fn preserves_shared_normal_exit_adapter_on_body_break() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let first_if = function.new_block();
        let second_if = function.new_block();
        let continue_node = function.new_block();
        let normal_exit = function.new_block();
        let other_exit = function.new_block();
        let join = function.new_block();
        let function_exit = function.new_block();
        function.set_entry(init);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let sink = RcLocal::new(Local::new(Some("sink".into())));

        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("body".into())).into());
        function.block_mut(first_if).unwrap().push(
            If::new(
                RValue::Global(Global::from("first")),
                Block::default(),
                Block::default(),
            )
            .into(),
        );
        function.block_mut(second_if).unwrap().push(
            If::new(
                RValue::Global(Global::from("second")),
                Block::default(),
                Block::default(),
            )
            .into(),
        );
        function
            .block_mut(continue_node)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("continue".into())).into());
        function.block_mut(normal_exit).unwrap().push(
            Assign::new(vec![LValue::Local(result.clone())], vec![RValue::Literal(
                Literal::Nil,
            )])
            .into(),
        );
        function
            .block_mut(other_exit)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("other exit".into())).into());
        function.block_mut(join).unwrap().push(
            Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(
                result.clone(),
            )])
            .into(),
        );
        function
            .block_mut(function_exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (normal_exit, BlockEdge::new(BranchType::Else)),
            (body, BlockEdge::new(BranchType::Then)),
        ]);
        function.set_edges(body, vec![(
            first_if,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        // The first Then arm breaks through the same adapter used by normal
        // exhaustion.  Skipping this adapter would leak the previous result.
        function.set_edges(first_if, vec![
            (normal_exit, BlockEdge::new(BranchType::Then)),
            (second_if, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(second_if, vec![
            (other_exit, BlockEdge::new(BranchType::Then)),
            (continue_node, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(continue_node, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(normal_exit, vec![(
            join,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(other_exit, vec![(
            join,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(join, vec![(
            function_exit,
            BlockEdge::new(BranchType::Unconditional),
        )]);

        let output = lift(function)
            .expect("a body break through normal exit must remain source-shaped")
            .to_string();
        assert!(output.contains("if first then"), "{output}");
        assert!(output.contains("result = nil"), "{output}");
        assert!(output.contains("break"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }
}
