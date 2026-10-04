//! Bounded facts for adjacent late AST passes. A session belongs to one tree
//! revision. Untracked edits invalidate it; only explicitly capture-preserving
//! edits or alias substitutions may carry capture facts into the next revision.
//! Numeric identities retain no AST/local owners. These facts prove neither
//! immutability nor motion safety: captured cells still require positional and
//! evaluation-order checks in each consumer.
//! Incomplete or locked trees refuse these optional cleanup passes unchanged;
//! the output may retain extra aliases/nil stores when the work bound is reached.

use rustc_hash::FxHashSet;

use crate::{Block, RValue, RcLocal, Statement, Traverse, Upvalue};

const NODE_LIMIT: usize = 1_000_000;
const BINDING_LIMIT: usize = 100_000;
const DEPTH_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnalysisStats {
    pub capture_builds: usize,
    pub capture_hits: usize,
    pub invalidations: usize,
    pub refusals: usize,
}

/// A bounded description of one mutation batch. Unknown edits always invalidate
/// the session. Captures may remain a conservative superset after deletion.
#[derive(Debug)]
pub struct ChangeSet {
    touched: FxHashSet<u64>,
    capture_preserving: bool,
    complete: bool,
}

impl ChangeSet {
    pub fn capture_preserving() -> Self {
        Self { touched: FxHashSet::default(), capture_preserving: true, complete: true }
    }

    pub fn unknown() -> Self {
        Self { capture_preserving: false, ..Self::capture_preserving() }
    }

    pub fn touch(&mut self, local: &RcLocal) {
        if self.touched.len() < BINDING_LIMIT || self.touched.contains(&local.stable_id()) {
            self.touched.insert(local.stable_id());
        } else { self.complete = false; }
    }
}

#[derive(Default)]
pub struct AnalysisSession {
    revision: u64,
    root: Option<usize>,
    capture_revision: u64,
    captures: Option<FxHashSet<u64>>,
    attempted: bool,
    stats: AnalysisStats,
}

impl AnalysisSession {
    pub fn new() -> Self { Self::default() }

    pub fn revision(&self) -> u64 { self.revision }
    pub fn stats(&self) -> AnalysisStats { self.stats }

    pub(crate) fn refuse(&mut self) { self.stats.refusals += 1; }

    /// Required after any mutation not reported through this session.
    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.captures = None;
        self.attempted = false;
        self.stats.invalidations += 1;
    }

    /// Complete, conservative capture membership at the current revision.
    /// None is unknown, never an empty-capture proof.
    pub fn capture_facts(&mut self, block: &Block) -> Option<&FxHashSet<u64>> {
        let root = block as *const Block as usize;
        if self.root != Some(root) {
            self.invalidate();
            self.root = Some(root);
        }
        if self.attempted && self.capture_revision == self.revision {
            self.stats.capture_hits += 1;
            return self.captures.as_ref();
        }
        self.stats.capture_builds += 1;
        self.capture_revision = self.revision;
        self.attempted = true;
        self.captures = CaptureCensus::collect(block);
        self.captures.as_ref()
    }

    /// Record an edit whose capture contract is known. Callers must invalidate
    /// instead if they add/remove arbitrary closures or change capture bindings.
    pub fn commit(&mut self, block: &Block, changes: ChangeSet) {
        if self.root != Some(block as *const Block as usize)
            || !changes.capture_preserving || !changes.complete {
            self.invalidate();
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        self.capture_revision = self.revision;
    }

    pub(crate) fn take_captures(&mut self, block: &Block) -> Option<(u64, FxHashSet<u64>)> {
        self.capture_facts(block)?;
        Some((self.revision, self.captures.take()?))
    }

    /// An alias rewrite remaps every use/capture and adds the new capture IDs
    /// to this conservative set before publishing. A stale revision is rejected.
    pub(crate) fn publish_captures(&mut self, block: &Block, revision: u64,
        captures: FxHashSet<u64>, changes: ChangeSet)
    {
        if revision != self.revision || captures.len() > BINDING_LIMIT {
            self.invalidate();
            return;
        }
        self.captures = Some(captures);
        self.commit(block, changes);
    }
}

#[derive(Default)]
struct CaptureCensus {
    captures: FxHashSet<u64>,
    functions: FxHashSet<usize>,
    nodes: usize,
}

