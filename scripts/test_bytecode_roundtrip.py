import collections
import struct
import unittest

from bytecode_roundtrip import (BytecodeError, Reader, _cancel_counted_setlists, capture_excess, capture_observations,
                                compare_captures, compare_chunks, parse_chunk, OP_INDEX)
from test_bytecode_dataflow import chunk, instruction as ins


def varint(n):
    result = bytearray()
    while n >= 128:
        result.append((n & 127) | 128)
        n >>= 7
    result.append(n)
    return bytes(result)


def proto_body(*, version=12, key=1, cost=None, extension=b"", feedback=b"\0", words=None):
    words = words or [4 | (42 << 16), 22 | (2 << 16)]
    encoded = []
    aux_next = False
    from bytecode_roundtrip import AUX_OPS
    for word in words:
        encoded.append(word if aux_next else (word & ~255) | ((word & 255) * pow(key, -1, 256) & 255))
        aux_next = not aux_next and (word & 255) in AUX_OPS
    body = bytes([2, 0, 0, 0, 8 if cost is not None else 0, 0])
    body += varint(len(words)) + b"".join(struct.pack("<I", w) for w in encoded)
    body += b"\0" * 6  # constants, children, line, name, line info, debug info
    if version >= 11:
        body += feedback
    if version >= 12 and cost is not None:
        body += varint(cost)
    return body + extension


def chunk_bytes(bodies, *, version=12, main=0, sizes=None):
    result = bytes([version, 3, 0, 0]) + varint(len(bodies))
    for i, body in enumerate(bodies):
        if version >= 12:
            result += varint(len(body) if sizes is None else sizes[i])
        result += body
    return result + varint(main)


