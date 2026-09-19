#!/usr/bin/env python3
"""Run the pinned, license-bearing V2 public source manifest, including holdout.

This checks strict decompilation/recompilation and reports separate dataflow and
AST metrics. It does not execute Roblox modules or certify their equivalence.
"""
import argparse
import collections
import concurrent.futures
import hashlib
import json
import pathlib
import re
import subprocess
import tempfile

from bytecode_roundtrip import compare_chunks, parse_chunk
from bytecode_dataflow import compare_dataflow
from roadmap_v2 import ROOT, checked, compile_source, fixture_path, sha256
from source_fidelity import compare_ast, parse_ast
from output_quality import analyze_tree


def restore_pinned_line_endings(path, expected_sha256):
    """Materialize the exact frozen text bytes despite Git's checkout EOL policy.

    Manifests can contain LF and CRLF files from different repositories. Only
    accept a conversion if its complete SHA-256 matches the existing manifest;
    any content change remains an error. Downstream audits still hash raw bytes.
    """
    original = path.read_bytes()
    if hashlib.sha256(original).hexdigest() == expected_sha256:
        return
    lf = original.replace(b'\r\n', b'\n')
    for candidate in (lf, lf.replace(b'\n', b'\r\n')):
        if hashlib.sha256(candidate).hexdigest() == expected_sha256:
            path.write_bytes(candidate)
            return
    raise ValueError(f'file differs from pinned content: {path}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=pathlib.Path, default=ROOT / "docs/source_corpus_v2.json")
    parser.add_argument("--vendor", type=pathlib.Path, required=True)
    parser.add_argument("--checkout", action="store_true", help="fetch missing repositories at exact manifest commits")
    for name in ("compiler", "lifter"):
        parser.add_argument(f"--{name}", type=pathlib.Path, required=True)
    parser.add_argument("--ast", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    parser.add_argument("--keep", type=pathlib.Path, required=True)
    parser.add_argument("--lifter-arg", action="append", default=[])
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    for name in ("compiler", "lifter", "ast"):
        if getattr(args, name):
            setattr(args, name, getattr(args, name).resolve(strict=True))
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    if manifest["schema_version"] != 1 or not manifest["sources"]:
        parser.error("unsupported or empty manifest")
    args.vendor.mkdir(parents=True, exist_ok=True)
    for repository in manifest["repositories"]:
        path = fixture_path(args.vendor, repository["name"])
        commit = repository["commit"]
        if not re.fullmatch(r"[0-9a-f]{40}", commit):
            parser.error("manifest commit must be a full SHA")
        if not path.exists() and args.checkout:
            checked(["git", "clone", "--no-checkout", repository["url"], path], timeout=180)
            checked(["git", "-C", path, "checkout", "--detach", commit], timeout=60)
        head, _ = checked(["git", "-C", path, "rev-parse", "HEAD"], timeout=args.timeout)
        if head.decode().strip() != commit:
            parser.error(f"wrong commit: {repository['name']}")
        license_ = repository["license"]
        if args.checkout:
            # Keep all subsequent consumers (including the exact source registry)
            # on the same locked byte representation on Windows and Linux.
            pinned_files = [(license_["path"], license_["sha256"])]
            pinned_files.extend((entry["file"], entry["source_sha256"])
                                for entry in manifest["sources"] if entry["repo"] == repository["name"])
            for relative, digest in pinned_files:
                try:
                    restore_pinned_line_endings(fixture_path(path, relative), digest)
                except ValueError as error:
                    parser.error(str(error))
        if sha256(fixture_path(path, license_["path"])) != license_["sha256"]:
            parser.error(f"license hash mismatch: {repository['name']}")
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="pv2-", dir=args.keep)).resolve()

    def process(item):
        entry, opt = item
        row = dict(entry, opt=opt, status="failed", dataflow={"status": "unavailable"},
                   source_fidelity={"status": "unavailable"})
        try:
            source = fixture_path(args.vendor / entry["repo"], entry["file"])
            if sha256(source) != entry["source_sha256"]:
                raise ValueError("source hash mismatch")
            directory = work / entry["repo"] / f"O{opt}" / pathlib.Path(entry["file"]).parent
            directory.mkdir(parents=True, exist_ok=True)
            raw = compile_source(args, source, opt, manifest["debug_level"])
            bytecode = directory / (source.name + ".luaubc")
            bytecode.write_bytes(raw)
            output, elapsed = checked([args.lifter, bytecode, "--strict-no-synthetic-control",
                                       "--script-name", entry["file"], *args.lifter_arg], timeout=args.timeout)
            if not output.strip():
                raise ValueError("empty decompiler output")
            emitted = directory / (source.name + ".out.luau")
            emitted.write_bytes(output)
            rebuilt = compile_source(args, emitted, opt, manifest["debug_level"])
            a, b = parse_chunk(raw, 1), parse_chunk(rebuilt, 1)
            row["dataflow"] = compare_dataflow(a, b)
            pairs, missing, extra = compare_chunks(a, b)
            row["legacy_normalized"] = dict(collections.Counter(p["tier"] for p in pairs),
                                              missing=len(missing), extra=len(extra))
            if args.ast:
                output_ast = parse_ast(args.ast, emitted, args.timeout)
                row["source_fidelity"] = compare_ast(parse_ast(args.ast, source, args.timeout),
                                                      output_ast)
                row["output_quality"] = analyze_tree(output_ast, output.decode('utf-8'))
            row.update(status="passed", output_sha256=sha256(emitted), output_bytes=len(output),
                       decompile_seconds=elapsed, output=str(emitted))
        except (RuntimeError, ValueError, OSError, subprocess.TimeoutExpired) as error:
            row["error"] = str(error)
        return row

    jobs = [(entry, opt) for entry in manifest["sources"] for opt in manifest["optimization_levels"]]
    rows = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as executor:
        for row in executor.map(process, jobs):
            rows.append(row)
            if row["status"] != "passed":
                print(json.dumps(row), flush=True)
            if len(rows) % 50 == 0:
                print(f"{len(rows)}/{len(jobs)}", flush=True)
    groups = {}
    for split in sorted({r["split"] for r in rows}):
        subset = [r for r in rows if r["split"] == split]
        groups[split] = {"total": len(subset), "status": dict(collections.Counter(r["status"] for r in subset)),
                         "dataflow": dict(collections.Counter(r["dataflow"]["status"] for r in subset)),
                         "ast": dict(collections.Counter(r["source_fidelity"]["status"] for r in subset))}
    report = {"schema_version": 1, "manifest_sha256": sha256(args.manifest), "summary": groups,
              "lifter_args": args.lifter_arg,
              "tools": {name: {"path": str(getattr(args, name)), "sha256": sha256(getattr(args, name))}
                        for name in ("compiler", "lifter", "ast") if getattr(args, name)},
              "compiler_commit_expected": manifest["compiler_commit"], "rows": rows,
              "limitations": "No Roblox runtime. Different AST/dataflow is a review signal, not a counterexample; unknown is not proof."}
    report["source_groups"] = {
        group: {"total": sum(group in row.get("groups", []) for row in rows),
                "status": dict(collections.Counter(row["status"] for row in rows if group in row.get("groups", []))),
                "dataflow": dict(collections.Counter(row["dataflow"]["status"] for row in rows if group in row.get("groups", []))),
                "ast": dict(collections.Counter(row["source_fidelity"]["status"] for row in rows if group in row.get("groups", [])))}
        for group in sorted({group for row in rows for group in row.get("groups", [])})}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(groups, indent=2))
    return int(any(r["status"] != "passed" for r in rows))


if __name__ == "__main__":
    raise SystemExit(main())
