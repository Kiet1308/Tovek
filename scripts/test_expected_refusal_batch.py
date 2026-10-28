import unittest

from expected_refusals import batch_refusals_match


class BatchRefusalTests(unittest.TestCase):
    def test_accepts_only_complete_exact_batch_evidence(self):
        diagnostic = "prototype 2, pc 15: cannot preserve open call results across an operation without a tuple-order proof\n"
        log = ("ok good.lua\nFAIL refused.lua\n      " + diagnostic
               + "----------------------------------------\n"
               + "Done: 1 decompiled, 0 skipped (no bytecode), 1 failed.\n"
               + "Output: /work/out\nTime: 0.01s  (2 files, 5.0 ms/file, 200 files/s, 1 threads)\n")
        outcome = dict(exit_code=1, stdout="", stderr=log)
        inputs = {"good.lua", "refused.lua"}

        def check(result):
            return batch_refusals_match(result, {"refused.lua": diagnostic}, inputs, "/work/out")

        self.assertTrue(check(outcome))
        self.assertTrue(check(dict(outcome, stderr=log.replace("\n", "\r\n"))))
        for changed in [
            dict(outcome, exit_code=0), dict(outcome, exit_code=True), dict(outcome, exit_code=2),
            dict(outcome, stdout="partial source"),
            dict(outcome, stderr="thread 'worker' panicked: boom\n" + log),
            dict(outcome, stderr=log + "error: another failure\n"),
            dict(outcome, stderr=log.replace(diagnostic, "unrelated failure\n")),
            dict(outcome, stderr=log.replace("pc 15", "pc 16")),
            dict(outcome, stderr=log.replace("FAIL refused.lua", "FAIL unknown.lua")),
            dict(outcome, stderr=log.replace("ok good.lua\n", "")),
            dict(outcome, stderr="ok good.lua\n" + log),
            dict(outcome, stderr=log.replace("1 failed.", "2 failed.")),
            dict(outcome, stderr=log.replace("Output: /work/out", "Output: /other/out")),
            dict(outcome, stderr=log.replace("2 files,", "3 files,")),
        ]:
            with self.subTest(changed=changed):
                self.assertFalse(check(changed))

    def test_successful_batch_also_rejects_hidden_errors(self):
        log = ("ok good.lua\n----------------------------------------\n"
               "Done: 1 decompiled, 0 skipped (no bytecode), 0 failed.\n"
               "Output: /work/out\nTime: 0.01s  (1 files, 10.0 ms/file, 100 files/s, 1 threads)\n")
        outcome = dict(exit_code=0, stdout="", stderr=log)
        self.assertTrue(batch_refusals_match(outcome, {}, {"good.lua"}, "/work/out"))
        self.assertFalse(batch_refusals_match(dict(outcome, stderr="error: worker failed\n" + log),
                                             {}, {"good.lua"}, "/work/out"))


if __name__ == "__main__":
    unittest.main()
