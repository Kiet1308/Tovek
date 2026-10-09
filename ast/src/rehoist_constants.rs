//! Introduce names for a small set of repeated literals with syntactic roles.
//!
//! The pass is intentionally conservative. A literal is hoisted only when it
//! occurs at least three times in one function scope and every counted use has
//! the same syntactic role (wait duration, magnitude threshold, or asset-id
//! property). This is synthesis, not evidence of an original source constant
//! or API purity. Only the literal moves; lookups, calls and stores stay put.
//!
//! A number that prints short (`0.2`, `1e-6`) or as an exact fraction
//! (`7 / 60`, [`crate::spell_constants`]) reads as well as any name, so it
//! stays where it is.

use itertools::Either;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    Assign, BinaryOperation, Block, Call, Global, Index, LValue, Literal, Local, RValue, RcLocal,
    Select, Statement, Traverse,
};

const MIN_OCCURRENCES: usize = 3;
/// The longest printed number left in place for its length alone.
const SHORT_NUMBER: usize = 8;

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
            Literal::Number(value) if value.is_finite() && Literal::format_number(*value).len() > SHORT_NUMBER
                && crate::spell_constants::exact_fraction(*value).is_none() => ValueKey::Number(value.to_bits()),
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

/// Hoist repeated literals with matching roles in the module and closure scopes.
/// Returns the number of declarations inserted.
pub fn rehoist_constants(body: &mut Block) -> usize {
    // Register estimation recursively examines values. Refuse an oversized
    // tree before either analysis or mutation, using the shared emitter budget.
    if crate::lower_conditionals::validate_local_rewrite_tree(body).is_err() {
        return 0;
    }
    crate::factor_common_tails::unshare_blocks(body);
    rehoist_scope_tree(body, &[])
}

fn rehoist_scope_tree(
    body: &mut Block,
    parameters: &[RcLocal],
) -> usize {
    let mut count = rehoist_one_scope(body, parameters);
    let mut functions = Vec::new();
    collect_nested_functions(&mut body.0, &mut functions);
    for function in functions {
        let mut function = function.0.lock();
        let parameters = function.parameters.clone();
        count += rehoist_scope_tree(&mut function.body, &parameters);
    }
    count
}

fn local_name(local: &RcLocal) -> Option<String> {
    local.0 .0.lock().0.clone()
}

fn reserve_local_name(local: &RcLocal, reserved: &mut FxHashSet<String>) {
    let name = {
        let local = local.0 .0.lock();
        local.0.as_ref().filter(|name| !reserved.contains(name.as_str())).cloned()
    };
    // Release the local guard before inserting, as in the original collector.
    if let Some(name) = name {
        reserved.insert(name);
    }
}

fn reserve_global_name(global: &Global, reserved: &mut FxHashSet<String>) {
    if let Ok(name) = std::str::from_utf8(&global.0)
        && !reserved.contains(name)
    {
        reserved.insert(name.to_owned());
    }
}

