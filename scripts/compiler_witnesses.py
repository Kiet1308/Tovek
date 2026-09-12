#!/usr/bin/env python3
"""Pinned compiler-family witnesses with runtime, call identity and mutant controls."""
import argparse
import base64
import collections
import hashlib
import json
import pathlib
import re
import subprocess
import tempfile

from bytecode_dataflow import compare_dataflow
from bytecode_roundtrip import OPCODES, parse_chunk
from provenance_audit import manifest, sidecar, validate_trace
from emission_map_audit import validate_emission_map, validate_parser_identity
from roadmap_v2 import ROOT, checked, sha256
from source_fidelity import parse_ast


def helper_calls(tree):
    """Return distinct lexical callee identities, not counts of matching text."""
    counts = collections.Counter()
    def walk(node):
        if isinstance(node, dict):
            if node.get('type') == 'AstExprCall':
                callee = node['func']
                if callee.get('type') == 'AstExprLocal':
                    binding = callee['local']
                    if binding['name'] == 'helper':
                        counts[binding['name'], binding['location']] += 1
            for value in node.values(): walk(value)
        elif isinstance(node, list):
            for value in node: walk(value)
    walk(tree)
    if len(counts) > 1: raise ValueError('fixture helper name is lexically ambiguous')
    return sum(counts.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=pathlib.Path,
                        default=ROOT / 'docs/failure_fixtures/compiler_witnesses/manifest.json')
    for name in ('compiler', 'luau', 'ast', 'lifter', 'keep', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    parser.add_argument('--synthesize-arithmetic-loops', action='store_true')
    parser.add_argument('--evaluation-use', choices=('development', 'initial-holdout', 'unblinded-regression'), default='development')
    args = parser.parse_args()
    spec = json.loads(args.manifest.read_text(encoding='utf-8'))
    root = args.manifest.parent.resolve()
    driver = root / 'driver.luau'
    if sha256(driver) != spec['driver_sha256']: raise ValueError('driver differs from locked witness')
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix='compiler-witness-', dir=args.keep)).resolve()
    inputs = work / 'input'; inputs.mkdir()
    subjects = []
    for case in spec['cases']:
        if re.fullmatch(r'[a-z][a-z_]*', case['family']) is None:
            raise ValueError('invalid witness family path')
        path = (root / case['source']).resolve(strict=True)
        if not path.is_relative_to(root) or sha256(path) != case['source_sha256']:
            raise ValueError('source differs from locked witness')
        for profile in case['profiles']:
            opt, debug = profile['opt'], profile['debug']
            name = f"{case['family']}_O{opt}_g{debug}"
            directory = work / name; directory.mkdir()
            source = directory / 'source.luau'; source.write_bytes(path.read_bytes())
            command = [args.compiler, f'-O{opt}', f'-g{debug}', *spec['compiler_flags']]
            raw = checked([*command, '--binary', source], timeout=30)[0]
            if hashlib.sha256(raw).hexdigest() != profile['bytecode_sha256']:
                raise ValueError('pinned compiler image changed: ' + name)
            assembly = checked([*command, '--text', source], timeout=30)[0].decode('utf-8')
            (directory / 'source.txt').write_text(assembly, encoding='utf-8', newline='\n')
            if (assembly.count('REMARK inlining succeeded'), assembly.count('REMARK loop unroll succeeded')) != (profile['inlines'], profile['unrolls']):
                raise ValueError('compiler transformation witness changed: ' + name)
            chunk = parse_chunk(raw, 1)
            run = [p for p in chunk.protos if p.name and chunk.strings[p.name - 1] == b'run']
            if len(run) != 1: raise ValueError('caller prototype not unique')
            counts = collections.Counter(OPCODES[i[1]] for i in run[0].insns)
            if any(counts[op] != count for op, count in profile['run_opcodes'].items()):
                raise ValueError('caller opcode witness changed')
            (inputs / (name + '.lua')).write_bytes(base64.b64encode(raw))
            (directory / 'input.luaubc').write_bytes(raw)
            subjects.append((case, profile, name, directory, raw, run[0]))
    for threads in (1, 4):
        stdout, _ = checked([args.lifter, 'decompile-folder', inputs, work / f'output{threads}',
                            '--key', 1, '--threads', threads, '--emit-binding-provenance',
                            '--strict-no-synthetic-control',
                            *(['--synthesize-arithmetic-loops'] if args.synthesize_arithmetic_loops else [])], timeout=180)
        (work / f'output{threads}.log').write_bytes(stdout)
    a, b = work / 'output1', work / 'output4'
    _, one = manifest(a); _, four = manifest(b)
    if one != four or any(sidecar(a, one[k]) != sidecar(b, four[k]) for k in one):
        raise ValueError('compiler witness metadata differs across thread counts')
    rows = []
    for case, profile, name, directory, raw, caller in subjects:
        row = dict(family=case['family'], **profile, status='failed', source_sha256=case['source_sha256'])
        try:
            metadata = sidecar(a, one[name + '.lua'])
            output = (a / metadata['source_path']).read_bytes()
            if hashlib.sha256(output).hexdigest() != metadata['source_sha256'] or output != (b / metadata['source_path']).read_bytes():
                raise ValueError('output hash/thread identity differs')
            emitted = directory / 'output.luau'; emitted.write_bytes(output)
            command = [args.compiler, f"-O{profile['opt']}", f"-g{profile['debug']}", *spec['compiler_flags']]
            rebuilt = checked([*command, '--binary', emitted], timeout=30)[0]
            (directory / 'output.txt').write_bytes(checked([*command, '--text', emitted], timeout=30)[0])
            trees = [parse_ast(args.ast, directory / (variant + '.luau')) for variant in ('source', 'output')]
            trace = metadata['binding_provenance']
            errors = validate_trace(trace) + validate_emission_map(trace, output)
            errors.extend(validate_parser_identity(trace, output, trees[1])[0])
            if errors: raise ValueError('; '.join(errors))
            input_chunk = parse_chunk(raw, 1)
            helper_prototypes = [p.id for p in input_chunk.protos if p.name and input_chunk.strings[p.name - 1] == b'helper']
            inferred_events = {e['event_id'] for e in trace['call_reconstruction']['events']
                               if e['callee_prototype'] in helper_prototypes}
            original = (directory / 'source.luau').read_text(encoding='utf-8')
            before, after = case['mutant']['from'], case['mutant']['to']
            if original.count(before) != 1: raise ValueError('mutant edit is not unique')
            observations = {}
            for variant, subject in (('source', original), ('output', output.decode('utf-8')),
                                     ('mutant', original.replace(before, after))):
                runner = directory / (variant + '-runner.luau')
                runner.write_text('local f = (function()\n' + subject + '\nend)()\n' + driver.read_text(encoding='utf-8'),
                                  encoding='utf-8', newline='\n')
                # A rejected/noncompiling mutant is not counted as detected behavior.
                checked([*command, '--binary', runner], timeout=30)
                observed = checked([args.luau, f"-O{profile['opt']}", f"-g{profile['debug']}",
                                    *spec['compiler_flags'], runner], timeout=30)[0]
                if len(observed.splitlines()) != case['vectors']: raise ValueError('runtime vector count differs')
                observations[variant] = hashlib.sha256(observed).hexdigest()
                (directory / (variant + '-observations.txt')).write_bytes(observed)
            expected = case['expected_observation_sha256']
            if observations['source'] != expected or observations['output'] != expected or observations['mutant'] == expected:
                raise ValueError('runtime mismatch or undetected compiled mutant')
            row.update(status='passed', runtime_vectors=case['vectors'], observations=observations,
                       contextual_recompile=dict(opt=profile['opt'], debug=profile['debug'], flags=spec['compiler_flags'],
                           bytecode_sha256=hashlib.sha256(rebuilt).hexdigest(), complete_module=True),
                       output_sha256=metadata['source_sha256'], sidecar_sha256=one[name + '.lua']['sidecar_sha256'],
                       dataflow=compare_dataflow(parse_chunk(raw, 1), parse_chunk(rebuilt, 1)),
                       source_helper_calls=helper_calls(trees[0]), output_helper_calls=helper_calls(trees[1]),
                       helper_prototypes=helper_prototypes,
                       reconstructed_helper_occurrences=sum(o['event_id'] in inferred_events for o in trace['call_reconstruction']['occurrences']),
                       synthesized_arithmetic_loops=sum(a['text'] == 'equivalent fixed-count loop synthesized; original loop unknown'
                                                        for a in trace['output_map']['annotations']),
                       call_events=trace['call_reconstruction'], caller_prototype=caller.id,
                       caller_instructions=[dict(zip(('pc', 'opcode', 'a', 'b', 'c', 'd', 'e', 'aux'),
                           (insn[0], OPCODES[insn[1]], *insn[2:]))) for insn in caller.insns])
        except (ValueError, RuntimeError, KeyError, OSError, subprocess.SubprocessError) as error:
            row['error'] = str(error)
        rows.append(row)
    report = dict(schema_version=1, manifest_sha256=sha256(args.manifest), compiler_commit_expected=spec['compiler_commit'],
                  dataset=spec.get('dataset', 'development-compiler-witnesses'),
                  evaluation_use=args.evaluation_use,
                  synthesize_arithmetic_loops=args.synthesize_arithmetic_loops,
                  tools={name: dict(path=str(getattr(args, name).resolve()), sha256=sha256(getattr(args, name)))
                         for name in ('compiler', 'luau', 'ast', 'lifter')}, work=str(work), rows=rows,
                  summary=dict(profiles=len(rows), status=dict(collections.Counter(r['status'] for r in rows)),
                               dataflow=dict(collections.Counter(r.get('dataflow', {}).get('status', 'unavailable') for r in rows)),
                               compiled_mutants_detected=sum(r['status'] == 'passed' for r in rows)),
                  contract='Compiler families locked before their first decompiler evaluation; evaluation_use records whether this run is development, an initial holdout, or an unblinded regression. The dataset name alone does not establish independence. Bytecode hashes retain registers/arity/capture/debug data; remarks and caller operands characterize transformations. The complete emitted module is recompiled in its original compiler profile. Source/helper call counts use lexical callee identity, but do not alone align original/output callsites or establish uniqueness. Runtime observations and compiled mutants do not promote unknown/different dataflow. Precision/recall requires a separate labeled-site review.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary']))
    return int(any(r['status'] != 'passed' for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
