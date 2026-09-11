//! Rebuild a private constructor across a single-property assignment diamond.
//! The selected value stays in explicit branch statements, evaluated once.
use std::collections::BTreeMap;

use rustc_hash::FxHashSet;
use serde::Serialize;

use crate::{Assign, Block, If, LValue, Local, LocalRw, RValue, RcLocal, Statement, Traverse};
use crate::lower_conditionals::{Frame, local_rewrite_frame, prepare_local_rewrite};
use crate::local_producers::{Ledger, Role};

const REGION_LIMIT: usize = 256;

#[derive(Default, Debug, Serialize)]
pub struct Report {
    pub model: &'static str,
    pub candidate_regions: usize,
    pub rebuilt_regions: usize,
    pub introduced_locals: usize,
    pub introduced_bindings: Ledger,
    pub initializer_snapshots: usize,
    pub folded_following_fields: usize,
    pub refused_regions: BTreeMap<&'static str, usize>,
    pub budget_exhausted: bool,
}

fn object(statement: &Statement) -> Option<&RcLocal> {
    let Statement::Assign(assign) = statement else { return None; };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    match (&assign.left[0], &assign.right[0]) {
        (LValue::Local(local), RValue::Table(_)) => Some(local),
        _ => None,
    }
}

fn potential(block: &Block) -> bool {
    block.0.windows(2).any(|pair| object(&pair[0]).is_some() && matches!(pair[1], Statement::If(_)))
}

pub fn rebuild_branch_constructors(block: &mut Block) -> Report {
    let report = Report { model: "luau-v9-private-property-diamond-v2", ..Default::default() };
    let inventory = match prepare_local_rewrite(block, potential) {
        Ok(Some(inventory)) => inventory,
        Ok(None) => return report,
        Err(_) => return Report { budget_exhausted: true, ..report },
    };
    // Numeric IDs retain no owners. A capture anywhere in the original tree is
    // enough to refuse moving the table's initialization.
    let mut state = State {
        report, reserved: inventory.reserved, captured: inventory.captured,
        next_name: 0, visited: FxHashSet::default(),
    };
    state.function(block, &[]);
    state.report
}

struct State {
    report: Report,
    reserved: FxHashSet<String>,
    captured: FxHashSet<u64>,
    next_name: usize,
    visited: FxHashSet<usize>,
}

impl State {
    fn function(&mut self, block: &mut Block, parameters: &[RcLocal]) {
        // One new local plus a literal-key/local-value pair in the constructor.
        let mut frame = local_rewrite_frame(block, parameters, 2);
        self.block(block, &mut frame);
    }

    fn value(&mut self, value: &mut RValue) {
        if let RValue::Closure(closure) = value {
            let id = (&*closure.function.0 as *const _) as usize;
            if self.visited.insert(id) {
                let mut function = closure.function.lock();
                let parameters = function.parameters.clone();
                self.function(&mut function.body, &parameters);
            }
        }
        for child in value.rvalues_mut() { self.value(child); }
    }

