//! Keep actual builtin calls when a different output optimization level could
//! fold them and bypass a replacement supplied by the external environment.
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{deinline_safety::CaptureSafety, effects, Block, Call, LValue, Literal, LocalRw,
    RValue, Select, Statement, Traverse};

/// Run before constant spelling, which intentionally creates
/// foldable constructor calls, and after callee/argument motion has finished.
pub fn preserve_builtin_calls(block: &mut Block, assume_standard_libraries: bool) -> Result<usize, &'static str> {
    group_builtin_calls(block, assume_standard_libraries).map(|(changed, _)| changed)
}

fn group_builtin_calls(block: &mut Block, assume_standard_libraries: bool)
    -> Result<(usize, FxHashSet<usize>), &'static str>
{
    let safety = CaptureSafety::writes_for_read_timing(block);
    let mut state = State { safety: &safety, aliases: FxHashMap::default(),
        visited: FxHashSet::default(), changed: 0, assume_standard_libraries, failure: None,
        native_candidates: FxHashSet::default() };
    state.block(block);
    match state.failure { Some(reason) => Err(reason), None => Ok((state.changed, state.native_candidates)) }
}

/// Start with the existing spelling; protect actual native calls with mutable
/// arguments and recover grouped dispatch only after proving that barrier.
pub fn preserve_native_builtin_calls(block: &mut Block, assume_standard_libraries: bool)
    -> Result<Option<crate::local_producers::Pass>, &'static str>
{
    let (_, candidates) = group_builtin_calls(block, assume_standard_libraries)?;
    protect_native_constant_arguments(block, candidates)
}

/// Separate assignments prevent folding an actual native call past its fallback.
/// Only inert leaves move to entry; dynamic declarations keep their original site.
fn protect_native_constant_arguments(
    body: &mut Block, candidates: FxHashSet<usize>,
) -> Result<Option<crate::local_producers::Pass>, &'static str> {
    if candidates.is_empty() { return Ok(None); }
    // shortcut: deep/oversized trees retain the prior grouping policy, without a new refusal.
    if crate::lower_conditionals::validate_local_rewrite_tree(body).is_err() { return Ok(None); }
    let safety = CaptureSafety::writes_for_read_timing(body);
    if !safety.complete() { return Ok(None); }
    fn contains_call(value: &RValue) -> bool {
        matches!(value, RValue::Call(call) | RValue::Select(Select::Call(call))
            if matches!(call.builtin_fastcall, Some((builtin, 2)) if builtin != 54))
            || value.rvalues().into_iter().any(contains_call)
    }
    let Some(inventory) = crate::lower_conditionals::prepare_local_rewrite(body, |block| {
        block.iter().any(|statement| {
            matches!(statement, Statement::Call(call)
                if matches!(call.builtin_fastcall, Some((builtin, 2)) if builtin != 54))
                || crate::deinline::stmt_rvalues(statement).into_iter().any(contains_call)
        })
    })? else { return Ok(None); };
    let mut state = LiteralArguments {
        reserved: inventory.reserved, visited: FxHashSet::default(), protected: FxHashSet::default(),
        captured: inventory.captured, constants: FxHashSet::default(), locals: FxHashSet::default(),
        unsplittable: FxHashSet::default(), splittable: FxHashSet::default(), parameters: FxHashSet::default(),
        site_locals: FxHashSet::default(), site_plans: FxHashMap::default(), duplicate_declarations: FxHashSet::default(),
        reused_results: FxHashMap::default(), declaration_count: Some(0),
        candidates, safety,
        pass: crate::local_producers::Pass { pass: "native_constant_arguments",
            rewrite_model: "native_argument_barrier_v2", introduced_locals: 0, ledger: Default::default() },
        changed: false,
    };
    state.function(body, &[]);
    Ok(state.changed.then_some(state.pass))
}

struct LiteralArguments {
    reserved: FxHashSet<String>,
    visited: FxHashSet<usize>,
    protected: FxHashSet<u64>,
    captured: FxHashSet<u64>,
    constants: FxHashSet<u64>,
    unsplittable: FxHashSet<u64>,
    splittable: FxHashSet<u64>,
    parameters: FxHashSet<u64>,
    candidates: FxHashSet<usize>,
    locals: FxHashSet<u64>,
    site_locals: FxHashSet<u64>,
    site_plans: FxHashMap<u64, Option<crate::RcLocal>>,
    duplicate_declarations: FxHashSet<u64>,
    reused_results: FxHashMap<u64, crate::RcLocal>,
    declaration_count: Option<usize>,
    safety: CaptureSafety,
    pass: crate::local_producers::Pass,
    changed: bool,
}

impl LiteralArguments {
    fn site_initializer(value: &RValue) -> bool {
        fn contains_closure(value: &RValue) -> bool {
            matches!(value, RValue::Closure(_)) || value.rvalues().into_iter().any(contains_closure)
        }
        match value {
            RValue::Index(index) => matches!(&*index.left, RValue::Local(_))
                && matches!(&*index.right, RValue::Local(_) | RValue::Literal(Literal::String(_))),
            RValue::Local(_) | RValue::VarArg(_) | RValue::Select(Select::VarArg(_)) => true,
            RValue::Call(_) | RValue::Select(Select::Call(_)) => !contains_closure(value),
            _ => false,
        }
    }

    fn find_duplicate_declarations(block: &Block, seen: &mut FxHashSet<u64>, duplicates: &mut FxHashSet<u64>) -> (usize, bool) {
        let (mut declarations, mut opaque) = (0, false);
        for statement in &block.0 {
            if matches!(statement, Statement::Assign(assign) if assign.prefix)
                || matches!(statement, Statement::NumericFor(_) | Statement::GenericFor(_))
            {
                statement.visit_local_writes(&mut |local| {
                    declarations += 1;
                    if !seen.insert(local.stable_id()) { duplicates.insert(local.stable_id()); }
                    true
                });
            }
            opaque |= matches!(statement, Statement::SetList(_) | Statement::NumForInit(_)
                | Statement::NumForNext(_) | Statement::GenericForInit(_) | Statement::GenericForNext(_)
                | Statement::Goto(_) | Statement::Label(_));
            let mut visit = |block: &Block| {
                let (count, hidden) = Self::find_duplicate_declarations(block, seen, duplicates);
                declarations += count;
                opaque |= hidden;
            };
            match statement {
                Statement::If(node) => {
                    visit(&node.then_block.lock());
                    visit(&node.else_block.lock());
                }
                Statement::While(node) => visit(&node.block.lock()),
                Statement::Repeat(node) => visit(&node.block.lock()),
                Statement::NumericFor(node) => visit(&node.block.lock()),
                Statement::GenericFor(node) => visit(&node.block.lock()),
                _ => {}
            }
        }
        (declarations, opaque)
    }

    fn initializer_shadows(assign: &crate::Assign) -> bool {
        let names: FxHashSet<_> = assign.left.iter().filter_map(|left| match left {
            LValue::Local(local) => local.0.lock().0.clone(), _ => None,
        }).collect();
        fn reads(value: &RValue, names: &FxHashSet<String>) -> bool {
            match value {
                RValue::Literal(Literal::Vector(..) | Literal::VectorD(..))
                    if names.contains("Vector3") || names.contains("vector") => true,
                RValue::Local(local) if local.0.lock().0.as_ref().is_some_and(|name| names.contains(name)) => true,
                RValue::Global(global) if std::str::from_utf8(&global.0).is_ok_and(|name| names.contains(name)) => true,
                _ => value.rvalues().into_iter().any(|child| reads(child, names)),
            }
        }
        assign.right.iter().any(|value| reads(value, &names))
    }

