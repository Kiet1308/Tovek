#!/usr/bin/env python3
"""Resolve an emitted identifier offset to bounded storage-origin PC sets."""
import argparse
import json
import pathlib

from provenance_audit import manifest, sidecar, validate_trace
from emission_map_audit import validate_emission_map
from roadmap_v2 import sha256


def lookup(trace, offset):
    output = trace['output_map']
    contains = lambda span: span['start']['byte_offset'] <= offset < span['end']['byte_offset']
    occurrences = [item for item in output['bindings'] if contains(item['span'])]
    final = {item['binding_id']: item for item in trace['final_bindings']}
    origin_sites = {}
    for function in trace['functions']:
        common = dict(function_id=function['function_id'], prototype=function['prototype'])
        sites = {(s['block'], s['statement_index']): s for s in function['lifted_statements']}
        for register in function['registers']:
            origin_sites[register['id']] = dict(**common, kind=register['kind'], register_slot=register['slot'],
                                               instruction_pcs=[], status='input_storage_without_definition_site')
        for definition in function['definitions']:
            site = sites.get((definition['block'], definition['statement_index']))
            origin_sites[definition['id']] = dict(**common, kind=definition['kind'],
                register_id=definition['original_register'], block=definition['block'],
                statement_index=definition['statement_index'], write_index=definition['write_index'],
                instruction_pcs=site['instruction_pcs'] if site else [], source_lines=site['source_lines'] if site else [],
                status='lifted_statement_cluster' if site else 'block_parameter_or_missing_site')
    rows = []
    for occurrence in occurrences:
        binding = final[occurrence['binding_id']]
        rows.append(dict(occurrence=occurrence, binding=binding,
                         origins=[dict(origin_id=origin, **origin_sites.get(origin, dict(status='unknown_origin', instruction_pcs=[])))
                                  for origin in binding['lineage']]))
    return dict(schema_version=1, byte_offset=offset, identifiers=rows,
                annotations=[item for item in output['annotations'] if contains(item['span'])],
                opaque_regions=[item for item in output['opaque_regions'] if contains(item['span'])],
                omitted_occurrences=output['omitted_occurrences'],
                contract='Identifier-to-final-binding mapping is exact at emitted tokens. PC sets belong to all retained storage ancestors, not uniquely to this value use. Missing sites, coalesced ancestors and unknown origins remain explicit. No source spelling, inlining, close, ownership or value-equivalence proof is inferred.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=pathlib.Path, required=True)
    parser.add_argument('--script', required=True)
    parser.add_argument('--byte-offset', type=int, required=True)
    args = parser.parse_args()
    _, entries = manifest(args.root)
    if args.script not in entries:
        parser.error('script identity is absent from the manifest')
    metadata = sidecar(args.root, entries[args.script])
    path = (args.root / metadata['source_path']).resolve()
    if not path.is_relative_to(args.root.resolve()) or sha256(path) != metadata['source_sha256']:
        parser.error('mapped source path/hash mismatch')
    source = path.read_bytes()
    if not 0 <= args.byte_offset < len(source):
        parser.error('byte offset is outside source')
    trace = metadata.get('binding_provenance')
    if not trace or 'output_map' not in trace:
        parser.error('regenerate metadata with --emit-binding-provenance')
    errors = validate_trace(trace) + validate_emission_map(trace, source)
    if errors:
        parser.error('; '.join(errors[:5]))
    print(json.dumps(lookup(trace, args.byte_offset), indent=2, ensure_ascii=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
