#!/usr/bin/env python3
"""Evaluate frozen PGO gates from complete samples, including failed experiments."""
import argparse
import collections
import json
import math
import pathlib
import statistics

from roadmap_v2 import sha256


def require(condition, message):
    if not condition:
        raise ValueError(message)


def validate_corpus(corpus):
    rows = corpus['rows']
    training = [r for r in rows if r['dataset'] == 'train' and r['status'] == 'included']
    images = {r['execution_image_sha256'] for r in training}
    sources = {r['source_sha256'] for r in training if r.get('source_sha256')}
    require(bool(images), 'empty training workload')
    for dataset in ('train', 'holdout', 'private'):
        group = [r for r in rows if r['dataset'] == dataset]
        require(dict(collections.Counter(r['status'] for r in group)) == corpus['summary'][dataset], 'corpus inventory differs')
        included = [r for r in group if r['status'] == 'included']
        require(len(included) == corpus['trees'][dataset]['files'] and len({r['path'] for r in group}) == len(group),
                'corpus file count/path uniqueness differs')
        if dataset != 'train':
            require(all(r['execution_image_sha256'] not in images for r in included), 'training image leaked into evaluation')
        if dataset == 'holdout':
            require(all(r.get('source_sha256') not in sources for r in group), 'training source leaked into holdout')


def compare_samples(report, corpus, dataset, build):
    policy = corpus['evaluation']
    threads, rounds = policy['threads'], policy['rounds']
    require(rounds >= 3 and len(set(threads)) == len(threads), 'invalid frozen sample policy')
    require(set(report['tools']) == {'baseline', 'pgo'}, 'unexpected benchmark labels')
    for label, field in (('baseline', 'baseline_sha256'), ('pgo', 'optimized_sha256')):
        require(report['tools'][label]['sha256'] == build[field], 'benchmark/build hash differs')
    require(report['input_count'] == corpus['trees'][dataset]['files'], 'input count differs')
    require(report['corpus_hash'] == corpus['trees'][dataset]['sha256'], 'input tree differs')
    require(report['analysis_modes'] == {} and report['lifter_args'] == {'baseline': [], 'pgo': []},
            'benchmark options differ')
    expected = {(label, thread, index) for label in ('baseline', 'pgo') for thread in threads for index in range(rounds)}
    seen, sources = set(), set()
    for row in report['rows']:
        require(row['label'] in ('baseline', 'pgo') and row['threads'] in threads, 'unexpected sample')
        require(type(row['warmup']) is bool and row['deterministic'] is True, 'invalid determinism/sample flag')
        require(row['output_count'] == report['input_count'], 'output count differs')
        require(isinstance(row['seconds'], (int, float)) and math.isfinite(row['seconds']) and row['seconds'] > 0,
                'invalid timing')
        require(row['peak_rss_bytes'] is None or type(row['peak_rss_bytes']) is int and row['peak_rss_bytes'] > 0,
                'invalid RSS')
        sources.add(row['output_hash'])
        if row['warmup']:
            continue
        key = row['label'], row['threads'], row['round']
        require(key in expected and key not in seen, 'duplicate/unexpected measured sample')
        seen.add(key)
    require(seen == expected, 'incomplete measured rounds')
    warmups = [(row['label'], row['threads'], row['round']) for row in report['rows'] if row['warmup']]
    require(collections.Counter(warmups) == collections.Counter((label, max(threads), 0) for label in ('baseline', 'pgo')),
            'warmup inventory differs')
    require(len(sources) == 1 and report['deterministic'] is True, 'source differs across builds/threads')
    comparisons = []
    for thread in threads:
        stats = {}
        for label in ('baseline', 'pgo'):
            rows = [r for r in report['rows'] if not r['warmup'] and r['label'] == label and r['threads'] == thread]
            times = sorted(r['seconds'] for r in rows)
            rss = [r['peak_rss_bytes'] for r in rows]
            stats[label] = dict(samples=len(rows), median_seconds=statistics.median(times),
                p95_nearest_rank_seconds=times[math.ceil(.95 * len(times)) - 1],
                median_peak_rss_bytes=statistics.median(rss) if None not in rss else None,
                max_peak_rss_bytes=max(rss) if None not in rss else None)
        before, after = stats['baseline'], stats['pgo']
        changes = {key: (after[key] / before[key] - 1) * 100 if before[key] is not None and after[key] is not None else None
                   for key in ('median_seconds', 'p95_nearest_rank_seconds', 'median_peak_rss_bytes', 'max_peak_rss_bytes')}
        if dataset == policy['primary_workload']:
            gates = dict(median=changes['median_seconds'] <= -policy['target_median_reduction_percent'],
                         p95=changes['p95_nearest_rank_seconds'] <= policy['allowed_p95_regression_percent'],
                         rss=changes['median_peak_rss_bytes'] is not None and
                             changes['median_peak_rss_bytes'] <= policy['allowed_rss_regression_percent'])
        else:
            require(dataset == policy['secondary_workload'], 'unclassified workload')
            gates = dict(median=changes['median_seconds'] <= policy['allowed_holdout_median_regression_percent'])
        comparisons.append(dict(dataset=dataset, threads=thread, baseline=before, pgo=after,
                                change_percent=changes, gates=gates, passed=all(gates.values())))
    return comparisons


