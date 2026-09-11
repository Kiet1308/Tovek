//! Lower existing scalar IfExpression nodes at their evaluation point.
//!
//! This is independent of disabling conditional reconstruction. It runs after
//! expression cleanup, so fresh snapshots cannot be inlined again. The v9 Luau
//! compiler's register-reuse positions (binary/index operands and NAMECALL
//! receiver) are distinct from ordinary call/tuple positions.
use std::collections::BTreeMap;

use rustc_hash::FxHashSet;
use serde::Serialize;
use crate::local_producers::{Ledger, Role};

use crate::{
    Assign, BinaryOperation, Block, If, LValue, Literal, Local, RValue, RcLocal, Select, Statement,
    Traverse, Unary, UnaryOperation,
};

const NODE_LIMIT: usize = 200_000;
const DEPTH_LIMIT: usize = 128;
const LOCAL_LIMIT: usize = 192;

#[derive(Default, Debug, Serialize)]
pub struct Report {
    pub model: &'static str,
    pub input_selects: usize,
    pub lowered_selects: usize,
    pub introduced_locals: usize,
    pub introduced_bindings: Ledger,
    pub refused_statements: BTreeMap<&'static str, usize>,
    pub budget_exhausted: bool,
}

struct Inventory {
    nodes: usize,
    selects: usize,
    collect_names: bool,
    names: FxHashSet<String>,
    functions: FxHashSet<usize>,
    probe: Option<fn(&Block) -> bool>,
    found: bool,
    captured: Option<FxHashSet<u64>>,
}

impl Inventory {
    fn tick(&mut self, depth: usize) -> Result<(), &'static str> {
        self.nodes += 1;
        if self.nodes > NODE_LIMIT || depth > DEPTH_LIMIT {
            Err("tree_budget")
        } else {
            Ok(())
        }
    }

    fn local(&mut self, local: &RcLocal) {
        if !self.collect_names {
            return;
        }
        if let Some(name) = &local.0.lock().0 {
            self.reserve(name);
        }
    }

    fn reserve(&mut self, name: &str) {
        // Generated names are much shorter. Long input names cannot collide and
        // need not be copied into a second unbounded string store.
        if self.collect_names && name.len() <= 64 {
            self.names.insert(name.into());
        }
    }

    fn value(&mut self, value: &RValue, depth: usize) -> Result<(), &'static str> {
        self.tick(depth)?;
        match value {
            RValue::IfExpression(_) => self.selects += 1,
            RValue::Local(local) => self.local(local),
            RValue::Global(global) => {
                if let Ok(name) = std::str::from_utf8(&global.0) {
                    self.reserve(name);
                }
            }
            RValue::Closure(closure) => {
                if let Some(captured) = &mut self.captured {
                    if closure.upvalues.len() > NODE_LIMIT.saturating_sub(self.nodes) {
                        return Err("tree_budget");
                    }
                    self.nodes += closure.upvalues.len();
                    for upvalue in &closure.upvalues {
                        let (crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local)) = upvalue;
                        captured.insert(local.stable_id());
                    }
                }
                let id = (&*closure.function.0 as *const _) as usize;
                if self.functions.insert(id) {
                    let function = closure.function.lock();
                    if function.parameters.len() > NODE_LIMIT.saturating_sub(self.nodes) {
                        return Err("tree_budget");
                    }
                    self.nodes += function.parameters.len();
                    for local in &function.parameters {
                        self.local(local);
                    }
                    self.block(&function.body, depth + 1)?;
                }
            }
            _ => {}
        }
        // Check width before Traverse allocates a child-reference vector.
        let width = match value {
            RValue::Table(table) => table.0.len().saturating_mul(2),
            RValue::Call(call) | RValue::Select(Select::Call(call)) => call.arguments.len() + 1,
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
                call.arguments.len() + 1
            }
            _ => 3,
        };
        if width > NODE_LIMIT.saturating_sub(self.nodes) {
            return Err("tree_budget");
        }
        for child in value.rvalues() {
            self.value(child, depth + 1)?;
        }
        Ok(())
    }

    fn block(&mut self, block: &Block, depth: usize) -> Result<(), &'static str> {
        self.tick(depth)?;
        if block.0.len() > NODE_LIMIT.saturating_sub(self.nodes) { return Err("tree_budget"); }
        if !self.found && self.probe.is_some_and(|probe| probe(block)) {
            self.found = true;
        }
        for statement in &block.0 {
            self.tick(depth)?;
            let width = match statement {
                Statement::Assign(node) => node.left.len().saturating_add(node.right.len()),
                Statement::Call(node) => node.arguments.len().saturating_add(1),
                Statement::MethodCall(node) => node.arguments.len().saturating_add(1),
                Statement::Return(node) => node.values.len(),
                Statement::GenericFor(node) => {
                    node.right.len().saturating_add(node.res_locals.len())
                }
                _ => 3,
            };
            if width > NODE_LIMIT.saturating_sub(self.nodes) {
                return Err("tree_budget");
            }
            match statement {
                Statement::Assign(assign) => {
                    for left in &assign.left {
                        self.tick(depth)?;
                        match left {
                            LValue::Local(local) => self.local(local),
                            LValue::Global(global) => {
                                if let Ok(name) = std::str::from_utf8(&global.0) {
                                    self.reserve(name);
                                }
                            }
                            LValue::Index(index) => {
                                self.value(&index.left, depth + 1)?;
                                self.value(&index.right, depth + 1)?;
                            }
                        }
                    }
                }
                Statement::NumericFor(node) => self.local(&node.counter),
                Statement::GenericFor(node) => {
                    for local in &node.res_locals {
                        self.local(local);
                    }
                }
                _ => {}
            }
            for value in statement.rvalues() {
                self.value(value, depth + 1)?;
            }
            match statement {
                Statement::If(node) => {
                    self.block(&node.then_block.lock(), depth + 1)?;
                    self.block(&node.else_block.lock(), depth + 1)?;
                }
                Statement::While(node) => self.block(&node.block.lock(), depth + 1)?,
                Statement::Repeat(node) => self.block(&node.block.lock(), depth + 1)?,
                Statement::NumericFor(node) => self.block(&node.block.lock(), depth + 1)?,
                Statement::GenericFor(node) => self.block(&node.block.lock(), depth + 1)?,
                _ => {}
            }
        }
        Ok(())
    }
}