class V12ReaderTests(unittest.TestCase):
    def test_wide_cost_extensions_and_next_proto_both_keys(self):
        for key in (1, 203):
            for cost in (0, 127, 128, 1 << 32, 1 << 63, (1 << 64) - 1):
                with self.subTest(key=key, cost=cost):
                    bodies = [proto_body(key=key, cost=cost, extension=b"\xa5\xff\x80"),
                              proto_body(key=key)]
                    ch = parse_chunk(chunk_bytes(bodies, main=1) + b"opaque trailer", key)
                    self.assertEqual(ch.protos[0].cost, cost)
                    self.assertEqual(ch.protos[0].extension_bytes, b"\xa5\xff\x80")
                    self.assertEqual(ch.protos[1].insns[0][5], 42)
                    self.assertEqual(ch.main, 1)
                    self.assertEqual(ch.trailing_bytes, b"opaque trailer")

    def test_feedback_aux_and_runtime_guard_remain_visible(self):
        words = [87 | (1 << 16) | (1 << 24), 0, 88 | (1 << 16), 123, 22 | (1 << 16)]
        for key in (1, 203):
            ch = parse_chunk(chunk_bytes([proto_body(key=key, words=words, feedback=b"\1\0\0")]), key)
            self.assertEqual([i[0] for i in ch.protos[0].insns], [0, 2, 4])
            self.assertEqual(ch.protos[0].insns[1][1], OP_INDEX["CMPPROTO"])
            self.assertEqual(ch.protos[0].insns[1][7], 123)

    def test_v14_fastpcall_is_a_plain_abc_instruction(self):
        # FASTPCALL (A=0 pcall, B=2 explicit args, C=1) then RETURN; decoding only.
        words = [89 | (2 << 16) | (1 << 24), 22 | (1 << 16)]
        for key in (1, 203):
            ch = parse_chunk(chunk_bytes([proto_body(version=14, key=key, words=words)], version=14), key)
            self.assertEqual(ch.version, 14)
            self.assertEqual([i[1] for i in ch.protos[0].insns], [OP_INDEX["FASTPCALL"], OP_INDEX["RETURN"]])
            self.assertEqual(ch.protos[0].insns[0][4], 1)

    def test_v13_double_vector_constant(self):
        body = bytearray(proto_body(version=13))
        # Replace the empty constant list (first of the six trailing zero bytes
        # before the feedback vector) with one VECTORD constant.
        constants_at = len(body) - 7
        vector = struct.pack("<4d", 1e300, -2.5, 16777217.0, 0.0)
        body[constants_at:constants_at + 1] = b"" + vector
        ch = parse_chunk(chunk_bytes([bytes(body)], version=13), 1)
        self.assertEqual(ch.protos[0].constants, [("vec", (1e300, -2.5, 16777217.0, 0.0))])

    def test_version_after_14_is_rejected(self):
        with self.assertRaises(BytecodeError):
            parse_chunk(chunk_bytes([proto_body(version=14)], version=15), 1)

    def test_previous_serializations_still_parse_without_size_or_cost(self):
        for version in range(4, 12):
            ch = parse_chunk(chunk_bytes([proto_body(version=version)], version=version), 1)
            self.assertEqual(ch.protos[0].insns[0][5], 42)
            self.assertIsNone(ch.protos[0].cost)

    def test_bad_boundaries_feedback_cost_and_main_fail(self):
        body = proto_body()
        valid = chunk_bytes([body])
        cases = {
            "zero size": chunk_bytes([body], sizes=[0]),
            "short size": chunk_bytes([body, body], sizes=[len(body) - 1, len(body)]),
            "oversized": chunk_bytes([body], sizes=[len(body) + 2]),
            "truncated body": valid[:-3],
            "no main": valid[:-1],
            "bad main": chunk_bytes([body], main=1),
            "empty protos": chunk_bytes([]),
            "bad feedback": chunk_bytes([proto_body(feedback=b"\1\1\0")]),
            "truncated cost": chunk_bytes([proto_body(cost=0)[:-1] + b"\x80", body], main=1),
            "cost overflow": chunk_bytes([proto_body(cost=1 << 64)]),
            "cost too long": chunk_bytes([proto_body(cost=0)[:-1] + b"\x80" * 10 + b"\0"]),
            "bad aux": chunk_bytes([proto_body(words=[87])]),
        }
        for name, data in cases.items():
            with self.subTest(name=name), self.assertRaises(BytecodeError):
                parse_chunk(data, 1)

    def test_varint_width_and_negative_length_are_bounded(self):
        self.assertEqual(Reader(varint((1 << 64) - 1)).varint(64), (1 << 64) - 1)
        for data in (varint(1 << 32), b"\x80" * 10):
            with self.assertRaises(BytecodeError):
                Reader(data).varint()
        with self.assertRaises(BytecodeError):
            Reader(b"abc").bytes(-1)


