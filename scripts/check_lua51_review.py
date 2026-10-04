#!/usr/bin/env python3
"""Compare Lua 5.1 bytecode with the lifter's Luau output on independent VMs.

The Lua 5.1 compiler/VM must use four-byte serialized size_t lengths, matching
the legacy reader. This harness does not require the output to be Lua 5.1 syntax.
"""
import argparse
import json
import pathlib
import struct
import subprocess

CASES = {
    'open_vararg': 'local function echo(...) return ... end; print(echo(11,22,33))',
    'arg_table': 'local function echo(...) return arg.n,arg[1],arg[2] end; print(echo(11,22,33))',
    'arg_holes': 'local function echo(...) return arg.n,arg[1],arg[2],arg[3],arg[4] end; print(echo()); print(echo(nil,22,nil,nil))',
    'fixed_parameters': 'local function echo(a,b,...) return a,b,arg.n,arg[1],arg[2] end; print(echo(5,6,nil,8,nil))',
    'nested_arg': 'local function outer(n) return function(...) return n,arg.n,arg[1] end end; local f=outer(9); print(f(3,nil)); print(f())',
    'rebound_select': 'select=function() return -99 end; local function echo(...) return arg.n,arg[1],arg[2] end; print(echo(11,nil,33))',
    'ordinary_capture': 'local function outer(a) return function(b) return a+b end end; print(outer(11)(22))',
    # SELF looks the method up before the arguments run; Luau's `o:m(...)`
    # would after them.
    'method_lookup_order': 'local o = setmetatable({}, {__index = function(_, k) print("lookup", k) '
                           'return function(self, ...) print("call", ...) end end}); '
                           'local function args() print("arg") return 1 end; o:m(args())',
    # ... and before reading an argument the lookup may change.
    'method_lookup_before_argument_read': 'local x = 1; local function invoke(t) t:m(x) end; '
                                          'local t = setmetatable({}, {__index = function() x = 2; '
                                          'return function(_, value) print(value) end end}); invoke(t)',
    # SELF indexes a userdata; Luau's `u:foo()` would go through `__namecall`.
    'method_namecall_userdata': 'local u = newproxy(true); local mt = getmetatable(u); '
                                'mt.__index = {foo = function(self) print("foo") end}; '
                                'mt.__namecall = function() print("namecall") end; u:foo()',
}

# A compiled chunk with one string constant respelled (same length): SELF
# with a name no identifier spells has no `object:name()` form.
PATCHED = {
    'method_invalid_name': ('local t = setmetatable({}, {__index = function(_, k) '
                            'return function(self) print(k, self ~= nil) end end}); t:bad_nam()', b'bad_nam', b'bad nam'),
}


def lua51_chunk(instructions, constants, max_stack):
    """A main chunk of `instructions` and number or string constants, with
    4-byte size_t."""
    def string(text):
        return struct.pack('<I', len(text) + 1) + text + b'\x00'
    def constant(value):
        return b'\x04' + string(value) if isinstance(value, bytes) else b'\x03' + struct.pack('<d', value)
    return (b'\x1bLuaQ\x00\x01\x04\x04\x04\x08\x00' + string(b'@crafted')
            + struct.pack('<ii', 0, 0) + bytes([0, 0, 2, max_stack])
            + struct.pack('<i', len(instructions)) + b''.join(struct.pack('<I', i) for i in instructions)
            + struct.pack('<i', len(constants)) + b''.join(map(constant, constants))
            + struct.pack('<iiii', 0, 0, 0, 0))


def abc(op, a, b, c): return op | a << 6 | c << 14 | b << 23
def abx(op, a, bx): return op | a << 6 | bx << 14
def asbx(op, a, sbx): return abx(op, a, sbx + 0x1ffff)


# Bytecode no source compiles to.
CRAFTED = {
    # TESTSET's copy runs on its own edge into the jump after it, not when
    # PC 2 jumps there directly: the chunk prints 11.
    'testset_shared_successor': lua51_chunk([
        abx(1, 0, 0),        # LOADK     R0 11
        abc(2, 1, 1, 0),     # LOADBOOL  R1 true
        asbx(22, 0, 1),      # JMP       -> 4
        abc(27, 0, 1, 1),    # TESTSET   R0 R1 1
        asbx(22, 0, 0),      # JMP       -> 5
        abx(5, 1, 1),        # GETGLOBAL R1 print
        abc(0, 2, 0, 0),     # MOVE      R2 R0
        abc(28, 1, 2, 1),    # CALL      R1 1 arg
        abc(30, 0, 1, 0),    # RETURN
    ], [11.0, b'print'], 3),
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
    inputs = [(name, source, stripped, None) for name, source in CASES.items() for stripped in (False, True)]
    inputs += [(name, source, stripped, (spelled, patched))
               for name, (source, spelled, patched) in PATCHED.items() for stripped in (False, True)]
    inputs += [(name, data, True, None) for name, data in CRAFTED.items()]
    for name, source, stripped, patch in inputs:
        folder = args.work/f'{name}.stripped{int(stripped)}'
        folder.mkdir(parents=True, exist_ok=True)
        if isinstance(source, bytes):
            (folder/'input.bc').write_bytes(source)
            steps = []
        else:
            (folder/'input.lua').write_text(source, encoding='utf8')
            steps = [run([args.lua51_compiler, *(['-s'] if stripped else []), '-o', folder/'input.bc', folder/'input.lua'])]
            if patch:
                data = (folder/'input.bc').read_bytes()
                (folder/'input.bc').write_bytes(data.replace(*patch))
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