/// A budget refusal leaves the input untouched. Individual unsupported
/// statements keep their expression form; production reconstruction already
/// keeps those regions as statements. No assertion of source recovery is made.
pub fn lower_existing_conditionals(block: &mut Block) -> Report {
    let mut report = Report {
        model: "luau-v9-scalar-select-statements-v1",
        ..Default::default()
    };
    let mut inventory = Inventory {
        nodes: 0,
        selects: 0,
        collect_names: false,
        names: FxHashSet::default(),
        functions: FxHashSet::default(),
        probe: None,
        found: false,
        captured: None,
    };
    if inventory.block(block, 0).is_err() {
        report.budget_exhausted = true;
        return report;
    }
    report.input_selects = inventory.selects;
    if inventory.selects == 0 {
        return report;
    }
    // The production pipeline normally has no remaining selects. Reserve
    // identifiers only when this immutable tree actually needs lowering.
    inventory.nodes = 0;
    inventory.selects = 0;
    inventory.functions.clear();
    inventory.collect_names = true;
    if inventory.block(block, 0).is_err() {
        report.budget_exhausted = true;
        return report;
    }
    let mut state = State {
        reserved: inventory.names,
        next_name: 0,
        visited: FxHashSet::default(),
        report,
    };
    state.function(block, &[]);
    state.report
}

pub(crate) struct Frame {
    pub(crate) locals: FxHashSet<u64>,
    pub(crate) headroom: usize,
}

pub(crate) struct RewriteInventory {
    pub(crate) reserved: FxHashSet<String>,
    pub(crate) captured: FxHashSet<u64>,
}

