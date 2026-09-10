#!/usr/bin/env python3
"""Verify a presentation-only change with the pinned Luau AST and line inventory.

Requires equal binding graphs, local names, operators, constants and type syntax.
Only parser trivia/locations are ignored. Unknown/missing/changed results fail.
Line classifications are a descriptive syntax inventory, not a fidelity score.
"""
import argparse
import collections
import concurrent.futures
import json
import pathlib
import re

from roadmap_v2 import sha256
from source_fidelity import canonicalize, conditional_count, parse_ast


def long_lines(text, tree, limit):
    literal_ranges = []

    def visit(node):
        if isinstance(node, list):
            for value in node:
                visit(value)
        elif isinstance(node, dict):
            if node.get("type") in ("AstExprConstantString", "AstExprInterpString"):
                match = re.fullmatch(r"(\d+),(\d+) - (\d+),(\d+)", node.get("location", ""))
                if match:
                    literal_ranges.append(tuple(map(int, match.groups())))
            for value in node.values():
                visit(value)

    visit(tree)
    rows = []
    for index, line in enumerate(text.splitlines()):
        width = len(line.expandtabs(4))
        if width <= limit:
            continue
        stripped = line.lstrip()
        literal = any(start <= index <= end and (start != end or last - first > limit // 2)
                      for start, first, end, last in literal_ranges)
        if literal:
            kind = "literal"
        elif re.match(r"(?:if|elseif|while|until|for)\b", stripped):
            kind = "control_flow"
        elif "{" in line or "function(" in line or "function (" in line:
            kind = "constructor_or_callback"
        elif stripped.startswith("return "):
            kind = "return_expression"
        else:
            kind = "expression"
        rows.append({"line": index + 1, "columns": width, "kind": kind})
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=pathlib.Path, required=True)
    parser.add_argument("--after", type=pathlib.Path, required=True)
    parser.add_argument("--ast", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    parser.add_argument("--line-limit", type=int, default=180)
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    paths = lambda root: {path.relative_to(root).as_posix(): path for path in root.rglob("*.luau")}
    before, after = paths(args.before), paths(args.after)

    def check(relative):
        row = {"file": relative}
        if relative not in before or relative not in after:
            return row | {"status": "missing_file"}
        try:
            first, second = before[relative], after[relative]
            row.update(before_sha256=sha256(first), after_sha256=sha256(second))
            a, b = first.read_text(encoding="utf-8"), second.read_text(encoding="utf-8")
            right = parse_ast(args.ast.resolve(), second, args.timeout)
            left = right if a == b else parse_ast(args.ast.resolve(), first, args.timeout)
            row["status"] = "identical_text" if a == b else "equal_ast" if canonicalize(left) == canonicalize(right) else "changed_ast"
            row["before_long_lines"] = long_lines(a, left, args.line_limit)
            row["after_long_lines"] = long_lines(b, right, args.line_limit)
            row["before_conditional_expressions"] = conditional_count(left)
            row["after_conditional_expressions"] = conditional_count(right)
        except Exception as error:
            row.update(status="unknown", reason=f"{type(error).__name__}: {error}")
        return row

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        rows = list(pool.map(check, sorted(before.keys() | after.keys())))
    summary = {"files": len(rows), "status": dict(collections.Counter(r["status"] for r in rows))}
    for side in ("before", "after"):
        summary[f"{side}_long_lines"] = dict(collections.Counter(
            line["kind"] for row in rows for line in row.get(f"{side}_long_lines", [])))
        summary[f"{side}_conditional_expressions"] = sum(row.get(f"{side}_conditional_expressions", 0) for row in rows)
    report = {"schema_version": 1, "ast_sha256": sha256(args.ast), "line_limit": args.line_limit,
              "columns": "Unicode characters, tabs expanded to four-column stops; no terminal-width or grapheme claim",
              "model": "pinned-luau-ast-with-binding-and-type-syntax-equality", "summary": summary, "rows": rows,
              "limitations": "Strict syntax equality ignores parser trivia/locations; it is appropriate for layout, not a general semantic-equivalence proof. Long-line categories are descriptive heuristics."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(summary, indent=2))
    return int(not rows or any(r["status"] not in ("identical_text", "equal_ast") for r in rows))


if __name__ == "__main__":
    raise SystemExit(main())
