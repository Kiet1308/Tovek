#!/usr/bin/env python3
"""Independent VM controls for ordered alias, guard and call reconstruction.

Each deliberately incorrect variant must compile and change at least one of
the locked observations at all six compiler profiles. These controls establish
driver sensitivity, not general equivalence of an ordered reconstruction.
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
    controls = {
        "late_snapshot": ('local snapshot = api.Value\n        api.mutate()', 'api.mutate()\n        local snapshot = api.Value'),
        "open_scalar_tail": ('local scalar = api.tail()\n        return false, scalar', 'return false, api.tail()'),
        "late_rhs": ('local rhs = api.value()\n        api.object()[api.key()] = rhs', 'api.object()[api.key()] = api.value()'),
        "operator_orientation": ('value * 2', '2 * value'),
        "eager_captured_argument": ('local result = api.test(cell) == "ok" and cell > 0', 'local result = predicate(cell)'),
        "lost_signed_zero": ('elseif value == 0 then\n            return value, 1 / value', 'elseif value == 0 then\n            return 0, 1 / 0'),
        "stale_guard_after_capture": ('change()\n            return value', 'change()\n            return 7'),
    }
    for name, (old, new) in controls.items():
        yield name, replace_once(source, old, new)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("compiler", "luau", "keep", "report"):
        parser.add_argument("--" + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.keep.mkdir(parents=True, exist_ok=False)
    fixtures = ROOT / "docs/failure_fixtures/roadmap_v2"
    manifest = fixtures / "manifest.json"
    case = next(case for case in json.loads(manifest.read_text(encoding="utf-8"))["cases"]
                if case["name"] == "ordered_reconstruction")
    source_path, driver_path = (fixtures / case[name] for name in ("source", "driver"))
    source, driver = (path.read_text(encoding="utf-8") for path in (source_path, driver_path))
    expected = case["expected_stdout"].splitlines()
    if len(expected) != 192 or case.get("runtime_compile_inline") is not True:
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
                  contract="Six original profiles and 42 compiled negative controls; 192 finite observations per profile. "
                           "No promotion of whole-chunk unknown/different certificates.")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=1) + "\n", encoding="utf-8", newline="\n")
    return int(any(row["status"] != "passed" for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
