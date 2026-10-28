import unittest
from bytecode_dataflow import compare_acyclic, compare_dataflow
from test_bytecode_dataflow import chunk, instruction as op


class OpenPackReadTests(unittest.TestCase):
    def namecall_tail(self, base=3, destination=None, receiver=0):
        destination = base - 2 if destination is None else destination
        return chunk([op('MOVE', base, 1), op('CALL', base, 1, 0),
                      op('NAMECALL', destination, receiver, aux=0),
                      op('CALL', destination, 0, 0), op('RETURN', destination, 0)],
                     constants=[('str', 1)], strings=[b'method'])

    def test_namecall_below_pending_top_preserves_receiver_and_tail(self):
        self.assertEqual(compare_acyclic(self.namecall_tail(), self.namecall_tail(8))['status'], 'proved')
        changed = self.namecall_tail(8)
        changed.strings = [b'other']
        self.assertEqual(compare_acyclic(self.namecall_tail(), changed)['status'], 'different')

    def test_namecall_pending_top_requires_closed_cells_and_disjoint_registers(self):
        for destination, receiver in [(2, 0), (3, 0), (1, 3)]:
            source = self.namecall_tail(destination=destination, receiver=receiver)
            self.assertEqual(compare_acyclic(source, source)['status'], 'unknown')
        for external_main in [False, True]:
            source = self.namecall_tail()
            if external_main:
                source.protos[0].num_upvalues = 1
            else:
                owner = chunk([op('NEWCLOSURE', 0, d=0), op('CAPTURE', 1, 0),
                               op('RETURN', 0, 2)], params=0).protos[0]
                child = chunk([op('GETUPVAL', 0, 0), op('RETURN', 0, 2)],
                              params=0, upvalues=1).protos[0]
                owner.id, owner.children, child.id = 1, [2], 2
                source.protos += [owner, child]
            result = compare_acyclic(source, source)
            self.assertEqual(result['status'], 'unknown')

    def test_namecall_requires_immediate_call_successor(self):
        for pending in [False, True]:
            prefix = [op('MOVE', 3, 1), op('CALL', 3, 1, 0)] if pending else []
            source = chunk([*prefix, op('NAMECALL', 1, 0, aux=0), op('RETURN', 1, 2)],
                           constants=[('str', 1)], strings=[b'method'])
            self.assertEqual(compare_acyclic(source, source)['status'], 'unknown')
            self.assertEqual(compare_dataflow(source, source)['status'], 'unknown')

    def test_adjacent_pack_fallback_preserves_loop_and_pack_identity(self):
        def sample(base, copied=False, argument=0):
            code = [op('JUMPIF', 1, d=-1), op('GETGLOBAL', base, aux=0)]
            if copied:
                code += [op('MOVE', base + 1, base)]
                base += 1
            code += [op('MOVE', base + 1, argument), op('CALL', base, 2, 0),
                     op('RETURN', base, 0)]
            return chunk(code, constants=[('str', 1)], strings=[b'f'])
        result = compare_dataflow(sample(2), sample(5, copied=True))
        self.assertEqual(result['status'], 'proved')
        self.assertEqual(result['model'], 'luau-private-adjacent-pack-cfg-v1')
        negative = compare_dataflow(sample(2), sample(5, copied=True, argument=1))
        self.assertEqual(negative['status'], 'unknown')
        self.assertIn('adjacent_packs', negative)

    def test_implicit_callback_cannot_clobber_high_locals_unobserved(self):
        def sample(slot):
            return chunk([op('GETGLOBAL', 1, aux=0), op('CALL', 1, 1, 0),
                          op('LOADN', slot, d=7), op('GETTABLEKS', 2, 0, aux=1),
                          op('RETURN', slot, 2)], params=1,
                         constants=[('str', 1), ('str', 2)], strings=[b'print', b'x'])
        self.assertEqual(compare_acyclic(sample(10), sample(20))['status'], 'unknown')
        self.assertEqual(compare_dataflow(sample(10), sample(20))['status'], 'unknown')

    def test_c_metamethod_top_reset_cannot_ignore_frame_size(self):
        code = [op('MOVE', 3, 1), op('CALL', 3, 1, 0),
                op('GETTABLEKS', 0, 0, aux=0), op('RETURN', 3, 0)]
        samples = []
        for frame_size in [8, 12]:
            source = chunk(code, constants=[('str', 1)], strings=[b'm'])
            source.protos[0].max_stack = frame_size
            samples.append(source)
        self.assertEqual(compare_acyclic(*samples)['status'], 'unknown')
        self.assertEqual(compare_dataflow(*samples)['status'], 'unknown')

    def test_pending_top_refuses_each_implicit_effect_and_comparison(self):
        effects = [op('GETGLOBAL', 2, aux=0), op('SETGLOBAL', 0, aux=0),
                   op('GETTABLE', 2, 0, 1), op('SETTABLE', 0, 0, 1),
                   op('NAMECALL', 2, 0, aux=0), op('ADD', 2, 0, 1),
                   op('MINUS', 2, 0), op('LENGTH', 2, 0), op('CONCAT', 2, 0, 1),
                   op('JUMPIFEQ', 0, d=1, aux=1)]
        for effect in effects:
            with self.subTest(operation=effect[0]):
                source = chunk([op('CALL', 3, 1, 0), effect, op('RETURN', 0, 2)],
                               params=4, constants=[('str', 1)], strings=[b'field'])
                self.assertEqual(compare_acyclic(source, source)['status'], 'unknown')

    def test_fixed_vararg_copy_does_not_restore_frame_top(self):
        def sample(slot):
            return chunk([op('PREPVARARGS', 2), op('CALL', 1, 1, 0),
                          op('GETVARARGS', 2, 2), op('LOADN', slot, d=7),
                          op('GETTABLEKS', 2, 0, aux=0), op('RETURN', slot, 2)],
                         params=2, vararg=True, constants=[('str', 1)], strings=[b'x'])
        self.assertEqual(compare_acyclic(sample(10), sample(20))['status'], 'unknown')
        self.assertEqual(compare_dataflow(sample(10), sample(20))['status'], 'unknown')

    def test_fixed_writes_cannot_be_forgotten_inside_open_tail(self):
        for producer in ['CALL', 'GETVARARGS']:
            for operation in ['LOADN', 'MOVE']:
                samples = []
                for value in [7, 8]:
                    code = [op('PREPVARARGS', 1)] if producer == 'GETVARARGS' else []
                    code += [op('LOADN', 0, d=value)] if operation == 'MOVE' else []
                    code += [op('GETVARARGS', 1, 0)] if producer == 'GETVARARGS' else [op('CALL', 1, 1, 0)]
                    code += [op('MOVE', 2, 0) if operation == 'MOVE' else op('LOADN', 2, d=value),
                             op('RETURN', 1, 0)]
                    samples.append(chunk(code, params=1 if producer == 'GETVARARGS' else 2,
                                         vararg=producer == 'GETVARARGS'))
                with self.subTest(producer=producer, operation=operation):
                    self.assertEqual(compare_acyclic(*samples)['status'], 'unknown')
                    self.assertEqual(compare_dataflow(*samples)['status'], 'unknown')

    def test_unknown_arity_does_not_define_every_fixed_register(self):
        for producer in ['CALL', 'FASTCALL1', 'GETVARARGS']:
            with self.subTest(producer=producer):
                samples = []
                for value in [7, 8]:
                    if producer == 'GETVARARGS':
                        code = [op('PREPVARARGS'), op('LOADN', 10, d=value),
                                op('GETVARARGS', 2), op('RETURN', 10, 2)]
                    else:
                        code = [op('LOADN', 10, d=value), op('LOADN', 0, d=5),
                                op('FASTCALL1', 40, 0, 3) if producer == 'FASTCALL1' else op('NOP'),
                                op('MOVE', 3, 0), op('GETGLOBAL', 2, aux=0),
                                op('CALL', 2, 2, 0), op('RETURN', 10, 2)]
                    samples.append(chunk(code, params=0, vararg=producer == 'GETVARARGS',
                                         constants=[('str', 1)], strings=[b'type']))
                self.assertEqual(compare_acyclic(*samples)['status'], 'unknown')
                self.assertNotEqual(compare_dataflow(*samples)['status'], 'proved')

    def test_open_tail_pack_itself_remains_supported(self):
        for producer in ['CALL', 'GETVARARGS']:
            code = ([op('CALL', 0, 1, 0)] if producer == 'CALL' else
                    [op('PREPVARARGS'), op('GETVARARGS', 0)])
            source = chunk([*code, op('RETURN', 0, 0)], params=1 if producer == 'CALL' else 0,
                           vararg=producer == 'GETVARARGS')
            self.assertEqual(compare_acyclic(source, source)['status'], 'proved')


if __name__ == '__main__':
    unittest.main()
