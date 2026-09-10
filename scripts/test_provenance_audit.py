import copy
import unittest

from provenance_audit import validate_trace


def valid_trace():
    return {'schema_version': 1, 'limits': {'ancestry_per_binding': 256, 'records_per_function': 50000},
            'functions': [{'instruction_count': 2, 'dropped_records': 0,
                'registers': [{'id': 'b1', 'final_bindings': []}],
                'definitions': [{'id': 'b2', 'original_register': 'b1', 'block': 0,
                    'statement_index': 0, 'write_index': 0, 'dependencies': ['b1'], 'final_bindings': ['b3']}],
                'lifted_statements': [{'block': 0, 'statement_index': 0, 'instruction_pcs': [0, 1],
                    'source_lines': [10], 'read_registers': ['b1'], 'written_registers': ['b1']}],
                'conditional_results': []}],
            'final_bindings': [{'binding_id': 'b3', 'lineage': ['b2'], 'incomplete': False,
                                'unknown_origins': [], 'has_conditional_result_ancestry': False}]}


class TraceChecks(unittest.TestCase):
    def test_accepts_consistent_trace(self):
        self.assertEqual(validate_trace(valid_trace()), [])

    def test_bad_pc_and_write_slot_are_detected(self):
        for kind in ('pc', 'slot'):
            trace = valid_trace()
            f = trace['functions'][0]
            if kind == 'pc':
                f['lifted_statements'][0]['instruction_pcs'] = [2]
            else:
                f['definitions'][0]['write_index'] = 1
            self.assertTrue(validate_trace(trace))

    def test_both_mapping_directions_checked(self):
        for direction in ('forward', 'reverse'):
            trace = valid_trace()
            if direction == 'forward':
                trace['functions'][0]['definitions'][0]['final_bindings'] = []
            else:
                trace['final_bindings'][0]['lineage'] = ['b1']
            self.assertTrue(validate_trace(trace))

    def test_missing_trace_is_never_complete(self):
        trace = valid_trace()
        final = trace['final_bindings'][0]
        final['lineage'].append('b4')
        final['unknown_origins'] = ['b4']
        self.assertTrue(validate_trace(trace))
        final['incomplete'] = True
        self.assertEqual(validate_trace(trace), [])

    def test_duplicate_ids_across_functions_rejected(self):
        trace = valid_trace()
        trace['functions'].append(copy.deepcopy(trace['functions'][0]))
        self.assertIn('origin ID reused across functions', validate_trace(trace))


if __name__ == '__main__':
    unittest.main()