    fn function(&mut self, body: &mut Block, parameters: &[crate::RcLocal]) {
        let frame = crate::lower_conditionals::local_rewrite_frame_with_bound(
            body, parameters, 0, crate::materialize_vectors::register_bound);
        let outer_locals = std::mem::replace(&mut self.locals, frame.locals);
        let outer_site_locals = std::mem::take(&mut self.site_locals);
        let outer_site_plans = std::mem::take(&mut self.site_plans);
        let outer_reused_results = std::mem::take(&mut self.reused_results);
        let outer_duplicates = std::mem::take(&mut self.duplicate_declarations);
        let mut declarations = parameters.iter().map(crate::RcLocal::stable_id).collect();
        let (count, opaque) = Self::find_duplicate_declarations(body, &mut declarations, &mut self.duplicate_declarations);
        let outer_count = std::mem::replace(&mut self.declaration_count, (!opaque).then_some(count + parameters.len()));
        let parameters: FxHashSet<_> = parameters.iter().filter(|local| self.safety.never_reassigned(local))
            .map(crate::RcLocal::stable_id).collect();
        self.constants.extend(parameters.iter().copied());
        let outer_parameters = std::mem::replace(&mut self.parameters, parameters);
        let mut headroom = frame.headroom;
        let mut prefix = Vec::new();
        self.block(body, &mut prefix, &mut headroom, true);
        self.split_literal_declarations(body);
        body.0.splice(0..0, prefix);
        self.locals = outer_locals;
        self.parameters = outer_parameters;
        self.site_locals = outer_site_locals;
        self.site_plans = outer_site_plans;
        self.reused_results = outer_reused_results;
        self.duplicate_declarations = outer_duplicates;
        self.declaration_count = outer_count;
    }

    fn reuse_result(&mut self, assign: &mut crate::Assign, headroom: usize) {
        // shortcut: nested result reuse needs a call-site register bound; outermost 192-local calls fit their scratch.
        if headroom != 0 || !self.declaration_count.is_some_and(|count| count <= 192)
            || !assign.prefix || Self::initializer_shadows(assign) { return; }
        let [LValue::Local(result)] = &assign.left[..] else { return; };
        if self.captured.contains(&result.stable_id()) || !self.safety.never_reassigned(result)
            || self.duplicate_declarations.contains(&result.stable_id()) { return; }
        let [RValue::Call(call) | RValue::Select(Select::Call(call))] = &mut assign.right[..] else { return; };
        if !self.candidates.contains(&(call as *const Call as usize))
            || !matches!(call.builtin_fastcall, Some((40 | 44, 2)))
            || !matches!(&*call.value, RValue::Local(_) | RValue::Global(_)) { return; }
        let [RValue::Local(source)] = &mut call.arguments[..] else { return; };
        if !self.site_locals.contains(&source.stable_id()) || !self.captured.contains(&source.stable_id())
            || self.site_plans.contains_key(&source.stable_id()) { return; }
        self.reused_results.insert(result.stable_id(), source.clone());
        *source = result.clone();
        call.group_callee = false;
    }

    fn constant_expression(&self, value: &RValue) -> bool {
        match value {
            RValue::Literal(_) => true,
            RValue::Local(local) => self.constants.contains(&local.stable_id()),
            RValue::Unary(_) | RValue::Binary(_) | RValue::IfExpression(_) =>
                value.rvalues().into_iter().all(|child| self.constant_expression(child)),
            RValue::Index(index) => matches!((&*index.left, &*index.right),
                (RValue::Global(global), RValue::Literal(Literal::String(member)))
                if global.0 == b"math" && matches!(member.as_slice(),
                    b"pi" | b"huge" | b"nan" | b"e" | b"phi" | b"sqrt2" | b"tau")),
            _ => false,
        }
    }

