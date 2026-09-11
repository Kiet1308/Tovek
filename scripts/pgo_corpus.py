#!/usr/bin/env python3
"""Freeze PGO train/holdout inputs without training on evaluation workloads."""
import argparse
import base64
import collections
import hashlib
import json
import pathlib
import tempfile

from roadmap_v2 import sha256
from source_fingerprint import fingerprint
from source_registry import read_input


def portable_tree_hash(root, pattern):
    digest, count = hashlib.sha256(), 0
    paths = sorted(root.rglob(pattern), key=lambda path: path.relative_to(root).as_posix().encode('utf-8'))
    for path in paths:
        for value in (path.relative_to(root).as_posix().encode('utf-8'), path.read_bytes()):
            digest.update(len(value).to_bytes(8, 'little')); digest.update(value)
        count += 1
    return digest.hexdigest(), count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('public-report', 'runtime-report', 'private', 'keep', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    public = json.loads(args.public_report.read_text(encoding='utf-8'))
    runtime = json.loads(args.runtime_report.read_text(encoding='utf-8'))
    if any(row['status'] != 'passed' for row in public['rows'] + runtime['cases']):
        raise ValueError('input reports must pass')
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix='corpus-', dir=args.keep)).resolve()
    rows, training_images = [], set()
    def publish(dataset, relative, raw, *, key, trailer=0, source_hash=None, source_group=None):
        image, facts = fingerprint(raw, key, trailer)
        row = dict(dataset=dataset, path=relative, bytecode_sha256=hashlib.sha256(raw).hexdigest(),
                   execution_image_sha256=image, opaque_trailer_bytes=facts['opaque_trailer_bytes'],
                   source_sha256=source_hash, source_group=source_group, status='included')
        if dataset == 'train': training_images.add(image)
        elif image in training_images: row['status'] = 'excluded_exact_training_image'
        if row['status'] == 'included':
            path = (work / dataset / relative).resolve()
            if not path.is_relative_to(work / dataset) or path.exists(): raise ValueError('duplicate or escaping corpus path')
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(base64.b64encode(raw))
        rows.append(row)
    for row in public['rows']:
        if row['split'] != 'development': continue
        raw = pathlib.Path(row['output'].removesuffix('.out.luau') + '.luaubc').read_bytes()
        relative = f"public/{row['repo']}/O{row['opt']}/" + str(pathlib.PurePosixPath(row['file']).with_suffix('.lua'))
        publish('train', relative, raw, key=1, source_hash=row['source_sha256'], source_group=row['repo'])
    for row in runtime['cases']:
        name = f"{row['case']}_O{row['opt']}_g{row['debug']}"
        raw = (pathlib.Path(runtime['work']) / name / 'input.luaubc').read_bytes()
        publish('train', 'runtime/' + name + '.lua', raw, key=1, source_hash=row['source_sha256'], source_group='runtime_development')
    for row in public['rows']:
        if row['split'] != 'holdout': continue
        raw = pathlib.Path(row['output'].removesuffix('.out.luau') + '.luaubc').read_bytes()
        relative = f"{row['repo']}/O{row['opt']}/" + str(pathlib.PurePosixPath(row['file']).with_suffix('.lua'))
        publish('holdout', relative, raw, key=1, source_hash=row['source_sha256'], source_group=row['repo'])
    for path in sorted(args.private.rglob('*.lua')):
        raw, _ = read_input(path, saved=True)
        if not raw:
            rows.append(dict(dataset='private', path=path.relative_to(args.private).as_posix(), status='excluded_empty'))
            continue
        publish('private', path.relative_to(args.private).as_posix(), raw, key=203, trailer=24)
    train_sources = {r['source_sha256'] for r in rows if r['dataset'] == 'train' and r['source_sha256']}
    if any(r.get('source_sha256') in train_sources for r in rows if r['dataset'] == 'holdout'):
        raise ValueError('source overlap across public repository holdout')
    groups = {label: dict(collections.Counter(r['status'] for r in rows if r['dataset'] == label))
              for label in ('train', 'holdout', 'private')}
    hashes = {label: dict(zip(('sha256', 'files'), portable_tree_hash(work / label, '*.lua'))) for label in groups}
    report = dict(schema_version=1, work=str(work), inputs=dict(public_report=sha256(args.public_report),
                  runtime_report=sha256(args.runtime_report)), summary=groups, trees=hashes, rows=rows,
                  tree_hash_model='utf8-relative-path-ordered-v1',
                  training_runs=[dict(api='decompile_batch_with_options', threads=threads, rounds=2) for threads in (1, 16)],
                  evaluation=dict(rounds=7, threads=[1,16], primary_workload='private', secondary_workload='holdout',
                      target_median_reduction_percent=10, allowed_p95_regression_percent=5, allowed_rss_regression_percent=5,
                      allowed_holdout_median_regression_percent=5, default_promotion=False),
                  contract='Training uses public development repositories and runtime fixtures only. Rodux repository holdout and the private workload never train profiles. Cross-split exact execution images are excluded after opcode decoding with explicit container trailers; known source hashes also cannot overlap public holdout. Private source lineage is unknown, so private is an external workload, not a certified independent source-family holdout. Each platform trains its own binary/profile in isolated target directories; all builds retain panic=unwind. PGO remains experimental regardless of one measurement.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(groups))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
