#![feature(box_patterns)]
//! Frozen pre-storage-reuse reduction oracle. No skipped or extra reduction calls.
use ast::*;
use by_address::ByAddress;
use parking_lot::Mutex;
use triomphe::Arc;

trait LegacyReduce {
    fn legacy_reduce(self) -> RValue;
    fn legacy_reduce_condition(self) -> RValue;
}
thread_local! { static REFERENCE_BOXES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
fn reference_box(value: RValue) -> Box<RValue> {
    REFERENCE_BOXES.with(|count| count.set(count.get() + 1));
    Box::new(value)
}

fn is_boolean(r: &RValue) -> bool {
    match r {
        RValue::Binary(binary) if binary.operation.is_comparator() => true,
        RValue::Binary(Binary {
            left,
            right,
            operation: BinaryOperation::And | BinaryOperation::Or,
            ..
        }) => is_boolean(left) && is_boolean(right),
        RValue::Unary(unary) if unary.operation == ast::UnaryOperation::Not => true,
        RValue::Literal(Literal::Boolean(_)) => true,
        // strings, numbers and tables are intentionally not matched: callers run
        // after reduce_condition, so a constant would already be folded.
        _ => false,
    }
}


impl LegacyReduce for RValue {
    fn legacy_reduce(self) -> RValue {
        let origin = node_origins::value(&self).map(|o| o.snapshot());
        let mut result = match self {
            Self::Unary(unary) => unary.legacy_reduce(),
            Self::Binary(binary) => binary.legacy_reduce(),
            Self::Literal(literal) => literal.reduce(),
            Self::Table(table) => table.reduce(),
            Self::Closure(closure) => closure.reduce(),
            Self::IfExpression(if_expression) => if_expression.legacy_reduce(),
            other => other,
        };
        node_origins::inherit(&mut result, origin.as_ref());
        result
    }

    fn legacy_reduce_condition(self) -> RValue {
        let origin = node_origins::value(&self).map(|o| o.snapshot());
        let mut result = match self {
            Self::Unary(unary) => unary.legacy_reduce_condition(),
            Self::Binary(binary) => binary.legacy_reduce_condition(),
            Self::Literal(literal) => literal.reduce_condition(),
            Self::Table(table) => table.reduce_condition(),
            Self::Closure(closure) => closure.reduce_condition(),
            Self::IfExpression(if_expression) => if_expression.legacy_reduce_condition(),
            other => other,
        };
        node_origins::inherit(&mut result, origin.as_ref());
        result
    }
}


impl LegacyReduce for Binary {
    fn legacy_reduce(self) -> RValue {
        // TODO: true == true, true == false, etc.
        // really anything without side effects should be true if l == r
        match (self.left.legacy_reduce(), self.right.legacy_reduce(), self.operation) {
            (
                RValue::Unary(Unary {
                    operation: UnaryOperation::Not,
                    value: left,
                    ..
                }),
                RValue::Unary(Unary {
                    operation: UnaryOperation::Not,
                    value: right,
                    ..
                }),
                BinaryOperation::And | BinaryOperation::Or,
            ) => Unary {
                node_origin: Default::default(),
                value: reference_box(
                    Binary {
                        node_origin: Default::default(),
                        left,
                        right,
                        operation: if self.operation == BinaryOperation::And {
                            BinaryOperation::Or
                        } else {
                            BinaryOperation::And
                        },
                    }
                    .into(),
                ),
                operation: UnaryOperation::Not,
            }
            .into(),
            (
                RValue::Literal(Literal::Boolean(left)),
                RValue::Literal(Literal::Boolean(right)),
                BinaryOperation::And | BinaryOperation::Or,
            ) => Literal::Boolean(if self.operation == BinaryOperation::And {
                left && right
            } else {
                left || right
            })
            .into(),
            (
                RValue::Literal(Literal::Boolean(left)),
                right,
                BinaryOperation::And | BinaryOperation::Or,
            ) => match self.operation {
                BinaryOperation::And if !left => RValue::Literal(Literal::Boolean(false)),
                BinaryOperation::And => right.legacy_reduce(),
                BinaryOperation::Or if left => RValue::Literal(Literal::Boolean(true)),
                BinaryOperation::Or => right.legacy_reduce(),
                _ => unreachable!(),
            },
            (left, right, BinaryOperation::And)
                if ast::is_total_pure(&left) && ast::is_total_pure(&right) && left == right =>
            {
                left
            }
            (
                RValue::Binary(Binary {
                    left:
                        box value @ RValue::Unary(Unary {
                            operation: UnaryOperation::Not,
                            ..
                        }),
                    right: box RValue::Literal(Literal::Boolean(true)),
                    operation: BinaryOperation::And,
                    ..
                }),
                RValue::Literal(Literal::Boolean(false)),
                BinaryOperation::Or,
            ) => value,
            (left, right, BinaryOperation::Or)
                if ast::is_total_pure(&left) && ast::is_total_pure(&right) && left == right =>
            {
                left
            }
            // TODO: concat numbers
            (
                RValue::Literal(Literal::String(left)),
                RValue::Literal(Literal::String(right)),
                BinaryOperation::Concat,
            ) => RValue::Literal(Literal::String(
                left.into_iter().chain(right.into_iter()).collect(),
            )),
            (left, right, operation) => Self {
                node_origin: Default::default(),
                left: reference_box(left),
                right: reference_box(right),
                operation,
            }
            .into(),
        }
    }

    fn legacy_reduce_condition(self) -> RValue {
        let (left, right) = if matches!(self.operation, BinaryOperation::And | BinaryOperation::Or)
        {
            (self.left.legacy_reduce_condition(), self.right.legacy_reduce_condition())
        } else {
            (self.left.legacy_reduce(), self.right.legacy_reduce())
        };
        match (left, right, self.operation) {
            (
                RValue::Unary(Unary {
                    operation: UnaryOperation::Not,
                    value: left,
                    ..
                }),
                RValue::Unary(Unary {
                    operation: UnaryOperation::Not,
                    value: right,
                    ..
                }),
                BinaryOperation::And | BinaryOperation::Or,
            ) => Unary {
                node_origin: Default::default(),
                value: reference_box(
                    Binary {
                        node_origin: Default::default(),
                        left,
                        right,
                        operation: if self.operation == BinaryOperation::And {
                            BinaryOperation::Or
                        } else {
                            BinaryOperation::And
                        },
                    }
                    .into(),
                ),
                operation: UnaryOperation::Not,
            }
            .into(),
            (
                RValue::Literal(Literal::Boolean(left)),
                RValue::Literal(Literal::Boolean(right)),
                BinaryOperation::And | BinaryOperation::Or,
            ) => Literal::Boolean(if self.operation == BinaryOperation::And {
                left && right
            } else {
                left || right
            })
            .into(),
            (
                RValue::Literal(Literal::Boolean(left)),
                right,
                BinaryOperation::And | BinaryOperation::Or,
            ) => match self.operation {
                BinaryOperation::And if !left => RValue::Literal(Literal::Boolean(false)),
                BinaryOperation::And => right.legacy_reduce(),
                BinaryOperation::Or if left => RValue::Literal(Literal::Boolean(true)),
                BinaryOperation::Or => right.legacy_reduce(),
                _ => unreachable!(),
            },
            (
                left,
                RValue::Literal(Literal::Boolean(right)),
                BinaryOperation::And | BinaryOperation::Or,
            ) => match self.operation {
                // `X and true` -> X, `X or false` -> X: the literal is the
                // identity here, so X is preserved.
                BinaryOperation::And if right => left.legacy_reduce(),
                BinaryOperation::Or if !right => left.legacy_reduce(),
                // `X and false` -> false, `X or true` -> true: the literal decides
                // the result, but X (the LEFT operand) is ALWAYS evaluated in Lua,
                // so it may only be dropped when it is both side-effect-free and
                // unable to raise; otherwise keep `X <op> <literal>` so X still
                // runs. `has_side_effects()` alone misses indexing, arithmetic,
                // and dynamic table-key errors.
                _ if ast::is_total_pure(&left) => RValue::Literal(Literal::Boolean(right)),
                operation => {
                    Binary::new(left, RValue::Literal(Literal::Boolean(right)), operation).into()
                }
            },
            // TODO: concat numbers
            (
                RValue::Literal(Literal::String(left)),
                RValue::Literal(Literal::String(right)),
                BinaryOperation::Concat,
            ) => RValue::Literal(Literal::String(
                left.into_iter().chain(right.into_iter()).collect(),
            )),
            (left, right, operation) => Self {
                node_origin: Default::default(),
                left: reference_box(left),
                right: reference_box(right),
                operation,
            }
            .into(),
        }
    }
}


impl LegacyReduce for Unary {
    fn legacy_reduce(self) -> RValue {
        // TODO: unnecessary clone
        let does_reduce = |r: &RValue| &r.clone().legacy_reduce_condition() != r;


        let ensure_boolean = |r| {
            if is_boolean(&r) {
                r
            } else {
                Binary::new(
                    Binary::new(r, Literal::Boolean(true).into(), BinaryOperation::And).into(),
                    Literal::Boolean(false).into(),
                    BinaryOperation::Or,
                )
                .into()
            }
        };

        match (self.value.legacy_reduce(), self.operation) {
            (RValue::Literal(Literal::Boolean(value)), UnaryOperation::Not) => {
                RValue::Literal(Literal::Boolean(!value))
            }
            (
                RValue::Unary(Unary {
                    box value,
                    operation: UnaryOperation::Not,
                    ..
                }),
                UnaryOperation::Not,
            ) => ensure_boolean(value.legacy_reduce_condition()),
            (RValue::Literal(Literal::Number(value)), UnaryOperation::Negate) => {
                RValue::Literal(Literal::Number(-value))
            }
            (RValue::Literal(Literal::String(value)), UnaryOperation::Length) => {
                // TODO: is this accurate w/ unicode in Luau?
                RValue::Literal(Literal::Number(value.len() as f64))
            }
            // NOTE (C1): `not (a < b)` is intentionally NOT rewritten to `a >= b`
            // (and the `<=`/`>`/`>=` variants likewise). Those flips are unsound for
            // NaN: `not (nan < 1)` is `true`, but `nan >= 1` is `false`. Ordering
            // comparisons in normal branch conditions already arrive operand-swapped
            // (`a >= b` ⇒ `b <= a`), which is NaN-correct, so dropping these flips only
            // affects an explicit `not (a <rel> b)` expression — kept verbatim here.
            // The equality flips below stay: `not (a == b)` ≡ `a ~= b` even for NaN.
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation: BinaryOperation::Equal,
                    ..
                }),
                UnaryOperation::Not,
            ) => Binary {
                node_origin: Default::default(),
                left,
                right,
                operation: BinaryOperation::NotEqual,
            }
            .legacy_reduce(),
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation: BinaryOperation::NotEqual,
                    ..
                }),
                UnaryOperation::Not,
            ) => Binary {
                node_origin: Default::default(),
                left,
                right,
                operation: BinaryOperation::Equal,
            }
            .legacy_reduce(),
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation,
                    ..
                }),
                UnaryOperation::Not,
            ) if (operation == BinaryOperation::And || operation == BinaryOperation::Or)
            // TODO: unnecessary clones
                && (does_reduce(&Unary {
                    node_origin: Default::default(),
                    value: left.clone(),
                    operation: UnaryOperation::Not,
                }.into()) || does_reduce(&Unary {
                    node_origin: Default::default(),
                    value: right.clone(),
                    operation: UnaryOperation::Not,
                }.into())) =>
            {
                ensure_boolean(
                    Binary {
                        node_origin: Default::default(),
                        left: reference_box(
                            Unary {
                                node_origin: Default::default(),
                                value: left,
                                operation: UnaryOperation::Not,
                            }
                            .legacy_reduce_condition(),
                        ),
                        right: reference_box(
                            Unary {
                                node_origin: Default::default(),
                                value: right,
                                operation: UnaryOperation::Not,
                            }
                            .legacy_reduce_condition(),
                        ),
                        operation: if operation == BinaryOperation::And {
                            BinaryOperation::Or
                        } else {
                            BinaryOperation::And
                        },
                    }
                    .legacy_reduce_condition(),
                )
            }
            (value, operation) => Self {
                node_origin: Default::default(),
                value: reference_box(value),
                operation,
            }
            .into(),
        }
    }

    fn legacy_reduce_condition(self) -> RValue {
        // Only `not` consumes a condition. __unm consumes the original value
        // and may return any type or raise; retain value-context reduction.
        if self.operation == UnaryOperation::Negate {
            return self.legacy_reduce();
        }
        // `#X` evaluates X as a VALUE (not a condition) and, when it succeeds,
        // yields a number — always truthy. But it can run a `__len` metamethod,
        // raise on a non-lengthable X (`#5`, `#nil`), and X itself may have side
        // effects. Reduce X in value context (so a string/table stays itself) and
        // only fold to `true` for a string or total table literal, where none of
        // that applies; otherwise keep `#X` as the condition. A computed nil/NaN
        // table key can raise even though `has_side_effects()` is false.
        if self.operation == UnaryOperation::Length {
            let value = self.value.legacy_reduce();
            return if ast::is_total_pure(&value)
                && matches!(
                    value,
                    RValue::Literal(Literal::String(_)) | RValue::Table(_)
                ) {
                RValue::Literal(Literal::Boolean(true))
            } else {
                Unary {
                    node_origin: Default::default(),
                    value: reference_box(value),
                    operation: UnaryOperation::Length,
                }
                .into()
            };
        }

        // TODO: unnecessary clone
        let does_reduce = |r: &RValue| &r.clone().legacy_reduce_condition() != r;

        match (self.value.legacy_reduce_condition(), self.operation) {
            (RValue::Literal(Literal::Boolean(value)), UnaryOperation::Not) => {
                RValue::Literal(Literal::Boolean(!value))
            }
            (
                RValue::Unary(Unary {
                    box value,
                    operation: UnaryOperation::Not,
                    ..
                }),
                UnaryOperation::Not,
            ) => value.legacy_reduce_condition(),
            (RValue::Literal(Literal::Number(value)), UnaryOperation::Negate) => {
                RValue::Literal(Literal::Number(-value))
            }
            // NOTE (C1): see `reduce` above — the `not (a <rel> b)` → flipped-relation
            // rewrites are omitted here too because they are NaN-unsound, and as a
            // branch condition this is exactly the case that would silently change
            // control flow. The equality flip below is NaN-safe and kept.
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation: BinaryOperation::Equal,
                    ..
                }),
                UnaryOperation::Not,
            ) => Binary {
                node_origin: Default::default(),
                left,
                right,
                operation: BinaryOperation::NotEqual,
            }
            .legacy_reduce_condition(),
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation: BinaryOperation::NotEqual,
                    ..
                }),
                UnaryOperation::Not,
            ) => Binary {
                node_origin: Default::default(),
                left,
                right,
                operation: BinaryOperation::Equal,
            }
            .legacy_reduce_condition(),
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation,
                    ..
                }),
                UnaryOperation::Not,
            ) if (operation == BinaryOperation::And || operation == BinaryOperation::Or)
            // TODO: unnecessary clones
                && (does_reduce(&Unary {
                    node_origin: Default::default(),
                    value: left.clone(),
                    operation: UnaryOperation::Not,
                }.into()) || does_reduce(&Unary {
                    node_origin: Default::default(),
                    value: right.clone(),
                    operation: UnaryOperation::Not,
                }.into())) =>
            {
                Binary {
                    node_origin: Default::default(),
                    left: reference_box(
                        Unary {
                            node_origin: Default::default(),
                            value: left,
                            operation: UnaryOperation::Not,
                        }
                        .legacy_reduce_condition(),
                    ),
                    right: reference_box(
                        Unary {
                            node_origin: Default::default(),
                            value: right,
                            operation: UnaryOperation::Not,
                        }
                        .legacy_reduce_condition(),
                    ),
                    operation: if operation == BinaryOperation::And {
                        BinaryOperation::Or
                    } else {
                        BinaryOperation::And
                    },
                }
                .legacy_reduce_condition()
            }
            (value, operation) => Self {
                node_origin: Default::default(),
                value: reference_box(value),
                operation,
            }
            .into(),
        }
    }
}


