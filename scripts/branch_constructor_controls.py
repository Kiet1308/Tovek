#!/usr/bin/env python3
"""Independent VM controls for private property-diamond reconstruction.

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
    def replace_function(name, edit):
        marker = "    " + name + " = function("
        if source.count(marker) != 1:
            raise ValueError("control function missing or duplicated: " + name)
        start = source.index(marker)
        end = source.index("    end,", start) + len("    end,")
        return source[:start] + edit(source[start:end]) + source[end:]

    def delayed(text, initializer):
        declaration = "        local props = " + initializer + "\n"
        text = replace_once(text, declaration, "        local props\n        local selected\n")
        text = replace_once(text, 'props.Value = make("then")', 'selected = make("then")')
        text = replace_once(text, 'props.Value = make("else")', 'selected = make("else")')
        return replace_once(text, '        props[key] = make("child")',
                            "        props = " + initializer + "\n        props.Value = selected\n"
                            '        props[key] = make("child")')

    normal = '{ Value = initial, Name = "Panel", [1] = "seed" }'
    initial_call = '{ Value = make("initial"), Name = "Panel", [1] = "seed" }'
    yield "late_initializer", replace_function("initializer", lambda text: delayed(text, initial_call))
    for name in ("observed", "captured"):
        yield "late_" + name + "_table", replace_function(name, lambda text: delayed(text, normal))

    def late_capture(text):
        text = replace_once(text, '        local props = { Value = state, Name = "Panel", [1] = "seed" }',
                            '        local props = { Name = "Panel", [1] = "seed" }')
        return replace_once(text, '        props[key] = make("child")',
                            '        props.Value = state\n        props[key] = make("child")')

    yield "late_captured_initializer", replace_function("captured_initial", late_capture)

    def eager(text):
        text = replace_once(text, 'props.Value = make("then")', 'props.Value = eagerThen')
        text = replace_once(text, 'props.Value = make("else")', 'props.Value = eagerElse')
        return replace_once(text, "        if condition() then",
                            '        local eagerThen = make("then")\n'
                            '        local eagerElse = make("else")\n        if condition() then')

    yield "eager_unselected_arm", replace_function("private", eager)
    yield "truncated_result_pack", replace_function("private", lambda text:
        replace_once(text, "return observe(props)", "return (observe(props))"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("compiler", "luau", "keep", "report"):
        parser.add_argument("--" + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.keep.mkdir(parents=True, exist_ok=False)
    fixtures = ROOT / "docs/failure_fixtures/roadmap_v2"
    manifest = fixtures / "manifest.json"
    case = next(case for case in json.loads(manifest.read_text(encoding="utf-8"))["cases"]
                if case["name"] == "branch_constructor_order")
    source_path, driver_path = (fixtures / case[name] for name in ("source", "driver"))
    source, driver = (path.read_text(encoding="utf-8") for path in (source_path, driver_path))
    expected = case["expected_stdout"].splitlines()
    if len(expected) != 1470 or case.get("runtime_compile_inline") is not True:
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
                  contract="Six original profiles and 36 compiled negative controls; 1470 finite observations per profile. "
                           "No promotion of whole-chunk unknown/different certificates.")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=1) + "\n", encoding="utf-8", newline="\n")
    return int(any(row["status"] != "passed" for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
