#!/usr/bin/env python3
"""Freeze inputs and compare fresh single-script API calls in alternating processes.

This runner never substitutes batch throughput, instrumented timings or cache
hits for script latency. Changed output is measured but requires a separate,
hash-bound quality report before its speedup is accepted.
"""
from __future__ import annotations

import argparse
import base64
import collections
import hashlib
import json
import math
import os
import pathlib
import platform
import statistics
import subprocess
import tempfile
import time

FEATURE_KEYS = {'allocation_counts', 'dhat_heap', 'byte_storage_trace', 'phase_allocation_trace'}

def digest(data):
    return hashlib.sha256(data).hexdigest()


def sha256(path):
    return digest(pathlib.Path(path).read_bytes())


def save(path, data):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')


def valid_hash(value):
    return isinstance(value, str) and len(value) == 64 and all(c in '0123456789abcdef' for c in value)


def relative_path(value):
    if not isinstance(value, str) or not value or any(c in value for c in ('\\', ':', '\0')):
        raise ValueError('invalid relative input path')
    if any(part in ('', '.', '..') for part in value.split('/')):
        raise ValueError('invalid relative input path')
    return pathlib.PurePosixPath(value)


def decode(data, encoding):
    if encoding == 'raw':
        return data
    if encoding != 'base64':
        raise ValueError('encoding must be raw or base64')
    compact = b''.join(b''.join(line.split()) for line in data.split(b'\n') if not line.startswith(b'--'))
    return base64.b64decode(compact, validate=True)


def load_manifest(path, root, group='all'):
    manifest = json.loads(pathlib.Path(path).read_text(encoding='utf-8'))
    if set(manifest) != {'schema_version', 'decode_key', 'scripts'} or manifest['schema_version'] != 1:
        raise ValueError('unsupported manifest')
    if type(manifest['decode_key']) is not int or not 0 <= manifest['decode_key'] <= 255:
        raise ValueError('invalid decode key')
    if not isinstance(manifest['scripts'], list) or not 1 <= len(manifest['scripts']) <= 10_000:
        raise ValueError('invalid input count')
    root = pathlib.Path(root).resolve(strict=True)
    seen, selected, total = set(), [], 0
    for item in manifest['scripts']:
        if (not isinstance(item, dict) or not {'path', 'encoding', 'input_sha256', 'groups'} <= item.keys()
                or item.keys() - {'path', 'encoding', 'input_sha256', 'groups', 'script_name', 'expected_output_sha256'}):
            raise ValueError('invalid script manifest fields')
        relative_path(item['path'])
        if item['path'] in seen or not valid_hash(item['input_sha256']):
            raise ValueError('duplicate input or invalid input hash')
        seen.add(item['path'])
        if item.get('expected_output_sha256') is not None and not valid_hash(item['expected_output_sha256']):
            raise ValueError('invalid expected output hash')
        if item.get('script_name') is not None and not isinstance(item['script_name'], str):
            raise ValueError('invalid script name')
        if not isinstance(item['groups'], list) or not all(isinstance(g, str) for g in item['groups']):
            raise ValueError('invalid groups')
        if group not in item['groups']:
            continue
        source = (root / item['path']).resolve(strict=True)
        if not source.is_relative_to(root) or source.stat().st_size > 16 * 1024 * 1024:
            raise ValueError('input escapes root or exceeds byte limit')
        saved = source.read_bytes()
        if digest(saved) != item['input_sha256']:
            raise ValueError('input hash mismatch: ' + item['path'])
        raw = decode(saved, item['encoding'])
        total += len(raw)
        if total > 128 * 1024 * 1024:
            raise ValueError('corpus byte limit exceeded')
        selected.append(dict(item, decoded_input_sha256=digest(raw), decoded_input_bytes=len(raw)))
    if not selected:
        raise ValueError('empty selected group')
    return manifest, selected


