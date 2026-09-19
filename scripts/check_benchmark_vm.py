"""Offline positive and negative controls for the exact-bytecode VM oracle."""
import argparse
from pathlib import Path
import tempfile

from decompiler_benchmark import compile_source, observe, profile_args, same_runtime


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', required=True, type=Path)
    parser.add_argument('--vm', required=True, type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='benchmark_vm_') as directory:
        root = Path(directory)

        def run(source, driver, version):
            for name, text in [('subject',source), ('driver',driver)]:
                path = root/(name+'.luau'); path.write_text(text, encoding='utf-8')
                raw = compile_source(profile_args(args.compiler, version), path, 2, 2)
                path.with_suffix('.luaubc').write_bytes(raw)
            return observe(args.vm, root/'subject.luaubc', root/'driver.luaubc')

        driver = 'local g=f(5)\nprint(select("#",g()),g())'
        source = 'return function(x) return function() return x, nil, 9 end end'
        for version in (9,12):
            result = run(source, driver, version)
            assert result == dict(exit=0, stdout='3\t5\tnil\t9\n', stderr=''), result
            assert not same_runtime(result, run(source.replace('x, nil, 9','x, 9'),driver,version))
            methods = run('return function(t) return t:sum(9) end',
                          'print(f({sum=function(self,x) return x+4 end}))', version)
            assert methods['stdout']=='13\n' and methods['exit']==0, methods
            sandbox = run('return function() return io, package, require, loadstring, os.execute end',
                          'print(f())', version)
            assert sandbox['stdout']=='nil\tnil\tnil\tnil\tnil\n', sandbox
        assert run('while true do end', '', 12)['exit'] != 0
        allocation = run('return string.rep("x", 300 * 1024 * 1024)', '', 12)
        assert allocation['exit'] != 0, allocation
    print('VM controls passed: v9/v12, nil arity, captures, methods, isolation, timeout, memory bound')


if __name__ == '__main__': main()
