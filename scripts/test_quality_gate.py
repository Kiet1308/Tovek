import copy
import json
import pathlib
import tempfile
import unittest

from quality_gate import GATE, SNAPSHOT, compare, freeze, normalize_report, validate_reviews


def row(name='case', output='c', proof='unknown', temps=0):
    return dict(case_id=name, name=name, report_label='fixtures', profile={'opt': 2, 'debug': 1},
                source_sha256='a'*64, decoded_input_sha256='b'*64, output_sha256=output*64,
                status='passed', compile_status='passed', proof={'status': proof, 'model': 'test-model'},
                runtime={'status': 'passed', 'model': 'exit+stdout', 'source': 'e'*64, 'output': 'e'*64},
                presentation={'discard_locals': temps}, fidelity={'exact_names': 4})


def snapshot(*rows):
    return dict(schema_version=1, kind=SNAPSHOT, contexts={'fixtures': {'compiler_sha256': 'f'*64}}, rows=list(rows))


def reviews(*violations):
    return dict(schema_version=1, reviews=[dict(v, reason='Reviewed exact change with independent semantic evidence.') for v in violations])


class QualityGateTests(unittest.TestCase):
    def test_unchanged_unknown_stays_unknown_and_is_not_a_new_proof(self):
        report = compare(snapshot(row()), snapshot(row()))
        self.assertEqual(report['status'], 'passed')
        self.assertEqual(report['summary']['current_proofs'], {'unknown': 1})
        self.assertEqual(report['approved_outputs'], [])

    def test_changed_unknown_requires_hash_bound_review_despite_runtime_pass(self):
        before, after = snapshot(row()), snapshot(row(output='d'))
        result = compare(before, after)
        self.assertEqual([r['check'] for r in result['failures']], ['unproved_output_change'])
        approved = compare(before, after, allowlist=reviews(*result['failures']))
        self.assertEqual(approved['status'], 'passed')
        self.assertEqual(approved['approved_outputs'][0]['decoded_input_sha256'], 'b'*64)
        self.assertEqual(approved['summary']['current_proofs'], {'unknown': 1})
        after['rows'][0]['output_sha256'] = 'f'*64
        self.assertEqual(compare(before, after, allowlist=reviews(*result['failures']))['status'], 'failed')

    def test_proof_loss_and_output_change_are_separate_review_decisions(self):
        before, after = snapshot(row(proof='proved')), snapshot(row(output='d'))
        result = compare(before, after)
        self.assertEqual({v['check'] for v in result['failures']}, {'proof_regression', 'unproved_output_change'})
        partial = compare(before, after, allowlist=reviews(result['failures'][0]))
        self.assertEqual(partial['status'], 'failed')
        self.assertEqual(len(partial['reviewed_exceptions']), 1)
        self.assertEqual(compare(before, after, allowlist=reviews(*result['failures']))['status'], 'passed')

    def test_proved_text_change_can_pass_but_presentation_regressions_cannot_hide(self):
        before = snapshot(row('a', proof='proved', temps=10), row('b', proof='proved'))
        after = snapshot(row('a', output='d', proof='proved'), row('b', output='d', proof='proved', temps=1))
        result = compare(before, after)
        self.assertEqual(len(result['failures']), 1)
        self.assertEqual(result['failures'][0]['case_id'], 'b')
        self.assertEqual(result['failures'][0]['check'], 'presentation_increase')
        self.assertEqual(compare(before, after, allowlist=reviews(*result['failures']))['status'], 'passed')

    def test_runtime_compile_missing_coverage_and_input_changes_are_unwaivable(self):
        for mutation, expected in [
            (lambda r: r.update(compile_status='unavailable'), 'compilation_unverified'),
            (lambda r: r.update(status='failed'), 'status_failed'),
            (lambda r: r['runtime'].update(status='failed'), 'runtime_failed_or_lost'),
            (lambda r: r.update(decoded_input_sha256='f'*64), 'input_identity_changed'),
        ]:
            with self.subTest(expected=expected):
                candidate = row()
                mutation(candidate)
                result = compare(snapshot(row()), snapshot(candidate))
                violation = next(v for v in result['failures'] if v['check'] == expected)
                with self.assertRaises(ValueError):
                    validate_reviews(reviews(violation))
        self.assertEqual(compare(snapshot(row('a'), row('b')), snapshot(row('a')))['failures'][0]['check'], 'case_coverage_changed')

    def test_nan_unavailable_metrics_and_fidelity_losses_do_not_pass(self):
        for value in (float('nan'), float('inf'), -1, True):
            candidate = row()
            candidate['presentation']['discard_locals'] = value
            self.assertEqual(compare(snapshot(row()), snapshot(candidate))['failures'][0]['check'], 'metric_unmeasured')
        candidate = row()
        candidate['presentation'] = None
        self.assertEqual(compare(snapshot(row()), snapshot(candidate))['failures'][0]['check'], 'metrics_unavailable')
        candidate = row()
        candidate['fidelity']['exact_names'] = 3
        self.assertEqual(compare(snapshot(row()), snapshot(candidate), minimum_metrics=['exact_names'])['failures'][0]['check'], 'fidelity_decrease')

    def test_stale_duplicate_wildcard_and_unexplained_reviews_fail(self):
        result = compare(snapshot(row()), snapshot(row(output='d')))
        allowlist = reviews(*result['failures'])
        self.assertEqual(compare(snapshot(row()), snapshot(row()), allowlist=allowlist)['failures'][0]['check'], 'stale_review')
        allowlist['reviews'].append(copy.deepcopy(allowlist['reviews'][0]))
        with self.assertRaises(ValueError):
            validate_reviews(allowlist)
        allowlist = reviews(*result['failures'])
        allowlist['reviews'][0]['reason'] = ''
        with self.assertRaises(ValueError):
            validate_reviews(allowlist)

    def test_freeze_binds_profiles_and_ignores_runtime_timing(self):
        observation = {'exit': 0, 'stdout': '2\n', 'stderr': '', 'seconds': 1.2}
        report = dict(bytecode_version=12, tools={'compiler': {'sha256': 'f'*64}}, cases=[
            dict(case='loop', opt=2, debug=1, status='passed', recompile='passed', source_sha256='a'*64,
                 decoded_input_sha256='b'*64, output_sha256='c'*64, dataflow={'status': 'unknown'},
                 output_quality={}, runtime={'source': observation, 'output': dict(observation, seconds=8)})])
        _, normalized = normalize_report('runtime', report)
        self.assertEqual(normalized[0]['runtime']['status'], 'passed')
        self.assertEqual(normalized[0]['profile'], {'bytecode_version': 12, 'opt': 2, 'debug': 1})
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory)/'report.json'
            path.write_text(json.dumps(report), encoding='utf-8')
            frozen = freeze([('runtime', path)])
            self.assertEqual(compare(frozen, frozen)['status'], 'passed')
            with self.assertRaises(ValueError):
                freeze([('runtime', path), ('runtime', path)])


if __name__ == '__main__':
    unittest.main()
