#!/usr/bin/env python3
"""Seeded, bounded source round-trips with a grammar-preserving failure reducer.

The generator version and seeds are recorded. Generated source is the runtime
reference, never the decompiler's output. Reduction deletes independent grammar
units and accepts only the same observed failure category; it has a hard budget.
"""
import argparse
import collections
import json
import pathlib
import random
import tempfile

from roadmap_v2 import checked, compile_source, observation, sha256
from bytecode_roundtrip import parse_chunk
from bytecode_dataflow import compare_dataflow


VERSION = "scalar-branch-table-capture-loop-v1"
PRELUDE = '''return function(input, flip)
    local events = {}
    local value = input
    local box = { value = input }
    local function tap(label, item)
        events[#events + 1] = label
        return item
    end
    local function read() return value end
'''
ENDING = '''    return value, 1 / value, box.value, table.concat(events, ",")
end
'''
DRIVER = '''local f = require("./MODULE")
local function describe(v)
    if type(v) ~= "number" then return type(v) .. ":" .. tostring(v) end
    if v ~= v then return "nan" end
    if v == 0 then return 1 / v < 0 and "-zero" or "+zero" end
    return tostring(v)
end
for _, input in {-3, -0.0, 0, 2, math.huge, -math.huge, 0/0} do
    for _, flip in {false, true} do
        local values = table.pack(pcall(f, input, flip))
        local text = {}
        for index = 1, values.n do text[index] = describe(values[index]) end
        print(values.n, table.concat(text, "|"))
    end
end
'''


def generate(seed, units=8):
    rng = random.Random(seed)
    result = []
    for index in range(units):
        amount = rng.randint(-4, 4)
        choices = [
            f'value = value + ({amount})',
            f'value = if flip or input < 0 then tap("t{index}", value * 2) else tap("f{index}", value - 3)',
            f'box.value = value\nlocal snapshot = box.value\nvalue = snapshot + ({amount})',
            f'''local frozen = value
local function getter() return frozen, value end
value = value + ({amount})
local first, second = getter()
value = first - second''',
            f'for index = 1, {rng.randint(1, 4)} do value = value + tap("l{index}" .. index, input * index) end',
            'local selected = if flip then false else nil\nif selected == false then value = value + 1 end',
            f'value = value + ({amount})\nvalue = read()',
        ]
        # Each unit has its own lexical scope, so deletion does not leave dangling
        # definitions or accidentally merge shadowed locals in the reducer.
        result.append("    do\n        " + rng.choice(choices).replace("\n", "\n        ") + "\n    end\n")
    return result


def reduce_units(units, predicate, budget=40):
    """Delete whole scope units while preserving the caller's failure predicate."""
    result, attempts, index = list(units), 0, 0
    while index < len(result) and attempts < budget:
        candidate = result[:index] + result[index + 1:]
        attempts += 1
        if predicate(candidate):
            result = candidate
            index = 0
        else:
            index += 1
    return result, attempts


def same_observation(left, right):
    return all(left[key] == right[key] for key in ("exit", "stdout", "stderr"))


def check(args, units, directory, opt, debug):
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / "source.luau"
    source.write_text(PRELUDE + "".join(units) + ENDING, encoding="utf-8", newline="\n")
    row = {"status": "failed", "failure": "source_compile", "source_sha256": sha256(source)}
    try:
        raw = compile_source(args, source, opt, debug)
        (directory / "input.luaubc").write_bytes(raw)
        row["failure"] = "decompile"
        output, elapsed = checked([args.lifter, directory / "input.luaubc", "--strict-no-synthetic-control"], timeout=args.timeout)
        emitted = directory / "output.luau"
        emitted.write_bytes(output)
        row.update(output_sha256=sha256(emitted), decompile_seconds=elapsed)
        row["failure"] = "recompile"
        rebuilt = compile_source(args, emitted, opt, debug)
        row["dataflow"] = compare_dataflow(parse_chunk(raw, 1), parse_chunk(rebuilt, 1))
        row["failure"] = "runtime"
        observations = {}
        for variant in ("source", "output"):
            driver = directory / f"{variant}_driver.luau"
            driver.write_text(DRIVER.replace("MODULE", variant), encoding="utf-8", newline="\n")
            observations[variant] = observation([args.luau, driver], timeout=args.timeout)
        row["observations"] = observations
        if observations["source"]["exit"] != 0:
            row["failure"] = "invalid_reference_runtime"
        elif not same_observation(observations["source"], observations["output"]):
            row["failure"] = "runtime_mismatch"
        else:
            row.update(status="passed", failure=None)
    except Exception as error:
        row["error"] = str(error)
    return row


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("compiler", "lifter", "luau"):
        parser.add_argument(f"--{name}", type=pathlib.Path, required=True)
    parser.add_argument("--seed-start", type=int, default=0)
    parser.add_argument("--seeds", type=int, default=12)
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--reduce", action="store_true")
    parser.add_argument("--keep", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()
    for name in ("compiler", "lifter", "luau"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    if args.seeds < 1:
        parser.error("--seeds must be positive")
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="generated-", dir=args.keep)).resolve()
    rows = []
    for seed in range(args.seed_start, args.seed_start + args.seeds):
        units = generate(seed)
        for opt in (0, 1, 2):
            for debug in (1, 2):
                directory = work / f"seed{seed}_O{opt}_g{debug}"
                row = dict(check(args, units, directory, opt, debug), seed=seed, opt=opt, debug=debug)
                if args.reduce and row["status"] == "failed" and row["failure"] not in ("source_compile", "invalid_reference_runtime"):
                    counter = [0]
                    def predicate(candidate):
                        counter[0] += 1
                        attempt = check(args, candidate, directory / f"reduce{counter[0]}", opt, debug)
                        return attempt["failure"] == row["failure"]
                    reduced, attempts = reduce_units(units, predicate)
                    path = directory / "reduced.luau"
                    path.write_text(PRELUDE + "".join(reduced) + ENDING, encoding="utf-8", newline="\n")
                    row["reducer"] = dict(attempts=attempts, original_units=len(units), reduced_units=len(reduced), source=str(path))
                rows.append(row)
        print(f"seed {seed}: {collections.Counter(r['status'] for r in rows if r['seed'] == seed)}", flush=True)
    report = {"schema_version": 1, "generator": VERSION, "generator_sha256": sha256(pathlib.Path(__file__)),
              "tools": {name: {"path": str(getattr(args, name)), "sha256": sha256(getattr(args, name))}
                        for name in ("compiler", "lifter", "luau")}, "work": str(work), "rows": rows,
              "summary": dict(collections.Counter(r["status"] for r in rows)),
              "limitations": "Finite generated grammar and runtime vectors; no exhaustive equivalence claim. Reducer preserves failure category, not necessarily root cause."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(report["summary"]))
    return int(any(row["status"] != "passed" for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
