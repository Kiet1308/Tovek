#!/usr/bin/env python3
"""Independent VM controls for bounded private constructor regions.

Each deliberately incorrect variant must compile and change at least one of
the locked observations at all six compiler profiles. These controls establish
driver sensitivity, not general equivalence of a constructor rewrite.
"""
import argparse
import collections
import hashlib
import json
import pathlib
import subprocess

from roadmap_v2 import ROOT, sha256


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise ValueError("control fixture shape changed: " + old)
    return text.replace(old, new)


def variants(source):
    def edit(name, old, new):
        marker = "    " + name + " = function("
        start = source.index(marker)
        end = source.index("    end,", start) + len("    end,")
        return source[:start] + replace_once(source[start:end], old, new) + source[end:]

    selection = 'if api.condition() then api.make("then") else api.make("else")'
    yield "eager_arms", edit("selected", selection,
        '(function() local a = api.make("then"); local b = api.make("else"); '
        'return if api.condition() then a else b end)()')
    yield "truncated_tail", edit("selected", "api.tail()", "(api.tail())")
    yield "lost_nil_overwrite", edit("selected", selection, '(' + selection + ') or "old"')
    yield "late_initializer", edit("initializer",
        'Snapshot = api.make("initial"),\n            [api.key()] = api.make("key-value"),',
        '[api.key()] = api.make("key-value"),\n            Snapshot = api.make("initial"),')
    yield "late_captured_snapshot", edit("snapshot",
        'Snapshot = state,\n            if change() then api.make("then") else api.make("else"),',
        'if change() then api.make("then") else api.make("else"),\n            Snapshot = state,')
    yield "late_observed_table", edit("observed", 'local result = { [1] = "old" }', 'local result')
    yield "rhs_before_address", edit("interleaved",
        'api.receiver()[api.key()] = api.make("then")',
        'local v = api.make("then")\n            api.receiver()[api.key()] = v')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("compiler", "luau", "keep", "report"):
        parser.add_argument("--" + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.keep.mkdir(parents=True, exist_ok=False)
    fixtures = ROOT / "docs/failure_fixtures/roadmap_v2"
    manifest = fixtures / "manifest.json"
    case = next(case for case in json.loads(manifest.read_text(encoding="utf-8"))["cases"]
                if case["name"] == "constructor_regions")
    source_path, driver_path = (fixtures / case[name] for name in ("source", "driver"))
    source, driver = (path.read_text(encoding="utf-8") for path in (source_path, driver_path))
    expected = case["expected_stdout"].splitlines()
    if len(expected) != 1386 or case.get("runtime_compile_inline") is not True:
        raise ValueError("locked fixture vector/profile contract changed")
    rows = []
    for name, code in [("original", source), *variants(source)]:
        path = args.keep / (name + ".luau")
        path.write_text(code, encoding="utf-8", newline="\n")
        runner = args.keep / (name + ".runner.luau")
        runner.write_text("local f = (function()\n" + code + "\nend)()\n" + driver,
                          encoding="utf-8", newline="\n")
        for opt in range(3):
            for debug in (1, 2):
                flags = [f"-O{opt}", f"-g{debug}", "--fflags=false"]
                row = dict(control=name, opt=opt, debug=debug, status="failed", source_sha256=sha256(path))
                try:
                    compiled = subprocess.run([str(args.compiler), "--binary", *flags, str(path)],
                                              capture_output=True, timeout=30)
                    if compiled.returncode:
                        raise ValueError("control did not compile: " + compiled.stderr.decode("utf-8", errors="replace"))
                    observed = subprocess.run([str(args.luau), *flags, str(runner)], capture_output=True, timeout=30)
                    if observed.returncode:
                        raise ValueError("control runner failed: " + observed.stderr.decode("utf-8", errors="replace"))
                    actual = observed.stdout.decode("utf-8").splitlines()
                    if len(actual) != len(expected):
                        raise ValueError("control vector count differs")
                    differences = [i for i, pair in enumerate(zip(expected, actual)) if pair[0] != pair[1]]
                    row.update(vectors=len(actual), changed_vectors=len(differences),
                               stdout_sha256=hashlib.sha256(observed.stdout).hexdigest())
                    if differences:
                        first = differences[0]
                        row["first_difference"] = dict(vector=first, expected=expected[first], actual=actual[first])
                    if (name == "original") != (len(differences) == 0):
                        raise ValueError("original mismatch or undetected mutant")
                    row["status"] = "passed"
                except (OSError, ValueError, subprocess.SubprocessError) as error:
                    row["error"] = str(error)
                rows.append(row)
                print(f"{row['status']}: {name} O{opt} g{debug} {row.get('changed_vectors', '?')}", flush=True)
    result = dict(schema_version=1, rows=rows,
                  summary=dict(collections.Counter(row["status"] for row in rows)),
                  source_sha256=sha256(source_path), driver_sha256=sha256(driver_path),
                  manifest_sha256=sha256(manifest),
                  tools={name: dict(path=str(getattr(args, name)), sha256=sha256(getattr(args, name)))
                         for name in ("compiler", "luau")},
                  contract="Six original profiles and 42 compiled negative controls; 1386 finite observations per profile. "
                           "No promotion of whole-chunk unknown/different certificates.")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=1) + "\n", encoding="utf-8", newline="\n")
    return int(any(row["status"] != "passed" for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
