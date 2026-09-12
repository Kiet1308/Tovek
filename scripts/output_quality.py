#!/usr/bin/env python3
"""Binding-aware presentation metrics. Counts are candidates, never motion proofs.

The optional baseline is immutable. Every per-file increase is reported even
when aggregate counts improve; only explicitly selected metrics gate a run.
"""
import argparse
import collections
import concurrent.futures
import hashlib
import json
from pathlib import Path
import re
import subprocess

from source_fidelity import parse_ast


def identity(local):
    return local.get('location'), local.get('name')


def unwrap(node):
    while isinstance(node, dict) and node.get('type') == 'AstExprGroup':
        node = node['expr']
    return node


def analyze_tree(tree, text=''):
    nodes, stack = [], [(tree, 0)]
    while stack:
        node, depth = stack.pop()
        if isinstance(node, list):
            stack.extend((x, depth) for x in node)
        elif isinstance(node, dict) and node.get('type') != 'AstLocal':
            nodes.append((node, depth))
            stack.extend((v, depth + (node.get('type') == 'AstExprFunction'))
                         for k, v in node.items() if k != 'local' or node.get('type') != 'AstExprLocal')
    refs = collections.Counter(identity(n['local']) for n, _ in nodes if n.get('type') == 'AstExprLocal')
    depths = collections.defaultdict(set)
    for node, depth in nodes:
        if node.get('type') == 'AstExprLocal':
            depths[identity(node['local'])].add(depth)
    metrics = collections.Counter()
    for node, depth in nodes:
        kind = node.get('type')
        if kind == 'AstStatLocal':
            metrics['local_declarations'] += 1
            metrics['local_bindings'] += len(node.get('vars', []))
            if len(node.get('vars', [])) == len(node.get('values', [])) == 1:
                local, value = node['vars'][0], unwrap(node['values'][0])
                key = identity(local)
                if refs[key] == 1:
                    suffix = value.get('type', 'unknown')
                    metrics['single_use_' + suffix] += 1
                    generated = re.fullmatch(r'[pv]\d*', local['name']) is not None
                    captured = any(d != depth for d in depths[key])
                    if generated:
                        metrics['generated_single_use_' + suffix] += 1
                    metrics[('captured_' if captured else 'uncaptured_') + 'single_use_' + suffix] += 1
                    if generated and not captured:
                        metrics['uncaptured_generated_single_use_' + suffix] += 1
        if kind != 'AstStatBlock':
            continue
        for first, second in zip(node['body'], node['body'][1:]):
            if first.get('type') != 'AstStatLocal' or len(first.get('vars', [])) != 1 or len(first.get('values', [])) != 1:
                continue
            call = unwrap(first['values'][0])
            if call.get('type') != 'AstExprCall' or call['func'].get('type') != 'AstExprGlobal' or call['func'].get('global') != 'require':
                continue
            if second.get('type') != 'AstStatAssign' or len(second.get('vars', [])) != 1 or len(second.get('values', [])) != 1:
                continue
            field, value = second['vars'][0], unwrap(second['values'][0])
            key = identity(first['vars'][0])
            if field.get('type') == 'AstExprIndexName' and value.get('type') == 'AstExprLocal' and identity(value['local']) == key:
                metrics['require_field_relays'] += 1
                if refs[key] == 1 and all(d == depth for d in depths[key]):
                    metrics['sole_use_require_field_relays'] += 1
    metrics['discard_locals'] = len(re.findall(r'(?m)^\s*local _ = ', text))
    metrics['inferred_call_annotations'] = text.count('equivalent call inferred; original call site unknown')
    metrics['lines'] = len(text.splitlines())
    metrics['bytes_lf'] = len(text.replace('\r\n', '\n').encode('utf-8'))
    return dict(sorted(metrics.items()))


def compare_rows(before, after, gates=()):
    previous, current = ({r['path']: r for r in rows} for rows in (before, after))
    rows, failures = [], []
    for name in sorted(previous.keys() | current.keys()):
        a, b = previous.get(name), current.get(name)
        if a is None or b is None or a['status'] != 'passed' or b['status'] != 'passed':
            rows.append({'path': name, 'status': 'unmeasured'})
            if b is None or b['status'] != 'passed':
                failures.append({'path': name, 'reason': 'missing_or_unparsed_current'})
            continue
        delta = {key: b['metrics'].get(key, 0) - a['metrics'].get(key, 0)
                 for key in sorted(a['metrics'].keys() | b['metrics'].keys())}
        rows.append({'path': name, 'status': 'measured', 'delta': delta,
                     'increased': [k for k, v in delta.items() if v > 0]})
        failures.extend({'path': name, 'metric': k, 'delta': delta[k]}
                        for k in gates if delta.get(k, 0) > 0)
    return {'rows': rows, 'gate_failures': failures}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ('root', 'ast', 'report'):
        parser.add_argument('--' + key, type=Path, required=True)
    parser.add_argument('--baseline', type=Path)
    parser.add_argument('--gate-metric', action='append', default=[])
    parser.add_argument('--workers', type=int, default=4)
    args = parser.parse_args()
    if args.workers < 1 or not args.root.is_dir():
        parser.error('a readable root and positive worker count are required')
    if args.gate_metric and not args.baseline:
        parser.error('metric gates require a baseline')
    paths = sorted(args.root.rglob('*.luau'))
    if not paths:
        parser.error('no Luau outputs found')
    def process(path):
        row = {'path': path.relative_to(args.root).as_posix(), 'status': 'failed'}
        try:
            row['sha256'] = hashlib.sha256(path.read_bytes()).hexdigest()
            row['metrics'] = analyze_tree(parse_ast(args.ast, path, timeout=60), path.read_text(encoding='utf-8'))
            row['status'] = 'passed'
        except (RuntimeError, ValueError, OSError, subprocess.TimeoutExpired) as error:
            row['error'] = str(error)[:1000]
        return row
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        rows = list(pool.map(process, paths))
    totals = collections.Counter()
    for row in rows:
        totals.update(row.get('metrics', {}))
    report = {'schema_version': 1, 'metric_schema': 'presentation-v1',
              'root': args.root.resolve().as_posix(), 'parser_sha256': hashlib.sha256(args.ast.read_bytes()).hexdigest(),
              'summary': {'files': len(rows), 'status': dict(collections.Counter(r['status'] for r in rows)),
                          'unique_outputs': len({r['sha256'] for r in rows if 'sha256' in r}), 'metrics': dict(totals)},
              'rows': rows, 'contract': 'Syntactic candidates only. Captured and uncaptured uses are reported separately. No semantic proof or independent-program count is inferred from file counts.'}
    if args.baseline:
        baseline = json.loads(args.baseline.read_text(encoding='utf-8'))
        if baseline.get('metric_schema') != report['metric_schema'] or baseline.get('parser_sha256') != report['parser_sha256']:
            parser.error('baseline metric schema/parser differs')
        report['baseline_sha256'] = hashlib.sha256(args.baseline.read_bytes()).hexdigest()
        report['comparison'] = compare_rows(baseline['rows'], rows, args.gate_metric)
    report['status'] = 'failed' if any(r['status'] != 'passed' for r in rows) or report.get('comparison', {}).get('gate_failures') else 'passed'
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=1) + '\n', encoding='utf-8')
    print(json.dumps({'status': report['status'], 'summary': report['summary']}, indent=2))
    return int(report['status'] != 'passed')


if __name__ == '__main__':
    raise SystemExit(main())
