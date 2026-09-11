#!/usr/bin/env python3
"""Run counterexamples to collapsing repeated index/operator evaluations."""
import argparse
import json
import pathlib
import subprocess
import time

from roadmap_v2 import ROOT, sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--luau', type=pathlib.Path, required=True)
    parser.add_argument('--report', type=pathlib.Path, required=True)
    args = parser.parse_args()
    source = ROOT / 'docs/failure_fixtures/roadmap_v2/formatter_counterexamples.luau'
    expected = 'base\t1\t2\t101\t100\nbase\t2\t1\t11\t100\nkey\t1\t2\t21\t20\nkey\t2\t1\t11\t20\n'
    rows = []
    for opt in (0, 1, 2):
        for debug in (1, 2):
            command = [str(args.luau.resolve()), f'-O{opt}', f'-g{debug}', '--fflags=false', str(source)]
            start = time.perf_counter()
            result = subprocess.run(command, capture_output=True, timeout=30)
            stdout = result.stdout.decode().replace('\r\n', '\n')
            rows.append(dict(opt=opt, debug=debug, seconds=time.perf_counter() - start,
                             status='passed' if result.returncode == 0 and stdout == expected else 'failed',
                             stdout=stdout, stderr=result.stderr.decode(errors='replace')))
    report = dict(schema_version=1, luau_sha256=sha256(args.luau), source_sha256=sha256(source),
                  compiler_commit='c2ec0d4e5ca50796ba174a7565298f59aa572268', flags=['--fflags=false'],
                  contract='Expanded assignment and the compound-assignment mutant intentionally differ. Formatter unit tests require the expanded form when the base/key has observable evaluation.', rows=rows)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(f"formatter effect controls: {sum(r['status'] == 'passed' for r in rows)}/{len(rows)}")
    return int(any(r['status'] != 'passed' for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
