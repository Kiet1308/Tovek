#!/usr/bin/env python3
"""Freeze public inputs, collect untouched decompiler outputs, then audit offline.

No network operation occurs without --online. Private corpora are not inputs to
this harness. Reports/outputs remain in the ignored --out directory. No repair,
AI judge, bytecode downgrade, score-dependent selection or baseline updates.
"""
import argparse
import collections
import concurrent.futures
import functools
import json
import platform
from pathlib import Path
import random
import re
import subprocess
import sys
import time
from types import SimpleNamespace

from benchmark_adapters import Expert, Native, digest
from generated_roundtrip import generate, PRELUDE, ENDING, DRIVER
from roadmap_v2 import compile_source, fixture_path, ROOT
from source_fidelity import compare_ast, parse_ast
from output_quality import analyze_tree

SCHEMA = 'decompiler-benchmark-v1'
PIN = 'c2ec0d4e5ca50796ba174a7565298f59aa572268'


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_suffix(path.suffix + '.tmp')
    temp.write_text(json.dumps(value, indent=1, ensure_ascii=True) + '\n', encoding='utf-8', newline='\n')
    temp.replace(path)


def read(path):
    return json.loads(path.read_text(encoding='utf-8'))


def command(args, timeout=30):
    p = subprocess.run([str(x) for x in args], capture_output=True, timeout=timeout)
    if p.returncode:
        raise ValueError(p.stderr.decode('utf-8', errors='replace')[:3000])
    return p.stdout


def observe(vm, subject, driver):
    try:
        p = subprocess.run([str(vm), str(subject), str(driver)], capture_output=True, timeout=8)
        return dict(exit=p.returncode, stdout=p.stdout.decode('utf-8', errors='replace').replace('\r\n', '\n'),
                    stderr=p.stderr.decode('utf-8', errors='replace').replace('\r\n', '\n')[:3000])
    except subprocess.TimeoutExpired:
        return dict(exit=None, stdout='', stderr='runtime timeout')


def same_runtime(a, b):
    return a['exit'] == b['exit'] == 0 and a['stdout'] == b['stdout'] and a['stderr'] == b['stderr']


def profile_args(compiler, version):
    return SimpleNamespace(compiler=compiler, bytecode_version=version, timeout=30)


