import unittest

from profile_v2 import AST_PHASES, validate


def valid_profile():
    passes = sorted(AST_PHASES | {'DECOMPILE', 'D_COLLECT_TARGETS', 'D_WRITE_CENSUS'})
    rows = []
    for name in passes:
        rows.append({'script': 'input.lua', 'prototype': None, 'pass': name,
                     'inclusive_ns': 10, 'exclusive_ns': 4, 'max_inclusive_ns': 10,
                     'calls': 1, 'node_samples': int(name in AST_PHASES),
                     'node_samples_incomplete': 0, 'counters': {'iterations': 1} if name == 'S_DEINLINE'
                     else {'deinline_factor_iterations': 1} if name == 'DECOMPILE' else {}})
    return {'schema_version': 1, 'model': 'tovek-pass-thread-wall-v1',
            'rows_count': len(rows), 'rows': rows, 'dropped_records': 0, 'misnested_spans': 0}


class ProfileChecks(unittest.TestCase):
    def test_complete_profile(self):
        self.assertEqual(validate(valid_profile(), {'input.lua'}), [])

    def test_counter_total_and_iteration_mismatches(self):
        for pass_name, counter in [('D_COLLECT_TARGETS', 'candidate_binders'), ('S_DEINLINE', 'iterations')]:
            profile = valid_profile()
            row = next(r for r in profile['rows'] if r['pass'] == pass_name)
            row['counters'][counter] = 2
            self.assertTrue(validate(profile, {'input.lua'}))

    def test_context_and_interval_corruption(self):
        for key, value in [('script', 'foreign.lua'), ('exclusive_ns', 11)]:
            profile = valid_profile()
            profile['rows'][0][key] = value
            self.assertTrue(validate(profile, {'input.lua'}))

    def test_dropped_or_unmeasured_work_cannot_pass_complete_gate(self):
        for key in ('dropped_records', 'misnested_spans'):
            profile = valid_profile()
            profile[key] = 1
            self.assertTrue(validate(profile, {'input.lua'}))
        profile = valid_profile()
        next(r for r in profile['rows'] if r['pass'] == 'S_FACTOR_INITIAL')['node_samples'] = 0
        self.assertTrue(validate(profile, {'input.lua'}))

    def test_ssa_cache_requires_consistent_counts_and_function_context(self):
        profile = valid_profile()
        row = dict(profile['rows'][0], prototype=3, **{'pass': 'F_SSA_INLINE'})
        row['counters'] = {f'ssa_fact_cache_{k}': v for k,v in
                           dict(hits=20, misses=8, uncached=3, invalidations=2, slots=6).items()}
        profile['rows'].append(row)
        profile['rows_count'] += 1
        self.assertEqual(validate(profile, {'input.lua'}), [])
        row['counters']['ssa_fact_cache_invalidations'] = 1
        self.assertIn('SSA cache accounting mismatch', validate(profile, {'input.lua'}))
        row['counters']['ssa_fact_cache_invalidations'] = 2
        row['prototype'] = None
        self.assertIn('SSA cache counter context mismatch', validate(profile, {'input.lua'}))
        del row['counters']['ssa_fact_cache_misses']
        self.assertIn('invalid SSA cache counters', validate(profile, {'input.lua'}))


if __name__ == '__main__':
    unittest.main()
