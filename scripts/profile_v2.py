#!/usr/bin/env python3
"""Validate opt-in pass profiling, exact source bytes and deterministic counters.

Timing is diagnostic. Use benchmark_v2.py separately for uninstrumented speed
comparisons. Raw profiles are compressed losslessly for review/replay.
"""
import argparse
import collections
import gzip
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import time

from benchmark_v2 import tree_hash
from roadmap_v2 import sha256


TIMING_FIELDS = {'inclusive_ns', 'exclusive_ns', 'max_inclusive_ns'}
AST_PHASES = {'S_FACTOR_INITIAL', 'S_FACTOR_FIXEDPOINT', 'S_DEINLINE'}


def profile_key(row):
    return row['script'], -1 if row['prototype'] is None else row['prototype'], row['pass']


def validate(profile, expected_scripts):
    errors = []
    if profile['schema_version'] != 1 or profile['model'] != 'tovek-pass-thread-wall-v1':
        errors.append('unknown schema')
    rows = profile['rows']
    keys = [profile_key(r) for r in rows]
    if len(keys) != len(set(keys)) or len(rows) != profile['rows_count']:
        errors.append('duplicate/missing rows')
    if profile['dropped_records'] or profile['misnested_spans']:
        errors.append('incomplete or misnested profile')
    by_script = collections.defaultdict(dict)
    for row in rows:
        if row['script'] not in expected_scripts:
            errors.append('foreign script context: ' + row['script'])
        if not 0 <= row['exclusive_ns'] <= row['inclusive_ns'] or not 0 <= row['max_inclusive_ns'] <= row['inclusive_ns']:
            errors.append('invalid timing interval')
        if not 0 <= row['node_samples'] <= row['calls'] or row['calls'] == 0:
            errors.append('invalid sample count')
        if row['node_samples_incomplete']:
            errors.append('incomplete node census')
        cache = {k.removeprefix('ssa_fact_cache_'): v for k, v in row['counters'].items()
                 if k.startswith('ssa_fact_cache_')}
        if cache:
            required = {'hits', 'misses', 'uncached', 'invalidations', 'slots'}
            if not required <= cache.keys() or any(type(v) is not int or v < 0 for v in cache.values()):
                errors.append('invalid SSA cache counters')
            elif (cache['invalidations'] > cache['misses']
                  or cache['misses'] > cache['slots'] + cache['invalidations']
                  or (cache['hits'] and not cache['misses'])):
                errors.append('SSA cache accounting mismatch')
            if row['pass'] != 'F_SSA_INLINE' or row['prototype'] is None:
                errors.append('SSA cache counter context mismatch')
        if row['prototype'] is None:
            by_script[row['script']][row['pass']] = row
            if row['pass'] in AST_PHASES and row['node_samples'] != row['calls']:
                errors.append('missing measured AST sample')
        if row['pass'] == 'D_COLLECT_TARGETS':
            counters = row['counters']
            if counters.get('candidate_binders', 0) != counters.get('accepted_targets', 0) + sum(
                    n for k, n in counters.items() if k.startswith('reject_')):
                errors.append('candidate/refusal accounting mismatch')
    if set(by_script) != expected_scripts:
        errors.append('missing script context')
    for script, passes in by_script.items():
        # This harness is for successfully decompiled fixture/corpus chunks.
        required = AST_PHASES | {'DECOMPILE', 'D_COLLECT_TARGETS', 'D_WRITE_CENSUS'}
        if not required <= passes.keys():
            errors.append('missing required phases: ' + script)
            continue
        if passes['S_FACTOR_INITIAL']['calls'] != 1:
            errors.append('initial factoring count changed')
        if passes['S_DEINLINE']['calls'] != passes['S_FACTOR_FIXEDPOINT']['calls']:
            errors.append('factoring/deinline iteration mismatch')
        if passes['DECOMPILE']['counters'].get('deinline_factor_iterations') != passes['S_DEINLINE']['calls']:
            errors.append('outer fixed-point counter context mismatch')
        if passes['D_WRITE_CENSUS']['calls'] != passes['S_DEINLINE']['calls']:
            errors.append('write census invocation count changed')
        if passes['D_COLLECT_TARGETS']['calls'] != passes['S_DEINLINE']['counters'].get('iterations'):
            errors.append('target-collection iteration count changed')
    return sorted(set(errors))


