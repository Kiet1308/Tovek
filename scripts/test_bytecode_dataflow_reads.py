import copy
import unittest
from bytecode_dataflow import compare_dataflow
from test_bytecode_dataflow import chunk, instruction as op

def closed(value):
    child = copy.deepcopy(value.protos[0]); child.id = 1
    n = child.num_upvalues
    outer = chunk([op('NEWCLOSURE', n, d=0), *[op('CAPTURE', 0, slot) for slot in range(n)], op('RETURN', n, 2)], params=n)
    outer.protos[0].children = [1]
    outer.protos.append(child)
    return outer

class CaptureNormalizationTests(unittest.TestCase):
    def compare(self, left, right, status, **kwargs):
        self.assertEqual(compare_dataflow(closed(chunk(left, **kwargs)), closed(chunk(right, **kwargs)))['status'], status)

    def test_duplicate_read_truthiness_preserves_false_nil_and_true_value(self):
        original = [op('GETUPVAL', 0, 0), op('RETURN', 0, 2)]
        candidate = [op('GETUPVAL', 0, 0), op('JUMPIF', 0, d=1), op('GETUPVAL', 0, 0), op('RETURN', 0, 2)]
        self.compare(original, candidate, 'proved', params=0, upvalues=1)
        candidate[2] = op('GETUPVAL', 0, 1)
        self.compare(original, candidate, 'different', params=0, upvalues=2)

    def test_duplicate_reads_interleave_slots_without_changing_order(self):
        original = [op('GETUPVAL', 0, 0), op('GETUPVAL', 1, 1), op('RETURN', 0, 3)]
        candidate = [op('GETUPVAL', 0, 0), op('GETUPVAL', 1, 1), op('GETUPVAL', 0, 0), op('RETURN', 0, 3)]
        self.compare(original, candidate, 'proved', params=0, upvalues=2)

    def test_mutation_capable_events_keep_late_upvalue_reads(self):
        for event in [op('CALL', 1, 1, 1), op('SETUPVAL', 1, 0), op('GETTABLE', 3, 1, 2),
                      op('ADD', 3, 1, 2), op('SETTABLE', 1, 2, 3)]:
            with self.subTest(event=event):
                prefix = [op('GETUPVAL', 0, 0), event]
                good = [*prefix, op('GETUPVAL', 0, 0), op('RETURN', 0, 2)]
                bad = [*prefix, op('RETURN', 0, 2)]
                self.compare(good, bad, 'different', params=4, upvalues=1)

    def test_effectful_compare_never_disappears_or_reuses_stale_read(self):
        prefix = [op('GETUPVAL', 0, 0), op('JUMPIFEQ', 1, d=1, aux=2)]
        good = [*prefix, op('GETUPVAL', 0, 0), op('RETURN', 0, 2)]
        stale = [*prefix, op('MOVE', 0, 0), op('RETURN', 0, 2)]
        self.compare(good, stale, 'different', params=3, upvalues=1)
        self.compare(good, [op('GETUPVAL', 0, 0), op('RETURN', 0, 2)], 'different', params=3, upvalues=1)

    def test_different_truthiness_arms_and_budget_remain_guarded(self):
        original = [op('GETUPVAL', 0, 0), op('RETURN', 0, 2)]
        different = [op('GETUPVAL', 0, 0), op('JUMPIF', 0, d=1), op('LOADB', 0, 1), op('RETURN', 0, 2)]
        self.compare(original, different, 'different', params=0, upvalues=1)
        c = chunk(original, params=0, upvalues=1)
        self.assertEqual(compare_dataflow(c, c, budget=0)['status'], 'unknown')

    def test_external_main_upvalues_do_not_reuse_reads_across_register_writes(self):
        original = chunk([op('GETUPVAL', 0, 0), op('LOADN', 0, d=7), op('GETUPVAL', 1, 0), op('RETURN', 1, 2)], params=0, upvalues=1)
        stale = chunk([op('GETUPVAL', 1, 0), op('LOADN', 0, d=7), op('RETURN', 1, 2)], params=0, upvalues=1)
        self.assertEqual(compare_dataflow(original, stale)['status'], 'unknown')

if __name__ == '__main__':
    unittest.main()
