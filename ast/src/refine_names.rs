//! Bounded role inference on the final binding graph. This pass only changes
//! Local.0: no expression, identity, capture mode, field, global or type changes.
//! Candidate priorities rank evidence; they are not probabilities or effect facts.
use std::collections::{BTreeMap, BTreeSet};

use crate::{Block, Call, LValue, Literal, RValue, RcLocal, Select, Statement, Traverse, Upvalue};

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
    ref_capture: bool,
    source_protected: bool,
    key_use: bool,
    module_leaf: Option<String>,
    candidates: Vec<Candidate>,
    type_evidence: Vec<TypeEvidence>,
    overflow: bool,
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
    globals: BTreeSet<String>,
    copies: Vec<(u64, u64)>,
    functions: BTreeMap<u64, FunctionRoles>,
    results: Vec<ResultUse>,
    reads: Vec<(u64, String)>,
    joins: Vec<(u64, u64, u64)>,
    calls: Vec<(Option<u64>, Vec<Option<u64>>)>,
    report: Report,
}

fn generated(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('p' | 'v')) && chars.all(|c| c.is_ascii_digit())
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
            let mut candidates = vec![Candidate {
                name: before.clone(),
                priority: 20,
                reason: "prior_deterministic_namer",
                witness: "selected legacy name; legacy alternatives are not collected here".into(),
                from_binding: None,
            }];
            for source in &data.2 {
                candidates.push(Candidate {
                    name: source.name.clone(),
                    priority: 255,
                    reason: "recorded_source_binding",
                    witness: format!("{:?}", source.origin),
                    from_binding: None,
                });
            }
            if data.4.conditional_result && !data.4.parameter && generated(&before) {
                candidates.push(Candidate {
                    name: "selected".into(), priority: 40,
                    reason: "preserved_conditional_result",
                    witness: "private two-arm SSA join; returned and observed separately; no source spelling claim".into(),
                    from_binding: None,
                });
            }
            let type_evidence = data.1.as_ref().map(|hint| TypeEvidence {
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
                    ref_capture: false,
                    source_protected,
                    key_use: false,
                    module_leaf: None,
                    candidates,
                    type_evidence,
                    overflow: false,
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
        }
    }

    fn candidate(&mut self, id: u64, candidate: Candidate) {
        if !useful(&candidate.name) {
            return;
        }
        let Some(node) = self.nodes.get_mut(&id) else {
            return;
        };
        if let Some(existing) = node.candidates.iter_mut().find(|c| {
            c.name == candidate.name
                && c.reason == candidate.reason
                && c.from_binding == candidate.from_binding
        }) {
            if candidate.priority > existing.priority {
                *existing = candidate;
            }
        } else if node.candidates.len() < 24 {
            node.candidates.push(candidate);
        } else {
            node.overflow = true;
        }
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
                witness: key.into(),
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
                    witness: format!("assert guard with one binding; message identifier `{name}`"),
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
                self.node(local);
            }
            RValue::Global(global) => {
                self.globals
                    .insert(String::from_utf8_lossy(&global.0).into());
            }
            RValue::Closure(closure) => {
                for upvalue in &closure.upvalues {
                    let (Upvalue::Copy(local) | Upvalue::Ref(local)) = upvalue;
                    if let Some(node) = self.node(local) {
                        node.ref_capture |= matches!(upvalue, Upvalue::Ref(_));
                    }
                }
                let child = self.child_scope(scope);
                let function = closure.function.lock();
                for (index, parameter) in function.parameters.iter().enumerate() {
                    self.declare(parameter, child, "parameter");
                    if let Some(node) = self.nodes.get_mut(&parameter.stable_id()) {
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
                self.block(&function.body, child, depth + 1);
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
        for child in value.rvalues() {
            self.expression(child, scope, depth + 1);
        }
    }

    fn child_scope(&mut self, parent: usize) -> usize {
        let child = self.scopes.len();
        self.scopes.push(Some(parent));
        child
    }

    fn child_block(&mut self, block: &Block, parent: usize, depth: usize) {
        let child = self.child_scope(parent);
        self.block(block, child, depth + 1);
    }

    fn block(&mut self, block: &Block, scope: usize, depth: usize) {
        for (index, statement) in block.iter().enumerate() {
            if !self.visit(depth) {
                return;
            }
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
                                if assign.prefix {
                                    self.declare(local, scope, "local");
                                }
                                if let Some(node) = self.node(local) {
                                    node.writes += 1;
                                }
                            }
                            LValue::Global(global) => {
                                self.globals
                                    .insert(String::from_utf8_lossy(&global.0).into());
                            }
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
                                        witness: "final expression shape; naming context only, not a numeric or motion proof".into(),
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
                    self.node(&list.object_local);
                    if list.tail.is_some() {
                        self.globals.insert("table".into());
                    }
                }
                Statement::Close(close) => {
                    for local in &close.locals {
                        self.node(local);
                    }
                    self.globals.insert("__close_uv".into());
                }
                // Repeat's condition is in its body's scope.
                Statement::Repeat(repeat) => {
                    let child = self.child_scope(scope);
                    self.block(&repeat.block.lock(), child, depth + 1);
                    self.expression(&repeat.condition, child, depth + 1);
                    continue;
                }
                _ => {}
            }
            for value in statement.rvalues() {
                self.expression(value, scope, depth + 1);
            }
            match statement {
                Statement::If(branch) => {
                    self.child_block(&branch.then_block.lock(), scope, depth);
                    self.child_block(&branch.else_block.lock(), scope, depth);
                }
                Statement::While(loop_) => self.child_block(&loop_.block.lock(), scope, depth),
                Statement::NumericFor(loop_) => {
                    let child = self.child_scope(scope);
                    self.declare(&loop_.counter, child, "iteration");
                    self.block(&loop_.block.lock(), child, depth + 1);
                }
                Statement::GenericFor(loop_) => {
                    let child = self.child_scope(scope);
                    for local in &loop_.res_locals {
                        self.declare(local, child, "iteration");
                    }
                    self.block(&loop_.block.lock(), child, depth + 1);
                }
                _ => {}
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

    fn propagate(&mut self) {
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
                witness: format!("fixed return field slot in helper b{callee}; role only, field evaluation is unchanged"),
                from_binding: Some(callee) });
        }
        for (id, name) in std::mem::take(&mut self.reads) {
            if self.immutable(id) {
                self.candidate(id, Candidate { name, priority: 80, reason: "record_field_read",
                    witness: "literal field read assigned to an immutable local; role only".into(), from_binding: None });
            } else { self.report.refused_edges += 1; }
        }
        let joins: Vec<_> = std::mem::take(&mut self.joins).into_iter().filter(|&(dest, a, b)| {
            let valid = self.immutable(a) && self.immutable(b) && self.nodes.get(&dest).is_some_and(|node|
                node.kind == "local" && node.scope.is_some() && !node.ambiguous_owner && !node.ref_capture && node.writes == 3);
            if !valid { self.report.refused_edges += 1; }
            valid
        }).collect();
        for _ in 0..4 {
            let mut pending = Vec::new();
            for &(dest, a, b) in &joins {
                let best = |id| {
                    let candidates = &self.nodes[&id].candidates;
                    let priority = candidates.iter().filter(|c| useful(&c.name)).map(|c| c.priority).max()?;
                    let names: BTreeSet<_> = candidates.iter().filter(|c| c.priority == priority).map(|c| &c.name).collect();
                    (priority >= 40 && names.len() == 1).then(|| ((*names.first().unwrap()).clone(), priority))
                };
                if let (Some((name, x)), Some((other, y))) = (best(a), best(b)) {
                    if name == other {
                        pending.push((dest, Candidate { name, priority: x.min(y).min(66) - 1,
                            reason: "private_diamond_role_consensus",
                            witness: format!("immutable inputs b{a} and b{b}; complete adjacent assignment diamond; role only"),
                            from_binding: None }));
                    }
                }
            }
            for &(source, target, reason) in &edges {
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
                            witness: "role only; binding identities remain distinct".into(),
                            from_binding: Some(source),
                        },
                    ));
                }
            }
            for (id, candidate) in pending {
                self.candidate(id, candidate);
            }
        }
    }

    fn interfere(&self, first: Option<usize>, second: Option<usize>) -> bool {
        if self.options.dont_reuse_var || first.is_none() || second.is_none() {
            return true;
        }
        let ancestor = |a, mut b| loop {
            if a == b {
                return true;
            }
            match self.scopes[b] {
                Some(parent) => b = parent,
                None => return false,
            }
        };
        ancestor(first.unwrap(), second.unwrap()) || ancestor(second.unwrap(), first.unwrap())
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
                            witness: "static script path leaf used as a table key".into(),
                            from_binding: None,
                        },
                    );
                }
            }
            self.propagate();
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
            } else if !(generated(&node.before) || generic_parameter || module_key) {
                "kept_existing_role"
            } else {
                let best = node
                    .candidates
                    .iter()
                    .filter(|c| c.priority >= 40 && (c.reason != "retained_arithmetic_snapshot" || node.writes == 1))
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
            statuses.insert(id, status);
        }
        // Reserve unchanged names first, including descendants and external
        // bindings. New names can never capture an existing global or local.
        let mut reserved: BTreeMap<String, Vec<Option<usize>>> = BTreeMap::new();
        for (&id, node) in &self.nodes {
            if !proposals.contains_key(&id) {
                reserved
                    .entry(node.before.clone())
                    .or_default()
                    .push(node.scope);
            }
        }
        for id in &self.order {
            if let Some(base) = proposals.get(id) {
                let node = &self.nodes[id];
                let mut name = base.clone();
                let mut suffix = 2;
                while self.globals.contains(&name)
                    || reserved.get(&name).is_some_and(|scopes| {
                        scopes
                            .iter()
                            .any(|&scope| self.interfere(node.scope, scope))
                    })
                {
                    name = format!("{base}{suffix}");
                    suffix += 1;
                }
                node.local.0.lock().0 = Some(name.clone());
                reserved.entry(name).or_default().push(node.scope);
                statuses.insert(*id, "renamed");
                self.report.renamed += 1;
            }
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

pub fn refine_final_names(block: &Block, options: Options) -> Report {
    let mut graph = Graph {
        options,
        nodes: BTreeMap::new(),
        order: Vec::new(),
        scopes: vec![None],
        globals: BTreeSet::new(),
        copies: Vec::new(),
        functions: BTreeMap::new(),
        results: Vec::new(),
        reads: Vec::new(),
        joins: Vec::new(),
        calls: Vec::new(),
        report: Report::default(),
    };
    graph.block(block, 0, 0);
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
        ]);
        let report = refine_final_names(&block, Options { emit_report: true, ..Default::default() });
        assert_eq!(first.to_string(), "halfExtentsSize2");
        assert_eq!(middle.to_string(), "midpoint");
        assert_eq!(written.to_string(), "v3");
        assert_eq!(sourced.to_string(), "v4");
        assert_eq!(generic.to_string(), "v5");
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
            parallel: false,
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
                right: vec![Call::new(helper.clone().into(), vec![]).into()], prefix: true, parallel: false }.into());
            let report = run(&Block(statements));
            assert_eq!(result.to_string(), if refuse == 0 { "width2" } else { "v3" });
            if refuse == 0 {
                assert_eq!(second.to_string(), "height2");
                let row = report.bindings.iter().find(|b| b.id == result.stable_id()).unwrap();
                assert!(row.candidates.iter().any(|c| c.reason == "resolved_local_call_result" && c.from_binding == Some(width.stable_id())));
                assert_ne!(width.stable_id(), result.stable_id());
            }
        }
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
                right: vec![RValue::Select(Select::Call(Call::new(helper.clone().into(), vec![])))], prefix: true, parallel: false }.into()]);
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
                right: vec![Call::new(helper.clone().into(), vec![]).into()], prefix: true, parallel: false }.into()];
            if rebound {
                body.push(Assign::new(vec![helper.clone().into()], vec![global("unknown")]).into());
            }
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
            assert_eq!(argument.to_string(), if dynamic { "p2" } else { "props" });
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
        let external = local("table2");
        let block = function(
            vec![p.clone()],
            vec![
                assertion(p.clone().into(), "Expected `table`"),
                crate::SetList::new(target, 1, vec![], Some(crate::VarArg {}.into())).into(),
                Return::new(vec![external.into()]).into(),
            ],
        );
        run(&block);
        assert_eq!(p.to_string(), "table3");
    }
}