impl LegacyReduce for IfExpression {
    fn legacy_reduce(self) -> RValue {
        Self {
            node_origin: Default::default(),
            condition: reference_box(self.condition.legacy_reduce_condition()),
            then_value: reference_box(self.then_value.legacy_reduce()),
            else_value: reference_box(self.else_value.legacy_reduce()),
        }
        .into()
    }

    fn legacy_reduce_condition(self) -> RValue {
        Self {
            node_origin: Default::default(),
            condition: reference_box(self.condition.legacy_reduce_condition()),
            then_value: reference_box(self.then_value.legacy_reduce_condition()),
            else_value: reference_box(self.else_value.legacy_reduce_condition()),
        }
        .into()
    }
}


fn run(value: RValue, condition: bool, direct: bool, legacy: bool) -> RValue {
    match (value, direct, condition, legacy) {
        (RValue::Binary(value), true, false, false) => value.reduce(),
        (RValue::Binary(value), true, true, false) => value.reduce_condition(),
        (RValue::Binary(value), true, false, true) => value.legacy_reduce(),
        (RValue::Binary(value), true, true, true) => value.legacy_reduce_condition(),
        (RValue::Unary(value), true, false, false) => value.reduce(),
        (RValue::Unary(value), true, true, false) => value.reduce_condition(),
        (RValue::Unary(value), true, false, true) => value.legacy_reduce(),
        (RValue::Unary(value), true, true, true) => value.legacy_reduce_condition(),
        (RValue::IfExpression(value), true, false, false) => value.reduce(),
        (RValue::IfExpression(value), true, true, false) => value.reduce_condition(),
        (RValue::IfExpression(value), true, false, true) => value.legacy_reduce(),
        (RValue::IfExpression(value), true, true, true) => value.legacy_reduce_condition(),
        (value, _, false, false) => value.reduce(),
        (value, _, true, false) => value.reduce_condition(),
        (value, _, false, true) => value.legacy_reduce(),
        (value, _, true, true) => value.legacy_reduce_condition(),
    }
}

