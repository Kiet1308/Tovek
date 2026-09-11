//! Emit actual pass-boundary inputs, including a pinned-compiler register witness.
use ast::{Assign, Block, Call, Closure, Function, Global, If, Index, LValue, Literal,
          Local, NumericFor, RValue, RcLocal, Return, Statement};
use by_address::ByAddress;
use parking_lot::Mutex;
use std::{fs, path::PathBuf};
use triomphe::Arc;

fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
fn global(name: &str) -> RValue { Global::from(name).into() }
fn number(value: f64) -> RValue { Literal::Number(value).into() }
fn string(value: &str) -> RValue { Literal::from(value).into() }
fn field(base: RValue, key: &str) -> RValue { Index::new(base, string(key)).into() }
fn wait(value: f64) -> Statement {
    Call::new(field(global("task"), "wait"), vec![number(value)]).into()
}
fn closure(parameters: Vec<RcLocal>, body: Block) -> RValue {
    Closure { function: ByAddress(Arc::new(Mutex::new(Function {
        parameters, body, ..Default::default()
    }))), upvalues: vec![] }.into()
}
fn returning(value: RValue) -> Statement { Return::new(vec![value]).into() }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::args_os().nth(1).ok_or("expected fresh output directory")?);
    fs::create_dir(&root)?;
    let mut cases = Vec::new();
    let mut add = |name: &str, parameters: usize, body: Block, expected: usize|
        -> Result<(), Box<dyn std::error::Error>> {
        let params = (0..parameters).map(|i| local(&format!("p{i}"))).collect();
        let mut module = Block(vec![returning(closure(params, body))]);
        let source = module.to_string();
        let introduced = ast::rehoist_constants::rehoist_constants(&mut module);
        let output = module.to_string();
        let dir = root.join(name);
        fs::create_dir(&dir)?;
        fs::write(dir.join("source.luau"), source)?;
        fs::write(dir.join("output.luau"), output)?;
        cases.push(serde_json::json!({"case": name, "parameters": parameters,
            "introduced": introduced, "expected_introduced": expected}));
        Ok(())
    };
    // The arguments are globals to avoid constructing mismatched RcLocal IDs.
    // A global read costs no fewer registers than the identical local witness.
    let durations = || Block(vec![wait(1.0), wait(1.0), wait(1.0),
        Call::new(field(global("task"), "delay"), vec![number(2.0), global("callback")]).into(),
        Call::new(field(global("task"), "delay"), vec![number(2.0), global("callback")]).into(),
        Call::new(field(global("task"), "delay"), vec![number(2.0), global("callback")]).into()]);
    let mut pressure = durations();
    pressure.0.push(Call::new(global("sink"), vec![global("argument"); 73]).into());
    add("register_pressure", 180, pressure, 0)?;
    add("ordinary_durations", 0, durations(), 2)?;
    add("local_pressure", 200, Block(vec![wait(1.0); 3]), 0)?;
    let mut loop_body = Block(vec![wait(1.0); 3]);
    for i in 0..15 {
        loop_body = Block(vec![NumericFor {
            counter: local(&format!("i{i}")), initial: number(1.0), limit: number(1.0),
            step: number(1.0), block: Arc::new(Mutex::new(loop_body)),
        }.into()]);
    }
    add("hidden_loop_registers", 180, loop_body, 0)?;
    add("branch_order", 0, Block(vec![wait(1.0), If::new(global("flag"),
        Block(vec![wait(1.0), wait(1.0)]), Block(vec![wait(2.0)])).into()]), 1)?;
    add("signed_zero", 0, Block(vec![wait(0.0), wait(-0.0), wait(0.0),
        wait(-0.0), wait(0.0), wait(-0.0)]), 2)?;
    add("different_roles", 0, Block(vec![wait(1.0), wait(1.0),
        Call::new(field(global("task"), "delay"), vec![number(1.0)]).into()]), 0)?;
    let mut collision = Block(vec![wait(1.0); 3]);
    collision.0.push(returning(closure(vec![], Block(vec![returning(global("WAIT_INTERVAL"))]))));
    add("descendant_global", 0, collision, 1)?;
    add("separate_scopes", 0, Block(vec![wait(1.0),
        returning(closure(vec![], Block(vec![wait(1.0), wait(1.0)])))]), 0)?;
    let asset = || Assign::new(vec![LValue::Index(Index::new(global("object"), string("SoundId")))],
        vec![string("rbxassetid://123\0\r\n]=]")]).into();
    add("asset_property_order", 0, Block(vec![asset(), asset(), asset()]), 1)?;
    fs::write(root.join("manifest.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1, "cases": cases
    }))?)?;
    Ok(())
}
