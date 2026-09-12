#!/usr/bin/env python3
"""Independently audit emission-map binding identity with the pinned Luau parser."""
import argparse
import collections
import concurrent.futures
import json
import pathlib
import subprocess

from emission_map_audit import validate_emission_map, validate_parser_identity
from provenance_audit import manifest, sidecar, validate_trace
from roadmap_v2 import sha256
from source_fidelity import parse_ast
from value_provenance import validate as validate_value_provenance, summarize


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('root', 'ast', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    _, entries = manifest(args.root)
    def check(key):
        row = dict(script_path=key)
        try:
            metadata = sidecar(args.root, entries[key])
            path = (args.root / metadata['source_path']).resolve()
            if not path.is_relative_to(args.root.resolve()) or sha256(path) != metadata['source_sha256']:
                raise ValueError('source path/hash mismatch')
            trace = metadata['binding_provenance']
            source = path.read_bytes()
            errors = validate_trace(trace) + validate_emission_map(trace, source)
            errors.extend(validate_value_provenance(trace, source))
            identity_errors, summary = validate_parser_identity(trace, source, parse_ast(args.ast, path))
            errors.extend(identity_errors)
            row.update(status='failed' if errors else 'passed', errors=errors[:20], summary=summary,
                       value_coverage=summarize(trace),
                       source_sha256=metadata['source_sha256'], sidecar_sha256=entries[key]['sidecar_sha256'])
        except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
            row.update(status='failed', error=str(error))
        return row
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        rows = list(pool.map(check, sorted(entries)))
    totals = collections.Counter()
    value_totals = collections.Counter()
    for row in rows:
        totals.update(row.get('summary', {}))
        value_totals.update(row.get('value_coverage', {}))
    report = dict(schema_version=1, ast_sha256=sha256(args.ast),
                  manifest_sha256=sha256(args.root / '.tovek-analysis/manifest.json'),
                  summary=dict(scripts=len(rows), status=dict(collections.Counter(r['status'] for r in rows)), **totals), rows=rows,
                  value_coverage=dict(value_totals),
                  contract='Exact source token locations are resolved by the pinned parser to declaration identities. All mapped occurrences of a parser binding must have the same final IR ID. Separate lexical declarations may reuse an IR storage ID and are counted explicitly. Missing occurrences require an opaque region or an exhausted output-map budget. This is not a value/PC equivalence proof.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary'], indent=2))
    return int(not rows or any(row['status'] != 'passed' for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
