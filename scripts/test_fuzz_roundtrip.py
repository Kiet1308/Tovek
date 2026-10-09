import random
import re
import struct
import unittest
from unittest import mock

from fuzz_roundtrip import (CAPTURE_FAMILIES, CAPTURE_SHAPES, CLOSURE_BINDERS, FAMILIES, RECURSIVE_ARMS,
                            Generator, comparable, generate, mutate, source_of)


class FuzzRoundtripTests(unittest.TestCase):
    def test_seeds_replay_and_explore(self):
        self.assertEqual(generate(7), generate(7))
        programs = {tuple(generate(seed)[0]) for seed in range(20)}
        self.assertEqual(len(programs), 20)

    def test_families_come_from_the_known_set(self):
        for seed in range(50):
            _, families = generate(seed)
            self.assertTrue(set(families) <= set(FAMILIES) | set(CAPTURE_FAMILIES))
            self.assertTrue(1 <= len(set(families) & set(FAMILIES)) <= 4)

    def test_source_wraps_units_in_a_returned_function(self):
        units, _ = generate(3)
        source = source_of(units)
        self.assertTrue(source.rstrip().endswith("end"))
        self.assertIn("return function(input, flip)", source)
        for unit in units:
            self.assertIn(unit, source)

    def test_capture_families_leave_every_other_unit_as_it_was(self):
        # The capture families draw from their own stream: without them a
        # seed builds the units it built before they existed, in order.
        drawn = set()
        for seed in range(60):
            units, families = generate(seed)
            with mock.patch.object(Generator, "capture_units", lambda self: []):
                plain, plain_families = generate(seed)
            captures = set(families) & set(CAPTURE_FAMILIES)
            drawn |= captures
            self.assertEqual(set(families) - captures, set(plain_families))
            self.assertEqual([unit for unit in units if unit in plain], plain)
            self.assertEqual(len(units) - len(plain), len(captures))
        self.assertEqual(drawn, set(CAPTURE_FAMILIES))

    def test_capture_units_come_before_the_pressure_unit(self):
        # Their locals must not stack on top of the pressure unit's ~185.
        for seed in range(400):
            units, families = generate(seed)
            if "pressure" in families and set(families) & set(CAPTURE_FAMILIES):
                self.assertTrue(units[-1].lstrip().startswith("local r"), seed)
                self.assertTrue(all(unit.startswith("    do\n") for unit in units[-1 - len(set(families) & set(CAPTURE_FAMILIES)):-1]))
                return
        self.fail("no seed draws both")

    def test_closure_binders_reach_the_assignment_three_ways(self):
        for shape in CAPTURE_SHAPES:
            for binder in CLOSURE_BINDERS:
                with self.subTest(shape=shape, binder=binder):
                    source = "\n".join(Generator(1).capture_factory(shape, binder))
                    self.assertTrue(source.startswith("do\n") and source.endswith("\nend"))
                    if binder == "factory":
                        # The closure is made inside the factory, never as a literal at the site.
                        self.assertRegex(source, r"local function make\d+\(")
                        self.assertNotRegex(source, r"\((?:keep\d+|record)\(function")
                    elif binder == "keep":
                        self.assertRegex(source, r"keep\d+\(function\(")
                    else:
                        self.assertRegex(source, r"record\(function\(")

    def test_shapes_keep_their_variables_out_of_reach_of_inlining(self):
        # The vulnerable code must run as its own prototype: the branch
        # shapes return the function that holds it, the others are reached
        # through `record`, so no inlined copy folds a constant argument.
        generator = Generator(2)
        self.assertRegex("\n".join(generator.capture_factory("loop", "record")),
                         r"local previous = acc\n\s+acc = record\(function\(x\) return previous\(x\) \+ i end\)")
        self.assertRegex("\n".join(generator.capture_factory("loop", "factory")), r"acc = make\d+\(acc, i\)")
        for shape in ("loop", "parallel"):
            self.assertRegex("\n".join(generator.capture_factory(shape, "keep")), r"= record\((chain|pair)\d+\)\(")
        self.assertRegex("\n".join(generator.capture_factory("branch", "keep")), r"record\(pick\d+\(if flip then")

    def test_recursive_arms(self):
        for arm in RECURSIVE_ARMS:
            for indirect in (False, True):
                with self.subTest(arm=arm, indirect=indirect):
                    source = "\n".join(Generator(3).recursive_arm(arm, indirect))
                    rec = re.search(r"local function (rec\d+)\(n\)", source).group(1)
                    self.assertIn(f"return {rec}(n - 1)", source)
                    self.assertIn(f"chosen = {rec}", source)
                    self.assertIn("if not flag then" if arm == "else" else "if flag then", source)
                    self.assertEqual("else\n" in source, arm.startswith("diamond"))
                    self.assertEqual(bool(re.search(r"= record\(choose\d+\)\(flip", source)), indirect)

    def test_mutations_keep_the_length_and_spell_no_identifier(self):
        data = b"\x06\x03\x02\x06record\x04next" + struct.pack("<d", 0.5)
        for seed in range(20):
            patched, note = mutate(random.Random(seed), data)
            self.assertEqual(len(patched), len(data))
            if note.startswith("rename"):
                self.assertIn(b" ", patched)
            elif note == "nan payload":
                self.assertNotIn(struct.pack("<d", 0.5), patched)

    def test_heap_addresses_compare_as_one_token(self):
        # Seed 142948 with "function" renamed: `describe` printed each function
        # by address, which differs between runs and across DUPCLOSURE levels.
        reference = '1:function: 0x0000023b3b4aaef0;1:"fallback" => true,-3'
        rebuilt = '1:function: 0x00000194410ead10;1:"fallback" => true,-3'
        self.assertEqual(comparable(reference), comparable(rebuilt))
        self.assertEqual(comparable("t: table: 0xAB12"), "t: table: 0x")
        # Everything else still differs.
        self.assertNotEqual(comparable("1:function: 0x1;1:2"), comparable("1:function: 0x1;1:3"))
        self.assertEqual(comparable('1:"0x10",16'), '1:"0x10",16')


if __name__ == "__main__":
    unittest.main()