    fn guard_leaf<'a>(&self, value: &'a mut RValue) -> Option<&'a mut RValue> {
        if matches!(value, RValue::Literal(Literal::Nil | Literal::Boolean(_)
            | Literal::Number(_) | Literal::Integer(_) | Literal::String(_)))
            || matches!(value, RValue::Local(local) if self.constants.contains(&local.stable_id())
                && (self.parameters.contains(&local.stable_id()) || self.splittable.contains(&local.stable_id())
                    || self.captured.contains(&local.stable_id()) && !self.locals.contains(&local.stable_id())))
        { return Some(value); }
        // Logical/conditional branches can make a protected operand irrelevant to folding.
        match value {
            RValue::Unary(unary) => self.guard_leaf(&mut unary.value),
            RValue::Binary(binary) if !matches!(binary.operation, crate::BinaryOperation::And | crate::BinaryOperation::Or) =>
                self.guard_leaf(&mut binary.left),
            _ => None,
        }
    }

    fn call(&mut self, call: &mut Call, prefix: &mut Vec<Statement>, headroom: &mut usize) {
        // These addresses were collected by the immediately preceding grouping walk;
        // bodies move only after all their candidate calls have been processed.
        if !self.candidates.contains(&(call as *const Call as usize)) { return; }
        // shortcut: extend beyond one-argument type predicates only with an ordered-argument proof.
        if matches!(call.builtin_fastcall, Some((40 | 44, 2)))
            && let [RValue::Local(local)] = &mut call.arguments[..]
            && self.site_locals.contains(&local.stable_id())
        {
            let id = local.stable_id();
            if !self.site_plans.contains_key(&id) {
                let snapshot = if self.captured.contains(&id) {
                    if *headroom == 0 { return; }
                    *headroom -= 1;
                    let snapshot = crate::rehoist_constants::unique_local("builtinArgument", &mut self.reserved);
                    self.pass.introduced_locals += 1;
                    self.declaration_count = self.declaration_count.map(|count| count + 1);
                    self.pass.ledger.record(&snapshot, crate::local_producers::Role::EvaluationSnapshot);
                    Some(snapshot)
                } else { None };
                self.site_plans.insert(id, snapshot);
            }
            if let Some(snapshot) = &self.site_plans[&id] { *local = snapshot.clone(); }
            call.group_callee = false;
            return;
        }
        if call.arguments.iter().any(|argument| matches!(argument, RValue::Local(local)
            if !self.safety.never_reassigned(local)))
        {
            call.group_callee = false;
            return;
        }
        let mut needs_compatibility = false;
        for argument in &call.arguments {
            argument.visit_local_reads(&mut |local| {
                let id = local.stable_id();
                self.protected.insert(id);
                needs_compatibility |= self.unsplittable.contains(&id)
                    || self.constants.contains(&id) && self.captured.contains(&id) && self.locals.contains(&id);
                true
            });
        }
        if !call.arguments.iter().all(|argument| self.constant_expression(argument)) { return; }
        if needs_compatibility {
            // Keep capture mutability and shadowed initializer scope unchanged.
            return;
        }
        let Some(leaf) = call.arguments.iter_mut().find_map(|argument| self.guard_leaf(argument)) else { return; };
        if let RValue::Local(local) = leaf && self.splittable.contains(&local.stable_id()) {
            self.protected.insert(local.stable_id());
            call.group_callee = false;
            return;
        }
        if *headroom == 0 {
            // shortcut: retain prior spelling when no mutable argument binding fits.
            return;
        }
        *headroom -= 1;
        let local = crate::rehoist_constants::unique_local("builtinArgument", &mut self.reserved);
        // Only inert literals or never-reassigned incoming captures move to entry.
        let literal = std::mem::replace(leaf, local.clone().into());
        call.group_callee = false;
        let mut declaration = crate::Assign::new(vec![local.clone().into()], vec![]);
        declaration.prefix = true;
        declaration.node_origin = crate::node_origins::Origin::synthesized("native_constant_arguments");
        let mut assign = crate::Assign::new(vec![local.clone().into()], vec![literal]);
        assign.node_origin = crate::node_origins::Origin::synthesized("native_constant_arguments");
        prefix.extend([declaration.into(), assign.into()]);
        self.pass.introduced_locals += 1;
        self.declaration_count = self.declaration_count.map(|count| count + 1);
        self.changed = true;
        self.pass.ledger.record(&local, crate::local_producers::Role::EvaluationSnapshot);
    }

    fn split_literal_declarations(&mut self, block: &mut Block) {
        let mut rewritten = Vec::with_capacity(block.0.len());
        for mut statement in std::mem::take(&mut block.0) {
            if let Statement::Assign(assign) = &mut statement && assign.prefix
                && let [LValue::Local(local)] = &assign.left[..]
                && let Some(source) = self.reused_results.remove(&local.stable_id())
            {
                // A later predicate may also request this result's split; the reuse already supplies it.
                self.site_plans.remove(&local.stable_id());
                let mut declaration = crate::Assign::new(assign.left.clone(), vec![]);
                declaration.prefix = true;
                declaration.node_origin = crate::node_origins::Origin::synthesized("native_constant_arguments");
                let mut copy = crate::Assign::new(assign.left.clone(), vec![source.into()]);
                copy.node_origin = crate::node_origins::Origin::synthesized("native_constant_arguments");
                assign.prefix = false;
                rewritten.extend([declaration.into(), copy.into(), statement]);
                self.changed = true;
                continue;
            }
            if let Statement::Assign(assign) = &mut statement && assign.prefix
                && let [LValue::Local(local)] = &assign.left[..]
                && let Some(snapshot) = self.site_plans.remove(&local.stable_id())
            {
                let local = local.clone();
                if let Some(snapshot) = snapshot {
                    // Keep captured declarations immutable; only this function reads the private copy.
                    rewritten.push(statement);
                    let mut declaration = crate::Assign::new(vec![snapshot.clone().into()], vec![]);
                    declaration.prefix = true;
                    declaration.node_origin = crate::node_origins::Origin::synthesized("native_constant_arguments");
                    let mut copy = crate::Assign::new(vec![snapshot.into()], vec![local.into()]);
                    copy.node_origin = crate::node_origins::Origin::synthesized("native_constant_arguments");
                    rewritten.extend([declaration.into(), copy.into()]);
                } else {
                    let mut declaration = crate::Assign::new(assign.left.clone(), vec![]);
                    declaration.prefix = true;
                    declaration.node_origin = crate::node_origins::Origin::synthesized("native_constant_arguments");
                    assign.prefix = false;
                    rewritten.extend([declaration.into(), statement]);
                }
                self.changed = true;
                continue;
            }
            if let Statement::Assign(assign) = &mut statement
                && assign.prefix && assign.left.len() == assign.right.len()
                && !Self::initializer_shadows(assign)
                && assign.right.iter().all(|value| self.constant_expression(value))
                && assign.left.iter().all(|left| matches!(left, LValue::Local(local)
                    if !self.captured.contains(&local.stable_id()) && !self.unsplittable.contains(&local.stable_id())))
                && assign.left.iter().any(|left| matches!(left, LValue::Local(local)
                    if self.protected.contains(&local.stable_id())))
            {
                // Keep uncaptured constant aliases mutable at their original site.
                let mut declaration = crate::Assign::new(assign.left.clone(), vec![]);
                declaration.prefix = true;
                declaration.node_origin = crate::node_origins::Origin::synthesized("native_constant_arguments");
                rewritten.push(declaration.into());
                assign.prefix = false;
                self.changed = true;
            }
            match &mut statement {
                Statement::If(node) => {
                    self.split_literal_declarations(&mut node.then_block.lock());
                    self.split_literal_declarations(&mut node.else_block.lock());
                }
                Statement::While(node) => self.split_literal_declarations(&mut node.block.lock()),
                Statement::Repeat(node) => self.split_literal_declarations(&mut node.block.lock()),
                Statement::NumericFor(node) => self.split_literal_declarations(&mut node.block.lock()),
                Statement::GenericFor(node) => self.split_literal_declarations(&mut node.block.lock()),
                _ => {}
            }
            rewritten.push(statement);
        }
        block.0 = rewritten;
    }

    fn value(&mut self, value: &mut RValue, prefix: &mut Vec<Statement>, headroom: &mut usize) {
        if let RValue::Closure(closure) = value {
            if self.visited.insert((&*closure.function.0 as *const _) as usize) {
                let mut function = closure.function.lock();
                let parameters = function.parameters.clone();
                self.function(&mut function.body, &parameters);
            }
            return;
        }
        if let RValue::Call(call) | RValue::Select(Select::Call(call)) = value { self.call(call, prefix, headroom); }
        value.visit_rvalues_mut(&mut |child| { self.value(child, prefix, headroom); true });
    }

    fn block(&mut self, block: &mut Block, prefix: &mut Vec<Statement>, headroom: &mut usize, outermost: bool) {
        let outer_site_locals = self.site_locals.clone();
        for statement in &mut block.0 {
            let mut site_local = None;
            if let Statement::Assign(assign) = &*statement && assign.prefix {
                let shadows = Self::initializer_shadows(assign);
                if !shadows && let [LValue::Local(local)] = &assign.left[..]
                    && let [value] = &assign.right[..] && Self::site_initializer(value)
                    && self.safety.never_reassigned(local)
                    && !self.duplicate_declarations.contains(&local.stable_id())
                { site_local = Some(local.stable_id()); }
                let can_split = !shadows && assign.left.len() == assign.right.len()
                    && assign.right.iter().all(|value| self.constant_expression(value))
                    && assign.left.iter().all(|left| matches!(left, LValue::Local(local)
                        if !self.captured.contains(&local.stable_id())));
                for (left, right) in assign.left.iter().zip(&assign.right) {
                    if let LValue::Local(local) = left && self.safety.never_reassigned(local)
                        && self.constant_expression(right)
                    {
                        self.constants.insert(local.stable_id());
                        if can_split { self.splittable.insert(local.stable_id()); }
                        if shadows && !self.captured.contains(&local.stable_id()) { self.unsplittable.insert(local.stable_id()); }
                    }
                }
            }
            if outermost && let Statement::Assign(assign) = statement { self.reuse_result(assign, *headroom); }
            if let Statement::Call(call) = statement { self.call(call, prefix, headroom); }
            statement.visit_lvalues_mut(&mut |left| {
                left.visit_rvalues_mut(&mut |value| { self.value(value, prefix, headroom); true }); true
            });
            statement.visit_rvalues_mut(&mut |value| { self.value(value, prefix, headroom); true });
            if let Some(id) = site_local { self.site_locals.insert(id); }
            match statement {
                Statement::If(node) => {
                    self.block(&mut node.then_block.lock(), prefix, headroom, false);
                    self.block(&mut node.else_block.lock(), prefix, headroom, false);
                }
                Statement::While(node) => self.block(&mut node.block.lock(), prefix, headroom, false),
                Statement::Repeat(node) => self.block(&mut node.block.lock(), prefix, headroom, false),
                Statement::NumericFor(node) => self.block(&mut node.block.lock(), prefix, headroom, false),
                Statement::GenericFor(node) => self.block(&mut node.block.lock(), prefix, headroom, false),
                _ => {}
            }
        }
        self.site_locals = outer_site_locals;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Builtin { Library, Function }

struct State<'a> {
    safety: &'a CaptureSafety,
    aliases: FxHashMap<u64, Builtin>,
    visited: FxHashSet<usize>,
    changed: usize,
    assume_standard_libraries: bool,
    native_candidates: FxHashSet<usize>,
    failure: Option<&'static str>,
}