def pin(root, patterns, encoding, key, names=True, expected_root=None):
    root = pathlib.Path(root).resolve(strict=True)
    paths = sorted({path for pattern in patterns for path in root.rglob(pattern) if path.is_file()})
    if not paths:
        raise ValueError('no selected inputs')
    rows, sizes = [], []
    for path in paths:
        relative = path.relative_to(root).as_posix()
        relative_path(relative)
        if not path.resolve().is_relative_to(root) or path.stat().st_size > 16 * 1024 * 1024:
            raise ValueError('input escapes root or exceeds byte limit')
        data = path.read_bytes()
        size = len(decode(data, encoding))
        row = dict(path=relative, encoding=encoding, input_sha256=digest(data),
                   script_name=relative if names else None, groups=['all'])
        if expected_root is not None:
            expected = pathlib.Path(expected_root) / (relative + '.luau')
            row['expected_output_sha256'] = sha256(expected)
        rows.append(row)
        sizes.append(size)
    ordered = sorted(sizes)
    small, large = ordered[(len(ordered)-1)//2], ordered[math.floor((len(ordered)-1)*.9)]
    for row, size in zip(rows, sizes):
        if size <= small:
            row['groups'].append('small')
        if size >= large:
            row['groups'].append('large')
    return dict(schema_version=1, decode_key=key, scripts=rows)


def validate_samples(report, selected, *, executable_hash, manifest_hash, threads, rounds, option_bits, decode_key):
    """Refuse missing/fabricated coverage; failed calls remain real observations."""
    if (report.get('schema_version') != 1 or report.get('kind') != 'tovek-single-script-samples-v1'
            or report.get('api') != 'try_decompile_bytecode_with_options' or report.get('complete') is not True):
        raise ValueError('wrong or incomplete single-script report')
    for key, expected in [('executable_sha256', executable_hash), ('manifest_sha256', manifest_hash),
                          ('threads', threads), ('rounds', rounds), ('option_bits', option_bits),
                          ('decode_key', decode_key), ('scripts', len(selected))]:
        if report.get(key) != expected:
            raise ValueError('sample context differs: ' + key)
    if (report.get('instrumented') is not False or not isinstance(report.get('features'), dict)
            or set(report['features']) != FEATURE_KEYS or any(value is not False for value in report['features'].values())):
        raise ValueError('instrumented or unspecified feature build cannot supply speed evidence')
    build = report.get('build', {})
    if build.get('debug_assertions') is not False or build.get('panic_unwind') is not True:
        raise ValueError('ordinary optimized unwind build required')
    inputs = {row['path']: row for row in selected}
    rows = report.get('rows', [])
    if len(rows) != len(inputs) * (rounds + 1):
        raise ValueError('missing or extra sample rows')
    seen, invocations, process_first = set(), set(), 0
    for row in rows:
        key = row.get('path'), row.get('round')
        if (key in seen or key[0] not in inputs or type(key[1]) is not int or not 0 <= key[1] <= rounds):
            raise ValueError('duplicate or foreign sample')
        seen.add(key)
        item = inputs[key[0]]
        for field in ('input_sha256', 'decoded_input_sha256', 'decoded_input_bytes'):
            if row.get(field) != item[field]:
                raise ValueError('sample input identity changed')
        if row.get('script_name') != item.get('script_name') or row.get('threads') != threads or row.get('option_bits') != option_bits:
            raise ValueError('sample options/name context changed')
        if row.get('first_for_file') is not (row['round'] == 0):
            raise ValueError('first-for-file label changed')
        invocation = row.get('invocation')
        if type(invocation) is not int or invocation in invocations or not 0 <= invocation < len(rows):
            raise ValueError('invalid invocation order')
        invocations.add(invocation)
        if row.get('first_in_process') is not (invocation == 0):
            raise ValueError('first-process label changed')
        process_first += row['first_in_process']
        seconds = row.get('seconds')
        if type(seconds) not in (int, float) or not math.isfinite(seconds) or seconds <= 0:
            raise ValueError('invalid wall time')
        if row.get('status') == 'passed':
            if not valid_hash(row.get('output_sha256')) or not valid_hash(row.get('cli_output_sha256')):
                raise ValueError('successful sample lacks output identity')
            if type(row.get('output_bytes')) is not int or row['output_bytes'] <= 0:
                raise ValueError('successful sample lacks source bytes')
        elif not isinstance(row.get('status'), str) or not row['status']:
            raise ValueError('sample status unavailable')
    if process_first != 1 or report.get('valid') != all(row['status'] == 'passed' for row in rows):
        raise ValueError('inconsistent sample validity')
    return rows


def timing(values):
    if not values:
        return None
    ordered = sorted(values)
    return dict(samples=len(values), mean_seconds=statistics.mean(values), median_seconds=statistics.median(values),
                min_seconds=ordered[0], max_seconds=ordered[-1],
                p95_nearest_rank_seconds=ordered[math.ceil(.95*len(values))-1])


def approved_change(before, after, quality_report):
    if before['output_sha256'] == after['output_sha256']:
        return 'identical'
    if quality_report is None:
        return 'pending'
    if quality_report.get('kind') != 'tovek-quality-gate-v1' or quality_report.get('status') != 'passed':
        raise ValueError('quality report is not a passed regression gate')
    before_hashes = {before['output_sha256'], before['cli_output_sha256']}
    after_hashes = {after['output_sha256'], after['cli_output_sha256']}
    for approval in quality_report.get('approved_outputs', []):
        if (approval.get('decoded_input_sha256') == before['decoded_input_sha256']
                and approval.get('before_output_sha256') in before_hashes
                and approval.get('after_output_sha256') in after_hashes):
            return 'quality_approved'
    return 'pending'


def summarize(samples, selected, thread_counts, quality_report=None):
    result = []
    by_key = collections.defaultdict(list)
    identities = collections.defaultdict(set)
    for sample in samples:
        by_key[sample['variant'], sample['path'], sample['threads']].append(sample)
        if sample['status'] == 'passed':
            identities[sample['variant'], sample['path']].add(sample['output_sha256'])
    for item in selected:
        for threads in thread_counts:
            pair = {variant: by_key[variant, item['path'], threads] for variant in ('before', 'after')}
            valid = all(rows and all(row['status'] == 'passed' for row in rows)
                        and len(identities[variant, item['path']]) == 1 for variant, rows in pair.items())
            measured = {variant: dict(repeated=timing([r['seconds'] for r in rows if not r['first_for_file']]),
                                      first_for_file=timing([r['seconds'] for r in rows if r['first_for_file']]),
                                      first_in_process=timing([r['seconds'] for r in rows if r['first_in_process']]))
                        for variant, rows in pair.items()}
            quality = approved_change(pair['before'][0], pair['after'][0], quality_report) if valid else 'failed'
            ratio = (measured['before']['repeated']['median_seconds'] / measured['after']['repeated']['median_seconds']) if valid else None
            result.append(dict(path=item['path'], decoded_input_sha256=item['decoded_input_sha256'],
                               groups=item['groups'], threads=threads, valid=valid, quality=quality, timing=measured,
                               observed_median_speedup=ratio,
                               accepted_median_speedup=ratio if quality in ('identical', 'quality_approved') else None))
    return result


def review_comparison(comparison, quality_report):
    """Attach a quality decision to the same archived cohort without retiming."""
    if comparison.get('kind') != 'tovek-single-script-comparison-v1' or comparison.get('complete') is not True:
        raise ValueError('a complete comparison is required')
    protocol = comparison['protocol']
    planned = {(variant, threads, process_round) for variant in ('before', 'after')
               for threads in protocol['threads'] for process_round in range(protocol['process_rounds'])}
    seen, samples = set(), []
    for run in comparison['process_runs']:
        key = run['variant'], run['threads'], run['process_round']
        if key not in planned or key in seen or run['timeout']:
            raise ValueError('incomplete or duplicate process cohort')
        seen.add(key)
        path = pathlib.Path(run['report'])
        if sha256(path) != run['report_sha256']:
            raise ValueError('raw sample report changed since measurement')
        raw = json.loads(path.read_text(encoding='utf-8'))
        rows = validate_samples(raw, comparison['selected_inputs'],
                                executable_hash=comparison['tools'][run['variant']]['sha256'],
                                manifest_hash=comparison['manifest_sha256'], threads=run['threads'],
                                rounds=protocol['rounds'], option_bits=protocol['option_bits'],
                                decode_key=comparison['manifest']['decode_key'])
        if (run['exit_code'] == 0) != raw['valid']:
            raise ValueError('raw process validity changed')
        samples.extend(dict(row, variant=run['variant'], process_round=run['process_round']) for row in rows)
    if seen != planned or samples != comparison['rows']:
        raise ValueError('recorded observations differ from the complete original cohort')
    result = dict(comparison)
    result['summary'] = summarize(samples, comparison['selected_inputs'], protocol['threads'], quality_report)
    valid = all(row['valid'] for row in result['summary'])
    approved = all(row['quality'] in ('identical', 'quality_approved') for row in result['summary'])
    result['status'] = 'failed' if not valid else 'passed' if approved else 'measured_quality_pending'
    result['performance_accepted'] = valid and approved
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='mode', required=True)
    make = commands.add_parser('pin')
    make.add_argument('--input-root', required=True, type=pathlib.Path)
    make.add_argument('--pattern', action='append', required=True)
    make.add_argument('--encoding', choices=('raw', 'base64'), required=True)
    make.add_argument('--decode-key', type=int, default=1)
    make.add_argument('--script-name-mode', choices=('relative', 'none'), default='relative')
    make.add_argument('--expected-root', type=pathlib.Path)
    make.add_argument('--manifest', required=True, type=pathlib.Path)
    run = commands.add_parser('run')
    for name in ('before', 'after', 'manifest', 'input-root', 'keep', 'report'):
        run.add_argument('--'+name, type=pathlib.Path, required=True)
    run.add_argument('--quality-report', type=pathlib.Path)
    run.add_argument('--require-quality', action='store_true')
    run.add_argument('--group', default='all')
    run.add_argument('--threads', type=int, nargs='+', default=[1])
    run.add_argument('--rounds', type=int, default=7)
    run.add_argument('--process-rounds', type=int, default=4)
    run.add_argument('--option-bits', type=int, default=8)
    run.add_argument('--timeout', type=float, default=600)
    run.add_argument('--cpus', type=int, nargs='+', help='Linux CPU affinity inherited by both variants; choose equivalent cores explicitly')
    run.add_argument('--before-revision')
    run.add_argument('--after-revision')
    review = commands.add_parser('review', help='attach a quality gate to an archived cohort without rerunning timing')
    for name in ('comparison', 'quality-report', 'report'):
        review.add_argument('--'+name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    if args.mode == 'pin':
        if not 0 <= args.decode_key <= 255:
            parser.error('decode key must be a byte')
        manifest = pin(args.input_root, args.pattern, args.encoding, args.decode_key,
                       args.script_name_mode == 'relative', args.expected_root)
        save(args.manifest, manifest)
        load_manifest(args.manifest, args.input_root)
        print(json.dumps(dict(scripts=len(manifest['scripts']), manifest_sha256=sha256(args.manifest))))
        return 0
    if args.mode == 'review':
        result = review_comparison(json.loads(args.comparison.read_text(encoding='utf-8')),
                                   json.loads(args.quality_report.read_text(encoding='utf-8')))
        result['quality_report_sha256'] = sha256(args.quality_report)
        result['original_comparison_sha256'] = sha256(args.comparison)
        save(args.report, result)
        print(json.dumps(dict(status=result['status'], performance_accepted=result['performance_accepted'])))
        return int(not result['performance_accepted'])
    if (not 1 <= args.rounds <= 1000 or not 2 <= args.process_rounds <= 100
            or args.process_rounds % 2 or any(not 1 <= t <= 64 for t in args.threads)
            or len(set(args.threads)) != len(args.threads) or args.timeout <= 0):
        parser.error('use positive threads, 1..1000 calls, and an even 2..100 process rounds')
    original_affinity = None
    if args.cpus is not None:
        if not hasattr(os, 'sched_setaffinity'):
            parser.error('--cpus requires Linux sched_setaffinity; pin the parent externally on other hosts')
        original_affinity = os.sched_getaffinity(0)
        if not args.cpus or not set(args.cpus) <= original_affinity:
            parser.error('requested CPUs are not available to this process')
        os.sched_setaffinity(0, args.cpus)
    try:
        manifest, selected = load_manifest(args.manifest, args.input_root, args.group)
        binaries = {variant: getattr(args, variant).resolve(strict=True) for variant in ('before', 'after')}
        pins = {variant: sha256(path) for variant, path in binaries.items()}
        manifest_hash = sha256(args.manifest)
        args.keep.mkdir(parents=True, exist_ok=True)
        work = pathlib.Path(tempfile.mkdtemp(prefix='single-', dir=args.keep)).resolve()
        env = {key: value for key, value in os.environ.items()
               if not key.upper().startswith('MEDAL_') and key.upper() != 'DEINLINE_ANCHOR_TRACE'}
        quality = json.loads(args.quality_report.read_text(encoding='utf-8')) if args.quality_report else None
        report = dict(schema_version=1, kind='tovek-single-script-comparison-v1', complete=False,
                      manifest_sha256=manifest_hash, manifest=manifest, selected_inputs=selected,
                      protocol=dict(threads=args.threads, rounds=args.rounds, process_rounds=args.process_rounds,
                                    option_bits=args.option_bits),
                      tools={variant: dict(path=str(path), sha256=pins[variant], revision=getattr(args, variant+'_revision'))
                             for variant, path in binaries.items()},
                      quality_report_sha256=sha256(args.quality_report) if args.quality_report else None,
                      system=dict(platform=platform.platform(), logical_processors=os.cpu_count(),
                                  cpu_affinity=sorted(os.sched_getaffinity(0)) if hasattr(os, 'sched_getaffinity') else None),
                      work=str(work), process_runs=[], rows=[], summary=[],
                      contract='Sequential AB/BA independent processes. Each API invocation runs a single uncached script. '
                               'Warm hardware caches are allowed; decompiled-result caches are not called. First-for-file and '
                               'first-in-process samples are separate. All attempts remain in raw rows; any failure disables '
                               'accepted speedups. p95 with few samples is an order statistic, not a service-tail estimate. '
                               'Build flags/toolchain must be matched independently; executable metadata cannot prove LTO settings.')
        try:
            for process_round in range(args.process_rounds):
                threads_order = args.threads if process_round % 2 == 0 else list(reversed(args.threads))
                for threads in threads_order:
                    for variant in ('before', 'after') if process_round % 2 == 0 else ('after', 'before'):
                        stem = f'{process_round}-{threads}-{variant}'
                        path = work / (stem+'.json')
                        command = [str(binaries[variant]), '--manifest', str(args.manifest.resolve()),
                                   '--input-root', str(args.input_root.resolve()), '--report', str(path),
                                   '--group', args.group, '--threads', str(threads), '--rounds', str(args.rounds),
                                   '--option-bits', str(args.option_bits), '--start-index', str(process_round % len(selected))]
                        if process_round % 2:
                            command.append('--reverse')
                        if process_round == 0:
                            command.extend(['--output-root', str(work / (stem+'-sources'))])
                        started = time.perf_counter()
                        with (work / (stem+'.log')).open('wb') as log:
                            try:
                                process = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, env=env, timeout=args.timeout)
                                exit_code, timeout = process.returncode, False
                            except subprocess.TimeoutExpired:
                                exit_code, timeout = None, True
                        run_row = dict(variant=variant, threads=threads, process_round=process_round, command=command,
                                       exit_code=exit_code, timeout=timeout, report=str(path),
                                       process_seconds_not_api_latency=time.perf_counter()-started)
                        report['process_runs'].append(run_row)
                        save(args.report, report)
                        if timeout or not path.is_file():
                            raise ValueError('timed out or missing report; failed attempt retained: '+stem)
                        raw_report = json.loads(path.read_text(encoding='utf-8'))
                        rows = validate_samples(raw_report, selected, executable_hash=pins[variant], manifest_hash=manifest_hash,
                                                threads=threads, rounds=args.rounds, option_bits=args.option_bits,
                                                decode_key=manifest['decode_key'])
                        run_row.update(report_sha256=sha256(path), build=raw_report['build'], features=raw_report['features'])
                        report['rows'].extend(dict(row, variant=variant, process_round=process_round) for row in rows)
                        if exit_code not in (0, 1) or (exit_code == 0) != raw_report['valid']:
                            raise ValueError('process exit and report validity disagree')
                        print(json.dumps(dict(variant=variant, threads=threads, process_round=process_round,
                                              passed=sum(r['status'] == 'passed' for r in rows), samples=len(rows))), flush=True)
                        save(args.report, report)
            if any(sha256(path) != pins[variant] for variant, path in binaries.items()) or sha256(args.manifest) != manifest_hash:
                raise ValueError('binary or manifest changed during measurement')
            # Check input drift again after all timed processes.
            load_manifest(args.manifest, args.input_root, args.group)
            report['summary'] = summarize(report['rows'], selected, args.threads, quality)
            report['complete'] = True
            valid = all(row['valid'] for row in report['summary'])
            quality_passed = all(row['quality'] in ('identical', 'quality_approved') for row in report['summary'])
            report['status'] = 'failed' if not valid else 'passed' if quality_passed else 'measured_quality_pending'
            report['performance_accepted'] = valid and quality_passed
        except (ValueError, OSError, KeyError, TypeError) as error:
            report.update(status='failed', error=str(error), performance_accepted=False)
        save(args.report, report)
        print(json.dumps(dict(status=report['status'], report=str(args.report), performance_accepted=report['performance_accepted'])))
        return int(report['status'] == 'failed' or (args.require_quality and not report['performance_accepted']))
    finally:
        if original_affinity is not None:
            os.sched_setaffinity(0, original_affinity)


if __name__ == '__main__':
    raise SystemExit(main())
