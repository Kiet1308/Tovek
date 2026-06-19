use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    Assign, Binary, Block, Break, Call, Continue, GenericFor, If, Index, LValue, Literal,
    MethodCall, NumericFor, RValue, RcLocal, Repeat, Return, Select, SetList, Statement, Table,
    Unary, While,
};

// ===================================================================
// Deep clone — duplicating a goto's continuation must not share the
// `Arc<Mutex<Block>>`/closure containers with the original, or later
// passes (LocalDeclarer, naming) would see aliased mutable state.
// `RcLocal`s ARE shared on purpose: the copy must reference the same
// variables. Regions containing closure literals are never cloned
// (see `seq_duplicable`), so the closure arm keeps the shared handle.
// ===================================================================

fn dc_block(block: &Block) -> Block {
    Block(block.0.iter().map(dc_stmt).collect())
}

fn dc_arc(block: &Arc<Mutex<Block>>) -> Arc<Mutex<Block>> {
    Arc::new(Mutex::new(dc_block(&block.lock())))
}

fn dc_lvalue(lvalue: &LValue) -> LValue {
    match lvalue {
        LValue::Index(index) => LValue::Index(Index {
            left: Box::new(dc_rvalue(&index.left)),
            right: Box::new(dc_rvalue(&index.right)),
        }),
        _ => lvalue.clone(),
    }
}

fn dc_call(call: &Call) -> Call {
    Call {
        value: Box::new(dc_rvalue(&call.value)),
        arguments: call.arguments.iter().map(dc_rvalue).collect(),
    }
}

fn dc_method_call(method_call: &MethodCall) -> MethodCall {
    MethodCall {
        value: Box::new(dc_rvalue(&method_call.value)),
        method: method_call.method.clone(),
        arguments: method_call.arguments.iter().map(dc_rvalue).collect(),
    }
}

fn dc_rvalue(rvalue: &RValue) -> RValue {
    match rvalue {
        RValue::Call(call) => RValue::Call(dc_call(call)),
        RValue::MethodCall(method_call) => RValue::MethodCall(dc_method_call(method_call)),
        RValue::Table(table) => RValue::Table(Table(
            table
                .0
                .iter()
                .map(|(k, v)| (k.as_ref().map(dc_rvalue), dc_rvalue(v)))
                .collect(),
        )),
        RValue::Index(index) => RValue::Index(Index {
            left: Box::new(dc_rvalue(&index.left)),
            right: Box::new(dc_rvalue(&index.right)),
        }),
        RValue::Unary(unary) => RValue::Unary(Unary {
            value: Box::new(dc_rvalue(&unary.value)),
            operation: unary.operation,
        }),
        RValue::Binary(binary) => RValue::Binary(Binary {
            left: Box::new(dc_rvalue(&binary.left)),
            right: Box::new(dc_rvalue(&binary.right)),
            operation: binary.operation,
        }),
        RValue::Select(select) => RValue::Select(match select {
            Select::Call(call) => Select::Call(dc_call(call)),
            Select::MethodCall(method_call) => Select::MethodCall(dc_method_call(method_call)),
            Select::VarArg(v) => Select::VarArg(v.clone()),
        }),
        // Local/Global/Literal/VarArg/Closure: shared handle (closures never reach here).
        _ => rvalue.clone(),
    }
}

