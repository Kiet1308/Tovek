#!/usr/bin/env python3
"""Bounded module naming summaries under an explicit script-path manifest.

This is a source-lookup/naming aid, not an evaluator or effect oracle. It never
loads modules, follows filesystem requires, or changes decompiler output.
"""
from __future__ import annotations

import argparse
import collections
import json
import pathlib

from roadmap_v2 import fixture_path, sha256
from source_fidelity import parse_ast

LIMITS = dict(modules=2000, source_bytes=524288, nodes=100000, depth=128, rounds=8, results=32, project_source_bytes=67108864, project_nodes=2000000)


def walk(root, *, skip_functions=False):
    pending = [(root, 0)]
    seen = 0
    while pending:
        node, depth = pending.pop()
        seen += 1
        if seen > LIMITS['nodes'] or depth > LIMITS['depth']:
            raise ValueError('AST analysis budget')
        if isinstance(node, dict):
            yield node
            if skip_functions and node.get('type') == 'AstExprFunction':
                continue
            pending.extend((v, depth + 1) for v in reversed(list(node.values())) if isinstance(v, (list, dict)))
        elif isinstance(node, list):
            pending.extend((v, depth + 1) for v in reversed(node) if isinstance(v, (list, dict)))


def binding(node):
    if not isinstance(node, dict):
        return None
    if node.get('type') == 'AstExprLocal':
        node = node['local']
    return (node['name'], node['location']) if node.get('type') == 'AstLocal' else None


def field(node):
    if node.get('type') == 'AstExprIndexName' and node.get('op') == '.':
        return node['expr'], node['index']
    if node.get('type') == 'AstExprIndexExpr' and node.get('index', {}).get('type') == 'AstExprConstantString':
        return node['expr'], node['index']['value']
    return None


def static_path(node, current):
    parts = []
    for _ in range(LIMITS['depth']):
        if node.get('type') == 'AstExprGlobal' and node.get('global') == 'script':
            result = list(current)
            for part in reversed(parts):
                if part == 'Parent':
                    if not result:
                        return None
                    result.pop()
                elif isinstance(part, str) and part and '/' not in part and '\\' not in part and part not in ('.', '..'):
                    result.append(part)
                else:
                    return None
            return tuple(result)
        item = field(node)
        if not item:
            return None
        node, part = item
        parts.append(part)
    return None


def components(graph):
    """Iterative Kosaraju, stable order, no recursion on a large require cycle."""
    seen, order = set(), []
    for start in sorted(graph):
        stack = [(start, False)]
        while stack:
            node, done = stack.pop()
            if done:
                order.append(node)
            elif node not in seen:
                seen.add(node)
                stack.append((node, True))
                stack.extend((child, False) for child in reversed(sorted(graph[node])) if child not in seen)
    reverse = {node: set() for node in graph}
    for node, edges in graph.items():
        for child in edges:
            reverse[child].add(node)
    seen, result = set(), []
    for start in reversed(order):
        if start in seen:
            continue
        group, pending = [], [start]
        seen.add(start)
        while pending:
            node = pending.pop()
            group.append(node)
            for child in sorted(reverse[node]):
                if child not in seen:
                    seen.add(child)
                    pending.append(child)
        result.append(sorted(group))
    return sorted(result)


