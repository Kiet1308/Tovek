#!/usr/bin/env python3
"""Recompile the registry matrix and audit declared private-corpus coverage."""
import argparse
import collections
import json
import pathlib
import time

from source_registry import Registry, compile_bytes, read_input
from roadmap_v2 import sha256, fixture_path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('registry', 'compiler', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    parser.add_argument('--corpus-report', type=pathlib.Path)
    parser.add_argument('--corpus', type=pathlib.Path)
    parser.add_argument('--prior-evidence', type=pathlib.Path)
    parser.add_argument('--materialize', type=pathlib.Path)
    args = parser.parse_args()
    if args.materialize and (not args.corpus_report or not args.corpus):
        parser.error('--materialize requires the corpus and its query report')
    args.compiler = args.compiler.resolve(strict=True)
    started = time.perf_counter()
    registry = Registry(args.registry, args.compiler)
    rows = []
    for entry in registry.index['entries']:
        raw = compile_bytes(args.compiler, registry.blobs[entry['source_artifact']], registry.profiles[entry['profile']])
        match = registry.match(raw)
        if entry['facts']['low_information']:
            valid = match['status'] == 'refused_low_information'
        elif match['status'] == 'refused_ambiguous_source_text':
            valid = entry['id'] in match['candidates'] and match['candidate_source_texts'] > 1
        else:
            valid = match['status'] == 'matched_upstream_source' and any(m['id'] == entry['id'] for m in match['matches'])
        rows.append(dict(entry_id=entry['id'], repo=entry['repo'], file=entry['file'], profile=entry['profile'],
                         split=entry['split'], status='passed' if valid else 'failed', lookup_status=match['status'],
                         candidate_source_texts=match['candidate_source_texts']))
    groups = {}
    for family in sorted({r['repo'] for r in rows}):
        group = [r for r in rows if r['repo'] == family]
        groups[family] = dict(source_files=len({r['file'] for r in group}), configurations=len(group),
                             lookup_status=dict(collections.Counter(r['lookup_status'] for r in group)))
    private, evidence_checks = None, []
    if args.corpus_report:
        corpus = json.loads(args.corpus_report.read_text(encoding='utf-8'))
        if corpus['registry_id'] != registry.index['registry_id']:
            parser.error('corpus query used a different registry')
        by_file = {r['file']: r for r in corpus['rows']}
        matches = [r for r in corpus['rows'] if r['status'] == 'matched_upstream_source']
        materialized = []
        if args.materialize:
            for row in matches:
                raw, artifact_hash = read_input(fixture_path(args.corpus, row['file']), saved=True)
                fresh = registry.match(raw, corpus['input_key'], max(corpus['allowed_opaque_trailer_bytes']))
                if artifact_hash != row['input_artifact_sha256'] or fresh['status'] != 'matched_upstream_source' or \
                        fresh['image_sha256'] != row['image_sha256'] or fresh['bytecode_sha256'] != row['bytecode_sha256'] or \
                        fresh['matches'] != row['matches']:
                    raise ValueError('corpus witness differs from current input/registry')
                materialized.append(dict(file=row['file'], path=registry.materialize(fresh, row['file'], args.materialize)))
        private = dict(report_sha256=sha256(args.corpus_report), summary=corpus['summary'],
                       unique_matched_input_images=len({r['image_sha256'] for r in matches}),
                       unique_matched_source_texts=len({m['source_sha256'] for r in matches for m in r['matches']}),
                       matched_sources_by_family={family: len({m['source_sha256'] for r in matches for m in r['matches'] if m['repo'] == family})
                                                  for family in groups}, materialized=materialized)
        if args.prior_evidence:
            prior = json.loads(args.prior_evidence.read_text(encoding='utf-8'))['library_matches']['matches']
            for item in prior:
                row = by_file.get(item['corpus'])
                valid = row is not None and (not item['trivial_return_nil'] or row['status'] == 'refused_low_information')
                evidence_checks.append(dict(file=item['corpus'], trivial_return_nil=item['trivial_return_nil'],
                                            status='passed' if valid else 'failed', current_status=row['status'] if row else 'missing'))
    report = dict(schema_version=1, registry_id=registry.index['registry_id'], compiler_sha256=sha256(args.compiler),
                  elapsed_seconds=time.perf_counter() - started,
                  summary=dict(total=len(rows), status=dict(collections.Counter(r['status'] for r in rows))),
                  groups=groups, rows=rows, private_corpus=private, historical_candidate_checks=evidence_checks,
                  contract='Public matrix results are independent recompilation and registry self-consistency checks, not holdout generalization or semantic runtime coverage. Private coverage keeps unmatched/ambiguous/low-information cases in the denominator. Previously observed nil collisions must refuse source selection; stricter metadata retention may reduce other historical candidate matches.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(dict(summary=report['summary'], groups=groups, private_corpus=private), indent=2))
    return int(any(r['status'] != 'passed' for r in rows + evidence_checks))


if __name__ == '__main__':
    raise SystemExit(main())
