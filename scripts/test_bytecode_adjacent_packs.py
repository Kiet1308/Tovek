import copy
import unittest
from unittest.mock import patch

import bytecode_graph
import test_bytecode_graph as existing
from bytecode_graph import compare_adjacent_pack_graph as compare
from test_bytecode_dataflow import chunk, instruction as ins


class AdjacentPacks(unittest.TestCase):
    def tail(self, base=2, snapshot=False):
        code = [ins('GETGLOBAL', base, aux=0)]
        if snapshot:
            code.append(ins('MOVE', base + 1, base))
            base += 1
        code += [ins('MOVE', base + 1, 0), ins('CALL', base, 2, 0), ins('RETURN', base, 0)]
        return chunk(code, constants=[('str', 1)], strings=[b'f'])

    def stats(self, value):
        graph = bytecode_graph.Graph(value, 20000, private_copies=True)
        graph._adjacent_packs = True
        return graph.build()[1]

    def test_tail_call_preserves_open_result_and_input(self):
        a, b = self.tail(), self.tail(snapshot=True)
        result = compare(a, b)
        self.assertEqual(result['status'], 'proved')
        self.assertEqual(result['model'], bytecode_graph.ADJACENT_PACK_MODEL)
        self.assertEqual(bytecode_graph.compare_private_graph(a, b)['status'], 'unknown')
        self.assertEqual(compare(a, b, budget=2)['status'], 'unknown')
        b.protos[0].insns[2] = (3, *ins('MOVE', 4, 1))
        self.assertEqual(compare(a, b)['status'], 'unknown')

    def test_vararg_call_immediate_pair(self):
        def make(base):
            return chunk([ins('PREPVARARGS', 1), ins('MOVE', base, 0),
                          ins('GETVARARGS', base + 1, 0), ins('CALL', base, 0, 2),
                          ins('RETURN', base, 2)], params=1, vararg=True)
        self.assertEqual(compare(make(1), make(3))['status'], 'proved')

    def test_chain_preserves_fixed_prefix_and_arity(self):
        def make(base, prefix=0):
            return chunk([ins('GETGLOBAL', base, aux=0), ins('MOVE', base + 1, prefix),
                          ins('GETGLOBAL', base + 2, aux=1), ins('CALL', base + 2, 1, 0),
                          ins('CALL', base, 0, 0), ins('RETURN', base, 0)],
                         constants=[('str', 1), ('str', 2)], strings=[b'f', b'g'])
        self.assertEqual(compare(make(2), make(5))['status'], 'proved')
        self.assertEqual(compare(make(2), make(5, 1))['status'], 'unknown')
        fixed = make(5)
        fixed.protos[0].insns[-1] = (7, *ins('RETURN', 5, 2))
        self.assertEqual(compare(make(2), fixed)['status'], 'unknown')

    def test_setlist_pair_preserves_table_and_index(self):
        def make(base, index=1):
            return chunk([ins('PREPVARARGS', 0), ins('NEWTABLE', 0, aux=0),
                          ins('GETVARARGS', base, 0), ins('SETLIST', 0, base, 0, aux=index),
                          ins('RETURN', 0, 2)], params=0, vararg=True)
        self.assertEqual(compare(make(1), make(4))['status'], 'proved')
        self.assertEqual(compare(make(1), make(4, 2))['status'], 'unknown')

    def test_main_upvalues_and_reference_captures_keep_exact_storage(self):
        a, b = self.tail(), self.tail(snapshot=True)
        a.protos[0].num_upvalues = b.protos[0].num_upvalues = 1
        self.assertEqual(compare(a, b)['status'], 'unknown')
        a, b = self.tail(), self.tail(snapshot=True)
        # An unreachable owner still prevents assuming whole-chunk privacy.
        owner = chunk([ins('NEWCLOSURE', 2, d=0), ins('CAPTURE', 1, 0),
                       ins('RETURN', 2, 2)]).protos[0]
        owner.id, owner.children = 1, [2]
        child = chunk([ins('GETUPVAL', 0, 0), ins('RETURN', 0, 2)],
                      params=0, upvalues=1).protos[0]
        child.id = 2
        for value in (a, b):
            value.protos += copy.deepcopy([owner, child])
        self.assertEqual(compare(a, b)['status'], 'unknown')

    def test_nonadjacent_pack_not_normalized(self):
        for barrier in (ins('NOP'), ins('LOADN', 2, d=7), ins('GETGLOBAL', 2, aux=0)):
            value = chunk([ins('CALL', 0, 1, 0), barrier, ins('RETURN', 0, 0)],
                          constants=[('str', 1)], strings=[b'callback'], params=1)
            self.assertNotIn('adjacent_pack_pairs', self.stats(value))

    def test_join_consumer_not_normalized(self):
        value = chunk([ins('JUMPIF', 1, d=2), ins('CALL', 0, 1, 0), ins('JUMP', d=1),
                       ins('CALL', 0, 1, 0), ins('RETURN', 0, 0)], params=2)
        self.assertNotIn('adjacent_pack_pairs', self.stats(value))

    def test_open_fastcall_success_bypass_not_normalized(self):
        value = chunk([ins('FASTCALL1', 40, 0, 3), ins('MOVE', 3, 0),
                       ins('GETGLOBAL', 2, aux=0), ins('CALL', 2, 2, 0),
                       ins('RETURN', 2, 0)], constants=[('str', 1)], strings=[b'type'])
        self.assertNotIn('adjacent_pack_pairs', self.stats(value))

    def test_stale_fixed_read_after_setlist_stays_unknown(self):
        for producer in (ins('CALL', 1, 1, 0), ins('GETVARARGS', 1, 0)):
            for tail in ([ins('RETURN', 1, 2)],
                         [ins('JUMPIF', 0, d=1), ins('LOADN', 1, d=7), ins('RETURN', 1, 2)],
                         [ins('JUMPIF', 0, d=-1), ins('RETURN', 1, 2)]):
                value = chunk([ins('PREPVARARGS', 2), ins('NEWTABLE', 0, aux=0), producer,
                               ins('SETLIST', 0, 1, 0, aux=1), *tail], params=2, vararg=True)
                result = compare(value, value)
                self.assertEqual(result['status'], 'unknown')
                self.assertIn('reaching definition', result['reasons']['original'])

    def test_malformed_vararg_preparation_not_admitted(self):
        for prefix in ([], [ins('NOP'), ins('PREPVARARGS', 1)],
                       [ins('PREPVARARGS', 1), ins('PREPVARARGS', 1)],
                       [ins('PREPVARARGS', 1), ins('JUMPIF', 0, d=-2)]):
            value = chunk([*prefix, ins('GETVARARGS', 1, 0), ins('RETURN', 1, 0)],
                          params=1, vararg=True)
            self.assertEqual(compare(value, value)['status'], 'unknown')

    def test_existing_graph_controls_under_adjacent_pack_certificate(self):
        for name in unittest.defaultTestLoader.getTestCaseNames(existing.GraphTests):
            with self.subTest(control=name), patch.object(existing, 'compare_graph', compare), \
                    patch.object(existing, 'compare_dataflow', compare):
                result = unittest.TestResult()
                existing.GraphTests(name).run(result)
                self.assertTrue(result.wasSuccessful(), result.failures + result.errors)


if __name__ == '__main__':
    unittest.main()
