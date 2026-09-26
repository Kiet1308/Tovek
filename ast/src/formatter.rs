use std::iter;
use std::{
    borrow::Cow,
    fmt::{self},
};

use itertools::Itertools;

use crate::{
    Assign, Binary, BinaryOperation, Block, Call, Closure, GenericFor, If, IfExpression, Index,
    LValue, Literal, LocalRw, MethodCall, NumericFor, RValue, RcLocal, Repeat, Return, Select,
    Statement, Table, Traverse, Unary, While,
};

/// The Luau compound-assignment operator for a binary operation, or `None` for
/// operations that have no compound form (the comparisons and `and`/`or`).
fn compound_assignment_operator(operation: BinaryOperation) -> Option<&'static str> {
    Some(match operation {
        BinaryOperation::Add => "+=",
        BinaryOperation::Sub => "-=",
        BinaryOperation::Mul => "*=",
        BinaryOperation::Div => "/=",
        BinaryOperation::IDiv => "//=",
        BinaryOperation::Mod => "%=",
        BinaryOperation::Pow => "^=",
        BinaryOperation::Concat => "..=",
        BinaryOperation::Equal
        | BinaryOperation::NotEqual
        | BinaryOperation::LessThanOrEqual
        | BinaryOperation::GreaterThanOrEqual
        | BinaryOperation::LessThan
        | BinaryOperation::GreaterThan
        | BinaryOperation::And
        | BinaryOperation::Or => return None,
    })
}

/// True when the assignment LHS and the binary's left operand denote the SAME
/// storage location and can be safely collapsed into a compound assignment.
///
/// * `LValue::Local(t)` matches `RValue::Local(t)` by id-based handle equality —
///   no re-evaluation is possible, so this is unconditionally safe.
/// * `LValue::Index(i)` matches `RValue::Index(j)` when the two indexes are
///   structurally identical AND both the base and key are [`pure_repeatable`].
///   Compound `t.k op= e` evaluates base+key once; the expanded form evaluates
///   them twice, so they coincide only when re-evaluation is unobservable.
fn compound_assign_target_matches(target: &LValue, binary_left: &RValue) -> bool {
    match (target, binary_left) {
        (LValue::Local(t), RValue::Local(l)) => t == l,
        (LValue::Index(lhs), RValue::Index(rhs)) => {
            pure_repeatable(&lhs.left) && pure_repeatable(&lhs.right) && lhs == rhs
        }
        _ => false,
    }
}

/// Whether an index base/key can be repeated without observing any operation
/// between its evaluations. A field read can invoke __index; arithmetic, length
/// and unary operators can invoke metamethods or throw. Bytecode type hints are
/// not proofs that either evaluation is pure, so only local/literal leaves are
/// accepted here. This predicate does not classify general expression purity.
fn pure_repeatable(rvalue: &RValue) -> bool {
    matches!(rvalue, RValue::Local(_) | RValue::Literal(_))
}

pub enum IndentationMode {
    Spaces(u8),
    Tab,
}

impl IndentationMode {
    pub fn display(&self, out: &mut impl fmt::Write, indentation_level: usize) -> fmt::Result {
        let string = match self {
            Self::Spaces(spaces) => Cow::Owned(" ".repeat(*spaces as usize)),
            Self::Tab => Cow::Borrowed("\u{09}"),
        };
        for _ in 0..indentation_level {
            out.write_str(&string)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, BinaryOperation, Function, Global, Local};
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

    fn boolean(value: bool) -> RValue {
        RValue::Literal(Literal::Boolean(value))
    }

    fn method_assignment(
        receiver: &RcLocal,
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
            vec![LValue::Index(Index::new(
                local_value(receiver),
                string(method),
            ))],
            vec![RValue::Closure(Closure {
                node_origin: Default::default(),
                function: ByAddress(Arc::new(Mutex::new(function))),
                upvalues: Vec::new(),
            })],
        )
        .into()
    }

    fn method_call(receiver: &RcLocal, method: &str, arguments: Vec<RValue>) -> Statement {
        MethodCall::new(local_value(receiver), method.to_string(), arguments).into()
    }

