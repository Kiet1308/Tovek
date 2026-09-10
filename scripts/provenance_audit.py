#!/usr/bin/env python3
"""Validate optional lineage records and exact output/previous-metadata preservation.

This checks trace consistency, not value equivalence. It streams manifest-selected
sidecars; abandoned content-addressed files are not counted a second time.
"""
import argparse
import collections
import json
import pathlib
import re

from roadmap_v2 import sha256


def validate_trace(trace):
    errors = []
    def require(ok, reason):
        if not ok and len(errors) < 20:
            errors.append(reason)

    require(trace['schema_version'] == 1, 'unsupported schema')
    final = {b['binding_id']: b for b in trace['final_bindings']}
    require(len(final) == len(trace['final_bindings']), 'duplicate final binding')
    origins = {}
    selects = set()
    for f in trace['functions']:
        require(sum(len(f.get(k, [])) for k in ('registers', 'definitions', 'lifted_statements', 'local_maps', 'conditional_results'))
                <= trace['limits']['records_per_function'], 'function record budget exceeded')
        registers = {r['id']: r for r in f['registers']}
        definitions = {d['id']: d for d in f['definitions']}
        require(len(registers) == len(f['registers']), 'duplicate register')
        require(len(definitions) == len(f['definitions']), 'duplicate definition')
        sites = {(s['block'], s['statement_index']): s for s in f['lifted_statements']}
        require(len(sites) == len(f['lifted_statements']), 'duplicate lifted statement')
        for s in sites.values():
            pcs = s['instruction_pcs']
            require(pcs == sorted(set(pcs)), 'PC set unordered or repeated')
            require(all(0 <= pc < f['instruction_count'] for pc in pcs), 'PC outside prototype')
            require(s['source_lines'] == sorted(set(s['source_lines'])), 'line set unordered or repeated')
            if not f['dropped_records']:
                require(set(s['read_registers'] + s['written_registers']) <= registers.keys(), 'unknown lifted register')
        for d in definitions.values():
            if not f['dropped_records']:
                require(d['original_register'] in registers, 'unknown definition register')
                require(set(d['dependencies']) <= registers.keys() | definitions.keys(), 'unknown definition dependency')
                if d['statement_index'] is not None:
                    s = sites.get((d['block'], d['statement_index']))
                    require(s is not None, 'definition has no lifted statement')
                    if s:
                        writes = s['written_registers']
                        i = d['write_index']
                        require(i is not None and i < len(writes) and writes[i] == d['original_register'], 'definition write slot changed')
        for s in f['conditional_results']:
            selects.add(s['binding_id'])
            require(s['then_predecessor'] != s['else_predecessor'], 'select predecessors equal')
            require(len({s['binding_id'], s['then_value'], s['else_value']}) == 3, 'trivial or self phi claimed')
            if s['phase'] == 'constructed_ssa' and not f['dropped_records']:
                require(s['binding_id'] in definitions, 'select has no phi definition')
            require(set(s['final_bindings']) <= final.keys(), 'select maps to missing final binding')
            require(all(s['binding_id'] in final[b]['lineage'] for b in s['final_bindings']), 'select reverse mapping changed')
        for origin in [*registers.values(), *definitions.values()]:
            require(origin['id'] not in origins, 'origin ID reused across functions')
            origins[origin['id']] = origin
            require(set(origin['final_bindings']) <= final.keys(), 'origin maps to missing binding')
            require(all(origin['id'] in final[b]['lineage'] for b in origin['final_bindings']), 'reverse lineage mapping changed')
    for bid, b in final.items():
        require(re.fullmatch(r'b\d+', bid) is not None, 'non-string binding ID')
        lineage = b['lineage']
        require(len(lineage) <= trace['limits']['ancestry_per_binding'], 'lineage budget exceeded')
        require(lineage == sorted(set(lineage), key=lambda x: int(x[1:])), 'lineage unordered or repeated')
        missing = set(lineage) - origins.keys()
        require(not missing or b['incomplete'], 'unknown origins claimed complete')
        if lineage:
            require(set(b['unknown_origins']) == missing, 'unknown-origin inventory changed')
            require(b['has_conditional_result_ancestry'] == bool(selects.intersection(lineage)), 'conditional ancestry flag changed')
        else:
            require(b['incomplete'], 'empty lineage claimed complete')
        for origin in set(lineage) & origins.keys():
            require(bid in origins[origin]['final_bindings'], 'forward lineage mapping changed')
    return errors


def manifest(root):
    data = json.loads((root / '.tovek-analysis/manifest.json').read_text(encoding='utf-8'))
    entries = {r['script_path']: r for r in data['scripts']}
    if len(entries) != len(data['scripts']):
        raise ValueError('duplicate manifest script identity')
    return data, entries


def sidecar(root, entry):
    path = (root / entry['sidecar_path']).resolve()
    if not path.is_relative_to(root.resolve()) or sha256(path) != entry['sidecar_sha256']:
        raise ValueError('invalid sidecar path or hash')
    return json.loads(path.read_text(encoding='utf-8'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('before', 'after', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    am, a = manifest(args.before)
    bm, b = manifest(args.after)
    source_paths = set(am['generated_source_paths']) | set(bm['generated_source_paths'])
    source_set_equal = set(am['generated_source_paths']) == set(bm['generated_source_paths'])
    changed_sources = [p for p in sorted(source_paths) if not (args.before / p).is_file()
                       or not (args.after / p).is_file() or (args.before / p).read_bytes() != (args.after / p).read_bytes()]
    totals, rows, examples = collections.Counter(), [], []
    for key in sorted(a.keys() | b.keys()):
        row = {'script_path': key, 'status': 'passed'}
        if key not in a or key not in b:
            rows.append(row | {'status': 'missing_metadata'})
            continue
        left, right = sidecar(args.before, a[key]), sidecar(args.after, b[key])
        ignored = {'binding_provenance', 'analysis_id', 'decompile_option_bits'}
        if {k: v for k, v in left.items() if k not in ignored} != {k: v for k, v in right.items() if k not in ignored}:
            row['status'] = 'prior_metadata_changed'
        if left['decompile_option_bits'] & ~16 != right['decompile_option_bits'] & ~16:
            row['status'] = 'source_options_changed'
        trace = right.get('binding_provenance')
        if trace is None:
            row['status'] = 'missing_trace'
        else:
            errors = validate_trace(trace)
            if errors:
                row.update(status='invalid_trace', errors=errors)
            row['summary'] = trace['summary']
            totals.update(trace['summary'])
            if key.startswith('conditional_O2_'):
                examples.append({'script_path': key, 'trace': trace})
        rows.append(row)
    report = {'schema_version': 1, 'summary': {'scripts': len(rows), 'source_files': len(source_paths),
              'source_set_equal': source_set_equal, 'source_files_changed': len(changed_sources),
              'status': dict(collections.Counter(r['status'] for r in rows)), **totals},
              'before_manifest_sha256': sha256(args.before / '.tovek-analysis/manifest.json'),
              'after_manifest_sha256': sha256(args.after / '.tovek-analysis/manifest.json'),
              'changed_sources': changed_sources, 'rows': rows, 'examples': examples,
              'contract': 'Exact source bytes and all prior sidecar fields except analysis identity/options are preserved. Trace checks cover PC bounds, write slots, ID uniqueness and bidirectional ancestry. No semantic proof is inferred from storage lineage.'}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary'], indent=2))
    return int(not rows or not source_set_equal or bool(changed_sources) or any(r['status'] != 'passed' for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