class TableTemplateTests(unittest.TestCase):
    def template(self, template, constants=(), strings=(b"field",)):
        pool = [("str", 1), *constants, template]
        return chunk([ins("DUPTABLE", 0, d=len(pool) - 1), ins("RETURN", 0, 2)],
                     params=0, constants=pool, strings=strings)

    def tier(self, a, b):
        rows, missing, extra = compare_chunks(a, b)
        self.assertEqual((missing, extra), ([], []))
        return rows[0]["tier"]

    def empty(self):
        return chunk([ins("NEWTABLE", 0), ins("RETURN", 0, 2)], params=0)

    def test_both_template_tags_store_zero_not_nil(self):
        zero = self.template(("tablek", ((0, 1),)), [("num", 0.0)])
        nil = self.template(("tablek", ((0, 1),)), [("nil",)])
        for template in (("table", (0,)), ("tablek", ((0, -1),)), ("tablek", ((0, -2),))):
            with self.subTest(template=template):
                source = self.template(template)
                self.assertIn(self.tier(source, zero), ("exact", "equiv"))
                self.assertEqual(self.tier(source, nil), "differ")
                self.assertEqual(self.tier(source, self.empty()), "differ")

    def test_unfolded_template_matches_explicit_zero_store(self):
        # NEWTABLE + LOADN + SETTABLEKS must include the same default value.
        explicit = chunk([ins("NEWTABLE", 0), ins("LOADN", 1, d=0),
                          ins("SETTABLEKS", 1, 0, aux=0), ins("RETURN", 0, 2)],
                         params=0, constants=[("str", 1)], strings=[b"field"])
        source = self.template(("table", (0,)))
        self.assertEqual(self.tier(source, explicit), "equiv")
        explicit.protos[0].insns[1] = (2, *ins("LOADN", 1, d=1))
        self.assertEqual(self.tier(source, explicit), "differ")

    def test_duplicate_keys_use_final_value_and_nil_second_pass(self):
        # Two pool indices (and two string IDs) can identify the same key.
        pool = [("str", 2), ("num", 7.0), ("nil",)]
        expected = self.template(("tablek", ((0, 1),)), [("num", 7.0)])
        duplicate = self.template(("tablek", ((0, -1), (1, 2))), pool, (b"field", b"field"))
        self.assertEqual(self.tier(duplicate, expected), "equiv")
        for pairs in (((0, 3), (1, 2)), ((0, 2), (1, 3))):
            with self.subTest(pairs=pairs):
                source = self.template(("tablek", pairs), pool, (b"field", b"field"))
                self.assertEqual(self.tier(source, self.empty()), "equiv")
                self.assertEqual(self.tier(source, expected), "differ")
        repeated = self.template(("table", (0, 1)), [("str", 2)], (b"field", b"field"))
        self.assertEqual(self.tier(repeated, self.template(("table", (0,)))), "equiv")

    def test_false_and_unsupported_keys_are_not_dropped(self):
        false = self.template(("tablek", ((0, 1),)), [("bool", False)])
        self.assertEqual(self.tier(false, self.empty()), "differ")
        numeric_key = self.template(("table", (1,)), [("num", 1.0)])
        self.assertEqual(self.tier(numeric_key, self.empty()), "differ")
        self.assertIn('DUPTABLE({1=0})', numeric_key.protos[0].sig)

    def test_vector_constructor_reads_remain_visible(self):
        # A constructor snapshot is a real environment read. Never cancel it
        # merely to make a legacy baseline pass.
        source = chunk([ins("RETURN", 0, 1)], params=0)
        rebuilt = chunk([ins("GETIMPORT", 0, d=2, aux=(2 << 30) | (1 << 10)),
                         ins("RETURN", 0, 1)], params=0,
                        constants=[("str", 1), ("str", 2), ("import", (2 << 30) | (1 << 10))],
                        strings=[b"vector", b"create"])
        rows, _, _ = compare_chunks(source, rebuilt)
        self.assertEqual(rows[0]["tier"], "differ")
        self.assertEqual(rows[0]["delta"]["added"],
                         {'GETIMPORT(@vector)': 1, 'GETTABLEKS("create")': 1})


class CountedSetListTriageTests(unittest.TestCase):
    def scaffold(self, base):
        result = collections.Counter({
            "GETIMPORT(@table)": 1, 'GETTABLEKS("pack")': 1, "CALL(*)": 1,
            'GETTABLEKS("n")': 1, "LOADK(1)": 2,
            "FORNPREP": 1, "FORNLOOP": 1, "GETTABLE": 1, "SETTABLE": 1,
        })
        if base:
            result["ADD"] += 1
            result[f"LOADK({base})"] += 1
        return result

    def test_exact_scaffold_and_offset_are_cancelled(self):
        for base in (0, 1, 3):
            with self.subTest(base=base):
                lost = collections.Counter({f"SETLIST(*,{base + 1})": 1})
                added = self.scaffold(base)
                _cancel_counted_setlists(lost, added)
                self.assertFalse(+lost)
                self.assertFalse(+added)

    def test_missing_count_read_or_wrong_offset_stays_visible(self):
        for variant in ("missing_count", "wrong_offset", "no_setlist"):
            with self.subTest(variant=variant):
                lost = collections.Counter({"SETLIST(*,4)": 1})
                added = self.scaffold(3)
                if variant == "missing_count":
                    del added['GETTABLEKS("n")']
                elif variant == "wrong_offset":
                    del added["LOADK(3)"]
                    added["LOADK(4)"] = 1
                else:
                    lost.clear()
                before = (lost.copy(), added.copy())
                _cancel_counted_setlists(lost, added)
                self.assertEqual((lost, added), before)

    def test_unrelated_call_is_never_cancelled(self):
        lost = collections.Counter({"SETLIST(*,1)": 1})
        added = self.scaffold(0)
        added["CALL(*)"] += 1
        added['GETIMPORT(@effect)'] = 1
        _cancel_counted_setlists(lost, added)
        self.assertEqual(+added, collections.Counter({"CALL(*)": 1, 'GETIMPORT(@effect)': 1}))


