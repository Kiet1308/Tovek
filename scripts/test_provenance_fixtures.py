import copy
import json
import pathlib
import shutil
import tempfile
import unittest

from expected_refusals import refusal_evidence
from provenance_fixtures import select_audit_rows
from roadmap_v2 import sha256


class FixtureRefusalChecks(unittest.TestCase):
    def setUp(self):
        self.root = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.profile = dict(opt=1, debug=1, bytecode_version=9,
                            compiler_flags=['--fflags=false'], lifter_args=[])
        self.reason = 'cannot preserve open call results across an operation without a tuple-order proof'

    def report(self, public=False, all_passed=False):
        for name in ('open', 'ordinary'):
            (self.root / f'{name}.luau').write_text('return "' + name + '"')
        digest = sha256(self.root / 'open.luau')
        entry = dict(name='open', repo='repo', file='open.luau', source='open.luau', source_sha256=digest,
                     group='order', split='development', groups=['order'],
                     expected_refusal=dict(reason=self.reason, profiles=[self.profile]))
        ordinary = dict(name='ordinary', repo='repo', file='ordinary.luau', source='ordinary.luau',
                        source_sha256=sha256(self.root / 'ordinary.luau'),
                        group='order', split='development', groups=['order'])
        path = self.root / ('public.json' if public else 'fixtures.json')
        path.write_text(json.dumps(dict(schema_version=1, optimization_levels=[1], debug_level=1,
                                       debug_levels=[1], negative_controls=[{'case': 'swap'}],
                                       **{'sources' if public else 'cases': [entry, ordinary]})))
        outcome = dict(exit_code=1, stdout='', stderr='prototype 0, pc 7: ' + self.reason + '\n')
        refused = dict(case='open', repo='repo', file='open.luau', opt=1, debug=1,
                       source_sha256=digest, status='expected_refusal', output_bytes=0,
                       group='order', split='development', groups=['order'],
                       dataflow={'status': 'unavailable'}, source_fidelity={'status': 'unavailable'},
                       input=str(self.root / 'open.luau.luaubc'),
                       refusal=refusal_evidence(entry, digest, self.profile, outcome))
        passed = dict(case='ordinary', repo='repo', file='ordinary.luau', opt=1, debug=1, status='passed',
                      source_sha256=ordinary['source_sha256'], group='order', split='development', groups=['order'],
                      dataflow={'status': 'unavailable'}, source_fidelity={'status': 'unavailable'})
        if all_passed:
            refused.update(status='passed')
            refused.pop('refusal')
        statuses = {'passed': 2} if all_passed else {'passed': 1, 'expected_refusal': 1}
        counters = dict(total=2, status=statuses, dataflow={'unavailable': 2}, ast={'unavailable': 2})
        summary = ({'development': counters} if public else
                   dict(total=2, status=statuses, dataflow={'unavailable': 2}, controls={'passed': 1},
                        groups={'order': statuses}))
        return dict(schema_version=1, manifest_path=str(path), manifest_sha256=sha256(path), bytecode_version=9,
                    compiler_flags=['--fflags=false'] if public else ['--binary', '--fflags=false'],
                    lifter_args=[], work=str(self.root), summary=summary, source_groups={'order': counters},
                    controls=[{'case': 'swap', 'opt': 1, 'status': 'passed'}],
                    **{'rows' if public else 'cases': [passed, refused]})

    def test_audits_only_successful_rows_without_losing_the_input_denominator(self):
        for public in (False, True):
            with self.subTest(public=public):
                report = self.report(public)
                passed, coverage = select_audit_rows(report, public=public)
                self.assertEqual(len(passed), 1)
                self.assertEqual((coverage['total'], coverage['audited'], coverage['expected_refusal']), (2, 1, 1))
                self.assertEqual(coverage['refusals'][0]['refusal']['reason'], self.reason)

    def test_unknown_failures_and_unvalidated_refusals_cannot_be_skipped(self):
        changes = [lambda r: r.update(status='failed'), lambda r: r.update(status='unknown'),
                   lambda r: r.pop('refusal'), lambda r: r.update(source_sha256='b' * 64),
                   lambda r: r.update(opt=2), lambda r: r.update(debug=2),
                   lambda r: r['refusal'].update(stderr='unknown refusal'),
                   lambda r: r['refusal'].update(exit_code=-11),
                   lambda r: r['refusal'].update(profile={**self.profile, 'opt': 2})]
        for public in (False, True):
            for change in changes:
                with self.subTest(public=public, change=change):
                    report = self.report(public)
                    change(report['rows' if public else 'cases'][1])
                    with self.assertRaises(ValueError):
                        select_audit_rows(report, public=public)

    def test_partial_output_and_successful_recompilation_cannot_be_refusals(self):
        for field, value in [('output', 'partial.luau'), ('output_sha256', 'b' * 64),
                             ('output_bytes', 1), ('recompile', 'passed'), ('runtime', {'output': {}})]:
            report = self.report()
            report['cases'][1][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                select_audit_rows(report, public=False)
        report = self.report()
        report['cases'][1]['refusal']['stdout'] = 'return 7'
        with self.assertRaises(ValueError):
            select_audit_rows(report, public=False)
        for public in (False, True):
            report = self.report(public)
            output = self.root / ('open.luau.out.luau' if public else 'open_O1_g1/output.luau')
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text('return 7')
            with self.subTest(public=public), self.assertRaises(ValueError):
                select_audit_rows(report, public=public)

    def test_changed_manifest_options_controls_and_duplicate_profiles_fail(self):
        changes = [lambda r: r.update(manifest_sha256='b' * 64),
                   lambda r: r.update(lifter_args=['--style=compact']),
                   lambda r: r.update(compiler_flags=['--binary', '--fflags=true']),
                   lambda r: r['controls'][0].update(status='failed'),
                   lambda r: r['cases'].append(copy.deepcopy(r['cases'][0])),
                   lambda r: r['cases'].pop(0)]
        for change in changes:
            report = self.report()
            change(report)
            with self.subTest(change=change), self.assertRaises(ValueError):
                select_audit_rows(report, public=False)

    def test_missing_profiles_are_rejected_even_when_the_summary_is_rewritten(self):
        for public in (False, True):
            for removed in (0, 1):
                for rewrite_summary in (False, True):
                    report = self.report(public)
                    rows = report['rows' if public else 'cases']
                    rows.pop(removed)
                    if rewrite_summary:
                        summary = report['summary']['development'] if public else report['summary']
                        summary.update(total=1, status={rows[0]['status']: 1})
                    with self.subTest(public=public, removed=removed, rewrite_summary=rewrite_summary), \
                            self.assertRaisesRegex(ValueError, 'missing manifest profiles'):
                        select_audit_rows(report, public=public)

    def test_complete_reports_must_have_consistent_summaries(self):
        for public in (False, True):
            for field, value in [('total', 1), ('status', {'passed': 2}), ('dataflow', {'unavailable': 1})]:
                report = self.report(public)
                summary = report['summary']['development'] if public else report['summary']
                summary[field] = value
                with self.subTest(public=public, field=field), self.assertRaises(ValueError):
                    select_audit_rows(report, public=public)
            report = self.report(public)
            if public:
                report['source_groups'] = {}
            else:
                report['controls'] = []
            with self.assertRaises(ValueError):
                select_audit_rows(report, public=public)

    def test_reports_without_refusals_still_require_a_locked_complete_manifest(self):
        for public in (False, True):
            report = self.report(public, all_passed=True)
            passed, coverage = select_audit_rows(report, public=public)
            self.assertEqual((len(passed), coverage['total'], coverage['expected_refusal']), (2, 2, 0))
            for change in [lambda r: r.pop('manifest_path'), lambda r: r.update(manifest_sha256='b' * 64),
                           lambda r: r['rows' if public else 'cases'].pop(),
                           lambda r: r['rows' if public else 'cases'][0].update(source_sha256='b' * 64)]:
                changed = copy.deepcopy(report)
                change(changed)
                with self.subTest(public=public, change=change), self.assertRaises((KeyError, ValueError)):
                    select_audit_rows(changed, public=public)

    def test_refusals_cannot_claim_output_analysis_even_with_matching_summaries(self):
        for public in (False, True):
            report = self.report(public)
            report['rows' if public else 'cases'][1]['dataflow'] = {'status': 'proved'}
            summary = report['summary']['development'] if public else report['summary']
            summary['dataflow'] = {'unavailable': 1, 'proved': 1}
            with self.subTest(public=public), self.assertRaisesRegex(ValueError, 'claims analyzed output'):
                select_audit_rows(report, public=public)


if __name__ == '__main__':
    unittest.main()
