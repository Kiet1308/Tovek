use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    Assign, Binary, Block, Break, Call, Continue, GenericFor, If, Index, LValue, Literal,
    MethodCall, NumericFor, RValue, RcLocal, Repeat, Return, Select, SetList, Statement, Table,
    Traverse, Unary, While,
};

// ===================================================================
// Deep clone — duplicating a goto's continuation must not share the
// `Arc<Mutex<Block>>`/closure containers with the original, or later
// passes (LocalDeclarer, naming) would see aliased mutable state.
// `RcLocal`s ARE shared on purpose: the copy must reference the same
// variables. Regions containing closure literals are never cloned
// (see `seq_duplicable`), so the closure arm keeps the shared handle.
// (NB: a NESTED closure is deliberately NOT deep-cloned — later passes,
// e.g. `expr_deinline`, key maps on its `Function` `Arc` identity via
// `Arc::as_ptr`, so minting a fresh Arc panics with "no entry found for
// key". `materialize_value_captures` accepts that residual; see there.)
// ===================================================================

/// `pub(crate)` so `materialize_value_captures` can un-share a de-inline-duplicated
/// closure body before snapshotting it. Rebuilds nested `Arc<Mutex<Block>>` sub-blocks
/// (`dc_arc`); nested CLOSURE Arcs stay shared (`dc_rvalue` catch-all). A capture read
/// strictly inside such a nested closure would therefore still leak a rename to the
/// sibling — a pre-existing residual that has zero corpus occurrence and is strictly
/// better than the pre-fix behaviour. It is NOT closed by deep-cloning nested closures:
/// that mints fresh `Function` Arcs and panics a later `Arc::as_ptr`-keyed lookup.
pub(crate) fn dc_block(block: &Block) -> Block {
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
        // Local/Global/Literal/VarArg/Closure: shared handle. A nested closure keeps its
        // shared `Function` Arc on purpose — later passes key on its `Arc::as_ptr`
        // identity (see the header note), so a fresh Arc would panic.
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
        Statement::Goto(g) => g.0.0 == label,
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
        Statement::Goto(g) => g.0.0 != allowed_goto,
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
                if let Some(new) = rename.get(&g.0.0) {
                    g.0.0 = new.clone();
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
    // Labels whose resolved continuation reaches the synthetic fall-through of
    // a finite `for`.  An edge emitted after that owner loop has already
    // exhausted must execute the shared tail once and then resume at the edge's
    // own continuation; it must not retry the iterator.  This provenance is
    // distinct from a source-level goto into a loop (which Luau cannot express).
    exhausted_for_tail: FxHashSet<String>,
    // Function-wide across every fixpoint iteration.  A copied continuation can
    // itself expose a goto only on the next snapshot, so resetting freshness per
    // iteration can collide with an earlier `dupN` and silently retarget edges.
    reserved_labels: FxHashSet<String>,
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
        after_exhausted_for: bool,
        current_loop: Option<usize>,
    ) {
        for (i, s) in block.0.iter().enumerate() {
            if let Statement::Label(l) = s {
                let cont = Self::resolve(&block.0[i + 1..], after);
                if after_exhausted_for && matches!(cont.last(), Some(Statement::Continue(_))) {
                    self.exhausted_for_tail.insert(l.0.clone());
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
                        after_exhausted_for,
                        current_loop,
                    );
                    self.collect(
                        &f.else_block.lock(),
                        &after_if,
                        after_exhausted_for,
                        current_loop,
                    );
                }
                // Falling off a loop body re-enters that loop. Only a finite
                // numeric/generic for also has an exhausted continuation that
                // can own an outside shared-tail edge.
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
                    loop {
                        self.fresh_counter += 1;
                        let candidate = format!("dup{}", self.fresh_counter);
                        if self.reserved_labels.insert(candidate.clone()) {
                            break (name, candidate);
                        }
                    }
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

    fn fresh_copy_exhausted_for_tail(
        &mut self,
        label: &str,
        site_after: &[Statement],
    ) -> Vec<Statement> {
        let mut copy: Vec<Statement> = {
            let continuation = &self.continuations[label];
            continuation[..continuation.len() - 1]
                .iter()
                .map(dc_stmt)
                .collect()
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
        definitely_exhausted_fors: &FxHashSet<usize>,
    ) -> usize {
        let stmts = std::mem::take(&mut block.0);
        let loop_after: [Statement; 1] = [Continue {}.into()];
        let mut replaced = 0;
        let mut inline_at: FxHashMap<usize, Vec<Statement>> = FxHashMap::default();
        let mut exhausted_here = definitely_exhausted_fors.clone();

        for (i, s) in stmts.iter().enumerate() {
            match s {
                Statement::If(f) => {
                    let child_after = Self::resolve(&stmts[i + 1..], after);
                    replaced += self.rewrite(
                        &mut f.then_block.lock(),
                        &child_after,
                        current_loop,
                        &exhausted_here,
                    );
                    replaced += self.rewrite(
                        &mut f.else_block.lock(),
                        &child_after,
                        current_loop,
                        &exhausted_here,
                    );
                }
                Statement::While(w) => {
                    let loop_id = self.next_loop_id();
                    replaced += self.rewrite(
                        &mut w.block.lock(),
                        &loop_after,
                        Some(loop_id),
                        &exhausted_here,
                    )
                }
                Statement::Repeat(r) => {
                    let loop_id = self.next_loop_id();
                    replaced += self.rewrite(
                        &mut r.block.lock(),
                        &loop_after,
                        Some(loop_id),
                        &exhausted_here,
                    )
                }
                Statement::NumericFor(nf) => {
                    let loop_id = self.next_loop_id();
                    replaced += self.rewrite(
                        &mut nf.block.lock(),
                        &loop_after,
                        Some(loop_id),
                        &exhausted_here,
                    );
                    exhausted_here.insert(loop_id);
                }
                Statement::GenericFor(gf) => {
                    let loop_id = self.next_loop_id();
                    replaced += self.rewrite(
                        &mut gf.block.lock(),
                        &loop_after,
                        Some(loop_id),
                        &exhausted_here,
                    );
                    exhausted_here.insert(loop_id);
                }
                Statement::Goto(g) => {
                    let label = g.0.0.clone();
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
                        } else if trailing_continue_only(cont)
                            && self.exhausted_for_tail.contains(&label)
                            && self
                                .continue_owner
                                .get(&label)
                                .is_some_and(|owner| exhausted_here.contains(owner))
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
                        let copy = self.fresh_copy_exhausted_for_tail(&label, &site_after);
                        inline_at.insert(i, copy);
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
                out.insert(g.0.0.clone());
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
// Forward-edge structuring without tail duplication
// ===================================================================
//
// Tail duplication is the cleanest representation for a small continuation,
// but deliberately stops at `MAX_DUP_SIZE` to avoid turning one cross-edge into
// hundreds or thousands of repeated source lines.  A forward goto whose label
// is in the same lexical block has a compact structured representation instead:
// use a one-bit escape flag and guard only the statements the jump skips.
//
//     if C then goto L end; A; ::L::; B
//
// becomes
//
//     escaped = false
//     if C then escaped = true end
//     if not escaped then A end
//     B
//
// The recursive rewrite propagates an escape through nested loops with `break`;
// this is effectively a single-target, zero-dispatch Relooper Multiple region.
// It is both smaller and more readable than duplicating a large `B` tail.

fn set_local_number(local: &RcLocal, value: usize) -> Statement {
    Assign::new(
        vec![LValue::Local(local.clone())],
        vec![Literal::Number(value as f64).into()],
    )
    .into()
}

enum EscapeMode<'a> {
    Forward {
        label: &'a str,
        signal: &'a RcLocal,
    },
    Restart {
        label: &'a str,
        signal: &'a RcLocal,
    },
    Dispatch {
        label_states: &'a FxHashMap<String, usize>,
        state: &'a RcLocal,
        signal: &'a RcLocal,
    },
}

impl EscapeMode<'_> {
    fn signal(&self) -> &RcLocal {
        match self {
            Self::Forward { signal, .. }
            | Self::Restart { signal, .. }
            | Self::Dispatch { signal, .. } => signal,
        }
    }

    fn goto_assignments(&self, goto: &crate::Goto) -> Option<Vec<Statement>> {
        match self {
            Self::Forward { label, signal } | Self::Restart { label, signal }
                if goto.0.0 == *label =>
            {
                Some(vec![set_local_bool(signal, true)])
            }
            Self::Dispatch {
                label_states,
                state,
                signal,
            } => label_states.get(&goto.0.0).map(|target| {
                vec![
                    set_local_number(state, *target),
                    set_local_bool(signal, true),
                ]
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
enum EscapeBoundary {
    /// Reaching the end of this lexical sequence is enough to reach the target.
    FallThrough,
    /// The sequence is directly inside the synthetic restart/dispatcher loop.
    Continue,
    /// The sequence is inside an original nested loop; escape it one level.
    Break,
}

fn rewrite_escape_sequence(
    statements: Vec<Statement>,
    mode: &EscapeMode<'_>,
    boundary: EscapeBoundary,
) -> (Vec<Statement>, usize) {
    let mut input = statements.into_iter();
    let mut output = Vec::new();

    while let Some(mut statement) = input.next() {
        if let Statement::Goto(goto) = &statement
            && let Some(assignments) = mode.goto_assignments(goto)
        {
            output.extend(assignments);
            match boundary {
                EscapeBoundary::FallThrough => {}
                EscapeBoundary::Continue => output.push(Continue {}.into()),
                EscapeBoundary::Break => output.push(Break {}.into()),
            }
            return (output, 1);
        }

        let child_changes = match &mut statement {
            Statement::If(node) => {
                let then_body = std::mem::take(&mut node.then_block.lock().0);
                let else_body = std::mem::take(&mut node.else_block.lock().0);
                let (then_body, then_changes) = rewrite_escape_sequence(then_body, mode, boundary);
                let (else_body, else_changes) = rewrite_escape_sequence(else_body, mode, boundary);
                node.then_block.lock().0 = then_body;
                node.else_block.lock().0 = else_body;
                then_changes + else_changes
            }
            Statement::While(node) => {
                let body = std::mem::take(&mut node.block.lock().0);
                let (body, changes) = rewrite_escape_sequence(body, mode, EscapeBoundary::Break);
                node.block.lock().0 = body;
                changes
            }
            Statement::Repeat(node) => {
                let body = std::mem::take(&mut node.block.lock().0);
                let (body, changes) = rewrite_escape_sequence(body, mode, EscapeBoundary::Break);
                node.block.lock().0 = body;
                changes
            }
            Statement::NumericFor(node) => {
                let body = std::mem::take(&mut node.block.lock().0);
                let (body, changes) = rewrite_escape_sequence(body, mode, EscapeBoundary::Break);
                node.block.lock().0 = body;
                changes
            }
            Statement::GenericFor(node) => {
                let body = std::mem::take(&mut node.block.lock().0);
                let (body, changes) = rewrite_escape_sequence(body, mode, EscapeBoundary::Break);
                node.block.lock().0 = body;
                changes
            }
            _ => 0,
        };
        output.push(statement);

        if child_changes == 0 {
            continue;
        }

        let remainder = input.collect::<Vec<_>>();
        let (remainder, remainder_changes) = rewrite_escape_sequence(remainder, mode, boundary);
        let signal = mode.signal().clone();
        match boundary {
            EscapeBoundary::FallThrough if !remainder.is_empty() => output.push(
                If::new(
                    RValue::Unary(Unary {
                        value: Box::new(RValue::Local(signal)),
                        operation: crate::UnaryOperation::Not,
                    }),
                    remainder.into(),
                    Block::default(),
                )
                .into(),
            ),
            EscapeBoundary::Continue => output.push(
                If::new(
                    RValue::Local(signal),
                    vec![Continue {}.into()].into(),
                    remainder.into(),
                )
                .into(),
            ),
            EscapeBoundary::Break => output.push(
                If::new(
                    RValue::Local(signal),
                    vec![Break {}.into()].into(),
                    remainder.into(),
                )
                .into(),
            ),
            EscapeBoundary::FallThrough => {}
        }
        return (output, child_changes + remainder_changes);
    }

    (output, 0)
}

/// Structure one direct forward label in `block`.  Returns true only after at
/// least one goto was removed.  Back-edges (a goto in the label's continuation)
/// are left for the loop structurer; mixing the two would turn iteration into a
/// one-shot guard.
fn structure_one_forward_label(block: &mut Block) -> bool {
    for label_index in 0..block.0.len() {
        let Statement::Label(label_statement) = &block.0[label_index] else {
            continue;
        };
        let label = label_statement.0.clone();
        if seq_contains_goto(&block.0[label_index + 1..], &label) {
            continue;
        }
        let Some(first_source) = block.0[..label_index]
            .iter()
            .position(|statement| seq_contains_goto(std::slice::from_ref(statement), &label))
        else {
            continue;
        };
        let mut intervening_labels = FxHashSet::default();
        collect_defined_labels(&block.0[first_source..label_index], &mut intervening_labels);
        if !intervening_labels.is_empty() {
            // Guarding this region would nest another entry label inside an
            // `if`, turning outside edges to that label into illegal scope
            // entries.  The Multiple dispatcher below handles the combined
            // multi-label region without changing lexical visibility.
            continue;
        }

        // Most forward edges are a one-branch skip.  Recover the source-like
        // guard directly before reaching for an escape flag:
        // `if C then goto L end; A; ::L::` -> `if not C then A end`.
        let simple_guard = match &block.0[first_source] {
            Statement::If(node)
                if node.else_block.lock().0.is_empty()
                    && matches!(
                        node.then_block.lock().0.as_slice(),
                        [Statement::Goto(goto)] if goto.0.0 == label
                    ) =>
            {
                Some((node.condition.clone(), true))
            }
            Statement::If(node)
                if node.then_block.lock().0.is_empty()
                    && matches!(
                        node.else_block.lock().0.as_slice(),
                        [Statement::Goto(goto)] if goto.0.0 == label
                    ) =>
            {
                Some((node.condition.clone(), false))
            }
            _ => None,
        };
        if let Some((condition, negate)) = simple_guard
            && first_source + 1 < label_index
            && !seq_contains_goto(&block.0[first_source + 1..label_index], &label)
        {
            let mut region = block.0.drain(first_source..label_index).collect::<Vec<_>>();
            region.remove(0);
            let condition = if negate {
                RValue::Unary(Unary {
                    value: Box::new(condition),
                    operation: crate::UnaryOperation::Not,
                })
            } else {
                condition
            };
            block.0.insert(
                first_source,
                If::new(condition, region.into(), Block::default()).into(),
            );
            return true;
        }

        let escaped = RcLocal::default();
        let region = block.0.drain(first_source..label_index).collect::<Vec<_>>();
        let mode = EscapeMode::Forward {
            label: &label,
            signal: &escaped,
        };
        let (mut region, changes) =
            rewrite_escape_sequence(region, &mode, EscapeBoundary::FallThrough);
        debug_assert!(changes > 0);
        region.insert(0, set_local_bool(&escaped, false));
        block.0.splice(first_source..first_source, region);
        return true;
    }
    false
}

fn structure_forward_gotos(block: &mut Block) -> usize {
    let mut changed = 0;
    for statement in &mut block.0 {
        match statement {
            Statement::If(node) => {
                changed += structure_forward_gotos(&mut node.then_block.lock());
                changed += structure_forward_gotos(&mut node.else_block.lock());
            }
            Statement::While(node) => changed += structure_forward_gotos(&mut node.block.lock()),
            Statement::Repeat(node) => changed += structure_forward_gotos(&mut node.block.lock()),
            Statement::NumericFor(node) => {
                changed += structure_forward_gotos(&mut node.block.lock())
            }
            Statement::GenericFor(node) => {
                changed += structure_forward_gotos(&mut node.block.lock())
            }
            _ => {}
        }
    }
    while structure_one_forward_label(block) {
        changed += 1;
    }
    changed
}

// A single-entry backward edge is a natural loop.  This is the cyclic sibling
// of the forward guard above and handles residual shapes that the CFG loop
// matcher could not recognize after other regions were collapsed:
//
//     ::head::; BODY; if C then goto head end; TAIL
//
// becomes a compact `while true` around BODY.  A restart flag is only needed to
// propagate the back-edge through nested loops; direct back-edges become
// `continue`.  Regions containing control for an *enclosing* loop or another
// label/goto are intentionally left to the more general structurer.

fn structure_one_backward_label(block: &mut Block) -> bool {
    for label_index in 0..block.0.len() {
        let Statement::Label(label_statement) = &block.0[label_index] else {
            continue;
        };
        let label = label_statement.0.clone();
        if seq_contains_goto(&block.0[..label_index], &label) {
            continue; // multiple entry: not a natural single-entry loop
        }
        let Some(last_source_offset) = block.0[label_index + 1..]
            .iter()
            .rposition(|statement| seq_contains_goto(std::slice::from_ref(statement), &label))
        else {
            continue;
        };
        let last_source = label_index + 1 + last_source_offset;
        let region = &block.0[label_index + 1..=last_source];
        if seq_needs_loop(region) || seq_contains_label_or_other_goto(region, &label) {
            continue;
        }

        let restart = RcLocal::default();
        let region = block
            .0
            .drain(label_index + 1..=last_source)
            .collect::<Vec<_>>();
        let mode = EscapeMode::Restart {
            label: &label,
            signal: &restart,
        };
        let (mut region, changes) =
            rewrite_escape_sequence(region, &mode, EscapeBoundary::Continue);
        debug_assert!(changes > 0);
        region.insert(0, set_local_bool(&restart, false));
        if !region.last().is_some_and(is_terminator) {
            region.push(Break {}.into());
        }
        block.0[label_index] = While::new(Literal::Boolean(true).into(), region.into()).into();
        return true;
    }
    false
}

fn structure_backward_gotos(block: &mut Block) -> usize {
    let mut changed = 0;
    for statement in &mut block.0 {
        match statement {
            Statement::If(node) => {
                changed += structure_backward_gotos(&mut node.then_block.lock());
                changed += structure_backward_gotos(&mut node.else_block.lock());
            }
            Statement::While(node) => changed += structure_backward_gotos(&mut node.block.lock()),
            Statement::Repeat(node) => changed += structure_backward_gotos(&mut node.block.lock()),
            Statement::NumericFor(node) => {
                changed += structure_backward_gotos(&mut node.block.lock())
            }
            Statement::GenericFor(node) => {
                changed += structure_backward_gotos(&mut node.block.lock())
            }
            _ => {}
        }
    }
    while structure_one_backward_label(block) {
        changed += 1;
    }
    changed
}

// General irreducible-region fallback (Relooper "Multiple" shape).  The common
// reducible cases above stay as ordinary guards/loops; only a lexical block with
// several mutually-entered direct labels receives a tiny local dispatcher.
// This avoids both source-level goto and whole-function state machines.

fn direct_dispatch_labels(block: &Block) -> Option<FxHashMap<String, usize>> {
    let mut direct = FxHashMap::default();
    let mut all = FxHashSet::default();
    let mut next_state = 1usize; // state 0 is entry before the first label
    for statement in &block.0 {
        if let Statement::Label(label) = statement {
            if direct.insert(label.0.clone(), next_state).is_some() {
                return None;
            }
            next_state += 1;
        }
    }
    collect_defined_labels(&block.0, &mut all);
    (!direct.is_empty() && direct.len() == all.len()).then_some(direct)
}

#[derive(Default)]
struct EnclosingControlCounts {
    breaks: usize,
    continues: usize,
}

/// A dispatcher adds a synthetic `while`, which would capture `break` and
/// `continue` originally targeting the enclosing source loop.  Encode those
/// exits, leave all controls owned by nested original loops untouched, then
/// replay the requested action immediately after the dispatcher.
fn encode_enclosing_loop_controls(
    statements: Vec<Statement>,
    action: &RcLocal,
    counts: &mut EnclosingControlCounts,
) -> Vec<Statement> {
    let mut output = Vec::new();
    for mut statement in statements {
        match &mut statement {
            Statement::Break(_) => {
                counts.breaks += 1;
                output.push(set_local_number(action, 1));
                output.push(Break {}.into());
                break;
            }
            Statement::Continue(_) => {
                counts.continues += 1;
                output.push(set_local_number(action, 2));
                output.push(Break {}.into());
                break;
            }
            Statement::If(node) => {
                let then_body = std::mem::take(&mut node.then_block.lock().0);
                let else_body = std::mem::take(&mut node.else_block.lock().0);
                node.then_block.lock().0 =
                    encode_enclosing_loop_controls(then_body, action, counts);
                node.else_block.lock().0 =
                    encode_enclosing_loop_controls(else_body, action, counts);
                output.push(statement);
            }
            // These loops own their own break/continue.
            Statement::While(_)
            | Statement::Repeat(_)
            | Statement::NumericFor(_)
            | Statement::GenericFor(_) => output.push(statement),
            _ => output.push(statement),
        }
    }
    output
}

fn structure_direct_label_dispatcher(
    block: &mut Block,
    externally_targeted: &FxHashSet<String>,
) -> bool {
    let Some(label_states) = direct_dispatch_labels(block) else {
        return false;
    };
    if label_states
        .keys()
        .any(|label| externally_targeted.contains(label))
    {
        // A parent/sibling edge still needs this lexical label. Consuming it in
        // a child-only dispatcher would leave a dangling goto in the ancestor.
        return false;
    }
    let mut targets = FxHashSet::default();
    collect_goto_targets(block, &mut targets);
    if targets.is_empty()
        || targets
            .iter()
            .any(|target| !label_states.contains_key(target))
    {
        return false;
    }

    // This fallback runs after LocalDeclarer.  Pull declarations at this lexical
    // level outside the synthetic loop so a state transition cannot redeclare
    // and nil-out a value on the next dispatcher iteration.  Initializers remain
    // in their original segment as ordinary assignments.  Declarations inside
    // original nested loops/branches stay there; a nested dispatcher handles its
    // own direct declaration level recursively.
    let mut declarations = Vec::new();
    let mut original = Vec::with_capacity(block.0.len());
    for statement in std::mem::take(&mut block.0) {
        match statement {
            Statement::Assign(mut assign) if assign.prefix => {
                for left in &assign.left {
                    if let LValue::Local(local) = left
                        && !declarations.contains(local)
                    {
                        declarations.push(local.clone());
                    }
                }
                if !assign.right.is_empty() {
                    assign.prefix = false;
                    original.push(Statement::Assign(assign));
                }
            }
            other => original.push(other),
        }
    }

    let state = RcLocal::new(crate::Local::new(Some("controlFlowState".to_string())));
    let jumped = RcLocal::new(crate::Local::new(Some("controlFlowJumped".to_string())));
    let exit_action = RcLocal::new(crate::Local::new(Some("controlFlowExit".to_string())));
    declarations.push(state.clone());
    declarations.push(jumped.clone());
    declarations.push(exit_action.clone());
    let mut segments = vec![Vec::new()];
    for statement in original {
        if matches!(statement, Statement::Label(_)) {
            segments.push(Vec::new());
        } else {
            segments.last_mut().unwrap().push(statement);
        }
    }
    debug_assert_eq!(segments.len(), label_states.len() + 1);

    let mut dispatch_else = Block(vec![Break {}.into()]);
    let mut total_changes = 0;
    let mode = EscapeMode::Dispatch {
        label_states: &label_states,
        state: &state,
        signal: &jumped,
    };
    let mut control_counts = EnclosingControlCounts::default();
    for (segment_state, segment) in segments.into_iter().enumerate().rev() {
        let segment = encode_enclosing_loop_controls(segment, &exit_action, &mut control_counts);
        let (mut segment, changes) =
            rewrite_escape_sequence(segment, &mode, EscapeBoundary::Continue);
        total_changes += changes;
        if !segment.last().is_some_and(is_terminator) {
            if segment_state + 1 < label_states.len() + 1 {
                segment.push(set_local_number(&state, segment_state + 1));
                segment.push(Continue {}.into());
            } else {
                segment.push(Break {}.into());
            }
        }
        let condition = Binary::new(
            RValue::Local(state.clone()),
            Literal::Number(segment_state as f64).into(),
            crate::BinaryOperation::Equal,
        )
        .into();
        dispatch_else = Block(vec![
            If::new(condition, segment.into(), dispatch_else).into(),
        ]);
    }
    debug_assert!(total_changes > 0);

    let dispatcher_body = Block(vec![
        set_local_bool(&jumped, false),
        dispatch_else.0.pop().unwrap(),
    ]);
    let mut declaration = Assign::new(
        declarations.into_iter().map(LValue::Local).collect(),
        Vec::new(),
    );
    declaration.prefix = true;
    block.0 = vec![
        declaration.into(),
        set_local_number(&state, 0),
        set_local_number(&exit_action, 0),
        While::new(Literal::Boolean(true).into(), dispatcher_body).into(),
    ];
    if control_counts.breaks != 0 {
        block.0.push(
            If::new(
                Binary::new(
                    RValue::Local(exit_action.clone()),
                    Literal::Number(1.0).into(),
                    crate::BinaryOperation::Equal,
                )
                .into(),
                vec![Break {}.into()].into(),
                Block::default(),
            )
            .into(),
        );
    }
    if control_counts.continues != 0 {
        block.0.push(
            If::new(
                Binary::new(
                    RValue::Local(exit_action),
                    Literal::Number(2.0).into(),
                    crate::BinaryOperation::Equal,
                )
                .into(),
                vec![Continue {}.into()].into(),
                Block::default(),
            )
            .into(),
        );
    }
    true
}

fn collect_goto_target_counts(block: &Block, counts: &mut FxHashMap<String, usize>) {
    for statement in &block.0 {
        match statement {
            Statement::Goto(goto) => *counts.entry(goto.0.0.clone()).or_default() += 1,
            Statement::If(node) => {
                collect_goto_target_counts(&node.then_block.lock(), counts);
                collect_goto_target_counts(&node.else_block.lock(), counts);
            }
            Statement::While(node) => collect_goto_target_counts(&node.block.lock(), counts),
            Statement::Repeat(node) => collect_goto_target_counts(&node.block.lock(), counts),
            Statement::NumericFor(node) => collect_goto_target_counts(&node.block.lock(), counts),
            Statement::GenericFor(node) => collect_goto_target_counts(&node.block.lock(), counts),
            _ => {}
        }
    }
}

fn child_external_targets(
    whole_counts: &FxHashMap<String, usize>,
    child: &Block,
    inherited: &FxHashSet<String>,
) -> FxHashSet<String> {
    let mut child_counts = FxHashMap::default();
    collect_goto_target_counts(child, &mut child_counts);
    let mut external = inherited.clone();
    for (label, total) in whole_counts {
        if child_counts.get(label).copied().unwrap_or_default() < *total {
            external.insert(label.clone());
        }
    }
    external
}

fn structure_irreducible_dispatchers_inner(
    block: &mut Block,
    externally_targeted: &FxHashSet<String>,
) -> usize {
    if !block_has_goto_or_label(block) {
        return 0;
    }
    let mut whole_counts = FxHashMap::default();
    collect_goto_target_counts(block, &mut whole_counts);
    let mut changed = 0;
    for statement in &mut block.0 {
        match statement {
            Statement::If(node) => {
                let then_external = child_external_targets(
                    &whole_counts,
                    &node.then_block.lock(),
                    externally_targeted,
                );
                let else_external = child_external_targets(
                    &whole_counts,
                    &node.else_block.lock(),
                    externally_targeted,
                );
                changed += structure_irreducible_dispatchers_inner(
                    &mut node.then_block.lock(),
                    &then_external,
                );
                changed += structure_irreducible_dispatchers_inner(
                    &mut node.else_block.lock(),
                    &else_external,
                );
            }
            Statement::While(node) => {
                let external =
                    child_external_targets(&whole_counts, &node.block.lock(), externally_targeted);
                changed +=
                    structure_irreducible_dispatchers_inner(&mut node.block.lock(), &external)
            }
            Statement::Repeat(node) => {
                let external =
                    child_external_targets(&whole_counts, &node.block.lock(), externally_targeted);
                changed +=
                    structure_irreducible_dispatchers_inner(&mut node.block.lock(), &external)
            }
            Statement::NumericFor(node) => {
                let external =
                    child_external_targets(&whole_counts, &node.block.lock(), externally_targeted);
                changed +=
                    structure_irreducible_dispatchers_inner(&mut node.block.lock(), &external)
            }
            Statement::GenericFor(node) => {
                let external =
                    child_external_targets(&whole_counts, &node.block.lock(), externally_targeted);
                changed +=
                    structure_irreducible_dispatchers_inner(&mut node.block.lock(), &external)
            }
            _ => {}
        }
    }
    if structure_direct_label_dispatcher(block, externally_targeted) {
        changed += 1;
    }
    changed
}

pub fn structure_irreducible_dispatchers(block: &mut Block) -> usize {
    structure_irreducible_dispatchers_inner(block, &FxHashSet::default())
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
    let replaced_gotos = replace_label_gotos_with_breaks(&mut hit_region, label, &hit_local, 0)?;
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
            Statement::Goto(g) if labels.contains(&g.0.0) => Some((i, g.0.0.clone())),
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
                if !visible.contains(&g.0.0) {
                    needy.insert(g.0.0.clone());
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
fn fresh_generated_label(
    prefix: &str,
    counter: &mut usize,
    reserved: &mut FxHashSet<String>,
) -> String {
    loop {
        *counter += 1;
        let candidate = format!("{prefix}{}", *counter);
        if reserved.insert(candidate.clone()) {
            return candidate;
        }
    }
}

fn raise_once(
    block: &mut Block,
    needy: &FxHashSet<String>,
    counter: &mut usize,
    reserved: &mut FxHashSet<String>,
) -> bool {
    // first, raise within nested blocks (deeper labels reach their branch top)
    for s in block.0.iter_mut() {
        let raised = match s {
            Statement::If(f) => {
                raise_once(&mut f.then_block.lock(), needy, counter, reserved)
                    || raise_once(&mut f.else_block.lock(), needy, counter, reserved)
            }
            Statement::While(w) => raise_once(&mut w.block.lock(), needy, counter, reserved),
            Statement::Repeat(r) => raise_once(&mut r.block.lock(), needy, counter, reserved),
            Statement::NumericFor(nf) => raise_once(&mut nf.block.lock(), needy, counter, reserved),
            Statement::GenericFor(gf) => raise_once(&mut gf.block.lock(), needy, counter, reserved),
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
        let after = fresh_generated_label("after", counter, reserved);
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
fn unstructure_one_loop(
    block: &mut Block,
    needy: &FxHashSet<String>,
    counter: &mut usize,
    reserved: &mut FxHashSet<String>,
) -> bool {
    for s in block.0.iter_mut() {
        let done = match s {
            Statement::If(f) => {
                unstructure_one_loop(&mut f.then_block.lock(), needy, counter, reserved)
                    || unstructure_one_loop(&mut f.else_block.lock(), needy, counter, reserved)
            }
            Statement::While(w) => {
                unstructure_one_loop(&mut w.block.lock(), needy, counter, reserved)
            }
            Statement::Repeat(r) => {
                unstructure_one_loop(&mut r.block.lock(), needy, counter, reserved)
            }
            Statement::NumericFor(nf) => {
                unstructure_one_loop(&mut nf.block.lock(), needy, counter, reserved)
            }
            Statement::GenericFor(gf) => {
                unstructure_one_loop(&mut gf.block.lock(), needy, counter, reserved)
            }
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
        let head = fresh_generated_label("loop", counter, reserved);
        let exit = fresh_generated_label("exit", counter, reserved);
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
                let cont = fresh_generated_label("loop", counter, reserved);
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

fn raise_labels(block: &mut Block, counter: &mut usize, reserved: &mut FxHashSet<String>) {
    for _ in 0..8192 {
        let mut needy = FxHashSet::default();
        compute_needy(block, &FxHashSet::default(), &mut needy);
        if needy.is_empty() {
            break;
        }
        if raise_once(block, &needy, counter, reserved) {
            continue;
        }
        if unstructure_one_loop(block, &needy, counter, reserved) {
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
    let mut reserved_labels = FxHashSet::default();
    collect_defined_labels(&block.0, &mut reserved_labels);
    let mut fresh_counter = 0usize;
    for _ in 0..256 {
        let mut fixer = GotoFixer {
            continuations: FxHashMap::default(),
            continue_owner: FxHashMap::default(),
            exhausted_for_tail: FxHashSet::default(),
            reserved_labels,
            fresh_counter,
            loop_counter: 0,
        };
        fixer.collect(block, &implicit_return, false, None);
        if fixer.continuations.is_empty() {
            fresh_counter = fixer.fresh_counter;
            reserved_labels = fixer.reserved_labels;
            break;
        }
        fixer.loop_counter = 0;
        let rewritten = fixer.rewrite(block, &implicit_return, None, &FxHashSet::default());
        fresh_counter = fixer.fresh_counter;
        reserved_labels = fixer.reserved_labels;
        if rewritten == 0 {
            break;
        }
    }
    // Large forward continuations deliberately rejected by tail duplication
    // are cheaper and clearer as guarded regions.  Run this only after the
    // duplication fixpoint so the common small-tail output stays unchanged.
    structure_forward_gotos(block);
    structure_backward_gotos(block);
    for _ in 0..128 {
        if structure_loop_entry_gotos(block) == 0 {
            break;
        }
    }
    raise_labels(block, &mut fresh_counter, &mut reserved_labels);
    // Raising exposes the last cross-scope edges at one lexical level.  Prefer
    // ordinary forward guards and natural loops once more; if an irreducible
    // multi-entry region remains, localize a Relooper Multiple dispatcher to
    // exactly that block.
    for _ in 0..128 {
        let changed = structure_forward_gotos(block) + structure_backward_gotos(block);
        if changed == 0 {
            break;
        }
    }
    fold_constant_conditions(block);
    let mut targets = FxHashSet::default();
    collect_goto_targets(block, &mut targets);
    remove_dead_labels(block, &targets);
}

/// Return whether this function body contains a `Goto` or `Label` statement.
/// Structured child blocks are included; nested closures are separate functions
/// and are intentionally not crossed. Cheap (early-exits on the first hit); used
/// by `simplify_gotos` to skip its goto machinery entirely.
pub fn block_has_goto_or_label(block: &Block) -> bool {
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

/// Final output invariant over a complete linked function tree.  Unlike
/// [`block_has_goto_or_label`], this descends through closure values as well as
/// structured blocks and de-duplicates shared `Function` arcs.
pub fn function_tree_has_goto_or_label(block: &Block) -> bool {
    fn value_has_goto(value: &RValue, visited: &mut FxHashSet<usize>) -> bool {
        if let RValue::Closure(closure) = value {
            let address = Arc::as_ptr(&closure.function.0) as usize;
            return visited.insert(address)
                && tree_block_has_goto(&closure.function.lock().body, visited);
        }
        value
            .rvalues()
            .into_iter()
            .any(|child| value_has_goto(child, visited))
    }

    fn tree_block_has_goto(block: &Block, visited: &mut FxHashSet<usize>) -> bool {
        for statement in &block.0 {
            if matches!(statement, Statement::Goto(_) | Statement::Label(_))
                || crate::deinline::stmt_rvalues(statement)
                    .into_iter()
                    .any(|value| value_has_goto(value, visited))
            {
                return true;
            }
            let nested = match statement {
                Statement::If(node) => {
                    tree_block_has_goto(&node.then_block.lock(), visited)
                        || tree_block_has_goto(&node.else_block.lock(), visited)
                }
                Statement::While(node) => tree_block_has_goto(&node.block.lock(), visited),
                Statement::Repeat(node) => tree_block_has_goto(&node.block.lock(), visited),
                Statement::NumericFor(node) => tree_block_has_goto(&node.block.lock(), visited),
                Statement::GenericFor(node) => tree_block_has_goto(&node.block.lock(), visited),
                _ => false,
            };
            if nested {
                return true;
            }
        }
        false
    }

    tree_block_has_goto(block, &mut FxHashSet::default())
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
    use crate::{BinaryOperation, Closure, Function, Global, Local};
    use by_address::ByAddress;

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
            Block(vec![
                If::new(
                    hit_condition,
                    vec![assign(&stage, bool_lit(false)), goto("tail")].into(),
                    Block::default(),
                )
                .into(),
            ]),
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
            matches!(
                reset_hit.right.as_slice(),
                [RValue::Literal(Literal::Boolean(false))]
            ),
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
        simplify_gotos(&mut block);
        assert!(contains_goto_or_label(&block.0));

        // Production ordering: declarations first, then the irreducible
        // dispatcher.  `stage` must be declared outside its synthetic while so
        // the fallback assignment survives the state transition to `tail`.
        let block = Arc::new(Mutex::new(block));
        crate::local_declarations::LocalDeclarer::default()
            .declare_locals(block.clone(), &FxHashSet::default());
        hoist_locals_for_gotos(&mut block.lock());
        structure_irreducible_dispatchers(&mut block.lock());
        let block = Arc::try_unwrap(block).unwrap().into_inner();
        assert!(
            !contains_goto_or_label(&block.0),
            "the complete pass must still eliminate the guarded loop edge:\n{}",
            block
        );
        let rendered = block.to_string();
        assert!(rendered.contains("controlFlowState"), "{}", rendered);
        assert_eq!(rendered.matches("print(stage)").count(), 1, "{}", rendered);
        assert_eq!(
            rendered.matches("stage = \"fallback\"").count(),
            1,
            "{}",
            rendered
        );
        let Statement::Assign(declaration) = &block.0[0] else {
            panic!(
                "dispatcher locals must be declared before the loop:\n{}",
                block
            );
        };
        assert!(declaration.prefix && declaration.right.is_empty());
        assert!(
            declaration
                .left
                .iter()
                .any(|left| matches!(left, LValue::Local(local) if local == &stage)),
            "stage must persist across dispatcher iterations:\n{}",
            block
        );
        let mut named = block;
        crate::name_locals::name_locals(&mut named, true);
        let named = named.to_string();
        assert!(named.contains("controlFlowState"), "{}", named);
        assert!(named.contains("controlFlowJumped"), "{}", named);
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
                    If::new(global("again"), vec![goto("tail")].into(), Block::default()).into(),
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

    #[test]
    fn structures_large_forward_tail_without_duplication() {
        let mut then_block = Block(vec![goto("tail")]);
        let mut block = Block(vec![
            If::new(
                global("skip"),
                std::mem::take(&mut then_block),
                Block::default(),
            )
            .into(),
            print(&local("before_tail")),
            label("tail"),
        ]);
        for _ in 0..=MAX_DUP_SIZE {
            block.0.push(print(&local("tail_value")));
        }

        simplify_gotos(&mut block);

        assert!(
            !contains_goto_or_label(&block.0),
            "large forward tails must use a guard rather than leak goto:\n{}",
            block
        );
        assert_eq!(
            block.to_string().matches("print(tail_value)").count(),
            MAX_DUP_SIZE + 1,
            "the large tail must not be duplicated:\n{}",
            block
        );
    }

    #[test]
    fn forward_escape_propagates_out_of_nested_loops() {
        let nested = While::new(
            global("running"),
            Block(vec![
                If::new(global("skip"), vec![goto("tail")].into(), Block::default()).into(),
            ]),
        );
        let mut block = Block(vec![
            NumericFor::new(
                Literal::Number(1.0).into(),
                Literal::Number(10.0).into(),
                Literal::Number(1.0).into(),
                local("i"),
                Block(vec![nested.into()]),
            )
            .into(),
            print(&local("skipped")),
            label("tail"),
            print(&local("kept")),
        ]);

        structure_forward_gotos(&mut block);
        let mut targets = FxHashSet::default();
        collect_goto_targets(&block, &mut targets);
        remove_dead_labels(&mut block, &targets);

        assert!(
            !contains_goto_or_label(&block.0),
            "the escape flag must propagate through every nested loop:\n{}",
            block
        );
        assert_eq!(block.to_string().matches("print(skipped)").count(), 1);
        assert_eq!(block.to_string().matches("print(kept)").count(), 1);
    }

    #[test]
    fn final_invariant_descends_into_closures() {
        let function = Arc::new(Mutex::new(Function {
            body: Block(vec![goto("inside"), label("inside")]),
            ..Function::default()
        }));
        let block = Block(vec![
            Return::new(vec![RValue::Closure(Closure {
                function: ByAddress(function),
                upvalues: Vec::new(),
            })])
            .into(),
        ]);

        assert!(!block_has_goto_or_label(&block));
        assert!(function_tree_has_goto_or_label(&block));
    }

    #[test]
    fn structures_single_entry_backward_edge_as_loop() {
        let mut block = Block(vec![
            label("head"),
            print(&local("body")),
            If::new(global("again"), vec![goto("head")].into(), Block::default()).into(),
            print(&local("tail")),
        ]);

        simplify_gotos(&mut block);

        assert!(
            !contains_goto_or_label(&block.0),
            "a single-entry back-edge must become a structured loop:\n{}",
            block
        );
        assert!(matches!(block.0.first(), Some(Statement::While(_))));
        assert_eq!(block.to_string().matches("print(body)").count(), 1);
        assert_eq!(block.to_string().matches("print(tail)").count(), 1);
    }

    #[test]
    fn backward_restart_propagates_out_of_nested_loop() {
        let nested = GenericFor::new(
            vec![local("item")],
            vec![global("items")],
            Block(vec![
                If::new(global("again"), vec![goto("head")].into(), Block::default()).into(),
            ]),
        );
        let mut block = Block(vec![
            label("head"),
            nested.into(),
            print(&local("after_loop")),
        ]);

        simplify_gotos(&mut block);

        assert!(
            !contains_goto_or_label(&block.0),
            "restart must propagate through the nested generic-for:\n{}",
            block
        );
        assert_eq!(block.to_string().matches("print(after_loop)").count(), 1);
    }

    #[test]
    fn exhausted_for_shared_tail_resumes_at_outside_continuation() {
        let mut block = Block(vec![
            GenericFor::new(
                vec![local("item")],
                vec![global("items")],
                Block(vec![
                    If::new(
                        global("stop"),
                        vec![Break {}.into()].into(),
                        Block::default(),
                    )
                    .into(),
                    label("tail"),
                    print(&local("item")),
                ]),
            )
            .into(),
            goto("tail"),
            Return::default().into(),
        ]);

        simplify_gotos(&mut block);

        assert!(!contains_goto_or_label(&block.0), "{}", block);
        let rendered = block.to_string();
        assert_eq!(rendered.matches("print(item)").count(), 2, "{}", rendered);
        assert!(matches!(block.0.last(), Some(Statement::Return(_))));
    }

    #[test]
    fn for_tail_is_not_copied_from_a_source_before_its_owner_loop() {
        let mut block = Block(vec![
            goto("tail"),
            GenericFor::new(
                vec![local("item")],
                vec![global("items")],
                Block(vec![label("tail"), print(&local("item"))]),
            )
            .into(),
            Return::default().into(),
        ]);

        simplify_gotos(&mut block);

        assert!(
            contains_goto_or_label(&block.0),
            "source-side exhaustion must be proven before removing the synthetic for back-edge:\n{}",
            block
        );
        assert_eq!(block.to_string().matches("print(item)").count(), 1);
    }

    #[test]
    fn generated_labels_skip_reserved_names_across_fixpoints() {
        let mut reserved = FxHashSet::default();
        reserved.insert("dup1".to_string());
        let mut fixer = GotoFixer {
            continuations: FxHashMap::default(),
            continue_owner: FxHashMap::default(),
            exhausted_for_tail: FxHashSet::default(),
            reserved_labels: reserved,
            fresh_counter: 0,
            loop_counter: 0,
        };
        let mut sequence = vec![label("original")];
        fixer.relabel_fresh(&mut sequence);
        assert!(matches!(&sequence[0], Statement::Label(label) if label.0 == "dup2"));

        let mut counter = 0;
        let mut reserved = FxHashSet::default();
        reserved.insert("after1".to_string());
        assert_eq!(
            fresh_generated_label("after", &mut counter, &mut reserved),
            "after2"
        );
    }

    #[test]
    fn dispatcher_replays_enclosing_break_and_continue() {
        let body = Block(vec![
            label("a"),
            If::new(global("to_b"), vec![goto("b")].into(), Block::default()).into(),
            Break {}.into(),
            label("b"),
            If::new(global("to_a"), vec![goto("a")].into(), Block::default()).into(),
            Continue {}.into(),
        ]);
        let mut root = Block(vec![While::new(global("running"), body).into()]);

        simplify_gotos(&mut root);
        let root = Arc::new(Mutex::new(root));
        crate::local_declarations::LocalDeclarer::default()
            .declare_locals(root.clone(), &FxHashSet::default());
        hoist_locals_for_gotos(&mut root.lock());
        structure_irreducible_dispatchers(&mut root.lock());
        let root = Arc::try_unwrap(root).unwrap().into_inner();

        assert!(!contains_goto_or_label(&root.0), "{}", root);
        let Statement::While(outer) = &root.0[0] else {
            panic!("expected original enclosing loop:\n{}", root);
        };
        let outer_body = outer.block.lock();
        assert!(
            outer_body.0.iter().any(|statement| matches!(
                statement,
                Statement::If(node)
                    if matches!(node.then_block.lock().0.as_slice(), [Statement::Break(_)])
            )),
            "encoded break must be replayed after the dispatcher:\n{}",
            root
        );
        assert!(
            outer_body.0.iter().any(|statement| matches!(
                statement,
                Statement::If(node)
                    if matches!(node.then_block.lock().0.as_slice(), [Statement::Continue(_)])
            )),
            "encoded continue must be replayed after the dispatcher:\n{}",
            root
        );
    }

    #[test]
    fn dispatcher_never_consumes_label_targeted_from_parent_scope() {
        let child = Block(vec![
            label("a"),
            If::new(global("to_b"), vec![goto("b")].into(), Block::default()).into(),
            label("b"),
            If::new(global("to_a"), vec![goto("a")].into(), Block::default()).into(),
            Break {}.into(),
        ]);
        let mut root = Block(vec![While::new(global("running"), child).into(), goto("a")]);

        assert_eq!(structure_irreducible_dispatchers(&mut root), 0);
        assert!(
            contains_goto_or_label(&root.0),
            "an open child region must remain intact for an outer structurer or the final fail-closed gate:\n{}",
            root
        );
        let Statement::While(loop_node) = &root.0[0] else {
            panic!("expected original loop")
        };
        assert!(matches!(
            loop_node.block.lock().0.first(),
            Some(Statement::Label(label)) if label.0 == "a"
        ));
    }
}
