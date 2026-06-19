//! §2.8 OOP method-colon recovery + `self`.
//!
//! `function T:method(...)` is exact Lua sugar for `function T.method(self, ...)`.
//! On real decompiler output the implicit receiver survives as an ordinary first
//! parameter named `p`/`pN` (never `self`), so the formatter — which only renders
//! colon-form when param[0] is literally named `"self"` (path-1,
//! formatter.rs:1255) or is wholly unused with a matching call site (path-2) —
//! keeps the dot-form for the common OOP idiom `function T.method(p) ... p.field
//! / p:siblingMethod() ...`.
//!
//! This pass detects genuine *method definitions* (an assignment of a closure to
//! `Prefix.name`) whose first parameter is genuinely the receiver, and renames
//! that parameter's [`RcLocal`] to `Some("self")`. Because `RcLocal` identity is
//! id-based (the same `Arc` is shared at every body occurrence) the rename shows
//! everywhere the parameter is used, and the formatter's path-1 then emits
//! `function T:method()` with the body reading `self.field`.
//!
//! Renaming the definition to colon-form is ALWAYS runtime-safe: it only declares
//! the existing first parameter as the implicit `self`. Call sites are rendered
//! from separate AST nodes and are untouched — a dot-call `T.method(obj, x)` and
//! a colon-call `obj:method(x)` both still bind `obj` to `self`; no argument is
//! added or dropped. The gate below exists purely for *fidelity* (don't convert a
//! plain static utility whose first arg merely happens to be indexed), never for
//! correctness.
//!
//! ## Gate (convert iff `base` holds AND at least one receiver signal fires)
//!
//! `base` (all required — B2/B3 are load-bearing: violating them makes the
//! formatter silently emit invalid `function T.method(self, self)` / shadowed
//! `self`):
//!   * **B1** param[0] is never reassigned in the body.
//!   * **B2** no *other* parameter is already named `"self"`.
//!   * **B3** the body mentions no local/global named `"self"`.
//!
//! receiver signals (>= 1 required):
//!   * **sibling_a** param[0] is the receiver of `p0:X(...)` where `X` is a method
//!     defined on the SAME prefix elsewhere in the script (a true
//!     `self:otherMethod()` self-call). Raw `p0:anyMethod()` is BANNED — 55/57 of
//!     its false positives are static utils calling Roblox-API methods like
//!     `npcModel:FindFirstChild`.
//!   * **b** param[0] is the base of an assignment-LHS index (`p0.field = ..` /
//!     `p0[k] = ..`).
//!   * **c** param[0] is the base of an index whose key is an underscore-prefixed
//!     identifier (`p0._private`), read or written.
//!   * **d** the method name occurs anywhere in the script as a colon-call
//!     `<expr>:method(`, UNLESS the same name also appears as a static dot-call
//!     `Prefix.method(arg, ..)` (shaves the `Create`-vs-`TweenService:Create`
//!     collision).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    Block, Index, LValue, Literal, LocalRw, RValue, RcLocal, Select, Statement, Traverse,
};

/// Stable id-key for an `RcLocal` (mirrors name_locals::local_ptr): identity is
/// the address of the inner `Arc<Mutex<Local>>`. Two clones of the same local
/// share the `Arc`, so they share this key.
fn local_ptr(local: &RcLocal) -> usize {
    &*local.0 .0 as *const _ as usize
}

/// `true` iff `name` is a valid (non-keyword) Lua identifier. Replicated from
/// the private `Formatter::is_valid_name` so this module does not couple to the
/// formatter.
fn is_valid_name(name: &[u8]) -> bool {
    if name.is_empty() {
        return false;
    }
    if !name
        .iter()
        .enumerate()
        .all(|(i, &c)| (i != 0 && c.is_ascii_digit()) || c.is_ascii_alphabetic() || c == b'_')
    {
        return false;
    }
    const RESERVED_KEYWORDS: &[&str] = &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
        "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    let name_str = std::str::from_utf8(name).unwrap_or("");
    !RESERVED_KEYWORDS.contains(&name_str)
}

