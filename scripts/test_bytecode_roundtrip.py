import collections
import unittest

from bytecode_roundtrip import _cancel_counted_setlists


class CountedSetListTriageTests(unittest.TestCase):
    def scaffold(self, base):
        result = collections.Counter({
            "GETIMPORT(@table)": 1, 'GETTABLEKS("pack")': 1, "CALL(*)": 1,
            'GETTABLEKS("n")': 1, "LOADK(1)": 2,
            "FORNPREP": 1, "FORNLOOP": 1, "GETTABLE": 1, "SETTABLE": 1,
        })
        if base:
            result["ADD"] += 1
            result[f"LOADK({base})"] += 1
        return result

    def test_exact_scaffold_and_offset_are_cancelled(self):
        for base in (0, 1, 3):
            with self.subTest(base=base):
                lost = collections.Counter({f"SETLIST(*,{base + 1})": 1})
                added = self.scaffold(base)
                _cancel_counted_setlists(lost, added)
                self.assertFalse(+lost)
                self.assertFalse(+added)

    def test_missing_count_read_or_wrong_offset_stays_visible(self):
        for variant in ("missing_count", "wrong_offset", "no_setlist"):
            with self.subTest(variant=variant):
                lost = collections.Counter({"SETLIST(*,4)": 1})
                added = self.scaffold(3)
                if variant == "missing_count":
                    del added['GETTABLEKS("n")']
                elif variant == "wrong_offset":
                    del added["LOADK(3)"]
                    added["LOADK(4)"] = 1
                else:
                    lost.clear()
                before = (lost.copy(), added.copy())
                _cancel_counted_setlists(lost, added)
                self.assertEqual((lost, added), before)

    def test_unrelated_call_is_never_cancelled(self):
        lost = collections.Counter({"SETLIST(*,1)": 1})
        added = self.scaffold(0)
        added["CALL(*)"] += 1
        added['GETIMPORT(@effect)'] = 1
        _cancel_counted_setlists(lost, added)
        self.assertEqual(+added, collections.Counter({"CALL(*)": 1, 'GETIMPORT(@effect)': 1}))


if __name__ == "__main__":
    unittest.main()
