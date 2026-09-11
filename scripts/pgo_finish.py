#!/usr/bin/env python3
"""Collect API PGO profiles and build a separate optimized CLI with unwind intact."""
import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess

from benchmark_v2 import tree_hash
from pgo_corpus import portable_tree_hash
from pgo_evaluate import validate_corpus
from roadmap_v2 import sha256


def build_source_hash(source):
    files = [source / 'Cargo.toml', source / 'Cargo.lock']
    for name in ('ast', 'cfg', 'restructure', 'luau-lifter', 'lua51-lifter', 'lua51-deserializer', 'luau-worker', 'web-server'):
        files += [path for path in (source / name).rglob('*') if path.is_file()
                  and path.suffix in ('.rs', '.toml', '.lock', '.c', '.h', '.cpp', '.S')]
    digest = hashlib.sha256()
    for path in sorted(files):
        for value in (path.relative_to(source).as_posix().encode(), path.read_bytes()):
            digest.update(len(value).to_bytes(8, 'little')); digest.update(value)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('stage', choices=('collect', 'build'))
    for name in ('prepared', 'source', 'corpus', 'api-manifest'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    config = json.loads(args.prepared.read_text(encoding='utf-8'))
    corpus = json.loads(args.corpus.read_text(encoding='utf-8'))
    validate_corpus(corpus)
    work = pathlib.Path(config['work']).resolve(strict=True)
    source = args.source.resolve(strict=True)
    suffix = '.exe' if os.name == 'nt' else ''
    trainer = work / ('train-api' + suffix)
    profiles = work / 'profiles'
    profiles.mkdir(exist_ok=True)
    target = config['target']
    if config['base_rustflags'] != '-Cpanic=unwind': raise ValueError('unwind setting differs')
    env = {k: v for k, v in os.environ.items()
           if not k.upper().startswith('MEDAL_') and k.upper() not in ('DEINLINE_ANCHOR_TRACE', 'LLVM_PROFILE_FILE', 'CARGO_ENCODED_RUSTFLAGS')}
    if os.name != 'nt': env['PATH'] = str(pathlib.Path.home() / '.cargo/bin') + ':' + env['PATH']
    sysroot = subprocess.check_output(['rustc', '+' + config['toolchain'], '--print', 'sysroot'], env=env).decode().strip()
    profdata = pathlib.Path(sysroot) / 'lib/rustlib' / target / 'bin' / ('llvm-profdata' + suffix)
    collection = work / 'collection.json'
    if 'source_hash' in config and config['source_hash'] != build_source_hash(source):
        raise ValueError('source changed after preparation')
    for field, path in (('corpus_sha256', args.corpus), ('api_manifest_sha256', args.api_manifest)):
        if field in config and config[field] != sha256(path):
            raise ValueError('prepared manifest changed: ' + field)
    if args.stage == 'collect':
        if collection.exists() or list(profiles.glob('*.profraw')):
            raise ValueError('profile collection must start empty; use a new prepared work directory')
        train = pathlib.Path(corpus['work']) / 'train'
        expected = corpus['trees']['train']
        hash_tree = portable_tree_hash if corpus.get('tree_hash_model') == 'utf8-relative-path-ordered-v1' else tree_hash
        if hash_tree(train, '*.lua') != (expected['sha256'], expected['files']):
            raise ValueError('frozen training corpus changed')
        env['LLVM_PROFILE_FILE'] = str(profiles / '%m-%p.profraw')
        rows = []
        for run in corpus['training_runs']:
            if run['api'] != 'decompile_batch_with_options': raise ValueError('unsupported training entrypoint')
            report = work / f"api-training-{run['threads']}.json"
            cmd = [trainer, '--manifest', args.api_manifest, '--input-root', train, '--report', report,
                   '--threads', run['threads'], '--rounds', run['rounds']]
            with report.with_suffix('.log').open('w') as log:
                subprocess.run([str(value) for value in cmd], env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
            result = json.loads(report.read_text(encoding='utf-8'))
            if result['scripts'] != expected['files'] or result['executable_sha256'] != sha256(trainer):
                raise ValueError('API trainer inventory/hash differs')
            rows.append(result)
        raw = sorted(profiles.glob('*.profraw'))
        if not raw or any(path.stat().st_size == 0 for path in raw): raise ValueError('no nonempty runtime profile data')
        merged = work / 'merged.profdata'
        subprocess.run([str(profdata), 'merge', '-o', str(merged), *map(str, raw)], check=True)
        summary = subprocess.check_output([str(profdata), 'show', str(merged)]).decode()
        report = dict(schema_version=1, config=config, source_hash=build_source_hash(source),
                      corpus_sha256=sha256(args.corpus), api_manifest_sha256=sha256(args.api_manifest),
                      trainer_sha256=sha256(trainer), baseline_sha256=sha256(work / ('baseline' + suffix)),
                      profiler_sha256=sha256(profdata), merged_sha256=sha256(merged),
                      raw_profiles=[dict(file=path.name, bytes=path.stat().st_size, sha256=sha256(path)) for path in raw],
                      llvm_summary=summary, training=rows,
                      contract='Only locked training inputs execute in the instrumented API process. Successful API runs independently enforce source hashes from the baseline CLI. Normal main return flushes LLVM counters; CLI process-exit behavior is unchanged. Profile data is platform/compiler-specific, and no profile files are published.')
        collection.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
        print(summary)
    else:
        record = json.loads(collection.read_text(encoding='utf-8'))
        if record['source_hash'] != build_source_hash(source) or record['merged_sha256'] != sha256(work / 'merged.profdata'):
            raise ValueError('profile/build source changed')
        if record['corpus_sha256'] != sha256(args.corpus) or record['api_manifest_sha256'] != sha256(args.api_manifest):
            raise ValueError('locked data manifest changed')
        env['RUSTFLAGS'] = '-Cpanic=unwind -Cprofile-use=' + str(work / 'merged.profdata') + ' -Cllvm-args=-pgo-warn-missing-function'
        command = ['cargo', '+' + config['toolchain'], 'build', '--release', '--locked', '--target', target,
                   '--target-dir', str(work / 'target'), '-p', 'luau-lifter', '--bin', 'luau-lifter']
        with (work / 'optimized-build.log').open('w') as log:
            subprocess.run(command, cwd=source, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
        if record['source_hash'] != build_source_hash(source): raise ValueError('source changed during build')
        output = work / ('optimized' + suffix)
        shutil.copyfile(work / 'target' / target / 'release' / ('luau-lifter' + suffix), output)
        if os.name != 'nt': output.chmod(0o755)
        record.update(optimized_sha256=sha256(output), build_command=command, rustflags=env['RUSTFLAGS'],
                      build_log_sha256=sha256(work / 'optimized-build.log'))
        (work / 'optimized.json').write_text(json.dumps(record, indent=1) + '\n', encoding='utf-8', newline='\n')
        print('PGO CLI ready: ' + str(output))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
