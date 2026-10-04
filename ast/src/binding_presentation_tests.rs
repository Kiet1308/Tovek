use crate::{Assign, BindingIntent, BindingOrigin, Block, Call, Closure, Function,
    Global, If, Index, LValue, Literal, Local, RValue, RcLocal, Return, SourceBinding,
    Statement, Upvalue};
use by_address::ByAddress;
use parking_lot::Mutex;
use triomphe::Arc;

fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
fn global(name: &str) -> RValue { Global::from(name).into() }
fn number(value: f64) -> RValue { Literal::Number(value).into() }
fn declare(local: &RcLocal, values: Vec<RValue>) -> Statement {
    let mut assign = Assign::new(vec![local.clone().into()], values);
    assign.prefix = true; assign.into()
}
fn call(value: RValue) -> Statement { Call::new(global("consume"), vec![value]).into() }
fn closure(function: Arc<Mutex<Function>>, upvalues: Vec<Upvalue>) -> RValue {
    Closure { node_origin: Default::default(), function: ByAddress(function), upvalues }.into()
}
fn source(local: &RcLocal, name: &str) {
    local.0.lock().add_source_binding(SourceBinding { name: name.into(),
        origin: BindingOrigin::DebugLocal { prototype: 0, register: 0, start_pc: 0, end_pc: 8 } });
}

#[test]
fn inferred_intent_survives_display_name_changes_and_source_evidence_overrides_it() {
    let temporary = local("v17");
    temporary.0.lock().0 = Some("readableIntermediate".into());
    assert!(temporary.is_inferred_temporary());
    let named = local("sourceValue");
    named.0.lock().0 = Some("v17".into());
    assert!(!named.is_inferred_temporary());
    source(&temporary, "recordedIntermediate");
    assert!(!temporary.is_inferred_temporary());
    assert_eq!(temporary.0.lock().4.intent, BindingIntent::Temporary,
        "recorded evidence overrides policy without rewriting its history");
}

#[test]
fn extra_analysis_owners_do_not_change_unused_local_or_parameter_names() {
    fn build(extra: usize, underscore_global: bool) -> (Block, Vec<RcLocal>) {
        let parameter = RcLocal::default();
        let unused = RcLocal::default();
        let recorded = RcLocal::default(); source(&recorded, "discountAmount");
        let function = Arc::new(Mutex::new(Function {
            parameters: vec![parameter.clone()],
            body: Block(vec![declare(&unused, vec![number(7.0)]),
                declare(&recorded, vec![number(11.0)])]),
            ..Default::default()
        }));
        let mut block = Block(vec![Return::new(vec![closure(function, vec![])]).into()]);
        if underscore_global { block.insert(0, Call::new(global("_"), vec![]).into()); }
        let held = (0..extra).flat_map(|_| [parameter.clone(), unused.clone(), recorded.clone()]).collect();
        (block, held)
    }
    for underscore in [false, true] {
        let (mut plain, _) = build(0, underscore);
        let (mut retained, owners) = build(32, underscore);
        crate::name_locals::name_locals(&mut plain, true);
        crate::name_locals::name_locals(&mut retained, true);
        assert_eq!(plain.to_string(), retained.to_string());
        let output = retained.to_string();
        assert!(output.contains("discountAmount"));
        if underscore {
            assert!(!output.contains("function(_)"));
            assert!(!output.contains("local _ ="));
        } else {
            assert!(output.contains("function(_)"), "{output}");
            assert!(output.contains("local _ = 7"), "{output}");
        }
        assert_eq!(owners.len(), 96);
    }
}

#[test]
fn unused_loop_slots_and_meaningful_structural_names_are_owner_independent() {
    use crate::{GenericFor, Table};
    fn build(extra: usize) -> (Block, Vec<RcLocal>) {
        let folders = RcLocal::default(); let helper = RcLocal::default();
        let index = RcLocal::default(); let value = RcLocal::default();
        let table = Table::new(["NPCS", "Debris"].into_iter().map(|name|
            (None, Index::new(global("workspace"), Literal::String(name.as_bytes().to_vec()).into()).into())).collect());
        let function = Arc::new(Mutex::new(Function { name: Some("releaseResources".into()), ..Default::default() }));
        let block = Block(vec![declare(&folders, vec![table.into()]),
            declare(&helper, vec![closure(function, vec![])]),
            GenericFor::new(vec![index.clone(), value.clone()],
                vec![Call::new(global("ipairs"), vec![global("items")]).into()],
                Block(vec![call(value.clone().into())])).into()]);
        let held = (0..extra).flat_map(|_| [folders.clone(), helper.clone(), index.clone(), value.clone()]).collect();
        (block, held)
    }
    let (mut plain, _) = build(0);
    let (mut retained, owners) = build(16);
    crate::name_locals::name_locals(&mut plain, true);
    crate::name_locals::name_locals(&mut retained, true);
    let output = retained.to_string();
    assert_eq!(plain.to_string(), output);
    assert!(output.contains("local TargetFolders ="), "{output}");
    assert!(output.contains("local function releaseResources("), "{output}");
    assert!(output.contains("for _,"), "{output}");
    assert_eq!(owners.len(), 64);
}