    fn block(&mut self, block: &mut Block, frame: &mut Frame) {
        for statement in &mut block.0 {
            if let Statement::Assign(assign) = statement {
                for left in &mut assign.left {
                    if let LValue::Index(index) = left {
                        self.value(&mut index.left);
                        self.value(&mut index.right);
                    }
                }
            }
            for value in statement.rvalues_mut() { self.value(value); }
            match statement {
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
        }
        let mut index = 0;
        while index + 1 < block.0.len() {
            if object(&block.0[index]).is_none() || !matches!(block.0[index + 1], Statement::If(_)) {
                index += 1;
                continue;
            }
            self.report.candidate_regions += 1;
            match self.rewrite(block, index, frame) {
                Ok(inserted) => index += inserted - 1, // Reconsider the moved constructor.
                Err(reason) => {
                    *self.report.refused_regions.entry(reason).or_default() += 1;
                    index += 1;
                }
            }
        }
    }

    fn inert_initializer(&self, value: &RValue, object: &RcLocal, frame: &Frame) -> bool {
        match value {
            RValue::Literal(_) => true,
            RValue::Local(local) => local != object && frame.locals.contains(&local.stable_id())
                && !self.captured.contains(&local.stable_id()),
            RValue::Table(table) => table.0.iter().all(|(key, value)| {
                key.as_ref().is_none_or(total_literal_key) && self.inert_initializer(value, object, frame)
            }),
            _ => false,
        }
    }

    fn rewrite(&mut self, block: &mut Block, index: usize, frame: &mut Frame) -> Result<usize, &'static str> {
        if self.report.rebuilt_regions >= REGION_LIMIT {
            self.report.budget_exhausted = true;
            return Err("region_budget");
        }
        if frame.headroom == 0 { return Err("local_or_register_budget"); }
        let object = object(&block.0[index]).unwrap().clone();
        if object.has_source_binding() { return Err("recorded_table_binding"); }
        if self.captured.contains(&object.stable_id()) { return Err("captured_table"); }
        let assign = block.0[index].as_assign().unwrap();
        let Statement::If(branch) = &block.0[index + 1] else { unreachable!() };
        let then_block = branch.then_block.lock();
        let else_block = branch.else_block.lock();
        let (key, then_value) = property_arm(&then_block, &object).ok_or("branch_shape")?;
        let (other_key, else_value) = property_arm(&else_block, &object).ok_or("branch_shape")?;
        if key != other_key || !total_literal_key(key) { return Err("branch_key"); }
        // Pulling anonymous handlers out of dispatch/props tables creates weak
        // helper aliases and hides their field context. Keep that layout; this
        // is a readability refusal, not a claim that closures are effectful.
        if contains_closure(then_value) || contains_closure(else_value) {
            return Err("callback_property_layout");
        }
        if [&branch.condition, then_value, else_value].into_iter()
            .any(|value| value.values_read().iter().any(|read| *read == &object)) {
            return Err("table_observed_in_branch");
        }
        let table = assign.right[0].as_table().unwrap();
        if table.0.iter().any(|(_, value)| contains_closure(value)) {
            return Err("callback_initializer_layout");
        }
        if table.0.iter().any(|(key, _)| !key.as_ref().is_none_or(total_literal_key)) {
            return Err("initializer_key");
        }
        if assign.right[0].values_read().iter().any(|read| *read == &object) {
            return Err("table_observed_in_initializer");
        }
        if table.0.last().is_some_and(|(key, value)| key.is_none()
            && matches!(value, RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_))) {
            return Err("open_array_tail");
        }
        let snapshots: Vec<_> = table.0.iter().enumerate()
            .filter_map(|(position, (_, value))| (!self.inert_initializer(value, &object, frame)).then_some(position))
            .collect();
        if snapshots.len() + 1 > frame.headroom { return Err("local_or_register_budget"); }
        let condition = branch.condition.clone();
        let (key, then_value, else_value) = (key.clone(), then_value.clone(), else_value.clone());
        drop(then_block);
        drop(else_block);

        // A fresh presentation local carries no recorded source, close or
        // ownership evidence from the table or either selected expression.
        let selected = self.fresh(frame, Role::ConstructorPropertyValue);
        let mut declaration = Assign::new(vec![LValue::Local(selected.clone())], vec![]);
        declaration.prefix = true;
        let arm = |value| Block(vec![Assign::new(vec![LValue::Local(selected.clone())], vec![value]).into()]);
        let new_branch = If::new(condition, arm(then_value), arm(else_value));
        let mut constructor = block.0.remove(index).into_assign().unwrap();
        let mut replacement = Vec::new();
        for position in snapshots {
            let snapshot = self.fresh(frame, Role::ConstructorInitializerSnapshot);
            let value = &mut constructor.right[0].as_table_mut().unwrap().0[position].1;
            let mut initializer = Assign::new(vec![snapshot.clone().into()],
                vec![std::mem::replace(value, snapshot.into())]);
            initializer.prefix = true;
            replacement.push(initializer.into());
            self.report.initializer_snapshots += 1;
        }
        constructor.right[0].as_table_mut().unwrap().0.push((Some(key), RValue::Local(selected.clone())));
        // Fresh arm blocks avoid mutating an Arc-backed branch shared elsewhere.
        replacement.extend([declaration.into(), new_branch.into(), constructor.into()]);
        let inserted = replacement.len();
        block.0.splice(index..index + 1, replacement);
        let constructor_index = index + inserted - 1;
        while constructor_index + 1 < block.0.len() {
            let Some((key, value)) = field_parts(&block.0[constructor_index + 1], &object) else { break; };
            if [key, value].into_iter().any(|value|
                value.values_read().iter().any(|read| *read == &object)) { break; }
            let mut field = block.0.remove(constructor_index + 1).into_assign().unwrap();
            let key = *field.left.remove(0).into_index().unwrap().right;
            let value = field.right.remove(0);
            block.0[constructor_index].as_assign_mut().unwrap().right[0].as_table_mut().unwrap()
                .0.push((Some(key), value));
            self.report.folded_following_fields += 1;
        }
        self.report.rebuilt_regions += 1;
        Ok(inserted)
    }

    fn fresh(&mut self, frame: &mut Frame, role: Role) -> RcLocal {
        let name = loop {
            self.next_name += 1;
            let name = format!("v{}", self.next_name);
            if self.reserved.insert(name.clone()) { break name; }
        };
        let local = RcLocal::new(Local::new(Some(name)));
        frame.locals.insert(local.stable_id());
        frame.headroom -= 1;
        self.report.introduced_locals += 1;
        self.report.introduced_bindings.record(&local, role);
        local
    }
}

