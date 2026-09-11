//! Emit original/statement-lowered AST fixtures for the pinned VM audit.
use ast::{
    Assign, Binary, BinaryOperation as Op, Block, Call, Closure, Function, Global, If,
    IfExpression, Index, LValue, Literal, Local, MethodCall, RValue, RcLocal, Return, Statement,
    Upvalue,
};
use by_address::ByAddress;
use parking_lot::Mutex;
use std::{fs, path::PathBuf};
use triomphe::Arc;

fn local(name: &str) -> RcLocal {
    RcLocal::new(Local::new(Some(name.into())))
}
fn string(value: &str) -> RValue {
    Literal::String(value.as_bytes().to_vec()).into()
}
fn number(value: f64) -> RValue {
    Literal::Number(value).into()
}
fn nil() -> RValue {
    Literal::Nil.into()
}
fn read(value: &RcLocal) -> RValue {
    value.clone().into()
}
fn ret(values: Vec<RValue>) -> Statement {
    Return::new(values).into()
}
fn select(flag: RValue, yes: RValue, no: RValue) -> RValue {
    IfExpression::new(flag, yes, no).into()
}
fn closure(parameters: Vec<RcLocal>, body: Block, captures: &[RcLocal]) -> RValue {
    Closure {
        function: ByAddress(Arc::new(Mutex::new(Function {
            parameters,
            body,
            ..Default::default()
        }))),
        upvalues: captures.iter().cloned().map(Upvalue::Ref).collect(),
    }
    .into()
}
fn assign(target: &RcLocal, value: RValue, prefix: bool) -> Statement {
    Assign {
        left: vec![target.clone().into()],
        right: vec![value],
        prefix,
        parallel: false,
    }
    .into()
}

