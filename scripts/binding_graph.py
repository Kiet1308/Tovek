#!/usr/bin/env python3
"""Export lexical declaration identities, separately from final IR storage ancestry.

This optional tool never edits source. The pinned parser resolves scope, including
shadowing, recursive functions, loop bindings, repeat scope and implicit self.
Neither a storage ID nor a generated spelling proves an original source binding.
"""
import argparse
import bisect
import collections
import concurrent.futures
import hashlib
import json
import pathlib
import re
import subprocess
import tempfile
import threading
import time

from emission_map_audit import validate_emission_map
from provenance_audit import manifest, validate_trace


MODEL = 'luau-lexical-declarations-v1'
LIMITS = dict(source_bytes=4 * 1024 * 1024, json_bytes=64 * 1024 * 1024,
              nodes=200000, depth=256, declarations=50000, tokens=100000,
              parser_seconds=30, stderr_bytes=1024 * 1024)
LOCATION = re.compile(r'(\d+),(\d+) - (\d+),(\d+)\Z')


class Refused(ValueError):
    """A bounded/unsupported input produces no partial graph."""


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_bounded(path, limit):
    with path.open('rb') as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise Refused('file_byte_budget')
    return data


def parse_source(executable, source, limits=LIMITS):
    """Bound both pipe buffers while the parser runs, including on Windows."""
    if len(source) > limits['source_bytes']:
        raise Refused('source_byte_budget')
    with tempfile.TemporaryDirectory(prefix='tovek-binding-') as directory:
        path = pathlib.Path(directory) / 'input.luau'
        path.write_bytes(source)
        started = time.monotonic()
        process = subprocess.Popen([str(executable.resolve()), str(path)],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        buffers, exceeded = [bytearray(), bytearray()], threading.Event()
        def drain(pipe, index, limit):
            with pipe:
                while chunk := pipe.read(65536):
                    if len(buffers[index]) + len(chunk) > limit:
                        exceeded.set()
                        process.kill()
                        return
                    buffers[index].extend(chunk)
        workers = [threading.Thread(target=drain, args=(pipe, i, limit))
                   for i, (pipe, limit) in enumerate(((process.stdout, limits['json_bytes']),
                                                     (process.stderr, limits['stderr_bytes'])))]
        for worker in workers:
            worker.start()
        try:
            remaining = limits['parser_seconds'] - (time.monotonic() - started)
            if remaining <= 0:
                raise subprocess.TimeoutExpired(process.args, limits['parser_seconds'])
            process.wait(timeout=remaining)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            raise Refused('parser_time_budget') from None
        finally:
            for worker in workers:
                worker.join()
        if exceeded.is_set():
            raise Refused('parser_output_budget')
        if process.returncode:
            raise Refused('parser_failed: ' + buffers[1][:500].decode('utf-8', errors='replace'))
        try:
            # Invalid bytes can occur in string literal payloads, not identifiers.
            return json.loads(buffers[0].decode('utf-8', errors='surrogateescape'))['root']
        except (ValueError, KeyError, RecursionError) as error:
            raise Refused('parser_json_invalid_or_deep') from error


def lexical_graph(root, source, limits=LIMITS):
    if len(source) > limits['source_bytes']:
        raise Refused('source_byte_budget')
    starts = [0] + [i + 1 for i, byte in enumerate(source) if byte == 10]
    def span(location):
        match = LOCATION.fullmatch(location)
        if not match:
            raise Refused('invalid_parser_location')
        a, b, c, d = map(int, match.groups())
        if a >= len(starts) or c >= len(starts):
            raise Refused('parser_location_outside_source')
        begin, end = starts[a] + b, starts[c] + d
        if not (0 <= begin <= end <= len(source)) or any(
                column > (starts[line + 1] - starts[line] - 1 if line + 1 < len(starts)
                          else len(source) - starts[line]) for line, column in ((a, b), (c, d))):
            raise Refused('parser_location_outside_line')
        return begin, end

    declarations, tokens = {}, {}
    functions = [dict(function_id='f0', parent=None, span=[0, len(source)])]
    # node, owner function, declaration/use role, type-only context, depth
    stack = [(root, 'f0', 'read', False, 0)]
    visited = 0
    while stack:
        node, owner, role, type_only, depth = stack.pop()
        visited += 1
        if visited > limits['nodes'] or len(stack) > limits['nodes']:
            raise Refused('ast_node_budget')
        if depth > limits['depth']:
            raise Refused('ast_depth_budget')
        if isinstance(node, list):
            if len(node) + len(stack) > limits['nodes']:
                raise Refused('ast_node_budget')
            stack.extend((child, owner, role, type_only, depth + 1) for child in reversed(node))
            continue
        if not isinstance(node, dict):
            continue
        kind = node.get('type', '')
        type_only |= kind.startswith(('AstType', 'AstStatType', 'AstGeneric'))
        if kind == 'AstExprFunction':
            previous = owner
            owner = 'f' + str(len(functions))
            functions.append(dict(function_id=owner, parent=previous, span=list(span(node['location']))))
        if kind in ('AstLocal', 'AstExprLocal'):
            binding = node if kind == 'AstLocal' else node['local']
            key = binding['name'], binding['location']
            bounds = span(node['location'])
            if kind == 'AstLocal':
                if key in declarations:
                    raise Refused('duplicate_parser_declaration')
                if role not in ('local', 'parameter', 'implicit_self', 'iteration', 'local_function'):
                    raise Refused('unrecognized_declaration_context')
                if len(declarations) >= limits['declarations']:
                    raise Refused('declaration_budget')
                declarations[key] = dict(name=key[0], parser_location=key[1], owner_function=owner,
                                         kind=role, declaration_span=list(bounds) if role != 'implicit_self' else None,
                                         anchor_span=list(bounds))
            if kind == 'AstExprLocal' or role != 'implicit_self':
                if source[bounds[0]:bounds[1]] != key[0].encode('utf-8') or bounds in tokens:
                    raise Refused('invalid_or_duplicate_local_token')
                if len(tokens) >= limits['tokens']:
                    raise Refused('token_budget')
                tokens[bounds] = dict(key=key, owner_function=owner, type_only=type_only,
                                     role='declaration' if kind == 'AstLocal' else role)
            if kind == 'AstExprLocal':
                # Its AstLocal is a reference to the declaration, not another declaration.
                continue
            if node.get('luauType') is not None:
                stack.append((node['luauType'], owner, 'read', True, depth + 1))
            continue
        children = []
        for field, child in node.items():
            if not isinstance(child, (list, dict)):
                continue
            child_role = 'read'
            if kind == 'AstExprFunction' and field in ('args', 'self'):
                child_role = 'parameter' if field == 'args' else 'implicit_self'
            elif kind == 'AstStatLocal' and field == 'vars':
                child_role = 'local'
            elif kind == 'AstStatLocalFunction' and field == 'name':
                child_role = 'local_function'
            elif kind in ('AstStatFor', 'AstStatForIn') and field in ('var', 'vars'):
                child_role = 'iteration'
            elif kind == 'AstStatAssign' and field == 'vars' or kind == 'AstStatFunction' and field == 'name':
                child_role = 'write'
            elif kind == 'AstStatCompoundAssign' and field == 'var':
                child_role = 'read_write'
            children.append((child, owner, child_role, type_only, depth + 1))
        stack.extend(reversed(children))

    ordered = sorted(declarations, key=lambda key: (span(key[1]), key[0]))
    ids = {key: 'd' + str(i) for i, key in enumerate(ordered)}
    rows = []
    for key in ordered:
        rows.append(dict(declaration_id=ids[key], **declarations[key], tokens=[]))
    by_id = {row['declaration_id']: row for row in rows}
    parents = {row['function_id']: row['parent'] for row in functions}
    for bounds, token in sorted(tokens.items()):
        key = token.pop('key')
        if key not in ids:
            raise Refused('reference_without_declaration')
        row = by_id[ids[key]]
        # Catch parser/schema drift that would attach a use to a sibling function.
        ancestor = token['owner_function']
        while ancestor is not None and ancestor != row['owner_function']:
            ancestor = parents[ancestor]
        if ancestor is None:
            raise Refused('reference_outside_owner_function')
        row['tokens'].append(dict(span=list(bounds), **token))
    for row in rows:
        row['captured_in_output'] = any(t['owner_function'] != row['owner_function'] and not t['type_only']
                                         for t in row['tokens'])
        row['written_in_output'] = any(t['role'] in ('write', 'read_write') and not t['type_only']
                                       for t in row['tokens'])
        # Parser evidence covers lexical capture, not VM REF/VAL mode or CLOSE.
        row['storage_id'] = None
        row['recorded_identity_status'] = 'no_storage_mapping'
        row['recorded_origins'] = []
    return dict(schema_version=1, model=MODEL, source_sha256=digest(source), limits=limits,
                visited_nodes=visited, functions=functions, declarations=rows, storage=[])


def attach_storage(graph, metadata, source):
    """Join exact identifier spans; never expand storage origins into source equality."""
    if metadata['source_sha256'] != digest(source) or graph['source_sha256'] != digest(source):
        raise Refused('source_hash_mismatch')
    trace = metadata['binding_provenance']
    errors = validate_trace(trace) + validate_emission_map(trace, source)
    if errors:
        raise Refused('invalid_trace: ' + '; '.join(errors[:5]))
    token_map = {tuple(token['span']): (row, token) for row in graph['declarations'] for token in row['tokens']}
    covered = set()
    storage_declarations = collections.defaultdict(set)
    for occurrence in trace['output_map']['bindings']:
        bounds = tuple(occurrence['span'][end]['byte_offset'] for end in ('start', 'end'))
        if bounds not in token_map:
            raise Refused('emitted_token_not_parser_local')
        row, token = token_map[bounds]
        bid = occurrence['binding_id']
        if row['storage_id'] is not None and row['storage_id'] != bid:
            raise Refused('conflicting_storage_for_lexical_binding')
        row['storage_id'] = bid
        token['emitter_role'] = occurrence['role']
        covered.add(bounds)
        storage_declarations[bid].add(row['declaration_id'])
    opaque = sorted((r['span']['start']['byte_offset'], r['span']['end']['byte_offset'])
                    for r in trace['output_map']['opaque_regions'])
    # Prefix maximum permits overlapping regions without an O(tokens * regions) scan.
    opaque_starts, opaque_ends = [], []
    for start, end in opaque:
        opaque_starts.append(start)
        opaque_ends.append(max(end, opaque_ends[-1] if opaque_ends else 0))
    for bounds, (_, token) in token_map.items():
        index = bisect.bisect_right(opaque_starts, bounds[0]) - 1
        in_opaque = index >= 0 and bounds[1] <= opaque_ends[index]
        token['storage_mapping'] = ('mapped' if bounds in covered else 'opaque' if in_opaque
                                    else 'output_map_budget' if trace['output_map']['omitted_occurrences'] else 'missing')
        if token['storage_mapping'] == 'missing':
            raise Refused('unexplained_unmapped_local_token')
    origin_index = {}
    for function in trace['functions']:
        for register in function['registers']:
            origin_index[register['id']] = dict(kind=register['kind'], prototype=function['prototype'],
                                                function_id=function['function_id'], slot=register['slot'])
    recorded = {row['binding_id']: row for row in metadata.get('source_recovery', {}).get('bindings', [])}
    for final in trace['final_bindings']:
        bid = final['binding_id']
        lexical = sorted(storage_declarations[bid], key=lambda d: int(d[1:]))
        graph['storage'].append(dict(storage_id=bid, declarations=lexical,
            lineage=final['lineage'], incomplete=final['incomplete'],
            has_conditional_result_ancestry=final.get('has_conditional_result_ancestry', False),
            input_slots=[dict(origin_id=origin, **origin_index[origin]) for origin in final['lineage'] if origin in origin_index],
            recorded_origins=recorded.get(bid, {}).get('origins', [])))
    for row in graph['declarations']:
        bid = row['storage_id']
        if bid is None:
            continue
        evidence = recorded.get(bid)
        if len(storage_declarations[bid]) != 1:
            row['recorded_identity_status'] = 'ambiguous_shared_storage'
        elif evidence and evidence.get('origins'):
            row['recorded_identity_status'] = 'recorded_on_unique_output_binding'
            row['recorded_origins'] = evidence['origins']
        else:
            row['recorded_identity_status'] = 'unrecorded'
        # Protect any recorded evidence, even when shared-storage attribution is ambiguous.
        row['protect_recorded_name'] = bool(evidence and evidence.get('origins'))
    return graph


def summarize(graph):
    rows = graph['declarations']
    result = collections.Counter(declarations=len(rows), functions=len(graph['functions']),
        tokens=sum(len(row['tokens']) for row in rows), captured_bindings=sum(row['captured_in_output'] for row in rows),
        written_bindings=sum(row['written_in_output'] for row in rows),
        shared_storage_ids=sum(len(row['declarations']) > 1 for row in graph['storage']))
    result.update('kind_' + row['kind'] for row in rows)
    result.update('identity_' + row['recorded_identity_status'] for row in rows)
    result.update('token_' + token['storage_mapping'] for row in rows for token in row['tokens'] if 'storage_mapping' in token)
    return dict(result)


CONTRACT = ('Lexical IDs identify declarations in these exact output bytes, independently of IR storage. '
            'Current syntax/capture roles and historical storage ancestry are separate. Recorded origins '
            'on shared storage remain ambiguous. No unique original source identity, compiler-temporary, '
            'synthesis, precise nested value/PC, REF/VAL, CLOSE, ownership or effect proof is inferred.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument('--source', type=pathlib.Path)
    inputs.add_argument('--root', type=pathlib.Path, help='Decompiled directory with provenance analysis manifest')
    parser.add_argument('--ast', type=pathlib.Path, required=True)
    parser.add_argument('--report', type=pathlib.Path, required=True)
    parser.add_argument('--graphs', type=pathlib.Path, help='Separate content-addressed graph directory')
    parser.add_argument('--threads', type=int, choices=range(1, 17), default=4)
    args = parser.parse_args()
    parser_hash = digest(args.ast.read_bytes())
    if args.source:
        source = read_bounded(args.source, LIMITS['source_bytes'])
        graph = lexical_graph(parse_source(args.ast, source), source)
        graph.update(ast_sha256=parser_hash, contract=CONTRACT)
        result = graph | dict(summary=summarize(graph))
    else:
        root = args.root.resolve()
        _, entries = manifest(root)
        if args.graphs:
            args.graphs.mkdir(parents=True, exist_ok=True)
        def check(key):
            start = time.perf_counter()
            row = dict(script_path=key)
            try:
                entry = entries[key]
                path = (root / entry['sidecar_path']).resolve()
                if not path.is_relative_to(root):
                    raise Refused('sidecar_path_outside_root')
                raw = read_bounded(path, LIMITS['json_bytes'])
                if digest(raw) != entry['sidecar_sha256']:
                    raise Refused('sidecar_hash_mismatch')
                metadata = json.loads(raw)
                path = (root / metadata['source_path']).resolve()
                if not path.is_relative_to(root):
                    raise Refused('source_path_outside_root')
                source = read_bounded(path, LIMITS['source_bytes'])
                graph = attach_storage(lexical_graph(parse_source(args.ast, source), source), metadata, source)
                graph.update(ast_sha256=parser_hash, sidecar_sha256=digest(raw), contract=CONTRACT)
                encoded = (json.dumps(graph, sort_keys=True, separators=(',', ':')) + '\n').encode('utf-8')
                graph_hash = digest(encoded)
                if args.graphs:
                    # Content addressing: duplicate source/metadata writes have identical bytes.
                    with tempfile.NamedTemporaryFile(dir=args.graphs, delete=False) as stream:
                        stream.write(encoded)
                        temp = pathlib.Path(stream.name)
                    temp.replace(args.graphs / (graph_hash + '.json'))
                row.update(status='passed', source_sha256=digest(source), sidecar_sha256=digest(raw),
                           graph_sha256=graph_hash, summary=summarize(graph))
            except (ValueError, KeyError, TypeError, OSError, RecursionError, subprocess.SubprocessError) as error:
                row.update(status='refused' if isinstance(error, Refused) else 'failed', reason=str(error))
            row['seconds'] = time.perf_counter() - start
            return row
        started = time.perf_counter()
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.threads) as pool:
            rows = list(pool.map(check, sorted(entries)))
        totals = collections.Counter()
        for row in rows:
            totals.update(row.get('summary', {}))
        result = dict(schema_version=1, model=MODEL, contract=CONTRACT, ast_sha256=parser_hash,
                      manifest_sha256=digest((root / '.tovek-analysis/manifest.json').read_bytes()),
                      limits=LIMITS, threads=args.threads, wall_seconds=time.perf_counter() - started,
                      summary=dict(scripts=len(rows), status=dict(collections.Counter(r['status'] for r in rows)), **totals), rows=rows)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(result, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(result['summary'], indent=2))
    return int(bool(args.root) and (not result['rows'] or any(row['status'] != 'passed' for row in result['rows'])))


if __name__ == '__main__':
    raise SystemExit(main())