fn total_literal_key(key: &RValue) -> bool {
    matches!(key, RValue::Literal(_)) && crate::is_total_table_key(key)
}

fn contains_closure(value: &RValue) -> bool {
    matches!(value, RValue::Closure(_)) || value.rvalues().into_iter().any(contains_closure)
}

fn property_arm<'a>(block: &'a Block, object: &RcLocal) -> Option<(&'a RValue, &'a RValue)> {
    let [statement] = block.0.as_slice() else { return None; };
    field_parts(statement, object)
}

fn field_parts<'a>(statement: &'a Statement, object: &RcLocal) -> Option<(&'a RValue, &'a RValue)> {
    let Statement::Assign(assign) = statement else { return None; };
    if assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 { return None; }
    let LValue::Index(index) = &assign.left[0] else { return None; };
    matches!(index.left.as_ref(), RValue::Local(local) if local == object)
        .then_some((index.right.as_ref(), &assign.right[0]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Call, Closure, Function, Global, Index, Literal, Return, Table, Upvalue};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
    fn string(text: &str) -> RValue { Literal::String(text.as_bytes().to_vec()).into() }
    fn call(name: &str, args: Vec<RValue>) -> RValue {
        Call::new(Global(name.as_bytes().to_vec()).into(), args).into()
    }
    fn decl(local: &RcLocal, value: RValue) -> Statement {
        let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
        assign.prefix = true;
        assign.into()
    }
    fn write(object: &RcLocal, key: RValue, value: RValue) -> Statement {
        Assign::new(vec![Index::new(object.clone().into(), key).into()], vec![value]).into()
    }
    fn diamond(object: &RcLocal, key: RValue, condition: RValue) -> Statement {
        If::new(condition,
            Block(vec![write(object, key.clone(), call("make", vec![Literal::Boolean(true).into()]))]),
            Block(vec![write(object, key, call("make", vec![Literal::Boolean(false).into()]))]),
        ).into()
    }
    fn sample(object: &RcLocal) -> Block {
        Block(vec![
            decl(object, Table(vec![(Some(string("Name")), string("Panel"))]).into()),
            diamond(object, string("Value"), call("condition", vec![])),
            Return::new(vec![object.clone().into()]).into(),
        ])
    }
    fn captured(local: &RcLocal) -> RValue {
        Closure { function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(local.clone())] }.into()
    }

    #[test]
    fn keeps_branches_and_scalar_results_and_rebuilds_following_fields() {
        let object = local("props");
        let mut body = sample(&object);
        body.0.insert(2, write(&object, string("Children"), Table(vec![(None, call("child", vec![]))]).into()));
        let report = rebuild_branch_constructors(&mut body);
        assert_eq!(report.rebuilt_regions, 1);
        assert_eq!(report.introduced_locals, 1);
        assert!(matches!(body.0[1], Statement::If(_)));
        let fresh = body.0[0].as_assign().unwrap().left[0].as_local().unwrap();
        assert!(!fresh.has_source_binding());
        assert_eq!(report.introduced_bindings.records.len(), 1);
        assert_eq!(report.introduced_bindings.records[0].binding_id, format!("b{}", fresh.stable_id()));
        assert!(matches!(report.introduced_bindings.records[0].role, Role::ConstructorPropertyValue));
        assert_eq!(report.folded_following_fields, 1);
        let text = body.to_string();
        assert!(text.contains("if condition() then"));
        assert!(text.contains("Value = v1"));
        assert!(text.contains("Children = { child() }"));
        assert!(text.find("make(false)").unwrap() < text.find("Name = \"Panel\"").unwrap());
        assert_eq!(rebuild_branch_constructors(&mut body).rebuilt_regions, 0);
    }

    #[test]
    fn sequential_diamonds_reuse_only_proven_inert_initializer_reads() {
        let object = local("props");
        let mut body = sample(&object);
        body.0.insert(2, diamond(&object, string("Second"), call("other", vec![])));
        let report = rebuild_branch_constructors(&mut body);
        assert_eq!(report.rebuilt_regions, 2);
        assert_eq!(body.0.len(), 6);
        assert!(body.0[4].as_assign().unwrap().right[0].as_table().unwrap().0.len() == 3);
    }

    #[test]
    fn refuses_invalid_initializer_open_tail_and_dynamic_branch_keys() {
        for kind in 0..5 {
            let object = local("props");
            let mut body = sample(&object);
            match kind {
                0 => body.0[0].as_assign_mut().unwrap().right[0].as_table_mut().unwrap().0 = vec![(None, call("initial", vec![]))],
                1 => body.0[0].as_assign_mut().unwrap().right[0].as_table_mut().unwrap().0[0].0 = Some(Literal::Nil.into()),
                2 => body.0[1] = diamond(&object, Literal::Number(f64::NAN).into(), call("condition", vec![])),
                3 => body.0[1] = diamond(&object, local("key").into(), call("condition", vec![])),
                _ => body.0[1] = diamond(&object, string("Value"), call("observe", vec![object.clone().into()])),
            }
            let before = body.to_string();
            let report = rebuild_branch_constructors(&mut body);
            assert_eq!(report.rebuilt_regions, 0);
            assert_eq!(report.refused_regions.values().sum::<usize>(), 1);
            assert_eq!(before, body.to_string());
        }
    }

    #[test]
    fn captures_in_assignment_addresses_protect_table_and_initializer() {
        for capture_table in [true, false] {
            let object = local("props");
            let input = local("input");
            let mut body = sample(&object);
            if !capture_table {
                body.0[0].as_assign_mut().unwrap().right[0].as_table_mut().unwrap().0[0].1 = input.clone().into();
                body.0.insert(0, decl(&input, Literal::Number(1.0).into()));
            }
            // This closure exists only in an LHS address, not an assignment RHS.
            body.0.insert(body.0.len() - 1, Assign::new(vec![Index::new(
                Call::new(captured(if capture_table { &object } else { &input }), vec![]).into(), string("X")
            ).into()], vec![Literal::Nil.into()]).into());
            let before = body.to_string();
            let report = rebuild_branch_constructors(&mut body);
            if capture_table {
                assert_eq!(report.rebuilt_regions, 0);
                assert_eq!(before, body.to_string());
                assert!(report.refused_regions.contains_key("captured_table"));
            } else {
                assert_eq!(report.rebuilt_regions, 1);
                assert_eq!(report.initializer_snapshots, 1);
                // Freeze the captured read before condition() can change it.
                assert!(body.0[1].to_string().contains("= input"));
            }
        }
    }

    #[test]
    fn snapshots_initial_effects_before_the_condition_in_original_order() {
        let object = local("props");
        let mut body = sample(&object);
        body.0[0].as_assign_mut().unwrap().right[0] = Table(vec![
            (Some(string("First")), call("first", vec![])),
            (Some(string("Second")), call("second", vec![])),
        ]).into();
        let report = rebuild_branch_constructors(&mut body);
        assert_eq!(report.rebuilt_regions, 1);
        assert_eq!(report.initializer_snapshots, 2);
        assert_eq!(report.introduced_locals, 3);
        assert_eq!(report.introduced_bindings.records.len(), 3);
        assert!(report.introduced_bindings.records[1..].iter().all(|r|
            matches!(r.role, Role::ConstructorInitializerSnapshot)));
        assert_eq!(body.0[0].to_string(), "local v2 = first()");
        assert_eq!(body.0[1].to_string(), "local v3 = second()");
        assert!(matches!(body.0[3], Statement::If(_)));
        assert_eq!(rebuild_branch_constructors(&mut body).rebuilt_regions, 0);
    }

    #[test]
    fn reserves_names_and_does_not_mutate_shared_arm_blocks() {
        let object = local("props");
        let mut body = sample(&object);
        let shared = body.0[1].as_if().unwrap().then_block.clone();
        let before = shared.lock().to_string();
        body.0.push(Call::new(Global(b"v1".to_vec()).into(), vec![]).into());
        let report = rebuild_branch_constructors(&mut body);
        assert_eq!(report.rebuilt_regions, 1);
        assert_eq!(before, shared.lock().to_string());
        assert_eq!(body.0[0].to_string(), "local v2");
    }

    #[test]
    fn local_and_register_budgets_include_unused_parameters_and_opaque_lowering() {
        for kind in 0..3 {
            let object = local("props");
            let mut function = Function::default();
            function.parameters = (0..if kind == 0 { 191 } else { 180 })
                .map(|i| local(&format!("p{i}"))).collect();
            function.body = sample(&object);
            if kind == 1 {
                function.body.0.push(Call::new(Global(b"wide".to_vec()).into(),
                    (0..80).map(|_| Literal::Nil.into()).collect()).into());
            } else if kind == 2 {
                function.body.0.push(crate::SetList::new(object, 1, vec![], None).into());
            }
            let mut body = Block(vec![Return::new(vec![Closure {
                function: ByAddress(Arc::new(Mutex::new(function))), upvalues: vec![],
            }.into()]).into()]);
            let before = body.to_string();
            let report = rebuild_branch_constructors(&mut body);
            assert_eq!(report.refused_regions.get("local_or_register_budget"), Some(&1));
            assert_eq!(before, body.to_string());
        }
    }

    #[test]
    fn tree_budget_refusal_is_atomic() {
        let object = local("props");
        let mut body = sample(&object);
        let mut deep = Literal::Nil.into();
        for _ in 0..140 { deep = crate::Unary::new(deep, crate::UnaryOperation::Not).into(); }
        body.0.push(Return::new(vec![deep]).into());
        let before = body.to_string();
        let report = rebuild_branch_constructors(&mut body);
        assert!(report.budget_exhausted);
        assert_eq!(report.rebuilt_regions, 0);
        assert_eq!(body.to_string(), before);
    }

    #[test]
    fn recorded_table_declarations_keep_their_original_source_structure() {
        let object = local("props");
        object.0.lock().add_source_binding(crate::SourceBinding {
            name: "props".into(),
            origin: crate::BindingOrigin::DebugLocal { prototype: 0, register: 0, start_pc: 1, end_pc: 30 },
        });
        let mut body = sample(&object);
        let before = body.to_string();
        let report = rebuild_branch_constructors(&mut body);
        assert_eq!(report.refused_regions.get("recorded_table_binding"), Some(&1));
        assert_eq!(body.to_string(), before);
    }

    #[test]
    fn snapshot_budget_is_checked_before_any_statement_changes() {
        let object = local("props");
        let mut function = Function::default();
        function.parameters = (0..190).map(|i| local(&format!("p{i}"))).collect();
        function.body = sample(&object);
        function.body.0[0].as_assign_mut().unwrap().right[0] = Table(vec![
            (Some(string("First")), call("first", vec![])),
            (Some(string("Second")), call("second", vec![])),
        ]).into();
        let mut body = Block(vec![Return::new(vec![Closure {
            function: ByAddress(Arc::new(Mutex::new(function))), upvalues: vec![],
        }.into()]).into()]);
        let before = body.to_string();
        let report = rebuild_branch_constructors(&mut body);
        assert_eq!(report.refused_regions.get("local_or_register_budget"), Some(&1));
        assert_eq!(report.introduced_locals, 0);
        assert!(report.introduced_bindings.records.is_empty());
        assert_eq!(body.to_string(), before);
    }

    #[test]
    fn whole_tree_region_budget_keeps_remaining_regions_untouched() {
        let mut fields = Vec::new();
        for _ in 0..=REGION_LIMIT {
            let object = local("props");
            let mut function = Function::default();
            function.body = sample(&object);
            fields.push((None, Closure {
                function: ByAddress(Arc::new(Mutex::new(function))), upvalues: vec![],
            }.into()));
        }
        let mut body = Block(vec![Return::new(vec![Table(fields).into()]).into()]);
        let report = rebuild_branch_constructors(&mut body);
        assert_eq!(report.rebuilt_regions, REGION_LIMIT);
        assert_eq!(report.refused_regions.get("region_budget"), Some(&1));
        assert!(report.budget_exhausted);
    }

    #[test]
    fn callback_tables_keep_field_context_instead_of_weak_helper_aliases() {
        for in_property in [false, true] {
            let object = local("Number");
            let mut body = sample(&object);
            let handler: RValue = Closure {
                function: ByAddress(Arc::new(Mutex::new(Function::default()))), upvalues: vec![],
            }.into();
            if in_property {
                body.0[1].as_if_mut().unwrap().then_block.lock().0[0].as_assign_mut().unwrap().right[0] = handler;
            } else {
                body.0[0].as_assign_mut().unwrap().right[0].as_table_mut().unwrap().0[0].1 = handler;
            }
            let before = body.to_string();
            let report = rebuild_branch_constructors(&mut body);
            assert_eq!(report.rebuilt_regions, 0);
            assert_eq!(report.refused_regions.get(if in_property { "callback_property_layout" } else { "callback_initializer_layout" }), Some(&1));
            assert_eq!(body.to_string(), before);
        }
    }
}
