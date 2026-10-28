import collections
import copy
import json
from pathlib import Path
import tempfile
import unittest

import check_residual_control as residual
from bytecode_roundtrip import read_saved_bytecode


class ResidualEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.output = self.work / "residual-control-strict"
        self.output.mkdir()
        self.path = self.work / "residual-control-strict.json"
        self.root, self.entries = residual.fixture_manifest()
        rows, lines = [], []
        for name, entry in self.entries.items():
            row = dict(file=name, fixture_sha256=entry["fixture_sha256"],
                       bytecode_sha256=entry["bytecode_sha256"])
            if "expected_refusal" in entry:
                refusal = entry["expected_refusal"]
                diagnostic = f"prototype {refusal['prototype']}, pc {refusal['pc']}: {refusal['reason']}\n"
                result = dict(exit_code=1, stdout="", stderr=diagnostic)
                evidence = residual.fixture_refusal_evidence(entry, read_saved_bytecode(self.root / name),
                                                             203, "strict", result)
                self.assertIsNotNone(evidence)
                row.update(status="expected_refusal", refusal=evidence)
                lines.extend(["FAIL " + name, "      " + diagnostic.rstrip("\n")])
            else:
                target = self.output / Path(name).with_suffix(".luau")
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b"return 1\n")
                row.update(status="emitted", output_sha256=residual.sha256(target.read_bytes()), output_bytes=9)
                lines.append("ok " + name)
            rows.append(row)
        lines.extend(["----------------------------------------", "Done: 5 decompiled, 0 skipped (no bytecode), 2 failed.",
                      f"Output: {self.output}", "Time: 0.10s  (7 files, 14.3 ms/file, 70 files/s, 1 threads)"])
        self.report = dict(schema_version=1, kind="residual_control", fixtures_path=str(self.root),
                           output_root=str(self.output), manifest_path=str(residual.MANIFEST_PATH),
                           manifest_sha256=residual.sha256(residual.MANIFEST_PATH.read_bytes()),
                           mode="strict", key=203, lifter_args=residual.mode_arguments("strict"),
                           files=rows, summary=dict(collections.Counter(row["status"] for row in rows)),
                           batch=dict(exit_code=1, stdout="", stderr="\n".join(lines) + "\n"))

    def validate(self, report=None):
        self.path.write_text(json.dumps(report if report is not None else self.report))
        return residual.reviewed_refusals(self.path, self.output)

    def test_accepts_only_two_reviewed_refusals_with_complete_inventory(self):
        self.assertEqual(len(self.validate()), 2)
        for row in self.report["files"]:
            if row["status"] != "expected_refusal":
                continue
            raw = read_saved_bytecode(self.root / row["file"])
            entry = self.entries[row["file"]]
            outcome = {key: row["refusal"][key] for key in ("exit_code", "stdout", "stderr")}
            for key, mode, payload in [(True, "strict", raw), (1, "strict", raw),
                                       (203, "unknown", raw), (203, "strict", raw + b"x")]:
                self.assertIsNone(residual.fixture_refusal_evidence(entry, payload, key, mode, outcome))

    def test_rejects_dropped_profiles_even_with_forged_counts(self):
        for status in ("emitted", "expected_refusal"):
            report = copy.deepcopy(self.report)
            row = next(row for row in report["files"] if row["status"] == status)
            report["files"].remove(row)
            report["summary"][status] -= 1
            with self.subTest(status=status), self.assertRaisesRegex(ValueError, "inventory"):
                self.validate(report)

    def test_rejects_context_hash_and_diagnostic_changes(self):
        for field, value in [("mode", "default"), ("key", True), ("key", 1),
                             ("manifest_sha256", "0" * 64), ("output_root", str(self.work))]:
            report = copy.deepcopy(self.report)
            report[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.validate(report)
        for mutation in ("pc", "stdout", "signal", "panic"):
            report = copy.deepcopy(self.report)
            row = next(row for row in report["files"] if row["status"] == "expected_refusal")
            evidence = row["refusal"]
            if mutation == "pc":
                evidence["stderr"] = evidence["stderr"].replace("pc ", "pc 1")
            elif mutation == "stdout":
                evidence["stdout"] = " "
            elif mutation == "signal":
                evidence["exit_code"] = -11
            else:
                report["batch"]["stderr"] = "worker panicked\n" + report["batch"]["stderr"]
            with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                self.validate(report)

    def test_rejects_partial_and_unexpected_output_artifacts(self):
        row = next(row for row in self.report["files"] if row["status"] == "expected_refusal")
        target = self.output / Path(row["file"]).with_suffix(".luau")
        target.parent.mkdir(parents=True, exist_ok=True)
        for content in (b"", b"return 1\n"):
            target.write_bytes(content)
            with self.subTest(content=content), self.assertRaises(ValueError):
                self.validate()
        target.unlink()
        target.symlink_to(self.work / "missing")
        with self.assertRaises(ValueError):
            self.validate()

    def test_rejects_missing_or_changed_emitted_output(self):
        row = next(row for row in self.report["files"] if row["status"] == "emitted")
        target = self.output / Path(row["file"]).with_suffix(".luau")
        for content in (b"", b"controlFlowState = 1\n", b"return 2\n"):
            target.write_bytes(content)
            with self.subTest(content=content), self.assertRaises(ValueError):
                self.validate()
        target.unlink()
        with self.assertRaises(ValueError):
            self.validate()


if __name__ == "__main__":
    unittest.main()
