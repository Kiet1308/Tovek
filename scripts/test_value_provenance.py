import copy
import unittest

from value_provenance import validate


def sample():
    return {
        'final_bindings': [{'binding_id': 'b1'}],
        'functions': [{
            'function_id': 'root:p0',
            'dropped_records': 0,
            'registers': [{'id': 'b0'}], 'definitions': [{'id': 'b1'}],
            'lifted_statements': [{'block': 0, 'statement_index': 0,
                                   'instruction_pcs': [2], 'source_lines': [7]}],
            'value_origins': [
                {'id': 0, 'block': 0, 'statement_index': 0, 'path': [1, 0],
                 'binding_id': None, 'children': [1]},
                {'id': 1, 'block': 0, 'statement_index': 0, 'path': [1, 0, 0],
                 'binding_id': 'b0', 'children': []}],
            'inline_events': [{'exact_final_value_mapping': False}]}],
        'value_provenance': {
            'schema_version': 1,
            'source_sites': {'f0:b0:s0': {'function_id': 'root:p0', 'block': 0, 'statement_index': 0,
                                         'instruction_pcs': [2], 'source_lines': [7]}},
            'limits': {'output_regions': 100, 'work': 100, 'sites_per_region': 64},
            'visited_dependencies': 2,
            'bindings': [{'binding_id': 'b1', 'source_sites': ['f0:b0:s0']}],
            'output_regions': [{'bindings': ['b1'], 'source_sites': ['f0:b0:s0'],
                                'start_byte': 0, 'end_byte': 3, 'incomplete': False,
                                'exact_value_producer': False, 'relation': 'storage_dependency_ancestry'}]}}


class ValueProvenanceTests(unittest.TestCase):
    def test_retained_node_origins_are_separate_from_storage_and_synthesis(self):
        trace = sample()
        report = trace['value_provenance']
        report['node_input_limit'] = 16
        node = dict(inputs=[dict(function_id='root:p0', value_origin=0, source_site='f0:b0:s0')],
                    inlined=True, cloned=True, synthesized_by=None, relation='retained_node_ancestry',
                    incomplete=False, exact_value_producer=False)
        report['output_regions'][0]['node_ancestry'] = node
        self.assertEqual(validate(trace, b'abc'), [])
        mutations = [
            lambda n: n.update(exact_value_producer=True),
            lambda n: n['inputs'][0].update(function_id='root:p999'),
            lambda n: n['inputs'][0].update(value_origin=999),
            lambda n: n['inputs'].append(copy.deepcopy(n['inputs'][0])),
            lambda n: n.update(relation='original_value_proved'),
            lambda n: n.update(synthesized_by='inferred_from_missing_debug'),
            lambda n: n.update(cloned=1),
        ]
        for mutation in mutations:
            changed = copy.deepcopy(trace)
            mutation(changed['value_provenance']['output_regions'][0]['node_ancestry'])
            self.assertTrue(validate(changed, b'abc'))
        node.update(inputs=[], inlined=False, cloned=False, synthesized_by='terminal_synthesis', relation='synthesized_node')
        self.assertEqual(validate(trace, b'abc'), [])
        node.update(synthesized_by=None, relation='unknown', incomplete=True)
        self.assertEqual(validate(trace, b'abc'), [])

    def test_valid_ancestry_and_unknown_literal(self):
        trace = sample()
        self.assertEqual(validate(trace, b'abc'), [])
        region = trace['value_provenance']['output_regions'][0]
        region.update(bindings=[], source_sites=[], relation='unknown', incomplete=True)
        self.assertEqual(validate(trace, b'abc'), [])

    def test_counterexamples_are_rejected(self):
        mutations = [
            lambda t: t['functions'][0]['value_origins'][0].update(children=[0]),
            lambda t: t['functions'][0]['value_origins'][1].update(path=[1, 1, 0]),
            lambda t: t['functions'][0]['value_origins'][1].update(binding_id='b999'),
            lambda t: t['value_provenance']['source_sites']['f0:b0:s0'].update(instruction_pcs=[99]),
            lambda t: t['value_provenance']['output_regions'][0].update(exact_value_producer=True),
            lambda t: t['value_provenance']['output_regions'][0].update(source_sites=[]),
            lambda t: t['value_provenance']['output_regions'][0].update(end_byte=99),
            lambda t: t['value_provenance'].update(visited_dependencies=101),
        ]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                trace = copy.deepcopy(sample())
                mutation(trace)
                self.assertTrue(validate(trace, b'abc'))

    def test_unicode_boundaries_and_absent_provenance(self):
        self.assertEqual(validate({}), [])
        trace = sample()
        region = trace['value_provenance']['output_regions'][0]
        self.assertEqual(validate(trace, '界'.encode()), [])
        region['end_byte'] = 1
        self.assertTrue(validate(trace, '界'.encode()))


if __name__ == '__main__':
    unittest.main()
