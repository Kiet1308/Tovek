//! Dirty blocks within one SSA inliner invocation. A changed global usage
//! count invalidates every block through one revision increment; explicit AST
//! changes invalidate their own block. The ordinary dead/table sweeps still
//! run globally and retain their original termination condition.
use ast::RcLocal;
use petgraph::{stable_graph::NodeIndex, visit::NodeIndexable};

use crate::function::Function;

#[derive(Default, Debug, Clone, Copy)]
pub(super) struct Statistics {
    pub sweeps: u64,
    pub block_visits: u64,
    pub blocks_skipped: u64,
    pub statement_visits: u64,
    pub legacy_statement_visits: u64,
    pub usage_invalidations: u64,
}

pub(super) struct Schedule {
    pub nodes: Vec<NodeIndex>,
    // Zero is explicitly dirty. Other values record the usage revision seen
    // when the block's latest inline scan started.
    last_seen: Vec<u64>,
    usage_revision: u64,
    enabled: bool,
    pub statistics: Statistics,
    changes: super::InlineChanges,
}

impl Schedule {
    pub fn new(function: &Function, enabled: bool) -> Self {
        let nodes = function.graph().node_indices().collect::<Vec<_>>();
        // One block cannot benefit from skipping unaffected blocks.
        let enabled = enabled && nodes.len() > 1;
        Self {
            nodes,
            last_seen: if enabled { vec![0; function.graph().node_bound()] } else { Vec::new() },
            usage_revision: 1,
            enabled,
            statistics: Statistics::default(),
            changes: super::InlineChanges::default(),
        }
    }

    pub fn begin_sweep(&mut self) {
        self.statistics.sweeps += 1;
    }

    pub fn visit(&mut self, node: NodeIndex, statements: usize) -> bool {
        self.statistics.legacy_statement_visits += statements as u64;
        if self.enabled {
            let last_seen = &mut self.last_seen[node.index()];
            if *last_seen == self.usage_revision {
                self.statistics.blocks_skipped += 1;
                return false;
            }
            *last_seen = self.usage_revision;
        }
        self.statistics.block_visits += 1;
        self.statistics.statement_visits += statements as u64;
        true
    }

    pub fn changed(&mut self, node: NodeIndex) {
        self.changes.block_changed(node);
        if self.enabled { self.last_seen[node.index()] = 0; }
    }

    pub fn layout_changed(&mut self, node: NodeIndex) {
        self.changed(node);
        self.changes.statement_layout_changed = true;
    }

    pub fn into_changes(mut self) -> super::InlineChanges {
        self.changes.finish();
        self.changes
    }

    pub fn usage_changed(&mut self, _local: &RcLocal) {
        self.changes.usage_counts_changed = true;
        if !self.enabled { return; }
        self.statistics.usage_invalidations += 1;
        // Later blocks observe this immediately. Earlier blocks are revisited
        // only if an ordinary dead/table change requests another sweep. No
        // dependency census, per-local allocation or O(blocks) fill is needed.
        if let Some(revision) = self.usage_revision.checked_add(1) {
            self.usage_revision = revision;
        } else {
            self.last_seen.fill(0);
            self.usage_revision = 1;
        }
    }

    pub fn record(&self) {
        if ast::telemetry::enabled() {
            for (name, count) in [
                ("ssa_inline_sweeps", self.statistics.sweeps),
                ("ssa_inline_block_visits", self.statistics.block_visits),
                ("ssa_inline_blocks_skipped", self.statistics.blocks_skipped),
                ("ssa_inline_statement_visits", self.statistics.statement_visits),
                ("ssa_inline_legacy_statement_visits", self.statistics.legacy_statement_visits),
                ("ssa_inline_dependency_indexes", 0),
                ("ssa_inline_dependency_occurrences", 0),
                ("ssa_inline_usage_invalidations", self.statistics.usage_invalidations),
            ] {
                ast::telemetry::count(name, count);
            }
        }
    }
}
