import unittest

from generated_roundtrip import generate, reduce_units, same_observation


class GeneratedRoundtripTests(unittest.TestCase):
    def test_seed_replays_and_distinct_seeds_explore_different_units(self):
        self.assertEqual(generate(1024), generate(1024))
        self.assertNotEqual(generate(1024), generate(1025))

    def test_reducer_preserves_failure_and_honors_budget(self):
        units, attempts = reduce_units(["a", "bad", "b"], lambda seq: "bad" in seq)
        self.assertEqual(units, ["bad"])
        self.assertGreater(attempts, 0)
        _, attempts = reduce_units(list(range(20)), lambda seq: False, budget=3)
        self.assertEqual(attempts, 3)

    def test_observations_ignore_latency_but_keep_arity_bytes_and_exit(self):
        left = dict(exit=0, stdout="2\tfalse|nil\n", stderr="", seconds=1)
        right = dict(left, seconds=2)
        self.assertTrue(same_observation(left, right))
        self.assertFalse(same_observation(left, dict(right, stdout="1\tfalse\n")))
        self.assertFalse(same_observation(left, dict(right, exit=1)))
