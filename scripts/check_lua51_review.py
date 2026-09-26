#!/usr/bin/env python3
"""Compare Lua 5.1 bytecode with the lifter's Luau output on independent VMs.

The Lua 5.1 compiler/VM must use four-byte serialized size_t lengths, matching
the legacy reader. This harness does not require the output to be Lua 5.1 syntax.
"""
import argparse
import json
import pathlib
import subprocess

CASES = {
    'open_vararg': 'local function echo(...) return ... end; print(echo(11,22,33))',
    'arg_table': 'local function echo(...) return arg.n,arg[1],arg[2] end; print(echo(11,22,33))',
    'arg_holes': 'local function echo(...) return arg.n,arg[1],arg[2],arg[3],arg[4] end; print(echo()); print(echo(nil,22,nil,nil))',
    'fixed_parameters': 'local function echo(a,b,...) return a,b,arg.n,arg[1],arg[2] end; print(echo(5,6,nil,8,nil))',
    'nested_arg': 'local function outer(n) return function(...) return n,arg.n,arg[1] end end; local f=outer(9); print(f(3,nil)); print(f())',
    'rebound_select': 'select=function() return -99 end; local function echo(...) return arg.n,arg[1],arg[2] end; print(echo(11,nil,33))',
    'ordinary_capture': 'local function outer(a) return function(b) return a+b end end; print(outer(11)(22))',
}

def run(command, cwd=None):
    result = subprocess.run(list(map(str, command)), cwd=cwd, capture_output=True, timeout=30)
    return {'command': list(map(str, command)), 'exit': result.returncode,
            'stdout': result.stdout.decode('utf8', 'replace').replace('\r\n', '\n'),
            'stderr': result.stderr.decode('utf8', 'replace').replace('\r\n', '\n')}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('lua51-compiler', 'lua51-vm', 'lua51-lifter', 'compiler', 'vm', 'work'):
        parser.add_argument('--'+option, type=pathlib.Path, required=True)
    args = parser.parse_args()
    for name, value in vars(args).items(): setattr(args, name, value.resolve())
    args.work.mkdir(parents=True, exist_ok=True)
    empty = args.work/'driver.luau'
    empty.write_text('', encoding='utf8')
    driver = args.work/'driver.bc'
    p = subprocess.run([str(args.compiler), '--binary', '--fflags=false', str(empty)], capture_output=True, check=True)
    driver.write_bytes(p.stdout)
    results = []
    for name, source in CASES.items():
        for stripped in (False, True):
            folder = args.work/f'{name}.stripped{int(stripped)}'
            folder.mkdir(parents=True, exist_ok=True)
            (folder/'input.lua').write_text(source, encoding='utf8')
            steps = [run([args.lua51_compiler, *(['-s'] if stripped else []), '-o', folder/'input.bc', folder/'input.lua'])]
            original = run([args.lua51_vm, folder/'input.bc'])
            steps.extend([original, run([args.lua51_lifter, '-f', folder/'input.bc'], folder)])
            for opt in (0, 2):
                row = {'case': name, 'stripped': stripped, 'optimization': opt, 'steps': steps, 'status': 'FAIL'}
                command = [str(args.compiler), '--binary', '--fflags=false', f'-O{opt}', str(folder/'input.dec.51.lua')]
                p = subprocess.run(command, capture_output=True, timeout=30)
                row['compile'] = {'command': command, 'exit': p.returncode, 'stderr': p.stderr.decode('utf8','replace')}
                if all(step['exit']==0 for step in steps) and p.returncode==0:
                    data = folder/f'output.O{opt}.bc'
                    data.write_bytes(p.stdout)
                    row['actual'] = actual = run([args.vm, data, driver])
                    if all(actual[key] == original[key] for key in ('exit', 'stdout', 'stderr')): row['status']='PASS'
                results.append(row)
                print(name, stripped, opt, row['status'], flush=True)
    (args.work/'results.json').write_text(json.dumps(results, indent=2), encoding='utf8')
    failed = sum(row['status']!='PASS' for row in results)
    print(f'Total {len(results)}; failed {failed}')
    return bool(failed)

if __name__ == '__main__': raise SystemExit(main())
