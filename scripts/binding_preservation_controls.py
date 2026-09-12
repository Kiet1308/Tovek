#!/usr/bin/env python3
"""Require R2 VM observations to detect nil/false and captured-cell mistakes."""
import argparse
import json
import pathlib
import subprocess

from roadmap_v2 import ROOT, sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('compiler', 'luau', 'keep', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.keep.mkdir(parents=True, exist_ok=False)
    fixtures = ROOT / 'docs/failure_fixtures/roadmap_v2'
    manifest = json.loads((fixtures / 'manifest.json').read_text(encoding='utf-8'))
    case = next(c for c in manifest['cases'] if c['name'] == 'binding_results')
    source = (fixtures / case['source']).read_text(encoding='utf-8')
    driver = (fixtures / case['driver']).read_text(encoding='utf-8')
    variants = [
        ('false_nil_selection', 'if condition then primary else fallback', 'condition and primary or fallback'),
        ('truthy_normalization', 'primary == nil', 'not primary'),
        ('lost_reference_write', 'primary = fallback', 'local primary = fallback'),
        ('early_capture_observation', 'if condition then\n            change()\n        end\n        observe(read())',
         'observe(read())\n        if condition then\n            change()\n        end'),
    ]
    rows = []
    for name, old, new in [('original', '', ''), *variants]:
        if name != 'original' and source.count(old) != 1:
            raise ValueError('control replacement is no longer unique: ' + name)
        code = source if name == 'original' else source.replace(old, new)
        subject = args.keep / (name + '.luau')
        subject.write_text(code, encoding='utf-8', newline='\n')
        runner = args.keep / (name + '.runner.luau')
        runner.write_text('local f = (function()\n' + code + '\nend)()\n' + driver,
                          encoding='utf-8', newline='\n')
        for opt in range(3):
            for debug in (1, 2):
                flags = [f'-O{opt}', f'-g{debug}', '--fflags=false']
                compiled = subprocess.run([str(args.compiler), '--binary', *flags, str(subject)],
                                          capture_output=True, timeout=30)
                observed = subprocess.run([str(args.luau), *flags, str(runner)], capture_output=True, timeout=30)
                actual = observed.stdout.decode('utf-8').splitlines()
                expected = case['expected_stdout'].splitlines()
                differences = [i for i, (a, b) in enumerate(zip(actual, expected)) if a != b]
                passed = compiled.returncode == observed.returncode == 0 and len(actual) == len(expected)
                passed &= not differences if name == 'original' else bool(differences)
                rows.append(dict(control=name, opt=opt, debug=debug, observations=len(actual),
                                 differing_vectors=differences, status='passed' if passed else 'failed'))
    report = dict(schema_version=1, compiler_sha256=sha256(args.compiler), luau_sha256=sha256(args.luau),
                  fixture_sha256=sha256(fixtures / case['source']), driver_sha256=sha256(fixtures / case['driver']),
                  status='passed' if all(r['status'] == 'passed' for r in rows) else 'failed', rows=rows,
                  contract='All mutated subjects compile; fixed VM observations must detect each mutation at every profile. This establishes driver sensitivity, not general decompiler equivalence.')
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(report['status'], len(rows), 'profiles')
    return int(report['status'] != 'passed')


if __name__ == '__main__':
    raise SystemExit(main())
