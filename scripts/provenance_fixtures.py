#!/usr/bin/env python3
"""Replay V2 fixture bytecode in analysis modes and verify lineage determinism."""
import argparse
import base64
import json
import pathlib
import sys
import tempfile

from provenance_audit import manifest, sidecar
from roadmap_v2 import ROOT, checked, run, sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    inputs_group = parser.add_mutually_exclusive_group(required=True)
    inputs_group.add_argument('--fixtures-report', type=pathlib.Path)
    inputs_group.add_argument('--public-report', type=pathlib.Path)
    parser.add_argument('--ast', type=pathlib.Path, help='also check emitted token binding identity with the pinned parser')
    parser.add_argument('--cache', action='store_true', help='also compare cold/warm artifact cache against uncached source and sidecars')
    parser.add_argument('--compact-annotations', action='store_true', help='also verify comment-only compact display, call spans and cache option isolation')
    for name in ('lifter', 'report', 'keep'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    if args.compact_annotations and not args.ast:
        parser.error('compact annotation checks require --ast')
    args.lifter = args.lifter.resolve(strict=True)
    input_report = args.fixtures_report or args.public_report
    fixtures = json.loads(input_report.read_text(encoding='utf-8'))
    rows = fixtures['cases'] if args.fixtures_report else fixtures['rows']
    if not rows or any(r['status'] != 'passed' for r in rows):
        parser.error('source fixture report must pass first')
    if sha256(args.lifter) != fixtures['tools']['lifter']['sha256']:
        parser.error('fixture and lineage binaries must be identical')
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix='lineage-', dir=args.keep)).resolve()
    inputs = work / 'input'
    inputs.mkdir()
    for row in rows:
        if args.fixtures_report:
            name = f"{row['case']}_O{row['opt']}_g{row['debug']}"
            raw = (pathlib.Path(fixtures['work']) / name / 'input.luaubc').read_bytes()
            target = inputs / (name + '.lua')
        else:
            raw = pathlib.Path(row['output'].removesuffix('.out.luau') + '.luaubc').read_bytes()
            target = inputs / row['repo'] / f"O{row['opt']}" / pathlib.Path(row['file']).with_suffix('.lua')
        if target.exists() or not target.resolve().is_relative_to(inputs):
            raise ValueError('duplicate or escaping fixture path')
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(base64.b64encode(raw))
    measurements = []
    for label, threads, flag in [('analysis', 1, '--emit-upvalue-analysis'),
                                 ('trace1', 1, '--emit-binding-provenance'),
                                 ('trace4', 4, '--emit-binding-provenance')]:
        output = work / label
        stdout, elapsed = checked([args.lifter, 'decompile-folder', inputs, output, '--key', 1,
                                   '--threads', threads, '--strict-no-synthetic-control', flag, *fixtures.get('lifter_args', [])], timeout=120)
        measurements.append({'mode': label, 'threads': threads, 'seconds': elapsed})
        (work / (label + '.log')).write_bytes(stdout)
    audit_path = work / 'audit.json'
    checked([sys.executable, ROOT / 'scripts/provenance_audit.py', '--before', work / 'analysis',
             '--after', work / 'trace1', '--report', audit_path], timeout=120)
    audit = json.loads(audit_path.read_text(encoding='utf-8'))
    capture_path = work / 'capture-effects-audit.json'
    checked([sys.executable, ROOT / 'scripts/capture_effects_audit.py', '--root', work / 'trace1',
             '--input', inputs, '--report', capture_path], timeout=120)
    capture_audit = json.loads(capture_path.read_text(encoding='utf-8'))
    _, a = manifest(work / 'trace1')
    _, b = manifest(work / 'trace4')
    identical = a.keys() == b.keys() and all(sidecar(work / 'trace1', a[k]) == sidecar(work / 'trace4', b[k]) for k in a)
    if not identical:
        raise RuntimeError('thread counts changed source or sidecar content')
    cache_checks = []
    if args.cache:
        for label, threads in [('cache_cold', 1), ('cache_warm', 4)]:
            output = work / label
            result, elapsed = run([args.lifter, 'decompile-folder', inputs, output, '--key', 1,
                '--threads', threads, '--strict-no-synthetic-control', '--emit-binding-provenance',
                '--cache-dir', work / 'cache', *fixtures.get('lifter_args', [])], timeout=180)
            (work / (label + '.log')).write_bytes(result.stdout + result.stderr)
            if result.returncode:
                raise RuntimeError(f'cached folder run failed: {label}')
            stats = [json.loads(line.removeprefix('TOVEK_CACHE ')) for line in result.stderr.decode().splitlines()
                     if line.startswith('TOVEK_CACHE ')]
            if len(stats) != 1 or stats[0]['io_errors'] or stats[0]['corrupt']:
                raise RuntimeError('cache diagnostics missing or contain errors')
            _, cached = manifest(output)
            if a != cached or any(sidecar(work / 'trace1', a[k]) != sidecar(output, cached[k]) for k in a):
                raise RuntimeError('cache changed source, sidecar hash or script identity')
            if any(sha256(work / 'trace1' / a[k]['source_path']) != sha256(output / cached[k]['source_path']) for k in a):
                raise RuntimeError('cache changed emitted source bytes')
            if label == 'cache_warm' and not stats[0]['hits']:
                raise RuntimeError('warm cache did not reuse any artifact')
            cache_checks.append(dict(mode=label, threads=threads, seconds=elapsed,
                                     identical_source_and_metadata=True, statistics=stats[0]))
    if any(sidecar(work / 'analysis', e).get('binding_provenance') is not None
           for e in manifest(work / 'analysis')[1].values()):
        raise RuntimeError('detailed lineage was enabled without opt-in')
    for example in audit['examples']:
        trace = example['trace']
        results = [r for f in trace['functions'] for r in f['conditional_results']
                   if r['phase'] == 'constructed_ssa' and r['final_bindings']]
        if not results:
            raise RuntimeError('conditional probe lost phi-to-final-binding trace')
        if '_g2.' in example['script_path']:
            names = {b['binding_id']: b['name'] for b in trace['final_bindings']}
            if not any(names[bid] == 'selected' for r in results for bid in r['final_bindings']):
                raise RuntimeError('conditional debug result binding lost')
    if args.fixtures_report and len(audit['examples']) != 2:
        raise RuntimeError('expected O2 g1/g2 conditional examples')
    emission_audit = None
    if args.ast:
        emission_path = work / 'emission-audit.json'
        checked([sys.executable, ROOT / 'scripts/emission_map_source_audit.py', '--root', work / 'trace1',
                 '--ast', args.ast, '--report', emission_path], timeout=180)
        emission_audit = json.loads(emission_path.read_text(encoding='utf-8'))
    report = {'schema_version': 1, 'lifter_sha256': sha256(args.lifter),
              'input_report_sha256': sha256(input_report), 'work': str(work),
              'dataset': 'runtime_fixtures' if args.fixtures_report else 'pinned_public_sources',
              'input_summary': fixtures['summary'], 'lifter_args': fixtures.get('lifter_args', []),
              'mode_timings': measurements, 'metadata_deterministic_threads_1_4': identical, 'audit': audit}
    if emission_audit is not None:
        report['emission_audit'] = emission_audit
    report['capture_effects_audit'] = capture_audit
    if args.cache:
        report['cache_checks'] = cache_checks
    if args.compact_annotations:
        compact_checks = []
        for label, threads, cached in [('compact1', 1, False), ('compact4', 4, True), ('compact_warm', 1, True)]:
            output = work / label
            result, elapsed = run([args.lifter, 'decompile-folder', inputs, output, '--key', 1,
                '--threads', threads, '--strict-no-synthetic-control', '--emit-binding-provenance',
                '--compact-annotations', *(['--cache-dir', work / 'cache'] if cached else []),
                *fixtures.get('lifter_args', [])], timeout=180)
            (work / (label + '.log')).write_bytes(result.stdout + result.stderr)
            if result.returncode:
                raise RuntimeError('compact folder run failed: ' + label)
            _, current = manifest(output)
            _, reference = manifest(work / 'compact1')
            if current != reference or any(sidecar(output, current[k]) != sidecar(work / 'compact1', reference[k])
                or sha256(output / current[k]['source_path']) != sha256(work / 'compact1' / reference[k]['source_path']) for k in current):
                raise RuntimeError('compact source/metadata changed across threads/cache')
            stats = [json.loads(line.removeprefix('TOVEK_CACHE ')) for line in result.stderr.decode().splitlines()
                     if line.startswith('TOVEK_CACHE ')]
            if cached and (len(stats) != 1 or stats[0]['io_errors'] or stats[0]['corrupt']
                           or label == 'compact_warm' and not stats[0]['hits']):
                raise RuntimeError('compact cache diagnostics differ')
            compact_checks.append(dict(mode=label, seconds=elapsed, threads=threads, statistics=stats))
        compact_path = work / 'call-annotations.json'
        checked([sys.executable, ROOT / 'scripts/call_reconstruction_audit.py', '--root', work / 'trace1',
                 '--compact-root', work / 'compact1', '--ast', args.ast, '--report', compact_path], timeout=180)
        report['call_annotations'] = json.loads(compact_path.read_text(encoding='utf-8'))
        report['compact_checks'] = compact_checks
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(audit['summary'], indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
