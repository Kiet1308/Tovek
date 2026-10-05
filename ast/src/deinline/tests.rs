use super::*;
use super::canonical::{graft, is_open_guard, unguard};
use super::targets::{
    branch_tuple_return, classify_returns, local_tuple_return, loop_return_split,
    returning_nil, value_leaf_shape,
};
use crate::{
    Break, Closure, Empty, ForOrigin, ForPrepKind, Function, Global, Index, Local, MethodCall,
    Table, Upvalue, VmProfileId,
};
use by_address::ByAddress;
use parking_lot::Mutex;
use rustc_hash::FxHashSet;

// Original owned-input implementation, retained as an independent oracle.
fn unguard_reference(mut stmts: Vec<Statement>) -> Vec<Statement> {
    let mut out: Vec<Statement> = Vec::new();
    let mut i = 0;
    while i < stmts.len() {
        if let Statement::If(f) = &stmts[i] {
            // A guard may do work before returning:
            // `if cond then PREFIX; return [X] end; REST`.  Re-nest the shared
            // continuation into the exact structured form produced at inlined
            // sites: `if not cond then REST else PREFIX; return X end`.  For a
            // void return the terminal return is omitted; tail fall-through is
            // equivalent and `PREFIX` remains in the else arm.
            let guard: Option<(Vec<Statement>, Option<RValue>)> = {
                let then = f.then_block.lock();
                let els = f.else_block.lock();
                if els.0.is_empty() {
                    match then.0.split_last() {
                        Some((Statement::Return(r), prefix)) if r.values.is_empty() => {
                            Some((prefix.to_vec(), None))
                        }
                        Some((Statement::Return(r), prefix)) if r.values.len() == 1 => {
                            Some((prefix.to_vec(), Some(r.values[0].clone())))
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            };
            if let Some((mut early_prefix, ret_val)) = guard {
                if i + 1 < stmts.len() {
                    let cond = f.condition.clone();
                    let suffix: Vec<Statement> = stmts.split_off(i + 1);
                    let folded = unguard_reference(suffix);
                    if let Some(x) = ret_val {
                        early_prefix.push(Statement::Return(Return { node_origin: Default::default(), values: vec![x] }));
                    }
                    out.push(Statement::If(If::new(
                        negate_canon(cond),
                        Block(folded),
                        Block(early_prefix),
                    )));
                    return out;
                }
            } else if i + 1 < stmts.len() && is_open_guard(f) {
                let suffix: Vec<Statement> = stmts.split_off(i + 1);
                let folded = unguard_reference(suffix);
                out.extend(graft(vec![stmts[i].clone()], folded));
                return out;
            }
        }
        out.push(stmts[i].clone());
        i += 1;
    }
    out
}


fn local(name: &str) -> RcLocal {
    RcLocal::new(Local::new(Some(name.to_string())))
}

fn global(name: &str) -> RValue {
    RValue::Global(Global::from(name))
}

fn string(value: &str) -> RValue {
    RValue::Literal(Literal::String(value.as_bytes().to_vec()))
}

fn number(value: f64) -> RValue {
    RValue::Literal(Literal::Number(value))
}

fn local_value(local: &RcLocal) -> RValue {
    RValue::Local(local.clone())
}

fn add_one(local: &RcLocal) -> RValue {
    RValue::Binary(Binary::new(
        local_value(local),
        number(1.0),
        BinaryOperation::Add,
    ))
}

fn assign_local(local: &RcLocal, value: RValue, prefix: bool) -> Statement {
    Statement::Assign(Assign {
        node_origin: Default::default(),
        left: vec![LValue::Local(local.clone())],
        right: vec![value],
        prefix,
        parallel: false, compound: false,
    })
}

fn return_one(value: RValue) -> Statement {
    Statement::Return(Return::new(vec![value]))
}

fn print_x() -> Statement {
    Statement::Call(Call::new(global("print"), vec![string("x")]))
}

fn void_target(pat: Vec<Statement>, locals: FxHashSet<RcLocal>) -> Target {
    let pat0 = pat.first().expect("test pat must be non-empty");
    let pat0_kind = std::mem::discriminant(pat0);
    let pat0_anchor_key = stmt_anchor_key(pat0);
    Target {
        f_local: local("f"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Void,
        pat_raw_len: pat.len(),
        pat_spine_len: tail_spine_len(&pat),
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtResultDecl,
        prefix_len: 0,
        pat0_kind,
        pat0_anchor_key,
        pat,
        params: FxHashSet::default(),
        locals,
        param_order: Vec::new(),
        written_params: Vec::new(),
        unread: FxHashSet::default(),
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: false,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: false,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    }
}

#[test]
fn written_upvalues_are_not_local_binders() {
    let state = local("state");
    let other = local("other");
    let pat = canon(&[print_x(), assign_local(&state, add_one(&state), false)]);

    let mut declared = FxHashSet::default();
    collect_declared_locals(&pat, &mut declared);
    assert!(!declared.contains(&state));

    let target = void_target(pat, declared);
    let cand = canon(&[print_x(), assign_local(&other, add_one(&other), false)]);

    assert!(try_unify_site(&target, &cand, &[], None).is_none());
}

#[test]
fn written_argument_copies_keep_order_dependencies_without_truncation() {
    let p = local("p"); let q = local("q"); let a = local("a"); let b = local("b");
    let mut target = void_target(vec![print_x()], [p.clone(), q.clone()].into_iter().collect());
    target.param_order = vec![p.clone(), q.clone()];
    target.written_params = target.param_order.clone();
    let bindings = Bindings { locals: [(p, a.clone()), (q, b.clone())].into_iter().collect(), ..Default::default() };
    let first: RValue = Call::new(global("first"), vec![]).into();
    let last: RValue = Call::new(global("last"), vec![]).into();
    let prefix = vec![(a.clone(), first.clone()), (b.clone(), last.clone())];
    let hit = finish_unified(&target, &[], bindings.clone(), &prefix, None).unwrap();
    // A non-variadic helper drops a trailing call's extra results itself.
    assert!(hit.args.iter().all(|v| matches!(v, RValue::Call(_))));
    assert!(finish_unified(&target, &[], bindings.clone(), &[(b.clone(), last), (a.clone(), first.clone())], None).is_none());
    assert!(finish_unified(&target, &[], bindings, &[(a.clone(), first), (b, a.into())], None).is_none());
}

/// `local name = function() BODY end`
fn helper_decl(name: &RcLocal, body: Vec<Statement>) -> Statement {
    let function = Function { body: Block(body), ..Function::default() };
    assign_local(
        name,
        RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: Vec::new(),
        }),
        true,
    )
}

fn global_call(name: &str, arguments: Vec<RValue>) -> Call {
    Call::new(global(name), arguments)
}

#[test]
fn a_flagged_loop_exit_is_the_helpers_return_only_at_the_end() {
    // local ok = true
    // while c do if d then ok = false; break end end
    // if ok then print("x") end
    let ok = local("ok");
    let exit = |flag: &RcLocal, tail: Vec<Statement>| {
        let mut body = vec![Statement::If(If::new(
            global("d"),
            Block(vec![
                assign_local(flag, boolean(false), false),
                Statement::Break(Break {}),
            ]),
            Block::default(),
        ))];
        body.extend(tail);
        vec![
            assign_local(flag, boolean(true), true),
            Statement::While(While::new(global("c"), Block(body))),
            Statement::If(If::new(local_value(flag), Block(vec![print_x()]), Block::default())),
        ]
    };
    let (unflagged, flags) = unflag_loop_exits(&exit(&ok, Vec::new())).expect("flag lowered");
    assert!(flags.contains(&ok));
    assert_eq!(
        Block(unflagged).to_string(),
        "while c do\n\tif d then\n\t\treturn\n\tend\nend\n\nprint(\"x\")"
    );

    // Something after the flagged `if` runs even when the flag is cleared.
    let mut followed = exit(&ok, Vec::new());
    followed.push(print_x());
    assert!(unflag_loop_exits(&followed).is_none());
    // A `break` that keeps the flag reaches REST, which a `return` skips.
    let plain_break = exit(&ok, vec![Statement::Break(Break {})]);
    assert!(unflag_loop_exits(&plain_break).is_none());
    // REST reads the flag.
    let reads = exit(&ok, Vec::new());
    if let Statement::If(guard) = &reads[2] {
        guard.then_block.lock().0.push(Statement::Call(Call::new(global("print"), vec![local_value(&ok)])));
    }
    assert!(unflag_loop_exits(&reads).is_none());
}

/// `local function findItem(items, name) for _, item in items do if
/// item.Name == name then return item end end return nil end`, inlined
/// into `local found = findItem(list, key)`: the store, a flag and a
/// `break` stand for the `return`; the helper's `return nil` is the
/// declaration's `nil`, its `return fallback` the flag's `if`.
#[test]
fn a_value_returned_from_inside_a_loop_rebuilds_from_its_flagged_exit() {
    let (helper, items, name, item) = (local("findItem"), local("items"), local("name"), local("item"));
    let named = |of: &RcLocal, name: RValue| {
        RValue::Binary(Binary::new(
            RValue::Index(crate::Index::new(local_value(of), string("Name"))),
            name,
            BinaryOperation::Equal,
        ))
    };
    let helper_body = |tail: RValue| {
        vec![
            Statement::GenericFor(GenericFor::new(
                vec![local("_"), item.clone()],
                vec![local_value(&items)],
                Block(vec![Statement::If(If::new(
                    named(&item, local_value(&name)),
                    Block(vec![return_one(local_value(&item))]),
                    Block::default(),
                ))]),
            )),
            return_one(tail),
        ]
    };
    let declare = |helper_body: Vec<Statement>| {
        let declaration = helper_decl(&helper, helper_body);
        if let Statement::Assign(assign) = &declaration
            && let RValue::Closure(closure) = &assign.right[0]
        {
            closure.function.lock().parameters = vec![items.clone(), name.clone()];
        }
        declaration
    };
    let (list, key, found, ok, x) = (local("list"), local("key"), local("found"), local("ok"), local("x"));
    // `exit` is the loop body's leaving arm; `after` follows the loop.
    let site = |exit: Vec<Statement>, after: Vec<Statement>| {
        let mut found_declaration = Assign::new(vec![LValue::Local(found.clone())], vec![RValue::Literal(Literal::Nil)]);
        found_declaration.prefix = true;
        let mut block = vec![
            found_declaration.into(),
            assign_local(&ok, boolean(true), true),
            Statement::GenericFor(GenericFor::new(
                vec![local("_"), x.clone()],
                vec![local_value(&list)],
                Block(vec![Statement::If(If::new(named(&x, local_value(&key)), Block(exit), Block::default()))]),
            )),
        ];
        block.extend(after);
        block.push(Statement::Call(global_call("print", vec![local_value(&found)])));
        block
    };
    let leave = || {
        vec![
            assign_local(&found, local_value(&x), false),
            assign_local(&ok, boolean(false), false),
            Statement::Break(Break {}),
        ]
    };
    let rebuilt = |helper_body: Vec<Statement>, site: Vec<Statement>| {
        let mut block = Block(vec![declare(helper_body)]);
        block.0.extend(site);
        deinline(&mut block);
        block.to_string()
    };

    let output = rebuilt(helper_body(RValue::Literal(Literal::Nil)), site(leave(), Vec::new()));
    assert!(output.contains("local found = findItem(list, key)"), "{output}");
    assert!(!output.contains("break"), "{output}");

    // The helper's `return fallback` runs where the flag is still set.
    let fallback = |found: &RcLocal| {
        Statement::If(If::new(
            local_value(&ok),
            Block(vec![assign_local(found, string("none"), false)]),
            Block::default(),
        ))
    };
    let output = rebuilt(helper_body(string("none")), site(leave(), vec![fallback(&found)]));
    assert!(output.contains("local found = findItem(list, key)"), "{output}");

    // A store that keeps looping is not a `return`.
    let keeps_looping = vec![assign_local(&found, local_value(&x), false)];
    let output = rebuilt(helper_body(RValue::Literal(Literal::Nil)), site(keeps_looping, Vec::new()));
    assert!(!output.contains("findItem(list"), "{output}");
    // A `break` leaving without the flag reaches what the flag guards.
    let mut plain_break = leave();
    plain_break.remove(1);
    let output = rebuilt(helper_body(RValue::Literal(Literal::Nil)), site(plain_break, Vec::new()));
    assert!(!output.contains("findItem(list"), "{output}");
    // The flag is read after the loop.
    let read_flag = vec![Statement::Call(global_call("print", vec![local_value(&ok)]))];
    let output = rebuilt(helper_body(RValue::Literal(Literal::Nil)), site(leave(), read_flag));
    assert!(!output.contains("findItem(list"), "{output}");
    // The fallback stores something else than the helper returns.
    let output = rebuilt(helper_body(string("none")), site(leave(), Vec::new()));
    assert!(!output.contains("findItem(list"), "{output}");
}

/// A numeric `for` returning from two guards (`if a then return "x" end;
/// if b then return "y" end`), copied with `break` exits the structurer
/// writes as `if a then … elseif b then … end`, and a helper falling off
/// its end (the declaration's `nil`), both rebuild.
#[test]
fn a_numeric_loop_with_two_returns_or_none_after_it_rebuilds() {
    let (helper, list, i, n) = (local("classify"), local("list"), local("i"), local("n"));
    let numeric = |counter: &RcLocal, body: Vec<Statement>| {
        Statement::NumericFor(Box::new(NumericFor::new(
            number(1.0),
            RValue::Unary(Unary::new(local_value(&list), UnaryOperation::Length)),
            number(1.0),
            counter.clone(),
            Block(body),
        )))
    };
    let test = |name: &str, of: &RcLocal| RValue::Call(global_call(name, vec![local_value(of)]));
    let guard = |condition: RValue, then_block: Vec<Statement>, else_block: Vec<Statement>| {
        Statement::If(If::new(condition, Block(then_block), Block(else_block)))
    };
    let run = |helper_body: Vec<Statement>, site: Vec<Statement>| {
        let declaration = helper_decl(&helper, helper_body);
        if let Statement::Assign(assign) = &declaration
            && let RValue::Closure(closure) = &assign.right[0]
        {
            closure.function.lock().parameters = vec![list.clone()];
        }
        let mut block = Block(vec![declaration]);
        block.0.extend(site);
        deinline(&mut block);
        block.to_string()
    };
    let (r, ok) = (local("r"), local("ok"));
    let declare_r = || {
        let mut declaration = Assign::new(vec![LValue::Local(r.clone())], vec![RValue::Literal(Literal::Nil)]);
        declaration.prefix = true;
        Statement::from(declaration)
    };
    let leave = |value: RValue| vec![assign_local(&r, value, false), assign_local(&ok, boolean(false), false), Statement::Break(Break {})];
    let use_r = || Statement::Call(global_call("print", vec![local_value(&r)]));

    // for i = 1, #list do if a(i) then return "x" end if b(i) then return "y" end end return "none"
    let helper_body = vec![
        numeric(&i, vec![
            guard(test("a", &i), vec![return_one(string("x"))], vec![]),
            guard(test("b", &i), vec![return_one(string("y"))], vec![]),
        ]),
        return_one(string("none")),
    ];
    let site = vec![
        declare_r(),
        assign_local(&ok, boolean(true), true),
        numeric(&n, vec![guard(test("a", &n), leave(string("x")), vec![guard(test("b", &n), leave(string("y")), vec![])])]),
        guard(local_value(&ok), vec![assign_local(&r, string("none"), false)], vec![]),
        use_r(),
    ];
    let output = run(helper_body, site);
    assert!(output.contains("local r = classify(list)"), "{output}");

    // for i = 1, #list do if a(i) then return i end end (nothing after)
    let helper_body = vec![numeric(&i, vec![guard(test("a", &i), vec![return_one(local_value(&i))], vec![])])];
    let site = vec![
        declare_r(),
        assign_local(&ok, boolean(true), true),
        numeric(&n, vec![guard(test("a", &n), leave(local_value(&n)), vec![])]),
        use_r(),
    ];
    let output = run(helper_body, site);
    assert!(output.contains("local r = classify(list)"), "{output}");
}

/// Luau folds the reads of a constant argument whose truth alone is
/// tested (`visible and 0 or 1` with `false` is `1`), leaving the copy no
/// trace of it: the constant that specializes the body into exactly the
/// copy is passed (`nil`, left out, for a parameter given a default).
#[test]
fn a_constant_argument_folded_away_is_inferred_from_its_truth() {
    let declare = |helper: &RcLocal, parameters: Vec<RcLocal>, body: Vec<Statement>| {
        let declaration = helper_decl(helper, body);
        if let Statement::Assign(assign) = &declaration
            && let RValue::Closure(closure) = &assign.right[0]
        {
            closure.function.lock().parameters = parameters;
        }
        declaration
    };
    let store = |object: &RcLocal, field: &str, value: RValue| {
        Statement::Assign(Assign::new(
            vec![LValue::Index(crate::Index::new(local_value(object), string(field)))],
            vec![value],
        ))
    };
    let method = |object: &RcLocal, name: &str| {
        Statement::MethodCall(MethodCall::new(local_value(object), name.to_string(), Vec::new()))
    };
    let select = |condition: RValue, yes: RValue, no: RValue| {
        RValue::Binary(Binary::new(RValue::Binary(Binary::new(condition, yes, BinaryOperation::And)), no, BinaryOperation::Or))
    };
    let run = |block: Vec<Statement>| {
        let mut block = Block(block);
        deinline(&mut block);
        block.to_string()
    };

    // local function fade(frame, visible) frame.Transparency = visible and 0 or 1; frame:Play() end
    let (fade, frame, visible, part) = (local("fade"), local("frame"), local("visible"), local("part"));
    let fade_body = vec![
        store(&frame, "Transparency", select(local_value(&visible), number(0.0), number(1.0))),
        method(&frame, "Play"),
    ];
    let output = run(vec![
        declare(&fade, vec![frame.clone(), visible.clone()], fade_body.clone()),
        store(&part, "Transparency", number(1.0)),
        method(&part, "Play"),
        store(&part, "Transparency", number(0.0)),
        method(&part, "Play"),
    ]);
    assert!(output.contains("fade(part, false)") && output.contains("fade(part, true)"), "{output}");
    // A value neither constant gives keeps the copy.
    let output = run(vec![
        declare(&fade, vec![frame.clone(), visible.clone()], fade_body),
        store(&part, "Transparency", number(0.5)),
        method(&part, "Play"),
    ]);
    assert!(!output.contains("fade(part"), "{output}");

    // local function attach(item, parent) item.Parent = parent or root; item:Init() end
    let (attach, item, parent, root) = (local("attach"), local("item"), local("parent"), local("root"));
    let output = run(vec![
        declare(&attach, vec![item.clone(), parent.clone()], vec![
            store(&item, "Parent", RValue::Binary(Binary::new(local_value(&parent), local_value(&root), BinaryOperation::Or))),
            method(&item, "Init"),
        ]),
        store(&part, "Parent", local_value(&root)),
        method(&part, "Init"),
    ]);
    assert!(output.contains("attach(part)"), "{output}");

    // local function add(item, timeout) item:Init(); item.Ready = true; if timeout then item.Value = timeout end end:
    // an optional value is left out, a flag is passed `false`.
    let (add, timeout) = (local("add"), local("timeout"));
    let output = run(vec![
        declare(&add, vec![item.clone(), timeout.clone()], vec![
            method(&item, "Init"),
            store(&item, "Ready", RValue::Literal(Literal::Boolean(true))),
            Statement::If(If::new(local_value(&timeout), Block(vec![store(&item, "Value", local_value(&timeout))]), Block::default())),
        ]),
        method(&part, "Init"),
        store(&part, "Ready", RValue::Literal(Literal::Boolean(true))),
    ]);
    assert!(output.contains("add(part)"), "{output}");

    // local function emit(enabled) if enabled then work("a") end end:
    // `false` leaves nothing, which no statement may stand for.
    let (emit, enabled) = (local("emit"), local("enabled"));
    let work = || Statement::Call(global_call("work", vec![string("a")]));
    let output = run(vec![
        declare(&emit, vec![enabled.clone()], vec![Statement::If(If::new(local_value(&enabled), Block(vec![work()]), Block::default()))]),
        print_x(),
        work(),
    ]);
    assert!(output.contains("emit(true)") && !output.contains("emit(false)"), "{output}");

    // local function start(time) cancel("x"); if time then work(time) end end:
    // the whole copy `start(t)` wins over its prefix, `start(nil)`.
    let (start, time, t) = (local("start"), local("time"), local("t"));
    let cancel = || Statement::Call(global_call("cancel", vec![string("x")]));
    let work = |of: &RcLocal| Statement::Call(global_call("work", vec![local_value(of)]));
    let body = |time: &RcLocal| {
        vec![cancel(), Statement::If(If::new(local_value(time), Block(vec![work(time)]), Block::default()))]
    };
    let mut site = body(&t);
    site.insert(0, declare(&start, vec![time.clone()], body(&time)));
    let output = run(site);
    assert!(output.contains("start(t)") && output.matches("cancel(").count() == 1, "{output}");
    // Where the copy goes on with the branch the constant removed (here
    // under another condition), the prefix is no call with `nil`.
    let now = local("now");
    let mut site = vec![
        cancel(),
        Statement::If(If::new(local_value(&now), Block(vec![Statement::Call(global_call("work", vec![local_value(&now), number(1.0)]))]), Block::default())),
    ];
    site.insert(0, declare(&start, vec![time.clone()], body(&time)));
    let output = run(site);
    assert!(output.matches("cancel(").count() == 2, "{output}");
}

#[test]
fn a_local_the_until_condition_reads_is_not_the_helpers() {
    // local helper = function() local ready = make("ready"); print(ready) end
    // repeat local ready = make("ready"); print(ready) until ready
    let body = |ready: &RcLocal| {
        vec![
            assign_local(ready, RValue::Call(global_call("make", vec![string("ready")])), true),
            Statement::Call(global_call("print", vec![local_value(ready)])),
        ]
    };
    let site = |condition: RValue, ready: &RcLocal| {
        Statement::Repeat(Repeat::new(condition, Block(body(ready))))
    };
    let helper = local("helper");
    let ready = local("ready");
    let mut block = Block(vec![
        helper_decl(&helper, body(&local("ready"))),
        site(local_value(&ready), &ready),
    ]);
    deinline(&mut block);
    assert_eq!(block.to_string().matches("helper()").count(), 1, "{block}");

    // The same body is rebuilt where the condition reads something else.
    let other = local("ready");
    let mut block = Block(vec![
        helper_decl(&helper, body(&local("ready"))),
        site(global("done"), &other),
    ]);
    deinline(&mut block);
    assert_eq!(block.to_string().matches("helper()").count(), 2, "{block}");
}

#[test]
fn a_rebuilt_value_keeps_the_locals_its_statement_still_uses() {
    // local helper = function() local temp = source("key"); return temp ~= nil end
    // local temp = source("key"); if temp ~= nil then print(temp) end
    let helper = local("helper");
    let pattern_temp = local("temp");
    let source = |temp: &RcLocal| {
        assign_local(temp, RValue::Call(global_call("source", vec![string("key")])), true)
    };
    let not_nil = |temp: &RcLocal| {
        RValue::Binary(Binary::new(local_value(temp), RValue::Literal(Literal::Nil), BinaryOperation::NotEqual))
    };
    let helper_body = vec![source(&pattern_temp), return_one(not_nil(&pattern_temp))];
    let temp = local("temp");
    let use_temp = |then_block: Vec<Statement>| {
        Statement::If(If::new(not_nil(&temp), Block(then_block), Block::default()))
    };
    let mut block = Block(vec![
        helper_decl(&helper, helper_body.clone()),
        source(&temp),
        use_temp(vec![Statement::Call(global_call("print", vec![local_value(&temp)]))]),
    ]);
    deinline(&mut block);
    assert_eq!(block.to_string().matches("helper()").count(), 1, "{block}");

    let mut block = Block(vec![
        helper_decl(&helper, helper_body),
        source(&temp),
        use_temp(vec![Statement::Call(global_call("print", vec![string("present")]))]),
    ]);
    deinline(&mut block);
    assert!(block.to_string().contains("if helper() then"), "{block}");
}

/// `local at = find(value); if at then return sub(value, at), at end;
/// return value, 1` stores into the caller's locals, `at` sharing the
/// second result's register; the rebuilt call declares both.
#[test]
fn a_tuple_returned_through_branches_rebuilds_into_its_results() {
    let (value, at) = (local("value"), local("at"));
    let ret = |values: Vec<RValue>| Statement::Return(Return::new(values));
    let find = |of: &RcLocal| RValue::Call(global_call("find", vec![local_value(of)]));
    let sub = |of: &RcLocal, at: &RcLocal| RValue::Call(global_call("sub", vec![local_value(of), local_value(at)]));
    let body = vec![
        assign_local(&at, find(&value), true),
        Statement::If(If::new(local_value(&at), Block(vec![ret(vec![sub(&value, &at), local_value(&at)])]), Block::default())),
        ret(vec![local_value(&value), number(1.0)]),
    ];
    let (lowered, results) = branch_tuple_return(&body, &[value.clone()]).expect("branch tuple");
    assert_eq!(results.len(), 2);
    assert_eq!(results[1], at, "the returned local is the second result");
    assert_eq!(
        Block(lowered).to_string().replace(&results[0].to_string(), "name"),
        "local at = find(value)\nlocal name\n\nif at then\n\tname = sub(value, at)\nelse\n\tname = value\n\tat = 1\nend"
    );

    // The site Luau inlines for `local name, position = split(text)`,
    // with `name` and `position` read afterwards.
    let helper = local("split");
    let (text, name, position) = (local("text"), local("name"), local("position"));
    let mut empty = Assign::new(vec![LValue::Local(name.clone())], Vec::new());
    empty.prefix = true;
    let mut block = Block(vec![
        helper_decl(&helper, body.clone()),
        assign_local(&position, find(&text), true),
        empty.into(),
        Statement::If(If::new(
            local_value(&position),
            Block(vec![assign_local(&name, sub(&text, &position), false)]),
            Block(vec![assign_local(&name, local_value(&text), false), assign_local(&position, number(1.0), false)]),
        )),
        Statement::Call(global_call("print", vec![local_value(&name), local_value(&position)])),
    ]);
    // The helper's parameter is `value`.
    if let Statement::Assign(declaration) = &block.0[0]
        && let RValue::Closure(closure) = &declaration.right[0]
    {
        closure.function.lock().parameters = vec![value.clone()];
    }
    deinline(&mut block);
    assert!(block.to_string().contains("local name, position = split(text)"), "{block}");

    // A later result reading the local keeps it apart from the results:
    // its store would overwrite what the later value reads.
    let later = vec![
        assign_local(&at, find(&value), true),
        Statement::If(If::new(
            local_value(&at),
            Block(vec![ret(vec![local_value(&at), RValue::Binary(Binary::new(local_value(&at), number(1.0), BinaryOperation::Add))])]),
            Block::default(),
        )),
        ret(vec![number(0.0), local_value(&value)]),
    ];
    let (_, results) = branch_tuple_return(&later, &[value.clone()]).expect("branch tuple");
    assert_ne!(results[0], at);

    // Every path must return the same number of values.
    let refused = |body: Vec<Statement>| branch_tuple_return(&body, &[value.clone()]).is_none();
    assert!(refused(vec![
        Statement::If(If::new(local_value(&at), Block(vec![ret(vec![number(1.0), number(2.0)])]), Block::default())),
        ret(vec![number(1.0)]),
    ]));
    // A path running off the end returns nothing.
    assert!(refused(vec![Statement::If(If::new(
        local_value(&at),
        Block(vec![ret(vec![number(1.0), number(2.0)])]),
        Block::default(),
    ))]));
    // The last value of a call spreads.
    assert!(refused(vec![
        Statement::If(If::new(local_value(&at), Block(vec![ret(vec![number(1.0), find(&value)])]), Block::default())),
        ret(vec![number(1.0), number(2.0)]),
    ]));
}

#[test]
fn a_helper_returning_its_own_locals_matches_its_body() {
    // `local a = 1; local b = {}; return a, b`
    let (a, b) = (local("a"), local("b"));
    let body = vec![
        assign_local(&a, number(1.0), true),
        assign_local(&b, RValue::Table(Table::default()), true),
        Statement::Return(Return::new(vec![local_value(&a), local_value(&b)])),
    ];
    let (rest, returned) = local_tuple_return(&body, &[]).expect("local tuple");
    assert_eq!(rest.len(), 2);
    assert_eq!(returned, vec![a.clone(), b.clone()]);

    let refused = |body: Vec<Statement>| local_tuple_return(&body, &[]).is_none();
    let ret = |values: Vec<RValue>| Statement::Return(Return::new(values));
    // One value is a value target; a non-local is not the caller's local.
    assert!(refused(vec![assign_local(&a, number(1.0), true), ret(vec![local_value(&a)])]));
    assert!(refused(vec![assign_local(&a, number(1.0), true), ret(vec![local_value(&a), number(2.0)])]));
    // A parameter, or the same local twice, is not one declaration each.
    assert!(refused(vec![assign_local(&a, number(1.0), true), ret(vec![local_value(&a), local_value(&b)])]));
    assert!(refused(vec![assign_local(&a, number(1.0), true), ret(vec![local_value(&a), local_value(&a)])]));
    // Another return means a call may not reach this one.
    let early = Statement::If(If::new(
        local_value(&local("c")),
        Block(vec![Statement::Return(Return::new(vec![]))]),
        Block::default(),
    ));
    assert!(refused(vec![
        early,
        assign_local(&a, number(1.0), true),
        assign_local(&b, number(2.0), true),
        ret(vec![local_value(&a), local_value(&b)]),
    ]));
    // A closure capturing `a` would share it with the caller's code.
    let capturing = RValue::Closure(Closure {
        node_origin: Default::default(),
        function: ByAddress(Arc::new(Mutex::new(Function::default()))),
        upvalues: vec![Upvalue::Ref(a.clone())],
    });
    assert!(refused(vec![
        assign_local(&a, number(1.0), true),
        assign_local(&b, capturing, true),
        ret(vec![local_value(&b), local_value(&a)]),
    ]));
}

#[test]
fn a_value_helper_that_falls_off_returns_nil_there() {
    // `if c then return "a" end` and `if c then return "a" else return end`
    let c = local("c");
    let falls = vec![Statement::If(If::new(local_value(&c), Block(vec![return_one(string("a"))]), Block::default()))];
    let void = vec![Statement::If(If::new(
        local_value(&c),
        Block(vec![return_one(string("a"))]),
        Block(vec![Statement::Return(Return::new(vec![]))]),
    ))];
    for body in [falls, void] {
        assert!(matches!(classify_returns(&body), Some((TKind::Value, true))));
        assert_eq!(
            Block(returning_nil(&body)).to_string(),
            "if c then\n\treturn \"a\"\nelse\n\treturn nil\nend"
        );
    }
}

#[test]
fn value_return_inside_loop_is_not_a_terminal_leaf() {
    let cond = local("cond");
    let pred = local("pred");
    let body = vec![
        Statement::While(While::new(
            local_value(&cond),
            Block(vec![
                Statement::If(If::new(
                    local_value(&pred),
                    Block(vec![return_one(string("a"))]),
                    Block::default(),
                )),
                Statement::Break(Break {}),
            ]),
        )),
        return_one(string("b")),
    ];

    let pat = canon(&body);
    assert!(!value_leaf_shape(&pat));
    // Matched only where the copy leaves the loop through a flag
    // (`match_value_loop`), never as a plain value region.
    assert!(matches!(classify_returns(&body), Some((TKind::Value, false))));
    assert_eq!(loop_return_split(&pat), Some(0));

    // A `return` two loops deep leaves through two flags: refused.
    let nested = vec![
        Statement::While(While::new(local_value(&cond), Block(vec![body[0].clone()]))),
        return_one(string("b")),
    ];
    assert!(loop_return_split(&canon(&nested)).is_none());
    assert!(classify_returns(&nested).is_none());
}

#[test]
fn value_guard_return_canonicalizes_to_terminal_leaves() {
    let pred = local("pred");
    let body = vec![
        Statement::If(If::new(
            local_value(&pred),
            Block(vec![return_one(string("a"))]),
            Block::default(),
        )),
        return_one(string("b")),
    ];

    let pat = canon(&body);
    assert!(value_leaf_shape(&pat));
    assert!(matches!(classify_returns(&body), Some((TKind::Value, false))));
}

#[test]
fn void_guard_with_prefix_canonicalizes_all_early_returns() {
    let body = vec![
        Statement::If(If::new(
            global("disabled"),
            Block(vec![Statement::Return(Return::default())]),
            Block::default(),
        )),
        print_x(),
        Statement::If(If::new(
            global("created"),
            Block(vec![
                Statement::Call(Call::new(global("markCreated"), vec![])),
                Statement::Return(Return::default()),
            ]),
            Block::default(),
        )),
        Statement::If(If::new(
            global("notDestroyed"),
            Block(vec![
                Statement::If(If::new(
                    global("disconnect"),
                    Block(vec![Statement::Call(Call::new(
                        global("markDisconnect"),
                        vec![],
                    ))]),
                    Block::default(),
                )),
                Statement::Return(Return::default()),
            ]),
            Block::default(),
        )),
        Statement::Call(Call::new(global("markDestroyed"), vec![])),
    ];

    let canonical = canon(&body);
    assert!(
        !block_has_return(&canonical),
        "all void early returns should become structured branches:\n{}",
        Block(canonical)
    );
}

#[test]
fn constant_specialized_void_site_refolds_after_verified_partial_evaluation() {
    let debug = local("debug");
    let event = local("event");
    let key = local("key");
    let raw = vec![
        Statement::If(If::new(
            RValue::Unary(Unary::new(local_value(&debug), UnaryOperation::Not)),
            Block(vec![Statement::Return(Return::default())]),
            Block::default(),
        )),
        Statement::Call(Call::new(
            global("emit"),
            vec![local_value(&event), local_value(&key)],
        )),
        Statement::If(If::new(
            RValue::Binary(Binary::new(
                local_value(&event),
                string("CREATED"),
                BinaryOperation::Equal,
            )),
            Block(vec![
                Statement::Call(Call::new(global("markCreated"), vec![local_value(&key)])),
                Statement::Return(Return::default()),
            ]),
            Block::default(),
        )),
        Statement::Call(Call::new(global("markOther"), vec![local_value(&key)])),
    ];
    let pat = canon(&raw);
    let mut params = FxHashSet::default();
    params.insert(event.clone());
    params.insert(key.clone());
    let target = Target {
        f_local: local("logEvent"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Void,
        pat_raw_len: raw.len(),
        pat_spine_len: raw.len(),
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtResultDecl,
        prefix_len: 0,
        pat0_kind: std::mem::discriminant(&pat[0]),
        pat0_anchor_key: stmt_anchor_key(&pat[0]),
        pat,
        params,
        locals: FxHashSet::default(),
        param_order: vec![event, key],
        written_params: Vec::new(),
        unread: FxHashSet::default(),
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: true,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: false,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    };

    let caller_key = local("callerKey");
    let candidate = canon(&[Statement::If(If::new(
        local_value(&debug),
        Block(vec![
            Statement::Call(Call::new(
                global("emit"),
                vec![string("OTHER"), local_value(&caller_key)],
            )),
            Statement::Call(Call::new(
                global("markOther"),
                vec![local_value(&caller_key)],
            )),
        ]),
        Block::default(),
    ))]);

    let unified = try_unify_site_any(&target, &candidate, &[], None)
        .expect("literal-specialized branch must refold only after exact verification");
    assert_eq!(unified.args.len(), 2);
    assert!(rvalue_exact_eq(&unified.args[0], &string("OTHER")));
    assert!(rvalue_exact_eq(&unified.args[1], &local_value(&caller_key)));

    let mut wrong = candidate.clone();
    let Statement::If(node) = &mut wrong[0] else {
        panic!()
    };
    node.then_block.lock().0[1] = Statement::Call(Call::new(
        global("differentEffect"),
        vec![local_value(&caller_key)],
    ));
    assert!(
        try_unify_site_any(&target, &wrong, &[], None).is_none(),
        "a non-specialization body difference must remain refused"
    );
}

#[test]
fn specialized_site_refuses_repeated_table_identity() {
    let flag = local("flag");
    let value = local("value");
    let raw = vec![Statement::Call(Call::new(
        global("consume"),
        vec![local_value(&flag), local_value(&value), local_value(&value)],
    ))];
    let pat = canon(&raw);
    let mut params = FxHashSet::default();
    params.insert(flag.clone());
    params.insert(value.clone());
    let target = Target {
        f_local: local("consumeTwice"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Void,
        pat_raw_len: raw.len(),
        pat_spine_len: raw.len(),
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtResultDecl,
        prefix_len: 0,
        pat0_kind: std::mem::discriminant(&pat[0]),
        pat0_anchor_key: stmt_anchor_key(&pat[0]),
        pat,
        params,
        locals: FxHashSet::default(),
        param_order: vec![flag, value],
        written_params: Vec::new(),
        unread: FxHashSet::default(),
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: true,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: false,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    };
    let candidate = canon(&[Statement::Call(Call::new(
        global("consume"),
        vec![
            string("enabled"),
            RValue::Table(Table::default()),
            RValue::Table(Table::default()),
        ],
    ))]);

    assert!(
        try_unify_specialized_site(&target, &candidate, &[], None).is_none(),
        "two fresh tables must never collapse into one reconstructed argument"
    );
}

#[test]
fn vector_equality_is_not_partially_evaluated() {
    let vector = Literal::Vector(1.0, 2.0, 3.0);
    assert_eq!(runtime_literal_equal(&vector, &vector), None);

    let mut expression = RValue::Binary(Binary::new(
        RValue::Literal(vector.clone()),
        RValue::Literal(vector),
        BinaryOperation::Equal,
    ));
    specialize_rvalue(&mut expression, &FxHashMap::default());
    assert!(matches!(expression, RValue::Binary(_)));
}

#[test]
fn statement_deinline_refuses_metamethod_risk_argument() {
    let parameter = local("parameter");
    let raw = vec![
        print_x(),
        Statement::Call(Call::new(global("consume"), vec![local_value(&parameter)])),
    ];
    let pat = canon(&raw);
    let mut params = FxHashSet::default();
    params.insert(parameter.clone());
    let target = Target {
        f_local: local("effectThenConsume"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Void,
        pat_raw_len: raw.len(),
        pat_spine_len: raw.len(),
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtResultDecl,
        prefix_len: 0,
        pat0_kind: std::mem::discriminant(&pat[0]),
        pat0_anchor_key: stmt_anchor_key(&pat[0]),
        pat,
        params,
        locals: FxHashSet::default(),
        param_order: vec![parameter],
        written_params: Vec::new(),
        unread: FxHashSet::default(),
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: false,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: false,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    };
    let left = local("left");
    let right = local("right");
    let candidate = canon(&[
        print_x(),
        Statement::Call(Call::new(
            global("consume"),
            vec![RValue::Binary(Binary::new(
                local_value(&left),
                local_value(&right),
                BinaryOperation::Add,
            ))],
        )),
    ]);

    assert!(
        try_unify_site(&target, &candidate, &[], None).is_none(),
        "moving a potentially metamethod-backed operator before print is unsound"
    );
}

#[test]
fn loop_return_cps_site_requires_exact_caller_continuation() {
    let frames = local("frames");
    let frame = local("frame");
    let existing = local("existing");
    let loop_guard = Statement::If(If::new(
        RValue::Binary(Binary::new(
            local_value(&existing),
            local_value(&frame),
            BinaryOperation::Equal,
        )),
        Block(vec![Statement::Return(Return::default())]),
        Block::default(),
    ));
    let raw = vec![Statement::If(If::new(
        local_value(&frame),
        Block(vec![
            Statement::GenericFor(GenericFor::new(
                vec![existing.clone()],
                vec![local_value(&frames)],
                Block(vec![loop_guard]),
            )),
            Statement::Call(Call::new(
                global("insertFrame"),
                vec![local_value(&frames), local_value(&frame)],
            )),
        ]),
        Block::default(),
    ))];
    let pat = canon(&raw);
    let mut params = FxHashSet::default();
    params.insert(frame.clone());
    let mut locals = FxHashSet::default();
    collect_declared_locals(&pat, &mut locals);
    let target = Target {
        f_local: local("addFrame"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Void,
        pat_raw_len: raw.len(),
        pat_spine_len: raw.len(),
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtResultDecl,
        prefix_len: 0,
        pat0_kind: std::mem::discriminant(&pat[0]),
        pat0_anchor_key: stmt_anchor_key(&pat[0]),
        pat,
        params,
        locals,
        param_order: vec![frame],
        written_params: Vec::new(),
        unread: FxHashSet::default(),
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: false,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: true,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    };

    let actual = local("actual");
    let caller_existing = local("callerExisting");
    let continuation = vec![
        Statement::Call(Call::new(global("afterHelper"), vec![local_value(&actual)])),
        Statement::Return(Return::default()),
    ];
    let mut loop_body = vec![Statement::If(If::new(
        RValue::Binary(Binary::new(
            local_value(&caller_existing),
            local_value(&actual),
            BinaryOperation::NotEqual,
        )),
        Block(vec![Statement::Continue(crate::Continue {})]),
        Block::default(),
    ))];
    loop_body.extend(continuation.clone());
    let mut normal_path = vec![
        Statement::GenericFor(GenericFor::new(
            vec![caller_existing.clone()],
            vec![local_value(&frames)],
            Block(loop_body),
        )),
        Statement::Call(Call::new(
            global("insertFrame"),
            vec![local_value(&frames), local_value(&actual)],
        )),
    ];
    // Luau clones K after both the loop-return edge and the helper's normal
    // fallthrough.  The enclosing empty arm reaches the external K directly.
    normal_path.extend(continuation.clone());
    let window = vec![Statement::If(If::new(local_value(&actual), Block(normal_path), Block::default()))];
    let candidate = canon(&window);

    let unified = try_unify_cps_site(&target, &window, &candidate, &continuation, &[], None)
        .expect("verified cloned continuation should recover the loop-return helper");
    assert!(rvalue_exact_eq(&unified.args[0], &local_value(&actual)));

    let structured_loop_body = vec![Statement::If(If::new(
        RValue::Binary(Binary::new(
            local_value(&caller_existing),
            local_value(&actual),
            BinaryOperation::Equal,
        )),
        Block(continuation.clone()),
        Block::default(),
    ))];
    let mut structured_normal_path = vec![
        Statement::GenericFor(GenericFor::new(
            vec![caller_existing],
            vec![local_value(&frames)],
            Block(structured_loop_body),
        )),
        Statement::Call(Call::new(
            global("insertFrame"),
            vec![local_value(&frames), local_value(&actual)],
        )),
    ];
    structured_normal_path.extend(continuation.clone());
    let structured_window =
        vec![Statement::If(If::new(local_value(&actual), Block(structured_normal_path), Block::default()))];
    let structured_candidate = canon(&structured_window);
    let structured = try_unify_cps_site(&target, &structured_window, &structured_candidate, &continuation, &[], None)
        .expect("pre-guard-continue structured loop exit should also refold");
    assert!(rvalue_exact_eq(&structured.args[0], &local_value(&actual)));

    let wrong_continuation = vec![
        Statement::Call(Call::new(
            global("differentAfter"),
            vec![local_value(&actual)],
        )),
        Statement::Return(Return::default()),
    ];
    assert!(
        try_unify_cps_site(&target, &window, &candidate, &wrong_continuation, &[], None).is_none(),
        "a different caller continuation must refuse CPS refolding"
    );
}

#[test]
fn cps_continuation_is_only_accepted_at_true_tail_positions() {
    let target = void_target(vec![print_x()], FxHashSet::default());
    let continuation = vec![
        Statement::Call(Call::new(global("after"), Vec::new())),
        Statement::Return(Return::default()),
    ];
    let pattern = vec![
        Statement::If(If::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![Statement::Call(Call::new(
                global("inside"),
                Vec::new(),
            ))]),
            Block::default(),
        )),
        Statement::Call(Call::new(global("laterInCallee"), Vec::new())),
    ];
    let mut nested = vec![Statement::Call(Call::new(global("inside"), Vec::new()))];
    nested.extend(continuation.clone());
    let candidate = vec![
        Statement::If(If::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(nested),
            Block::default(),
        )),
        Statement::Call(Call::new(global("laterInCallee"), Vec::new())),
    ];

    assert!(!cps_unify_block(
        &target,
        &pattern,
        &candidate,
        &continuation,
        true,
        &mut Bindings::default(),
    ));
}

#[test]
fn cps_exact_loop_return_cannot_skip_caller_continuation() {
    let target = void_target(vec![print_x()], FxHashSet::default());
    let continuation = vec![Statement::Return(Return::default())];
    let pattern = vec![Statement::While(While::new(
        RValue::Literal(Literal::Boolean(true)),
        Block(vec![Statement::Return(Return::default())]),
    ))];
    let candidate = canon(&pattern);

    assert!(!cps_unify_block(
        &target,
        &pattern,
        &candidate,
        &continuation,
        true,
        &mut Bindings::default(),
    ));
}

#[test]
fn cps_exact_repeat_return_cannot_skip_caller_continuation() {
    let target = void_target(vec![print_x()], FxHashSet::default());
    let continuation = vec![Statement::Return(Return::default())];
    let pattern = vec![Statement::Repeat(Repeat::new(
        RValue::Literal(Literal::Boolean(false)),
        Block(vec![Statement::Return(Return::default())]),
    ))];
    let candidate = canon(&pattern);

    assert!(!cps_unify_block(
        &target,
        &pattern,
        &candidate,
        &continuation,
        true,
        &mut Bindings::default(),
    ));
}

#[test]
fn cps_continuation_requires_return_not_loop_control() {
    assert!(!sequence_has_return_tail(&[Statement::Break(Break {})]));
    assert!(!sequence_has_return_tail(&[Statement::Continue(
        crate::Continue {},
    )]));
    assert!(sequence_has_return_tail(&[Statement::Return(
        Return::default(),
    )]));

    let mixed = vec![
        Statement::If(If::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![Statement::Break(Break {})]),
            Block::default(),
        )),
        Statement::Return(Return::default()),
    ];
    assert!(sequence_has_return_tail(&mixed));
    assert!(has_depth_zero_loop_control(&mixed, 0));
}

/// P7-A: a call/method-call leaf (`return g(x)`) is an admissible Value leaf —
/// the single-LHS `RESULT = g(x)` inlined site truncates it to one value, so
/// the candidate shape proves the arity. (Refused pre-P7-A.)
#[test]
fn call_leaf_is_an_admissible_value_leaf_p7a() {
    let c = local("c");
    let body = vec![Statement::If(If::new(
        local_value(&c),
        Block(vec![return_one(RValue::Call(Call::new(
            global("g"),
            vec![local_value(&c)],
        )))]),
        Block(vec![return_one(RValue::Literal(Literal::Nil))]),
    ))];
    let pat = canon(&body);
    assert!(value_leaf_shape(&pat), "call leaf must be admissible");
    assert!(matches!(classify_returns(&body), Some((TKind::Value, false))));
}

/// P7-A boundary: a bare `...` (vararg) leaf is STILL refused — its multi-value
/// spread has no provable single-value truncation point. Likewise a 2-value
/// `return a, b` stays refused (returns_bad's multi-value arm).
#[test]
fn vararg_and_multivalue_leaves_still_refused_p7a() {
    let vararg_body = vec![Statement::Return(Return::new(vec![RValue::VarArg(
        crate::VarArg,
    )]))];
    assert!(
        classify_returns(&vararg_body).is_none(),
        "a bare vararg return must stay refused"
    );

    let multi_body = vec![Statement::Return(Return::new(vec![
        string("a"),
        string("b"),
    ]))];
    assert!(
        classify_returns(&multi_body).is_none(),
        "a 2-value return must stay refused"
    );
}

// === Soundness-boundary tripwires (lock in the SKIP decisions; guard the
//     P6/P7 widenings from ever matching an unsound shape) ===

/// P3: the `anchors_in_block < 2` readability gate keeps a trivial body
/// (`return x + 1`, 0 anchors) out — de-inlining it to `f(x)` would be LESS
/// readable than the inlined form. Lowering this gate is the report's
/// largest-recall idea but is refused on readability grounds.
#[test]
fn anchor_gate_refuses_trivial_body_p3() {
    let x = local("x");
    let trivial = canon(&[return_one(add_one(&x))]); // `return x + 1`
    assert!(
        anchors_in_block(&trivial) < 2,
        "a trivial add-one helper must stay below the anchor floor"
    );
}

/// P12: `unify_local` injectivity must refuse mapping TWO distinct callee
/// locals onto ONE caller local — coalescing two simultaneously-live locals
/// into one would assert shared storage the original did not have.
#[test]
fn injectivity_two_locals_one_caller_refused_p12() {
    let a = local("a");
    let b = local("b");
    let mut locals = FxHashSet::default();
    locals.insert(a.clone());
    locals.insert(b.clone());
    let pat = vec![
        Statement::Call(Call::new(global("print"), vec![local_value(&a)])),
        Statement::Call(Call::new(global("print"), vec![local_value(&b)])),
    ];
    let t = void_target(pat, locals);

    let c = local("c");
    let cand = vec![
        Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
        Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
    ];
    assert!(
        try_unify_site(&t, &cand, &[], None).is_none(),
        "two callee locals mapping to one caller local must be refused"
    );
}

/// P11-A: a window covering a function's ENTIRE top-level body is refused (the
/// thin-wrapper / mutual-clone hazard — a whole-body structural match is the
/// least-evidential match for the -O2 marker). The same window matches fine
/// when it is NOT the whole body.
#[test]
fn whole_body_wrapper_refused_p11a() {
    let pat = vec![print_x(), Statement::Call(Call::new(global("foo"), vec![]))];
    let t = void_target(pat, FxHashSet::default());
    let cand = vec![print_x(), Statement::Call(Call::new(global("foo"), vec![]))];
    let mut canon_cache = CanonCache::default();

    // is_func_body_top = true AND the window is the whole body -> refused.
    assert!(
        match_void(&cand, 0, &t, false, true, &[], &mut None, &mut canon_cache, None).is_none(),
        "replacing a function's entire body with one call must be refused"
    );
    // Not the whole body (is_func_body_top = false) -> matches.
    assert!(
        match_void(&cand, 0, &t, false, false, &[], &mut None, &mut canon_cache, None).is_some(),
        "the same region matches when it is not the whole body"
    );
}

/// P8: a mutable-parameter accumulator (`p = math.max(p, 0)`, p used as LHS)
/// must NOT de-inline. Register coalescing makes it `arg = math.max(arg, 0)` on
/// a caller-visible local in place — `f(arg)` would be wrong (the call does not
/// write arg). `unify_local`'s param-identity requirement refuses it, which the
/// P6 prefix widening must not loosen.
#[test]
fn mutable_param_accumulator_refused_p8() {
    let p = local("p");
    let math_max = |v: RValue| {
        RValue::Call(Call::new(
            RValue::Index(Index::new(global("math"), string("max"))),
            vec![v, number(0.0)],
        ))
    };
    // helper body: `p = math.max(p, 0) ; return p` (p is a PARAMETER).
    let pat = canon(&[
        assign_local(&p, math_max(local_value(&p)), false),
        return_one(local_value(&p)),
    ]);
    let pat0_kind = std::mem::discriminant(&pat[0]);
    let pat0_anchor_key = stmt_anchor_key(&pat[0]);
    let mut params = FxHashSet::default();
    params.insert(p.clone());
    let t = Target {
        f_local: local("f"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Value,
        pat_raw_len: 2,
        pat_spine_len: 2,
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtPrefix,
        prefix_len: 1,
        pat0_kind,
        pat0_anchor_key,
        pat,
        params,
        locals: FxHashSet::default(),
        param_order: vec![p.clone()],
        written_params: Vec::new(),
        unread: FxHashSet::default(),
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: false,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: false,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    };

    let arg = local("arg");
    let v = local("v");
    let cand = vec![
        assign_local(&arg, math_max(local_value(&arg)), false),
        init_less_decl(&v),
        assign_local(&v, local_value(&arg), false),
        print_x(),
    ];
    assert!(
        match_value_prefixed(&cand, 0, &t, None, false, &mut None).is_none(),
        "an in-place accumulator with a param-LHS must not de-inline"
    );
}

/// F2: a candidate region carrying TWO interposed `CALL_MARKER`s (two inner
/// de-inlines in a chained reconstruction) must still match. The old raw ceiling
/// `pat_raw_len + 1` capped the window at 4 raw statements — one short of the 5
/// needed (3 calls + 2 markers) — silently missing the outer reconstruction; the
/// effective-count ceiling (trivia don't consume the budget) reaches it.
#[test]
fn void_region_with_two_interposed_markers_matches_f2() {
    let mk = || Statement::Comment(Comment::trailing(CALL_MARKER.to_string()));
    let call = |s: &str| Statement::Call(Call::new(global("print"), vec![string(s)]));
    let pat = vec![call("a"), call("b"), call("c")];
    let t = void_target(pat, FxHashSet::default());
    let cand = vec![
        call("a"),
        mk(),
        call("b"),
        mk(),
        call("c"),
        print_x(), // trailing real stmt: the window must stop before it (canon != kc)
    ];
    let mut canon_cache = CanonCache::default();
    let hit = match_void(&cand, 0, &t, false, false, &[], &mut None, &mut canon_cache, None)
        .expect("two interposed markers must not exceed the effective window ceiling");
    assert_eq!(
        hit.consume, 5,
        "window spans the 3 calls + 2 interior markers"
    );
}

/// Build a Void target with the given param order and a set of NEVER-read params
/// (F6a). `print(read_param)` twice is the body; the read param binds, the unread
/// ones are supplied as `nil` (trailing ones trimmed) by `try_unify_site`.
fn unused_param_void_target(
    param_order: Vec<RcLocal>,
    read: &RcLocal,
    unread: &[RcLocal],
) -> Target {
    let pat = vec![
        Statement::Call(Call::new(global("print"), vec![local_value(read)])),
        Statement::Call(Call::new(global("print"), vec![local_value(read)])),
    ];
    let pat0_kind = std::mem::discriminant(&pat[0]);
    let pat0_anchor_key = stmt_anchor_key(&pat[0]);
    let params: FxHashSet<RcLocal> = param_order.iter().cloned().collect();
    let unread_set: FxHashSet<RcLocal> = unread.iter().cloned().collect();
    Target {
        f_local: local("f"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Void,
        // These F6a tests call `try_unify_site` directly (which never reads
        // `pat_raw_len`); canon len == raw len == 2 here, so the nominal value is
        // fine. A window-scan (match_void) test would need the RAW body length.
        pat_raw_len: pat.len(),
        pat_spine_len: tail_spine_len(&pat),
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtResultDecl,
        prefix_len: 0,
        pat0_kind,
        pat0_anchor_key,
        pat,
        params,
        locals: FxHashSet::default(),
        param_order,
        written_params: Vec::new(),
        unread: unread_set,
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: false,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: false,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    }
}

/// F6a: a TRAILING never-read parameter no longer blocks de-inline; the dropped
/// arg is trimmed (`f(a, unused)` called `f(x, 999)` reconstructs as `f(x)`).
#[test]
fn unused_trailing_param_de_inlines_with_trimmed_arg_f6a() {
    let a = local("a");
    let unused = local("u");
    let t = unused_param_void_target(vec![a.clone(), unused.clone()], &a, &[unused.clone()]);
    let c = local("c");
    let cand = vec![
        Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
        Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
    ];
    let u = try_unify_site(&t, &cand, &[], None).expect("unused trailing param must not block de-inline");
    assert_eq!(
        u.args.len(),
        1,
        "trailing nil for the unused param is trimmed"
    );
    assert!(matches!(&u.args[0], RValue::Local(l) if l == &c));
}

/// F6a: an INTERIOR never-read parameter is preserved as `nil` to keep positions
/// (`f(unused, b)` called `f(999, x)` reconstructs as `f(nil, x)`).
#[test]
fn unused_interior_param_de_inlines_with_nil_f6a() {
    let unused = local("u");
    let b = local("b");
    let t = unused_param_void_target(vec![unused.clone(), b.clone()], &b, &[unused.clone()]);
    let c = local("c");
    let cand = vec![
        Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
        Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
    ];
    let u = try_unify_site(&t, &cand, &[], None).expect("interior unused param must not block de-inline");
    assert_eq!(u.args.len(), 2);
    assert!(
        matches!(&u.args[0], RValue::Literal(Literal::Nil)),
        "interior unused param -> nil placeholder"
    );
    assert!(matches!(&u.args[1], RValue::Local(l) if l == &c));
}

/// F6a soundness boundary: a param that the body READS but that fails to bind
/// (genuine mismatch) still refuses the whole site — only NEVER-read params get
/// the nil treatment.
#[test]
fn read_param_that_fails_to_bind_still_refused_f6a() {
    let a = local("a");
    // `a` IS read by the body but is NOT in `unread`.
    let t = unused_param_void_target(vec![a.clone()], &a, &[]);
    // candidate whose second print reads a DIFFERENT local than the first ->
    // `a` binds to the first, the second occurrence mismatches -> refuse.
    let c = local("c");
    let d = local("d");
    let cand = vec![
        Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
        Statement::Call(Call::new(global("print"), vec![local_value(&d)])),
    ];
    assert!(
        try_unify_site(&t, &cand, &[], None).is_none(),
        "a read param with inconsistent bindings must refuse, never default to nil"
    );
}

// === §8: call-site value de-inline with an interposed RESULT decl ===

fn boolean(b: bool) -> RValue {
    RValue::Literal(Literal::Boolean(b))
}

fn field(obj: RValue, name: &str) -> RValue {
    RValue::Index(Index::new(obj, string(name)))
}

fn bin(left: RValue, op: BinaryOperation, right: RValue) -> RValue {
    RValue::Binary(Binary::new(left, right, op))
}

fn call1(callee: RValue, arg: RValue) -> RValue {
    RValue::Call(Call::new(callee, vec![arg]))
}

fn not_rv(v: RValue) -> RValue {
    RValue::Unary(Unary {
        node_origin: Default::default(),
        value: Box::new(v),
        operation: UnaryOperation::Not,
    })
}

fn if_stmt(cond: RValue, then_b: Vec<Statement>, else_b: Vec<Statement>) -> Statement {
    Statement::If(If::new(cond, Block(then_b), Block(else_b)))
}

/// init-less `local l` (a RESULT-register declaration).
fn init_less_decl(l: &RcLocal) -> Statement {
    Statement::Assign(Assign {
        node_origin: Default::default(),
        left: vec![LValue::Local(l.clone())],
        right: vec![],
        prefix: true,
        parallel: false, compound: false,
    })
}

fn void_return() -> Statement {
    Statement::Return(Return::default())
}

/// Build an `AtPrefix` Value target directly from a callee body whose canon is
/// `[<one prefix Assign>, <value branch>]` (mirrors `collect_targets`).
fn value_prefix_target(body: &[Statement]) -> Target {
    let pat = canon(body);
    assert_eq!(pat.len(), 2, "test body must canon to [prefix, branch]");
    assert!(
        matches!(pat[0], Statement::Assign(_)),
        "prefix must be an Assign"
    );
    let mut locals = FxHashSet::default();
    collect_declared_locals(&pat, &mut locals);
    let pat0_kind = std::mem::discriminant(&pat[0]);
    let pat0_anchor_key = stmt_anchor_key(&pat[0]);
    Target {
        f_local: local("f"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Value,
        pat_raw_len: body.len(),
        pat_spine_len: body.len(),
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtPrefix,
        prefix_len: 1,
        pat0_kind,
        pat0_anchor_key,
        pat,
        params: FxHashSet::default(),
        locals,
        param_order: Vec::new(),
        written_params: Vec::new(),
        unread: FxHashSet::default(),
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: false,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: false,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    }
}

/// P6: build an `AtPrefix` Value target with K>=1 leading non-branch prefix
/// statements (`prefix_len == pat.len() - 1`), mirroring `collect_targets`.
fn value_prefix_target_k(body: &[Statement]) -> Target {
    let pat = canon(body);
    let k = pat.len() - 1;
    assert!(k >= 1, "need at least one prefix statement");
    assert!(
        pat[..k].iter().all(|s| matches!(
            s,
            Statement::Assign(_) | Statement::Call(_) | Statement::MethodCall(_)
        )),
        "prefix statements must be non-branch"
    );
    let mut locals = FxHashSet::default();
    collect_declared_locals(&pat, &mut locals);
    let pat0_kind = std::mem::discriminant(&pat[0]);
    let pat0_anchor_key = stmt_anchor_key(&pat[0]);
    Target {
        f_local: local("f"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Value,
        pat_raw_len: body.len(),
        pat_spine_len: body.len(),
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtPrefix,
        prefix_len: k,
        pat0_kind,
        pat0_anchor_key,
        pat,
        params: FxHashSet::default(),
        locals,
        param_order: Vec::new(),
        written_params: Vec::new(),
        unread: FxHashSet::default(),
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: false,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: false,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    }
}

/// The flagship AfkClient `isAfkEnabled` case: a guard-leading value callee with
/// a callee-prefix local before the interposed RESULT decl. Exercises BOTH §8
/// changes — the prefix-aware window AND the guard-polarity flip (the inline
/// copy's `if Enabled == false then v2=false …` is the NEGATED+SWAPPED mirror of
/// the canon'd pattern's `if Enabled ~= false then … else return false`).
#[test]
fn afk_value_prefix_guard_flip_matches() {
    let afk = local("afkConfig"); // external/upvalue — same RcLocal both sides
    let place_id = local("placeId");
    let body = vec![
        assign_local(
            &place_id,
            call1(global("tonumber"), field(local_value(&afk), "PlaceId")),
            true,
        ),
        if_stmt(
            bin(
                field(local_value(&afk), "Enabled"),
                BinaryOperation::Equal,
                boolean(false),
            ),
            vec![return_one(boolean(false))],
            vec![],
        ),
        if_stmt(
            bin(
                local_value(&place_id),
                BinaryOperation::And,
                bin(
                    local_value(&place_id),
                    BinaryOperation::GreaterThan,
                    number(0.0),
                ),
            ),
            vec![return_one(bin(
                field(global("game"), "PlaceId"),
                BinaryOperation::Equal,
                local_value(&place_id),
            ))],
            vec![return_one(bin(
                field(global("game"), "PlaceId"),
                BinaryOperation::Equal,
                number(0.0),
            ))],
        ),
    ];
    let t = value_prefix_target(&body);

    let v = local("v");
    let v2 = local("v2");
    let candidate = vec![
        assign_local(
            &v,
            call1(global("tonumber"), field(local_value(&afk), "PlaceId")),
            true,
        ),
        init_less_decl(&v2),
        if_stmt(
            bin(
                field(local_value(&afk), "Enabled"),
                BinaryOperation::Equal,
                boolean(false),
            ),
            vec![assign_local(&v2, boolean(false), false)],
            vec![if_stmt(
                bin(
                    local_value(&v),
                    BinaryOperation::And,
                    bin(local_value(&v), BinaryOperation::GreaterThan, number(0.0)),
                ),
                vec![assign_local(
                    &v2,
                    bin(
                        field(global("game"), "PlaceId"),
                        BinaryOperation::Equal,
                        local_value(&v),
                    ),
                    false,
                )],
                vec![assign_local(
                    &v2,
                    bin(
                        field(global("game"), "PlaceId"),
                        BinaryOperation::Equal,
                        number(0.0),
                    ),
                    false,
                )],
            )],
        ),
        if_stmt(not_rv(local_value(&v2)), vec![void_return()], vec![]),
    ];

    let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
        .expect("isAfkEnabled prefix + guard-polarity flip should match");
    assert_eq!(hit.consume, 3, "consume prefix + decl + value branch");
    assert_eq!(hit.results, vec![v2]);
    assert!(hit.args.is_empty(), "isAfkEnabled has no parameters");
}

/// An if/else value callee (non-empty else, NOT a guard) keeps the SAME polarity
/// on both sides, so the prefix fix alone suffices and the flip is a no-op.
#[test]
fn value_prefix_if_else_matches_without_flip() {
    let obj = local("obj");
    let k = local("k");
    let body = vec![
        assign_local(&k, field(local_value(&obj), "Field"), true),
        if_stmt(
            bin(local_value(&k), BinaryOperation::Equal, number(1.0)),
            vec![return_one(string("a"))],
            vec![return_one(string("b"))],
        ),
    ];
    let t = value_prefix_target(&body);

    let k2 = local("k2");
    let v = local("v");
    let candidate = vec![
        assign_local(&k2, field(local_value(&obj), "Field"), true),
        init_less_decl(&v),
        if_stmt(
            bin(local_value(&k2), BinaryOperation::Equal, number(1.0)),
            vec![assign_local(&v, string("a"), false)],
            vec![assign_local(&v, string("b"), false)],
        ),
        print_x(), // trailing stmt: window isn't whole-body; doesn't read k2
    ];

    let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
        .expect("if/else value prefix should match without a flip");
    assert_eq!(hit.consume, 3);
    assert_eq!(hit.results, vec![v]);
}

/// F10a hardening: a value-return result-write leaf carrying `prefix = true` (a
/// `local v = X` redeclaration rather than the plain `v = X` reassignment the
/// single-declaration invariant guarantees) must NOT unify as the result lane —
/// splicing it would change `v`'s scope. The (Return, Assign) arm now requires
/// `!prefix && !parallel`, mirroring `result_decl` and the (Assign, Assign) arm.
#[test]
fn value_leaf_with_prefix_redeclaration_refused_f10a() {
    let obj = local("obj");
    let k = local("k");
    let body = vec![
        assign_local(&k, field(local_value(&obj), "Field"), true),
        if_stmt(
            bin(local_value(&k), BinaryOperation::Equal, number(1.0)),
            vec![return_one(string("a"))],
            vec![return_one(string("b"))],
        ),
    ];
    let t = value_prefix_target(&body);

    let k2 = local("k2");
    let v = local("v");
    let candidate = vec![
        assign_local(&k2, field(local_value(&obj), "Field"), true),
        init_less_decl(&v),
        if_stmt(
            bin(local_value(&k2), BinaryOperation::Equal, number(1.0)),
            // result-write leaves as `local v = …` (prefix=true) -> refused.
            vec![assign_local(&v, string("a"), true)],
            vec![assign_local(&v, string("b"), true)],
        ),
        print_x(),
    ];

    assert!(
        match_value_prefixed(&candidate, 0, &t, None, false, &mut None).is_none(),
        "a prefix=true result-write leaf must not unify as the result lane (F10a)"
    );
}

/// P1 regression: a `CALL_MARKER` an inner de-inline spliced between the
/// callee-prefix statement and the interposed `local RESULT` decl must NOT
/// break the AtPrefix match. The old `d = i + p` offset pointed at the marker
/// (`result_decl` -> None -> bail), silently killing chained reconstruction;
/// `nth_effective_index` skips the marker and still finds the decl, and the
/// `consume` span removes the marker along with the window.
#[test]
fn value_prefix_marker_between_prefix_and_result_decl_still_matches() {
    let obj = local("obj");
    let k = local("k");
    let body = vec![
        assign_local(&k, field(local_value(&obj), "Field"), true),
        if_stmt(
            bin(local_value(&k), BinaryOperation::Equal, number(1.0)),
            vec![return_one(string("a"))],
            vec![return_one(string("b"))],
        ),
    ];
    let t = value_prefix_target(&body);

    let k2 = local("k2");
    let v = local("v");
    let marker = Statement::Comment(Comment::trailing(CALL_MARKER.to_string()));
    let candidate = vec![
        assign_local(&k2, field(local_value(&obj), "Field"), true),
        marker, // interposed by an inner de-inline of the prefix
        init_less_decl(&v),
        if_stmt(
            bin(local_value(&k2), BinaryOperation::Equal, number(1.0)),
            vec![assign_local(&v, string("a"), false)],
            vec![assign_local(&v, string("b"), false)],
        ),
        print_x(), // trailing stmt: window isn't whole-body; doesn't read k2
    ];

    let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
        .expect("interposed marker must not break the AtPrefix match");
    // span = prefix(0) + marker(1) + decl(2) + region-if(3): removes 4 stmts,
    // leaving the trailing print.
    assert_eq!(hit.consume, 4);
    assert_eq!(hit.results, vec![v]);
}

/// P6: a Value helper with TWO leading non-branch prefix statements (K==2)
/// inlines as `<prefix1> ; <prefix2> ; local RESULT ; <value branch>`. The
/// generalised `prefix_len = pat.len()-1` + logical-index RESULT lookup must
/// match it (the old K==1 scope refused everything but a single prefix stmt).
#[test]
fn value_prefix_k2_matches() {
    let obj = local("obj");
    let a = local("a");
    let b = local("b");
    let body = vec![
        assign_local(&a, field(local_value(&obj), "A"), true),
        assign_local(&b, field(local_value(&obj), "B"), true),
        if_stmt(
            bin(
                local_value(&a),
                BinaryOperation::GreaterThan,
                local_value(&b),
            ),
            vec![return_one(local_value(&a))],
            vec![return_one(local_value(&b))],
        ),
    ];
    let t = value_prefix_target_k(&body);
    assert_eq!(t.prefix_len, 2);

    let a2 = local("a2");
    let b2 = local("b2");
    let v = local("v");
    let candidate = vec![
        assign_local(&a2, field(local_value(&obj), "A"), true),
        assign_local(&b2, field(local_value(&obj), "B"), true),
        init_less_decl(&v),
        if_stmt(
            bin(
                local_value(&a2),
                BinaryOperation::GreaterThan,
                local_value(&b2),
            ),
            vec![assign_local(&v, local_value(&a2), false)],
            vec![assign_local(&v, local_value(&b2), false)],
        ),
        print_x(), // trailing: window isn't whole-body
    ];

    let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
        .expect("K==2 value prefix should match");
    // span = prefix a2(0) + prefix b2(1) + RESULT decl(2) + value-if(3).
    assert_eq!(hit.consume, 4);
    assert_eq!(hit.results, vec![v]);
}

/// P9: a guard whose condition is RELATIONAL (`<`) IS now polarity-flipped.
/// The flip is the value-exact, NaN-safe identity `if C then A else B ≡
/// if not C then B else A` realised by a structural `not`-wrap — it never
/// rewrites `not (k < 0)` into the NaN-unsafe `k >= 0`, so it is sound for any
/// condition. (Was `refuse_relational_guard_not_flipped` pre-P9.)
#[test]
fn relational_guard_is_polarity_flipped() {
    let obj = local("obj");
    let k = local("k");
    let body = vec![
        assign_local(&k, field(local_value(&obj), "Field"), true),
        if_stmt(
            bin(local_value(&k), BinaryOperation::LessThan, number(0.0)),
            vec![return_one(boolean(false))],
            vec![],
        ),
        return_one(local_value(&k)),
    ];
    let t = value_prefix_target(&body); // pat = [assign k, If(not(k<0), [return k], [return false])]

    let k2 = local("k2");
    let v = local("v");
    let candidate = vec![
        assign_local(&k2, field(local_value(&obj), "Field"), true),
        init_less_decl(&v),
        if_stmt(
            bin(local_value(&k2), BinaryOperation::LessThan, number(0.0)),
            vec![assign_local(&v, boolean(false), false)],
            vec![assign_local(&v, local_value(&k2), false)],
        ),
        print_x(),
    ];

    // f(obj) = k=obj.Field; if k<0 then return false end; return k. The
    // candidate computes v = (k2<0) ? false : k2 == f(obj). The flip negates
    // the candidate's `k2<0` to `not (k2<0)` (NOT `k2>=0`) and swaps branches.
    let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
        .expect("relational guard condition IS polarity-flipped under P9");
    assert_eq!(hit.consume, 3); // prefix k2(0) + RESULT decl(1) + value-if(2)
    assert_eq!(hit.results, vec![v]);
}

/// Red-team: the polarity flip lines the diamond up correctly, but a leaf value
/// DIVERGES from the pattern. Exact unification must still refuse — the flip is
/// only a structural re-orientation, never a relaxation of value equality.
#[test]
fn flip_with_divergent_leaf_is_refused() {
    let afk = local("afkConfig");
    let place_id = local("placeId");
    let body = vec![
        assign_local(
            &place_id,
            call1(global("tonumber"), field(local_value(&afk), "PlaceId")),
            true,
        ),
        if_stmt(
            bin(
                field(local_value(&afk), "Enabled"),
                BinaryOperation::Equal,
                boolean(false),
            ),
            vec![return_one(boolean(false))],
            vec![],
        ),
        if_stmt(
            bin(
                local_value(&place_id),
                BinaryOperation::And,
                bin(
                    local_value(&place_id),
                    BinaryOperation::GreaterThan,
                    number(0.0),
                ),
            ),
            vec![return_one(bin(
                field(global("game"), "PlaceId"),
                BinaryOperation::Equal,
                local_value(&place_id),
            ))],
            vec![return_one(bin(
                field(global("game"), "PlaceId"),
                BinaryOperation::Equal,
                number(0.0),
            ))],
        ),
    ];
    let t = value_prefix_target(&body);

    let v = local("v");
    let v2 = local("v2");
    let candidate = vec![
        assign_local(
            &v,
            call1(global("tonumber"), field(local_value(&afk), "PlaceId")),
            true,
        ),
        init_less_decl(&v2),
        if_stmt(
            bin(
                field(local_value(&afk), "Enabled"),
                BinaryOperation::Equal,
                boolean(false),
            ),
            // DIVERGENT: pattern's early-return value is `false`, here it is `true`.
            vec![assign_local(&v2, boolean(true), false)],
            vec![if_stmt(
                bin(
                    local_value(&v),
                    BinaryOperation::And,
                    bin(local_value(&v), BinaryOperation::GreaterThan, number(0.0)),
                ),
                vec![assign_local(
                    &v2,
                    bin(
                        field(global("game"), "PlaceId"),
                        BinaryOperation::Equal,
                        local_value(&v),
                    ),
                    false,
                )],
                vec![assign_local(
                    &v2,
                    bin(
                        field(global("game"), "PlaceId"),
                        BinaryOperation::Equal,
                        number(0.0),
                    ),
                    false,
                )],
            )],
        ),
        if_stmt(not_rv(local_value(&v2)), vec![void_return()], vec![]),
    ];

    assert!(
        match_value_prefixed(&candidate, 0, &t, None, false, &mut None).is_none(),
        "a divergent leaf literal must be refused even when the flip aligns the diamond"
    );
}

// === DeInlineReview fixes ===

/// F4 FIX 1: `rvalue_exact_eq` is sign-of-zero / NaN bit-exact, unlike the
/// derived `==` the return-folding and arg-consistency gates previously used.
#[test]
fn rvalue_exact_eq_signed_zero_and_nan() {
    assert!(!rvalue_exact_eq(&number(0.0), &number(-0.0)));
    assert!(rvalue_exact_eq(&number(0.0), &number(0.0)));
    // derived `f64` eq says `NaN != NaN`; bit-exact (same payload) says equal —
    // this only RE-ENABLES correct de-inlines, never an unsound one.
    assert!(rvalue_exact_eq(&number(f64::NAN), &number(f64::NAN)));
    // recursion still distinguishes a nested ±0.0.
    assert!(!rvalue_exact_eq(
        &bin(number(1.0), BinaryOperation::Add, number(0.0)),
        &bin(number(1.0), BinaryOperation::Add, number(-0.0)),
    ));
}

/// F4 FIX 1 in the return-folding gate: an early `return +0.0` must not be
/// treated as equal to a tail `return -0.0` (they differ as `1/x`).
#[test]
fn value_tail_signed_zero_returns_refused() {
    let body = vec![if_stmt(
        local_value(&local("pred")),
        vec![return_one(number(0.0))],
        vec![],
    )];
    assert!(!all_returns_are(&body, &number(-0.0)));
    assert!(all_returns_are(&body, &number(0.0)));
}

/// F4 FIX 2: a parameter occurring twice, bound to two DISTINCT table
/// constructors, is refused (different table identities); a repeated bare local
/// (same value) is fine — and a single-use table never reaches the repeat path.
#[test]
fn repeated_identity_arg_refused_local_ok() {
    let p = local("p");
    let mut params = FxHashSet::default();
    params.insert(p.clone());
    let locals = FxHashSet::default();
    let ctx = MatchCtx {
        params: &params,
        locals: &locals,
    };

    let mut b = Bindings::default();
    assert!(
        unify_rvalue(
            &ctx,
            &local_value(&p),
            &RValue::Table(Table::default()),
            &mut b
        )
        .is_ok()
    );
    assert!(
        unify_rvalue(
            &ctx,
            &local_value(&p),
            &RValue::Table(Table::default()),
            &mut b
        )
        .is_err(),
        "two distinct `{{}}` arguments must not be shared across param occurrences"
    );

    let x = local("x");
    let mut b2 = Bindings::default();
    assert!(unify_rvalue(&ctx, &local_value(&p), &local_value(&x), &mut b2).is_ok());
    assert!(unify_rvalue(&ctx, &local_value(&p), &local_value(&x), &mut b2).is_ok());
}

/// F3 boundary: a synthetic closure has no bytecode provenance, including
/// when hidden in an if-expression, and therefore remains unsafe.
#[test]
fn body_unsafe_sees_closure_inside_if_expression() {
    let closure = RValue::Closure(Closure {
        node_origin: Default::default(),
        function: ByAddress(Arc::new(Mutex::new(Function::default()))),
        upvalues: Vec::new(),
    });
    let unsafe_body = vec![Statement::Return(Return::new(vec![RValue::IfExpression(
        crate::IfExpression::new(local_value(&local("c")), closure, boolean(false)),
    )]))];
    assert!(body_unsafe(&unsafe_body));

    let safe_body = vec![Statement::Return(Return::new(vec![RValue::IfExpression(
        crate::IfExpression::new(local_value(&local("c")), number(1.0), boolean(false)),
    )]))];
    assert!(!body_unsafe(&safe_body));
}

#[test]
fn bytecode_closure_unifies_by_proto_capture_mode_and_mapping() {
    let parameter = local("parameter");
    let argument = local("argument");
    let make = |proto, upvalue| Closure {
        node_origin: Default::default(),
        function: ByAddress(Arc::new(Mutex::new(Function {
            bytecode_proto_id: Some(proto),
            ..Function::default()
        }))),
        upvalues: vec![upvalue],
    };
    let pattern = RValue::Closure(make(41, Upvalue::Copy(parameter.clone())));
    let candidate = RValue::Closure(make(41, Upvalue::Copy(argument.clone())));
    let mut params = FxHashSet::default();
    params.insert(parameter.clone());
    let locals = FxHashSet::default();
    let ctx = MatchCtx {
        params: &params,
        locals: &locals,
    };
    let mut bindings = Bindings::default();

    unify_rvalue(&ctx, &pattern, &candidate, &mut bindings)
        .expect("same bytecode proto and Copy capture must unify");
    assert!(rvalue_exact_eq(
        bindings.params.get(&parameter).unwrap(),
        &local_value(&argument)
    ));

    let wrong_mode = RValue::Closure(make(41, Upvalue::Ref(argument.clone())));
    assert!(unify_rvalue(&ctx, &pattern, &wrong_mode, &mut Bindings::default()).is_err());

    let wrong_proto = RValue::Closure(make(42, Upvalue::Copy(argument)));
    assert!(unify_rvalue(&ctx, &pattern, &wrong_proto, &mut Bindings::default()).is_err());
}

#[test]
fn body_unsafe_allows_only_bytecode_proven_nested_closures() {
    let callback = |proto| {
        RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function {
                bytecode_proto_id: proto,
                ..Function::default()
            }))),
            upvalues: Vec::new(),
        })
    };
    let body = |value| vec![Statement::Call(Call::new(global("spawn"), vec![value]))];

    assert!(!body_unsafe(&body(callback(Some(7)))));
    assert!(body_unsafe(&body(callback(None))));
}

