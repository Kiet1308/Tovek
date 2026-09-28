use rustc_hash::FxHashSet;

use crate::{
    inline_temps::collect_usage, Block, LValue, Literal, RValue, RcLocal, Select, Statement,
    Traverse, Upvalue,
};

/// Recover a connection assignment the SSA dropped (C13).
///
/// When a closure captures a local `cell` BY REFERENCE and the bytecode writes the
/// connect result into that cell (`MOVE cell = result`), the parent SSA — which
/// captures the PRE-write version and never models the closure's by-ref write —
/// judges the post-write version dead and emits
///
/// ```text
///   local _ = sig:Connect(function() ... if cell then cell:Disconnect() end end)
/// ```
///
/// leaving `cell` forever `nil` so the self-disconnect never fires. The connect
/// result PROVABLY belongs to `cell`, so re-target the dead `_` to it. Gated hard:
///   * `_` is a dead generated temp and the RHS is a `Call`/`MethodCall` (NOT a
///     bare `local function …`, which the closure-disconnect shape would otherwise
///     match and clobber).
///   * among the cells the closure ref-captures AND `:Disconnect()`s, exactly ONE
///     has NO non-`nil` assignment anywhere (its write really was the dropped one).
///     Connections that ARE stored (a non-`nil` assignment exists) are never
///     touched, which also disambiguates a closure that manages several handles.
pub fn recover_dropped_connection(block: &mut Block) {
    // Every rewrite needs a call declaration whose closure ref-captures and
    // disconnects a cell. Without one, skip both module-wide censuses.
    if !block.any_statement_deep(&mut connection_declaration) {
        return;
    }
    let usage = collect_usage(block);
    let mut assigned = FxHashSet::default();
    collect_non_nil_assigned(block, &mut assigned);
    recover_in_block(block, &usage, &assigned);
}

fn recover_in_block(
    block: &mut Block,
    usage: &rustc_hash::FxHashMap<RcLocal, crate::inline_temps::Usage>,
    assigned: &FxHashSet<RcLocal>,
) {
    for statement in &mut block.0 {
        let mut functions = Vec::new();
        statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
            if let RValue::Closure(closure) = rvalue {
                functions.push(closure.function.clone());
            }
            None
        });
        for function in functions {
            recover_in_block(&mut function.lock().body, usage, assigned);
        }
        match statement {
            Statement::If(r#if) => {
                recover_in_block(&mut r#if.then_block.lock(), usage, assigned);
                recover_in_block(&mut r#if.else_block.lock(), usage, assigned);
            }
            Statement::While(r#while) => {
                recover_in_block(&mut r#while.block.lock(), usage, assigned)
            }
            Statement::Repeat(repeat) => {
                recover_in_block(&mut repeat.block.lock(), usage, assigned)
            }
            Statement::NumericFor(numeric_for) => {
                recover_in_block(&mut numeric_for.block.lock(), usage, assigned)
            }
            Statement::GenericFor(generic_for) => {
                recover_in_block(&mut generic_for.block.lock(), usage, assigned)
            }
            _ => {}
        }
    }

    for statement in &mut block.0 {
        let Statement::Assign(assign) = statement else {
            continue;
        };
        if !assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
            continue;
        }
        // RHS must be a call (the connection result) — possibly adjust-to-one
        // wrapped in `Select` — NOT a bare closure (`local function f`).
        if !matches!(
            assign.right[0],
            RValue::Call(_)
                | RValue::MethodCall(_)
                | RValue::Select(Select::Call(_) | Select::MethodCall(_))
        ) {
            continue;
        }
        let Some(dst) = assign.left[0].as_local().cloned() else {
            continue;
        };
        // `dst` must be DEAD: a connect result the SSA discarded. (The RHS-is-call
        // gate above already excludes a bare `local function f`.)
        if usage.get(&dst).map_or(1, |u| u.reads) != 0 {
            continue;
        }
        let Some(cell) = dropped_connection_cell(&assign.right[0], assigned) else {
            continue;
        };
        if cell == dst {
            continue;
        }
        assign.left[0] = LValue::Local(cell);
        assign.prefix = false;
    }
}

/// Necessary shape of a rewritten statement, before the census-dependent gates.
fn connection_declaration(statement: &Statement) -> bool {
    let Statement::Assign(assign) = statement else { return false; };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1
        || !matches!(assign.left[0], LValue::Local(_))
        || !matches!(assign.right[0], RValue::Call(_) | RValue::MethodCall(_)
            | RValue::Select(Select::Call(_) | Select::MethodCall(_)))
    {
        return false;
    }
    let mut found = false;
    for_each_closure(&assign.right[0], &mut |closure| {
        found = found || closure.upvalues.iter().any(|upvalue| matches!(upvalue,
            Upvalue::Ref(cell) if closure_disconnects(&closure.function.lock().body, cell)));
    });
    found
}

