//! Frozen independent naming censuses and usage walk before W7 fusion.
use super::*;
#[derive(Default)]
struct NamingPreparation {
    create_element_aliases: FxHashSet<usize>,
    collapse_candidates: FxHashSet<usize>,
    class_signal_locals: FxHashSet<usize>,
}

impl NamingPreparation {
    fn collect(block: &Block) -> Self {
        let mut result = Self::default();
        result.block(block);
        result
    }

    fn call(&mut self, call: &Call) {
        if global_name(&call.value) == Some("setmetatable")
            && let Some(RValue::Local(meta)) = call.arguments.get(1) {
            self.class_signal_locals.insert(local_ptr(meta));
        }
    }

    fn method(&mut self, call: &MethodCall) {
        if let RValue::Local(receiver) = &*call.value {
            self.class_signal_locals.insert(local_ptr(receiver));
        }
    }

    fn children(&mut self, owner: &impl Traverse) {
        owner.visit_lvalues(&mut |left| { self.children(left); true });
        owner.visit_rvalues(&mut |right| { self.expression(right); true });
    }

    fn expression(&mut self, value: &RValue) {
        preparation_tests::REFERENCE_EXPRESSIONS.with(|count| count.set(count.get() + 1));
        match value {
            RValue::Closure(closure) => self.block(&closure.function.lock().body),
            RValue::Call(call) | RValue::Select(Select::Call(call)) => self.call(call),
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => self.method(call),
            _ => {}
        }
        self.children(value);
    }

    fn block(&mut self, block: &Block) {
        preparation_tests::REFERENCE_STATEMENTS.with(|count| count.set(count.get() + block.len()));
        for window in block.windows(3) {
            if let Some(local) = empty_decl_local(&window[0])
                && let Statement::If(branch) = &window[1]
                && arm_assigns_only(&branch.then_block.lock(), &local)
                && arm_assigns_only(&branch.else_block.lock(), &local)
                && window[2].any_local_read(&mut |read| read == &local) {
                self.collapse_candidates.insert(local_ptr(&local));
            }
        }
        for statement in block.iter() {
            if let Statement::Assign(assign) = statement {
                if assign.prefix {
                    for (left, right) in assign.left.iter().zip(&assign.right) {
                        if let Some(local) = left.as_local() && let RValue::Index(index) = right
                            && index_key(index) == Some("createElement") {
                            self.create_element_aliases.insert(local_ptr(local));
                        }
                    }
                }
                for left in &assign.left {
                    if let LValue::Index(index) = left && let RValue::Local(base) = &*index.left
                        && index_key(index) == Some("__index") {
                        self.class_signal_locals.insert(local_ptr(base));
                    }
                }
            }
            match statement {
                Statement::Call(call) => self.call(call),
                Statement::MethodCall(call) => self.method(call),
                _ => {}
            }
            self.children(statement);
            match statement {
                Statement::If(node) => {
                    self.block(&node.then_block.lock());
                    self.block(&node.else_block.lock());
                }
                Statement::While(node) => self.block(&node.block.lock()),
                Statement::Repeat(node) => self.block(&node.block.lock()),
                Statement::NumericFor(node) => self.block(&node.block.lock()),
                Statement::GenericFor(node) => self.block(&node.block.lock()),
                _ => {}
            }
        }
    }
}

fn collect_field_aliases(block: &Block, aliases: &mut FxHashMap<usize, String>) {
    preparation_tests::REFERENCE_STATEMENTS.with(|count| count.set(count.get() + block.len()));
    for statement in &block.0 {
        if let Statement::Assign(assign) = statement
            && assign.prefix
        {
            for (lvalue, rvalue) in assign.left.iter().zip(&assign.right) {
                if let LValue::Local(local) = lvalue
                    && let RValue::Index(index) = rvalue
                    && let Some(key) = index_key(index)
                {
                    aliases.insert(local_ptr(local), key.to_string());
                }
            }
        }
        let mut functions = Vec::new();
        collect_closures_in_statement(statement, &mut |closure| {
            functions.push(closure.function.clone());
        });
        for function in functions {
            collect_field_aliases(&function.lock().body, aliases);
        }
        match statement {
            Statement::If(node) => {
                collect_field_aliases(&node.then_block.lock(), aliases);
                collect_field_aliases(&node.else_block.lock(), aliases);
            }
            Statement::While(node) => collect_field_aliases(&node.block.lock(), aliases),
            Statement::Repeat(node) => collect_field_aliases(&node.block.lock(), aliases),
            Statement::NumericFor(node) => collect_field_aliases(&node.block.lock(), aliases),
            Statement::GenericFor(node) => collect_field_aliases(&node.block.lock(), aliases),
            _ => {}
        }
    }
}