class Module:
    def __init__(self, entry, root, path_index):
        self.entry, self.root = entry, root
        self.imports, self.functions, self.exports = {}, {}, {}
        self.require_values = {}
        self.requires, self.calls = [], []
        self.export_status = 'unknown_return_shape'
        self.writes = collections.Counter()
        self.definitions = {}
        nodes = list(walk(root))
        functions = [n for n in nodes if n.get('type') == 'AstExprFunction']
        self.function_ids = {id(n): f"{entry['id']}:f{i}" for i, n in enumerate(functions)}
        for node in nodes:
            kind = node.get('type')
            if kind == 'AstStatLocal':
                for index, local in enumerate(node['vars']):
                    key = binding(local)
                    self.writes[key] += 1
                    if index < len(node['values']):
                        self.definitions[key] = node['values'][index]
            elif kind == 'AstStatLocalFunction':
                key = binding(node['name'])
                self.writes[key] += 1
                self.definitions[key] = node['func']
            elif kind in ('AstStatAssign', 'AstStatCompoundAssign'):
                for target in node.get('vars', [node.get('var')]):
                    if binding(target):
                        self.writes[binding(target)] += 1
            elif kind == 'AstStatFunction' and binding(node['name']):
                self.writes[binding(node['name'])] += 1
        environment_unknown = any(
            (n.get('type') == 'AstExprCall' and n.get('func', {}).get('type') == 'AstExprGlobal'
             and n['func'].get('global') in ('getfenv', 'setfenv'))
            or (n.get('type') in ('AstStatAssign', 'AstStatCompoundAssign')
                and any(v and v.get('type') == 'AstExprGlobal' and v.get('global') in ('require', 'script')
                        for v in n.get('vars', [n.get('var')]))) for n in nodes)
        owners = {id(value): key for key, value in self.definitions.items()}
        for expression in nodes:
            if expression.get('type') != 'AstExprCall':
                continue
            callee = expression['func']
            if callee.get('type') != 'AstExprGlobal' or callee.get('global') != 'require':
                continue
            key = owners.get(id(expression))
            path = static_path(expression['args'][0], entry['script_path']) if len(expression['args']) == 1 else None
            targets = path_index.get(path, [])
            mutable = key is not None and self.writes[key] != 1
            status = 'unknown_module_environment' if environment_unknown else 'mutable_import_binding' if mutable else 'resolved_static_path' if len(targets) == 1 else 'ambiguous_path' if targets else 'unknown_path'
            self.requires.append(dict(binding=list(key) if key else None, location=expression['location'], status=status,
                                      script_path=list(path) if path is not None else None, candidates=targets))
            if not mutable and not environment_unknown and len(targets) == 1:
                self.require_values[id(expression)] = targets[0]
                if key:
                    self.imports[key] = targets[0]
        for expression in functions:
            fid = self.function_ids[id(expression)]
            parameters = [dict(position=i, observed_name=p['name'], declaration=p['location'])
                          for i, p in enumerate(expression['args'])]
            self.functions[fid] = dict(id=fid, location=expression['location'], parameters=parameters,
                                       variadic=expression['vararg'], returns=None, forward=None)
            body = expression['body']['body']
            # No branch/loop/early-return inference or open argument pack guess.
            straight = body and body[-1].get('type') == 'AstStatReturn' and all(
                n.get('type') in ('AstStatLocal', 'AstStatLocalFunction', 'AstStatAssign', 'AstStatExpr') for n in body[:-1])
            if straight and not expression['vararg']:
                values = body[-1]['list']
                if len(values) <= LIMITS['results']:
                    summary = [self.scalar_role(v) for v in values]
                    if all(v is not None for v in summary):
                        self.functions[fid]['returns'] = [dict(slot, origin_function=fid) for slot in summary]
                    elif len(values) == 1 and values[0].get('type') == 'AstExprCall' and not values[0].get('self'):
                        call = values[0]
                        # This is forwarding metadata, not a call-effect proof.
                        if all(a.get('type') not in ('AstExprCall', 'AstExprVarargs') for a in call['args']):
                            self.functions[fid]['forward'] = dict(reference=self.reference(call['func']), arity=len(call['args']))
            for node in walk(expression['body'], skip_functions=True):
                if node.get('type') == 'AstExprCall':
                    self.calls.append(dict(function=fid, location=node['location'], reference=self.reference(node['func']),
                                           arguments=len(node['args']), self_call=node.get('self', False),
                                           argument_bindings=[list(binding(a)) if binding(a) else None for a in node['args']],
                                           tail_open=bool(node['args']) and node['args'][-1].get('type') in ('AstExprCall', 'AstExprVarargs')))
        self.find_exports()

    def reference(self, expression):
        if id(expression) in self.require_values:
            return dict(module=self.require_values[id(expression)], export='default')
        key = binding(expression)
        if key and self.writes[key] == 1:
            value = self.definitions.get(key, {})
            if value.get('type') == 'AstExprFunction':
                return dict(function=self.function_ids[id(value)])
            if key in self.imports:
                return dict(module=self.imports[key], export='default')
        item = field(expression)
        if item and binding(item[0]) in self.imports:
            return dict(module=self.imports[binding(item[0])], export=item[1])
        if expression.get('type') == 'AstExprFunction':
            return dict(function=self.function_ids[id(expression)])
        return None

    def scalar_role(self, expression):
        kind = expression.get('type')
        key = binding(expression)
        if key:
            return dict(kind='local_read', observed_name=key[0], declaration=key[1])
        item = field(expression)
        if item:
            return dict(kind='field_read', role=item[1])
        if kind in ('AstExprConstantNil', 'AstExprConstantBool', 'AstExprConstantNumber', 'AstExprConstantString'):
            return dict(kind=kind)
        return None

    def find_exports(self):
        body = self.root.get('body', [])
        if not body or body[-1].get('type') != 'AstStatReturn' or len(body[-1]['list']) != 1:
            return
        if any(n.get('type') not in ('AstStatLocal', 'AstStatLocalFunction', 'AstStatFunction', 'AstStatAssign', 'AstStatExpr',
                                     'AstStatTypeAlias', 'AstStatTypeFunction') for n in body[:-1]):
            self.export_status = 'unknown_control_flow'
            return
        value = body[-1]['list'][0]
        reference = self.reference(value)
        if reference:
            self.exports['default'] = reference
            self.export_status = 'static_function_or_forward'
            return
        key = binding(value)
        if key:
            if self.writes[key] != 1:
                self.export_status = 'mutable_export_binding'
                return
            value = self.definitions.get(key, {})
        if value.get('type') != 'AstExprTable':
            return
        # Table exports require a literal with no aliases, mutation or escaping
        # use. Rich module-builder/method patterns remain unknown in this model.
        if key:
            references = [n for n in walk(self.root) if n.get('type') == 'AstExprLocal' and binding(n) == key]
            if len(references) != 1:
                self.export_status = 'export_table_observed_or_mutated'
                return
        exports = {}
        for item in value['items']:
            name = item.get('key', {})
            if name.get('type') != 'AstExprConstantString' or name['value'] in exports:
                self.export_status = 'dynamic_or_duplicate_export_key'
                return
            exports[name['value']] = self.reference(item['value'])
        self.exports = exports
        self.export_status = 'private_literal_exports'


