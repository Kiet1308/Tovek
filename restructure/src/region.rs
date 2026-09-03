//! A conservative, source-shaped CFG structurer.
//!
//! The regular matcher is intentionally permissive because it is useful for
//! partially reduced graphs.  This pass is the opposite: it never mutates the
//! CFG and only returns an AST after proving that all reachable blocks have a
//! unique source-level owner.  Any uncertainty returns `None`, leaving the
//! existing structurer and its semantics-preserving dispatcher as fallbacks.

use ast::{
    Assign, Binary, Block, GenericFor, If, LValue, Literal, LocalRw, RValue, RcLocal, Reduce,
    Statement, Traverse, Unary, UnaryOperation,
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
    while_condition: Option<RValue>,
    /// Numeric-for loops do not carry the generic iterator provenance or
    /// result tuple.  Keep their semantic operands alongside the common
    /// region ownership so nested numeric loops can use the same path/exit
    /// machinery as generic-for loops.
    numeric: Option<NumericLoopInfo>,
}

#[derive(Clone)]
struct NumericLoopInfo {
    counter: RcLocal,
    initial: RValue,
    limit: RValue,
    step: RValue,
}

struct Analysis {
    reachable: FxHashSet<NodeIndex>,
    nodes: Vec<NodeIndex>,
    post_dominators: FxHashMap<NodeIndex, FxHashSet<NodeIndex>>,
    live_in: FxHashMap<NodeIndex, FxHashSet<RcLocal>>,
    live_out: FxHashMap<NodeIndex, FxHashSet<RcLocal>>,
    loops_by_init: FxHashMap<NodeIndex, LoopInfo>,
    loops_by_header: FxHashMap<NodeIndex, LoopInfo>,
    numeric_loops_by_init: FxHashMap<NodeIndex, LoopInfo>,
    numeric_loops_by_header: FxHashMap<NodeIndex, LoopInfo>,
    while_loops_by_header: FxHashMap<NodeIndex, LoopInfo>,
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

fn block_has_rewritten_closure(block: &Block, rewrite: &FxHashMap<RcLocal, RcLocal>) -> bool {
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

fn rvalue_contains_close_with_seen(value: &RValue, seen_closures: &mut FxHashSet<usize>) -> bool {
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

fn block_contains_close_with_seen(block: &Block, seen_closures: &mut FxHashSet<usize>) -> bool {
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
        Statement::While(node) => block_contains_close_with_seen(&node.block.lock(), seen_closures),
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
                || block_contains_unlowered_control_with_seen(
                    &node.else_block.lock(),
                    seen_closures,
                )
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
        let (numeric_loops_by_init, numeric_loops_by_header) =
            Self::find_numeric_loops(function, &nodes, &reachable, &dominators, &post_dominators);
        let while_loops_by_header =
            Self::find_while_loops(function, &nodes, &reachable, &dominators, &post_dominators);
        Some(Self {
            reachable,
            nodes,
            post_dominators,
            live_in,
            live_out,
            loops_by_init,
            loops_by_header,
            numeric_loops_by_init,
            numeric_loops_by_header,
            while_loops_by_header,
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
    fn ref_captured_locals_before_init(
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
                collect_statement_ref_captures_with_seen(
                    statement,
                    &mut captured,
                    &mut FxHashSet::default(),
                );
            }
            for edge in function.edges(*node) {
                if pre_nodes.contains(&edge.target()) {
                    for (_, value) in &edge.weight().arguments {
                        collect_rvalue_ref_captures(value, &mut captured);
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
                    || function.block_at_pc(origin.body_pc).is_some_and(|node| {
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
                        if !reachable.contains(&target) || target == header || target == normal_exit
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
                owned
                    .contains(&body_entry)
                    .then_some((header, owned, origin))
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
        for (header, mut nodes_in_loop) in candidates {
            let Some(next) = function
                .block(header)
                .and_then(|block| block.last())
                .and_then(|statement| statement.as_generic_for_next())
            else {
                // Unmodelled numeric/irreducible cycles are handled by the
                // existing path and dispatcher structurers.
                continue;
            };
            // The provenance envelope identifies the bytecode interval owned
            // by this FORGLOOP body.  Natural-loop reverse walks can cross an
            // enclosing loop when a body branch exits early and later
            // re-enters the iterator preparation (the common `for` inside a
            // retry/while shape).  Trim only nodes whose concrete PC range is
            // outside that interval; unknown-range adapter blocks are kept
            // for the structural checks below.  This turns the compiler's
            // exact body/follow metadata into an ownership boundary instead
            // of rejecting an otherwise reducible nested loop.
            if let Some(origin) = next.origin() {
                nodes_in_loop.retain(|node| {
                    *node == header
                        || function.block_pc_range(*node).is_none_or(|range| {
                            range.start >= origin.body_pc && range.start < origin.follow_pc
                        })
                });
            }
            let (then_edge, else_edge) = function.conditional_edges(header)?;
            let body_entry = then_edge.target();
            let normal_exit = else_edge.target();
            let direct_break = body_entry == normal_exit;
            if direct_break {
                // The direct-break form has no body node: the follow block is
                // shared by both FORGLOOP arms and must remain outside the
                // loop so the surrounding path can consume it once.
                if nodes_in_loop.len() != 1 {
                    continue;
                }
            } else if !nodes_in_loop.contains(&body_entry) || nodes_in_loop.contains(&normal_exit) {
                continue;
            }
            // A natural loop with an entry other than its header cannot be
            // represented by a single source `for`.  Reject the candidate as
            // a whole; a `continue` inside this inner iterator would merely
            // advance to the next node and accidentally accept the region.
            if nodes_in_loop.iter().any(|node| {
                *node != header
                    && function
                        .predecessor_blocks(*node)
                        .any(|predecessor| !nodes_in_loop.contains(&predecessor))
            }) {
                continue;
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
                continue;
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
                continue;
            }
            let res_locals = next
                .res_locals
                .iter()
                .map(|lvalue| lvalue.as_local().cloned())
                .collect::<Option<Vec<_>>>()?;
            if res_locals.is_empty() || init_statement.0.right.is_empty() {
                continue;
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
                        continue;
                    }
                    Some(init_origin)
                }
                _ => None,
            }) else {
                continue;
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
                || function.block_at_pc(origin.body_pc).is_some_and(|node| {
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
                continue;
            }
            let external_targets = nodes_in_loop
                .iter()
                .flat_map(|node| function.successor_blocks(*node))
                .filter(|target| !nodes_in_loop.contains(target))
                .unique()
                .collect_vec();
            let join = common_postdominator(&external_targets, post_dominators)?;
            if nodes_in_loop.contains(&join) {
                continue;
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
                while_condition: None,
                numeric: None,
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
                continue;
            }
        }
        Some((by_init, by_header))
    }

    /// Discover reducible `while` regions from a conditional header and a
    /// natural backedge.  Luau's jump structuring may leave a source `while`
    /// as a plain `If` block whose body eventually reaches the header through
    /// a nested iterator; recovering that shape here prevents a harmless
    /// loop-carried condition from becoming a cross-loop goto.
    fn find_while_loops(
        function: &Function,
        nodes: &[NodeIndex],
        reachable: &FxHashSet<NodeIndex>,
        dominators: &Dominators<NodeIndex>,
        post_dominators: &FxHashMap<NodeIndex, FxHashSet<NodeIndex>>,
    ) -> FxHashMap<NodeIndex, LoopInfo> {
        let mut candidates = FxHashMap::<NodeIndex, FxHashSet<NodeIndex>>::default();
        for source in nodes {
            for edge in function.edges(*source) {
                let header = edge.target();
                if !reachable.contains(&header)
                    || *source == header
                    || !dominators
                        .dominators(*source)
                        .is_some_and(|mut ds| ds.any(|candidate| candidate == header))
                {
                    continue;
                }
                let Some(block) = function.block(header) else {
                    continue;
                };
                if block
                    .last()
                    .and_then(|statement| statement.as_if())
                    .is_none()
                    || function.conditional_edges(header).is_none()
                {
                    continue;
                }
                let owned = candidates.entry(header).or_default();
                owned.insert(header);
                owned.insert(*source);
                let mut work = vec![*source];
                while let Some(node) = work.pop() {
                    for predecessor in function.predecessor_blocks(node) {
                        if reachable.contains(&predecessor)
                            && predecessor != header
                            && owned.insert(predecessor)
                        {
                            work.push(predecessor);
                        }
                    }
                }
            }
        }

        let mut result = FxHashMap::default();
        for (header, mut owned) in candidates {
            let Some((then_edge, else_edge)) = function.conditional_edges(header) else {
                continue;
            };
            let then_target = then_edge.target();
            let else_target = else_edge.target();
            let then_inside = owned.contains(&then_target);
            let else_inside = owned.contains(&else_target);
            if then_inside == else_inside {
                continue;
            }
            let (body_entry, normal_exit) = if then_inside {
                (then_target, else_target)
            } else {
                (else_target, then_target)
            };
            // A body node with an incoming edge bypassing the header is a
            // multi-entry region and cannot be represented by one `while`.
            if owned.iter().any(|node| {
                *node != header
                    && function
                        .predecessor_blocks(*node)
                        .any(|predecessor| !owned.contains(&predecessor))
            }) {
                continue;
            }
            // Keep ownership inside this candidate's natural interval.  A
            // nested source loop may contribute its own nodes, but a sibling
            // tail reached only after the loop must not be absorbed.
            owned.insert(body_entry);
            let external_targets = owned
                .iter()
                .flat_map(|node| function.successor_blocks(*node))
                .filter(|target| !owned.contains(target))
                .unique()
                .collect_vec();
            let Some(join) = common_postdominator(&external_targets, post_dominators) else {
                continue;
            };
            if owned.contains(&join) || join == header {
                continue;
            }
            let mut condition = function
                .block(header)
                .and_then(|block| block.last())
                .and_then(|statement| statement.as_if())
                .map(|if_statement| if_statement.condition.clone());
            let Some(mut condition) = condition.take() else {
                continue;
            };
            if !then_inside {
                condition = Unary::new(condition, UnaryOperation::Not).reduce_condition();
            }
            result.insert(
                header,
                LoopInfo {
                    header,
                    init: header,
                    body_entry,
                    normal_exit,
                    join,
                    nodes: owned,
                    res_locals: Vec::new(),
                    right: Vec::new(),
                    origin: None,
                    while_condition: Some(condition),
                    numeric: None,
                },
            );
        }
        result
    }

    /// Discover numeric-for regions from the explicit FORNPREP/FORNLOOP
    /// markers.  Numeric loops have no generic iterator protocol or
    /// provenance payload, but their marker pair still carries the complete
    /// source operands and a reducible CFG gives us an exact source-level
    /// representation.  Keep this analysis separate from generic-for
    /// discovery so a malformed numeric candidate cannot weaken the latter's
    /// provenance checks.
    fn find_numeric_loops(
        function: &Function,
        nodes: &[NodeIndex],
        reachable: &FxHashSet<NodeIndex>,
        dominators: &Dominators<NodeIndex>,
        post_dominators: &FxHashMap<NodeIndex, FxHashSet<NodeIndex>>,
    ) -> (
        FxHashMap<NodeIndex, LoopInfo>,
        FxHashMap<NodeIndex, LoopInfo>,
    ) {
        let mut candidates = FxHashMap::<NodeIndex, FxHashSet<NodeIndex>>::default();
        for source in nodes {
            for edge in function.edges(*source) {
                let header = edge.target();
                if !reachable.contains(&header)
                    || function
                        .block(header)
                        .and_then(|block| block.last())
                        .and_then(|statement| statement.as_num_for_next())
                        .is_none()
                    || !dominators
                        .dominators(*source)
                        .is_some_and(|mut ds| ds.any(|candidate| candidate == header))
                {
                    continue;
                }
                let set = candidates.entry(header).or_default();
                set.insert(header);
                set.insert(*source);
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

        let mut by_init = FxHashMap::default();
        let mut by_header = FxHashMap::default();
        for (header, nodes_in_loop) in candidates {
            let Some(next) = function
                .block(header)
                .and_then(|block| block.last())
                .and_then(|statement| statement.as_num_for_next())
            else {
                continue;
            };
            let Some((then_edge, else_edge)) = function.conditional_edges(header) else {
                continue;
            };
            let body_entry = then_edge.target();
            let normal_exit = else_edge.target();
            // A header whose body and exhaustion arms alias the same CFG node
            // has no source-level representation: emitting `break` would
            // execute one iteration, while an aliased branch can also denote
            // an empty/infinite protocol shape.  Keep this ambiguous marker
            // pair fail-closed rather than guessing a body.
            if body_entry == normal_exit {
                continue;
            }
            if !nodes_in_loop.contains(&body_entry) || nodes_in_loop.contains(&normal_exit) {
                continue;
            }

            // Every body node must be owned solely by this loop.  A shared
            // entry/tail would require a path-sensitive region split that the
            // source-like builder does not yet model.
            if nodes_in_loop.iter().any(|node| {
                *node != header
                    && function
                        .predecessor_blocks(*node)
                        .any(|predecessor| !nodes_in_loop.contains(&predecessor))
            }) {
                continue;
            }
            let inits = function
                .predecessor_blocks(header)
                .filter(|predecessor| !nodes_in_loop.contains(predecessor))
                .filter(|predecessor| {
                    function.block(*predecessor).is_some_and(|block| {
                        block
                            .iter()
                            .any(|statement| statement.as_num_for_init().is_some())
                    })
                })
                .collect_vec();
            if inits.len() != 1
                || function.predecessor_blocks(header).any(|predecessor| {
                    !nodes_in_loop.contains(&predecessor) && predecessor != inits[0]
                })
            {
                continue;
            }
            let init = inits[0];
            let Some(init_statement) = function
                .block(init)
                .and_then(|block| block.iter().find_map(|s| s.as_num_for_init()))
            else {
                continue;
            };
            let (Some(counter), Some(limit_local), Some(step_local)) = (
                init_statement.counter.0.as_local().cloned(),
                init_statement.limit.0.as_local().cloned(),
                init_statement.step.0.as_local().cloned(),
            ) else {
                continue;
            };
            let Some(next_counter) = next.counter.0.as_local() else {
                continue;
            };
            if next_counter != &counter
                || next.counter.1 != RValue::Local(counter.clone())
                || next.limit != RValue::Local(limit_local)
                || next.step != RValue::Local(step_local)
            {
                continue;
            }
            let external_targets = nodes_in_loop
                .iter()
                .flat_map(|node| function.successor_blocks(*node))
                .filter(|target| !nodes_in_loop.contains(target))
                .unique()
                .collect_vec();
            let Some(join) = common_postdominator(&external_targets, post_dominators) else {
                continue;
            };
            if nodes_in_loop.contains(&join) {
                continue;
            }
            let info = LoopInfo {
                header,
                init,
                body_entry,
                normal_exit,
                join,
                nodes: nodes_in_loop,
                res_locals: Vec::new(),
                right: Vec::new(),
                origin: None,
                while_condition: None,
                numeric: Some(NumericLoopInfo {
                    counter,
                    initial: init_statement.counter.1.clone(),
                    limit: init_statement.limit.1.clone(),
                    step: init_statement.step.1.clone(),
                }),
            };
            if by_init.insert(init, info.clone()).is_some()
                || by_header.insert(header, info).is_some()
            {
                // Ambiguous marker identity is unsupported; discard both
                // maps rather than selecting one candidate nondeterministically.
                return (FxHashMap::default(), FxHashMap::default());
            }
        }
        // Natural loops may be nested, but two partially-overlapping numeric
        // regions cannot each be represented by one source `for`.  Reject the
        // whole analysis rather than allowing hash-map iteration order to pick
        // one candidate and silently consume the other's body.
        let infos = by_header.values().collect_vec();
        for (index, left) in infos.iter().enumerate() {
            for right in infos.iter().skip(index + 1) {
                let overlap = left.nodes.intersection(&right.nodes).count();
                if overlap != 0
                    && !(left.nodes.is_subset(&right.nodes) || right.nodes.is_subset(&left.nodes))
                {
                    return (FxHashMap::default(), FxHashMap::default());
                }
            }
        }
        (by_init, by_header)
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
    /// Set to `false` on a body-side `break` when the loop has an
    /// exhaustion-only adapter.  The adapter must not run for an explicit
    /// break: doing so can overwrite a value selected by the break path.
    exhaustion_flag: Option<RcLocal>,
}

struct PathResult {
    block: Block,
    next: Option<NodeIndex>,
}

/// A post-loop tail that deterministically either re-enters a loop
/// preparation or terminates.  This narrow shape is how Luau represents a
/// source `while true` wrapped around a generic iterator.
struct ReentryTail {
    nodes: FxHashSet<NodeIndex>,
}

struct ReentryTailResult {
    block: Block,
    reenters: bool,
    terminates: bool,
}

struct Builder<'a> {
    function: &'a Function,
    analysis: Analysis,
    visited: FxHashSet<NodeIndex>,
    rewrite: FxHashMap<RcLocal, RcLocal>,
    protected_locals: FxHashSet<RcLocal>,
    unsafe_reason: Option<UnsafeStructureReason>,
    /// Structure a tail shared by both arms of a conditional once, after the
    /// `if`, instead of once per arm.  Disabled on the retry pass so the
    /// readability optimization can never turn a structurable function into
    /// an unsupported one.
    allow_shared_tail: bool,
}

impl<'a> Builder<'a> {
    fn new(
        function: &'a Function,
        analysis: Analysis,
        protected_locals: FxHashSet<RcLocal>,
    ) -> Self {
        let suffix_unsafe_reason = analysis
            .loops_by_init
            .values()
            .filter(|info| info.numeric.is_none())
            .find_map(|info| {
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
                let pre_init_captures =
                    analysis.ref_captured_locals_before_init(function, info.init);
                if suffix.clone().any(|statement| {
                    statement
                        .values_written()
                        .into_iter()
                        .any(|written| pre_init_captures.contains(written))
                }) {
                    return Some(UnsafeStructureReason::CapturedCellReorder);
                }
                let right_captures =
                    info.right
                        .iter()
                        .fold(FxHashSet::default(), |mut captures, value| {
                            collect_rvalue_ref_captures(value, &mut captures);
                            captures
                        });
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
                analysis
                    .nodes
                    .iter()
                    .any(|node| {
                        function
                            .block(*node)
                            .is_some_and(block_contains_hidden_unlowered_control)
                            || function.edges(*node).any(|edge| {
                                edge.weight()
                                    .arguments
                                    .iter()
                                    .any(|(_, value)| rvalue_contains_unlowered_control(value))
                            })
                    })
                    .then_some(UnsafeStructureReason::UnmodeledControl)
            })
            .or_else(|| {
                analysis
                    .nodes
                    .iter()
                    .any(|node| {
                        function.block(*node).is_some_and(block_contains_close)
                            || function.edges(*node).any(|edge| {
                                edge.weight()
                                    .arguments
                                    .iter()
                                    .any(|(_, value)| rvalue_contains_close(value))
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
            allow_shared_tail: true,
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

    /// Prove the value copied into a loop result on the exhaustion path is
    /// nil.  Luau commonly lowers a loop-scoped result that is reused by an
    /// outer register as `result = saved_nil` rather than an explicit literal
    /// nil store.  Treating every local copy as nil would be unsound, so the
    /// source local must have one explicit nil definition and no other writes
    /// (including edge-local SSA transfers).
    fn is_proven_nil_copy(&self, info: &LoopInfo, statement: &Statement, result: &RcLocal) -> bool {
        let Statement::Assign(assign) = statement else {
            return false;
        };
        if assign.left.len() != 1
            || assign.right.len() != 1
            || assign.left[0].as_local() != Some(result)
        {
            return false;
        }
        let RValue::Local(source) = &assign.right[0] else {
            return false;
        };
        let mut saw_nil_definition = false;
        for node in &self.analysis.nodes {
            let Some(block) = self.function.block(*node) else {
                continue;
            };
            for candidate in block.iter() {
                if !candidate
                    .values_written()
                    .into_iter()
                    .any(|written| written == source)
                {
                    continue;
                }
                if *node == info.init && Self::is_nil_assignment(candidate, source) {
                    saw_nil_definition = true;
                } else {
                    return false;
                }
            }
            if self.function.edges(*node).any(|edge| {
                edge.weight()
                    .arguments
                    .iter()
                    .any(|(destination, _)| destination == source)
            }) {
                return false;
            }
        }
        saw_nil_definition
    }

    fn normal_adapter_nodes(
        &self,
        info: &LoopInfo,
        exports: &[(RcLocal, RcLocal)],
    ) -> Option<Vec<NodeIndex>> {
        let mut nil_writes = FxHashSet::default();
        let mut adapter_writes = FxHashSet::default();
        let mut nodes = Vec::new();
        let mut current = info.normal_exit;
        let mut seen = FxHashSet::default();
        while current != info.join {
            if !seen.insert(current) || info.nodes.contains(&current) {
                return None;
            }
            // The exhaustion adapter is emitted exactly once after the source
            // `for`.  Its only reachable entries must therefore be the
            // FORGLOOP exhaustion edge, a body-side break, or the preceding
            // adapter in this linear chain.  If an unrelated path shares the
            // adapter, emitting it unconditionally after the loop would move
            // its side effects across that path (and marking it visited would
            // make the other path disappear from the source-like walk).
            let predecessors = self
                .function
                .predecessor_blocks(current)
                .filter(|predecessor| self.analysis.reachable.contains(predecessor));
            if predecessors.into_iter().any(|predecessor| {
                !info.nodes.contains(&predecessor)
                    && !seen.contains(&predecessor)
                    && !(current == info.normal_exit && predecessor == info.header)
            }) {
                return None;
            }
            let block = self.function.block(current)?;
            for statement in block.iter() {
                for local in &info.res_locals {
                    if matches!(
                        statement,
                        Statement::Assign(assign)
                            if assign.left.len() == 1
                                && assign.right.len() == 1
                                && assign.left[0].as_local() == Some(local)
                    ) {
                        adapter_writes.insert(local.clone());
                    }
                    if Self::is_nil_assignment(statement, local)
                        || self.is_proven_nil_copy(info, statement, local)
                    {
                        nil_writes.insert(local.clone());
                    }
                }
                if !is_ignorable(statement)
                    && !info
                        .res_locals
                        .iter()
                        .any(|local| Self::is_nil_assignment(statement, local))
                    && !is_linear_statement(statement)
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
            !proven && !adapter_writes.contains(local)
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
                                && is_linear_statement(statement))
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

    /// Reference-capturing a generic-for result requires a per-iteration cell.
    /// The CFG currently treats `Close` as an unmodelled event, so there is no
    /// sound way to prove that lifetime.  Reject every such capture rather
    /// than treating a body mutation as evidence of a close operation.
    fn has_ref_captured_result(&self, info: &LoopInfo) -> bool {
        info.nodes.iter().any(|node| {
            self.function.block(*node).is_some_and(|block| {
                block
                    .iter()
                    .any(|statement| statement_has_ref_capture_of(statement, &info.res_locals))
            }) || self.function.edges(*node).any(|edge| {
                edge.weight()
                    .arguments
                    .iter()
                    .any(|(_, value)| rvalue_has_ref_capture_of(value, &info.res_locals))
            })
        })
    }

    /// A generic-for result captured by reference is exact as a plain source
    /// `for` body capture when the result local stays owned by the loop: it
    /// is declared by the emitted `for`, is not renamed or exported, and no
    /// statement or transfer outside the loop's own nodes reads, writes, or
    /// captures it.  Each source iteration then binds a fresh cell exactly as
    /// the VM does (the compiler closes captured loop-scope locals on every
    /// fallthrough/`continue`/`break` boundary), so closures created in
    /// different iterations observe their own iteration's final value.
    fn captured_result_is_loop_owned(
        &self,
        info: &LoopInfo,
        exports: &[(RcLocal, RcLocal)],
    ) -> bool {
        info.res_locals.iter().all(|result| {
            let captured = info.nodes.iter().any(|node| {
                self.function.block(*node).is_some_and(|block| {
                    block.iter().any(|statement| {
                        statement_has_ref_capture_of(statement, std::slice::from_ref(result))
                    })
                })
            });
            if !captured {
                return true;
            }
            // A ref-captured result is itself an upvalue cell, so it is always
            // in the protected set; that is expected here and is not an escape.
            if self.rewrite.contains_key(result) || exports.iter().any(|(local, _)| local == result)
            {
                return false;
            }
            !self.analysis.nodes.iter().any(|node| {
                if info.nodes.contains(node) {
                    return false;
                }
                self.function.block(*node).is_some_and(|block| {
                    block.iter().any(|statement| {
                        statement.values_read().into_iter().any(|read| read == result)
                            || statement
                                .values_written()
                                .into_iter()
                                .any(|written| written == result)
                            || statement_captures_any(statement, std::slice::from_ref(result))
                    })
                }) || self.function.edges(*node).any(|edge| {
                    edge.weight().arguments.iter().any(|(destination, value)| {
                        destination == result
                            || value.values_read().into_iter().any(|read| read == result)
                            || rvalue_captures_any(value, std::slice::from_ref(result))
                    })
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
                edge.weight()
                    .arguments
                    .iter()
                    .any(|(_, value)| rvalue_captures_any(value, &info.res_locals))
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

    /// Materialize one proven numeric-for region.  The marker pair already
    /// contains the VM's converted operands; unlike generic-for there is no
    /// hidden iterator state or exhaustion result to export.  We therefore
    /// reuse the typed exit handling while keeping the proof deliberately
    /// narrow (one init edge, one reducible body, and no post-prep suffix).
    fn build_numeric_loop(&mut self, info: &LoopInfo) -> Option<PathResult> {
        let numeric = info.numeric.as_ref()?;
        if !self.visited.insert(info.init) {
            return None;
        }
        let init_block = self.function.block(info.init)?;
        let init_index = init_block
            .iter()
            .position(|statement| statement.as_num_for_init().is_some())?;
        let mut output: Block = init_block
            .iter()
            .take(init_index)
            .cloned()
            .map(|statement| self.rewrite_statement(statement))
            .collect_vec()
            .into();
        let init_edges = self.function.edges(info.init).collect_vec();
        if init_edges.len() != 1
            || init_edges[0].target() != info.header
            || init_edges[0].weight().branch_type != BranchType::Unconditional
            || !init_edges[0].weight().arguments.is_empty()
        {
            return None;
        }
        // FORNPREP has no source-level slot between preparation and the first
        // iteration.  Even a total local-only statement can observe one of
        // the hidden limit/step destinations, or a value changed while the
        // bounds are evaluated.  Until numeric tuple staging carries exact
        // ordering/provenance, a non-trivia suffix is therefore unsafe.
        let numeric_suffix = init_block
            .iter()
            .skip(init_index + 1)
            .filter(|statement| !is_ignorable(statement))
            .cloned()
            .collect_vec();
        if !numeric_suffix.is_empty() {
            return self.reject_unsafe(UnsafeStructureReason::ForInitSuffixOrder);
        }
        if init_block
            .iter()
            .take(init_index)
            .any(|statement| !is_linear_statement(statement))
        {
            return None;
        }
        if self.rewrite.contains_key(&numeric.counter)
            || self.protected_locals.contains(&numeric.counter)
        {
            // A numeric-for counter is a fresh loop binding.  Reusing a
            // function parameter, upvalue, or an already-exported SSA cell
            // would alter lexical scope/capture identity.
            return None;
        }
        if self
            .analysis
            .ref_captured_locals_before_init(self.function, info.init)
            .contains(&numeric.counter)
        {
            // A `for` counter introduces a fresh lexical binding.  Reusing a
            // register whose pre-loop cell is already captured by reference
            // would make closures observe the shadowing loop variable instead
            // of the original cell after source-level lowering.
            return None;
        }
        let header = self.function.block(info.header)?;
        if header
            .iter()
            .take(header.len().saturating_sub(1))
            .any(|statement| !is_ignorable(statement))
            || header
                .last()
                .and_then(|statement| statement.as_num_for_next())
                .is_none()
        {
            return None;
        }
        if self
            .function
            .edges(info.header)
            .any(|edge| !edge.weight().arguments.is_empty())
        {
            // There is no transfer slot on a numeric loop's body/exhaustion
            // ports in the source syntax.  Preserve edge values via the
            // certified fallback instead of dropping them.
            return None;
        }
        // `build_path` stops at the numeric FORNLOOP header while constructing
        // the body.  Mark the marker block consumed here, mirroring the
        // generic-for builder, so the final reachability check cannot mistake
        // the protocol header for an unstructured residual node.
        if !self.visited.insert(info.header) {
            return None;
        }
        let init_statement = init_block
            .get(init_index)
            .and_then(|statement| statement.as_num_for_init())?;
        let counter = init_statement.counter.0.as_local()?;
        let limit_local = init_statement.limit.0.as_local()?.clone();
        let step_local = init_statement.step.0.as_local()?.clone();
        if counter != &numeric.counter
            || init_statement.counter.1 != numeric.initial
            || init_statement.limit.1 != numeric.limit
            || init_statement.step.1 != numeric.step
        {
            return None;
        }
        // FORNPREP copies limit/step into hidden numeric-loop registers.  A
        // source `for` evaluates those expressions once and keeps the hidden
        // copies independent from the body.  If the CFG body touches either
        // marker register directly, emitting a NumericFor would instead make
        // those reads/writes observe the source-level outer locals.  Keep the
        // candidate fail-closed until register aliasing is modelled explicitly.
        let hidden_operands = [limit_local.clone(), step_local.clone()];
        if info
            .nodes
            .iter()
            .filter(|node| **node != info.header)
            .any(|node| {
                self.function.block(*node).is_some_and(|block| {
                    block.iter().any(|statement| {
                        statement
                            .values_read()
                            .into_iter()
                            .chain(statement.values_written())
                            .any(|local| hidden_operands.iter().any(|hidden| *hidden == *local))
                            || statement_captures_any(statement, &hidden_operands)
                    })
                })
            })
        {
            return None;
        }
        output.extend(
            numeric_suffix
                .iter()
                .cloned()
                .map(|statement| self.rewrite_statement(statement)),
        );
        // A numeric FORNLOOP can target a short normal-exhaustion adapter
        // before the post-loop join.  NumericFor has no result export that
        // could absorb such a block, so only an entirely trivia adapter may
        // be skipped; any executable statement keeps the candidate on the
        // fail-closed path.
        let mut normal_adapters = Vec::new();
        let mut adapter_cursor = info.normal_exit;
        let mut adapter_seen = FxHashSet::default();
        while adapter_cursor != info.join {
            if !adapter_seen.insert(adapter_cursor) || info.nodes.contains(&adapter_cursor) {
                return None;
            }
            let block = self.function.block(adapter_cursor)?;
            if block.iter().any(|statement| !is_ignorable(statement)) {
                return None;
            }
            let edges = self.function.edges(adapter_cursor).collect_vec();
            if edges.len() != 1
                || edges[0].weight().branch_type != BranchType::Unconditional
                || !edges[0].weight().arguments.is_empty()
            {
                return None;
            }
            normal_adapters.push(adapter_cursor);
            adapter_cursor = edges[0].target();
        }
        // Iterator operands are evaluated before entering the body.  Capture
        // their incoming rewrite environment now; a nested loop may publish
        // an export mapping while the body is structured, but that mapping
        // must not retroactively rewrite the outer loop's bounds.
        let initial = self.rewrite_rvalue(numeric.initial.clone());
        let limit = self.rewrite_rvalue(numeric.limit.clone());
        let step = self.rewrite_rvalue(numeric.step.clone());
        let context = LoopContext {
            info,
            exports: &[],
            exhaustion_flag: None,
        };
        let body_result = match self.build_path(info.body_entry, Some(info.header), Some(&context))
        {
            Some(result) => result,
            None => return None,
        };
        if body_result.next != Some(info.header)
            && body_result.next != Some(info.join)
            && body_result.next.is_some()
        {
            return None;
        }
        let mut body = body_result.block;
        strip_trailing_continues(&mut body);
        output.push(
            ast::NumericFor::new(
                initial,
                limit,
                step,
                numeric.counter.clone(),
                body,
            )
            .into(),
        );
        self.visited.extend(normal_adapters);
        Some(PathResult {
            block: output,
            next: Some(info.join),
        })
    }

    fn build_while_loop(&mut self, info: &LoopInfo) -> Option<PathResult> {
        let condition = info.while_condition.clone()?;
        // The condition is evaluated at the loop header, before any nested
        // loop in the body can publish an export rewrite.  Capture its
        // incoming binding now; rewriting it after building the body would
        // let a nested generic-for result alias the header register and turn
        // `while tail do` into a test of an uninitialised post-loop export.
        let rewritten_condition = self.rewrite_rvalue(condition.clone());
        if !self.visited.insert(info.header) {
            return None;
        }
        let header = self.function.block(info.header)?;
        let if_statement = header.last()?.as_if()?;
        if if_statement.condition != condition
            || header
                .iter()
                .take(header.len().saturating_sub(1))
                // The conditional header executes on every natural backedge.
                // There is no source-level slot to place a non-trivial
                // prefix outside the test and inside the loop at the same
                // time, so accepting one here would move side effects to a
                // one-time preheader (e.g. `x += 1; while x < 3`).
                .any(|statement| !is_ignorable(statement))
            || self
                .function
                .edges(info.header)
                .any(|edge| !edge.weight().arguments.is_empty())
        {
            return None;
        }
        let mut output: Block = header
            .iter()
            .take(header.len().saturating_sub(1))
            .cloned()
            .map(|statement| self.rewrite_statement(statement))
            .collect_vec()
            .into();
        let context = LoopContext {
            info,
            exports: &[],
            exhaustion_flag: None,
        };
        let body_result = self.build_path(info.body_entry, Some(info.header), Some(&context))?;
        if body_result.next != Some(info.header) && body_result.next != Some(info.join) {
            return None;
        }
        let mut body = body_result.block;
        let mut loop_condition = rewritten_condition;
        // A nested generic-for may reuse the enclosing while condition's SSA
        // register for its result tuple.  In bytecode the FORGLOOP result is
        // visible on a break-to-header edge, but a source-level `for` binds
        // its result locals only inside the loop body.  Materialize the
        // carried value explicitly: keep a fresh `carry` cell for the while
        // test, copy it to a stable `current` cell before resetting it, and
        // feed the nested iterator from `current`.
        if let RValue::Local(condition_local) = &condition {
            let has_aliasing_generic = body.iter().any(|statement| {
                matches!(statement, Statement::GenericFor(for_loop)
                    if for_loop.res_locals.iter().any(|result| result == condition_local))
            });
            if has_aliasing_generic {
                let Some(carry) = self.rewrite.get(condition_local).cloned() else {
                    // Without an export mapping there is no distinct
                    // carry-cell to bridge the nested loop's result binding
                    // back to the enclosing while condition.  The formatter
                    // would otherwise shadow the shared SSA local and drop
                    // the loop-carried update (the Shenron tail-chain CFG).
                    return None;
                };
                if carry != *condition_local {
                    let current = RcLocal::default();
                    rewrite_while_carried_alias(&mut body, condition_local, &carry, &current);
                    body.0.insert(
                        0,
                        Assign::new(
                            vec![LValue::Local(current.clone())],
                            vec![RValue::Local(carry.clone())],
                        )
                        .into(),
                    );
                    output.push(
                        Assign::new(
                            vec![LValue::Local(carry.clone())],
                            vec![loop_condition.clone()],
                        )
                        .into(),
                    );
                    loop_condition = RValue::Local(carry);
                }
            }
        }
        strip_trailing_continues(&mut body);
        output.push(ast::While::new(loop_condition, body).into());
        Some(PathResult {
            block: output,
            next: Some(info.join),
        })
    }

    fn build_loop(
        &mut self,
        info: &LoopInfo,
        context: Option<&LoopContext<'_>>,
    ) -> Option<PathResult> {
        // A post-loop re-entry tail can only be lowered as a top-level
        // `while true` wrapper.  Inside an enclosing loop, returning `next =
        // None` would escape the nested path and alter the parent's control
        // flow, so keep the conservative generic-for lowering there.
        self.build_loop_inner_with_reentry(info, context.is_none())
    }

    /// Prove a post-loop tail that either terminates or re-enters this
    /// generic-for's preparation block.  Luau emits this shape when a source
    /// `while true` wraps a `for`: an iteration can break to the tail,
    /// exhaustion reaches the same tail, and the tail prepares the next
    /// iterator.  Every node/edge is checked before AST construction.
    fn reentry_tail(&self, info: &LoopInfo) -> Option<ReentryTail> {
        if info.numeric.is_some() || info.while_condition.is_some() {
            return None;
        }
        let mut nodes = FxHashSet::default();
        let mut work = vec![info.join];
        let mut reenters = false;
        let mut terminates = false;
        while let Some(current) = work.pop() {
            if current == info.init {
                reenters = true;
                continue;
            }
            if !self.analysis.reachable.contains(&current)
                || info.nodes.contains(&current)
                || !nodes.insert(current)
            {
                return None;
            }
            let block = self.function.block(current)?;
            // A re-entry tail that reads one of the generic-for result
            // registers needs the value that was live before the first
            // iteration (for example `tail = FindFirstChild(...); while
            // tail do ...`).  The generic-for builder deliberately exports
            // those registers into fresh locals initialized to `nil`; this
            // wrapper has no transfer slot to seed them from the preheader.
            // Reject the shape until that seed assignment is represented,
            // rather than silently turning a non-empty loop into `while nil`.
            if block.iter().any(|statement| {
                statement
                    .values_read()
                    .into_iter()
                    .any(|read| info.res_locals.iter().any(|local| local == read))
                    || statement_captures_any(statement, &info.res_locals)
            }) {
                return None;
            }
            if block.iter().any(|statement| {
                matches!(
                    statement,
                    Statement::GenericForInit(_)
                        | Statement::GenericForNext(_)
                        | Statement::NumForInit(_)
                        | Statement::NumForNext(_)
                )
            }) {
                return None;
            }
            let successors = self.function.successor_blocks(current).collect_vec();
            let edges = self.function.edges(current).collect_vec();
            match successors.as_slice() {
                [] => {
                    // A terminal tail without an explicit return is merely
                    // function fallthrough.  Wrapping it in `while true`
                    // would turn that fallthrough into an infinite loop.
                    // Require a real return and keep any preceding prefix
                    // linear so the wrapper owns exactly the proven tail.
                    if !matches!(block.last(), Some(Statement::Return(_)))
                        || block
                            .iter()
                            .take(block.len().saturating_sub(1))
                            .any(|statement| !is_linear_statement(statement))
                    {
                        return None;
                    }
                    terminates = true;
                }
                [target] => {
                    if edges.len() != 1
                        || edges[0].target() != *target
                        || edges[0].weight().branch_type != BranchType::Unconditional
                        || !edges[0].weight().arguments.is_empty()
                        || block
                            .iter()
                            .any(|statement| !is_linear_statement(statement))
                    {
                        return None;
                    }
                    work.push(*target);
                }
                [_, _] => {
                    let Some(if_statement) = block.last().and_then(|statement| statement.as_if())
                    else {
                        return None;
                    };
                    if !if_statement.then_block.lock().is_empty()
                        || !if_statement.else_block.lock().is_empty()
                        || block
                            .iter()
                            .take(block.len().saturating_sub(1))
                            .any(|statement| !is_linear_statement(statement))
                        || edges.len() != 2
                        || edges.iter().any(|edge| !edge.weight().arguments.is_empty())
                    {
                        return None;
                    }
                    let (then_edge, else_edge) = self.function.conditional_edges(current)?;
                    if then_edge.weight().branch_type != BranchType::Then
                        || else_edge.weight().branch_type != BranchType::Else
                    {
                        return None;
                    }
                    work.push(then_edge.target());
                    work.push(else_edge.target());
                }
                _ => return None,
            }
        }
        if !reenters || !terminates {
            return None;
        }
        // The join may be entered from the generic body and its exhaustion
        // adapter only.  Every later tail node must have all predecessors in
        // the tail; otherwise a sibling branch could execute it without
        // passing through this loop's enclosing iteration.
        for node in &nodes {
            if self
                .function
                .predecessor_blocks(*node)
                .filter(|predecessor| self.analysis.reachable.contains(predecessor))
                .any(|predecessor| {
                    if *node == info.join {
                        !info.nodes.contains(&predecessor)
                            && predecessor != info.normal_exit
                            && !self.is_body_exit_adapter(info, predecessor, *node)
                    } else {
                        !nodes.contains(&predecessor)
                    }
                })
            {
                return None;
            }
        }
        Some(ReentryTail { nodes })
    }

    /// Prove that a linear block immediately before the re-entry tail is the
    /// unique adapter for an explicit body-side break.  Such a block is not
    /// part of the loop's FORGLOOP body region, but it is still semantically
    /// owned by the loop and must be allowed as a predecessor of its join.
    fn is_body_exit_adapter(&self, info: &LoopInfo, node: NodeIndex, target: NodeIndex) -> bool {
        if info.nodes.contains(&node) || node == info.normal_exit {
            return false;
        }
        let Some(block) = self.function.block(node) else {
            return false;
        };
        if block.is_empty()
            || block.iter().any(|statement| {
                !is_linear_statement(statement)
                    || matches!(statement, Statement::Assign(assign) if assign.prefix)
            })
        {
            return false;
        }
        let edges = self.function.edges(node).collect_vec();
        if edges.len() != 1
            || edges[0].target() != target
            || edges[0].weight().branch_type != BranchType::Unconditional
            || !edges[0].weight().arguments.is_empty()
        {
            return false;
        }
        let predecessors = self
            .function
            .predecessor_blocks(node)
            .filter(|predecessor| self.analysis.reachable.contains(predecessor))
            .collect_vec();
        !predecessors.is_empty()
            && predecessors
                .iter()
                .all(|predecessor| info.nodes.contains(predecessor))
    }

    /// Build a previously proven re-entry tail.  Branch rewrites must agree
    /// exactly; a path-sensitive export would need a richer result than this
    /// wrapper can carry and remains unsupported.
    fn build_reentry_tail(
        &mut self,
        current: NodeIndex,
        init: NodeIndex,
        allowed: &FxHashSet<NodeIndex>,
        stack: &mut FxHashSet<NodeIndex>,
    ) -> Option<ReentryTailResult> {
        if current == init {
            return Some(ReentryTailResult {
                block: Block::default(),
                reenters: true,
                terminates: false,
            });
        }
        if !allowed.contains(&current) || !stack.insert(current) || !self.visited.insert(current) {
            return None;
        }
        let block = self.function.block(current)?;
        let successors = self.function.successor_blocks(current).collect_vec();
        let edges = self.function.edges(current).collect_vec();
        match successors.as_slice() {
            [] => {
                if !matches!(block.last(), Some(Statement::Return(_)))
                    || block
                        .iter()
                        .take(block.len().saturating_sub(1))
                        .any(|statement| !is_linear_statement(statement))
                {
                    return None;
                }
                Some(ReentryTailResult {
                    block: block
                        .iter()
                        .cloned()
                        .map(|statement| self.rewrite_statement(statement))
                        .collect_vec()
                        .into(),
                    reenters: false,
                    terminates: true,
                })
            }
            [target] => {
                if edges.len() != 1
                    || edges[0].target() != *target
                    || edges[0].weight().branch_type != BranchType::Unconditional
                    || !edges[0].weight().arguments.is_empty()
                {
                    return None;
                }
                let mut result = self.build_reentry_tail(*target, init, allowed, stack)?;
                let mut output: Block = block
                    .iter()
                    .cloned()
                    .map(|statement| self.rewrite_statement(statement))
                    .collect_vec()
                    .into();
                output.extend(result.block.0);
                result.block = output;
                Some(result)
            }
            [_, _] => {
                let if_statement = block.last()?.as_if()?.clone();
                if !if_statement.then_block.lock().is_empty()
                    || !if_statement.else_block.lock().is_empty()
                {
                    return None;
                }
                let (then_edge, else_edge) = self.function.conditional_edges(current)?;
                if then_edge.weight().branch_type != BranchType::Then
                    || else_edge.weight().branch_type != BranchType::Else
                    || !then_edge.weight().arguments.is_empty()
                    || !else_edge.weight().arguments.is_empty()
                {
                    return None;
                }
                let base_rewrite = self.rewrite.clone();
                let then_result =
                    self.build_reentry_tail(then_edge.target(), init, allowed, stack)?;
                let then_rewrite = self.rewrite.clone();
                self.rewrite = base_rewrite.clone();
                let else_result =
                    self.build_reentry_tail(else_edge.target(), init, allowed, stack)?;
                let else_rewrite = self.rewrite.clone();
                if then_rewrite != else_rewrite {
                    return None;
                }
                let mut output: Block = block
                    .iter()
                    .take(block.len().saturating_sub(1))
                    .cloned()
                    .map(|statement| self.rewrite_statement(statement))
                    .collect_vec()
                    .into();
                let mut condition = self.rewrite_rvalue(if_statement.condition.clone());
                let mut then_block = then_result.block;
                let mut else_block = else_result.block;
                simplify_conditional(&mut condition, &mut then_block, &mut else_block);
                output.push(If::new(condition, then_block, else_block).into());
                Some(ReentryTailResult {
                    block: output,
                    reenters: then_result.reenters || else_result.reenters,
                    terminates: then_result.terminates || else_result.terminates,
                })
            }
            _ => None,
        }
    }

    /// Move the exhaustion-only boolean write that the compiler places after
    /// a generic-for before the loop itself when lowering a re-entry wrapper.
    ///
    /// A FORGLOOP body can break directly to the shared join, bypassing the
    /// normal-exhaustion adapter.  Emitting that adapter's `flag = true`
    /// unconditionally after the source-level `for` would therefore erase a
    /// body-side `flag = false` and make the tail take the wrong branch.  The
    /// write is safe to move only for the narrow sentinel shape below: one
    /// trailing `local = true`, the tail's first condition reads that exact
    /// local, no pre-loop/body code reads or captures it, and body writes are
    /// exclusively `local = false`.  Any other adapter remains fail-closed.
    fn move_reentry_reset_before_for(&self, generic: &mut Block, tail: &Block) -> Option<()> {
        fn block_reads_or_captures(block: &Block, local: &RcLocal) -> bool {
            block.iter().any(|statement| {
                statement
                    .values_read()
                    .into_iter()
                    .any(|read| read == local)
                    || statement_captures_any(statement, std::slice::from_ref(local))
                    || match statement {
                        Statement::If(node) => {
                            block_reads_or_captures(&node.then_block.lock(), local)
                                || block_reads_or_captures(&node.else_block.lock(), local)
                        }
                        Statement::While(node) => {
                            block_reads_or_captures(&node.block.lock(), local)
                        }
                        Statement::Repeat(node) => {
                            block_reads_or_captures(&node.block.lock(), local)
                        }
                        Statement::NumericFor(node) => {
                            block_reads_or_captures(&node.block.lock(), local)
                        }
                        Statement::GenericFor(node) => {
                            block_reads_or_captures(&node.block.lock(), local)
                        }
                        _ => false,
                    }
            })
        }

        fn is_bool_assignment(statement: &Statement, local: &RcLocal, value: bool) -> bool {
            matches!(
                statement,
                Statement::Assign(assign)
                    if assign.left.len() == 1
                        && assign.right.len() == 1
                        && !assign.prefix
                        && !assign.parallel
                        && assign.left[0].as_local() == Some(local)
                        && matches!(assign.right[0], RValue::Literal(Literal::Boolean(v)) if v == value)
            )
        }

        fn block_has_bad_writes(block: &Block, local: &RcLocal) -> bool {
            block.iter().any(|statement| {
                (statement
                    .values_written()
                    .into_iter()
                    .any(|written| written == local)
                    && !is_bool_assignment(statement, local, false))
                    || match statement {
                        Statement::If(node) => {
                            block_has_bad_writes(&node.then_block.lock(), local)
                                || block_has_bad_writes(&node.else_block.lock(), local)
                        }
                        Statement::While(node) => block_has_bad_writes(&node.block.lock(), local),
                        Statement::Repeat(node) => block_has_bad_writes(&node.block.lock(), local),
                        Statement::NumericFor(node) => {
                            block_has_bad_writes(&node.block.lock(), local)
                        }
                        Statement::GenericFor(node) => {
                            block_has_bad_writes(&node.block.lock(), local)
                        }
                        _ => false,
                    }
            })
        }

        fn block_writes_local(block: &Block, local: &RcLocal) -> bool {
            block.iter().any(|statement| {
                statement
                    .values_written()
                    .into_iter()
                    .any(|written| written == local)
                    || match statement {
                        Statement::If(node) => {
                            block_writes_local(&node.then_block.lock(), local)
                                || block_writes_local(&node.else_block.lock(), local)
                        }
                        Statement::While(node) => block_writes_local(&node.block.lock(), local),
                        Statement::Repeat(node) => block_writes_local(&node.block.lock(), local),
                        Statement::NumericFor(node) => {
                            block_writes_local(&node.block.lock(), local)
                        }
                        Statement::GenericFor(node) => {
                            block_writes_local(&node.block.lock(), local)
                        }
                        _ => false,
                    }
            })
        }

        fn block_has_unanchored_false_write(
            block: &Block,
            local: &RcLocal,
            allowed_intervening: Option<&RcLocal>,
        ) -> bool {
            block.0.iter().enumerate().any(|(index, statement)| {
                let false_write = is_bool_assignment(statement, local, false);
                let anchored = false_write && {
                    let mut suffix = block.0.get(index + 1..).unwrap_or_default().iter();
                    loop {
                        let Some(next) = suffix.find(|next| !is_ignorable(next)) else {
                            break false;
                        };
                        if matches!(next, Statement::Break(_)) {
                            break true;
                        }
                        if allowed_intervening
                            .is_some_and(|allowed| is_bool_assignment(next, allowed, false))
                        {
                            continue;
                        }
                        break false;
                    }
                };
                (false_write && !anchored)
                    || match statement {
                        Statement::If(node) => {
                            block_has_unanchored_false_write(
                                &node.then_block.lock(),
                                local,
                                allowed_intervening,
                            ) || block_has_unanchored_false_write(
                                &node.else_block.lock(),
                                local,
                                allowed_intervening,
                            )
                        }
                        Statement::While(node) => block_writes_local(&node.block.lock(), local),
                        Statement::Repeat(node) => block_writes_local(&node.block.lock(), local),
                        Statement::NumericFor(node) => {
                            block_writes_local(&node.block.lock(), local)
                        }
                        Statement::GenericFor(node) => {
                            block_writes_local(&node.block.lock(), local)
                        }
                        _ => false,
                    }
            })
        }

        /// Remove the compiler's private exhaustion sentinel write when it is
        /// immediately before a break.  In the re-entry shape the source
        /// value write (`result = false`) is followed by this private write
        /// (`exhausted = false`) and only then by `break`; accepting the
        /// sentinel as part of the proof lets us move the real result reset
        /// before the `for` without changing the break path.
        fn remove_guard_false_before_break(block: &mut Block, guard: &RcLocal) -> usize {
            let mut removed = 0;
            let mut index = 0;
            while index < block.0.len() {
                match &mut block.0[index] {
                    Statement::If(node) => {
                        removed += remove_guard_false_before_break(
                            &mut node.then_block.lock(),
                            guard,
                        );
                        removed += remove_guard_false_before_break(
                            &mut node.else_block.lock(),
                            guard,
                        );
                        index += 1;
                    }
                    statement if is_bool_assignment(statement, guard, false) => {
                        let anchored = block
                            .0
                            .get(index + 1..)
                            .unwrap_or_default()
                            .iter()
                            .find(|next| !is_ignorable(next))
                            .is_some_and(|next| matches!(next, Statement::Break(_)));
                        if anchored {
                            block.0.remove(index);
                            removed += 1;
                        } else {
                            index += 1;
                        }
                    }
                    _ => index += 1,
                }
            }
            removed
        }

        fn count_bool_assignments(block: &Block, local: &RcLocal, value: bool) -> usize {
            block.iter().fold(0, |count, statement| {
                count
                    + usize::from(is_bool_assignment(statement, local, value))
                    + match statement {
                        Statement::If(node) => {
                            count_bool_assignments(&node.then_block.lock(), local, value)
                                + count_bool_assignments(&node.else_block.lock(), local, value)
                        }
                        // A private exhaustion sentinel captured by a nested
                        // loop is not owned by this generic-for; reject such
                        // a shape instead of moving its write across scope.
                        Statement::While(node) => {
                            count_bool_assignments(&node.block.lock(), local, value)
                        }
                        Statement::Repeat(node) => {
                            count_bool_assignments(&node.block.lock(), local, value)
                        }
                        Statement::NumericFor(node) => {
                            count_bool_assignments(&node.block.lock(), local, value)
                        }
                        Statement::GenericFor(node) => {
                            count_bool_assignments(&node.block.lock(), local, value)
                        }
                        _ => 0,
                    }
            })
        }

        fn count_local_writes(block: &Block, local: &RcLocal) -> usize {
            block.iter().fold(0, |count, statement| {
                count
                    + usize::from(
                        statement
                            .values_written()
                            .into_iter()
                            .any(|written| written == local),
                    )
                    + match statement {
                        Statement::If(node) => {
                            count_local_writes(&node.then_block.lock(), local)
                                + count_local_writes(&node.else_block.lock(), local)
                        }
                        Statement::While(node) => count_local_writes(&node.block.lock(), local),
                        Statement::Repeat(node) => count_local_writes(&node.block.lock(), local),
                        Statement::NumericFor(node) => {
                            count_local_writes(&node.block.lock(), local)
                        }
                        Statement::GenericFor(node) => {
                            count_local_writes(&node.block.lock(), local)
                        }
                        _ => 0,
                    }
            })
        }

        let mut for_index = generic
            .0
            .iter()
            .position(|statement| matches!(statement, Statement::GenericFor(_)))?;
        // The generic builder protects normal-exhaustion adapters with its
        // own `exhausted` flag.  A re-entry sentinel is the one narrow case
        // where that adapter is intentionally moved before the loop; unwrap
        // the guard after proving it contains exactly the sentinel write.
        // CutsceneUtil has one extra compiler write to that private flag on
        // the body-side break (`result=false; exhausted=false; break`).  Keep
        // the distinction between the semantic result local and that private
        // guard so only the proven sentinel writes are removed below.
        let mut wrapped_guard: Option<RcLocal> = None;
        if generic.0.len() == for_index + 2 {
            let guarded_reset = match generic.0.get(for_index + 1) {
                Some(Statement::If(node))
                    if node.else_block.lock().is_empty()
                        && node.then_block.lock().len() == 1
                        && matches!(node.condition, RValue::Local(_)) =>
                {
                    let then_block = node.then_block.lock();
                    then_block.0.first().cloned().and_then(|statement| {
                        let guard = match &node.condition {
                            RValue::Local(local) => local,
                            _ => unreachable!(),
                        };
                        if is_bool_assignment(&statement, guard, true) {
                            None
                        } else if matches!(statement, Statement::Assign(_)) {
                            // A guarded result reset is valid only when the
                            // branch consists of one literal-true local
                            // assignment.  Return it below after recording
                            // the separate exhausted flag.
                            let valid = matches!(
                                &statement,
                                Statement::Assign(assign)
                                    if assign.left.len() == 1
                                        && assign.right.len() == 1
                                        && !assign.prefix
                                        && !assign.parallel
                                        && assign.left[0].as_local().is_some()
                                        && matches!(assign.right[0], RValue::Literal(Literal::Boolean(true)))
                            );
                            if valid {
                                if let Statement::Assign(assign) = &statement {
                                    if assign.left[0].as_local() != Some(guard) {
                                        wrapped_guard = Some(guard.clone());
                                    }
                                }
                                Some(statement)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                }
                _ => None,
            };
            if let Some(reset) = guarded_reset {
                generic.0.pop();
                generic.0.push(reset);
            }
        }
        // The adapter must consist of exactly one sentinel write.  Moving a
        // call or any additional assignment would change observable order on
        // body-side breaks.
        if generic.0.len() != for_index + 2 {
            return None;
        }
        let reset = generic.0.get(for_index + 1)?.clone();
        let Statement::Assign(assign) = &reset else {
            return None;
        };
        if assign.left.len() != 1
            || assign.right.len() != 1
            || assign.prefix
            || assign.parallel
            || assign.left[0].as_local().is_none()
            || !matches!(assign.right[0], RValue::Literal(Literal::Boolean(true)))
        {
            return None;
        }
        let local = assign.left[0].as_local()?.clone();
        let tail_local = match tail.0.first()? {
            Statement::If(node) => match &node.condition {
                RValue::Local(condition_local) => condition_local,
                _ => return None,
            },
            _ => return None,
        };
        if *tail_local != local {
            return None;
        }
        let prefix = Block::from(generic.0[..for_index].to_vec());
        if block_reads_or_captures(&prefix, &local)
            || block_writes_local(&prefix, &local)
            || block_writes_local(tail, &local)
            || statement_captures_any(&reset, std::slice::from_ref(&local))
        {
            return None;
        }
        if let Some(guard) = wrapped_guard.clone() {
            // The private guard must be initialized exactly once before the
            // loop and must never be read/captured by the iterator or body.
            // Its only body write is the false assignment immediately before
            // this loop's break; otherwise moving it would alter a visible
            // cell or a nested-loop protocol.
            let guard_shape_valid = {
                let Statement::GenericFor(for_loop) = &generic.0[for_index] else {
                    return None;
                };
                count_local_writes(&prefix, &guard) == 1
                    && count_bool_assignments(&prefix, &guard, true) == 1
                    && count_bool_assignments(&prefix, &guard, false) == 0
                    && !block_reads_or_captures(&prefix, &guard)
                    && !for_loop
                        .right
                        .iter()
                        .any(|value| value.values_read().into_iter().any(|read| read == &guard))
                    && !for_loop
                        .right
                        .iter()
                        .any(|value| rvalue_captures_any(value, std::slice::from_ref(&guard)))
                    && !block_reads_or_captures(&for_loop.block.lock(), &guard)
                    && count_bool_assignments(&for_loop.block.lock(), &guard, false) == 1
                    && count_bool_assignments(&for_loop.block.lock(), &guard, true) == 0
                    && !block_has_bad_writes(&for_loop.block.lock(), &guard)
                    && !block_has_unanchored_false_write(&for_loop.block.lock(), &guard, None)
            };
            if !guard_shape_valid {
                return None;
            }
            // Drop the guard's pre-loop initialization and body-side write;
            // both are compiler protocol, not source-visible state.  The
            // suffix guarded reset was already normalized to `local=true`.
            let init_index = generic
                .0
                .iter()
                .position(|statement| is_bool_assignment(statement, &guard, true))?;
            generic.0.remove(init_index);
            for_index -= usize::from(init_index < for_index);
            let Statement::GenericFor(for_loop) = &mut generic.0[for_index] else {
                return None;
            };
            if remove_guard_false_before_break(&mut for_loop.block.lock(), &guard) != 1 {
                return None;
            }
            // `for_loop` was only borrowed to strip the guard.  Continue with
            // the existing semantic-result checks below using the adjusted
            // index and block.
        }
        // The iterator RHS and body must not observe the reset value.  The
        // body may only clear it to false on a break path.
        let Statement::GenericFor(for_loop) = &generic.0[for_index] else {
            return None;
        };
        if for_loop
            .right
            .iter()
            .any(|value| value.values_read().into_iter().any(|read| read == &local))
            || for_loop
                .right
                .iter()
                .any(|value| rvalue_captures_any(value, std::slice::from_ref(&local)))
            || block_reads_or_captures(&for_loop.block.lock(), &local)
            || block_has_bad_writes(&for_loop.block.lock(), &local)
            || block_has_unanchored_false_write(
                &for_loop.block.lock(),
                &local,
                wrapped_guard.as_ref(),
            )
        {
            return None;
        }
        let reset = generic.0.remove(for_index + 1);
        generic.0.insert(for_index, reset);
        Some(())
    }

    fn build_reentry_loop(&mut self, info: &LoopInfo, tail: ReentryTail) -> Option<PathResult> {
        let generic = match self.build_loop_inner_with_reentry(info, false) {
            Some(generic) => generic,
            None => return None,
        };
        if generic.next != Some(info.join) {
            return None;
        }
        let mut stack = FxHashSet::default();
        let tail_result = match self.build_reentry_tail(info.join, info.init, &tail.nodes, &mut stack) {
            Some(result) => result,
            None => return None,
        };
        if !tail_result.reenters || !tail_result.terminates {
            return None;
        }
        let mut generic = generic.block;
        if self.move_reentry_reset_before_for(&mut generic, &tail_result.block).is_none() {
            return None;
        }
        let mut body = generic;
        body.extend(tail_result.block.0);
        Some(PathResult {
            block: Block::from(vec![
                ast::While::new(RValue::Literal(Literal::Boolean(true)), body).into(),
            ]),
            next: None,
        })
    }

    fn build_loop_inner(&mut self, info: &LoopInfo) -> Option<PathResult> {
        self.build_loop_inner_with_reentry(info, true)
    }

    fn build_loop_inner_with_reentry(
        &mut self,
        info: &LoopInfo,
        allow_reentry: bool,
    ) -> Option<PathResult> {
        if allow_reentry {
            let tail = self.reentry_tail(info);
            if let Some(tail) = tail {
                return self.build_reentry_loop(info, tail);
            }
        }
        if info.while_condition.is_some() {
            return self.build_while_loop(info);
        }
        if info.numeric.is_some() {
            return self.build_numeric_loop(info);
        }
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
        let right_captures = info
            .right
            .iter()
            .fold(FxHashSet::default(), |mut captures, value| {
                // Only reference captures retain a mutable cell identity.
                // `Upvalue::Copy` snapshots the value at closure construction,
                // so rebinding that local in the init suffix cannot be
                // observed through the iterator RHS.
                collect_rvalue_ref_captures(value, &mut captures);
                captures
            });
        let pre_init_captures = self
            .analysis
            .ref_captured_locals_before_init(self.function, info.init);
        // A total/pure suffix expression may read an ordinary local when the
        // iterator RHS cannot observe or mutate that local.  The previous
        // read-free restriction rejected common optimizer output such as
        // `local count = flag and 2 or 1`, even though the value-flow proof
        // below establishes that `flag` is independent of the iterator tuple.
        // Captured locals remain fail-closed because an indirect callable RHS
        // has no effect summary precise enough to prove their stability.
        if init_suffix.iter().any(|statement| {
            statement
                .values_read()
                .into_iter()
                .any(|read| pre_init_captures.contains(read) || right_captures.contains(read))
        }) {
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
        let adapters = match self.normal_adapter_nodes(info, &exports) {
            Some(adapters) => adapters,
            None => return None,
        };
        if self.has_ref_captured_result(info)
            && !self.captured_result_is_loop_owned(info, &exports)
        {
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
                Assign::new(
                    vec![LValue::Local(export.clone())],
                    vec![RValue::Literal(Literal::Nil)],
                )
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
                protocol_locals
                    .iter()
                    .any(|protocol| protocol == destination)
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
            || init_block
                .iter()
                .any(|statement| statement_captures_any(statement, &protocol_locals))
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
                        && block.iter().skip(init_index + 1).any(|statement| {
                            statement
                                .values_written()
                                .into_iter()
                                .any(|local| info.res_locals.iter().any(|result| result == local))
                        });
                    let mut captures = FxHashSet::default();
                    collect_statement_captures(statement, &mut captures);
                    let captures_result = captures
                        .iter()
                        .any(|captured| info.res_locals.iter().any(|result| result == captured));
                    writes_result || captures_result
                })
            }) || (node != &info.init
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
        // An exhaustion adapter is a post-loop CFG path.  Source-level
        // `break` exits the loop before that path, so retain a small,
        // source-readable sentinel to guard the adapter when the body has an
        // explicit break.  The flag is local to this lowered loop and is
        // never used as a dispatcher/state-machine program counter.
        let exhaustion_flag = (!adapters.is_empty()).then(RcLocal::default);
        if let Some(flag) = &exhaustion_flag {
            output.push(
                Assign::new(
                    vec![LValue::Local(flag.clone())],
                    vec![RValue::Literal(Literal::Boolean(true))],
                )
                .into(),
            );
        }
        let context = LoopContext {
            info,
            exports: &exports,
            exhaustion_flag: exhaustion_flag.clone(),
        };
        // The iterator RHS is evaluated before the loop body.  Capture its
        // rewrite environment now; nested loops in the body may introduce
        // exports for locals that happen to share an SSA identity, but those
        // exports must not retroactively rewrite the outer iterator setup.
        let mut right: Vec<RValue> = info
            .right
            .iter()
            .cloned()
            .map(|value| self.rewrite_rvalue(value))
            .collect();
        // Generalized iteration (`for k, v in table do`) is lowered as a
        // three-value iterator tuple with two synthetic nil operands.  Those
        // operands are VM protocol details, not source expressions; retaining
        // them would print `in table, nil, nil`, which is legal-looking but
        // not source-like and invokes the wrong iterator protocol at runtime.
        if matches!(
            info.origin.map(|origin| origin.prep_kind),
            Some(ast::ForPrepKind::Generic)
        ) && right.len() == 3
            && right[1..]
                .iter()
                .all(|value| matches!(value, RValue::Literal(Literal::Nil)))
            && !info
                .origin
                .is_some_and(|origin| origin.explicit_nil_args)
            && !matches!(right.first(), Some(RValue::Call(_)) | Some(RValue::VarArg(_)))
        {
            right.truncate(1);
        }
        let body_result = if info.body_entry == info.normal_exit {
            // The compiler's unconditional-break shape aliases the body and
            // follow targets.  Do not walk the follow block into the loop;
            // materialise the source-level break and let the outer path visit
            // that block after the loop.
            PathResult {
                block: {
                    let mut block = Block::default();
                    if let Some(flag) = &exhaustion_flag {
                        block.push(
                            Assign::new(
                                vec![LValue::Local(flag.clone())],
                                vec![RValue::Literal(Literal::Boolean(false))],
                            )
                            .into(),
                        );
                    }
                    block.push(Statement::Break(ast::Break {}).into());
                    block
                },
                next: Some(info.join),
            }
        } else {
            match self.build_path(info.body_entry, Some(info.header), Some(&context)) {
                Some(result) => result,
                None => return None,
            }
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
        // A direct normal-exit block is also the join when the FORGLOOP
        // exhaustion edge has no separate adapter node.  If a body-side
        // `break` can reach that same block, any result-register write there
        // is path-sensitive: the exhaustion path may need a nil/phi value,
        // while the break path must retain the value selected in the body.
        // There is no source-level slot to condition that write in this
        // direct shape, so reject it rather than emitting an unconditional
        // copy that erases the break result (the Forge/GameModifiers/
        // Disassembly nil-phi CFGs).
        let direct_normal_exit = adapters.is_empty() && info.normal_exit == info.join;
        if direct_normal_exit
            && block_has_owned_break(&body_result.block)
            && self.function.block(info.normal_exit).is_some_and(|block| {
                block.iter().any(|statement| {
                    info.res_locals.iter().any(|local| {
                        statement.values_written().into_iter().any(|written| {
                            written == local && !Self::is_nil_assignment(statement, local)
                        })
                    })
                })
            })
        {
            return None;
        }
        let mut body = body_result.block;
        strip_trailing_continues(&mut body);
        let mut generic_for = GenericFor::new(info.res_locals.clone(), right, body);
        generic_for.origin = info.origin;
        output.push(generic_for.into());
        // Exhaustion adapters run after the VM FORGLOOP marker and may copy a
        // live result register (for example, the lower interpolation
        // candidate) into the value consumed by the post-loop join.  Apply
        // the fresh export mapping while materialising those adapter writes;
        // otherwise the copy lands in the hidden loop binding and the
        // source-like join still observes the export's initial nil.
        let mut adapter_rewrite = self.rewrite.clone();
        for (local, export) in &exports {
            adapter_rewrite.insert(local.clone(), export.clone());
        }
        let mut adapter_output = Block::default();
        for node in adapters {
            // Preserve source-visible linear statements on an exhaustion
            // adapter (for example, clearing a sentinel used by an enclosing
            // while condition).  Nil stores to hidden result cells are
            // represented by the source-level iterator semantics and remain
            // implicit.
            if let Some(block) = self.function.block(node) {
                adapter_output.extend(
                    block
                        .iter()
                        .filter(|statement| {
                            !info
                                .res_locals
                                .iter()
                                .any(|local| Self::is_nil_assignment(statement, local))
                        })
                        .cloned()
                        .map(|statement| {
                            let mut statement = statement;
                            for local in statement.values_read_mut() {
                                if let Some(replacement) = adapter_rewrite.get(local) {
                                    *local = replacement.clone();
                                }
                            }
                            for local in statement.values_written_mut() {
                                if let Some(replacement) = adapter_rewrite.get(local) {
                                    *local = replacement.clone();
                                }
                            }
                            statement
                        }),
                );
            }
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
        if !adapter_output.0.is_empty() {
            if let Some(flag) = exhaustion_flag {
                output.push(If::new(RValue::Local(flag), adapter_output, Block::default()).into());
            } else {
                output.extend(adapter_output.0);
            }
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
        self.build_path_inner(start, stop, context)
    }

    fn build_path_inner(
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
            if self.analysis.loops_by_header.contains_key(&current)
                || self.analysis.numeric_loops_by_header.contains_key(&current)
            {
                return None;
            }
            if !self.visited.insert(current) {
                return None;
            }
            if let Some(info) = self
                .analysis
                .while_loops_by_header
                .get(&current)
                .or_else(|| self.analysis.loops_by_init.get(&current))
                .or_else(|| self.analysis.numeric_loops_by_init.get(&current))
                .cloned()
            {
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
                let nested = match self.build_loop(&info, context) {
                    Some(nested) => nested,
                    None => return None,
                };
                output.extend(nested.block.0);
                let Some(next) = nested.next else {
                    // A terminal structured loop (currently the proven
                    // generic-for re-entry wrapper) owns the remainder of
                    // this path.  Preserve its terminal result instead of
                    // rejecting it via `?` on `next`.
                    return Some(PathResult {
                        block: output,
                        next: None,
                    });
                };
                current = next;
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
                        if !ctx.info.nodes.contains(target)
                            && self.external_region_reaches_header(*target, ctx)
                        {
                            // Proven continuation region whose PC envelope
                            // was widened away by an optimizer-generated
                            // shared tail.  It has no path to the loop join,
                            // so it is an ordinary source-level fallthrough
                            // to the next iteration rather than a break.
                            current = *target;
                            continue;
                        }
                        if !ctx.info.nodes.contains(target) {
                            // This edge originates in the loop body, not in
                            // the FORGLOOP header. Even when it happens to
                            // target the header's normal-exhaustion adapter,
                            // that adapter is part of the body break path and
                            // must execute before the break.
                            let adapter = self.build_exit_adapter(
                                *target,
                                ctx.info.join,
                                ctx,
                                Some(current),
                            )?;
                            output.extend(adapter.block.0);
                            self.append_export(&mut output, ctx.exports);
                            if let Some(flag) = &ctx.exhaustion_flag {
                                // A one-successor break path bypasses
                                // `build_transfer_arm_inner`, so mark it
                                // explicitly before leaving the loop.  Without
                                // this write the normal-exhaustion adapter is
                                // emitted unconditionally and can overwrite a
                                // body-selected result (the Lerp/GameModifiers
                                // phi shapes).
                                output.push(
                                    Assign::new(
                                        vec![LValue::Local(flag.clone())],
                                        vec![RValue::Literal(Literal::Boolean(false))],
                                    )
                                    .into(),
                                );
                            }
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
                        stop,
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

    /// Prove that every path from an owner-external target returns to the
    /// enclosing loop header before reaching its join or a terminal.  This
    /// is deliberately a small acyclic proof: cycles are rejected unless the
    /// cycle closes at the enclosing header, and nested loop headers are
    /// handled by their own typed region before this predicate is consulted.
    fn external_region_reaches_header(&self, start: NodeIndex, context: &LoopContext<'_>) -> bool {
        fn visit(
            builder: &Builder<'_>,
            node: NodeIndex,
            context: &LoopContext<'_>,
            active: &mut FxHashSet<NodeIndex>,
            memo: &mut FxHashMap<NodeIndex, bool>,
        ) -> bool {
            if node == context.info.header {
                return true;
            }
            if node == context.info.join || !builder.analysis.reachable.contains(&node) {
                return false;
            }
            if let Some(result) = memo.get(&node) {
                return *result;
            }
            if !active.insert(node) {
                return false;
            }
            let successors = builder.function.successor_blocks(node).collect_vec();
            let result = !successors.is_empty()
                && successors
                    .iter()
                    .all(|target| visit(builder, *target, context, active, memo));
            active.remove(&node);
            memo.insert(node, result);
            result
        }

        visit(
            self,
            start,
            context,
            &mut FxHashSet::default(),
            &mut FxHashMap::default(),
        )
    }

    fn build_exit_adapter(
        &mut self,
        start: NodeIndex,
        join: NodeIndex,
        context: &LoopContext<'_>,
        source: Option<NodeIndex>,
    ) -> Option<PathResult> {
        let mut output = Block::default();
        let mut current = start;
        let mut previous = source;
        let mut path_nodes = Vec::new();
        while current != join {
            if context.info.nodes.contains(&current) || self.visited.contains(&current) {
                return None;
            }
            let predecessors = self
                .function
                .predecessor_blocks(current)
                .filter(|predecessor| self.analysis.reachable.contains(predecessor))
                .collect_vec();
            let unique_entry = previous
                .is_some_and(|expected| predecessors.len() == 1 && predecessors[0] == expected);
            let block = self.function.block(current)?;
            if block_has_rewritten_closure(block, &self.rewrite) {
                return None;
            }
            let trivia_or_nil = block.iter().all(|statement| {
                is_ignorable(statement)
                    || context
                        .info
                        .res_locals
                        .iter()
                        .any(|local| Self::is_nil_assignment(statement, local))
            });
            let linear_tail = unique_entry
                && block.iter().all(is_linear_statement)
                // A declaration in an adapter would be scoped to the
                // enclosing loop body when cloned behind `break`, while the
                // original CFG exposes it at the post-loop join.  Keep the
                // adapter fail-closed rather than producing an out-of-scope
                // or accidentally-global read after the loop.
                && !block.iter().any(|statement| {
                    matches!(statement, Statement::Assign(assign) if assign.prefix)
                })
                && !block.iter().any(|statement| {
                    matches!(
                        statement,
                        Statement::GenericForInit(_)
                            | Statement::GenericForNext(_)
                            | Statement::NumForInit(_)
                            | Statement::NumForNext(_)
                    )
                });
            if !trivia_or_nil && !linear_tail {
                return None;
            }
            let successors = self.function.successor_blocks(current).collect_vec();
            let edges = self.function.edges(current).collect_vec();
            if successors.len() != 1
                || edges.len() != 1
                || edges[0].target() != successors[0]
                || edges[0].weight().branch_type != BranchType::Unconditional
                || !edges[0].weight().arguments.is_empty()
            {
                return None;
            }
            path_nodes.push(current);
            output.extend(
                block
                    .iter()
                    .cloned()
                    .map(|statement| self.rewrite_statement(statement)),
            );
            output.extend(self.edge_transfer(edges[0].weight(), &self.rewrite)?.0);
            previous = Some(current);
            current = successors[0];
        }
        self.visited.extend(path_nodes);
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
        stop: Option<NodeIndex>,
    ) -> Option<PathResult> {
        let result = self.build_conditional_inner(
            source,
            statement,
            then_target,
            else_target,
            context,
            stop,
        );
        result
    }

    /// `stop` is the node at which the enclosing path walk ends.  When one arm
    /// targets it directly, the conditional is a guard (`if c then ... end`)
    /// followed by the stop node rather than a diamond.
    fn build_conditional_inner(
        &mut self,
        source: NodeIndex,
        statement: If,
        then_target: NodeIndex,
        else_target: NodeIndex,
        context: Option<&LoopContext<'_>>,
        stop: Option<NodeIndex>,
    ) -> Option<PathResult> {
        if let Some(ctx) = context {
            // A common compiler diamond can expose a shared tail through one
            // arm of a nested conditional:
            //
            //     if outer then if inner then T else C end else T end
            //
            // The post-dominator of the outer arms is the enclosing loop
            // header (not `T`), so the ordinary path walk would consume `T`
            // once and then reject the second entry as already visited.  The
            // boolean equivalent `if outer and not inner then C else T end`
            // evaluates the same conditions (with short-circuiting) and lets
            // us consume each CFG region exactly once.  Apply this only when
            // the nested node is a pure conditional terminator and all four
            // edges carry no phi transfers; edge-sensitive variants remain
            // on the certified fallback path.
            if let Some((inner, common_target, alternate_target, inner_then_is_common)) =
                self.shared_tail_shape(then_target, else_target, ctx)
            {
                return self.build_shared_tail_conditional(
                    source,
                    statement,
                    then_target,
                    common_target,
                    inner,
                    alternate_target,
                    inner_then_is_common,
                    ctx,
                );
            }
            let inside_join =
                common_postdominator(&[then_target, else_target], &self.analysis.post_dominators)
                    .filter(|join| *join != ctx.info.header && ctx.info.nodes.contains(join));
            if let Some(join) = inside_join {
                return self.build_inside_join_conditional(
                    source,
                    &statement,
                    then_target,
                    else_target,
                    join,
                    false,
                    ctx,
                );
            }
            // No post-dominator join exists when an arm terminates (`return`,
            // `break`, `continue`).  If the arms still share a tail inside the
            // loop, structure it once after the `if` instead of once per arm.
            // Candidate joins, earliest first: the shared node both arms flow
            // into, then the enclosing walk's own stop (an arm that does not
            // reach it must terminate).  Each attempt is rolled back on
            // failure.
            let stop_join = stop.filter(|join| {
                self.allow_shared_tail
                    && *join != ctx.info.header
                    && ctx.info.nodes.contains(join)
                    && !self.block_is_small_terminal(*join)
            });
            let candidates = [
                self.shared_tail_join(then_target, else_target, Some(ctx), stop),
                stop_join,
            ];
            let mut tried = None;
            for join in candidates.into_iter().flatten() {
                if tried == Some(join) {
                    continue;
                }
                tried = Some(join);
                let base_rewrite = self.rewrite.clone();
                let base_visited = self.visited.clone();
                if let Some(result) = self.build_inside_join_conditional(
                    source,
                    &statement,
                    then_target,
                    else_target,
                    join,
                    true,
                    ctx,
                )
                    // The continuation must still be able to start at the
                    // join: an arm may have consumed it through a nested
                    // region (for example a loop exit adapter).
                    && !self.visited.contains(&join)
                {
                    if std::env::var_os("MEDAL_DEBUG_RESTRUCTURE").is_some() {
                        eprintln!(
                            "shared tail (loop): source={} join={}",
                            source.index(),
                            join.index()
                        );
                    }
                    return Some(result);
                }
                if std::env::var_os("MEDAL_DEBUG_RESTRUCTURE").is_some() {
                    eprintln!(
                        "shared tail (loop) FAILED: source={} join={}",
                        source.index(),
                        join.index()
                    );
                }
                self.rewrite = base_rewrite;
                self.visited = base_visited;
            }
            let base_rewrite = self.rewrite.clone();
            let base_visited = self.visited.clone();
            let then_edge = self
                .function
                .edges(source)
                .find(|edge| {
                    edge.target() == then_target && edge.weight().branch_type == BranchType::Then
                })?
                .weight()
                .clone();
            let then_transfer = self.edge_transfer(&then_edge, &base_rewrite)?;
            let then_result = self.build_transfer_arm(source, then_target, ctx)?;
            let mut then_rewrite = self.rewrite.clone();
            self.rewrite = base_rewrite.clone();
            let then_visited = self.visited.clone();
            self.visited = base_visited.clone();
            let else_edge = self
                .function
                .edges(source)
                .find(|edge| {
                    edge.target() == else_target && edge.weight().branch_type == BranchType::Else
                })?
                .weight()
                .clone();
            let else_transfer = self.edge_transfer(&else_edge, &base_rewrite)?;
            let else_result = self.build_transfer_arm(source, else_target, ctx)?;
            let mut else_rewrite = self.rewrite.clone();
            self.visited.extend(then_visited);
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
            if join.is_none() {
                // A returning arm has no common post-dominator with its
                // sibling.  When both arms still flow into one tail (or one
                // arm flows into the enclosing walk's stop while the other
                // terminates), build the arms up to that node and emit the
                // tail once after the `if`.  Each attempt is rolled back on
                // failure.
                // A one-statement terminal stop (`return x`) is not used as a
                // guard target: duplicating it keeps every inlined copy of a
                // body in the same shape for the de-inline pass.
                let stop_join = stop
                    .filter(|join| self.allow_shared_tail && !self.block_is_small_terminal(*join));
                let candidates = [
                    self.shared_tail_join(then_target, else_target, None, stop),
                    stop_join,
                ];
                let mut tried = None;
                for shared in candidates.into_iter().flatten() {
                    if tried == Some(shared) {
                        continue;
                    }
                    tried = Some(shared);
                    let base_rewrite = self.rewrite.clone();
                    let base_visited = self.visited.clone();
                    if let Some(result) = self.build_plain_conditional(
                        source,
                        &statement,
                        then_target,
                        else_target,
                        Some(shared),
                        true,
                    )
                        && !self.visited.contains(&shared)
                    {
                        if std::env::var_os("MEDAL_DEBUG_RESTRUCTURE").is_some() {
                            eprintln!(
                                "shared tail: source={} join={}",
                                source.index(),
                                shared.index()
                            );
                        }
                        return Some(result);
                    }
                    if std::env::var_os("MEDAL_DEBUG_RESTRUCTURE").is_some() {
                        eprintln!(
                            "shared tail FAILED: source={} join={}",
                            source.index(),
                            shared.index()
                        );
                    }
                    self.rewrite = base_rewrite;
                    self.visited = base_visited;
                }
            }
            self.build_plain_conditional(source, &statement, then_target, else_target, join, false)
        }
    }

    /// Conditional inside a loop whose arms meet at `join` (a node owned by the
    /// loop).  With `shared_tail`, `join` was inferred from reachability rather
    /// than post-dominance, so an arm may terminate instead of reaching it.
    fn build_inside_join_conditional(
        &mut self,
        source: NodeIndex,
        statement: &If,
        then_target: NodeIndex,
        else_target: NodeIndex,
        join: NodeIndex,
        shared_tail: bool,
        ctx: &LoopContext<'_>,
    ) -> Option<PathResult> {
        // Rewrites created by a nested loop are path-sensitive.  Do
        // not let a loop that is only present in one arm leak its
        // export mapping into the other arm (or into the join).
        let base_rewrite = self.rewrite.clone();
        let base_visited = self.visited.clone();
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
        let then_visited = self.visited.clone();
        self.visited = base_visited.clone();
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
        let mut then_result = then_result;
        let mut else_result = else_result;
        if shared_tail {
            Self::seal_shared_tail_arm(&mut then_result, join, ctx.info.header);
            Self::seal_shared_tail_arm(&mut else_result, join, ctx.info.header);
        }
        if !Self::conditional_arms_join(Some(join), shared_tail, &then_result, &else_result) {
            return None;
        }
        let both_reach = then_result.next == Some(join) && else_result.next == Some(join);
        let mut else_rewrite = self.rewrite.clone();
        if !both_reach && then_rewrite != else_rewrite {
            // A terminal arm has no join environment to publish a rewrite
            // into; leave this shape to the ordinary arm builder.
            return None;
        }
        self.visited.extend(then_visited);
        let mut then_block = then_transfer;
        then_block.extend(then_result.block.0);
        let mut else_block = else_transfer;
        else_block.extend(else_result.block.0);
        self.materialize_optional_export_gaps(
            &base_rewrite,
            &mut then_rewrite,
            &mut else_rewrite,
            Some(join),
            both_reach,
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

    /// Conditional outside any loop.  `join` is the structured join (or `None`
    /// when every arm terminates); with `shared_tail` an arm may terminate
    /// while the other reaches `join`.
    fn build_plain_conditional(
        &mut self,
        source: NodeIndex,
        statement: &If,
        then_target: NodeIndex,
        else_target: NodeIndex,
        join: Option<NodeIndex>,
        shared_tail: bool,
    ) -> Option<PathResult> {
        let base_rewrite = self.rewrite.clone();
        let base_visited = self.visited.clone();
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
        let then_visited = self.visited.clone();
        self.visited = base_visited.clone();
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
        if !Self::conditional_arms_join(join, shared_tail, &then_result, &else_result) {
            return None;
        }
        let both_reach =
            join.is_some_and(|join| then_result.next == Some(join) && else_result.next == Some(join));
        if shared_tail && !both_reach && then_rewrite != else_rewrite {
            return None;
        }
        self.visited.extend(then_visited);
        let mut then_block = then_transfer;
        then_block.extend(then_result.block.0);
        let mut else_block = else_transfer;
        else_block.extend(else_result.block.0);
        self.materialize_optional_export_gaps(
            &base_rewrite,
            &mut then_rewrite,
            &mut else_rewrite,
            join,
            both_reach,
            &mut then_block,
            &mut else_block,
        )?;
        self.rewrite =
            self.reconcile_rewrite(&base_rewrite, &then_rewrite, &else_rewrite, join)?;
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

    /// Earliest node reachable from both arms of a conditional such that the
    /// arms consume disjoint node sets before it.  Nodes already consumed are
    /// ignored; inside a loop the search stays within the loop's own nodes and
    /// never crosses the header.  Returns `None` when the arms share nothing
    /// (or when the shared region has no single entry).
    fn shared_tail_join(
        &self,
        then_target: NodeIndex,
        else_target: NodeIndex,
        ctx: Option<&LoopContext<'_>>,
        stop: Option<NodeIndex>,
    ) -> Option<NodeIndex> {
        if !self.allow_shared_tail {
            return None;
        }
        let allowed = |node: NodeIndex| {
            self.analysis.reachable.contains(&node)
                && !self.visited.contains(&node)
                && ctx.is_none_or(|ctx| node != ctx.info.header && ctx.info.nodes.contains(&node))
        };
        // Forward reachability that records but never expands `stop` (the
        // enclosing walk's end) and `candidate` (the join under test).
        let reach = |start: NodeIndex, candidate: Option<NodeIndex>| {
            let mut seen = FxHashSet::default();
            let mut work = vec![start];
            while let Some(node) = work.pop() {
                if !allowed(node) || !seen.insert(node) {
                    continue;
                }
                if Some(node) == candidate || Some(node) == stop {
                    continue;
                }
                work.extend(self.function.successor_blocks(node));
            }
            seen
        };
        let then_all = reach(then_target, None);
        let else_all = reach(else_target, None);
        let mut common = then_all.intersection(&else_all).copied().collect_vec();
        if common.is_empty() || common.len() > 256 {
            return None;
        }
        common.sort_by_key(|node| node.index());
        let mut best: Option<(usize, NodeIndex)> = None;
        for candidate in common {
            // The continuation walk must be able to start at the join: a
            // loop header (generic/numeric/while) is only enterable through
            // its own init/region machinery.
            if self.analysis.loops_by_header.contains_key(&candidate)
                || self.analysis.numeric_loops_by_header.contains_key(&candidate)
                || self.analysis.while_loops_by_header.contains_key(&candidate)
            {
                continue;
            }
            let mut pre_then = reach(then_target, Some(candidate));
            let mut pre_else = reach(else_target, Some(candidate));
            pre_then.remove(&candidate);
            pre_else.remove(&candidate);
            if !pre_then.is_disjoint(&pre_else) {
                continue;
            }
            let size = pre_then.len() + pre_else.len();
            if best.is_none_or(|(best_size, _)| size < best_size) {
                best = Some((size, candidate));
            }
        }
        best.map(|(_, join)| join)
    }

    /// A block with no successor whose only real statement is a `return` or
    /// `break`.
    fn block_is_small_terminal(&self, node: NodeIndex) -> bool {
        if self.function.successor_blocks(node).next().is_some() {
            return false;
        }
        let Some(block) = self.function.block(node) else {
            return false;
        };
        let mut statements = block.iter().filter(|statement| !is_ignorable(statement));
        matches!(
            (statements.next(), statements.next()),
            (Some(Statement::Return(_) | Statement::Break(_)), None)
        )
    }

    /// An arm that flows straight back to the loop header (no `stop` hit) is
    /// an explicit `continue` once a tail is emitted after the `if`.
    fn seal_shared_tail_arm(result: &mut PathResult, join: NodeIndex, header: NodeIndex) {
        if result.next == Some(header) && join != header && !block_ends_terminal(&result.block) {
            result.block.push(Statement::Continue(ast::Continue {}));
            result.next = None;
        }
    }

    /// Whether two conditional arms meet the structured join.  Without a
    /// shared tail both arms must reach `join`.  With a shared tail an arm may
    /// instead terminate (its block ends in `return`/`break`/`continue`) while
    /// the other reaches the join, so the tail is emitted exactly once.
    fn conditional_arms_join(
        join: Option<NodeIndex>,
        shared_tail: bool,
        then_result: &PathResult,
        else_result: &PathResult,
    ) -> bool {
        if !shared_tail {
            return then_result.next == join && else_result.next == join;
        }
        let arm_ok =
            |result: &PathResult| result.next == join || block_ends_terminal(&result.block);
        arm_ok(then_result)
            && arm_ok(else_result)
            && (then_result.next == join || else_result.next == join)
    }
    fn build_transfer_arm(
        &mut self,
        source: NodeIndex,
        target: NodeIndex,
        context: &LoopContext<'_>,
    ) -> Option<PathResult> {
        let result = self.build_transfer_arm_inner(source, target, context);
        result
    }

    fn build_transfer_arm_inner(
        &mut self,
        source: NodeIndex,
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
        if self.external_region_reaches_header(target, context) {
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
        let adapter = self.build_exit_adapter(target, context.info.join, context, Some(source))?;
        block.extend(adapter.block.0);
        self.append_export(&mut block, context.exports);
        if let Some(flag) = &context.exhaustion_flag {
            // This transfer is an explicit body-side break.  Mark it before
            // leaving the loop so the normal-exhaustion adapter is skipped.
            block.push(
                Assign::new(
                    vec![LValue::Local(flag.clone())],
                    vec![RValue::Literal(Literal::Boolean(false))],
                )
                .into(),
            );
        }
        block.push(Statement::Break(ast::Break {}).into());
        Some(PathResult {
            block,
            next: Some(context.info.join),
        })
    }

    /// Return the nested conditional shape described in
    /// [`build_conditional`].  The returned boolean records whether the
    /// nested Then edge reaches the shared outer-Else target; in that case
    /// the alternate branch is selected by `outer and not inner`.
    fn shared_tail_shape(
        &self,
        inner_target: NodeIndex,
        common_target: NodeIndex,
        context: &LoopContext<'_>,
    ) -> Option<(If, NodeIndex, NodeIndex, bool)> {
        if inner_target == common_target || !context.info.nodes.contains(&inner_target) {
            return None;
        }
        let block = self.function.block(inner_target)?;
        let statement = block.last()?.clone();
        if block
            .iter()
            .take(block.len().saturating_sub(1))
            .any(|statement| !is_ignorable(statement))
        {
            return None;
        }
        let inner = statement.as_if()?.clone();
        if !inner.then_block.lock().is_empty() || !inner.else_block.lock().is_empty() {
            return None;
        }
        let (then_edge, else_edge) = self.function.conditional_edges(inner_target)?;
        if !then_edge.weight().arguments.is_empty() || !else_edge.weight().arguments.is_empty() {
            return None;
        }
        let inner_then = then_edge.target();
        let inner_else = else_edge.target();
        let inner_then_is_common = inner_then == common_target;
        if !inner_then_is_common && inner_else != common_target {
            return None;
        }
        let alternate = if inner_then_is_common {
            inner_else
        } else {
            inner_then
        };
        if alternate == common_target || !context.info.nodes.contains(&alternate) {
            return None;
        }
        Some((inner, common_target, alternate, inner_then_is_common))
    }

    fn build_shared_tail_conditional(
        &mut self,
        source: NodeIndex,
        statement: If,
        inner_target: NodeIndex,
        common_target: NodeIndex,
        inner: If,
        alternate_target: NodeIndex,
        inner_then_is_common: bool,
        context: &LoopContext<'_>,
    ) -> Option<PathResult> {
        let base_rewrite = self.rewrite.clone();
        let outer_then = self
            .function
            .edges(source)
            .find(|edge| {
                edge.target() == inner_target && edge.weight().branch_type == BranchType::Then
            })?
            .weight()
            .clone();
        let outer_else = self
            .function
            .edges(source)
            .find(|edge| {
                edge.target() == common_target && edge.weight().branch_type == BranchType::Else
            })?
            .weight()
            .clone();
        if !outer_then.arguments.is_empty() || !outer_else.arguments.is_empty() {
            return None;
        }
        let (inner_then_edge, inner_else_edge) = self.function.conditional_edges(inner_target)?;
        let alternate_edge = if inner_then_is_common {
            inner_else_edge
        } else {
            inner_then_edge
        };
        if !alternate_edge.weight().arguments.is_empty() {
            return None;
        }

        // Build the alternate arm first, then the shared arm from the common
        // outer-Else target.  Both paths are required to reach this loop's
        // header; otherwise the one-successor PathResult cannot encode their
        // distinct terminal ports.
        let alternate_result = self.build_transfer_arm(source, alternate_target, context)?;
        let alternate_rewrite = self.rewrite.clone();
        self.rewrite = base_rewrite.clone();
        let common_result = self.build_transfer_arm(source, common_target, context)?;
        let common_rewrite = self.rewrite.clone();
        if alternate_result.next != common_result.next {
            return None;
        }
        let continuation = alternate_result.next;
        let mut alternate_block = alternate_result.block;
        let mut common_block = common_result.block;
        let mut alternate_map = alternate_rewrite;
        let mut common_map = common_rewrite;
        self.materialize_optional_export_gaps(
            &base_rewrite,
            &mut alternate_map,
            &mut common_map,
            continuation,
            continuation.is_some_and(|join| {
                alternate_result.next == Some(join) && common_result.next == Some(join)
            }),
            &mut alternate_block,
            &mut common_block,
        )?;
        self.rewrite =
            self.reconcile_rewrite(&base_rewrite, &alternate_map, &common_map, continuation)?;

        // The nested conditional node is consumed by this factoring rewrite,
        // not emitted as a statement.  Its block has no executable prefix by
        // construction; marking it visited prevents a later shared-tail walk
        // from trying to consume it again.
        self.visited.insert(inner_target);
        let mut outer_condition = statement.condition.clone();
        let mut inner_condition = inner.condition.clone();
        for local in outer_condition.values_read_mut() {
            if let Some(replacement) = base_rewrite.get(local) {
                *local = replacement.clone();
            }
        }
        for local in inner_condition.values_read_mut() {
            if let Some(replacement) = base_rewrite.get(local) {
                *local = replacement.clone();
            }
        }
        if inner_then_is_common {
            inner_condition = Unary::new(inner_condition, UnaryOperation::Not).reduce_condition();
        }
        let mut condition =
            Binary::new(outer_condition, inner_condition, ast::BinaryOperation::And)
                .reduce_condition();
        strip_terminal_continue(&mut alternate_block);
        strip_terminal_continue(&mut common_block);
        simplify_conditional(&mut condition, &mut alternate_block, &mut common_block);
        let output = Block::from(vec![
            If::new(condition, alternate_block, common_block).into(),
        ]);
        Some(PathResult {
            block: output,
            next: continuation,
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

/// Detect a break owned by the current loop body.  Breaks nested inside an
/// already-structured loop target that inner loop, so recurse only through
/// conditionals here; descending into another loop would over-reject safe
/// adapter paths while still missing the path-sensitive direct break case.
fn block_has_owned_break(block: &Block) -> bool {
    block.iter().any(|statement| match statement {
        Statement::Break(_) => true,
        Statement::If(if_statement) => {
            block_has_owned_break(&if_statement.then_block.lock())
                || block_has_owned_break(&if_statement.else_block.lock())
        }
        _ => false,
    })
}

/// Rewrite the outer references of a while body whose condition register is
/// reused as a nested generic-for result.  Generic-for bodies have their own
/// result bindings and must not be rewritten; only the iterator RHS (which is
/// evaluated in the enclosing scope) observes the carried `current` value.
fn rewrite_while_carried_alias(
    block: &mut Block,
    condition_local: &RcLocal,
    carry_local: &RcLocal,
    current_local: &RcLocal,
) {
    let mut generic_seen = false;
    for statement in &mut block.0 {
        match statement {
            Statement::GenericFor(for_loop) => {
                for value in &mut for_loop.right {
                    for local in value.values_read_mut() {
                        if local == condition_local {
                            *local = current_local.clone();
                        }
                    }
                }
                generic_seen = true;
            }
            Statement::If(if_statement) => {
                for local in if_statement.condition.values_read_mut() {
                    if local == condition_local {
                        *local = current_local.clone();
                    }
                }
                rewrite_while_carried_alias(
                    &mut if_statement.then_block.lock(),
                    condition_local,
                    carry_local,
                    current_local,
                );
                rewrite_while_carried_alias(
                    &mut if_statement.else_block.lock(),
                    condition_local,
                    carry_local,
                    current_local,
                );
            }
            Statement::While(while_statement) => {
                for local in while_statement.condition.values_read_mut() {
                    if local == condition_local {
                        *local = current_local.clone();
                    }
                }
                rewrite_while_carried_alias(
                    &mut while_statement.block.lock(),
                    condition_local,
                    carry_local,
                    current_local,
                );
            }
            Statement::Repeat(repeat_statement) => {
                for local in repeat_statement.condition.values_read_mut() {
                    if local == condition_local {
                        *local = current_local.clone();
                    }
                }
                rewrite_while_carried_alias(
                    &mut repeat_statement.block.lock(),
                    condition_local,
                    carry_local,
                    current_local,
                );
            }
            Statement::Assign(assign)
                if generic_seen
                    && assign.left.len() == 1
                    && (assign.left[0].as_local() == Some(condition_local)
                        || assign.left[0].as_local() == Some(carry_local)) =>
            {
                // Keep ordinary post-loop assignments intact.  Their source
                // AST shape is indistinguishable from a compiler-generated
                // exhaustion adapter, and a historical `= nil` seed is not
                // provenance (nor is it killed reliably by the old helper).
                // Exact exhaustion-only copies are handled by the CFG-backed
                // builder before this rewrite runs.
                for local in assign.values_read_mut() {
                    if local == condition_local {
                        *local = current_local.clone();
                    }
                }
                for local in assign.values_written_mut() {
                    if local == condition_local {
                        *local = current_local.clone();
                    }
                }
            }
            _ => {
                for local in statement.values_read_mut() {
                    if local == condition_local {
                        *local = current_local.clone();
                    }
                }
                for local in statement.values_written_mut() {
                    if local == condition_local {
                        *local = current_local.clone();
                    }
                }
            }
        }
    }
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

/// Whether control cannot fall out of the end of `block`: its last real
/// statement is `return`/`break`/`continue`, or an `if` whose two arms both
/// end that way.
fn block_ends_terminal(block: &Block) -> bool {
    let Some(last) = block.iter().rev().find(|statement| !is_ignorable(statement)) else {
        return false;
    };
    match last {
        Statement::Return(_) | Statement::Break(_) | Statement::Continue(_) => true,
        Statement::If(if_statement) => {
            let then_block = if_statement.then_block.lock();
            let else_block = if_statement.else_block.lock();
            !then_block.is_empty()
                && !else_block.is_empty()
                && block_ends_terminal(&then_block)
                && block_ends_terminal(&else_block)
        }
        _ => false,
    }
}

/// Remove `continue` statements that end a loop body, including those that
/// end the arms of a trailing `if`: control would reach the next iteration
/// anyway, so they carry no information.
fn strip_trailing_continues(block: &mut Block) {
    let Some(index) = block
        .0
        .iter()
        .rposition(|statement| !is_ignorable(statement))
    else {
        return;
    };
    match &mut block.0[index] {
        Statement::Continue(_) => {
            block.0.remove(index);
        }
        Statement::If(if_statement) => {
            strip_trailing_continues(&mut if_statement.then_block.lock());
            strip_trailing_continues(&mut if_statement.else_block.lock());
        }
        _ => {}
    }
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
fn source_proves_for_prep_kind(init: &ast::GenericForInit, origin: ast::ForOrigin) -> bool {
    source_proves_for_prep_kind_with_alias(init, origin, None)
}

/// Resolve the small same-block value-flow pattern emitted by Luau's optimizer
/// for specialized iterator loops.  The compiler may first copy the builtin
/// `ipairs`/`pairs` function into a local and then call that local in the
/// `GenericForInit` marker (`local iter = ipairs; ... in iter(t)`).  The
/// source-like printer preserves the local call expression, so it is safe to
/// accept this only when the latest write to that local in the same pre-marker
/// block is the direct builtin global assignment.  An incoming upvalue or an
/// otherwise unwritten local proves stability, not builtin identity, and stays
/// on the certified fallback path.
fn source_proves_for_prep_kind_with_alias(
    init: &ast::GenericForInit,
    origin: ast::ForOrigin,
    alias_context: Option<(&ast::Block, usize)>,
) -> bool {
    source_proves_for_prep_kind_with_alias_and_upvalues(
        init,
        origin,
        alias_context,
        &FxHashSet::default(),
    )
}

/// Like [`source_proves_for_prep_kind_with_alias`], additionally accepting a
/// callee that is one of `stable_upvalues`: an incoming upvalue of this
/// function that no statement or edge transfer in the function ever writes.
///
/// Luau's compiler emits `FORGPREP_NEXT`/`FORGPREP_INEXT` only after proving,
/// on the source side, that the callee is a never-written alias chain ending
/// in the builtin (`local ipairs = ipairs` at module scope is the canonical
/// example).  The specialized opcode is therefore itself the compiler's proof
/// that such an alias is the builtin, and printing the alias call preserves the
/// exact source form; recompiling the emitted source selects the same prep.
fn source_proves_for_prep_kind_with_alias_and_upvalues(
    init: &ast::GenericForInit,
    origin: ast::ForOrigin,
    alias_context: Option<(&ast::Block, usize)>,
    stable_upvalues: &FxHashSet<RcLocal>,
) -> bool {
    let ipairs_aux = origin.aux & 0x8000_0000 != 0;
    let canonical_aux = (if ipairs_aux { 0x8000_0000 } else { 0 }) | origin.result_count as u32;
    if origin.result_count == 0 || origin.aux != canonical_aux {
        return false;
    }

    // A local is a proven alias of the builtin `name` when it is a stable
    // incoming upvalue (see above) or when the latest write to it in the same
    // pre-marker block is the direct builtin global assignment.
    let local_is_builtin_alias = |callee: &RcLocal, name: &str| {
        if stable_upvalues.contains(callee) {
            return true;
        }
        let Some((block, marker)) = alias_context else {
            return false;
        };
        // Find the latest local write before the marker, not merely a direct
        // assignment.  A later non-global write must invalidate the alias.
        let definition = block.iter().take(marker).rev().find(|statement| {
            statement
                .values_written()
                .into_iter()
                .any(|written| written == callee)
        });
        let Some(Statement::Assign(assign)) = definition else {
            return false;
        };
        assign.left.len() == 1
            && assign.right.len() == 1
            && assign.left[0].as_local() == Some(callee)
            && matches!(&assign.right[0], RValue::Global(global) if global_is_named(global, name))
    };
    let value_is_builtin_or_alias = |value: &RValue, name: &str| match value {
        RValue::Global(global) => global_is_named(global, name),
        RValue::Local(local) => local_is_builtin_alias(local, name),
        _ => false,
    };
    let call_is_builtin_or_alias = |value: &RValue, name: &str| {
        if call_is_named(value, name) {
            return true;
        }
        let call = match value {
            RValue::Call(call) => call,
            RValue::Select(ast::Select::Call(call)) => call,
            _ => return false,
        };
        let RValue::Local(callee) = call.value.as_ref() else {
            return false;
        };
        local_is_builtin_alias(callee, name)
    };

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
                    [value] => call_is_builtin_or_alias(value, "pairs"),
                    [generator, _state] => value_is_builtin_or_alias(generator, "next"),
                    [generator, _state, RValue::Literal(Literal::Nil)] => {
                        value_is_builtin_or_alias(generator, "next")
                    }
                    _ => false,
                }
        }
        ast::ForPrepKind::Inext => {
            ipairs_aux
                && origin.result_count <= 2
                && init.0.right.len() == 1
                && call_is_builtin_or_alias(&init.0.right[0], "ipairs")
        }
    }
}

/// Validate the identity-bearing half of the generic-for protocol before any
/// region discovery mutates or consumes the CFG.  Production output always
/// carries provenance; a reachable marker without it is an explicit unsafe
/// metadata-loss condition rather than a request to guess the VM protocol.
fn validate_for_origins(
    function: &Function,
    protected_locals: &FxHashSet<RcLocal>,
) -> Result<(), UnsafeStructureReason> {
    let Some(entry) = function.entry().as_ref().copied() else {
        return Ok(());
    };
    // Incoming upvalues that this function never writes (through a statement
    // or an edge transfer) and that are not parameters.  See
    // `source_proves_for_prep_kind_with_alias_and_upvalues`.
    let stable_upvalues = {
        let mut written = FxHashSet::default();
        for (_, block) in function.blocks() {
            for statement in block.iter() {
                written.extend(statement.values_written().into_iter().cloned());
            }
        }
        for edge in function.graph().edge_weights() {
            written.extend(edge.arguments.iter().map(|(param, _)| param.clone()));
        }
        protected_locals
            .iter()
            .filter(|local| !function.parameters.contains(local) && !written.contains(*local))
            .cloned()
            .collect::<FxHashSet<_>>()
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
        for (statement_index, statement) in block.iter().enumerate() {
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
                    let proven = source_proves_for_prep_kind_with_alias_and_upvalues(
                        init,
                        origin,
                        Some((block, statement_index)),
                        &stable_upvalues,
                    );
                    init_source_proven.insert(origin.id(), proven);
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
    if let Err(reason) = validate_for_origins(&function, protected_locals) {
        return StructureAttempt::Unsafe(reason);
    }
    let allow_shared_tail = std::env::var_os("MEDAL_NO_SHARED_TAIL").is_none();
    let mut attempt = structure_once(&function, protected_locals, allow_shared_tail);
    if std::env::var_os("MEDAL_DEBUG_RESTRUCTURE").is_some() {
        eprintln!(
            "source-like first attempt id={} -> {}",
            function.id,
            match &attempt {
                StructureAttempt::Structured(block) => format!("Structured({} stmts)", block.len()),
                StructureAttempt::Unsupported => "Unsupported".to_string(),
                StructureAttempt::Unsafe(reason) => format!("Unsafe({reason:?})"),
            }
        );
    }
    if matches!(attempt, StructureAttempt::Unsupported) {
        // The shared-tail optimization is speculative: it commits to a join
        // before the rest of the enclosing region is proven.  Never let it
        // cost a function its structured output.
        ast::set_local_id_base(local_ids.base);
        attempt = structure_once(&function, protected_locals, false);
        if std::env::var_os("MEDAL_DEBUG_RESTRUCTURE").is_some() {
            eprintln!(
                "source-like retry id={} -> {}",
                function.id,
                match &attempt {
                    StructureAttempt::Structured(block) => format!("Structured({} stmts)", block.len()),
                    StructureAttempt::Unsupported => "Unsupported".to_string(),
                    StructureAttempt::Unsafe(reason) => format!("Unsafe({reason:?})"),
                }
            );
        }
    }
    if matches!(attempt, StructureAttempt::Structured(_)) {
        local_ids.committed = true;
    }
    attempt
}

fn structure_once(
    function: &Function,
    protected_locals: &FxHashSet<RcLocal>,
    allow_shared_tail: bool,
) -> StructureAttempt {
    let Some(analysis) = Analysis::new(function) else {
        return StructureAttempt::Unsupported;
    };
    let Some(entry) = function.entry().as_ref().copied() else {
        return StructureAttempt::Unsupported;
    };
    let mut builder = Builder::new(function, analysis, protected_locals.clone());
    builder.allow_shared_tail = allow_shared_tail;
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
        Analysis, Builder, StructureAttempt, UnsafeStructureReason, lift as production_lift,
        lift_attempt_with_ignored_locals as production_lift_attempt,
    };
    use ast::{
        Assign, Block, Call, Close, Closure, ForOrigin, ForPrepKind, GenericFor, GenericForInit,
        GenericForNext, Global, If, LValue, Literal, Local, LocalRw, NumForInit, NumForNext,
        RValue, RcLocal, Statement, Table, Upvalue, VmProfileId,
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
        assert!(
            continue_block
                .0
                .iter()
                .all(|statement| !matches!(statement, Statement::Continue(_)))
        );
    }

    #[test]
    fn moves_reentry_exhaustion_flag_before_generic_for() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);

        let flag = RcLocal::new(Local::new(Some("exhausted".into())));
        let generic = GenericFor::new(
            Vec::new(),
            vec![RValue::Global(Global::from("items"))],
            Block::default(),
        );
        let mut generic_block = Block::from(vec![
            generic.into(),
            Assign::new(
                vec![LValue::Local(flag.clone())],
                vec![RValue::Literal(Literal::Boolean(true))],
            )
            .into(),
        ]);
        let tail = Block::from(vec![
            If::new(
                RValue::Local(flag.clone()),
                Block::default(),
                Block::default(),
            )
            .into(),
        ]);

        let analysis = Analysis::new(&function).expect("linear fixture is analyzable");
        let builder = Builder::new(&function, analysis, FxHashSet::default());
        builder
            .move_reentry_reset_before_for(&mut generic_block, &tail)
            .expect("narrow exhaustion sentinel is source-safe");
        assert!(matches!(
            generic_block.0.first(),
            Some(Statement::Assign(_))
        ));
        assert!(matches!(
            generic_block.0.get(1),
            Some(Statement::GenericFor(_))
        ));
    }

    #[test]
    fn moves_guarded_reentry_result_with_private_break_sentinel() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);

        let result = RcLocal::new(Local::new(Some("all_loaded".into())));
        let guard = RcLocal::new(Local::new(Some("exhausted".into())));
        let condition = RcLocal::new(Local::new(Some("track_ready".into())));
        let body = If::new(
            RValue::Local(condition),
            Block::from(vec![
                Assign::new(
                    vec![LValue::Local(result.clone())],
                    vec![RValue::Literal(Literal::Boolean(false))],
                )
                .into(),
                Assign::new(
                    vec![LValue::Local(guard.clone())],
                    vec![RValue::Literal(Literal::Boolean(false))],
                )
                .into(),
                Statement::Break(ast::Break {}).into(),
            ]),
            Block::default(),
        );
        let generic = GenericFor::new(
            Vec::new(),
            vec![RValue::Global(Global::from("tracks"))],
            Block::from(vec![body.into()]),
        );
        let mut generic_block = Block::from(vec![
            Assign::new(
                vec![LValue::Local(guard.clone())],
                vec![RValue::Literal(Literal::Boolean(true))],
            )
            .into(),
            generic.into(),
            If::new(
                RValue::Local(guard.clone()),
                Block::from(vec![
                    Assign::new(
                        vec![LValue::Local(result.clone())],
                        vec![RValue::Literal(Literal::Boolean(true))],
                    )
                    .into(),
                ]),
                Block::default(),
            )
            .into(),
        ]);
        let tail = Block::from(vec![
            If::new(RValue::Local(result.clone()), Block::default(), Block::default()).into(),
        ]);

        let analysis = Analysis::new(&function).expect("linear fixture is analyzable");
        let builder = Builder::new(&function, analysis, FxHashSet::default());
        builder
            .move_reentry_reset_before_for(&mut generic_block, &tail)
            .expect("private break sentinel is proven source-safe");
        assert_eq!(generic_block.0.len(), 2);
        assert!(matches!(generic_block.0[0], Statement::Assign(_)));
        assert!(matches!(generic_block.0[1], Statement::GenericFor(_)));
        let Statement::Assign(reset) = &generic_block.0[0] else {
            panic!("expected result reset before generic for")
        };
        assert_eq!(reset.left[0].as_local(), Some(&result));
        assert!(matches!(
            generic_block.0[1],
            Statement::GenericFor(ref loop_node)
                if !loop_node
                    .block
                    .lock()
                    .iter()
                    .any(|statement| statement.values_written().into_iter().any(|local| local == &guard))
        ));
    }

    #[test]
    fn refuses_reentry_flag_write_captured_by_nested_loop_break() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);

        let flag = RcLocal::new(Local::new(Some("exhausted".into())));
        let nested = GenericFor::new(
            Vec::new(),
            vec![RValue::Global(Global::from("inner_items"))],
            Block::from(vec![
                Assign::new(
                    vec![LValue::Local(flag.clone())],
                    vec![RValue::Literal(Literal::Boolean(false))],
                )
                .into(),
                Statement::Break(ast::Break {}).into(),
            ]),
        );
        let generic = GenericFor::new(
            Vec::new(),
            vec![RValue::Global(Global::from("items"))],
            Block::from(vec![nested.into()]),
        );
        let mut generic_block = Block::from(vec![
            generic.into(),
            Assign::new(
                vec![LValue::Local(flag.clone())],
                vec![RValue::Literal(Literal::Boolean(true))],
            )
            .into(),
        ]);
        let tail = Block::from(vec![
            If::new(RValue::Local(flag), Block::default(), Block::default()).into(),
        ]);

        let analysis = Analysis::new(&function).expect("linear fixture is analyzable");
        let builder = Builder::new(&function, analysis, FxHashSet::default());
        assert!(
            builder
                .move_reentry_reset_before_for(&mut generic_block, &tail)
                .is_none()
        );
    }

    // Hand-built CFG fixtures predate provenance-bearing marker constructors.
    // Attach deterministic, source-proven test origins at the test boundary so
    // those fixtures exercise the same proof path as real lifter output. Tests
    // that specifically validate metadata loss call the production entry point
    // through `super::` and therefore bypass this compatibility helper.
    fn attach_test_origins(function: &mut Function) {
        let nodes = function.blocks().map(|(node, _)| node).collect::<Vec<_>>();
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
                [RValue::Call(call)] if matches!(call.value.as_ref(), RValue::Global(global) if global.0 == b"pairs") => {
                    ForPrepKind::Next
                }
                [RValue::Call(call)] if matches!(call.value.as_ref(), RValue::Global(global) if global.0 == b"ipairs") => {
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
                explicit_nil_args: false,
            };
            if let Some(block) = function.block_mut(init_node) {
                if let Some(marker) = block.iter_mut().find_map(|s| s.as_generic_for_init_mut()) {
                    marker.1 = Some(origin);
                }
            }
            if let Some(block) = function.block_mut(next_node) {
                if let Some(marker) = block.iter_mut().find_map(|s| s.as_generic_for_next_mut()) {
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
        function.set_edges(
            entry,
            vec![(
                exit,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(
                        RcLocal::new(Local::new(Some("x".into()))),
                        RValue::Literal(Literal::Nil),
                    )],
                },
            )],
        );
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
        function.set_edges(
            entry,
            vec![
                (
                    then_node,
                    BlockEdge {
                        branch_type: BranchType::Then,
                        arguments: vec![(incoming.clone(), Literal::Number(1.0).into())],
                    },
                ),
                (
                    else_node,
                    BlockEdge {
                        branch_type: BranchType::Else,
                        arguments: vec![(incoming.clone(), Literal::Number(2.0).into())],
                    },
                ),
            ],
        );
        function.set_edges(
            then_node,
            vec![(join, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            else_node,
            vec![(join, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function.set_edges(
            entry,
            vec![
                (left, BlockEdge::new(BranchType::Unconditional)),
                (right, BlockEdge::new(BranchType::Unconditional)),
            ],
        );
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
                    Assign::new(
                        vec![LValue::Local(value.clone())],
                        vec![Literal::Number(1.0).into()],
                    )
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
        function.set_edges(
            entry,
            vec![(exit, BlockEdge::new(BranchType::Unconditional))],
        );

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
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![Literal::Number(1.0).into()],
            )
            .into(),
        );
        function.block_mut(else_node).unwrap().push(
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![Literal::Number(2.0).into()],
            )
            .into(),
        );
        function.block_mut(join).unwrap().push(
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Local(result.clone())],
            )
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            entry,
            vec![
                (else_node, BlockEdge::new(BranchType::Else)),
                (then_node, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            then_node,
            vec![(join, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            else_node,
            vec![(join, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(exit, BlockEdge::new(BranchType::Unconditional))],
        );

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
            explicit_nil_args: false,
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

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        // Deliberately insert Else first: graph insertion order is not branch
        // semantics, so the structurer must honor the edge tags.
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
            explicit_nil_args: false,
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![
            RValue::Global(Global::from("next")),
            RValue::Global(Global::from("items")),
            RValue::Global(Global::from("extra")),
        ];
        for_init.1 = Some(origin);
        function.block_mut(init).unwrap().push(for_init.into());
        let mut for_next =
            GenericForNext::new(vec![value, value2], generator.into(), state, control);
        for_next.origin = Some(origin);
        function.block_mut(header).unwrap().push(for_next.into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
                explicit_nil_args: false,
            };
            let mut for_init =
                GenericForInit::new(generator.clone(), state.clone(), control.clone());
            for_init.0.right = vec![right];
            for_init.1 = Some(origin);
            function.block_mut(init).unwrap().push(for_init.into());
            let mut for_next = GenericForNext::new(vec![value], generator.into(), state, control);
            for_next.origin = Some(origin);
            function.block_mut(header).unwrap().push(for_next.into());
            function
                .block_mut(exit)
                .unwrap()
                .push(Statement::Return(Default::default()).into());
            function.set_edges(
                init,
                vec![(header, BlockEdge::new(BranchType::Unconditional))],
            );
            function.set_edges(
                header,
                vec![
                    (body, BlockEdge::new(BranchType::Then)),
                    (exit, BlockEdge::new(BranchType::Else)),
                ],
            );
            function.set_edges(
                body,
                vec![(header, BlockEdge::new(BranchType::Unconditional))],
            );
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
    fn rejects_stable_local_inext_alias_without_builtin_provenance() {
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let callee = RcLocal::new(Local::new(Some("iter".into())));
        let mut init = GenericForInit::new(generator, state, control);
        init.0.right = vec![RValue::Call(Call::new(
            RValue::Local(callee),
            vec![RValue::Global(Global::from("items"))],
        ))];
        let origin = ForOrigin {
            prep_pc: 1,
            step_pc: 2,
            body_pc: 3,
            follow_pc: 4,
            prep_kind: ForPrepKind::Inext,
            base_register: 0,
            result_count: 1,
            aux: 0x8000_0001,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
            explicit_nil_args: false,
        };
        assert!(!super::source_proves_for_prep_kind(&init, origin));
    }

    #[test]
    fn accepts_same_block_ipairs_alias_with_latest_builtin_definition() {
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let callee = RcLocal::new(Local::new(Some("iter".into())));
        let mut init = GenericForInit::new(generator, state, control);
        init.0.right = vec![RValue::Call(Call::new(
            RValue::Local(callee.clone()),
            vec![RValue::Global(Global::from("items"))],
        ))];
        let origin = ForOrigin {
            prep_pc: 1,
            step_pc: 2,
            body_pc: 3,
            follow_pc: 4,
            prep_kind: ForPrepKind::Inext,
            base_register: 0,
            result_count: 1,
            aux: 0x8000_0001,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
            explicit_nil_args: false,
        };
        let block = Block::from(vec![
            Assign::new(
                vec![LValue::Local(callee)],
                vec![RValue::Global(Global::from("ipairs"))],
            )
            .into(),
            init.clone().into(),
        ]);
        assert!(super::source_proves_for_prep_kind_with_alias(
            &init,
            origin,
            Some((&block, 1)),
        ));
    }

    #[test]
    fn accepts_stable_upvalue_ipairs_alias_for_inext() {
        // `local ipairs = ipairs` at module scope, called through an incoming
        // upvalue that the function never writes (the BoatTween/Lerps shape).
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let callee = RcLocal::new(Local::new(Some("ipairs2".into())));
        let mut init = GenericForInit::new(generator, state, control);
        init.0.right = vec![RValue::Call(Call::new(
            RValue::Local(callee.clone()),
            vec![RValue::Global(Global::from("items"))],
        ))];
        let origin = ForOrigin {
            prep_pc: 1,
            step_pc: 2,
            body_pc: 3,
            follow_pc: 4,
            prep_kind: ForPrepKind::Inext,
            base_register: 0,
            result_count: 2,
            aux: 0x8000_0002,
            bytecode_version: 9,
            vm_profile: VmProfileId::Luau,
            explicit_nil_args: false,
        };
        let stable = FxHashSet::from_iter([callee.clone()]);
        assert!(super::source_proves_for_prep_kind_with_alias_and_upvalues(
            &init,
            origin,
            None,
            &stable,
        ));
        // The same callee without the upvalue proof stays rejected.
        assert!(!super::source_proves_for_prep_kind_with_alias(&init, origin, None));
        assert!(!super::source_proves_for_prep_kind_with_alias_and_upvalues(
            &init,
            origin,
            None,
            &FxHashSet::default(),
        ));
    }

    #[test]
    fn accepts_next_alias_tuple_forms_for_next_prep() {
        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let next_alias = RcLocal::new(Local::new(Some("next2".into())));
        let origin = ForOrigin {
            prep_pc: 1,
            step_pc: 2,
            body_pc: 3,
            follow_pc: 4,
            prep_kind: ForPrepKind::Next,
            base_register: 0,
            result_count: 1,
            aux: 1,
            bytecode_version: 9,
            vm_profile: VmProfileId::Luau,
            explicit_nil_args: false,
        };
        let stable = FxHashSet::from_iter([next_alias.clone()]);
        for right in [
            vec![
                RValue::Local(next_alias.clone()),
                RValue::Global(Global::from("items")),
            ],
            vec![
                RValue::Local(next_alias.clone()),
                RValue::Global(Global::from("items")),
                RValue::Literal(Literal::Nil),
            ],
        ] {
            let mut init =
                GenericForInit::new(generator.clone(), state.clone(), control.clone());
            init.0.right = right;
            assert!(super::source_proves_for_prep_kind_with_alias_and_upvalues(
                &init,
                origin,
                None,
                &stable,
            ));
            assert!(!super::source_proves_for_prep_kind_with_alias(&init, origin, None));
        }
        // Same-block latest-write alias of `next` is also accepted in tuple form.
        let mut init = GenericForInit::new(generator, state, control);
        init.0.right = vec![
            RValue::Local(next_alias.clone()),
            RValue::Global(Global::from("items")),
        ];
        let block = Block::from(vec![
            Assign::new(
                vec![LValue::Local(next_alias)],
                vec![RValue::Global(Global::from("next"))],
            )
            .into(),
            init.clone().into(),
        ]);
        assert!(super::source_proves_for_prep_kind_with_alias(
            &init,
            origin,
            Some((&block, 1)),
        ));
    }

    #[test]
    fn accepts_loop_owned_ref_captured_result() {
        // `for _, v in items do v = v * 2; fns[1] = function() return v end end`
        // The closure keeps the iteration cell; nothing outside the loop
        // touches the result, so the plain source `for` is exact.
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
        let fns = RcLocal::new(Local::new(Some("fns".into())));
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
        function.block_mut(init).unwrap().push(
            Assign::new(vec![LValue::Local(fns.clone())], vec![RValue::Table(Table(vec![]))])
                .into(),
        );
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Binary(ast::Binary::new(
                    RValue::Local(result.clone()),
                    Literal::Number(2.0).into(),
                    ast::BinaryOperation::Mul,
                ))],
            )
            .into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(
                vec![LValue::Index(ast::Index::new(
                    RValue::Local(fns.clone()),
                    Literal::Number(1.0).into(),
                ))],
                vec![RValue::Closure(closure)],
            )
            .into(),
        );
        function
            .block_mut(join)
            .unwrap()
            .push(ast::Return::new(vec![RValue::Local(fns)]).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

        let attempt = lift_attempt_with_ignored_locals(function, &FxHashSet::default());
        let StructureAttempt::Structured(block) = attempt else {
            panic!("loop-owned ref capture must structure, got {attempt:?}");
        };
        let generic_for = block
            .iter()
            .find_map(|statement| statement.as_generic_for())
            .expect("source-level generic for");
        assert_eq!(generic_for.res_locals, vec![result.clone()]);
        let body = generic_for.block.lock();
        assert!(body.iter().any(|statement| {
            let mut captures = FxHashSet::default();
            super::collect_statement_captures(statement, &mut captures);
            captures.contains(&result)
        }));
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
            explicit_nil_args: false,
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        for_init.1 = Some(origin);
        function.block_mut(init).unwrap().push(for_init.into());
        let mut for_next =
            GenericForNext::new(vec![value, value2], generator.into(), state, control);
        for_next.origin = Some(origin);
        function.block_mut(header).unwrap().push(for_next.into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![result], generator.into(), state, control).into());
        function.block_mut(join).unwrap().push(
            ast::Return::new(vec![RValue::Call(Call::new(
                RValue::Local(callback.clone()),
                Vec::new(),
            ))])
            .into(),
        );
        function.set_edges(
            pre,
            vec![(
                init,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(callback, RValue::Closure(closure))],
                },
            )],
        );
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![result], generator.into(), state, control).into());
        function.block_mut(join).unwrap().push(
            ast::Return::new(vec![RValue::Call(Call::new(
                RValue::Local(callback.clone()),
                Vec::new(),
            ))])
            .into(),
        );
        function.set_edges(
            pre,
            vec![(
                init,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(callback, RValue::Closure(closure))],
                },
            )],
        );
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        function
            .block_mut(tail)
            .unwrap()
            .push(ast::Return::new(vec![RValue::Local(sink.clone())]).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(
                tail,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(sink, RValue::Local(result))],
                },
            )],
        );

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
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(
                vec![LValue::Local(callback.clone())],
                vec![RValue::Closure(closure)],
            )
            .into(),
        );
        function.block_mut(tail).unwrap().push(
            ast::Return::new(vec![RValue::Call(Call::new(
                RValue::Local(callback),
                Vec::new(),
            ))])
            .into(),
        );
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(
                tail,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(result, Literal::Number(42.0).into())],
                },
            )],
        );

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
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(
                vec![LValue::Local(callback.clone())],
                vec![RValue::Closure(closure)],
            )
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

        // Even an explicit nil write on direct exhaustion targets the shared
        // VM result cell. A source-level `for` binding would instead leave the
        // closure observing its last loop-local value.
        assert!(lift(function).is_none());
    }

    #[test]
    /// A loop result captured by reference inside the loop body, with no use
    /// of that result outside the loop, is exactly a source `for` whose body
    /// creates the closure: each iteration binds a fresh cell.  This is the
    /// GameUpgrade.lua:p2 corpus shape.
    fn structures_loop_owned_ref_capture_of_loop_result() {
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
            }))),
            upvalues: vec![Upvalue::Ref(result.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function
            .block_mut(header)
            .unwrap()
            .push(
                GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

        let attempt = lift_attempt_with_ignored_locals(function, &FxHashSet::default());
        let StructureAttempt::Structured(block) = attempt else {
            panic!("loop-owned ref capture must structure, got {attempt:?}");
        };
        let generic_for = block
            .iter()
            .find_map(|statement| statement.as_generic_for())
            .expect("source-level generic for");
        assert!(generic_for.block.lock().iter().any(|statement| {
            let mut captures = FxHashSet::default();
            super::collect_statement_captures(statement, &mut captures);
            captures.contains(&result)
        }));
    }

    #[test]
    /// A body write to the captured result before the closure is created is
    /// ordinary source (`v = v; collect(function() return v end)`); the
    /// closure still observes its own iteration's final value.
    fn structures_loop_owned_ref_capture_with_body_write() {
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
            }))),
            upvalues: vec![Upvalue::Ref(result.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(for_init.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Local(result.clone())],
            )
            .into(),
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

        let attempt = lift_attempt_with_ignored_locals(function, &FxHashSet::default());
        let StructureAttempt::Structured(block) = attempt else {
            panic!("loop-owned ref capture must structure, got {attempt:?}");
        };
        let generic_for = block
            .iter()
            .find_map(|statement| statement.as_generic_for())
            .expect("source-level generic for");
        assert!(generic_for.block.lock().iter().any(|statement| {
            let mut captures = FxHashSet::default();
            super::collect_statement_captures(statement, &mut captures);
            captures.contains(&result)
        }));
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
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(
                tail,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(callback, RValue::Closure(closure))],
                },
            )],
        );

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
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(tail, BlockEdge::new(BranchType::Unconditional))],
        );

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
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
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
            Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(result)]).into(),
            Assign::new(vec![LValue::Local(outer)], vec![outer_value]).into(),
            ast::Return::new(vec![RValue::Call(Call::new(call_outer, Vec::new()))]).into(),
        ]);
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(tail, BlockEdge::new(BranchType::Unconditional))],
        );

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
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(tail, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![result], generator.into(), state, control).into());
        function
            .block_mut(body)
            .unwrap()
            .push(Assign::new(vec![LValue::Local(outer.clone())], vec![outer_value]).into());
        let call_outer = RValue::Call(Call::new(RValue::Local(outer), Vec::new()));
        function
            .block_mut(tail)
            .unwrap()
            .push(ast::Return::new(vec![RValue::Call(Call::new(call_outer, Vec::new()))]).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(tail, BlockEdge::new(BranchType::Unconditional))],
        );

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
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (join, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(tail, BlockEdge::new(BranchType::Unconditional))],
        );

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
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
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

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        // This is a phi copy on the backedge into the hidden FORGLOOP control
        // register.  Emitting it as a visible assignment in a source `for`
        // body would not affect the VM's hidden iterator state.
        function.set_edges(
            body,
            vec![(
                header,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(control, Literal::Number(42.0).into())],
                },
            )],
        );

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
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value], generator.into(), state, control).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(
            init,
            vec![(
                header,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(incoming, Literal::Number(1.0).into())],
                },
            )],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
            explicit_nil_args: false,
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
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
    fn does_not_classify_for_init_suffix_write_to_copy_capture_as_cell_reorder() {
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
            // A value capture is a snapshot at closure construction. Rebinding
            // the local after iterator preparation cannot affect that snapshot.
            upvalues: vec![Upvalue::Copy(captured.clone())],
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Call(Call::new(
            RValue::Closure(closure),
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        let attempt = lift_attempt_with_ignored_locals(function, &FxHashSet::default());
        // This minimal fixture may remain unsupported for another structural
        // reason, but a value capture must never be reported as a mutable-cell
        // reorder.
        assert!(!matches!(
            attempt,
            StructureAttempt::Unsafe(UnsafeStructureReason::CapturedCellReorder)
        ));
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function.block_mut(header).unwrap().push(
            GenericForNext::new(vec![value.clone()], generator.into(), state, control).into(),
        );
        function.block_mut(body).unwrap().push(
            Close {
                locals: vec![value],
            }
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
            GenericForNext::new(vec![result.clone()], generator.into(), state, control).into(),
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![result], generator.into(), state, control).into());
        function.block_mut(exhausted).unwrap().push(
            Assign::new(
                vec![LValue::Local(callback.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        function
            .block_mut(tail)
            .unwrap()
            .push(ast::Return::new(vec![RValue::Local(callback.clone())]).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exhausted, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            exhausted,
            vec![(
                tail,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(callback, closure)],
                },
            )],
        );

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

        let mut child_init = GenericForInit::new(
            child_generator.clone(),
            child_state.clone(),
            child_control.clone(),
        );
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
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value], generator.into(), state, control).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function.set_edges(
            join,
            vec![(read, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            read,
            vec![(join, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function
            .block_mut(join)
            .unwrap()
            .push(Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(raw.clone())]).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            join,
            vec![(exit, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function
            .block_mut(join)
            .unwrap()
            .push(Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(raw.clone())]).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            join,
            vec![(exit, BlockEdge::new(BranchType::Unconditional))],
        );

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

        assert!(
            builder
                .materialize_optional_export_gaps(
                    &base,
                    &mut then_map,
                    &mut else_map,
                    Some(join),
                    false,
                    &mut then_block,
                    &mut else_block,
                )
                .is_none()
        );
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
        function.set_edges(
            entry,
            vec![(
                exit,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(destination, RValue::Local(source.clone()))],
                },
            )],
        );
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
        function.set_edges(
            entry,
            vec![(
                exit,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![
                        (a.clone(), RValue::Local(b.clone())),
                        (b.clone(), RValue::Local(a.clone())),
                    ],
                },
            )],
        );
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
            Assign::new(
                vec![LValue::Local(sink)],
                vec![RValue::Local(reused_result)],
            )
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(
            first_init,
            vec![(first_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            first_header,
            vec![
                (between, BlockEdge::new(BranchType::Else)),
                (first_body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            first_body,
            vec![(first_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            between,
            vec![(second_init, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            second_init,
            vec![(second_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            second_header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (second_body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            second_body,
            vec![(second_header, BlockEdge::new(BranchType::Unconditional))],
        );

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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        // An empty loop body is a legal compiler shape: FORGLOOP's Then edge
        // points back to its own header and Else leaves the loop.
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (header, BlockEdge::new(BranchType::Then)),
            ],
        );

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
            explicit_nil_args: false,
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (empty_body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            empty_body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
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
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value], generator.into(), state, control).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        // Source `break` is patched directly to the follow block; there is no
        // body-to-header dominance backedge to seed a natural loop.
        function.set_edges(
            body,
            vec![(exit, BlockEdge::new(BranchType::Unconditional))],
        );

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
            explicit_nil_args: false,
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

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        // Both FORGLOOP arms target the follow block.  The Then arm is the
        // source-level unconditional break; the Else arm is exhaustion.
        function.set_edges(
            header,
            vec![
                (follow, BlockEdge::new(BranchType::Else)),
                (follow, BlockEdge::new(BranchType::Then)),
            ],
        );

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
        function.block_mut(body).unwrap().push(
            Statement::Return(ast::Return {
                values: vec![value.into()],
            })
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
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
        function.block_mut(return_block).unwrap().push(
            Statement::Return(ast::Return {
                values: vec![value.into()],
            })
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body_if, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body_if,
            vec![
                (return_block, BlockEdge::new(BranchType::Then)),
                (continue_block, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            continue_block,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
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
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![value], generator.into(), state, control).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
            explicit_nil_args: false,
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
        function.set_edges(
            entry,
            vec![
                (terminal, BlockEdge::new(BranchType::Then)),
                (infinite, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            infinite,
            vec![(infinite, BlockEdge::new(BranchType::Unconditional))],
        );

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
            Assign::new(
                vec![LValue::Local(result)],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        function
            .block_mut(join)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (normal_exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
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
        function
            .block_mut(header)
            .unwrap()
            .push(GenericForNext::new(vec![result], generator.into(), state, control).into());
        function
            .block_mut(body)
            .unwrap()
            .push(If::new(RValue::Local(keep), Block::default(), Block::default()).into());
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

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (normal_exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![
                (break_adapter, BlockEdge::new(BranchType::Else)),
                (header, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            break_adapter,
            vec![(join, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            normal_exit,
            vec![(
                join,
                BlockEdge {
                    branch_type: BranchType::Unconditional,
                    arguments: vec![(sink, Literal::Number(42.0).into())],
                },
            )],
        );

        let output = lift(function).map(|block| block.to_string());
        assert!(
            output.is_none(),
            "unexpected source-shaped output: {:?}",
            output
        );
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

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (join, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
            Assign::new(
                vec![LValue::Local(sink)],
                vec![RValue::Local(result.clone())],
            )
            .into(),
            Assign::new(
                vec![LValue::Local(result)],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
            Statement::Return(Default::default()).into(),
        ]);

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (join, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

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
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        // This write is outside the loop/normal adapter but the result is
        // exported below.  Rewriting it to the export would leave closures or
        // aliases of the original result register observing the wrong cell.
        function.block_mut(join).unwrap().extend(
            vec![
                Assign::new(
                    vec![LValue::Local(result.clone())],
                    vec![RValue::Literal(Literal::Nil)],
                )
                .into(),
                Assign::new(vec![LValue::Local(sink)], vec![RValue::Local(result)]).into(),
                Statement::Return(Default::default()).into(),
            ]
            .into_iter(),
        );
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (normal_exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            normal_exit,
            vec![(join, BlockEdge::new(BranchType::Unconditional))],
        );

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
            Assign::new(
                vec![LValue::Local(sink.clone())],
                vec![RValue::Local(control.clone())],
            )
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
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            exit,
            vec![(after, BlockEdge::new(BranchType::Unconditional))],
        );

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
            Assign::new(
                vec![LValue::Local(sink)],
                vec![RValue::Local(inner_value.clone())],
            )
            .into(),
        );
        function
            .block_mut(outer_exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(
            outer_init,
            vec![(outer_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            outer_header,
            vec![
                (outer_exit, BlockEdge::new(BranchType::Else)),
                (outer_body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            outer_body,
            vec![(inner_init, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            inner_init,
            vec![(inner_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            inner_header,
            vec![
                (inner_exit, BlockEdge::new(BranchType::Else)),
                (inner_body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            inner_body,
            vec![(inner_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            inner_exit,
            vec![(after_inner, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            after_inner,
            vec![(outer_header, BlockEdge::new(BranchType::Unconditional))],
        );
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
            let mut exhaustion_nil = Assign::new(
                vec![LValue::Local(pet.clone())],
                vec![RValue::Literal(Literal::Nil)],
            );
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
            Assign::new(
                vec![LValue::Local(sink)],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        function
            .block_mut(outer_exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(
            outer_init,
            vec![(outer_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            outer_header,
            vec![
                (outer_exit, BlockEdge::new(BranchType::Else)),
                (inner_init, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            inner_init,
            vec![(inner_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            inner_header,
            vec![
                (inner_exhaustion, BlockEdge::new(BranchType::Else)),
                (inner_if, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            inner_if,
            vec![
                (inner_continue, BlockEdge::new(BranchType::Else)),
                (inner_break_adapter, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            inner_continue,
            vec![(inner_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            inner_break_adapter,
            vec![(after_inner, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            inner_exhaustion,
            vec![(after_inner, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            after_inner,
            vec![
                (keep, BlockEdge::new(BranchType::Then)),
                (remove, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            keep,
            vec![(outer_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            remove,
            vec![(outer_header, BlockEdge::new(BranchType::Unconditional))],
        );

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

        function.set_edges(
            outer_init,
            vec![(outer_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            outer_header,
            vec![
                (outer_exit, BlockEdge::new(BranchType::Else)),
                (inner_init, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            inner_init,
            vec![(inner_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            inner_header,
            vec![
                (after_inner, BlockEdge::new(BranchType::Else)),
                (inner_if, BlockEdge::new(BranchType::Then)),
            ],
        );
        // This branch skips the enclosing loop's body tail and exits the
        // parent directly.  A nested source `break` cannot represent that
        // transfer, so source-like structuring must decline the graph.
        function.set_edges(
            inner_if,
            vec![
                (outer_exit, BlockEdge::new(BranchType::Then)),
                (inner_continue, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            inner_continue,
            vec![(inner_header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            after_inner,
            vec![(outer_header, BlockEdge::new(BranchType::Unconditional))],
        );

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
            Assign::new(
                vec![LValue::Local(result.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        function
            .block_mut(other_exit)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("other exit".into())).into());
        function.block_mut(join).unwrap().push(
            Assign::new(
                vec![LValue::Local(sink)],
                vec![RValue::Local(result.clone())],
            )
            .into(),
        );
        function
            .block_mut(function_exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (normal_exit, BlockEdge::new(BranchType::Else)),
                (body, BlockEdge::new(BranchType::Then)),
            ],
        );
        function.set_edges(
            body,
            vec![(first_if, BlockEdge::new(BranchType::Unconditional))],
        );
        // The first Then arm breaks through the same adapter used by normal
        // exhaustion.  Skipping this adapter would leak the previous result.
        function.set_edges(
            first_if,
            vec![
                (normal_exit, BlockEdge::new(BranchType::Then)),
                (second_if, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            second_if,
            vec![
                (other_exit, BlockEdge::new(BranchType::Then)),
                (continue_node, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            continue_node,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            normal_exit,
            vec![(join, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            other_exit,
            vec![(join, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            join,
            vec![(function_exit, BlockEdge::new(BranchType::Unconditional))],
        );

        let output = lift(function)
            .expect("a body break through normal exit must remain source-shaped")
            .to_string();
        assert!(output.contains("if first then"), "{output}");
        assert!(output.contains("result = nil"), "{output}");
        assert!(output.contains("break"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn structures_reducible_numeric_for_markers() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let counter = RcLocal::new(Local::new(Some("i".into())));
        let limit = RcLocal::new(Local::new(Some("limit".into())));
        let step = RcLocal::new(Local::new(Some("step".into())));
        let mut marker = NumForInit::new(counter.clone(), limit.clone(), step.clone());
        marker.counter.1 = Literal::Number(1.0).into();
        marker.limit.1 = Literal::Number(3.0).into();
        marker.step.1 = Literal::Number(1.0).into();
        function.block_mut(init).unwrap().push(marker.into());
        function.block_mut(header).unwrap().push(
            NumForNext::new(
                counter.clone(),
                RValue::Local(limit.clone()),
                RValue::Local(step.clone()),
            )
            .into(),
        );
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("body".into())).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

        let output = lift(function)
            .expect("a reducible numeric marker pair must become a source for")
            .to_string();
        assert!(output.contains("for i = 1, 3 do"), "{output}");
        assert!(!output.contains("NumForInit"), "{output}");
        assert!(!output.contains("goto "), "{output}");
    }

    #[test]
    fn rejects_numeric_for_with_post_prep_suffix() {
        let mut function = Function::new(0);
        let init = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(init);

        let counter = RcLocal::new(Local::new(Some("i".into())));
        let limit = RcLocal::new(Local::new(Some("limit".into())));
        let step = RcLocal::new(Local::new(Some("step".into())));
        let observed = RcLocal::new(Local::new(Some("observed".into())));
        let mut marker = NumForInit::new(counter.clone(), limit.clone(), step.clone());
        marker.counter.1 = Literal::Number(1.0).into();
        marker.limit.1 = Literal::Number(3.0).into();
        marker.step.1 = Literal::Number(1.0).into();
        function.block_mut(init).unwrap().extend([
            marker.into(),
            // This read would observe a different value if moved before
            // FORNPREP's hidden bound conversion.
            Assign::new(
                vec![LValue::Local(observed)],
                vec![RValue::Local(limit.clone())],
            )
            .into(),
        ]);
        function.block_mut(header).unwrap().push(
            NumForNext::new(
                counter.clone(),
                RValue::Local(limit.clone()),
                RValue::Local(step.clone()),
            )
            .into(),
        );
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("body".into())).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

        assert!(matches!(
            lift_attempt_with_ignored_locals(function, &FxHashSet::default()),
            StructureAttempt::Unsafe(UnsafeStructureReason::ForInitSuffixOrder)
        ));
    }

    #[test]
    fn rewrite_while_alias_preserves_post_loop_copy_after_stale_nil_seed() {
        let condition = RcLocal::default();
        let carry = RcLocal::default();
        let current = RcLocal::default();
        let source = RcLocal::default();
        let mut block = Block::from(vec![
            Assign::new(
                vec![LValue::Local(source.clone())],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
            Assign::new(
                vec![LValue::Local(source.clone())],
                vec![RValue::Literal(Literal::Number(42.0))],
            )
            .into(),
            GenericFor::new(
                Vec::new(),
                vec![RValue::Local(condition.clone())],
                Block::default(),
            )
            .into(),
            Assign::new(
                vec![LValue::Local(carry.clone())],
                vec![RValue::Local(source.clone())],
            )
            .into(),
        ]);

        super::rewrite_while_carried_alias(&mut block, &condition, &carry, &current);

        assert_eq!(block.0.len(), 4, "ordinary post-loop copy must not be removed");
        assert!(matches!(block.0[3], Statement::Assign(_)));
        let Statement::Assign(copy) = &block.0[3] else {
            panic!("expected post-loop assignment");
        };
        assert_eq!(copy.left[0].as_local(), Some(&carry));
        assert_eq!(copy.right[0].as_local(), Some(&source));
    }

    #[test]
    fn generic_loop_with_external_body_entry_is_not_discovered() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let init = function.new_block();
        let side_entry = function.new_block();
        let header = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);

        let generator = RcLocal::new(Local::new(Some("generator".into())));
        let state = RcLocal::new(Local::new(Some("state".into())));
        let control = RcLocal::new(Local::new(Some("control".into())));
        let result = RcLocal::new(Local::new(Some("result".into())));
        let origin = ForOrigin {
            prep_pc: 10,
            step_pc: 20,
            body_pc: 21,
            follow_pc: 30,
            prep_kind: ForPrepKind::Generic,
            base_register: 0,
            result_count: 1,
            aux: 1,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
            explicit_nil_args: false,
        };
        let mut for_init = GenericForInit::new(generator.clone(), state.clone(), control.clone());
        for_init.0.right = vec![RValue::Global(Global::from("items"))];
        for_init.1 = Some(origin);
        function.block_mut(init).unwrap().push(for_init.into());
        let mut for_next = GenericForNext::new(vec![result], generator.into(), state, control);
        for_next.origin = Some(origin);
        function.block_mut(header).unwrap().push(for_next.into());
        function
            .block_mut(body)
            .unwrap()
            .push(Statement::Comment(ast::Comment::new("body".into())).into());
        function
            .block_mut(exit)
            .unwrap()
            .push(Statement::Return(Default::default()).into());

        function.set_edges(
            entry,
            vec![
                (init, BlockEdge::new(BranchType::Then)),
                (side_entry, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            init,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            side_entry,
            vec![(body, BlockEdge::new(BranchType::Unconditional))],
        );
        function.set_edges(
            header,
            vec![
                (body, BlockEdge::new(BranchType::Then)),
                (exit, BlockEdge::new(BranchType::Else)),
            ],
        );
        function.set_edges(
            body,
            vec![(header, BlockEdge::new(BranchType::Unconditional))],
        );

        let analysis = Analysis::new(&function).expect("multi-entry CFG remains analyzable");
        assert!(!analysis.loops_by_header.contains_key(&header));
    }
}