def validate_quality(quality, corpus, build, corpus_hash):
    require(quality['corpus_sha256'] == corpus_hash == build['corpus_sha256'], 'quality/training corpus differs')
    for label, field in (('baseline', 'baseline_sha256'), ('optimized', 'optimized_sha256')):
        require(quality['binaries'][label]['sha256'] == build[field], 'quality/build hash differs')
    expected = []
    for dataset in ('holdout', 'private'):
        expected += [(dataset, 'source', label, thread) for label in ('baseline', 'optimized') for thread in (1, 16)]
        expected += [(dataset, 'full_sidecars', None, 16)]
    expected += [(None, 'real_panic_isolation', label, thread) for label in ('baseline', 'optimized') for thread in (1, 16)]
    actual = [(r.get('dataset'), r['mode'], r.get('binary'), r['threads']) for r in quality['rows']]
    require(collections.Counter(actual) == collections.Counter(expected), 'quality coverage differs')
    source_hashes, panic_hashes = {}, set()
    for row in quality['rows']:
        require(row['status'] == 'passed', 'quality failure')
        if row['mode'] == 'source':
            dataset = row['dataset']
            require(row['files'] == corpus['trees'][dataset]['files'], 'quality source count differs')
            source_hashes.setdefault(dataset, set()).add(row['source_sha256'])
        elif row['mode'] == 'full_sidecars':
            require(row['scripts'] == corpus['trees'][row['dataset']]['files'], 'sidecar count differs')
        else:
            require(row['expected_exit'] == 1, 'panic isolation was not checked')
            panic_hashes.add(row['good_item_sha256'])
    require(all(len(values) == 1 for values in source_hashes.values()) and len(panic_hashes) == 1,
            'quality source/panic hashes differ')
    require(build['config']['base_rustflags'] == '-Cpanic=unwind', 'unwind not retained')
    require('-Cpanic=unwind' in build['rustflags'].split() and '-Cpanic=abort' not in build['rustflags'].split(),
            'optimized unwind setting differs')
    training = build['training']
    require(len(training) == len(corpus['training_runs']), 'training inventory differs')
    for result, planned in zip(training, corpus['training_runs']):
        require(result['api'] == planned['api'] and result['threads'] == planned['threads'], 'training entrypoint differs')
        require(result['executable_sha256'] == build['trainer_sha256'] and
                result['manifest_sha256'] == build['api_manifest_sha256'], 'training hash differs')
        require(result['scripts'] == corpus['trees']['train']['files'] and result['option_bits'] == 8 and
                result['decode_key'] == 1 and result['group'] == 'all' and result['allocation_instrumented'] is False,
                'training inputs/options differ')
        require([r['round'] for r in result['rows']] == list(range(planned['rounds'] + 1)), 'training rounds differ')
        require([r['first_call'] for r in result['rows']] == [True] + [False] * planned['rounds'], 'training first-call differs')
        require(len({r['output_tree_hash'] for r in result['rows']}) == 1, 'training output differs')
    return {dataset: next(iter(values)) for dataset, values in source_hashes.items()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('corpus', 'build', 'quality', 'holdout', 'private', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    paths = {name: getattr(args, name) for name in ('corpus', 'build', 'quality', 'holdout', 'private')}
    records = {name: json.loads(path.read_text(encoding='utf-8')) for name, path in paths.items()}
    corpus, build = records['corpus'], records['build']
    validate_corpus(corpus)
    require(corpus['evaluation']['default_promotion'] is False, 'automatic promotion is outside this experiment')
    sources = validate_quality(records['quality'], corpus, build, sha256(args.corpus))
    rows = [row for dataset in ('holdout', 'private') for row in compare_samples(records[dataset], corpus, dataset, build)]
    report = dict(schema_version=1, status='evaluated', performance_gates_passed=all(r['passed'] for r in rows),
                  default_promoted=False, build_commit=build['config']['commit'], target=build['config']['target'],
                  input_reports={name: dict(file=path.name, sha256=sha256(path)) for name, path in paths.items()},
                  policy=corpus['evaluation'], quality_source_sha256=sources, rows=rows,
                  contract='Gates use all seven measured samples at each frozen thread count: primary workload median reduction, nearest-rank p95 regression and median per-process peak RSS regression, plus secondary workload median regression. Maximum RSS is reported separately. Any missing primary RSS fails its gate. Summaries are recalculated from individual samples. Seven-sample p95 equals the maximum; no significance, cold-cache, cross-machine, whole-VM memory or automatic-promotion claim.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(dict(performance_gates_passed=report['performance_gates_passed'], rows=rows)))
    # A correctly measured negative experiment is successful execution, with an
    # explicit failed-gate result. Invalid/missing evidence above raises instead.
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
