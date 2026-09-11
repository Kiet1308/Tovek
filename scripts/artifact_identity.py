#!/usr/bin/env python3
"""Require exact emitted source and detailed sidecars across two CLI builds."""
import argparse
import json
import pathlib

from pgo_corpus import portable_tree_hash
from provenance_audit import manifest
from roadmap_v2 import sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('before', 'after', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    roots = [args.before.resolve(strict=True), args.after.resolve(strict=True)]
    trees = [portable_tree_hash(root, '*.luau') for root in roots]
    if trees[0] != trees[1] or trees[0][1] == 0:
        raise ValueError('source path/content trees differ or are empty')
    artifacts = [manifest(root) for root in roots]
    manifests = [data for data, _ in artifacts]
    # repo_head is `git rev-parse HEAD` in the CLI's current working directory
    # at export time (batch.rs). It is invocation metadata, not a sidecar fact
    # or the commit from which the running executable was necessarily built.
    invocation_fields = ('command', 'tool_path', 'tool_sha256', 'repo_head', 'threads')
    filtered = [{key: value for key, value in data.items() if key not in invocation_fields} for data in manifests]
    if filtered[0] != filtered[1] or artifacts[0][1] != artifacts[1][1]:
        raise ValueError('analysis facts differ')
    for root, (record, entries) in zip(roots, artifacts):
        binary = pathlib.Path(record['command'][0])
        if not binary.samefile(record['tool_path']) or sha256(binary) != record['tool_sha256']:
            raise ValueError('actual manifest executable differs')
        command = record['command']
        requested_threads = int(command[command.index('--threads') + 1]) if '--threads' in command else 0
        if record['threads'] != requested_threads:
            raise ValueError('manifest thread count differs from invocation')
        for row in entries.values():
            if sha256(root / row['sidecar_path']) != row['sidecar_sha256']:
                raise ValueError('actual sidecar bytes differ')
    count = len(artifacts[0][1])
    report = dict(schema_version=1, status='passed', source_files=trees[0][1], sidecars=count,
                  source_tree_sha256=trees[0][0], tree_hash_model='utf8-relative-path-ordered-v1',
                  roots={label: dict(path=str(root), tool_sha256=record['tool_sha256'], invocation_repo_head=record['repo_head'],
                                    requested_threads=record['threads'])
                         for label, root, record in zip(('before', 'after'), roots, manifests)},
                  verifier_sha256=sha256(pathlib.Path(__file__)),
                  excluded_manifest_fields=list(invocation_fields),
                  contract='Exact source path/byte tree and all detailed sidecar bytes agree. Actual manifest executable identities are checked. Only top-level invocation/tool/current-directory Git identity fields are excluded and recorded separately. Source files without bytecode sidecars are counted separately. No sidecar provenance field, unknown proof, inference or output annotation is normalized away.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(dict(status='passed', source_files=trees[0][1], sidecars=count)))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
