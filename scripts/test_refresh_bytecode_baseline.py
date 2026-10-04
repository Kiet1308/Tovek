import json
import unittest

from refresh_bytecode_baseline import refresh_bytes, summarize, value_span


class BaselineSummaryTests(unittest.TestCase):
    def test_refresh_preserves_all_bytes_outside_summary_and_is_idempotent(self):
        data = b'{\r\n "provenance": {"summary": "not the top level", "note": "\\u00e9"},\n "summary": {"inputs": 15},\r\n "files": [\r\n  {"file":"loop","status":"ok","nonequiv":2,"protos":7},\n  {"file":"return","status":"ok","nonequiv":0,"protos":1}\r\n ]\r\n}\n'
        updated, changed = refresh_bytes(data)
        self.assertTrue(changed)
        a, b = value_span(data.decode(), 'summary')
        c, d = value_span(updated.decode(), 'summary')
        self.assertEqual(data[:a], updated[:c])
        self.assertEqual(data[b:], updated[d:])
        summary = json.loads(updated)['summary']
        self.assertEqual((summary['inputs'], summary['original_protos'], summary['nonequiv']), (2, 8, 2))
        self.assertNotIn('equiv_ratio', summary)
        self.assertNotIn('exact', summary)
        self.assertEqual(refresh_bytes(updated), (updated, False))

    def test_duplicate_rows_and_invalid_thresholds_refuse_to_rewrite(self):
        row = dict(file='x', status='ok', protos=2, nonequiv=1)
        with self.assertRaises(ValueError):
            summarize({'files': [row, row]})
        for bad in (-1, True, '2'):
            with self.assertRaises(ValueError):
                summarize({'files': [dict(row, nonequiv=bad)]})

    def test_summary_key_in_a_string_is_not_mistaken_for_the_summary_object(self):
        text = '{"note":"summary and } , \\\"quoted\\\"", "summary":{"inputs":0}, "files":[]}'
        start, end = value_span(text, 'summary')
        self.assertEqual(text[start:end], '{"inputs":0}')


if __name__ == '__main__':
    unittest.main()
