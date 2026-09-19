"""Separate coverage, correctness, source metrics and user-visible latency."""
import collections
import html
import json
import random
import statistics
from benchmark_adapters import digest


def interval(values, seed=731, repeats=2000):
    """Paired bootstrap over program/seed clusters, never individual profiles."""
    if not values: return None
    rng = random.Random(seed)
    samples = sorted(statistics.mean(rng.choices(values, k=len(values))) for _ in range(repeats))
    return dict(mean=statistics.mean(values), low=samples[int(.025*repeats)],
                high=samples[min(repeats-1, int(.975*repeats))], clusters=len(values))


def summarize(rows):
    groups = collections.defaultdict(list)
    for row in rows: groups[row['suite'],row['version'],row['provider']].append(row)
    result = []
    for (suite, version, provider), subset in sorted(groups.items()):
        measured = [r['fidelity'] for r in subset if r.get('fidelity',{}).get('status')=='measured']
        runtime = [r for r in subset if r['runtime_eligible']]
        result.append(dict(suite=suite,version=version,provider=provider,profiles=len(subset),
            programs=len({r['program'] for r in subset}),clusters=len({r['cluster'] for r in subset}),
            status=dict(collections.Counter(r['status'] for r in subset)),compiled=sum(r['compile'] for r in subset),
            runtime_eligible=len(runtime),runtime_pass=sum(r.get('runtime_pass') is True for r in runtime),
            runtime_mismatch=sum(r.get('status')=='runtime_mismatch' for r in subset),
            empty_programs=sum(bool(r.get('empty_program')) for r in subset),
            fidelity_measured=len(measured),fidelity_unknown=len(subset)-len(measured),
            mean_structure=statistics.mean(m['raw_structural_ratio'] for m in measured) if measured else None))
    return result


def paired(rows, left='tovek-v2', right='lua-expert'):
    providers = {name:{r['id']:r for r in rows if r['provider']==name} for name in (left,right)}
    groups = collections.defaultdict(lambda: collections.defaultdict(lambda: collections.defaultdict(list)))
    for cid in sorted(providers[left].keys() & providers[right].keys()):
        a,b = providers[left][cid],providers[right][cid]
        if a['status']=='not_run_unsupported_version' or b['status']=='not_run_unsupported_version': continue
        group = a['suite'],a['version']
        if a['runtime_eligible']:
            groups[group]['runtime'][a['cluster']].append(int(a.get('runtime_pass') is True)-int(b.get('runtime_pass') is True))
        af,bf = a.get('fidelity',{}),b.get('fidelity',{})
        if af.get('status')==bf.get('status')=='measured':
            groups[group]['structure'][a['cluster']].append(af['raw_structural_ratio']-bf['raw_structural_ratio'])
    return [dict(suite=suite,version=version,metric=metric,left=left,right=right,
                 paired_profiles=sum(map(len, clusters.values())),
                 **interval([statistics.mean(v) for v in clusters.values()]))
            for (suite,version),metrics in sorted(groups.items()) for metric,clusters in sorted(metrics.items())]