/// Bound recursive analysis before a pass inspects expression/register costs.
/// This does not reserve names, collect captures, or authorize any rewrite.
pub(crate) fn validate_local_rewrite_tree(block: &Block) -> Result<(), &'static str> {
    Inventory {
        nodes: 0,
        selects: 0,
        collect_names: false,
        names: FxHashSet::default(),
        functions: FxHashSet::default(),
        probe: None,
        found: false,
        captured: None,
    }.block(block, 0)
}

/// Validate the entire tree before a caller performs recursive analysis. Name
/// reservation only runs when the bounded probe finds a possible rewrite.
pub(crate) fn prepare_local_rewrite(
    block: &Block,
    probe: fn(&Block) -> bool,
) -> Result<Option<RewriteInventory>, &'static str> {
    let mut inventory = Inventory {
        nodes: 0,
        selects: 0,
        collect_names: false,
        names: FxHashSet::default(),
        functions: FxHashSet::default(),
        probe: Some(probe),
        found: false,
        captured: None,
    };
    inventory.block(block, 0)?;
    if !inventory.found { return Ok(None); }
    inventory.nodes = 0;
    inventory.functions.clear();
    inventory.collect_names = true;
    inventory.probe = None;
    inventory.captured = Some(FxHashSet::default());
    inventory.block(block, 0)?;
    Ok(Some(RewriteInventory {
        reserved: inventory.names,
        captured: inventory.captured.unwrap(),
    }))
}

/// Caller must first validate the tree with the bounded inventory. Each added
/// local consumes one declaration and `extra_scratch` additional expression
/// registers; declarations in disjoint scopes are counted conservatively.
pub(crate) fn local_rewrite_frame(block: &Block, parameters: &[RcLocal], extra_scratch: usize) -> Frame {
    let mut locals = parameters.iter().map(RcLocal::stable_id).collect();
    let mut declarations = parameters.len();
    let (mut hidden, mut scratch) = (0, 0);
    frame_locals(block, &mut locals, &mut declarations, &mut hidden, &mut scratch);
    Frame {
        locals,
        headroom: LOCAL_LIMIT.saturating_sub(declarations).min(
            240usize.saturating_sub(declarations + hidden + scratch) / (1 + extra_scratch)
        ),
    }
}

fn value_register_bound(value: &RValue) -> usize {
    let own = match value {
        RValue::Closure(closure) => 1 + closure.upvalues.len(),
        _ => 1,
    };
    own + value
        .rvalues()
        .into_iter()
        .map(value_register_bound)
        .sum::<usize>()
}

fn frame_locals(
    block: &Block,
    locals: &mut FxHashSet<u64>,
    declarations: &mut usize,
    hidden: &mut usize,
    scratch: &mut usize,
) {
    for statement in &block.0 {
        // Sum expression nodes rather than assume that local headroom implies
        // register headroom. Open packs/callee/arguments and closure capture
        // operands consume registers too. Reserve additional protocol/scratch
        // space and count disjoint scopes conservatively together.
        let destination_scratch = match statement {
            Statement::Assign(assign) => {
                assign.left.len()
                    + assign
                        .left
                        .iter()
                        .map(|left| match left {
                            LValue::Index(index) => {
                                value_register_bound(&index.left)
                                    + value_register_bound(&index.right)
                            }
                            _ => 0,
                        })
                        .sum::<usize>()
            }
            // Internal/opaque lowering has its own register contract. Do not
            // add locals elsewhere in such a function without that contract.
            Statement::SetList(_)
            | Statement::NumForInit(_)
            | Statement::NumForNext(_)
            | Statement::GenericForInit(_)
            | Statement::GenericForNext(_)
            | Statement::Goto(_)
            | Statement::Label(_) => 240,
            _ => 0,
        };
        *scratch = (*scratch).max(
            destination_scratch
                + statement
                    .rvalues()
                    .into_iter()
                    .map(value_register_bound)
                    .sum::<usize>(),
        );
        match statement {
            Statement::Assign(assign) if assign.prefix => {
                for left in &assign.left {
                    if let LValue::Local(local) = left {
                        locals.insert(local.stable_id());
                        *declarations += 1;
                    }
                }
            }
            Statement::NumericFor(node) => {
                locals.insert(node.counter.stable_id());
                *declarations += 1;
                *hidden += 4;
            }
            Statement::GenericFor(node) => {
                *hidden += 3;
                for local in &node.res_locals {
                    locals.insert(local.stable_id());
                    *declarations += 1;
                }
            }
            _ => {}
        }
        match statement {
            Statement::If(node) => {
                frame_locals(
                    &node.then_block.lock(),
                    locals,
                    declarations,
                    hidden,
                    scratch,
                );
                frame_locals(
                    &node.else_block.lock(),
                    locals,
                    declarations,
                    hidden,
                    scratch,
                );
            }
            Statement::While(node) => {
                frame_locals(&node.block.lock(), locals, declarations, hidden, scratch)
            }
            Statement::Repeat(node) => {
                frame_locals(&node.block.lock(), locals, declarations, hidden, scratch)
            }
            Statement::NumericFor(node) => {
                frame_locals(&node.block.lock(), locals, declarations, hidden, scratch)
            }
            Statement::GenericFor(node) => {
                frame_locals(&node.block.lock(), locals, declarations, hidden, scratch)
            }
            _ => {}
        }
    }
}

