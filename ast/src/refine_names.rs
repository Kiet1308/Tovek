//! Bounded role inference on the final binding graph. This pass only changes
//! Local.0: no expression, identity, capture mode, field, global or type changes.
//! Candidate priorities rank evidence; they are not probabilities or effect facts.
use std::{borrow::Cow, collections::{BTreeMap, BTreeSet}, fmt::Write};

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{Block, Call, Function, LValue, Literal, RValue, RcLocal, Select, Statement, Traverse, Upvalue};

#[derive(Clone, Copy)]
pub struct Options {
    pub dont_reuse_var: bool,
    pub emit_report: bool,
    pub node_budget: usize,
    pub binding_budget: usize,
    pub depth_budget: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dont_reuse_var: false,
            emit_report: false,
            node_budget: 100_000,
            binding_budget: 50_000,
            depth_budget: 256,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub priority: u8,
    pub reason: &'static str,
    pub witness: String,
    pub from_binding: Option<u64>,
}

#[derive(Debug)]
pub struct BindingReport {
    pub id: u64,
    pub before: String,
    pub after: String,
    pub kind: &'static str,
    pub scope: Option<usize>,
    pub status: &'static str,
    pub candidates: Vec<Candidate>,
    pub type_evidence: Vec<TypeEvidence>,
}

#[derive(Debug, Clone)]
pub struct TypeEvidence {
    pub representation: String,
    pub origin: &'static str,
}

#[derive(Debug, Default)]
pub struct Report {
    pub visited_nodes: usize,
    pub binding_count: usize,
    pub scope_count: usize,
    pub renamed: usize,
    pub conflicts: usize,
    pub unresolved_calls: usize,
    pub refused_edges: usize,
    pub budget_exhausted: bool,
    pub bindings: Vec<BindingReport>,
}

struct Node {
    local: RcLocal,
    before: String,
    kind: &'static str,
    scope: Option<usize>,
    ambiguous_owner: bool,
    writes: usize,
    reads: usize,
    ref_capture: bool,
    source_protected: bool,
    key_use: bool,
    module_leaf: Option<String>,
    candidates: Vec<Candidate>,
    type_evidence: Vec<TypeEvidence>,
    overflow: bool,
    changed_round: u8,
    /// The byte length of the base a namer counted from to spell `before`, and
    /// its counter (`part` and 2 for `part2`; `v` and 7 for `v7`): what suffix
    /// compaction may return to.
    stem: Option<(usize, usize)>,
    /// The spelling compaction tries before counting from the base: the
    /// private field the base was read from (`_light` for `light2` of
    /// `self._light`). Never for a recorded source name.
    alternative: Option<String>,
    /// Clock at which the binding becomes visible: the end of its declaring
    /// statement, or the start of a parameter's or loop variable's body.
    visible_from: Option<u32>,
    /// `[visible_from, exit of its scope]` on the graph clock (`Graph::tick`).
    region: Option<(u32, u32)>,
    /// The clock of every reference (read, write or capture) and of the
    /// declaration itself, in order.
    references: Vec<u32>,
    /// Where the binding was first met in the walk (its index in `order`).
    position: usize,
}

enum ReturnRole {
    Binding(u64),
    Field(String),
}

struct FunctionRoles {
    parameters: Vec<u64>,
    // Only a straight-line body with a fixed tuple of scalar local/field
    // reads. Field names describe result roles, not effect-free value aliases.
    // Never a guessed open pack, implicit nil, branch or nested-function return.
    returns: Option<Vec<ReturnRole>>,
    variadic: bool,
}

struct ResultUse {
    callee: Option<u64>,
    arguments: Option<usize>,
    destinations: Vec<Option<u64>>,
}

struct Graph {
    options: Options,
    nodes: BTreeMap<u64, Node>,
    order: Vec<u64>,
    scopes: Vec<Option<usize>>,
    /// One preorder clock over statements, references and scope exits. Two
    /// statements of one scope are ordered, and a binding's region is never
    /// empty (it holds at least its scope's exit).
    clock: u32,
    /// The bindings each scope declares, closed into regions at its exit.
    scope_declarations: Vec<Vec<u64>>,
    /// Globals the output prints without a node for them (`table.pack` of an
    /// open SETLIST, `vector.create` of a vector literal, ...): no binding may
    /// take these names anywhere.
    globals: BTreeSet<String>,
    /// The clocks at which the tree reads or writes each global.
    global_references: FxHashMap<String, Vec<u32>>,
    copies: Vec<(u64, u64)>,
    functions: BTreeMap<u64, FunctionRoles>,
    results: Vec<ResultUse>,
    reads: Vec<(u64, String)>,
    joins: Vec<(u64, u64, u64)>,
    calls: Vec<(Option<u64>, Vec<Option<u64>>)>,
    /// `local value = use(state)`: (value, state).
    state_reads: Vec<(u64, u64)>,
    /// Locals declared as a table constructor.
    tables: BTreeSet<u64>,
    /// The locals `merge(table, props)` calls start from.
    merge_bases: Vec<u64>,
    /// `local a, b = helper(...)` calls the de-inliner rebuilt: (helper,
    /// result locals).
    rebuilt_results: Vec<(u64, Vec<Option<u64>>)>,
    /// The function each local is declared as, read only for the helpers of
    /// `rebuilt_results`.
    closures: BTreeMap<u64, Arc<Mutex<Function>>>,
    report: Report,
}

// Explanations never participate in candidate ranking, deduplication or budgets.
// Source-only callers still collect all naming evidence, but need no report text.
fn report_witness(emit: bool, build: impl FnOnce() -> String) -> String {
    if emit { build() } else { String::new() }
}

fn generated(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('p' | 'v')) && chars.all(|c| c.is_ascii_digit())
}

/// `v7` counts from `v`, `p3` from `p`.
fn generated_stem(name: &str) -> Option<(usize, usize)> {
    let stem = name.get(..1).filter(|_| generated(name))?;
    Some((1, crate::name_spelling::suffix_of(name, stem)?))
}

/// A local named only after the Fusion constructor that made it (`computed`
/// for `scope:Computed(..)`, `value` for `scope:Value(..)`): strong role
/// evidence, such as the property it is bound to, names it better.
fn weak(node: &Node) -> bool {
    node.kind == "local" && matches!(node.before.trim_end_matches(|c: char| c.is_ascii_digit()), "computed" | "value")
}

/// The name `node` carries before any counter a namer added (`part` for
/// `part2`): what its role is when another binding borrows it.
fn base_name(node: &Node) -> &str {
    node.stem.map_or(&node.before, |(stem, _)| &node.before[..stem])
}

/// The lowest priority that renames `node`'s current name.
fn rename_floor(node: &Node) -> u8 {
    if weak(node) { 80 } else { 40 }
}

fn useful(name: &str) -> bool {
    name.len() <= 64
        && crate::valid_source_name(name)
        && name != "self"
        && name != "_"
        && !generated(name)
}

fn string(value: &RValue) -> Option<&str> {
    match value {
        RValue::Literal(Literal::String(bytes)) => std::str::from_utf8(bytes).ok(),
        _ => None,
    }
}

