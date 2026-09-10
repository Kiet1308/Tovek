#!/usr/bin/env python3
"""Audit the bounded arithmetic de-inline family against pinned compiler witnesses.

Requires passing before/after runtime matrices. Compiler remarks and syntax call
counts are source evidence, never a replacement for the runtime/dataflow gates.
"""
from __future__ import annotations

import argparse
import json
import pathlib

from roadmap_v2 import checked, sha256
from source_fidelity import parse_ast


def local_calls(value, name):
    if isinstance(value, list):
        return sum(local_calls(child, name) for child in value)
    if not isinstance(value, dict):
        return 0
    callee = value.get("func", {})
    own = value.get("type") == "AstExprCall" and callee.get("type") == "AstExprLocal" \
        and callee["local"]["name"] == name
    return int(own) + sum(local_calls(child, name) for child in value.values())


def load_report(path):
    report = json.loads(path.read_text(encoding="utf-8"))
    if report["summary"]["status"] != {"passed": len(report["cases"])} \
            or report["summary"]["controls"] != {"passed": 9}:
        raise ValueError(f"runtime matrix did not pass: {path}")
    return report, {(row["case"], row["opt"], row["debug"]): row for row in report["cases"]}


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
        if before["tools"][tool]["sha256"] != after["tools"][tool]["sha256"]:
            raise ValueError(f"mismatched {tool}")
        if sha256(pathlib.Path(after["tools"][tool]["path"])) != after["tools"][tool]["sha256"]:
            raise ValueError(f"tool changed: {tool}")
    compiler, ast = (after["tools"][key]["path"] for key in ("compiler", "ast"))
    rows = []
    expected = {"helper_loop": ("adjust", 2, 2, 1),
                "arithmetic_effects": ("weighted", 1, 2, 0),
                "arithmetic_capture": ("weighted", 0, 0, 0)}
    for case, (name, calls_after, inlines, unrolls) in expected.items():
        for debug in (1, 2):
            key = (case, 2, debug)
            old, new = before_rows[key], after_rows[key]
            if old["source_sha256"] != new["source_sha256"]:
                raise ValueError(f"source changed: {key}")
            directory = f"{case}_O2_g{debug}"
            source = pathlib.Path(after["work"]) / directory / "source.luau"
            emitted = pathlib.Path(after["work"]) / directory / "output.luau"
            previous = pathlib.Path(before["work"]) / directory / "output.luau"
            for path, digest in ((source, new["source_sha256"]), (emitted, new["output_sha256"]),
                                 (previous, old["output_sha256"])):
                if sha256(path) != digest:
                    raise ValueError(f"artifact changed: {path}")
            command = [compiler, "--text", "-O2", f"-g{debug}", "--fflags=false"]
            original_text = checked([*command, source], timeout=30)[0].decode("utf-8")
            rebuilt_text = checked([*command, emitted], timeout=30)[0].decode("utf-8")
            old_calls = local_calls(parse_ast(ast, previous), name)
            new_calls = local_calls(parse_ast(ast, emitted), name)
            if (old_calls, new_calls) != (0, calls_after):
                raise ValueError(f"unexpected helper calls: {key}: {old_calls} -> {new_calls}")
            if original_text.count("REMARK inlining succeeded") != inlines \
                    or original_text.count("REMARK loop unroll succeeded") != unrolls:
                raise ValueError(f"compiler witness changed: {key}")
            output = emitted.read_text(encoding="utf-8")
            if ("original call sites unknown" in output) != (calls_after > 0):
                raise ValueError(f"missing/unexpected inference classification: {key}")
            rows.append(dict(case=case, opt=2, debug=debug, source_sha256=new["source_sha256"],
                output_sha256=new["output_sha256"], before_output_sha256=old["output_sha256"],
                calls_before=old_calls, calls_after=new_calls, compiler_inlines=inlines,
                compiler_unrolls=unrolls, original_disassembly=original_text,
                recompiled_disassembly=rebuilt_text, before_source=previous.read_text(encoding="utf-8"),
                after_source=output, dataflow_before=old["dataflow"], dataflow_after=new["dataflow"],
                runtime="passed", classification="equivalent call inference" if calls_after else "refused reference capture"))
    report = dict(schema_version=1, compiler_commit=after["compiler_commit_expected"],
        compiler_flags=["--text", "-O2", "-g1/-g2", "--fflags=false"], tools=after["tools"],
        before_report_sha256=sha256(args.before), after_report_sha256=sha256(args.after), rows=rows,
        limitations="Development fixtures only. No original-call uniqueness proof. Loop remains unrolled. "
                    "The second arithmetic_effects call stays as statements; unknown dataflow is not proof.")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"{len(rows)} arithmetic compiler witnesses passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
