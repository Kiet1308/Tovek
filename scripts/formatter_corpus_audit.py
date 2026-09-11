#!/usr/bin/env python3
"""Audit compound-assignment expansion and input bytecode read-count witnesses.

AST expansion is a shape check, deliberately NOT an equivalence certificate:
the purpose of this fix is to preserve repeated observable base/key evaluation.
"""
import argparse
import collections
import concurrent.futures
import copy
import json
import pathlib
import subprocess
import tempfile

from bytecode_dataflow import compare_dataflow
from bytecode_roundtrip import OPCODES, const_repr, parse_chunk, read_saved_bytecode
from roadmap_v2 import sha256
from source_fidelity import canonicalize, parse_ast


def expand(node):
    if isinstance(node, list):
        return [expand(v) for v in node]
    if not isinstance(node, dict):
        return node
    node = {k: expand(v) for k, v in node.items()}
    if node.get('type') == 'AstStatCompoundAssign':
        return dict(type='AstStatAssign', vars=[node['var']], values=[dict(
            type='AstExprBinary', op=node['op'], left=copy.deepcopy(node['var']), right=node['value'])])
    return node


def read_counts(chunk):
    result = {}
    names = [chunk.strings[p.name - 1].decode('utf-8', 'surrogateescape') if p.name else '<anonymous>' for p in chunk.protos]
    duplicates = collections.Counter(names)
    for name, proto in zip(names, chunk.protos):
        if duplicates[name] != 1:
            continue
        counts = collections.Counter()
        for pc, op, a, b, c, d, e, aux in proto.insns:
            kind = OPCODES[op]
            if kind == 'GETTABLEKS':
                counts[kind + ':' + const_repr(chunk, proto, aux & 0xffffff)] += 1
            elif kind in ('GETTABLE', 'GETTABLEN'):
                counts[kind] += 1
        result[name] = counts
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('before', 'after', 'input', 'ast', 'compiler', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    parser.add_argument('--key', type=int, default=203)
    args = parser.parse_args()
    paths = lambda root: {p.relative_to(root).as_posix(): p for p in root.rglob('*.luau')}
    before, after = paths(args.before), paths(args.after)
    if before.keys() != after.keys():
        parser.error('source file sets differ')
    changed = [key for key in sorted(before) if sha256(before[key]) != sha256(after[key])]

    def check(key):
        row = dict(file=key, before_sha256=sha256(before[key]), after_sha256=sha256(after[key]))
        try:
            a, b = parse_ast(args.ast, before[key]), parse_ast(args.ast, after[key])
            if canonicalize(expand(a)) != canonicalize(expand(b)):
                raise ValueError('change is not solely compound-assignment expansion')
            source = args.input / pathlib.Path(key).with_suffix('.lua')
            original = parse_chunk(read_saved_bytecode(source), args.key)
            chunks = []
            with tempfile.TemporaryDirectory(prefix='tovek_compound_') as temporary:
                for path in (before[key], after[key]):
                    staged = pathlib.Path(temporary) / 'input.luau'
                    staged.write_bytes(path.read_bytes())
                    raw = subprocess.check_output([str(args.compiler), '--binary', '-O2', '-g1', '--fflags=false',
                        '--vector-lib=Vector3', '--vector-ctor=new', '--vector-type=Vector3', str(staged)], timeout=30)
                    chunks.append(parse_chunk(raw, 1))
            old, new = (compare_dataflow(original, chunk) for chunk in chunks)
            if old['status'] == 'proved' and new['status'] != 'proved':
                raise ValueError('lost an existing bounded input certificate')
            original_counts, old_counts, new_counts = map(read_counts, (original, *chunks))
            witnesses = []
            for function in sorted(old_counts.keys() & new_counts.keys()):
                for operation in sorted(old_counts[function].keys() | new_counts[function].keys()):
                    left, right = old_counts[function][operation], new_counts[function][operation]
                    if left != right:
                        known = original_counts.get(function, {}).get(operation)
                        witnesses.append(dict(function=function, operation=operation, original=known, before=left, after=right,
                                              after_matches_input=known == right if known is not None else None))
            row.update(status='passed', original_source_sha256=sha256(source), before_dataflow=old, after_dataflow=new,
                       changed_read_counts=witnesses)
        except (ValueError, OSError, subprocess.SubprocessError, TypeError) as error:
            row.update(status='failed', error=str(error))
        return row

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        rows = list(pool.map(check, changed))
    report = dict(schema_version=1, compiler_sha256=sha256(args.compiler), ast_sha256=sha256(args.ast),
                  summary=dict(total_files=len(before), unchanged_files=len(before) - len(rows), changed_files=len(rows),
                               status=dict(collections.Counter(r['status'] for r in rows))), rows=rows,
                  contract='Only AST compound-assignment expansion is permitted. Ordered runtime counterexamples validate the formatter gate separately. Read counts in uniquely named prototypes are diagnostic witnesses, not an equivalence proof. Whole-chunk unknown remains unknown.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(report['summary'], indent=2))
    return int(any(r['status'] != 'passed' for r in rows))


if __name__ == '__main__':
    raise SystemExit(main())
