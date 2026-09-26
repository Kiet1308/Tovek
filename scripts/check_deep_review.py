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
}


def run(command):
    result = subprocess.run([str(arg) for arg in command], capture_output=True, timeout=60)
    return {"exit_code": result.returncode,
            "stdout": result.stdout.decode("utf-8", errors="replace"),
            "stderr": result.stderr.decode("utf-8", errors="replace")}


def normalize(output):
    return re.sub(r'[^\s:]+\.luau:\d+(?::\d+)?', 'FILE:N', output.replace('\r\n', '\n')).strip()


def compile_source(args, source, destination, optimization, debug):
    command = [str(args.compiler), '--binary', '--fflags=false', f'-O{optimization}', f'-g{debug}',
               '--vector-lib=vector', '--vector-ctor=create', str(source)]
    result = subprocess.run(command, capture_output=True, timeout=60)
    destination.write_bytes(result.stdout)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors='replace'))


def check(args, name, source, driver, optimization, debug, bytecode=None):
    directory = args.work / f'{name}.O{optimization}.g{debug}'
    directory.mkdir(parents=True, exist_ok=True)
    original = directory / 'input.luau'
    original.write_text(source, encoding='utf-8')
    driver_source = directory / 'driver.luau'
    driver_source.write_text(driver, encoding='utf-8')
    data, output_data, driver_data = [directory / filename for filename in ('input.bc', 'output.bc', 'driver.bc')]
    evidence = {"ca": name, "toi_uu": optimization, "debug": debug}
    try:
        compile_source(args, driver_source, driver_data, 1, 1)
        if bytecode is None:
            compile_source(args, original, data, optimization, debug)
        else:
            data.write_bytes(bytecode)
        evidence['goc'] = run([args.vm, data, driver_data])
        assert evidence['goc']['exit_code'] == 0, evidence['goc']
        evidence['decompile'] = run([args.lifter, data])
        assert evidence['decompile']['exit_code'] == 0, evidence['decompile']
        output_source = directory / 'output.luau'
        output_source.write_text(evidence['decompile']['stdout'], encoding='utf-8')
        compile_source(args, output_source, output_data, optimization, debug)
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
                results.append(check(args, name, source, driver, optimization, debug))
    for path in sorted((ROOT / '_harness/_bugs').glob('C*.luau')):
        if '.dec.' in path.name:
            continue
        for optimization in (0, 1, 2):
            results.append(check(args, path.stem, path.read_text(encoding='utf-8-sig'), '', optimization, 1))
    def abc(op, a=0, b=0, c=0):
        return op | (a << 8) | (b << 16) | (c << 24)
    def chunk(words):
        return bytes([6,3,0,0,1,3,0,0,0,0,0,len(words)]) + b''.join(struct.pack('<I', word) for word in words) + bytes(7)
    for name, words, driver in [
        ('LOADB_skip', [abc(3,b=1,c=1), abc(4,b=5), abc(22,b=2)], 'print(f)'),
        ('SETLIST_sparse', [abc(53),0,abc(4,a=1,b=42),abc(55,b=1,c=2),5,abc(22,b=2)], 'print(f[1],f[5])'),
    ]:
        results.append(check(args, name, '-- Bytecode tu tao: xem script sinh instruction.', driver, 1, 1, chunk(words)))
    (args.work / 'results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding='utf-8')
    failed = sum(result['ket_qua'] != 'PASS' for result in results)
    print(f'Tổng: {len(results)}; pass: {len(results)-failed}; fail: {failed}')
    return bool(failed)


if __name__ == '__main__':
    raise SystemExit(main())