def summarize(profile):
    phases = collections.defaultdict(collections.Counter)
    for row in profile['rows']:
        stats = phases[row['pass']]
        for field in ('calls', 'inclusive_ns', 'exclusive_ns', 'node_samples', 'node_samples_incomplete'):
            stats[field] += row[field]
        stats.update({'counter:' + k: v for k, v in row['counters'].items()})
        for when in ('before', 'after'):
            stats[f'statements_{when}'] += row[f'nodes_{when}_sum']['statements']
            stats[f'values_{when}'] += row[f'nodes_{when}_sum']['values']
    return {key: dict(value) for key, value in sorted(phases.items())}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('before', 'after', 'corpus', 'keep', 'report'):
        parser.add_argument('--' + name, required=True, type=pathlib.Path)
    parser.add_argument('--key', type=int, default=203)
    parser.add_argument('--lifter-arg', action='append', default=[])
    parser.add_argument('--threads', nargs='+', type=int, default=[1, 16])
    parser.add_argument('--timeout', type=float, default=600)
    args = parser.parse_args()
    for field in ('before', 'after', 'corpus'):
        setattr(args, field, getattr(args, field).resolve(strict=True))
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix='profile-', dir=args.keep)).resolve()
    env = {k: v for k, v in os.environ.items() if not k.startswith('MEDAL_')}
    samples, projections, profiles = [], [], []
    expected_hash = None
    expected_scripts = set()
    jobs = [('before', args.before, max(args.threads), False)] + [
        ('after', args.after, threads, profile) for threads in args.threads for profile in (False, True)]
    for label, binary, threads, profiling in jobs:
        name = f"{label}-t{threads}-{'profile' if profiling else 'plain'}"
        output = work / name
        raw_path = work / (name + '.json')
        current_env = dict(env)
        if profiling:
            current_env['MEDAL_PROFILE_JSON'] = str(raw_path)
        command = [str(binary), 'decompile-folder', str(args.corpus), str(output), '--key', str(args.key),
                   '--threads', str(threads), '--strict-no-synthetic-control', *args.lifter_arg]
        start = time.perf_counter()
        with (work / (name + '.log')).open('wb') as log:
            proc = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, env=current_env, timeout=args.timeout)
        elapsed = time.perf_counter() - start
        if proc.returncode:
            raise RuntimeError(f'{name} failed with exit {proc.returncode}')
        output_hash = tree_hash(output, '*.luau')
        if expected_hash is None:
            expected_hash = output_hash
            expected_scripts = {p.relative_to(output).with_suffix('.lua').as_posix()
                                for p in output.rglob('*.luau') if p.stat().st_size}
        if output_hash != expected_hash:
            raise RuntimeError('profile/binary/thread changed source: ' + name)
        samples.append({'mode': name, 'threads': threads, 'profile_enabled': profiling,
                        'seconds': elapsed, 'output_hash': output_hash[0], 'output_files': output_hash[1]})
        if profiling:
            data = raw_path.read_bytes()
            profile = json.loads(data)
            errors = validate(profile, expected_scripts)
            if errors:
                raise RuntimeError(f'{name}: {errors}')
            if profile['binary_sha256'] != sha256(binary):
                raise RuntimeError('profile binary hash mismatch')
            projection = [{k: v for k, v in row.items() if k not in TIMING_FIELDS}
                          for row in sorted(profile['rows'], key=profile_key)]
            projection_hash = hashlib.sha256(json.dumps(projection, sort_keys=True).encode()).hexdigest()
            projections.append(projection_hash)
            compressed = raw_path.with_suffix('.json.gz')
            compressed.write_bytes(gzip.compress(data, mtime=0))
            profiles.append({'mode': name, 'rows': len(profile['rows']), 'raw_sha256': sha256(raw_path),
                             'compressed': str(compressed), 'compressed_sha256': sha256(compressed),
                             'counter_projection_sha256': projection_hash, 'summary': summarize(profile)})
        print(f'{name}: {elapsed:.3f}s, source identical', flush=True)
    if len(set(projections)) != 1:
        raise RuntimeError('thread scheduling changed context, counters or node census')
    report = {'schema_version': 1, 'source_byte_identical': True, 'counters_deterministic': True,
              'input_hash': tree_hash(args.corpus, '*.lua')[0], 'scripts': len(expected_scripts),
              'lifter_args': args.lifter_arg, 'binaries': {name: {'path': str(getattr(args, name)), 'sha256': sha256(getattr(args, name))}
                           for name in ('before', 'after')}, 'samples': samples, 'profiles': profiles,
              'limitations': 'Single diagnostic runs include profiling and JSON export overhead. Counters/node census are compared without timing fields. These timings do not establish a performance improvement.'}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps({'scripts': report['scripts'], 'source_byte_identical': True, 'counters_deterministic': True}))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