fn expression(seed: &mut u64, depth: usize, locals: &[RcLocal], function: &Arc<Mutex<Function>>) -> RValue {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    let choice = (*seed >> 32) as usize;
    if depth == 0 {
        return match choice % 11 {
            0 => locals[0].clone().into(), 1 => Literal::Boolean(true).into(),
            2 => Literal::Boolean(false).into(), 3 => Literal::Nil.into(),
            4 => Literal::Number(f64::from_bits(0x7ff8_0000_0000_1234)).into(),
            5 => Literal::String(vec![0, 255]).into(), 6 => Global::from("global").into(),
            7 => Literal::Number(-0.0).into(), 8 => Literal::VectorD(f64::NAN, -0.0, f64::INFINITY).into(),
            9 => Select::VarArg(VarArg).into(), _ => locals[1].clone().into(),
        };
    }
    let left = expression(seed, depth - 1, locals, function);
    let right = expression(seed, depth - 1, locals, function);
    let mut result = match choice % 18 {
        0..=6 => Binary::new(left, right, [BinaryOperation::And, BinaryOperation::Or,
            BinaryOperation::Concat, BinaryOperation::Equal, BinaryOperation::NotEqual,
            BinaryOperation::LessThan, BinaryOperation::Add][choice % 7]).into(),
        7..=9 => Unary::new(left, [UnaryOperation::Not, UnaryOperation::Negate, UnaryOperation::Length][choice % 3]).into(),
        10 => IfExpression::new(left, right, Literal::Boolean(false).into()).into(),
        11 => Call::new(Global::from("observe").into(), vec![left, right]).into(),
        12 => Index::new(left, right).into(),
        13 => Table::new(vec![(Some(left), right)]).into(),
        14 => Select::Call(Call::new(left, vec![right])).into(),
        15 => Closure { node_origin: Default::default(), function: ByAddress(function.clone()),
            upvalues: vec![Upvalue::Ref(locals[0].clone()), Upvalue::Copy(locals[1].clone())] }.into(),
        16 => Unary::new(Binary::new(left, right, BinaryOperation::Or).into(), UnaryOperation::Not).into(),
        _ => Binary::new(Unary::new(left.clone(), UnaryOperation::Not).into(),
            Unary::new(left, UnaryOperation::Not).into(), BinaryOperation::And).into(),
    };
    if let Some(origin) = node_origins::value_mut(&mut result) {
        *origin = node_origins::Origin::input(node_origins::Input {
            function: "reduce_storage".into(), block: choice / 8, statement: choice, value: Some(depth),
        });
        let data = origin.0.as_mut().unwrap();
        data.inlined = choice & 1 != 0;
        data.cloned = choice & 2 != 0;
        data.incomplete = choice & 4 != 0;
        if choice & 8 != 0 { data.synthesized = Some("test_origin"); }
        if choice & 16 != 0 { data.inputs.push(data.inputs[0].clone()); }
    }
    result
}

