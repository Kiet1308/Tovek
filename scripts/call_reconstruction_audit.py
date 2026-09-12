#!/usr/bin/env python3
"""Audit committed call events, parser spans and comment-only compact display."""
import argparse
import bisect
import collections
import concurrent.futures
import copy
import hashlib
import json
import pathlib

from call_reconstruction import compact_annotation, validate, validate_parser_calls
from emission_map_audit import validate_emission_map, validate_parser_identity
from provenance_audit import manifest, sidecar, validate_trace
from roadmap_v2 import sha256
from source_fidelity import canonicalize, parse_ast


def source_at(root, metadata):
    path = (root / metadata['source_path']).resolve(strict=True)
    if not path.is_relative_to(root.resolve()) or sha256(path) != metadata['source_sha256']:
        raise ValueError('source path/hash differs')
    return path, path.read_bytes()


def compact_comparison(full, compact, original, output):
    """Translate every stored output position by only the retained comment edits."""
    annotations = full['binding_provenance']['output_map']['annotations']
    after = compact['binding_provenance']['output_map']['annotations']
    if len(annotations) != len(after): raise ValueError('annotation coverage differs')
    edits = []
    expected = copy.deepcopy(full)
    for old, new, target in zip(annotations, after, expected['binding_provenance']['output_map']['annotations']):
        displayed = new.get('displayed_text')
        if displayed is not None:
            if old['text_truncated'] or compact_annotation(old['text']) != displayed:
                raise ValueError('compact text has no complete retained original')
            a, b = old['span']['start']['byte_offset'], old['span']['end']['byte_offset']
            edits.append((a, b, ('-- ' + displayed).encode('utf-8')))
            target['displayed_text'] = displayed
    predicted = original
    for a, b, replacement in reversed(edits): predicted = predicted[:a] + replacement + predicted[b:]
    if predicted != output: raise ValueError('compact mode changed bytes beyond recorded comments')
    starts = [0] + [i + 1 for i, byte in enumerate(output) if byte == 10]
    def translate_offset(old):
        if any(a < old < b for a, b, _ in edits):
            raise ValueError('non-boundary source position inside replaced comment')
        return old + sum(len(replacement) - (b - a) for a, b, replacement in edits if b <= old)
    def translate(value):
        if isinstance(value, dict):
            if set(value) == {'byte_offset', 'line_one_based', 'column_one_based'}:
                offset = translate_offset(value['byte_offset'])
                line = bisect.bisect_right(starts, offset)
                value.update(byte_offset=offset, line_one_based=line,
                             column_one_based=len(output[starts[line - 1]:offset].decode('utf-8')) + 1)
            else:
                for child in value.values(): translate(child)
        elif isinstance(value, list):
            for child in value: translate(child)
    translate(expected)
    # These fields use byte offsets rather than SourcePosition objects. Only
    # translate this documented output inventory; input PCs and value paths
    # elsewhere in the metadata are not positions in the emitted source.
    for region in expected['binding_provenance'].get('value_provenance', {}).get('output_regions', []):
        for field in ('start_byte', 'end_byte'):
            region[field] = translate_offset(region[field])
    expected['decompile_option_bits'] |= 64
    expected['source_sha256'] = compact['source_sha256']  # Independently checked against bytes.
    # Identity hashes include options; they are not semantic certificates.
    identity = (expected['bytecode_artifact_id'].encode() + expected['tovek_version'].encode()
                + expected['schema_version'].to_bytes(4, 'little')
                + expected['decompile_option_bits'].to_bytes(4, 'little'))
    expected['analysis_id'] = 'sha256:' + hashlib.sha256(identity).hexdigest()
    if expected != compact: raise ValueError('compact mode changed metadata beyond options/hash/comment positions')
    return len(edits)


