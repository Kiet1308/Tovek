import copy
import unittest

from bytecode_dataflow import compare_dataflow
from bytecode_graph import compare_graph
from bytecode_roundtrip import Reader, _parse_constant
from test_bytecode_dataflow import chunk, instruction as ins


class GraphTests(unittest.TestCase):
    def proved(self, a, b):
        result = compare_graph(a, b)
        self.assertEqual(result['status'], 'proved', result)
        return result

    def refuses(self, a, b):
        self.assertNotEqual(compare_graph(a, b)['status'], 'proved')
        self.assertNotEqual(compare_dataflow(a, b)['status'], 'proved')

    def while_loop(self, temporary=2):
        return chunk([ins('LOADN', temporary, d=0), ins('JUMPIFNOT', 0, d=3),
                      ins('ADDK', temporary, temporary, 0), ins('MOVE', 0, 1),
                      ins('JUMPBACK', d=-4), ins('RETURN', temporary, 2)],
                     constants=[('num', 1.0)])

    def test_loop_bisimulation_preserves_renamed_storage_and_backedge(self):
        a, b = self.while_loop(), self.while_loop(9)
        result = self.proved(a, b)
        self.assertEqual(result['summaries'][0]['back_edges'], 1)
        self.assertEqual(compare_dataflow(a, b)['model'], result['model'])
        b.protos[0].insns[4] = (4, *ins('JUMPBACK', d=0))
        self.refuses(a, b)

    def test_loop_operand_and_condition_mutants(self):
        a = self.while_loop()
        for index, replacement in [(1, ins('JUMPIF', 0, d=3)),
                                   (2, ins('ADDK', 2, 1, 0)),
                                   (3, ins('MOVE', 1, 0)),
                                   (5, ins('RETURN', 0, 2))]:
            b = copy.deepcopy(a)
            b.protos[0].insns[index] = (index, *replacement)
            self.refuses(a, b)

    def test_join_requires_definition_on_both_predecessors(self):
        a = chunk([ins('JUMPIF', 0, d=2), ins('LOADN', 2, d=3),
                   ins('JUMP', d=1), ins('LOADN', 2, d=4), ins('RETURN', 2, 2)])
        self.assertEqual(self.proved(a, a)['summaries'][0]['joins'], 1)
        b = copy.deepcopy(a)
        b.protos[0].insns[3] = (3, *ins('NOP'))
        self.refuses(b, b)

    def closure(self, mode=1, capture=0, close=True):
        child = chunk([ins('GETUPVAL', 0, 0), ins('RETURN', 0, 2)], params=0, upvalues=1).protos[0]
        child.id = 1
        code = [ins('NEWCLOSURE', 2, d=0), ins('CAPTURE', mode, capture)]
        if close:
            code.append(ins('CLOSEUPVALS', 0))
        code.append(ins('RETURN', 2, 2))
        root = chunk(code, upvalues=2)
        root.protos[0].children = [1]
        root.protos.append(child)
        return root

    def test_reference_and_inherited_capture_lifetime(self):
        for mode in [1, 2]:
            a = self.closure(mode)
            self.proved(a, a)
            self.assertEqual(compare_dataflow(a, a)['status'], 'proved')
            self.refuses(a, self.closure(mode, 1))
        self.refuses(self.closure(1), self.closure(0))
        self.refuses(self.closure(1), self.closure(1, close=False))

    def test_close_partition_and_ref_frame_layout_are_preserved(self):
        def make(base, close):
            root = chunk([ins('LOADN', base, d=4), ins('LOADN', base + 1, d=5),
                          ins('NEWCLOSURE', base + 2, d=0),
                          ins('CAPTURE', 1, base), ins('CAPTURE', 1, base + 1),
                          ins('CLOSEUPVALS', close), ins('RETURN', base + 2, 2)])
            child = chunk([ins('GETUPVAL', 0, 0), ins('GETUPVAL', 1, 1), ins('RETURN', 0, 3)],
                          params=0, upvalues=2).protos[0]
            child.id = 1
            root.protos[0].children = [1]
            root.protos.append(child)
            return root
        self.proved(make(4, 5), make(4, 5))
        self.refuses(make(4, 5), make(10, 11))
        self.refuses(make(4, 5), make(4, 4))

    def test_orphan_capture_target_payload_and_uninitialized_cell_refuse(self):
        for a in [chunk([ins('CAPTURE', 1, 0), ins('RETURN', 0, 1)]),
                  self.closure(capture=9)]:
            self.refuses(a, a)
        a = self.closure()
        a.protos[0].insns[-1] = (3, *ins('JUMP', d=-3))
        self.refuses(a, a)

    def test_closure_constant_sharing_is_not_erased(self):
        child = chunk([ins('RETURN', 0, 1)], params=0).protos[0]
        child.id = 1
        def make(second):
            a = chunk([ins('DUPCLOSURE', 2, d=0), ins('DUPCLOSURE', 3, d=second), ins('RETURN', 2, 3)],
                      constants=[('closure', 1), ('closure', 1)])
            a.protos.append(copy.deepcopy(child))
            return a
        self.proved(make(0), make(0))
        self.refuses(make(0), make(1))

    def test_duplicate_closure_ref_capture_is_invalid(self):
        a = self.closure()
        a.protos[0].constants = [('closure', 1)]
        a.protos[0].insns[0] = (0, *ins('DUPCLOSURE', 2, d=0))
        self.refuses(a, a)

    def test_numeric_and_generic_loops_keep_arity_and_implicit_registers(self):
        numeric = chunk([ins('LOADN', 2, d=4), ins('LOADN', 3, d=1), ins('LOADN', 4, d=1),
                         ins('FORNPREP', 2, d=2), ins('MOVE', 0, 4), ins('FORNLOOP', 2, d=-2),
                         ins('RETURN', 0, 2)])
        self.proved(numeric, numeric)
        generic = chunk([ins('FORGPREP', 0, d=1), ins('MOVE', 5, 3),
                         ins('FORGLOOP', 0, d=-2, aux=2), ins('RETURN', 0, 1)], params=3)
        self.proved(generic, generic)
        b = copy.deepcopy(generic)
        b.protos[0].insns[2] = (2, *ins('FORGLOOP', 0, d=-2, aux=1))
        self.refuses(generic, b)
        b.protos[0].insns[2] = (2, *ins('FORGLOOP', 1, d=-2, aux=2))
        self.refuses(generic, b)

    def test_multret_and_fixed_call_arity(self):
        a = chunk([ins('CALL', 0, 2, 0), ins('RETURN', 0, 0)])
        self.proved(a, a)
        b = chunk([ins('CALL', 0, 2, 2), ins('RETURN', 0, 2)])
        self.refuses(a, b)
        bad = chunk([ins('RETURN', 0, 0)])
        self.refuses(bad, bad)

    def test_pool_reorder_and_distinct_literal_bits(self):
        a = self.while_loop()
        b = self.while_loop(9)
        b.protos[0].constants = [('num', -0.0), ('num', 1.0)]
        b.protos[0].insns[2] = (2, *ins('ADDK', 9, 9, 1))
        self.proved(a, b)
        b.protos[0].constants[1] = ('num', 0.0)
        self.refuses(a, b)

    def test_integer_payload_is_not_rounded_before_comparison(self):
        def encoded(value):
            data = bytearray([9, 0])
            while value >= 128:
                data.append((value & 127) | 128)
                value >>= 7
            data.append(value)
            return bytes(data)
        left = _parse_constant(Reader(encoded(2**53)), 9)
        right = _parse_constant(Reader(encoded(2**53 + 1)), 9)
        self.assertNotEqual(left, right)
        code = [ins('LOADK', 2, d=0), ins('RETURN', 2, 2)]
        self.refuses(chunk(code, constants=[left]), chunk(code, constants=[right]))

    def test_budget_version_and_unsupported_fastcall_refuse(self):
        a = self.while_loop()
        self.assertEqual(compare_graph(a, a, budget=1)['status'], 'unknown')
        a.version = 11
        self.refuses(a, a)
        a = chunk([ins('FASTCALL', 1, 0, 0), ins('RETURN', 0, 1)])
        self.refuses(a, a)

    def test_fastcall_preserves_builtin_arguments_fallback_and_result(self):
        # Both fast success and ordinary CALL must reach RETURN with a result.
        a = chunk([ins('FASTCALL2', 2, 0, 5, aux=1),
                   ins('GETGLOBAL', 3, aux=0), ins('MOVE', 4, 0), ins('MOVE', 5, 1),
                   ins('CALL', 3, 3, 2), ins('RETURN', 3, 2)],
                  constants=[('str', 1)], strings=[b'max'])
        self.assertEqual(self.proved(a, a)['summaries'][0]['fastcall_sites'], 1)
        for replacement in [ins('FASTCALL2', 3, 0, 5, aux=1),
                            ins('FASTCALL2', 2, 0, 5, aux=0),
                            ins('FASTCALL2', 2, 0, 6, aux=1)]:
            b = copy.deepcopy(a)
            b.protos[0].insns[0] = (0, *replacement)
            self.refuses(a, b)
        b = copy.deepcopy(a)
        b.strings = [b'min']
        self.refuses(a, b)
        b = copy.deepcopy(a)
        b.protos[0].insns[-2] = (6, *ins('CALL', 3, 3, 1))
        self.refuses(b, b)

    def test_open_pack_is_consumed_and_aux_register_is_not_truncated(self):
        bad = chunk([ins('MOVE', 3, 0), ins('NEWTABLE', 2, aux=0), ins('CALL', 3, 1, 0),
                     ins('SETLIST', 2, 3, 0, aux=1), ins('RETURN', 3, 0)])
        self.refuses(bad, bad)
        a = chunk([ins('JUMPIFEQ', 0, d=1, aux=256), ins('RETURN', 0, 1)])
        self.refuses(a, a)


if __name__ == '__main__':
    unittest.main()