class CaptureObservationTests(unittest.TestCase):
    """A by-reference capture counts when its closure can see the variable
    change: a write of the register reachable before CLOSEUPVALS closes it,
    or a capture of the closure's own register."""

    REF, VAL = 1, 0

    def kinds(self, code, **kw):
        return [hit["kind"] for hit in capture_observations(chunk(code, params=0, **kw))]

    def closure(self, destination, register, kind=REF):
        return [ins("NEWCLOSURE", destination, d=0), ins("CAPTURE", kind, register)]

    def test_later_write_of_a_reference_capture_is_observed(self):
        code = self.closure(1, 0) + [ins("LOADN", 0, d=5), ins("RETURN", 1, 2)]
        self.assertEqual(self.kinds(code), ["write"])
        # A by-value capture keeps its value whatever happens to the register.
        self.assertEqual(self.kinds(self.closure(1, 0, self.VAL) + code[2:]), [])
        # No write after the closure: nothing to see.
        self.assertEqual(self.kinds(self.closure(1, 0) + [ins("RETURN", 1, 2)]), [])

    def test_a_closure_capturing_its_own_register(self):
        # `local function f() f() end` (by value) and `f = function() f() end`
        # with `f` written elsewhere (by reference) both hold themselves.
        for kind in (self.VAL, self.REF):
            self.assertEqual(self.kinds(self.closure(0, 0, kind) + [ins("RETURN", 0, 2)]), ["self"])
        # By reference, a later write also replaces what the closure calls.
        code = self.closure(0, 0) + [ins("LOADN", 0, d=1), ins("RETURN", 0, 2)]
        self.assertEqual(self.kinds(code), ["self", "write"])
        # An upvalue of the enclosing function is no register, whatever its index.
        self.assertEqual(self.kinds(self.closure(0, 0, 2) + [ins("RETURN", 0, 2)]), [])

    def test_a_closure_that_now_captures_itself_rises(self):
        # Promise `_andThen`: the bytecode captured `reject` (R0) into the
        # closure written to R2; the output's closure captures its own variable.
        captured_other = chunk(self.closure(2, 0, self.VAL) + [ins("RETURN", 2, 2)], params=0)
        captures_itself = chunk(self.closure(2, 2) + [ins("RETURN", 2, 2)], params=0)
        self.assertEqual(capture_excess({"captures": compare_captures(captured_other, captures_itself)}), 1)
        # A recursive local function printed as `f = function() f() end`
        # (VAL in the bytecode, REF rebuilt) is still one self capture.
        recursive = chunk(self.closure(2, 2, self.VAL) + [ins("RETURN", 2, 2)], params=0)
        self.assertEqual(capture_excess({"captures": compare_captures(recursive, captures_itself)}), 0)

    def test_closeupvals_ends_what_the_closure_can_see(self):
        write = [ins("LOADN", 3, d=5), ins("RETURN", 1, 2)]
        for close, seen in ((3, []), (2, []), (4, ["write"])):
            with self.subTest(close=close):
                # CLOSEUPVALS A closes every register >= A.
                self.assertEqual(self.kinds(self.closure(1, 3) + [ins("CLOSEUPVALS", close)] + write), seen)

    def test_a_write_in_a_sibling_branch_is_not_reached(self):
        # if c then f = function() ... r0 ... end else r0 = 7 end: the write
        # comes later in pc order but no path from the closure reaches it.
        code = [ins("JUMPIFNOT", 2, d=3), *self.closure(1, 0), ins("JUMP", d=1), ins("LOADN", 0, d=7),
                ins("RETURN", 1, 2)]
        self.assertEqual(self.kinds(code), [])
        fall_through = [ins("JUMPIFNOT", 2, d=2), *self.closure(1, 0), ins("LOADN", 0, d=7), ins("RETURN", 1, 2)]
        self.assertEqual(self.kinds(fall_through), ["write"])

    def test_boolean_materialisation_jumps_over_its_other_arm(self):
        # `LOADB R3 true +1` always jumps: the instruction after it is not on this path.
        skipped = [*self.closure(2, 0), ins("LOADB", 3, 1, 1), ins("LOADN", 0, d=5), ins("RETURN", 2, 2)]
        self.assertEqual(self.kinds(skipped), [])
        skipped[2] = ins("LOADB", 3, 1, 0)
        self.assertEqual(self.kinds(skipped), ["write"])

    def test_loop_back_edge_reaches_writes_above_the_closure(self):
        # while true do r0 = 1; fs[#fs + 1] = function() ... r0 ... end end
        loop = [ins("LOADN", 0, d=0), ins("LOADN", 0, d=1), *self.closure(2, 0), ins("JUMPBACK", d=-4),
                ins("RETURN", 0, 1)]
        self.assertEqual(self.kinds(loop), ["write"])
        # A local of the body is closed before the next iteration writes it again.
        closed = loop[:4] + [ins("CLOSEUPVALS", 0), ins("JUMPBACK", d=-5), ins("RETURN", 0, 1)]
        self.assertEqual(self.kinds(closed), [])

    def test_written_register_ranges(self):
        from bytecode_roundtrip import _written_registers
        op = OP_INDEX.get
        self.assertEqual(_written_registers(op("CALL"), 2, 1, 0, 0), (2, 256))  # multret: up to the top
        self.assertEqual(_written_registers(op("CALL"), 2, 1, 3, 0), (2, 4))
        self.assertEqual(_written_registers(op("CALL"), 2, 1, 1, 0), (2, 2))
        self.assertEqual(_written_registers(op("GETVARARGS"), 4, 0, 0, 0), (4, 256))
        self.assertEqual(_written_registers(op("NAMECALL"), 4, 1, 0, 0), (4, 6))
        self.assertEqual(_written_registers(op("FORGLOOP"), 4, 0, 0, 2), (6, 9))
        self.assertEqual(_written_registers(op("FORNLOOP"), 4, 0, 0, 0), (6, 7))
        for name in ("SETTABLEKS", "SETUPVAL", "FASTCALL1", "FASTPCALL", "CLOSEUPVALS", "CAPTURE"):
            self.assertEqual(_written_registers(op(name), 4, 0, 0, 0), (0, 0), name)
        # A multret call overwrites a captured register above its base.
        code = self.closure(1, 5) + [ins("CALL", 2, 1, 0), ins("RETURN", 1, 2)]
        self.assertEqual(self.kinds(code), ["write"])
        code[2] = ins("CALL", 2, 1, 2)
        self.assertEqual(self.kinds(code), [])

    def test_hits_name_the_child_prototype_and_its_line(self):
        main = chunk([ins("NEWCLOSURE", 0, d=0), ins("RETURN", 0, 2)], params=0)
        child = chunk(self.closure(1, 0) + [ins("LOADN", 0, d=1), ins("RETURN", 1, 2)], params=0).protos[0]
        child.id, child.line_defined = 1, 12
        main.protos[0].children = [1]
        main.protos.append(child)
        self.assertEqual(capture_observations(main),
                         [{"proto": "0", "line": 12, "pc": 0, "register": 0, "kind": "write"}])

    def test_a_file_is_flagged_only_when_the_rebuilt_chunk_sees_more(self):
        write = [ins("LOADN", 0, d=5), ins("RETURN", 1, 2)]
        by_value = chunk(self.closure(1, 0, self.VAL) + write, params=0)
        by_reference = chunk(self.closure(1, 0) + write, params=0)
        flagged = compare_captures(by_value, by_reference)
        self.assertEqual((flagged["original"], flagged["rebuilt"], len(flagged["rebuilt_hits"])), (0, 1, 1))
        self.assertEqual(capture_excess({"captures": flagged}), 1)
        for orig, new in ((by_reference, by_reference), (by_reference, by_value)):
            result = compare_captures(orig, new, rebuild_at_o1=lambda: self.fail("no rise, no -O1 rebuild"))
            self.assertNotIn("rebuilt_hits", result)
            self.assertEqual(capture_excess({"captures": result}), 0)
        self.assertEqual(capture_excess({"status": "recompile-fail"}), 0)

    def test_a_rise_must_hold_without_the_pinned_inliner(self):
        write = [ins("LOADN", 0, d=5), ins("RETURN", 1, 2)]
        by_value = chunk(self.closure(1, 0, self.VAL) + write, params=0)
        by_reference = chunk(self.closure(1, 0) + write, params=0)
        confirmed = compare_captures(by_value, by_reference, rebuild_at_o1=lambda: by_reference)
        self.assertEqual((confirmed["rebuilt_O1"], capture_excess({"captures": confirmed})), (1, 1))
        self.assertEqual(len(confirmed["rebuilt_hits"]), 1)
        # At -O1 the write stayed in the function that makes it: an -O2 inlining artifact.
        inlined = compare_captures(by_value, by_reference, rebuild_at_o1=lambda: by_value)
        self.assertEqual((inlined["rebuilt"], inlined["rebuilt_O1"]), (1, 0))
        self.assertNotIn("rebuilt_hits", inlined)
        self.assertEqual(capture_excess({"captures": inlined}), 0)
        # No -O1 chunk (it did not compile): the -O2 rise stands.
        unchecked = compare_captures(by_value, by_reference, rebuild_at_o1=lambda: None)
        self.assertNotIn("rebuilt_O1", unchecked)
        self.assertEqual(capture_excess({"captures": unchecked}), 1)