impl CaptureCensus {
    fn collect(block: &Block) -> Option<FxHashSet<u64>> {
        let mut census = Self::default();
        census.block(block, 0).then_some(census.captures)
    }

    fn room(&mut self, depth: usize) -> bool {
        self.nodes += 1;
        depth <= DEPTH_LIMIT && self.nodes <= NODE_LIMIT && self.captures.len() <= BINDING_LIMIT
    }

    fn value(&mut self, value: &RValue, depth: usize) -> bool {
        if !self.room(depth) { return false; }
        if let RValue::Closure(closure) = value {
            // Every capture site contributes, even when its function body was
            // already inspected at another occurrence.
            for upvalue in &closure.upvalues {
                if !self.room(depth) { return false; }
                let (Upvalue::Copy(local) | Upvalue::Ref(local)) = upvalue;
                self.captures.insert(local.stable_id());
                if self.captures.len() > BINDING_LIMIT { return false; }
            }
            if self.functions.insert(triomphe::Arc::as_ptr(&closure.function.0) as usize) {
                let Some(function) = closure.function.try_lock() else { return false; };
                if !self.block(&function.body, depth + 1) { return false; }
            }
        }
        value.visit_rvalues(&mut |child| self.value(child, depth + 1))
    }

    fn block(&mut self, block: &Block, depth: usize) -> bool {
        if !self.room(depth) { return false; }
        for statement in &block.0 {
            if !self.room(depth) { return false; }
            if !statement.visit_lvalues(&mut |left|
                self.room(depth + 1) && left.visit_rvalues(&mut |value| self.value(value, depth + 1)))
                || !statement.visit_rvalues(&mut |value| self.value(value, depth + 1)) {
                return false;
            }
            let mut child = |block: &triomphe::Arc<parking_lot::Mutex<Block>>| {
                block.try_lock().is_some_and(|body| self.block(&body, depth + 1))
            };
            let complete = match statement {
                Statement::If(node) => child(&node.then_block) && child(&node.else_block),
                Statement::While(node) => child(&node.block),
                Statement::Repeat(node) => child(&node.block),
                Statement::NumericFor(node) => child(&node.block),
                Statement::GenericFor(node) => child(&node.block),
                _ => true,
            };
            if !complete { return false; }
        }
        true
    }
}