struct State {
    reserved: FxHashSet<String>,
    next_name: usize,
    visited: FxHashSet<usize>,
    report: Report,
}

impl State {
    fn function(&mut self, block: &mut Block, parameters: &[RcLocal]) {
        let mut frame = local_rewrite_frame(block, parameters, 0);
        self.block(block, &mut frame);
    }

    fn block(&mut self, block: &mut Block, frame: &mut Frame) {
        let mut output = Vec::with_capacity(block.0.len());
        for mut statement in std::mem::take(&mut block.0) {
            statement.post_traverse_rvalues(&mut |value| -> Option<()> {
                if let RValue::Closure(closure) = value {
                    let id = (&*closure.function.0 as *const _) as usize;
                    if self.visited.insert(id) {
                        let mut function = closure.function.lock();
                        let parameters = function.parameters.clone();
                        self.function(&mut function.body, &parameters);
                    }
                }
                None
            });
            match &mut statement {
                Statement::If(node) => {
                    self.block(&mut node.then_block.lock(), frame);
                    self.block(&mut node.else_block.lock(), frame);
                }
                Statement::While(node) => self.block(&mut node.block.lock(), frame),
                Statement::Repeat(node) => self.block(&mut node.block.lock(), frame),
                Statement::NumericFor(node) => self.block(&mut node.block.lock(), frame),
                Statement::GenericFor(node) => self.block(&mut node.block.lock(), frame),
                _ => {}
            }
            let has_select = statement.rvalues().iter().any(|value| contains(value))
                || matches!(&statement, Statement::Assign(assign) if assign.left.iter().any(|left|
                    matches!(left, LValue::Index(index) if contains(&index.left) || contains(&index.right))));
            if !has_select {
                output.push(statement);
                continue;
            }
            let mut attempt = Attempt {
                frame,
                reserved: &self.reserved,
                next_name: self.next_name,
                names: Vec::new(),
                producers: Ledger::default(),
                lowered: 0,
            };
            match attempt.statement(&mut statement) {
                Ok(prefix) => {
                    self.next_name = attempt.next_name;
                    self.report.lowered_selects += attempt.lowered;
                    self.report.introduced_locals += attempt.names.len();
                    self.report.introduced_bindings.append(attempt.producers);
                    attempt.frame.headroom -= attempt.names.len();
                    self.reserved.extend(attempt.names);
                    output.extend(prefix);
                }
                Err(reason) => {
                    *self.report.refused_statements.entry(reason).or_default() += 1;
                }
            }
            output.push(statement);
        }
        block.0 = output;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choose() -> RValue {
        crate::IfExpression::new(
            Literal::Boolean(true).into(),
            Literal::Nil.into(),
            Literal::Boolean(false).into(),
        )
        .into()
    }

    #[test]
    fn refusal_does_not_publish_partial_branches_or_locals() {
        let value = crate::IfExpression::new(
            choose(),
            choose(),
            crate::Table(vec![(None, choose())]).into(),
        )
        .into();
        let mut block = Block(vec![crate::Return::new(vec![value]).into()]);
        let before = block.to_string();
        let report = lower_existing_conditionals(&mut block);
        assert_eq!(block.to_string(), before);
        assert_eq!(report.lowered_selects, 0);
        assert_eq!(report.introduced_locals, 0);
        assert!(report.introduced_bindings.records.is_empty());
        assert_eq!(report.introduced_bindings.omitted_records, 0);
        assert_eq!(report.refused_statements["table_constructor_order"], 1);
    }

    #[test]
    fn tree_budget_is_atomic_and_does_not_claim_coverage() {
        let mut value = choose();
        for _ in 0..140 {
            value = Unary::new(value, UnaryOperation::Not).into();
        }
        let mut block = Block(vec![crate::Return::new(vec![value]).into()]);
        let before = block.to_string();
        let report = lower_existing_conditionals(&mut block);
        assert!(report.budget_exhausted);
        assert_eq!(report.lowered_selects, 0);
        assert_eq!(block.to_string(), before);
    }

    #[test]
    fn scratch_register_pressure_refuses_even_with_local_headroom() {
        let mut block = Block(
            (0..160)
                .map(|i| {
                    assign(
                        &RcLocal::new(Local::new(Some(format!("local{i}")))),
                        Literal::Nil.into(),
                        true,
                    )
                })
                .collect(),
        );
        let mut arguments = vec![Literal::Nil.into(); 90];
        arguments.push(choose());
        block
            .0
            .push(crate::Call::new(crate::Global::from("consume").into(), arguments).into());
        let before = block.to_string();
        let report = lower_existing_conditionals(&mut block);
        assert_eq!(report.refused_statements["local_budget"], 1);
        assert_eq!(block.to_string(), before);
    }
}

fn contains(value: &RValue) -> bool {
    matches!(value, RValue::IfExpression(_)) || value.rvalues().into_iter().any(contains)
}

fn has_continue(block: &Block) -> bool {
    block.0.iter().any(|statement| match statement {
        Statement::Continue(_) => true,
        Statement::If(node) => {
            has_continue(&node.then_block.lock()) || has_continue(&node.else_block.lock())
        }
        _ => false, // A nested loop's continue targets that loop.
    })
}

fn assign(local: &RcLocal, value: RValue, prefix: bool) -> Statement {
    Assign {
        left: vec![local.clone().into()],
        right: vec![value],
        prefix,
        parallel: false,
    }
    .into()
}

struct Attempt<'a> {
    frame: &'a mut Frame,
    reserved: &'a FxHashSet<String>,
    next_name: usize,
    names: Vec<String>,
    producers: Ledger,
    lowered: usize,
}

