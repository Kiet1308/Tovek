import contextlib
import copy
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

from expected_refusals import refusal_evidence
import public_source_roundtrip
import roadmap_v2
import semantic_roundtrip


REASON = "cannot preserve open call results across an operation without a tuple-order proof"
PROFILE = dict(opt=1, debug=1, bytecode_version=9, compiler_flags=["--fflags=false"], lifter_args=[])
OUTCOME = dict(exit_code=1, stdout="", stderr=f"prototype 2, pc 15: {REASON}\n")


def reviewed(digest):
    return dict(source_sha256=digest, expected_refusal=dict(reason=REASON, profiles=[copy.deepcopy(PROFILE)]))


class ExpectedRefusalTests(unittest.TestCase):
    def test_requires_exact_source_options_and_complete_empty_output_diagnostic(self):
        entry = reviewed("a" * 64)
        self.assertIsNotNone(refusal_evidence(entry, "a" * 64, PROFILE, OUTCOME))
        self.assertIsNone(refusal_evidence({}, "a" * 64, PROFILE, OUTCOME))
        self.assertIsNone(refusal_evidence(entry, "b" * 64, PROFILE, OUTCOME))
        for key, value in [("opt", 0), ("opt", True), ("debug", 2), ("bytecode_version", 14),
                           ("compiler_flags", ["--fflags=true"]), ("lifter_args", ["--style=compact"])]:
            profile = dict(PROFILE, **{key: value})
            with self.subTest(profile=profile):
                self.assertIsNone(refusal_evidence(entry, "a" * 64, profile, OUTCOME))
        for changes in [dict(exit_code=0), dict(exit_code=True), dict(exit_code=-6), dict(stdout="return 1\n"), dict(stdout="\n"),
                        dict(stderr="another reason"), dict(stderr=OUTCOME["stderr"] + "other failure\n"),
                        dict(stderr="panic: " + OUTCOME["stderr"])]:
            with self.subTest(changes=changes):
                self.assertIsNone(refusal_evidence(entry, "a" * 64, PROFILE, dict(OUTCOME, **changes)))

    def test_roadmap_refusal_is_not_runtime_or_dataflow_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "case.luau"
            source.write_text("return function() end\n")
            (root / "driver.luau").write_text("f()\n")
            case = dict(name="case", group="order", source=source.name, driver="driver.luau",
                        expected_stdout="", **reviewed(hashlib.sha256(source.read_bytes()).hexdigest()))
            args = types.SimpleNamespace(lifter=Path("lifter"), lifter_arg=[], timeout=1,
                                         bytecode_version=9, ast=None, determinism=False)
            work = root / "work"
            work.mkdir()
            outcome = subprocess.CompletedProcess([], 1, b"", OUTCOME["stderr"].encode())
            with patch.object(roadmap_v2, "compile_source", return_value=b"input"), \
                    patch.object(roadmap_v2, "run", return_value=(outcome, 0)):
                row = roadmap_v2.check_case(args, case, root, work, 1, 1)
            self.assertEqual(row["status"], "expected_refusal")
            self.assertEqual(row["dataflow"]["status"], "unavailable")
            self.assertNotIn("runtime", row)
            self.assertNotIn("output", row)
            self.assertEqual(row["output_bytes"], 0)

    def test_public_gate_counts_reviewed_refusal_separately_and_rejects_wrong_reason(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "vendor" / "repo" / "case.luau"
            source.parent.mkdir(parents=True)
            source.write_text("return 1\n")
            tools = root / "tool"
            tools.write_bytes(b"test tool")
            entry = dict(repo="repo", file="case.luau", split="development",
                         **reviewed(hashlib.sha256(source.read_bytes()).hexdigest()))
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps(dict(schema_version=1, repositories=[], sources=[entry],
                                                optimization_levels=[1], debug_level=1, compiler_commit="test")))
            for diagnostic, expected in [(OUTCOME["stderr"], "expected_refusal"), ("different failure\n", "failed")]:
                report = root / "report.json"
                argv = ["public_source_roundtrip.py", "--manifest", str(manifest), "--vendor", str(root / "vendor"),
                        "--compiler", str(tools), "--lifter", str(tools), "--report", str(report), "--keep", str(root / "keep")]
                outcome = subprocess.CompletedProcess([], 1, b"", diagnostic.encode())
                with patch.object(sys, "argv", argv), patch.object(public_source_roundtrip, "compile_source", return_value=b"input"), \
                        patch.object(public_source_roundtrip, "run", return_value=(outcome, 0)), contextlib.redirect_stdout(io.StringIO()):
                    result = public_source_roundtrip.main()
                data = json.loads(report.read_text())
                self.assertEqual(result, int(expected == "failed"))
                self.assertEqual(data["summary"]["development"]["status"], {expected: 1})
                self.assertNotIn("output", data["rows"][0])

    def test_semantic_gate_does_not_waive_unlisted_optimization(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = root / "fixtures"
            fixtures.mkdir()
            source = fixtures / "case.luau"
            source.write_text("return 1\n")
            entry = dict(source=source.name, **reviewed(hashlib.sha256(source.read_bytes()).hexdigest()))
            (fixtures / "expected_refusals.json").write_text(json.dumps(dict(schema_version=1, cases=[entry])))
            tool = root / "tool"
            tool.write_bytes(b"test tool")
            work = root / "work"
            argv = ["semantic_roundtrip.py", "--compiler", str(tool), "--vm", str(tool), "--lifter", str(tool),
                    "--fixtures", str(fixtures), "--keep", str(work)]
            outcome = subprocess.CompletedProcess([], 1, "", OUTCOME["stderr"])
            with patch.object(sys, "argv", argv), \
                    patch.object(semantic_roundtrip.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"\x09input", b"")), \
                    patch.object(semantic_roundtrip, "run", return_value=outcome), contextlib.redirect_stdout(io.StringIO()):
                result = semantic_roundtrip.main()
            report = json.loads((work / "results.json").read_text())
            self.assertEqual(result, 1)
            self.assertEqual(report["summary"]["status"], {"failed": 2, "expected_refusal": 1})

    def test_semantic_refusal_requires_clean_exact_batch_and_no_output_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = root / "fixtures"
            fixtures.mkdir()
            source = fixtures / "case.luau"
            source.write_text("return 1\n")
            entry = dict(source=source.name, **reviewed(hashlib.sha256(source.read_bytes()).hexdigest()))
            (fixtures / "expected_refusals.json").write_text(json.dumps(dict(schema_version=1, cases=[entry])))
            tool = root / "tool"
            tool.write_bytes(b"test tool")
            for variant in ("clean", "wrong_inventory", "panic", "wrong_diagnostic", "empty_artifact"):
                work = root / variant

                def fake_run(command, **_):
                    if command[1] == "decompile-folder":
                        output = Path(command[3])
                        output.mkdir()
                        for opt in (0, 2):
                            (output / f"case__O{opt}.luau").write_text("return 1\n")
                        if variant == "empty_artifact":
                            (output / "case__O1.luau").write_bytes(b"")
                        name = "unrelated.lua" if variant == "wrong_inventory" else "case__O1.lua"
                        diagnostic = "different failure\n" if variant == "wrong_diagnostic" else OUTCOME["stderr"]
                        log = ("ok case__O0.lua\nok case__O2.lua\n" + f"FAIL {name}\n      {diagnostic}"
                               "----------------------------------------\n"
                               "Done: 2 decompiled, 0 skipped (no bytecode), 1 failed.\n"
                               f"Output: {output.resolve()}\nTime: 0.01s  (3 files, 3.3 ms/file, 300 files/s, 1 threads)\n")
                        if variant == "panic":
                            log = "thread 'worker' panicked at unexpected bug\n" + log
                        return subprocess.CompletedProcess(command, 1, "", log)
                    if str(command[1]).endswith("case__O1.luaubc"):
                        return subprocess.CompletedProcess(command, 1, "", OUTCOME["stderr"])
                    return subprocess.CompletedProcess(command, 0, "", "")

                argv = ["semantic_roundtrip.py", "--compiler", str(tool), "--vm", str(tool), "--lifter", str(tool),
                        "--fixtures", str(fixtures), "--keep", str(work)]
                with self.subTest(variant=variant), patch.object(sys, "argv", argv), \
                        patch.object(semantic_roundtrip.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"\x09input", b"")), \
                        patch.object(semantic_roundtrip, "run", side_effect=fake_run), contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(semantic_roundtrip.main(), int(variant != "clean"))
                report = json.loads((work / "results.json").read_text())
                if variant == "empty_artifact":
                    self.assertEqual(report["cases"][1]["status"], "failed")


if __name__ == "__main__":
    unittest.main()
