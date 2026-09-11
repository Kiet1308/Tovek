#!/usr/bin/env python3
"""Check finite-loop syntax, preserved helper calls and pinned compiler witnesses."""
from __future__ import annotations

import argparse
import json
import pathlib

from arithmetic_witness import load_report, local_calls
from roadmap_v2 import checked, sha256
from source_fidelity import parse_ast


def numeric_loops(value):
    if isinstance(value, list):
        return [node for item in value for node in numeric_loops(item)]
    if not isinstance(value, dict):
        return []
    return ([value] if value.get("type") == "AstStatFor" else []) + [
        node for item in value.values() for node in numeric_loops(item)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=pathlib.Path, required=True)
    parser.add_argument("--after", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()
    before, before_rows = load_report(args.before)
    after, after_rows = load_report(args.after)
    for key in ("compiler_commit_expected", "manifest_sha256"):
        if before[key] != after[key]:
            raise ValueError(f"mismatched {key}")
    for tool in ("compiler", "luau", "ast"):
        if before["tools"][tool]["sha256"] != after["tools"][tool]["sha256"] \
                or sha256(pathlib.Path(after["tools"][tool]["path"])) != after["tools"][tool]["sha256"]:
            raise ValueError(f"tool changed: {tool}")
    compiler, ast = (after["tools"][key]["path"] for key in ("compiler", "ast"))
    rows = []
    cases = {"helper_loop": ("adjust", 2), "unrolled_effects": (None, 0),
             "unrolled_capture": (None, 0), "sum_helper": ("weightedSum", 2)}
    for case, (helper, calls) in cases.items():
        for opt in ((0, 1, 2) if case == "unrolled_capture" else (2,)):
            for debug in (1, 2):
                key = (case, opt, debug)
                old, new = before_rows[key], after_rows[key]
                if old["status"] != "passed" or new["status"] != "passed":
                    raise ValueError(f"runtime failure: {key}")
                directory = f"{case}_O{opt}_g{debug}"
                source = pathlib.Path(after["work"]) / directory / "source.luau"
                emitted = pathlib.Path(after["work"]) / directory / "output.luau"
                previous = pathlib.Path(before["work"]) / directory / "output.luau"
                for path, digest in ((source, new["source_sha256"]), (emitted, new["output_sha256"]),
                                     (previous, old["output_sha256"])):
                    if sha256(path) != digest:
                        raise ValueError(f"artifact changed: {path}")
                if old["source_sha256"] != new["source_sha256"]:
                    raise ValueError(f"source changed: {key}")
                original_ast, before_ast, after_ast = (parse_ast(ast, path) for path in (source, previous, emitted))
                loops = numeric_loops(after_ast)
                expected_loops = int(case != "sum_helper")
                if numeric_loops(before_ast) or len(loops) != expected_loops:
                    raise ValueError(f"unexpected loop count: {key}")
                for loop in loops:
                    if loop["from"].get("value") != 1 or loop["to"].get("value") != 4 or loop.get("step"):
                        raise ValueError(f"unexpected loop bounds: {key}")
                if helper and (local_calls(before_ast, helper), local_calls(after_ast, helper)) != (calls, calls):
                    raise ValueError(f"helper calls lost: {key}")
                if case == "sum_helper" and old["output_sha256"] != new["output_sha256"]:
                    raise ValueError(f"existing helper changed: {key}")
                output = emitted.read_text(encoding="utf-8")
                if ("equivalent fixed-count loop synthesized; original loop unknown" in output) != bool(expected_loops):
                    raise ValueError(f"synthesis classification missing: {key}")
                if old["dataflow"]["status"] == "proved" and new["dataflow"]["status"] != "proved":
                    raise ValueError(f"lost dataflow proof: {key}")
                command = [compiler, "--text", f"-O{opt}", f"-g{debug}", "--fflags=false"]
                original_text = checked([*command, source], timeout=30)[0].decode("utf-8")
                rebuilt_text = checked([*command, emitted], timeout=30)[0].decode("utf-8")
                original_unrolls = original_text.count("REMARK loop unroll succeeded")
                rebuilt_unrolls = rebuilt_text.count("REMARK loop unroll succeeded")
                if original_unrolls != int(opt == 2 and case in ("helper_loop", "unrolled_effects")) \
                        or rebuilt_unrolls != int(opt == 2 and bool(expected_loops)):
                    raise ValueError(f"compiler witness changed: {key}: {original_unrolls} -> {rebuilt_unrolls}")
                rows.append(dict(case=case, opt=opt, debug=debug, source_sha256=new["source_sha256"],
                    before_output_sha256=old["output_sha256"], output_sha256=new["output_sha256"],
                    source_loops=len(numeric_loops(original_ast)), before_loops=0, after_loops=len(loops),
                    helper_calls_preserved=calls, original_unrolls=original_unrolls, rebuilt_unrolls=rebuilt_unrolls,
                    original_disassembly=original_text, recompiled_disassembly=rebuilt_text,
                    before_source=previous.read_text(encoding="utf-8"), after_source=output,
                    dataflow_before=old["dataflow"], dataflow_after=new["dataflow"],
                    source_fidelity_before=old["source_fidelity"], source_fidelity_after=new["source_fidelity"],
                    runtime="passed"))
    report = dict(schema_version=1, compiler_commit=after["compiler_commit_expected"], tools=after["tools"],
        before_report_sha256=sha256(args.before), after_report_sha256=sha256(args.after), rows=rows,
        before_lifter_args=before.get("lifter_args", []), after_lifter_args=after.get("lifter_args", []),
        limitations="Development witnesses, not independent precision/recall. Loops are classified as synthesis. "
                    "unrolled_capture has no source loop and retains unknown dataflow; runtime traces are finite evidence.")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"{len(rows)} finite-loop compiler witnesses passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
