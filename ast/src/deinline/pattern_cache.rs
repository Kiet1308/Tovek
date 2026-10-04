//! Canonical helper syntax within one statement de-inline invocation. No
//! capture/effect/call-graph answer is cached. Canonicalization already owns
//! its syntax; moving it here adds no AST clone or function owner.
use super::*;

pub(super) struct CompiledPattern {
    pub(super) kind: TKind,
    pub(super) falls_off: bool,
    pub(super) pat: Vec<Statement>,
}

/// Return classification already needs canonical syntax for a value helper.
/// Carry its successful allocation into collection; void helpers still wait
/// until after the shape gate before allocating their canonical pattern.
pub(super) struct PreparedPattern {
    pub(super) kind: TKind,
    pub(super) falls_off: bool,
    canonical: Option<Vec<Statement>>,
}

impl PreparedPattern {
    pub(super) fn new(body: &[Statement]) -> Option<Self> {
        let mut has_void = false;
        let mut has_value = false;
        if returns_bad(body, &mut has_void, &mut has_value) { return None; }
        if !has_value {
            return Some(Self { kind: TKind::Void, falls_off: false, canonical: None });
        }
        if !has_void {
            let pattern = compile(body);
            if value_leaf_shape(&pattern) || loop_return_split(&pattern).is_some() {
                return Some(Self { kind: TKind::Value, falls_off: false, canonical: Some(pattern) });
            }
        }
        let pattern = compile(&returning_nil(body));
        (value_leaf_shape(&pattern) || loop_return_split(&pattern).is_some())
            .then_some(Self { kind: TKind::Value, falls_off: true, canonical: Some(pattern) })
    }

    pub(super) fn finish(self, body: &[Statement]) -> CompiledPattern {
        CompiledPattern { kind: self.kind, falls_off: self.falls_off,
            pat: self.canonical.unwrap_or_else(|| compile(body)) }
    }
}

impl From<CompiledPattern> for PreparedPattern {
    fn from(pattern: CompiledPattern) -> Self {
        Self { kind: pattern.kind, falls_off: pattern.falls_off, canonical: Some(pattern.pat) }
    }
}

fn compile(body: &[Statement]) -> Vec<Statement> {
    crate::telemetry::count("canonical_helper_builds", 1);
    canon(body)
}

#[derive(Default)]
pub(super) struct CompiledPatterns {
    patterns: FxHashMap<FnPtr, CompiledPattern>,
    reusable: FxHashMap<FnPtr, bool>,
    #[cfg(test)]
    pub(super) disabled: bool,
    #[cfg(test)]
    pub(super) hits: usize,
}

impl CompiledPatterns {
    /// The structural entry gate and first collection observe one revision.
    pub(super) fn seed(&mut self, function: FnPtr, pattern: CompiledPattern) {
        #[cfg(test)]
        if self.disabled { return; }
        self.patterns.insert(function, pattern);
    }

    pub(super) fn take(&mut self, function: FnPtr) -> Option<CompiledPattern> {
        let pattern = self.patterns.remove(&function)?;
        crate::telemetry::count("canonical_helper_reuses", 1);
        #[cfg(test)]
        { self.hits += 1; }
        Some(pattern)
    }

    pub(super) fn remember(&mut self, function: FnPtr, body: &[Statement], borrowed_lowering: bool) {
        #[cfg(test)]
        if self.disabled { return; }
        let reusable = self.reusable.entry(function).or_insert_with(|| owns_pattern_blocks(body));
        *reusable &= borrowed_lowering;
    }

    pub(super) fn retain_unchanged(
        &mut self,
        patterns: impl IntoIterator<Item = (FnPtr, CompiledPattern)>,
        changed: &FxHashSet<Option<FnPtr>>,
    ) {
        self.patterns.clear();
        #[cfg(test)]
        if self.disabled { return; }
        for (function, pattern) in patterns {
            if self.reusable.get(&function) == Some(&true) && !changed.contains(&Some(function)) {
                self.patterns.insert(function, pattern);
            }
        }
    }
}