def summarize(entries, trees):
    if len(entries) > LIMITS['modules']:
        raise ValueError('module budget')
    ids = [e['id'] for e in entries]
    if len(ids) != len(set(ids)):
        raise ValueError('duplicate module ID')
    paths = collections.defaultdict(list)
    for entry in entries:
        paths[tuple(entry['script_path'])].append(entry['id'])
    modules, errors = {}, {}
    for entry in sorted(entries, key=lambda e: e['id']):
        try:
            modules[entry['id']] = Module(entry, trees[entry['id']], paths)
        except (ValueError, KeyError, TypeError) as error:
            errors[entry['id']] = str(error)
    graph = {key: {r['candidates'][0] for r in module.requires
                   if r['status'] in ('resolved_static_path', 'mutable_import_binding')
                   and len(r['candidates']) == 1 and r['candidates'][0] in modules}
             for key, module in modules.items()}
    groups = components(graph)
    functions = {key: value for module in modules.values() for key, value in module.functions.items()}

    def resolve(reference):
        seen = set()
        for _ in range(LIMITS['rounds']):
            if not reference:
                return None
            if 'function' in reference:
                return reference['function'] if reference['function'] in functions else None
            key = reference['module'], reference['export']
            if key in seen or key[0] not in modules:
                return None
            seen.add(key)
            reference = modules[key[0]].exports.get(key[1])
        return None

    rounds = 0
    for rounds in range(1, LIMITS['rounds'] + 1):
        pending = []
        for fid, function in sorted(functions.items()):
            forward = function['forward']
            if function['returns'] is None and forward:
                target = functions.get(resolve(forward['reference']))
                if target and not target['variadic'] and len(target['parameters']) == forward['arity'] and target['returns'] is not None:
                    pending.append((fid, target['returns']))
        if not pending:
            break
        for fid, returns in pending:
            # Keep the origin module/function's identifiers and identity, never
            # attribute them to the wrapper's local bindings by equal spelling.
            functions[fid]['returns'] = [dict(slot, forwarded=True) for slot in returns]
    rows = []
    for key, module in sorted(modules.items()):
        calls = []
        for call in module.calls:
            target = resolve(call['reference']) if not call['self_call'] else None
            function = functions.get(target)
            exact = function and not function['variadic'] and not call['tail_open'] and len(function['parameters']) == call['arguments']
            roles = [dict(position=i, argument_binding=key, parameter=function['parameters'][i], origin_function=target)
                     for i, key in enumerate(call['argument_bindings']) if key] if exact else []
            calls.append(dict(call, resolved_function=target, argument_roles=roles,
                              arity_status='fixed_exact' if exact else 'unknown'))
        rows.append(dict(module=key, requires=module.requires, exports=module.exports, export_status=module.export_status,
                         functions=list(module.functions.values()), calls=calls))
    return dict(schema_version=1, model='static-module-naming-v1', limits=LIMITS, rounds=rounds,
                propagation_round_limit_hit=rounds == LIMITS['rounds'] and bool(pending),
                context='Script paths are supplied by the operator. Resolutions are syntactic naming candidates under that layout; no runtime require/callee identity, load order, effect, alias or totality proof.',
                sccs=[dict(modules=g, cyclic=len(g) > 1 or g[0] in graph[g[0]]) for g in groups],
                summary=dict(modules=len(entries), analyzed=len(modules), errors=len(errors), functions=len(functions),
                             require_status=dict(collections.Counter(r['status'] for m in modules.values() for r in m.requires)),
                             export_status=dict(collections.Counter(m.export_status for m in modules.values())),
                             resolved_calls=sum(c['resolved_function'] is not None for r in rows for c in r['calls']),
                             unknown_returns=sum(f['returns'] is None for f in functions.values())),
                rows=rows, errors=errors)