/// F2: an indexed-LHS value collapse is refused (it would reorder the target
/// prefix relative to the moved-in call); a bare-local LHS still collapses.
#[test]
fn collapse_refuses_indexed_lhs_keeps_local_lhs() {
    let v = local("v");
    let t = local("t");
    let call = call1(global("f"), number(1.0));

    let indexed = Statement::Assign(Assign {
        node_origin: Default::default(),
        left: vec![LValue::Index(Index::new(local_value(&t), string("field")))],
        right: vec![local_value(&v)],
        prefix: false,
        parallel: false, compound: false,
    });
    let empty = FxHashSet::default();
    assert!(collapse_use(&indexed, &v, &call, &empty).is_none());

    let x = local("x");
    let local_lhs = assign_local(&x, local_value(&v), false);
    match collapse_use(&local_lhs, &v, &call, &empty).expect("local LHS must collapse") {
        Statement::Assign(a) => assert!(matches!(a.right[0], RValue::Call(_))),
        _ => panic!("expected an Assign"),
    }
}

/// P7-A regression: only a helper proven to return exactly one value (its
/// binder is in `single_valued`) is collapsed into a multi-value context.
/// `local v = helper(args); return v` keeps its form otherwise (a bare
/// `return helper(args)` would propagate ALL of the helper's values, or none,
/// where `local v =` adjusted to one); same for a MULTI-LHS `a, b = v`.
/// Single-value contexts (`if v`, single-LHS `x = v`) collapse for any helper.
#[test]
fn multivalue_helper_not_spread_into_multivalue_context_p7a() {
    let helper = local("helper");
    let v = local("v");
    let call = call1(local_value(&helper), number(1.0));
    let mut proven = FxHashSet::default();
    proven.insert(helper.clone());
    let unknown = FxHashSet::default();

    // `return v` — multi-value context: only a proven single-value helper.
    let ret = Statement::Return(Return::new(vec![local_value(&v)]));
    assert!(collapse_use(&ret, &v, &call, &unknown).is_none(), "return v must NOT collapse an unproven helper");
    assert!(collapse_use(&ret, &v, &call, &proven).is_some(), "return v DOES collapse a single-value helper");

    // MULTI-LHS `a, b = v` — multi-value context.
    let a = local("a");
    let b = local("b");
    let multi_lhs = Statement::Assign(Assign {
        node_origin: Default::default(),
        left: vec![LValue::Local(a.clone()), LValue::Local(b.clone())],
        right: vec![local_value(&v)],
        prefix: false,
        parallel: false, compound: false,
    });
    assert!(collapse_use(&multi_lhs, &v, &call, &unknown).is_none(), "multi-LHS a,b = v must NOT collapse an unproven helper");

    // SINGLE-LHS `x = v` and `if v` truncate to one value for any helper.
    let x = local("x");
    let single_lhs = assign_local(&x, local_value(&v), false);
    assert!(collapse_use(&single_lhs, &v, &call, &unknown).is_some(), "single-LHS x = v collapses any helper (truncates)");
    let if_v = if_stmt(local_value(&v), vec![print_x()], vec![]);
    assert!(collapse_use(&if_v, &v, &call, &unknown).is_some(), "if v collapses any helper (single-value condition)");
}

