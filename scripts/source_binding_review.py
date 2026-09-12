#!/usr/bin/env python3
"""Compare R2 output without exporting source text or claiming VM equivalence."""
import argparse
import collections
import concurrent.futures
import json
import pathlib
import re
import subprocess

from roadmap_v2 import sha256
from source_fidelity import canonicalize, parse_ast


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('before', 'after', 'compiler', 'ast', 'report'):
        parser.add_argument('--' + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    before, after = args.before.resolve(strict=True), args.after.resolve(strict=True)
    old_paths = {p.relative_to(before).as_posix(): p for p in before.rglob('*.luau')}
    new_paths = {p.relative_to(after).as_posix(): p for p in after.rglob('*.luau')}
    if not old_paths or old_paths.keys() != new_paths.keys():
        raise ValueError('source path inventories differ or are empty')
    changed = [name for name in sorted(old_paths) if old_paths[name].read_bytes() != new_paths[name].read_bytes()]

    def check(name):
        old, new = old_paths[name], new_paths[name]
        row = dict(path=name, before_sha256=sha256(old), after_sha256=sha256(new), status='failed')
        try:
            a, old_names, old_types = canonicalize(parse_ast(args.ast, old))
            b, new_names, new_types = canonicalize(parse_ast(args.ast, new))
            alpha_equal = a == b and old_types == new_types
            row['only_binding_names_or_trivia'] = alpha_equal
            # Only aligned, otherwise identical syntax supports this comparison.
            if alpha_equal:
                changed_names = [(old_names[k], new_names[k]) for k in old_names
                                 if old_names[k] != new_names[k]]
                row['renamed_bindings'] = len(changed_names)
                regressions = [(a, b) for a, b in changed_names if
                               not re.fullmatch(r'[pv]\d*', a) and re.fullmatch(r'selected\d*', b)]
                row['specific_names_replaced_by_generic_selected'] = len(regressions)
                if regressions:
                    raise ValueError('a specific existing name became generic selected')
            for flags in (['--only-parse'], ['--binary', '-O0'], ['--binary', '-O2']):
                result = subprocess.run([str(args.compiler), *flags, '--fflags=false', str(new)],
                                        capture_output=True, timeout=60)
                if result.returncode:
                    raise ValueError('changed output failed pinned compiler check: ' + ' '.join(flags))
            row['status'] = 'passed'
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            row['error'] = str(error)
        return row

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        rows = list(pool.map(check, changed))
    summary = dict(source_files=len(old_paths), changed_files=len(changed),
                   status=dict(collections.Counter(r['status'] for r in rows)),
                   only_binding_names_or_trivia=sum(r.get('only_binding_names_or_trivia', False) for r in rows),
                   renamed_bindings=sum(r.get('renamed_bindings', 0) for r in rows),
                   specific_name_regressions=sum(r.get('specific_names_replaced_by_generic_selected', 0) for r in rows))
    report = dict(schema_version=1, summary=summary, compiler_sha256=sha256(args.compiler),
                  ast_sha256=sha256(args.ast), rows=rows,
                  contract='Identical parser AST after lexical binding renaming and trivia removal establishes syntax/binding shape only. Other changes remain unclassified. Changed outputs must parse and compile at O0/O2. No VM equivalence or original-name recovery is inferred; source text is not exported.')
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + '\n', encoding='utf-8', newline='\n')
    print(json.dumps(summary, indent=2))
    return int(any(row['status'] != 'passed' for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