/// The no-candidate fast path has the same work/locking bounds as the facts it
/// guards. A probe must not block on a body before the census can refuse it.
/// Function bodies are inspected once; every site's ordinary operands are still
/// visited. These consumers use pure structural predicates.
pub(crate) fn any_statement_bounded(
    block: &Block,
    predicate: &mut impl FnMut(&Statement) -> bool,
) -> Option<bool> {
    struct Probe<'a, F> {
        predicate: &'a mut F,
        functions: FxHashSet<usize>,
        nodes: usize,
        found: bool,
    }
    impl<F: FnMut(&Statement) -> bool> Probe<'_, F> {
        fn room(&mut self, depth: usize) -> bool {
            self.nodes += 1;
            depth <= DEPTH_LIMIT && self.nodes <= NODE_LIMIT
        }

        fn value(&mut self, value: &RValue, depth: usize) -> bool {
            if !self.room(depth) { return false; }
            if let RValue::Closure(closure) = value
                && self.functions.insert(triomphe::Arc::as_ptr(&closure.function.0) as usize) {
                let Some(function) = closure.function.try_lock() else { return false; };
                if !self.block(&function.body, depth + 1) { return false; }
            }
            value.visit_rvalues(&mut |value| self.value(value, depth + 1))
        }

        fn block(&mut self, block: &Block, depth: usize) -> bool {
            if !self.room(depth) { return false; }
            for statement in &block.0 {
                if !self.room(depth) { return false; }
                if (self.predicate)(statement) {
                    self.found = true;
                    return false;
                }
                if !statement.visit_lvalues(&mut |left| self.room(depth + 1)
                    && left.visit_rvalues(&mut |value| self.value(value, depth + 1)))
                    || !statement.visit_rvalues(&mut |value| self.value(value, depth + 1)) {
                    return false;
                }
                let mut child = |block: &triomphe::Arc<parking_lot::Mutex<Block>>| {
                    block.try_lock().is_some_and(|body| self.block(&body, depth + 1))
                };
                let complete = match statement {
                    Statement::If(node) => child(&node.then_block) && child(&node.else_block),
                    Statement::While(node) => child(&node.block),
                    Statement::Repeat(node) => child(&node.block),
                    Statement::NumericFor(node) => child(&node.block),
                    Statement::GenericFor(node) => child(&node.block),
                    _ => true,
                };
                if !complete { return false; }
            }
            true
        }
    }
    let mut probe = Probe { predicate, functions: FxHashSet::default(), nodes: 0, found: false };
    let complete = probe.block(block, 0);
    if probe.found { Some(true) } else { complete.then_some(false) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Call, Closure, Function, Global, Return};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn closure(function: &Arc<Mutex<Function>>, local: &RcLocal) -> RValue {
        Closure { node_origin: Default::default(), function: ByAddress(function.clone()),
            upvalues: vec![Upvalue::Ref(local.clone())] }.into()
    }

    #[test]
    fn shared_body_sites_keep_distinct_captures_without_local_owners() {
        let a = RcLocal::default(); let b = RcLocal::default();
        let body = Arc::new(Mutex::new(Function::default()));
        let block = Block(vec![Call::new(Global::from("consume").into(),
            vec![closure(&body, &a), closure(&body, &b)]).into()]);
        let owners = (Arc::count(&a.0.0), Arc::count(&b.0.0));
        let mut session = AnalysisSession::new();
        let facts = session.capture_facts(&block).unwrap();
        assert!(facts.contains(&a.stable_id()) && facts.contains(&b.stable_id()));
        assert_eq!((Arc::count(&a.0.0), Arc::count(&b.0.0)), owners);
        session.commit(&block, ChangeSet::capture_preserving());
        assert!(session.capture_facts(&block).unwrap().contains(&a.stable_id()));
        assert_eq!(session.stats().capture_builds, 1);
        assert_eq!(session.stats().capture_hits, 1);
    }

    #[test]
    fn unknown_edits_and_stale_publication_cannot_reuse_capture_facts() {
        let a = RcLocal::default(); let b = RcLocal::default();
        let function = Arc::new(Mutex::new(Function::default()));
        let mut block = Block(vec![Return::new(vec![closure(&function, &a)]).into()]);
        let mut session = AnalysisSession::new();
        let (old_revision, old) = session.take_captures(&block).unwrap();
        block[0] = Return::new(vec![closure(&function, &b)]).into();
        session.commit(&block, ChangeSet::unknown());
        session.publish_captures(&block, old_revision, old, ChangeSet::capture_preserving());
        let facts = session.capture_facts(&block).unwrap();
        assert!(facts.contains(&b.stable_id()));
        assert!(!facts.contains(&a.stable_id()));
        assert_eq!(session.stats().capture_builds, 2);
    }

    #[test]
    fn locked_body_is_unknown_not_uncaptured() {
        let local = RcLocal::default();
        let function = Arc::new(Mutex::new(Function::default()));
        let block = Block(vec![Return::new(vec![closure(&function, &local)]).into()]);
        let _guard = function.lock();
        assert!(AnalysisSession::new().capture_facts(&block).is_none());
    }

    #[test]
    fn locked_bodies_refuse_cleanup_consumers_without_mutation() {
        use crate::{Assign, Literal, Local};
        // Exercise both the bounded candidate probe and the capture census:
        // the inaccessible body can appear before or after an obvious candidate.
        for function_first in [false, true] {
            let source = RcLocal::new(Local::new(Some("source".into())));
            let alias = RcLocal::new(Local::new(Some("v1".into())));
            let mut declaration = Assign::new(vec![source.clone().into()], vec![]);
            declaration.prefix = true;
            let redundant_nil = Assign::new(vec![source.clone().into()], vec![Literal::Nil.into()]);
            let mut copy = Assign::new(vec![alias.clone().into()], vec![source.into()]);
            copy.prefix = true;
            let function = Arc::new(Mutex::new(Function::default()));
            let mut block = Block(vec![declaration.into(), redundant_nil.into(), copy.into()]);
            let closure_site = Call::new(Global::from("consume").into(), vec![closure(&function, &alias)]).into();
            let insertion = if function_first { 0 } else { block.len() };
            block.insert(insertion, closure_site);
            let before = block.to_string();
            let guard = function.lock();
            let mut session = AnalysisSession::new();
            crate::copy_cleanup::copy_cleanup_with_analysis(&mut block, &mut session);
            crate::eliminate_nil::eliminate_redundant_nil_with_analysis(&mut block, &mut session);
            assert_eq!(session.stats().refusals, 2);
            assert_eq!(session.stats().capture_builds, usize::from(!function_first));
            drop(guard);
            assert_eq!(block.to_string(), before);
        }
    }

    #[test]
    fn overdeep_facts_refuse_cleanup_consumers_without_mutation() {
        use crate::{Assign, If, Literal, Local};
        let source = RcLocal::new(Local::new(Some("source".into())));
        let alias = RcLocal::new(Local::new(Some("v1".into())));
        let mut declaration = Assign::new(vec![source.clone().into()], vec![]);
        declaration.prefix = true;
        let redundant_nil = Assign::new(vec![source.clone().into()], vec![Literal::Nil.into()]);
        let mut copy = Assign::new(vec![alias.into()], vec![source.into()]);
        copy.prefix = true;
        let mut nested = Block::default();
        for _ in 0..=DEPTH_LIMIT {
            nested = Block(vec![If::new(Literal::Boolean(true).into(), nested, Block::default()).into()]);
        }
        let mut block = Block(vec![declaration.into(), redundant_nil.into(), copy.into()]);
        block.0.append(&mut nested.0);
        let mut session = AnalysisSession::new();
        crate::copy_cleanup::copy_cleanup_with_analysis(&mut block, &mut session);
        crate::eliminate_nil::eliminate_redundant_nil_with_analysis(&mut block, &mut session);
        assert_eq!(block.len(), 4);
        assert!(matches!(&block[1], Statement::Assign(assign) if !assign.prefix));
        assert!(matches!(&block[2], Statement::Assign(assign) if assign.prefix));
        assert_eq!(session.stats().capture_builds, 1);
        assert_eq!(session.stats().refusals, 2);
    }

    #[test]
    fn copy_then_nil_reuses_one_census_and_publishes_new_capture_membership() {
        use crate::{Assign, LValue, Literal, Local};
        fn fixture() -> Block {
            let source = RcLocal::new(Local::new(Some("source".into())));
            let alias = RcLocal::new(Local::new(Some("v1".into())));
            let free = RcLocal::new(Local::new(Some("free".into())));
            let declare = |local: &RcLocal| {
                let mut assign = Assign::new(vec![LValue::Local(local.clone())], vec![Literal::Nil.into()]);
                assign.prefix = true; Statement::from(assign)
            };
            let mut copy = Assign::new(vec![alias.clone().into()], vec![source.clone().into()]);
            copy.prefix = true;
            let function = Arc::new(Mutex::new(Function { body: Block(vec![Return::new(vec![alias.clone().into()]).into()]), ..Default::default() }));
            Block(vec![declare(&source),
                Assign::new(vec![source.into()], vec![Literal::Nil.into()]).into(), copy.into(),
                Call::new(Global::from("consume").into(), vec![closure(&function, &alias)]).into(),
                declare(&free), Assign::new(vec![free.clone().into()], vec![Literal::Nil.into()]).into(),
                Return::new(vec![free.into()]).into()])
        }
        let mut expected = fixture(); let mut actual = fixture();
        crate::copy_cleanup::copy_cleanup(&mut expected);
        crate::eliminate_nil::eliminate_redundant_nil(&mut expected);
        let mut session = AnalysisSession::new();
        crate::copy_cleanup::copy_cleanup_with_analysis(&mut actual, &mut session);
        let fresh: FxHashSet<_> = crate::inline_temps::collect_usage(&actual).into_iter()
            .filter_map(|(local, usage)| usage.captured.then(|| local.stable_id())).collect();
        assert!(fresh.is_subset(session.capture_facts(&actual).unwrap()));
        crate::eliminate_nil::eliminate_redundant_nil_with_analysis(&mut actual, &mut session);
        assert_eq!(actual.to_string(), expected.to_string());
        assert_eq!(session.stats().capture_builds, 1);
        assert!(session.stats().capture_hits >= 1);
        assert!(actual.to_string().contains("source = nil"));
        assert!(!actual.to_string().contains("v1"));
    }
}
