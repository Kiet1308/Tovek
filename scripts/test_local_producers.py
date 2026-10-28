import copy
import unittest

from local_producers import MODEL, PASSES, RECORD_LIMIT, introductions, validate_local_producers


def fixture(size=1, omitted=0, name='branch_constructors', role='constructor_property_value'):
    records = [dict(binding_id=f'b{i}', role=role) for i in range(size)]
    report = dict(model=PASSES[name][0], introduced_locals=size + omitted,
                  introduced_bindings=dict(records=records, omitted_records=omitted))
    final = [dict(binding_id=f'b{i}', name='v', lineage=[], incomplete=True,
                  emitter_introduction={'pass': name, 'record': i}) for i in range(size)]
    final.append(dict(binding_id=f'b{size}', name='v', lineage=[], incomplete=True))
    trace = dict(final_bindings=final, local_producers=dict(schema_version=1, model=MODEL,
        records_per_pass=RECORD_LIMIT, recorded_introductions=size, omitted_records=omitted,
        passes=[dict(pass_=name, rewrite_model=report['model'],
                     introduced_locals=size + omitted, records=records, omitted_records=omitted)]))
    group = trace['local_producers']['passes'][0]
    group['pass'] = group.pop('pass_')
    metadata = {name: report, 'binding_provenance': trace, 'source_recovery': dict(bindings=[])}
    return trace, metadata


class LocalProducerTests(unittest.TestCase):
    def test_native_literal_snapshots_require_exact_model_role_and_report(self):
        name = 'native_constant_arguments'
        trace, metadata = fixture(name=name, role='evaluation_snapshot')
        self.assertEqual(validate_local_producers(trace, metadata), [])
        self.assertEqual(introductions(trace)['b0'], {
            'pass': name, 'rewrite_model': 'mutable_literal_argument_v1', 'role': 'evaluation_snapshot'})
        def group(d): return d['binding_provenance']['local_producers']['passes'][0]
        mutations = [
            lambda d: group(d).update(rewrite_model='entry_constructor_snapshot_v1'),
            lambda d: group(d)['records'][0].update(role='vector_constructor'),
            lambda d: d.pop(name),
            lambda d: d[name].update(model='unknown'),
            lambda d: d[name].update(introduced_locals=2),
            lambda d: d['binding_provenance']['final_bindings'][0].update(lineage=['b123']),
            lambda d: d['binding_provenance']['final_bindings'][0].update(incomplete=False),
        ]
        for index, mutate in enumerate(mutations):
            changed = copy.deepcopy(metadata)
            mutate(changed)
            with self.subTest(index=index):
                self.assertTrue(validate_local_producers(changed['binding_provenance'], changed))

    def test_native_snapshot_ledger_uses_the_shared_record_budget(self):
        trace, metadata = fixture(RECORD_LIMIT, 2, name='native_constant_arguments', role='evaluation_snapshot')
        self.assertEqual(validate_local_producers(trace, metadata), [])
        self.assertNotIn(f'b{RECORD_LIMIT}', introductions(trace))
        trace['local_producers']['passes'][0]['omitted_records'] = 1
        self.assertTrue(validate_local_producers(trace, metadata))

    def test_vector_snapshot_and_inline_models_have_distinct_binding_contracts(self):
        name = 'materialize_vectors'
        trace, metadata = fixture(name=name, role='vector_constructor')
        self.assertEqual(validate_local_producers(trace, metadata), [])
        for size in (0, 1):
            trace, metadata = fixture(size, name=name, role='vector_constructor')
            metadata[name]['model'] = 'inline_constructor_v1'
            trace['local_producers']['passes'][0]['rewrite_model'] = 'inline_constructor_v1'
            self.assertEqual(bool(validate_local_producers(trace, metadata)), size != 0)

    def test_open_list_snapshots_require_matching_report_and_snapshot_role(self):
        trace, metadata = fixture()
        name = 'table_open_list_snapshots'
        report = metadata.pop('branch_constructors')
        metadata[name] = report
        report['model'] = PASSES[name][0]
        group = trace['local_producers']['passes'][0]
        group.update({'pass': name, 'rewrite_model': report['model']})
        group['records'][0]['role'] = 'constructor_initializer_snapshot'
        trace['final_bindings'][0]['emitter_introduction']['pass'] = name
        self.assertEqual(validate_local_producers(trace, metadata), [])
        group['records'][0]['role'] = 'constructor_property_value'
        self.assertTrue(validate_local_producers(trace, metadata))
        group['records'][0]['role'] = 'constructor_initializer_snapshot'
        metadata.pop(name)
        self.assertTrue(validate_local_producers(trace, metadata))

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
