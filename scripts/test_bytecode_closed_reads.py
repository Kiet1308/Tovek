import unittest
from unittest.mock import patch

import bytecode_graph
import test_bytecode_graph as existing
from bytecode_graph import compare_closed_read_graph as compare
from test_bytecode_dataflow import chunk, instruction as ins


def wrapped(code, params=0):
    child = chunk(code, params=params, upvalues=2).protos[0]
    child.id = 1
    parent = chunk([ins('NEWCLOSURE', 2, d=0), ins('CAPTURE', 0, 0),
                    ins('CAPTURE', 0, 1), ins('RETURN', 2, 2)], params=2)
    parent.protos[0].children = [1]
    parent.protos.append(child)
    return parent


class ClosedReads(unittest.TestCase):
    def test_dataflow_fallback_preserves_loop_and_closed_cell_identity(self):
        from bytecode_dataflow import compare_dataflow
        original = wrapped([ins('JUMPIF', 0, d=-1), ins('GETUPVAL', 1, 0),
                            ins('RETURN', 1, 2)], params=1)
        rebuilt = wrapped([ins('JUMPIF', 0, d=-1), ins('GETUPVAL', 1, 0),
                           ins('JUMPIF', 1, d=1), ins('GETUPVAL', 1, 0),
                           ins('RETURN', 1, 2)], params=1)
        result = compare_dataflow(original, rebuilt)
        self.assertEqual(result['status'], 'proved')
        self.assertEqual(result['model'], bytecode_graph.CLOSED_READ_MODEL)
        self.assertIn('adjacent_packs_unknown', result)
        rebuilt.protos[1].insns[3] = (3, *ins('GETUPVAL', 1, 1))
        negative = compare_dataflow(original, rebuilt)
        self.assertEqual(negative['status'], 'unknown')
        self.assertIn('closed_reads', negative)

    def pair(self, branch='JUMPIF'):
        return (wrapped([ins('GETUPVAL', 0, 0), ins('RETURN', 0, 2)]),
                wrapped([ins('GETUPVAL', 0, 0), ins(branch, 0, d=1),
                         ins('GETUPVAL', 0, 0), ins('RETURN', 0, 2)]))

    def removed(self, value):
        return compare(value, value)['summaries'][0]['closed_read_triangles_removed']

    def test_closed_read_triangle_both_truthiness_directions_and_budget(self):
        for branch in ('JUMPIF', 'JUMPIFNOT'):
            a, b = self.pair(branch)
            result = compare(a, b)
            self.assertEqual(result['status'], 'proved')
            self.assertEqual(result['model'], bytecode_graph.CLOSED_READ_MODEL)
            self.assertEqual(self.removed(b), 1)
            self.assertEqual(compare(a, b, budget=2)['status'], 'unknown')
            self.assertEqual(bytecode_graph.compare_adjacent_pack_graph(a, b)['status'], 'unknown')

    def test_wrong_upvalue_and_destination_not_collapsed(self):
        for changed in (ins('GETUPVAL', 0, 1), ins('GETUPVAL', 1, 0)):
            a, b = self.pair()
            b.protos[1].insns[2] = (2, *changed)
            self.assertEqual(compare(a, b)['status'], 'unknown')
            self.assertEqual(self.removed(b), 0)

    def test_intervening_effect_not_collapsed(self):
        a, _ = self.pair()
        b = wrapped([ins('GETUPVAL', 0, 0), ins('JUMPIF', 0, d=2), ins('SETUPVAL', 0, 1),
                     ins('GETUPVAL', 0, 0), ins('RETURN', 0, 2)])
        self.assertEqual(compare(a, b)['status'], 'unknown')
        self.assertEqual(self.removed(b), 0)

    def test_main_and_reference_captures_block(self):
        a, b = self.pair()
        for value in (a, b):
            value.protos[value.main].num_upvalues = 1
        self.assertEqual(compare(a, b)['status'], 'unknown')
        a, b = self.pair()
        for value in (a, b):
            value.protos[0].insns[1] = (1, *ins('CAPTURE', 1, 0))
        self.assertEqual(compare(a, b)['status'], 'unknown')

    def test_alternate_predecessor_into_reread_or_branch_blocked(self):
        for target in (1, 2):
            value = wrapped([ins('JUMPIF', 0, d=target), ins('GETUPVAL', 1, 0),
                             ins('JUMPIF', 1, d=1), ins('GETUPVAL', 1, 0),
                             ins('RETURN', 1, 2)], params=2)
            self.assertEqual(self.removed(value), 0)

    def test_wrong_branch_target_or_condition_blocked(self):
        a, b = self.pair()
        b.protos[1].insns[1] = (1, *ins('JUMPIF', 0, d=0))
        self.assertEqual(compare(a, b)['status'], 'unknown')
        self.assertEqual(self.removed(b), 0)
        value = wrapped([ins('GETUPVAL', 1, 0), ins('JUMPIF', 0, d=1),
                         ins('GETUPVAL', 1, 0), ins('RETURN', 1, 2)], params=1)
        self.assertEqual(self.removed(value), 0)

    def test_unrelated_join_predecessor_preserved(self):
        a = wrapped([ins('JUMPIF', 0, d=1), ins('GETUPVAL', 1, 0), ins('RETURN', 1, 2)], params=2)
        b = wrapped([ins('JUMPIF', 0, d=3), ins('GETUPVAL', 1, 0), ins('JUMPIF', 1, d=1),
                     ins('GETUPVAL', 1, 0), ins('RETURN', 1, 2)], params=2)
        self.assertEqual(compare(a, b)['status'], 'proved')

    def test_first_read_remains_explicit(self):
        a, b = self.pair()
        a.protos[1].insns[0] = (0, *ins('GETUPVAL', 0, 1))
        self.assertEqual(compare(a, b)['status'], 'unknown')
        self.assertEqual(self.removed(b), 1)

    def test_all_existing_graph_controls(self):
        for name in unittest.defaultTestLoader.getTestCaseNames(existing.GraphTests):
            with self.subTest(control=name), patch.object(existing, 'compare_graph', compare), \
                    patch.object(existing, 'compare_dataflow', compare):
                result = unittest.TestResult()
                existing.GraphTests(name).run(result)
                self.assertTrue(result.wasSuccessful(), result.failures + result.errors)


if __name__ == '__main__':
    unittest.main()
