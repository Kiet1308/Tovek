#!/usr/bin/env python3
"""Check PGO source/sidecar identity and real panic isolation before benchmarking."""
import argparse
import base64
import json
import pathlib
import subprocess
import tempfile

from pgo_corpus import portable_tree_hash
from provenance_audit import manifest
from roadmap_v2 import sha256


def run(command, log, expected_exit=0):
    with log.open('w') as output:
        result = subprocess.run([str(value) for value in command], stdout=output, stderr=subprocess.STDOUT, timeout=300)
    if result.returncode != expected_exit:
        raise ValueError(f'process exit {result.returncode} != {expected_exit}; see {log}')


def compare_sidecars(before, after, commands):
    a, b = manifest(before), manifest(after)
    for value, command in zip((a, b), commands):
        data = value[0]
        if data['command'] != command or data['tool_sha256'] != sha256(pathlib.Path(command[0])):
            raise ValueError('manifest invocation/tool identity differs')
        if not pathlib.Path(data['tool_path']).samefile(command[0]):
            raise ValueError('manifest tool path differs')
    filtered = [{key: value for key, value in data.items() if key not in ('command', 'tool_path', 'tool_sha256')}
                for data, _ in (a, b)]
    if filtered[0] != filtered[1] or a[1] != b[1]: raise ValueError('analysis facts differ')
    # Matching content-addressed JSON bytes preserves all existing certificates,
    # including their unknown/different statuses. Recheck the actual file bytes.
    for root in (before, after):
        for row in a[1].values():
            path = root / row['sidecar_path']
            if sha256(path) != row['sidecar_sha256']: raise ValueError('sidecar content/hash differs')
    return len(a[1])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('baseline', 'optimized', 'corpus', 'keep', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    binaries = dict(baseline=args.baseline.resolve(strict=True), optimized=args.optimized.resolve(strict=True))
    corpus = json.loads(args.corpus.read_text(encoding='utf-8'))
    source = pathlib.Path(corpus['work'])
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix='quality-', dir=args.keep)).resolve()
    rows = []
    for dataset, key in (('holdout', 1), ('private', 203)):
        reference = None
        metadata_commands = []
        for label, binary in binaries.items():
            for threads in (1, 16):
                output = work / f'{dataset}-{label}-{threads}'
                run([binary, 'decompile-folder', source / dataset, output, '--key', key, '--threads', threads,
                     '--strict-no-synthetic-control'], output.with_suffix('.log'))
                digest = portable_tree_hash(output, '*.luau')
                if reference is None: reference = digest
                if digest != reference or digest[1] != corpus['summary'][dataset]['included']:
                    raise ValueError('PGO source/threads identity differs: ' + dataset)
                rows.append(dict(dataset=dataset, binary=label, mode='source', threads=threads,
                                 source_sha256=digest[0], files=digest[1], status='passed'))
            output = work / f'{dataset}-{label}-metadata'
            command = [str(value) for value in (binary, 'decompile-folder', source / dataset, output, '--key', key,
                       '--threads', 16, '--strict-no-synthetic-control', '--emit-binding-provenance')]
            metadata_commands.append(command)
            run(command, output.with_suffix('.log'))
            if portable_tree_hash(output, '*.luau') != reference: raise ValueError('metadata mode changed source')
        count = compare_sidecars(work / f'{dataset}-baseline-metadata', work / f'{dataset}-optimized-metadata', metadata_commands)
        rows.append(dict(dataset=dataset, mode='full_sidecars', threads=16, scripts=count, status='passed'))
    # A known real deserializer panic must be caught per file. The good item is
    # also checked after the panic in serial mode, which exercises context reset.
    panic_input = work / 'panic-input'; panic_input.mkdir()
    good = next(iter(sorted((source / 'train').rglob('*.lua')))).read_bytes()
    (panic_input / '0_bad.lua').write_bytes(base64.b64encode(bytes([99, 0, 0])))
    (panic_input / '1_good.lua').write_bytes(good)
    good_input = work / 'good-input'; good_input.mkdir(); (good_input / '1_good.lua').write_bytes(good)
    expected = None
    for label, binary in binaries.items():
        clean = work / f'good-{label}'
        run([binary, 'decompile-folder', good_input, clean, '--key', 1, '--threads', 1], clean.with_suffix('.log'))
        digest = sha256(clean / '1_good.luau')
        if expected is None: expected = digest
        if digest != expected: raise ValueError('clean good item differs')
        for threads in (1, 16):
            output = work / f'panic-{label}-{threads}'
            run([binary, 'decompile-folder', panic_input, output, '--key', 1, '--threads', threads], output.with_suffix('.log'), expected_exit=1)
            if sha256(output / '1_good.luau') != expected: raise ValueError('panic contaminated the good item')
            rows.append(dict(mode='real_panic_isolation', binary=label, threads=threads, expected_exit=1,
                             good_item_sha256=expected, status='passed'))
    report = dict(schema_version=1, binaries={label: dict(path=str(path), sha256=sha256(path)) for label, path in binaries.items()},
                  corpus_sha256=sha256(args.corpus), work=str(work), rows=rows,
                  contract='Full source bytes agree across builds at 1/16 threads. Full provenance sidecar bytes agree at 16 threads. Unsupported bytecode version 99 triggers the actual deserializer panic; process exit must be 1, with a valid following file unchanged, at 1/16 threads. No unknown proof is promoted and no panic=abort build is admitted.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(f'{len(rows)} PGO quality configurations passed')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
