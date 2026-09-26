#!/usr/bin/env python3
"""Kiểm tra hồi quy bằng bytecode gốc và source decompile được biên dịch lại."""
import argparse
import json
import pathlib
import re
import struct
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CASES = {
    "math_shadow": ('local math={huge=5,pi=7}; print(math.huge,1/0,math.pi,3.141592653589793)', ''),
    "math_parameter": ('local function f(math) return 1/0,math end; print(f(42))', ''),
    "math_global": ('math={huge=5,pi=7}; print(1/0,3.141592653589793)', ''),
    "vector_shadow": ('local Vector3={new=function() return "fake" end}; print(Vector3.new(),vector.create(1/0,0,0))', ''),
    "vector_global": ('print(vector.create(1,2,3))', ''),
    "vector_local": ('local v=vector.create(1,2,3); local vector={create=function() return "fake" end}; print(vector.create(),v)', ''),
    "format_table": ('print(("%*"):format({}) ~= nil)', ''),
    "format_method": ('local n=5; print(("x%*y"):format(n):sub(1,1))', ''),
    "format_index": ('local n=5; print(("x%*y"):format(n)["sub"]("abc",1,1))', ''),
    "conditional_value": ('return function(a,b,x) local z=x; if a then z=b; if z then return 99 end end; return z end',
        'for _,a in {false,true} do for _,b in {false,true} do print(a,b,f(a,b,11)) end end'),
    "numeric_break": ('return function(n,s) for i=1,n,s do break end; return "after" end',
        'for _,n in {-2,0,1,3,1/0,0/0} do for _,s in {1,-1,0} do print(f(n,s)) end end; local ok=pcall(f,{},1); print(ok)'),
    "snapshot_return_call": ('return function() local a=1; local function mutate() a=2; return 3 end; local old=a; return mutate(),old end', 'print(f())'),
    "snapshot_return_operator": ('return function() local a=1; local t=setmetatable({},{__add=function() a=2; return 3 end}); local old=a; return old,t+t,old end', 'print(f())'),
    "snapshot_return_index": ('return function() local a=1; local t=setmetatable({},{__index=function() a=2; return 3 end}); local old=a; return t.x,old end', 'print(f())'),
    "snapshot_across_operator": ('return function() local a=1; local t=setmetatable({},{__add=function() a=2; return 3 end}); local old=a; local z=t+t; return old,z,z end', 'print(f())'),
    "unary_condition_and": ('return function(a) if -(a and 3) then return 7 else return 8 end end', 'for _,a in {true,false,0,2} do local ok,v=pcall(f,a); print(ok,if ok then v else "ERROR") end'),
    "unary_condition_or": ('return function(a) if -(a or 3) then return 7 else return 8 end end', 'for _,a in {true,false,0,2} do local ok,v=pcall(f,a); print(ok,if ok then v else "ERROR") end'),
    "api_vector3": ('Vector3={new=function(x,y,z) print("new",x,y,z); return 7 end,zero=99}; print(Vector3.new(0,0,0))', ''),
    "api_vector2": ('Vector2={new=function(x,y) print("new",x,y); return 7 end,one=99}; print(Vector2.new(1,1))', ''),
    "api_cframe": ('CFrame={new=function() print("new"); return 7 end,identity=99}; print(CFrame.new())', ''),
    "interpolation_open_call": ('local function nothing() end; return function() print(("%*"):format(nothing())) end', 'print(pcall(f))'),
    "interpolation_open_vararg": ('return function(...) print(("%*"):format(...)) end', 'print(pcall(f)); print(pcall(f,1,2))'),
    "conditional_captured_cell": ('return function(n,flag) local a=n; local read=function() return a end; if flag then a=false; if a then return "bad" end end; return a,read() end', 'for _,n in {1,0,false} do for _,flag in {false,true} do print(n,flag,f(n,flag)) end end'),
    "dominator_parallel_loop": ('return function(n,flip) local a,b,c=n,n+1,n+2; for j=1,2 do a,c=b,b+c; if j==2 then a,c=c,a+b; continue end; if flip then a,b=c,a; break end; a=a+1 end; c=c+3; return a,b,c end', 'for _,n in {-2,0,1,2,3,5,9} do for _,flag in {false,true} do print(n,flag,f(n,flag)) end end'),
    "integer_literals": ('return {42i,9007199254740993i,-0x8000000000000000i,9223372036854775807i,-42i,0i,{n=9007199254740993i}}', 'for i=1,6 do local v=f[i]; print(type(v),tostring(v),v==42) end; print(type(f[7].n),tostring(f[7].n))'),
}


def run(command):
    result = subprocess.run([str(arg) for arg in command], capture_output=True, timeout=60)
    return {"exit_code": result.returncode,
            "stdout": result.stdout.decode("utf-8", errors="replace"),
            "stderr": result.stderr.decode("utf-8", errors="replace")}


def normalize(output):
    output = re.sub(r'(subject|driver):\d+', 'FILE:N', output)
    return re.sub(r'[^\s:]+\.luau:\d+(?::\d+)?', 'FILE:N', output.replace('\r\n', '\n')).strip()


def compile_source(args, source, destination, optimization, debug, integer=False):
    flags = '--fflags=false,LuauIntegerType2=true' if integer else '--fflags=false'
    command = [str(args.compiler), '--binary', flags, f'-O{optimization}', f'-g{debug}',
               '--vector-lib=vector', '--vector-ctor=create', str(source)]
    result = subprocess.run(command, capture_output=True, timeout=60)
    destination.write_bytes(result.stdout)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors='replace'))