def report(root):
    plan=json.loads((root/'plan.json').read_text())
    plan_hash=digest((root/'plan.json').read_bytes())
    results=json.loads((root/'results.json').read_text())
    if results['plan_sha256'] != plan_hash: raise ValueError('results belong to another plan')
    rows=results['rows']
    programs={p['id']:p for p in plan['programs']}
    groups=summarize(rows)
    comparisons=paired(rows)
    labels=sorted({r['provider'] for r in rows})
    identities={p:json.loads((root/'providers'/p/'identity.json').read_text()) for p in labels}
    capabilities={p:json.loads((root/'providers'/p/'capabilities.json').read_text()) for p in labels}
    timing=json.loads((root/'timing.json').read_text()) if (root/'timing.json').exists() else None
    if timing and timing['plan_sha256'] != plan_hash: raise ValueError('timing belongs to another plan')
    timings=[]
    if timing:
        quality={(r['provider'],r['id']):r for r in rows}
        by=collections.defaultdict(list)
        for r in timing['rows']:
            if not r['warmup']: by[r['provider'],r['case']].append(r)
        for (provider,cid),samples in sorted(by.items()):
            valid=[r for r in samples if r['status']=='output' and r['output_same_as_quality']
                   and quality[provider,cid]['compile'] and not quality[provider,cid].get('empty_program')]
            values=sorted(r['attempts'][0]['seconds'] for r in valid)
            timings.append(dict(provider=provider,case=cid,samples=len(samples),valid_samples=len(values),
                                median_seconds=statistics.median(values) if values else None,
                                max_seconds=max(values) if values else None,
                                output_stable=all(r['output_same_as_quality'] for r in samples)))
    canaries=json.loads((root/'canary-results.json').read_text())
    summary=dict(plan_sha256=plan_hash,groups=groups,paired=comparisons,timing=timings,capabilities=capabilities,
                 canary_outcomes=[{k:r.get(k) for k in ('id','provider','version','status','runtime_pass')} for r in canaries],
                 provider_identity=identities,mutation_controls_detected=sum(c['detected'] for c in plan['mutation_controls']),
                 bootstrap_contract='Paired differences (V2 minus lua.expert), averaged within program/seed clusters, '
                 'then 2000 percentile bootstrap resamples of clusters. Describes this finite corpus, not all Luau. '
                 'Structure only uses the common measured subset; missingness is reported in group totals. '
                 'No pooled regression/public/generated score. Unsupported strata have no paired score.')
    (root/'summary.json').write_text(json.dumps(summary,indent=2)+'\n',encoding='utf-8')
    concise=[]
    texts={}
    for row in rows:
        p=programs[row['program']]
        texts.setdefault(p['source'],(root/p['source']).read_text(encoding='utf-8',errors='replace'))
        if row.get('output'): texts.setdefault(row['output'],(root/row['output']).read_text(encoding='utf-8',errors='replace'))
        concise.append({k:row.get(k) for k in ('id','suite','version','opt','debug','provider','status','compile','runtime_pass',
                                             'output','fidelity','presentation','error','runtime_observed','evidence','response')}
                       | dict(name=p['name'],source=p['source']))
    payload=json.dumps(dict(rows=concise,texts=texts),ensure_ascii=True).replace('<','\\u003c')
    def rate(x,n): return f'{x} / {n}' if n else '—'
    table=''.join('<tr>'+''.join(f'<td>{html.escape(str(v))}</td>' for v in (
        g['suite'],f"v{g['version']}",g['provider'],g['profiles'],rate(g['compiled'],g['profiles']),
        rate(g['runtime_pass'],g['runtime_eligible']),g['runtime_mismatch'],rate(g['fidelity_measured'],g['profiles']),
        f"{g['mean_structure']:.4f}" if g['mean_structure'] is not None else '—'))+'</tr>' for g in groups)
    pair_table=''.join(f'<tr><td>{x["suite"]}</td><td>v{x["version"]}</td><td>{x["metric"]}</td>'
                      f'<td>{x["mean"]:+.4f}</td><td>[{x["low"]:+.4f}, {x["high"]:+.4f}]</td>'
                      f'<td>{x["clusters"]}</td><td>{x["paired_profiles"]}</td></tr>' for x in comparisons)
    timing_table=''.join(f'<tr><td>{t["provider"]}</td><td>{t["case"]}</td><td>{t["valid_samples"]}/{t["samples"]}</td>'
                        f'<td>{t["median_seconds"]*1000:.2f} ms</td><td>{t["output_stable"]}</td></tr>'
                        for t in timings if t['median_seconds'] is not None)
    canary_table=''.join('<tr>'+''.join('<td>'+html.escape(str(r.get(k,'—')))+'</td>'
                        for k in ('provider','id','status'))+'</tr>' for r in canaries)
    page='''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Tovek / lua.expert — reproducible benchmark</title><style>
:root{color-scheme:dark;font-family:system-ui,sans-serif;background:#101419;color:#e4e9f0}body{max-width:1500px;margin:40px auto;padding:0 24px}h1{font-size:clamp(28px,4vw,48px);letter-spacing:-.035em}h2{margin-top:48px;font-size:24px}p,li{line-height:1.6;color:#bac4d0}a{color:#8cc7ff}table{border-collapse:collapse;width:100%;font-size:13px}th{text-align:left;color:#a6bbd2;background:#1b2430}th,td{padding:11px;border-bottom:1px solid #2a3542}section{overflow:auto}select,input,button{background:#1b2430;color:inherit;border:1px solid #425268;padding:10px;border-radius:6px}button{cursor:pointer}label{display:inline-flex;gap:8px;align-items:center;margin:8px 14px 8px 0}.tag{color:#8dd6bd;font-size:12px;text-transform:uppercase;letter-spacing:.14em}.panes{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:12px}.pane{min-width:0}pre{background:#090d12;max-height:620px;overflow:auto;padding:18px;font:12px/1.65 ui-monospace,monospace;tab-size:4;border:1px solid #2a3542}dialog{width:min(1450px,94vw);max-height:92vh;overflow:auto;background:#101419;color:inherit;border:1px solid #425268}dialog::backdrop{background:#000b}.sticky{position:sticky;top:0;background:#101419;padding:10px 0}.note{border-left:3px solid #8dd6bd;padding-left:18px}.muted{font-size:12px;color:#91a0b2}@media(max-width:750px){.panes{grid-template-columns:1fr}body{padding:0 12px}}
</style><p class="tag">Frozen inputs · untouched outputs · offline audit</p>
<h1>Tovek V2 / lua.expert / v0.9 beta</h1>
<p>Same bytecode. Separate answers for compatibility, tested behavior, source structure and user-visible latency.</p>
<p class="note">No AI judge, automatic repair or composite winner score. Runtime checks execute the exact compiler-produced input bytecode and independently recompiled output in the same pinned VM. They cover the supplied drivers, not every possible execution.</p>
<p><a href="plan.json">Frozen plan</a> · <a href="summary.json">Summary & identities</a> · <a href="results.json">Every outcome</a> · <a href="timing.json">Raw timing attempts</a></p>
<p class="muted">Plan frozen __DATE__ · SHA-256 __PLAN_HASH__</p>
<h2>Capability probes</h2><p>Three simple programs per bytecode version. Supported responses also undergo runtime verification. These probes are excluded from quality totals.</p>
<section><table><thead><tr><th>Provider</th><th>Probe</th><th>Observed outcome</th></tr></thead><tbody>__CANARIES__</tbody></table></section>
<h2>Coverage and outcomes</h2><p>Every planned profile remains in its denominator. <b>not_run_unsupported_version</b> means three separate canaries explicitly rejected the version; those profiles were not individually requested. Compile success is not a semantic proof. Structural means are conditional on measurable output; consult the paired comparison before ranking.</p>
<section><table><thead><tr><th>Suite</th><th>Bytecode</th><th>Provider</th><th>Profiles</th><th>Recompiled</th><th>Runtime passed / eligible</th><th>Observed mismatches</th><th>Structure measured</th><th>Mean structure</th></tr></thead><tbody>__GROUPS__</tbody></table></section>
<ul><li><b>Regression:</b> Tovek development fixtures; intentionally exposed during implementation.</li><li><b>Public:</b> all 171 selected files from five pinned, licensed repositories. No complete Roblox environment is executed.</li><li><b>Generated:</b> __SEEDS__ fresh seeds plus alpha-renamed variants from an existing grammar. Seeds are frozen before scored requests; variants share a statistical cluster. This is not an independent language-family holdout.</li><li>O0/O1/O2, stripped/debug metadata and v9/v12 are distinct profiles, not independent programs. Alignment-budget unknowns remain visible.</li></ul>
<h2>Paired differences, with uncertainty</h2><p>V2 minus lua.expert. Runtime is end-to-end tested success; structure uses only their common measured subset. Each program/seed is averaged before a 2,000-resample cluster bootstrap. Intervals describe this corpus, not a representative sample of all Roblox code.</p>
<section><table><thead><tr><th>Suite</th><th>Bytecode</th><th>Metric</th><th>Mean delta</th><th>95% interval</th><th>Clusters</th><th>Paired profiles</th></tr></thead><tbody>__PAIRS__</tbody></table></section>
<h2>Inspect every case</h2><p>Select a row to inspect the original source and every untouched provider response side by side. Failures and regressions are searchable.</p>
<div><label>Suite <select id="suite"><option value="">All</option><option>regression</option><option>public</option><option>generated</option></select></label><label>Bytecode <select id="version"><option value="">All</option><option>9</option><option>12</option></select></label><label>Debug <select id="debug"><option value="">All</option><option>0</option><option>1</option><option>2</option></select></label><label>Status <input id="status" placeholder="e.g. runtime_mismatch"></label><label>Search <input id="search" placeholder="source, provider, case"></label></div>
<p id="count" class="muted"></p><section><table><thead><tr><th>Case</th><th>Provider</th><th>Profile</th><th>Outcome</th><th>Structure</th><th></th></tr></thead><tbody id="cases"></tbody></table></section><button id="more">Show more</button>
<h2>Latency, without a misleading engine speed ratio</h2><p>Local CLI: process startup + decompilation + I/O, one worker thread. Hosted API: HTTPS request including network/TLS; rate-limit sleeps excluded. Provider hardware and server-side caching are unknown. One warmup and __ROUNDS__ interleaved rounds; no retries during measurement. Only unchanged, nonempty outputs that passed recompilation enter latency summaries; all attempts remain in the raw record. This small sample does not estimate a stable per-case tail percentile.</p>
<section><table><thead><tr><th>Provider</th><th>Workload</th><th>Valid samples</th><th>Median request/process</th><th>Output stable</th></tr></thead><tbody>__TIMING__</tbody></table></section>
<h2>Methodology references</h2><p><a href="https://lua.expert/docs">lua.expert API</a> · <a href="https://luau.org/performance/">Luau compiler optimizations</a> · <a href="https://luau.org/sandbox/">Luau VM isolation</a> · <a href="https://arxiv.org/abs/2505.11340">DecompileBench</a>. These inform separating syntax, behavior and usability; this harness uses no learned judge or repair model.</p>
<dialog id="detail"><div class="sticky"><button id="close">Close</button><h2 id="title"></h2></div><div class="panes" id="panes"></div></dialog>
<script id="data" type="application/json">__DATA__</script><script>
const data=JSON.parse(document.getElementById('data').textContent);let limit=100;
const byId=new Map();for(const r of data.rows){if(!byId.has(r.id))byId.set(r.id,[]);byId.get(r.id).push(r)}
function show(id){const rows=byId.get(id),panes=document.getElementById('panes');panes.replaceChildren();document.getElementById('title').textContent=rows[0].name+' · '+id;const entries=[['Source',data.texts[rows[0].source],null],...rows.map(r=>[r.provider+' · '+r.status,r.output?data.texts[r.output]:'No output: '+r.status,r])];for(const [name,text,row] of entries){const pane=document.createElement('div');pane.className='pane';const h=document.createElement('h3');h.textContent=name;const pre=document.createElement('pre');pre.textContent=text;pane.append(h,pre);if(row){const detail=document.createElement('details'),summary=document.createElement('summary'),audit=document.createElement('pre');summary.textContent='Runtime / structure / presentation diagnostics';audit.textContent=JSON.stringify({runtime:row.runtime_observed,structure:row.fidelity,presentation:row.presentation,error:row.error,evidence:row.evidence},null,2);detail.append(summary,audit);pane.append(detail);if(row.response){const link=document.createElement('a');link.href=row.response;link.textContent='Original response receipt';pane.append(link)}}panes.append(pane)}document.getElementById('detail').showModal()}
function render(){const get=id=>document.getElementById(id).value.toLowerCase();const rows=data.rows.filter(r=>(!get('suite')||r.suite===get('suite'))&&(!get('version')||String(r.version)===get('version'))&&(!get('debug')||String(r.debug)===get('debug'))&&r.status.includes(get('status'))&&(r.name+' '+r.id+' '+r.provider).toLowerCase().includes(get('search')));document.getElementById('count').textContent=rows.length+' matching profiles; showing '+Math.min(limit,rows.length);const body=document.getElementById('cases');body.replaceChildren();for(const r of rows.slice(0,limit)){const tr=document.createElement('tr');for(const value of [r.name,r.provider,'v'+r.version+' O'+r.opt+' g'+r.debug,r.status,r.fidelity?.raw_structural_ratio?.toFixed(4)??'—']){const td=document.createElement('td');td.textContent=value;tr.append(td)}const td=document.createElement('td'),button=document.createElement('button');button.textContent='Compare';button.onclick=()=>show(r.id);td.append(button);tr.append(td);body.append(tr)}document.getElementById('more').hidden=limit>=rows.length}
for(const id of ['suite','version','debug','status','search'])document.getElementById(id).addEventListener('input',()=>{limit=100;render()});document.getElementById('more').onclick=()=>{limit+=100;render()};document.getElementById('close').onclick=()=>document.getElementById('detail').close();render();
</script></html>'''
    page=page.replace('__GROUPS__',table).replace('__PAIRS__',pair_table).replace('__TIMING__',timing_table)
    page=page.replace('__CANARIES__',canary_table).replace('__SEEDS__',str(plan['generated_clusters']))
    page=page.replace('__ROUNDS__',str(timing['rounds']) if timing else 'unmeasured')
    page=page.replace('__DATE__',html.escape(plan['created_utc'])).replace('__PLAN_HASH__',plan_hash)
    page=page.replace('__DATA__',payload)
    (root/'index.html').write_text(page,encoding='utf-8',newline='\n')
    print(json.dumps(summary['groups'],indent=1))
