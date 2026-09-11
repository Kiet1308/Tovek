#!/usr/bin/env python3
"""Compile every changed capture-order output and retain bounded input certificates.

This inventory is not a blanket equivalence certificate: unknown remains unknown.
The regression fixtures and the inliner dependency argument are separate evidence.
"""
import argparse
import collections
import concurrent.futures
import difflib
import json
import pathlib
import subprocess
import tempfile

from benchmark_v2 import tree_hash
from bytecode_dataflow import compare_dataflow
from bytecode_roundtrip import parse_chunk, read_saved_bytecode
from roadmap_v2 import sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("before", "after", "input", "compiler", "report", "review"):
        parser.add_argument("--" + name, type=pathlib.Path, required=True)
    parser.add_argument("--key", type=int, default=203)
    args = parser.parse_args()
    paths = lambda root: {p.relative_to(root).as_posix(): p for p in root.rglob("*.luau")}
    before, after = paths(args.before), paths(args.after)
    if before.keys() != after.keys():
        parser.error("source file sets differ")
    changed = [name for name in sorted(before) if sha256(before[name]) != sha256(after[name])]
    args.review.parent.mkdir(parents=True, exist_ok=True)
    with args.review.open("w", encoding="utf-8", newline="\n") as stream:
        for name in changed:
            stream.writelines(difflib.unified_diff(before[name].read_text(encoding="utf-8").splitlines(True),
                                                  after[name].read_text(encoding="utf-8").splitlines(True),
                                                  fromfile="before/" + name, tofile="after/" + name, n=3))

    def check(name):
        row = dict(file=name, before_sha256=sha256(before[name]), after_sha256=sha256(after[name]))
        try:
            original_path = args.input / pathlib.Path(name).with_suffix(".lua")
            original = parse_chunk(read_saved_bytecode(original_path), args.key)
            comparisons = []
            with tempfile.TemporaryDirectory(prefix="tovek_capture_") as temporary:
                for path in (before[name], after[name]):
                    # Stage to an ASCII path for the pinned Windows compiler.
                    staged = pathlib.Path(temporary) / "input.luau"
                    staged.write_bytes(path.read_bytes())
                    proc = subprocess.run([str(args.compiler), "--binary", "-O2", "-g1", "--fflags=false",
                                           "--vector-lib=Vector3", "--vector-ctor=new", "--vector-type=Vector3",
                                           str(staged)], capture_output=True, timeout=30)
                    if proc.returncode:
                        raise ValueError(proc.stderr.decode("utf-8", errors="replace"))
                    comparisons.append(compare_dataflow(original, parse_chunk(proc.stdout, 1)))
            a, b = comparisons
            row.update(input_sha256=sha256(original_path), before_dataflow=a, after_dataflow=b,
                       before_bytes=before[name].stat().st_size, after_bytes=after[name].stat().st_size)
            if a["status"] == "proved" and b["status"] != "proved":
                raise ValueError("lost existing bounded input certificate")
            row["status"] = "passed"
        except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
            row.update(status="failed", error=str(error))
        return row

    rows = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        for row in pool.map(check, changed):
            rows.append(row)
            if len(rows) % 25 == 0 or row["status"] != "passed":
                print(f"{len(rows)}/{len(changed)}: {row['status']} {row['file']}", flush=True)
    report = dict(schema_version=1, total_files=len(before), changed_files=len(changed), rows=rows,
                  before_tree_hash=tree_hash(args.before, "*.luau")[0],
                  after_tree_hash=tree_hash(args.after, "*.luau")[0], compiler_sha256=sha256(args.compiler),
                  review_sha256=sha256(args.review), summary=dict(collections.Counter(r["status"] for r in rows)),
                  dataflow_transitions=dict(collections.Counter(
                      r["before_dataflow"]["status"] + "->" + r["after_dataflow"]["status"]
                      for r in rows if "before_dataflow" in r)),
                  contract="All changed outputs compile; no existing bounded input certificate may be lost. "
                           "Unknown/different are retained, not promoted by this inventory. "
                           "The separate local review patch contains every textual change.")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({k: report[k] for k in ("total_files", "changed_files", "summary", "dataflow_transitions")}))
    return int(any(row["status"] != "passed" for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