/// The single cell a closure in `rvalue` ref-captures and `:Disconnect()`s that
/// has NO non-`nil` assignment (so its connect-result write was the dropped one).
/// `None` if there is no such cell, or more than one (ambiguous).
fn dropped_connection_cell(rvalue: &RValue, assigned: &FxHashSet<RcLocal>) -> Option<RcLocal> {
    let mut candidates: FxHashSet<RcLocal> = FxHashSet::default();
    for_each_closure(rvalue, &mut |closure| {
        for upvalue in &closure.upvalues {
            if let Upvalue::Ref(cell) = upvalue {
                if !assigned.contains(cell)
                    && closure_disconnects(&closure.function.lock().body, cell)
                {
                    candidates.insert(cell.clone());
                }
            }
        }
    });
    if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    }
}

fn for_each_closure(rvalue: &RValue, callback: &mut impl FnMut(&crate::Closure)) {
    if let RValue::Closure(closure) = rvalue {
        callback(closure);
    }
    rvalue.visit_rvalues(&mut |child| { for_each_closure(child, callback); true });
}

/// True if `block` (a closure body) calls `cell:Disconnect()` / `cell:disconnect()`.
fn closure_disconnects(block: &Block, cell: &RcLocal) -> bool {
    let is_disconnect = |method_call: &crate::MethodCall| {
        matches!(method_call.method.as_str(), "Disconnect" | "disconnect")
            && matches!(method_call.value.as_ref(), RValue::Local(l) if l == cell)
    };
    for statement in &block.0 {
        if let Statement::MethodCall(method_call) = statement {
            if is_disconnect(method_call) {
                return true;
            }
        }
        if !visit_statement_rhs(statement, &mut |value| {
            !matches!(value, RValue::MethodCall(call) if is_disconnect(call))
        }) || !visit_statement_blocks(statement, &mut |body| !closure_disconnects(body, cell)) {
            return true;
        }
    }
    false
}

/// Every local that receives a NON-`nil` value via an assignment anywhere in the
/// tree (so a connection that IS already stored is never re-targeted onto).
fn collect_non_nil_assigned(block: &Block, set: &mut FxHashSet<RcLocal>) {
    for statement in &block.0 {
        if let Statement::Assign(assign) = statement {
            for (i, lvalue) in assign.left.iter().enumerate() {
                if let LValue::Local(local) = lvalue {
                    let is_nil = matches!(
                        assign.right.get(i),
                        Some(RValue::Literal(Literal::Nil)) | None
                    );
                    if !is_nil {
                        set.insert(local.clone());
                    }
                }
            }
        }
        visit_statement_rhs(statement, &mut |rvalue| {
            if let RValue::Closure(closure) = rvalue {
                collect_non_nil_assigned(&closure.function.lock().body, set);
            }
            true
        });
        visit_statement_blocks(statement, &mut |child| {
            collect_non_nil_assigned(&child, set);
            true
        });
    }
}

// This analysis deliberately inspects assignment RHS values only, in preorder.
// Other statement operands and indexed LHS expressions have different scopes in
// the original proof. Do not broaden them when replacing its temporary vectors.
fn visit_statement_rhs(statement: &Statement, visit: &mut impl FnMut(&RValue) -> bool) -> bool {
    fn walk(value: &RValue, visit: &mut impl FnMut(&RValue) -> bool) -> bool {
        visit(value) && value.visit_rvalues(&mut |child| walk(child, visit))
    }
    if let Statement::Assign(assign) = statement {
        return assign.right.iter().all(|value| walk(value, visit));
    }
    true
}

fn visit_statement_blocks(statement: &Statement, visit: &mut impl FnMut(&Block) -> bool) -> bool {
    match statement {
        Statement::If(value) => {
            // Drop each lock before the next branch: the two bodies may alias.
            let then = visit(&value.then_block.lock());
            then && visit(&value.else_block.lock())
        }
        Statement::While(value) => visit(&value.block.lock()),
        Statement::Repeat(value) => visit(&value.block.lock()),
        Statement::NumericFor(value) => visit(&value.block.lock()),
        Statement::GenericFor(value) => visit(&value.block.lock()),
        _ => true,
    }
}

#[cfg(test)]
fn statement_rvalues_deep(statement: &Statement) -> Vec<&RValue> {
    let mut out = Vec::new();
    fn walk<'a>(rvalue: &'a RValue, out: &mut Vec<&'a RValue>) {
        out.push(rvalue);
        rvalue.visit_rvalues(&mut |child| {
            walk(child, out);
            true
        });
    }
    if let Statement::Assign(assign) = statement {
        for rvalue in &assign.right {
            walk(rvalue, &mut out);
        }
    }
    out
}

