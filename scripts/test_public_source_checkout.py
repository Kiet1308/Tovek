import hashlib
import pathlib
import tempfile
import unittest

from public_source_roundtrip import restore_pinned_line_endings


class PinnedCheckoutTests(unittest.TestCase):
    def test_lf_and_crlf_checkouts_restore_exact_pinned_bytes(self):
        lf = b'-- comment\nreturn "source"\n'
        crlf = lf.replace(b'\n', b'\r\n')
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'source.luau'
            for original, expected in ((lf, crlf), (crlf, lf), (lf, lf), (crlf, crlf)):
                with self.subTest(original=original, expected=expected):
                    path.write_bytes(original)
                    restore_pinned_line_endings(path, hashlib.sha256(expected).hexdigest())
                    self.assertEqual(path.read_bytes(), expected)

    def test_changed_content_is_rejected_without_modifying_checkout(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'LICENSE'
            original = b'Modified terms\n'
            path.write_bytes(original)
            with self.assertRaisesRegex(ValueError, 'differs from pinned content'):
                restore_pinned_line_endings(path, hashlib.sha256(b'Original terms\r\n').hexdigest())
            self.assertEqual(path.read_bytes(), original)


if __name__ == '__main__':
    unittest.main()