def prepare(args):
    if (args.out / 'plan.json').exists():
        raise ValueError('plan already frozen; use another output directory')
    args.out.mkdir(parents=True, exist_ok=True)
    cases, programs, controls = [], [], []
    runtime_spec = read(ROOT / 'docs/failure_fixtures/roadmap_v2/manifest.json')
    public_spec = read(ROOT / 'docs/source_corpus_v2.json')
    if public_spec['compiler_commit'] != PIN or runtime_spec['compiler_commit'] != PIN:
        raise ValueError('compiler manifest revision changed')
    # Verify every public checkout, license and selected source before staging.
    for repo in public_spec['repositories']:
        path = fixture_path(args.vendor, repo['name'])
        if command(['git', '-C', path, 'rev-parse', 'HEAD']).decode().strip() != repo['commit']:
            raise ValueError('public repository revision mismatch')
        if digest((path / repo['license']['path']).read_bytes()) != repo['license']['sha256']:
            raise ValueError('public license hash mismatch; prepare vendor with public_source_roundtrip --checkout')

    def add(suite, name, source, driver=None, cluster=None, expected=None, provenance=None):
        pid = f'{suite}-{len(programs):04d}'
        directory = args.out / 'programs' / pid
        directory.mkdir(parents=True)
        (directory / 'source.luau').write_bytes(source)
        program = dict(id=pid, suite=suite, name=name, cluster=cluster or pid,
                       source=f'programs/{pid}/source.luau', source_sha256=digest(source),
                       provenance=provenance, runtime=driver is not None)
        if driver is not None:
            (directory / 'driver.luau').write_text(driver, encoding='utf-8', newline='\n')
            raw_driver = compile_source(profile_args(args.compiler, 9), directory / 'driver.luau', 0, 1)
            (directory / 'driver.luaubc').write_bytes(raw_driver)
            program['driver'] = f'programs/{pid}/driver.luaubc'
            program['driver_sha256'] = digest(raw_driver)
        programs.append(program)
        profiles = ([(o, g) for o in (0, 1, 2) for g in (0, 1, 2)] if suite == 'regression'
                    else [(o, 1) for o in (0, 1, 2)] if suite == 'public'
                    else [(o, g) for o in (0, 2) for g in (0, 2)] if suite == 'generated'
                    else [(2, 2)])
        for version in (9, 12):
            for opt, debug in profiles:
                raw = compile_source(profile_args(args.compiler, version), directory / 'source.luau', opt, debug)
                sha = digest(raw)
                path = args.out / 'inputs' / (sha + '.luaubc')
                path.parent.mkdir(exist_ok=True)
                path.write_bytes(raw)
                case = dict(id=f'{pid}-v{version}-o{opt}-g{debug}', program=pid, suite=suite,
                            cluster=program['cluster'], version=version, opt=opt, debug=debug,
                            input_sha256=sha, input_bytes=len(raw), input=f'inputs/{sha}.luaubc')
                if driver is not None:
                    reference = observe(args.vm, path, directory / 'driver.luaubc')
                    if reference['exit'] != 0 or (expected is not None and reference['stdout'] != expected):
                        save(args.out / 'reference-failure.json', dict(case=case, observed=reference, expected=expected))
                        raise ValueError('reference VM differs from locked driver: ' + case['id'])
                    case['reference'] = reference
                cases.append(case)
        return program

    for case in runtime_spec['cases']:
        base = ROOT / 'docs/failure_fixtures/roadmap_v2'
        add('regression', case['name'], (base / case['source']).read_bytes(),
            (base / case['driver']).read_text(encoding='utf-8'), expected=case['expected_stdout'],
            provenance=dict(group=case['group'], use='Tovek development regressions; not independent holdout'))
    for entry in public_spec['sources']:
        raw = fixture_path(args.vendor / entry['repo'], entry['file']).read_bytes()
        if digest(raw) != entry['source_sha256']: raise ValueError('public source hash differs')
        add('public', entry['repo'] + '/' + entry['file'], raw, provenance=entry)
    rng = random.Random(args.seed)
    driver = DRIVER.split('\n', 1)[1]
    for index in range(args.seeds):
        seed = rng.randrange(1 << 63)
        source = PRELUDE + ''.join(generate(seed, units=10)) + ENDING
        cluster = 'generated-seed-' + str(seed)
        for variant in ('original', 'alpha'):
            subject = source
            if variant == 'alpha':
                mapping = dict(value='accumulator', input='argument', flip='selectLeft',
                               box='stateCell', tap='record', events='trace', read='readAccumulator')
                subject = re.sub(r'\b(?:' + '|'.join(mapping) + r')\b', lambda m: mapping[m[0]], subject)
            add('generated', f'seed-{seed}-{variant}', subject.encode(), driver, cluster=cluster,
                provenance=dict(seed=seed, grammar='scalar-branch-table-capture-loop-v1', units=10,
                                variant=variant, use='fresh seeds in an existing grammar; not an independent family'))
        # Verify the runtime oracle detects a deliberately changed observable.
        mutant = args.out / 'controls' / f'{index}.luau'
        mutant.parent.mkdir(exist_ok=True)
        mutant.write_text(source.replace('table.concat(events, ",")', '"MUTATED"'), encoding='utf-8', newline='\n')
        raw = compile_source(profile_args(args.compiler, 9), mutant, 2, 2)
        mutant.with_suffix('.luaubc').write_bytes(raw)
        base_case = next(c for c in reversed(cases) if c['cluster'] == cluster and c['version'] == 9)
        base_program = next(p for p in programs if p['id'] == base_case['program'])
        result = observe(args.vm, mutant.with_suffix('.luaubc'), args.out / base_program['driver'])
        detected = not same_runtime(base_case['reference'], result)
        controls.append(dict(cluster=cluster, detected=detected, mutant_sha256=digest(raw), observed=result))
        if not detected: raise ValueError('runtime mutation control was not detected')
    # Simple independent canaries are excluded from quality summaries.
    canaries = [
        ('arithmetic', 'return function(x) return x * 3 + 1 end', 'print(f(7))'),
        ('method', 'return function(t, x) return t:sum(x) end', 'print(f({sum=function(self,x) return x+4 end}, 9))'),
        ('captures', 'return function(x) local function g() return x, nil, 9 end return g end',
         'local g=f(5)\nprint(select("#",g()),g())'),
    ]
    for name, source, driver_ in canaries:
        add('canary', name, source.encode(), driver_)
    timing_ids = []
    for repo in public_spec['repositories']:
        candidates = sorted((c for c in cases if c['suite'] == 'public' and c['version'] == 9 and c['opt'] == 2
                             and next(p for p in programs if p['id'] == c['program'])['provenance']['repo'] == repo['name']),
                            key=lambda c: (c['input_bytes'], c['id']))
        timing_ids.extend(candidates[int((len(candidates)-1)*q)]['id'] for q in (0, .5, 1))
    plan = dict(schema=SCHEMA, created_utc=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
                seed=args.seed, generated_clusters=args.seeds, bytecode_key=1, programs=programs, cases=cases,
                timing_ids=sorted(set(timing_ids)), mutation_controls=controls,
                tools={n: dict(path=str(getattr(args, n).resolve()), sha256=digest(getattr(args, n).read_bytes()))
                       for n in ('compiler', 'ast', 'vm')}, compiler_commit=PIN,
                manifests={str(p.relative_to(ROOT)): digest(p.read_bytes()) for p in
                           (ROOT/'docs/source_corpus_v2.json', ROOT/'docs/failure_fixtures/roadmap_v2/manifest.json')},
                contract='Frozen before scored decompilation. Identical raw bytecode per profile. No source repair. '
                         'Public modules are not executed as Roblox experiences. Regression and generated grammar have '
                         'Tovek development exposure. Profile/alpha variants are not independent programs. '
                         'Source likeness and readability signals do not prove semantics. No composite winner score.')
    save(args.out / 'plan.json', plan)
    print(json.dumps(dict(profiles=len(cases), programs=len(programs),
                          suites=dict(collections.Counter(c['suite'] for c in cases)),
                          unique_inputs=len({c['input_sha256'] for c in cases}), plan_sha256=digest((args.out/'plan.json').read_bytes()))))