    fn closure_call(body: Block) -> Statement {
        let function = Function {
            body,
            ..Default::default()
        };
        Call::new(
            RValue::Closure(Closure {
                node_origin: Default::default(),
                function: ByAddress(Arc::new(Mutex::new(function))),
                upvalues: Vec::new(),
            }),
            vec![],
        )
        .into()
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    fn binary(left: RValue, right: RValue, operation: BinaryOperation) -> RValue {
        RValue::Binary(Binary::new(left, right, operation))
    }

    fn reassign(target: &RcLocal, rhs: RValue) -> Statement {
        Assign::new(vec![LValue::Local(target.clone())], vec![rhs]).into()
    }

    #[test]
    fn compound_assignment_for_arithmetic_and_concat_locals() {
        let total = local("total");
        let text = local("text");
        let block = Block(vec![
            reassign(
                &total,
                binary(local_value(&total), number(1.0), BinaryOperation::Add),
            ),
            reassign(
                &total,
                binary(local_value(&total), number(2.0), BinaryOperation::Sub),
            ),
            reassign(
                &total,
                binary(local_value(&total), number(3.0), BinaryOperation::Mul),
            ),
            reassign(
                &total,
                binary(local_value(&total), number(4.0), BinaryOperation::Div),
            ),
            reassign(
                &total,
                binary(local_value(&total), number(7.0), BinaryOperation::IDiv),
            ),
            reassign(
                &total,
                binary(local_value(&total), number(5.0), BinaryOperation::Mod),
            ),
            reassign(
                &total,
                binary(local_value(&total), number(6.0), BinaryOperation::Pow),
            ),
            reassign(
                &text,
                binary(local_value(&text), string("!"), BinaryOperation::Concat),
            ),
        ]);

        assert_eq!(
            block.to_string(),
            "total += 1\ntotal -= 2\ntotal *= 3\ntotal /= 4\ntotal //= 7\ntotal %= 5\ntotal ^= 6\ntext ..= \"!\""
        );
    }

    #[test]
    fn compound_assignment_groups_rhs_without_redundant_parens() {
        // `x = x - (a - b)` -> `x -= a - b`; `x -= e` already groups the whole `e`.
        let x = local("x");
        let a = local("a");
        let b = local("b");
        let block = Block(vec![reassign(
            &x,
            binary(
                local_value(&x),
                binary(local_value(&a), local_value(&b), BinaryOperation::Sub),
                BinaryOperation::Sub,
            ),
        )]);

        assert_eq!(block.to_string(), "x -= a - b");
    }

    #[test]
    fn no_compound_assignment_when_local_is_right_operand() {
        // `x = a - x` is not `x -= a`; even `x = a + x` differs from `x += a` under
        // an order-sensitive `__add` metamethod.
        let x = local("x");
        let a = local("a");
        let block = Block(vec![
            reassign(
                &x,
                binary(local_value(&a), local_value(&x), BinaryOperation::Sub),
            ),
            reassign(
                &x,
                binary(local_value(&a), local_value(&x), BinaryOperation::Add),
            ),
        ]);

        assert_eq!(block.to_string(), "x = a - x\nx = a + x");
    }

    #[test]
    fn no_compound_assignment_for_operations_without_a_compound_form() {
        let x = local("x");
        let block = Block(vec![
            reassign(
                &x,
                binary(local_value(&x), number(1.0), BinaryOperation::Equal),
            ),
            reassign(
                &x,
                binary(local_value(&x), boolean(true), BinaryOperation::And),
            ),
            reassign(
                &x,
                binary(local_value(&x), boolean(false), BinaryOperation::Or),
            ),
        ]);

        assert_eq!(
            block.to_string(),
            "x = x == 1\nx = x and true\nx = x or false"
        );
    }

    #[test]
    fn no_compound_assignment_for_parallel_assignment() {
        // A `parallel` single-target assign must not be rewritten — its
        // read-then-write timing differs from sequential `x += 1`.
        let x = local("x");
        let mut parallel = Assign::new(
            vec![LValue::Local(x.clone())],
            vec![binary(local_value(&x), number(1.0), BinaryOperation::Add)],
        );
        parallel.parallel = true;
        let block = Block(vec![parallel.into()]);

        // Not rewritten to `x += 1`; the `-- parallel` suffix is the formatter's
        // existing annotation for parallel assignments.
        assert_eq!(block.to_string(), "x = x + 1 -- parallel");
    }

    #[test]
    fn no_compound_assignment_on_index_or_declaration() {
        // A pure-base/literal-key index LHS now collapses (§2.9 B); a `local`
        // declaration stays uncollapsed (a fresh binding, not an update).
        let record = local("record");
        let count = local("count");
        let mut declaration = Assign::new(
            vec![LValue::Local(count.clone())],
            vec![binary(
                local_value(&count),
                number(1.0),
                BinaryOperation::Add,
            )],
        );
        declaration.prefix = true;
        let block = Block(vec![
            Assign::new(
                vec![LValue::Index(Index::new(
                    local_value(&record),
                    string("Count"),
                ))],
                vec![binary(
                    RValue::Index(Index::new(local_value(&record), string("Count"))),
                    number(1.0),
                    BinaryOperation::Add,
                )],
            )
            .into(),
            declaration.into(),
        ]);

        assert_eq!(
            block.to_string(),
            "record.Count += 1\nlocal count = count + 1"
        );
    }

    #[test]
    fn no_compound_assignment_for_nested_observable_index() {
        // Reading a.b twice can invoke __index twice or return different tables.
        let a = local("a");
        let abc = || {
            RValue::Index(Index::new(
                RValue::Index(Index::new(local_value(&a), string("b"))),
                string("c"),
            ))
        };
        let block = Block(vec![
            Assign::new(
                vec![abc().into_lvalue().unwrap()],
                vec![binary(abc(), number(1.0), BinaryOperation::Add)],
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "a.b.c = a.b.c + 1");
    }

    #[test]
    fn no_compound_assignment_for_index_with_impure_key() {
        // `t[f()] = t[f()] + 1` stays — the key call would be evaluated twice.
        let t = local("t");
        let key = || RValue::Call(Call::new(global("f"), vec![]));
        let lhs = || Index::new(local_value(&t), key());
        let block = Block(vec![
            Assign::new(
                vec![LValue::Index(lhs())],
                vec![binary(
                    RValue::Index(lhs()),
                    number(1.0),
                    BinaryOperation::Add,
                )],
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "t[f()] = t[f()] + 1");
    }

    #[test]
    fn no_compound_assignment_for_index_with_impure_base() {
        // `getT().k = getT().k + 1` stays — the base call would be evaluated twice.
        let base = || RValue::Call(Call::new(global("getT"), vec![]));
        let lhs = || Index::new(base(), string("k"));
        let block = Block(vec![
            Assign::new(
                vec![LValue::Index(lhs())],
                vec![binary(
                    RValue::Index(lhs()),
                    number(1.0),
                    BinaryOperation::Add,
                )],
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "(getT()).k = (getT()).k + 1");
    }

    #[test]
    fn no_compound_assignment_for_nested_impure_base() {
        // `t[g()].k = t[g()].k + 1` keeps both index/call evaluations.
        let t = local("t");
        let lhs = || {
            Index::new(
                RValue::Index(Index::new(
                    local_value(&t),
                    RValue::Call(Call::new(global("g"), vec![])),
                )),
                string("k"),
            )
        };
        let block = Block(vec![
            Assign::new(
                vec![LValue::Index(lhs())],
                vec![binary(
                    RValue::Index(lhs()),
                    number(1.0),
                    BinaryOperation::Add,
                )],
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "t[g()].k = t[g()].k + 1");
    }

    #[test]
    fn computed_index_keys_keep_operator_count_even_with_type_hints() {
        let t = local("t");
        let key = local("key");
        key.0.lock().1 = Some("number".into());
        let index = || {
            Index::new(
                local_value(&t),
                binary(local_value(&key), number(1.0), BinaryOperation::Add),
            )
        };
        let block = Block(vec![Assign::new(
            vec![LValue::Index(index())],
            vec![binary(
                RValue::Index(index()),
                number(1.0),
                BinaryOperation::Add,
            )],
        )
        .into()]);
        assert_eq!(block.to_string(), "t[key + 1] = t[key + 1] + 1");
    }

    #[test]
    fn no_compound_assignment_for_distinct_same_named_locals() {
        // `t.k = t2.k + 1` stays — the two index bases are distinct locals, so
        // the LHS and the binary's left operand are not the same location.
        let t = local("t");
        let t2 = local("t2");
        let block = Block(vec![
            Assign::new(
                vec![LValue::Index(Index::new(local_value(&t), string("k")))],
                vec![binary(
                    RValue::Index(Index::new(local_value(&t2), string("k"))),
                    number(1.0),
                    BinaryOperation::Add,
                )],
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "t.k = t2.k + 1");
    }

    #[test]
    fn no_compound_assignment_for_index_with_different_key() {
        // `t.k = t.j + 1` stays — same base local, different key, so the LHS and
        // the binary's left operand denote different locations.
        let t = local("t");
        let block = Block(vec![
            Assign::new(
                vec![LValue::Index(Index::new(local_value(&t), string("k")))],
                vec![binary(
                    RValue::Index(Index::new(local_value(&t), string("j"))),
                    number(1.0),
                    BinaryOperation::Add,
                )],
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "t.k = t.j + 1");
    }

    #[test]
    fn escape_string_keeps_printable_utf8() {
        assert_eq!(
            Formatter::<String>::escape_string("đăng nhập 7 ngày - ô_ngày_giờ ✓".as_bytes()),
            "đăng nhập 7 ngày - ô_ngày_giờ ✓"
        );
    }

    #[test]
    fn escape_string_keeps_utf8_before_later_escape() {
        assert_eq!(
            Formatter::<String>::escape_string("café\n1".as_bytes()).as_ref(),
            r"café\n1"
        );
    }

    #[test]
    fn escape_string_escapes_control_quotes_and_backslashes() {
        assert_eq!(
            Formatter::<String>::escape_string(b"line\n\"quote\"\\path\x07").as_ref(),
            r#"line\n\"quote\"\\path\7"#
        );
    }

    #[test]
    fn escape_string_preserves_invalid_utf8_as_decimal_escapes() {
        assert_eq!(
            Formatter::<String>::escape_string(&[b'a', 0xff, b'b', 0x07, b'1']).as_ref(),
            r#"a\255b\0071"#
        );
    }

    #[test]
    fn escape_string_keeps_apostrophe_bare_inside_double_quotes() {
        // The delimiter is always `"`, so a `'` is the same byte whether written
        // `'` or `\'`. The formatter must emit a bare `'` (idiomatic, value-exact).
        let block = Block(vec![
            Return::new(vec![string("No part tagged 'BossPortal' found")]).into(),
        ]);
        assert_eq!(
            block.to_string(),
            "return \"No part tagged 'BossPortal' found\""
        );
    }

    #[test]
    fn escape_string_keeps_apostrophe_bare_via_byte_path() {
        // The invalid-UTF-8 byte path must also leave `'` bare.
        assert_eq!(
            Formatter::<String>::escape_string(&[b'a', b'\'', 0xff]).as_ref(),
            r#"a'\255"#
        );
    }

    #[test]
    fn format_interpolation_three_star_args() {
        // `("[%*] %* [%*kg]"):format(a, b, c)` -> `` `[{a}] {b} [{c}kg]` ``
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let block = Block(vec![
            Return::new(vec![RValue::MethodCall(MethodCall::new(
                string("[%*] %* [%*kg]"),
                "format".to_string(),
                vec![local_value(&a), local_value(&b), local_value(&c)],
            ))])
            .into(),
        ]);

        assert_eq!(block.to_string(), "return `[{a}] {b} [{c}kg]`");
    }

    #[test]
    fn format_interpolation_double_percent_becomes_literal_percent() {
        // `("100%% %*"):format(x)` -> `` `100% {x}` ``
        let x = local("x");
        let block = Block(vec![
            Return::new(vec![RValue::MethodCall(MethodCall::new(
                string("100%% %*"),
                "format".to_string(),
                vec![local_value(&x)],
            ))])
            .into(),
        ]);

        assert_eq!(block.to_string(), "return `100% {x}`");
    }

    #[test]
    fn format_interpolation_aborts_on_other_specifier() {
        // A `%d` is not `%*`; keep the normal `:format` call unchanged.
        let x = local("x");
        let block = Block(vec![
            Return::new(vec![RValue::MethodCall(MethodCall::new(
                string("count: %d"),
                "format".to_string(),
                vec![local_value(&x)],
            ))])
            .into(),
        ]);

        assert_eq!(block.to_string(), "return (\"count: %d\"):format(x)");
    }

    #[test]
    fn format_interpolation_aborts_on_arity_mismatch() {
        // Two `%*` but only one argument — abort to `:format`.
        let x = local("x");
        let block = Block(vec![
            Return::new(vec![RValue::MethodCall(MethodCall::new(
                string("%* and %*"),
                "format".to_string(),
                vec![local_value(&x)],
            ))])
            .into(),
        ]);

        assert_eq!(block.to_string(), "return (\"%* and %*\"):format(x)");
    }

    #[test]
    fn format_interpolation_escapes_backtick_and_brace() {
        // Static `` ` ``, `{` must be escaped inside the backtick string; `"`, `'`,
        // and `}` stay bare.
        let x = local("x");
        let block = Block(vec![
            Return::new(vec![RValue::MethodCall(MethodCall::new(
                string("a`b{c} \"d\" 'e' %*"),
                "format".to_string(),
                vec![local_value(&x)],
            ))])
            .into(),
        ]);

        assert_eq!(block.to_string(), "return `a\\`b\\{c} \"d\" 'e' {x}`");
    }

    #[test]
    fn format_interpolation_preserves_open_call_argument() {
        // `("%* [%*kg]"):format(fruit, tostring(weight))` matches the real corpus
        // shape: the call argument is rendered via the normal rvalue path.
        let fruit = local("fruit");
        let weight = local("weight");
        let block = Block(vec![
            Return::new(vec![RValue::MethodCall(MethodCall::new(
                string("%* [%*kg]"),
                "format".to_string(),
                vec![
                    local_value(&fruit),
                    RValue::Call(Call::new(global("tostring"), vec![local_value(&weight)])),
                ],
            ))])
            .into(),
        ]);

        // A global named tostring is not proof of a one-result builtin.
        assert_eq!(block.to_string(), "return (\"%* [%*kg]\"):format(fruit, tostring(weight))");
    }

    #[test]
    fn formats_infinity_literals_as_math_huge() {
        let block = Block(vec![
            Return::new(vec![
                RValue::Literal(Literal::Number(f64::INFINITY)),
                RValue::Literal(Literal::Number(f64::NEG_INFINITY)),
            ])
            .into(),
        ]);

        assert_eq!(block.to_string(), "return 1e999, -1e999");
    }

    #[test]
    fn formats_vector_infinity_components_as_math_huge() {
        let block = Block(vec![
            Return::new(vec![RValue::Literal(Literal::Vector(
                f32::INFINITY,
                f32::NEG_INFINITY,
                1.0,
            ))])
            .into(),
        ]);

        assert_eq!(
            block.to_string(),
            "return vector.create(1e999, -1e999, 1)"
        );
    }

    #[test]
    fn wraps_negative_infinity_when_precedence_requires_it() {
        let block = Block(vec![
            Return::new(vec![RValue::Binary(Binary::new(
                RValue::Literal(Literal::Number(2.0)),
                RValue::Literal(Literal::Number(f64::NEG_INFINITY)),
                BinaryOperation::Pow,
            ))])
            .into(),
        ]);

        assert_eq!(block.to_string(), "return 2 ^ (-1e999)");
    }

    #[test]
    fn recovers_colon_method_for_unused_first_param_with_matching_call_site() {
        let module = local("Collision");
        let ignored = local("_");
        let folder = local("folder");
        let target = local("target");

        let block = Block(vec![
            method_assignment(
                &module,
                "DisableCollision",
                vec![ignored, folder.clone()],
                Block(vec![Return::new(vec![local_value(&folder)]).into()]),
            ),
            method_call(&module, "DisableCollision", vec![local_value(&target)]),
        ]);

        assert_eq!(
            block.to_string(),
            "function Collision:DisableCollision(folder)\n\treturn folder\nend\n\nCollision:DisableCollision(target)"
        );
    }

    #[test]
    fn keeps_dot_function_when_unused_first_param_has_no_colon_call_evidence() {
        let module = local("AdminPanel");
        let ignored = local("_");
        let player = local("player");

        let block = Block(vec![method_assignment(
            &module,
            "Init",
            vec![ignored, player.clone()],
            Block(vec![Return::new(vec![local_value(&player)]).into()]),
        )]);

        assert_eq!(
            block.to_string(),
            "function AdminPanel.Init(_, player)\n\treturn player\nend"
        );
    }

    #[test]
    fn keeps_dot_function_when_first_param_is_written() {
        let module = local("Collision");
        let ignored = local("_");
        let folder = local("folder");
        let target = local("target");

        let block = Block(vec![
            method_assignment(
                &module,
                "DisableCollision",
                vec![ignored.clone(), folder.clone()],
                Block(vec![
                    Assign::new(vec![LValue::Local(ignored)], vec![local_value(&folder)]).into(),
                ]),
            ),
            method_call(&module, "DisableCollision", vec![local_value(&target)]),
        ]);

        assert_eq!(
            block.to_string(),
            "function Collision.DisableCollision(_, folder)\n\t_ = folder\nend\n\nCollision:DisableCollision(target)"
        );
    }

    #[test]
    fn keeps_dot_function_when_first_param_is_written_in_nested_closure() {
        let module = local("Collision");
        let ignored = local("_");
        let folder = local("folder");
        let target = local("target");

        let block = Block(vec![
            method_assignment(
                &module,
                "DisableCollision",
                vec![ignored.clone(), folder.clone()],
                Block(vec![closure_call(Block(vec![
                    Assign::new(vec![LValue::Local(ignored)], vec![local_value(&folder)]).into(),
                ]))]),
            ),
            method_call(&module, "DisableCollision", vec![local_value(&target)]),
        ]);

        let output = block.to_string();
        assert!(
            output.contains("function Collision.DisableCollision(_, folder)"),
            "{output}"
        );
        assert!(
            !output.contains("function Collision:DisableCollision(folder)"),
            "{output}"
        );
    }

    #[test]
    fn keeps_dot_function_when_first_param_is_read_in_nested_block() {
        let module = local("Collision");
        let ignored = local("_");
        let folder = local("folder");
        let target = local("target");

        let block = Block(vec![
            method_assignment(
                &module,
                "DisableCollision",
                vec![ignored.clone(), folder],
                Block(vec![
                    If::new(
                        boolean(true),
                        Block(vec![Return::new(vec![local_value(&ignored)]).into()]),
                        Block::default(),
                    )
                    .into(),
                ]),
            ),
            method_call(&module, "DisableCollision", vec![local_value(&target)]),
        ]);

        assert_eq!(
            block.to_string(),
            "function Collision.DisableCollision(_, folder)\n\tif true then\n\t\treturn _\n\tend\nend\n\nCollision:DisableCollision(target)"
        );
    }

    #[test]
    fn keeps_dot_function_when_later_parameter_is_self() {
        let module = local("Controller");
        let ignored = local("_");
        let self_param = local("self");
        let target = local("target");

        let block = Block(vec![
            method_assignment(
                &module,
                "Configure",
                vec![ignored, self_param.clone()],
                Block(vec![Return::new(vec![local_value(&self_param)]).into()]),
            ),
            method_call(&module, "Configure", vec![local_value(&target)]),
        ]);

        assert_eq!(
            block.to_string(),
            "function Controller.Configure(_, self)\n\treturn self\nend\n\nController:Configure(target)"
        );
    }

    #[test]
    fn keeps_dot_function_when_body_mentions_global_self() {
        let module = local("Controller");
        let ignored = local("_");
        let target = local("target");

        let block = Block(vec![
            method_assignment(
                &module,
                "ReadGlobalSelf",
                vec![ignored],
                Block(vec![Return::new(vec![global("self")]).into()]),
            ),
            method_call(&module, "ReadGlobalSelf", vec![local_value(&target)]),
        ]);

        assert_eq!(
            block.to_string(),
            "function Controller.ReadGlobalSelf(_)\n\treturn self\nend\n\nController:ReadGlobalSelf(target)"
        );
    }

    #[test]
    fn ignores_colon_call_evidence_from_nested_closure() {
        let module = local("Collision");
        let ignored = local("_");

        let block = Block(vec![
            method_assignment(&module, "DisableCollision", vec![ignored], Block::default()),
            closure_call(Block(vec![method_call(
                &module,
                "DisableCollision",
                vec![],
            )])),
        ]);

        let output = block.to_string();
        assert!(
            output.contains("function Collision.DisableCollision(_)"),
            "{output}"
        );
        assert!(
            !output.contains("function Collision:DisableCollision()"),
            "{output}"
        );
    }

    #[test]
    fn recovers_colon_method_for_existing_self_parameter() {
        let module = local("Controller");
        let self_param = local("self");

        let block = Block(vec![method_assignment(
            &module,
            "GetValue",
            vec![self_param.clone()],
            Block(vec![
                Return::new(vec![RValue::Index(Index::new(
                    local_value(&self_param),
                    string("Value"),
                ))])
                .into(),
            ]),
        )]);

        assert_eq!(
            block.to_string(),
            "function Controller:GetValue()\n\treturn self.Value\nend"
        );
    }

    #[test]
    fn does_not_recover_colon_for_non_self_first_param_that_is_read() {
        let module = local("Controller");
        let object = local("object");

        let block = Block(vec![method_assignment(
            &module,
            "GetValue",
            vec![object.clone()],
            Block(vec![
                Return::new(vec![RValue::Index(Index::new(
                    local_value(&object),
                    string("Value"),
                ))])
                .into(),
            ]),
        )]);

        assert_eq!(
            block.to_string(),
            "function Controller.GetValue(object)\n\treturn object.Value\nend"
        );
    }

    #[test]
    fn still_formats_regular_global_function_assignments() {
        let block = Block(vec![
            Assign::new(
                vec![LValue::Global(Global::from("make"))],
                vec![RValue::Closure(Closure {
                    node_origin: Default::default(),
                    function: ByAddress(Arc::new(Mutex::new(Function {
                        body: Block(vec![Return::new(vec![global("value")]).into()]),
                        ..Default::default()
                    }))),
                    upvalues: Vec::new(),
                })],
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "function make()\n\treturn value\nend");
    }

    #[test]
    fn callback_property_keeps_assignment_function_syntax() {
        let remotes = local("remotes");
        let block = Block(vec![method_assignment(
            &remotes,
            "OnClientInvoke",
            vec![],
            Block::default(),
        )]);

        assert_eq!(block.to_string(), "remotes.OnClientInvoke = function() end");
    }

    #[test]
    fn formats_if_expression_in_return() {
        let flag = local("flag");
        let block = Block(vec![
            Return::new(vec![
                IfExpression::new(local_value(&flag), string("A"), string("B")).into(),
            ])
            .into(),
        ]);

        assert_eq!(block.to_string(), "return if flag then \"A\" else \"B\"");
    }

    #[test]
    fn formats_if_expression_in_table_field_and_call_arg() {
        let flag = local("flag");
        let block = Block(vec![
            Return::new(vec![RValue::Table(Table::new(vec![
                (
                    Some(string("Value")),
                    IfExpression::new(local_value(&flag), string("A"), string("B")).into(),
                ),
                (
                    Some(string("Printed")),
                    Call::new(
                        global("print"),
                        vec![
                            IfExpression::new(local_value(&flag), string("yes"), string("no"))
                                .into(),
                        ],
                    )
                    .into(),
                ),
            ]))])
            .into(),
        ]);

        assert_eq!(
            block.to_string(),
            "return {\n\tValue = if flag then \"A\" else \"B\",\n\tPrinted = print(if flag then \"yes\" else \"no\")\n}"
        );
    }

    #[test]
    fn empty_string_keys_use_bracket_syntax() {
        let table = local("t");
        let block = Block(vec![
            Return::new(vec![RValue::Table(Table::new(vec![
                (Some(string("")), string("empty")),
                (Some(string("field")), string("value")),
            ]))])
            .into(),
            Return::new(vec![RValue::Index(Index::new(
                local_value(&table),
                string(""),
            ))])
            .into(),
        ]);

        assert_eq!(
            block.to_string(),
            "return {\n\t[\"\"] = \"empty\",\n\tfield = \"value\"\n}\nreturn t[\"\"]"
        );
    }

    #[test]
    fn parenthesizes_if_expression_index_receiver() {
        let flag = local("flag");
        let active = local("active");
        let inactive = local("inactive");
        let block = Block(vec![
            Return::new(vec![RValue::Index(Index::new(
                IfExpression::new(
                    local_value(&flag),
                    local_value(&active),
                    local_value(&inactive),
                )
                .into(),
                string("Offset"),
            ))])
            .into(),
        ]);

        assert_eq!(
            block.to_string(),
            "return (if flag then active else inactive).Offset"
        );
    }

    #[test]
    fn trailing_comment_renders_on_the_preceding_statements_line() {
        let block = Block(vec![
            Call::new(global("loadAfkRewards"), vec![]).into(),
            crate::Comment::trailing("inlined by Luau -O2 (UNHOOKABLE)".to_string()).into(),
        ]);

        assert_eq!(
            block.to_string(),
            "loadAfkRewards() -- inlined by Luau -O2 (UNHOOKABLE)"
        );
    }

    #[test]
    fn leading_comment_keeps_its_own_line() {
        // A default (non-trailing) comment introduces the next statement on its
        // own line, unchanged by the trailing-comment path.
        let block = Block(vec![
            crate::Comment::new(" header".to_string()).into(),
            Call::new(global("f"), vec![]).into(),
        ]);

        assert_eq!(block.to_string(), "--  header\nf()");
    }

    #[test]
    fn trailing_comment_as_first_statement_falls_back_to_its_own_line() {
        // Nothing precedes it, so there is no line to trail.
        let block = Block(vec![
            crate::Comment::trailing("orphan".to_string()).into(),
            Call::new(global("f"), vec![]).into(),
        ]);

        assert_eq!(block.to_string(), "-- orphan\nf()");
    }

    #[test]
    fn disambiguating_semicolon_precedes_a_trailing_comment() {
        // `f()` followed by a call on a wrapped value needs a `;` separator (a
        // line comment is not a separator in Lua). It must sit before the trailing
        // comment: `f(); -- note`, not `f() -- note;`.
        let block = Block(vec![
            Call::new(global("f"), vec![]).into(),
            crate::Comment::trailing("note".to_string()).into(),
            closure_call(Block::default()),
        ]);

        assert_eq!(block.to_string(), "f(); -- note\n(function() end)()");
    }

    #[test]
    fn statement_after_a_trailing_comment_starts_a_fresh_indented_line() {
        let block = Block(vec![
            Call::new(global("a"), vec![]).into(),
            crate::Comment::trailing("mark".to_string()).into(),
            Call::new(global("b"), vec![]).into(),
        ]);

        assert_eq!(block.to_string(), "a() -- mark\nb()");
    }

    #[test]
    fn source_map_records_assigned_callback_identity_and_final_binding() {
        let remote = local("remote");
        let captured = local("state");
        let function = Function {
            bytecode_proto_id: Some(1),
            bytecode_function_id: Some("root:p0/p0@pc1:p1".to_string()),
            ..Default::default()
        };
        let block = Block(vec![
            Assign::new(
                vec![LValue::Index(Index::new(
                    local_value(&remote),
                    string("OnClientInvoke"),
                ))],
                vec![RValue::Closure(Closure {
                    node_origin: Default::default(),
                    function: ByAddress(Arc::new(Mutex::new(function))),
                    upvalues: vec![crate::Upvalue::Ref(captured.clone())],
                })],
            )
            .into(),
        ]);

        let (mapped, occurrences) = format_with_source_map(&block, IndentationMode::Tab).unwrap();
        assert_eq!(mapped, block.to_string());
        assert_eq!(occurrences.len(), 1);
        let occurrence = &occurrences[0];
        assert_eq!(occurrence.syntax_kind, ClosureSyntaxKind::AssignedClosure);
        assert_eq!(
            occurrence.display_name.as_deref(),
            Some("remote.OnClientInvoke")
        );
        assert_eq!(occurrence.upvalue_bindings.len(), 1);
        assert_eq!(
            occurrence.upvalue_bindings[0].stable_id(),
            captured.stable_id()
        );
        assert_eq!(
            &mapped[occurrence.span.start.byte_offset..occurrence.span.end.byte_offset],
            "function() end"
        );
    }

    #[test]
    fn emission_map_preserves_shadow_identity_parameters_loops_and_unicode_offsets() {
        let outer = local("item");
        let parameter = local("item");
        let counter = local("i");
        let helper = local("helper");
        let function = Function {
            parameters: vec![parameter.clone()],
            parameter_annotations: vec![Some("number".into())],
            is_variadic: true,
            body: Block(vec![
                NumericFor::new(number(1.0), number(3.0), number(1.0), counter.clone(),
                    Block(vec![Call::new(global("observe"), vec![local_value(&parameter), local_value(&counter)]).into()])).into(),
                Return::new(vec![local_value(&parameter)]).into(),
            ]),
            ..Default::default()
        };
        let mut assignment = Assign::new(vec![outer.clone().into()], vec![number(1.0)]);
        assignment.prefix = true;
        let mut declaration = Assign::new(vec![helper.clone().into()], vec![RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(function))), upvalues: vec![],
        })]);
        declaration.prefix = true;
        let block = Block(vec![assignment.into(), crate::Comment::trailing("界 annotation".into()).into(),
            declaration.into(), Return::new(vec![local_value(&outer), local_value(&helper)]).into()]);
        let (source, _, map) = format_with_emission_map(&block, IndentationMode::Tab, true).unwrap();
        assert_eq!(source, block.to_string());
        assert_eq!(map.omitted_occurrences, 0);
        assert!(map.opaque_regions.is_empty());
        assert_eq!(map.annotations.len(), 1);
        assert!(source.contains("function helper(item: number, ...)"));
        let names = [(outer.stable_id(), "item"), (parameter.stable_id(), "item"),
            (counter.stable_id(), "i"), (helper.stable_id(), "helper")];
        for occurrence in &map.bindings {
            let name = names.iter().find(|(id, _)| *id == occurrence.binding_id).unwrap().1;
            assert_eq!(&source[occurrence.span.start.byte_offset..occurrence.span.end.byte_offset], name);
            for position in [occurrence.span.start, occurrence.span.end] {
                let prefix = &source[..position.byte_offset];
                assert_eq!(position.line_one_based, prefix.bytes().filter(|b| *b == b'\n').count() + 1);
                assert_eq!(position.column_one_based, prefix.rsplit('\n').next().unwrap().chars().count() + 1);
            }
        }
        assert!(map.bindings.iter().any(|b| b.binding_id == parameter.stable_id() && b.role == "parameter"));
        assert!(map.bindings.iter().any(|b| b.binding_id == outer.stable_id() && b.role == "declaration"));
        assert!(map.bindings.iter().any(|b| b.binding_id == counter.stable_id() && b.role == "iteration_binding"));
        let (_, _, disabled) = format_with_emission_map(&block, IndentationMode::Tab, false).unwrap();
        assert!(disabled.bindings.is_empty() && disabled.annotations.is_empty());
    }

    #[test]
    fn compact_comments_preserve_full_mapping_and_fail_closed_without_room() {
        let known = "equivalent call inferred; original call site unknown";
        let unknown = "custom diagnostic: 界";
        let oversized = format!("[DEDUP] synthesized from {}", "界".repeat(2000));
        let block = Block(vec![crate::Comment::new(known.into()).into(),
            crate::Comment::new(unknown.into()).into(), crate::Comment::new(oversized.clone()).into()]);
        let (compact, _, map) = super::format_with_emission_map_options(&block, IndentationMode::Tab, true, true).unwrap();
        assert!(compact.starts_with("-- inferred call\n"));
        assert!(compact.contains(unknown) && compact.contains(&oversized));
        assert_eq!(map.annotations[0].text, known);
        assert_eq!(map.annotations[0].displayed_text.as_deref(), Some("inferred call"));
        assert!(map.annotations[1].displayed_text.is_none());
        assert!(map.annotations[2].truncated && map.annotations[2].displayed_text.is_none());
        for annotation in &map.annotations {
            let span = &annotation.span;
            assert!(compact[span.start.byte_offset..span.end.byte_offset].starts_with("--"));
        }
        let (fallback, _, map) = super::format_with_emission_map_options(&block, IndentationMode::Tab, false, true).unwrap();
        assert_eq!(fallback, block.to_string());
        assert!(map.annotations.is_empty());
        let mut output = String::new();
        let mut exhausted = crate::emission_map::EmissionMap::default();
        let position = SourcePosition { byte_offset: 0, line_one_based: 1, column_one_based: 1 };
        exhausted.opaque_regions = vec![crate::emission_map::OpaqueOccurrence {
            reason: "test", span: SourceSpan { start: position, end: position },
        }; crate::emission_map::OCCURRENCE_LIMIT];
        let mut formatter = super::Formatter { indentation_level: 0, indentation_mode: IndentationMode::Tab,
            output: &mut output, colon_method_calls: vec![], position_query: None, closure_observer: None,
            emission_map: Some(&mut exhausted), layout_budget: None, compact_annotations: true };
        formatter.format_comment(&crate::Comment::new(known.into())).unwrap();
        assert!(output.contains(known));
    }

    #[test]
    fn emission_map_marks_interpolation_subrendering_opaque() {
        let item = local("item");
        let block = Block(vec![Return::new(vec![RValue::MethodCall(MethodCall {
            node_origin: Default::default(),
            value: Box::new(string("%*")), method: "format".into(), arguments: vec![local_value(&item)],
        })]).into()]);
        let (source, _, map) = format_with_emission_map(&block, IndentationMode::Tab, true).unwrap();
        assert_eq!(source, "return `{item}`");
        assert_eq!(source, block.to_string());
        assert!(map.bindings.is_empty());
        assert_eq!(map.opaque_regions.len(), 1);
        let region = &map.opaque_regions[0];
        assert_eq!(&source[region.span.start.byte_offset..region.span.end.byte_offset], "`{item}`");
    }

    #[test]
    fn generic_for_preserves_explicit_trailing_nil_iterator_argument() {
        // The final nil is an explicit second iterator expression, not an
        // implicit protocol placeholder.  Dropping it changes how a
        // multret-producing first expression is adjusted by the VM.
        let value = local("value");
        let block = Block(vec![
            GenericFor::new(
                vec![value],
                vec![Call::new(global("make"), vec![]).into(), Literal::Nil.into()],
                Block::default(),
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "for value in make(), nil do\n\nend");
    }

    #[test]
    fn wide_groups_preserve_tail_result_adjustment() {
        for truncate in [false, true] {
            let call = Call::new(global("two"), vec![]);
            let tail = if truncate { RValue::Select(Select::Call(call)) } else { call.into() };
            let values = vec![string(&"a".repeat(70)), string(&"b".repeat(70)), tail];
            let call = Call::new(global("collect"), values.clone()).to_string();
            let returned = Return::new(values.clone()).to_string();
            let array = Table::new(values.into_iter().map(|v| (None, v)).collect()).to_string();
            assert!(call.starts_with("collect(\n"));
            assert!(returned.starts_with("return\n"));
            assert!(array.starts_with("{\n"));
            let final_value = if truncate { "(two())" } else { "two()" };
            assert!(call.ends_with(&format!("\t{final_value}\n)")));
            assert!(returned.ends_with(&format!("\t{final_value}")));
            assert!(array.ends_with(&format!("\t{final_value}\n}}")));
        }
    }

    #[test]
    fn layout_counts_assignment_prefix_and_matches_source_map_mode() {
        let result = local(&"r".repeat(90));
        let block = Block(vec![Assign::new(
            vec![result.into()],
            vec![Call::new(global("collect"), vec![string("argument_one"), string("argument_two")]).into()],
        ).into()]);
        let plain = block.to_string();
        let (mapped, _) = format_with_source_map(&block, IndentationMode::Tab).unwrap();
        assert_eq!(plain, mapped);
        assert!(plain.contains("collect(\n"), "{plain}");
        assert!(plain.lines().all(|line| line.chars().count() <= PREFERRED_LINE_WIDTH));
    }

    #[test]
    fn short_calls_and_single_constructor_arguments_stay_compact() {
        assert_eq!(Call::new(global("f"), vec![number(1.0), number(2.0)]).to_string(), "f(1, 2)");
        let table = Table::new(vec![(Some(string("field")), number(1.0))]);
        assert_eq!(Call::new(global("f"), vec![table.into()]).to_string(), "f({\n\tfield = 1\n})");
    }

    #[test]
    fn generic_for_keeps_final_select_call_multret() {
        // The lifter represents a fixed multi-result call feeding the
        // generic-for protocol as Select::Call.  In the final iterator
        // position it must remain bare so generator/state/control all spread.
        let value = local("value");
        let block = Block(vec![
            GenericFor::new(
                vec![value],
                vec![RValue::Select(Select::Call(Call::new(
                    global("make"),
                    vec![],
                )))],
                Block::default(),
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "for value in make() do\n\nend");
    }

    #[test]
    fn generic_for_keeps_final_select_vararg_multret() {
        // Select::VarArg is used for a genuine multret vararg in the final
        // iterator position.  Parenthesizing it would truncate the iterator
        // tuple to one value and change the loop protocol.
        let value = local("value");
        let block = Block(vec![
            GenericFor::new(
                vec![value],
                vec![RValue::Select(Select::VarArg(crate::VarArg))],
                Block::default(),
            )
            .into(),
        ]);

        assert_eq!(block.to_string(), "for value in ... do\n\nend");
    }
}

impl fmt::Display for IndentationMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.display(f, 1)
    }
}

impl Default for IndentationMode {
    fn default() -> Self {
        Self::Tab
    }
}

pub(crate) fn format_arg_list(list: &[RValue]) -> String {
    let mut s = String::new();
    for (index, rvalue) in list.iter().enumerate() {
        if index + 1 == list.len() {
            if matches!(rvalue, RValue::Select(_)) {
                s += &format!("({})", rvalue);
            } else {
                s += &rvalue.to_string();
            }
        } else {
            s += &format!("{}, ", rvalue);
        }
    }
    s
}

pub struct Formatter<'a, W: fmt::Write> {
    pub(crate) indentation_level: usize,
    pub(crate) indentation_mode: IndentationMode,
    pub(crate) output: &'a mut W,
    pub(crate) colon_method_calls: Vec<(RValue, String)>,
    pub(crate) position_query: Option<fn(&W) -> SourcePosition>,
    pub(crate) closure_observer: Option<&'a mut dyn ClosureObserver>,
    pub(crate) emission_map: Option<&'a mut crate::emission_map::EmissionMap>,
    /// Some only in a bounded, non-emitting layout preview. Normal formatting
    /// uses None; previews never recursively ask for another width preview.
    pub(crate) layout_budget: Option<usize>,
    pub(crate) compact_annotations: bool,
}

const PREFERRED_LINE_WIDTH: usize = 120;

/// Stops as soon as a group cannot fit flat. No rendered String is allocated
/// and a multiline literal/closure is never scanned through its body.
struct FlatWidth {
    remaining: usize,
    already_multiline: bool,
}

impl fmt::Write for FlatWidth {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for character in text.chars() {
            let width = if character == '\t' { 4 } else { 1 };
            if character == '\n' || character == '\r' {
                self.already_multiline = true;
                return Err(fmt::Error);
            }
            if width > self.remaining {
                return Err(fmt::Error);
            }
            self.remaining -= width;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourcePosition {
    pub byte_offset: usize,
    pub line_one_based: usize,
    pub column_one_based: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    pub start: SourcePosition,
    pub end: SourcePosition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosureSourceOccurrence {
    pub function_id: String,
    pub syntax_kind: ClosureSyntaxKind,
    pub display_name: Option<String>,
    pub upvalue_bindings: Vec<RcLocal>,
    pub span: SourceSpan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosureSyntaxKind {
    Anonymous,
    AssignedClosure,
    LocalFunction,
    NamedFunction,
    MethodFunction,
}

pub trait ClosureObserver {
    fn closure_emitted(&mut self, occurrence: ClosureSourceOccurrence);
}

struct PositionTrackingWriter<'a, W: fmt::Write> {
    inner: &'a mut W,
    position: SourcePosition,
}

impl<'a, W: fmt::Write> PositionTrackingWriter<'a, W> {
    fn new(inner: &'a mut W) -> Self {
        Self {
            inner,
            position: SourcePosition {
                byte_offset: 0,
                line_one_based: 1,
                column_one_based: 1,
            },
        }
    }
}

impl<W: fmt::Write> fmt::Write for PositionTrackingWriter<'_, W> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.inner.write_str(text)?;
        self.position.byte_offset += text.len();
        for character in text.chars() {
            if character == '\n' {
                self.position.line_one_based += 1;
                self.position.column_one_based = 1;
            } else {
                self.position.column_one_based += 1;
            }
        }
        Ok(())
    }
}

fn tracked_position<W: fmt::Write>(writer: &PositionTrackingWriter<'_, W>) -> SourcePosition {
    writer.position
}

struct VecClosureObserver<'a>(&'a mut Vec<ClosureSourceOccurrence>);

impl ClosureObserver for VecClosureObserver<'_> {
    fn closure_emitted(&mut self, occurrence: ClosureSourceOccurrence) {
        self.0.push(occurrence);
    }
}

pub fn format_with_source_map(
    main: &Block,
    indentation_mode: IndentationMode,
) -> Result<(String, Vec<ClosureSourceOccurrence>), fmt::Error> {
    let (source, closures, _) = format_with_emission_map(main, indentation_mode, false)?;
    Ok((source, closures))
}

pub fn format_with_emission_map(
    main: &Block,
    indentation_mode: IndentationMode,
    detailed: bool,
) -> Result<(String, Vec<ClosureSourceOccurrence>, crate::emission_map::EmissionMap), fmt::Error> {
    format_with_emission_map_options(main, indentation_mode, detailed, false)
}

pub fn format_with_emission_map_options(
    main: &Block,
    indentation_mode: IndentationMode,
    detailed: bool,
    compact_annotations: bool,
) -> Result<(String, Vec<ClosureSourceOccurrence>, crate::emission_map::EmissionMap), fmt::Error> {
    let mut output = String::new();
    let mut tracked = PositionTrackingWriter::new(&mut output);
    let mut occurrences = Vec::new();
    let mut emission_map = crate::emission_map::EmissionMap::default();
    {
        let mut observer = VecClosureObserver(&mut occurrences);
        let mut formatter = Formatter {
            indentation_level: 0,
            indentation_mode,
            output: &mut tracked,
            colon_method_calls: collect_colon_method_calls(main),
            position_query: Some(tracked_position::<String>),
            closure_observer: Some(&mut observer),
            emission_map: detailed.then_some(&mut emission_map),
            layout_budget: None,
            compact_annotations,
        };
        formatter.format_block_no_indent(main)?;
    }
    occurrences.sort_by_key(|occurrence| occurrence.span.start.byte_offset);
    emission_map.sort();
    Ok((output, occurrences, emission_map))
}

fn collect_colon_method_calls(block: &Block) -> Vec<(RValue, String)> {
    let mut calls = Vec::new();
    collect_colon_method_calls_in_block(block, &mut calls);
    calls
}

fn collect_colon_method_calls_in_block(block: &Block, calls: &mut Vec<(RValue, String)>) {
    for statement in block.iter() {
        collect_colon_method_calls_in_statement(statement, calls);
    }
}

fn collect_colon_method_calls_in_statement(
    statement: &Statement,
    calls: &mut Vec<(RValue, String)>,
) {
    if let Statement::MethodCall(method_call) = statement {
        collect_colon_method_call(method_call, calls);
    }
    for rvalue in statement.rvalues() {
        collect_colon_method_calls_in_rvalue(rvalue, calls);
    }

    match statement {
        Statement::If(r#if) => {
            collect_colon_method_calls_in_block(&r#if.then_block.lock(), calls);
            collect_colon_method_calls_in_block(&r#if.else_block.lock(), calls);
        }
        Statement::While(r#while) => {
            collect_colon_method_calls_in_block(&r#while.block.lock(), calls)
        }
        Statement::Repeat(repeat) => {
            collect_colon_method_calls_in_block(&repeat.block.lock(), calls)
        }
        Statement::NumericFor(numeric_for) => {
            collect_colon_method_calls_in_block(&numeric_for.block.lock(), calls)
        }
        Statement::GenericFor(generic_for) => {
            collect_colon_method_calls_in_block(&generic_for.block.lock(), calls)
        }
        _ => {}
    }
}

fn collect_colon_method_calls_in_rvalue(rvalue: &RValue, calls: &mut Vec<(RValue, String)>) {
    match rvalue {
        RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
            collect_colon_method_call(method_call, calls);
        }
        RValue::Closure(_) => return,
        _ => {}
    }

    for child in rvalue.rvalues() {
        collect_colon_method_calls_in_rvalue(child, calls);
    }
}

fn collect_colon_method_call(method_call: &MethodCall, calls: &mut Vec<(RValue, String)>) {
    calls.push((
        method_call.value.as_ref().clone(),
        method_call.method.clone(),
    ));
}

impl<'a, W: fmt::Write> Formatter<'a, W> {
    pub fn format(
        main: &Block,
        output: &'a mut W,
        indentation_mode: IndentationMode,
    ) -> fmt::Result {
        // Layout must see the same column with and without source-map output.
        let mut tracked = PositionTrackingWriter::new(output);
        let mut formatter = Formatter {
            indentation_level: 0,
            indentation_mode,
            output: &mut tracked,
            colon_method_calls: collect_colon_method_calls(main),
            position_query: Some(tracked_position::<W>),
            closure_observer: None,
            emission_map: None,
            layout_budget: None,
            compact_annotations: false,
        };
        formatter.format_block_no_indent(main)
    }

    fn fits_flat(&self, render: impl FnOnce(&mut Formatter<'_, FlatWidth>) -> fmt::Result) -> bool {
        let indentation_width = match self.indentation_mode {
            IndentationMode::Spaces(n) => usize::from(n),
            IndentationMode::Tab => 4,
        };
        let column = self.position_query.map_or(self.indentation_level * indentation_width, |query| {
            let column = query(self.output).column_one_based.saturating_sub(1);
            // Source positions count a tab as one character; the layout budget
            // treats indentation tabs as four display columns.
            column + if matches!(self.indentation_mode, IndentationMode::Tab) { self.indentation_level * 3 } else { 0 }
        });
        let mut width = FlatWidth {
            remaining: PREFERRED_LINE_WIDTH.saturating_sub(column),
            already_multiline: false,
        };
        let mut preview = Formatter {
            indentation_level: self.indentation_level,
            indentation_mode: match self.indentation_mode {
                IndentationMode::Spaces(n) => IndentationMode::Spaces(n),
                IndentationMode::Tab => IndentationMode::Tab,
            },
            output: &mut width,
            colon_method_calls: Vec::new(),
            position_query: None,
            closure_observer: None,
            emission_map: None,
            layout_budget: Some(256),
            compact_annotations: self.compact_annotations,
        };
        let fits = render(&mut preview).is_ok();
        // Existing constructor/callback layouts already break the group. Keep
        // their shape when its opening line fits; nested calls get their own
        // budgets during real emission. Literal payload lines stay untouched.
        fits || width.already_multiline
    }

    fn indent(&mut self) -> fmt::Result {
        self.indentation_mode
            .display(&mut self.output, self.indentation_level)
    }

    // (function() end)()
    // (function() end)[1]
    fn should_wrap_left_rvalue(value: &RValue) -> bool {
        // A format call can be printed as a backtick literal, which is not a
        // prefix expression in Luau. Classify the emitted syntax, not just
        // the original Select::MethodCall node.
        if matches!(value, RValue::Select(Select::MethodCall(call))
            if call.method == "format" && matches!(call.value.as_ref(), RValue::Literal(Literal::String(_)))) {
            return true;
        }
        !matches!(
            value,
            RValue::Local(_)
                | RValue::Global(_)
                | RValue::Index(_)
                | RValue::Select(Select::Call(_) | Select::MethodCall(_))
        )
    }

    /// Whether formatting this expression at the beginning of a statement
    /// starts with `(`.  Luau treats a parenthesized call receiver as an
    /// ambiguous continuation of the preceding statement unless a semicolon
    /// separates the two lines (for example `(obj or fallback).Method()`).
    fn rvalue_starts_with_parenthesis(value: &RValue) -> bool {
        match value {
            RValue::Index(index) => Self::should_wrap_left_rvalue(&index.left),
            RValue::Call(call) => {
                Self::should_wrap_left_rvalue(&call.value)
                    || Self::rvalue_starts_with_parenthesis(&call.value)
            }
            RValue::MethodCall(method_call) => {
                Self::should_wrap_left_rvalue(&method_call.value)
                    || Self::rvalue_starts_with_parenthesis(&method_call.value)
            }
            RValue::Select(Select::Call(call)) => {
                Self::should_wrap_left_rvalue(&call.value)
                    || Self::rvalue_starts_with_parenthesis(&call.value)
            }
            RValue::Select(Select::MethodCall(method_call)) => {
                Self::should_wrap_left_rvalue(&method_call.value)
                    || Self::rvalue_starts_with_parenthesis(&method_call.value)
            }
            _ => false,
        }
    }

    fn format_block(&mut self, block: &Block) -> fmt::Result {
        self.indentation_level += 1;
        self.format_block_no_indent(block)?;
        self.indentation_level -= 1;
        Ok(())
    }

    // Statements that span multiple lines read better with blank lines around
    // them. Control-flow blocks and function definitions qualify.
    fn is_block_statement(statement: &Statement) -> bool {
        match statement {
            Statement::If(_)
            | Statement::While(_)
            | Statement::Repeat(_)
            | Statement::NumericFor(_)
            | Statement::GenericFor(_) => true,
            Statement::Assign(assign) => {
                assign.right.iter().any(|r| matches!(r, RValue::Closure(_)))
            }
            _ => false,
        }
    }

    // Separate large statements from their neighbours with a blank line, but
    // keep comments attached to the statement they document.
    fn wants_blank_line(prev: &Statement, next: &Statement) -> bool {
        if matches!(prev, Statement::Comment(_)) || matches!(next, Statement::Comment(_)) {
            return false;
        }
        Self::is_block_statement(prev) || Self::is_block_statement(next)
    }

    fn format_block_no_indent(&mut self, block: &Block) -> fmt::Result {
        for (i, statement) in block.iter().enumerate() {
            // A trailing comment is appended to the PRECEDING statement's line
            // (` -- text`): no leading newline, no indentation. Guarded on `i != 0`
            // so a comment with nothing before it falls back to its own line. The
            // preceding statement already ran its own disambiguation `;` logic in
            // its iteration (a line comment is not a statement separator in Lua), so
            // `f(); -- text` stays correctly disambiguated.
            if i != 0
                && let Statement::Comment(comment) = statement
                && comment.trailing
            {
                write!(self.output, " ")?;
                self.format_comment(comment)?;
                continue;
            }
            if i != 0 {
                writeln!(self.output)?;
                if Self::wants_blank_line(&block[i - 1], statement) {
                    writeln!(self.output)?;
                }
            }
            self.format_statement(statement)?;
            if let Some(next_statement) =
                block.iter().skip(i + 1).find(|s| s.as_comment().is_none())
            {
                fn is_ambiguous(r: &RValue) -> bool {
                    match r {
                        RValue::Local(_)
                        | RValue::Global(_)
                        | RValue::Index(_)
                        | RValue::Call(_)
                        | RValue::MethodCall(_)
                        | RValue::Select(Select::Call(_) | Select::MethodCall(_)) => true,
                        RValue::Binary(binary) => is_ambiguous(&binary.right),
                        RValue::IfExpression(if_expression) => {
                            is_ambiguous(&if_expression.else_value)
                        }
                        _ => false,
                    }
                }

                let disambiguate = match statement {
                    Statement::Call(_) | Statement::MethodCall(_) => true,
                    Statement::Repeat(repeat) => is_ambiguous(&repeat.condition),
                    Statement::Assign(Assign { right: list, .. })
                    | Statement::Return(Return { values: list, .. }) => {
                        if let Some(last) = list.last() {
                            is_ambiguous(last)
                        } else {
                            false
                        }
                    }
                    Statement::Goto(_) | Statement::Continue(_) | Statement::Break(_) => true,
                    _ => false,
                };
                let disambiguate = disambiguate
                    && match next_statement {
                        Statement::Assign(Assign {
                            left,
                            prefix: false,
                            ..
                        }) => {
                            if let Some(index) = left[0].as_index() {
                                Self::should_wrap_left_rvalue(&index.left)
                            } else {
                                false
                            }
                        }
                        Statement::Call(Call { value, .. })
                        | Statement::MethodCall(MethodCall { value, .. }) => {
                            Self::should_wrap_left_rvalue(value)
                        }
                        Statement::Comment(_) => unimplemented!(),
                        _ => false,
                    };
                if disambiguate {
                    write!(self.output, ";")?;
                }
            }
        }
        Ok(())
    }

    fn format_lvalue(&mut self, lvalue: &LValue) -> fmt::Result {
        match lvalue {
            LValue::Index(index) => self.format_index(index),
            LValue::Local(local) => self.format_local(local, "assignment_target"),
            _ => write!(self.output, "{}", lvalue),
        }
    }

    fn format_local(&mut self, local: &RcLocal, role: &'static str) -> fmt::Result {
        let start = self.current_position();
        write!(self.output, "{}", local)?;
        if let (Some(start), Some(end), Some(map)) =
            (start, self.current_position(), self.emission_map.as_deref_mut())
        {
            map.binding(local.stable_id(), role, SourceSpan { start, end });
        }
        Ok(())
    }

    fn format_comment(&mut self, comment: &crate::Comment) -> fmt::Result {
        let start = self.current_position();
        // Only shorten text when the complete original can be retained. Opaque
        // subrenders and exhausted maps keep the full source diagnostic.
        let retainable = comment.text.len() <= crate::emission_map::ANNOTATION_BYTE_LIMIT
            && self.emission_map.as_ref().is_some_and(|map| map.can_record());
        let compact = (self.compact_annotations && retainable)
            .then(|| crate::annotations::compact_text(&comment.text)).flatten();
        if let Some(text) = compact {
            write!(self.output, "-- {}", text)?;
        } else {
            write!(self.output, "{}", comment)?;
        }
        if let (Some(start), Some(end), Some(map)) =
            (start, self.current_position(), self.emission_map.as_deref_mut())
        {
            let before = map.annotations.len();
            map.annotation(&comment.text, SourceSpan { start, end });
            if map.annotations.len() > before {
                map.annotations.last_mut().unwrap().displayed_text = compact.map(str::to_owned);
            }
        }
        Ok(())
    }

    fn record_opaque(&mut self, start: Option<SourcePosition>, reason: &'static str) {
        if let (Some(start), Some(end), Some(map)) =
            (start, self.current_position(), self.emission_map.as_deref_mut())
        {
            map.opaque(reason, SourceSpan { start, end });
        }
    }

    fn are_table_keys_sequential(table: &Table) -> bool {
        // A table can be rendered as a *pure positional array* `{v1, .., vn}`
        // (i.e. with every key stripped) only when that does not change which
        // index each value lands on. That holds in exactly two cases:
        //
        //   * every entry is positional (no explicit key), or
        //   * every entry has an explicit integer key and those keys are
        //     exactly 1, 2, .., n in order (`{[1]=a,[2]=b}` ≡ `{a,b}`).
        //
        // A *mix* of positional and keyed entries can NOT be stripped: in Luau
        // the positional entries get their own 1-based numbering that ignores
        // the explicit keys, so dropping the keys relocates values (C2 — e.g.
        // `{[1]=11,[2]=22,"a","b"}` must keep its keys, otherwise the positional
        // "a","b" stop overwriting slots 1,2).
        //
        // The key match uses an exact float compare, `*x == i+1`, NOT a cast
        // through `usize`: `f64 as usize` saturates negatives to 0 and truncates
        // fractions, which made `[0]`, `[-1]`, `[0.5]`, `[1.5]` look sequential
        // and silently dropped those keys (C2b).
        if table.0.is_empty() {
            return true;
        }
        let any_keyed = table.0.iter().any(|(k, _)| !k.is_none());
        if !any_keyed {
            return true; // all positional
        }
        if table.0.iter().any(|(k, _)| k.is_none()) {
            return false; // mixed positional + keyed — must render keys
        }
        // all entries keyed: require keys to be exactly 1..n, integral, in order
        table.0.iter().enumerate().all(|(i, (k, _))| {
            matches!(k, Some(RValue::Literal(Literal::Number(x)))
                    if *x == (i as f64) + 1.0)
        })
    }

    fn contains_table(table: &Table) -> bool {
        table.0.iter().any(|(_, v)| matches!(v, RValue::Table(_x)))
    }

    /// An expression that yields multiple values when in a tail/spreading
    /// position (a function/method call, `...`, or a `Select` over one). In a
    /// table that is NOT the open positional tail (i.e. a keyed entry) it must be
    /// parenthesized to truncate to a single value.
    fn is_multret_expression(value: &RValue) -> bool {
        matches!(
            value,
            RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_)
        )
    }

    pub(crate) fn format_table(&mut self, table: &Table) -> fmt::Result {
        let sequential_keys = Self::are_table_keys_sequential(table);
        let should_space = !table.0.is_empty();
        let mut should_format = !table.0.is_empty() && (!sequential_keys || table.0.len() > 3)
            || Self::contains_table(table);
        if !should_format && table.0.len() > 1 && self.layout_budget.is_none() {
            should_format = !self.fits_flat(|preview| preview.format_table(table));
        }
        write!(self.output, "{{")?;
        if should_format {
            writeln!(self.output)?;
        } else if should_space {
            write!(self.output, " ")?;
        }
        self.indentation_level += 1;
        for (index, (key, value)) in table.0.iter().enumerate() {
            if should_format {
                self.indent()?;
            }
            let is_last = index + 1 == table.0.len();
            if is_last && key.is_none() {
                let wrap = matches!(value, RValue::Select(_));
                if wrap {
                    write!(self.output, "(")?;
                }
                self.format_rvalue(value)?;
                if wrap {
                    write!(self.output, ")")?;
                }
            } else {
                if !sequential_keys {
                    if let Some(key) = key {
                        match key {
                            RValue::Literal(Literal::String(field))
                                if Self::is_valid_name(field) =>
                            {
                                write!(self.output, "{} = ", std::str::from_utf8(field).unwrap())?;
                            }
                            _ => {
                                write!(self.output, "[")?;
                                self.format_rvalue(key)?;
                                write!(self.output, "] = ")?;
                            }
                        }
                    }
                }
                // A KEYED last entry (key.is_some(); the spreading multret tail has
                // key.is_none() and is handled above) truncates its value to one
                // element. Only when the key is DROPPED (sequential table) does the
                // entry render positionally and risk spreading, so wrap a multret
                // value in parens there to keep it truncated — `{[1] = f()}`
                // (=> 1 element) must NOT render as `{f()}` (=> all of f()'s
                // results). A rendered key (`field = f()` / `[k] = f()`, non-
                // sequential) already truncates, so no wrap is needed.
                let wrap = is_last && sequential_keys && Self::is_multret_expression(value);
                if wrap {
                    write!(self.output, "(")?;
                }
                self.format_rvalue(value)?;
                if wrap {
                    write!(self.output, ")")?;
                }
                if !is_last {
                    write!(self.output, ",")?;
                    write!(self.output, "{}", if should_format { "\n" } else { " " })?;
                }
            }
        }
        self.indentation_level -= 1;
        if should_format {
            writeln!(self.output)?;
            self.indent()?;
        } else if should_space {
            write!(self.output, " ")?;
        }
        write!(self.output, "}}")
    }

    pub(crate) fn format_unary(&mut self, unary: &Unary) -> fmt::Result {
        write!(self.output, "{}", unary.operation)?;
        let wrap = unary.group();
        if wrap {
            write!(self.output, "(")?;
        }
        self.format_rvalue(&unary.value)?;
        if wrap {
            write!(self.output, ")")?;
        }
        Ok(())
    }

    pub(crate) fn format_binary(&mut self, binary: &Binary) -> fmt::Result {
        let parentheses = |f: &mut Self, wrap: bool, rvalue: &RValue| -> fmt::Result {
            if wrap {
                write!(f.output, "(")?;
            }
            f.format_rvalue(rvalue)?;
            if wrap {
                write!(f.output, ")")?;
            }
            Ok(())
        };

        parentheses(self, binary.left_group(), &binary.left)?;
        write!(self.output, " {} ", binary.operation)?;
        parentheses(self, binary.right_group(), &binary.right)
    }

    pub(crate) fn format_if_expression(&mut self, if_expression: &IfExpression) -> fmt::Result {
        fn format_part<W: fmt::Write>(
            formatter: &mut Formatter<'_, W>,
            value: &RValue,
        ) -> fmt::Result {
            let wrap = matches!(value, RValue::IfExpression(_) | RValue::Select(_));
            if wrap {
                write!(formatter.output, "(")?;
            }
            formatter.format_rvalue(value)?;
            if wrap {
                write!(formatter.output, ")")?;
            }
            Ok(())
        }

        write!(self.output, "if ")?;
        format_part(self, &if_expression.condition)?;
        write!(self.output, " then ")?;
        format_part(self, &if_expression.then_value)?;
        write!(self.output, " else ")?;
        format_part(self, &if_expression.else_value)
    }

    fn format_closure_parameters_from(&mut self, closure: &Closure, skip: usize) -> fmt::Result {
        let function = closure.function.lock();
        for (index, parameter) in function.parameters.iter().enumerate().skip(skip) {
            if index != skip { write!(self.output, ", ")?; }
            self.format_local(parameter, "parameter")?;
            if let Some(annotation) = function.parameter_annotations.get(index).and_then(|a| a.as_deref()) {
                write!(self.output, ": {annotation}")?;
            }
        }
        if function.is_variadic {
            if function.parameters.len() > skip { write!(self.output, ", ")?; }
            write!(self.output, "...")?;
        }
        Ok(())
    }

    fn format_closure_parameters(&mut self, closure: &Closure) -> fmt::Result {
        self.format_closure_parameters_from(closure, 0)
    }

    fn format_closure_body(&mut self, closure: &Closure) -> fmt::Result {
        let function = closure.function.lock();
        if !function.body.is_empty() {
            writeln!(self.output)?;
            self.format_closure_block(&function.body)?;
            writeln!(self.output)?;
            self.indent()
        } else {
            write!(self.output, " ")
        }
    }

    fn format_closure_block(&mut self, block: &Block) -> fmt::Result {
        self.indentation_level += 1;
        let previous_calls = std::mem::replace(
            &mut self.colon_method_calls,
            collect_colon_method_calls(block),
        );
        let result = self.format_block_no_indent(block);
        self.colon_method_calls = previous_calls;
        self.indentation_level -= 1;
        result
    }

    pub(crate) fn format_closure(&mut self, closure: &Closure) -> fmt::Result {
        self.format_closure_with_identity(closure, ClosureSyntaxKind::Anonymous, None)
    }

    fn format_assigned_closure(&mut self, closure: &Closure, target: &LValue) -> fmt::Result {
        let display_name = self.closure_observer.is_some().then(|| target.to_string());
        self.format_closure_with_identity(closure, ClosureSyntaxKind::AssignedClosure, display_name)
    }

    fn format_closure_with_identity(
        &mut self,
        closure: &Closure,
        syntax_kind: ClosureSyntaxKind,
        display_name: Option<String>,
    ) -> fmt::Result {
        let start = self.current_position();
        write!(self.output, "function(")?;
        self.format_closure_parameters(closure)?;
        write!(self.output, ")")?;
        self.format_closure_body(closure)?;
        write!(self.output, "end")?;
        self.record_closure(closure, start, syntax_kind, display_name);
        Ok(())
    }

    fn format_named_function(
        &mut self,
        name: &LValue,
        closure: &Closure,
        local_declaration: bool,
    ) -> fmt::Result {
        let start = self.current_position();
        let (syntax_kind, display_name) =
            self.format_named_function_inner(name, closure, local_declaration)?;
        self.record_closure(closure, start, syntax_kind, display_name);
        Ok(())
    }

    fn format_named_function_inner(
        &mut self,
        name: &LValue,
        closure: &Closure,
        local_declaration: bool,
    ) -> Result<(ClosureSyntaxKind, Option<String>), fmt::Error> {
        if let Some((receiver, method)) = Self::colon_method_target(name) {
            if self.can_format_colon_method(receiver, method, closure) {
                let display_name = self
                    .closure_observer
                    .is_some()
                    .then(|| format!("{receiver}:{method}"));
                write!(self.output, "function ")?;
                self.format_rvalue(receiver)?;
                write!(self.output, ":{}(", method)?;
                self.format_closure_parameters_from(closure, 1)?;
                write!(self.output, ")")?;
                self.format_closure_body(closure)?;
                write!(self.output, "end")?;
                return Ok((ClosureSyntaxKind::MethodFunction, display_name));
            }
        }

        let display_name = self.closure_observer.is_some().then(|| name.to_string());
        write!(self.output, "function ")?;
        if local_declaration && let LValue::Local(local) = name {
            self.format_local(local, "function_declaration")?;
        } else {
            self.format_lvalue(name)?;
        }
        write!(self.output, "(")?;
        self.format_closure_parameters(closure)?;
        write!(self.output, ")")?;
        self.format_closure_body(closure)?;
        write!(self.output, "end")?;
        Ok((
            if local_declaration {
                ClosureSyntaxKind::LocalFunction
            } else {
                ClosureSyntaxKind::NamedFunction
            },
            display_name,
        ))
    }

    fn current_position(&self) -> Option<SourcePosition> {
        self.position_query.map(|query| query(self.output))
    }

    fn record_closure(
        &mut self,
        closure: &Closure,
        start: Option<SourcePosition>,
        syntax_kind: ClosureSyntaxKind,
        display_name: Option<String>,
    ) {
        if let (Some(start), Some(end)) = (start, self.current_position()) {
            if let Some(map) = self.emission_map.as_mut() {
                let bindings = closure.values_read().into_iter().map(RcLocal::stable_id)
                    .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                map.region("closure", bindings, SourceSpan { start, end }, Some(&closure.node_origin));
            }
        }
        let (Some(start), Some(end), Some(observer)) = (
            start,
            self.current_position(),
            self.closure_observer.as_deref_mut(),
        ) else {
            return;
        };
        let Some(function_id) = closure.function.lock().bytecode_function_id.clone() else {
            return;
        };
        observer.closure_emitted(ClosureSourceOccurrence {
            function_id,
            syntax_kind,
            display_name,
            upvalue_bindings: closure
                .upvalues
                .iter()
                .map(|upvalue| match upvalue {
                    crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local) => local.clone(),
                })
                .collect(),
            span: SourceSpan { start, end },
        });
    }

    fn colon_method_target(name: &LValue) -> Option<(&RValue, &str)> {
        let LValue::Index(index) = name else {
            return None;
        };
        let RValue::Literal(Literal::String(method)) = index.right.as_ref() else {
            return None;
        };
        if !Self::is_valid_name(method) || !Self::is_valid_named_function_prefix(&index.left) {
            return None;
        }
        Some((&index.left, std::str::from_utf8(method).unwrap()))
    }

    fn is_valid_named_function_prefix(value: &RValue) -> bool {
        match value {
            RValue::Global(_) | RValue::Local(_) => true,
            RValue::Index(index) => {
                matches!(
                    index.right.as_ref(),
                    RValue::Literal(Literal::String(key)) if Self::is_valid_name(key)
                ) && Self::is_valid_named_function_prefix(&index.left)
            }
            _ => false,
        }
    }

    fn can_format_colon_method(&self, receiver: &RValue, method: &str, closure: &Closure) -> bool {
        let function = closure.function.lock();
        let Some(first_parameter) = function.parameters.first() else {
            return false;
        };
        if function
            .parameters
            .iter()
            .skip(1)
            .any(|param| param.0.0.lock().0.as_deref() == Some("self"))
        {
            return false;
        }

        let first_parameter_name = first_parameter.0.0.lock().0.clone();
        if first_parameter_name.as_deref() == Some("self") {
            return true;
        }

        !Self::block_uses_local(&function.body, first_parameter)
            && !Self::block_mentions_self_name(&function.body)
            && self.has_colon_call(receiver, method)
    }

    fn has_colon_call(&self, receiver: &RValue, method: &str) -> bool {
        self.colon_method_calls
            .iter()
            .any(|(call_receiver, call_method)| call_method == method && call_receiver == receiver)
    }

    fn block_uses_local(block: &Block, local: &RcLocal) -> bool {
        block
            .iter()
            .any(|statement| Self::statement_uses_local(statement, local))
    }

    fn statement_uses_local(statement: &Statement, local: &RcLocal) -> bool {
        if statement
            .values_read()
            .into_iter()
            .any(|read| read == local)
        {
            return true;
        }
        if statement
            .values_written()
            .into_iter()
            .any(|written| written == local)
        {
            return true;
        }
        if statement
            .rvalues()
            .into_iter()
            .any(|rvalue| Self::closure_body_uses_local(rvalue, local))
        {
            return true;
        }

        match statement {
            Statement::If(r#if) => {
                Self::block_uses_local(&r#if.then_block.lock(), local)
                    || Self::block_uses_local(&r#if.else_block.lock(), local)
            }
            Statement::While(r#while) => Self::block_uses_local(&r#while.block.lock(), local),
            Statement::Repeat(repeat) => Self::block_uses_local(&repeat.block.lock(), local),
            Statement::NumericFor(numeric_for) => {
                Self::block_uses_local(&numeric_for.block.lock(), local)
            }
            Statement::GenericFor(generic_for) => {
                Self::block_uses_local(&generic_for.block.lock(), local)
            }
            _ => false,
        }
    }

    fn closure_body_uses_local(rvalue: &RValue, local: &RcLocal) -> bool {
        match rvalue {
            RValue::Closure(closure) => {
                Self::block_uses_local(&closure.function.lock().body, local)
            }
            _ => rvalue
                .rvalues()
                .into_iter()
                .any(|child| Self::closure_body_uses_local(child, local)),
        }
    }

    fn block_mentions_self_name(block: &Block) -> bool {
        block
            .iter()
            .any(|statement| Self::statement_mentions_self_name(statement))
    }

    fn statement_mentions_self_name(statement: &Statement) -> bool {
        if statement
            .values_read()
            .into_iter()
            .chain(statement.values_written())
            .any(Self::local_is_named_self)
        {
            return true;
        }

        if statement
            .rvalues()
            .into_iter()
            .any(Self::rvalue_mentions_self_name)
        {
            return true;
        }

        if let Statement::Assign(assign) = statement {
            if assign.left.iter().any(Self::lvalue_mentions_self_name) {
                return true;
            }
        }

        match statement {
            Statement::If(r#if) => {
                Self::block_mentions_self_name(&r#if.then_block.lock())
                    || Self::block_mentions_self_name(&r#if.else_block.lock())
            }
            Statement::While(r#while) => Self::block_mentions_self_name(&r#while.block.lock()),
            Statement::Repeat(repeat) => Self::block_mentions_self_name(&repeat.block.lock()),
            Statement::NumericFor(numeric_for) => {
                Self::block_mentions_self_name(&numeric_for.block.lock())
            }
            Statement::GenericFor(generic_for) => {
                Self::block_mentions_self_name(&generic_for.block.lock())
            }
            _ => false,
        }
    }

    fn lvalue_mentions_self_name(lvalue: &LValue) -> bool {
        match lvalue {
            LValue::Local(local) => Self::local_is_named_self(local),
            LValue::Global(global) => global.0.as_slice() == b"self",
            LValue::Index(index) => {
                Self::rvalue_mentions_self_name(&index.left)
                    || Self::rvalue_mentions_self_name(&index.right)
            }
        }
    }

    fn rvalue_mentions_self_name(rvalue: &RValue) -> bool {
        match rvalue {
            RValue::Local(local) => Self::local_is_named_self(local),
            RValue::Global(global) => global.0.as_slice() == b"self",
            RValue::Closure(closure) => {
                let function = closure.function.lock();
                !function.parameters.iter().any(Self::local_is_named_self)
                    && Self::block_mentions_self_name(&function.body)
            }
            _ => rvalue
                .rvalues()
                .into_iter()
                .any(Self::rvalue_mentions_self_name),
        }
    }

    fn local_is_named_self(local: &RcLocal) -> bool {
        local.0.0.lock().0.as_deref() == Some("self")
    }

    fn format_rvalue(&mut self, rvalue: &RValue) -> fmt::Result {
        let start = self.emission_map.as_ref().and_then(|_| self.current_position());
        let result = self.format_rvalue_inner(rvalue);
        if result.is_ok() && !matches!(rvalue, RValue::Closure(_)) {
            if let (Some(start), Some(end)) = (start, self.current_position()) {
                if let Some(map) = self.emission_map.as_mut() {
                    let bindings = rvalue.values_read().into_iter().map(RcLocal::stable_id)
                        .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                    map.region(crate::emission_map::value_kind(rvalue), bindings, SourceSpan { start, end },
                        crate::node_origins::value(rvalue));
                }
            }
        }
        result
    }

    fn format_rvalue_inner(&mut self, rvalue: &RValue) -> fmt::Result {
        if let Some(budget) = self.layout_budget.as_mut() {
            if *budget == 0 { return Err(fmt::Error); }
            *budget -= 1;
        }
        match rvalue {
            RValue::Local(local) => self.format_local(local, "read"),
            RValue::Select(Select::Call(call)) | RValue::Call(call) => self.format_call(call),
            RValue::Select(Select::MethodCall(method_call)) | RValue::MethodCall(method_call) => {
                self.format_method_call(method_call)
            }
            RValue::Table(table) => self.format_table(table),
            RValue::Index(index) => self.format_index(index),
            RValue::Unary(unary) => self.format_unary(unary),
            RValue::Binary(binary) => self.format_binary(binary),
            RValue::Closure(closure) => self.format_closure(closure),
            RValue::IfExpression(if_expression) => self.format_if_expression(if_expression),
            _ => write!(self.output, "{}", rvalue),
        }
    }

    fn format_arg_list(&mut self, list: &[RValue], multiline: bool) -> fmt::Result {
        if multiline {
            writeln!(self.output)?;
            self.indentation_level += 1;
        }
        for (index, rvalue) in list.iter().enumerate() {
            if multiline { self.indent()?; }
            if index + 1 == list.len() {
                let wrap = matches!(rvalue, RValue::Select(_));
                if wrap {
                    write!(self.output, "(")?;
                }
                self.format_rvalue(rvalue)?;
                if wrap {
                    write!(self.output, ")")?;
                }
            } else {
                self.format_rvalue(rvalue)?;
                write!(self.output, ",")?;
                if multiline { writeln!(self.output)?; } else { write!(self.output, " ")?; }
            }
        }
        if multiline {
            self.indentation_level -= 1;
            writeln!(self.output)?;
            self.indent()?;
        }
        Ok(())
    }
    pub(crate) fn is_valid_name(name: &[u8]) -> bool {
        if name.is_empty() {
            return false;
        }
        if !(name
            .iter()
            .enumerate()
            .all(|(i, &c)| (i != 0 && c.is_ascii_digit()) || c.is_ascii_alphabetic() || c == b'_'))
        {
            return false;
        }
        // TODO: Consider adding "goto" to reserved keywords
        const RESERVED_KEYWORDS: &[&str] = &[
            "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
            "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
        ];

        let name_str = std::str::from_utf8(name).unwrap_or("");
        if RESERVED_KEYWORDS.contains(&name_str) {
            return false;
        }
        return true;
    }

    fn is_printable_string_char(c: char) -> bool {
        // A bare `'` is left unescaped: the string delimiter is always `"`, so an
        // apostrophe is the same byte whether written `'` or `\'` and re-lexes to
        // the same constant. Source never escapes it inside `"..."`.
        !c.is_control() && c != '\\' && c != '"'
    }

    fn push_escaped_byte(output: &mut String, byte: u8, next: Option<u8>) {
        match byte {
            b'\n' => output.push_str(r"\n"),
            b'\r' => output.push_str(r"\r"),
            b'\t' => output.push_str(r"\t"),
            b'\"' => output.push_str(r#"\""#),
            b'\\' => output.push_str(r"\\"),
            12 => output.push_str(r"\f"),
            _ => {
                let mut buffer = itoa::Buffer::new();
                let printed = buffer.format(byte);
                output.push('\\');
                if printed.len() != 3 && next.is_some_and(|next| next.is_ascii_digit()) {
                    output.extend(iter::repeat('0').take(3 - printed.len()));
                }
                output.push_str(printed);
            }
        };
    }

    fn escape_utf8_string<'s>(string: &'s [u8], text: &'s str) -> Cow<'s, str> {
        let mut owned: Option<String> = None;
        let mut iter = text.char_indices().peekable();
        while let Some((i, c)) = iter.next() {
            if Self::is_printable_string_char(c) {
                if let Some(owned) = &mut owned {
                    owned.push(c);
                }
            } else {
                if owned.is_none() {
                    let mut output = text[..i].to_string();
                    output.reserve((string.len() - i) * 2);
                    owned = Some(output);
                }

                let owned = owned.as_mut().unwrap();
                match c {
                    '\n' => owned.push_str(r"\n"),
                    '\r' => owned.push_str(r"\r"),
                    '\t' => owned.push_str(r"\t"),
                    '"' => owned.push_str(r#"\""#),
                    '\\' => owned.push_str(r"\\"),
                    '\u{000C}' => owned.push_str(r"\f"),
                    _ => {
                        let end = iter
                            .peek()
                            .map(|(next_i, _)| *next_i)
                            .unwrap_or(string.len());
                        for byte_i in i..end {
                            Self::push_escaped_byte(
                                owned,
                                string[byte_i],
                                string.get(byte_i + 1).copied(),
                            );
                        }
                    }
                };
            }
        }

        if let Some(owned) = owned {
            owned.into()
        } else {
            text.into()
        }
    }

    fn escape_bytes<'s>(string: &'s [u8]) -> Cow<'s, str> {
        let mut owned: Option<String> = None;
        for (i, &byte) in string.iter().enumerate() {
            if byte == b' ' || (byte.is_ascii_graphic() && byte != b'\\' && byte != b'\"') {
                if let Some(owned) = &mut owned {
                    owned.push(byte as char);
                }
            } else {
                if owned.is_none() {
                    let mut output = std::str::from_utf8(&string[..i]).unwrap().to_string();
                    output.reserve((string.len() - i) * 2);
                    owned = Some(output);
                }

                Self::push_escaped_byte(owned.as_mut().unwrap(), byte, string.get(i + 1).copied());
            }
        }

        owned
            .map(Cow::Owned)
            .unwrap_or_else(|| std::str::from_utf8(string).unwrap().into())
    }

    pub(crate) fn escape_string<'s>(string: &'s [u8]) -> Cow<'s, str> {
        if let Ok(text) = std::str::from_utf8(string) {
            Self::escape_utf8_string(string, text)
        } else {
            Self::escape_bytes(string)
        }
    }

    pub(crate) fn format_index(&mut self, index: &Index) -> fmt::Result {
        let wrap = Self::should_wrap_left_rvalue(&index.left);
        if wrap {
            write!(self.output, "(")?;
        }
        self.format_rvalue(&index.left)?;
        if wrap {
            write!(self.output, ")")?;
        }

        match index.right.as_ref() {
            RValue::Literal(super::Literal::String(field)) if Self::is_valid_name(field) => {
                write!(self.output, ".{}", std::str::from_utf8(field).unwrap())
            }
            _ => {
                write!(self.output, "[")?;
                self.format_rvalue(&index.right)?;
                write!(self.output, "]")
            }
        }
    }

    pub(crate) fn format_call(&mut self, call: &Call) -> fmt::Result {
        let multiline = self.layout_budget.is_none() && call.arguments.len() > 1
            && !self.fits_flat(|preview| preview.format_call(call));
        let start = self.current_position();
        let wrap = Self::should_wrap_left_rvalue(&call.value);
        if wrap {
            write!(self.output, "(")?;
        }
        self.format_rvalue(&call.value)?;
        if wrap {
            write!(self.output, ")")?;
        }

        write!(self.output, "(")?;
        self.format_arg_list(&call.arguments, multiline)?;
        write!(self.output, ")")?;
        if call.reconstruction_event != 0 {
            if let (Some(start), Some(end), Some(map)) =
                (start, self.current_position(), self.emission_map.as_deref_mut())
            {
                map.reconstructed_call(call.reconstruction_event, call.value.as_local().map(RcLocal::stable_id), SourceSpan { start, end });
            }
        }
        Ok(())
    }

    pub(crate) fn format_method_call(&mut self, method_call: &MethodCall) -> fmt::Result {
        // `("...%*..."):format(args)` -> Luau interpolated string `` `...{args}...` ``.
        // `%*` is exactly the tostring-coercion that `{expr}` performs and evaluation
        // order is preserved, so the result re-lexes to the same string with the same
        // runtime behavior. Refuse-by-default: any other specifier or an unsafe static
        // byte aborts to the normal `:format` path below.
        if method_call.method == "format"
            && let RValue::Literal(Literal::String(bytes)) = method_call.value.as_ref()
            && let Some(interpolated) = self.try_format_interpolation(bytes, &method_call.arguments)
        {
            let start = self.current_position();
            write!(self.output, "{}", interpolated)?;
            // Arguments are rendered into an intermediate string, so recording
            // their temporary offsets as final identifier spans would be wrong.
            self.record_opaque(start, "interpolated_string_argument_rendering");
            return Ok(());
        }

        let multiline = self.layout_budget.is_none() && method_call.arguments.len() > 1
            && !self.fits_flat(|preview| preview.format_method_call(method_call));
        let wrap = Self::should_wrap_left_rvalue(&method_call.value);
        if wrap {
            write!(self.output, "(")?;
        }
        self.format_rvalue(&method_call.value)?;
        if wrap {
            write!(self.output, ")")?;
        }

        write!(self.output, ":{}", method_call.method)?;

        write!(self.output, "(")?;
        self.format_arg_list(&method_call.arguments, multiline)?;
        write!(self.output, ")")
    }

    /// Render an rvalue using the normal formatter path into a fresh `String`,
    /// sharing the current indentation level and colon-method context.
    fn render_rvalue_to_string(&self, rvalue: &RValue) -> Option<String> {
        let mut buffer = String::new();
        let mut sub = Formatter {
            indentation_level: self.indentation_level,
            indentation_mode: match &self.indentation_mode {
                IndentationMode::Spaces(n) => IndentationMode::Spaces(*n),
                IndentationMode::Tab => IndentationMode::Tab,
            },
            output: &mut buffer,
            colon_method_calls: self.colon_method_calls.clone(),
            position_query: None,
            closure_observer: None,
            emission_map: None,
            layout_budget: self.layout_budget,
            compact_annotations: self.compact_annotations,
        };
        sub.format_rvalue(rvalue).ok()?;
        Some(buffer)
    }

    /// Try to convert `("<fmt>"):format(<args>)` into a backtick interpolated
    /// string. Returns `None` (abort to `:format`) on any specifier other than
    /// `%*`/`%%`, on an arity mismatch, or on a static byte that cannot be safely
    /// represented inside backticks.
    fn try_format_interpolation(&self, bytes: &[u8], arguments: &[RValue]) -> Option<String> {
        // An open tail may supply zero values. A placeholder would scalarize
        // it to nil and suppress format's missing-argument error.
        if matches!(arguments.last(), Some(RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_))) {
            return None;
        }
        // Static text re-lexes inside backticks; bytes must be valid UTF-8 so we
        // can reason about each character (invalid UTF-8 aborts).
        let text = std::str::from_utf8(bytes).ok()?;

        let mut out = String::from("`");
        let mut arg_index = 0usize;
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                match chars.next() {
                    Some('%') => out.push('%'),
                    Some('*') => {
                        let arg = arguments.get(arg_index)?;
                        arg_index += 1;
                        out.push('{');
                        let expression = self.render_rvalue_to_string(arg)?;
                        // `{{` starts an invalid token in a backtick string.
                        // Parentheses delimit a table constructor unambiguously.
                        if expression.starts_with('{') {
                            out.push('(');
                            out.push_str(&expression);
                            out.push(')');
                        } else {
                            out.push_str(&expression);
                        }
                        out.push('}');
                    }
                    // Any other specifier (`%s` `%d` `%.2f` `%q` `%x` ...) — abort.
                    _ => return None,
                }
            } else {
                Self::push_backtick_static_char(&mut out, c)?;
            }
        }

        // Require exactly one `%*` per argument.
        if arg_index != arguments.len() {
            return None;
        }

        out.push('`');
        Some(out)
    }

    /// Append a static character to a backtick-string buffer with correct Luau
    /// interpolated-string escaping. Returns `None` if the character cannot be
    /// safely represented (refuse-by-default). Inside backticks `` ` ``, `{`, and
    /// `\` are escaped; `"`, `'`, and `}` stay bare; control chars use their
    /// named escapes. Anything else unrepresentable aborts.
    fn push_backtick_static_char(out: &mut String, c: char) -> Option<()> {
        match c {
            '`' => out.push_str(r"\`"),
            '{' => out.push_str(r"\{"),
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            '\u{000C}' => out.push_str(r"\f"),
            // Other control characters have no safe backtick form here — abort.
            c if c.is_control() => return None,
            c => out.push(c),
        }
        Some(())
    }

    pub(crate) fn format_if(&mut self, r#if: &If) -> fmt::Result {
        write!(self.output, "if ")?;

        self.format_rvalue(&r#if.condition)?;

        writeln!(self.output, " then")?;

        let then_block = r#if.then_block.lock();
        if !then_block.is_empty() {
            self.format_block(&then_block)?;
            writeln!(self.output)?;
        }

        let else_block = r#if.else_block.lock();
        if !else_block.is_empty() {
            self.indent()?;
            if let Some(else_if) = else_block.iter().exactly_one().ok().and_then(|s| s.as_if()) {
                write!(self.output, "else")?;
                return self.format_if(else_if);
            }
            writeln!(self.output, "else")?;
            self.format_block(&else_block)?;
            writeln!(self.output)?;
        }

        self.indent()?;
        write!(self.output, "end")
    }

    pub(crate) fn format_assign(&mut self, assign: &Assign) -> fmt::Result {
        if assign.prefix {
            write!(self.output, "local ")?;
        }

        if assign.left.len() == 1
            && assign.right.len() == 1
            && let RValue::Closure(closure) = &assign.right[0]
            && !Self::is_callback_property(&assign.left[0])
        {
            let left = &assign.left[0];
            if assign.prefix || left.as_global().is_some() || {
                if let LValue::Index(index) = left {
                    let mut index = index;
                    let mut valid = true;
                    loop {
                        if let box RValue::Literal(Literal::String(key)) = &index.right
                            && Self::is_valid_name(key)
                        {
                            match index.left {
                                box RValue::Index(ref i) => {
                                    index = i;
                                    continue;
                                }
                                box RValue::Global(_) | box RValue::Local(_) => {}
                                _ => valid = false,
                            }
                        } else {
                            valid = false;
                        }
                        break;
                    }
                    valid
                } else {
                    false
                }
            } {
                return self.format_named_function(left, closure, assign.prefix);
            }
        }

        // Compound assignment: render `x = x <op> rhs` as `x <op>= rhs`, matching
        // source style. This is always semantics-preserving because Luau defines
        // `x <op>= e` as exactly `x = x <op> (e)` — same operand order, so it holds
        // even for `__add`/`__concat`/... metamethods. Requirements that keep it
        // sound:
        //   * `x` must be the LEFT operand of the binary. `x = rhs - x` is not a
        //     compound op, and even for the commutative-looking `+`/`*` the
        //     metamethod is order-sensitive, so `x = rhs + x` (`__add(rhs, x)`) is
        //     NOT `x += rhs` (`__add(x, rhs)`).
        //   * the LHS target and the binary's left operand must be the SAME
        //     reference. For a plain `LValue::Local` that is handle equality. For
        //     an `LValue::Index` (e.g. `t.k`) it is structural equality of the
        //     two `Index` values AND both the base (`index.left`) and key
        //     (`index.right`) must be PURE-REPEATABLE: `t.k op= e` evaluates
        //     base+key once, while the expanded `t.k = t.k op e` evaluates them
        //     twice, so they agree only when re-evaluation is unobservable.
        //     `t[f()] = t[f()] + 1` is therefore left alone (the `f()` would be
        //     called twice). NOTE: do NOT use `has_side_effects` to gate this —
        //     `Index::has_side_effects` is always `true` (it models `__index`),
        //     which would over-reject the safe local-base/literal-key case; use
        //     the dedicated `pure_repeatable` helper instead.
        // The whole binary RHS is grouped by `<op>=`, so the right operand is
        // emitted as a standalone expression (`format_rvalue`) with no extra
        // parentheses — `x -= a - b` means `x = x - (a - b)`, exactly the AST.
        if !assign.prefix
            && !assign.parallel
            && assign.left.len() == 1
            && assign.right.len() == 1
            && let RValue::Binary(binary) = &assign.right[0]
            && let Some(op) = compound_assignment_operator(binary.operation)
            && compound_assign_target_matches(&assign.left[0], binary.left.as_ref())
        {
            self.format_lvalue(&assign.left[0])?;
            write!(self.output, " {} ", op)?;
            return self.format_rvalue(binary.right.as_ref());
        }

        for (i, lvalue) in assign.left.iter().enumerate() {
            if i != 0 {
                write!(self.output, ", ")?;
            }
            if assign.prefix && let LValue::Local(local) = lvalue {
                self.format_local(local, "declaration")?;
            } else {
                self.format_lvalue(lvalue)?;
            }
        }

        if !assign.right.is_empty() {
            write!(self.output, " = ")?;
        } else {
            assert!(assign.prefix);
        }

        // TODO: REFACTOR: move to format_rvalue_list function
        for (i, rvalue) in assign.right.iter().enumerate() {
            if i != 0 {
                write!(self.output, ", ")?;
            }
            if let (RValue::Closure(closure), Some(target)) = (rvalue, assign.left.get(i)) {
                self.format_assigned_closure(closure, target)?;
            } else {
                self.format_rvalue(rvalue)?;
            }
        }

        if assign.parallel {
            write!(self.output, " -- parallel")?;
        }

        Ok(())
    }

    fn is_callback_property(left: &LValue) -> bool {
        let LValue::Index(index) = left else {
            return false;
        };
        matches!(
            index.right.as_ref(),
            RValue::Literal(Literal::String(key))
                if matches!(
                    key.as_slice(),
                    b"OnClientInvoke" | b"OnServerInvoke" | b"OnIncomingMessage"
                )
        )
    }

    pub(crate) fn format_while(&mut self, r#while: &While) -> fmt::Result {
        write!(self.output, "while ")?;

        self.format_rvalue(&r#while.condition)?;

        writeln!(self.output, " do")?;

        self.format_block(&r#while.block.lock())?;
        writeln!(self.output)?;
        self.indent()?;
        write!(self.output, "end")
    }

    pub(crate) fn format_repeat(&mut self, r#repeat: &Repeat) -> fmt::Result {
        writeln!(self.output, "repeat")?;
        self.format_block(&repeat.block.lock())?;
        writeln!(self.output)?;
        self.indent()?;

        write!(self.output, "until ")?;

        self.format_rvalue(&repeat.condition)
    }

    pub(crate) fn format_numeric_for(&mut self, numeric_for: &NumericFor) -> fmt::Result {
        write!(self.output, "for ")?;
        self.format_local(&numeric_for.counter, "iteration_binding")?;
        write!(self.output, " = ")?;
        self.format_rvalue(&numeric_for.initial)?;
        write!(self.output, ", ")?;
        self.format_rvalue(&numeric_for.limit)?;
        let skip_step = if let RValue::Literal(Literal::Number(n)) = numeric_for.step {
            n == 1.0
        } else {
            false
        };
        if !skip_step {
            write!(self.output, ", ")?;
            self.format_rvalue(&numeric_for.step)?;
        }
        writeln!(self.output, " do")?;
        self.format_block(&numeric_for.block.lock())?;
        writeln!(self.output)?;
        self.indent()?;
        write!(self.output, "end")
    }

    pub(crate) fn format_generic_for(&mut self, generic_for: &GenericFor) -> fmt::Result {
        write!(self.output, "for ")?;
        for (index, local) in generic_for.res_locals.iter().enumerate() {
            if index != 0 { write!(self.output, ", ")?; }
            self.format_local(local, "iteration_binding")?;
        }
        write!(self.output, " in ")?;
        for (i, rvalue) in generic_for.right.iter().enumerate() {
            if i != 0 {
                write!(self.output, ", ")?;
            }
            self.format_rvalue(rvalue)?;
        }
        writeln!(self.output, " do")?;
        self.format_block(&generic_for.block.lock())?;
        writeln!(self.output)?;
        self.indent()?;
        write!(self.output, "end")
    }

    pub(crate) fn format_return(&mut self, r#return: &Return) -> fmt::Result {
        let multiline = self.layout_budget.is_none() && r#return.values.len() > 1
            && !self.fits_flat(|preview| preview.format_return(r#return));
        write!(self.output, "return")?;
        if multiline { self.indentation_level += 1; }
        for (i, rvalue) in r#return.values.iter().enumerate() {
            if multiline {
                if i != 0 { write!(self.output, ",")?; }
                writeln!(self.output)?;
                self.indent()?;
            } else if i == 0 {
                write!(self.output, " ")?;
            } else {
                write!(self.output, ", ")?;
            }
            // A multret value (`Select`, the adjust-to-one wrapper the lifter mints
            // for `(call())` / `(...)`) in the FINAL position must keep its
            // truncating parentheses: `return (two())` yields ONE value, not two
            // (C5). `return` is the only multret context that omitted this wrap;
            // mirror `format_arg_list`. Non-last values are already arity-truncated
            // by the trailing comma, so they need no wrap. A bare `RValue::Call`/
            // `VarArg` (genuine multret) is not a `Select`, so it stays paren-free.
            let wrap = i + 1 == r#return.values.len() && matches!(rvalue, RValue::Select(_));
            if wrap {
                write!(self.output, "(")?;
            }
            self.format_rvalue(rvalue)?;
            if wrap {
                write!(self.output, ")")?;
            }
        }

        if multiline { self.indentation_level -= 1; }
        Ok(())
    }

    fn format_statement(&mut self, statement: &Statement) -> fmt::Result {
        let start = self.emission_map.as_ref().and_then(|_| self.current_position());
        let result = self.format_statement_inner(statement);
        if result.is_ok() {
            if let (Some(start), Some(end)) = (start, self.current_position()) {
                if let Some(map) = self.emission_map.as_mut() {
                    let bindings = statement.values().into_iter().map(RcLocal::stable_id)
                        .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                    map.region("statement", bindings, SourceSpan { start, end },
                        crate::node_origins::statement(statement));
                }
            }
        }
        result
    }

    fn format_statement_inner(&mut self, statement: &Statement) -> fmt::Result {
        self.indent()?;

        if matches!(
            statement,
            Statement::Call(call)
                if Self::rvalue_starts_with_parenthesis(&call.value)
        ) || matches!(
            statement,
            Statement::MethodCall(method_call)
                if Self::rvalue_starts_with_parenthesis(&method_call.value)
        ) {
            // A leading semicolon is valid Luau and prevents an expression
            // statement beginning with `(` from being parsed as arguments to
            // the preceding call/control statement.
            write!(self.output, ";")?;
        }

        match statement {
            Statement::Assign(assign) => self.format_assign(assign),
            Statement::If(r#if) => self.format_if(r#if),
            Statement::While(r#while) => self.format_while(r#while),
            Statement::Repeat(repeat) => self.format_repeat(repeat),
            Statement::NumericFor(numeric_for) => self.format_numeric_for(numeric_for),
            Statement::GenericFor(generic_for) => self.format_generic_for(generic_for),
            Statement::Call(call) => self.format_call(call),
            Statement::MethodCall(method_call) => self.format_method_call(method_call),
            Statement::Return(r#return) => self.format_return(r#return),
            Statement::Comment(comment) => self.format_comment(comment),
            _ => {
                let start = self.current_position();
                write!(self.output, "{}", statement)?;
                if !statement.values().is_empty() {
                    self.record_opaque(start, "statement_display_fallback");
                }
                Ok(())
            }
        }
    }
}
