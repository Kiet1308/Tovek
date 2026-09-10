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
    overflow: bool,
}

struct Graph {
    options: Options,
    nodes: BTreeMap<u64, Node>,
    order: Vec<u64>,
    scopes: Vec<Option<usize>>,
    globals: BTreeSet<String>,
    copies: Vec<(u64, u64)>,
    functions: BTreeMap<u64, Vec<u64>>,
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
                for parameter in &function.parameters {
                    self.declare(parameter, child, "parameter");
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
        for statement in block.iter() {
            if !self.visit(depth) {
                return;
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
                    for (left, right) in assign.left.iter().zip(&assign.right) {
                        match left {
                            LValue::Index(index) => self.role(&index.right, right),
                            LValue::Local(local) => {
                                if let RValue::Local(source) = right {
                                    self.copies.push((local.stable_id(), source.stable_id()));
                                }
                                if let Some(node) = self.nodes.get_mut(&local.stable_id()) {
                                    node.module_leaf = module_leaf(right);
                                }
                                if let RValue::Closure(closure) = right {
                                    self.functions.insert(
                                        local.stable_id(),
                                        closure
                                            .function
                                            .lock()
                                            .parameters
                                            .iter()
                                            .map(RcLocal::stable_id)
                                            .collect(),
                                    );
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
            let Some(id) = callee.filter(|id| self.immutable(*id)) else {
                self.report.unresolved_calls += 1;
                continue;
            };
            let Some(parameters) = self.functions.get(&id) else {
                self.report.unresolved_calls += 1;
                continue;
            };
            // Only exact-arity local arguments are connected, never guessed tail
            // results, dynamic dispatch, writes or mutable capture cells.
            if parameters.len() != arguments.len() {
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
        for _ in 0..4 {
            let mut pending = Vec::new();
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
                    .filter(|c| c.priority >= 40)
                    .map(|c| c.priority)
                    .max();
                let names: BTreeSet<_> = node
                    .candidates
                    .iter()
                    .filter(|c| Some(c.priority) == best)
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
    fn text(value: &str) -> RValue {
        Literal::String(value.as_bytes().to_vec()).into()
    }
    fn global(name: &str) -> RValue {
        Global::from(name).into()
    }
    fn declare(local: &RcLocal, value: RValue) -> Statement {
        Assign {
            left: vec![local.clone().into()],
            right: vec![value],
            prefix: true,
            parallel: false,
        }
        .into()
    }
    fn record(key: &str, value: &RcLocal) -> Statement {
        Return::new(vec![
            Table(vec![(Some(text(key)), value.clone().into())]).into()
        ])
        .into()
    }
    fn closure(parameters: Vec<RcLocal>, body: Block) -> RValue {
        Closure {
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
            left: Box::new(global("script")),
            right: Box::new(text("Children")),
        }
        .into();
        let body = vec![
            assertion(parameter.clone().into(), "Expected `children`"),
            Return::new(vec![Table(vec![(
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
