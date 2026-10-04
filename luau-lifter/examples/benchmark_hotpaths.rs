//! Focused production-path microbenchmarks. Build this identical example in
//! baseline and candidate trees; scripts/benchmark_hotpaths.py checks their
//! output hashes and alternates process order. Fixture creation, output checks
//! and result disposal are outside each measured interval.
use ast::{Assign, Binary, BinaryOperation, Block, Call, Global, If, Literal, Local, MethodCall, RValue, RcLocal, Return, Select, Table};
use sha2::{Digest, Sha256};
use std::time::Instant;

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn string(text: &str) -> RValue { Literal::String(text.as_bytes().to_vec()).into() }
fn global(text: &str) -> RValue { Global::from(text).into() }

fn fixture(mode: &str, size: usize) -> Block {
    ast::reset_local_ids();
    if mode.starts_with("interpolation-") {
        let format = match mode {
            "interpolation-specifier" => "%* %d",
            "interpolation-control" => "%*\u{0001}",
            "interpolation-arity" => "%* %*",
            "interpolation-valid" => "%*",
            _ => panic!("unknown mode: {mode}"),
        };
        let mut value = string("leaf");
        for _ in 0..size {
            value = RValue::Select(Select::MethodCall(MethodCall::new(
                string(format), "format".into(), vec![value],
            )));
        }
        return vec![Return::new(vec![value]).into()].into();
    }
    if mode == "formatter-tail" {
        let mut block: Block = vec![Call::new(global("sink"), vec![]).into()].into();
        for _ in 0..size {
            block = vec![
                If::new(global("condition"), block, Block::default()).into(),
                Call::new(Binary::new(global("f"), global("g"), BinaryOperation::Or).into(), vec![]).into(),
            ].into();
        }
        return block;
    }
    assert!(matches!(mode, "table-unique" | "table-duplicate"));
    // A generated single-use temp forces the public UI reconstruction entry
    // point to run duplicate merging, including when every table key is unique.
    let temp = RcLocal::new(Local::new(Some("v".into())));
    let mut assign = Assign::new(vec![temp.clone().into()], vec![Literal::Number(7.0).into()]);
    assign.prefix = true;
    let mut fields = (0..size).map(|i| (Some(string(&format!("key{i}"))), Literal::Number(0.0).into())).collect::<Vec<_>>();
    if mode == "table-duplicate" {
        fields.extend((0..size).map(|i| (Some(string(&format!("key{i}"))), Literal::Number((i + 1) as f64).into())));
    }
    fields.push((Some(string("inlined")), temp.into()));
    vec![assign.into(), Return::new(vec![Table::new(fields).into()]).into()].into()
}

fn main() -> anyhow::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    anyhow::ensure!(args.len() >= 3, "usage: benchmark_hotpaths MODE SIZE [ITERATIONS]");
    let mode = &args[1];
    let size: usize = args[2].parse()?;
    let iterations: usize = args.get(3).map_or(Ok(5), |s| s.parse())?;
    anyhow::ensure!(size > 0 && (1..=1000).contains(&iterations), "invalid size/iterations");
    let mut rows = Vec::new();
    let mut expected = None;
    let mut bytes = 0;
    for iteration in 0..=iterations {
        let mut block = fixture(mode, size);
        let is_table = mode.starts_with("table-");
        let started = Instant::now();
        let output = if is_table {
            assert!(ast::inline_temps::rebuild_ui_expression_trees(&mut block));
            None
        } else { Some(std::hint::black_box(&block).to_string()) };
        let nanoseconds = started.elapsed().as_nanos();
        let output = output.unwrap_or_else(|| block.to_string());
        if is_table {
            assert_eq!(block.0.len(), 1, "single-use temp must inline");
            let ast::Statement::Return(ret) = &block.0[0] else { panic!("expected return") };
            let RValue::Table(table) = &ret.values[0] else { panic!("expected table") };
            assert_eq!(table.0.len(), size + 1, "duplicate keys must merge");
        }
        let hash = format!("{:x}", Sha256::digest(output.as_bytes()));
        if let Some(expected) = &expected { anyhow::ensure!(*expected == hash, "nondeterministic output"); }
        expected = Some(hash);
        bytes = output.len();
        rows.push(serde_json::json!({"iteration":iteration,"warmup":iteration == 0,"nanoseconds":nanoseconds}));
    }
    println!("{}", serde_json::json!({"schema_version":1,"mode":mode,"size":size,"iterations":iterations,
        "output_sha256":expected,"output_bytes":bytes,"rows":rows,
        "timing_contract":if mode.starts_with("table-") { "Public rebuild_ui_expression_trees; fixture construction, formatting, hashing and disposal excluded." } else { "AST to_string including output allocation; fixture construction, hashing and disposal excluded." }}));
    Ok(())
}