def check(args, name, source, driver, optimization, debug, bytecode=None, integer=False,
          output_optimization=None):
    output_optimization = optimization if output_optimization is None else output_optimization
    suffix = '' if output_optimization == optimization else f'.outO{output_optimization}'
    directory = args.work / f'{name}.O{optimization}.g{debug}{suffix}'
    directory.mkdir(parents=True, exist_ok=True)
    original = directory / 'input.luau'
    original.write_text(source, encoding='utf-8')
    driver_source = directory / 'driver.luau'
    driver_source.write_text(driver, encoding='utf-8')
    data, output_data, driver_data = [directory / filename for filename in ('input.bc', 'output.bc', 'driver.bc')]
    evidence = {"ca": name, "toi_uu": optimization, "debug": debug}
    try:
        compile_source(args, driver_source, driver_data, 1, 1, integer)
        if bytecode is None:
            compile_source(args, original, data, optimization, debug, integer)
        else:
            data.write_bytes(bytecode)
        evidence['goc'] = run([args.vm, data, driver_data])
        assert evidence['goc']['exit_code'] == 0, evidence['goc']
        evidence['decompile'] = run([args.lifter, data])
        assert evidence['decompile']['exit_code'] == 0, evidence['decompile']
        output_source = directory / 'output.luau'
        output_source.write_text(evidence['decompile']['stdout'], encoding='utf-8')
        compile_source(args, output_source, output_data, output_optimization, debug, integer)
        evidence['sau_sua'] = run([args.vm, output_data, driver_data])
        assert evidence['sau_sua']['exit_code'] == 0, evidence['sau_sua']
        assert normalize(evidence['goc']['stdout']) == normalize(evidence['sau_sua']['stdout']), evidence
        evidence['ket_qua'] = 'PASS'
    except (AssertionError, RuntimeError, subprocess.TimeoutExpired) as error:
        evidence['ket_qua'] = 'FAIL'
        evidence['loi'] = str(error)
    (directory / 'result.json').write_text(json.dumps(evidence, ensure_ascii=False, indent=2), encoding='utf-8')
    print(name, optimization, debug, evidence['ket_qua'], flush=True)
    return evidence


def main():
    sys.stdout.reconfigure(encoding='utf-8')
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('compiler', 'vm', 'lifter', 'work'):
        parser.add_argument('--' + option, type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.work.mkdir(parents=True, exist_ok=True)
    results = []
    for name, (source, driver) in CASES.items():
        for debug in (0, 2):
            for optimization in (0, 1, 2):
                results.append(check(args, name, source, driver, optimization, debug,
                                     integer=name == 'integer_literals'))
    for path in sorted((ROOT / '_harness/_bugs').glob('C*.luau')):
        if '.dec.' in path.name:
            continue
        for optimization in (0, 1, 2):
            results.append(check(args, path.stem, path.read_text(encoding='utf-8-sig'), '', optimization, 1))
    for name, source, driver in [
        ('vector_environment', 'return function() return vector.create(1,2,3) end',
         'getfenv(f).vector={create=function() return "fake" end}; print(type(f()),f())'),
        ('vector_nested_environment', 'local createVector=7; return function(n) return function() return vector.create(1,2,3),createVector+n end end',
         'local g=f(2); getfenv(g).vector={create=function() return "fake" end}; print(type(g()),g())'),
    ]:
        for output_optimization in (0, 1, 2):
            results.append(check(args, name, source, driver, 2, 2,
                                 output_optimization=output_optimization))
    def abc(op, a=0, b=0, c=0):
        return op | (a << 8) | (b << 16) | (c << 24)
    def chunk(words):
        return bytes([6,3,0,0,1,3,0,0,0,0,0,len(words)]) + b''.join(struct.pack('<I', word) for word in words) + bytes(7)
    for name, words, driver in [
        ('LOADB_skip', [abc(3,b=1,c=1), abc(4,b=5), abc(22,b=2)], 'print(f)'),
        ('SETLIST_sparse', [abc(53),0,abc(4,a=1,b=42),abc(55,b=1,c=2),5,abc(22,b=2)], 'print(f[1],f[5])'),
    ]:
        results.append(check(args, name, '-- Bytecode tu tao: xem script sinh instruction.', driver, 1, 1, chunk(words)))
    # Both VM loader template tags initialize absent values to number zero.
    for tag in (5, 8):
        words = [abc(54, b=1), abc(22, b=2)]
        data = (bytes([9, 3, 1, 5]) + b'field' + bytes([0, 1, 1, 0, 0, 0, 0, 0, 2])
                + b''.join(struct.pack('<I', word) for word in words)
                + bytes([2, 3, 1, tag, 1, 0])
                + (struct.pack('<i', -1) if tag == 8 else b'') + bytes(6))
        results.append(check(args, f'table_template_{tag}', '-- VM template with zero field',
                             'print(f.field,type(f.field))', 1, 1, data))
    (args.work / 'results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding='utf-8')
    failed = sum(result['ket_qua'] != 'PASS' for result in results)
    print(f'Tổng: {len(results)}; pass: {len(results)-failed}; fail: {failed}')
    return bool(failed)


if __name__ == '__main__':
    raise SystemExit(main())