/// `true` iff `value` is a legal prefix for a named-function definition: a
/// Global, a Local, or an index-chain whose every key is a valid-name string.
/// Replicated from the private `Formatter::is_valid_named_function_prefix`.
fn is_valid_named_function_prefix(value: &RValue) -> bool {
    match value {
        RValue::Global(_) | RValue::Local(_) => true,
        RValue::Index(index) => {
            matches!(
                index.right.as_ref(),
                RValue::Literal(Literal::String(key)) if is_valid_name(key)
            ) && is_valid_named_function_prefix(&index.left)
        }
        _ => false,
    }
}

/// If `name` is a method-definition LHS (`Prefix.method` with a valid name and a
/// valid prefix), return `(prefix, method)`. Mirrors
/// `Formatter::colon_method_target`.
fn method_target(name: &LValue) -> Option<(&RValue, &str)> {
    let LValue::Index(index) = name else {
        return None;
    };
    let RValue::Literal(Literal::String(method)) = index.right.as_ref() else {
        return None;
    };
    if !is_valid_name(method) || !is_valid_named_function_prefix(&index.left) {
        return None;
    }
    Some((&index.left, std::str::from_utf8(method).ok()?))
}

/// Module-level facts gathered in a single pre-scan of the whole body tree.
#[derive(Default)]
struct ScriptScan {
    /// For every method definition in the script: methodName -> the set of
    /// prefixes that define it. Keyed by name (a cheap hashable `String`) because
    /// `RValue` is only `PartialEq` (it carries an `f64`), so prefixes are
    /// compared linearly within the (tiny) per-name bucket. `RValue: PartialEq`
    /// is id-based for locals and structural elsewhere, so prefixes compare
    /// correctly. Used by receiver signal `sibling_a`.
    sibling_defs: FxHashMap<String, Vec<RValue>>,
    /// Every method name called colon-style (`<expr>:method(`) anywhere — signal
    /// `d`.
    colon_call_methods: FxHashSet<String>,
    /// Every method name called as a *static dot-call* (`Prefix.method(arg, ..)`,
    /// i.e. a plain `Call` whose callee is `Index{ .., String(method) }`) — used
    /// to suppress signal `d`.
    static_dot_call_methods: FxHashSet<String>,
}

/// Entry point. Pre-scan the whole tree for module-level facts, then walk it
/// again converting each genuine method definition in place.
pub fn recover_methods(block: &mut Block) {
    let mut scan = ScriptScan::default();
    scan_block(block, &mut scan);
    convert_block(block, &scan);
}

// ---------------------------------------------------------------------------
// Pre-scan (read-only): gather sibling defs + colon/static-dot call method names
// ---------------------------------------------------------------------------

fn scan_block(block: &Block, scan: &mut ScriptScan) {
    for statement in block.iter() {
        scan_statement(statement, scan);
    }
}

fn scan_statement(statement: &Statement, scan: &mut ScriptScan) {
    // Method definitions (for sibling_a).
    if let Statement::Assign(assign) = statement {
        if assign.left.len() == 1
            && assign.right.len() == 1
            && matches!(assign.right[0], RValue::Closure(_))
        {
            if let Some((prefix, method)) = method_target(&assign.left[0]) {
                let bucket = scan.sibling_defs.entry(method.to_string()).or_default();
                if !bucket.contains(prefix) {
                    bucket.push(prefix.clone());
                }
            }
        }
    }

    // Statement-position colon call: `obj:method(...)`.
    if let Statement::MethodCall(method_call) = statement {
        scan.colon_call_methods.insert(method_call.method.clone());
    }

    // Statement-position plain call: `Prefix.method(arg, ..)` (static dot-call).
    // `statement.rvalues()` only yields the call's *children* (callee + args),
    // never the `Call` node itself, so it must be matched here directly.
    if let Statement::Call(call) = statement {
        note_static_dot_call(call, scan);
    }

    // Expression-position calls inside this statement (descends into nested
    // closures too — a call there is still "somewhere in the script").
    for rvalue in statement.rvalues() {
        scan_rvalue(rvalue, scan);
    }

    // Recurse into nested blocks.
    for_each_child_block(statement, |child| scan_block(child, scan));
}