def load_plan(args):
    plan = read(args.out / 'plan.json')
    if plan['schema'] != SCHEMA: raise ValueError('unsupported plan')
    for name, tool in plan['tools'].items():
        if digest(Path(tool['path']).read_bytes()) != tool['sha256']: raise ValueError('tool changed: ' + name)
    for p in plan['programs']:
        if digest((args.out/p['source']).read_bytes()) != p['source_sha256']: raise ValueError('source drift')
        if p.get('driver') and digest((args.out/p['driver']).read_bytes()) != p['driver_sha256']: raise ValueError('driver drift')
    for c in plan['cases']:
        if digest((args.out/c['input']).read_bytes()) != c['input_sha256']: raise ValueError('bytecode drift')
    return plan


def providers(args):
    result = {}
    for value in args.native:
        label, sep, path = value.partition('=')
        if not sep or not re.fullmatch(r'[a-z0-9-]+', label) or label == 'lua-expert' or label in result:
            raise ValueError('use unique simple provider labels: --native label=exe')
        result[label] = Native(path)
    if args.online: result['lua-expert'] = Expert(args.rpm)
    if not result: raise ValueError('no providers selected')
    return result


def collect(args):
    plan, adapters = load_plan(args), providers(args)
    plan_hash = digest((args.out/'plan.json').read_bytes())

    def provider_run(label, adapter):
        folder = args.out / 'providers' / label
        folder.mkdir(parents=True, exist_ok=True)
        identity = dict(plan_sha256=plan_hash, **adapter.identity)
        if (folder/'identity.json').exists() and read(folder/'identity.json') != identity:
            raise ValueError('provider/plan identity changed: use another run')
        save(folder/'identity.json', identity)
        def request(case):
            sha = case['input_sha256']
            receipt, output = folder/(sha+'.json'), folder/(sha+'.luau')
            if receipt.exists():
                saved = read(receipt)
                if not output.exists() or digest(output.read_bytes()) != saved['output_sha256']:
                    raise ValueError('cached output changed')
                return saved
            result, body = adapter.invoke(args.out/case['input'])
            result.update(input_sha256=sha)
            output.write_bytes(body)
            save(receipt, result)
            return result
        capability = {}
        for version in (9, 12):
            canaries = [c for c in plan['cases'] if c['suite']=='canary' and c['version']==version]
            statuses = [request(c)['status'] for c in canaries]
            capability[str(version)] = dict(canaries=statuses, unsupported=all(s=='unsupported_version' for s in statuses),
                                           inference='skip this version only after three explicit unsupported responses')
        save(folder/'capabilities.json', capability)
        cases = [c for c in plan['cases'] if c['suite']!='canary']
        random.Random(plan['seed']).shuffle(cases)
        done = set()
        for case in cases:
            sha = case['input_sha256']
            if sha in done or capability[str(case['version'])]['unsupported']: continue
            request(case); done.add(sha)
            if len(done) % 50 == 0: print(f'{label}: {len(done)} unique inputs collected', flush=True)
        print(f'{label}: collection complete; {len(done)} unique scored inputs', flush=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(adapters)) as pool:
        for future in [pool.submit(provider_run, label, adapter) for label, adapter in adapters.items()]: future.result()


