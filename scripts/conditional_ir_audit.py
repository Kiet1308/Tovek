#!/usr/bin/env python3
"""Compile and run independently emitted original/lowered IR fixture pairs.

Unlike a decompiler roundtrip, these inputs contain actual IfExpression nodes
at the pass boundary. Run the original and lowered source at every pinned
optimization/debug profile; retain unknown dataflow and explicit refusals.
"""
import argparse
import collections
import json
import pathlib
import re
import subprocess

from bytecode_dataflow import compare_dataflow
from bytecode_roundtrip import parse_chunk
from roadmap_v2 import ROOT, sha256, parse_ast, conditional_count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("fixtures", "compiler", "luau", "ast", "report"):
        parser.add_argument("--" + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.fixtures = args.fixtures.resolve(strict=True)
    manifest = json.loads((args.fixtures / "manifest.json").read_text(encoding="utf-8"))
    driver_path = ROOT / "docs/failure_fixtures/conditional_ir.driver.luau"
    driver = driver_path.read_text(encoding="utf-8")
    rows = []
    for case in manifest["cases"]:
        name = case["case"]
        extra = case.get("extra_arguments", 0)
        if type(extra) is not int or extra not in (0, 2) or driver.count("--[[IR_EXTRA_ARGS]]") != 1:
            raise ValueError("invalid variadic driver profile")
        case_driver = driver.replace("--[[IR_EXTRA_ARGS]]", ', "vararg", nil' if extra else '')
        directory = args.fixtures / name
        if directory.parent != args.fixtures or not directory.is_dir():
            raise ValueError("invalid fixture name")
        paths = [directory / (variant + ".luau") for variant in ("source", "output")]
        counts = [conditional_count(parse_ast(args.ast, path, 30)) for path in paths]
        report = case["report"]
        if counts[0] != report["input_selects"] or counts[1] != counts[0] - report["lowered_selects"]:
            raise ValueError("parser/pass conditional counts disagree: " + name)
        for opt in range(3):
            for debug in (1, 2):
                row = dict(case=name, opt=opt, debug=debug, status="failed", report=report,
                           extra_arguments=extra,
                           source_sha256=sha256(paths[0]), output_sha256=sha256(paths[1]),
                           source_selects=counts[0], output_selects=counts[1])
                try:
                    binaries, observations = [], []
                    for variant, path in zip(("source", "output"), paths):
                        compiled = subprocess.run([str(args.compiler), "--binary", f"-O{opt}", f"-g{debug}",
                                                   "--fflags=false", str(path)], capture_output=True, timeout=30)
                        if compiled.returncode:
                            raise ValueError(compiled.stderr.decode("utf-8", errors="replace"))
                        binaries.append(parse_chunk(compiled.stdout, 1))
                        # Inline each module source in a wrapper, so -O/-g apply
                        # to it and the driver without require-cache ambiguity.
                        runner = directory / f"{variant}_O{opt}_g{debug}.runner.luau"
                        runner.write_text("local f = (function()\n" + path.read_text(encoding="utf-8") + "\nend)()\n" + case_driver,
                                          encoding="utf-8", newline="\n")
                        result = subprocess.run([str(args.luau), f"-O{opt}", f"-g{debug}", "--fflags=false", str(runner)],
                                                capture_output=True, timeout=10)
                        if result.returncode:
                            raise ValueError(result.stderr.decode("utf-8", errors="replace"))
                        observation = result.stdout.decode("utf-8")
                        if len(observation.splitlines()) != 128:
                            raise ValueError("driver vector count differs")
                        observations.append(observation)
                    row["dataflow"] = compare_dataflow(*binaries)
                    row["runtime"] = dict(source=observations[0], output=observations[1])
                    if observations[0] != observations[1]:
                        raise ValueError("observable result/arity/order/error differs")
                    row["status"] = "passed"
                except (ValueError, OSError, subprocess.SubprocessError) as error:
                    row["error"] = str(error)
                rows.append(row)
                print(f"{row['status']}: {name} O{opt} g{debug} {row.get('error','')}", flush=True)
    controls = []
    def late_callee(text):
        match = re.search(r"\tlocal (selectedValue\d+) = callback\n", text)
        if not match: raise ValueError("callee control fixture shape changed")
        return re.sub(r"\b" + match[1] + r"\b", "callback", text[:match.start()] + text[match.end():])

    def scalar_tail(text):
        call = 'emit("tail", 3)'
        if text.count(call) != 1: raise ValueError("tail control fixture shape changed")
        return text.replace(call, "(" + call + ")")

    def eager_arm(text):
        call = 'emit("no", false)'
        if text.count(call) != 1: raise ValueError("arm control fixture shape changed")
        text = text.replace(call, "eagerArm")
        first, rest = text.split("\n", 1)
        return first + '\n\tlocal eagerArm = emit("no", false)\n' + rest

    for name, case_name, mutate in [("late_callee", "callee_frame", late_callee),
                                   ("truncated_open_tail", "tuple_open_tail", scalar_tail),
                                   ("eager_unselected_arm", "scalar_return", eager_arm)]:
        directory = args.fixtures / case_name
        mutant = directory / (name + ".mutant.luau")
        mutant.write_text(mutate((directory / "output.luau").read_text(encoding="utf-8")), encoding="utf-8", newline="\n")
        for opt in range(3):
            for debug in (1, 2):
                original = next(row for row in rows if (row["case"], row["opt"], row["debug"]) == (case_name, opt, debug))
                control = dict(control=name, opt=opt, debug=debug, status="failed", mutant_sha256=sha256(mutant))
                try:
                    compiled = subprocess.run([str(args.compiler), "--binary", f"-O{opt}", f"-g{debug}", "--fflags=false", str(mutant)], capture_output=True, timeout=30)
                    if compiled.returncode: raise ValueError("mutant did not compile")
                    runner = directory / f"{name}_O{opt}_g{debug}.runner.luau"
                    runner.write_text("local f = (function()\n" + mutant.read_text(encoding="utf-8") + "\nend)()\n" + driver,
                                      encoding="utf-8", newline="\n")
                    observed = subprocess.run([str(args.luau), f"-O{opt}", f"-g{debug}", "--fflags=false", str(runner)], capture_output=True, timeout=10)
                    text = observed.stdout.decode("utf-8")
                    if observed.returncode or len(text.splitlines()) != 128 or original["status"] != "passed":
                        raise ValueError("control execution/reference failed")
                    if text == original["runtime"]["output"]: raise ValueError("observable mutation was not detected")
                    control.update(status="passed", observation=text)
                except (ValueError, OSError, subprocess.SubprocessError) as error:
                    control["error"] = str(error)
                controls.append(control)
    result = dict(schema_version=1, tools={name: dict(path=str(getattr(args, name)), sha256=sha256(getattr(args, name)))
                                          for name in ("compiler", "luau", "ast")},
                  driver_sha256=sha256(driver_path), manifest_sha256=sha256(args.fixtures / "manifest.json"),
                  rows=rows, summary=dict(collections.Counter(row["status"] for row in rows)),
                  dataflow=dict(collections.Counter(row.get("dataflow", {}).get("status", "unavailable") for row in rows)),
                  controls=controls, control_summary=dict(collections.Counter(row["status"] for row in controls)),
                  contract="Actual AST pass pairs; VM O0/O1/O2 and g1/g2 with all fast flags false. "
                           "64 vectors per configuration cover nil/false/truthy values, result packs, mutation and caught errors. "
                           "Finite runtime observations do not promote unknown whole-chunk certificates.")
    args.report.write_text(json.dumps(result, indent=1) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(result["summary"]))
    return int(any(row["status"] != "passed" for row in [*rows, *controls]))


if __name__ == "__main__":
    raise SystemExit(main())
