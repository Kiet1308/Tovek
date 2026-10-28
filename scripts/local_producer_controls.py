#!/usr/bin/env python3
"""Exercise introduction-ledger refusals against real emitted source/parser data."""
import argparse
import collections
import copy
import json
import pathlib

from binding_graph import Refused, attach_storage, digest, lexical_graph, parse_source, summarize
from local_producers import PASSES
from provenance_audit import manifest, sidecar


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('root', 'ast', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    parser.add_argument('--producer', choices=sorted(PASSES),
                        help='Require real emitted introductions from this producer.')
    args = parser.parse_args()
    args.root = args.root.resolve(strict=True)
    _, entries = manifest(args.root)
    examples = {}
    for key, entry in sorted(entries.items()):
        metadata = sidecar(args.root, entry)
        trace = metadata.get('binding_provenance', {})
        groups = trace.get('local_producers', {}).get('passes', [])
        for group in groups:
            if group['records'] and (args.producer is None or group['pass'] == args.producer):
                examples.setdefault(group['pass'], (key, entry, metadata, group))
    if not examples:
        # No pass introduced a local in this output set: there is nothing
        # real to corrupt. The refusals stay covered by test_local_producers.
        result = dict(schema_version=2, rows=[], summary=dict(no_introductions=1),
                      contract='No emitted local introduction in this output set; controls not applicable.')
        if args.producer is not None:
            result['summary'] = dict(failed=1)
            result['contract'] = f'Required producer {args.producer} has no emitted introduction.'
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(result, indent=1) + '\n', encoding='utf-8', newline='\n')
        print(json.dumps(result['summary']))
        return int(args.producer is not None)
    rows, sources = [], []
    for pass_name, (key, entry, metadata, group) in sorted(examples.items()):
        controls, source = exercise(args, metadata, group)
        rows.extend(dict(row, producer=pass_name, script_path=key) for row in controls)
        sources.append(dict(producer=pass_name, script_path=key, source_sha256=digest(source),
                            sidecar_sha256=entry['sidecar_sha256']))
    result = dict(schema_version=2, sources=sources, ast_sha256=digest(args.ast.read_bytes()),
                  rows=rows, summary=dict(collections.Counter(r['status'] for r in rows)),
                  contract='Real emitted local introductions and pinned-parser lexical bindings. '
                           'Every observed producer gets one positive and ten corrupted metadata controls; '
                           'no source edits or inferred input-origin proof.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(result['summary']))
    return int(any(r['status'] != 'passed' for r in rows))


def exercise(args, metadata, group):
    pass_name = group['pass']
    foreign_role = sorted(role for _, roles in PASSES.values() for role in roles
                          if role not in PASSES[pass_name][1])[0]
    source_path = (args.root / metadata['source_path']).resolve(strict=True)
    if not source_path.is_relative_to(args.root):
        raise ValueError('source path outside root')
    source = source_path.read_bytes()
    graph = lexical_graph(parse_source(args.ast, source), source)
    positive = attach_storage(copy.deepcopy(graph), metadata, source)
    rows = [dict(control='original', status='passed', summary=summarize(positive))]
    bid = group['records'][0]['binding_id']

    def producer(d):
        return next(g for g in d['binding_provenance']['local_producers']['passes'] if g['pass'] == pass_name)
    def final(d):
        return next(r for r in d['binding_provenance']['final_bindings'] if r['binding_id'] == bid)
    def copied_source(d):
        d['source_recovery']['bindings'].append(dict(binding_id=bid, origins=[dict(kind='debug_local')]))
    def copied_ancestry(d):
        final(d).update(lineage=['b9999999999999999'], unknown_origins=['b9999999999999999'],
                        has_conditional_result_ancestry=False)
    def duplicate(d):
        g = producer(d)
        g['records'].append(copy.deepcopy(g['records'][0]))
        g['introduced_locals'] += 1
        d['binding_provenance']['local_producers']['recorded_introductions'] += 1
        d[pass_name]['introduced_locals'] += 1
        d[pass_name]['introduced_bindings']['records'].append(copy.deepcopy(g['records'][0]))
    def missing_token(d):
        trace = d['binding_provenance']
        tokens = trace['output_map']['bindings']
        index = next(i for i, token in enumerate(tokens) if token['binding_id'] == bid and token['role'] == 'read')
        tokens.pop(index)
        trace['summary']['identifier_spans'] -= 1

    mutations = [
        ('missing_final_identity', lambda d: producer(d)['records'][0].update(binding_id='b18446744073709551616')),
        ('copied_debug_identity', copied_source), ('copied_input_ancestry', copied_ancestry),
        ('wrong_record_pointer', lambda d: final(d)['emitter_introduction'].update(record=9999)),
        ('duplicate_introduction', duplicate),
        ('wrong_pass_role', lambda d: producer(d)['records'][0].update(role=foreign_role)),
        ('wrong_rewrite_model', lambda d: producer(d).update(rewrite_model='unrecognized')),
        ('changed_introduction_count', lambda d: d[pass_name].update(introduced_locals=0)),
        ('missing_pass_report', lambda d: d.pop(pass_name)),
        ('missing_lexical_use_token', missing_token),
    ]
    for name, mutate in mutations:
        changed = copy.deepcopy(metadata)
        mutate(changed)
        try:
            attach_storage(copy.deepcopy(graph), changed, source)
            row = dict(control=name, status='failed', reason='invalid metadata accepted')
        except Refused as error:
            row = dict(control=name, status='passed', reason=str(error))
        rows.append(row)
    return rows, source


if __name__ == '__main__':
    raise SystemExit(main())
