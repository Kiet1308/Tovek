#!/usr/bin/env python3
"""Execute the production bytecode reader on wasm32, without a browser/server.

Requires the pinned Rust toolchain, wasm32-unknown-unknown and Node. Builds only
the real deserializer/opcode modules in a small temporary cdylib; no copied
parser implementation and no network requests at execution time.
"""
import argparse
import json
import pathlib
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent

PROBE = r'''
fn varint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (n & 127) as u8;
        n >>= 7;
        out.push(byte | if n == 0 { 0 } else { 128 });
        if n == 0 { break; }
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn pointer_bits() -> u32 { usize::BITS }
#[unsafe(no_mangle)]
pub extern "C" fn check_v12_costs() -> u32 {
    let values = [0u64, 127, 128, 1 << 32, 1 << 63, u64::MAX];
    for (index, cost) in values.into_iter().enumerate() {
        let mut first = vec![1, 0, 0, 0, 8, 0, 1];
        first.extend_from_slice(&[22, 0, 1, 0]); // RETURN, no values
        first.extend_from_slice(&[0; 7]); // constants/children/debug/feedback
        varint(cost, &mut first);
        first.extend_from_slice(&[0xa5, 0xff, 0x80]); // unknown extension
        let mut second = vec![1, 0, 0, 0, 0, 0, 2];
        second.extend_from_slice(&[4, 0, 43, 0, 22, 0, 2, 0]);
        second.extend_from_slice(&[0; 7]);
        let mut bytes = vec![12, 3, 0, 0, 2];
        for body in [first, second] {
            varint(body.len() as u64, &mut bytes);
            bytes.extend(body);
        }
        bytes.push(1); // main is second prototype
        let Ok(deserializer::bytecode::Bytecode::Chunk(chunk)) =
            deserializer::deserialize(&bytes, 1) else { return 10 + index as u32; };
        if chunk.main != 1 || chunk.functions.len() != 2 { return 20 + index as u32; }
        if !matches!(chunk.functions[1].instructions[0], instruction::Instruction::AD {
            op_code: op_code::OpCode::LOP_LOADN, d: 43, ..
        }) { return 30 + index as u32; }
    }
    0
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--toolchain", default="nightly-2024-12-15")
    parser.add_argument("--keep", type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="wasm-reader-", dir=args.keep.resolve()))
    (work / "Cargo.toml").write_text('''[package]
name = "v12-reader-probe"
version = "0.0.0"
edition = "2024"
[workspace]
[lib]
path = "probe.rs"
crate-type = ["cdylib"]
[dependencies]
nom = "7.1.0"
nom-leb128 = "0.2.0"
num_enum = "0.5.6"
''', encoding="utf-8")
    modules = {"deserializer": "deserializer/mod.rs", "instruction": "instruction.rs", "op_code": "op_code.rs"}
    includes = "\n".join(f'#[path = {json.dumps((ROOT / "luau-lifter/src" / file).as_posix())}] mod {name};'
                         for name, file in modules.items())
    (work / "probe.rs").write_text(includes + "\n" + PROBE, encoding="utf-8")
    with (work / "build.log").open("w", encoding="utf-8") as log:
        subprocess.run(["cargo", "+" + args.toolchain, "build", "--offline", "--release",
                        "--target", "wasm32-unknown-unknown", "--manifest-path", str(work / "Cargo.toml")],
                       stdout=log, stderr=subprocess.STDOUT, check=True, timeout=180)
    runner = work / "run.cjs"
    runner.write_text('''const fs = require('fs');
const moduleBytes = fs.readFileSync(process.argv[2]);
const mod = new WebAssembly.Module(moduleBytes);
const instance = new WebAssembly.Instance(mod, {});
const result = {pointer_bits: instance.exports.pointer_bits(),
                result: instance.exports.check_v12_costs(), cost_cases: 6};
console.log(JSON.stringify(result));
if (result.pointer_bits !== 32 || result.result !== 0) process.exit(1);
''', encoding="utf-8")
    wasm = work / "target/wasm32-unknown-unknown/release/v12_reader_probe.wasm"
    result = subprocess.run(["node", str(runner), str(wasm)], capture_output=True, text=True, timeout=30)
    (work / "result.json").write_text(result.stdout, encoding="utf-8")
    print(result.stdout.strip())
    print(f"Evidence: {work}")
    if result.returncode:
        print(result.stderr)
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
