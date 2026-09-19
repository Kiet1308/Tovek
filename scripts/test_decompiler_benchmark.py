import base64
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

from benchmark_adapters import Expert, classify_body, digest
from benchmark_report import interval, paired, summarize, report
from decompiler_benchmark import same_runtime


class Response(io.BytesIO):
    status = 200
    headers = {'Content-Type': 'text/plain', 'Date': 'test date'}


class BenchmarkTests(unittest.TestCase):
    def test_http_200_unsupported_is_not_success(self):
        body = b'-- https://lua.expert/\r\nBytecode version (12) unhandled\r\n'
        self.assertEqual(classify_body(body), 'unsupported_version')
        self.assertEqual(classify_body(b'  \n'), 'empty_response')
        self.assertEqual(classify_body(b'return "Bytecode version (12) unhandled"'), 'output')

    def test_api_receives_exact_bytes_and_preserves_response(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'input.luaubc'
            raw = b'\x09\x03\x00\xff'; path.write_bytes(raw)
            body = b'-- watermark\r\nreturn "<script>"\r\n'
            adapter = Expert()
            with patch.object(adapter.opener, 'open', return_value=Response(body)) as opened:
                receipt, actual = adapter.invoke(path)
            payload = json.loads(opened.call_args.args[0].data)
            self.assertEqual(base64.b64decode(payload['script']), raw)
            self.assertEqual(actual, body)
            self.assertEqual(receipt['output_sha256'], digest(body))
            self.assertEqual(receipt['attempts'][0]['headers']['content-type'], 'text/plain')

    def test_retry_retains_rate_limit_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'input'; path.write_bytes(b'bytes')
            adapter = Expert()
            error = urllib.error.HTTPError('https://api.lua.expert/decompile', 429,
                                          'limited', {'Retry-After': '2'}, io.BytesIO(b'limited'))
            with patch.object(adapter.opener, 'open', side_effect=[error, Response(b'return 1')]), \
                 patch('benchmark_adapters.time.sleep') as sleep:
                receipt, body = adapter.invoke(path)
            self.assertEqual([a['http_status'] for a in receipt['attempts']], [429, 200])
            self.assertEqual(body, b'return 1')
            self.assertTrue(any(c.args[0] >= 2 for c in sleep.call_args_list))

    def test_timing_disables_retries(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'input'; path.write_bytes(b'bytes')
            adapter = Expert()
            with patch.object(adapter.opener, 'open', side_effect=TimeoutError('offline')) as opened:
                receipt, _ = adapter.invoke(path, retry=False)
            self.assertEqual(opened.call_count, 1)
            self.assertEqual(receipt['status'], 'transport_error')

    def test_rate_cannot_exceed_conservative_cap(self):
        for rate in (0, 241, -1):
            with self.assertRaises(ValueError): Expert(rate)

    def test_observations_preserve_nil_arity_and_failure(self):
        reference = dict(exit=0, stdout='3\t5\tnil\t9\n', stderr='')
        self.assertTrue(same_runtime(reference, dict(reference)))
        self.assertFalse(same_runtime(reference, dict(reference, stdout='2\t5\t9\n')))
        self.assertFalse(same_runtime(reference, dict(reference, exit=1)))

    @staticmethod
    def row(cid, provider, cluster, success, status=None):
        return dict(id=cid, provider=provider, cluster=cluster, program=cluster,
                    suite='generated', version=9, runtime_eligible=True,
                    compile=success, runtime_pass=success,
                    status=status or ('runtime_pass' if success else 'compile_failed'))

    def test_failures_stay_in_denominator_and_unknown_is_not_zero_structure(self):
        rows = [self.row('a', 'tovek-v2', 'seed', True),
                self.row('b', 'tovek-v2', 'seed', False)]
        rows[0]['fidelity'] = dict(status='measured', raw_structural_ratio=.8)
        group = summarize(rows)[0]
        self.assertEqual((group['profiles'], group['compiled'], group['runtime_pass']), (2,1,1))
        self.assertEqual((group['fidelity_measured'], group['fidelity_unknown']), (1,1))
        self.assertEqual(group['mean_structure'], .8)

    def test_bootstrap_clusters_do_not_count_profile_variants_as_programs(self):
        rows = []
        for i in range(18):
            rows += [self.row(str(i), 'tovek-v2', 'seed-a', True),
                     self.row(str(i), 'lua-expert', 'seed-a', False)]
        rows += [self.row('last', 'tovek-v2', 'seed-b', False),
                 self.row('last', 'lua-expert', 'seed-b', True)]
        result = paired(rows)[0]
        self.assertEqual(result['mean'], 0)
        self.assertEqual(result['clusters'], 2)
        self.assertEqual(result['paired_profiles'], 19)
        self.assertEqual(paired(list(reversed(rows))), paired(rows))

    def test_unsupported_versions_have_no_paired_score(self):
        rows = [self.row('a', 'tovek-v2', 'seed', True),
                self.row('a', 'lua-expert', 'seed', False, 'not_run_unsupported_version')]
        self.assertEqual(paired(rows), [])
        self.assertIsNone(interval([]))

    def test_report_keeps_source_in_inert_json_and_rejects_mismatched_plan(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def save(name, value):
                path = root/name; path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(json.dumps(value), encoding='utf-8')
            hostile = 'return "</script><script>alert(1)</script>"'
            (root/'source.luau').write_text(hostile, encoding='utf-8')
            (root/'output.luau').write_text(hostile, encoding='utf-8')
            save('plan.json', dict(programs=[dict(id='seed', source='source.luau', name='safe')],
                                  mutation_controls=[],generated_clusters=1,created_utc='test'))
            plan_hash = digest((root/'plan.json').read_bytes())
            row = self.row('a', 'tovek-v2', 'seed', True)
            row.update(output='output.luau',opt=0,debug=2)
            save('results.json',dict(plan_sha256=plan_hash,rows=[row]))
            save('canary-results.json',[])
            save('providers/tovek-v2/identity.json',{})
            save('providers/tovek-v2/capabilities.json',{})
            with contextlib.redirect_stdout(io.StringIO()): report(root)
            page = (root/'index.html').read_text(encoding='utf-8')
            self.assertNotIn('</script><script>alert', page)
            data = page.split('<script id="data" type="application/json">')[1].split('</script>')[0]
            self.assertEqual(json.loads(data)['texts']['output.luau'], hostile)
            save('results.json',dict(plan_sha256='different',rows=[row]))
            with self.assertRaisesRegex(ValueError, 'another plan'): report(root)


if __name__ == '__main__': unittest.main()