impl State<'_> {
    fn builtin(&self, value: &RValue) -> Option<Builtin> {
        match value {
            RValue::Global(global) => match global.0.as_slice() {
                b"type" | b"typeof" => Some(Builtin::Function),
                b"math" | b"bit32" | b"string" | b"vector" | b"Vector3" => Some(Builtin::Library),
                _ => None,
            },
            RValue::Local(local) => self.aliases.get(&local.stable_id()).copied(),
            RValue::Index(index) if matches!(&*index.right, RValue::Literal(Literal::String(_))) => match self.builtin(&index.left) {
                Some(Builtin::Library) => Some(Builtin::Function),
                _ => None,
            },
            // Luau also recognizes `local math = math or replacement` as a library alias.
            RValue::Binary(binary) if binary.operation == crate::BinaryOperation::Or
                && self.builtin(&binary.left) == Some(Builtin::Library) => Some(Builtin::Library),
            _ => None,
        }
    }

    fn call(&mut self, call: &mut Call) {
        // Keep the existing configured-vector-constructor policy.
        if self.failure.is_some() || call.group_callee || matches!(call.builtin_fastcall, Some((54, _)))
        { return; }
        if matches!(call.builtin_fastcall, Some((_, 2))) {
            self.native_candidates.insert(call as *const Call as usize);
        }
        if self.builtin(&call.value) != Some(Builtin::Function) { return; }
        let mut group = true;
        let mut pure = true;
        for argument in &call.arguments {
            if !self.assume_standard_libraries && matches!(argument, RValue::Literal(Literal::Number(_))) { continue; }
            let summary = effects::summarize(argument, &|local|
                !self.safety.uncaptured(local) && !self.safety.never_reassigned(local));
            if summary.exhausted {
                self.failure = Some("builtin call timing exceeds the operand analysis budget");
                return;
            }
            pure &= summary.effects.is_total_pure();
            group &= matches!(&*call.value, RValue::Local(_)) || !summary.effects.has_order_dependency();
        }
        if !pure {
            let RValue::Local(callee) = &*call.value else { return };
            if !self.safety.complete() {
                self.failure = Some("builtin call timing exceeds the capture analysis budget");
                return;
            }
            if !self.safety.uncaptured(callee) && !self.safety.never_reassigned(callee) { return; }
            let mut later_effect = false;
            for argument in call.arguments.iter().rev() {
                if later_effect && let RValue::Local(local) = argument
                    && !self.safety.uncaptured(local) && !self.safety.never_reassigned(local) { return; }
                later_effect |= effects::may_write_capture(argument);
            }
            // The callee snapshot is fixed; grouping preserves argument order unless FASTCALL borrowed a mutable early argument.
            group = true;
        }
        if group {
            call.group_callee = true;
            self.changed += 1;
        } else if !self.safety.complete() {
            self.failure = Some("builtin call timing exceeds the capture analysis budget");
        }
    }

    fn value(&mut self, value: &mut RValue) {
        if self.failure.is_some() { return; }
        if let RValue::Closure(closure) = value {
            if self.visited.insert((&*closure.function.0 as *const _) as usize) {
                self.block(&mut closure.function.lock().body);
            }
            return;
        }
        if let RValue::Call(call) | RValue::Select(Select::Call(call)) = value { self.call(call); }
        value.visit_rvalues_mut(&mut |child| { self.value(child); true });
    }

    fn block(&mut self, block: &mut Block) {
        for statement in &mut block.0 {
            if self.failure.is_some() { return; }
            if let Statement::Assign(assign) = &*statement {
                for (left, right) in assign.left.iter().zip(&assign.right) {
                    if let LValue::Local(local) = left && let Some(builtin) = self.builtin(right) {
                        self.aliases.insert(local.stable_id(), builtin);
                    }
                }
            }
            if let Statement::Call(call) = statement { self.call(call); }
            statement.visit_lvalues_mut(&mut |left| {
                left.visit_rvalues_mut(&mut |value| { self.value(value); true }); true
            });
            statement.visit_rvalues_mut(&mut |value| { self.value(value); true });
            match statement {
                Statement::If(node) => {
                    self.block(&mut node.then_block.lock());
                    self.block(&mut node.else_block.lock());
                }
                Statement::While(node) => self.block(&mut node.block.lock()),
                Statement::Repeat(node) => self.block(&mut node.block.lock()),
                Statement::NumericFor(node) => self.block(&mut node.block.lock()),
                Statement::GenericFor(node) => self.block(&mut node.block.lock()),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Binary, BinaryOperation, Closure, Global, Index, Local, RcLocal, Return, Table, Unary, UnaryOperation, Upvalue};

    fn protect_native_constant_arguments(body: &mut Block) -> Result<Option<crate::local_producers::Pass>, &'static str> {
        preserve_native_builtin_calls(body, false)
    }

    fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
    fn number(n: f64) -> RValue { Literal::Number(n).into() }
    fn maximum() -> RValue {
        Index::new(Global::from("math").into(), Literal::String(b"max".to_vec()).into()).into()
    }
    fn declaration(local: &RcLocal, value: RValue) -> Statement {
        let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    #[test]
    fn groups_direct_alias_and_named_constant_calls_without_grouping_other_helpers() {
        let first = local("first");
        let maximum_alias = local("maximum");
        let library = local("library");
        let mut body = Block(vec![
            declaration(&first, number(1.0)), declaration(&maximum_alias, maximum()),
            declaration(&library, Global::from("math").into()),
            Return::new(vec![
                Call::new(maximum(), vec![number(1.0), number(2.0)]).into(),
                Call::new(maximum_alias.into(), vec![first.into(), number(2.0)]).into(),
                Call::new(Index::new(library.into(), Literal::String(b"max".to_vec()).into()).into(), vec![number(1.0), number(2.0)]).into(),
                Call::new(Global::from("print").into(), vec![number(1.0)]).into(),
                Call::new(local("helper").into(), vec![number(1.0)]).into(),
            ]).into(),
        ]);
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(3));
        let output = body.to_string();
        assert!(output.contains("(math.max)(1, 2)"), "{output}");
        assert!(output.contains("(maximum)(first, 2)"), "{output}");
        assert!(output.contains("(library.max)(1, 2)"), "{output}");
        assert!(output.contains("print(1)"), "{output}");
        assert!(output.contains("helper(1)"), "{output}");
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(0));
    }

    #[test]
    fn does_not_move_captured_reads_or_observable_arguments_before_the_callee() {
        let cell = local("cell");
        let maximum_alias = local("maximum");
        let writer = Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(cell.clone())] };
        writer.function.lock().body.0.push(Assign::new(vec![cell.clone().into()], vec![number(20.0)]).into());
        let mut body = Block(vec![declaration(&maximum_alias, maximum()), Return::new(vec![
            Call::new(maximum(), vec![cell.clone().into(), number(2.0)]).into(),
            Call::new(maximum(), vec![Call::new(Global::from("change").into(), vec![]).into(), number(2.0)]).into(),
            Call::new(maximum(), vec![Global::from("value").into(), number(2.0)]).into(),
            writer.into(),
        ]).into()]);
        let before = body.to_string();
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(0));
        assert_eq!(body.to_string(), before);
        body.0.push(Return::new(vec![Call::new(maximum_alias.into(), vec![cell.into(), number(2.0)]).into()]).into());
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1));
        assert!(body.to_string().ends_with("return (maximum)(cell, 2)"));
    }

    #[test]
    fn readonly_captures_allow_grouping_without_changing_their_read_value() {
        let cell = local("cell");
        let maximum_alias = local("maximum");
        let mut body = Block(vec![declaration(&cell, number(1.0)), declaration(&maximum_alias, maximum()), Return::new(vec![
            Call::new(maximum(), vec![cell.clone().into(), number(2.0)]).into(),
            Call::new(maximum_alias.into(), vec![cell.clone().into(), number(2.0)]).into(),
            Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(cell)] }.into(),
        ]).into()]);
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(2));
        assert!(body.to_string().contains("(maximum)(cell, 2)"));
    }

    #[test]
    fn exact_numeric_literals_respect_later_library_spelling() {
        for assume_standard_libraries in [false, true] {
            let mut body = Block(vec![Return::new([std::f64::consts::PI, f64::INFINITY, f64::NAN]
                .into_iter().map(|n| Call::new(maximum(), vec![number(n), number(2.0)]).into()).collect()).into()]);
            assert_eq!(preserve_builtin_calls(&mut body, assume_standard_libraries), Ok(if assume_standard_libraries { 0 } else { 3 }));
        }
    }

    #[test]
    fn saved_callees_preserve_arithmetic_string_and_effectful_arguments() {
        for shape in 0..6 {
            let first = local("first");
            let maximum_alias = local("maximum");
            let argument = match shape {
                0 => Unary::new(first.clone().into(), UnaryOperation::Negate).into(),
                1 => Binary::new(first.clone().into(), number(1.0), BinaryOperation::Add).into(),
                2 => Binary::new(number(1.0), number(0.0), BinaryOperation::Div).into(),
                3 => Binary::new(Literal::String(b"a".to_vec()).into(), Literal::String(b"b".to_vec()).into(), BinaryOperation::Concat).into(),
                4 => Unary::new(Literal::String(b"abc".to_vec()).into(), UnaryOperation::Length).into(),
                _ => Select::Call(Call::new(Global::from("change").into(), vec![])).into(),
            };
            let mut call = Call::new(maximum_alias.clone().into(), vec![argument, number(2.0)]);
            call.callee_after_arguments = true;
            let mut body = Block(vec![declaration(&first, number(1.0)), declaration(&maximum_alias, maximum()),
                Return::new(vec![call.into()]).into()]);
            assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1), "shape {shape}");
            assert!(body.to_string().contains("return (maximum)("));
        }
    }

    #[test]
    fn saved_callees_still_reject_argument_effects_that_change_borrowed_reads() {
        for changes_callee in [false, true] {
            let first = local("first");
            let maximum_alias = local("maximum");
            let changed = if changes_callee { maximum_alias.clone() } else { first.clone() };
            let writer = Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(changed.clone())] };
            writer.function.lock().body.0.push(Assign::new(vec![changed.into()], vec![Literal::Nil.into()]).into());
            let arguments = vec![first.clone().into(), Select::Call(Call::new(Global::from("change").into(), vec![])).into()];
            let mut call = Call::new(maximum_alias.clone().into(), arguments);
            call.callee_after_arguments = true;
            let mut body = Block(vec![declaration(&first, number(1.0)), declaration(&maximum_alias, maximum()),
                Return::new(vec![call.into(), writer.into()]).into()]);
            assert_eq!(preserve_builtin_calls(&mut body, false), Ok(0));
        }
    }

    #[test]
    fn configured_vector_fastcalls_keep_their_native_path_through_aliases() {
        for (fast, name) in [(false, "new"), (true, "new"), (true, "customConstructor")] {
            let library = local("library");
            let constructor = local("constructor");
            let index = |value| Index::new(value, Literal::String(name.as_bytes().to_vec()).into()).into();
            let make = |value| {
                let mut call = Call::new(value, vec![number(1.0), number(2.0), number(3.0)]);
                call.callee_after_arguments = fast;
                call.builtin_fastcall = fast.then_some((54, 2));
                let copied = call.with_parts((*call.value).clone(), call.arguments.clone());
                assert_eq!(copied.builtin_fastcall, fast.then_some((54, 2)));
                assert_eq!(copied, call);
                if fast {
                    let mut ordinary = call.clone();
                    ordinary.builtin_fastcall = None;
                    assert_ne!(ordinary, call);
                    assert!(format!("{call:?}").contains("builtin_fastcall: (54, 2)"));
                }
                call.into()
            };
            let mut body = Block(vec![declaration(&library, Global::from("Vector3").into()),
                declaration(&constructor, index(library.into())),
                Return::new(vec![make(index(Global::from("Vector3").into())), make(constructor.into())]).into()]);
            assert_eq!(preserve_builtin_calls(&mut body, false), Ok(if fast { 0 } else { 2 }));
        }
    }

    #[test]
    fn standard_vector_member_fastcalls_keep_a_guarded_native_path() {
        for (member, builtin) in [("magnitude", 78), ("normalize", 79), ("floor", 82)] {
          for native in [false, true] {
            let parameter = local("value");
            let path = Index::new(Global::from("vector").into(), Literal::String(member.as_bytes().to_vec()).into()).into();
            let mut call = Call::new(path, vec![parameter.clone().into()]);
            call.callee_after_arguments = true;
            call.builtin_fastcall = native.then_some((builtin, 2));
            let closure = Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![] };
            closure.function.lock().parameters = vec![parameter];
            closure.function.lock().body = Block(vec![Return::new(vec![call.into()]).into()]);
            let mut body = Block(vec![Return::new(vec![closure.into()]).into()]);
            assert_eq!(protect_native_constant_arguments(&mut body).unwrap().is_some(), native);
            let output = body.to_string();
            assert!(output.contains(&if native { format!("return vector.{member}(builtinArgument)") }
                else { format!("return (vector.{member})(value)") }), "{output}");
          }
        }
    }

    #[test]
    fn native_builtin_identity_survives_copying_and_stays_distinct_from_fallback() {
        for (name, builtin) in [("type", 40), ("typeof", 44)] {
            for native in [false, true] {
                let mut call = Call::new(Global::from(name).into(), vec![local("value").into()]);
                call.callee_after_arguments = true;
                call.builtin_fastcall = native.then_some((builtin, 2));
                let copied = call.with_parts((*call.value).clone(), call.arguments.clone());
                assert_eq!(copied, call);
                let mut other = call.clone();
                other.builtin_fastcall = Some((builtin + 1, 2));
                assert_ne!(other, call);
                other.builtin_fastcall = Some((builtin, 0));
                assert_ne!(other, call);
                let mut body = Block(vec![Return::new(vec![copied.into()]).into()]);
                assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1));
                assert_eq!(body.to_string(), format!("return ({name})(value)"));
            }
        }
    }

    #[test]
    fn only_scalar_native_calls_change_the_existing_result_arity_policy() {
        for results in [0, 1, 2, 3] {
            let mut call = Call::new(Global::from("typeof").into(), vec![number(1.0)]);
            call.builtin_fastcall = Some((44, results));
            let mut body = Block(vec![Return::new(vec![call.into()]).into()]);
            assert_eq!(protect_native_constant_arguments(&mut body).unwrap().is_some(), results == 2);
            assert!(body.to_string().contains(if results == 2 { "return typeof(builtinArgument)" }
                else { "return (typeof)(1)" }));
        }
    }

    #[test]
    fn scalar_native_literals_use_a_mutable_local_in_their_own_function() {
        let mut native = Call::new(Global::from("type").into(), vec![Literal::Boolean(true).into()]);
        native.builtin_fastcall = Some((40, 2));
        let closure = Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![] };
        closure.function.lock().body = Block(vec![Return::new(vec![native.into()]).into()]);
        let mut body = Block(vec![Return::new(vec![closure.into()]).into()]);
        let pass = protect_native_constant_arguments(&mut body).unwrap().unwrap();
        assert_eq!(pass.introduced_locals, 1);
        assert_eq!(pass.ledger.records.len(), 1);
        assert_eq!(body.0.len(), 1, "the inner argument must not become an upvalue");
        let output = body.to_string();
        assert!(output.contains("local builtinArgument\n"), "{output}");
        assert!(output.contains("builtinArgument = true"), "{output}");
        assert!(output.contains("return type(builtinArgument)"), "{output}");
        assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
        assert_eq!(body.to_string(), output);
    }

    #[test]
    fn captured_literal_aliases_keep_mutability_and_use_a_private_child_snapshot() {
        for literal in [Literal::Number(-1.0), Literal::Integer(i64::MAX), Literal::Vector(3.0, 4.0, 0.0)] {
            let value = local("value");
            let mut native = Call::new(Global::from("type").into(), vec![value.clone().into()]);
            native.builtin_fastcall = Some((40, 2));
            let closure = Closure { node_origin: Default::default(), function: Default::default(),
                upvalues: vec![Upvalue::Copy(value.clone())] };
            closure.function.lock().body = Block(vec![Return::new(vec![native.into()]).into()]);
            let mut body = Block(vec![Call::new(Global::from("effect").into(), vec![]).into(),
                declaration(&value, literal.into()), Return::new(vec![closure.into()]).into()]);
            let pass = protect_native_constant_arguments(&mut body).unwrap().unwrap();
            assert_eq!(pass.introduced_locals, 1);
            assert_eq!(pass.ledger.records.len(), 1);
            assert!(matches!(&body.0[0], Statement::Call(_)));
            let Statement::Assign(assign) = &body.0[1] else { panic!() };
            assert!(assign.prefix && matches!(assign.right[0], RValue::Literal(_)));
            assert_eq!(body.0.len(), 3);
            let Statement::Return(ret) = &body.0[2] else { panic!() };
            let RValue::Closure(closure) = &ret.values[0] else { panic!() };
            assert!(matches!(&closure.upvalues[..], [Upvalue::Copy(captured)] if captured == &value));
            assert!(closure.function.lock().body.to_string().contains("builtinArgument = value"));
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
        }
    }

    #[test]
    fn closed_expressions_keep_their_operations_at_the_call_site() {
        let expression = Binary::new(number(2.0), number(3.0), BinaryOperation::Add).into();
        let mut call = Call::new(maximum(), vec![expression, number(4.0)]);
        call.builtin_fastcall = Some((19, 2));
        let mut body = Block(vec![Call::new(Global::from("effect").into(), vec![]).into(),
            Return::new(vec![call.into()]).into()]);
        assert_eq!(protect_native_constant_arguments(&mut body).unwrap().unwrap().introduced_locals, 1);
        let output = body.to_string();
        assert!(output.contains("builtinArgument = 2"), "{output}");
        assert!(output.contains("effect()\nreturn math.max(builtinArgument + 3, 4)"), "{output}");
    }

    #[test]
    fn shadowed_constant_initializers_keep_their_original_scope() {
        for global in [false, true] {
            let outer = local("value");
            let inner = local(if global { "math" } else { "value" });
            let value = if global { Index::new(Global::from("math").into(),
                Literal::String(b"pi".to_vec()).into()).into() } else { outer.clone().into() };
            let mut call = Call::new(Global::from("type").into(), vec![inner.clone().into()]);
            call.builtin_fastcall = Some((40, 2));
            let mut body = Block(vec![declaration(&outer, number(7.0)), declaration(&inner, value),
                Return::new(vec![call.into()]).into()]);
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
            assert_eq!(body.0.len(), 3);
            let Statement::Assign(assign) = &body.0[1] else { panic!() };
            assert!(assign.prefix && !assign.right.is_empty());
            assert!(body.to_string().contains("return (type)("));
        }
    }

    #[test]
    fn rebound_shadowed_initializers_are_not_split() {
        let outer = local("value");
        let inner = local("value");
        let mut call = Call::new(maximum(), vec![inner.clone().into(), number(1.0)]);
        call.builtin_fastcall = Some((19, 2));
        let mut body = Block(vec![declaration(&outer, number(7.0)), declaration(&inner, outer.into()),
            crate::If::new(Global::from("flag").into(), Block(vec![Assign::new(vec![inner.into()],
                vec![number(9.0)]).into()]), Block(vec![])).into(),
            Return::new(vec![call.into()]).into()]);
        assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
        assert_eq!(body.0.len(), 4);
        let Statement::Assign(assign) = &body.0[1] else { panic!() };
        assert!(assign.prefix && !assign.right.is_empty());
    }

    #[test]
    fn nested_vector_literals_retain_the_existing_grouping_policy() {
        let callee = local("kind");
        let expression = Binary::new(Literal::Vector(1.0, 2.0, 3.0).into(),
            Literal::Vector(4.0, 5.0, 6.0).into(), BinaryOperation::Add).into();
        let mut call = Call::new(callee.clone().into(), vec![expression]);
        call.builtin_fastcall = Some((40, 2));
        let mut body = Block(vec![declaration(&callee, Global::from("type").into()),
            Return::new(vec![call.into()]).into()]);
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1));
        assert!(body.to_string().contains("return (kind)("));
    }

    #[test]
    fn late_vector_spelling_does_not_shadow_its_constructor() {
        for name in ["vector", "Vector3"] {
            let value = local(name);
            let mut call = Call::new(Global::from("type").into(), vec![value.clone().into()]);
            call.builtin_fastcall = Some((40, 2));
            let mut body = Block(vec![declaration(&value, Literal::Vector(1.0, 2.0, 3.0).into()),
                Return::new(vec![call.into()]).into()]);
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
            let Statement::Assign(assign) = &body.0[0] else { panic!() };
            assert!(assign.prefix && !assign.right.is_empty());
        }
    }

    #[test]
    fn explicit_grouping_and_unsupported_operands_are_never_relaxed() {
        for explicit in [false, true] {
            let mut call = Call::new(maximum(), vec![if explicit { number(1.0) }
                else { Index::new(Global::from("math").into(), Literal::String(b"pi".to_vec()).into()).into() }]);
            call.builtin_fastcall = Some((19, 2));
            call.group_callee = explicit;
            let mut body = Block(vec![Return::new(vec![call.into()]).into()]);
            let mut expected = body.clone();
            preserve_builtin_calls(&mut expected, false).unwrap();
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
            assert_eq!(body.to_string(), expected.to_string());
        }
    }

    #[test]
    fn empty_native_calls_keep_the_existing_compatibility_spelling() {
        let mut call = Call::new(Index::new(Global::from("string").into(),
            Literal::String(b"char".to_vec()).into()).into(), vec![]);
        call.builtin_fastcall = Some((42, 2));
        let mut body = Block(vec![Return::new(vec![call.into()]).into()]);
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1));
        assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
        assert_eq!(body.to_string(), "return (string.char)()");
    }

    #[test]
    fn native_argument_probe_validates_depth_before_recursing() {
        let mut value = number(1.0);
        for _ in 0..140 { value = Unary::new(value, UnaryOperation::Not).into(); }
        let mut body = Block(vec![Return::new(vec![value]).into()]);
        assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
    }

    #[test]
    fn ordinary_open_dynamic_and_vector_constructor_calls_do_not_gain_literal_bindings() {
        for (provenance, argument) in [
            (None, number(1.0)), (Some((40, 0)), number(1.0)),
            (Some((54, 2)), number(1.0)), (Some((40, 2)), local("value").into()),
            (Some((40, 2)), Call::new(Global::from("effect").into(), vec![]).into()),
        ] {
            let mut call = Call::new(Global::from("type").into(), vec![argument]);
            call.builtin_fastcall = provenance;
            let mut body = Block(vec![Return::new(vec![call.into()]).into()]);
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
            assert!(!body.to_string().contains("builtinArgument"));
        }
    }

    #[test]
    fn native_literal_bindings_respect_the_existing_local_budget() {
        let mut body = Block((0..200).map(|i| declaration(&local(&format!("v{i}")), number(i as f64))).collect());
        let mut call = Call::new(Global::from("type").into(), vec![number(1.0)]);
        call.builtin_fastcall = Some((40, 2));
        body.0.push(Return::new(vec![call.into()]).into());
        assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
        assert!(body.to_string().contains("return (type)(1)"));
    }

    fn native_kind(value: &RcLocal) -> Call {
        let mut call = Call::new(Global::from("typeof").into(), vec![value.clone().into()]);
        call.builtin_fastcall = Some((44, 2));
        call
    }

    fn captured_reader(value: &RcLocal) -> Closure {
        let closure = Closure { node_origin: Default::default(), function: Default::default(),
            upvalues: vec![Upvalue::Copy(value.clone())] };
        closure.function.lock().body = Block(vec![Return::new(vec![value.clone().into()]).into()]);
        closure
    }

    #[test]
    fn native_site_barriers_keep_scalar_initializers_after_prior_effects() {
        let source = local("source");
        for rhs in [
            RValue::Index(Index::new(source.clone().into(), Literal::String(b"value".to_vec()).into())),
            Index::new(source.clone().into(), local("key").into()).into(),
            crate::VarArg.into(), Select::VarArg(crate::VarArg).into(), source.clone().into(),
            Call::new(source.clone().into(), vec![number(1.0)]).into(),
            Select::Call(Call::new(source.clone().into(), vec![number(1.0)])).into(),
        ] {
            let value = local("value");
            let mut body = Block(vec![Call::new(Global::from("effect").into(), vec![]).into(),
                declaration(&value, rhs.clone()), Return::new(vec![native_kind(&value).into()]).into()]);
            let pass = protect_native_constant_arguments(&mut body).unwrap().unwrap();
            assert_eq!(pass.rewrite_model, "native_argument_barrier_v2");
            assert_eq!(pass.introduced_locals, 0);
            assert!(pass.ledger.records.is_empty());
            assert_eq!(body.0.len(), 4);
            assert!(matches!(&body.0[0], Statement::Call(_)));
            let Statement::Assign(declare) = &body.0[1] else { panic!() };
            assert!(declare.prefix && declare.right.is_empty());
            let Statement::Assign(assign) = &body.0[2] else { panic!() };
            assert!(!assign.prefix && assign.right == vec![rhs]);
            assert!(body.to_string().ends_with("return typeof(value)"));
            let output = body.to_string();
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
            assert_eq!(body.to_string(), output);
        }
    }

    #[test]
    fn captured_site_barriers_reuse_one_private_copy_without_changing_captures() {
        for rhs in [RValue::Index(Index::new(local("object").into(), local("key").into())),
            crate::VarArg.into(), local("source").into(),
            Call::new(local("callback").into(), vec![]).into()]
        {
            let value = local("value");
            let reader = captured_reader(&value);
            let mut body = Block(vec![declaration(&value, rhs.clone()),
                Return::new(vec![native_kind(&value).into(), native_kind(&value).into(), reader.clone().into()]).into()]);
            let pass = protect_native_constant_arguments(&mut body).unwrap().unwrap();
            assert_eq!(pass.introduced_locals, 1);
            assert_eq!(pass.ledger.records.len(), 1);
            assert_eq!(body.0.len(), 4);
            let Statement::Assign(assign) = &body.0[0] else { panic!() };
            assert!(assign.prefix && assign.right == vec![rhs]);
            let Statement::Assign(copy) = &body.0[2] else { panic!() };
            assert!(!copy.prefix && copy.right == vec![value.clone().into()]);
            assert!(matches!(&reader.upvalues[..], [Upvalue::Copy(local)] if local == &value));
            assert_eq!(reader.function.lock().body.to_string(), "return value");
            assert_eq!(body.to_string().matches("typeof(builtinArgument)").count(), 2);
        }
    }

    #[test]
    fn native_site_snapshots_are_not_captured_by_nested_functions() {
        let value = local("value");
        let child = captured_reader(&value);
        child.function.lock().body = Block(vec![Return::new(vec![native_kind(&value).into()]).into()]);
        let mut body = Block(vec![declaration(&value, Index::new(local("object").into(),
            Literal::String(b"value".to_vec()).into()).into()),
            Return::new(vec![native_kind(&value).into(), child.clone().into()]).into()]);
        assert_eq!(protect_native_constant_arguments(&mut body).unwrap().unwrap().introduced_locals, 1);
        assert_eq!(child.function.lock().body.to_string(), "return (typeof)(value)");
        assert!(matches!(&child.upvalues[..], [Upvalue::Copy(local)] if local == &value));
        assert!(body.to_string().contains("typeof(builtinArgument)"));
    }

    #[test]
    fn native_site_facts_require_a_unique_dominating_declaration_in_the_same_scope() {
        for shape in 0..4 {
            let value = local("value");
            let declare = || declaration(&value, Index::new(local("object").into(),
                Literal::String(b"value".to_vec()).into()).into());
            let ret = || Return::new(vec![native_kind(&value).into()]).into();
            let mut body = match shape {
                0 => Block(vec![ret(), declare()]),
                1 => Block(vec![declare(), declare(), ret()]),
                2 => Block(vec![crate::If::new(Global::from("flag").into(),
                    Block(vec![declare()]), Block(vec![ret()])).into()]),
                _ => Block(vec![crate::If::new(Global::from("flag").into(),
                    Block(vec![declare()]), Block(vec![])).into(), ret()]),
            };
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none(), "shape {shape}");
            assert!(body.to_string().contains("(typeof)(value)"));
        }
        let value = local("value");
        let mut body = Block(vec![declaration(&value, crate::VarArg.into()),
            crate::If::new(Global::from("flag").into(),
                Block(vec![Return::new(vec![native_kind(&value).into()]).into()]), Block(vec![])).into()]);
        assert_eq!(protect_native_constant_arguments(&mut body).unwrap().unwrap().introduced_locals, 0);
        assert!(body.to_string().contains("typeof(value)"));
    }

    #[test]
    fn native_site_initializers_keep_shadowed_names_and_nested_closures_unchanged() {
        for shape in 0..4 {
            let value = local("value");
            let outer = local("value");
            let rhs = match shape {
                0 => outer.clone().into(),
                1 => Index::new(outer.clone().into(), Literal::String(b"key".to_vec()).into()).into(),
                2 => Index::new(local("object").into(), outer.clone().into()).into(),
                _ => Call::new(local("callback").into(), vec![captured_reader(&outer).into()]).into(),
            };
            let mut body = Block(vec![declaration(&value, rhs), Return::new(vec![native_kind(&value).into()]).into()]);
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none(), "shape {shape}");
            assert_eq!(body.0.len(), 2);
            let Statement::Assign(assign) = &body.0[0] else { panic!() };
            assert!(assign.prefix && !assign.right.is_empty());
            assert!(body.to_string().contains("return (typeof)(value)"));
        }
    }

    #[test]
    fn native_site_admission_keeps_the_single_argument_predicate_boundary() {
        for shape in 0..5 {
            let value = local("value");
            let mut call = native_kind(&value);
            match shape {
                0 => call.builtin_fastcall = None,
                1 => call.builtin_fastcall = Some((44, 0)),
                2 => call.group_callee = true,
                3 => call.arguments.push(number(1.0)),
                _ => call.builtin_fastcall = Some((2, 2)),
            }
            let mut body = Block(vec![declaration(&value, crate::VarArg.into()),
                Return::new(vec![call.into()]).into()]);
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none(), "shape {shape}");
            assert_eq!(body.0.len(), 2);
            assert!(body.to_string().contains("return (typeof)(value"));
        }
    }

    #[test]
    fn native_site_snapshot_budget_failure_keeps_original_declaration_and_grouping() {
        let value = local("value");
        let reader = captured_reader(&value);
        let mut body = Block((0..199).map(|i| declaration(&local(&format!("v{i}")), number(i as f64))).collect());
        body.0.push(declaration(&value, crate::VarArg.into()));
        body.0.push(Return::new(vec![native_kind(&value).into(), reader.into()]).into());
        assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
        assert_eq!(body.0.len(), 201);
        let Statement::Assign(assign) = &body.0[199] else { panic!() };
        assert!(assign.prefix && assign.right.len() == 1);
        assert!(body.to_string().contains("return (typeof)(value)"));
    }

    fn pressured_result_body(select: bool, chained: bool) -> (Block, RcLocal, RcLocal) {
        let value = local("value");
        let observed = local("observed");
        let call = native_kind(&value);
        let mut body = Block(vec![declaration(&value, Index::new(local("object").into(),
            Literal::String(b"field".to_vec()).into()).into()),
            declaration(&local("reader"), captured_reader(&value).into()),
            declaration(&observed, if select { Select::Call(call).into() } else { call.into() })]);
        if chained { body.0.push(declaration(&local("chained"), native_kind(&observed).into())); }
        body.0.push(Call::new(Global::from("consume").into(), vec![number(1.0); 240]).into());
        body.0.push(Return::new(vec![observed.clone().into()]).into());
        (body, value, observed)
    }

    #[test]
    fn exhausted_scratch_reuses_a_private_result_without_adding_locals() {
        for select in [false, true] {
            let (mut body, value, observed) = pressured_result_body(select, true);
            let pass = protect_native_constant_arguments(&mut body).unwrap().unwrap();
            assert_eq!(pass.introduced_locals, 0);
            assert!(pass.ledger.records.is_empty());
            let Statement::Assign(original) = &body.0[0] else { panic!() };
            assert!(original.prefix && matches!(&original.right[..], [RValue::Index(_)]));
            let Statement::Assign(declare) = &body.0[2] else { panic!() };
            assert!(declare.prefix && declare.left == vec![observed.clone().into()] && declare.right.is_empty());
            let Statement::Assign(copy) = &body.0[3] else { panic!() };
            assert!(!copy.prefix && copy.right == vec![value.into()]);
            let Statement::Assign(call_assign) = &body.0[4] else { panic!() };
            assert!(!call_assign.prefix);
            let [RValue::Call(call) | RValue::Select(Select::Call(call))] = &call_assign.right[..] else { panic!() };
            assert!(!call.group_callee && call.arguments == vec![observed.into()]);
            let output = body.to_string();
            assert_eq!(output.matches("local observed").count(), 1, "{output}");
            assert!(output.contains("local chained = typeof(observed)"), "{output}");
            assert!(!output.contains("builtinArgument"));
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none());
            assert_eq!(body.to_string(), output);
        }
    }

    #[test]
    fn result_reuse_preserves_captured_shadowed_and_explicitly_grouped_results() {
        for shape in 0..6 {
            let (mut body, value, observed) = pressured_result_body(false, false);
            match shape {
                0 => body.0.push(Return::new(vec![captured_reader(&observed).into()]).into()),
                1 => observed.0.lock().0 = Some("typeof".into()),
                2 => observed.0.lock().0 = Some("value".into()),
                3 => {
                    let Statement::Assign(assign) = &mut body.0[2] else { panic!() };
                    let RValue::Call(call) = &mut assign.right[0] else { panic!() };
                    call.group_callee = true;
                }
                4 => {
                    let Statement::Assign(assign) = &mut body.0[2] else { panic!() };
                    let RValue::Call(call) = &mut assign.right[0] else { panic!() };
                    call.value = Box::new(Index::new(local("library").into(),
                        Literal::String(b"kind".to_vec()).into()).into());
                }
                _ => {
                    let Statement::Assign(assign) = &mut body.0[2] else { panic!() };
                    assign.left.push(local("extra").into());
                }
            }
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none(), "shape {shape}");
            let Statement::Assign(assign) = &body.0[2] else { panic!() };
            let RValue::Call(call) = &assign.right[0] else { panic!() };
            assert!(assign.prefix && call.arguments == vec![value.into()]);
        }
    }

    #[test]
    fn result_reuse_rejects_nested_scopes_opaque_protocols_and_excess_declaration_occurrences() {
        for shape in 0..3 {
            let (mut body, _, _) = pressured_result_body(false, false);
            match shape {
                0 => {
                    let statement = body.0.remove(2);
                    body.0.insert(2, crate::NumericFor::new(number(1.0), number(1.0), number(1.0),
                        local("i"), Block(vec![statement])).into());
                }
                1 => body.0.insert(2, crate::Label("opaque".into()).into()),
                _ => {
                    let repeated = local("repeated");
                    body.0.splice(0..0, (0..190).map(|_| declaration(&repeated, Literal::Nil.into())));
                }
            }
            assert!(protect_native_constant_arguments(&mut body).unwrap().is_none(), "shape {shape}");
            assert!(body.to_string().contains("local observed = (typeof)(value)"));
        }
    }

    #[test]
    fn result_reuse_does_not_replace_available_private_snapshot_headroom() {
        let (mut body, _, _) = pressured_result_body(false, false);
        body.0.remove(3);
        let pass = protect_native_constant_arguments(&mut body).unwrap().unwrap();
        assert_eq!(pass.introduced_locals, 1);
        let output = body.to_string();
        assert!(output.contains("local observed = typeof(builtinArgument)"), "{output}");
        assert!(!output.contains("observed = value"));
    }

    #[test]
    fn large_literal_contents_do_not_hide_readonly_captured_arguments() {
        let cell = local("cell");
        let mut body = Block(vec![
            declaration(&local("payload"), Literal::String(vec![b'x'; 8 * 1024 * 1024 + 1]).into()),
            declaration(&cell, number(1.0)),
            Return::new(vec![Call::new(maximum(), vec![cell.clone().into(), number(2.0)]).into(),
                Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(cell)] }.into(),
            ]).into(),
        ]);
        assert_eq!(preserve_builtin_calls(&mut body, false), Ok(1));
    }

    #[test]
    fn exhausted_capture_facts_refuse_only_calls_that_need_them() {
        for shape in 0..4 {
            let cell = local("cell");
            let maximum_alias = local("maximum");
            let mut body = Block(vec![Statement::Empty(crate::Empty {}); 200_001]);
            body.0.push(declaration(&cell, number(1.0)));
            body.0.push(declaration(&maximum_alias, maximum()));
            let callee = if shape >= 2 { maximum_alias.into() } else { maximum() };
            let argument = match shape {
                1 => number(1.0),
                3 => Unary::new(cell.clone().into(), UnaryOperation::Negate).into(),
                _ => cell.clone().into(),
            };
            body.0.push(Return::new(vec![Call::new(callee, vec![argument, number(2.0)]).into(),
                Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![Upvalue::Ref(cell)] }.into(),
            ]).into());
            assert_eq!(preserve_builtin_calls(&mut body, false), if shape == 0 || shape == 3 {
                Err("builtin call timing exceeds the capture analysis budget")
            } else { Ok(1) });
        }
    }

    #[test]
    fn grouped_calls_keep_semantics_through_copying_and_statement_boundaries() {
        let mut call = Call::new(maximum(), vec![number(1.0), number(2.0)]);
        let original = call.clone();
        call.group_callee = true;
        assert_ne!(call, original);
        let copied = call.with_parts((*call.value).clone(), call.arguments.clone());
        assert_eq!(copied, call);
        assert!(format!("{call:?}").contains("group_callee: true"));
        let body = Block(vec![
            Call::new(Global::from("consume").into(), vec![]).into(), copied.clone().into(),
            Assign::new(vec![Index::new(Select::Call(copied).into(), number(1.0)).into()], vec![Table::default().into()]).into(),
        ]);
        let output = body.to_string();
        assert!(output.contains("consume();\n(math.max)(1, 2);\n(math.max)(1, 2)[1] = {}"), "{output}");
    }
}