def evaluate(args):
    plan = load_plan(args)
    programs = {p['id']: p for p in plan['programs']}
    source_trees = {p['id']: parse_ast(Path(plan['tools']['ast']['path']), args.out/p['source']) for p in plan['programs']}
    compiler, vm, ast = [Path(plan['tools'][n]['path']) for n in ('compiler', 'vm', 'ast')]
    output_paths = {}

    @functools.lru_cache(maxsize=None)
    def structure(program_id, output_hash):
        # Syntax/presentation depend only on source and untouched output bytes,
        # not on the compile profile. Runtime checks remain per profile.
        output = output_paths[output_hash]
        tree = parse_ast(ast, output)
        fidelity = compare_ast(source_trees[program_id], tree)
        fidelity.pop('bindings', None)
        return fidelity, analyze_tree(tree, output.read_text(encoding='utf-8')), bool(tree.get('body'))

    jobs = []
    plan_hash = digest((args.out/'plan.json').read_bytes())
    for folder in sorted((args.out/'providers').iterdir()):
        if not (folder/'identity.json').exists(): continue
        if read(folder/'identity.json')['plan_sha256'] != plan_hash:
            raise ValueError('provider belongs to another plan')
        capabilities = read(folder/'capabilities.json')
        for case in plan['cases']:
            jobs.append((folder, capabilities, case))
    def check(job):
        folder, capabilities, case = job
        program = programs[case['program']]
        row = dict(**case, provider=folder.name, compile=False, runtime_pass=None, runtime_eligible=program['runtime'])
        row.pop('reference', None)
        if case['suite'] != 'canary' and capabilities[str(case['version'])]['unsupported']:
            row.update(status='not_run_unsupported_version', evidence='three independent capability probes; not per-profile requests')
            return row
        receipt = folder/(case['input_sha256']+'.json')
        if not receipt.exists(): row['status']='not_collected'; return row
        response = read(receipt)
        row['status'] = response['status']
        row['response'] = str(receipt.relative_to(args.out)).replace('\\','/')
        output = folder/(case['input_sha256']+'.luau')
        row['output'] = str(output.relative_to(args.out)).replace('\\','/')
        if digest(output.read_bytes()) != response['output_sha256']: raise ValueError('output receipt mismatch')
        if response['status'] != 'output': return row
        output_paths[response['output_sha256']] = output
        directory = args.out/'evaluation'/folder.name/case['id']
        directory.mkdir(parents=True, exist_ok=True)
        try:
            # Recompile every untouched response under the matching profile.
            raw = compile_source(profile_args(compiler, case['version']), output, case['opt'], case['debug'])
            rebuilt = directory/'output.luaubc'; rebuilt.write_bytes(raw)
            row.update(compile=True, status='compiled', output_sha256=response['output_sha256'])
            if program['runtime']:
                observed = observe(vm, rebuilt, args.out/program['driver'])
                row['runtime_pass'] = same_runtime(case['reference'], observed)
                row['runtime_observed'] = observed
                row['status'] = 'runtime_pass' if row['runtime_pass'] else 'runtime_mismatch'
            try:
                row['fidelity'], row['presentation'], row['nonempty_ast'] = structure(
                    case['program'], response['output_sha256'])
                if not row['nonempty_ast'] and source_trees[case['program']].get('body'):
                    row['empty_program'] = True
                    if not program['runtime']: row['status'] = 'empty_program'
            except (ValueError, OSError, subprocess.SubprocessError) as error:
                row['fidelity'] = dict(status='unknown', reason=str(error)[:500])
        except (ValueError, OSError, RuntimeError, subprocess.SubprocessError) as error:
            row.update(status='compile_failed', error=str(error)[:2000])
        save(directory/'result.json', row)
        return row
    rows = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        for row in pool.map(check, jobs):
            rows.append(row)
            if len(rows)%100==0: print(f'evaluated {len(rows)}/{len(jobs)}',flush=True)
    save(args.out/'canary-results.json', [r for r in rows if r['suite']=='canary'])
    rows = [r for r in rows if r['suite']!='canary']
    save(args.out/'results.json', dict(schema=SCHEMA, plan_sha256=plan_hash, rows=rows,
        audit=dict(python=sys.version, platform=platform.platform(), machine=platform.machine(),
                   source_hashes={p.name:digest(p.read_bytes()) for p in
                       [Path(__file__), ROOT/'scripts/source_fidelity.py', ROOT/'scripts/output_quality.py']})))
    print(collections.Counter(r['status'] for r in rows))


