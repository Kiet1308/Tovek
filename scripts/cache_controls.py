#!/usr/bin/env python3
"""Exercise executable-context folder-cache invalidation and safe fallbacks."""
import argparse
import base64
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile

from benchmark_v2 import tree_hash
from roadmap_v2 import ROOT, sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('lifter', 'compiler', 'keep', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.lifter = args.lifter.resolve(strict=True)
    args.compiler = args.compiler.resolve(strict=True)
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix='cache-controls-', dir=args.keep)).resolve()
    inputs = work / 'input'
    cache = work / 'cache'
    source = ROOT / 'luau-lifter/tests/fixtures/cache_context.luau'
    original = subprocess.check_output([str(args.compiler), '--binary', '-O2', '-g1', '--fflags=false', str(source)], timeout=30)
    names = ['A/Widget.lua', 'B/Gadget.lua', 'C/Widget/init.lua']
    for name in names:
        path = inputs / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(base64.b64encode(original))
    rows = []

    def run(label, *, cached=True, flags=(), expected_exit=0, env_extra=None):
        output = work / label
        command = [str(args.lifter), 'decompile-folder', str(inputs), str(output), '--key', '1',
                   '--threads', '1', '--strict-no-synthetic-control', *flags]
        if cached:
            command += ['--cache-dir', str(cache)]
        env = {k: v for k, v in os.environ.items() if not k.startswith('MEDAL_') and k != 'DEINLINE_ANCHOR_TRACE'}
        env.update(env_extra or {})
        result = subprocess.run(command, capture_output=True, timeout=120, env=env)
        (work / (label + '.log')).write_bytes(result.stdout + result.stderr)
        stats = [json.loads(line.removeprefix('TOVEK_CACHE ')) for line in result.stderr.decode(errors='replace').splitlines()
                 if line.startswith('TOVEK_CACHE ')]
        assert result.returncode == expected_exit, (label, result.stderr.decode(errors='replace')[-1500:])
        if cached and not env_extra:
            assert len(stats) == 1 and stats[0]['io_errors'] == 0, (label, stats)
        row = dict(case=label, status='passed', exit_code=result.returncode,
                   source_tree_hash=tree_hash(output, '*.luau')[0], cache=stats[0] if stats else None)
        rows.append(row)
        return row

    baseline = run('baseline', cached=False)
    assert (work / 'baseline/A/Widget.luau').read_bytes() != (work / 'baseline/B/Gadget.luau').read_bytes()
    cold = run('cold')
    assert cold['source_tree_hash'] == baseline['source_tree_hash']
    assert (cold['cache']['misses'], cold['cache']['hits']) == (2, 1)
    warm = run('warm')
    assert warm['source_tree_hash'] == baseline['source_tree_hash']
    assert (warm['cache']['hits'], warm['cache']['misses']) == (3, 0)
    changed_options = run('changed_options', flags=['--dont-reuse-var'])
    option_baseline = run('option_baseline', cached=False, flags=['--dont-reuse-var'])
    assert changed_options['source_tree_hash'] == option_baseline['source_tree_hash']
    assert changed_options['cache']['misses'] == 2
    assert run('old_options_again')['cache']['hits'] == 3

    # Wrapper comments are excluded by the native decoder, so the exact decoded
    # bytes and output context stay equal and must still hit.
    (inputs / names[0]).write_bytes(b'-- exporter comment\n' + base64.b64encode(original))
    comment = run('wrapper_comment')
    assert comment['cache']['hits'] == 3 and comment['source_tree_hash'] == baseline['source_tree_hash']

    changed_source = work / 'changed.luau'
    changed_source.write_text(source.read_text(encoding='utf-8').replace('count = 0', 'count = 17'), encoding='utf-8', newline='\n')
    changed = subprocess.check_output([str(args.compiler), '--binary', '-O2', '-g1', '--fflags=false', str(changed_source)], timeout=30)
    (inputs / names[0]).write_bytes(base64.b64encode(changed))
    edited = run('edited_bytecode')
    edited_baseline = run('edited_baseline', cached=False)
    assert edited['cache']['misses'] == 1 and edited['source_tree_hash'] == edited_baseline['source_tree_hash']
    assert edited['source_tree_hash'] != baseline['source_tree_hash']
    (inputs / names[0]).write_bytes(base64.b64encode(original))

    # Corrupt a payload while retaining its stored digest. A valid checksum is
    # an integrity check, not authentication of a cache populated by a stranger.
    candidates = []
    for path in cache.glob('*.json'):
        entry = json.loads(path.read_text(encoding='utf-8'))
        if entry['key']['option_bits'] == 8 and entry['key']['bytecode_sha256'] == hashlib.sha256(original).hexdigest():
            candidates.append((path, entry))
    assert candidates
    path, entry = candidates[0]
    entry['artifact']['source'] = 'return "corrupt"'
    path.write_text(json.dumps(entry), encoding='utf-8', newline='\n')
    repaired = run('corrupt_payload')
    assert repaired['cache']['corrupt'] > 0 and repaired['source_tree_hash'] == baseline['source_tree_hash']
    assert run('repaired_warm')['cache']['hits'] == 3

    # An actual decode failure must be recomputed and rejected on every run.
    bad = inputs / 'Bad.lua'
    bad.write_bytes(base64.b64encode(b'\xffinvalid-bytecode'))
    count = len(list(cache.glob('*.json')))
    for label in ['failure_first', 'failure_again']:
        failure = run(label, expected_exit=1)
        assert failure['cache']['misses'] == 1
        assert len(list(cache.glob('*.json'))) == count
    bad.unlink()
    diagnostic = run('diagnostic_bypass', env_extra={'MEDAL_PROF': '1'})
    assert diagnostic['cache'] is None and diagnostic['source_tree_hash'] == baseline['source_tree_hash']

    report = dict(schema_version=1, lifter_sha256=sha256(args.lifter), compiler_sha256=sha256(args.compiler),
                  fixture_sha256=sha256(source), compiler_flags=['--binary', '-O2', '-g1', '--fflags=false'],
                  work=str(work), summary=dict(total=len(rows), passed=len(rows)), rows=rows,
                  contract='Actual CLI counterexamples and invalidation checks. Artifact checksums detect accidental corruption; cache files are trusted local build products, not signed semantic certificates.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary']))


if __name__ == '__main__':
    main()