impl Attempt<'_> {
    fn fresh(&mut self, role: Role) -> Result<RcLocal, &'static str> {
        if self.names.len() >= self.frame.headroom {
            return Err("local_budget");
        }
        loop {
            self.next_name += 1;
            let name = format!("selectedValue{}", self.next_name);
            if !self.reserved.contains(&name) {
                self.names.push(name.clone());
                let local = RcLocal::new(Local::new(Some(name)));
                self.producers.record(&local, role);
                return Ok(local);
            }
        }
    }

    fn snapshot(
        &mut self,
        value: RValue,
        prefix: &mut Vec<Statement>,
        reuse_frame: bool,
    ) -> Result<RValue, &'static str> {
        if matches!(&value, RValue::Literal(_))
            || (reuse_frame
                && matches!(&value,
            RValue::Local(local) if self.frame.locals.contains(&local.stable_id())))
        {
            return Ok(value);
        }
        let local = self.fresh(Role::EvaluationSnapshot)?;
        prefix.push(assign(&local, value, true));
        Ok(local.into())
    }

    fn sequence(
        &mut self,
        values: &[RValue],
        reuse_first_frame: bool,
    ) -> Result<(Vec<Statement>, Vec<RValue>), &'static str> {
        let last = values.iter().rposition(contains);
        // O2 inlining can upgrade an upvalue to a frame register. In these
        // positions the pinned compiler then defers its read; introducing an
        // early snapshot would freeze the O0/O1 behavior instead. Refuse unless
        // the expressions crossed cannot write captured state. No type/name
        // hint or current closure shape is a no-inlining certificate.
        if reuse_first_frame
            && last.is_some_and(|last| last > 0)
            && matches!(values.first(), Some(RValue::Local(local)) if !self.frame.locals.contains(&local.stable_id()))
            && values[1..]
                .iter()
                .any(|value| contains(value) && crate::effects::may_write_capture(value))
        {
            return Err("captured_register_reuse");
        }
        let mut prefix = Vec::new();
        let mut result = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            let (statements, mut value) = self.value(value)?;
            prefix.extend(statements);
            if last.is_some_and(|last| index < last) {
                value = self.snapshot(value, &mut prefix, index == 0 && reuse_first_frame)?;
            }
            result.push(value);
        }
        Ok((prefix, result))
    }

    fn value(&mut self, value: &RValue) -> Result<(Vec<Statement>, RValue), &'static str> {
        if !contains(value) {
            return Ok((Vec::new(), value.clone()));
        }
        match value {
            RValue::IfExpression(node) => {
                let (mut prefix, condition) = self.value(&node.condition)?;
                let local = self.fresh(Role::ScalarSelectResult)?;
                let (mut yes, yes_value) = self.value(&node.then_value)?;
                let (mut no, no_value) = self.value(&node.else_value)?;
                // One destination adjusts each branch to one result, including
                // call/vararg tails and nil. No unselected branch is evaluated.
                yes.push(assign(&local, yes_value, false));
                no.push(assign(&local, no_value, false));
                prefix.push(
                    Assign {
                        left: vec![local.clone().into()],
                        right: vec![],
                        prefix: true,
                        parallel: false,
                    }
                    .into(),
                );
                prefix.push(If::new(condition, Block(yes), Block(no)).into());
                self.lowered += 1;
                Ok((prefix, local.into()))
            }
            RValue::Binary(node)
                if matches!(node.operation, BinaryOperation::And | BinaryOperation::Or) =>
            {
                let (mut prefix, left) = self.value(&node.left)?;
                if !contains(&node.right) {
                    let mut result = node.clone();
                    result.left = Box::new(left);
                    return Ok((prefix, result.into()));
                }
                let local = self.fresh(Role::ShortCircuitResult)?;
                prefix.push(assign(&local, left, true));
                let (mut branch, right) = self.value(&node.right)?;
                branch.push(assign(&local, right, false));
                let condition = if node.operation == BinaryOperation::And {
                    local.clone().into()
                } else {
                    Unary::new(local.clone().into(), UnaryOperation::Not).into()
                };
                prefix.push(If::new(condition, Block(branch), Block::default()).into());
                Ok((prefix, local.into()))
            }
            RValue::Binary(node) => {
                // CONCAT prepares all operands in consecutive temporary
                // registers; arithmetic/comparison may reuse a frame local.
                let (prefix, mut values) = self.sequence(
                    &[*node.left.clone(), *node.right.clone()],
                    node.operation != BinaryOperation::Concat,
                )?;
                let mut node = node.clone();
                node.right = Box::new(values.pop().unwrap());
                node.left = Box::new(values.pop().unwrap());
                Ok((prefix, node.into()))
            }
            RValue::Index(node) => {
                let (prefix, mut values) =
                    self.sequence(&[*node.left.clone(), *node.right.clone()], true)?;
                let mut node = node.clone();
                node.right = Box::new(values.pop().unwrap());
                node.left = Box::new(values.pop().unwrap());
                Ok((prefix, node.into()))
            }
            RValue::Unary(node) => {
                let (prefix, value) = self.value(&node.value)?;
                let mut node = node.clone();
                node.value = Box::new(value);
                Ok((prefix, node.into()))
            }
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                let values: Vec<_> = std::iter::once(*call.value.clone())
                    .chain(call.arguments.iter().cloned())
                    .collect();
                let (prefix, mut values) = self.sequence(&values, false)?;
                let call = crate::Call::new(values.remove(0), values);
                Ok((
                    prefix,
                    if matches!(value, RValue::Select(_)) {
                        RValue::Select(Select::Call(call))
                    } else {
                        call.into()
                    },
                ))
            }
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
                let values: Vec<_> = std::iter::once(*call.value.clone())
                    .chain(call.arguments.iter().cloned())
                    .collect();
                let (prefix, mut values) = self.sequence(&values, true)?;
                let call = crate::MethodCall::new(values.remove(0), call.method.clone(), values);
                Ok((
                    prefix,
                    if matches!(value, RValue::Select(_)) {
                        RValue::Select(Select::MethodCall(call))
                    } else {
                        call.into()
                    },
                ))
            }
            RValue::Table(_) => Err("table_constructor_order"),
            _ => Err("unsupported_expression"),
        }
    }

    // Construct all replacement values first. A refusal cannot partly rewrite
    // an expression, alter stores, or leave references to unpublished locals.
    fn statement(&mut self, statement: &mut Statement) -> Result<Vec<Statement>, &'static str> {
        match statement {
            Statement::Assign(node)
                if node
                    .left
                    .iter()
                    .all(|left| matches!(left, LValue::Local(_))) =>
            {
                let (prefix, values) = self.sequence(&node.right, false)?;
                node.right = values;
                Ok(prefix)
            }
            Statement::Return(node) => {
                let (prefix, values) = self.sequence(&node.values, false)?;
                node.values = values;
                Ok(prefix)
            }
            Statement::Call(node) => {
                let (prefix, value) = self.value(&node.clone().into())?;
                *node = value.into_call().unwrap();
                Ok(prefix)
            }
            Statement::MethodCall(node) => {
                let (prefix, value) = self.value(&node.clone().into())?;
                *node = value.into_method_call().unwrap();
                Ok(prefix)
            }
            Statement::If(node) => {
                let (prefix, condition) = self.value(&node.condition)?;
                node.condition = condition;
                Ok(prefix)
            }
            Statement::While(node) => {
                let (mut prefix, condition) = self.value(&node.condition)?;
                prefix.push(
                    If::new(
                        Unary::new(condition, UnaryOperation::Not).into(),
                        Block(vec![crate::Break {}.into()]),
                        Block::default(),
                    )
                    .into(),
                );
                let mut body = node.block.lock();
                prefix.append(&mut body.0);
                body.0 = prefix;
                node.condition = Literal::Boolean(true).into();
                Ok(Vec::new())
            }
            Statement::Repeat(node) => {
                if has_continue(&node.block.lock()) {
                    return Err("repeat_continue");
                }
                if matches!(
                    node.block.lock().0.iter().rev().find(|statement| !matches!(
                        statement,
                        Statement::Comment(_) | Statement::Empty(_)
                    )),
                    Some(Statement::Return(_))
                ) {
                    return Err("repeat_terminal_return");
                }
                let (mut prefix, condition) = self.value(&node.condition)?;
                node.block.lock().0.append(&mut prefix);
                node.condition = condition;
                Ok(Vec::new())
            }
            Statement::NumericFor(node) => {
                let (prefix, mut values) = self.sequence(
                    &[node.initial.clone(), node.limit.clone(), node.step.clone()],
                    false,
                )?;
                node.step = values.pop().unwrap();
                node.limit = values.pop().unwrap();
                node.initial = values.pop().unwrap();
                Ok(prefix)
            }
            Statement::GenericFor(node) => {
                let (prefix, values) = self.sequence(&node.right, false)?;
                node.right = values;
                Ok(prefix)
            }
            _ => Err("statement_store_or_internal_control"),
        }
    }
}
