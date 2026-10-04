#!/usr/bin/env python3
"""Per-case semantic and presentation regression gates for text-changing rewrites.

Freeze existing roadmap/public/generated reports, then compare immutable
snapshots. Unknown is never promoted to proof. A reviewed exception matches one
exact case, check, before/after value and output-hash pair, with a written reason.
Status, compilation, runtime and input-identity failures cannot be waived.
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import math
import pathlib
import subprocess
from types import SimpleNamespace


SNAPSHOT = 'tovek-quality-snapshot-v1'
GATE = 'tovek-quality-gate-v1'
DEFAULT_METRICS = ('discard_locals', 'require_field_relays', 'generated_single_use_AstExprLocal')
REVIEWABLE = {'proof_regression', 'unproved_output_change', 'presentation_increase', 'fidelity_decrease'}
REVIEW_FIELDS = {'case_id', 'check', 'before', 'after', 'before_output_sha256', 'after_output_sha256', 'reason'}


def sha256(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


def hash_value(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()


def save(path, value):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')


def valid_hash(value):
    return isinstance(value, str) and len(value) == 64 and all(c in '0123456789abcdef' for c in value)


def runtime_evidence(row):
    observations = row.get('observations')
    fields = ('exit', 'stdout', 'stderr')
    if observations is None:
        observations = row.get('runtime')
        # roadmap_v2's declared observation contract is exit/stdout; generated
        # tests also compare stderr. Do not silently strengthen or weaken either.
        fields = ('exit', 'stdout')
    if not isinstance(observations, dict) or not {'source', 'output'} <= observations.keys():
        return dict(status='unavailable', model=None, source=None, output=None)
    if any(not isinstance(observations[side], dict) or any(key not in observations[side] for key in fields)
           for side in ('source', 'output')):
        return dict(status='failed', model='+'.join(fields), source=None, output=None)
    normalized = {side: {key: observations[side][key] for key in fields} for side in ('source', 'output')}
    passed = normalized['source'] == normalized['output'] and normalized['source']['exit'] == 0
    return dict(status='passed' if passed else 'failed', model='+'.join(fields),
                source=hash_value(normalized['source']), output=hash_value(normalized['output']))


def normalize_report(label, report):
    if not label or any(c in label for c in '/\\:'):
        raise ValueError('report labels must be unique simple names')
    if 'cases' in report and isinstance(report['cases'], list):
        raw_rows = report['cases']
    elif 'rows' in report and isinstance(report['rows'], list):
        raw_rows = report['rows']
    else:
        raise ValueError('expected a roadmap, public-source or generated result report')
    if not raw_rows:
        raise ValueError('empty source report')
    tools = report.get('tools', {})
    context = {key: report.get(key) for key in ('manifest_sha256', 'compiler_commit_expected',
               'bytecode_version', 'lifter_args', 'generator', 'generator_sha256')}
    context['compiler_sha256'] = tools.get('compiler', {}).get('sha256')
    context['parser_sha256'] = tools.get('ast', {}).get('sha256')
    rows = []
    for row in raw_rows:
        if not isinstance(row, dict) or 'status' not in row:
            raise ValueError('input is a manifest, not an evaluated report')
        if 'case' in row:
            name = str(row['case'])
        elif 'repo' in row and 'file' in row:
            name = str(row['repo']) + '/' + str(row['file'])
        elif 'seed' in row:
            name = 'seed-' + str(row['seed'])
        elif 'path' in row:
            name = str(row['path'])
        else:
            raise ValueError('case has no stable identity')
        profile = dict(bytecode_version=row.get('bytecode_version', report.get('bytecode_version', 9)),
                       opt=row.get('opt'), debug=row.get('debug'))
        # JSON tuples avoid ambiguous delimiter concatenation in arbitrary paths.
        case_id = json.dumps([label, name, profile['bytecode_version'], profile['opt'], profile['debug']], separators=(',', ':'))
        proof = row.get('dataflow') or {'status': 'unavailable'}
        if not isinstance(proof, dict) or proof.get('status') not in ('proved', 'different', 'unknown', 'unavailable'):
            raise ValueError('unknown dataflow status')
        rows.append(dict(case_id=case_id, name=name, report_label=label, profile=profile,
                         status=row['status'], compile_status=row.get('recompile', 'unavailable'),
                         source_sha256=row.get('source_sha256'), decoded_input_sha256=row.get('decoded_input_sha256'),
                         output_sha256=row.get('output_sha256'), proof=proof, runtime=runtime_evidence(row),
                         presentation=row.get('output_quality'), fidelity=row.get('source_fidelity')))
    return context, rows


def freeze(inputs):
    contexts, rows, sources = {}, [], {}
    for label, path in inputs:
        if label in contexts:
            raise ValueError('duplicate source report label')
        report = json.loads(pathlib.Path(path).read_text(encoding='utf-8'))
        contexts[label], added = normalize_report(label, report)
        rows.extend(added)
        sources[label] = dict(path=str(pathlib.Path(path).resolve()), sha256=sha256(path), tools=report.get('tools'))
    if len({row['case_id'] for row in rows}) != len(rows) or not rows:
        raise ValueError('duplicate or empty case set')
    return dict(schema_version=1, kind=SNAPSHOT, contexts=contexts, source_reports=sources,
                rows=sorted(rows, key=lambda row: row['case_id']),
                contract='A snapshot of evidence, not an equivalence certificate. Proof model limits and unknown cases remain explicit.')


def capture(args):
    """Evaluate saved API sources separately from all performance measurements."""
    from benchmark_single import decode, load_manifest
    from bytecode_dataflow import compare_dataflow
    from bytecode_roundtrip import BytecodeError, parse_chunk
    from output_quality import analyze_tree
    from roadmap_v2 import compile_source
    from source_fidelity import parse_ast
    manifest, selected = load_manifest(args.manifest, args.input_root, args.group)
    output_root = args.output_root.resolve(strict=True)
    input_root = args.input_root.resolve(strict=True)
    compiler, ast = args.compiler.resolve(strict=True), args.ast.resolve(strict=True)
    rows = []
    for item in selected:
        row = dict(path=item['path'], status='failed', recompile='unavailable', opt=args.opt, debug=args.debug,
                   decoded_input_sha256=item['decoded_input_sha256'], dataflow={'status': 'unavailable'})
        try:
            raw = decode((input_root/item['path']).read_bytes(), item['encoding'])
            # Verify the input again instead of silently reading a changed corpus.
            if hashlib.sha256(raw).hexdigest() != item['decoded_input_sha256']:
                raise ValueError('input changed during quality collection')
            if not raw or raw[0] == 0:
                raise ValueError('empty or compiler-error input is not successful decompilation')
            row['bytecode_version'] = raw[0]
            source = (output_root/(item['path']+'.luau')).resolve(strict=True)
            if not source.is_relative_to(output_root) or source.stat().st_size > 64*1024*1024:
                raise ValueError('output escapes root or exceeds byte limit')
            text = source.read_text(encoding='utf-8')
            if not text.strip():
                raise ValueError('empty source')
            row['output_sha256'] = sha256(source)
            settings = SimpleNamespace(compiler=compiler, timeout=args.timeout,
                                       bytecode_version=raw[0] if raw[0] in (9, 12, 14) else 9)
            rebuilt = compile_source(settings, source, args.opt, args.debug)
            row['recompile'] = 'passed'
            row['dataflow'] = compare_dataflow(parse_chunk(raw, manifest['decode_key']), parse_chunk(rebuilt, 1))
            row['output_quality'] = analyze_tree(parse_ast(ast, source, args.timeout), text)
            row['status'] = 'passed'
        except (ValueError, RuntimeError, OSError, BytecodeError, subprocess.TimeoutExpired) as error:
            row['error'] = str(error)
        rows.append(row)
    raw_report = dict(manifest_sha256=sha256(args.manifest),
                      tools={'compiler': {'path': str(compiler), 'sha256': sha256(compiler)},
                             'ast': {'path': str(ast), 'sha256': sha256(ast)}}, rows=rows)
    context, normalized = normalize_report('single', raw_report)
    context['recompile_profile'] = dict(opt=args.opt, debug=args.debug)
    return dict(schema_version=1, kind=SNAPSHOT, contexts={'single': context},
                source_reports={'single': dict(manifest_sha256=sha256(args.manifest), output_root=str(output_root),
                                               tools=raw_report['tools'])}, rows=normalized, capture_rows=rows,
                contract='Offline compilation, independent bytecode/dataflow comparison and presentation metrics. '
                         'Arbitrary saved bytecode has no source reference or runtime driver here. Unknown remains unknown; '
                         'changed unknown outputs require exact reviewed exceptions. None of these times are speed evidence.')


def validate_snapshot(snapshot):
    if snapshot.get('schema_version') != 1 or snapshot.get('kind') != SNAPSHOT or not isinstance(snapshot.get('contexts'), dict):
        raise ValueError('unsupported quality snapshot')
    rows = snapshot.get('rows')
    if not isinstance(rows, list) or not rows:
        raise ValueError('empty quality snapshot')
    by_id = {}
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get('case_id'), str) or row['case_id'] in by_id:
            raise ValueError('duplicate or missing case identity')
        if row.get('proof', {}).get('status') not in ('proved', 'different', 'unknown', 'unavailable'):
            raise ValueError('invalid proof status')
        if row.get('proof', {}).get('status') == 'proved' and not row['proof'].get('model'):
            raise ValueError('proof lacks its model')
        if row.get('runtime', {}).get('status') not in ('passed', 'failed', 'unavailable'):
            raise ValueError('invalid runtime status')
        for field in ('source_sha256', 'decoded_input_sha256', 'output_sha256'):
            if row.get(field) is not None and not valid_hash(row[field]):
                raise ValueError('invalid identity hash: ' + field)
        for field in ('presentation', 'fidelity'):
            if row.get(field) is not None and not isinstance(row[field], dict):
                raise ValueError('invalid metrics')
        by_id[row['case_id']] = row
    return by_id


def validate_reviews(allowlist):
    if allowlist is None:
        return []
    if set(allowlist) != {'schema_version', 'reviews'} or allowlist['schema_version'] != 1 or not isinstance(allowlist['reviews'], list):
        raise ValueError('unsupported reviewed allowlist')
    reviews, seen = [], set()
    for review in allowlist['reviews']:
        if (not isinstance(review, dict) or set(review) != REVIEW_FIELDS or review['check'] not in REVIEWABLE
                or not isinstance(review['case_id'], str) or not review['case_id']
                or not isinstance(review['reason'], str) or len(review['reason'].strip()) < 8):
            raise ValueError('review must name one exact reviewable check and explain its reason')
        if not valid_hash(review['after_output_sha256']) or (review['before_output_sha256'] is not None and not valid_hash(review['before_output_sha256'])):
            raise ValueError('review lacks exact output identities')
        key = hash_value({key: value for key, value in review.items() if key != 'reason'})
        if key in seen:
            raise ValueError('duplicate reviewed exception')
        seen.add(key)
        reviews.append(review)
    return reviews


def compare(before, after, *, metrics=DEFAULT_METRICS, minimum_metrics=(), allowlist=None):
    previous, current = validate_snapshot(before), validate_snapshot(after)
    reviews = validate_reviews(allowlist)
    violations, pairs = [], []

    def issue(case_id, check, old, new, a=None, b=None):
        violations.append(dict(case_id=case_id, check=check, before=old, after=new,
                               before_output_sha256=(a or {}).get('output_sha256'),
                               after_output_sha256=(b or {}).get('output_sha256')))

    if before['contexts'] != after['contexts']:
        issue('__evidence_context__', 'evidence_context_changed', before['contexts'], after['contexts'])
    for case_id in sorted(previous.keys() | current.keys()):
        a, b = previous.get(case_id), current.get(case_id)
        if a is None or b is None:
            issue(case_id, 'case_coverage_changed', a is not None, b is not None, a, b)
            continue
        for field in ('source_sha256', 'decoded_input_sha256', 'profile'):
            if a.get(field) != b.get(field):
                issue(case_id, 'input_identity_changed', {field: a.get(field)}, {field: b.get(field)}, a, b)
        if b.get('status') not in ('passed', 'ok'):
            issue(case_id, 'status_failed', a.get('status'), b.get('status'), a, b)
        if b.get('compile_status') != 'passed':
            issue(case_id, 'compilation_unverified', a.get('compile_status'), b.get('compile_status'), a, b)
        if not valid_hash(b.get('output_sha256')) or (a.get('status') in ('passed', 'ok') and not valid_hash(a.get('output_sha256'))):
            issue(case_id, 'output_identity_missing', a.get('output_sha256'), b.get('output_sha256'), a, b)
        old_runtime, new_runtime = a['runtime'], b['runtime']
        if new_runtime['status'] == 'failed' or (old_runtime['status'] == 'passed' and new_runtime['status'] != 'passed'):
            issue(case_id, 'runtime_failed_or_lost', old_runtime, new_runtime, a, b)
        elif old_runtime['status'] == 'passed' and (old_runtime.get('source') != new_runtime.get('source')
                                                   or old_runtime.get('model') != new_runtime.get('model')):
            issue(case_id, 'runtime_reference_changed', old_runtime, new_runtime, a, b)
        old_proof, new_proof = a['proof']['status'], b['proof']['status']
        if ((old_proof == 'proved' and new_proof != 'proved')
                or (old_proof == 'different' and new_proof in ('unknown', 'unavailable'))):
            issue(case_id, 'proof_regression', old_proof, new_proof, a, b)
        changed = a.get('output_sha256') != b.get('output_sha256')
        if changed and new_proof != 'proved':
            issue(case_id, 'unproved_output_change', old_proof, new_proof, a, b)
        for field, selected, increasing, check in [('presentation', metrics, True, 'presentation_increase'),
                                                   ('fidelity', minimum_metrics, False, 'fidelity_decrease')]:
            if not selected:
                continue
            old_values, new_values = a.get(field), b.get(field)
            if old_values is None or new_values is None:
                issue(case_id, 'metrics_unavailable', field if old_values is None else 'measured',
                      field if new_values is None else 'measured', a, b)
                continue
            for metric in selected:
                old, new = old_values.get(metric, 0 if increasing else None), new_values.get(metric, 0 if increasing else None)
                if (type(old) not in (int, float) or type(new) not in (int, float)
                        or not math.isfinite(old) or not math.isfinite(new) or old < 0 or new < 0):
                    issue(case_id, 'metric_unmeasured', {metric: old}, {metric: new}, a, b)
                elif (new > old if increasing else new < old):
                    issue(case_id, check, {metric: old}, {metric: new}, a, b)
        pairs.append(dict(case_id=case_id, decoded_input_sha256=b.get('decoded_input_sha256'),
                          before_output_sha256=a.get('output_sha256'), after_output_sha256=b.get('output_sha256'),
                          proof_status=new_proof, proof_model=b['proof'].get('model'),
                          runtime_status=new_runtime['status'], output_changed=changed))
    used, accepted, failed = set(), [], []
    for violation in violations:
        matches = [index for index, review in enumerate(reviews)
                   if violation['check'] in REVIEWABLE and all(review[key] == value for key, value in violation.items())]
        if len(matches) == 1:
            used.add(matches[0])
            accepted.append(dict(violation, reason=reviews[matches[0]]['reason']))
        else:
            failed.append(violation)
    for index, review in enumerate(reviews):
        if index not in used:
            failed.append(dict(case_id=review['case_id'], check='stale_review', reason=review['reason']))
    return dict(schema_version=1, kind=GATE, status='failed' if failed else 'passed',
                metrics=list(metrics), minimum_metrics=list(minimum_metrics),
                failures=failed, reviewed_exceptions=accepted, cases=pairs,
                approved_outputs=[pair for pair in pairs if pair['output_changed']] if not failed else [],
                summary=dict(cases_before=len(previous), cases_after=len(current), failures=len(failed),
                             reviewed_exceptions=len(accepted), output_changes=sum(pair['output_changed'] for pair in pairs),
                             current_proofs=dict(collections.Counter(row['proof']['status'] for row in current.values()))),
                contract='Per-case gates; no aggregate improvement cancels an individual regression. Unknown/different are not proof. '
                         'Reviewed exceptions are exact, hash-bound and visible; runtime/status/compile/input failures cannot be waived. '
                         'A passed finite test gate does not prove universal equivalence or human readability.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='mode', required=True)
    snapshot = commands.add_parser('freeze')
    snapshot.add_argument('--input', action='append', required=True, metavar='LABEL=REPORT')
    snapshot.add_argument('--report', type=pathlib.Path, required=True)
    collect = commands.add_parser('capture')
    for name in ('manifest', 'input-root', 'output-root', 'compiler', 'ast', 'report'):
        collect.add_argument('--'+name, type=pathlib.Path, required=True)
    collect.add_argument('--group', default='all')
    collect.add_argument('--opt', type=int, choices=(0, 1, 2), default=2)
    collect.add_argument('--debug', type=int, choices=(0, 1, 2), default=1)
    collect.add_argument('--timeout', type=float, default=30)
    gate = commands.add_parser('compare')
    for name in ('before', 'after', 'report'):
        gate.add_argument('--'+name, type=pathlib.Path, required=True)
    gate.add_argument('--allowlist', type=pathlib.Path)
    gate.add_argument('--metric', action='append', help='presentation count that must not increase; omitted counters count as zero')
    gate.add_argument('--minimum-metric', action='append', default=[], help='source-fidelity metric that must not decrease')
    args = parser.parse_args()
    if args.mode == 'freeze':
        inputs = []
        for value in args.input:
            label, separator, path = value.partition('=')
            if not separator or not path:
                parser.error('--input requires LABEL=REPORT')
            inputs.append((label, pathlib.Path(path)))
        report = freeze(inputs)
    elif args.mode == 'capture':
        report = capture(args)
    else:
        read = lambda path: json.loads(path.read_text(encoding='utf-8'))
        report = compare(read(args.before), read(args.after), metrics=args.metric if args.metric is not None else DEFAULT_METRICS,
                         minimum_metrics=args.minimum_metric, allowlist=read(args.allowlist) if args.allowlist else None)
        report['evidence'] = dict(before_sha256=sha256(args.before), after_sha256=sha256(args.after),
                                  allowlist_sha256=sha256(args.allowlist) if args.allowlist else None)
    save(args.report, report)
    print(json.dumps(report['summary'] if args.mode == 'compare' else {'cases': len(report['rows'])}))
    return int(report.get('status') == 'failed' or (args.mode == 'capture' and any(row['status'] != 'passed' for row in report['rows'])))


if __name__ == '__main__':
    raise SystemExit(main())
