#!/usr/bin/env python3
"""Check native rehoist fixtures against the pinned compiler, parser and VM.

The register control restores the actual pre-fix two-constant rewrite; O0 must
reject it while the same original source compiles. Runtime controls deliberately
shadow a descendant global, erase negative zero, and move a lookup across calls.
"""
import argparse
import base64
import collections
import json
import pathlib
import subprocess
import tempfile

from bytecode_dataflow import compare_dataflow
from bytecode_roundtrip import parse_chunk
from roadmap_v2 import ROOT, parse_ast, sha256
from provenance_audit import manifest as source_manifest, sidecar, validate_trace


def command(args):
    return subprocess.run([str(x) for x in args], capture_output=True, timeout=30)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('fixtures', 'compiler', 'luau', 'ast', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    parser.add_argument('--lifter', type=pathlib.Path, help='also replay the register witness through the full pipeline at six profiles')
    args = parser.parse_args()
    root = args.fixtures.resolve(strict=True)
    manifest = json.loads((root / 'manifest.json').read_text(encoding='utf-8'))
    driver_path = ROOT / 'docs/failure_fixtures/rehoist_constants.driver.luau'
    driver = driver_path.read_text(encoding='utf-8')
    assert driver.count('--[[REHOIST_MODULE]]') == 1

    def compile_file(path, opt, debug):
        return command([args.compiler, '--binary', f'-O{opt}', f'-g{debug}', '--fflags=false', path])

    def observe(path, opt, debug):
        runner = path.with_name(path.stem + f'_O{opt}_g{debug}.runner.luau')
        runner.write_text(driver.replace('--[[REHOIST_MODULE]]', path.read_text(encoding='utf-8')),
                          encoding='utf-8', newline='\n')
        result = command([args.luau, f'-O{opt}', f'-g{debug}', '--fflags=false', runner])
        if result.returncode or len(result.stdout.splitlines()) != 26:
            raise ValueError('runtime driver failed: ' + result.stderr.decode(errors='replace'))
        return result.stdout.decode('utf-8').replace('\r\n', '\n')

    rows, observations = [], {}
    for case in manifest['cases']:
        name = case['case']
        directory = root / name
        if directory.parent != root or not directory.is_dir():
            raise ValueError('invalid fixture name')
        paths = [directory / (v + '.luau') for v in ('source', 'output')]
        for path in paths:
            parse_ast(args.ast, path, 30)
        for opt in range(3):
            for debug in (1, 2):
                row = dict(case=name, opt=opt, debug=debug, status='failed',
                           introduced=case['introduced'], expected_introduced=case['expected_introduced'],
                           source_sha256=sha256(paths[0]), output_sha256=sha256(paths[1]))
                try:
                    if case['introduced'] != case['expected_introduced']:
                        raise ValueError('pass budget/role decision differs')
                    binaries, results = [], []
                    for path in paths:
                        compiled = compile_file(path, opt, debug)
                        if compiled.returncode:
                            raise ValueError(compiled.stderr.decode(errors='replace'))
                        binaries.append(parse_chunk(compiled.stdout, 1))
                        results.append(observe(path, opt, debug))
                    row['dataflow'] = compare_dataflow(*binaries)
                    row['runtime'] = dict(source=results[0], output=results[1])
                    observations[name, opt, debug] = results[1]
                    if results[0] != results[1]:
                        raise ValueError('lookup/call/store order, errors, arity or value differs')
                    row['status'] = 'passed'
                except (ValueError, OSError, subprocess.SubprocessError) as error:
                    row['error'] = str(error)
                rows.append(row)
                print(f"{row['status']}: {name} O{opt} g{debug} {row.get('error', '')}", flush=True)

    # This exact insertion was emitted by the old pass on the native witness.
    source = (root / 'register_pressure/source.luau').read_text(encoding='utf-8')
    old = source.replace('\n', '\n\tlocal WAIT_INTERVAL = 1\n\tlocal DELAY_DURATION = 2\n', 1)
    old = old.replace('task.wait(1)', 'task.wait(WAIT_INTERVAL)').replace('task.delay(2,', 'task.delay(DELAY_DURATION,')
    register_control = root / 'register_pressure/old_budget.mutant.luau'
    register_control.write_text(old, encoding='utf-8', newline='\n')
    controls = []
    for debug in (1, 2):
        compiled = compile_file(register_control, 0, debug)
        error = compiled.stderr.decode(errors='replace')
        controls.append(dict(control='old_local_only_budget', opt=0, debug=debug,
                             status='passed' if compiled.returncode and 'exceeded limit 255' in error else 'failed',
                             mutant_sha256=sha256(register_control), error=error))

    def shadow(text):
        if 'local WAIT_INTERVAL_2 =' not in text:
            raise ValueError('global collision control shape differs')
        return text.replace('WAIT_INTERVAL_2', 'WAIT_INTERVAL')

    def zero(text):
        if '= -0' not in text: raise ValueError('signed zero control shape differs')
        return text.replace('= -0', '= 0')

    def early_lookup(text):
        if text.count('task.wait(') != 3: raise ValueError('lookup control shape differs')
        return text.replace('\n', '\n\tlocal cachedWait = task.wait\n', 1).replace('task.wait(', 'cachedWait(')

    for control, name, mutate in [('shadow_global', 'descendant_global', shadow),
                                   ('erase_negative_zero', 'signed_zero', zero),
                                   ('move_lookup', 'ordinary_durations', early_lookup)]:
        path = root / name / (control + '.mutant.luau')
        path.write_text(mutate((root / name / 'output.luau').read_text(encoding='utf-8')),
                        encoding='utf-8', newline='\n')
        for opt in range(3):
            for debug in (1, 2):
                original = observations.get((name, opt, debug))
                compiled = compile_file(path, opt, debug)
                observation = observe(path, opt, debug)
                passed = original is not None and not compiled.returncode and original != observation
                controls.append(dict(control=control, case=name, opt=opt, debug=debug,
                                     status='passed' if passed else 'failed', mutant_sha256=sha256(path),
                                     differing_vectors=sum(a != b for a, b in zip((original or '').splitlines(), observation.splitlines()))))
    pipeline = []
    if args.lifter:
        work = pathlib.Path(tempfile.mkdtemp(prefix='pipeline-', dir=root))
        inputs = work / 'input'
        inputs.mkdir()
        source = root / 'register_pressure/source.luau'
        for opt in range(3):
            for debug in (1, 2):
                compiled = compile_file(source, opt, debug)
                if compiled.returncode: raise ValueError('pipeline source compilation failed')
                (inputs / f'pressure_O{opt}_g{debug}.lua').write_bytes(base64.b64encode(compiled.stdout))
        outputs = [work / 'threads1', work / 'threads4']
        for threads, output in zip((1, 4), outputs):
            result = command([args.lifter, 'decompile-folder', inputs, output, '--key', '1',
                              '--threads', str(threads), '--strict-no-synthetic-control', '--emit-binding-provenance'])
            if result.returncode: raise ValueError('pipeline decompilation failed: ' + result.stderr.decode(errors='replace'))
        first, second = [source_manifest(p)[1] for p in outputs]
        if first.keys() != second.keys() or len(first) != 6:
            raise ValueError('pipeline output identities differ')
        for opt in range(3):
            for debug in (1, 2):
                key = f'pressure_O{opt}_g{debug}.lua'
                output = outputs[0] / first[key]['source_path']
                a, b = sidecar(outputs[0], first[key]), sidecar(outputs[1], second[key])
                if a != b or output.read_bytes() != (outputs[1] / second[key]['source_path']).read_bytes():
                    raise ValueError('pipeline thread count changes output')
                if validate_trace(a['binding_provenance'], a):
                    raise ValueError('pipeline provenance invalid')
                parse_ast(args.ast, output, 30)
                compiled = compile_file(output, opt, debug)
                if compiled.returncode: raise ValueError('pipeline recompile failed: ' + compiled.stderr.decode(errors='replace'))
                observation = observe(output, opt, debug)
                original = observations['register_pressure', opt, debug]
                if observation != original: raise ValueError('pipeline register witness behavior differs')
                source_binary = compile_file(source, opt, debug).stdout
                pipeline.append(dict(case=key, opt=opt, debug=debug, status='passed',
                                     input_sha256=sha256(inputs / key), output_sha256=sha256(output),
                                     deterministic_source_and_metadata_1_4=True, provenance_valid=True,
                                     dataflow=compare_dataflow(parse_chunk(source_binary, 1), parse_chunk(compiled.stdout, 1)),
                                     runtime=dict(source=original, output=observation)))
    summary = dict(total=len(rows), status=dict(collections.Counter(r['status'] for r in rows)),
                   controls=dict(collections.Counter(r['status'] for r in controls)),
                   pipeline=dict(collections.Counter(r['status'] for r in pipeline)),
                   dataflow=dict(collections.Counter(r.get('dataflow', {}).get('status', 'missing') for r in rows)))
    report = dict(schema_version=1, compiler_sha256=sha256(args.compiler), luau_sha256=sha256(args.luau),
                  ast_sha256=sha256(args.ast), driver_sha256=sha256(driver_path),
                  lifter_sha256=sha256(args.lifter) if args.lifter else None,
                  manifest=manifest, summary=summary, cases=rows, controls=controls, pipeline=pipeline)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(summary))
    return int(any(r['status'] != 'passed' for r in rows + controls))


if __name__ == '__main__':
    raise SystemExit(main())
