import contextlib
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import semantic_roundtrip as semantic


class SemanticOracleTests(unittest.TestCase):
    def run_case(self, variant="same", with_prelude=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixtures = root / "fixtures"
            fixtures.mkdir()
            (fixtures / "case.luau").write_text("print('profile-dependent source')\n")
            if with_prelude:
                (fixtures / "prelude").mkdir()
                (fixtures / "prelude/case.luau").write_text("getfenv()\nShared = {}\n")
            tool = root / "tool"
            tool.write_bytes(b"test tool")
            work = root / "work"
            compiled, executed = [], []

            def compile_bytecode(compiler, source, optimization):
                source = Path(source)
                compiled.append((source.parent.name, source.name, int(optimization)))
                body = b"\x09" + str(optimization).encode() + b":" + source.name.encode()
                return subprocess.CompletedProcess([], 0, body, b"")

            def decompile(command, **_):
                self.assertEqual(command[1], "decompile-folder")
                output = Path(command[3])
                output.mkdir()
                for opt in range(3):
                    (output / f"case__O{opt}.luau").write_text(f"print({opt})\n")
                log = ("".join(f"ok case__O{opt}.lua\n" for opt in range(3))
                       + "----------------------------------------\n"
                       "Done: 3 decompiled, 0 skipped (no bytecode), 0 failed.\n"
                       f"Output: {output.resolve()}\nTime: 0.01s  (3 files, 3.3 ms/file, 300 files/s, 1 threads)\n")
                return subprocess.CompletedProcess(command, 0, "", log)

            def execute(command, **kwargs):
                self.assertEqual(len(command), 3)
                if with_prelude:
                    prelude, bytecode = Path(command[1]), Path(command[2])
                    self.assertEqual(prelude.read_bytes(), b"\x091:case.luau")
                    self.assertEqual(prelude.parent.name, "prelude")
                else:
                    bytecode, driver = Path(command[1]), Path(command[2])
                    self.assertEqual(driver.read_bytes(), b"\x090:driver.luau")
                data = bytecode.read_bytes()
                self.assertEqual(data[0], 9)
                executed.append((bytecode.parent.name, bytecode.name, data))
                output = bytecode.parent.name == "rebuilt"
                stdout, stderr, status = b"O" + data[1:2] + b"\n", b"", 0
                if variant == "stderr" and output:
                    stderr = b"unexpected error\n"
                if variant == "whitespace" and output:
                    stdout = b" " + stdout
                if variant == "invalid_utf8":
                    stdout = b"\xfe\n" if output else b"\xff\n"
                if variant == "both_error":
                    stdout, stderr, status = b"", b"same runtime error\n", 1
                if variant == "timeout" and output:
                    raise subprocess.TimeoutExpired(command, kwargs["timeout"])
                return subprocess.CompletedProcess(command, status, stdout, stderr)

            argv = ["semantic_roundtrip.py", "--compiler", str(tool), "--vm", str(tool),
                    "--lifter", str(tool), "--fixtures", str(fixtures), "--keep", str(work)]
            with patch.object(sys, "argv", argv), patch.object(semantic, "compile_bytecode", side_effect=compile_bytecode), \
                    patch.object(semantic, "run", side_effect=decompile), \
                    patch.object(semantic.subprocess, "run", side_effect=execute), contextlib.redirect_stdout(io.StringIO()):
                result = semantic.main()
            return result, json.loads((work / "results.json").read_text()), compiled, executed

    def test_executes_each_actual_input_and_matching_rebuilt_profile(self):
        result, report, compiled, executed = self.run_case()
        self.assertEqual(result, 0)
        self.assertEqual(report["summary"]["status"], {"passed": 3})
        self.assertTrue(report["summary"]["batch_valid"])
        for opt, row in enumerate(report["cases"]):
            self.assertEqual(row["output_optimization"], opt)
            self.assertEqual(row["input_observation"]["stdout"], f"O{opt}\n")
            self.assertEqual(row["input_observation"], row["rebuilt_observation"])
            self.assertIn(("fixtures", "case.luau", opt), compiled)
            self.assertIn(("out", f"case__O{opt}.luau", 0), compiled)
            self.assertIn(("out", f"case__O{opt}.luau", opt), compiled)
            self.assertIn(("bytecode", f"case__O{opt}.luaubc", b"\x09" + str(opt).encode() + b":case.luau"), executed)
        self.assertEqual(len(executed), 6)

    def test_prelude_identity_and_fixed_profile_are_shared_by_each_runtime_pair(self):
        result, report, compiled, executed = self.run_case(with_prelude=True)
        self.assertEqual(result, 0)
        self.assertEqual([row for row in compiled if row[0] == "prelude"], [("prelude", "case.luau", 1)])
        self.assertEqual(len(report["cases"]), 3)
        self.assertEqual(len(executed), 6)
        for row in report["cases"]:
            self.assertEqual(row["runtime_order"], "prelude_then_input")
            self.assertEqual(row["prelude"]["source_sha256"], hashlib.sha256(b"getfenv()\nShared = {}\n").hexdigest())
            self.assertEqual(row["prelude"]["bytecode_sha256"], hashlib.sha256(b"\x091:case.luau").hexdigest())
            self.assertEqual(row["prelude"]["optimization"], 1)
            self.assertEqual(row["prelude"]["debug"], 1)
            self.assertEqual(row["prelude"]["compiler_flags"], ["--fflags=false"])

    def test_observation_changes_and_unsuccessful_execution_never_pass(self):
        for variant in ("stderr", "whitespace", "invalid_utf8", "both_error", "timeout"):
            with self.subTest(variant=variant):
                result, report, _, _ = self.run_case(variant)
                self.assertEqual(result, 1)
                self.assertEqual(report["summary"]["status"], {"failed": 3})
                self.assertTrue(report["summary"]["batch_valid"])

    def test_observe_preserves_full_raw_bytes(self):
        stdout, stderr = b"\xff\r\n ", b"x" * 4000 + b"\xfe\n"
        process = subprocess.CompletedProcess([], 0, stdout, stderr)
        with patch.object(semantic.subprocess, "run", return_value=process):
            observed = semantic.observe("vm", "input.bc", "driver.bc")
        self.assertEqual(observed["stdout"].encode("utf-8", errors="surrogateescape"), stdout)
        self.assertEqual(observed["stderr"].encode("utf-8", errors="surrogateescape"), stderr)


if __name__ == "__main__":
    unittest.main()
