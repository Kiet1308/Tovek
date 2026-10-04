//! A bounded proof-before-materialization pilot. The plan contains CFG node
//! IDs only: a rejected candidate never clones ASTs, locals, or closure owners.
//! Loops, nested branches, edge transfers and scope declarations retain the
//! full region prover until their proof obligations have a plan representation.
use super::*;

const MAX_NODES: usize = 64;

#[derive(Clone, Copy)]
enum Successor { Exit, Next(NodeIndex), Branch(NodeIndex, NodeIndex) }

struct Split {
    node: NodeIndex,
    then_arm: Vec<NodeIndex>,
    else_arm: Vec<NodeIndex>,
}

pub(super) struct RegionPlan<'a> {
    function: &'a Function,
    prefix: Vec<NodeIndex>,
    split: Option<Split>,
    tail: Vec<NodeIndex>,
}

impl<'a> RegionPlan<'a> {
    pub(super) fn prove(function: &'a Function) -> Option<Self> {
        #[cfg(test)]
        if REFERENCE_REGION_PLAN.with(std::cell::Cell::get)
            || REFERENCE_TERMINAL_BLOCK.with(std::cell::Cell::get)
            || REFERENCE_VISITED.with(std::cell::Cell::get)
            || REFERENCE_TERMINATOR_COPY.with(std::cell::Cell::get)
        { return None; }
        if ast::env_flag!("MEDAL_NO_REGION_PLAN") { return None; }
        let count = function.graph().node_count();
        if !(2..=MAX_NODES).contains(&count) { return None; }
        let entry = *function.entry().as_ref()?;
        ast::telemetry::count("restructure_region_plan_candidates", 1);
        if function.iteration_capture_obligations.iter().any(|(local, required)| {
            required.iter().any(|id| !function.iteration_capture_proofs.get(local)
                .is_some_and(|proven| proven.contains(id)))
        }) { return None; }

        let mut successors = FxHashMap::default();
        let mut close_seen = FxHashSet::default();
        let mut control_seen = FxHashSet::default();
        for (node, block) in function.blocks() {
            let edges = function.edges(node).collect_vec();
            if edges.iter().any(|edge| !edge.weight().arguments.is_empty()) { return None; }
            let (successor, prefix_len) = match edges.as_slice() {
                [] => {
                    let terminal = usize::from(matches!(block.last(), Some(Statement::Return(_))));
                    (Successor::Exit, block.len().saturating_sub(terminal))
                }
                [edge] if edge.weight().branch_type == BranchType::Unconditional =>
                    (Successor::Next(edge.target()), block.len()),
                [_, _] => {
                    let condition = block.last()?.as_if()?;
                    if !condition.then_block.lock().is_empty() || !condition.else_block.lock().is_empty() { return None; }
                    let (then_edge, else_edge) = function.conditional_edges(node)?;
                    (Successor::Branch(then_edge.target(), else_edge.target()), block.len() - 1)
                }
                _ => return None,
            };
            // There are no source-scope changes in the pilot. A local prefix
            // inside a newly introduced branch may have uses after the join.
            if block.iter().take(prefix_len).any(|statement|
                !is_linear_statement(statement) || matches!(statement,
                    Statement::Close(_) | Statement::Assign(Assign { prefix: true, .. })))
                || block_contains_close_with_seen(block, &mut close_seen)
                || block_contains_unlowered_control_with_seen(block, &mut control_seen)
            { return None; }
            successors.insert(node, successor);
        }

        let mut prefix = Vec::new();
        let mut owned = FxHashSet::default();
        let mut current = entry;
        let split = loop {
            if !owned.insert(current) { return None; }
            match *successors.get(&current)? {
                Successor::Next(next) => { prefix.push(current); current = next; }
                Successor::Exit => {
                    prefix.push(current);
                    if owned.len() != count { return None; }
                    return Some(Self { function, prefix, split: None, tail: Vec::new() });
                }
                Successor::Branch(then_node, else_node) => break (current, then_node, else_node),
            }
        };
        let then_path = linear_path(split.1, &successors)?;
        let else_path = linear_path(split.2, &successors)?;
        // Two acyclic linear paths can intersect only in an identical suffix.
        // Validate that fact explicitly before assigning a unique source owner.
        let join = then_path.iter().enumerate().find_map(|(then_at, node)|
            else_path.iter().position(|other| node == other).map(|else_at| (then_at, else_at)));
        let (then_end, else_end, tail) = if let Some((then_at, else_at)) = join {
            if then_path[then_at..] != else_path[else_at..] { return None; }
            (then_at, else_at, then_path[then_at..].to_vec())
        } else { (then_path.len(), else_path.len(), Vec::new()) };
        let then_arm = then_path[..then_end].to_vec();
        let else_arm = else_path[..else_end].to_vec();
        for node in then_arm.iter().chain(&else_arm).chain(&tail) {
            if !owned.insert(*node) { return None; }
        }
        if owned.len() != count { return None; }
        Some(Self { function, prefix, split: Some(Split { node: split.0, then_arm, else_arm }), tail })
    }

    pub(super) fn materialize(self) -> Block {
        fn append(function: &Function, nodes: &[NodeIndex], block: &mut Block) {
            for node in nodes { block.extend(function.block(*node).unwrap().iter().cloned()); }
        }
        let mut block = Block::default();
        append(self.function, &self.prefix, &mut block);
        if let Some(split) = self.split {
            let source = self.function.block(split.node).unwrap();
            block.extend(source.iter().take(source.len() - 1).cloned());
            let mut condition = source.last().unwrap().as_if().unwrap().condition.clone();
            let mut then_block = Block::default();
            let mut else_block = Block::default();
            append(self.function, &split.then_arm, &mut then_block);
            append(self.function, &split.else_arm, &mut else_block);
            simplify_conditional(&mut condition, &mut then_block, &mut else_block);
            // Fresh control syntax has unknown origin, as in the full prover;
            // retained expressions keep ordinary clone ancestry and owners.
            block.push(If::new(condition, then_block, else_block).into());
        }
        append(self.function, &self.tail, &mut block);
        ast::telemetry::count("restructure_region_plan_admitted", 1);
        ast::telemetry::count("restructure_region_plan_nodes", self.function.graph().node_count() as u64);
        #[cfg(test)]
        REGION_PLAN_ADMISSIONS.with(|count| count.set(count.get() + 1));
        block
    }
}

fn linear_path(start: NodeIndex, successors: &FxHashMap<NodeIndex, Successor>) -> Option<Vec<NodeIndex>> {
    let mut nodes = Vec::new();
    let mut seen = FxHashSet::default();
    let mut current = start;
    loop {
        if !seen.insert(current) { return None; }
        nodes.push(current);
        match *successors.get(&current)? {
            Successor::Next(next) => current = next,
            Successor::Exit => return Some(nodes),
            Successor::Branch(..) => return None,
        }
    }
}

#[cfg(test)]
thread_local! {
    static REFERENCE_REGION_PLAN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REGION_PLAN_ADMISSIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