struct Inputs {
    params: Vec<RcLocal>,
    vararg_count: Option<usize>,
}
impl Inputs {
    fn new() -> Self {
        Self {
            vararg_count: None,
            params: [
                "flag",
                "emit",
                "value",
                "callback",
                "object",
                "otherCallback",
                "otherObject",
                "iterator",
                "receiver",
            ]
            .iter()
            .map(|name| local(name))
            .collect(),
        }
    }
    fn p(&self, index: usize) -> RValue {
        read(&self.params[index])
    }
    fn emit(&self, label: &str, value: RValue) -> RValue {
        Call::new(self.p(1), vec![string(label), value]).into()
    }
    fn choose(&self, yes: RValue, no: RValue) -> RValue {
        select(self.p(0), yes, no)
    }
    fn choice(&self) -> RValue {
        self.choose(
            self.emit("yes", nil()),
            self.emit("no", Literal::Boolean(false).into()),
        )
    }
    fn mutation(&self, target: usize, value: RValue, result: RValue) -> RValue {
        let setter = closure(
            vec![],
            Block(vec![assign(&self.params[target], value, false)]),
            &self.params,
        );
        Call::new(self.p(1), vec![string("mutate"), result, setter]).into()
    }
    fn module(&self, body: Block, captured: bool) -> Block {
        let body = if captured {
            Block(vec![ret(vec![Call::new(
                closure(vec![], body, &self.params),
                vec![],
            )
            .into()])])
        } else {
            body
        };
        let function = closure(self.params.clone(), body, &[]);
        if self.vararg_count.is_some() {
            function.as_closure().unwrap().function.lock().is_variadic = true;
        }
        Block(vec![ret(vec![function])])
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("expected fresh output directory")?,
    );
    fs::create_dir(&root)?;
    let mut cases = Vec::new();
    let mut add = |name: &str,
                   inputs: &Inputs,
                   body: Block,
                   captured: bool,
                   refusal: Option<&str>|
     -> Result<(), Box<dyn std::error::Error>> {
        let mut module = inputs.module(body, captured);
        let before = module.to_string();
        let report = ast::lower_conditionals::lower_existing_conditionals(&mut module);
        let after = module.to_string();
        if let Some(reason) = refusal {
            assert_eq!(before, after, "refusal changed {name}");
            assert!(
                report.refused_statements.contains_key(reason),
                "{name}: {report:?}"
            );
        } else {
            assert!(
                report.input_selects > 0 && report.input_selects == report.lowered_selects,
                "{name}: {report:?}"
            );
            assert!(report.refused_statements.is_empty());
        }
        let second = ast::lower_conditionals::lower_existing_conditionals(&mut module);
        assert_eq!(
            after,
            module.to_string(),
            "lowering is not idempotent: {name}"
        );
        assert_eq!(second.lowered_selects, 0);
        let dir = root.join(name);
        fs::create_dir(&dir)?;
        fs::write(dir.join("source.luau"), before + "\n")?;
        fs::write(dir.join("output.luau"), after + "\n")?;
        cases.push(
            serde_json::json!({"case":name,"report":report,"expected_refusal":refusal,
            "extra_arguments":inputs.vararg_count.unwrap_or(0)}),
        );
        Ok(())
    };
    let x = Inputs::new();
    for count in [0, 2] {
        let mut v = Inputs::new();
        v.vararg_count = Some(count);
        add(
            &format!("vararg_scalar_branch_{count}"),
            &v,
            Block(vec![ret(vec![
                v.choose(ast::VarArg.into(), v.emit("no", nil()))
            ])]),
            false,
            None,
        )?;
        add(
            &format!("vararg_open_tail_{count}"),
            &v,
            Block(vec![ret(vec![v.choice(), ast::VarArg.into()])]),
            false,
            None,
        )?;
        add(
            &format!("vararg_prefix_{count}"),
            &v,
            Block(vec![ret(vec![ast::VarArg.into(), v.choice()])]),
            false,
            None,
        )?;
        add(
            &format!("vararg_single_tail_{count}"),
            &v,
            Block(vec![ret(vec![
                v.choice(),
                RValue::Select(ast::Select::VarArg(ast::VarArg)),
            ])]),
            false,
            None,
        )?;
    }
    add(
        "scalar_return",
        &x,
        Block(vec![ret(vec![x.choice()])]),
        false,
        None,
    )?;
    add(
        "tuple_prefix",
        &x,
        Block(vec![ret(vec![x.emit("prefix", number(4.)), x.choice()])]),
        false,
        None,
    )?;
    add(
        "tuple_open_tail",
        &x,
        Block(vec![ret(vec![x.choice(), x.emit("tail", number(3.))])]),
        false,
        None,
    )?;
    add(
        "tuple_single_tail",
        &x,
        Block(vec![ret(vec![
            x.choice(),
            RValue::Select(ast::Select::Call(Call::new(
                x.p(1),
                vec![string("tail"), number(3.)],
            ))),
        ])]),
        false,
        None,
    )?;
    add(
        "nested_select",
        &x,
        Block(vec![ret(vec![x.choose(
            x.choose(x.emit("yes", nil()), x.emit("unused", number(9.))),
            x.choice(),
        )])]),
        false,
        None,
    )?;
    let chosen = local("chosen");
    add(
        "multiple_result_uses",
        &x,
        Block(vec![
            assign(&chosen, x.choice(), true),
            ret(vec![read(&chosen), read(&chosen)]),
        ]),
        false,
        None,
    )?;
    add(
        "call_arguments",
        &x,
        Block(vec![ret(vec![Call::new(
            x.p(3),
            vec![
                x.emit("prefix", number(1.)),
                x.choice(),
                x.emit("tail", number(2.)),
            ],
        )
        .into()])]),
        false,
        None,
    )?;
    add(
        "conditional_callee",
        &x,
        Block(vec![ret(vec![Call::new(
            x.choose(x.p(3), x.p(5)),
            vec![x.emit("argument", number(3.))],
        )
        .into()])]),
        false,
        None,
    )?;
    for captured in [false, true] {
        let label = if captured { "capture" } else { "frame" };
        let mutation = x.choose(x.mutation(3, x.p(5), number(3.)), x.emit("no", number(4.)));
        add(
            &format!("callee_{label}"),
            &x,
            Block(vec![ret(vec![Call::new(x.p(3), vec![mutation]).into()])]),
            captured,
            None,
        )?;
        let mutation = x.choose(x.mutation(4, x.p(6), number(3.)), x.emit("no", number(4.)));
        add(
            &format!("method_{label}"),
            &x,
            Block(vec![ret(vec![MethodCall::new(
                x.p(4),
                "consume".into(),
                vec![mutation, x.emit("tail", number(8.))],
            )
            .into()])]),
            captured,
            captured.then_some("captured_register_reuse"),
        )?;
        let mutation = x.choose(
            x.mutation(4, x.p(6), string("Value")),
            x.emit("no", string("Value")),
        );
        add(
            &format!("index_{label}"),
            &x,
            Block(vec![ret(vec![Index::new(x.p(4), mutation).into()])]),
            captured,
            captured.then_some("captured_register_reuse"),
        )?;
        for operation in [Op::Add, Op::Concat] {
            let mutation = x.choose(
                x.mutation(2, number(99.), number(3.)),
                x.emit("no", number(4.)),
            );
            add(
                &format!("binary_{operation:?}_{label}"),
                &x,
                Block(vec![ret(vec![
                    Binary::new(x.p(2), mutation, operation).into()
                ])]),
                captured,
                (captured && operation != Op::Concat).then_some("captured_register_reuse"),
            )?;
        }
    }
    add(
        "captured_pure_arms",
        &x,
        Block(vec![ret(vec![Binary::new(
            x.p(2),
            x.choose(number(3.), number(4.)),
            Op::Add,
        )
        .into()])]),
        true,
        None,
    )?;
    add(
        "method_dynamic_receiver",
        &x,
        Block(vec![ret(vec![MethodCall::new(
            Call::new(x.p(8), vec![]).into(),
            "consume".into(),
            vec![x.choice()],
        )
        .into()])]),
        false,
        None,
    )?;
    for op in [Op::And, Op::Or] {
        add(
            &format!("short_circuit_{op:?}"),
            &x,
            Block(vec![ret(vec![Binary::new(x.p(0), x.choice(), op).into()])]),
            false,
            None,
        )?;
    }
    let condition = x.choose(
        x.emit("condition", Literal::Boolean(true).into()),
        x.emit("condition", Literal::Boolean(false).into()),
    );
    add(
        "while_continue",
        &x,
        Block(vec![
            ast::While::new(
                condition,
                Block(vec![
                    Call::new(x.p(1), vec![string("body"), nil()]).into(),
                    If::new(
                        x.p(0),
                        Block(vec![ast::Continue {}.into()]),
                        Block::default(),
                    )
                    .into(),
                    Call::new(x.p(1), vec![string("after-continue"), nil()]).into(),
                ]),
            )
            .into(),
            ret(vec![number(1.)]),
        ]),
        false,
        None,
    )?;
    let condition = x.choose(
        x.emit("done", Literal::Boolean(true).into()),
        x.emit("done", Literal::Boolean(false).into()),
    );
    add(
        "repeat_condition",
        &x,
        Block(vec![
            ast::Repeat::new(
                condition.clone(),
                Block(vec![Call::new(x.p(1), vec![string("body"), nil()]).into()]),
            )
            .into(),
            ret(vec![number(1.)]),
        ]),
        false,
        None,
    )?;
    add(
        "repeat_continue_refusal",
        &x,
        Block(vec![
            ast::Repeat::new(condition.clone(), Block(vec![ast::Continue {}.into()])).into(),
            ret(vec![number(1.)]),
        ]),
        false,
        Some("repeat_continue"),
    )?;
    add(
        "repeat_return_refusal",
        &x,
        Block(vec![ast::Repeat::new(
            condition,
            Block(vec![ret(vec![number(2.)])]),
        )
        .into()]),
        false,
        Some("repeat_terminal_return"),
    )?;
    let counter = local("index");
    add(
        "numeric_for_bounds",
        &x,
        Block(vec![
            ast::NumericFor::new(
                x.emit("start", number(1.)),
                x.choose(x.emit("limit", number(2.)), x.emit("limit", number(3.))),
                x.emit("step", number(1.)),
                counter.clone(),
                Block(vec![Call::new(
                    x.p(1),
                    vec![string("body"), read(&counter)],
                )
                .into()]),
            )
            .into(),
            ret(vec![number(1.)]),
        ]),
        false,
        None,
    )?;
    add(
        "generic_for_iterator",
        &x,
        Block(vec![
            ast::GenericFor::new(
                vec![counter.clone()],
                vec![x.choose(x.p(7), x.p(7))],
                Block(vec![Call::new(
                    x.p(1),
                    vec![string("body"), read(&counter)],
                )
                .into()]),
            )
            .into(),
            ret(vec![number(1.)]),
        ]),
        false,
        None,
    )?;
    add(
        "table_refusal",
        &x,
        Block(vec![ret(vec![ast::Table(vec![(None, x.choice())]).into()])]),
        false,
        Some("table_constructor_order"),
    )?;
    add(
        "store_refusal",
        &x,
        Block(vec![
            Assign::new(
                vec![LValue::Index(Index::new(x.p(4), string("Value")))],
                vec![x.choice()],
            )
            .into(),
            ret(vec![number(1.)]),
        ]),
        false,
        Some("statement_store_or_internal_control"),
    )?;
    let mut body = (0..184)
        .map(|i| assign(&local(&format!("occupied{i}")), nil(), true))
        .collect::<Vec<_>>();
    body.push(ret(vec![x.choice()]));
    add(
        "local_budget_refusal",
        &x,
        Block(body),
        false,
        Some("local_budget"),
    )?;
    // A nested parameter/global name may not be shadowed by an outer temporary.
    let collision = local("selectedValue1");
    add(
        "name_collision",
        &x,
        Block(vec![ret(vec![
            x.choice(),
            Call::new(
                closure(
                    vec![collision.clone()],
                    Block(vec![ret(vec![
                        read(&collision),
                        RValue::Global(Global::from("selectedValue2")),
                    ])]),
                    &[],
                ),
                vec![number(17.)],
            )
            .into(),
        ])]),
        false,
        None,
    )?;
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({"schema_version":1,"cases":cases}))?,
    )?;
    println!("{} fixtures", cases.len());
    Ok(())
}
