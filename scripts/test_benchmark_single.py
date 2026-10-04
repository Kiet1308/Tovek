import base64
import copy
import json
import pathlib
import tempfile
import unittest

from benchmark_single import FEATURE_KEYS, approved_change, decode, digest, load_manifest, pin, review_comparison, sha256, summarize, validate_samples


def item(path='a'):
    return dict(path=path, encoding='raw', input_sha256='a'*64, decoded_input_sha256='b'*64,
                decoded_input_bytes=12, script_name=path, groups=['all'])


def sample_report(inputs, rounds=2):
    rows = []
    for round_ in range(rounds+1):
        for source in inputs:
            index = len(rows)
            rows.append(dict(path=source['path'], input_sha256=source['input_sha256'],
                             decoded_input_sha256=source['decoded_input_sha256'], decoded_input_bytes=source['decoded_input_bytes'],
                             script_name=source['script_name'], round=round_, invocation=index, first_for_file=round_ == 0,
                             first_in_process=index == 0, threads=1, option_bits=8, status='passed',
                             seconds=10 if round_ == 0 else 1, output_sha256='c'*64, cli_output_sha256='d'*64,
                             output_bytes=15, fallback_count=None, retry_count=None))
    return dict(schema_version=1, kind='tovek-single-script-samples-v1', api='try_decompile_bytecode_with_options',
                complete=True, executable_sha256='e'*64, manifest_sha256='f'*64, threads=1, rounds=rounds,
                option_bits=8, decode_key=1, scripts=len(inputs), instrumented=False,
                features={key: False for key in FEATURE_KEYS}, build={'debug_assertions': False, 'panic_unwind': True}, rows=rows, valid=True)


def validate(report, inputs):
    return validate_samples(report, inputs, executable_hash='e'*64, manifest_hash='f'*64,
                            threads=1, rounds=2, option_bits=8, decode_key=1)


class SingleScriptBenchmarkTests(unittest.TestCase):
    def test_raw_base64_inputs_and_expected_hashes_are_pinned_before_timing(self):
        raw = b'\x09\x00\xff\x01'
        self.assertEqual(decode(b'-- saved file\n'+base64.b64encode(raw)+b'\r\n', 'base64'), raw)
        self.assertEqual(decode(raw, 'raw'), raw)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root/'a.luaubc').write_bytes(raw)
            manifest = pin(root, ['*.luaubc'], 'raw', 1)
            path = root/'manifest.json'
            path.write_text(json.dumps(manifest), encoding='utf-8')
            _, selected = load_manifest(path, root)
            self.assertEqual(selected[0]['decoded_input_sha256'], digest(raw))
            (root/'a.luaubc').write_bytes(b'drift')
            with self.assertRaisesRegex(ValueError, 'hash mismatch'):
                load_manifest(path, root)

    def test_missing_slow_sample_duplicates_and_changed_input_cannot_improve_score(self):
        inputs = [item('a'), item('b')]
        good = sample_report(inputs)
        self.assertEqual(len(validate(good, inputs)), 6)
        for mutate in [lambda r: r['rows'].pop(),
                       lambda r: r['rows'].__setitem__(1, r['rows'][0]),
                       lambda r: r['rows'][2].update(input_sha256='0'*64),
                       lambda r: r['rows'][0].update(first_in_process=False)]:
            report = copy.deepcopy(good)
            mutate(report)
            with self.assertRaises(ValueError):
                validate(report, inputs)

    def test_instrumented_debug_nonfinite_and_wrong_option_samples_are_refused(self):
        for mutate in [lambda r: r.update(instrumented=True),
                       lambda r: r['features'].update(allocation_counts=True),
                       lambda r: r['build'].update(debug_assertions=True),
                       lambda r: r['rows'][0].update(seconds=float('nan')),
                       lambda r: r['rows'][0].update(seconds=0),
                       lambda r: r['rows'][0].update(option_bits=4)]:
            report = sample_report([item()])
            mutate(report)
            with self.assertRaises(ValueError):
                validate(report, [item()])

    def test_first_calls_are_separate_and_failures_disable_speedups(self):
        inputs = [item()]
        rows = []
        for variant, scale in [('before', 2), ('after', 1)]:
            for sample in sample_report(inputs)['rows']:
                rows.append(dict(sample, seconds=sample['seconds']*scale, variant=variant, process_round=0))
        result = summarize(rows, inputs, [1])[0]
        self.assertEqual(result['accepted_median_speedup'], 2)
        self.assertEqual(result['timing']['after']['repeated']['samples'], 2)
        self.assertEqual(result['timing']['after']['first_for_file']['median_seconds'], 10)
        rows[-1]['status'] = 'failed'
        self.assertIsNone(summarize(rows, inputs, [1])[0]['accepted_median_speedup'])

    def test_changed_output_requires_same_input_and_exact_approved_hash_pair(self):
        before = sample_report([item()])['rows'][0]
        after = dict(before, output_sha256='e'*64, cli_output_sha256='f'*64)
        self.assertEqual(approved_change(before, after, None), 'pending')
        approval = dict(kind='tovek-quality-gate-v1', status='passed', approved_outputs=[
            dict(decoded_input_sha256='b'*64, before_output_sha256='c'*64, after_output_sha256='e'*64)])
        self.assertEqual(approved_change(before, after, approval), 'quality_approved')
        approval['approved_outputs'][0]['decoded_input_sha256'] = '0'*64
        self.assertEqual(approved_change(before, after, approval), 'pending')
        approval['status'] = 'failed'
        with self.assertRaises(ValueError):
            approved_change(before, after, approval)

    def test_changed_text_is_measured_without_calling_it_an_accepted_gain(self):
        rows = []
        for variant in ('before', 'after'):
            for sample in sample_report([item()])['rows']:
                rows.append(dict(sample, variant=variant, process_round=0,
                                 output_sha256=('c' if variant == 'before' else 'e')*64,
                                 cli_output_sha256=('d' if variant == 'before' else 'f')*64))
        result = summarize(rows, [item()], [1])[0]
        self.assertEqual(result['quality'], 'pending')
        self.assertEqual(result['observed_median_speedup'], 1)
        self.assertIsNone(result['accepted_median_speedup'])

    def test_quality_attachment_reuses_only_the_complete_unchanged_raw_cohort(self):
        with tempfile.TemporaryDirectory() as directory:
            comparison = dict(kind='tovek-single-script-comparison-v1', complete=True,
                              protocol=dict(threads=[1], process_rounds=2, rounds=2, option_bits=8),
                              selected_inputs=[item()], manifest={'decode_key': 1}, manifest_sha256='f'*64,
                              tools={variant: {'sha256': 'e'*64} for variant in ('before', 'after')},
                              process_runs=[], rows=[])
            for round_ in range(2):
                for variant in ('before', 'after'):
                    raw = sample_report([item()])
                    path = pathlib.Path(directory)/f'{variant}-{round_}.json'
                    path.write_text(json.dumps(raw), encoding='utf-8')
                    comparison['process_runs'].append(dict(variant=variant, threads=1, process_round=round_,
                                                           timeout=False, exit_code=0, report=str(path), report_sha256=sha256(path)))
                    comparison['rows'].extend(dict(row, variant=variant, process_round=round_) for row in raw['rows'])
            quality = dict(kind='tovek-quality-gate-v1', status='passed', approved_outputs=[])
            self.assertTrue(review_comparison(comparison, quality)['performance_accepted'])
            path.write_text('{}', encoding='utf-8')
            with self.assertRaisesRegex(ValueError, 'changed since measurement'):
                review_comparison(comparison, quality)


if __name__ == '__main__':
    unittest.main()
