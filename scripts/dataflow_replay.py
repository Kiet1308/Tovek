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
from roadmap_v2 import checked, sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument('--fixtures-report', type=pathlib.Path)
    group.add_argument('--public-report', type=pathlib.Path)
    parser.add_argument('--vendor', type=pathlib.Path)
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
    rows = original['cases'] if args.fixtures_report else original['rows']
    if any(row['status'] != 'passed' for row in rows):
        parser.error('input report must pass first')

    def process(row):
        if args.fixtures_report:
            case = f"{row['case']}_O{row['opt']}_g{row['debug']}"
            directory = pathlib.Path(original['work']) / case
            source, output, bytecode = (directory / name for name in ('source.luau', 'output.luau', 'input.luaubc'))
            debug = row['debug']
            identity = {k: row[k] for k in ('case', 'opt', 'debug', 'group')}
        else:
            source = args.vendor / row['repo'] / row['file']
            output = pathlib.Path(row['output'])
            bytecode = pathlib.Path(str(output).removesuffix('.out.luau') + '.luaubc')
            debug = 1
            identity = {k: row[k] for k in ('repo', 'file', 'opt', 'split')}
        if sha256(source) != row['source_sha256'] or sha256(output) != row['output_sha256']:
            raise ValueError(f'source/output changed: {identity}')
        command = [compiler, '--binary', f"-O{row['opt']}", f'-g{debug}', '--fflags=false']
        raw = checked([*command, source], timeout=30)[0]
        if raw != bytecode.read_bytes():
            raise ValueError(f'input recompile mismatch: {identity}')
        rebuilt = checked([*command, output], timeout=30)[0]
        a, b = parse_chunk(raw, 1), parse_chunk(rebuilt, 1)
        started = time.perf_counter()
        result = compare_dataflow(a, b, budget=args.budget)
        elapsed = time.perf_counter() - started
        if row['dataflow']['status'] == 'proved' and result['status'] != 'proved':
            raise ValueError(f'previous proof lost: {identity}: {result}')
        self_check = compare_graph(a, a, budget=args.budget)
        return dict(**identity, source_sha256=row['source_sha256'], output_sha256=row['output_sha256'],
                    input_sha256=sha256(bytecode), before=row['dataflow'], after=result,
                    graph_self_control=self_check, validator_seconds=elapsed)

    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        results = list(pool.map(process, rows))
    transitions = collections.Counter(f"{r['before']['status']}->{r['after']['status']}" for r in results)
    summary = dict(files=len(results), transitions=dict(transitions),
                   certificates=dict(collections.Counter(r['after']['model'] for r in results if r['after']['status'] == 'proved')),
                   graph_self_controls=dict(collections.Counter(r['graph_self_control']['status'] for r in results)))
    report = dict(schema_version=1, input_report_sha256=sha256(source_report),
                  dataset='runtime' if args.fixtures_report else 'public', tools=original['tools'],
                  compiler_commit_expected=original['compiler_commit_expected'],
                  budget_per_model=args.budget, seconds=time.perf_counter() - started,
                  summary=summary, rows=results,
                  limitations='Re-scoring only. No new decompiler output or runtime observations. Graph identity is a bounded sufficient condition; mismatch/unsupported is unknown, never proof.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
