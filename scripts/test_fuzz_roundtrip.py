import random
import struct
import unittest

from fuzz_roundtrip import FAMILIES, generate, mutate, source_of


class FuzzRoundtripTests(unittest.TestCase):
    def test_seeds_replay_and_explore(self):
        self.assertEqual(generate(7), generate(7))
        programs = {tuple(generate(seed)[0]) for seed in range(20)}
        self.assertEqual(len(programs), 20)

    def test_families_come_from_the_known_set(self):
        for seed in range(50):
            _, families = generate(seed)
            self.assertTrue(set(families) <= set(FAMILIES))
            self.assertTrue(1 <= len(families) <= 4)

    def test_source_wraps_units_in_a_returned_function(self):
        units, _ = generate(3)
        source = source_of(units)
        self.assertTrue(source.rstrip().endswith("end"))
        self.assertIn("return function(input, flip)", source)
        for unit in units:
            self.assertIn(unit, source)

    def test_mutations_keep_the_length_and_spell_no_identifier(self):
        data = b"\x06\x03\x02\x06record\x04next" + struct.pack("<d", 0.5)
        for seed in range(20):
            patched, note = mutate(random.Random(seed), data)
            self.assertEqual(len(patched), len(data))
            if note.startswith("rename"):
                self.assertIn(b" ", patched)
            elif note == "nan payload":
                self.assertNotIn(struct.pack("<d", 0.5), patched)


if __name__ == "__main__":
    unittest.main()