pub(super) fn gather_usage(
    block: &mut Block,
    in_loop: bool,
    aliases: &FxHashSet<usize>,
    usage: &mut FxHashMap<usize, LocalUsage>,
) {
    let mut field_aliases = FxHashMap::default();
    collect_field_aliases(block, &mut field_aliases);
    let mut context = UsageContext {
        aliases,
        field_aliases: &field_aliases,
        counters: Vec::new(),
    };
    gather_usage_in(block, in_loop, &mut context, usage);
}

/// Read-only census context: the React `createElement` aliases, the
/// `local x = Y.Key` field aliases, and the stack of enclosing numeric-for
/// counters (for `t[i]` array-shape detection).
struct UsageContext<'a> {
    aliases: &'a FxHashSet<usize>,
    field_aliases: &'a FxHashMap<usize, String>,
    counters: Vec<usize>,
}

fn gather_usage_in(
    block: &mut Block,
    in_loop: bool,
    context: &mut UsageContext<'_>,
    usage: &mut FxHashMap<usize, LocalUsage>,
) {
    let aliases = context.aliases;
    for statement in &mut block.0 {
        // Expression-local facts: field reads/writes, callees, callback table fields.
        statement.post_traverse_values(&mut |value| -> Option<()> {
            preparation_tests::REFERENCE_USAGE_VALUES.with(|count| count.set(count.get() + 1));
            match value {
                Either::Right(RValue::Index(index)) => {
                    if let RValue::Local(local) = &*index.left {
                        let entry = usage.entry(local_ptr(local)).or_default();
                        match string_literal(&index.right) {
                            Some(key) => {
                                entry.string_fields_read.insert(key.to_string());
                            }
                            None => {
                                entry.dynamic_indexed = true;
                                let numeric_key = match &*index.right {
                                    RValue::Literal(Literal::Number(_)) => true,
                                    RValue::Local(key) => context.counters.contains(&local_ptr(key)),
                                    _ => false,
                                };
                                if numeric_key {
                                    entry.numeric_indexed = true;
                                }
                                if is_props_special_key(&index.right, context.field_aliases) {
                                    entry.props_key_indexed = true;
                                }
                            }
                        }
                    }
                }
                Either::Right(RValue::Unary(unary)) => {
                    if unary.operation == UnaryOperation::Length
                        && let RValue::Local(local) = &*unary.value
                    {
                        usage.entry(local_ptr(local)).or_default().length_taken = true;
                    }
                }
                Either::Left(crate::LValue::Index(index)) => {
                    if let RValue::Local(local) = &*index.left {
                        let entry = usage.entry(local_ptr(local)).or_default();
                        match string_literal(&index.right) {
                            Some(_) => entry.field_written = true,
                            None => {
                                entry.dynamic_indexed = true;
                                if matches!(&*index.right, RValue::Literal(Literal::Number(_)))
                                    || matches!(&*index.right, RValue::Local(key) if context.counters.contains(&local_ptr(key)))
                                {
                                    entry.numeric_indexed = true;
                                }
                            }
                        }
                    }
                }
                Either::Right(RValue::Call(call))
                | Either::Right(RValue::Select(Select::Call(call))) => {
                    note_call_usage(call, in_loop, aliases, usage)
                }
                Either::Right(RValue::Table(table)) => {
                    for (key, val) in &table.0 {
                        if let (Some(key), RValue::Local(local)) = (key.as_ref(), val)
                            && let Some(key) = string_literal(key)
                            && is_callback_key(key)
                        {
                            note_callback_name(local, key.to_string(), usage);
                        }
                    }
                }
                Either::Right(RValue::MethodCall(method_call))
                | Either::Right(RValue::Select(Select::MethodCall(method_call))) => {
                    note_method_usage(method_call, usage);
                }
                Either::Right(RValue::Binary(binary)) => {
                    note_type_guard(binary, usage);
                    note_or_default(binary, usage);
                    note_elapsed_clock_base(binary, usage);
                }
                _ => {}
            }
            None
        });

        match &*statement {
            Statement::Call(call) => note_call_usage(call, in_loop, aliases, usage),
            Statement::MethodCall(method_call) => note_method_usage(method_call, usage),
            Statement::Assign(assign) => {
                for (lvalue, rvalue) in assign.left.iter().zip(assign.right.iter()) {
                    note_field_store(lvalue, rvalue, usage);
                    if let LValue::Local(local) = lvalue {
                        note_local_write(local, rvalue, usage);
                    }
                    if let crate::LValue::Index(index) = lvalue
                        && let RValue::Local(local) = &*index.left
                    {
                        if string_literal(&index.right).is_none() {
                            note_collection_fill(local, rvalue, usage);
                            note_map_key(local, &index.right, usage);
                        }
                        let create_element = is_create_element_call(rvalue, aliases);
                        let entry = usage.entry(local_ptr(local)).or_default();
                        if in_loop {
                            entry.keyed_assign_in_loop = true;
                        }
                        if create_element {
                            entry.create_element_fill_count += 1;
                            if in_loop {
                                entry.create_element_fill_in_loop = true;
                            }
                        }
                    }
                }
                if assign
                    .right
                    .last()
                    .is_some_and(|value| matches!(value, RValue::Select(_) | RValue::VarArg(_)))
                {
                    for lvalue in assign.left.iter().skip(assign.right.len()) {
                        if let LValue::Local(local) = lvalue {
                            note_unknown_local_write(local, usage);
                        }
                    }
                }
            }
            Statement::Return(ret) => {
                for value in &ret.values {
                    if let RValue::Local(local) = value {
                        usage.entry(local_ptr(local)).or_default().returned = true;
                    }
                }
            }
            Statement::GenericFor(generic_for) => {
                for rvalue in &generic_for.right {
                    if let RValue::Local(local) = unwrap_iter_arg(rvalue) {
                        usage.entry(local_ptr(local)).or_default().iterated = true;
                    }
                }
            }
            Statement::If(node) => {
                if let RValue::Local(local) = &node.condition {
                    usage.entry(local_ptr(local)).or_default().boolean_guarded = true;
                }
            }
            Statement::While(node) => {
                if let RValue::Local(local) = &node.condition {
                    usage.entry(local_ptr(local)).or_default().boolean_guarded = true;
                }
            }
            Statement::Repeat(node) => {
                if let RValue::Local(local) = &node.condition {
                    usage.entry(local_ptr(local)).or_default().boolean_guarded = true;
                }
            }
            _ => {}
        }

        // Recurse: closures reset the loop context; loops set it.
        let mut functions = Vec::new();
        statement.post_traverse_values(&mut |value| -> Option<()> {
            preparation_tests::REFERENCE_USAGE_VALUES.with(|count| count.set(count.get() + 1));
            if let Either::Right(RValue::Closure(closure)) = value {
                functions.push(closure.function.clone());
            }
            None
        });
        for function in functions {
            gather_usage_in(&mut function.lock().body, false, context, usage);
        }
        match &*statement {
            Statement::If(r#if) => {
                gather_usage_in(&mut r#if.then_block.lock(), in_loop, context, usage);
                gather_usage_in(&mut r#if.else_block.lock(), in_loop, context, usage);
            }
            Statement::While(r#while) => {
                gather_usage_in(&mut r#while.block.lock(), true, context, usage)
            }
            Statement::Repeat(repeat) => {
                gather_usage_in(&mut repeat.block.lock(), true, context, usage)
            }
            Statement::NumericFor(numeric_for) => {
                context.counters.push(local_ptr(&numeric_for.counter));
                gather_usage_in(&mut numeric_for.block.lock(), true, context, usage);
                context.counters.pop();
            }
            Statement::GenericFor(generic_for) => {
                gather_usage_in(&mut generic_for.block.lock(), true, context, usage)
            }
            _ => {}
        }
    }
}

/// The local of a `local v` / `local v = nil` empty declaration (the shape a
/// `conditional_expressions` diamond temp is declared with). Mirrors that pass's
/// `candidate_decl` (conditional_expressions.rs:141).
fn collect_local_function_definitions(
    block: &Block,
    definitions: &mut FxHashMap<usize, Vec<RcLocal>>,
    invalid: &mut FxHashSet<usize>,
) {
    preparation_tests::REFERENCE_STATEMENTS.with(|count| count.set(count.get() + block.len()));
    for statement in &block.0 {
        if let Statement::Assign(assign) = statement {
            let multi_tail = assign
                .right
                .last()
                .is_some_and(|value| matches!(value, RValue::Select(_) | RValue::VarArg(_)));
            for (index, left) in assign.left.iter().enumerate() {
                let LValue::Local(binder) = left else {
                    continue;
                };
                let ptr = local_ptr(binder);
                match assign.right.get(index) {
                    Some(RValue::Closure(closure)) => {
                        let parameters = closure.function.lock().parameters.clone();
                        if definitions.insert(ptr, parameters).is_some() {
                            invalid.insert(ptr);
                        }
                    }
                    Some(RValue::Literal(Literal::Nil)) if assign.prefix => {}
                    None if assign.prefix && !multi_tail => {}
                    _ => {
                        invalid.insert(ptr);
                    }
                }
            }
        }
        for value in crate::deinline::stmt_rvalues(statement) {
            collect_definitions_in_rvalue(value, definitions, invalid);
        }
        match statement {
            Statement::If(node) => {
                collect_local_function_definitions(&node.then_block.lock(), definitions, invalid);
                collect_local_function_definitions(&node.else_block.lock(), definitions, invalid);
            }
            Statement::While(node) => {
                collect_local_function_definitions(&node.block.lock(), definitions, invalid)
            }
            Statement::Repeat(node) => {
                collect_local_function_definitions(&node.block.lock(), definitions, invalid)
            }
            Statement::NumericFor(node) => {
                collect_local_function_definitions(&node.block.lock(), definitions, invalid)
            }
            Statement::GenericFor(node) => {
                collect_local_function_definitions(&node.block.lock(), definitions, invalid)
            }
            _ => {}
        }
    }
}

fn collect_definitions_in_rvalue(
    value: &RValue,
    definitions: &mut FxHashMap<usize, Vec<RcLocal>>,
    invalid: &mut FxHashSet<usize>,
) {
    preparation_tests::REFERENCE_EXPRESSIONS.with(|count| count.set(count.get() + 1));
    if let RValue::Closure(closure) = value {
        collect_local_function_definitions(&closure.function.lock().body, definitions, invalid);
        return;
    }
    for child in value.rvalues() {
        collect_definitions_in_rvalue(child, definitions, invalid);
    }
}


pub(super) fn prepare(block: &Block, collect_evidence: bool) -> super::NamingPreparation {
    let old = NamingPreparation::collect(block);
    let mut field_aliases = FxHashMap::default();
    collect_field_aliases(block, &mut field_aliases);
    let mut identities = collect_evidence.then(FxHashMap::default);
    let counts = collect_usage(block).into_iter().map(|(local, usage)| {
        if let Some(identities) = &mut identities { identities.insert(local_ptr(&local), local.stable_id()); }
        (local_ptr(&local), usage)
    }).collect();
    let mut definitions = FxHashMap::default();
    let mut invalid = FxHashSet::default();
    collect_local_function_definitions(block, &mut definitions, &mut invalid);
    definitions.retain(|binder, _| !invalid.contains(binder));
    let definitions = definitions.into_iter().map(|(binder, parameters)| {
        (binder, parameters.iter().map(|local| ParameterIdentity::of(local)).collect())
    }).collect();
    super::NamingPreparation { create_element_aliases: old.create_element_aliases,
        collapse_candidates: old.collapse_candidates, class_signal_locals: old.class_signal_locals,
        field_aliases, counts, identities, definitions, invalid_definitions: FxHashSet::default() }
}

pub(super) fn interprocedural_param_hints(namer: &mut Namer, block: &Block) {
        let mut definitions = FxHashMap::<usize, Vec<RcLocal>>::default();
        let mut invalid = FxHashSet::default();
        collect_local_function_definitions(block, &mut definitions, &mut invalid);
        definitions.retain(|binder, _| !invalid.contains(binder));
        if definitions.is_empty() {
            return;
        }

        let mut consensus = FxHashMap::<usize, ParamConsensus>::default();
        collect_local_function_calls(block, &definitions, namer, &mut consensus);
        let mut hints = Vec::new();
        for (binder, state) in consensus {
            if state.calls == 0 {
                continue;
            }
            let Some(parameters) = definitions.get(&binder) else {
                continue;
            };
            for (index, parameter) in parameters.iter().enumerate() {
                if state.valid.get(index) == Some(&true)
                    && let Some(name) = state.names.get(index).and_then(Clone::clone)
                {
                    hints.push((parameter.clone(), name));
                }
            }
        }
        // Drop all temporary RcLocal clones before `apply` performs Arc-count-
        // based unused detection; only pointer-keyed hints remain in `self`.
        drop(definitions);
        for (parameter, hint) in hints {
            // The old owning parameter remains alive until this iteration ends.
            // Use the same hint emitter so evidence locations compare exactly.
            namer.apply_callsite_hint(ParameterIdentity::of(&parameter), hint);
        }
    }
