#!/usr/bin/env python3
"""Audit narrowed rehoisting without accepting unrelated corpus changes.

Unchanged source must have identical full sidecars. A changed file must be
exactly the removal of an unrecorded numeric constant declaration and replacement
of all its parser-resolved reads by its original literal spelling. No capture,
write, missing mapping, source-name loss or unrelated textual change is accepted.
This compares emitter outputs; it does not certify the original input bytecode.
"""
import argparse
import collections
import json
import pathlib
import re

from benchmark_v2 import tree_hash
from binding_graph import attach_storage, lexical_graph
from naming_metadata_audit import recorded_contract
from provenance_audit import manifest, sidecar
from roadmap_v2 import parse_ast, sha256


def literal_removals(before, after, metadata, current, ast, source_path):
    graph = lexical_graph(parse_ast(ast, source_path, 30), before)
    attach_storage(graph, metadata, before)
    retained = {row['binding_id'] for row in current['source_recovery']['bindings']}
    removed = [row for row in metadata['source_recovery']['bindings'] if row['binding_id'] not in retained]
    edits, reviews = [], []
    for binding in removed:
        if binding['origins'] or not re.fullmatch(r'(WAIT_INTERVAL|DELAY_DURATION|DISTANCE_EPSILON|DISTANCE_THRESHOLD|SOUND_ID|IMAGE_ID)(_[0-9]+)?', binding['name']):
            raise ValueError('removed binding is outside the reviewed inferred-constant family')
        declarations = [row for row in graph['declarations'] if row['storage_id'] == binding['binding_id']]
        if len(declarations) != 1:
            raise ValueError('constant lacks unique lexical declaration identity')
        row = declarations[0]
        if row['kind'] != 'local' or row['captured_in_output'] or row['written_in_output']:
            raise ValueError('constant is captured, mutated or not an ordinary local')
        start, end = row['declaration_span']
        line_start = before.rfind(b'\n', 0, start) + 1
        line_end = before.index(b'\n', end) + 1
        line = before[line_start:line_end]
        match = re.fullmatch(rb'[\t ]*local ' + re.escape(binding['name'].encode('ascii'))
                             + rb' = (-?[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)\n', line)
        if not match:
            raise ValueError('declaration is not a single numeric literal')
        literal = match[1]
        if before[line_end:line_end + 1] == b'\n':
            line_end += 1
        edits.append((line_start, line_end, b''))
        reads = []
        for token in row['tokens']:
            if token['role'] == 'declaration':
                continue
            if token['role'] != 'read' or token['type_only'] or token['owner_function'] != row['owner_function']:
                raise ValueError('nonlocal or nonvalue constant use')
            edits.append((*token['span'], literal))
            reads.append(token['span'])
        if len(reads) < 3:
            raise ValueError('reviewed constant has fewer than three reads')
        reviews.append(dict(binding_id=binding['binding_id'], declaration_id=row['declaration_id'],
                            literal=literal.decode('ascii'), declaration_span=row['declaration_span'],
                            read_spans=reads, recorded_origins=binding['origins']))
    if not reviews:
        raise ValueError('changed source has no reviewed constant removal')
    expected = before
    previous = len(before)
    for start, end, value in sorted(edits, reverse=True):
        if end > previous:
            raise ValueError('overlapping constant edits')
        expected = expected[:start] + value + expected[end:]
        previous = start
    if expected != after:
        raise ValueError('source differs beyond exact parser-bound constant inlining')
    return reviews


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('before', 'after', 'ast', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    _, first = manifest(args.before)
    _, second = manifest(args.after)
    if first.keys() != second.keys():
        raise ValueError('manifest identities differ')
    rows = []
    for key in sorted(first):
        a, b = sidecar(args.before, first[key]), sidecar(args.after, second[key])
        path_a, path_b = args.before / first[key]['source_path'], args.after / second[key]['source_path']
        source_a, source_b = path_a.read_bytes(), path_b.read_bytes()
        recorded = [recorded_contract(r) for r in a['source_recovery']['records']] == \
                   [recorded_contract(r) for r in b['source_recovery']['records']]
        bindings = {r['binding_id']: r for r in b['source_recovery']['bindings']}
        protected = all(bindings.get(r['binding_id']) == r for r in a['source_recovery']['bindings'] if r['origins'])
        captures = a['capture_effects'] == b['capture_effects']
        if not recorded or not protected or not captures or a['bytecode_sha256'] != b['bytecode_sha256']:
            raise ValueError('input/recorded-name/capture contract changed: ' + key)
        if source_a == source_b and a != b:
            raise ValueError('unchanged source has changed metadata: ' + key)
        review = []
        if source_a != source_b:
            current_graph = lexical_graph(parse_ast(args.ast, path_b, 30), source_b)
            attach_storage(current_graph, b, source_b)
            review = literal_removals(source_a, source_b, a, b, args.ast, path_a)
        rows.append(dict(script_path=key, status='passed', source_sha256=sha256(path_b),
                         source_unchanged=source_a == source_b, full_sidecar_equal=a == b,
                         recorded_contracts_equal=recorded and protected, capture_certificates_equal=captures,
                         reviewed_literal_removals=review))
        if len(rows) % 1000 == 0: print(f'{len(rows)}/{len(first)}', flush=True)
    old_files = {p.relative_to(args.before) for p in args.before.rglob('*.luau')}
    new_files = {p.relative_to(args.after) for p in args.after.rglob('*.luau')}
    if old_files != new_files:
        raise ValueError('source file identities differ')
    # Empty bytecode placeholders have no sidecar. Account for them explicitly.
    mapped = {pathlib.Path(e['source_path']) for e in first.values()}
    empty = old_files - mapped
    if any((args.before / p).read_bytes() != (args.after / p).read_bytes() for p in empty):
        raise ValueError('unmapped source changed')
    report = dict(schema_version=1, before=str(args.before), after=str(args.after),
                  ast_sha256=sha256(args.ast), source_files=len(old_files), unmapped_source_files=len(empty),
                  before_tree_hash=tree_hash(args.before, '*.luau')[0],
                  after_tree_hash=tree_hash(args.after, '*.luau')[0], rows=rows,
                  summary=dict(scripts=len(rows), status=dict(collections.Counter(r['status'] for r in rows)),
                               source_changed=sum(not r['source_unchanged'] for r in rows),
                               full_sidecars_equal=sum(r['full_sidecar_equal'] for r in rows)),
                  contract=__doc__)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary']))


if __name__ == '__main__':
    main()