fn dc_stmt(statement: &Statement) -> Statement {
    match statement {
        Statement::Assign(assign) => Statement::Assign(Assign {
            left: assign.left.iter().map(dc_lvalue).collect(),
            right: assign.right.iter().map(dc_rvalue).collect(),
            prefix: assign.prefix,
            parallel: assign.parallel,
        }),
        Statement::Call(call) => Statement::Call(dc_call(call)),
        Statement::MethodCall(method_call) => Statement::MethodCall(dc_method_call(method_call)),
        Statement::Return(r#return) => Statement::Return(Return {
            values: r#return.values.iter().map(dc_rvalue).collect(),
        }),
        Statement::If(r#if) => Statement::If(If {
            condition: dc_rvalue(&r#if.condition),
            then_block: dc_arc(&r#if.then_block),
            else_block: dc_arc(&r#if.else_block),
        }),
        Statement::While(r#while) => Statement::While(While {
            condition: dc_rvalue(&r#while.condition),
            block: dc_arc(&r#while.block),
        }),
        Statement::Repeat(repeat) => Statement::Repeat(Repeat {
            condition: dc_rvalue(&repeat.condition),
            block: dc_arc(&repeat.block),
        }),
        Statement::NumericFor(numeric_for) => Statement::NumericFor(NumericFor {
            initial: dc_rvalue(&numeric_for.initial),
            limit: dc_rvalue(&numeric_for.limit),
            step: dc_rvalue(&numeric_for.step),
            counter: numeric_for.counter.clone(),
            block: dc_arc(&numeric_for.block),
        }),
        Statement::GenericFor(generic_for) => Statement::GenericFor(GenericFor {
            res_locals: generic_for.res_locals.clone(),
            right: generic_for.right.iter().map(dc_rvalue).collect(),
            block: dc_arc(&generic_for.block),
        }),
        Statement::SetList(set_list) => Statement::SetList(SetList {
            object_local: set_list.object_local.clone(),
            index: set_list.index,
            values: set_list.values.iter().map(dc_rvalue).collect(),
            tail: set_list.tail.as_ref().map(dc_rvalue),
        }),
        // Goto/Label/Break/Continue/Comment/Empty/Close and unused for-internals
        // hold no nested block containers, so a shallow clone is already deep.
        _ => statement.clone(),
    }
}

// ===================================================================
// Region analysis
// ===================================================================

fn is_terminator(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Return(_) | Statement::Break(_) | Statement::Continue(_) | Statement::Goto(_)
    )
}

// A region is safe to duplicate unless it contains an upvalue-closing `Close`
// or the lowering-internal for-loop nodes (which assume a single occurrence).
// Closures ARE allowed: duplicating shares the function `Arc`, which keeps
// upvalue linking (keyed by that Arc) working and is idempotent.
fn seq_duplicable(stmts: &[Statement]) -> bool {
    stmts.iter().all(|s| match s {
        Statement::Close(_)
        | Statement::NumForInit(_)
        | Statement::NumForNext(_)
        | Statement::GenericForInit(_)
        | Statement::GenericForNext(_) => false,
        Statement::If(f) => {
            seq_duplicable(&f.then_block.lock().0) && seq_duplicable(&f.else_block.lock().0)
        }
        Statement::While(w) => seq_duplicable(&w.block.lock().0),
        Statement::Repeat(r) => seq_duplicable(&r.block.lock().0),
        Statement::NumericFor(nf) => seq_duplicable(&nf.block.lock().0),
        Statement::GenericFor(gf) => seq_duplicable(&gf.block.lock().0),
        _ => true,
    })
}

// Rough statement count (descending into nested blocks and closure bodies) used
// to avoid duplicating very large continuations, which would bloat the output.
fn seq_size(stmts: &[Statement]) -> usize {
    fn rvalue_size(_r: &RValue) -> usize {
        // A `Closure`'s body is decompiled by its OWN per-function pass, which is
        // ordered strictly AFTER this (enclosing) function in the serial lift
        // order — so when `simplify_gotos` runs here the child body is always
        // still empty and contributed 0. Returning 0 directly preserves that
        // exactly while NOT locking the child's `Arc<Mutex<Block>>`: under the
        // parallelized per-function loop the child may be decompiling
        // concurrently, and reading its half-written body would be a race that
        // makes goto-duplication (and thus the output) scheduling-dependent.
        0
    }
    stmts
        .iter()
        .map(|s| {
            1 + match s {
                Statement::If(f) => {
                    seq_size(&f.then_block.lock().0) + seq_size(&f.else_block.lock().0)
                }
                Statement::While(w) => seq_size(&w.block.lock().0),
                Statement::Repeat(r) => seq_size(&r.block.lock().0),
                Statement::NumericFor(nf) => seq_size(&nf.block.lock().0),
                Statement::GenericFor(gf) => seq_size(&gf.block.lock().0),
                Statement::Assign(a) => a.right.iter().map(rvalue_size).sum(),
                _ => 0,
            }
        })
        .sum()
}

const MAX_DUP_SIZE: usize = 200;

// Does this sequence contain a break/continue that targets an *enclosing* loop
// (i.e. not nested inside a loop within the sequence)? If so it can only be
// duplicated into a site that is itself inside a loop.
fn seq_needs_loop(stmts: &[Statement]) -> bool {
    stmts.iter().any(|s| match s {
        Statement::Break(_) | Statement::Continue(_) => true,
        Statement::If(f) => {
            seq_needs_loop(&f.then_block.lock().0) || seq_needs_loop(&f.else_block.lock().0)
        }
        // a loop captures its own break/continue
        Statement::While(_)
        | Statement::Repeat(_)
        | Statement::NumericFor(_)
        | Statement::GenericFor(_) => false,
        _ => false,
    })
}

fn collect_defined_labels(stmts: &[Statement], out: &mut FxHashSet<String>) {
    for s in stmts {
        match s {
            Statement::Label(l) => {
                out.insert(l.0.clone());
            }
            Statement::If(f) => {
                collect_defined_labels(&f.then_block.lock().0, out);
                collect_defined_labels(&f.else_block.lock().0, out);
            }
            Statement::While(w) => collect_defined_labels(&w.block.lock().0, out),
            Statement::Repeat(r) => collect_defined_labels(&r.block.lock().0, out),
            Statement::NumericFor(nf) => collect_defined_labels(&nf.block.lock().0, out),
            Statement::GenericFor(gf) => collect_defined_labels(&gf.block.lock().0, out),
            _ => {}
        }
    }
}

fn seq_contains_goto(stmts: &[Statement], label: &str) -> bool {
    stmts.iter().any(|s| match s {
        Statement::Goto(g) => g.0 .0 == label,
        Statement::If(f) => {
            seq_contains_goto(&f.then_block.lock().0, label)
                || seq_contains_goto(&f.else_block.lock().0, label)
        }
        Statement::While(w) => seq_contains_goto(&w.block.lock().0, label),
        Statement::Repeat(r) => seq_contains_goto(&r.block.lock().0, label),
        Statement::NumericFor(nf) => seq_contains_goto(&nf.block.lock().0, label),
        Statement::GenericFor(gf) => seq_contains_goto(&gf.block.lock().0, label),
        _ => false,
    })
}

fn seq_contains_label_or_other_goto(stmts: &[Statement], allowed_goto: &str) -> bool {
    stmts.iter().any(|s| match s {
        Statement::Label(_) => true,
        Statement::Goto(g) => g.0 .0 != allowed_goto,
        Statement::If(f) => {
            seq_contains_label_or_other_goto(&f.then_block.lock().0, allowed_goto)
                || seq_contains_label_or_other_goto(&f.else_block.lock().0, allowed_goto)
        }
        Statement::While(w) => seq_contains_label_or_other_goto(&w.block.lock().0, allowed_goto),
        Statement::Repeat(r) => seq_contains_label_or_other_goto(&r.block.lock().0, allowed_goto),
        Statement::NumericFor(nf) => {
            seq_contains_label_or_other_goto(&nf.block.lock().0, allowed_goto)
        }
        Statement::GenericFor(gf) => {
            seq_contains_label_or_other_goto(&gf.block.lock().0, allowed_goto)
        }
        _ => false,
    })
}

// Rename labels *defined* within `stmts` to fresh names (and rewrite the gotos
// inside `stmts` that target them) so an inlined copy never duplicates a label.
fn relabel(stmts: &mut [Statement], rename: &FxHashMap<String, String>) {
    for s in stmts.iter_mut() {
        match s {
            Statement::Label(l) => {
                if let Some(new) = rename.get(&l.0) {
                    l.0 = new.clone();
                }
            }
            Statement::Goto(g) => {
                if let Some(new) = rename.get(&g.0 .0) {
                    g.0 .0 = new.clone();
                }
            }
            Statement::If(f) => {
                relabel(&mut f.then_block.lock().0, rename);
                relabel(&mut f.else_block.lock().0, rename);
            }
            Statement::While(w) => relabel(&mut w.block.lock().0, rename),
            Statement::Repeat(r) => relabel(&mut r.block.lock().0, rename),
            Statement::NumericFor(nf) => relabel(&mut nf.block.lock().0, rename),
            Statement::GenericFor(gf) => relabel(&mut gf.block.lock().0, rename),
            _ => {}
        }
    }
}

// ===================================================================
// The pass
// ===================================================================

struct GotoFixer {
    // label name -> the statement sequence that executes starting at the label,
    // continued (across fall-through) until a hard terminator.
    continuations: FxHashMap<String, Vec<Statement>>,
    // If a continuation ends with a synthesized loop-body fall-through
    // `continue`, this records which loop owns that continue.
    continue_owner: FxHashMap<String, usize>,
    // labels whose trailing `continue` belongs to a `for` loop (which always
    // terminates). Only for these is it safe, when inlining outside the loop, to
    // treat the `continue` as "loop exhausted -> exit" (a `while`/`repeat` may be
    // infinite, so the same rewrite would wrongly drop a real back-edge).
    exhaustible: FxHashSet<String>,
    fresh_counter: usize,
    loop_counter: usize,
}

impl GotoFixer {
    // Take statements until (and including) the first top-level terminator;
    // if none, fall through to `after`.
    fn resolve(stmts: &[Statement], after: &[Statement]) -> Vec<Statement> {
        let mut out = Vec::new();
        for s in stmts {
            out.push(dc_stmt(s));
            if is_terminator(s) {
                return out;
            }
        }
        out.extend(after.iter().map(dc_stmt));
        out
    }

    fn next_loop_id(&mut self) -> usize {
        self.loop_counter += 1;
        self.loop_counter
    }

    fn collect(
        &mut self,
        block: &Block,
        after: &[Statement],
        after_exhaustible: bool,
        current_loop: Option<usize>,
    ) {
        for (i, s) in block.0.iter().enumerate() {
            if let Statement::Label(l) = s {
                let cont = Self::resolve(&block.0[i + 1..], after);
                if after_exhaustible && matches!(cont.last(), Some(Statement::Continue(_))) {
                    self.exhaustible.insert(l.0.clone());
                }
                if matches!(cont.last(), Some(Statement::Continue(_)))
                    && let Some(loop_id) = current_loop
                {
                    self.continue_owner.insert(l.0.clone(), loop_id);
                }
                self.continuations.insert(l.0.clone(), cont);
            }
        }
        for (i, s) in block.0.iter().enumerate() {
            match s {
                Statement::If(f) => {
                    let after_if = Self::resolve(&block.0[i + 1..], after);
                    self.collect(
                        &f.then_block.lock(),
                        &after_if,
                        after_exhaustible,
                        current_loop,
                    );
                    self.collect(
                        &f.else_block.lock(),
                        &after_if,
                        after_exhaustible,
                        current_loop,
                    );
                }
                // falling off a loop body re-enters the loop == `continue`. Only
                // `for` loops are exhaustible; `while`/`repeat` may be infinite.
                Statement::While(w) => {
                    let loop_id = self.next_loop_id();
                    self.collect(&w.block.lock(), &[Continue {}.into()], false, Some(loop_id));
                }
                Statement::Repeat(r) => {
                    let loop_id = self.next_loop_id();
                    self.collect(&r.block.lock(), &[Continue {}.into()], false, Some(loop_id));
                }
                Statement::NumericFor(nf) => {
                    let loop_id = self.next_loop_id();
                    self.collect(&nf.block.lock(), &[Continue {}.into()], true, Some(loop_id));
                }
                Statement::GenericFor(gf) => {
                    let loop_id = self.next_loop_id();
                    self.collect(&gf.block.lock(), &[Continue {}.into()], true, Some(loop_id));
                }
                _ => {}
            }
        }
    }

    fn relabel_fresh(&mut self, seq: &mut [Statement]) {
        let mut defined = FxHashSet::default();
        collect_defined_labels(seq, &mut defined);
        if !defined.is_empty() {
            let rename: FxHashMap<String, String> = defined
                .into_iter()
                .map(|name| {
                    self.fresh_counter += 1;
                    (name, format!("dup{}", self.fresh_counter))
                })
                .collect();
            relabel(seq, &rename);
        }
    }

    fn fresh_copy(&mut self, label: &str) -> Vec<Statement> {
        let mut copy: Vec<Statement> = self.continuations[label].iter().map(dc_stmt).collect();
        self.relabel_fresh(&mut copy);
        copy
    }

    // Like `fresh_copy`, but for a continuation that ends in a synthesized
    // `continue` (a loop body's fall-through) being inlined at a site *outside*
    // that loop. The site is reached after the loop is exhausted, so re-entering
    // the loop is equivalent to leaving it — i.e. running the site's own
    // continuation. So drop the trailing `continue` and append `site_after`.
    fn fresh_copy_replace_continue(
        &mut self,
        label: &str,
        site_after: &[Statement],
    ) -> Vec<Statement> {
        let mut copy: Vec<Statement> = {
            let cont = &self.continuations[label];
            cont[..cont.len() - 1].iter().map(dc_stmt).collect()
        };
        copy.extend(site_after.iter().map(dc_stmt));
        self.relabel_fresh(&mut copy);
        copy
    }

    // Replaces each eliminable `goto` with a copy of its continuation. `after`
    // is the continuation of the current block (terminator-ended); `in_loop`
    // tracks whether the current scope is inside a loop.
    fn rewrite(
        &mut self,
        block: &mut Block,
        after: &[Statement],
        current_loop: Option<usize>,
    ) -> usize {
        let stmts = std::mem::take(&mut block.0);
        let loop_after: [Statement; 1] = [Continue {}.into()];
        let mut replaced = 0;
        let mut inline_at: FxHashMap<usize, Vec<Statement>> = FxHashMap::default();

        for (i, s) in stmts.iter().enumerate() {
            match s {
                Statement::If(f) => {
                    let child_after = Self::resolve(&stmts[i + 1..], after);
                    replaced += self.rewrite(&mut f.then_block.lock(), &child_after, current_loop);
                    replaced += self.rewrite(&mut f.else_block.lock(), &child_after, current_loop);
                }
                Statement::While(w) => {
                    let loop_id = self.next_loop_id();
                    replaced += self.rewrite(&mut w.block.lock(), &loop_after, Some(loop_id))
                }
                Statement::Repeat(r) => {
                    let loop_id = self.next_loop_id();
                    replaced += self.rewrite(&mut r.block.lock(), &loop_after, Some(loop_id))
                }
                Statement::NumericFor(nf) => {
                    let loop_id = self.next_loop_id();
                    replaced += self.rewrite(&mut nf.block.lock(), &loop_after, Some(loop_id))
                }
                Statement::GenericFor(gf) => {
                    let loop_id = self.next_loop_id();
                    replaced += self.rewrite(&mut gf.block.lock(), &loop_after, Some(loop_id))
                }
                Statement::Goto(g) => {
                    let label = g.0 .0.clone();
                    let plan = if let Some(cont) = self.continuations.get(&label) {
                        if !seq_duplicable(cont)
                            || seq_size(cont) > MAX_DUP_SIZE
                            || seq_contains_goto(cont, &label)
                        {
                            0
                        } else if !seq_needs_loop(cont)
                            || (trailing_continue_only(cont)
                                && self.continue_owner.get(&label).copied() == current_loop)
                        {
                            1
                        } else if trailing_continue_only(cont) && self.exhaustible.contains(&label)
                        {
                            2
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    if plan == 1 {
                        let c = self.fresh_copy(&label);
                        inline_at.insert(i, c);
                        replaced += 1;
                    } else if plan == 2 {
                        let site_after = Self::resolve(&stmts[i + 1..], after);
                        let c = self.fresh_copy_replace_continue(&label, &site_after);
                        inline_at.insert(i, c);
                        replaced += 1;
                    }
                }
                _ => {}
            }
        }

        let mut out: Vec<Statement> = Vec::with_capacity(stmts.len());
        for (i, s) in stmts.into_iter().enumerate() {
            match inline_at.remove(&i) {
                Some(body) => out.extend(body),
                None => out.push(s),
            }
        }
        block.0 = out;
        replaced
    }
}

// A continuation safe to inline outside its loop: its only loop-control is the
// single trailing `continue` (the synthesized fall-through), which gets swapped
// for the inline site's own continuation.
fn trailing_continue_only(cont: &[Statement]) -> bool {
    matches!(cont.last(), Some(Statement::Continue(_))) && !seq_needs_loop(&cont[..cont.len() - 1])
}

// Tail duplication can leave `x = <bool>` immediately followed by `if x then ...`,
// where the condition is now constant. Replace such an `if` with the branch that
// actually runs, dropping the dead one. Pure cleanup — the assignment is kept in
// case `x` is read elsewhere.
fn fold_constant_conditions(block: &mut Block) {
    for s in block.0.iter_mut() {
        match s {
            Statement::If(f) => {
                fold_constant_conditions(&mut f.then_block.lock());
                fold_constant_conditions(&mut f.else_block.lock());
            }
            Statement::While(w) => fold_constant_conditions(&mut w.block.lock()),
            Statement::Repeat(r) => fold_constant_conditions(&mut r.block.lock()),
            Statement::NumericFor(nf) => fold_constant_conditions(&mut nf.block.lock()),
            Statement::GenericFor(gf) => fold_constant_conditions(&mut gf.block.lock()),
            _ => {}
        }
    }

    let mut out: Vec<Statement> = Vec::with_capacity(block.0.len());
    let mut it = std::mem::take(&mut block.0).into_iter().peekable();
    while let Some(s) = it.next() {
        // Is `s` a `x = <bool>` whose value the next `if x` tests?
        let taken: Option<bool> = match &s {
            Statement::Assign(a) if a.left.len() == 1 && a.right.len() == 1 => {
                match (a.left.first(), a.right.first()) {
                    (Some(LValue::Local(x)), Some(RValue::Literal(Literal::Boolean(b)))) => {
                        match it.peek() {
                            Some(Statement::If(f)) if matches!(&f.condition, RValue::Local(y) if y == x) => {
                                Some(*b)
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        out.push(s);
        let Some(b) = taken else { continue };

        // Only fold when it stays valid: either the chosen branch doesn't end in
        // a terminator, or the `if` is the last statement (so nothing illegally
        // follows the inlined `return`/`break`/`continue`).
        let terminates = if let Some(Statement::If(f)) = it.peek() {
            let branch = if b { &f.then_block } else { &f.else_block };
            matches!(branch.lock().0.last(), Some(last) if is_terminator(last))
        } else {
            false
        };
        let if_stmt = it.next().unwrap();
        if terminates && it.peek().is_some() {
            out.push(if_stmt); // not safe to fold; keep the (still-correct) if
        } else if let Statement::If(f) = if_stmt {
            let branch = if b { f.then_block } else { f.else_block };
            out.extend(std::mem::take(&mut branch.lock().0));
        }
    }
    block.0 = out;
}

fn collect_goto_targets(block: &Block, out: &mut FxHashSet<String>) {
    for s in &block.0 {
        match s {
            Statement::Goto(g) => {
                out.insert(g.0 .0.clone());
            }
            Statement::If(f) => {
                collect_goto_targets(&f.then_block.lock(), out);
                collect_goto_targets(&f.else_block.lock(), out);
            }
            Statement::While(w) => collect_goto_targets(&w.block.lock(), out),
            Statement::Repeat(r) => collect_goto_targets(&r.block.lock(), out),
            Statement::NumericFor(nf) => collect_goto_targets(&nf.block.lock(), out),
            Statement::GenericFor(gf) => collect_goto_targets(&gf.block.lock(), out),
            _ => {}
        }
    }
}

fn remove_dead_labels(block: &mut Block, targets: &FxHashSet<String>) {
    block
        .0
        .retain(|s| !matches!(s, Statement::Label(l) if !targets.contains(&l.0)));
    for s in block.0.iter_mut() {
        match s {
            Statement::If(f) => {
                remove_dead_labels(&mut f.then_block.lock(), targets);
                remove_dead_labels(&mut f.else_block.lock(), targets);
            }
            Statement::While(w) => remove_dead_labels(&mut w.block.lock(), targets),
            Statement::Repeat(r) => remove_dead_labels(&mut r.block.lock(), targets),
            Statement::NumericFor(nf) => remove_dead_labels(&mut nf.block.lock(), targets),
            Statement::GenericFor(gf) => remove_dead_labels(&mut gf.block.lock(), targets),
            _ => {}
        }
    }
}

// ===================================================================
// Reloop shared tails — the graph structurer can leave this shape:
//
//      while true do
//          ...
//          if C then break end
//          ::tail::
//          TAIL
//      end
//      FALLBACK
//      goto tail
//
// The `break` is not a source-level loop exit; it is an edge to a fallback
// branch that rejoins at the loop tail. Move that fallback back into the break
// site so the loop stays structured and no Luau-incompatible label is needed.
// ===================================================================

fn direct_label_names(stmts: &[Statement]) -> FxHashSet<String> {
    stmts
        .iter()
        .filter_map(|s| match s {
            Statement::Label(l) => Some(l.0.clone()),
            _ => None,
        })
        .collect()
}

fn direct_label_index(stmts: &[Statement], label: &str) -> Option<usize> {
    stmts
        .iter()
        .position(|s| matches!(s, Statement::Label(l) if l.0 == label))
}

fn set_local_bool(local: &RcLocal, value: bool) -> Statement {
    Assign::new(
        vec![LValue::Local(local.clone())],
        vec![Literal::Boolean(value).into()],
    )
    .into()
}

fn replace_label_gotos_with_breaks(
    stmts: &mut Vec<Statement>,
    label: &str,
    hit_local: &RcLocal,
    loop_depth: usize,
) -> Option<usize> {
    let mut replaced = 0;
    let mut out = Vec::with_capacity(stmts.len());

    for mut statement in std::mem::take(stmts) {
        let is_target_goto = matches!(&statement, Statement::Goto(g) if g.0 .0 == label);
        if is_target_goto {
            if loop_depth != 1 {
                return None;
            }
            out.push(set_local_bool(hit_local, true));
            out.push(Break {}.into());
            replaced += 1;
            continue;
        }

        match &mut statement {
            Statement::Goto(_) => {}
            Statement::If(f) => {
                replaced += replace_label_gotos_with_breaks(
                    &mut f.then_block.lock().0,
                    label,
                    hit_local,
                    loop_depth,
                )?;
                replaced += replace_label_gotos_with_breaks(
                    &mut f.else_block.lock().0,
                    label,
                    hit_local,
                    loop_depth,
                )?;
            }
            Statement::While(w) => {
                replaced += replace_label_gotos_with_breaks(
                    &mut w.block.lock().0,
                    label,
                    hit_local,
                    loop_depth + 1,
                )?;
            }
            Statement::Repeat(r) => {
                replaced += replace_label_gotos_with_breaks(
                    &mut r.block.lock().0,
                    label,
                    hit_local,
                    loop_depth + 1,
                )?;
            }
            Statement::NumericFor(nf) => {
                replaced += replace_label_gotos_with_breaks(
                    &mut nf.block.lock().0,
                    label,
                    hit_local,
                    loop_depth + 1,
                )?;
            }
            Statement::GenericFor(gf) => {
                replaced += replace_label_gotos_with_breaks(
                    &mut gf.block.lock().0,
                    label,
                    hit_local,
                    loop_depth + 1,
                )?;
            }
            _ => {}
        }
        out.push(statement);
    }
    *stmts = out;
    Some(replaced)
}

fn normalize_loop_entry_region(label: &str, region: &[Statement]) -> Option<Vec<Statement>> {
    if !matches!(region.last(), Some(Statement::Goto(g)) if g.0 .0 == label) {
        return None;
    }

    let mut replacement: Vec<Statement> = region[..region.len() - 1].iter().map(dc_stmt).collect();
    if seq_needs_loop(&replacement) {
        return None;
    }
    if seq_contains_label_or_other_goto(&replacement, label) {
        return None;
    }
    if !seq_contains_goto(&replacement, label) {
        return Some(replacement);
    }

    let hit_index = replacement
        .iter()
        .position(|statement| seq_contains_goto(std::slice::from_ref(statement), label))?;
    if !matches!(
        &replacement[hit_index],
        Statement::While(_)
            | Statement::Repeat(_)
            | Statement::NumericFor(_)
            | Statement::GenericFor(_)
    ) {
        return None;
    }
    let suffix = replacement.split_off(hit_index + 1);
    if suffix.is_empty() || seq_contains_goto(&suffix, label) {
        return None;
    }
    let hit_statement = replacement.pop()?;
    let hit_local = RcLocal::default();
    let mut hit_region = vec![hit_statement];
    let replaced_gotos =
        replace_label_gotos_with_breaks(&mut hit_region, label, &hit_local, 0)?;
    if replaced_gotos == 0 || seq_contains_goto(&hit_region, label) {
        return None;
    }

    let mut output = Vec::with_capacity(replacement.len() + hit_region.len() + 2);
    output.push(set_local_bool(&hit_local, false));
    output.append(&mut replacement);
    output.append(&mut hit_region);
    let guard = RValue::Unary(Unary {
        value: Box::new(RValue::Local(hit_local)),
        operation: crate::UnaryOperation::Not,
    });
    output.push(If::new(guard, suffix.into(), Block::default()).into());
    Some(output)
}

fn replace_current_loop_breaks(
    stmts: &mut Vec<Statement>,
    replacement: &[Statement],
    tail: &[Statement],
) -> usize {
    let mut changed = 0;
    let mut out = Vec::with_capacity(stmts.len());
    for mut s in std::mem::take(stmts) {
        match &mut s {
            Statement::Break(_) => {
                out.extend(replacement.iter().map(dc_stmt));
                out.extend(tail.iter().map(dc_stmt));
                if !tail.last().is_some_and(is_terminator) {
                    out.push(Continue {}.into());
                }
                changed += 1;
            }
            Statement::If(f) => {
                changed +=
                    replace_current_loop_breaks(&mut f.then_block.lock().0, replacement, tail);
                changed +=
                    replace_current_loop_breaks(&mut f.else_block.lock().0, replacement, tail);
                out.push(s);
            }
            // Nested loops capture their own `break`, so do not rewrite inside.
            Statement::While(_)
            | Statement::Repeat(_)
            | Statement::NumericFor(_)
            | Statement::GenericFor(_) => out.push(s),
            _ => out.push(s),
        }
    }
    *stmts = out;
    changed
}

fn try_structure_loop_entry_goto_at(block: &mut Block, loop_index: usize) -> bool {
    let labels = match &block.0[loop_index] {
        Statement::While(w) if matches!(w.condition, RValue::Literal(Literal::Boolean(true))) => {
            direct_label_names(&w.block.lock().0)
        }
        _ => return false,
    };
    if labels.is_empty() {
        return false;
    }

    let Some((goto_index, label)) =
        ((loop_index + 1)..block.0.len()).find_map(|i| match &block.0[i] {
            Statement::Goto(g) if labels.contains(&g.0 .0) => Some((i, g.0 .0.clone())),
            _ => None,
        })
    else {
        return false;
    };

    let replacement =
        match normalize_loop_entry_region(&label, &block.0[loop_index + 1..=goto_index]) {
            Some(replacement) => replacement,
            None => return false,
        };

    let changed = match &mut block.0[loop_index] {
        Statement::While(w) => {
            let mut body = w.block.lock();
            let Some(label_index) = direct_label_index(&body.0, &label) else {
                return false;
            };
            let tail_after_label = &body.0[label_index + 1..];
            let mut tail_labels = FxHashSet::default();
            collect_defined_labels(tail_after_label, &mut tail_labels);
            if !seq_duplicable(tail_after_label) || seq_size(tail_after_label) > MAX_DUP_SIZE {
                return false;
            }
            if !tail_labels.is_empty() || seq_contains_goto(tail_after_label, &label) {
                return false;
            }
            let mut tail = body.0.split_off(label_index);
            tail.remove(0);
            let changed = replace_current_loop_breaks(&mut body.0, &replacement, &tail);
            body.0.append(&mut tail);
            changed
        }
        _ => 0,
    };

    if changed == 0 {
        return false;
    }

    block.0.drain(loop_index + 1..=goto_index);
    true
}

fn structure_loop_entry_gotos(block: &mut Block) -> usize {
    let mut changed = 0;
    for s in block.0.iter_mut() {
        match s {
            Statement::If(f) => {
                changed += structure_loop_entry_gotos(&mut f.then_block.lock());
                changed += structure_loop_entry_gotos(&mut f.else_block.lock());
            }
            Statement::While(w) => changed += structure_loop_entry_gotos(&mut w.block.lock()),
            Statement::Repeat(r) => changed += structure_loop_entry_gotos(&mut r.block.lock()),
            Statement::NumericFor(nf) => {
                changed += structure_loop_entry_gotos(&mut nf.block.lock())
            }
            Statement::GenericFor(gf) => {
                changed += structure_loop_entry_gotos(&mut gf.block.lock())
            }
            _ => {}
        }
    }

    let mut i = 0;
    while i < block.0.len() {
        if try_structure_loop_entry_goto_at(block, i) {
            changed += 1;
        } else {
            i += 1;
        }
    }
    changed
}

// ===================================================================
// Label raising — for gotos that survive duplication (real loops whose
// header sits in a nested scope). Lua forbids jumping *into* a block, but a
// backward goto to a label in an *enclosing* block is fine. So we raise such
// labels to the function's top level, where every goto can see them.
//
// Raising a label out of an `if` branch, preserving semantics:
//      if C then A; ::L::; R end ; rest
//   becomes
//      if C then A; goto L end ; goto AFTER ; ::L:: R ; ::AFTER:: ; rest
// The fall-through into the label is replaced by an explicit goto; the
// not-taken paths jump over the raised region via a fresh AFTER label.
// Labels inside a structured loop body are NOT raised out (that would change
// the loop), only up to the loop body's own top.
// ===================================================================

fn defined_directly(block: &Block, out: &mut FxHashSet<String>) {
    for s in &block.0 {
        if let Statement::Label(l) = s {
            out.insert(l.0.clone());
        }
    }
}

fn compute_needy(block: &Block, enclosing: &FxHashSet<String>, needy: &mut FxHashSet<String>) {
    let mut visible = enclosing.clone();
    defined_directly(block, &mut visible);
    for s in &block.0 {
        match s {
            Statement::Goto(g) => {
                if !visible.contains(&g.0 .0) {
                    needy.insert(g.0 .0.clone());
                }
            }
            Statement::If(f) => {
                compute_needy(&f.then_block.lock(), &visible, needy);
                compute_needy(&f.else_block.lock(), &visible, needy);
            }
            Statement::While(w) => compute_needy(&w.block.lock(), &visible, needy),
            Statement::Repeat(r) => compute_needy(&r.block.lock(), &visible, needy),
            Statement::NumericFor(nf) => compute_needy(&nf.block.lock(), &visible, needy),
            Statement::GenericFor(gf) => compute_needy(&gf.block.lock(), &visible, needy),
            _ => {}
        }
    }
}

// Raise one needy label one level up into `block`. Returns true if it did.
fn raise_once(block: &mut Block, needy: &FxHashSet<String>, counter: &mut usize) -> bool {
    // first, raise within nested blocks (deeper labels reach their branch top)
    for s in block.0.iter_mut() {
        let raised = match s {
            Statement::If(f) => {
                raise_once(&mut f.then_block.lock(), needy, counter)
                    || raise_once(&mut f.else_block.lock(), needy, counter)
            }
            Statement::While(w) => raise_once(&mut w.block.lock(), needy, counter),
            Statement::Repeat(r) => raise_once(&mut r.block.lock(), needy, counter),
            Statement::NumericFor(nf) => raise_once(&mut nf.block.lock(), needy, counter),
            Statement::GenericFor(gf) => raise_once(&mut gf.block.lock(), needy, counter),
            _ => false,
        };
        if raised {
            return true;
        }
    }

    // then, raise a needy label out of a direct child `if` branch into `block`
    let mut found: Option<(usize, Vec<Statement>, Statement)> = None;
    'outer: for (j, s) in block.0.iter().enumerate() {
        if let Statement::If(f) = s {
            for branch in [&f.then_block, &f.else_block] {
                let mut br = branch.lock();
                if let Some(i_l) =
                    br.0.iter()
                        .position(|st| matches!(st, Statement::Label(l) if needy.contains(&l.0)))
                {
                    // [.. before .., ::L::, region ..]
                    let mut region = br.0.split_off(i_l); // [::L::, region..]
                    let label = region.remove(0); // ::L::
                    let name = match &label {
                        Statement::Label(l) => l.0.clone(),
                        _ => unreachable!(),
                    };
                    br.0.push(crate::Goto::new(name.clone().into()).into());
                    found = Some((j, region, label));
                    break 'outer;
                }
            }
        }
    }

    if let Some((j, region, label)) = found {
        *counter += 1;
        let after = format!("after{}", counter);
        let mut insert: Vec<Statement> = vec![crate::Goto::new(after.clone().into()).into(), label];
        insert.extend(region);
        insert.push(crate::Label(after).into());
        block.0.splice(j + 1..j + 1, insert);
        return true;
    }
    false
}

// Convert `break`/`continue` that target THIS loop into gotos. Stops at nested
// loops (which capture their own break/continue).
fn convert_break_continue(stmts: &mut [Statement], exit: &str, head: &str) {
    for s in stmts.iter_mut() {
        match s {
            Statement::Break(_) => *s = crate::Goto::new(exit.into()).into(),
            Statement::Continue(_) => *s = crate::Goto::new(head.into()).into(),
            Statement::If(f) => {
                convert_break_continue(&mut f.then_block.lock().0, exit, head);
                convert_break_continue(&mut f.else_block.lock().0, exit, head);
            }
            _ => {}
        }
    }
}

fn body_has_needy(body: &Block, needy: &FxHashSet<String>) -> bool {
    let mut defined = FxHashSet::default();
    collect_defined_labels(&body.0, &mut defined);
    defined.intersection(needy).next().is_some()
}

// Turn a `while`/`repeat` that traps a needy label into explicit goto form, so
// the label rises to the loop's parent scope. Returns true if it un-structured
// one. (Numeric/generic `for` loops are left intact.)
fn unstructure_one_loop(block: &mut Block, needy: &FxHashSet<String>, counter: &mut usize) -> bool {
    for s in block.0.iter_mut() {
        let done = match s {
            Statement::If(f) => {
                unstructure_one_loop(&mut f.then_block.lock(), needy, counter)
                    || unstructure_one_loop(&mut f.else_block.lock(), needy, counter)
            }
            Statement::While(w) => unstructure_one_loop(&mut w.block.lock(), needy, counter),
            Statement::Repeat(r) => unstructure_one_loop(&mut r.block.lock(), needy, counter),
            Statement::NumericFor(nf) => unstructure_one_loop(&mut nf.block.lock(), needy, counter),
            Statement::GenericFor(gf) => unstructure_one_loop(&mut gf.block.lock(), needy, counter),
            _ => false,
        };
        if done {
            return true;
        }
    }

    let target = block.0.iter().position(|s| match s {
        Statement::While(w) => body_has_needy(&w.block.lock(), needy),
        Statement::Repeat(r) => body_has_needy(&r.block.lock(), needy),
        _ => false,
    });

    if let Some(j) = target {
        *counter += 1;
        let head = format!("loop{}", counter);
        *counter += 1;
        let exit = format!("exit{}", counter);
        let goto = |name: &str| -> Statement { crate::Goto::new(name.into()).into() };
        let label = |name: String| -> Statement { crate::Label(name).into() };

        let replacement: Vec<Statement> = match block.0.remove(j) {
            Statement::While(w) => {
                let mut body = std::mem::take(&mut *w.block.lock());
                convert_break_continue(&mut body.0, &exit, &head);
                let not_cond = RValue::Unary(Unary {
                    value: Box::new(w.condition),
                    operation: crate::UnaryOperation::Not,
                });
                let guard: Block = vec![goto(&exit)].into();
                let mut out = vec![
                    label(head.clone()),
                    If::new(not_cond, guard, Block::default()).into(),
                ];
                out.extend(body.0);
                out.push(goto(&head));
                out.push(label(exit));
                out
            }
            Statement::Repeat(r) => {
                *counter += 1;
                let cont = format!("loop{}", counter);
                let mut body = std::mem::take(&mut *r.block.lock());
                convert_break_continue(&mut body.0, &exit, &cont);
                let not_cond = RValue::Unary(Unary {
                    value: Box::new(r.condition),
                    operation: crate::UnaryOperation::Not,
                });
                let guard: Block = vec![goto(&head)].into();
                let mut out = vec![label(head.clone())];
                out.extend(body.0);
                out.push(label(cont));
                out.push(If::new(not_cond, guard, Block::default()).into());
                out.push(label(exit));
                out
            }
            _ => unreachable!(),
        };
        block.0.splice(j..j, replacement);
        return true;
    }
    false
}

fn raise_labels(block: &mut Block) {
    let mut counter = 0usize;
    for _ in 0..8192 {
        let mut needy = FxHashSet::default();
        compute_needy(block, &FxHashSet::default(), &mut needy);
        if needy.is_empty() {
            break;
        }
        if raise_once(block, &needy, &mut counter) {
            continue;
        }
        if unstructure_one_loop(block, &needy, &mut counter) {
            continue;
        }
        break; // remaining needy labels are trapped in `for` loops
    }
}

/// Eliminates `goto`/`::label::` pairs left by the control-flow structurer.
/// First by tail duplication (replacing a `goto` with a copy of the code it
/// jumps to — runs before local declarations so `LocalDeclarer` scopes the
/// copies correctly), then, for gotos that remain (loop headers in nested
/// scopes), by raising their labels to the function top level so the gotos
/// become valid backward jumps. The result is structured Lua that is both more
/// readable and free of the invalid goto-scoping the structurer can emit.
pub fn simplify_gotos(block: &mut Block) {
    // Skip-guard: the entire goto machinery (the 256× collect/rewrite fixpoint,
    // the 128× loop-entry structurer, `raise_labels`, `collect_goto_targets` and
    // `remove_dead_labels`) is provably a no-op on a function that contains no
    // `goto` and no `::label::` anywhere — `collect` finds no continuations and
    // breaks, `structure_loop_entry_gotos`/`try_structure_loop_entry_goto_at`
    // need a goto to a loop-body label (returns 0), and `compute_needy` only
    // populates from `Goto` statements (so `raise_labels` breaks immediately).
    // The majority of the ~250 functions in a large script are goto-free, yet
    // each previously paid ~5 full-tree walks here. Only `fold_constant_conditions`
    // has a goto-independent effect, so on goto-free input we run just it.
    if !block_has_goto_or_label(block) {
        fold_constant_conditions(block);
        return;
    }

    let implicit_return: [Statement; 1] = [Return::default().into()];
    for _ in 0..256 {
        let mut fixer = GotoFixer {
            continuations: FxHashMap::default(),
            continue_owner: FxHashMap::default(),
            exhaustible: FxHashSet::default(),
            fresh_counter: 0,
            loop_counter: 0,
        };
        fixer.collect(block, &implicit_return, false, None);
        if fixer.continuations.is_empty() {
            break;
        }
        fixer.loop_counter = 0;
        if fixer.rewrite(block, &implicit_return, None) == 0 {
            break;
        }
    }
    for _ in 0..128 {
        if structure_loop_entry_gotos(block) == 0 {
            break;
        }
    }
    raise_labels(block);
    fold_constant_conditions(block);
    let mut targets = FxHashSet::default();
    collect_goto_targets(block, &mut targets);
    remove_dead_labels(block, &targets);
}

/// Recursively scan a block (descending into structured control flow, but NOT
/// into closures — `simplify_gotos` runs per-function and never crosses closure
/// boundaries) for any `goto` or `::label::`. Cheap (early-exits on the first
/// hit); used by `simplify_gotos` to skip its goto machinery entirely.
fn block_has_goto_or_label(block: &Block) -> bool {
    block.0.iter().any(|s| match s {
        Statement::Goto(_) | Statement::Label(_) => true,
        Statement::If(f) => {
            block_has_goto_or_label(&f.then_block.lock())
                || block_has_goto_or_label(&f.else_block.lock())
        }
        Statement::While(w) => block_has_goto_or_label(&w.block.lock()),
        Statement::Repeat(r) => block_has_goto_or_label(&r.block.lock()),
        Statement::NumericFor(nf) => block_has_goto_or_label(&nf.block.lock()),
        Statement::GenericFor(gf) => block_has_goto_or_label(&gf.block.lock()),
        _ => false,
    })
}

/// Post-`LocalDeclarer` fixup: in any block that still contains a `goto`, move
/// the block's `local` declarations to the top. This prevents the remaining
/// (valid) gotos from jumping *into* the scope of a local declared between the
/// goto and its target — which Lua forbids. Declaring earlier (uninitialised
/// until the original assignment) is semantically safe because SSA guarantees
/// every read is dominated by a write.
pub fn hoist_locals_for_gotos(block: &mut Block) {
    for s in block.0.iter_mut() {
        match s {
            Statement::If(f) => {
                hoist_locals_for_gotos(&mut f.then_block.lock());
                hoist_locals_for_gotos(&mut f.else_block.lock());
            }
            Statement::While(w) => hoist_locals_for_gotos(&mut w.block.lock()),
            Statement::Repeat(r) => hoist_locals_for_gotos(&mut r.block.lock()),
            Statement::NumericFor(nf) => hoist_locals_for_gotos(&mut nf.block.lock()),
            Statement::GenericFor(gf) => hoist_locals_for_gotos(&mut gf.block.lock()),
            _ => {}
        }
    }

    if !block.0.iter().any(|s| matches!(s, Statement::Goto(_))) {
        return;
    }

    let mut declared: Vec<crate::RcLocal> = Vec::new();
    for s in &block.0 {
        if let Statement::Assign(a) = s {
            if a.prefix {
                for l in &a.left {
                    if let LValue::Local(rc) = l {
                        if !declared.contains(rc) {
                            declared.push(rc.clone());
                        }
                    }
                }
            }
        }
    }
    if declared.is_empty() {
        return;
    }

    let mut rebuilt: Vec<Statement> = Vec::with_capacity(block.0.len() + 1);
    let mut declaration = Assign::new(declared.into_iter().map(LValue::Local).collect(), vec![]);
    declaration.prefix = true;
    rebuilt.push(declaration.into());
    for s in std::mem::take(&mut block.0) {
        match s {
            Statement::Assign(mut a) if a.prefix => {
                if a.right.is_empty() {
                    // bare declaration, now provided at the top — drop it
                } else {
                    a.prefix = false;
                    rebuilt.push(Statement::Assign(a));
                }
            }
            other => rebuilt.push(other),
        }
    }
    block.0 = rebuilt;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BinaryOperation, Global, Local};

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn bool_lit(value: bool) -> RValue {
        RValue::Literal(Literal::Boolean(value))
    }

    fn string_lit(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn local_value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn assign(local: &RcLocal, value: RValue) -> Statement {
        Assign::new(vec![LValue::Local(local.clone())], vec![value]).into()
    }

    fn goto(label: &str) -> Statement {
        crate::Goto::new(label.into()).into()
    }

    fn label(label: &str) -> Statement {
        crate::Label(label.into()).into()
    }

    fn print(local: &RcLocal) -> Statement {
        Call::new(global("print"), vec![local_value(local)]).into()
    }

    fn contains_goto_or_label(stmts: &[Statement]) -> bool {
        stmts.iter().any(|statement| match statement {
            Statement::Goto(_) | Statement::Label(_) => true,
            Statement::If(r#if) => {
                contains_goto_or_label(&r#if.then_block.lock().0)
                    || contains_goto_or_label(&r#if.else_block.lock().0)
            }
            Statement::While(r#while) => contains_goto_or_label(&r#while.block.lock().0),
            Statement::Repeat(repeat) => contains_goto_or_label(&repeat.block.lock().0),
            Statement::NumericFor(numeric_for) => {
                contains_goto_or_label(&numeric_for.block.lock().0)
            }
            Statement::GenericFor(generic_for) => {
                contains_goto_or_label(&generic_for.block.lock().0)
            }
            _ => false,
        })
    }

    #[test]
    fn reloops_infinite_while_shared_tail_with_hit_flag() {
        let stage = local("stage");
        let key = local("key");
        let entry = local("entry");

        let break_condition = RValue::Unary(Unary {
            value: Box::new(local_value(&stage)),
            operation: crate::UnaryOperation::Not,
        });
        let hit_condition = RValue::Binary(Binary {
            left: Box::new(local_value(&entry)),
            right: Box::new(bool_lit(true)),
            operation: BinaryOperation::Equal,
        });
        let nested_for = GenericFor::new(
            vec![key, entry.clone()],
            vec![global("pairs")],
            Block(vec![If::new(
                hit_condition,
                vec![assign(&stage, bool_lit(false)), goto("tail")].into(),
                Block::default(),
            )
            .into()]),
        );

        let mut block = Block(vec![
            While::new(
                bool_lit(true),
                Block(vec![
                    If::new(
                        break_condition,
                        vec![Break {}.into()].into(),
                        Block::default(),
                    )
                    .into(),
                    assign(&stage, string_lit("intervening")),
                    label("tail"),
                    print(&stage),
                ]),
            )
            .into(),
            nested_for.into(),
            print(&stage),
            assign(&stage, string_lit("fallback")),
            goto("tail"),
        ]);

        assert_eq!(structure_loop_entry_gotos(&mut block), 1);
        assert!(
            !contains_goto_or_label(&block.0),
            "shared-tail reloop should remove labels and gotos:\n{}",
            block
        );

        let Statement::While(r#while) = &block.0[0] else {
            panic!("expected while after reloop:\n{}", block);
        };
        let body = r#while.block.lock();
        let Statement::If(replaced_break) = &body.0[0] else {
            panic!(
                "expected original break guard to receive fallback region:\n{}",
                block
            );
        };
        let then_body = replaced_break.then_block.lock();
        assert_eq!(
            then_body.0.len(),
            5,
            "replacement should be reset flag, fallback search, guarded suffix, tail copy, continue:\n{}",
            block
        );
        let Statement::Assign(reset_hit) = &then_body.0[0] else {
            panic!("expected hit flag reset before fallback search:\n{}", block);
        };
        let [LValue::Local(hit_local)] = reset_hit.left.as_slice() else {
            panic!("hit flag reset should assign a local:\n{}", block);
        };
        assert!(
            matches!(reset_hit.right.as_slice(), [RValue::Literal(Literal::Boolean(false))]),
            "hit flag must reset before each replacement execution:\n{}",
            block
        );

        let Statement::GenericFor(rewritten_for) = &then_body.0[1] else {
            panic!(
                "expected fallback search loop inside break branch:\n{}",
                block
            );
        };
        let for_body = rewritten_for.block.lock();
        let Statement::If(hit_if) = &for_body.0[0] else {
            panic!("expected hit branch inside fallback loop:\n{}", block);
        };
        let hit_then = hit_if.then_block.lock();
        assert!(
            matches!(
                &hit_then.0[1],
                Statement::Assign(assign)
                    if matches!(assign.left.as_slice(), [LValue::Local(local)] if local == hit_local)
                        && matches!(assign.right.as_slice(), [RValue::Literal(Literal::Boolean(true))])
            ) && matches!(&hit_then.0[2], Statement::Break(_)),
            "target goto should become hit-flag assignment plus break:\n{}",
            block
        );

        let Statement::If(fallback_if) = &then_body.0[2] else {
            panic!(
                "expected fallback suffix to be guarded by hit flag:\n{}",
                block
            );
        };
        let guarded_suffix = fallback_if.then_block.lock();
        assert!(
            matches!(
                guarded_suffix.0.as_slice(),
                [Statement::Call(_), Statement::Assign(_)]
            ),
            "hit flag must guard every statement skipped by the original goto:\n{}",
            block
        );
        assert!(
            !matches!(
                &fallback_if.condition,
                RValue::Unary(Unary { value, operation: crate::UnaryOperation::Not })
                    if matches!(&**value, RValue::Local(local) if local == &stage)
            ),
            "fallback guard must use a dedicated hit flag, not the payload local:\n{}",
            block
        );
        assert!(
            matches!(&then_body.0[3], Statement::Call(_))
                && matches!(&then_body.0[4], Statement::Continue(_)),
            "break replacement must jump to a duplicated tail instead of falling through pre-label statements:\n{}",
            block
        );
        assert!(
            matches!(&body.0[1], Statement::Assign(_)) && matches!(&body.0[2], Statement::Call(_)),
            "non-break path must keep the pre-label statement before the original tail:\n{}",
            block
        );
    }

    #[test]
    fn does_not_reloop_conditional_while_shared_tail() {
        let stage = local("stage");
        let mut block = Block(vec![
            While::new(
                global("running"),
                Block(vec![
                    If::new(
                        global("done"),
                        vec![Break {}.into()].into(),
                        Block::default(),
                    )
                    .into(),
                    label("tail"),
                    print(&stage),
                ]),
            )
            .into(),
            assign(&stage, string_lit("fallback")),
            goto("tail"),
        ]);

        assert_eq!(structure_loop_entry_gotos(&mut block), 0);
        assert!(
            contains_goto_or_label(&block.0),
            "conditional loops must not be rewritten by the infinite-loop-only pass"
        );
    }

    #[test]
    fn does_not_reloop_region_with_enclosing_loop_control() {
        let stage = local("stage");
        let mut block = Block(vec![
            While::new(
                bool_lit(true),
                Block(vec![
                    If::new(
                        global("done"),
                        vec![Break {}.into()].into(),
                        Block::default(),
                    )
                    .into(),
                    label("tail"),
                    print(&stage),
                ]),
            )
            .into(),
            If::new(
                global("outer_done"),
                vec![Break {}.into()].into(),
                Block::default(),
            )
            .into(),
            assign(&stage, string_lit("fallback")),
            goto("tail"),
        ]);

        assert_eq!(structure_loop_entry_gotos(&mut block), 0);
        assert!(
            contains_goto_or_label(&block.0),
            "regions with enclosing-loop break/continue must not be moved into the inner loop"
        );
    }

    #[test]
    fn does_not_reloop_when_tail_jumps_to_entry_label() {
        let stage = local("stage");
        let mut block = Block(vec![
            While::new(
                bool_lit(true),
                Block(vec![
                    If::new(
                        global("done"),
                        vec![Break {}.into()].into(),
                        Block::default(),
                    )
                    .into(),
                    label("tail"),
                    If::new(
                        global("again"),
                        vec![goto("tail")].into(),
                        Block::default(),
                    )
                    .into(),
                    print(&stage),
                ]),
            )
            .into(),
            assign(&stage, string_lit("fallback")),
            goto("tail"),
        ]);

        assert_eq!(structure_loop_entry_gotos(&mut block), 0);
        assert!(
            contains_goto_or_label(&block.0),
            "tail that still jumps to its entry label must keep the label"
        );
    }

    #[test]
    fn does_not_reloop_region_with_labels_to_duplicate() {
        let stage = local("stage");
        let mut block = Block(vec![
            While::new(
                bool_lit(true),
                Block(vec![
                    If::new(
                        global("done"),
                        vec![Break {}.into()].into(),
                        Block::default(),
                    )
                    .into(),
                    label("tail"),
                    print(&stage),
                ]),
            )
            .into(),
            label("fallback"),
            assign(&stage, string_lit("fallback")),
            goto("tail"),
        ]);

        assert_eq!(structure_loop_entry_gotos(&mut block), 0);
        assert!(
            contains_goto_or_label(&block.0),
            "fallback regions with labels must not be duplicated into break sites"
        );
    }
}
