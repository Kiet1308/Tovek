#!/usr/bin/env python3
"""Re-score immutable runtime/public pairs with the current layered validator.

Existing observations and source metrics are not overwritten. Source hashes,
compiler hash/flags and freshly compiled input bytes must match the old report.
"""
import argparse
import collections
import concurrent.futures
import json
import pathlib
import time

from bytecode_dataflow import compare_dataflow
from bytecode_graph import compare_graph
from bytecode_roundtrip import parse_chunk
from provenance_fixtures import select_audit_rows
from roadmap_v2 import checked, fixture_path, sha256


def public_gap_progress(results, lock, *, compiler_sha256, debug, flags):
    """Account for every frozen input without treating a refusal as a certificate."""
    profiles = lock['profiles']
    if (lock.get('schema_version') != 1 or not profiles
            or type(lock.get('total_profiles')) is not int or lock['total_profiles'] != len(profiles)
            or lock.get('initial_status_counts') != dict(collections.Counter(row['initial_status'] for row in profiles))
            or lock['compiler']['sha256'] != compiler_sha256
            or lock['debug'] != debug or lock['compiler_flags'] != flags):
        raise ValueError('proof-gap manifest or compiler profile differs')
    key = lambda row: (row['repo'], row['file'], row['opt'])
    by_key = {key(row): row for row in results}
    if (len(by_key) != len(results) or len({key(row) for row in profiles}) != len(profiles)
            or any(type(row['index']) is not int or row['index'] != index
                   or type(row['opt']) is not int or row['opt'] not in (0, 1, 2)
                   or row['initial_status'] not in ('different', 'unknown')
                   for index, row in enumerate(profiles))):
        raise ValueError('duplicate or invalid proof-gap identity')
    rows = []
    for initial in profiles:
        current = by_key.get(key(initial))
        if current is None:
            raise ValueError(f'missing frozen proof-gap input: {key(initial)}')
        if any(current[field] != initial[field] for field in ('source_sha256', 'input_sha256')):
            raise ValueError(f'frozen proof-gap input changed: {key(initial)}')
        if current['status'] not in ('passed', 'expected_refusal'):
            raise ValueError('proof-gap input was not validated')
        status = 'expected_refusal' if current['status'] == 'expected_refusal' else current['after']['status']
        if status not in ('proved', 'different', 'unknown', 'expected_refusal'):
            raise ValueError('invalid proof-gap result')
        rows.append(dict(**initial, status=status, output_sha256=current.get('output_sha256'),
                         output_changed=current.get('output_sha256') != initial['initial_output_sha256'],
                         result=current.get('after'), refusal=current.get('refusal')))
    return dict(total=len(rows), status=dict(collections.Counter(row['status'] for row in rows)),
                transitions=dict(collections.Counter(f"{row['initial_status']}->{row['status']}" for row in rows)),
                profiles=rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('--fixtures-report', type=pathlib.Path)
    group.add_argument('--public-report', type=pathlib.Path)
    parser.add_argument('--vendor', type=pathlib.Path)
    parser.add_argument('--proof-gaps', type=pathlib.Path,
                        help='account for every frozen profile in this public proof-gap manifest')
    parser.add_argument('--report', type=pathlib.Path, required=True)
    parser.add_argument('--workers', type=int, default=4)
    parser.add_argument('--budget', type=int, default=20000)
    args = parser.parse_args()
    source_report = args.fixtures_report or args.public_report
    original = json.loads(source_report.read_text(encoding='utf-8'))
    compiler = pathlib.Path(original['tools']['compiler']['path'])
    if sha256(compiler) != original['tools']['compiler']['sha256']:
        parser.error('compiler binary changed')
    if args.public_report and not args.vendor:
        parser.error('--vendor is required for public source recompile verification')
    if args.proof_gaps and not args.public_report:
        parser.error('--proof-gaps requires --public-report')
    try:
        passed, coverage = select_audit_rows(original, public=bool(args.public_report))
    except (KeyError, TypeError, ValueError, OSError) as error:
        parser.error(str(error))
    spec = json.loads(pathlib.Path(original['manifest_path']).read_text(encoding='utf-8'))
    flags = original['compiler_flags'] if args.public_report else original['compiler_flags'][1:]
    rows = [*passed, *coverage['refusals']]

    def process(row):
        if args.fixtures_report:
            case = f"{row['case']}_O{row['opt']}_g{row['debug']}"
            directory = pathlib.Path(original['work']) / case
            source, output, bytecode = (directory / name for name in ('source.luau', 'output.luau', 'input.luaubc'))
            debug = row['debug']
            identity = {k: row[k] for k in ('case', 'opt', 'debug', 'group')}
        else:
            source = fixture_path(args.vendor, row['repo'] + '/' + row['file'])
            output = pathlib.Path(row['output']) if row['status'] == 'passed' else None
            bytecode = pathlib.Path(row['input'])
            debug = spec['debug_level']
            identity = {k: row[k] for k in ('repo', 'file', 'opt', 'split')}
        if sha256(source) != row['source_sha256']:
            raise ValueError(f'source changed: {identity}')
        command = [compiler, '--binary', f"-O{row['opt']}", f'-g{debug}', *flags]
        raw = checked([*command, source], timeout=30)[0]
        if raw != bytecode.read_bytes():
            raise ValueError(f'input recompile mismatch: {identity}')
        if row['status'] == 'expected_refusal':
            return dict(**identity, status='expected_refusal', source_sha256=row['source_sha256'],
                        input_sha256=sha256(bytecode), refusal=row['refusal'])
        if sha256(output) != row['output_sha256']:
            raise ValueError(f'output changed: {identity}')
        rebuilt = checked([*command, output], timeout=30)[0]
        a, b = parse_chunk(raw, 1), parse_chunk(rebuilt, 1)
        started = time.perf_counter()
        result = compare_dataflow(a, b, budget=args.budget)
        elapsed = time.perf_counter() - started
        if row['dataflow']['status'] == 'proved' and result['status'] != 'proved':
            raise ValueError(f'previous proof lost: {identity}: {result}')
        self_check = compare_graph(a, a, budget=args.budget)
        return dict(**identity, status='passed', source_sha256=row['source_sha256'], output_sha256=row['output_sha256'],
                    input_sha256=sha256(bytecode), before=row['dataflow'], after=result,
                    graph_self_control=self_check, validator_seconds=elapsed)

    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        outcomes = list(pool.map(process, rows))
    results = [row for row in outcomes if row['status'] == 'passed']
    transitions = collections.Counter(f"{r['before']['status']}->{r['after']['status']}" for r in results)
    summary = dict(files=len(results), transitions=dict(transitions),
                   certificates=dict(collections.Counter(r['after']['model'] for r in results if r['after']['status'] == 'proved')),
                   graph_self_controls=dict(collections.Counter(r['graph_self_control']['status'] for r in results)))
    report = dict(schema_version=1, input_report_sha256=sha256(source_report),
                  dataset='runtime' if args.fixtures_report else 'public', tools=original['tools'],
                  compiler_commit_expected=original['compiler_commit_expected'],
                  budget_per_model=args.budget, seconds=time.perf_counter() - started,
                  summary=summary, rows=results, input_coverage=coverage,
                  refusal_input_checks=[row for row in outcomes if row['status'] == 'expected_refusal'],
                  limitations='Re-scoring only. No new decompiler output or runtime observations. Graph identity is a bounded sufficient condition; mismatch/unsupported is unknown, never proof.')
    if args.proof_gaps:
        report['proof_gaps'] = public_gap_progress(outcomes, json.loads(args.proof_gaps.read_text(encoding='utf-8')),
                                                 compiler_sha256=sha256(compiler), debug=spec['debug_level'], flags=flags)
        report['proof_gaps']['manifest_sha256'] = sha256(args.proof_gaps)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