#[cfg(test)]
fn statement_block_children(statement: &Statement) -> Vec<Block> {
    match statement {
        Statement::If(r#if) => {
            vec![
                r#if.then_block.lock().clone(),
                r#if.else_block.lock().clone(),
            ]
        }
        Statement::While(r#while) => vec![r#while.block.lock().clone()],
        Statement::Repeat(repeat) => vec![repeat.block.lock().clone()],
        Statement::NumericFor(numeric_for) => vec![numeric_for.block.lock().clone()],
        Statement::GenericFor(generic_for) => vec![generic_for.block.lock().clone()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Call, Closure, Function, GenericFor, Global, If, Index, Local,
        MethodCall, NumericFor, Repeat, Return, While};
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn legacy_assigned(block: &Block, set: &mut FxHashSet<RcLocal>) {
        for statement in &block.0 {
            if let Statement::Assign(assign) = statement {
                for (index, destination) in assign.left.iter().enumerate() {
                    if let LValue::Local(local) = destination {
                        if !matches!(assign.right.get(index), Some(RValue::Literal(Literal::Nil)) | None) {
                            set.insert(local.clone());
                        }
                    }
                }
            }
            let mut functions = Vec::new();
            for value in statement_rvalues_deep(statement) {
                if let RValue::Closure(closure) = value { functions.push(closure.function.clone()); }
            }
            for function in functions { legacy_assigned(&function.lock().body, set); }
            for child in statement_block_children(statement) { legacy_assigned(&child, set); }
        }
    }

    fn legacy_disconnects(block: &Block, cell: &RcLocal) -> bool {
        let matches = |call: &MethodCall| matches!(call.method.as_str(), "Disconnect" | "disconnect")
            && matches!(call.value.as_ref(), RValue::Local(local) if local == cell);
        for statement in &block.0 {
            if matches!(statement, Statement::MethodCall(call) if matches(call)) { return true; }
            for value in statement_rvalues_deep(statement) {
                if matches!(value, RValue::MethodCall(call) if matches(call)) { return true; }
            }
            if statement_block_children(statement).iter().any(|b| legacy_disconnects(b, cell)) { return true; }
        }
        false
    }

    fn closure(body: Block) -> RValue {
        Closure { node_origin: Default::default(), upvalues: vec![],
            function: Arc::new(Mutex::new(Function { body, ..Default::default() })).into() }.into()
    }

    #[test]
    fn borrowed_connection_censuses_match_original_scope_and_order() {
        for seed in 0..128 {
            let locals: Vec<_> = (0..8).map(|i| RcLocal::new(Local::new(Some(format!("v{i}"))))).collect();
            let mut block = Block::default();
            for index in 0..20 {
                let local = &locals[(index + seed) % locals.len()];
                let method = match (index + seed) % 3 { 0 => "Disconnect", 1 => "disconnect", _ => "Other" };
                let call = MethodCall::new(local.clone().into(), method.into(), vec![]);
                let statement = match (index + seed) % 7 {
                    0 => call.into(),
                    1 => Assign::new(vec![locals[0].clone().into()], vec![call.into()]).into(),
                    2 => Return::new(vec![call.into()]).into(), // intentionally outside RHS census
                    3 => Assign::new(vec![Index::new(closure(Block(vec![call.into()])), Literal::Number(1.0).into()).into()], vec![]).into(),
                    4 => Assign::new(vec![local.clone().into()], vec![closure(Block(vec![Assign::new(
                        vec![locals[7].clone().into()], vec![Literal::Boolean(true).into()]).into()]))]).into(),
                    5 => Assign::new(vec![local.clone().into(), locals[6].clone().into()], vec![Literal::Nil.into()]).into(),
                    _ => Call::new(Global::from("f").into(), vec![closure(Block(vec![call.into()]))]).into(),
                };
                block.0.push(statement);
            }
            for depth in 0..8 {
                let condition = Literal::Boolean(true).into();
                block = Block(vec![match (depth + seed) % 5 {
                    0 => If::new(condition, block, Block::default()).into(),
                    1 => While::new(condition, block).into(),
                    2 => Repeat::new(condition, block).into(),
                    3 => NumericFor::new(Literal::Number(1.0).into(), Literal::Number(3.0).into(),
                        Literal::Number(1.0).into(), locals[0].clone(), block).into(),
                    _ => GenericFor::new(vec![locals[1].clone()], vec![], block).into(),
                }]);
            }
            let mut expected = FxHashSet::default();
            let mut actual = FxHashSet::default();
            legacy_assigned(&block, &mut expected);
            collect_non_nil_assigned(&block, &mut actual);
            assert_eq!(actual, expected, "assigned seed {seed}");
            for local in &locals {
                assert_eq!(closure_disconnects(&block, local), legacy_disconnects(&block, local), "disconnect seed {seed}");
            }
        }
    }

    #[test]
    fn child_visits_borrow_and_release_each_aliased_branch() {
        let shared = Arc::new(Mutex::new(Block(vec![Statement::Empty(crate::Empty {})])));
        let statement = Statement::If(If { then_block: shared.clone(), else_block: shared.clone(),
            ..If::new(Literal::Boolean(true).into(), Block::default(), Block::default()) });
        let address = { let guard = shared.lock(); (&*guard) as *const Block };
        let mut visits = 0;
        assert!(visit_statement_blocks(&statement, &mut |body| {
            assert_eq!(body as *const Block, address);
            visits += 1;
            true
        }));
        assert_eq!(visits, 2);
        visits = 0;
        assert!(!visit_statement_blocks(&statement, &mut |_| { visits += 1; false }));
        assert_eq!(visits, 1);
        assert!(shared.try_lock().is_some());
    }
}