/// Second review, item 1: the arity proof is read off the declarations
/// before any rewrite. A declaration rebuilt into `local f = factory()` no
/// longer shows `f`'s body, which returns `produce(...)`'s results; a
/// collapse after it must not turn `local r = f(); return r` (one value)
/// into `return f()`.
#[test]
fn closures_are_equal_only_with_the_same_captures() {
    let function = ByAddress(Arc::new(Mutex::new(Function::default())));
    let (a, b) = (local("a"), local("b"));
    let closure = |upvalue: Upvalue| {
        RValue::Closure(Closure { node_origin: Default::default(), function: function.clone(), upvalues: vec![upvalue] })
    };
    assert!(rvalue_exact_eq(&closure(Upvalue::Copy(a.clone())), &closure(Upvalue::Copy(a.clone()))));
    assert!(!rvalue_exact_eq(&closure(Upvalue::Copy(a.clone())), &closure(Upvalue::Copy(b))));
    assert!(!rvalue_exact_eq(&closure(Upvalue::Copy(a.clone())), &closure(Upvalue::Ref(a))));
}

#[test]
fn a_helper_returning_a_call_is_never_proven_single_valued() {
    let produce = |tag: &str| RValue::Call(Call::new(global("produce"), vec![string(tag)]));
    assert!(!returns_exactly_one(&[Statement::Return(Return::new(vec![produce("tag")]))]));
    let one = RValue::Select(crate::Select::Call(Call::new(global("produce"), vec![string("tag")])));
    assert!(returns_exactly_one(&[Statement::Return(Return::new(vec![one]))]));
    // Falls off the end when `c` fails: no value at all.
    let c = local("c");
    let falls = Statement::If(If::new(local_value(&c), Block(vec![return_one(number(1.0))]), Block::default()));
    assert!(!returns_exactly_one(&[falls]));
    assert!(returns_exactly_one(&[return_one(number(1.0))]));
}