/// Progress tracks edits by owning function. Shared child blocks could change
/// through a different function, and nested closures have their own owner, so
/// neither may reuse a parent's canonical plan across iterations. Ordinary
/// de-inline splices introduce calls/values, never new shared statement blocks;
/// this ownership property persists for the rest of the invocation.
fn owns_pattern_blocks(body: &[Statement]) -> bool {
    fn value_owned(value: &RValue) -> bool {
        !matches!(value, RValue::Closure(_)) && value.visit_rvalues(&mut |child| value_owned(child))
    }
    fn block(block: &Arc<Mutex<Block>>) -> bool {
        Arc::strong_count(block) == 1 && owns_pattern_blocks(&block.lock().0)
    }
    body.iter().all(|statement| {
        visit_stmt_rvalues(statement, &mut value_owned) && match statement {
            Statement::If(branch) => block(&branch.then_block) && block(&branch.else_block),
            Statement::While(node) => block(&node.block),
            Statement::Repeat(node) => block(&node.block),
            Statement::NumericFor(node) => block(&node.block),
            Statement::GenericFor(node) => block(&node.block),
            _ => true,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement() -> Statement { Call::new(crate::Global::from("print").into(), vec![Literal::Number(1.0).into()]).into() }
    fn identity(number: usize) -> FnPtr { number as FnPtr }
    fn pattern(body: &[Statement]) -> CompiledPattern { PreparedPattern::new(body).unwrap().finish(body) }

    #[test]
    fn prepared_return_shapes_match_the_original_classifier_and_reuse_its_canonical_allocation() {
        let guard = |then: Vec<Statement>, other: Vec<Statement>| -> Statement {
            If::new(Literal::Boolean(true).into(), Block(then), Block(other)).into()
        };
        let number = || Return::new(vec![Literal::Number(1.0).into()]).into();
        let empty = || Return::new(vec![]).into();
        let bodies = [
            vec![], vec![statement()], vec![empty()], vec![number()],
            vec![statement(), number()], vec![guard(vec![number()], vec![number()])],
            vec![guard(vec![number()], vec![])], vec![guard(vec![number()], vec![empty()])],
            vec![guard(vec![empty()], vec![]), number()],
            vec![Return::new(vec![Literal::Number(1.0).into(), Literal::Number(2.0).into()]).into()],
            vec![Return::new(vec![Call::new(crate::Global::from("f").into(), vec![]).into()]).into()],
        ];
        for body in bodies {
            let legacy = classify_returns(&body);
            let prepared = PreparedPattern::new(&body);
            assert_eq!(prepared.is_some(), legacy.is_some());
            let Some(prepared) = prepared else { continue; };
            let (kind, falls_off) = legacy.unwrap();
            assert!(prepared.kind == kind);
            assert_eq!(prepared.falls_off, falls_off);
            let allocation = prepared.canonical.as_ref().map(Vec::as_ptr);
            let actual = prepared.finish(&body);
            if let Some(allocation) = allocation {
                assert_eq!(actual.pat.as_ptr(), allocation, "classification's canonical Vec moves into the plan");
            }
            let expected = if falls_off { canon(&returning_nil(&body)) } else { canon(&body) };
            assert_eq!(Block(actual.pat).to_string(), Block(expected).to_string());
        }
    }

    #[test]
    fn helper_plans_move_across_unchanged_revisions_and_drop_dirty_bodies() {
        let mut cache = CompiledPatterns::default();
        let body = vec![statement()];
        for function in [identity(1), identity(2)] { cache.remember(function, &body, true); }
        cache.seed(identity(1), pattern(&body));
        let pattern = cache.take(identity(1)).unwrap();
        let allocation = pattern.pat.as_ptr();
        cache.retain_unchanged([(identity(1), pattern), (identity(2), self::pattern(&body))], &FxHashSet::from_iter([Some(identity(2))]));
        assert_eq!(cache.take(identity(1)).unwrap().pat.as_ptr(), allocation, "reuse moves the existing plan");
        assert!(cache.take(identity(2)).is_none(), "an edited helper must be compiled again");
        assert_eq!(cache.hits, 2);
    }

    #[test]
    fn shared_blocks_nested_functions_and_minted_tuple_patterns_are_not_retained() {
        let mut cache = CompiledPatterns::default();
        let shared = Arc::new(Mutex::new(Block(vec![statement()])));
        let body = vec![If { node_origin: Default::default(), condition: Literal::Boolean(true).into(), then_block: shared.clone(),
            else_block: Arc::new(Mutex::new(Block::default())) }.into()];
        cache.remember(identity(1), &body, true);
        let closure = Closure { node_origin: Default::default(), upvalues: vec![],
            function: by_address::ByAddress(Arc::new(Mutex::new(Function::default()))) };
        cache.remember(identity(2), &[Return::new(vec![closure.into()]).into()], true);
        cache.remember(identity(3), &[statement()], false);
        cache.retain_unchanged((1..=3).map(|id| (identity(id), pattern(&[statement()]))), &FxHashSet::default());
        for id in 1..=3 { assert!(cache.take(identity(id)).is_none()); }
        assert_eq!(Arc::strong_count(&shared), 2, "the cache retains no original block owners");
    }

    #[test]
    fn cached_helper_revisions_match_uncached_nested_reconstruction_and_call_events() {
        fn make() -> Block {
            crate::reset_local_ids();
            let local = |name: &str| RcLocal::new(crate::Local::new(Some(name.into())));
            let (first, second, third, argument) = (local("first"), local("second"), local("third"), local("argument"));
            let (p, q, r) = (local("p"), local("q"), local("r"));
            let mark = |name: &str, value: &RcLocal| -> Statement {
                Call::new(crate::Global::from("print").into(), vec![Literal::String(name.as_bytes().to_vec()).into(), value.clone().into()]).into()
            };
            let declare = |binder: &RcLocal, parameter: RcLocal, statements: Vec<Statement>, upvalues| -> Statement {
                let function = Function { parameters: vec![parameter], body: Block(statements), ..Default::default() };
                let closure = Closure { node_origin: Default::default(), upvalues,
                    function: by_address::ByAddress(Arc::new(Mutex::new(function))) };
                let mut declaration = Assign::new(vec![binder.clone().into()], vec![closure.into()]);
                declaration.prefix = true;
                declaration.into()
            };
            let first_body = vec![mark("A", &p), mark("B", &p)];
            let second_body = vec![Call::new(first.clone().into(), vec![q.clone().into()]).into(), mark("C", &q)];
            let third_body = vec![mark("A", &r), mark("B", &r), mark("C", &r), mark("D", &r)];
            Block(vec![
                declare(&first, p, first_body, vec![]),
                declare(&second, q, second_body, vec![Upvalue::Copy(first)]),
                declare(&third, r, third_body, vec![]),
                mark("A", &argument), mark("B", &argument), mark("C", &argument), mark("D", &argument),
                Return::new(vec![argument.into()]).into(),
            ])
        }
        let mut actual = make();
        let report = crate::call_origins::enter(true);
        deinline_with_patterns(&mut actual, CompiledPatterns::default());
        let actual_events = format!("{:?}", report.take_report());
        let mut expected = make();
        let report = crate::call_origins::enter(true);
        deinline_with_patterns(&mut expected, CompiledPatterns { disabled: true, ..Default::default() });
        assert_eq!(actual.to_string(), expected.to_string());
        assert_eq!(actual_events, format!("{:?}", report.take_report()));
        assert!(actual.to_string().contains(CALL_MARKER), "the fixture must exercise actual reconstruction");
    }
}
