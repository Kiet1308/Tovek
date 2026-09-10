import unittest

from structurer_inventory import inventory


class StructurerInventoryTests(unittest.TestCase):
    def test_requires_terminal_serial_run_and_complete_file_records(self):
        for log in (
            "source-like retry id=0 -> Unsupported\n",
            "Done: 1 decompiled, 0 skipped (no bytecode), 0 failed.\nTime: 1s (1 threads)\n",
            "ok a.lua\nDone: 1 decompiled, 0 skipped (no bytecode), 0 failed.\nTime: 1s (8 threads)\n",
        ):
            with self.subTest(log=log), self.assertRaises(ValueError):
                inventory(log)

    def test_scopes_prototype_ids_to_input_and_discards_failed_first_attempt(self):
        log = """source-like unsupported id=0 shared_tail=true reason=speculative node=1 stop=None
source-like first attempt id=0 -> Unsupported
source-like unsupported id=0 shared_tail=false reason=actual node=2 stop=None
source-like unsupported id=0 shared_tail=false reason=path node=0 stop=None
source-like retry id=0 -> Unsupported
ok a.lua
source-like first attempt id=0 -> Structured(1 stmts)
ok b.lua
Done: 2 decompiled, 0 skipped (no bytecode), 0 failed.
Time: 1s (2 files, 1 threads)
"""
        rows = inventory(log)
        self.assertEqual([(r['file'], r['proto'], r['reason']) for r in rows], [('a.lua', 0, 'actual')])
        self.assertEqual(len(rows[0]['trace']), 2)

    def test_does_not_report_a_successful_retry_as_legacy(self):
        log = """source-like first attempt id=0 -> Unsupported
source-like retry id=0 -> Structured(2 stmts)
ok a.lua
Done: 1 decompiled, 0 skipped (no bytecode), 0 failed.
Time: 1s (1 files, 1 threads)
"""
        self.assertEqual(inventory(log), [])


if __name__ == '__main__':
    unittest.main()