/// Reserve every identifier whose textual binding could change when a new local
/// is emitted at this scope's head. Descendant closures are included because an
/// outer declaration also shadows their global lookups after recompilation.
/// Consumers use this set only for membership and fresh-name insertion, never
/// iteration or capacity. Repeated references need not copy an existing name.
pub(crate) fn collect_reserved_identifiers(body: &mut Block, reserved: &mut FxHashSet<String>) {
    for statement in &mut body.0 {
        let mut functions = Vec::new();
        statement.post_traverse_values(&mut |value| -> Option<()> {
            match value {
                Either::Right(RValue::Local(local)) | Either::Left(LValue::Local(local)) => {
                    reserve_local_name(local, reserved);
                }
                Either::Right(RValue::Global(global)) | Either::Left(LValue::Global(global)) => {
                    reserve_global_name(global, reserved);
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
            for parameter in &function.parameters {
                reserve_local_name(parameter, reserved);
            }
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
                reserve_local_name(&node.counter, reserved);
                collect_reserved_identifiers(&mut node.block.lock(), reserved);
            }
            Statement::GenericFor(node) => {
                for local in &node.res_locals {
                    reserve_local_name(local, reserved);
                }
                collect_reserved_identifiers(&mut node.block.lock(), reserved);
            }
            _ => {}
        }
    }
}

fn collect_nested_functions(
    stmts: &mut [Statement],
    functions: &mut Vec<by_address::ByAddress<triomphe::Arc<parking_lot::Mutex<crate::Function>>>>,
) {
    for statement in stmts {
        crate::deinline::visit_stmt_rvalues_mut(statement, &mut |value| {
            collect_functions_in_rvalue(value, functions);
            true
        });
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
    value.visit_rvalues_mut(&mut |child| {
        collect_functions_in_rvalue(child, functions);
        true
    });
}

fn rehoist_one_scope(
    body: &mut Block,
    parameters: &[RcLocal],
) -> usize {
    let mut counts = FxHashMap::default();
    count_block(&body.0, &mut counts);
    let mut selected: Vec<CandidateKey> = counts
        .into_iter()
        .filter_map(|(candidate, count)| (count >= MIN_OCCURRENCES).then_some(candidate))
        .collect();
    selected.sort_by_key(CandidateKey::order_key);
    if selected.is_empty() {
        return 0;
    }
    // Local count alone is insufficient: a 180-parameter function with a
    // 73-argument call compiles at O0, but two extra constants overflow the
    // compiler's 255 registers. Include hidden loop registers and expression
    // scratch, and retain the shared margin for later emitter rewrites.
    selected.truncate(crate::lower_conditionals::local_rewrite_frame(body, parameters, 0).headroom);
    if selected.is_empty() {
        return 0;
    }

    let mut used_names: FxHashSet<String> = parameters.iter().filter_map(local_name).collect();
    collect_reserved_identifiers(body, &mut used_names);

    let mut replacements = FxHashMap::default();
    let mut declarations = Vec::with_capacity(selected.len());
    for candidate in selected {
        let name = unique_name(candidate.base_name(), &mut used_names);
        let local = RcLocal::new(Local::new(Some(name)));
        declarations.push(Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Local(local.clone())],
            right: vec![RValue::Literal(candidate.literal())],
            prefix: true,
            parallel: false,
            compound: false,
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
        crate::deinline::visit_stmt_rvalues(statement, &mut |value| {
            count_rvalue(value, counts);
            true
        });
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
    value.visit_rvalues(&mut |child| {
        count_rvalue(child, counts);
        true
    });
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
        crate::deinline::visit_stmt_rvalues_mut(statement, &mut |value| {
            replace_rvalue(value, replacements);
            true
        });
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
    value.visit_rvalues_mut(&mut |child| {
        replace_rvalue(child, replacements);
        true
    });
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

    /// Durations that print long and have no exact fraction: the only
    /// numbers left to hoist.
    const LONG: f64 = 0.123456789;
    const OTHER: f64 = 2.123456789;

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
            wait(LONG),
            Statement::If(If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block(vec![wait(LONG), wait(LONG)]),
                Block::default(),
            )),
        ]);

        assert_eq!(rehoist_constants(&mut body), 1);
        crate::name_locals::name_locals(&mut body, true);
        assert!(matches!(&body.0[0], Statement::Assign(assign)
            if matches!(assign.left.as_slice(), [LValue::Local(local)]
                if local.0.0.lock().0.as_deref() == Some("WAIT_INTERVAL"))));
    }

    /// `task.wait(7 / 60)` and `task.wait(0.2)` read as well as any name.
    #[test]
    fn short_numbers_and_exact_fractions_stay_inline() {
        for value in [7.0 / 60.0, 0.2, 1e-6, 10.0] {
            let mut body = Block(vec![wait(value), wait(value), wait(value)]);
            assert_eq!(rehoist_constants(&mut body), 0, "{value}");
        }
    }

    #[test]
    fn two_occurrences_stay_inline() {
        let mut body = Block(vec![wait(LONG), wait(LONG)]);
        assert_eq!(rehoist_constants(&mut body), 0);
        assert!(matches!(body.0[0], Statement::Call(_)));
    }

    #[test]
    fn roles_do_not_cross_count() {
        let magnitude = || RValue::Index(Index::new(global("delta"), string("Magnitude")));
        let mut body = Block(vec![
            wait(LONG),
            wait(LONG),
            Statement::If(If::new(
                RValue::Binary(Binary::new(
                    magnitude(),
                    number(LONG),
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
        let mut body = Block(vec![wait(LONG), wait(LONG), delay(LONG)]);
        assert_eq!(rehoist_constants(&mut body), 0);
    }

    #[test]
    fn refuses_hoist_without_local_register_headroom() {
        let mut statements = Vec::new();
        for index in 0..200 {
            statements.push(Statement::Assign(Assign {
                node_origin: Default::default(),
                left: vec![LValue::Local(RcLocal::new(Local::new(Some(format!(
                    "local{index}"
                )))))],
                right: vec![number(index as f64)],
                prefix: true,
                parallel: false, compound: false,
            }));
        }
        statements.extend([wait(LONG), wait(LONG), wait(LONG)]);
        let mut body = Block(statements);
        assert_eq!(rehoist_constants(&mut body), 0);
        assert!(!body.to_string().contains("WAIT_INTERVAL"));
    }

    #[test]
    fn unnamed_parameters_still_consume_local_headroom() {
        let function = triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
            parameters: (0..200).map(|_| RcLocal::default()).collect(),
            body: Block(vec![wait(LONG), wait(LONG), wait(LONG)]),
            ..crate::Function::default()
        }));
        let closure = RValue::Closure(crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(function.clone()),
            upvalues: Vec::new(),
        });
        let mut body = Block(vec![Statement::Return(Return::new(vec![closure]))]);
        assert_eq!(rehoist_constants(&mut body), 0);
        assert!(!function.lock().body.to_string().contains("WAIT_INTERVAL"));
    }

    #[test]
    fn call_arguments_consume_register_headroom_even_below_local_limit() {
        let mut body = Block(vec![wait(LONG); 3]);
        body.0.extend([delay(OTHER), delay(OTHER), delay(OTHER)]);
        body.0.push(Call::new(global("sink"), vec![global("argument"); 73]).into());
        let before = body.to_string();
        let parameters = (0..180).map(|_| RcLocal::default()).collect::<Vec<_>>();
        assert_eq!(rehoist_one_scope(&mut body, &parameters), 0);
        assert_eq!(body.to_string(), before);
    }

    #[test]
    fn over_depth_budget_refuses_before_mutation() {
        let mut value = number(1.0);
        for _ in 0..140 {
            value = Index::new(value, string("field")).into();
        }
        let mut body = Block(vec![wait(LONG), wait(LONG), wait(LONG),
            Return::new(vec![value]).into()]);
        let before = body.to_string();
        assert_eq!(rehoist_constants(&mut body), 0);
        assert_eq!(body.to_string(), before);
    }

    #[test]
    fn ordinary_magnitude_value_uses_neutral_threshold_name() {
        let comparison = || {
            RValue::Binary(Binary::new(
                RValue::Index(Index::new(global("delta"), string("Magnitude"))),
                number(25.123456789),
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
                number(0.000123456789),
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
            node_origin: Default::default(),
            function: by_address::ByAddress(function.clone()),
            upvalues: Vec::new(),
        });
        let binder = RcLocal::default();
        let mut body = Block(vec![Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Local(binder)],
            right: vec![closure],
            prefix: true,
            parallel: false, compound: false,
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
        function.body = Block(vec![wait(LONG), wait(LONG)]);
        let closure = RValue::Closure(crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(function))),
            upvalues: Vec::new(),
        });
        let local = RcLocal::default();
        let mut body = Block(vec![
            wait(LONG),
            Statement::Assign(Assign {
                node_origin: Default::default(),
                left: vec![LValue::Local(local)],
                right: vec![closure],
                prefix: true,
                parallel: false, compound: false,
            }),
            Statement::Return(Return::default()),
        ]);
        assert_eq!(rehoist_constants(&mut body), 0);
    }
}

#[cfg(test)]
#[path = "rehoist_constants/visitor_tests.rs"]
mod visitor_tests;
