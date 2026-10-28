import copy
import unittest

from test_bytecode_dataflow import chunk, instruction
from bytecode_roundtrip import OP_INDEX, OPCODES, AUX_OPS
from bytecode_dataflow import compare_acyclic, symbolic_tree


def subject(kind, base=6, copied=False):
    count = 1 if kind == 'FASTCALL1' else 3 if kind == 'FASTCALL3' else 2
    setup = ([instruction('MOVE', 20, 0), instruction('MOVE', base + 1, 20)] if copied
             else [instruction('MOVE', base + 1, 0)])
    setup += [instruction('MOVE', base + 1 + n, n) for n in range(1, count)]
    aux = 1 | (2 << 8) if kind == 'FASTCALL3' else 1
    width = 2 if OP_INDEX[kind] in AUX_OPS else 1
    code = setup + [instruction(kind, 40, 0, width + 1, aux=aux),
                    instruction('GETGLOBAL', base, aux=0),
                    instruction('CALL', base, count + 1, 2),
                    instruction('RETURN', base, 2)]
    return chunk(code, params=3, constants=[('str', 1), ('num', 3.0)], strings=[b'fallback'])


class FastcallTests(unittest.TestCase):
    def test_each_variant_keeps_two_paths_and_allows_copy_temporary_renaming(self):
        for kind in ('FASTCALL', 'FASTCALL1', 'FASTCALL2', 'FASTCALL2K', 'FASTCALL3'):
            with self.subTest(kind=kind):
                original = subject(kind)
                tree = symbolic_tree(original, original.protos[0])
                self.assertIn('FASTCALL_SUCCESS', repr(tree))
                self.assertIn('GETGLOBAL', repr(tree))
                self.assertIn("'CALL'", repr(tree))
                self.assertEqual(compare_acyclic(original, subject(kind, base=10, copied=True))['status'], 'proved')

    def test_builtin_argument_fallback_and_result_mutants_never_proved(self):
        for kind in ('FASTCALL', 'FASTCALL1', 'FASTCALL2', 'FASTCALL2K', 'FASTCALL3'):
            original = subject(kind)
            for mutation in ('id', 'argument', 'fallback', 'arity', 'skip', 'remove', 'aux'):
                with self.subTest(kind=kind, mutation=mutation):
                    bad = copy.deepcopy(original)
                    p = bad.protos[0]
                    at = next(n for n, row in enumerate(p.insns) if OPCODES[row[1]] == kind)
                    pc, _, a, b, c, _, _, aux = p.insns[at]
                    if mutation == 'fallback':
                        bad.strings = [b'other']
                    elif mutation == 'arity':
                        n = next(n for n, row in enumerate(p.insns) if OPCODES[row[1]] == 'CALL')
                        row = p.insns[n]
                        p.insns[n] = (row[0], *instruction('CALL', row[2], row[3], 3))
                    elif mutation == 'remove':
                        p.insns[at] = (pc, *instruction('NOP'))
                    elif mutation == 'argument' and kind == 'FASTCALL':
                        p.insns[0] = (0, *instruction('MOVE', 7, 1))
                    else:
                        if mutation == 'id': a += 1
                        elif mutation == 'argument': b = 1
                        elif mutation == 'skip': c -= 1
                        elif mutation == 'aux':
                            if kind == 'FASTCALL3': aux |= 1 << 16
                            elif kind in ('FASTCALL2', 'FASTCALL2K'): aux = 99
                            else: a = 0
                        p.insns[at] = (pc, *instruction(kind, a, b, c, aux=aux))
                    self.assertNotEqual(compare_acyclic(original, bad)['status'], 'proved')

    def test_budget_and_uninitialized_result_are_not_certificates(self):
        original = subject('FASTCALL1')
        self.assertEqual(compare_acyclic(original, original, budget=1)['status'], 'unknown')
        broken = copy.deepcopy(original)
        p = broken.protos[0]
        at = next(n for n, row in enumerate(p.insns) if OPCODES[row[1]] == 'CALL')
        pc, _, a, b, _, _, _, _ = p.insns[at]
        p.insns[at] = (pc, *instruction('CALL', a, b, 1))
        self.assertEqual(compare_acyclic(broken, broken)['status'], 'unknown')

    def test_open_results_cannot_invent_fixed_register_values(self):
        def open_result(value):
            return chunk([
                instruction('LOADN', 10, d=value),
                instruction('FASTCALL1', 40, 0, 3),
                instruction('GETGLOBAL', 2, aux=0),
                instruction('MOVE', 3, 0),
                instruction('CALL', 2, 2, 0),
                instruction('RETURN', 10, 2),
            ], params=1, constants=[('str', 1)], strings=[b'type'])
        self.assertEqual(compare_acyclic(open_result(7), open_result(8))['status'], 'unknown')


