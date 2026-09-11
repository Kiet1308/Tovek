#!/usr/bin/env python3
"""Check named assignment-order prototypes without promoting whole-chunk results."""
import argparse
import collections
import hashlib
import json
import pathlib
import subprocess

from bytecode_dataflow import symbolic_tree
from bytecode_roundtrip import parse_chunk
from roadmap_v2 import sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("fixtures_report", "compiler", "report"):
        parser.add_argument("--" + name.replace("_", "-"), type=pathlib.Path, required=True)
    args = parser.parse_args()
    fixtures = json.loads(args.fixtures_report.read_text(encoding="utf-8"))
    if sha256(args.compiler) != fixtures["tools"]["compiler"]["sha256"]:
        raise ValueError("fixture/compiler hash mismatch")
    cases = [r for r in fixtures["cases"] if r["case"] == "nested_assignment_order"]
    if len(cases) != 6 or {(r["opt"], r["debug"]) for r in cases} != {
        (o, g) for o in (0, 1, 2) for g in (1, 2)
    } or any(r["status"] != "passed" for r in cases):
        raise ValueError("expected all six passing assignment-order profiles")
    rows = []
    for case in cases:
        directory = pathlib.Path(fixtures["work"]) / (
            f"nested_assignment_order_O{case['opt']}_g{case['debug']}"
        )
        trees = []
        for variant, hash_field in (("source", "source_sha256"), ("output", "output_sha256")):
            source = directory / (variant + ".luau")
            if sha256(source) != case[hash_field]:
                raise ValueError("fixture source hash mismatch")
            raw = subprocess.check_output([
                str(args.compiler), "--binary", f"-O{case['opt']}", f"-g{case['debug']}",
                "--fflags=false", str(source),
            ], timeout=30)
            if variant == "source" and raw != (directory / "input.luaubc").read_bytes():
                raise ValueError("original bytecode replay mismatch")
            chunk = parse_chunk(raw, 1)
            named = collections.defaultdict(list)
            for proto in chunk.protos:
                if proto.name:
                    named[chunk.strings[proto.name - 1].decode("utf-8")].append(proto)
            names = ("base", "key", "both", "leaf")
            if any(len(named[name]) != 1 for name in names):
                raise ValueError("missing or ambiguous fixture prototype")
            trees.append({name: symbolic_tree(chunk, named[name][0]) for name in names})
        for name in names:
            values = [tree[name] for tree in trees]
            rows.append(dict(opt=case["opt"], debug=case["debug"], function=name,
                             status="proved" if values[0] == values[1] else "different",
                             fingerprints=[hashlib.sha256(repr(v).encode()).hexdigest() for v in values]))
    report = dict(model="luau-acyclic-use-def-v1", compiler_sha256=sha256(args.compiler),
                  fixture_report_sha256=sha256(args.fixtures_report), rows=rows,
                  summary=dict(collections.Counter(r["status"] for r in rows)),
                  scope="Four uniquely named fixture prototypes per profile. Module allocation/closure/store "
                        "construction is excluded; whole-chunk different/unknown is not promoted.")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(report["summary"]))
    return int(any(r["status"] != "proved" for r in rows))


if __name__ == "__main__":
    raise SystemExit(main())
