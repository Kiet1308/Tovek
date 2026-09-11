#!/usr/bin/env python3
"""Prepare isolated baseline/API-instrumented builds and lock training outputs."""
import argparse
import json
import os
import pathlib
import shutil
import subprocess
import tempfile

from benchmark_v2 import tree_hash
from pgo_corpus import portable_tree_hash
from pgo_evaluate import validate_corpus
from pgo_finish import build_source_hash
from roadmap_v2 import sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('source', 'corpus', 'keep', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    parser.add_argument('--target', required=True, choices=('x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu'))
    parser.add_argument('--commit', required=True, help='identity of the reviewed source checkout/archive')
    parser.add_argument('--toolchain', default='nightly-2024-12-15')
    args = parser.parse_args()
    if (os.name == 'nt') != (args.target == 'x86_64-pc-windows-msvc'):
        parser.error('prepare and run on the native target platform')
    source = args.source.resolve(strict=True)
    corpus = json.loads(args.corpus.read_text(encoding='utf-8'))
    validate_corpus(corpus)
    train = pathlib.Path(corpus['work']) / 'train'
    hash_tree = portable_tree_hash if corpus.get('tree_hash_model') == 'utf8-relative-path-ordered-v1' else tree_hash
    if hash_tree(train, '*.lua') != (corpus['trees']['train']['sha256'], corpus['trees']['train']['files']):
        raise ValueError('training tree changed')
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix='pgo-', dir=args.keep)).resolve()
    # Rust splits RUSTFLAGS on whitespace, so these absolute profile paths must
    # not contain spaces. Keep source paths unrestricted (subprocess arg lists).
    if any(c.isspace() for c in str(work)):
        raise ValueError('choose a --keep path without whitespace for LLVM profile flags')
    config = dict(commit=args.commit, work=str(work), source=str(source), target=args.target,
                  toolchain=args.toolchain, base_rustflags='-Cpanic=unwind', source_hash=build_source_hash(source))
    suffix = '.exe' if os.name == 'nt' else ''
    env = {k: v for k, v in os.environ.items()
           if not k.upper().startswith('MEDAL_') and k.upper() not in ('DEINLINE_ANCHOR_TRACE', 'LLVM_PROFILE_FILE', 'CARGO_ENCODED_RUSTFLAGS')}
    if os.name != 'nt': env['PATH'] = str(pathlib.Path.home() / '.cargo/bin') + ':' + env['PATH']
    env['RUSTFLAGS'] = config['base_rustflags']
    base = ['cargo', '+' + args.toolchain, 'build', '--release', '--locked', '--target', args.target,
            '--target-dir', str(work / 'target'), '-p', 'luau-lifter']
    builds = []
    for label, selector, name in (('baseline', '--bin', 'luau-lifter'), ('train-api', '--example', 'benchmark_api')):
        if label == 'train-api': env['RUSTFLAGS'] += ' -Cprofile-generate=' + str(work / 'profiles')
        command = base + [selector, name]
        with (work / (label + '-build.log')).open('w') as log:
            subprocess.run(command, cwd=source, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
        product = work / 'target' / args.target / 'release'
        if selector == '--example': product /= 'examples'
        output = work / (label + suffix)
        shutil.copyfile(product / (name + suffix), output)
        if os.name != 'nt': output.chmod(0o755)
        builds.append(dict(command=command, rustflags=env['RUSTFLAGS'], binary_sha256=sha256(output),
                           log_sha256=sha256(work / (label + '-build.log'))))
        print(label + ' ready', flush=True)
    golden = work / 'training-baseline'
    with (work / 'training-baseline.log').open('w') as log:
        subprocess.run([str(work / ('baseline' + suffix)), 'decompile-folder', str(train), str(golden), '--key', '1',
                        '--threads', '1', '--strict-no-synthetic-control'], env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    scripts = [dict(path=path.relative_to(train).as_posix(), input_sha256=sha256(path),
                    source_sha256=sha256((golden / path.relative_to(train)).with_suffix('.luau')), groups=['all'])
               for path in sorted(train.rglob('*.lua'), key=lambda p: p.relative_to(train).as_posix().encode('utf-8'))]
    if build_source_hash(source) != config['source_hash']:
        raise ValueError('source changed during preparation')
    if hash_tree(train, '*.lua') != (corpus['trees']['train']['sha256'], corpus['trees']['train']['files']):
        raise ValueError('training inputs changed during preparation')
    (work / 'training-api.json').write_text(json.dumps(dict(schema_version=1, decode_key=1, scripts=scripts), indent=1) + '\n',
                                            encoding='utf-8', newline='\n')
    config.update(builds=builds, corpus_sha256=sha256(args.corpus), api_manifest_sha256=sha256(work / 'training-api.json'))
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(config, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(str(work))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
