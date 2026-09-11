#!/usr/bin/env python3
"""Exercise introduction-ledger refusals against real emitted source/parser data."""
import argparse
import collections
import copy
import json
import pathlib

from binding_graph import Refused, attach_storage, digest, lexical_graph, parse_source, summarize
from provenance_audit import manifest, sidecar


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('root', 'ast', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.root = args.root.resolve(strict=True)
    _, entries = manifest(args.root)
    for key, entry in sorted(entries.items()):
        metadata = sidecar(args.root, entry)
        trace = metadata.get('binding_provenance', {})
        groups = trace.get('local_producers', {}).get('passes', [])
        group = next((g for g in groups if g['pass'] == 'branch_constructors' and g['records']), None)
        if group is not None:
            break
    else:
        raise ValueError('no actual constructor introduction found')
    source_path = (args.root / metadata['source_path']).resolve(strict=True)
    if not source_path.is_relative_to(args.root):
        raise ValueError('source path outside root')
    source = source_path.read_bytes()
    graph = lexical_graph(parse_source(args.ast, source), source)
    positive = attach_storage(copy.deepcopy(graph), metadata, source)
    rows = [dict(control='original', status='passed', summary=summarize(positive))]
    bid = group['records'][0]['binding_id']

    def producer(d):
        return next(g for g in d['binding_provenance']['local_producers']['passes'] if g['pass'] == 'branch_constructors')
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
        d['branch_constructors']['introduced_locals'] += 1
        d['branch_constructors']['introduced_bindings']['records'].append(copy.deepcopy(g['records'][0]))
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
        ('wrong_pass_role', lambda d: producer(d)['records'][0].update(role='evaluation_snapshot')),
        ('missing_pass_report', lambda d: d.pop('branch_constructors')),
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
    result = dict(schema_version=1, script_path=key, source_sha256=digest(source),
                  sidecar_sha256=entry['sidecar_sha256'], ast_sha256=digest(args.ast.read_bytes()),
                  rows=rows, summary=dict(collections.Counter(r['status'] for r in rows)),
                  contract='Real emitted local introductions and pinned-parser lexical bindings. One positive and eight deliberately corrupted metadata controls; no source edits or inferred input-origin proof.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(result['summary']))
    return int(any(r['status'] != 'passed' for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