#[test]
fn shared_body_occurrences_and_capture_slots_are_not_unused_binders() {
    let parameter = RcLocal::default();
    let unread = RcLocal::default();
    let shared = Arc::new(Mutex::new(Function { parameters: vec![parameter.clone()],
        body: Block(vec![declare(&unread, vec![number(3.0)])]), ..Default::default() }));
    let mut block = Block(vec![Return::new(vec![closure(shared.clone(), vec![]), closure(shared.clone(), vec![])]).into()]);
    crate::name_locals::name_locals(&mut block, true);
    assert_ne!(parameter.0.lock().0.as_deref(), Some("_"));
    assert_ne!(unread.0.lock().0.as_deref(), Some("_"));

    let captured = RcLocal::default();
    let child = Arc::new(Mutex::new(Function::default()));
    let outer = Arc::new(Mutex::new(Function { parameters: vec![captured.clone()],
        body: Block(vec![Return::new(vec![closure(child, vec![Upvalue::Copy(captured.clone())])]).into()]),
        ..Default::default() }));
    let mut block = Block(vec![Return::new(vec![closure(outer, vec![])]).into()]);
    crate::name_locals::name_locals(&mut block, true);
    assert_ne!(captured.0.lock().0.as_deref(), Some("_"));
}

#[test]
fn alias_and_temporary_rewrites_are_invariant_under_display_renaming() {
    fn input(rename: bool, recorded: bool) -> Block {
        let value = local("v1"); let origin = local("origin");
        if rename { value.0.lock().0 = Some("clearAlias".into()); }
        if recorded { source(&value, "recordedAlias"); }
        Block(vec![declare(&origin, vec![global("input")]),
            declare(&value, vec![origin.clone().into()]), call(value.clone().into()), call(value.into())])
    }
    let mut a = input(false, false); let mut b = input(true, false);
    crate::copy_cleanup::copy_cleanup(&mut a); crate::copy_cleanup::copy_cleanup(&mut b);
    assert_eq!(a.to_string(), b.to_string());
    assert_eq!(a.len(), 3);
    let mut protected = input(true, true); crate::copy_cleanup::copy_cleanup(&mut protected);
    assert_eq!(protected.len(), 4);

    let value = local("v2"); value.0.lock().0 = Some("descriptiveScalar".into());
    let mut scalar = Block(vec![declare(&value, vec![number(9.0)]), call(value.into())]);
    assert!(crate::inline_temps::inline_single_use_temps(&mut scalar));
    assert_eq!(scalar.to_string(), "consume(9)");
}

#[test]
fn conditional_and_terminal_rewrites_use_intent_not_rendered_spelling() {
    fn diamond(rename: bool, named: bool) -> (Block, RcLocal) {
        let value = local(if named { "namedResult" } else { "v3" });
        if rename { value.0.lock().0 = Some(if named { "v3" } else { "choiceValue" }.into()); }
        let store = |number_| Block(vec![Assign::new(vec![value.clone().into()], vec![number(number_)]).into()]);
        (Block(vec![declare(&value, vec![]), If::new(global("flag"), store(1.0), store(2.0)).into(), call(value.clone().into())]), value)
    }
    let (mut plain, plain_binder) = diamond(false, false);
    let (mut renamed, renamed_binder) = diamond(true, false);
    crate::conditional_expressions::reconstruct_conditional_expressions(&mut plain);
    crate::conditional_expressions::reconstruct_conditional_expressions(&mut renamed);
    // Evaluating the global condition before the callee lookup still requires
    // a declaration. Normalize only that exact binding's spelling to compare
    // transformation decisions, not unrelated names in the emitted text.
    assert_eq!(renamed_binder.0.lock().0.as_deref(), Some("choiceValue"));
    renamed_binder.0.lock().0 = plain_binder.0.lock().0.clone();
    assert_eq!(plain.to_string(), renamed.to_string());
    assert_eq!(plain.len(), 2);
    let (mut named, _) = diamond(true, true);
    crate::conditional_expressions::reconstruct_conditional_expressions(&mut named);
    assert_eq!(named.len(), 3);

    let value = local("v4"); value.0.lock().0 = Some("terminalChoice".into());
    let mut terminal = Block(vec![declare(&value, vec![number(5.0)]), Return::new(vec![value.into()]).into()]);
    crate::terminal_returns::reconstruct_terminal_returns(&mut terminal);
    assert_eq!(terminal.to_string(), "return 5");
}

#[test]
fn explicit_receiver_role_survives_display_rename_and_scope_collision_stays_dot_form() {
    let receiver = local("p0"); receiver.mark_method_receiver();
    receiver.0.lock().0 = Some("displayOnlyName".into());
    let function = Arc::new(Mutex::new(Function { parameters: vec![receiver.clone()],
        body: Block(vec![Return::new(vec![Index::new(receiver.into(), Literal::String(b"Value".to_vec()).into()).into()]).into()]),
        ..Default::default() }));
    let target: LValue = Index::new(global("object"), Literal::String(b"run".to_vec()).into()).into();
    let block = Block(vec![Assign::new(vec![target.clone()], vec![closure(function, vec![])]).into()]);
    let output = block.to_string();
    assert!(output.contains("function object:run()"), "{output}");
    assert!(output.contains("return self.Value"), "{output}");
    assert!(!output.contains("displayOnlyName"));

    let first = local("p0"); let other = local("self");
    let function = Arc::new(Mutex::new(Function { parameters: vec![first, other], ..Default::default() }));
    let block = Block(vec![Assign::new(vec![target], vec![closure(function, vec![])]).into()]);
    assert!(block.to_string().contains("function object.run(p0, self)"));
}