fn scan_rvalue(rvalue: &RValue, scan: &mut ScriptScan) {
    match rvalue {
        RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
            scan.colon_call_methods.insert(method_call.method.clone());
        }
        RValue::Call(call) | RValue::Select(Select::Call(call)) => {
            note_static_dot_call(call, scan);
        }
        _ => {}
    }

    // Descend into a closure body so calls nested inside lambdas are seen.
    if let RValue::Closure(closure) = rvalue {
        scan_block(&closure.function.lock().body, scan);
    }

    for child in rvalue.rvalues() {
        scan_rvalue(child, scan);
    }
}

/// Record a static dot-call `Prefix.method(arg, ..)` — a plain `Call` whose
/// callee is `Index{ .., String(method) }` — for signal-`d` suppression.
fn note_static_dot_call(call: &crate::Call, scan: &mut ScriptScan) {
    if let RValue::Index(index) = call.value.as_ref() {
        if let RValue::Literal(Literal::String(method)) = index.right.as_ref() {
            if is_valid_name(method) {
                scan.static_dot_call_methods
                    .insert(String::from_utf8_lossy(method).into_owned());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion pass
// ---------------------------------------------------------------------------

fn convert_block(block: &mut Block, scan: &ScriptScan) {
    for statement in &mut block.0 {
        convert_statement(statement, scan);
    }
}

fn convert_statement(statement: &mut Statement, scan: &ScriptScan) {
    // Convert any method definitions reachable from this statement, including
    // those that are the RHS of an assignment or nested inside other closures.
    // We handle the direct `Assign -> Closure` here, and recurse into every
    // closure body (so a method defined inside another function is also seen).
    if let Statement::Assign(assign) = statement {
        if assign.left.len() == 1 && assign.right.len() == 1 {
            // Borrow the method name (and validate the LHS shape) before taking a
            // mutable borrow of the RHS closure.
            let target = method_target(&assign.left[0])
                .map(|(prefix, method)| (prefix.clone(), method.to_string()));
            if let Some((prefix, method)) = target {
                if let RValue::Closure(closure) = &assign.right[0] {
                    try_convert_method(&prefix, &method, closure, scan);
                }
            }
        }
    }

    // Recurse into nested closure bodies (method defs / receivers can live
    // inside other functions).
    let mut nested = Vec::new();
    collect_nested_functions_in_statement(statement, &mut nested);
    for function in nested {
        convert_block(&mut function.lock().body, scan);
    }

    // Recurse into nested control-flow blocks.
    for_each_child_block_mut(statement, |child| convert_block(child, scan));
}

/// Evaluate the AND-gate for one method definition and, if it passes, rename
/// param[0] to `self`.
fn try_convert_method(
    prefix: &RValue,
    method: &str,
    closure: &crate::Closure,
    scan: &ScriptScan,
) {
    let function = closure.function.lock();
    let Some(p0) = function.parameters.first() else {
        return;
    };

    // Already a `self` receiver (idempotence / pre-existing) — nothing to do.
    if p0.0 .0.lock().0.as_deref() == Some("self") {
        return;
    }

    let p0_key = local_ptr(p0);

    // ---- base gate ----
    // B2: no OTHER parameter is named "self".
    if function
        .parameters
        .iter()
        .skip(1)
        .any(|param| param.0 .0.lock().0.as_deref() == Some("self"))
    {
        return;
    }
    // B3: body mentions no local/global "self".
    if block_mentions_self_name(&function.body) {
        return;
    }
    // B1: param[0] never reassigned in the body.
    if block_writes_local(&function.body, p0_key) {
        return;
    }

    // ---- receiver signals (>= 1) ----
    let mut signals = ReceiverSignals::default();
    gather_receiver_signals(&function.body, prefix, p0_key, scan, &mut signals);

    // signal d: method called colon-style anywhere, unless also a static dot-call.
    let signal_d = scan.colon_call_methods.contains(method)
        && !scan.static_dot_call_methods.contains(method);

    if !(signals.sibling_a || signals.assign_lhs_index || signals.underscore_field || signal_d) {
        return;
    }

    // All gates passed: declare param[0] as `self`. Leave it IN the parameters
    // Vec — the formatter strips index 0 when rendering colon-form; popping it
    // would desync param/arg counts.
    p0.0 .0.lock().0 = Some("self".into());
}

#[derive(Default)]
struct ReceiverSignals {
    sibling_a: bool,
    assign_lhs_index: bool,
    underscore_field: bool,
}

/// Walk a method body collecting the per-body receiver signals for param[0]
/// (`p0_key`). Descends into nested blocks AND nested closures (a capture of the
/// receiver counts).
fn gather_receiver_signals(
    block: &Block,
    self_prefix: &RValue,
    p0_key: usize,
    scan: &ScriptScan,
    out: &mut ReceiverSignals,
) {
    for statement in block.iter() {
        // signal b: assignment-LHS index whose base is p0 (`p0.field = ..`).
        if let Statement::Assign(assign) = statement {
            for lvalue in &assign.left {
                if let LValue::Index(index) = lvalue {
                    if rvalue_is_local(&index.left, p0_key) {
                        out.assign_lhs_index = true;
                        // signal c: written underscore field.
                        if index_key_is_underscore(index) {
                            out.underscore_field = true;
                        }
                    }
                }
            }
        }

        // Statement-position method call: `p0:X(...)`.
        if let Statement::MethodCall(method_call) = statement {
            check_sibling_call(method_call, self_prefix, p0_key, scan, out);
        }

        // Expression-position signals (reads of `p0._x`, `p0:X(...)`, nested
        // closures capturing p0, etc.).
        for rvalue in statement.rvalues() {
            gather_signals_in_rvalue(rvalue, self_prefix, p0_key, scan, out);
        }

        for_each_child_block(statement, |child| {
            gather_receiver_signals(child, self_prefix, p0_key, scan, out)
        });
    }
}

fn gather_signals_in_rvalue(
    rvalue: &RValue,
    self_prefix: &RValue,
    p0_key: usize,
    scan: &ScriptScan,
    out: &mut ReceiverSignals,
) {
    match rvalue {
        // signal c (read): `p0._private`.
        RValue::Index(index) => {
            if rvalue_is_local(&index.left, p0_key) && index_key_is_underscore(index) {
                out.underscore_field = true;
            }
        }
        // signal a: `p0:X(...)` where X is a sibling method.
        RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
            check_sibling_call(method_call, self_prefix, p0_key, scan, out);
        }
        // Descend into nested closures (capture of the receiver counts).
        RValue::Closure(closure) => {
            gather_receiver_signals(
                &closure.function.lock().body,
                self_prefix,
                p0_key,
                scan,
                out,
            );
        }
        _ => {}
    }

    for child in rvalue.rvalues() {
        gather_signals_in_rvalue(child, self_prefix, p0_key, scan, out);
    }
}

/// signal a: a colon-call whose receiver is p0 AND whose method is defined on the
/// SAME prefix as the current method (a genuine `self:otherMethod()` self-call).
fn check_sibling_call(
    method_call: &crate::MethodCall,
    self_prefix: &RValue,
    p0_key: usize,
    scan: &ScriptScan,
    out: &mut ReceiverSignals,
) {
    if rvalue_is_local(&method_call.value, p0_key)
        && scan
            .sibling_defs
            .get(&method_call.method)
            .is_some_and(|prefixes| prefixes.iter().any(|p| p == self_prefix))
    {
        out.sibling_a = true;
    }
}

// ---------------------------------------------------------------------------
// Small predicates
// ---------------------------------------------------------------------------

fn rvalue_is_local(rvalue: &RValue, key: usize) -> bool {
    matches!(rvalue, RValue::Local(local) if local_ptr(local) == key)
}

/// `true` iff the index key is an underscore-prefixed identifier (`p0._private`).
fn index_key_is_underscore(index: &Index) -> bool {
    matches!(
        index.right.as_ref(),
        RValue::Literal(Literal::String(key))
            if key.first() == Some(&b'_') && is_valid_name(key)
    )
}

/// `true` iff some statement writes the local identified by `key` (full
/// traversal incl. nested blocks AND nested closures — a write captured in a
/// lambda still disqualifies B1).
fn block_writes_local(block: &Block, key: usize) -> bool {
    block.iter().any(|s| statement_writes_local(s, key))
}

fn statement_writes_local(statement: &Statement, key: usize) -> bool {
    if statement
        .values_written()
        .into_iter()
        .any(|w| local_ptr(w) == key)
    {
        return true;
    }
    // Writes nested inside closures captured by this statement.
    if statement
        .rvalues()
        .into_iter()
        .any(|r| rvalue_writes_local(r, key))
    {
        return true;
    }
    let mut found = false;
    for_each_child_block(statement, |child| {
        found = found || block_writes_local(child, key);
    });
    found
}

fn rvalue_writes_local(rvalue: &RValue, key: usize) -> bool {
    if let RValue::Closure(closure) = rvalue {
        if block_writes_local(&closure.function.lock().body, key) {
            return true;
        }
    }
    rvalue
        .rvalues()
        .into_iter()
        .any(|child| rvalue_writes_local(child, key))
}

/// `true` iff the block mentions any local/global named `"self"` (full traversal
/// incl. nested blocks and closures). Mirrors the semantics of the formatter's
/// private `block_mentions_self_name`.
fn block_mentions_self_name(block: &Block) -> bool {
    block.iter().any(statement_mentions_self_name)
}

fn statement_mentions_self_name(statement: &Statement) -> bool {
    if statement
        .values_read()
        .into_iter()
        .chain(statement.values_written())
        .any(local_is_named_self)
    {
        return true;
    }
    if statement.rvalues().into_iter().any(rvalue_mentions_self_name) {
        return true;
    }
    if let Statement::Assign(assign) = statement {
        if assign.left.iter().any(lvalue_mentions_self_name) {
            return true;
        }
    }
    let mut found = false;
    for_each_child_block(statement, |child| {
        found = found || block_mentions_self_name(child);
    });
    found
}

fn lvalue_mentions_self_name(lvalue: &LValue) -> bool {
    match lvalue {
        LValue::Local(local) => local_is_named_self(local),
        LValue::Global(global) => global.0.as_slice() == b"self",
        LValue::Index(index) => {
            rvalue_mentions_self_name(&index.left) || rvalue_mentions_self_name(&index.right)
        }
    }
}

fn rvalue_mentions_self_name(rvalue: &RValue) -> bool {
    match rvalue {
        RValue::Local(local) => local_is_named_self(local),
        RValue::Global(global) => global.0.as_slice() == b"self",
        RValue::Closure(closure) => {
            let function = closure.function.lock();
            // A nested closure that itself declares a `self` parameter shadows the
            // name, so its body's `self` references do not count for the OUTER
            // method (matches Formatter::rvalue_mentions_self_name).
            !function.parameters.iter().any(local_is_named_self)
                && block_mentions_self_name(&function.body)
        }
        _ => rvalue.rvalues().into_iter().any(rvalue_mentions_self_name),
    }
}

fn local_is_named_self(local: &RcLocal) -> bool {
    local.0 .0.lock().0.as_deref() == Some("self")
}

// ---------------------------------------------------------------------------
// Child-block traversal helpers (read-only and mutable)
// ---------------------------------------------------------------------------

fn for_each_child_block(statement: &Statement, mut f: impl FnMut(&Block)) {
    match statement {
        Statement::If(r#if) => {
            f(&r#if.then_block.lock());
            f(&r#if.else_block.lock());
        }
        Statement::While(r#while) => f(&r#while.block.lock()),
        Statement::Repeat(repeat) => f(&repeat.block.lock()),
        Statement::NumericFor(numeric_for) => f(&numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => f(&generic_for.block.lock()),
        _ => {}
    }
}

fn for_each_child_block_mut(statement: &mut Statement, mut f: impl FnMut(&mut Block)) {
    match statement {
        Statement::If(r#if) => {
            f(&mut r#if.then_block.lock());
            f(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => f(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => f(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => f(&mut numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => f(&mut generic_for.block.lock()),
        _ => {}
    }
}

/// Collect the `Function` handles of every closure reachable from `statement`'s
/// rvalues (so we can recurse into method defs nested inside other functions).
/// Mirrors inline_temps::inline_closures_in_statement.
fn collect_nested_functions_in_statement(
    statement: &mut Statement,
    out: &mut Vec<by_address::ByAddress<triomphe::Arc<parking_lot::Mutex<crate::Function>>>>,
) {
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            out.push(closure.function.clone());
        }
        None
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Assign, Block, Call, Closure, Function, Global, Index, LValue, Literal, Local, MethodCall,
        RValue, RcLocal, Return, Statement,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn local_value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global(name.as_bytes().to_vec()))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    /// `Prefix.method = function(params) body end`
    fn method_assignment(
        prefix: RValue,
        method: &str,
        parameters: Vec<RcLocal>,
        body: Block,
    ) -> Statement {
        let function = Function {
            parameters,
            body,
            ..Default::default()
        };
        Assign::new(
            vec![LValue::Index(Index::new(prefix, string(method)))],
            vec![RValue::Closure(Closure {
                function: ByAddress(Arc::new(Mutex::new(function))),
                upvalues: Vec::new(),
            })],
        )
        .into()
    }

    fn method_call_stmt(receiver: RValue, method: &str, arguments: Vec<RValue>) -> Statement {
        MethodCall::new(receiver, method.to_string(), arguments).into()
    }

    /// First-parameter name of the method-def assignment at `block[index]`.
    fn first_param_name(block: &Block, index: usize) -> Option<String> {
        let Statement::Assign(assign) = &block.0[index] else {
            panic!("expected assign");
        };
        let RValue::Closure(closure) = &assign.right[0] else {
            panic!("expected closure");
        };
        let function = closure.function.lock();
        let name = function.parameters[0].0 .0.lock().0.clone();
        name
    }

    // --- conversions ---

    #[test]
    fn converts_when_sibling_self_call() {
        // function T.Update(p) p:Helper() end ; function T.Helper(p) end
        // p:Helper is a sibling method on T -> signal a.
        let t = global("T");
        let p = local("p");
        let p2 = local("p");
        let block = Block(vec![
            method_assignment(
                t.clone(),
                "Update",
                vec![p.clone()],
                Block(vec![method_call_stmt(local_value(&p), "Helper", vec![])]),
            ),
            method_assignment(t.clone(), "Helper", vec![p2], Block::default()),
        ]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("self"));
    }

    #[test]
    fn converts_when_underscore_field_read() {
        // function T.Get(p) return p._value end -> signal c.
        let t = global("T");
        let p = local("p");
        let block = Block(vec![method_assignment(
            t,
            "Get",
            vec![p.clone()],
            Block(vec![Return::new(vec![RValue::Index(Index::new(
                local_value(&p),
                string("_value"),
            ))])
            .into()]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("self"));
    }

    #[test]
    fn converts_when_field_assigned() {
        // function T.Set(p) p.x = 1 end -> signal b.
        let t = global("T");
        let p = local("p");
        let block = Block(vec![method_assignment(
            t,
            "Set",
            vec![p.clone()],
            Block(vec![Assign::new(
                vec![LValue::Index(Index::new(local_value(&p), string("x")))],
                vec![number(1.0)],
            )
            .into()]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("self"));
    }

    #[test]
    fn converts_when_method_called_colon_style_elsewhere() {
        // function T.Run(p) ... (no per-body signal) ... and `obj:Run()` somewhere
        // -> signal d. Body only reads a non-underscore field so b/c/a don't fire.
        let t = global("T");
        let p = local("p");
        let obj = local("obj");
        let block = Block(vec![
            method_assignment(
                t,
                "Run",
                vec![p.clone()],
                Block(vec![Return::new(vec![RValue::Index(Index::new(
                    local_value(&p),
                    string("Public"),
                ))])
                .into()]),
            ),
            method_call_stmt(local_value(&obj), "Run", vec![]),
        ]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("self"));
    }

    #[test]
    fn converts_receiver_deref_inside_nested_closure() {
        // function T.Get(p) local f = function() return p._x end end -> signal c
        // fires only by descending into the nested closure.
        let t = global("T");
        let p = local("p");
        let inner = Function {
            parameters: vec![],
            body: Block(vec![Return::new(vec![RValue::Index(Index::new(
                local_value(&p),
                string("_x"),
            ))])
            .into()]),
            ..Default::default()
        };
        let nested_closure = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(inner))),
            upvalues: Vec::new(),
        });
        let f = local("f");
        let block = Block(vec![method_assignment(
            t,
            "Get",
            vec![p.clone()],
            Block(vec![
                Assign::new(vec![LValue::Local(f)], vec![nested_closure]).into()
            ]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("self"));
    }

    // --- correctly kept as dot ---

    #[test]
    fn keeps_dot_when_sibling_name_on_different_prefix() {
        // function T.Update(p) p:Helper() end ; function U.Helper(p) end
        // `Helper` is defined on U, not on p0's own prefix T, so sibling_a must
        // NOT fire — this exercises the load-bearing prefix-equality check in
        // check_sibling_call. "Update" is never colon-called, so signal d is
        // absent too. T.Update must keep dot (first param stays "p").
        let t = global("T");
        let u = global("U");
        let p = local("p");
        let p2 = local("p");
        let mut block = Block(vec![
            method_assignment(
                t,
                "Update",
                vec![p.clone()],
                Block(vec![method_call_stmt(local_value(&p), "Helper", vec![])]),
            ),
            method_assignment(u, "Helper", vec![p2], Block::default()),
        ]);
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("p"));
    }

    #[test]
    fn keeps_dot_for_static_util_calling_roblox_api() {
        // function Util.Validate(p) p:FindFirstChild("X") end
        // FindFirstChild is NOT a sibling method on Util -> raw colon-call is
        // banned, no other signal -> keep dot.
        let util = global("Util");
        let p = local("p");
        let block = Block(vec![method_assignment(
            util,
            "Validate",
            vec![p.clone()],
            Block(vec![method_call_stmt(
                local_value(&p),
                "FindFirstChild",
                vec![string("X")],
            )]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("p"));
    }

    #[test]
    fn keeps_dot_for_public_field_read_only() {
        // function Vec.Mag(p) return p.X end -> only a public (non-underscore)
        // field read, no other signal -> keep dot.
        let vec_ = global("Vec");
        let p = local("p");
        let block = Block(vec![method_assignment(
            vec_,
            "Mag",
            vec![p.clone()],
            Block(vec![Return::new(vec![RValue::Index(Index::new(
                local_value(&p),
                string("X"),
            ))])
            .into()]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("p"));
    }

    #[test]
    fn keeps_dot_for_value_only_argument() {
        // function Util.Apply(p) Util.Other(p) end -> p only forwarded as a value.
        let util = global("Util");
        let p = local("p");
        let block = Block(vec![method_assignment(
            util.clone(),
            "Apply",
            vec![p.clone()],
            Block(vec![Call::new(
                RValue::Index(Index::new(util, string("Other"))),
                vec![local_value(&p)],
            )
            .into()]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("p"));
    }

    #[test]
    fn keeps_dot_when_param_reassigned() {
        // function T.Get(p) p._x ; p = nil end -> B1 fails even though c fires.
        let t = global("T");
        let p = local("p");
        let block = Block(vec![method_assignment(
            t,
            "Get",
            vec![p.clone()],
            Block(vec![
                Return::new(vec![RValue::Index(Index::new(
                    local_value(&p),
                    string("_x"),
                ))])
                .into(),
                Assign::new(
                    vec![LValue::Local(p.clone())],
                    vec![RValue::Literal(Literal::Nil)],
                )
                .into(),
            ]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("p"));
    }

    #[test]
    fn keeps_dot_when_later_param_is_self() {
        // function T.Get(p, self) p._x end -> B2 fails.
        let t = global("T");
        let p = local("p");
        let self_param = local("self");
        let block = Block(vec![method_assignment(
            t,
            "Get",
            vec![p.clone(), self_param],
            Block(vec![Return::new(vec![RValue::Index(Index::new(
                local_value(&p),
                string("_x"),
            ))])
            .into()]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("p"));
    }

    #[test]
    fn keeps_dot_when_body_mentions_self() {
        // function T.Get(p) local self = p ; return p._x end -> B3 fails.
        let t = global("T");
        let p = local("p");
        let self_local = local("self");
        let block = Block(vec![method_assignment(
            t,
            "Get",
            vec![p.clone()],
            Block(vec![
                Assign::new(vec![LValue::Local(self_local)], vec![local_value(&p)]).into(),
                Return::new(vec![RValue::Index(Index::new(
                    local_value(&p),
                    string("_x"),
                ))])
                .into(),
            ]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("p"));
    }

    #[test]
    fn suppresses_signal_d_when_also_static_dot_call() {
        // function T.Create(p) return p.X end (only public read) and BOTH a
        // colon-call `obj:Create()` and a static dot-call `TweenService.Create(x)`
        // exist -> signal d suppressed, no other signal -> keep dot.
        let t = global("T");
        let p = local("p");
        let obj = local("obj");
        let tween = global("TweenService");
        let block = Block(vec![
            method_assignment(
                t,
                "Create",
                vec![p.clone()],
                Block(vec![Return::new(vec![RValue::Index(Index::new(
                    local_value(&p),
                    string("X"),
                ))])
                .into()]),
            ),
            method_call_stmt(local_value(&obj), "Create", vec![]),
            Call::new(
                RValue::Index(Index::new(tween, string("Create"))),
                vec![number(1.0)],
            )
            .into(),
        ]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("p"));
    }

    #[test]
    fn idempotent() {
        // Running twice yields the same result, and an already-`self` param is
        // left alone.
        let t = global("T");
        let p = local("p");
        let block = Block(vec![method_assignment(
            t,
            "Set",
            vec![p.clone()],
            Block(vec![Assign::new(
                vec![LValue::Index(Index::new(local_value(&p), string("x")))],
                vec![number(1.0)],
            )
            .into()]),
        )]);
        let mut block = block;
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("self"));
        recover_methods(&mut block);
        assert_eq!(first_param_name(&block, 0).as_deref(), Some("self"));
    }

    #[test]
    fn recurses_into_nested_method_def() {
        // A method defined inside another function's body is still converted.
        let t = global("T");
        let p = local("p");
        let inner_def = method_assignment(
            t,
            "Inner",
            vec![p.clone()],
            Block(vec![Assign::new(
                vec![LValue::Index(Index::new(local_value(&p), string("y")))],
                vec![number(2.0)],
            )
            .into()]),
        );
        let outer_fn = Function {
            parameters: vec![],
            body: Block(vec![inner_def]),
            ..Default::default()
        };
        let outer_closure = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(outer_fn))),
            upvalues: Vec::new(),
        });
        let setup = local("setup");
        let mut block = Block(vec![Assign::new(
            vec![LValue::Local(setup)],
            vec![outer_closure],
        )
        .into()]);
        recover_methods(&mut block);

        // Reach into the nested method def and check its first param.
        let Statement::Assign(outer_assign) = &block.0[0] else {
            panic!("expected outer assign");
        };
        let RValue::Closure(outer) = &outer_assign.right[0] else {
            panic!("expected outer closure");
        };
        let inner_body = &outer.function.lock().body;
        assert_eq!(first_param_name(inner_body, 0).as_deref(), Some("self"));
    }
}
