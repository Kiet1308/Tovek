import collections
import json
import pathlib
import struct
import tempfile
import unittest

from bytecode_roundtrip import OP_INDEX, parse_chunk
from scorecard import (census, census_hits, count_lines, declarations, GENERATED, marker_calls, named_copies,
                       parse_any_key, score, tokens)


def varint(n):
    result = bytearray()
    while n >= 128:
        result.append((n & 127) | 128)
        n >>= 7
    result.append(n)
    return bytes(result)


def word(name, a=0, d=0):
    return OP_INDEX[name] | (a << 8) | ((d & 0xFFFF) << 16)


def proto(code, lines, line_defined, name=0, children=(), key=1):
    """A version-6 prototype with line info: `code` is a list of opcode names."""
    encoded = [(word(op) & ~255) | (OP_INDEX[op] * pow(key, -1, 256) & 255) for op in code]
    body = bytes([1, 0, 0, 0, 0]) + varint(0)  # stack, params, upvalues, vararg, flags, type info
    body += varint(len(encoded)) + b"".join(struct.pack("<I", w) for w in encoded)
    body += varint(0) + varint(len(children)) + b"".join(varint(c) for c in children)
    body += varint(line_defined) + varint(name)
    base = min(lines)
    offsets = [line - base for line in lines]
    deltas = bytes((offset - previous) & 255 for previous, offset in zip([0] + offsets, offsets))
    body += b"\1" + bytes([24]) + deltas + struct.pack("<i", base)  # one line interval
    return body + b"\0"  # no debug info


def chunk(protos, strings=(b"helper",), main=None):
    data = bytes([6, 0]) + varint(len(strings)) + b"".join(varint(len(s)) + s for s in strings)
    data += varint(len(protos)) + b"".join(protos)
    return data + varint(len(protos) - 1 if main is None else main)


def helper_and_caller(key=1):
    """`local function helper() ... end` on lines 1-3, inlined twice back to
    back into the main chunk after its definition on line 1."""
    helper = proto(["LOADN", "ADD", "RETURN"], [2, 3, 3], 1, name=1, key=key)
    caller = proto(["DUPCLOSURE", "LOADN", "ADD", "LOADN", "ADD", "LOADN", "RETURN"], [1, 2, 3, 2, 3, 10, 10], 0,
                   children=[0], key=key)
    return chunk([helper, caller])


class Tokens(unittest.TestCase):
    def test_comments_are_dropped_and_strings_kept_whole(self):
        self.assertEqual(tokens('local s = "-- not a comment" -- comment\n--[[ long ]] f(s)'),
                         ["local", "s", "=", '"-- not a comment"', "f", "(", "s", ")"])

    def test_declarations_cover_locals_functions_parameters_and_loops(self):
        # As in the research scorecard, a `local function`'s parameters are
        # not read (its numbers stay comparable).
        text = """
            local a, b: Map<string, number> = 1, {}
            local function f(p: { x: number }, q, ...) end
            for i, v: number in pairs(t) do end
            local g = function(r) end
        """
        self.assertEqual(declarations(text), [("local", "a"), ("local", "b"), ("fn", "f"), ("for", "i"), ("for", "v"),
                                              ("local", "g"), ("param", "r")])

    def test_generated_names(self):
        self.assertTrue(all(GENERATED["tovek"].match(n) for n in ("v", "v12", "p", "p3")))
        self.assertFalse(any(GENERATED["tovek"].match(n) for n in ("value", "pv", "v1x")))
        self.assertTrue(GENERATED["luacid"].match("tbl3"))
        self.assertFalse(GENERATED["luacid"].match("tbl"))

    def test_lines_skip_blanks_and_luacid_banners(self):
        self.assertEqual(count_lines("-- [[ luacid.dev ]]\n\nlocal x = 1\n  \n-- Luau bytecode version 6\nreturn x\n"), 2)

    def test_marker_calls_count_sites_not_definitions(self):
        text = """-- equivalent calls inferred from this helper; original call sites unknown
            local function f() end
            f() -- equivalent call inferred; original call site unknown
            -- inferred call
            if f() then end"""
        self.assertEqual(marker_calls(text, "tovek"), 2)
        current = """local function f() -- 2 equivalent calls inferred from this helper
            local function g() -- 1 equivalent call inferred from this helper
            local function h() -- 4 equivalent arithmetic calls inferred from this helper
            f(g()) -- inferred equivalent call
            if f() then -- inferred equivalent call"""
        self.assertEqual(marker_calls(current, "tovek"), 2)
        self.assertEqual(marker_calls("-- 3 call sites recovered\n-- 1 call site recovered", "luacid"), 4)


class Census(unittest.TestCase):
    def test_line_info_is_decoded_per_word(self):
        main = parse_chunk(helper_and_caller(), 1).protos[1]
        self.assertEqual(main.lines, [1, 2, 3, 2, 3, 10, 10])
        self.assertEqual(main.line_defined, 0)

    def test_copies_back_to_back_count_twice_and_the_definition_not_at_all(self):
        self.assertEqual(census(parse_any_key(helper_and_caller())), collections.Counter({"helper#0": 2}))

    def test_roblox_encoded_opcodes_are_read_with_key_203(self):
        self.assertEqual(census(parse_any_key(helper_and_caller(key=203))), collections.Counter({"helper#0": 2}))

    def test_recall_credits_each_helper_with_at_most_its_copies(self):
        copies = named_copies(collections.Counter({"expand#3": 5, "sign#4": 2, "?#7": 9, "expand#9": 1}))
        self.assertEqual(copies, collections.Counter({"expand": 6, "sign": 2}))
        self.assertEqual(census_hits(copies, {"expand": 4, "sign": 3, "other": 8}), 4 + 2)


class Score(unittest.TestCase):
    def test_score_reads_calls_from_stats_and_names_from_public_sources(self):
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            (root / "public" / "sample").mkdir(parents=True)
            (root / "public" / "sample" / "source.luau").write_text("local count = 1\nlocal function step(amount) end\n",
                                                                   encoding="utf-8")
            index = [dict(kind="public", name="sample")]
            output = "local count = 1\nlocal function v1(p1) end\nv1() -- inferred call\n"
            stats = {"sample": {"reconstructed_calls": {"total": 3}}}
            result = score(root, index, {"sample": output}, stats, "tovek")
            self.assertEqual((result["calls"], result["markers"]), (3, 1))
            self.assertEqual((result["generated"], result["decls"]), (1, 2))
            self.assertEqual((result["name_hit"], result["name_total"]), (1, 2))
            self.assertEqual(result["lines"], 3)


if __name__ == "__main__":
    unittest.main()