class GateTests(unittest.TestCase):
    """The gate must fail when an input disappears or does not decode, or
    the decompiler fails, however the remaining inputs compare."""

    RETURN = bytes([6, 1, 0, 1, 1, 0, 0, 0, 0, 0, 1]) + struct.pack('<I', 22 | (1 << 16)) + bytes([0, 0, 0, 0, 0, 0, 0])

    def run_gate(self, files, baseline, decompiler_exit=0, extra=(), fails_on=None):
        import base64
        import json
        import pathlib
        import subprocess
        import sys
        import tempfile
        from unittest import mock
        import bytecode_roundtrip
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            corpus = root / 'corpus'
            corpus.mkdir()
            for name, text in files.items():
                (corpus / name).write_text(text)
            (root / 'base.json').write_text(json.dumps(baseline))

            def fake_run(command, **_):
                if command[1] == 'decompile-folder':
                    out = pathlib.Path(command[3])
                    out.mkdir(parents=True, exist_ok=True)
                    (out / 'good.luau').write_text('return')
                    # `fails_on`: an input the decompiler fails whenever it is given.
                    given = fails_on is not None and (pathlib.Path(command[2]) / fails_on).exists()
                    return subprocess.CompletedProcess(command, 1 if given else decompiler_exit, b'', b'')
                return subprocess.CompletedProcess(command, 0, self.RETURN, b'')

            argv = ['bytecode_roundtrip.py', '--lifter', 'lifter', '--compiler', 'compiler', '--corpus', str(corpus),
                    '--key', '1', '--threads', '1', '--work', str(root / 'work'), '--baseline', str(root / 'base.json'),
                    *extra]
            with mock.patch.object(bytecode_roundtrip, 'run', fake_run), mock.patch.object(sys, 'argv', argv), \
                    mock.patch('builtins.print'):
                return bytecode_roundtrip.main()

    def test_undecodable_missing_and_failed_inputs_fail_the_gate(self):
        import base64
        good = base64.b64encode(self.RETURN).decode()
        ok = lambda name: {'file': name, 'status': 'ok', 'nonequiv': 0, 'protos': 1}
        self.assertEqual(self.run_gate({'good.lua': good}, {'files': [ok('good')]}), 0)
        # A body that is not base64 fails instead of vanishing.
        self.assertEqual(self.run_gate({'good.lua': good, 'broken.lua': 'a'}, {'files': [ok('good'), ok('broken')]}), 1)
        # A baselined input that is gone fails; a header-only script is skipped.
        self.assertEqual(self.run_gate({'good.lua': good, 'empty.lua': '-- no bytecode'},
                                       {'files': [ok('good'), ok('gone')]}), 1)
        self.assertEqual(self.run_gate({'good.lua': good, 'empty.lua': '-- no bytecode'}, {'files': [ok('good')]}), 0)
        # So does a decompiler that exits with an error.
        self.assertEqual(self.run_gate({'good.lua': good}, {'files': [ok('good')]}, decompiler_exit=1), 1)

    def test_a_selection_answers_only_for_the_selected_inputs(self):
        import base64
        good = base64.b64encode(self.RETURN).decode()
        ok = lambda name: {'file': name, 'status': 'ok', 'nonequiv': 0, 'protos': 1}
        files = {'good.lua': good, 'zbad.lua': good}
        self.assertEqual(self.run_gate(files, {'files': [ok('good'), ok('zbad')]}, fails_on='zbad.lua'), 1)
        for selection in (['--filter', 'good'], ['--limit', '1']):
            self.assertEqual(self.run_gate(files, {'files': [ok('good'), ok('zbad')]}, extra=selection,
                                           fails_on='zbad.lua'), 0, selection)

    def test_source_likeness_skips_inputs_too_long_to_match(self):
        import time
        import bytecode_roundtrip
        long_source = 'local sum = 0\n' + 'sum += n\n' * 3000
        start = time.time()
        self.assertIsNone(bytecode_roundtrip.source_likeness(long_source, long_source))
        self.assertLess(time.time() - start, 5)
        self.assertEqual(bytecode_roundtrip.source_likeness('return 1', 'return 1'), 1.0)

    def test_capture_tier_reports_but_never_fails_the_gate(self):
        import base64
        import json
        import pathlib
        import tempfile
        from unittest import mock
        import bytecode_roundtrip
        good = base64.b64encode(self.RETURN).decode()
        baseline = {'files': [{'file': 'good', 'status': 'ok', 'nonequiv': 0, 'protos': 1}]}
        hit = {"proto": "main", "line": 1, "pc": 0, "register": 0, "kind": "write"}
        # Observed in order: the original, the -O2 rebuild, the -O1 rebuild
        # (only after a rise). A rise the -O1 rebuild does not repeat is no flag.
        for seen, excess in (([0, 1, 1], 1), ([0, 1, 0], None), ([0, 0], None)):
            calls = []

            def observe(chunk):
                calls.append(chunk)
                return [hit] * seen[len(calls) - 1]

            with self.subTest(seen=seen), tempfile.TemporaryDirectory() as out:
                written = pathlib.Path(out) / 'baseline.json'
                with mock.patch.object(bytecode_roundtrip, 'capture_observations', observe):
                    code = self.run_gate({'good.lua': good}, baseline, extra=['--write-baseline', str(written)])
                self.assertEqual(code, 0)
                self.assertEqual(len(calls), len(seen))
                self.assertEqual(json.loads(written.read_text())['files'][0].get('capture_excess'), excess)


if __name__ == "__main__":
    unittest.main()
