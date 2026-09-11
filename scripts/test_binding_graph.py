import copy
import unittest

from binding_graph import LIMITS, Refused, attach_storage, digest, lexical_graph


class BindingGraphTests(unittest.TestCase):
    def fixture(self):
        source = b'local x = 1; do local x = 2; print(x) end; return x'
        offsets = [i for i, byte in enumerate(source) if byte == ord('x')]
        def location(i):
            return f'0,{i} - 0,{i + 1}'
        locals_ = [dict(type='AstLocal', name='x', location=location(i)) for i in offsets[:2]]
        def declaration(i):
            return dict(type='AstStatLocal', vars=[locals_[i]], values=[])
        def reference(i, binding):
            return dict(type='AstExprLocal', location=location(offsets[i]), local=locals_[binding])
        root = dict(type='AstStatBlock', body=[declaration(0),
            dict(type='AstStatBlock', body=[declaration(1), reference(2, 1)]), reference(3, 0)])
        bindings = ['b1', 'b2', 'b2', 'b1']
        def position(i):
            return dict(byte_offset=i, line_one_based=1, column_one_based=i + 1)
        trace = dict(schema_version=1, limits=dict(records_per_function=50000, ancestry_per_binding=64),
            functions=[], final_bindings=[dict(binding_id=bid, name='x', lineage=[], incomplete=True) for bid in ('b1', 'b2')],
            output_map=dict(schema_version=1, limits=dict(occurrences=100000, annotation_text_bytes=4096),
                bindings=[dict(binding_id=bid, role='declaration' if i < 2 else 'read',
                               span=dict(start=position(offset), end=position(offset + 1)))
                          for i, (bid, offset) in enumerate(zip(bindings, offsets))],
                annotations=[], opaque_regions=[], omitted_occurrences=0),
            summary=dict(identifier_spans=4, annotation_spans=0, opaque_output_regions=0,
                         omitted_output_occurrences=0, bindings_without_identifier_tokens=0))
        metadata = dict(source_sha256=digest(source), binding_provenance=trace,
            source_recovery=dict(bindings=[dict(binding_id='b1', name='x', origins=[dict(
                kind='debug_local', prototype=0, register=0, start_pc=0, end_pc=5)])]))
        return source, root, metadata

    def test_shared_storage_does_not_merge_lexical_declarations_or_copy_recorded_identity(self):
        source, root, metadata = self.fixture()
        trace = metadata['binding_provenance']
        for token in trace['output_map']['bindings']:
            token['binding_id'] = 'b1'
        trace['final_bindings'].pop()
        graph = attach_storage(lexical_graph(root, source), metadata, source)
        self.assertEqual([r['declaration_id'] for r in graph['declarations']], ['d0', 'd1'])
        self.assertEqual(graph['storage'][0]['declarations'], ['d0', 'd1'])
        for row in graph['declarations']:
            self.assertEqual(row['recorded_identity_status'], 'ambiguous_shared_storage')
            self.assertEqual(row['recorded_origins'], [])
            self.assertTrue(row['protect_recorded_name'])

    def test_same_spelling_wrong_binding_reference_is_refused(self):
        source, root, metadata = self.fixture()
        metadata['binding_provenance']['output_map']['bindings'][-1]['binding_id'] = 'b2'
        with self.assertRaisesRegex(Refused, 'conflicting_storage'):
            attach_storage(lexical_graph(root, source), metadata, source)

    def test_missing_tokens_need_opaque_region_or_budget(self):
        source, root, metadata = self.fixture()
        trace = metadata['binding_provenance']
        token = trace['output_map']['bindings'].pop()
        trace['summary']['identifier_spans'] -= 1
        with self.assertRaisesRegex(Refused, 'unexplained_unmapped'):
            attach_storage(lexical_graph(root, source), metadata, source)
        trace['output_map']['opaque_regions'].append(dict(reason='interpolated_string_argument_rendering', span=token['span']))
        trace['summary']['opaque_output_regions'] = 1
        graph = attach_storage(lexical_graph(root, source), metadata, source)
        self.assertEqual(graph['declarations'][0]['tokens'][-1]['storage_mapping'], 'opaque')

    def test_source_hash_and_trace_spans_are_checked(self):
        source, root, metadata = self.fixture()
        with self.assertRaisesRegex(Refused, 'source_hash'):
            attach_storage(lexical_graph(root, source), metadata, source + b' ')
        metadata['binding_provenance']['output_map']['bindings'][0]['span']['end']['byte_offset'] += 1
        with self.assertRaisesRegex(Refused, 'invalid_trace'):
            attach_storage(lexical_graph(root, source), metadata, source)

    def test_absent_recorded_name_is_unrecorded_not_a_compiler_temporary(self):
        source, root, metadata = self.fixture()
        graph = attach_storage(lexical_graph(root, source), metadata, source)
        self.assertEqual(graph['declarations'][1]['recorded_identity_status'], 'unrecorded')
        self.assertEqual(graph['declarations'][1]['kind'], 'local')
        self.assertFalse(graph['declarations'][1]['protect_recorded_name'])

    def test_budget_exhaustion_does_not_return_partial_graph(self):
        source, root, _ = self.fixture()
        for field, value in dict(source_bytes=4, nodes=2, depth=1, declarations=1, tokens=1).items():
            with self.subTest(field=field), self.assertRaises(Refused):
                lexical_graph(root, source, LIMITS | {field: value})

    def test_reference_without_declaration_and_duplicate_declarations_are_refused(self):
        source, root, _ = self.fixture()
        missing = copy.deepcopy(root)
        missing['body'].pop(0)
        with self.assertRaisesRegex(Refused, 'reference_without_declaration'):
            lexical_graph(missing, source)
        root['body'].append(root['body'][0])
        with self.assertRaisesRegex(Refused, 'duplicate_parser_declaration'):
            lexical_graph(root, source)

    def test_parser_location_cannot_cross_line_by_excess_column(self):
        source = b'local x = 1\nreturn x'
        local = dict(type='AstLocal', name='x', location='0,6 - 0,7')
        root = dict(type='AstStatBlock', body=[dict(type='AstStatLocal', vars=[local]),
            dict(type='AstExprLocal', location='0,19 - 0,20', local=local)])
        with self.assertRaisesRegex(Refused, 'parser_location_outside_line'):
            lexical_graph(root, source)


if __name__ == '__main__':
    unittest.main()
