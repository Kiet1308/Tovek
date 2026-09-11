import copy
import unittest

from bytecode_dataflow import compare_dataflow
from bytecode_roundtrip import Chunk, Proto, OP_INDEX, _decode_insn, compare_chunks


def instruction(name, a=0, b=0, c=0, *, d=None, aux=0):
    word = OP_INDEX[name] | (a << 8)
    word |= ((d & 0xffff) << 16) if d is not None else (b << 16) | (c << 24)
    return (*_decode_insn(word, 1), aux)


def chunk(code, params=2, constants=(), strings=(), upvalues=0, vararg=False):
    from bytecode_roundtrip import AUX_OPS
    p = Proto()
    p.id, p.max_stack, p.num_params = 0, 32, params
    p.num_upvalues, p.is_vararg = upvalues, vararg
    p.constants, p.children, p.line_defined, p.name = list(constants), [], 0, 0
    pc = 0
    for insn in code:
        p.insns.append((pc, *insn))
        pc += 2 if insn[0] in AUX_OPS else 1
    p.code = [0] * pc
    ch = Chunk()
    ch.version, ch.types_version, ch.main = 9, 3, 0
    ch.strings, ch.protos = list(strings), [p]
    return ch


class DataflowTests(unittest.TestCase):
    def status(self, a, b, expected):
        self.assertEqual(compare_dataflow(a, b)["status"], expected)

    def test_three_legacy_false_positives(self):
        pairs = [
            ([instruction("SUB", 2, 0, 1), instruction("RETURN", 2, 2)],
             [instruction("SUB", 2, 1, 0), instruction("RETURN", 2, 2)]),
            ([instruction("RETURN", 0, 2)], [instruction("RETURN", 1, 2)]),
            ([instruction("SETTABLEKS", 0, 2), instruction("RETURN", 0, 1)],
             [instruction("SETTABLEKS", 1, 2), instruction("RETURN", 0, 1)]),
        ]
        for a, b in pairs:
            left, right = [chunk(c, params=3, constants=[("str", 1)], strings=[b"Value"])
                           for c in (a, b)]
            self.assertEqual(compare_chunks(left, right)[0][0]["tier"], "exact")
            self.status(left, right, "different")

    def test_temporary_renumber_and_copy(self):
        a = chunk([instruction("SUB", 2, 0, 1), instruction("RETURN", 2, 2)])
        b = chunk([instruction("MOVE", 5, 0), instruction("SUB", 9, 5, 1),
                   instruction("MOVE", 11, 9), instruction("RETURN", 11, 2)])
        self.status(a, b, "proved")

    def test_constant_and_string_pool_reorder_and_metadata(self):
        a = chunk([instruction("LOADK", 2, d=0), instruction("RETURN", 2, 2)],
                  constants=[("str", 1), ("num", 3.0)], strings=[b"ok", b"other"])
        b = chunk([instruction("LOADK", 8, d=1), instruction("RETURN", 8, 2)],
                  constants=[("num", 3.0), ("str", 2)], strings=[b"other", b"ok"])
        b.protos[0].name, b.protos[0].line_defined = 1, 90
        self.status(a, b, "proved")

    def test_signed_zero_and_string_bytes_stay_distinct(self):
        for left, right in (([("num", 0.0)], [("num", -0.0)]),
                            ([("str", 1)], [("str", 2)])):
            code = [instruction("LOADK", 2, d=0), instruction("RETURN", 2, 2)]
            self.status(chunk(code, constants=left, strings=[b"\xff", b"\\xff"]),
                        chunk(code, constants=right, strings=[b"\xff", b"\\xff"]), "different")

    def test_branch_polarity_and_successors(self):
        a = chunk([instruction("JUMPIF", 0, d=1), instruction("RETURN", 0, 2),
                   instruction("RETURN", 1, 2)])
        bad = copy.deepcopy(a)
        bad.protos[0].insns[0] = (0, *instruction("JUMPIFNOT", 0, d=1))
        self.status(a, bad, "different")
        b = chunk([instruction("JUMPIFNOT", 0, d=1), instruction("RETURN", 1, 2),
                   instruction("RETURN", 0, 2)])
        self.status(a, b, "proved")

    def test_call_target_arguments_and_return_arity(self):
        a = chunk([instruction("CALL", 0, 2, 0), instruction("RETURN", 0, 0)])
        b = chunk([instruction("MOVE", 3, 1), instruction("MOVE", 4, 0),
                   instruction("CALL", 3, 2, 0), instruction("RETURN", 3, 0)])
        self.status(a, b, "different")
        b = chunk([instruction("CALL", 0, 2, 2), instruction("RETURN", 0, 2)])
        self.status(a, b, "different")
        self.status(a, a, "proved")

    def test_effect_order_and_upvalue_slot(self):
        effects = [instruction("SETGLOBAL", 0, aux=0), instruction("SETGLOBAL", 1, aux=1)]
        kwargs = dict(constants=[("str", 1), ("str", 2)], strings=[b"x", b"y"])
        self.status(chunk([*effects, instruction("RETURN", 0, 1)], **kwargs),
                    chunk([*reversed(effects), instruction("RETURN", 0, 1)], **kwargs), "different")
        self.status(chunk([instruction("GETUPVAL", 2, 0), instruction("RETURN", 2, 2)], upvalues=2),
                    chunk([instruction("GETUPVAL", 2, 1), instruction("RETURN", 2, 2)], upvalues=2),
                    "different")

    def test_capture_close_and_iterator_mutants_never_proved(self):
        for name, a in (("CAPTURE", 0), ("CAPTURE", 1), ("CLOSEUPVALS", 0),
                        ("FORGPREP", 0), ("FORGLOOP", 0), ("JUMPBACK", 0)):
            source = chunk([instruction(name, a), instruction("RETURN", 0, 1)])
            other = chunk([instruction("RETURN", 0, 1)])
            self.status(source, other, "unknown")
            self.status(source, source, "proved" if name in ("CLOSEUPVALS", "JUMPBACK") else "unknown")

    def test_value_capture_binding_and_reference_capture_refusal(self):
        child = chunk([instruction("GETUPVAL", 0, 0), instruction("RETURN", 0, 2)],
                      params=0, upvalues=1).protos[0]
        child.id = 1

        def closure(mode, register, close=False):
            code = [instruction("NEWCLOSURE", 2, d=0), instruction("CAPTURE", mode, register)]
            if close:
                code.append(instruction("CLOSEUPVALS", 0))
            code.append(instruction("RETURN", 2, 2))
            result = chunk(code)
            result.protos[0].children = [1]
            result.protos.append(copy.deepcopy(child))
            return result

        a = closure(0, 0)
        self.status(a, a, "proved")
        self.status(a, closure(0, 1), "different")
        self.status(a, closure(1, 0), "unknown")
        self.status(closure(1, 0, True), closure(1, 0), "unknown")

    def test_budget_invalid_target_and_uninitialized_read(self):
        a = chunk([instruction("RETURN", 0, 2)])
        self.assertEqual(compare_dataflow(a, a, budget=0)["status"], "unknown")
        for insn in (instruction("JUMP", d=20),
                     instruction("RETURN", 9, 2)):
            b = chunk([insn])
            self.status(b, b, "unknown")


if __name__ == "__main__":
    unittest.main()