class AdditionalFastcallTests(unittest.TestCase):
    def test_fastcall3_scratch_clobber_after_open_pack_is_unknown(self):
        def sample(base):
            return chunk([instruction('GETGLOBAL', 0, aux=0), instruction('CALL', 0, 1, 0),
                          instruction('LOADB', 4), instruction('LOADN', 2, d=2), instruction('LOADN', 3, d=3),
                          instruction('GETGLOBAL', base, aux=0), instruction('FASTCALL3', 2, 4, 1, aux=2 | (3 << 8)),
                          instruction('CALL', base, 1, 0), instruction('RETURN', base, 0)],
                         params=0, constants=[('str', 1)], strings=[b'print'])
        self.assertEqual(compare_acyclic(sample(0), sample(10))['status'], 'unknown')

    def test_vararg_setup_required_at_entry_and_only_once(self):
        fixed = subject('FASTCALL3')
        missing = copy.deepcopy(fixed)
        missing.protos[0].is_vararg = True
        self.assertEqual(compare_acyclic(missing, missing)['status'], 'unknown')
        p = fixed.protos[0]
        setup = instruction('PREPVARARGS', p.num_params)
        good = chunk([setup, *[row[1:] for row in p.insns]], params=p.num_params,
                     vararg=True, constants=p.constants, strings=fixed.strings)
        self.assertEqual(compare_acyclic(good, good)['status'], 'proved')
        bad = chunk([setup, setup, *[row[1:] for row in p.insns]], params=p.num_params,
                    vararg=True, constants=p.constants, strings=fixed.strings)
        self.assertEqual(compare_acyclic(bad, bad)['status'], 'unknown')

    def test_generic_open_argument_pack_and_open_result_are_preserved(self):
        def sample(base, varargbase):
            return chunk([instruction('PREPVARARGS', 1), instruction('GETVARARGS', varargbase, 0),
                          instruction('FASTCALL', 40, 0, 1), instruction('MOVE', base, 0),
                          instruction('CALL', base, 0, 0), instruction('RETURN', base, 0)],
                         params=1, vararg=True)
        self.assertEqual(compare_acyclic(sample(2, 3), sample(8, 9))['status'], 'proved')
        self.assertEqual(compare_acyclic(sample(2, 3), sample(8, 10))['status'], 'unknown')

    def test_argument_and_result_bounds_refuse_even_for_self_comparison(self):
        for variant in ('FASTCALL', 'FASTCALL1', 'FASTCALL2', 'FASTCALL2K', 'FASTCALL3'):
            for mutation in ('builtin_zero', 'call_result_span', 'call_argument_span', 'skip_back'):
                bad = subject(variant)
                p = bad.protos[0]
                at = next(i for i, row in enumerate(p.insns) if OPCODES[row[1]] == variant)
                row = p.insns[at]
                call_at = next(i for i, row in enumerate(p.insns) if OPCODES[row[1]] == 'CALL')
                call = p.insns[call_at]
                if mutation == 'builtin_zero':
                    p.insns[at] = (row[0], *instruction(variant, 0, row[3], row[4], aux=row[-1]))
                elif mutation == 'call_result_span':
                    p.insns[call_at] = (call[0], *instruction('CALL', 30, call[3], 5))
                elif mutation == 'call_argument_span':
                    p.insns[call_at] = (call[0], *instruction('CALL', call[2], 255, call[4]))
                else:
                    p.insns[at] = (row[0], *instruction(variant, row[2], row[3], 0, aux=row[-1]))
                with self.subTest(variant=variant, mutation=mutation):
                    self.assertEqual(compare_acyclic(bad, bad)['status'], 'unknown')

    def test_specialized_fixed_results_reject_pending_tinstruction(self):
        for variant in ('FASTCALL1', 'FASTCALL2', 'FASTCALL2K', 'FASTCALL3'):
            original = subject(variant)
            p = original.protos[0]
            pending = chunk([instruction('PREPVARARGS', p.num_params), instruction('GETVARARGS', 25, 0),
                             *[row[1:] for row in p.insns]], params=p.num_params,
                            vararg=True, constants=p.constants, strings=original.strings)
            self.assertEqual(compare_acyclic(pending, pending)['status'], 'unknown')


if __name__ == "__main__":
    unittest.main()
