import unittest

from reconstruction_study_review import argument_signature, byte_span, result_roles


class ReviewControls(unittest.TestCase):
    def test_parameter_identity_survives_rename_but_not_slot_swap(self):
        function = {'args': [{'location': 'a'}, {'location': 'b'}]}
        def call(places):
            return {'args': [{'type': 'AstExprLocal', 'local': {'name': 'same', 'location': p}} for p in places]}
        self.assertEqual(argument_signature(call(['a', 'b']), function), [['parameter', 0], ['parameter', 1]])
        self.assertNotEqual(argument_signature(call(['b', 'a']), function), [['parameter', 0], ['parameter', 1]])

    def test_result_roles_follow_bindings_in_return_order(self):
        first = {'type': 'AstExprConstantNumber', 'value': 7}
        second = {'type': 'AstExprConstantNumber', 'value': 11}
        tree = {'type': 'AstExprFunction', 'debugname': 'run', 'body': {'body': [
            {'type': 'AstStatLocal', 'vars': [{'location': 'a'}, {'location': 'b'}], 'values': [first, second]},
            {'type': 'AstStatReturn', 'list': [
                {'type': 'AstExprLocal', 'local': {'location': 'b'}},
                {'type': 'AstExprLocal', 'local': {'location': 'a'}},
            ]},
        ]}}
        self.assertEqual(result_roles(tree)[1], [second, first])
        tree['body']['body'][-1]['list'].pop()
        with self.assertRaises(ValueError): result_roles(tree)

    def test_emitted_locations_use_bytes_with_crlf_and_utf8(self):
        subject = 'local x = "é"\r\nreturn helper(x)\r\n'.encode('utf-8')
        start, end = byte_span('1,7 - 1,16', subject)
        self.assertEqual(subject[start:end], b'helper(x)')


if __name__ == '__main__':
    unittest.main()
