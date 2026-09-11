import copy
import unittest

from emission_map_audit import validate_emission_map, validate_parser_identity
from provenance_lookup import lookup


class EmissionMapTests(unittest.TestCase):
    def fixture(self):
        source = b'local x = 1\nlocal function f(x) return x end\nreturn x'
        offsets = [source.index(b'x'), source.index(b'f('), source.index(b'(x') + 1,
                   source.index(b'return x') + 7, source.rindex(b'x')]
        def position(offset):
            prefix = source[:offset]
            return dict(byte_offset=offset, line_one_based=prefix.count(b'\n') + 1,
                        column_one_based=len(prefix.rsplit(b'\n', 1)[-1]) + 1)
        def span(offset):
            return dict(start=position(offset), end=position(offset + 1))
        def location(offset):
            p = position(offset)
            return f"{p['line_one_based']-1},{p['column_one_based']-1} - {p['line_one_based']-1},{p['column_one_based']}"
        declarations = [dict(type='AstLocal', name=name, location=location(offset))
                        for name, offset in zip(('x', 'f', 'x'), offsets)]
        root = declarations + [dict(type='AstExprLocal', location=location(offsets[3]), local=declarations[2]),
                               dict(type='AstExprLocal', location=location(offsets[4]), local=declarations[0])]
        ids = ['b1', 'b2', 'b3', 'b3', 'b1']
        trace = dict(final_bindings=[dict(binding_id=bid, name=name) for bid, name in zip(ids[:3], ('x', 'f', 'x'))],
                     output_map=dict(schema_version=1, limits=dict(occurrences=100000, annotation_text_bytes=4096),
                                     bindings=[dict(binding_id=bid, role='read', span=span(offset)) for bid, offset in zip(ids, offsets)],
                                     annotations=[], opaque_regions=[], omitted_occurrences=0),
                     summary=dict(identifier_spans=5, annotation_spans=0, opaque_output_regions=0,
                                  omitted_output_occurrences=0, bindings_without_identifier_tokens=0))
        return trace, source, root

    def test_shadow_binding_swap_is_rejected_even_when_spelling_matches(self):
        trace, source, root = self.fixture()
        self.assertEqual(validate_emission_map(trace, source), [])
        self.assertEqual(validate_parser_identity(trace, source, root)[0], [])
        trace['output_map']['bindings'][-1]['binding_id'] = 'b3'
        self.assertEqual(validate_emission_map(trace, source), [])
        self.assertIn('one parser binding maps to conflicting final IDs', validate_parser_identity(trace, source, root)[0])

    def test_wrong_span_and_unknown_id_are_rejected(self):
        trace, source, _ = self.fixture()
        original = copy.deepcopy(trace)
        trace['output_map']['bindings'][0]['span']['start']['byte_offset'] += 1
        self.assertTrue(validate_emission_map(trace, source))
        original['output_map']['bindings'][0]['binding_id'] = 'b9'
        self.assertIn('identifier references missing final binding', validate_emission_map(original, source))

    def test_missing_token_requires_explicit_opaque_region(self):
        trace, source, root = self.fixture()
        occurrence = trace['output_map']['bindings'].pop()
        self.assertTrue(validate_parser_identity(trace, source, root)[0])
        trace['output_map']['opaque_regions'].append(dict(reason='interpolated_string_argument_rendering', span=occurrence['span']))
        errors, summary = validate_parser_identity(trace, source, root)
        self.assertEqual(errors, [])
        self.assertEqual(summary['opaque_local_tokens'], 1)

    def test_unicode_columns_count_characters_not_bytes(self):
        trace, source, _ = self.fixture()
        trace['output_map']['bindings'] = [trace['output_map']['bindings'][0]]
        trace['summary'].update(identifier_spans=1, bindings_without_identifier_tokens=2)
        source = '"界"; x'.encode('utf-8')
        span = trace['output_map']['bindings'][0]['span']
        span['start'].update(byte_offset=7, line_one_based=1, column_one_based=6)
        span['end'].update(byte_offset=8, line_one_based=1, column_one_based=7)
        self.assertEqual(validate_emission_map(trace, source), [])
        span['start']['byte_offset'] = 2
        self.assertIn('output offset splits UTF-8 character', validate_emission_map(trace, source))

    def test_lookup_retains_multiple_storage_origins_and_unknowns(self):
        trace, _, _ = self.fixture()
        trace['final_bindings'][0]['lineage'] = ['b10', 'b11', 'b12']
        trace['functions'] = [dict(function_id='root', prototype=0,
            registers=[dict(id='b10', kind='parameter', slot=0)],
            lifted_statements=[dict(block=0, statement_index=1, instruction_pcs=[3, 5], source_lines=[7])],
            definitions=[dict(id='b11', kind='ssa_definition', original_register='b10', block=0,
                              statement_index=1, write_index=0)])]
        result = lookup(trace, trace['output_map']['bindings'][0]['span']['start']['byte_offset'])
        origins = result['identifiers'][0]['origins']
        self.assertEqual([r['instruction_pcs'] for r in origins], [[], [3, 5], []])
        self.assertEqual(origins[0]['status'], 'input_storage_without_definition_site')
        self.assertEqual(origins[-1]['status'], 'unknown_origin')
        self.assertEqual(lookup(trace, 0)['identifiers'], [])


if __name__ == '__main__':
    unittest.main()
