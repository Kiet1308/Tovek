#!/usr/bin/env python3
"""Real-compiler controls for exact registry matches, ambiguity and refusal."""
import argparse
import copy
import json
import os
import pathlib
import subprocess
import tempfile
import time

from source_registry import COMPILER_COMMIT, Registry, build, compile_bytes, digest, encoded, store
from source_fingerprint import MODEL, execution_image, fingerprint
from roadmap_v2 import ROOT, sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=pathlib.Path, required=True)
    parser.add_argument('--keep', type=pathlib.Path, required=True)
    parser.add_argument('--report', type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.compiler = args.compiler.resolve(strict=True)
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix='registry-controls-', dir=args.keep)).resolve()
    if not work.is_relative_to(args.keep.resolve()):
        raise ValueError('fixture work path escapes keep directory')
    vendor = work / 'vendor'
    repo = vendor / 'fixture'
    repo.mkdir(parents=True)
    source = b'''local function transform(left, right)
    local difference = left - right
    local total = left + right
    local output = { Difference = difference, Total = total }
    return output.Difference, output.Total
end
return transform
'''
    ambiguous = source.replace(b'transform', b'ambiguous')
    sources = {'unique.luau': source, 'ambiguous_a.luau': ambiguous,
               'ambiguous_b.luau': b'-- a different source text\n' + ambiguous,
               'nil_a.luau': b'return nil\n', 'nil_b.luau': b'-- different type-only source\nreturn nil\n'}
    for name, text in sources.items():
        (repo / name).write_bytes(text)
    license_text = b'Synthetic registry-test metadata; this token is not a real upstream license claim.\n'
    (repo / 'LICENSE.fixture').write_bytes(license_text)
    env = dict(os.environ, GIT_AUTHOR_NAME='Registry fixture', GIT_COMMITTER_NAME='Registry fixture',
               GIT_AUTHOR_EMAIL='fixture@example.invalid', GIT_COMMITTER_EMAIL='fixture@example.invalid',
               GIT_AUTHOR_DATE='2026-09-11T00:00:00Z', GIT_COMMITTER_DATE='2026-09-11T00:00:00Z')
    for command in (['git', 'init', str(repo)], ['git', '-C', str(repo), 'add', '.'],
                    ['git', '-C', str(repo), '-c', 'commit.gpgsign=false', 'commit', '-m', 'Synthetic registry fixtures']):
        subprocess.run(command, env=env, capture_output=True, check=True, timeout=30)
    commit = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD']).decode().strip()
    manifest = dict(schema_version=1, compiler_commit=COMPILER_COMMIT,
        repositories=[dict(name='fixture', commit=commit, url='https://example.invalid/fixture',
                           license=dict(spdx='LicenseRef-RegistryTest', path='LICENSE.fixture', sha256=digest(license_text),
                                        url='https://example.invalid/fixture/LICENSE.fixture'))],
        sources=[dict(repo='fixture', file=name, source_sha256=digest(text), split='synthetic_controls', lineage='fixture',
                      url=f'https://example.invalid/fixture/{name}') for name, text in sources.items()])
    (work / 'sources.json').write_bytes(encoded(manifest))
    config = json.loads((ROOT / 'docs/source_registry_v2.json').read_text(encoding='utf-8'))
    config['source_manifest'] = 'sources.json'
    config['profiles'] = [p for p in config['profiles'] if p['id'] in ('o0', 'o2', 'native_o2')]
    (work / 'config.json').write_bytes(encoded(config))
    registry_root = work / 'registry'
    started = time.perf_counter()
    built = build(argparse.Namespace(config=work / 'config.json', vendor=vendor, compiler=args.compiler, registry=registry_root))
    registry = Registry(registry_root, args.compiler)
    rows = []
    def require(name, condition, **evidence):
        rows.append(dict(case=name, status='passed' if condition else 'failed', **evidence))
    for profile in config['profiles']:
        raw = compile_bytes(args.compiler, source, profile)
        match = registry.match(raw)
        require('unique_' + profile['id'], match['status'] == 'matched_upstream_source', result=match['status'])
        destination = registry.materialize(match, profile['id'] + '.luau', work / 'materialized')
        require('materialize_' + profile['id'], destination is not None and
                (work / 'materialized' / destination).read_bytes().startswith(profile['source_preamble'].encode() + b'-- Tovek: matched upstream source'))
        other = compile_bytes(args.compiler, ambiguous, profile)
        result = registry.match(other)
        require('ambiguity_' + profile['id'], result['status'] == 'refused_ambiguous_source_text' and result['candidate_source_texts'] == 2,
                result=result['status'])
        nil = compile_bytes(args.compiler, sources['nil_a.luau'], profile)
        result = registry.match(nil)
        require('return_nil_' + profile['id'], result['status'] == 'refused_low_information', result=result['status'])
        fork = compile_bytes(args.compiler, source.replace(b'left - right', b'right - left'), profile)
        result = registry.match(fork)
        require('modified_fork_' + profile['id'], result['status'] == 'no_match', result=result['status'])
        pairs = [
            (source, source.replace(b'left - right', b'right - left')),
            (source, source.replace(b'return output.Difference, output.Total', b'return output.Total, output.Difference')),
            (source, source.replace(b'Difference = difference', b'Difference = total')),
        ]
        for number, (left, right) in enumerate(pairs):
            a, b = (compile_bytes(args.compiler, text, profile) for text in (left, right))
            require(f'ordered_negative_{profile["id"]}_{number}', fingerprint(a)[0] != fingerprint(b)[0],
                    image_sha256=[fingerprint(a)[0], fingerprint(b)[0]])
    before = (registry_root / 'index.json').read_bytes()
    again = build(argparse.Namespace(config=work / 'config.json', vendor=vendor, compiler=args.compiler, registry=registry_root))
    require('deterministic_registry_rebuild', (registry_root / 'index.json').read_bytes() == before and built == again)
    unique_row = next(r for r in registry.index['entries'] if r['file'] == 'unique.luau' and r['profile'] == 'o0')
    original = registry.blobs[unique_row['bytecode_artifact']]
    forged = copy.deepcopy(registry.index)
    forged['entries'] = [copy.deepcopy(unique_row)]
    row = forged['entries'][0]
    fork_source = source.replace(b'left - right', b'right - left')
    row['source_artifact'] = store(registry_root, 'sources', fork_source, '.luau')
    row['source_sha256'] = digest(fork_source)
    row['id'] = digest(encoded({k: v for k, v in row.items() if k != 'id'}))
    forged['registry_id'] = digest(encoded({k: v for k, v in forged.items() if k != 'registry_id'}))
    (registry_root / 'index.json').write_bytes(encoded(forged))
    result = Registry(registry_root, args.compiler).match(original)
    require('forged_source_relation_requires_recompile', result['status'] == 'refused_source_recompile_mismatch', result=result['status'])
    (registry_root / 'index.json').write_bytes(before)
    for kind, artifact in [('source', unique_row['source_artifact']), ('license', unique_row['license']['artifact'])]:
        path = registry_root / artifact
        previous = path.read_bytes()
        path.write_bytes(previous + b'changed')
        refused = False
        try:
            Registry(registry_root, args.compiler)
        except ValueError:
            refused = True
        path.write_bytes(previous)
        require(kind + '_hash_mismatch', refused)
    report = dict(schema_version=1, model=MODEL, compiler_sha256=sha256(args.compiler), compiler_commit_expected=COMPILER_COMMIT,
                  registry=built, elapsed_seconds=time.perf_counter() - started, work=str(work), rows=rows,
                  summary=dict(total=len(rows), passed=sum(r['status'] == 'passed' for r in rows)),
                  contract='Synthetic local git sources exercise commit/license/source pins, exact-image lookup, independently recompiled materialization, ambiguous comment variants, trivial nil, modified forks, ordered operands/results/stores, corrupt artifacts and forged source-image relations. They are not a public holdout quality score.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary'], indent=2))
    return int(any(row['status'] != 'passed' for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
