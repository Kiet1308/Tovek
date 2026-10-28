import copy
import hashlib
import json
import pathlib
import tempfile
import unittest

from output_size import measure, regressions, reviewed_refusals


def report(**files):
    return dict(schema_version=1, files=files)


class OutputSizeTests(unittest.TestCase):
    def test_blocks_transform_scale_regression_even_if_other_files_shrink(self):
        before = report(Transform=dict(lines=470, bytes=20000), Other=dict(lines=10000, bytes=90000))
        after = report(Transform=dict(lines=5195, bytes=180000), Other=dict(lines=1, bytes=10))
        self.assertEqual(len(regressions(after, before)), 2)

    def test_requires_baseline_coverage_and_every_expected_file(self):
        failures = regressions(report(new=dict(lines=1, bytes=1)), report(old=dict(lines=1, bytes=1)))
        self.assertEqual(failures, ["missing output: old", "unbaselined output: new"])

    def test_allows_small_changes_without_allowing_arbitrary_one_line_growth(self):
        before = report(file=dict(lines=10, bytes=100))
        self.assertFalse(regressions(report(file=dict(lines=20, bytes=200)), before))
        self.assertTrue(regressions(report(file=dict(lines=10, bytes=10000)), before))

    def test_normalizes_platform_newlines_and_rejects_empty_roots(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with self.assertRaises(ValueError):
                measure([("fixture", root)])
            path = root / "a.luau"
            path.write_bytes(b"one\r\ntwo\r\n")
            windows = measure([("fixture", root)])
            path.write_bytes(b"one\ntwo\n")
            self.assertEqual(windows, measure([("fixture", root)]))

    def test_missing_outputs_require_exact_source_profile_and_refusal_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fixtures, work = root / "fixtures", root / "work"
            fixtures.mkdir()
            (work / "out").mkdir(parents=True)
            source = fixtures / "tail.luau"
            source.write_text("return function(f) return math.abs(f()) end")
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            profile = dict(opt=2, debug=1, bytecode_version=9,
                           compiler_flags=["--fflags=false"], lifter_args=[])
            reason = "cannot preserve open call results across an operation without a tuple-order proof"
            entry = dict(source=source.name, source_sha256=digest,
                         expected_refusal=dict(reason=reason, profiles=[profile]))
            manifest = fixtures / "expected_refusals.json"
            manifest.write_text(json.dumps(dict(schema_version=1, cases=[entry])))
            evidence = dict(reason=reason, source_sha256=digest, profile=profile,
                            exit_code=1, stdout="", stderr=f"prototype 0, pc 3: {reason}\n")
            row = dict(case="tail__O2", source=str(source), source_sha256=digest,
                       opt=2, debug=1, status="expected_refusal", output_bytes=0, refusal=evidence)
            report_path = work / "results.json"
            run = dict(schema_version=1, manifest_path=str(manifest),
                       manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest(),
                       fixtures_path=str(fixtures), bytecode_version=9,
                       compiler_flags=["--fflags=false"], lifter_args=[], cases=[row])
            run["batch"] = dict(exit_code=1, stdout="", stderr=(
                "FAIL tail__O2.lua\n      " + evidence["stderr"]
                + "----------------------------------------\n"
                + "Done: 0 decompiled, 0 skipped (no bytecode), 1 failed.\n"
                + f"Output: {(work / 'out').resolve()}\n"
                + "Time: 0.01s  (1 files, 10.0 ms/file, 100 files/s, 1 threads)\n"))
            current = report()
            baseline = report(**{"semantic/tail__O2.luau": dict(lines=1, bytes=10)})

            def checked(value, output=current):
                report_path.write_text(json.dumps(value))
                return reviewed_refusals([("semantic", report_path)], {"semantic": work / "out"}, output)

            self.assertEqual(regressions(current, baseline), ["missing output: semantic/tail__O2.luau"])
            refused = checked(run)
            self.assertEqual(list(refused), ["semantic/tail__O2.luau"])
            self.assertEqual(regressions(current, baseline, refused), [])
            mutations = [
                lambda r: r.update(manifest_sha256="0" * 64),
                lambda r: r["cases"][0].update(source_sha256="0" * 64),
                lambda r: r["cases"][0].update(opt=0),
                lambda r: r["cases"][0].update(status="failed"),
                lambda r: r["cases"][0]["refusal"].update(stdout="partial source"),
                lambda r: r["cases"][0]["refusal"].update(stderr="another failure"),
                lambda r: r["cases"][0]["refusal"].update(exit_code=0),
                lambda r: r["batch"].update(stderr="thread 'worker' panicked: boom\n" + r["batch"]["stderr"]),
                lambda r: r["batch"].update(stderr=r["batch"]["stderr"].replace(reason, "another failure")),
                lambda r: r["cases"].append(copy.deepcopy(r["cases"][0])),
            ]
            for mutate in mutations:
                changed = copy.deepcopy(run)
                mutate(changed)
                with self.assertRaises(ValueError):
                    checked(changed)
            with self.assertRaises(ValueError):
                checked(run, report(**{"semantic/tail__O2.luau": dict(lines=1, bytes=1)}))
            source.write_text("return 1")
            with self.assertRaises(ValueError):
                checked(run)

    def test_residual_receipts_reuse_validation_and_cannot_cover_an_existing_output(self):
        from unittest import mock
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            path = root / "residual.json"
            path.write_text(json.dumps(dict(kind="residual_control")))
            evidence = {"reason": "reviewed typed refusal"}
            with mock.patch("check_residual_control.reviewed_refusals", return_value={"nested/a.luau": evidence}) as check:
                receipts = [("residual", path)]
                roots = {"residual": root / "out"}
                self.assertEqual(reviewed_refusals(receipts, roots, report()), {"residual/nested/a.luau": evidence})
                check.assert_called_once_with(path, roots["residual"])
                with self.assertRaises(ValueError):
                    reviewed_refusals(receipts, roots, report(**{"residual/nested/a.luau": dict(lines=1, bytes=1)}))
                with self.assertRaises(ValueError):
                    reviewed_refusals(receipts * 2, roots, report())
            with mock.patch("check_residual_control.reviewed_refusals", side_effect=ValueError("bad receipt")):
                with self.assertRaisesRegex(ValueError, "bad receipt"):
                    reviewed_refusals(receipts, roots, report())


if __name__ == "__main__":
    unittest.main()