fn field_role(key: &str) -> Option<String> {
    // Internal warning/constant keys with shouting underscore-separated words
    // are poor parameter roles (and lowercasing only their first letter yields
    // names such as eXTREMELY_DANGEROUS_usedAsValue). Keep the prior name instead.
    // Ordinary private fields such as `_scope` still carry useful role evidence.
    if key.contains('_')
        && key.split('_').any(|word| {
            word.len() > 1
                && word
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
    {
        return None;
    }
    crate::name_locals::param_name_from_field_key(key)
}

/// `FrameClock()` builds a `frameClock` (`name_locals::type_call_name`).
fn type_call_role(value: &RValue) -> Option<String> {
    let RValue::Local(callee) = as_call(value)?.value.as_ref() else { return None; };
    let name = callee.0.lock().0.clone()?;
    crate::name_locals::type_call_name(&name)
}

/// `Utils.merge` or a local `merge`: a call merging tables into a new one.
fn is_merge(callee: &RValue) -> bool {
    match callee {
        RValue::Index(index) => matches!(string(&index.right), Some("merge" | "Merge")),
        RValue::Local(local) => local.0.lock().0.as_deref() == Some("merge"),
        _ => false,
    }
}

/// The state a Fusion read `use(state)` (or `use(state) or default`) reads.
fn state_read_source(value: &RValue) -> Option<u64> {
    let value = match value {
        RValue::Binary(binary) if binary.operation == crate::BinaryOperation::Or => &*binary.left,
        value => value,
    };
    let call = as_call(value)?;
    let [RValue::Local(state)] = call.arguments.as_slice() else { return None; };
    let reader = match call.value.as_ref() {
        RValue::Local(callee) => callee.0.lock().0.clone()?,
        RValue::Global(global) => String::from_utf8_lossy(&global.0).into_owned(),
        RValue::Index(index) => string(&index.right)?.to_owned(),
        _ => return None,
    };
    crate::name_locals::is_state_reader(&reader).then(|| state.stable_id())
}

fn local_id(value: &RValue) -> Option<u64> {
    value.as_local().map(RcLocal::stable_id)
}

/// Describe a retained arithmetic snapshot after motion has finished. These
/// names describe syntax, not recovered source names or runtime numeric facts.
/// Keep this deliberately small: generic `sum/product/value` labels obscure
/// dependencies, while midpoint and half-of-a-named-quantity carry a role.
fn arithmetic_snapshot_role(value: &RValue) -> Option<String> {
    let RValue::Binary(binary) = value else { return None; };
    if binary.operation != crate::BinaryOperation::Div
        || !matches!(&*binary.right, RValue::Literal(Literal::Number(n)) if *n == 2.0) { return None; }
    if let RValue::Binary(sum) = &*binary.left && sum.operation == crate::BinaryOperation::Add {
        let axis = |value: &RValue| -> Option<char> {
            let name = match value {
                RValue::Local(local) => local.0.lock().0.clone(),
                RValue::Index(index) => string(&index.right).map(str::to_owned),
                _ => None,
            }?;
            let mut chars = name.chars(); let first = chars.next()?;
            (matches!(first, 'X' | 'Y' | 'Z' | 'x' | 'y' | 'z') && chars.all(|c| c.is_ascii_digit()))
                .then_some(first.to_ascii_uppercase())
        };
        if let Some(first) = axis(&sum.left) && axis(&sum.right) == Some(first) {
            return Some(format!("midpoint{first}"));
        }
        return Some("midpoint".into());
    }
    let subject = match &*binary.left {
        RValue::Local(local) => local.0.lock().0.clone(),
        RValue::Index(index) => string(&index.right).and_then(field_role),
        _ => None,
    }?;
    if !useful(&subject) || subject.len() > 32 { return None; }
    let stem = subject.trim_end_matches(|c: char| c.is_ascii_digit());
    if matches!(stem, "vector" | "number" | "value" | "data" | "object" | "item" | "result") { return None; }
    let mut chars = subject.chars();
    Some(format!("half{}{}", chars.next()?.to_ascii_uppercase(), chars.as_str()))
}

fn as_call(value: &RValue) -> Option<&Call> {
    match value {
        RValue::Call(c) | RValue::Select(Select::Call(c)) => Some(c),
        _ => None,
    }
}

// Only a syntactically static script path. This is naming context, not module
// resolution or a claim that require is pure or that exports have known types.
fn module_leaf(value: &RValue) -> Option<String> {
    let call = as_call(value)?;
    if !matches!(&*call.value, RValue::Global(g) if g.0 == b"require") || call.arguments.len() != 1
    {
        return None;
    }
    let mut current = &call.arguments[0];
    let mut leaf = None;
    for _ in 0..64 {
        match current {
            RValue::Index(index) => {
                let key = string(&index.right)?;
                if !useful(key) {
                    return None;
                }
                if leaf.is_none() {
                    leaf = Some(key.to_string());
                }
                current = &index.left;
            }
            RValue::Global(g) if g.0 == b"script" => {
                return leaf.filter(|name| {
                    name.as_bytes()[0].is_ascii_uppercase() && name != "Parent" && name != "Init"
                })
            }
            _ => return None,
        }
    }
    None
}

// Assertion messages must name exactly one identifier, and the guard must
// refer to exactly one local through supported expressions. No free-text guess.
fn assertion_name(message: &str) -> Option<&str> {
    let parts: Vec<_> = message.split('`').take(4).collect();
    (parts.len() == 3 && useful(parts[1])).then(|| parts[1])
}

fn guard_local(value: &RValue, depth: usize) -> Option<u64> {
    fn collect(value: &RValue, depth: usize, ids: &mut BTreeSet<u64>) -> bool {
        if depth == 0 {
            return false;
        }
        match value {
            RValue::Local(local) => {
                ids.insert(local.stable_id());
                true
            }
            RValue::Literal(_) => true,
            RValue::Unary(_) | RValue::Binary(_) => value
                .rvalues()
                .into_iter()
                .all(|v| collect(v, depth - 1, ids)),
            RValue::Call(c) | RValue::Select(Select::Call(c))
                if matches!(&*c.value, RValue::Global(g) if g.0 == b"typeof" || g.0 == b"type")
                    && c.arguments.len() == 1 =>
            {
                collect(&c.arguments[0], depth - 1, ids)
            }
            _ => false,
        }
    }
    let mut ids = BTreeSet::new();
    (collect(value, depth.min(64), &mut ids) && ids.len() == 1).then(|| *ids.first().unwrap())
}

impl Graph {
    fn new(options: Options) -> Self {
        Self {
            options, nodes: BTreeMap::new(), order: Vec::new(), scopes: vec![None],
            clock: 0, scope_declarations: vec![Vec::new()], global_references: FxHashMap::default(),
            globals: BTreeSet::new(), copies: Vec::new(), functions: BTreeMap::new(),
            results: Vec::new(), reads: Vec::new(), joins: Vec::new(), calls: Vec::new(),
            state_reads: Vec::new(), tables: BTreeSet::new(), merge_bases: Vec::new(),
            rebuilt_results: Vec::new(), closures: BTreeMap::new(),
            report: Report::default(),
        }
    }

    fn visit(&mut self, depth: usize) -> bool {
        if self.report.budget_exhausted {
            return false;
        }
        if depth > self.options.depth_budget
            || self.report.visited_nodes >= self.options.node_budget
        {
            self.report.budget_exhausted = true;
            return false;
        }
        self.report.visited_nodes += 1;
        true
    }

    fn node(&mut self, local: &RcLocal) -> Option<&mut Node> {
        let id = local.stable_id();
        if !self.nodes.contains_key(&id) {
            if self.nodes.len() >= self.options.binding_budget {
                self.report.budget_exhausted = true;
                return None;
            }
            let data = local.0.lock();
            let before = data.0.clone().unwrap_or_default();
            let source_protected = !data.2.is_empty();
            // Only a counter some pass appended (and recorded) is one: in
            // `wheel_2` for the child `Wheel_2` the number is evidence. A
            // recorded source name only ever compacts back to itself.
            let stem = data
                .namer_stem()
                .map(|(stem, counter)| (stem.len(), counter))
                .or_else(|| generated_stem(&before))
                .filter(|&(stem, _)| !source_protected || data.source_name() == Some(&before[..stem]));
            let alternative = data
                .namer_stem()
                .filter(|_| data.4.private_stem && !source_protected)
                .map(|(stem, _)| format!("_{stem}"));
            let mut candidates = vec![Candidate {
                name: before.clone(),
                priority: 20,
                reason: "prior_deterministic_namer",
                witness: report_witness(self.options.emit_report, || "selected legacy name; legacy alternatives are not collected here".into()),
                from_binding: None,
            }];
            for source in &data.2 {
                candidates.push(Candidate {
                    name: source.name.clone(),
                    priority: 255,
                    reason: "recorded_source_binding",
                    witness: report_witness(self.options.emit_report, || format!("{:?}", source.origin)),
                    from_binding: None,
                });
            }
            if data.4.conditional_result && !data.4.parameter && generated(&before) {
                candidates.push(Candidate {
                    name: "selected".into(), priority: 40,
                    reason: "preserved_conditional_result",
                    witness: report_witness(self.options.emit_report, || "private two-arm SSA join; returned and observed separately; no source spelling claim".into()),
                    from_binding: None,
                });
            }
            let type_evidence = data.1.as_ref().filter(|_| self.options.emit_report).map(|hint| TypeEvidence {
                representation: hint.clone(), origin: "recorded_bytecode_local_type_naming_hint",
            }).into_iter().collect();
            drop(data);
            self.nodes.insert(
                id,
                Node {
                    local: local.clone(),
                    before,
                    kind: "external",
                    scope: None,
                    ambiguous_owner: false,
                    writes: 0,
                    reads: 0,
                    ref_capture: false,
                    source_protected,
                    key_use: false,
                    module_leaf: None,
                    candidates,
                    type_evidence,
                    overflow: false,
                    changed_round: 0,
                    stem,
                    alternative,
                    visible_from: None,
                    region: None,
                    references: Vec::new(),
                    position: self.order.len(),
                },
            );
            self.order.push(id);
        }
        self.nodes.get_mut(&id)
    }

    fn declare(&mut self, local: &RcLocal, scope: usize, kind: &'static str) {
        if let Some(node) = self.node(local) {
            if node.scope.is_some() {
                node.ambiguous_owner = true;
            }
            node.scope = Some(scope);
            node.kind = kind;
            self.scope_declarations[scope].push(local.stable_id());
        }
    }

    fn tick(&mut self) -> u32 {
        self.clock += 1;
        self.clock
    }

    /// A read, write or capture of `local` at the current point.
    fn reference(&mut self, local: &RcLocal) -> Option<&mut Node> {
        let at = self.tick();
        let node = self.node(local)?;
        node.references.push(at);
        Some(node)
    }

    fn global_reference(&mut self, name: &[u8]) {
        let at = self.tick();
        // A name no identifier spells prints as `getfenv(1)["name"]`.
        if !crate::formatter::Formatter::<std::fmt::Formatter>::is_valid_name(name) {
            self.globals.insert("getfenv".into());
        }
        let name = String::from_utf8_lossy(name);
        match self.global_references.get_mut(name.as_ref()) {
            Some(clocks) => clocks.push(at),
            None => {
                self.global_references.insert(name.into_owned(), vec![at]);
            }
        }
    }

    /// The bindings `locals` become visible at `at`. The declaration counts
    /// as a reference too, so two bindings of one name never overlap, even
    /// unread: no output shadows a binding.
    fn make_visible<'b>(&mut self, locals: impl IntoIterator<Item = &'b RcLocal>, at: u32) {
        for local in locals {
            if let Some(node) = self.nodes.get_mut(&local.stable_id())
                && node.visible_from.is_none()
            {
                node.visible_from = Some(at);
                // A local function's body references it before its statement ends.
                let position = node.references.partition_point(|&earlier| earlier < at);
                node.references.insert(position, at);
            }
        }
    }

    /// Close the regions of the bindings `scope` declared.
    fn exit_scope(&mut self, scope: usize) {
        let exit = self.tick();
        for id in std::mem::take(&mut self.scope_declarations[scope]) {
            if let Some(node) = self.nodes.get_mut(&id)
                && node.scope == Some(scope)
                && let Some(start) = node.visible_from
            {
                node.region = Some((start, exit));
            }
        }
    }

    fn candidate(&mut self, id: u64, candidate: Candidate) -> bool {
        if !useful(&candidate.name) {
            return false;
        }
        let Some(node) = self.nodes.get_mut(&id) else {
            return false;
        };
        if let Some(existing) = node.candidates.iter_mut().find(|c| {
            c.name == candidate.name
                && c.reason == candidate.reason
                && c.from_binding == candidate.from_binding
        }) {
            if candidate.priority > existing.priority {
                *existing = candidate;
                return true;
            }
        } else if node.candidates.len() < 24 {
            node.candidates.push(candidate);
            return true;
        } else {
            node.overflow = true;
        }
        false
    }

    fn role(&mut self, key: &RValue, value: &RValue) {
        let (Some(key), RValue::Local(local)) = (string(key), value) else {
            return;
        };
        let Some(name) = field_role(key) else {
            return;
        };
        self.node(local);
        self.candidate(
            local.stable_id(),
            Candidate {
                name,
                priority: 85,
                reason: "record_field_value",
                witness: report_witness(self.options.emit_report, || key.into()),
                from_binding: None,
            },
        );
    }

    fn key(&mut self, value: &RValue) {
        if let RValue::Local(local) = value
            && let Some(node) = self.node(local)
        {
            node.key_use = true;
        }
    }

    fn call(&mut self, call: &Call) {
        if call.arguments.len() >= 2
            && let RValue::Local(base) = &call.arguments[0]
            && is_merge(&call.value)
        {
            self.merge_bases.push(base.stable_id());
        }
        self.calls.push((
            local_id(&call.value),
            call.arguments.iter().map(local_id).collect(),
        ));
        if matches!(&*call.value, RValue::Global(g) if g.0 == b"assert")
            && call.arguments.len() == 2
            && let Some(message) = string(&call.arguments[1])
            && let Some(name) = assertion_name(message)
            && let Some(id) = guard_local(&call.arguments[0], self.options.depth_budget)
            && self
                .nodes
                .get(&id)
                .is_some_and(|node| node.kind == "parameter")
        {
            self.candidate(
                id,
                Candidate {
                    name: name.into(),
                    priority: 95,
                    reason: "assertion_parameter",
                    witness: report_witness(self.options.emit_report, || format!("assert guard with one binding; message identifier `{name}`")),
                    from_binding: None,
                },
            );
        }
    }

    fn expression(&mut self, value: &RValue, scope: usize, depth: usize) {
        if !self.visit(depth) {
            return;
        }
        match value {
            RValue::Local(local) => {
                if let Some(node) = self.reference(local) {
                    node.reads += 1;
                }
            }
            RValue::Global(global) => self.global_reference(&global.0),
            // Vector literals print through the `vector` library.
            RValue::Literal(Literal::Vector(..) | Literal::VectorD(..)) => {
                self.globals.insert("vector".into());
            }
            RValue::Closure(closure) => {
                for upvalue in &closure.upvalues {
                    let (Upvalue::Copy(local) | Upvalue::Ref(local)) = upvalue;
                    if let Some(node) = self.reference(local) {
                        node.ref_capture |= matches!(upvalue, Upvalue::Ref(_));
                    }
                }
                let child = self.child_scope(scope);
                let function = closure.function.lock();
                for (index, parameter) in function.parameters.iter().enumerate() {
                    self.declare(parameter, child, "parameter");
                    if self.options.emit_report && let Some(node) = self.nodes.get_mut(&parameter.stable_id()) {
                        if let Some(annotation) = function.parameter_annotations.get(index).and_then(Option::as_ref) {
                            node.type_evidence.push(TypeEvidence { representation: annotation.clone(),
                                origin: "recorded_bytecode_parameter_annotation" });
                        }
                        if let Some(hint) = function.parameter_name_hints.get(index).and_then(Option::as_ref) {
                            node.type_evidence.push(TypeEvidence { representation: hint.clone(),
                                origin: "recorded_bytecode_parameter_type_naming_hint" });
                        }
                    }
                }
                let body = self.tick();
                self.make_visible(&function.parameters, body);
                self.block(&function.body, child, depth + 1);
                self.exit_scope(child);
                return;
            }
            RValue::Table(table) => {
                for (key, value) in &table.0 {
                    if let Some(key) = key {
                        self.key(key);
                        self.role(key, value);
                    }
                }
            }
            RValue::Index(index) => self.key(&index.right),
            RValue::Call(call) | RValue::Select(Select::Call(call)) => self.call(call),
            RValue::MethodCall(_) | RValue::Select(Select::MethodCall(_)) => {
                self.report.unresolved_calls += 1
            }
            _ => {}
        }
        value.visit_rvalues(&mut |child| {
            self.expression(child, scope, depth + 1);
            true
        });
    }

    fn child_scope(&mut self, parent: usize) -> usize {
        let child = self.scopes.len();
        self.scopes.push(Some(parent));
        self.scope_declarations.push(Vec::new());
        child
    }

    fn child_block(&mut self, block: &Block, parent: usize, depth: usize) {
        let child = self.child_scope(parent);
        self.block(block, child, depth + 1);
        self.exit_scope(child);
    }

    fn block(&mut self, block: &Block, scope: usize, depth: usize) {
        for (index, statement) in block.iter().enumerate() {
            if !self.visit(depth) {
                return;
            }
            let start = self.tick();
            if let Statement::If(branch) = statement {
                let arm = |body: &Block| -> Option<(u64, u64)> {
                    if body.len() != 1 { return None; }
                    let Statement::Assign(a) = &body[0] else { return None; };
                    if a.prefix || a.left.len() != 1 || a.right.len() != 1 { return None; }
                    Some((a.left[0].as_local()?.stable_id(), local_id(&a.right[0])?))
                };
                let (a, b) = (arm(&branch.then_block.lock()), arm(&branch.else_block.lock()));
                if let (Some((dest, left)), Some((other, right))) = (a, b) {
                    if dest == other && index > 0 {
                        if let Statement::Assign(declaration) = &block[index - 1] {
                            if declaration.prefix && declaration.left.len() == 1
                                && declaration.left[0].as_local().is_some_and(|l| l.stable_id() == dest)
                                && (declaration.right.is_empty() || matches!(declaration.right.as_slice(), [RValue::Literal(Literal::Nil)])) {
                                self.joins.push((dest, left, right));
                            }
                        }
                    }
                }
            }
            match statement {
                Statement::Assign(assign) => {
                    for left in &assign.left {
                        match left {
                            LValue::Local(local) => {
                                let node = if assign.prefix {
                                    self.declare(local, scope, "local");
                                    self.node(local)
                                } else {
                                    self.reference(local)
                                };
                                if let Some(node) = node {
                                    node.writes += 1;
                                }
                            }
                            LValue::Global(global) => self.global_reference(&global.0),
                            LValue::Index(index) => {
                                self.key(&index.right);
                                self.expression(&index.left, scope, depth + 1);
                                self.expression(&index.right, scope, depth + 1);
                            }
                        }
                    }
                    if assign.right.len() == 1 {
                        if let Some(call) = as_call(&assign.right[0]) {
                            // In assignment position the lifter also uses
                            // Select::Call for a fixed multi-result CALL. The
                            // formatter emits it bare; the LHS fixes its arity.
                            self.results.push(ResultUse { callee: local_id(&call.value),
                                arguments: call.arguments.last().is_none_or(|arg| !matches!(arg, RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_))).then_some(call.arguments.len()),
                                destinations: assign.left.iter().map(|v| v.as_local().map(RcLocal::stable_id)).collect() });
                            if call.rebuilt.is_some() && assign.prefix && let Some(callee) = local_id(&call.value) {
                                self.rebuilt_results.push((callee, assign.left.iter().map(|v| v.as_local().map(RcLocal::stable_id)).collect()));
                            }
                        }
                    }
                    for (left, right) in assign.left.iter().zip(&assign.right) {
                        match left {
                            LValue::Index(index) => self.role(&index.right, right),
                            LValue::Local(local) => {
                                if let RValue::Local(source) = right {
                                    self.copies.push((local.stable_id(), source.stable_id()));
                                }
                                if assign.prefix && !assign.parallel && assign.left.len() == 1 && assign.right.len() == 1
                                    && let Some(name) = arithmetic_snapshot_role(right)
                                {
                                    self.candidate(local.stable_id(), Candidate {
                                        name, priority: 40, reason: "retained_arithmetic_snapshot",
                                        witness: report_witness(self.options.emit_report, || "final expression shape; naming context only, not a numeric or motion proof".into()),
                                        from_binding: None,
                                    });
                                }
                                if assign.prefix && let RValue::Table(_) = right {
                                    self.tables.insert(local.stable_id());
                                }
                                if assign.prefix && assign.left.len() == 1
                                    && let RValue::Table(table) = right
                                    && let Some(name) = crate::name_locals::module_table_hint(table)
                                {
                                    self.candidate(local.stable_id(), Candidate {
                                        name, priority: 45, reason: "module_table",
                                        witness: report_witness(self.options.emit_report, || "table of required modules; role only".into()),
                                        from_binding: None,
                                    });
                                }
                                if assign.prefix && assign.left.len() == 1
                                    && let Some(state) = state_read_source(right)
                                {
                                    self.state_reads.push((local.stable_id(), state));
                                }
                                if assign.prefix && assign.left.len() == 1
                                    && let Some(name) = type_call_role(right)
                                {
                                    self.candidate(local.stable_id(), Candidate {
                                        name, priority: 41, reason: "type_named_call",
                                        witness: report_witness(self.options.emit_report, || "call to a local function named like a type; role only".into()),
                                        from_binding: None,
                                    });
                                }
                                if let Some(node) = self.nodes.get_mut(&local.stable_id()) {
                                    node.module_leaf = module_leaf(right);
                                }
                                if let RValue::Index(index) = right {
                                    if let Some(role) = string(&index.right).and_then(field_role) {
                                        self.reads.push((local.stable_id(), role));
                                    }
                                }
                                if let RValue::Closure(closure) = right {
                                    self.closures.insert(local.stable_id(), closure.function.0.clone());
                                    let function = closure.function.lock();
                                    let returns = (function.body.len() <= self.options.node_budget && function.parameters.len() <= self.options.binding_budget).then(|| function.body.last()).flatten().and_then(|tail| {
                                        let Statement::Return(ret) = tail else { return None; };
                                        if function.body.iter().take(function.body.len() - 1).any(|s| !matches!(s,
                                            Statement::Assign(_) | Statement::Call(_) | Statement::MethodCall(_))) {
                                            return None;
                                        }
                                        ret.values.iter().map(|value| match value {
                                            RValue::Local(local) => Some(ReturnRole::Binding(local.stable_id())),
                                            RValue::Index(index) => string(&index.right).and_then(field_role).map(ReturnRole::Field),
                                            _ => None,
                                        }).collect::<Option<Vec<_>>>()
                                    });
                                    self.functions.insert(local.stable_id(), FunctionRoles {
                                        parameters: function.parameters.iter().map(RcLocal::stable_id).collect(),
                                        returns, variadic: function.is_variadic,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Statement::Call(call) => self.call(call),
                Statement::MethodCall(_) => self.report.unresolved_calls += 1,
                Statement::SetList(list) => {
                    if let Some(node) = self.reference(&list.object_local) {
                        node.reads += 1;
                    }
                    if list.tail.is_some() {
                        self.globals.insert("table".into());
                    }
                }
                Statement::Close(close) => {
                    for local in &close.locals {
                        if let Some(node) = self.reference(local) {
                            node.reads += 1;
                        }
                    }
                    self.globals.insert("__close_uv".into());
                }
                // Repeat's condition is in its body's scope.
                Statement::Repeat(repeat) => {
                    let child = self.child_scope(scope);
                    self.block(&repeat.block.lock(), child, depth + 1);
                    self.expression(&repeat.condition, child, depth + 1);
                    self.exit_scope(child);
                    continue;
                }
                _ => {}
            }
            statement.visit_rvalues(&mut |value| {
                self.expression(value, scope, depth + 1);
                true
            });
            match statement {
                Statement::If(branch) => {
                    self.child_block(&branch.then_block.lock(), scope, depth);
                    self.child_block(&branch.else_block.lock(), scope, depth);
                }
                Statement::While(loop_) => self.child_block(&loop_.block.lock(), scope, depth),
                Statement::NumericFor(loop_) => {
                    let child = self.child_scope(scope);
                    self.declare(&loop_.counter, child, "iteration");
                    let body = self.tick();
                    self.make_visible([&loop_.counter], body);
                    self.block(&loop_.block.lock(), child, depth + 1);
                    self.exit_scope(child);
                }
                Statement::GenericFor(loop_) => {
                    let child = self.child_scope(scope);
                    for local in &loop_.res_locals {
                        self.declare(local, child, "iteration");
                    }
                    let body = self.tick();
                    self.make_visible(&loop_.res_locals, body);
                    self.block(&loop_.block.lock(), child, depth + 1);
                    self.exit_scope(child);
                }
                _ => {}
            }
            // A declared local is visible after its statement, except that
            // `local f = function ... end` prints as `local function f`, whose
            // body already sees `f`.
            if let Statement::Assign(assign) = statement
                && assign.prefix
            {
                let local_function = matches!(
                    (assign.left.as_slice(), assign.right.as_slice()),
                    ([LValue::Local(_)], [RValue::Closure(_)])
                );
                let visible = if local_function { start } else { self.tick() };
                self.make_visible(assign.left.iter().filter_map(LValue::as_local), visible);
            }
        }
    }

    fn immutable(&self, id: u64) -> bool {
        self.nodes.get(&id).is_some_and(|node| {
            !node.ambiguous_owner
                && !node.ref_capture
                && node.scope.is_some()
                && if node.kind == "parameter" {
                    node.writes == 0
                } else {
                    node.kind == "local" && node.writes == 1
                }
        })
    }

    fn resolvable_function(&self, id: u64) -> bool {
        // A Ref capture alone does not rebind a function. Resolve only a
        // lexically owned closure declaration with exactly one write in the
        // entire graph, including nested closures. Value-role copy edges still
        // use the stricter immutable() gate and never unify capture cells.
        self.functions.contains_key(&id) && self.nodes.get(&id).is_some_and(|node|
            node.kind == "local" && node.writes == 1 && node.scope.is_some() && !node.ambiguous_owner)
    }

    /// The one name the best useful candidates of `id` agree on, if they rank
    /// at 40 or above.
    fn best(&self, id: u64) -> Option<(String, u8)> {
        let candidates = &self.nodes[&id].candidates;
        let priority = candidates.iter().filter(|c| useful(&c.name)).map(|c| c.priority).max()?;
        let names: BTreeSet<_> = candidates.iter().filter(|c| c.priority == priority).map(|c| &c.name).collect();
        (priority >= 40 && names.len() == 1).then(|| ((*names.first().unwrap()).clone(), priority))
    }

    fn propagate<const INCREMENTAL: bool>(&mut self) {
        let mut edges = Vec::new();
        for &(left, right) in &self.copies {
            if self.immutable(left) && self.immutable(right) {
                edges.push((left, right, "immutable_copy_role"));
                edges.push((right, left, "immutable_copy_role"));
            } else {
                self.report.refused_edges += 1;
            }
        }
        for (callee, arguments) in &self.calls {
            let Some(id) = callee.filter(|id| self.resolvable_function(*id)) else {
                self.report.unresolved_calls += 1;
                continue;
            };
            let Some(function) = self.functions.get(&id) else {
                self.report.unresolved_calls += 1;
                continue;
            };
            // Only exact-arity local arguments are connected, never guessed tail
            // results, dynamic dispatch, writes or mutable capture cells.
            let parameters = &function.parameters;
            if function.variadic || parameters.len() != arguments.len() {
                self.report.refused_edges += 1;
                continue;
            }
            for (parameter, argument) in parameters.iter().zip(arguments) {
                if let Some(argument) = argument
                    && self.immutable(*parameter)
                    && self.immutable(*argument)
                {
                    edges.push((*parameter, *argument, "resolved_local_call_argument"));
                } else {
                    self.report.refused_edges += 1;
                }
            }
        }
        let mut result_fields = Vec::new();
        for result in &self.results {
            let function = result.callee.filter(|id| self.resolvable_function(*id))
                .and_then(|id| self.functions.get(&id));
            let Some(function) = function else { continue; };
            let Some(returns) = &function.returns else { self.report.refused_edges += 1; continue; };
            if function.variadic || Some(function.parameters.len()) != result.arguments || returns.len() != result.destinations.len() {
                self.report.refused_edges += 1;
                continue;
            }
            for (source, target) in returns.iter().zip(&result.destinations) {
                let Some(target) = target.filter(|&id| self.immutable(id)) else {
                    self.report.refused_edges += 1;
                    continue;
                };
                match source {
                    ReturnRole::Binding(source) if self.immutable(*source) => {
                        edges.push((*source, target, "resolved_local_call_result"));
                    }
                    ReturnRole::Field(name) => result_fields.push((target, name.clone(), result.callee.unwrap())),
                    _ => self.report.refused_edges += 1,
                }
            }
        }
        for (id, name, callee) in result_fields {
            self.candidate(id, Candidate { name, priority: 65, reason: "resolved_local_call_field_result",
                witness: report_witness(self.options.emit_report, || format!("fixed return field slot in helper b{callee}; role only, field evaluation is unchanged")),
                from_binding: Some(callee) });
        }
        for (id, name) in std::mem::take(&mut self.reads) {
            if self.immutable(id) {
                self.candidate(id, Candidate { name, priority: 80, reason: "record_field_read",
                    witness: report_witness(self.options.emit_report, || "literal field read assigned to an immutable local; role only".into()), from_binding: None });
            } else { self.report.refused_edges += 1; }
        }
        let joins: Vec<_> = std::mem::take(&mut self.joins).into_iter().filter(|&(dest, a, b)| {
            let valid = self.immutable(a) && self.immutable(b) && self.nodes.get(&dest).is_some_and(|node|
                node.kind == "local" && node.scope.is_some() && !node.ambiguous_owner && !node.ref_capture && node.writes == 3);
            if !valid { self.report.refused_edges += 1; }
            valid
        }).collect();
        // Keep exactly four synchronous rounds and the original join/edge order.
        // Candidates are only appended or replaced by a strictly higher priority.
        // Re-emitting an unchanged source cannot change a target, even at its cap:
        // an earlier refusal already set overflow, and candidates are never removed.
        for round in 0..4 {
            let mut pending = Vec::new();
            for &(dest, a, b) in &joins {
                if INCREMENTAL && self.nodes[&a].changed_round < round
                    && self.nodes[&b].changed_round < round { continue; }
                if let (Some((name, x)), Some((other, y))) = (self.best(a), self.best(b)) {
                    if name == other {
                        pending.push((dest, Candidate { name, priority: x.min(y).min(66) - 1,
                            reason: "private_diamond_role_consensus",
                            witness: report_witness(self.options.emit_report, || format!("immutable inputs b{a} and b{b}; complete adjacent assignment diamond; role only")),
                            from_binding: None }));
                    }
                }
            }
            for &(source, target, reason) in &edges {
                if INCREMENTAL && self.nodes[&source].changed_round < round { continue; }
                for candidate in &self.nodes[&source].candidates {
                    if candidate.priority < 40 || !useful(&candidate.name) {
                        continue;
                    }
                    pending.push((
                        target,
                        Candidate {
                            name: candidate.name.clone(),
                            priority: candidate.priority.min(66) - 1,
                            reason,
                            witness: report_witness(self.options.emit_report, || "role only; binding identities remain distinct".into()),
                            from_binding: Some(source),
                        },
                    ));
                }
            }
            for (id, candidate) in pending {
                if self.candidate(id, candidate) {
                    self.nodes.get_mut(&id).unwrap().changed_round = round + 1;
                }
            }
        }
    }

    /// The results of a call the de-inliner rebuilt take the names of the
    /// locals the helper returns (`local track = loadTrack(...)` for a
    /// helper ending `return track`), else the subject its name gives
    /// (`loadTrack` -> `track`): the inlined copy held the value in that
    /// local before the call was rebuilt. Priority 40, the rename floor, so
    /// any other role evidence of the result decides first.
    fn name_rebuilt_results(&mut self) {
        let mut returned: BTreeMap<(u64, usize), Vec<Option<u64>>> = BTreeMap::new();
        for (callee, destinations) in std::mem::take(&mut self.rebuilt_results) {
            if !self.resolvable_function(callee) {
                continue;
            }
            let Some(function) = self.closures.get(&callee) else { continue };
            let budget = self.options.node_budget;
            let slots = returned
                .entry((callee, destinations.len()))
                .or_insert_with(|| returned_locals(&function.lock(), destinations.len(), budget))
                .clone();
            for (slot, destination) in destinations.iter().enumerate() {
                let Some(destination) = *destination else { continue };
                let source = slots[slot];
                let name = source
                    .and_then(|id| self.nodes.get(&id))
                    .map(|node| base_name(node).to_string())
                    .filter(|name| useful(name))
                    .or_else(|| {
                        let helper = &self.nodes.get(&callee)?.before;
                        (slot == 0).then(|| crate::name_locals::helper_result_noun(helper)).flatten()
                    });
                let Some(name) = name else { continue };
                self.candidate(destination, Candidate {
                    name, priority: 40, reason: "rebuilt_call_result",
                    witness: report_witness(self.options.emit_report, || format!("result of a rebuilt call of helper b{callee}; role only")),
                    from_binding: source,
                });
            }
        }
    }

    /// Final spellings, each checked against declared regions (`NameIndex`):
    /// - an unread generated local becomes `_`, as source spells a discarded
    ///   value (`local _, value = f()`), unless another `_` overlaps it;
    /// - a role proposal takes its name, or the first free counted spelling;
    /// - a counted name returns to the lowest free spelling of its base:
    ///   `part2` -> `part` once the binding that held `part` there is gone,
    ///   `p2` -> `p` after the receiver became `self`, a nested `i2` -> `j`.
    fn respell(&mut self, proposals: &BTreeMap<u64, String>, statuses: &mut BTreeMap<u64, &'static str>) {
        // Every reference must lie in the binding's own region, or printing
        // could resolve it elsewhere.
        fn renamable(node: &Node) -> bool {
            !node.ambiguous_owner
                && node.scope.is_some()
                && node.region.is_some_and(|region| {
                    node.references.first().is_none_or(|&first| first >= region.0)
                        && node.references.last().is_none_or(|&last| last <= region.1)
                })
        }
        let emit = self.options.emit_report;
        // The bases every query below spells from: only names counted from one
        // of them are ever asked about, so only those are indexed.
        let mut bases: FxHashSet<Cow<str>> = proposals.values().map(|base| Cow::Owned(proposal_base(base))).collect();
        let mut underscore_unread = !self.global_references.contains_key("_");
        let mut unread = Vec::new();
        let mut compactions = Vec::new();
        for (&id, node) in &self.nodes {
            underscore_unread &= node.before != "_" || node.reads == 0;
            if proposals.contains_key(&id) || !renamable(node) {
                continue;
            }
            if node.kind == "local" && node.reads == 0 && !node.source_protected && generated(&node.before) {
                unread.push((node.position, id));
            } else if node.stem.is_some() && node.before != "self" {
                compactions.push((node.position, id));
            }
        }
        // Unread temps become `_` unless the script reads a `_`; one whose
        // region another `_` overlaps keeps its counted name instead.
        if !underscore_unread {
            compactions.extend(unread.drain(..).filter(|(_, id)| self.nodes[id].stem.is_some()));
        }
        if proposals.is_empty() && unread.is_empty() && compactions.is_empty() {
            return;
        }
        if !unread.is_empty() {
            bases.insert(Cow::Borrowed("_"));
        }
        unread.sort_unstable();
        for (_, id) in unread.iter().chain(&compactions) {
            let node = &self.nodes[id];
            let stem = &node.before[..node.stem.map_or(0, |(stem, _)| stem)];
            if node.kind == "iteration" && stem == "i" {
                bases.extend(["j", "k"].map(Cow::Borrowed));
            }
            bases.insert(Cow::Borrowed(stem));
            if let Some(alternative) = &node.alternative {
                bases.insert(Cow::Borrowed(alternative));
            }
        }
        // Every candidate is a base, or a base and a counter (`part2`,
        // `bit32_2`, `clone_2`).
        let relevant = |name: &str| {
            let counted = name.trim_end_matches(|c: char| c.is_ascii_digit());
            bases.contains(name) || bases.contains(counted) || counted.strip_suffix('_').is_some_and(|base| bases.contains(base))
        };
        let mut index = NameIndex::new(&self.nodes, &self.globals, &self.global_references, self.options.dont_reuse_var, relevant);
        for (position, id) in unread {
            let node = &self.nodes[&id];
            if !index.free("_", node) {
                if node.stem.is_some() {
                    compactions.push((position, id));
                }
                continue;
            }
            index.remove(&node.before, node);
            index.insert(Cow::Borrowed("_"), node);
            node.local.0.lock().0 = Some("_".into());
            if emit { statuses.insert(id, "discarded_unread"); }
            self.report.renamed += 1;
        }
        // Earlier bindings take the lower counts.
        compactions.sort_unstable();
        let mut candidate = String::new();
        let mut proposed: Vec<_> = proposals.iter().map(|(id, base)| (self.nodes[id].position, *id, base)).collect();
        proposed.sort_unstable();
        for (_, id, base) in proposed {
            let node = &self.nodes[&id];
            if !renamable(node) {
                continue;
            }
            index.remove(&node.before, node);
            let base = proposal_base(base);
            let found = (1..=256).any(|counter| {
                spell_counted(&mut candidate, &base, counter);
                !crate::name_spelling::soft_reserved(&candidate) && index.free(&candidate, node)
            });
            if !found {
                index.insert(Cow::Borrowed(node.before.as_str()), node);
                continue;
            }
            index.insert(Cow::Owned(candidate.clone()), node);
            node.local.0.lock().0 = Some(candidate.clone());
            if emit { statuses.insert(id, "renamed"); }
            self.report.renamed += 1;
        }
        for (_, id) in compactions {
            let node = &self.nodes[&id];
            let Some((stem, counter)) = node.stem else { continue };
            let stem = &node.before[..stem];
            // Nested loop counters read `i`, `j`, `k`, as source spells them.
            let ladder: &[&str] = if node.kind == "iteration" && stem == "i" { &["i", "j", "k"] } else { &[stem] };
            // The base's distinct spelling comes before any counter.
            let rungs = ladder.len() + usize::from(node.alternative.is_some());
            let attempts = rungs + counter.saturating_sub(2);
            for attempt in 0..attempts.min(32) {
                match (ladder.get(attempt), &node.alternative) {
                    (Some(name), _) => {
                        candidate.clear();
                        candidate.push_str(name);
                    }
                    (None, Some(alternative)) if attempt == ladder.len() => {
                        candidate.clear();
                        candidate.push_str(alternative);
                    }
                    // A `clone_3` family stays spelled with `_`.
                    _ if node.before.as_bytes()[stem.len()] == b'_' && !stem.ends_with(|c: char| c.is_ascii_digit()) => {
                        candidate.clear();
                        let _ = write!(candidate, "{stem}_{}", attempt - rungs + 2);
                    }
                    _ => spell_counted(&mut candidate, stem, attempt - rungs + 2),
                }
                if candidate == node.before {
                    break;
                }
                if index.free(&candidate, node) {
                    index.remove(&node.before, node);
                    index.insert(Cow::Owned(candidate.clone()), node);
                    node.local.0.lock().0 = Some(candidate.clone());
                    if emit { statuses.insert(id, "compacted"); }
                    self.report.renamed += 1;
                    break;
                }
            }
        }
    }

    fn solve(mut self) -> Report {
        self.report.binding_count = self.nodes.len();
        self.report.scope_count = self.scopes.len();
        if !self.report.budget_exhausted {
            for id in self.order.clone() {
                let node = &self.nodes[&id];
                if node.key_use
                    && node.writes == 1
                    && let Some(name) = &node.module_leaf
                {
                    self.candidate(
                        id,
                        Candidate {
                            name: name.clone(),
                            priority: 90,
                            reason: "static_module_key",
                            witness: report_witness(self.options.emit_report, || "static script path leaf used as a table key".into()),
                            from_binding: None,
                        },
                    );
                }
            }
            self.propagate::<true>();
            // A read of a state's current value is named after the state, once
            // the state's own name is settled (`use(anchorPoint)` ->
            // `currentAnchorPoint`).
            for (value, state) in std::mem::take(&mut self.state_reads) {
                let Some(node) = self.nodes.get(&state) else { continue; };
                let name = match self.best(state) {
                    Some((name, priority)) if (generated(&node.before) || weak(node)) && priority >= rename_floor(node) => Some(name),
                    _ if generated(&node.before) => None,
                    _ => Some(base_name(node).to_string()),
                };
                if self.immutable(value)
                    && let Some(name) = name.as_deref().and_then(crate::name_locals::state_value_name)
                {
                    self.candidate(value, Candidate {
                        name, priority: 40, reason: "state_current_value",
                        witness: report_witness(self.options.emit_report, || "single read of a named state; role only".into()),
                        from_binding: Some(state),
                    });
                }
            }
            // The table `merge(table, props)` starts from holds the defaults
            // the props override.
            for table in std::mem::take(&mut self.merge_bases) {
                if self.tables.contains(&table) && self.immutable(table) {
                    self.candidate(table, Candidate {
                        name: "defaults".into(), priority: 42, reason: "merge_defaults",
                        witness: report_witness(self.options.emit_report, || "table constructor merged with overrides; role only".into()),
                        from_binding: None,
                    });
                }
            }
            self.name_rebuilt_results();
        }
        let mut statuses = BTreeMap::new();
        let mut proposals = BTreeMap::new();
        for (&id, node) in &self.nodes {
            let generic_parameter = node.kind == "parameter"
                && matches!(
                    node.before.as_str(),
                    "value"
                        | "data"
                        | "object"
                        | "options"
                        | "callback"
                        | "flag"
                        | "number"
                        | "string"
                        | "table"
                        | "items"
                        | "list"
                );
            let module_key = node.key_use
                && node
                    .module_leaf
                    .as_ref()
                    .is_some_and(|leaf| node.before == leaf.to_lowercase());
            let status = if self.report.budget_exhausted {
                "budget_exhausted"
            } else if node.source_protected {
                "source_protected"
            } else if node.before == "self" {
                "method_receiver_protected"
            } else if node.ambiguous_owner || node.scope.is_none() {
                "unknown_owner"
            } else if node.overflow {
                "candidate_budget_exhausted"
            } else if !(generated(&node.before) || generic_parameter || module_key || weak(node)) {
                "kept_existing_role"
            } else {
                let floor = rename_floor(node);
                let best = node
                    .candidates
                    .iter()
                    .filter(|c| c.priority >= floor && (c.reason != "retained_arithmetic_snapshot" || node.writes == 1))
                    .map(|c| c.priority)
                    .max();
                let names: BTreeSet<_> = node
                    .candidates
                    .iter()
                    .filter(|c| Some(c.priority) == best && (c.reason != "retained_arithmetic_snapshot" || node.writes == 1))
                    .map(|c| &c.name)
                    .collect();
                if best.is_none() {
                    "no_stronger_evidence"
                } else if names.len() > 1 {
                    self.report.conflicts += 1;
                    "conflicting_evidence"
                } else {
                    let name = (*names.first().unwrap()).clone();
                    if name == node.before {
                        "kept_matching_role"
                    } else {
                        proposals.insert(id, name);
                        "proposed"
                    }
                }
            };
            if self.options.emit_report { statuses.insert(id, status); }
        }
        if !self.report.budget_exhausted {
            self.respell(&proposals, &mut statuses);
        }
        if self.options.emit_report {
            for id in self.order {
                let node = self.nodes.remove(&id).unwrap();
                let after = node.local.0.lock().0.clone().unwrap_or_default();
                self.report.bindings.push(BindingReport {
                    id,
                    before: node.before,
                    after,
                    kind: node.kind,
                    scope: node.scope,
                    status: statuses[&id],
                    candidates: node.candidates,
                    type_evidence: node.type_evidence,
                });
            }
        }
        self.report
    }
}

/// Where every name is declared and referenced, on the graph clock
/// (`Graph::tick`). Clocks run in preorder, so every scope spans one clock
/// interval, and a binding's region (from where it becomes visible to its
/// scope's exit) holds exactly the points it is visible at. A name is free for
/// a binding exactly when no other entity of that name, binding or global, is
/// referenced inside the binding's region (a declaration counts as a
/// reference), and no reference of the binding lies inside another same-named
/// binding's region: the new spelling neither captures a reference nor
/// shadows a binding.
///
/// A binding indexed by region has every reference inside it (any other is
/// reserved everywhere), so only bindings whose regions overlap can conflict:
/// a name with few holders is checked holder by holder, and a crowded one
/// (`p`, `v`) through sorted lists built on first use. A query costs one hash
/// lookup and O(log n) per reference of the binding.
struct NameIndex<'a> {
    file_unique: bool,
    names: FxHashMap<Cow<'a, str>, NameEntry<'a>>,
}

#[derive(Default)]
struct NameEntry<'a> {
    /// No binding may take the name anywhere: a global the output prints
    /// without a node, or a holder without a region holding all its references.
    universal: bool,
    /// The sorted reference clocks of the global of this name.
    global: &'a [u32],
    holders: Holders<'a>,
    occupancy: Option<Occupancy<'a>>,
}

/// The bindings holding one name; most names have one.
enum Holders<'a> {
    One(&'a Node),
    Many(Vec<&'a Node>),
}

impl Default for Holders<'_> {
    fn default() -> Self {
        Holders::Many(Vec::new())
    }
}

impl<'a> Holders<'a> {
    fn nodes(&self) -> &[&'a Node] {
        match self {
            Holders::One(node) => std::slice::from_ref(node),
            Holders::Many(nodes) => nodes,
        }
    }

    fn push(&mut self, node: &'a Node) {
        match self {
            Holders::One(first) => *self = Holders::Many(vec![*first, node]),
            Holders::Many(nodes) if nodes.is_empty() => *self = Holders::One(node),
            Holders::Many(nodes) => nodes.push(node),
        }
    }

    fn remove(&mut self, node: &Node) {
        match self {
            Holders::One(first) if std::ptr::eq(*first, node) => *self = Holders::Many(Vec::new()),
            Holders::One(_) => {}
            Holders::Many(nodes) => {
                if let Some(position) = nodes.iter().position(|&holder| std::ptr::eq(holder, node)) {
                    nodes.swap_remove(position);
                }
            }
        }
    }
}

/// A name with more holders than this is checked through sorted lists.
const CROWDED: usize = 16;

/// One crowded name's references and regions, sorted for binary search.
#[derive(Default)]
struct Occupancy<'a> {
    references: Vec<u32>,
    /// Sorted by start, with `reach[i]` the latest end among the first `i + 1`:
    /// some region holds `at` exactly when the last one starting at or before
    /// `at` has `reach >= at`.
    regions: Vec<(u32, u32)>,
    reach: Vec<u32>,
    /// Holders that took the name after the lists were built, checked one by one.
    joined: Vec<&'a Node>,
    /// Holders in the lists that have since left the name, discounted exactly.
    left: Vec<&'a Node>,
}

impl<'a> Occupancy<'a> {
    fn build(holders: &[&'a Node]) -> Self {
        let mut occupancy = Self::default();
        for node in holders {
            occupancy.references.extend(&node.references);
            occupancy.regions.extend(node.region);
        }
        occupancy.references.sort_unstable();
        occupancy.regions.sort_unstable();
        let mut reach = 0;
        occupancy.reach = occupancy.regions.iter().map(|&(_, end)| { reach = reach.max(end); reach }).collect();
        occupancy
    }

    /// Whether a reference of a current listed holder lies in `region`.
    fn referenced_within(&self, region: (u32, u32)) -> bool {
        let all = count_within(&self.references, region);
        // The references of holders that left are a subset of the lists.
        all > 0 && all > self.left.iter().map(|node| count_within(&node.references, region)).sum::<usize>()
    }

    /// Whether some listed region holds `at`, the regions of holders that left
    /// included.
    fn covers(&self, at: u32) -> bool {
        let starting = self.regions.partition_point(|&(start, _)| start <= at);
        starting > 0 && self.reach[starting - 1] >= at
    }
}

/// How many references in the sorted `references` lie in `[start, end]`.
fn count_within(references: &[u32], (start, end): (u32, u32)) -> usize {
    references.partition_point(|&at| at <= end) - references.partition_point(|&at| at < start)
}

/// Whether some reference in the sorted `references` lies in `[start, end]`.
fn referenced_within(references: &[u32], (start, end): (u32, u32)) -> bool {
    let first = references.partition_point(|&at| at < start);
    references.get(first).is_some_and(|&at| at <= end)
}

fn holds(region: Option<(u32, u32)>, at: u32) -> bool {
    region.is_some_and(|(start, end)| start <= at && at <= end)
}

/// The base a role proposal spells from: a builtin's spelling reads as its
/// alternative (`type` -> `kind`).
fn proposal_base(base: &str) -> String {
    match crate::name_spelling::soft_reserved(base) {
        true => crate::name_spelling::builtin_alternative(base).unwrap_or_else(|| base.to_string()),
        false => base.to_string(),
    }
}

/// Spell the `counter`-th name counted from `base` into `out`, the base
/// itself first.
fn spell_counted(out: &mut String, base: &str, counter: usize) {
    out.clear();
    if counter < 2 {
        out.push_str(base);
    } else {
        crate::name_spelling::push_suffixed(out, base, counter);
    }
}

/// Whether two bindings may share a name: neither references a point inside
/// the other's region.
fn compatible(node: &Node, other: &Node) -> bool {
    let (Some(region), Some(other_region)) = (node.region, other.region) else { return false };
    other_region.1 < region.0
        || region.1 < other_region.0
        || (!referenced_within(&other.references, region) && !referenced_within(&node.references, other_region))
}

impl<'a> NameIndex<'a> {
    /// Index the bindings and globals whose names are `relevant`: the only
    /// names queries ask about.
    fn new(
        nodes: &'a BTreeMap<u64, Node>, globals: &'a BTreeSet<String>, references: &'a FxHashMap<String, Vec<u32>>,
        file_unique: bool, relevant: impl Fn(&str) -> bool,
    ) -> Self {
        let mut index = Self { file_unique, names: FxHashMap::default() };
        for name in globals {
            index.names.entry(Cow::Borrowed(name.as_str())).or_default().universal = true;
        }
        for (name, clocks) in references {
            if relevant(name) {
                index.names.entry(Cow::Borrowed(name.as_str())).or_default().global = clocks;
            }
        }
        for node in nodes.values() {
            if relevant(&node.before) {
                index.insert(Cow::Borrowed(node.before.as_str()), node);
            }
        }
        index
    }

    fn insert(&mut self, name: Cow<'a, str>, node: &'a Node) {
        if name.is_empty() {
            return;
        }
        let enclosed = node.region.is_some_and(|region| {
            node.references.first().is_none_or(|&first| first >= region.0)
                && node.references.last().is_none_or(|&last| last <= region.1)
        });
        let entry = self.names.entry(name).or_default();
        entry.universal |= !enclosed || node.ambiguous_owner;
        entry.holders.push(node);
        if let Some(occupancy) = &mut entry.occupancy {
            // Rebuilt once enough holders joined to make one-by-one checks slow.
            if occupancy.joined.len() < 64 {
                occupancy.joined.push(node);
            } else {
                entry.occupancy = None;
            }
        }
    }

    fn remove(&mut self, name: &str, node: &'a Node) {
        let Some(entry) = self.names.get_mut(name) else { return };
        entry.holders.remove(node);
        if let Some(occupancy) = &mut entry.occupancy {
            match occupancy.joined.iter().position(|&joined| std::ptr::eq(joined, node)) {
                Some(position) => {
                    occupancy.joined.swap_remove(position);
                }
                // Rebuilt once enough holders left to make discounting slow.
                None if occupancy.left.len() < 64 => occupancy.left.push(node),
                None => entry.occupancy = None,
            }
        }
    }

    /// Whether `node` may be spelled `name`.
    fn free(&mut self, name: &str, node: &Node) -> bool {
        let Some(entry) = self.names.get_mut(name) else { return true };
        if entry.universal {
            return false;
        }
        let holders = entry.holders.nodes();
        if self.file_unique {
            return holders.is_empty() && entry.global.is_empty();
        }
        let Some(region) = node.region else { return false };
        if referenced_within(entry.global, region) {
            return false;
        }
        if holders.len() <= CROWDED {
            return holders.iter().all(|other| compatible(node, other));
        }
        let occupancy = entry.occupancy.get_or_insert_with(|| Occupancy::build(holders));
        let covered = |at: u32| {
            occupancy.covers(at)
                && (!occupancy.left.iter().any(|other| holds(other.region, at))
                    // A region of a holder that left holds `at`: ask the holders.
                    || holders.iter().any(|other| holds(other.region, at)))
        };
        !occupancy.referenced_within(region)
            && !node.references.iter().any(|&at| covered(at))
            && occupancy.joined.iter().all(|other| compatible(node, other))
    }
}

/// The local a function returns in each of its first `slots` results: the
/// same one of its own locals (never a parameter) on every `return` that
/// gives the slot a value other than `nil`. Nested functions' returns are
/// theirs; `budget` bounds the statements visited.
fn returned_locals(function: &Function, slots: usize, budget: usize) -> Vec<Option<u64>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Slot {
        Unseen,
        Local(u64),
        Mixed,
    }
    fn visit(block: &[Statement], parameters: &[RcLocal], slots: &mut [Slot], budget: &mut usize) {
        for statement in block {
            if *budget == 0 {
                slots.fill(Slot::Mixed);
                return;
            }
            *budget -= 1;
            match statement {
                Statement::Return(ret) => {
                    for (slot, value) in slots.iter_mut().zip(&ret.values) {
                        let seen = match value {
                            RValue::Literal(Literal::Nil) => continue,
                            RValue::Local(local) if !parameters.contains(local) => Slot::Local(local.stable_id()),
                            _ => Slot::Mixed,
                        };
                        *slot = match *slot {
                            Slot::Unseen => seen,
                            same if same == seen => same,
                            _ => Slot::Mixed,
                        };
                    }
                }
                Statement::If(branch) => {
                    visit(&branch.then_block.lock().0, parameters, slots, budget);
                    visit(&branch.else_block.lock().0, parameters, slots, budget);
                }
                Statement::While(node) => visit(&node.block.lock().0, parameters, slots, budget),
                Statement::Repeat(node) => visit(&node.block.lock().0, parameters, slots, budget),
                Statement::NumericFor(node) => visit(&node.block.lock().0, parameters, slots, budget),
                Statement::GenericFor(node) => visit(&node.block.lock().0, parameters, slots, budget),
                _ => {}
            }
        }
    }
    let mut found = vec![Slot::Unseen; slots];
    let mut budget = budget;
    visit(&function.body.0, &function.parameters, &mut found, &mut budget);
    found
        .into_iter()
        .map(|slot| match slot {
            Slot::Local(id) => Some(id),
            _ => None,
        })
        .collect()
}

pub fn refine_final_names(block: &Block, options: Options) -> Report {
    let mut graph = Graph::new(options);
    graph.block(block, 0, 0);
    graph.exit_scope(0);
    graph.solve()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Assign, Binary, BinaryOperation, BindingOrigin, Closure, Function, Global, If, Index,
        Local, Return, SourceBinding, Table,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    #[test]
    fn dirty_role_rounds_match_full_sweeps_including_caps_and_witnesses() {
        for seed in 1..100u64 {
            let locals: Vec<_> = (0..32).map(|i| local(&format!("v{i}"))).collect();
            let build = || {
                let mut graph = Graph::new(Options { emit_report: true, ..Default::default() });
                let mut random = seed;
                let mut next = || { random ^= random << 13; random ^= random >> 7; random ^= random << 17; random as usize };
                for (index, local) in locals.iter().enumerate() {
                    let node = graph.node(local).unwrap();
                    node.kind = "local";
                    node.scope = Some(0);
                    node.writes = if index < 24 { 1 } else { 3 };
                    for _ in 0..(next() % 30) {
                        let role = next() % 12;
                        graph.candidate(local.stable_id(), Candidate {
                            name: format!("role{role}"), priority: (next() % 100) as u8,
                            reason: if next() % 2 == 0 { "record_field_value" } else { "assertion_parameter" },
                            witness: format!("seed {seed}, local {index}, role {role}"),
                            from_binding: None,
                        });
                    }
                }
                for _ in 0..96 {
                    graph.copies.push((locals[next() % 24].stable_id(), locals[next() % 24].stable_id()));
                }
                for index in 24..32 {
                    graph.joins.push((locals[index].stable_id(), locals[next() % 24].stable_id(), locals[next() % 24].stable_id()));
                }
                for index in 0..8 {
                    let callee = locals[index].stable_id();
                    graph.functions.insert(callee, FunctionRoles {
                        parameters: vec![locals[next() % 24].stable_id()],
                        returns: Some(vec![ReturnRole::Binding(locals[next() % 24].stable_id()), ReturnRole::Field("payload".into())]),
                        variadic: index == 7,
                    });
                    graph.calls.push((Some(callee), vec![Some(locals[next() % 32].stable_id())]));
                    graph.results.push(ResultUse { callee: Some(callee), arguments: Some(1),
                        destinations: vec![Some(locals[next() % 24].stable_id()), Some(locals[next() % 24].stable_id())] });
                }
                graph
            };
            let mut indexed = build();
            let mut reference = build();
            indexed.propagate::<true>();
            reference.propagate::<false>();
            for (id, expected) in &reference.nodes {
                let actual = &indexed.nodes[id];
                assert_eq!(format!("{:?}", actual.candidates), format!("{:?}", expected.candidates), "seed {seed}, local {id}");
                assert_eq!(actual.overflow, expected.overflow, "seed {seed}, local {id}");
            }
            assert_eq!(format!("{:?}", indexed.report), format!("{:?}", reference.report));
        }
    }

    #[test]
    fn source_only_naming_omits_explanations_without_changing_decisions() {
        let input = local("v0");
        input.0.lock().1 = Some("number".into());
        let output = local("v1");
        let block = Block(vec![declare(&input, Literal::Number(4.0).into()),
            declare(&output, input.clone().into()),
            Assign::new(vec![Index::new(global("record"), Literal::String(b"width".to_vec()).into()).into()], vec![output.clone().into()]).into()]);
        let mut source = Graph::new(Options::default());
        let mut detailed = Graph::new(Options { emit_report: true, ..Default::default() });
        source.block(&block, 0, 0);
        detailed.block(&block, 0, 0);
        assert!(source.nodes.values().all(|node| node.type_evidence.is_empty() && node.candidates.iter().all(|c| c.witness.is_empty())));
        assert!(!detailed.nodes[&input.stable_id()].type_evidence.is_empty());
        let report = source.solve();
        let names = (input.to_string(), output.to_string());
        input.0.lock().0 = Some("v0".into()); output.0.lock().0 = Some("v1".into());
        let detailed_report = detailed.solve();
        assert_eq!(names, (input.to_string(), output.to_string()));
        assert_eq!(report.renamed, detailed_report.renamed);
        assert_eq!(report.conflicts, detailed_report.conflicts);
        assert_eq!(report.refused_edges, detailed_report.refused_edges);
        assert!(report.bindings.is_empty());
        assert!(detailed_report.bindings.iter().flat_map(|b| &b.candidates).all(|c| !c.witness.is_empty()));
    }

    fn store(target: &str, key: &str, value: &RcLocal) -> Statement {
        Assign::new(vec![Index::new(global(target), text(key)).into()], vec![value.clone().into()]).into()
    }

    fn counted(name: &str, base: &str) -> RcLocal {
        let mut local = Local::new(None);
        local.set_counted_name(name.into(), base);
        RcLocal::new(local)
    }

    fn numeric_for(counter: &RcLocal, body: Vec<Statement>) -> Statement {
        Statement::NumericFor(Box::new(crate::NumericFor::new(
            Literal::Number(1.0).into(), Literal::Number(9.0).into(), Literal::Number(1.0).into(),
            counter.clone(), Block(body),
        )))
    }

    #[test]
    fn crowded_name_lists_answer_exactly_as_holder_by_holder_checks() {
        for seed in 1..60u64 {
            let mut random = seed;
            let mut next = move || { random ^= random << 13; random ^= random >> 7; random ^= random << 17; random as u32 };
            // Laminar scopes as preorder clock intervals, each binding visible
            // from a point of its scope to the scope's exit.
            let mut scopes = vec![(0u32, 100_000u32)];
            while scopes.len() < 24 {
                let (start, end) = scopes[next() as usize % scopes.len()];
                if end - start < 8 { continue; }
                let a = start + 1 + next() % (end - start - 2);
                let b = a + 1 + next() % (end - a);
                scopes.push((a, b.min(end - 1).max(a + 1)));
            }
            let mut graph = Graph::new(Options::default());
            let locals: Vec<_> = (0..60).map(|i| local(if i < 50 { "x" } else { "y" })).collect();
            for local in &locals {
                let (start, end) = scopes[next() as usize % scopes.len()];
                let visible = start + next() % (end - start + 1);
                let mut references = vec![visible];
                for _ in 0..next() % 4 {
                    references.push(visible + next() % (end - visible + 1));
                }
                references.sort_unstable();
                let node = graph.node(local).unwrap();
                node.scope = Some(0);
                node.region = Some((visible, end));
                node.references = references;
            }
            let globals = BTreeSet::new();
            let global_references = FxHashMap::default();
            let mut index = NameIndex::new(&graph.nodes, &globals, &global_references, false, |_| true);
            let mut holders: Vec<u64> = locals[..50].iter().map(RcLocal::stable_id).collect();
            for step in 0..40 {
                for query in &locals[50..] {
                    let node = &graph.nodes[&query.stable_id()];
                    let expected = holders.iter().all(|id| compatible(node, &graph.nodes[id]));
                    assert_eq!(index.free("x", node), expected, "seed {seed}, step {step}");
                }
                // Holders leave and join the crowded name between queries.
                let id = locals[next() as usize % 60].stable_id();
                let node = &graph.nodes[&id];
                if let Some(position) = holders.iter().position(|&holder| holder == id) {
                    holders.remove(position);
                    index.remove("x", node);
                    index.insert(Cow::Borrowed("z"), node);
                } else if !locals[50..].iter().any(|local| local.stable_id() == id) {
                    holders.push(id);
                    index.remove("z", node);
                    index.insert(Cow::Borrowed("x"), node);
                }
            }
        }
    }

    #[test]
    fn sequential_bindings_of_one_scope_keep_distinct_names() {
        // `local a = 1; local b = 2; return a + b`: a scope-entry clock gave
        // both the empty region of a scope with no later child, and could
        // spell them `x` and `x`.
        let a = local("v");
        let b = local("v2");
        let block = Block(vec![
            declare(&a, Literal::Number(1.0).into()),
            declare(&b, Literal::Number(2.0).into()),
            store("T", "size", &a),
            store("T", "size", &b),
            Return::new(vec![Binary::new(a.clone().into(), b.clone().into(), BinaryOperation::Add).into()]).into(),
        ]);
        run(&block);
        assert_eq!(a.to_string(), "size");
        assert_eq!(b.to_string(), "size2");
    }

    #[test]
    fn a_name_used_only_before_a_declaration_is_free_after_it() {
        // `if c then local part = ...; use(part) end; local part2 = ...`: the
        // earlier binding's region ended, so the later one compacts to `part`.
        let early = local("part");
        let late = counted("part2", "part");
        let block = Block(vec![
            If::new(
                global("c"),
                Block(vec![declare(&early, global("x")), Return::new(vec![early.clone().into()]).into()]),
                Block(vec![]),
            )
            .into(),
            declare(&late, global("y")),
            Return::new(vec![late.clone().into()]).into(),
        ]);
        run(&block);
        assert_eq!(late.to_string(), "part");
        // Still visible and read after the declaration: no compaction.
        let outer = local("part");
        let inner = counted("part2", "part");
        let block = Block(vec![
            declare(&outer, global("x")),
            If::new(
                global("c"),
                Block(vec![declare(&inner, global("y")), Return::new(vec![inner.clone().into()]).into()]),
                Block(vec![]),
            )
            .into(),
            Return::new(vec![outer.clone().into()]).into(),
        ]);
        run(&block);
        assert_eq!(inner.to_string(), "part2");
        // Not read after the inner declaration, but visible there: compacting
        // would shadow it, so it is refused too.
        let outer = local("part");
        let inner = counted("part2", "part");
        let block = Block(vec![
            declare(&outer, global("x")),
            Call::new(global("use"), vec![outer.clone().into()]).into(),
            If::new(
                global("c"),
                Block(vec![declare(&inner, global("y")), Return::new(vec![inner.clone().into()]).into()]),
                Block(vec![]),
            )
            .into(),
        ]);
        run(&block);
        assert_eq!(inner.to_string(), "part2");
    }

    #[test]
    fn generated_counters_compact_and_unread_temps_become_underscore() {
        let read = local("v3");
        let unread = local("v5");
        let block = Block(vec![
            declare(&read, global("x")),
            declare(&unread, Call::new(global("f"), vec![]).into()),
            Assign::new(vec![unread.clone().into()], vec![Call::new(global("g"), vec![]).into()]).into(),
            Return::new(vec![read.clone().into()]).into(),
        ]);
        run(&block);
        assert_eq!(read.to_string(), "v");
        assert_eq!(unread.to_string(), "_");
        // Another `_` visible in its region keeps an unread temp off `_`: a
        // write would land on whichever `_` is innermost.
        let outer = local("v4");
        let inner = local("_");
        let block = Block(vec![
            declare(&outer, Call::new(global("f"), vec![]).into()),
            If::new(global("c"), Block(vec![
                declare(&inner, Call::new(global("g"), vec![]).into()),
                Assign::new(vec![outer.clone().into()], vec![global("x")]).into(),
            ]), Block(vec![])).into(),
        ]);
        run(&block);
        assert_eq!(outer.to_string(), "v");
        // A script reading the global `_` keeps every binding off that name.
        let unread = local("v5");
        let block = Block(vec![
            declare(&unread, Call::new(global("f"), vec![]).into()),
            Assign::new(vec![unread.clone().into()], vec![Call::new(global("g"), vec![]).into()]).into(),
            Return::new(vec![global("_")]).into(),
        ]);
        run(&block);
        assert_eq!(unread.to_string(), "v");
    }

    #[test]
    fn late_underscore_counters_compact_within_their_family() {
        // `clone_3` from a late pass's `unique_name`: `clone` is held in its
        // region, `clone_2` is not.
        let held = local("clone");
        let gone = counted("clone_2", "clone");
        let counted = counted("clone_3", "clone");
        let block = Block(vec![
            If::new(global("c"), Block(vec![declare(&gone, global("x")), Return::new(vec![gone.clone().into()]).into()]), Block(vec![])).into(),
            declare(&held, global("y")),
            declare(&counted, global("z")),
            Return::new(vec![held.clone().into(), counted.clone().into()]).into(),
        ]);
        run(&block);
        assert_eq!((held.to_string(), counted.to_string()), ("clone".into(), "clone_2".into()));
    }

    #[test]
    fn a_taken_base_falls_back_to_its_private_spelling_before_a_counter() {
        // `local light2 = self._light` beside a visible `light`: the field's
        // own spelling reads better than a counter. Another counted name
        // keeps counting.
        let light = local("light");
        let private = counted("light2", "light");
        private.0 .0.lock().4.private_stem = true;
        let other = counted("part2", "part");
        let part = local("part");
        let block = Block(vec![
            declare(&light, global("a")),
            declare(&part, global("c")),
            declare(&private, global("d")),
            declare(&other, global("f")),
            Return::new([&light, &part, &private, &other].map(|l| l.clone().into()).into()).into(),
        ]);
        run(&block);
        assert_eq!((private.to_string(), other.to_string()), ("_light".into(), "part2".into()));
    }

    #[test]
    fn a_free_base_still_wins_over_its_private_spelling() {
        let private = counted("light2", "light");
        private.0 .0.lock().4.private_stem = true;
        let block = Block(vec![declare(&private, global("a")), Return::new(vec![private.clone().into()]).into()]);
        run(&block);
        assert_eq!(private.to_string(), "light");
    }

    #[test]
    fn a_suffix_the_evidence_spelled_is_no_counter() {
        // `FindFirstChild("Wheel_2")` and `("Wheel_3")` name `wheel_2` and
        // `wheel_3`: nothing appended those numbers, so nothing renumbers them,
        // although `wheel` is free.
        let front = local("wheel_2");
        let back = local("wheel_3");
        let level = local("level_10");
        let block = Block(vec![
            declare(&front, global("x")),
            declare(&back, global("y")),
            declare(&level, global("z")),
            Return::new(vec![front.clone().into(), back.clone().into(), level.clone().into()]).into(),
        ]);
        run(&block);
        assert_eq!((front.to_string(), back.to_string(), level.to_string()), ("wheel_2".into(), "wheel_3".into(), "level_10".into()));
    }

    #[test]
    fn nested_loop_counters_read_i_j_k() {
        let outer = local("i");
        let middle = counted("i2", "i");
        let inner = counted("i3", "i");
        let use_all = Call::new(global("use"), vec![outer.clone().into(), middle.clone().into(), inner.clone().into()]);
        let block = Block(vec![numeric_for(&outer, vec![numeric_for(&middle, vec![numeric_for(&inner, vec![use_all.into()])])])]);
        run(&block);
        assert_eq!((outer.to_string(), middle.to_string(), inner.to_string()), ("i".into(), "j".into(), "k".into()));
    }

    #[test]
    fn a_global_blocks_a_name_only_inside_the_region_it_is_read_in() {
        // `local type2 = type` becomes `local type = type`: the global is read
        // only in the declaration, before the local is visible.
        let alias = counted("type2", "type");
        let block = Block(vec![
            declare(&alias, global("type")),
            Return::new(vec![Call::new(alias.clone().into(), vec![global("x")]).into()]).into(),
        ]);
        run(&block);
        assert_eq!(alias.to_string(), "type");
        // A later read of the global would be captured.
        let alias = counted("type2", "type");
        let block = Block(vec![
            declare(&alias, global("type")),
            Return::new(vec![Call::new(alias.clone().into(), vec![Call::new(global("type"), vec![]).into()]).into()]).into(),
        ]);
        run(&block);
        assert_eq!(alias.to_string(), "type2");
    }

    #[test]
    fn a_local_function_sees_its_own_name_in_its_body() {
        // `local size2 = function() return size end` prints as `local function`,
        // whose body would read the function itself as `size`.
        let function_local = counted("size2", "size");
        let block = Block(vec![
            declare(&function_local, closure(vec![], Block(vec![Return::new(vec![global("size")]).into()]))),
            Return::new(vec![function_local.clone().into()]).into(),
        ]);
        run(&block);
        assert_eq!(function_local.to_string(), "size2");
        // A plain value is visible only after its declaration.
        let value = counted("size2", "size");
        let block = Block(vec![declare(&value, global("size")), Return::new(vec![value.clone().into()]).into()]);
        run(&block);
        assert_eq!(value.to_string(), "size");
    }

    #[test]
    fn method_parameters_compact_after_self_and_captures_block_compaction() {
        let receiver = local("self");
        let parameter = counted("p2", "p");
        let block = function(vec![receiver.clone(), parameter.clone()], vec![
            Return::new(vec![receiver.clone().into(), parameter.clone().into()]).into(),
        ]);
        run(&block);
        assert_eq!(parameter.to_string(), "p");
        // An outer `p` read inside the inner function keeps the inner `p2`.
        let outer = local("p");
        let inner = counted("p2", "p");
        let block = function(vec![outer.clone()], vec![Return::new(vec![closure(
            vec![inner.clone()],
            Block(vec![Return::new(vec![outer.clone().into(), inner.clone().into()]).into()]),
        )])
        .into()]);
        run(&block);
        assert_eq!(inner.to_string(), "p2");
    }

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.into())))
    }

    #[test]
    fn retained_arithmetic_names_are_weak_final_roles_with_source_and_collision_guards() {
        use crate::{Binary, BinaryOperation as Op};
        let half = |value: RValue| -> RValue { Binary::new(value, Literal::Number(2.0).into(), Op::Div).into() };
        let axes = half(Binary::new(local("X").into(), local("X2").into(), Op::Add).into());
        assert_eq!(arithmetic_snapshot_role(&axes).as_deref(), Some("midpointX"));
        let size = local("extentsSize");
        let first = local("v"); let middle = local("v2"); let written = local("v3"); let sourced = local("v4");
        let generic = local("v5");
        sourced.0.lock().add_source_binding(crate::SourceBinding {
            name: "v4".into(), origin: crate::BindingOrigin::DebugLocal { prototype: 0, register: 4, start_pc: 0, end_pc: 30 },
        });
        let sum = Binary::new(global("low"), global("high"), Op::Add).into();
        let block = Block(vec![
            declare(&size, global("extent")), declare(&first, half(size.clone().into())),
            declare(&middle, half(sum)), declare(&written, half(size.clone().into())),
            Assign::new(vec![written.clone().into()], vec![Literal::Number(7.0).into()]).into(),
            declare(&sourced, half(size.clone().into())), declare(&generic, half(local("vector2").into())),
            Call::new(global("halfExtentsSize"), vec![first.clone().into(), middle.clone().into()]).into(),
            Call::new(global("print"), vec![written.clone().into(), generic.clone().into()]).into(),
        ]);
        let report = refine_final_names(&block, Options { emit_report: true, ..Default::default() });
        assert_eq!(first.to_string(), "halfExtentsSize2");
        assert_eq!(middle.to_string(), "midpoint");
        // No role, so only their counters compact into the names given up.
        assert_eq!(written.to_string(), "v");
        assert_eq!(sourced.to_string(), "v4");
        assert_eq!(generic.to_string(), "v2");
        let candidates = &report.bindings.iter().find(|b| b.id == middle.stable_id()).unwrap().candidates;
        assert!(candidates.iter().any(|c| c.priority == 40 && c.reason == "retained_arithmetic_snapshot"));
    }
    fn text(value: &str) -> RValue {
        Literal::String(value.as_bytes().to_vec()).into()
    }
    fn global(name: &str) -> RValue {
        Global::from(name).into()
    }
    fn declare(local: &RcLocal, value: RValue) -> Statement {
        Assign {
            node_origin: Default::default(),
            left: vec![local.clone().into()],
            right: vec![value],
            prefix: true,
            parallel: false, compound: false,
        }
        .into()
    }
    fn record(key: &str, value: &RcLocal) -> Statement {
        Return::new(vec![
            Table::new(vec![(Some(text(key)), value.clone().into())]).into()
        ])
        .into()
    }
    fn closure(parameters: Vec<RcLocal>, body: Block) -> RValue {
        Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function {
                parameters,
                body,
                ..Default::default()
            }))),
            upvalues: vec![],
        }
        .into()
    }
    fn function(parameters: Vec<RcLocal>, statements: Vec<Statement>) -> Block {
        Block(vec![Return::new(vec![closure(
            parameters,
            Block(statements),
        )])
        .into()])
    }
    fn assertion(condition: RValue, message: &str) -> Statement {
        Call::new(global("assert"), vec![condition, text(message)]).into()
    }
    fn run(block: &Block) -> Report {
        refine_final_names(
            block,
            Options {
                emit_report: true,
                ..Default::default()
            },
        )
    }

    #[test]
    fn module_and_defaults_tables_are_named_by_contents_and_use() {
        let require = |path: &[&str]| -> RValue {
            let mut node = global("package");
            for key in path {
                node = Index::new(node, text(key)).into();
            }
            Call::new(global("require"), vec![node]).into()
        };
        let (components, modules, defaults, merged, merged_again, props) =
            (local("v1"), local("v2"), local("v3"), local("v4"), local("v5"), local("p"));
        let block = function(vec![props.clone()], vec![
            declare(&components, Table::new(vec![
                (Some(text("Stroke")), require(&["Components", "Base", "Stroke"])),
                (Some(text("Shine")), require(&["Components", "Effects", "Shine"])),
            ]).into()),
            declare(&modules, Table::new(vec![(Some(text("Util")), require(&["Util"]))]).into()),
            declare(&defaults, Table::new(vec![(Some(text("Size")), global("size"))]).into()),
            declare(&merged, Call::new(Index::new(global("Utils"), text("merge")).into(),
                vec![defaults.clone().into(), props.clone().into()]).into()),
            // Merging onto a call's result names nothing.
            declare(&merged_again, Call::new(Index::new(global("Utils"), text("merge")).into(),
                vec![merged.clone().into(), props.clone().into()]).into()),
            Call::new(global("print"), vec![components.clone().into(), modules.clone().into(), merged_again.clone().into()]).into(),
        ]);
        run(&block);
        assert_eq!(components.to_string(), "components");
        assert_eq!(modules.to_string(), "modules");
        assert_eq!(defaults.to_string(), "defaults");
        // No role; its counter compacts into the names given up.
        assert_eq!(merged.to_string(), "v");
    }

    #[test]
    fn a_constructor_noun_yields_only_to_strong_role_evidence() {
        let computed = |name: &str| local(name);
        let (bound, read, unbound, reader) = (computed("computed"), computed("v2"), computed("computed2"), local("use"));
        let construct = || -> RValue { Call::new(global("Computed"), vec![]).into() };
        let block = function(vec![reader.clone()], vec![
            declare(&bound, construct()),
            declare(&unbound, construct()),
            declare(&read, Call::new(reader.clone().into(), vec![bound.clone().into()]).into()),
            Call::new(global("print"), vec![read.clone().into(), unbound.clone().into()]).into(),
            Return::new(vec![Table::new(vec![(Some(text("Text")), bound.clone().into())]).into()]).into(),
        ]);
        run(&block);
        assert_eq!(bound.to_string(), "text");
        assert_eq!(read.to_string(), "currentText");
        // A state read of it is no reason to rename the state itself.
        assert_eq!(unbound.to_string(), "computed2");
    }

    #[test]
    fn a_state_read_is_named_after_the_settled_state() {
        let (reader, state, value, written) = (local("use"), local("v1"), local("v2"), local("v3"));
        let read = |default: Option<RValue>| {
            let call: RValue = Call::new(reader.clone().into(), vec![state.clone().into()]).into();
            match default {
                Some(default) => crate::Binary::new(call, default, crate::BinaryOperation::Or).into(),
                None => call,
            }
        };
        let block = function(vec![reader.clone()], vec![
            declare(&state, Call::new(global("Value"), vec![]).into()),
            declare(&value, read(Some(global("fallback")))),
            declare(&written, read(None)),
            Assign::new(vec![written.clone().into()], vec![global("other")]).into(),
            Call::new(global("print"), vec![value.clone().into(), written.clone().into()]).into(),
            record("AnchorPoint", &state),
        ]);
        let report = run(&block);
        assert_eq!(state.to_string(), "anchorPoint");
        assert_eq!(value.to_string(), "currentAnchorPoint");
        // A local written again holds more than the state's value.
        assert_eq!(written.to_string(), "v");
        let candidates = &report.bindings.iter().find(|b| b.id == value.stable_id()).unwrap().candidates;
        assert!(candidates.iter().any(|c| c.reason == "state_current_value" && c.from_binding == Some(state.stable_id())));
    }

    #[test]
    fn inferred_selection_keeps_a_more_specific_existing_name() {
        let value = local("playerGui");
        value.0.lock().4.conditional_result = true;
        let block = Block(vec![declare(&value, Literal::Nil.into()), Return::new(vec![value.clone().into()]).into()]);
        run(&block);
        assert_eq!(value.to_string(), "playerGui");
    }

    #[test]
    fn inferred_selection_yields_to_field_evidence() {
        for selected in [false, true] {
            let value = local("v");
            value.0.lock().4.conditional_result = selected;
            let block = Block(vec![declare(&value, Literal::Nil.into()),
                Assign::new(vec![Index::new(global("object"), text("_CachedFolder")).into()],
                    vec![value.clone().into()]).into(),
                Return::new(vec![value.clone().into()]).into()]);
            run(&block);
            assert_eq!(value.to_string(), "cachedFolder");
        }
    }

    #[test]
    fn diamond_roles_require_both_immutable_inputs_to_agree() {
        for conflict in [false, true] {
            let a = local("p");
            let b = local("p2");
            let selected = local("v");
            let copy = |from: &RcLocal| Block(vec![Assign::new(vec![selected.clone().into()], vec![from.clone().into()]).into()]);
            let block = function(vec![a.clone(), b.clone()], vec![
                assertion(a.clone().into(), "Expected `props`"),
                assertion(b.clone().into(), if conflict { "Expected `state`" } else { "Expected `props`" }),
                declare(&selected, Literal::Nil.into()),
                If::new(global("condition"), copy(&a), copy(&b)).into(),
                Return::new(vec![selected.clone().into()]).into(),
            ]);
            let report = run(&block);
            assert_eq!(selected.to_string(), if conflict { "v" } else { "props3" });
            if !conflict {
                assert!(report.bindings.iter().flat_map(|r| &r.candidates).any(|c| c.reason == "private_diamond_role_consensus"));
            }
        }
    }

    #[test]
    fn fixed_local_return_tuple_propagates_roles_without_merging_identity() {
        for refuse in 0..5 {
            let helper = local("helper");
            let width = local("v");
            let height = local("v2");
            let result = local("v3");
            let second = local("v4");
            let field = |key| Index::new(global("record"), text(key)).into();
            let mut body = vec![declare(&width, field("Width")), declare(&height, field("Height"))];
            if refuse == 1 {
                body.push(Assign::new(vec![width.clone().into()], vec![Literal::Nil.into()]).into());
            }
            if refuse == 2 {
                body.push(If::new(Literal::Boolean(true).into(), Block::default(), Block::default()).into());
            }
            body.push(Return::new(if refuse == 3 { vec![Call::new(global("unknown"), vec![]).into()] }
                else { vec![width.clone().into(), height.clone().into()] }).into());
            let mut statements = vec![declare(&helper, closure(vec![], Block(body)))];
            if refuse == 4 {
                statements.push(Assign::new(vec![helper.clone().into()], vec![global("unknown")]).into());
            }
            statements.push(Assign { node_origin: Default::default(), left: vec![result.clone().into(), second.clone().into()],
                right: vec![Call::new(helper.clone().into(), vec![]).into()], prefix: true, parallel: false, compound: false}.into());
            statements.push(Return::new(vec![result.clone().into(), second.clone().into()]).into());
            let report = run(&Block(statements));
            // The helper's own `width` is not visible where the result is.
            if refuse == 0 {
                assert_eq!(result.to_string(), "width");
            } else {
                assert!(generated(&result.to_string()), "refusal {refuse} named {result}");
            }
            if refuse == 0 {
                assert_eq!(second.to_string(), "height");
                let row = report.bindings.iter().find(|b| b.id == result.stable_id()).unwrap();
                assert!(row.candidates.iter().any(|c| c.reason == "resolved_local_call_result" && c.from_binding == Some(width.stable_id())));
                assert_ne!(width.stable_id(), result.stable_id());
            }
        }
    }

    /// A call the de-inliner rebuilt names its result after the local the
    /// helper returns (`loadTrack` returning `track`), else after the subject
    /// of the helper's name; other role evidence of the result decides first,
    /// and a call that was never inlined keeps its name.
    #[test]
    fn a_rebuilt_call_result_takes_the_name_the_helper_returns() {
        let call = |helper: &RcLocal, arguments: Vec<RValue>, rebuilt: bool| -> RValue {
            let mut call = Call::new(helper.clone().into(), arguments);
            call.rebuilt = rebuilt.then_some(crate::call_origins::Kind::StatementDeinline);
            call.into()
        };
        let make = || -> RValue { Call::new(global("make"), vec![]).into() };
        // local function loadTrack() local track = make(); return track end
        let (load, track) = (local("loadTrack"), local("track"));
        let load_body = Block(vec![declare(&track, make()), Return::new(vec![track.clone().into()]).into()]);
        // local function getOwnPlot(plot) if plot then return plot end return nil end
        let (get, plot) = (local("getOwnPlot"), local("plot"));
        let get_body = Block(vec![
            If::new(plot.clone().into(), Block(vec![Return::new(vec![plot.clone().into()]).into()]), Block::default()).into(),
            Return::new(vec![Literal::Nil.into()]).into(),
        ]);
        // local function fade(frame) return frame end
        let (fade, frame) = (local("fade"), local("frame"));
        let fade_body = Block(vec![Return::new(vec![frame.clone().into()]).into()]);
        let (first, second, plain, owned, faded, recorded) =
            (local("v"), local("v2"), local("v3"), local("v4"), local("v5"), local("v6"));
        let caller = Block(vec![
            declare(&first, call(&load, vec![], true)),
            declare(&second, call(&load, vec![], true)),
            declare(&plain, call(&load, vec![], false)),
            declare(&owned, call(&get, vec![make()], true)),
            declare(&faded, call(&fade, vec![make()], true)),
            declare(&recorded, call(&load, vec![], true)),
            Call::new(global("print"), vec![first.clone().into(), second.clone().into(), plain.clone().into(),
                owned.clone().into(), faded.clone().into()]).into(),
            record("Animation", &recorded),
        ]);
        run(&Block(vec![
            declare(&load, closure(vec![], load_body)),
            declare(&get, closure(vec![plot.clone()], get_body)),
            declare(&fade, closure(vec![frame.clone()], fade_body)),
            Return::new(vec![closure(vec![], caller)]).into(),
        ]));
        let names = [&first, &second, &plain, &owned, &faded, &recorded].map(|local| local.to_string());
        assert_eq!(names, ["track", "track2", "v", "ownPlot", "v2", "animation"]);
    }

    #[test]
    fn direct_field_result_slots_survive_prior_temp_inlining() {
        let helper = local("measures");
        let width = local("v");
        let height = local("v2");
        let reads = vec![Index::new(global("record"), text("Width")).into(),
            Index::new(global("record"), text("Height")).into()];
        let block = Block(vec![declare(&helper, closure(vec![], Block(vec![Return::new(reads).into()]))),
            Assign { node_origin: Default::default(), left: vec![width.clone().into(), height.clone().into()],
                right: vec![RValue::Select(Select::Call(Call::new(helper.clone().into(), vec![])))], prefix: true, parallel: false, compound: false}.into()]);
        let report = run(&block);
        assert_eq!(width.to_string(), "width");
        assert_eq!(height.to_string(), "height");
        assert_eq!(block.to_string().matches("record.Width").count(), 1);
        assert_eq!(block.to_string().matches("record.Height").count(), 1);
        assert_eq!(report.renamed, 2);
    }

    #[test]
    fn ref_captured_helper_resolves_only_without_rebinding_in_any_closure() {
        for rebound in [false, true] {
            let helper = local("measures");
            let width = local("v");
            let height = local("v2");
            let mut body = vec![Assign { node_origin: Default::default(), left: vec![width.clone().into(), height.clone().into()],
                right: vec![Call::new(helper.clone().into(), vec![]).into()], prefix: true, parallel: false, compound: false}.into()];
            if rebound {
                body.push(Assign::new(vec![helper.clone().into()], vec![global("unknown")]).into());
            }
            body.push(Return::new(vec![width.clone().into(), height.clone().into()]).into());
            let mut returned = closure(vec![], Block(body));
            if let RValue::Closure(closure) = &mut returned { closure.upvalues.push(Upvalue::Ref(helper.clone())); }
            let block = Block(vec![declare(&helper, closure(vec![], Block(vec![Return::new(vec![
                Index::new(global("record"), text("Width")).into(),
                Index::new(global("record"), text("Height")).into(),
            ]).into()]))), Return::new(vec![returned]).into()]);
            run(&block);
            assert_eq!(width.to_string(), if rebound { "v" } else { "width" });
            assert_eq!(height.to_string(), if rebound { "v2" } else { "height" });
        }
    }

    #[test]
    fn recorded_numeric_type_is_reported_without_inventing_a_domain_role() {
        let parameter = local("p");
        let mut function = Function { parameters: vec![parameter.clone()],
            parameter_annotations: vec![Some("number".into())],
            parameter_name_hints: vec![Some("number".into())],
            body: Block(vec![Return::new(vec![parameter.clone().into()]).into()]), ..Default::default() };
        let block = Block(vec![Return::new(vec![Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(std::mem::take(&mut function)))), upvalues: vec![],
        }.into()]).into()]);
        let report = run(&block);
        assert_eq!(parameter.to_string(), "p");
        assert_eq!(report.bindings[0].type_evidence.len(), 2);
        assert!(report.bindings[0].type_evidence.iter().all(|e| e.representation == "number"));
    }

    #[test]
    fn assertion_and_record_candidates_keep_evidence_and_identity() {
        let component = local("p");
        let id = component.stable_id();
        let block = function(
            vec![component.clone()],
            vec![
                assertion(component.clone().into(), "Expected `component`"),
                record("component", &component),
            ],
        );
        let report = run(&block);
        assert_eq!(component.to_string(), "component");
        assert_eq!(component.stable_id(), id);
        assert_eq!(report.renamed, 1);
        assert!(report.bindings[0]
            .candidates
            .iter()
            .any(|c| c.reason == "assertion_parameter"));
        assert!(report.bindings[0]
            .candidates
            .iter()
            .any(|c| c.reason == "record_field_value"));
    }

    #[test]
    fn warning_keys_are_not_parameter_roles_but_private_fields_are() {
        assert_eq!(field_role("EXTREMELY_DANGEROUS_usedAsValue"), None);
        assert_eq!(field_role("MAX_SPEED"), None);
        assert_eq!(field_role("_scope"), Some("scope".into()));
        assert_eq!(
            field_role("BackgroundColor3"),
            Some("backgroundColor".into())
        );
    }

    #[test]
    fn equal_priority_conflict_refuses_and_ambiguous_assertions_do_not_guess() {
        let width = local("p");
        let other = local("p2");
        let block = function(
            vec![width.clone(), other.clone()],
            vec![
                assertion(
                    Binary::new(
                        width.clone().into(),
                        other.clone().into(),
                        BinaryOperation::And,
                    )
                    .into(),
                    "Expected `height`",
                ),
                assertion(other.clone().into(), "Expected `left` or `right`"),
                record("Width", &width),
                record("Height", &width),
            ],
        );
        let report = run(&block);
        assert_eq!(width.to_string(), "p");
        assert_eq!(other.to_string(), "p2");
        assert_eq!(report.conflicts, 1);
    }

    #[test]
    fn source_and_method_receiver_are_protected() {
        let p = local("p");
        p.0.lock().add_source_binding(SourceBinding {
            name: "p".into(),
            origin: BindingOrigin::DebugLocal {
                prototype: 0,
                register: 0,
                start_pc: 0,
                end_pc: 10,
            },
        });
        let receiver = local("self");
        let block = function(
            vec![p.clone(), receiver.clone()],
            vec![record("component", &p), record("props", &receiver)],
        );
        let report = run(&block);
        assert_eq!(p.to_string(), "p");
        assert_eq!(receiver.to_string(), "self");
        assert_eq!(report.renamed, 0);
        assert_eq!(report.bindings[0].status, "source_protected");
    }

    #[test]
    fn copies_propagate_roles_but_mutable_cells_do_not() {
        for mutable in [false, true] {
            let parameter = local("p");
            let copy = local("v");
            let mut body = vec![declare(&copy, parameter.clone().into())];
            if mutable {
                body.push(
                    Assign::new(vec![parameter.clone().into()], vec![Literal::Nil.into()]).into(),
                );
            }
            body.push(record("props", &copy));
            let report = run(&function(vec![parameter.clone()], body));
            assert_ne!(parameter.stable_id(), copy.stable_id());
            assert_eq!(copy.to_string(), if mutable { "props" } else { "props2" });
            assert_eq!(parameter.to_string(), if mutable { "p" } else { "props" });
            assert_eq!(report.refused_edges, usize::from(mutable));
        }
    }

    #[test]
    fn sibling_scopes_can_reuse_but_captures_and_globals_cannot() {
        for unique in [false, true] {
            let a = local("v");
            let b = local("v2");
            let child =
                |p: &RcLocal| Block(vec![declare(p, Literal::Nil.into()), record("props", p)]);
            let block = Block(vec![If::new(
                Literal::Boolean(true).into(),
                child(&a),
                child(&b),
            )
            .into()]);
            refine_final_names(
                &block,
                Options {
                    dont_reuse_var: unique,
                    ..Default::default()
                },
            );
            assert_eq!(a.to_string(), "props");
            assert_eq!(b.to_string(), if unique { "props2" } else { "props" });
        }
        let a = local("p");
        let inner = local("props");
        let block = function(
            vec![a.clone()],
            vec![
                Call::new(global("props2"), vec![]).into(),
                Return::new(vec![closure(vec![inner], Block(vec![record("props", &a)]))]).into(),
            ],
        );
        run(&block);
        assert_eq!(a.to_string(), "props3");
    }

    #[test]
    fn static_module_key_frees_lowercase_parameter_name() {
        let key = local("children");
        let parameter = local("p");
        let path = Index {
            node_origin: Default::default(),
            left: Box::new(global("script")),
            right: Box::new(text("Children")),
        }
        .into();
        let body = vec![
            assertion(parameter.clone().into(), "Expected `children`"),
            Return::new(vec![Table::new(vec![(
                Some(key.clone().into()),
                parameter.clone().into(),
            )])
            .into()])
            .into(),
        ];
        let block = Block(vec![
            declare(&key, Call::new(global("require"), vec![path]).into()),
            Return::new(vec![closure(vec![parameter.clone()], Block(body))]).into(),
        ]);
        run(&block);
        assert_eq!(key.to_string(), "Children");
        assert_eq!(parameter.to_string(), "children");
    }

    #[test]
    fn resolved_local_calls_connect_only_immutable_exact_positions() {
        for dynamic in [false, true] {
            let helper = local("helper");
            let parameter = local("p");
            let argument = local("p2");
            let call = Call::new(
                if dynamic {
                    global("helper")
                } else {
                    helper.clone().into()
                },
                vec![argument.clone().into()],
            );
            let block = function(
                vec![argument.clone()],
                vec![
                    declare(
                        &helper,
                        closure(
                            vec![parameter.clone()],
                            Block(vec![record("props", &parameter)]),
                        ),
                    ),
                    call.into(),
                ],
            );
            let report = run(&block);
            // Unresolved, the argument only compacts once the helper's `p`
            // became `props`.
            assert_eq!(argument.to_string(), if dynamic { "p" } else { "props" });
            assert!(
                dynamic
                    || report.bindings.iter().any(|b| b
                        .candidates
                        .iter()
                        .any(|c| c.reason == "resolved_local_call_argument"))
            );
        }
    }

    #[test]
    fn budget_exhaustion_is_atomic_and_multiple_declarations_are_unknown() {
        let p = local("p");
        let block = function(
            vec![p.clone()],
            vec![record("props", &p), record("props", &p)],
        );
        for options in [
            Options {
                node_budget: 4,
                ..Default::default()
            },
            Options {
                binding_budget: 0,
                ..Default::default()
            },
            Options {
                depth_budget: 0,
                ..Default::default()
            },
        ] {
            let report = refine_final_names(&block, options);
            assert!(report.budget_exhausted);
            assert_eq!(report.renamed, 0);
            assert_eq!(p.to_string(), "p");
        }
        let block = Block(vec![
            declare(&p, Literal::Nil.into()),
            declare(&p, Literal::Nil.into()),
            record("props", &p),
        ]);
        let report = run(&block);
        assert_eq!(p.to_string(), "p");
        assert_eq!(report.bindings[0].status, "unknown_owner");
    }

    #[test]
    fn implicit_setlist_global_and_external_references_are_reserved() {
        let p = local("p");
        let target = local("target");
        let external = local("tbl");
        let block = function(
            vec![p.clone()],
            vec![
                assertion(p.clone().into(), "Expected `table`"),
                crate::SetList::new(target.clone(), 1, vec![], Some(crate::VarArg {}.into())).into(),
                Return::new(vec![external.into()]).into(),
            ],
        );
        run(&block);
        // A builtin's spelling reads as its alternative, and the undeclared
        // `tbl` is reserved everywhere.
        assert_eq!(p.to_string(), "tbl2");
        // The open SETLIST prints `table.pack`: an alias counted from `table`
        // keeps its counter.
        let alias = counted("table2", "table");
        let block = Block(vec![
            declare(&alias, global("table")),
            crate::SetList::new(target, 1, vec![], Some(crate::VarArg {}.into())).into(),
            Return::new(vec![alias.clone().into()]).into(),
        ]);
        run(&block);
        assert_eq!(alias.to_string(), "table2");
    }
}