type OriginSnapshot = Option<(Vec<std::sync::Arc<node_origins::Input>>, bool, bool, bool, Option<&'static str>)>;
fn snapshot(value: &RValue, origins: &mut Vec<OriginSnapshot>, bits: &mut Vec<u64>) {
    if let Some(origin) = node_origins::value(value) {
        origins.push(origin.0.as_ref().map(|data| (data.inputs.clone(), data.inlined,
            data.cloned, data.incomplete, data.synthesized)));
    }
    match value {
        RValue::Literal(Literal::Number(number)) => bits.push(number.to_bits()),
        RValue::Literal(Literal::Vector(x, y, z)) => bits.extend([x.to_bits() as u64, y.to_bits() as u64, z.to_bits() as u64]),
        RValue::Literal(Literal::VectorD(x, y, z)) => bits.extend([x.to_bits(), y.to_bits(), z.to_bits()]),
        RValue::Closure(closure) => {
            for statement in &closure.function.lock().body.0 {
                statement.traverse_rvalues_ref(&mut |value| snapshot(value, origins, bits));
            }
        }
        _ => {}
    }
    value.visit_rvalues(&mut |child| { snapshot(child, origins, bits); true });
}

#[test]
fn reused_operand_storage_matches_recursive_legacy_rules_and_metadata() {
    let locals = [RcLocal::new(Local::new(Some("a".into()))), RcLocal::new(Local::new(Some("b".into())))];
    let function = Arc::new(Mutex::new(Function {
        parameters: vec![locals[0].clone()],
        body: Block(vec![Return::new(vec![locals[1].clone().into()]).into()]),
        ..Default::default()
    }));
    let owners = || [Arc::strong_count(&locals[0].0.0), Arc::strong_count(&locals[1].0.0), Arc::strong_count(&function)];
    let baseline_owners = owners();
    for seed in 1..=2048u64 {
        for condition in [false, true] {
            for rounds in 1..=3 {
                // Direct struct APIs deliberately discard their root origin;
                // RValue APIs inherit it. Both contracts must remain exact.
                let direct = seed % 4 == 0;
                let mut actual = expression(&mut seed.clone(), 3, &locals, &function);
                for _ in 0..rounds { actual = run(actual, condition, direct, false); }
                let actual_shape = format!("{actual:?}");
                let actual_source = actual.to_string();
                let (mut actual_origins, mut actual_bits) = (Vec::new(), Vec::new());
                snapshot(&actual, &mut actual_origins, &mut actual_bits);
                let actual_effects = (actual.has_side_effects(), ast::is_total_pure(&actual));
                let actual_owners = owners();
                drop(actual);
                assert_eq!(owners(), baseline_owners);
                let mut expected = expression(&mut seed.clone(), 3, &locals, &function);
                for _ in 0..rounds { expected = run(expected, condition, direct, true); }
                assert_eq!(format!("{expected:?}"), actual_shape, "seed {seed}, condition {condition}, rounds {rounds}");
                assert_eq!(expected.to_string(), actual_source);
                let (mut expected_origins, mut expected_bits) = (Vec::new(), Vec::new());
                snapshot(&expected, &mut expected_origins, &mut expected_bits);
                assert_eq!(expected_origins, actual_origins, "origins: seed {seed}, condition {condition}, rounds {rounds}");
                assert_eq!(expected_bits, actual_bits);
                assert_eq!((expected.has_side_effects(), ast::is_total_pure(&expected)), actual_effects);
                assert_eq!(owners(), actual_owners, "local/closure ownership");
                drop(expected);
            }
        }
    }
}

fn stable_tree(depth: usize, local: &RcLocal) -> RValue {
    if depth == 0 { return local.clone().into(); }
    match depth % 3 {
        0 => Binary::new(stable_tree(depth - 1, local), stable_tree(depth - 1, local), BinaryOperation::Add).into(),
        1 => Unary::new(stable_tree(depth - 1, local), UnaryOperation::Negate).into(),
        _ => IfExpression::new(local.clone().into(), stable_tree(depth - 1, local), stable_tree(depth - 1, local)).into(),
    }
}
fn operand_addresses(value: &RValue, out: &mut Vec<usize>) {
    value.visit_rvalues(&mut |child| { out.push(child as *const RValue as usize); operand_addresses(child, out); true });
}

#[test]
fn unchanged_reductions_reuse_every_box_across_repeated_value_and_condition_calls() {
    let local = RcLocal::default();
    for depth in [3, 6, 9] {
        for condition in [false, true] {
            let mut actual = stable_tree(depth, &local);
            let mut addresses = Vec::new();
            operand_addresses(&actual, &mut addresses);
            let mut expected = stable_tree(depth, &local);
            REFERENCE_BOXES.with(|count| count.set(0));
            for _ in 0..4 {
                actual = run(actual, condition, false, false);
                expected = run(expected, condition, false, true);
                let mut after = Vec::new();
                operand_addresses(&actual, &mut after);
                assert_eq!(after, addresses, "depth {depth}, condition {condition}");
                assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
            }
            assert_eq!(REFERENCE_BOXES.with(|count| count.get()), addresses.len() * 4);
        }
    }
}

#[test]
fn repeated_reduction_still_has_legacy_non_idempotent_steps() {
    let local = RcLocal::new(Local::new(Some("a".into())));
    let not = || Unary::new(local.clone().into(), UnaryOperation::Not).into();
    let value: RValue = Binary::new(not(), not(), BinaryOperation::And).into();
    let once = value.reduce();
    assert_eq!(once.to_string(), "not (a or a)");
    assert_eq!(once.reduce().to_string(), "not a");
    let value: RValue = Unary::new(Literal::Number(1.0).into(), UnaryOperation::Negate).into();
    let once = value.reduce_condition();
    assert_eq!(once.to_string(), "-1");
    assert_eq!(once.reduce_condition().to_string(), "true");
    let value: RValue = Binary::new(Literal::String(b"a".to_vec()).into(),
        Literal::String(b"b".to_vec()).into(), BinaryOperation::Concat).into();
    let once = value.reduce_condition();
    assert_eq!(once.to_string(), "\"ab\"");
    assert_eq!(once.reduce_condition().to_string(), "true");
}
