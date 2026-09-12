#!/usr/bin/env python3
"""Align the locked study's helper calls by returned-result role and arguments.

This small source-known reviewer is intentionally specific to the six two-result
fixtures. It is not a general source-callsite oracle and does not infer source
certainty from semantic equivalence or from matching call counts.
"""
import argparse
import collections
import json
import pathlib
import re

from roadmap_v2 import sha256
from source_fidelity import parse_ast


FAMILIES = ('helper_let', 'helper_phi', 'helper_guard', 'handwritten_shape',
            'missing_prototype', 'ambiguous_helpers')


def nodes(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from nodes(child)
    elif isinstance(value, list):
        for child in value:
            yield from nodes(child)


def ungroup(node):
    while node.get('type') == 'AstExprGroup':
        node = node['expr']
    return node


def result_roles(tree):
    functions = [n for n in nodes(tree) if n.get('type') == 'AstExprFunction'
                 and n.get('debugname') == 'run']
    if len(functions) != 1:
        raise ValueError('study caller is not unique')
    function = functions[0]
    statements = function['body']['body']
    if statements[-1]['type'] != 'AstStatReturn' or len(statements[-1]['list']) != 2:
        raise ValueError('two-result review shape changed')
    definitions = {}
    for statement in statements:
        if statement['type'] == 'AstStatLocal':
            for local, value in zip(statement['vars'], statement['values']):
                definitions[local['location']] = value
    results = []
    for value in statements[-1]['list']:
        for _ in range(16):
            value = ungroup(value)
            if value['type'] != 'AstExprLocal' or value['local']['location'] not in definitions:
                break
            value = definitions[value['local']['location']]
        else:
            raise ValueError('alias review budget')
        results.append(value)
    return function, results


def argument_signature(call, function):
    params = {arg['location']: index for index, arg in enumerate(function['args'])}
    result = []
    for arg in call['args']:
        arg = ungroup(arg)
        if arg['type'] == 'AstExprLocal' and arg['local']['location'] in params:
            result.append(['parameter', params[arg['local']['location']]])
        elif arg['type'].startswith('AstExprConstant'):
            result.append([arg['type'], arg.get('value')])
        else:
            raise ValueError('argument is outside the locked review vocabulary')
    return result


def byte_span(location, source):
    line, column, end_line, end_column = map(int, re.fullmatch(r'(\d+),(\d+) - (\d+),(\d+)', location).groups())
    lines = source.splitlines(keepends=True)
    return (sum(map(len, lines[:line])) + column, sum(map(len, lines[:end_line])) + end_column)


def helper_call(node):
    return (node['type'] == 'AstExprCall' and node['func']['type'] == 'AstExprLocal'
            and node['func']['local']['name'] == 'helper')


def review(report_path, ast, work=None):
    report = json.loads(report_path.read_text(encoding='utf-8'))
    if report['manifest_sha256'] != 'bfd98a9fba874e42351ed544d76081923988fcb528f7ec23ff2dbfecb45dafbf':
        raise ValueError('review is restricted to the locked study manifest')
    rows = []
    totals = collections.Counter()
    for row in report['rows']:
        if row['opt'] != 2 or row['family'] not in FAMILIES:
            continue
        if row['status'] != 'passed':
            raise ValueError('cannot review a failed profile')
        directory = pathlib.Path(work or report['work']) / f"{row['family']}_O2_g{row['debug']}"
        original = directory / 'source.luau'
        output = directory / 'output.luau'
        if sha256(original) != row['source_sha256'] or sha256(output) != row['output_sha256']:
            raise ValueError('review source/output hash changed')
        source_function, source_values = result_roles(parse_ast(ast, original))
        output_function, output_values = result_roles(parse_ast(ast, output))
        events = {e['event_id']: e for e in row['call_events']['events']}
        occurrences = {}
        for occurrence in row['call_events']['occurrences']:
            event = events[occurrence['event_id']]
            if event['callee_prototype'] in row['helper_prototypes']:
                span = occurrence['span']
                occurrences[span['start']['byte_offset'], span['end']['byte_offset']] = event
        subject = output.read_bytes()
        for index, (before, after) in enumerate(zip(source_values, output_values)):
            expected = helper_call(before)
            event = occurrences.get(byte_span(after['location'], subject)) if helper_call(after) else None
            predicted = event is not None
            original_args = argument_signature(before, source_function) if expected else None
            output_args = argument_signature(after, output_function) if predicted else None
            matched = expected and predicted and original_args == output_args
            classification = 'true_positive' if matched else ('false_positive' if predicted else ('false_negative' if expected else 'true_negative'))
            totals[classification] += 1
            if expected and predicted and not matched:
                totals['false_negative'] += 1
            rows.append(dict(family=row['family'], debug=row['debug'], result_slot=index + 1,
                             classification=classification, source_call=expected,
                             source_location=before['location'], output_location=after['location'],
                             source_arguments=original_args, output_arguments=output_args,
                             event=event, source_sha256=row['source_sha256'], output_sha256=row['output_sha256']))
    if len(rows) != 24:
        raise ValueError('locked six-family O2/g1,g2 review is incomplete')
    tp, fp, fn = (totals[k] for k in ('true_positive', 'false_positive', 'false_negative'))
    return dict(report_sha256=sha256(report_path), binary_sha256=report['tools']['lifter']['sha256'],
                evaluation_use=report.get('evaluation_use', 'initial-holdout'),
                counts=dict(totals), precision=tp / (tp + fp) if tp + fp else None,
                recall=tp / (tp + fn) if tp + fn else None, rows=rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('initial', 'current', 'ast', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    parser.add_argument('--initial-work', type=pathlib.Path)
    parser.add_argument('--current-work', type=pathlib.Path)
    args = parser.parse_args()
    result = dict(schema_version=1, initial=review(args.initial, args.ast, args.initial_work), current=review(args.current, args.ast, args.current_work),
                  reviewer_sha256=sha256(pathlib.Path(__file__)),
                  contract='Known-source O2 lost-call sites in six locked two-result families, aligned by return slot, lexical helper identity, parameter argument slots and emitted producer-event spans. Handwritten matching expressions are false positives for original-source call prediction even when equivalent-call inference is sound. No call-count alignment or original-PC proof is used. Post-unblinding figures describe development regression, not independent generalization. The three distinct two-call helper families and the two-call ambiguous family supply 16 lost source-call sites; the two handwritten/missing-helper families supply eight negative result slots.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps({name: {k: result[name][k] for k in ('counts', 'precision', 'recall')} for name in ('initial', 'current')}))


if __name__ == '__main__':
    main()
