import collections
import copy
import contextlib
import hashlib
import io
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

import check_deep_review as review


def observed(stdout, exit_code=0, stderr=''):
    return dict(exit_code=exit_code, stdout=stdout, stderr=stderr)


def mismatch():
    return dict(ca='case', input_optimization=0, output_optimization=1, debug=0,
                source_sha256=hashlib.sha256(b'return 1').hexdigest(),
                driver_sha256=hashlib.sha256(b'').hexdigest(),
                compiler_options=review.compiler_options(), driver_optimization=1, driver_debug=1,
                status='failed', ket_qua='FAIL', goc=observed('before\n'),
                sau_sua=observed('event\nafter\n'), source_at_output_optimization=observed('event\nafter\n'))


def baseline(row):
    return {field: copy.deepcopy(row[field]) for field in review.IDENTITY_FIELDS} | dict(
        status='compiler_sensitive', observations=review.observation_fingerprints(row))


class DeepReviewTests(unittest.TestCase):
    def test_only_exact_reviewed_mismatches_are_compiler_sensitive(self):
        row = mismatch()
        known = baseline(row)
        review.classify_baseline(row, None)
        self.assertEqual(row['status'], 'failed')
        review.classify_baseline(row, known)
        self.assertEqual(row['status'], 'compiler_sensitive')
        self.assertNotEqual(row['ket_qua'], 'PASS')
        mutations = [
            lambda row: row.update(source_sha256='changed'),
            lambda row: row.update(driver_sha256='changed'),
            lambda row: row['compiler_options'].append('--fflags=true'),
            lambda row: row.update(driver_optimization=2),
            lambda row: row['sau_sua'].update(stdout='wrong\n'),
            lambda row: row['goc'].update(stdout='changed input\n'),
            lambda row: row['source_at_output_optimization'].update(stdout='changed reference\n'),
            lambda row: [row[key].update(stdout='after\nevent\n') for key in ('sau_sua', 'source_at_output_optimization')],
            lambda row: [row[key].update(stderr='different error\n') for key in ('sau_sua', 'source_at_output_optimization')],
            lambda row: [row[key].update(exit_code=1) for key in ('sau_sua', 'source_at_output_optimization')],
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                row = mismatch()
                mutate(row)
                review.classify_baseline(row, known)
                self.assertEqual(row['status'], 'failed')

    def test_raw_fingerprint_cannot_hide_changed_printed_locations(self):
        row = mismatch()
        for key in ('sau_sua', 'source_at_output_optimization'):
            row[key]['stdout'] = 'after subject:12 /file.luau:34\n'
        known = baseline(row)
        for key in ('sau_sua', 'source_at_output_optimization'):
            row[key]['stdout'] = 'after subject:99 /file.luau:98\n'
        self.assertEqual(review.observation_fingerprints(row)['sau_sua']['normalized'],
                         known['observations']['sau_sua']['normalized'])
        review.classify_baseline(row, known)
        self.assertEqual(row['status'], 'failed')

    def test_previous_sensitive_profile_that_now_matches_is_an_improvement(self):
        row = mismatch()
        known = baseline(row)
        row.update(status='passed', ket_qua='PASS', sau_sua=copy.deepcopy(row['goc']))
        review.classify_baseline(row, known)
        self.assertEqual(row['status'], 'passed')
        self.assertEqual(row['improved_from'], 'compiler_sensitive')

    def test_refusals_are_separate_and_do_not_match_a_sensitivity_baseline(self):
        with tempfile.TemporaryDirectory() as directory:
            args = types.SimpleNamespace(work=Path(directory), vm=Path('vm'), lifter=Path('lifter'))
            for refusal, known, expected in [(None, None, 'failed'),
                                             ('exact refusal', None, 'expected_refusal'),
                                             ('another refusal', None, 'failed'),
                                             (None, baseline(mismatch()), 'failed')]:
                with self.subTest(refusal=refusal, baseline=known is not None), \
                        patch.object(review, 'compile_source'), \
                        patch.object(review, 'run', side_effect=[observed('before\n'), observed('', 1, 'exact refusal')]), \
                        contextlib.redirect_stdout(io.StringIO()):
                    row = review.check(args, 'case', 'return 1', '', 0, 0,
                                       output_optimization=1, refusal=refusal, baseline=known)
                    self.assertEqual(row['status'], expected)
                    self.assertNotIn('sau_sua', row)
                    self.assertEqual(row['input_optimization'], 0)
                    self.assertEqual(row['output_optimization'], 1)

    def test_successful_exit_does_not_hide_stderr_or_event_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            args = types.SimpleNamespace(work=Path(directory), vm=Path('vm'), lifter=Path('lifter'))
            for before, output in [
                ('first\nsecond\n', observed('second\nfirst\n')),
                ('first\nsecond\n', observed('first\nsecond\n', stderr='different error')),
                ('subject:12\n', observed('subject:99\n')),
                ('event\n', observed(' event\n')),
                ('event\n', observed('event\n\n')),
            ]:
                with patch.object(review, 'compile_source'), \
                        patch.object(review, 'run', side_effect=[observed(before), observed('return 1'), output]), \
                        contextlib.redirect_stdout(io.StringIO()):
                    row = review.check(args, 'case', 'return 1', '', 0, 0, output_optimization=1)
                    self.assertEqual(row['status'], 'failed')
                    self.assertEqual(row['failure_stage'], 'runtime_comparison')

    def test_invalid_utf8_bytes_remain_distinct_through_run_and_check(self):
        results = [types.SimpleNamespace(returncode=0, stdout=stdout, stderr=b'')
                   for stdout in (b'\xff\n', b'return 1', b'\xfe\n')]
        with tempfile.TemporaryDirectory() as directory:
            args = types.SimpleNamespace(work=Path(directory), vm=Path('vm'), lifter=Path('lifter'))
            with patch.object(review, 'compile_source'), \
                    patch.object(review.subprocess, 'run', side_effect=results), \
                    contextlib.redirect_stdout(io.StringIO()):
                row = review.check(args, 'binary_output', 'return 1', '', 0, 0, output_optimization=1)
            self.assertEqual(row['status'], 'failed')
            self.assertEqual(row['failure_stage'], 'runtime_comparison')
            self.assertEqual(row['goc']['stdout'].encode('utf-8', errors='surrogateescape'), b'\xff\n')
            self.assertEqual(row['sau_sua']['stdout'].encode('utf-8', errors='surrogateescape'), b'\xfe\n')
            self.assertNotEqual(row['observations']['goc']['raw'], row['observations']['sau_sua']['raw'])
            saved = json.loads(next(args.work.glob('*/result.json')).read_text())
            self.assertEqual(saved['goc'], row['goc'])
            self.assertEqual(saved['sau_sua'], row['sau_sua'])

    def test_matrix_exercises_all_pairs_and_fails_for_unexercised_baseline(self):
        def checked(args, name, source, driver, optimization, debug, **kwargs):
            return dict(ca=name, input_optimization=optimization, output_optimization=kwargs['output_optimization'],
                        debug=debug, group='test', status='passed')

        with tempfile.TemporaryDirectory() as directory:
            args = types.SimpleNamespace(work=Path(directory), compiler=Path(__file__), vm=Path(__file__), lifter=Path(__file__))
            for known, failed in [({}, False), ({('missing', 0, 1, 0): baseline(mismatch())}, True)]:
                with patch.object(review, 'load_cross_optimization_baseline', return_value=known), \
                        patch.object(review, 'CROSS_OPT_BASELINE', Path(__file__)), \
                        patch.object(review, 'cross_optimization_cases', return_value=[('case', '', '', False)]), \
                        patch.object(review, 'check', side_effect=checked), contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(review.run_cross_optimization(args), failed)
                    rows = json.loads((args.work / 'results.json').read_text())
                    expected = {('case', source, output, debug) for source in range(3) for output in range(3) for debug in (0, 2)}
                    self.assertEqual(collections.Counter(map(review.profile_key, rows)), collections.Counter(expected))


if __name__ == '__main__':
    unittest.main()
