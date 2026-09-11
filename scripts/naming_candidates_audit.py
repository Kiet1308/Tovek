#!/usr/bin/env python3
"""Check bounded legacy naming evidence without treating inferred names as proof."""
import argparse
import collections
import json
import pathlib
import re

from provenance_audit import manifest, sidecar
from roadmap_v2 import sha256


def validate(inference):
    evidence = inference['legacy_candidates']
    limits = evidence['limits']
    final = {b['binding_id'] for b in inference['rows']}
    errors = []
    def require(ok, reason):
        if not ok and reason not in errors:
            errors.append(reason)
    require(evidence['enabled'], 'evidence disabled')
    require(len(evidence['rows']) <= limits['bindings'], 'binding budget exceeded')
    ids = [b['binding_id'] for b in evidence['rows']]
    require(all(re.fullmatch(r'b\d+', bid) for bid in ids), 'invalid binding ID')
    require(len(ids) == len(set(ids)), 'duplicate binding ID')
    if all(re.fullmatch(r'b\d+', bid) for bid in ids):
        require(ids == sorted(ids, key=lambda bid: int(bid[1:])), 'unstable binding order')
    for row in evidence['rows']:
        require(row['final_binding_present'] == (row['binding_id'] in final), 'incorrect final identity mapping')
        candidates = row['candidates']
        require(len(candidates) <= limits['candidates_per_binding'], 'candidate budget exceeded')
        require(len({json.dumps(c, sort_keys=True) for c in candidates}) == len(candidates), 'duplicate candidate')
        for candidate in candidates:
            require(len(candidate['name'].encode()) <= limits['name_bytes'], 'name budget exceeded')
            require(0 <= candidate['priority'] <= 255, 'invalid ordinal priority')
            require(bool(candidate['reason']), 'missing rule')
            site = candidate['rule_site']
            require(site['line'] > 0 and site['column'] > 0 and site['file'].endswith('.rs'), 'missing rule witness')
        selected = row['selected_hint']
        if selected and not row['truncated']:
            require(any(c['name'] == selected['name'] and c['priority'] == selected['priority']
                        for c in candidates), 'winner lacks candidate evidence')
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('input', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    _, entries = manifest(args.input)
    totals, rules = collections.Counter(), collections.Counter()
    rows = []
    for key, entry in sorted(entries.items()):
        inference = sidecar(args.input, entry)['name_inference']
        errors = validate(inference)
        evidence = inference['legacy_candidates']
        counts = collections.Counter(bindings=len(evidence['rows']), candidate_attempts=evidence['candidate_attempts'],
                                     omitted_attempts=evidence['omitted_attempts'],
                                     binding_budget_exhausted=evidence['binding_budget_exhausted'])
        for binding in evidence['rows']:
            counts['candidates'] += len(binding['candidates'])
            counts['bindings_with_candidates'] += bool(binding['candidates'])
            counts['bindings_with_alternatives'] += len({c['name'] for c in binding['candidates']}) > 1
            counts['bindings_with_invalidations'] += bool(binding['invalidations'])
            counts['truncated_bindings'] += binding['truncated']
            counts['final_binding_present'] += binding['final_binding_present']
            rules.update(c['reason'] for c in binding['candidates'])
        totals.update(counts)
        rows.append({'script_path': key, 'status': 'failed' if errors else 'passed', 'errors': errors, **counts})
    report = {'schema_version': 1, 'manifest_sha256': sha256(args.input / '.tovek-analysis/manifest.json'),
              'summary': {'scripts': len(rows), 'status': dict(collections.Counter(r['status'] for r in rows)), **totals},
              'rules': dict(sorted(rules.items())), 'rows': rows,
              'limitations': 'Pre-cleanup proposals can refer to bindings absent from the final AST; no transfer by spelling. Ordinal priorities do not measure name accuracy or prove value semantics.'}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary'], indent=2))
    return int(not rows or any(r['errors'] for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