def controls(trace, source, tree):
    report = trace['call_reconstruction']
    if not report['occurrences']: raise ValueError('no real call occurrence for controls')
    def change_callee(r):
        row = r['occurrences'][0]
        row['current_callee_binding'] = next(b['binding_id'] for b in trace['final_bindings']
                                             if b['binding_id'] != row['current_callee_binding'])
    def clip_call(r):
        end = r['occurrences'][0]['span']['end']
        end['byte_offset'] -= 1
        end['column_one_based'] -= 1
    mutations = [
        ('invented_caller_pc', lambda r: r['events'][0].update(caller_pc=0)),
        ('duplicate_event_id', lambda r: r['events'].append(copy.deepcopy(r['events'][0]))),
        ('dangling_event', lambda r: r['occurrences'][0].update(event_id=len(r['events']) + 1)),
        ('wrong_existing_callee', change_callee), ('clipped_call_extent', clip_call),
        ('duplicate_occurrence', lambda r: r['occurrences'].append(copy.deepcopy(r['occurrences'][0]))),
        ('false_omission', lambda r: r.update(omitted_events=1)),
    ]
    rows = [dict(control='original', status='passed' if not validate(trace, source) + validate_parser_calls(trace, source, tree) else 'failed')]
    for name, mutate in mutations:
        changed = copy.deepcopy(trace)
        mutate(changed['call_reconstruction'])
        errors = validate(changed, source)
        if not errors: errors = validate_parser_calls(changed, source, tree)
        rows.append(dict(control=name, status='passed' if errors else 'failed', errors=errors[:2]))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('root', 'ast', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    parser.add_argument('--before', type=pathlib.Path, help='require old source and all existing metadata to remain equal')
    parser.add_argument('--compact-root', type=pathlib.Path)
    args = parser.parse_args()
    _, entries = manifest(args.root)
    before = manifest(args.before)[1] if args.before else None
    compact = manifest(args.compact_root)[1] if args.compact_root else None
    if any(other is not None and entries.keys() != other.keys() for other in (before, compact)):
        raise ValueError('script inventory differs')
    def check(key):
        row = dict(script_path=key, status='failed')
        try:
            metadata = sidecar(args.root, entries[key])
            path, source = source_at(args.root, metadata)
            trace = metadata['binding_provenance']
            errors = validate_trace(trace) + validate_emission_map(trace, source)
            if errors: raise ValueError('; '.join(errors[:4]))
            report = trace['call_reconstruction']
            if report['occurrences']:
                tree = parse_ast(args.ast, path)
                errors = validate_parser_calls(trace, source, tree)
                if errors: raise ValueError('; '.join(errors[:4]))
            else: tree = None
            if before is not None:
                old = sidecar(args.before, before[key])
                _, previous = source_at(args.before, old)
                filtered = copy.deepcopy(metadata)
                filtered['binding_provenance'].pop('call_reconstruction')
                if previous != source or old != filtered:
                    raise ValueError('default source or previous metadata changed')
            if compact is not None:
                short = sidecar(args.compact_root, compact[key])
                short_path, output = source_at(args.compact_root, short)
                errors = validate_trace(short['binding_provenance']) + validate_emission_map(short['binding_provenance'], output)
                if errors: raise ValueError('; '.join(errors[:4]))
                row['compacted_comments'] = compact_comparison(metadata, short, source, output)
                if source != output:
                    if tree is None: tree = parse_ast(args.ast, path)
                    short_tree = parse_ast(args.ast, short_path)
                    if canonicalize(tree) != canonicalize(short_tree):
                        raise ValueError('compact output changed parsed structure, binding names or type syntax')
                    errors, _ = validate_parser_identity(short['binding_provenance'], output, short_tree)
                    if errors: raise ValueError('; '.join(errors[:4]))
            observed = collections.Counter(r['event_id'] for r in report['occurrences'])
            row.update(status='passed', source_sha256=metadata['source_sha256'], sidecar_sha256=entries[key]['sidecar_sha256'],
                events=len(report['events']), occurrences=len(report['occurrences']),
                producers=dict(collections.Counter(r['producer'] for r in report['events'])),
                with_callee_prototype=sum(r['callee_prototype'] is not None for r in report['events']),
                events_without_output_occurrence=sum(r['event_id'] not in observed for r in report['events']),
                copied_events=sum(count > 1 for count in observed.values()),
                omitted={k: report[k] for k in ('omitted_events', 'omitted_occurrences', 'omitted_callee_registrations')})
        except (ValueError, KeyError, OSError, TypeError) as error:
            row['error'] = str(error)
        return row
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        rows = list(pool.map(check, sorted(entries)))
    totals, producers, omissions = collections.Counter(), collections.Counter(), collections.Counter()
    for row in rows:
        totals.update({k: row[k] for k in ('events', 'occurrences', 'with_callee_prototype', 'events_without_output_occurrence', 'copied_events', 'compacted_comments') if k in row})
        producers.update(row.get('producers', {})); omissions.update(row.get('omitted', {}))
    control_rows = []
    example = next((r for r in rows if r['status'] == 'passed' and r['occurrences']), None)
    if example:
        metadata = sidecar(args.root, entries[example['script_path']])
        path, source = source_at(args.root, metadata)
        control_rows = controls(metadata['binding_provenance'], source, parse_ast(args.ast, path))
    result = dict(schema_version=1, ast_sha256=sha256(args.ast),
                  manifest_sha256=sha256(args.root / '.tovek-analysis/manifest.json'),
                  summary=dict(scripts=len(rows), statuses=dict(collections.Counter(r['status'] for r in rows)),
                               **totals, producers=dict(producers), omitted=dict(omissions)),
                  rows=rows, controls=control_rows,
                  contract='Creation events and exact parser call extents are diagnostic facts. Callee prototypes are not caller PCs. Missing occurrences remain unclassified; cloned occurrences share event IDs. Optional old-sidecar check excludes only the new call_reconstruction report. Compact comparison preserves every other byte and translates every metadata position by only those comment edits; parser structure, names and type syntax must agree.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(result['summary']))
    return int(not rows or any(r['status'] != 'passed' for r in rows + control_rows))


if __name__ == '__main__':
    raise SystemExit(main())
