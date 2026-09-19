import collections
import struct
import unittest

from bytecode_roundtrip import BytecodeError, Reader, _cancel_counted_setlists, parse_chunk, OP_INDEX


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


if __name__ == "__main__":
    unittest.main()
