import hashlib
import unittest

from compiler_witnesses import observation_sha256


class ObservationHashTests(unittest.TestCase):
    def test_linux_and_windows_print_records_match_locked_hash(self):
        windows = b'negative\tfalse\t0\ttrue\t2\t-6|+zero\t\r\nnan\ttrue\t1\r\n'
        expected = hashlib.sha256(windows).hexdigest()
        self.assertEqual(observation_sha256(windows), expected)
        self.assertEqual(observation_sha256(windows.replace(b'\r\n', b'\n')), expected)

    def test_record_contents_order_and_termination_remain_observable(self):
        original = b'value\t-0\nerror\tstop\n'
        variants = (b'value\t0\nerror\tstop\n', b'value -0\nerror\tstop\n',
                    b'error\tstop\nvalue\t-0\n', original.rstrip(b'\n'),
                    original.replace(b'stop', b'stop '), original.replace(b'-0', b'\r-0'))
        for variant in variants:
            with self.subTest(variant=variant):
                self.assertNotEqual(observation_sha256(original), observation_sha256(variant))


if __name__ == '__main__':
    unittest.main()
