import unittest

from local_producers import MODEL, PASSES, RECORD_LIMIT, introductions, validate_local_producers


def fixture(size=1, omitted=0):
    records = [dict(binding_id=f'b{i}', role='constructor_property_value') for i in range(size)]
    report = dict(model=PASSES['branch_constructors'][0], introduced_locals=size + omitted,
                  introduced_bindings=dict(records=records, omitted_records=omitted))
    final = [dict(binding_id=f'b{i}', name='v', lineage=[], incomplete=True,
                  emitter_introduction={'pass': 'branch_constructors', 'record': i}) for i in range(size)]
    final.append(dict(binding_id=f'b{size}', name='v', lineage=[], incomplete=True))
    trace = dict(final_bindings=final, local_producers=dict(schema_version=1, model=MODEL,
        records_per_pass=RECORD_LIMIT, recorded_introductions=size, omitted_records=omitted,
        passes=[dict(pass_='branch_constructors', rewrite_model=report['model'],
                     introduced_locals=size + omitted, records=records, omitted_records=omitted)]))
    group = trace['local_producers']['passes'][0]
    group['pass'] = group.pop('pass_')
    metadata = dict(branch_constructors=report, binding_provenance=trace, source_recovery=dict(bindings=[]))
    return trace, metadata


class LocalProducerTests(unittest.TestCase):
    def test_explicit_record_only_does_not_promote_unattributed_binding(self):
        trace, metadata = fixture()
        self.assertEqual(validate_local_producers(trace, metadata), [])
        self.assertEqual(list(introductions(trace)), ['b0'])
        self.assertTrue(all(row['incomplete'] for row in trace['final_bindings']))

    def test_mutants_cannot_relabel_input_or_fake_committed_introduction(self):
        def group(d): return d['binding_provenance']['local_producers']['passes'][0]
        def record(d): return group(d)['records'][0]
        def final(d): return d['binding_provenance']['final_bindings'][0]
        mutations = [
            lambda d: record(d).update(binding_id='b999'),
            lambda d: record(d).update(binding_id='b00'),
            lambda d: record(d).update(role='parameter'),
            lambda d: record(d).update(role='evaluation_snapshot'),
            lambda d: group(d).update(pass_='ignored', **{'pass': 'unknown'}),
            lambda d: group(d).update(rewrite_model='unknown'),
            lambda d: final(d).update(lineage=['b123']),
            lambda d: final(d).update(recorded_source_origins=[dict(kind='debug_local')]),
            lambda d: final(d).update(incomplete=False),
            lambda d: final(d).update(emitter_introduction={'pass': 'branch_constructors', 'record': 1}),
            lambda d: d['source_recovery']['bindings'].append(dict(binding_id='b0', origins=[dict(kind='debug_local')])),
            lambda d: d['branch_constructors'].update(introduced_locals=2),
            lambda d: d.pop('branch_constructors'),
            lambda d: d['binding_provenance']['local_producers'].update(recorded_introductions=0),
        ]
        for index, mutate in enumerate(mutations):
            _, metadata = fixture()
            mutate(metadata)
            with self.subTest(index=index):
                self.assertTrue(validate_local_producers(metadata['binding_provenance'], metadata))

    def test_duplicate_record_and_reverse_link_are_rejected(self):
        trace, metadata = fixture(2)
        trace['local_producers']['passes'][0]['records'][1]['binding_id'] = 'b0'
        self.assertTrue(validate_local_producers(trace, metadata))
        trace, metadata = fixture()
        trace['final_bindings'][1]['emitter_introduction'] = {'pass': 'branch_constructors', 'record': 0}
        self.assertTrue(validate_local_producers(trace, metadata))

    def test_budget_omissions_remain_unknown_and_must_be_accounted_for(self):
        trace, metadata = fixture(RECORD_LIMIT, 2)
        self.assertEqual(validate_local_producers(trace, metadata), [])
        self.assertNotIn(f'b{RECORD_LIMIT}', introductions(trace))
        trace['local_producers']['omitted_records'] = 0
        self.assertTrue(validate_local_producers(trace, metadata))
        trace, metadata = fixture(1, 1)
        self.assertTrue(validate_local_producers(trace, metadata))

    def test_old_sidecars_are_readable_without_new_origin_claims(self):
        trace, _ = fixture()
        trace.pop('local_producers')
        trace['final_bindings'][0].pop('emitter_introduction')
        self.assertEqual(validate_local_producers(trace), [])
        self.assertEqual(introductions(trace), {})
        trace['final_bindings'][0]['emitter_introduction'] = {'pass': 'branch_constructors', 'record': 0}
        self.assertTrue(validate_local_producers(trace))

    def test_malformed_ledger_is_refused(self):
        for value in (None, 'bad', 3, {}, [1]):
            trace, metadata = fixture()
            trace['local_producers']['passes'] = value
            self.assertTrue(validate_local_producers(trace, metadata))


if __name__ == '__main__':
    unittest.main()
