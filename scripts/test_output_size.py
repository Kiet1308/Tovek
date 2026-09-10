import pathlib
import tempfile
import unittest

from output_size import measure, regressions


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


if __name__ == "__main__":
    unittest.main()
