#!/usr/bin/env python3
"""Measure each emitted Luau file and gate unexpected output growth in CI.

Example: output_size.py --root fixtures=out/fixtures --baseline baseline.json
Use --write-baseline PATH only after reviewing the per-file changes. The gate
allows 25% growth, with a small absolute allowance for short fixtures. Missing
files and files without a baseline fail so coverage cannot silently shrink.
"""
import argparse
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


def regressions(current, baseline):
    if baseline.get("schema_version") != 1 or not baseline.get("files"):
        raise ValueError("expected a nonempty output-size baseline with schema_version=1")
    before, after = baseline["files"], current["files"]
    failures = [f"missing output: {name}" for name in sorted(before.keys() - after.keys())]
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


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", action="append", required=True, metavar="LABEL=DIR")
    parser.add_argument("--baseline", type=pathlib.Path)
    parser.add_argument("--write-baseline", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args()
    roots = []
    for spec in args.root:
        label, separator, path = spec.partition("=")
        if not separator or not label or not path or any(c in label for c in "/\\"):
            parser.error("--root must be LABEL=DIR with a simple label")
        roots.append((label, pathlib.Path(path)))
    try:
        current = measure(roots)
        failures = regressions(current, json.loads(args.baseline.read_text(encoding="utf-8"))) if args.baseline else []
        if args.report:
            write(args.report, dict(**current, regressions=failures))
        for failure in failures:
            print(f"REGRESSION {failure}")
        print(f"output size: {len(current['files'])} files, {len(failures)} regressions")
        if failures:
            return 1
        if args.write_baseline:
            write(args.write_baseline, current)
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"output size: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