def check_expectations(report, expected):
    modules = {row['module']: row for row in report['rows']}
    for module, rules in expected.items():
        row = modules[module]
        if 'export_status' in rules and row['export_status'] != rules['export_status']:
            raise ValueError('export expectation failed: ' + module)
        if 'require_status' in rules and [r['status'] for r in row['requires']] != rules['require_status']:
            raise ValueError('require expectation failed: ' + module)
        if rules.get('unknown_returns') and any(f['returns'] is not None for f in row['functions']):
            raise ValueError('cycle/dynamic return was guessed: ' + module)
        if 'return_origin' in rules:
            returns = row['functions'][0]['returns']
            if not returns or len(returns) != rules['return_arity'] or any(r['origin_function'] != rules['return_origin'] for r in returns):
                raise ValueError('forwarded result identity changed: ' + module)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('manifest', 'root', 'ast', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text(encoding='utf-8'))
    if manifest.get('schema_version') != 1:
        parser.error('unsupported manifest')
    trees, inputs, parse_errors = {}, [], {}
    if len(manifest['modules']) > LIMITS['modules']:
        parser.error('module budget')
    project_bytes = project_nodes = 0
    for entry in manifest['modules']:
        path = fixture_path(args.root.resolve(), entry['file'])
        project_bytes += path.stat().st_size
        if project_bytes > LIMITS['project_source_bytes']:
            parser.error('project source byte budget')
        if path.stat().st_size > LIMITS['source_bytes']:
            parser.error('source byte budget: ' + entry['id'])
        if sha256(path) != entry['source_sha256']:
            parser.error('source hash mismatch: ' + entry['id'])
        try:
            tree = parse_ast(args.ast, path)
            project_nodes += sum(1 for _ in walk(tree))
            if project_nodes > LIMITS['project_nodes']:
                parser.error('project AST node budget')
            trees[entry['id']] = tree
        except (ValueError, OSError) as error:
            parse_errors[entry['id']] = str(error)
        inputs.append(dict(entry))
    report = summarize(manifest['modules'], trees)
    report['errors'].update(parse_errors)
    if manifest.get('expected'):
        check_expectations(report, manifest['expected'])
        report['fixture_expectations_passed'] = True
    report.update(manifest_sha256=sha256(args.manifest), ast_sha256=sha256(args.ast), inputs=inputs,
                  project_source_bytes=project_bytes, project_ast_nodes=project_nodes)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary'], indent=2))
    return int(bool(report['errors']))


if __name__ == '__main__':
    raise SystemExit(main())
