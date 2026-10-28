import unittest
from test_bytecode_dataflow import chunk, instruction as op
from bytecode_reference_graph import compare_reference_graph

def sample(base=2, cell=1, shift=0, close=False, loop=False):
    child = chunk([op('GETUPVAL', shift, 0), op('LOADN', shift, d=9), op('GETUPVAL', shift + 1, 0), op('RETURN', shift + 1, 2)], params=0, upvalues=1).protos[0]
    child.id = 1
    code = [op('LOADN', cell, d=7), op('NEWCLOSURE', base, d=0), op('CAPTURE', 1, cell)]
    if close:
        code.append(op('CLOSEUPVALS', cell))
    code.append(op('CALL', base, 1, 2))
    code.append(op('JUMPBACK', d=-len(code) - 1) if loop else op('RETURN', base, 2))
    outer = chunk(code, params=0)
    outer.protos[0].children = [1]
    outer.protos.append(child)
    return outer

class ReferenceFrameTests(unittest.TestCase):

    def test_safe_owner_allows_only_private_child_storage(self):
        self.assertEqual(compare_reference_graph(sample(), sample(shift=3))['status'], 'proved')

    def test_dataflow_fallback_requires_reference_frame_certificate(self):
        from bytecode_dataflow import compare_dataflow
        positive = compare_dataflow(sample(), sample(shift=3))
        self.assertEqual(positive['status'], 'proved')
        self.assertEqual(positive['model'], 'luau-guarded-reference-context-v1')
        negative = compare_dataflow(sample(base=0), sample(base=0, shift=3))
        self.assertEqual(negative['status'], 'unknown')
        self.assertIn('reference_context', negative)

    def test_high_cell_alias_is_not_a_private_temporary(self):
        for close in [False, True]:
            self.assertEqual(compare_reference_graph(sample(base=0, close=close), sample(base=0, close=close, shift=3))['status'], 'proved' if close else 'unknown')

    def test_loop_reopens_cells_and_requires_close_before_each_call(self):
        for close in [False, True]:
            self.assertEqual(compare_reference_graph(sample(base=0, close=close, loop=True), sample(base=0, close=close, loop=True, shift=3))['status'], 'proved' if close else 'unknown')

    def test_one_predecessor_close_does_not_close_the_join(self):
        left = sample(base=1, cell=3)
        p = left.protos[0]
        joined = chunk([op('LOADN', 3, d=7), op('NEWCLOSURE', 1, d=0), op('CAPTURE', 1, 3), op('JUMPIF', 0, d=1), op('JUMP', d=1), op('CLOSEUPVALS', 3), op('CALL', 1, 1, 2), op('RETURN', 1, 2)], params=1).protos[0]
        joined.children = [1]
        left.protos[0] = joined
        self.assertEqual(compare_reference_graph(left, left)['status'], 'unknown')

    def test_changed_owner_capture_lifetime_remains_different(self):
        self.assertEqual(compare_reference_graph(sample(), sample(close=True))['status'], 'unknown')
        self.assertEqual(compare_reference_graph(sample(), sample(cell=0))['status'], 'unknown')

    def test_open_top_before_capture_and_implicit_call_is_rejected(self):
        a = sample()
        p = chunk([op('CALL', 0, 1, 0), op('LOADN', 10, d=7), op('NEWCLOSURE', 2, d=0), op('CAPTURE', 1, 10), op('GETGLOBAL', 1, aux=0), op('LOADN', 5, d=1), op('GETTABLE', 4, 1, 5), op('RETURN', 2, 2)], params=1, constants=[('str', 1)], strings=[b'object']).protos[0]
        p.children = [1]
        a.protos[0] = p
        a.strings = [b'object']
        self.assertEqual(compare_reference_graph(a, a)['status'], 'unknown')

    def test_vararg_owner_requires_single_entry_preparation(self):
        a = sample()
        p = a.protos[0]
        p.is_vararg = True
        self.assertEqual(compare_reference_graph(a, a)['status'], 'unknown')
        p.insns = [(0, *op('PREPVARARGS'))] + [(i[0] + 1, *i[1:]) for i in p.insns]
        p.code = [0] * (len(p.code) + 1)
        self.assertEqual(compare_reference_graph(a, a)['status'], 'proved')
        loop = sample(loop=True)
        p = loop.protos[0]
        p.is_vararg = True
        p.insns = [(0, *op('PREPVARARGS'))] + [(i[0] + 1, *i[1:]) for i in p.insns]
        p.code = [0] * (len(p.code) + 1)
        self.assertEqual(compare_reference_graph(loop, loop)['status'], 'proved')
        pc = p.insns[-1][0]
        p.insns[-1] = (pc, *op('JUMPBACK', d=-pc - 1))
        self.assertEqual(compare_reference_graph(loop, loop)['status'], 'unknown')

    def test_external_main_and_budget_remain_unknown(self):
        a = sample()
        a.protos[0].num_upvalues = 1
        self.assertEqual(compare_reference_graph(a, a)['status'], 'unknown')
        self.assertEqual(compare_reference_graph(sample(), sample(), budget=2)['status'], 'unknown')

    def test_nested_owners_and_forwarded_upvalues_keep_each_call_frame_safe(self):

        def nested(shift=0, base=2, swapped=False):
            outer = sample()
            middle = chunk([op('LOADN', 1, d=9), op('NEWCLOSURE', base, d=0), op('CAPTURE', 1, 1), op('CAPTURE', 2, 0), op('CALL', base, 1, 2), op('RETURN', base, 2)], params=0, upvalues=1).protos[0]
            middle.id = 1
            middle.children = [2]
            child = chunk([op('GETUPVAL', shift, int(swapped)), op('GETUPVAL', shift + 1, int(not swapped)), op('ADD', shift + 2, shift, shift + 1), op('RETURN', shift + 2, 2)], params=0, upvalues=2).protos[0]
            child.id = 2
            outer.protos = [outer.protos[0], middle, child]
            return outer
        self.assertEqual(compare_reference_graph(nested(), nested(shift=5))['status'], 'proved')
        self.assertEqual(compare_reference_graph(nested(base=0), nested(base=0, shift=5))['status'], 'unknown')
        self.assertEqual(compare_reference_graph(nested(), nested(swapped=True))['status'], 'unknown')

    def test_recursive_self_reference_requires_exact_owner_and_closure_slot(self):

        def recursive(shift=0, cell=0):
            child = chunk([op('GETUPVAL', shift, 0), op('RETURN', shift, 2)], params=0, upvalues=1).protos[0]
            child.id = 1
            parent = chunk([op('LOADN', 1, d=7), op('NEWCLOSURE', 0, d=0), op('CAPTURE', 1, cell), op('CLOSEUPVALS', 0), op('RETURN', 0, 2)], params=0)
            parent.protos[0].children = [1]
            parent.protos.append(child)
            return parent
        self.assertEqual(compare_reference_graph(recursive(), recursive(shift=5))['status'], 'proved')
        self.assertEqual(compare_reference_graph(recursive(), recursive(cell=1))['status'], 'unknown')

    def test_generic_iterator_checks_prepare_and_iteration_call_frames(self):

        def iterator(cell, close=False, prepare=True):
            parent = sample(cell=cell)
            code = [op('NEWCLOSURE', 12, d=0), op('CAPTURE', 1, cell)]
            if close:
                code.append(op('CLOSEUPVALS', cell))
            if prepare:
                code.append(op('FORGPREP', 6, d=0))
            code += [op('FORGLOOP', 6, d=-1, aux=1), op('RETURN', 0, 1)]
            p = chunk(code, params=12).protos[0]
            p.children = [1]
            parent.protos[0] = p
            return parent
        self.assertEqual(compare_reference_graph(iterator(1), iterator(1))['status'], 'proved')
        self.assertEqual(compare_reference_graph(iterator(8), iterator(8))['status'], 'unknown')
        self.assertEqual(compare_reference_graph(iterator(8, prepare=False), iterator(8, prepare=False))['status'], 'proved')
        self.assertEqual(compare_reference_graph(iterator(9, prepare=False), iterator(9, prepare=False))['status'], 'unknown')
        self.assertEqual(compare_reference_graph(iterator(9, close=True), iterator(9, close=True))['status'], 'proved')
if __name__ == '__main__':
    unittest.main()