/// F5: `body_unsafe` exempts our own reconstruction markers (so a callee body
/// that gained a CALL_MARKER from an inner de-inline stays a valid target),
/// while still refusing a genuine source comment; `canon_top` drops the marker
/// so a re-collected pattern stays length-aligned with its candidates.
#[test]
fn internal_markers_exempted_in_body_unsafe_and_canon() {
    let marked = vec![
        print_x(),
        Statement::Comment(Comment::trailing(CALL_MARKER.to_string())),
    ];
    assert!(!body_unsafe(&marked));

    let real_comment = vec![
        print_x(),
        Statement::Comment(Comment::new(" a real source comment".to_string())),
    ];
    assert!(body_unsafe(&real_comment));

    assert_eq!(
        canon_top(&marked, true).len(),
        1,
        "marker dropped by canon_top"
    );
}

#[test]
fn canon_preserves_generic_for_provenance() {
    let origin = ForOrigin {
        prep_pc: 10,
        step_pc: 20,
        body_pc: 21,
        follow_pc: 22,
        prep_kind: ForPrepKind::Generic,
        base_register: 0,
        result_count: 1,
        aux: 1,
        bytecode_version: 6,
        vm_profile: VmProfileId::Luau,
        explicit_nil_args: false,
    };
    let statement = GenericFor {
        res_locals: vec![local("value")],
        right: vec![global("items")],
        block: Arc::new(Mutex::new(Block::default())),
        origin: Some(origin),
    }
    .into();

    let canonical = canon(&[statement]);
    assert_eq!(
        canonical[0]
            .as_generic_for()
            .expect("canonicalization retains the loop")
            .origin,
        Some(origin)
    );
}

