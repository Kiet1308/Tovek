#!/usr/bin/env python3
"""Measure each emitted Luau file and gate unexpected output growth in CI.

Example: output_size.py --root fixtures=out/fixtures --baseline baseline.json
Use --write-baseline PATH only after reviewing the per-file changes. The gate
allows 25% growth, with a small absolute allowance for short fixtures. Missing
files and files without a baseline fail so coverage cannot silently shrink.
"""
import argparse
import hashlib
import json
import math
import pathlib


def measure(roots):
    files = {}
    for label, root in roots:
        paths = sorted(root.rglob("*.luau"))
        if not paths:
            raise ValueError(f"no Luau output under {root}")
        for path in paths:
            key = f"{label}/{path.relative_to(root).as_posix()}"
            if key in files:
                raise ValueError(f"duplicate output key: {key}")
            # read_text normalizes CRLF so Windows and Linux use the same gate.
            source = path.read_text(encoding="utf-8-sig")
            files[key] = dict(lines=len(source.splitlines()), bytes=len(source.encode("utf-8")))
    return dict(schema_version=1, files=files)


def regressions(current, baseline, expected_refusals=()):
    if baseline.get("schema_version") != 1 or not baseline.get("files"):
        raise ValueError("expected a nonempty output-size baseline with schema_version=1")
    before, after = baseline["files"], current["files"]
    failures = [f"missing output: {name}" for name in sorted(before.keys() - after.keys() - set(expected_refusals))]
    failures += [f"unbaselined output: {name}" for name in sorted(after.keys() - before.keys())]
    for name in sorted(before.keys() & after.keys()):
        for metric, allowance in (("lines", 40), ("bytes", 2048)):
            old, new = before[name][metric], after[name][metric]
            if type(old) is not int or old < 0:
                raise ValueError(f"invalid baseline {metric} for {name}")
            maximum = old + max(allowance, math.ceil(old / 4))
            if new > maximum:
                failures.append(f"{name}: {metric} {old} -> {new} (limit {maximum})")
    return failures


def reviewed_refusals(reports, roots, current):
    """Account for missing outputs only after rechecking the fixture run's evidence."""
    from expected_refusals import batch_refusals_match, refusal_evidence
    refused = {}
    for label, path in reports:
        report = json.loads(path.read_text(encoding="utf-8"))
        if report.get("kind") == "residual_control":
            from check_residual_control import reviewed_refusals as residual_refusals
            if label not in roots:
                raise ValueError("refusal report does not describe an output root")
            for relative, evidence in residual_refusals(path, roots[label]).items():
                key = f"{label}/{relative}"
                if key in current["files"] or key in refused:
                    raise ValueError("refusal evidence or output identity mismatch")
                refused[key] = evidence
            continue
        if label not in roots or roots[label].resolve() != (path.parent / "out").resolve():
            raise ValueError("refusal report does not describe its output root")
        manifest_path = pathlib.Path(report["manifest_path"])
        manifest_bytes = manifest_path.read_bytes()
        if hashlib.sha256(manifest_bytes).hexdigest() != report["manifest_sha256"]:
            raise ValueError("refusal manifest hash mismatch")
        manifest = json.loads(manifest_bytes)
        if report.get("schema_version") != 1 or manifest.get("schema_version") != 1:
            raise ValueError("unsupported refusal report or manifest")
        entries = {entry["source"]: entry for entry in manifest["cases"]}
        if len(entries) != len(manifest["cases"]):
            raise ValueError("duplicate refusal source")
        fixtures = pathlib.Path(report["fixtures_path"]).resolve()
        expected_failures = {row["case"] + ".lua": row["refusal"]["stderr"]
                             for row in report["cases"] if row["status"] == "expected_refusal"}
        if not batch_refusals_match(report["batch"], expected_failures,
                                    [row["case"] + ".lua" for row in report["cases"]], str(roots[label].resolve())):
            raise ValueError("semantic batch diagnostics or inventory mismatch")
        seen = set()
        for row in report["cases"]:
            if row["case"] in seen:
                raise ValueError("duplicate semantic profile")
            seen.add(row["case"])
            if row["status"] == "passed":
                continue
            if row["status"] != "expected_refusal":
                raise ValueError("semantic run contains an unreviewed failure")
            source = pathlib.Path(row["source"]).resolve()
            relative = source.relative_to(fixtures).as_posix()
            entry = entries.get(relative, {})
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            profile = dict(opt=row["opt"], debug=row["debug"],
                           bytecode_version=report["bytecode_version"],
                           compiler_flags=report["compiler_flags"], lifter_args=report["lifter_args"])
            evidence = row["refusal"]
            checked = refusal_evidence(entry, digest, profile,
                                       {key: evidence[key] for key in ("exit_code", "stdout", "stderr")})
            name = f"{source.stem}__O{row['opt']}"
            key = f"{label}/{name}.luau"
            if (checked is None or checked != evidence or row["source_sha256"] != digest
                    or row["case"] != name or row.get("output") or row.get("output_bytes", 0)
                    or row.get("output_sha256") or key in current["files"] or key in refused):
                raise ValueError("refusal evidence or output identity mismatch")
            refused[key] = evidence
    return refused


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", action="append", required=True, metavar="LABEL=DIR")
    parser.add_argument("--baseline", type=pathlib.Path)
    parser.add_argument("--write-baseline", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    parser.add_argument("--refusal-report", action="append", default=[], metavar="LABEL=JSON",
                        help="verified fixture run accounting for reviewed missing outputs")
    args = parser.parse_args()
    roots = []
    for spec in args.root:
        label, separator, path = spec.partition("=")
        if not separator or not label or not path or any(c in label for c in "/\\"):
            parser.error("--root must be LABEL=DIR with a simple label")
        roots.append((label, pathlib.Path(path)))
    try:
        current = measure(roots)
        reports = []
        for spec in args.refusal_report:
            label, separator, path = spec.partition("=")
            if not separator or not label or not path:
                raise ValueError("--refusal-report must be LABEL=JSON")
            reports.append((label, pathlib.Path(path)))
        refused = reviewed_refusals(reports, dict(roots), current)
        if refused:
            current["expected_refusals"] = refused
        baseline = json.loads(args.baseline.read_text(encoding="utf-8")) if args.baseline else None
        failures = regressions(current, baseline, refused) if baseline else []
        if args.report:
            write(args.report, dict(**current, regressions=failures))
        for failure in failures:
            print(f"REGRESSION {failure}")
        print(f"output size: {len(current['files'])} outputs, {len(refused)} expected refusals, {len(failures)} regressions")
        if failures:
            return 1
        if args.write_baseline:
            # Keep reviewed historical size limits for inputs that have no output in this run.
            if baseline:
                current["files"].update({name: baseline["files"][name] for name in refused if name in baseline["files"]})
            write(args.write_baseline, current)
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"output size: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