def timing(args):
    plan, adapters = load_plan(args), providers(args)
    cases = {c['id']: c for c in plan['cases']}
    rows = []
    for round_ in range(-1, args.rounds):
        order = list(adapters)
        random.Random(plan['seed'] + round_).shuffle(order)
        for cid in plan['timing_ids']:
            for label in order:
                adapter = adapters[label]
                expected_identity = read(args.out/'providers'/label/'identity.json')
                if any(expected_identity.get(k) != v for k,v in adapter.identity.items()):
                    raise ValueError('timing provider differs from quality provider')
                result, body = adapter.invoke(args.out/cases[cid]['input'], retry=False)
                output_path = args.out/'timing-outputs'/(digest(body)+'.luau')
                if not output_path.exists():
                    output_path.parent.mkdir(exist_ok=True)
                    output_path.write_bytes(body)
                receipt = read(args.out/'providers'/label/(cases[cid]['input_sha256']+'.json'))
                row = dict(case=cid, provider=label, round=round_, warmup=round_==-1,
                           output_same_as_quality=digest(body)==receipt['output_sha256'], **result)
                rows.append(row)
                save(args.out/'timing.json', dict(rounds=args.rounds, plan_sha256=digest((args.out/'plan.json').read_bytes()),
                     rows=rows, contract='One warmup and measured rounds. Sequential interleaved '
                     'provider requests; no audit work during timing; no cache reuse by harness. Provider-side cache unknown. '
                     'No retries. API includes network/TLS; CLI includes startup/I/O. No intrinsic engine speed ratio.'))
        print(f'timing round {round_}: complete',flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('stage', choices=('prepare','collect','evaluate','timing','report'))
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--vm', type=Path)
    parser.add_argument('--ast', type=Path)
    parser.add_argument('--vendor', type=Path)
    parser.add_argument('--seed', type=int, default=2026091901)
    parser.add_argument('--seeds', type=int, default=24)
    parser.add_argument('--native', action='append', default=[], metavar='LABEL=EXE')
    parser.add_argument('--online', action='store_true', help='send only this frozen public corpus to lua.expert')
    parser.add_argument('--rpm', type=int, default=120)
    parser.add_argument('--workers', type=int, default=4)
    parser.add_argument('--rounds', type=int, default=7)
    args = parser.parse_args()
    args.out = args.out.resolve()
    if args.stage == 'prepare':
        for n in ('compiler','vm','ast','vendor'):
            if getattr(args,n) is None: parser.error('--'+n+' is required for prepare')
            setattr(args,n,getattr(args,n).resolve(strict=True))
    if args.workers<1 or args.rounds<3 or args.seeds<1: parser.error('positive workers/seeds and at least three timing rounds required')
    if args.stage=='report':
        from benchmark_report import report
        report(args.out)
    else:
        globals()[args.stage](args)


if __name__ == '__main__': main()
