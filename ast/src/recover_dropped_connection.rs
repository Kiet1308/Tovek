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
    for child in rvalue.rvalues() {
        for_each_closure(child, callback);
    }
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
        for rvalue in statement_rvalues_deep(statement) {
            if let RValue::MethodCall(method_call) = rvalue {
                if is_disconnect(method_call) {
                    return true;
                }
            }
        }
        if statement_block_children(statement)
            .iter()
            .any(|b| closure_disconnects(b, cell))
        {
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
        let mut functions = Vec::new();
        for rvalue in statement_rvalues_deep(statement) {
            if let RValue::Closure(closure) = rvalue {
                functions.push(closure.function.clone());
            }
        }
        for function in functions {
            collect_non_nil_assigned(&function.lock().body, set);
        }
        for child in statement_block_children(statement) {
            collect_non_nil_assigned(&child, set);
        }
    }
}

fn statement_rvalues_deep(statement: &Statement) -> Vec<&RValue> {
    let mut out = Vec::new();
    fn walk<'a>(rvalue: &'a RValue, out: &mut Vec<&'a RValue>) {
        out.push(rvalue);
        for child in rvalue.rvalues() {
            walk(child, out);
        }
    }
    if let Statement::Assign(assign) = statement {
        for rvalue in &assign.right {
            walk(rvalue, &mut out);
        }
    }
    out
}

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
