import copy
import unittest

from pgo_evaluate import compare_samples, validate_corpus


def fixture():
    corpus = dict(evaluation=dict(rounds=7, threads=[1, 16], primary_workload='private', secondary_workload='holdout',
        target_median_reduction_percent=10, allowed_p95_regression_percent=5, allowed_rss_regression_percent=5,
        allowed_holdout_median_regression_percent=5, default_promotion=False),
        trees={d: dict(sha256='input-' + d, files=2) for d in ('train', 'holdout', 'private')},
        summary={d: dict(included=2) for d in ('train', 'holdout', 'private')},
        rows=[dict(dataset=d, status='included', path=str(i), execution_image_sha256=d+str(i), source_sha256=d+str(i))
              for d in ('train', 'holdout', 'private') for i in range(2)])
    build = dict(baseline_sha256='before', optimized_sha256='after')
    report = dict(tools={label: dict(sha256=value) for label, value in (('baseline', 'before'), ('pgo', 'after'))},
        input_count=2, corpus_hash='input-private', analysis_modes={}, lifter_args=dict(baseline=[], pgo=[]),
        deterministic=True, rows=[])
    for warmup, threads, index in [(True, 16, 0)] + [(False, t, i) for t in (1, 16) for i in range(7)]:
        for label in ('baseline', 'pgo'):
            report['rows'].append(dict(label=label, threads=threads, round=index, warmup=warmup, deterministic=True,
                output_count=2, output_hash='same-source', seconds=10.0 if label == 'baseline' else 8.0, peak_rss_bytes=1000))
    return corpus, build, report


class PgoEvaluationTests(unittest.TestCase):
    def test_complete_samples_and_recomputed_summary(self):
        corpus, build, report = fixture()
        validate_corpus(corpus)
        report['summary'] = 'ignored: untrusted cached summaries cannot establish speedups'
        rows = compare_samples(report, corpus, 'private', build)
        self.assertTrue(all(row['passed'] for row in rows))
        self.assertEqual([row['baseline']['samples'] for row in rows], [7, 7])
        self.assertAlmostEqual(rows[0]['change_percent']['median_seconds'], -20)

    def test_fast_different_output_and_incomplete_evidence_rejected(self):
        corpus, build, report = fixture()
        mutations = {
            'different_output': lambda r: r['rows'][3].update(output_hash='wrong'),
            'missing_file': lambda r: r['rows'][3].update(output_count=1),
            'missing_sample': lambda r: r['rows'].pop(),
            'duplicate_sample': lambda r: r['rows'].append(copy.deepcopy(r['rows'][-1])),
            'false_determinism': lambda r: r['rows'][3].update(deterministic=False),
            'unknown_thread': lambda r: r['rows'][3].update(threads=8),
            'nan_time': lambda r: r['rows'][3].update(seconds=float('nan')),
            'wrong_binary': lambda r: r['tools']['pgo'].update(sha256='unrelated'),
            'wrong_corpus': lambda r: r.update(corpus_hash='unrelated'),
            'different_options': lambda r: r['lifter_args']['pgo'].append('--compact-annotations'),
            'missing_warmup': lambda r: r['rows'].pop(0),
        }
        for name, mutate in mutations.items():
            with self.subTest(name=name):
                broken = copy.deepcopy(report)
                mutate(broken)
                with self.assertRaises(ValueError): compare_samples(broken, corpus, 'private', build)

    def test_regressions_are_reported_not_discarded(self):
        corpus, build, report = fixture()
        pgo = [r for r in report['rows'] if not r['warmup'] and r['label'] == 'pgo' and r['threads'] == 1]
        pgo[-1]['seconds'] = 12
        for row in pgo: row['peak_rss_bytes'] = 1100
        result = compare_samples(report, corpus, 'private', build)[0]
        self.assertEqual(result['gates'], dict(median=True, p95=False, rss=False))
        self.assertFalse(result['passed'])
        pgo[-1]['peak_rss_bytes'] = None
        result = compare_samples(report, corpus, 'private', build)[0]
        self.assertIsNone(result['change_percent']['median_peak_rss_bytes'])
        self.assertFalse(result['gates']['rss'])

    def test_holdout_has_distinct_frozen_median_gate(self):
        corpus, build, report = fixture()
        report['corpus_hash'] = 'input-holdout'
        for row in report['rows']:
            if row['label'] == 'pgo': row['seconds'] = 10.4
        self.assertTrue(all(r['passed'] for r in compare_samples(report, corpus, 'holdout', build)))
        for row in report['rows']:
            if row['label'] == 'pgo': row['seconds'] = 10.6
        self.assertFalse(any(r['passed'] for r in compare_samples(report, corpus, 'holdout', build)))

    def test_cross_split_leakage_and_count_drift_rejected(self):
        corpus, _, _ = fixture()
        for field, value in (('execution_image_sha256', 'train0'), ('source_sha256', 'train1'), ('path', '1')):
            with self.subTest(field=field):
                broken = copy.deepcopy(corpus)
                broken['rows'][2][field] = value
                with self.assertRaises(ValueError): validate_corpus(broken)
        corpus['trees']['holdout']['files'] = 1
        with self.assertRaises(ValueError): validate_corpus(corpus)


if __name__ == '__main__':
    unittest.main()
