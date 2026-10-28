import copy
import unittest

import bytecode_dataflow as model
from test_bytecode_dataflow import chunk, instruction


def fixture(saved=False, shared=False):
    leaf = chunk([instruction('GETUPVAL', 0, 0), instruction('GETUPVAL', 1, 1),
                  instruction('RETURN', 0, 3)], params=0, upvalues=2).protos[0]
    leaf.id = 2
    kind = 'DUPCLOSURE' if shared else 'NEWCLOSURE'
    code = [instruction(kind, 2, d=0), instruction('CAPTURE', 2, 0), instruction('CAPTURE', 2, 1)]
    if shared:
        code += [instruction(kind, 3, d=0), instruction('CAPTURE', 2, 0), instruction('CAPTURE', 2, 1)]
    code += [instruction('RETURN', 2, 3 if shared else 2)]
    middle = chunk(code, upvalues=2, constants=[('closure', 2), ('closure', 2)]).protos[0]
    middle.id, middle.children = 1, [2]
    code = [instruction('MOVE', 7, 0), instruction('MOVE', 8, 1)] if saved else []
    register = 10 if saved else 2
    code += [instruction('NEWCLOSURE', register, d=0), instruction('CAPTURE', 0, 7 if saved else 0),
             instruction('CAPTURE', 0, 8 if saved else 1), instruction('RETURN', register, 2)]
    root = chunk(code)
    root.protos[0].children = [1]
    root.protos += [middle, leaf]
    return root


class ForwardedCaptureTests(unittest.TestCase):
    def test_alias_copies_preserve_forwarded_parent_cells(self):
        for shared in [False, True]:
            self.assertEqual(model.compare_acyclic(fixture(shared=shared), fixture(True, shared))['status'], 'proved')

    def test_changed_slots_modes_writes_and_sharing_are_not_proved(self):
        for change in ['slot', 'order', 'mode', 'write', 'sharing']:
            with self.subTest(change=change):
                original = fixture(shared=change == 'sharing')
                mutated = copy.deepcopy(original)
                middle = mutated.protos[1]
                if change == 'slot':
                    middle.insns[1] = (1, *instruction('CAPTURE', 2, 1))
                elif change == 'order':
                    middle.insns[1] = (1, *instruction('CAPTURE', 2, 1))
                    middle.insns[2] = (2, *instruction('CAPTURE', 2, 0))
                elif change == 'mode':
                    middle.insns[1] = (1, *instruction('CAPTURE', 0, 0))
                elif change == 'write':
                    leaf = mutated.protos[2]
                    leaf.insns = [leaf.insns[0], (1, *instruction('SETUPVAL', 0, 1)),
                                  (2, *leaf.insns[1][1:]), (3, *leaf.insns[2][1:])]
                    leaf.code = [0] * 4
                else:
                    middle.insns[3] = (3, *instruction('DUPCLOSURE', 3, d=1))
                self.assertEqual(model.compare_acyclic(original, mutated)['status'], 'different')

    def test_external_reference_and_invalid_slots_keep_barriers(self):
        for change in ['external', 'ref', 'unreachable_ref', 'invalid_slot', 'invalid_mode']:
            with self.subTest(change=change):
                original, rebuilt = fixture(), fixture(True)
                for sample in [original, rebuilt]:
                    if change == 'external':
                        sample.protos[0].num_upvalues = 1
                    elif change == 'ref':
                        row = sample.protos[0].insns[1 if sample is original else 3]
                        sample.protos[0].insns[1 if sample is original else 3] = (row[0], *instruction('CAPTURE', 1, 0))
                    elif change == 'unreachable_ref':
                        extra = chunk([instruction('CAPTURE', 1, 0), instruction('RETURN', 0, 1)]).protos[0]
                        sample.protos.append(extra)
                    else:
                        sample.protos[1].insns[1] = (1, *instruction('CAPTURE', 2 if change == 'invalid_slot' else 3, 2 if change == 'invalid_slot' else 0))
                self.assertEqual(model.compare_acyclic(original, rebuilt)['status'], 'unknown')

    def test_domain_traversal_obeys_budget(self):
        self.assertEqual(model.compare_acyclic(fixture(), fixture(True), budget=5)['status'], 'unknown')

    def test_external_main_does_not_float_table_publication(self):
        read = instruction('GETGLOBAL', 1, aux=0)
        allocate = instruction('NEWTABLE', 0, aux=0)
        kwargs = dict(params=0, upvalues=1, constants=[('str', 1)], strings=[b'observe'])
        before = chunk([read, allocate, instruction('RETURN', 0, 2)], **kwargs)
        after = chunk([allocate, read, instruction('RETURN', 0, 2)], **kwargs)
        self.assertEqual(model.compare_acyclic(before, after)['status'], 'unknown')

    def test_external_cells_keep_physical_register_identity(self):
        original = chunk([instruction('LOADN', 0, d=7), instruction('GETUPVAL', 1, 0),
                          instruction('RETURN', 1, 2)], params=0, upvalues=1)
        renamed = chunk([instruction('LOADN', 2, d=7), instruction('GETUPVAL', 1, 0),
                         instruction('RETURN', 1, 2)], params=0, upvalues=1)
        self.assertEqual(model.compare_acyclic(original, renamed)['status'], 'unknown')
        self.assertEqual(model.compare_dataflow(original, renamed)['status'], 'unknown')
        identical = model.compare_dataflow(original, original)
        self.assertEqual(identical['status'], 'proved')
        self.assertEqual(identical['model'], 'luau-register-cfg-bisimulation-v2')


def self_capture(destination=0, source=0, prior=True, shared=False, mode=0):
    leaf = chunk([instruction('GETUPVAL', 0, 0), instruction('RETURN', 0, 2)],
                 params=0, upvalues=1).protos[0]
    leaf.id = 1
    code = [instruction('LOADN', source, d=7)] if prior else []
    code += [instruction('DUPCLOSURE' if shared else 'NEWCLOSURE', destination, d=0),
             instruction('CAPTURE', mode, source), instruction('RETURN', destination, 2)]
    result = chunk(code, params=0, constants=[('closure', 1)])
    result.protos[0].children = [1]
    result.protos.append(leaf)
    return result


class SelfCaptureTests(unittest.TestCase):
    def test_newclosure_binds_self_before_captures(self):
        for prior in [False, True]:
            self.assertEqual(model.compare_acyclic(self_capture(prior=prior),
                self_capture(7, 7, prior=prior))['status'], 'proved')

    def test_self_and_old_destination_value_are_different(self):
        self.assertEqual(model.compare_dataflow(self_capture(),
            self_capture(0, 1))['status'], 'different')
        self.assertEqual(model.compare_acyclic(self_capture(prior=False),
            self_capture())['status'], 'proved')

    def test_other_register_value_capture_stays_unchanged(self):
        self.assertEqual(model.compare_acyclic(self_capture(0, 1),
            self_capture(5, 7))['status'], 'proved')

    def test_shared_self_reference_and_invalid_source_remain_unknown(self):
        for prior in [False, True]:
            shared = self_capture(prior=prior, shared=True)
            self.assertEqual(model.compare_acyclic(shared, shared)['status'], 'unknown')
        self.assertEqual(model.compare_dataflow(self_capture(shared=True),
            self_capture(0, 1, shared=True))['status'], 'unknown')
        self.assertEqual(model.compare_acyclic(self_capture(mode=1),
            self_capture())['status'], 'unknown')
        self.assertEqual(model.compare_acyclic(self_capture(0, 31, prior=False),
            self_capture())['status'], 'unknown')


if __name__ == '__main__':
    unittest.main()
