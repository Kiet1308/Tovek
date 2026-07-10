//! Recreate a small set of role-proven constants folded away by Luau `-O2`.
//!
//! The pass is intentionally conservative. A literal is hoisted only when it
//! occurs at least three times in one function scope and every counted use has
//! the same explicit API role (wait duration, magnitude threshold, or asset-id
//! property). It never guesses from raw frequency alone.

use itertools::Either;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    Assign, BinaryOperation, Block, Call, Global, Index, LValue, Literal, Local, RValue, RcLocal,
    Select, Statement, Traverse,
};

const MIN_OCCURRENCES: usize = 3;
const MAX_ACTIVE_LOCALS: usize = 200;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Role {
    WaitInterval,
    DelayDuration,
    Distance,
    SoundId,
    ImageId,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ValueKey {
    Number(u64),
    String(Vec<u8>),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CandidateKey {
    role: Role,
    value: ValueKey,
}

impl CandidateKey {
    fn new(role: Role, literal: &Literal) -> Option<Self> {
        let value = match literal {
            Literal::Number(value) if value.is_finite() => ValueKey::Number(value.to_bits()),
            Literal::String(value) if value.len() >= 4 => ValueKey::String(value.clone()),
            _ => return None,
        };
        Some(Self { role, value })
    }

    fn literal(&self) -> Literal {
        match &self.value {
            ValueKey::Number(bits) => Literal::Number(f64::from_bits(*bits)),
            ValueKey::String(value) => Literal::String(value.clone()),
        }
    }

    fn base_name(&self) -> &'static str {
        match self.role {
            Role::WaitInterval => "WAIT_INTERVAL",
            Role::DelayDuration => "DELAY_DURATION",
            Role::Distance
                if matches!(&self.value, ValueKey::Number(bits)
                    if f64::from_bits(*bits) > 0.0 && f64::from_bits(*bits) <= 0.01) =>
            {
                "DISTANCE_EPSILON"
            }
            Role::Distance => "DISTANCE_THRESHOLD",
            Role::SoundId => "SOUND_ID",
            Role::ImageId => "IMAGE_ID",
        }
    }

    fn order_key(&self) -> (Role, u8, Vec<u8>) {
        match &self.value {
            ValueKey::Number(bits) => (self.role, 0, bits.to_be_bytes().to_vec()),
            ValueKey::String(value) => (self.role, 1, value.clone()),
        }
    }
}

/// Hoist role-proven repeated literals in the module and every closure scope.
/// Returns the number of declarations inserted.
pub fn rehoist_constants(body: &mut Block) -> usize {
    crate::factor_common_tails::unshare_blocks(body);
    rehoist_scope_tree(body, 0, &[])
}

fn rehoist_scope_tree(
    body: &mut Block,
    parameter_count: usize,
    parameter_names: &[String],
) -> usize {
    let mut count = rehoist_one_scope(body, parameter_count, parameter_names);
    let mut functions = Vec::new();
    collect_nested_functions(&mut body.0, &mut functions);
    for function in functions {
        let mut function = function.0.lock();
        let parameter_count = function.parameters.len();
        let parameter_names = function
            .parameters
            .iter()
            .filter_map(local_name)
            .collect::<Vec<_>>();
        count += rehoist_scope_tree(&mut function.body, parameter_count, &parameter_names);
    }
    count
}

fn local_name(local: &RcLocal) -> Option<String> {
    local.0 .0.lock().0.clone()
}

fn global_name(global: &Global) -> Option<String> {
    std::str::from_utf8(&global.0).ok().map(str::to_string)
}

/// Reserve every identifier whose textual binding could change when a new local
/// is emitted at this scope's head. Descendant closures are included because an
/// outer declaration also shadows their global lookups after recompilation.
pub(crate) fn collect_reserved_identifiers(body: &mut Block, reserved: &mut FxHashSet<String>) {
    for statement in &mut body.0 {
        let mut functions = Vec::new();
        statement.post_traverse_values(&mut |value| -> Option<()> {
            match value {
                Either::Right(RValue::Local(local)) | Either::Left(LValue::Local(local)) => {
                    if let Some(name) = local_name(&local) {
                        reserved.insert(name);
                    }
                }
                Either::Right(RValue::Global(global)) | Either::Left(LValue::Global(global)) => {
                    if let Some(name) = global_name(&global) {
                        reserved.insert(name);
                    }
                }
                Either::Right(RValue::Closure(closure)) => {
                    functions.push(closure.function.clone());
                }
                _ => {}
            }
            None
        });
        for function in functions {
            let mut function = function.lock();
            reserved.extend(function.parameters.iter().filter_map(local_name));
            collect_reserved_identifiers(&mut function.body, reserved);
        }
        match statement {
            Statement::If(node) => {
                collect_reserved_identifiers(&mut node.then_block.lock(), reserved);
                collect_reserved_identifiers(&mut node.else_block.lock(), reserved);
            }
            Statement::While(node) => {
                collect_reserved_identifiers(&mut node.block.lock(), reserved)
            }
            Statement::Repeat(node) => {
                collect_reserved_identifiers(&mut node.block.lock(), reserved)
            }
            Statement::NumericFor(node) => {
                if let Some(name) = local_name(&node.counter) {
                    reserved.insert(name);
                }
                collect_reserved_identifiers(&mut node.block.lock(), reserved);
            }
            Statement::GenericFor(node) => {
                reserved.extend(node.res_locals.iter().filter_map(local_name));
                collect_reserved_identifiers(&mut node.block.lock(), reserved);
            }
            _ => {}
        }
    }
}

fn max_active_locals(stmts: &[Statement]) -> usize {
    let mut active = 0;
    let mut maximum = 0;
    for statement in stmts {
        if let Statement::Assign(assign) = statement
            && assign.prefix
        {
            active += assign
                .left
                .iter()
                .filter(|left| matches!(left, LValue::Local(_)))
                .count();
            maximum = maximum.max(active);
        }
        let nested = match statement {
            Statement::If(node) => max_active_locals(&node.then_block.lock().0)
                .max(max_active_locals(&node.else_block.lock().0)),
            Statement::While(node) => max_active_locals(&node.block.lock().0),
            Statement::Repeat(node) => max_active_locals(&node.block.lock().0),
            Statement::NumericFor(node) => 1 + max_active_locals(&node.block.lock().0),
            Statement::GenericFor(node) => {
                node.res_locals.len() + max_active_locals(&node.block.lock().0)
            }
            _ => 0,
        };
        maximum = maximum.max(active + nested);
    }
    maximum
}

fn collect_nested_functions(
    stmts: &mut [Statement],
    functions: &mut Vec<by_address::ByAddress<triomphe::Arc<parking_lot::Mutex<crate::Function>>>>,
) {
    for statement in stmts {
        for value in crate::deinline::stmt_rvalues_mut(statement) {
            collect_functions_in_rvalue(value, functions);
        }
        match statement {
            Statement::If(node) => {
                collect_nested_functions(&mut node.then_block.lock().0, functions);
                collect_nested_functions(&mut node.else_block.lock().0, functions);
            }
            Statement::While(node) => collect_nested_functions(&mut node.block.lock().0, functions),
            Statement::Repeat(node) => {
                collect_nested_functions(&mut node.block.lock().0, functions)
            }
            Statement::NumericFor(node) => {
                collect_nested_functions(&mut node.block.lock().0, functions)
            }
            Statement::GenericFor(node) => {
                collect_nested_functions(&mut node.block.lock().0, functions)
            }
            _ => {}
        }
    }
}

fn collect_functions_in_rvalue(
    value: &mut RValue,
    functions: &mut Vec<by_address::ByAddress<triomphe::Arc<parking_lot::Mutex<crate::Function>>>>,
) {
    if let RValue::Closure(closure) = value {
        functions.push(closure.function.clone());
        return;
    }
    for child in value.rvalues_mut() {
        collect_functions_in_rvalue(child, functions);
    }
}

fn rehoist_one_scope(
    body: &mut Block,
    parameter_count: usize,
    parameter_names: &[String],
) -> usize {
    let mut counts = FxHashMap::default();
    count_block(&body.0, &mut counts);
    let mut selected: Vec<CandidateKey> = counts
        .into_iter()
        .filter_map(|(candidate, count)| (count >= MIN_OCCURRENCES).then_some(candidate))
        .collect();
    selected.sort_by_key(CandidateKey::order_key);
    let active = parameter_count.saturating_add(max_active_locals(&body.0));
    selected.truncate(MAX_ACTIVE_LOCALS.saturating_sub(active));
    if selected.is_empty() {
        return 0;
    }

    let mut used_names: FxHashSet<String> = parameter_names.iter().cloned().collect();
    collect_reserved_identifiers(body, &mut used_names);

    let mut replacements = FxHashMap::default();
    let mut declarations = Vec::with_capacity(selected.len());
    for candidate in selected {
        let name = unique_name(candidate.base_name(), &mut used_names);
        let local = RcLocal::new(Local::new(Some(name)));
        declarations.push(Statement::Assign(Assign {
            left: vec![LValue::Local(local.clone())],
            right: vec![RValue::Literal(candidate.literal())],
            prefix: true,
            parallel: false,
        }));
        replacements.insert(candidate, local);
    }
    replace_block(&mut body.0, &replacements);
    let count = declarations.len();
    body.0.splice(0..0, declarations);
    count
}

pub(crate) fn unique_name(base: &str, used: &mut FxHashSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    for suffix in 2.. {
        let candidate = format!("{base}_{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!()
}

fn record(role: Role, value: &RValue, counts: &mut FxHashMap<CandidateKey, usize>) {
    if let RValue::Literal(literal) = value
        && let Some(candidate) = CandidateKey::new(role, literal)
    {
        *counts.entry(candidate).or_default() += 1;
    }
}

fn count_block(stmts: &[Statement], counts: &mut FxHashMap<CandidateKey, usize>) {
    for statement in stmts {
        if let Statement::Assign(assign) = statement {
            for (left, right) in assign.left.iter().zip(&assign.right) {
                if let Some(role) = property_role(left) {
                    record(role, right, counts);
                }
            }
        }
        if let Statement::Call(call) = statement {
            count_call(call, counts);
        }
        for value in crate::deinline::stmt_rvalues(statement) {
            count_rvalue(value, counts);
        }
        match statement {
            Statement::If(node) => {
                count_block(&node.then_block.lock().0, counts);
                count_block(&node.else_block.lock().0, counts);
            }
            Statement::While(node) => count_block(&node.block.lock().0, counts),
            Statement::Repeat(node) => count_block(&node.block.lock().0, counts),
            Statement::NumericFor(node) => count_block(&node.block.lock().0, counts),
            Statement::GenericFor(node) => count_block(&node.block.lock().0, counts),
            _ => {}
        }
    }
}

fn count_rvalue(value: &RValue, counts: &mut FxHashMap<CandidateKey, usize>) {
    match value {
        RValue::Closure(_) => return,
        RValue::Call(call) | RValue::Select(Select::Call(call)) => count_call(call, counts),
        RValue::Binary(binary) if is_relational(binary.operation) => {
            if is_magnitude(&binary.left) {
                record(Role::Distance, &binary.right, counts);
            }
            if is_magnitude(&binary.right) {
                record(Role::Distance, &binary.left, counts);
            }
        }
        _ => {}
    }
    for child in value.rvalues() {
        count_rvalue(child, counts);
    }
}

fn count_call(call: &Call, counts: &mut FxHashMap<CandidateKey, usize>) {
    if call_is(call, b"task", &["wait"])
        && let Some(duration) = call.arguments.first()
    {
        record(Role::WaitInterval, duration, counts);
    }
    if call_is(call, b"task", &["delay"])
        && let Some(duration) = call.arguments.first()
    {
        record(Role::DelayDuration, duration, counts);
    }
}

fn replace_block(stmts: &mut [Statement], replacements: &FxHashMap<CandidateKey, RcLocal>) {
    for statement in stmts {
        if let Statement::Assign(assign) = statement {
            for (left, right) in assign.left.iter().zip(&mut assign.right) {
                if let Some(role) = property_role(left) {
                    replace_role_literal(role, right, replacements);
                }
            }
        }
        if let Statement::Call(call) = statement {
            replace_call(call, replacements);
        }
        for value in crate::deinline::stmt_rvalues_mut(statement) {
            replace_rvalue(value, replacements);
        }
        match statement {
            Statement::If(node) => {
                replace_block(&mut node.then_block.lock().0, replacements);
                replace_block(&mut node.else_block.lock().0, replacements);
            }
            Statement::While(node) => replace_block(&mut node.block.lock().0, replacements),
            Statement::Repeat(node) => replace_block(&mut node.block.lock().0, replacements),
            Statement::NumericFor(node) => replace_block(&mut node.block.lock().0, replacements),
            Statement::GenericFor(node) => replace_block(&mut node.block.lock().0, replacements),
            _ => {}
        }
    }
}

fn replace_rvalue(value: &mut RValue, replacements: &FxHashMap<CandidateKey, RcLocal>) {
    match value {
        RValue::Closure(_) => return,
        RValue::Call(call) | RValue::Select(Select::Call(call)) => replace_call(call, replacements),
        RValue::Binary(binary) if is_relational(binary.operation) => {
            if is_magnitude(&binary.left) {
                replace_role_literal(Role::Distance, &mut binary.right, replacements);
            }
            if is_magnitude(&binary.right) {
                replace_role_literal(Role::Distance, &mut binary.left, replacements);
            }
        }
        _ => {}
    }
    for child in value.rvalues_mut() {
        replace_rvalue(child, replacements);
    }
}

fn replace_call(call: &mut Call, replacements: &FxHashMap<CandidateKey, RcLocal>) {
    if call_is(call, b"task", &["wait"])
        && let Some(duration) = call.arguments.first_mut()
    {
        replace_role_literal(Role::WaitInterval, duration, replacements);
    }
    if call_is(call, b"task", &["delay"])
        && let Some(duration) = call.arguments.first_mut()
    {
        replace_role_literal(Role::DelayDuration, duration, replacements);
    }
}

fn replace_role_literal(
    role: Role,
    value: &mut RValue,
    replacements: &FxHashMap<CandidateKey, RcLocal>,
) {
    let RValue::Literal(literal) = value else {
        return;
    };
    let Some(candidate) = CandidateKey::new(role, literal) else {
        return;
    };
    if let Some(local) = replacements.get(&candidate) {
        *value = RValue::Local(local.clone());
    }
}

fn property_role(value: &LValue) -> Option<Role> {
    let LValue::Index(index) = value else {
        return None;
    };
    match index_key(index)? {
        "SoundId" => Some(Role::SoundId),
        "Image" | "ImageId" | "Texture" | "TextureId" => Some(Role::ImageId),
        _ => None,
    }
}

fn is_relational(operation: BinaryOperation) -> bool {
    matches!(
        operation,
        BinaryOperation::LessThan
            | BinaryOperation::LessThanOrEqual
            | BinaryOperation::GreaterThan
            | BinaryOperation::GreaterThanOrEqual
    )
}

fn is_magnitude(value: &RValue) -> bool {
    matches!(value, RValue::Index(index) if index_key(index) == Some("Magnitude"))
}

fn index_key(index: &Index) -> Option<&str> {
    if let RValue::Literal(Literal::String(key)) = &*index.right {
        std::str::from_utf8(key).ok()
    } else {
        None
    }
}

fn call_is(call: &Call, namespace: &[u8], members: &[&str]) -> bool {
    let RValue::Index(index) = &*call.value else {
        return false;
    };
    matches!(&*index.left, RValue::Global(Global(name)) if name.as_slice() == namespace)
        && index_key(index).is_some_and(|member| members.contains(&member))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Binary, If, Return};

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::from(value))
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    fn wait(value: f64) -> Statement {
        Statement::Call(Call::new(
            RValue::Index(Index::new(global("task"), string("wait"))),
            vec![number(value)],
        ))
    }

    fn delay(value: f64) -> Statement {
        Statement::Call(Call::new(
            RValue::Index(Index::new(global("task"), string("delay"))),
            vec![number(value), global("callback")],
        ))
    }

    #[test]
    fn hoists_three_wait_intervals_across_nested_blocks() {
        let mut body = Block(vec![
            wait(10.0),
            Statement::If(If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block(vec![wait(10.0), wait(10.0)]),
                Block::default(),
            )),
        ]);

        assert_eq!(rehoist_constants(&mut body), 1);
        crate::name_locals::name_locals(&mut body, true);
        assert!(matches!(&body.0[0], Statement::Assign(assign)
            if matches!(assign.left.as_slice(), [LValue::Local(local)]
                if local.0.0.lock().0.as_deref() == Some("WAIT_INTERVAL"))));
    }

    #[test]
    fn two_occurrences_stay_inline() {
        let mut body = Block(vec![wait(10.0), wait(10.0)]);
        assert_eq!(rehoist_constants(&mut body), 0);
        assert!(matches!(body.0[0], Statement::Call(_)));
    }

    #[test]
    fn roles_do_not_cross_count() {
        let magnitude = || RValue::Index(Index::new(global("delta"), string("Magnitude")));
        let mut body = Block(vec![
            wait(10.0),
            wait(10.0),
            Statement::If(If::new(
                RValue::Binary(Binary::new(
                    magnitude(),
                    number(10.0),
                    BinaryOperation::LessThan,
                )),
                Block::default(),
                Block::default(),
            )),
        ]);
        assert_eq!(rehoist_constants(&mut body), 0);
    }

    #[test]
    fn wait_and_delay_durations_do_not_cross_count() {
        let mut body = Block(vec![wait(10.0), wait(10.0), delay(10.0)]);
        assert_eq!(rehoist_constants(&mut body), 0);
    }

    #[test]
    fn refuses_hoist_without_local_register_headroom() {
        let mut statements = Vec::new();
        for index in 0..MAX_ACTIVE_LOCALS {
            statements.push(Statement::Assign(Assign {
                left: vec![LValue::Local(RcLocal::new(Local::new(Some(format!(
                    "local{index}"
                )))))],
                right: vec![number(index as f64)],
                prefix: true,
                parallel: false,
            }));
        }
        statements.extend([wait(10.0), wait(10.0), wait(10.0)]);
        let mut body = Block(statements);
        assert_eq!(rehoist_constants(&mut body), 0);
        assert!(!body.to_string().contains("WAIT_INTERVAL"));
    }

    #[test]
    fn unnamed_parameters_still_consume_local_headroom() {
        let function = triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
            parameters: (0..MAX_ACTIVE_LOCALS).map(|_| RcLocal::default()).collect(),
            body: Block(vec![wait(10.0), wait(10.0), wait(10.0)]),
            ..crate::Function::default()
        }));
        let closure = RValue::Closure(crate::Closure {
            function: by_address::ByAddress(function.clone()),
            upvalues: Vec::new(),
        });
        let mut body = Block(vec![Statement::Return(Return::new(vec![closure]))]);
        assert_eq!(rehoist_constants(&mut body), 0);
        assert!(!function.lock().body.to_string().contains("WAIT_INTERVAL"));
    }

    #[test]
    fn ordinary_magnitude_value_uses_neutral_threshold_name() {
        let comparison = || {
            RValue::Binary(Binary::new(
                RValue::Index(Index::new(global("delta"), string("Magnitude"))),
                number(25.0),
                BinaryOperation::LessThan,
            ))
        };
        let mut body = Block(vec![
            Statement::If(If::new(comparison(), Block::default(), Block::default())),
            Statement::If(If::new(comparison(), Block::default(), Block::default())),
            Statement::If(If::new(comparison(), Block::default(), Block::default())),
        ]);
        assert_eq!(rehoist_constants(&mut body), 1);
        assert!(body.to_string().contains("DISTANCE_THRESHOLD"));
    }

    #[test]
    fn small_magnitude_threshold_is_named_epsilon() {
        let comparison = || {
            RValue::Binary(Binary::new(
                RValue::Index(Index::new(global("delta"), string("Magnitude"))),
                number(0.001),
                BinaryOperation::LessThan,
            ))
        };
        let mut body = Block(vec![
            Statement::If(If::new(comparison(), Block::default(), Block::default())),
            Statement::If(If::new(comparison(), Block::default(), Block::default())),
            Statement::If(If::new(comparison(), Block::default(), Block::default())),
        ]);
        assert_eq!(rehoist_constants(&mut body), 1);
        assert!(body.to_string().contains("DISTANCE_EPSILON"));
        assert!(!body.to_string().contains("DISTANCE_THRESHOLD"));
    }

    #[test]
    fn hoists_repeated_asset_property() {
        let assign = || {
            Statement::Assign(Assign::new(
                vec![LValue::Index(Index::new(
                    global("sound"),
                    string("SoundId"),
                ))],
                vec![string("rbxassetid://123")],
            ))
        };
        let mut body = Block(vec![assign(), assign(), assign()]);
        assert_eq!(rehoist_constants(&mut body), 1);
        assert!(body.to_string().contains("SOUND_ID"));
    }

    #[test]
    fn generated_constant_never_shadows_referenced_global() {
        let assign = || {
            Statement::Assign(Assign::new(
                vec![LValue::Index(Index::new(
                    global("sound"),
                    string("SoundId"),
                ))],
                vec![string("rbxassetid://123")],
            ))
        };
        let mut body = Block(vec![
            Statement::Call(Call::new(global("print"), vec![global("SOUND_ID")])),
            assign(),
            assign(),
            assign(),
        ]);
        assert_eq!(rehoist_constants(&mut body), 1);
        let output = body.to_string();
        assert!(output.contains("local SOUND_ID_2 ="), "{output}");
        assert!(output.contains("print(SOUND_ID)"), "{output}");
    }

    #[test]
    fn generated_constant_never_shadows_function_parameter() {
        let parameter = RcLocal::new(Local::new(Some("IMAGE_ID".to_string())));
        let assign = || {
            Statement::Assign(Assign::new(
                vec![LValue::Index(Index::new(global("image"), string("Image")))],
                vec![string("rbxassetid://456")],
            ))
        };
        let function = triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
            parameters: vec![parameter],
            body: Block(vec![assign(), assign(), assign()]),
            ..crate::Function::default()
        }));
        let closure = RValue::Closure(crate::Closure {
            function: by_address::ByAddress(function.clone()),
            upvalues: Vec::new(),
        });
        let binder = RcLocal::default();
        let mut body = Block(vec![Statement::Assign(Assign {
            left: vec![LValue::Local(binder)],
            right: vec![closure],
            prefix: true,
            parallel: false,
        })]);
        assert_eq!(rehoist_constants(&mut body), 1);
        assert!(
            function
                .lock()
                .body
                .to_string()
                .contains("local IMAGE_ID_2 ="),
            "{}",
            function.lock().body
        );
    }

    #[test]
    fn closure_occurrences_form_their_own_scope() {
        let mut function = crate::Function::default();
        function.body = Block(vec![wait(5.0), wait(5.0)]);
        let closure = RValue::Closure(crate::Closure {
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(function))),
            upvalues: Vec::new(),
        });
        let local = RcLocal::default();
        let mut body = Block(vec![
            wait(5.0),
            Statement::Assign(Assign {
                left: vec![LValue::Local(local)],
                right: vec![closure],
                prefix: true,
                parallel: false,
            }),
            Statement::Return(Return::default()),
        ]);
        assert_eq!(rehoist_constants(&mut body), 0);
    }
}