#[test]
fn consuming_unguard_matches_reference_shape_origins_and_owners() {
    use crate::{node_origins, Traverse, Upvalue};
    type OriginSnapshot = Option<(Vec<std::sync::Arc<node_origins::Input>>, bool, bool, bool, Option<&'static str>)>;
    fn origin(origin: &node_origins::Origin, out: &mut Vec<OriginSnapshot>) {
        out.push(origin.0.as_ref().map(|data| (data.inputs.clone(), data.inlined,
            data.cloned, data.incomplete, data.synthesized)));
    }
    fn snapshot_value(value: &RValue, tags: &mut Vec<OriginSnapshot>, numbers: &mut Vec<u64>) {
        if let Some(value) = node_origins::value(value) { origin(value, tags); }
        if let RValue::Literal(Literal::Number(value)) = value { numbers.push(value.to_bits()); }
        if let RValue::Closure(closure) = value {
            snapshot(&closure.function.0.lock().body.0, tags, numbers);
        }
        value.visit_rvalues(&mut |value| { snapshot_value(value, tags, numbers); true });
    }
    fn snapshot(statements: &[Statement], tags: &mut Vec<OriginSnapshot>, numbers: &mut Vec<u64>) {
        for statement in statements {
            if let Some(value) = node_origins::statement(statement) { origin(value, tags); }
            for value in stmt_rvalues(statement) { snapshot_value(value, tags, numbers); }
            match statement {
                Statement::If(node) => {
                    snapshot(&node.then_block.lock().0, tags, numbers);
                    snapshot(&node.else_block.lock().0, tags, numbers);
                }
                Statement::While(node) => snapshot(&node.block.lock().0, tags, numbers),
                Statement::Repeat(node) => snapshot(&node.block.lock().0, tags, numbers),
                Statement::NumericFor(node) => snapshot(&node.block.lock().0, tags, numbers),
                Statement::GenericFor(node) => snapshot(&node.block.lock().0, tags, numbers),
                _ => {}
            }
        }
    }
    fn new_origin(index: &mut usize) -> node_origins::Origin {
        *index += 1;
        let mut origin = node_origins::Origin::input(node_origins::Input {
            function: "unguard_differential".into(), block: *index / 4,
            statement: *index, value: Some(*index % 4),
        });
        let data = origin.0.as_mut().unwrap();
        data.inlined = *index & 1 != 0;
        data.cloned = *index & 2 != 0;
        data.incomplete = *index & 4 != 0;
        if *index & 8 != 0 { data.synthesized = Some("test_origin"); }
        origin
    }
    fn annotate_value(value: &mut RValue, index: &mut usize) {
        if let Some(origin) = node_origins::value_mut(value) { *origin = new_origin(index); }
        if let RValue::Closure(closure) = value {
            annotate(&mut closure.function.0.lock().body.0, index);
        }
        value.visit_rvalues_mut(&mut |value| { annotate_value(value, index); true });
    }
    fn annotate(statements: &mut [Statement], index: &mut usize) {
        for statement in statements {
            if let Some(origin) = node_origins::statement_mut(statement) { *origin = new_origin(index); }
            for value in stmt_rvalues_mut(statement) { annotate_value(value, index); }
            match statement {
                Statement::If(node) => {
                    annotate(&mut node.then_block.lock().0, index);
                    annotate(&mut node.else_block.lock().0, index);
                }
                Statement::While(node) => annotate(&mut node.block.lock().0, index),
                Statement::Repeat(node) => annotate(&mut node.block.lock().0, index),
                _ => {}
            }
        }
    }
    let binding = local("value");
    for seed in 0..1024usize {
        let function = Arc::new(Mutex::new(Function {
            bytecode_proto_id: Some(7),
            body: Block(vec![return_one(add_one(&binding))]),
            ..Default::default()
        }));
        let closure = || RValue::Closure(Closure {
            node_origin: Default::default(), function: ByAddress(function.clone()),
            upvalues: vec![Upvalue::Ref(binding.clone()), Upvalue::Copy(binding.clone())],
        });
        let condition = |choice: usize| {
            let comparison: RValue = Binary::new(local_value(&binding),
                number(f64::from_bits(0x7ff8_0000_0000_1234)),
                [BinaryOperation::Equal, BinaryOperation::NotEqual,
                    BinaryOperation::LessThan, BinaryOperation::LessThanOrEqual][choice % 4]).into();
            if choice & 4 == 0 { comparison }
            else { Unary::new(comparison, UnaryOperation::Not).into() }
        };
        let mut source = Vec::new();
        let mut choices = seed;
        for at in 0..7 {
            let choice = (choices + at) % 10;
            choices = choices / 7 + 3;
            source.push(match choice {
                0 => print_x(),
                1 => Statement::Call(Call::new(global("consume"), vec![closure(), string("a\0\u{ff}7")])),
                2 => void_return(),
                3 => return_one(number(-0.0)),
                4 => if_stmt(condition(seed + at), vec![void_return()], vec![]),
                5 => if_stmt(condition(seed + at), vec![print_x(), return_one(add_one(&binding))], vec![]),
                6 => if_stmt(condition(seed + at), vec![return_one(closure())], vec![]),
                7 => if_stmt(condition(seed + at), vec![void_return()], vec![print_x()]),
                8 => if_stmt(condition(seed + at), vec![return_one(number(2.0)), print_x()], vec![]),
                _ => Statement::While(While::new(condition(seed + at), Block(vec![
                    if_stmt(condition(seed), vec![void_return()], vec![]), print_x()]))),
            });
        }
        annotate(&mut source, &mut 0);
        let (mut source_tags, mut source_numbers) = (Vec::new(), Vec::new());
        snapshot(&source, &mut source_tags, &mut source_numbers);
        // This is unguard's exact production precondition: canon_top has
        // already cloned retained statements, but still shares block/body Arcs.
        let actual = unguard(source.clone());
        let shape = format!("{actual:?}");
        let rendered = Block(actual.clone()).to_string();
        let (mut tags, mut numbers) = (Vec::new(), Vec::new());
        snapshot(&actual, &mut tags, &mut numbers);
        let owners = (Arc::count(&binding.0.0), Arc::strong_count(&function));
        drop(actual);
        let expected = unguard_reference(source.clone());
        assert_eq!(format!("{expected:?}"), shape, "seed {seed}: shape");
        assert_eq!(Block(expected.clone()).to_string(), rendered, "seed {seed}: source");
        let (mut expected_tags, mut expected_numbers) = (Vec::new(), Vec::new());
        snapshot(&expected, &mut expected_tags, &mut expected_numbers);
        assert_eq!(expected_tags, tags, "seed {seed}: full origins");
        assert_eq!(expected_numbers, numbers, "seed {seed}: float bits");
        assert_eq!((Arc::count(&binding.0.0), Arc::strong_count(&function)), owners,
            "seed {seed}: local and closure ownership");
        let (mut after_tags, mut after_numbers) = (Vec::new(), Vec::new());
        snapshot(&source, &mut after_tags, &mut after_numbers);
        assert_eq!(after_tags, source_tags, "seed {seed}: shared inputs unchanged");
        assert_eq!(after_numbers, source_numbers);
    }
}

#[test]
fn consuming_unguard_reuses_already_cloned_operand_and_literal_storage() {
    fn storage(statements: &[Statement]) -> Vec<(usize, usize)> {
        statements.iter().map(|statement| {
            let Statement::Call(call) = statement else { unreachable!() };
            let RValue::Literal(Literal::String(bytes)) = &call.arguments[0] else { unreachable!() };
            (call.value.as_ref() as *const RValue as usize, bytes.as_ptr() as usize)
        }).collect()
    }
    for count in [64, 256, 1024] {
        let source: Vec<_> = (0..count).map(|_| Statement::Call(Call::new(
            global("observe"), vec![Literal::String(vec![0xff; 128]).into()]))).collect();
        let prepared = source.clone();
        let before = storage(&prepared);
        assert_eq!(storage(&unguard(prepared)), before);
        let reference_input = source.clone();
        let before = storage(&reference_input);
        let after = storage(&unguard_reference(reference_input));
        assert!(before.iter().zip(&after).all(|(a, b)| a.0 != b.0 && a.1 != b.1));
    }
}

/// Exhaustive equivalence: `canon_top_len(stmts, tail) == canon_top(stmts, tail).len()`
/// over EVERY sequence of length 0..=4 from a canon-relevant alphabet (Empty /
/// internal-marker / source-comment trivia; plain / void-return / value-return
/// statements; foldable + several non-foldable guard shapes; a 2-value return), for
/// both tail values — ~41k cases. Computes the real length via `canon_top` directly
/// (independent of the in-function debug_assert), pinning the non-allocating length
/// mirror to `canon_top` even for release builds where the debug_assert is gone.
#[test]
fn canon_top_len_mirrors_canon_top_exhaustively() {
    let make = |sym: u8| -> Statement {
        match sym {
            0 => Statement::Empty(Empty {}),
            1 => Statement::Comment(Comment::trailing(CALL_MARKER.to_string())), // internal trivia
            2 => Statement::Comment(Comment::new(" source".to_string())),        // NOT trivia
            3 => print_x(),                                                      // plain stmt
            4 => void_return(),                                                  // void return
            5 => return_one(number(1.0)),                                        // value return
            6 => if_stmt(global("c"), vec![void_return()], vec![]), // foldable void guard
            7 => if_stmt(global("c"), vec![return_one(number(2.0))], vec![]), // foldable value guard
            8 => if_stmt(global("c"), vec![void_return()], vec![print_x()]),  // else nonempty
            9 => if_stmt(global("c"), vec![print_x(), void_return()], vec![]), // then len 2
            10 => if_stmt(global("c"), vec![print_x()], vec![]),              // then non-return
            _ => if_stmt(
                global("c"),
                vec![Statement::Return(Return::new(vec![
                    number(1.0),
                    number(2.0),
                ]))],
                vec![],
            ), // 2-value return then-block
        }
    };
    const ALPHA: u8 = 12;
    for len in 0..=4usize {
        let mut idx = vec![0u8; len];
        loop {
            let stmts: Vec<Statement> = idx.iter().map(|&s| make(s)).collect();
            for &tail in &[false, true] {
                assert_eq!(
                    canon_top_len(&stmts, tail),
                    canon_top(&stmts, tail).len(),
                    "canon_top_len mismatch: tail={} seq={:?}",
                    tail,
                    idx
                );
            }
            if len == 0 {
                break;
            }
            let mut p = len - 1;
            loop {
                idx[p] += 1;
                if idx[p] < ALPHA {
                    break;
                }
                idx[p] = 0;
                if p == 0 {
                    break;
                }
                p -= 1;
            }
            if idx.iter().all(|&x| x == 0) {
                break;
            }
        }
    }
}

/// P-perf prefilter: `stmt_anchor_key` must give EQUAL keys for equal fixed names
/// and DISTINCT keys for distinct names (a method name, or a global-call callee),
/// and `None` where there is no fixed name (a local-callee call, a non-call). This
/// is the contract the name prefilter relies on for false-negative freedom.
#[test]
fn stmt_anchor_key_contract() {
    let recv = local("o");
    let mc = |m: &str| {
        Statement::MethodCall(MethodCall {
            node_origin: Default::default(),
            value: Box::new(local_value(&recv)),
            method: m.to_string(),
            arguments: vec![],
        })
    };
    assert!(stmt_anchor_key(&mc("Foo")).is_some());
    assert_eq!(stmt_anchor_key(&mc("Foo")), stmt_anchor_key(&mc("Foo")));
    assert_ne!(stmt_anchor_key(&mc("Foo")), stmt_anchor_key(&mc("Bar")));

    let gc = |g: &str| Statement::Call(Call::new(global(g), vec![]));
    assert!(stmt_anchor_key(&gc("foo")).is_some());
    assert_ne!(stmt_anchor_key(&gc("foo")), stmt_anchor_key(&gc("bar")));
    // method vs global with same text are distinct (kind-tagged).
    assert_ne!(stmt_anchor_key(&mc("foo")), stmt_anchor_key(&gc("foo")));

    // no fixed name -> None (the prefilter then never skips).
    assert!(stmt_anchor_key(&Statement::Call(Call::new(local_value(&recv), vec![]))).is_none());
    assert!(stmt_anchor_key(&void_return()).is_none());
    assert!(stmt_anchor_key(&print_x()).is_some()); // print(...) is a global call
}

// ---- ROADMAP C: canon/shape extensions ----

fn print_local(l: &RcLocal) -> Statement {
    Statement::Call(Call::new(global("print"), vec![local_value(l)]))
}


#[test]
fn tail_spine_len_counts_lifted_arms() {
    // `a; if c then x; y end; return` -> at a site: `a; if not c then return end; x; y; return`
    let c = local("c");
    let body = vec![
        print_x(),
        if_stmt(local_value(&c), vec![print_x(), print_x()], vec![]),
        Statement::Return(Return::default()),
    ];
    assert_eq!(tail_spine_len(&body), 3 + 2);
    // nested tail `if`s lift recursively
    let nested = vec![if_stmt(
        local_value(&c),
        vec![print_x(), if_stmt(local_value(&c), vec![print_x()], vec![print_x()])],
        vec![],
    )];
    assert_eq!(tail_spine_len(&nested), 1 + 2 + 1 + 1);
}

#[test]
fn void_return_after_statement_makes_it_tail() {
    assert!(continues_with_void_return_only(&[Statement::Return(Return::default())]));
    assert!(continues_with_void_return_only(&[
        Statement::Empty(Empty {}),
        Statement::Return(Return::default()),
    ]));
    assert!(!continues_with_void_return_only(&[]));
    assert!(!continues_with_void_return_only(&[print_x(), Statement::Return(Return::default())]));
    assert!(!continues_with_void_return_only(&[return_one(number(1.0))]));
    assert!(!continues_with_void_return_only(&[Statement::Break(Break {})]));
    // The backward pass agrees with scanning every suffix.
    let empty = || Statement::Empty(Empty {});
    let void = || Statement::Return(Return::default());
    let blocks = [
        vec![print_x(), empty(), void(), empty()],
        vec![void(), empty(), void()],
        vec![empty(), empty(), print_x()],
        vec![print_x(), return_one(number(1.0)), empty()],
    ];
    for block in blocks {
        let expected: Vec<bool> = (0..block.len()).map(|j| continues_with_void_return_only(&block[j + 1..])).collect();
        assert_eq!(void_return_tails(&block), expected);
    }
}

#[test]
fn arm_tail_ret_requires_every_path_to_return_the_value() {
    let clone = local("clone");
    let c = local("c");
    let window = vec![if_stmt(
        local_value(&c),
        vec![print_x(), return_one(local_value(&clone))],
        vec![return_one(local_value(&clone))],
    )];
    let ret = arm_tail_ret(&window, 0, 1, true).expect("both arms return `clone`");
    assert!(rvalue_exact_eq(&ret, &local_value(&clone)));
    // not at the block end / not func tail -> refused
    assert!(arm_tail_ret(&window, 0, 1, false).is_none());
    // a fall-through arm -> refused
    let falls = vec![if_stmt(
        local_value(&c),
        vec![print_x()],
        vec![return_one(local_value(&clone))],
    )];
    assert!(arm_tail_ret(&falls, 0, 1, true).is_none());
    // the value must not be written inside the window
    let written = vec![if_stmt(
        local_value(&c),
        vec![assign_local(&clone, number(1.0), false), return_one(local_value(&clone))],
        vec![return_one(local_value(&clone))],
    )];
    assert!(arm_tail_ret(&written, 0, 1, true).is_none());
}

#[test]
fn alias_result_leaves_rewrites_early_result_write() {
    // if c then RESULT = create(); use(RESULT) else RESULT = x end
    let r = local("result");
    let c = local("c");
    let x = local("x");
    let region = vec![if_stmt(
        local_value(&c),
        vec![
            assign_local(&r, RValue::Call(Call::new(global("create"), vec![])), false),
            print_local(&r),
        ],
        vec![assign_local(&r, local_value(&x), false)],
    )];
    let rewritten = alias_result_leaves(&region, &r).expect("then-leaf is the alias shape");
    let Statement::If(f) = &rewritten[0] else {
        panic!()
    };
    let then = f.then_block.lock();
    assert_eq!(then.0.len(), 3);
    let Statement::Assign(decl) = &then.0[0] else {
        panic!()
    };
    assert!(decl.prefix, "the early write becomes a fresh local decl");
    let LValue::Local(t) = &decl.left[0] else {
        panic!()
    };
    assert_ne!(t, &r);
    assert_eq!(count_local_reads(&then.0[1..2], t), 1, "uses are redirected to the temp");
    let Statement::Assign(last) = &then.0[2] else {
        panic!()
    };
    assert!(!last.prefix && matches!(&last.left[0], LValue::Local(l) if l == &r));
    // the else leaf already ends in the result write: untouched
    assert_eq!(f.else_block.lock().0.len(), 1);
    // a leaf already ending in `RESULT = X` everywhere -> nothing to do
    let plain = vec![if_stmt(
        local_value(&c),
        vec![assign_local(&r, number(1.0), false)],
        vec![assign_local(&r, local_value(&x), false)],
    )];
    assert!(alias_result_leaves(&plain, &r).is_none());
    // RESULT read before its write -> refused (left unchanged)
    let read_first = vec![if_stmt(
        local_value(&c),
        vec![print_local(&r), assign_local(&r, number(1.0), false), print_local(&r)],
        vec![assign_local(&r, local_value(&x), false)],
    )];
    assert!(alias_result_leaves(&read_first, &r).is_none());
}

#[test]
fn written_param_binds_through_prefix_copy() {
    // helper(p, v): if p then v = v + 1 end; print(v)   -- `v` is WRITTEN
    let p = local("p");
    let v = local("v");
    let pat = canon(&[
        if_stmt(local_value(&p), vec![assign_local(&v, add_one(&v), false)], vec![]),
        print_local(&v),
    ]);
    let pat0_kind = std::mem::discriminant(&pat[0]);
    let mut locals = FxHashSet::default();
    locals.insert(v.clone());
    let t = Target {
        f_local: local("f"),
        func_ptr: std::ptr::null::<Mutex<Function>>(),
        kind: TKind::Void,
        pat_raw_len: 2,
        pat_spine_len: tail_spine_len(&pat),
        pat_nodes: 1,
        focused: true,
        value_anchor: ValueAnchor::AtResultDecl,
        prefix_len: 0,
        pat0_kind,
        pat0_anchor_key: None,
        pat,
        params: [p.clone()].into_iter().collect(),
        locals,
        param_order: vec![p.clone(), v.clone()],
        written_params: vec![v.clone()],
        unread: FxHashSet::default(),
        first_reads: Vec::new(),
        first_register_reads: Vec::new(),
        free_cells: Vec::new(),
        specializable: false,
        truth_params: Vec::new(),
        optional_params: Vec::new(),
        specializations: Default::default(),
        falls_off: false,
        cps_loop_return: false,
        loop_exit_at: None,
        returns: Vec::new(),
        captures: Default::default(),
        search: Default::default(),
    };
    // site: local L = 7; if q then L = L + 1 end; print(L)
    let q = local("q");
    let l = local("L");
    let cand = canon(&[
        if_stmt(local_value(&q), vec![assign_local(&l, add_one(&l), false)], vec![]),
        print_local(&l),
    ]);
    let prefix = vec![(l.clone(), number(7.0))];
    let u = try_unify_site(&t, &cand, &prefix, None).expect("written param binds to the copy");
    assert_eq!(u.args.len(), 2);
    assert!(rvalue_exact_eq(&u.args[0], &local_value(&q)));
    assert!(rvalue_exact_eq(&u.args[1], &number(7.0)));
    assert!(u.callee_locals.contains(&l), "the copy is a callee temp (must be dead after)");
    // without the copy the written param has no argument -> refused
    assert!(try_unify_site(&t, &cand, &[], None).is_none());
    // a copy that binds no param would be silently deleted -> refused
    let stray = vec![(l.clone(), number(7.0)), (local("other"), number(1.0))];
    assert!(try_unify_site(&t, &cand, &stray, None).is_none());
}
