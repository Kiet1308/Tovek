import copy
import io
import json
import pathlib
import sys
import unittest
from unittest import mock

import dataflow_replay
import test_provenance_fixtures as fixture_reports
from roadmap_v2 import compiler_flags, sha256


class DataflowReplayTests(unittest.TestCase):
    def setUp(self):
        self.fixtures = fixture_reports.FixtureRefusalChecks(methodName='runTest')
        self.fixtures.setUp()
        self.addCleanup(self.fixtures.doCleanups)
        self.root = self.fixtures.root
        self.compiler = self.root / 'compiler'
        self.compiler.write_bytes(b'pinned compiler')
        self.counter = 0

    def prepare(self, public):
        report = self.fixtures.report(public)
        report['tools'] = {'compiler': {'path': str(self.compiler), 'sha256': sha256(self.compiler)}}
        report['compiler_commit_expected'] = 'a' * 40
        for row in report['rows' if public else 'cases']:
            source = self.root / row['file']
            directory = self.root / 'repo' if public else self.root / f"{row['case']}_O1_g1"
            directory.mkdir(exist_ok=True)
            (directory / (row['file'] if public else 'source.luau')).write_bytes(source.read_bytes())
            bytecode = directory / (row['file'] + '.luaubc' if public else 'input.luaubc')
            bytecode.write_bytes(b'input bytecode')
            row['input'] = str(bytecode)
            if row['status'] == 'passed':
                output = directory / (row['file'] + '.out.luau' if public else 'output.luau')
                output.write_text('return 7')
                row.update(output=str(output), output_sha256=sha256(output))
        return report

    def run_replay(self, report, public=True, lock=None, certificate='proved'):
        self.counter += 1
        source = self.root / f'input-report-{self.counter}.json'
        target = self.root / f'replay-{self.counter}.json'
        source.write_text(json.dumps(report))
        argv = ['dataflow_replay', '--public-report' if public else '--fixtures-report', str(source),
                '--report', str(target), '--workers', '1']
        if public:
            argv += ['--vendor', str(self.root)]
        if lock is not None:
            path = self.root / f'lock-{self.counter}.json'
            path.write_text(json.dumps(lock))
            argv += ['--proof-gaps', str(path)]
        with mock.patch.object(sys, 'argv', argv), mock.patch('sys.stderr', new_callable=io.StringIO), \
                mock.patch('builtins.print'), \
                mock.patch.object(dataflow_replay, 'checked', return_value=(b'input bytecode', 0)) as compile_, \
                mock.patch.object(dataflow_replay, 'parse_chunk', return_value=object()), \
                mock.patch.object(dataflow_replay, 'compare_dataflow',
                                  return_value={'status': certificate, 'model': 'test-model'}) as compare, \
                mock.patch.object(dataflow_replay, 'compare_graph', return_value={'status': 'proved'}):
            try:
                self.assertEqual(dataflow_replay.main(), 0)
            except SystemExit:
                compile_.assert_not_called()
                raise
            result = json.loads(target.read_text())
        return result, compile_.call_args_list, compare.call_count

    def lock(self, report):
        profiles = []
        for index, row in enumerate(report['rows']):
            profiles.append(dict(index=index, repo=row['repo'], file=row['file'], opt=row['opt'],
                                 source_sha256=row['source_sha256'], input_sha256=sha256(pathlib.Path(row['input'])),
                                 initial_status='unknown', initial_category='test',
                                 initial_output_sha256='c' * 64))
        return dict(schema_version=1, compiler=report['tools']['compiler'], debug=1,
                    compiler_flags=report['compiler_flags'], profiles=profiles,
                    total_profiles=len(profiles), initial_status_counts={'unknown': len(profiles)})

    def test_replays_only_output_pairs_and_verifies_refused_inputs_without_losing_coverage(self):
        for public in (False, True):
            with self.subTest(public=public):
                report = self.prepare(public)
                result, commands, comparisons = self.run_replay(report, public)
                self.assertEqual((result['summary']['files'], comparisons), (1, 1))
                self.assertEqual({k: result['input_coverage'][k] for k in ('total', 'audited', 'expected_refusal')},
                                 {'total': 2, 'audited': 1, 'expected_refusal': 1})
                self.assertEqual(result['input_coverage']['refusals'], [report['rows' if public else 'cases'][1]])
                refusal = result['refusal_input_checks'][0]
                self.assertEqual(refusal['status'], 'expected_refusal')
                self.assertEqual(refusal['input_sha256'], sha256(pathlib.Path(report['rows' if public else 'cases'][1]['input'])))
                self.assertNotIn('after', refusal)
                self.assertEqual(len(commands), 3)  # Two inputs, only one rebuilt output.

    def test_bad_reports_are_rejected_before_compilation(self):
        for mutate in (lambda r: r['rows'][1].update(status='failed'),
                       lambda r: r['rows'][1]['refusal'].update(stderr='not the reviewed diagnostic'),
                       lambda r: r['rows'].pop(),
                       lambda r: r['rows'].append(copy.deepcopy(r['rows'][0])),
                       lambda r: r.update(manifest_sha256='f' * 64)):
            report = self.prepare(True)
            mutate(report)
            with self.subTest(mutate=mutate), self.assertRaises(SystemExit) as raised:
                self.run_replay(report)
            self.assertEqual(raised.exception.code, 2)

    def test_changed_refused_input_or_source_is_not_skipped(self):
        for field in ('source', 'input'):
            report = self.prepare(True)
            row = report['rows'][1]
            path = self.root / 'repo' / row['file'] if field == 'source' else pathlib.Path(row['input'])
            path.write_bytes(b'tampered')
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'source changed|input recompile mismatch'):
                self.run_replay(report)

    def test_compiler_profile_is_replayed_and_proof_loss_still_fails(self):
        report = self.prepare(True)
        spec = pathlib.Path(report['manifest_path'])
        manifest = json.loads(spec.read_text())
        flags = compiler_flags(dataflow_replay.argparse.Namespace(bytecode_version=12))
        manifest['debug_level'] = 2
        profile = copy.deepcopy(report['rows'][1]['refusal']['profile'])
        profile.update(debug=2, bytecode_version=12, compiler_flags=flags)
        report['rows'][1]['refusal']['profile'] = profile
        manifest['sources'][0]['expected_refusal']['profiles'] = [profile]
        spec.write_text(json.dumps(manifest))
        report.update(manifest_sha256=sha256(spec), bytecode_version=12, compiler_flags=flags)
        for row in report['rows']:
            row['debug'] = 2
        result, commands, _ = self.run_replay(report)
        for command in commands:
            self.assertIn('-g2', command.args[0])
            self.assertEqual(command.args[0][4:-1], flags)
        self.assertEqual(result['input_coverage']['expected_refusal'], 1)

        report = self.prepare(True)
        report['rows'][0]['dataflow'] = {'status': 'proved'}
        for counters in (report['summary']['development'], report['source_groups']['order']):
            counters['dataflow'] = {'proved': 1, 'unavailable': 1}
        with self.assertRaisesRegex(ValueError, 'previous proof lost'):
            self.run_replay(report, certificate='unknown')

    def test_frozen_inventory_records_output_changes_and_refusals_explicitly(self):
        report = self.prepare(True)
        result, _, _ = self.run_replay(report, lock=self.lock(report))
        progress = result['proof_gaps']
        self.assertEqual(progress['total'], 2)
        self.assertEqual(progress['status'], {'proved': 1, 'expected_refusal': 1})
        self.assertEqual(progress['transitions'], {'unknown->proved': 1, 'unknown->expected_refusal': 1})
        self.assertEqual([row['index'] for row in progress['profiles']], [0, 1])
        self.assertTrue(all(row['output_changed'] for row in progress['profiles']))
        self.assertIsNone(progress['profiles'][1]['result'])

    def test_frozen_inventory_cannot_drop_duplicate_or_relabel_inputs(self):
        changes = [lambda m: m['profiles'][0].update(source_sha256='f' * 64),
                   lambda m: m['profiles'][0].update(input_sha256='f' * 64),
                   lambda m: m['profiles'][0].update(file='missing.luau'),
                   lambda m: m['profiles'].append(copy.deepcopy(m['profiles'][0])),
                   lambda m: m['profiles'][0].update(index=1),
                   lambda m: m['profiles'][0].update(initial_status='proved'),
                   lambda m: m['profiles'].pop(),
                   lambda m: m['profiles'][0].update(initial_status='different'),
                   lambda m: m.update(total_profiles=True),
                   lambda m: m.update(initial_status_counts={'different': 2}),
                   lambda m: m.update(profiles=[]),
                   lambda m: m.update(debug=0),
                   lambda m: m['compiler'].update(sha256='f' * 64)]
        report = self.prepare(True)
        for change in changes:
            lock = copy.deepcopy(self.lock(report))
            change(lock)
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.run_replay(report, lock=lock)


if __name__ == '__main__':
    unittest.main()
