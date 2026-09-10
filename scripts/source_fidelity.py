"""Binding-aware source metrics using JSON from the pinned official luau-ast CLI.

Scores describe syntax, never runtime equivalence. Binding matches require a
one-to-one alignment of EVERY declaration/reference occurrence on both sides;
partial or conflicting matches remain unaligned and in the coverage denominator.
"""
from __future__ import annotations

import collections
import difflib
import json
import subprocess


TRIVIA = {"location", "varargLocation", "functionDepth", "debugname", "hasEnd",
          "hasThen", "hasDo", "hasIn", "indexLocation", "opPosition", "argLocation"}
TYPE_FIELDS = {"luauType", "annotation", "returnAnnotation", "varargAnnotation",
               "generics", "genericPacks"}
TYPE_STATEMENTS = {"AstStatTypeAlias", "AstStatTypeFunction", "AstStatDeclareFunction",
                   "AstStatDeclareGlobal", "AstStatDeclareExternType"}


def parse_ast(executable, source, timeout=30):
    result = subprocess.run([str(executable), str(source)], capture_output=True, timeout=timeout)
    if result.returncode:
        raise ValueError(result.stderr.decode(errors="replace")[:2000])
    # The pinned CLI writes string-constant bytes directly, including non-UTF-8
    # bytes. Preserve those as distinct surrogate escapes instead of replacing
    # them (which would conflate, for example, \xff and \xfe constants).
    return json.loads(result.stdout.decode("utf-8", errors="surrogateescape"))["root"]


def conditional_count(value):
    if isinstance(value, dict):
        return int(value.get("type") == "AstExprIfElse") + sum(conditional_count(v) for v in value.values())
    if isinstance(value, list):
        return sum(conditional_count(v) for v in value)
    return 0


def canonicalize(root, *, statement_style=False):
    bindings, names, types = {}, {}, []

    def normalize(value):
        if isinstance(value, list):
            result = []
            for item in value:
                item = normalize(item)
                if item is not None:
                    result.append(item)
            return result
        if not isinstance(value, dict):
            return value
        kind = value.get("type")
        if kind in TYPE_STATEMENTS or (kind and (kind.startswith("AstType") or kind.startswith("AstGeneric"))):
            types.append(strip_locations(value))
            return None
        if kind in ("AstExprTypeAssertion", "AstExprInstantiate"):
            for key in TYPE_FIELDS:
                if value.get(key) is not None:
                    types.append(strip_locations(value[key]))
            return normalize(value["expr"])
        if kind == "AstLocal":
            # All references serialize the AstLocal's DECLARATION location, not
            # their own use-site location. Shadowing creates a different key.
            key = (value["name"], value["location"])
            index = bindings.setdefault(key, len(bindings))
            names[index] = value["name"]
            if value.get("luauType") is not None:
                types.append(strip_locations(value["luauType"]))
            return {"type": "Binding", "id": index}
        if kind == "AstStatBlock":
            body = []
            for statement in value["body"]:
                if statement_style and statement.get("type") == "AstStatLocal" \
                        and len(statement["vars"]) == len(statement["values"]) == 1 \
                        and statement["values"][0].get("type") == "AstExprIfElse":
                    # A single local initializer and each branch assignment both
                    # truncate to one result. Binding identity preserves outer
                    # references in the initializer; no arity-changing ungrouping.
                    expr = statement["values"][0]
                    binding = normalize(statement["vars"][0])
                    body.append({"type": "AstStatLocal", "vars": [binding], "values": []})
                    branches = {}
                    for arm, key in (("thenbody", "trueExpr"), ("elsebody", "falseExpr")):
                        branches[arm] = {"type": "AstStatBlock", "body": [{"type": "AstStatAssign",
                            "vars": [{"type": "AstExprLocal", "local": binding}],
                            "values": [normalize(expr[key])]}]}
                    body.append({"type": "AstStatIf", "condition": normalize(expr["condition"]), **branches})
                else:
                    item = normalize(statement)
                    if item is not None:
                        body.append(item)
            return {"type": kind, "body": body}
        result = {}
        for key, item in value.items():
            if key in TRIVIA:
                continue
            if key in TYPE_FIELDS:
                if item:
                    types.append(strip_locations(item))
                continue
            # Native/checked attributes affect execution; keep them separately
            # from erased type annotations in the structural representation.
            result[key] = normalize(item)
        return result

    normalized = normalize(root)
    return normalized, names, types


def strip_locations(value):
    if isinstance(value, list):
        return [strip_locations(v) for v in value]
    if isinstance(value, dict):
        return {k: strip_locations(v) for k, v in value.items() if k not in TRIVIA}
    return value


def tokens(value, out=None):
    if out is None:
        out = []
    if isinstance(value, dict):
        if value.get("type") == "Binding":
            out.append(("binding", value["id"]))
        else:
            out.append(("node", value.get("type", "record")))
            for key in sorted(k for k in value if k != "type"):
                out.append(("key", key))
                tokens(value[key], out)
            out.append(("end", "node"))
    elif isinstance(value, list):
        out.append(("list", "begin"))
        for item in value:
            tokens(item, out)
        out.append(("end", "list"))
    else:
        out.append((type(value).__name__, json.dumps(value, sort_keys=True)))
    return out


def compare_ast(source, output, *, token_pair_budget=4_000_000):
    left, left_names, left_types = canonicalize(source)
    right, right_names, right_types = canonicalize(output)
    a, b = tokens(left), tokens(right)
    if len(a) * len(b) > token_pair_budget:
        return {"model": "luau-ast-binding-v1", "status": "unknown", "reason": "alignment token-pair budget",
                "source_bindings": len(left_names), "output_bindings": len(right_names)}
    raw = difflib.SequenceMatcher(None, a, b, autojunk=False).ratio()
    # Align structure without spelling or arbitrary binding numbering, then
    # demand consistent, complete binding-graph correspondence.
    erase = lambda seq: [(kind, "*" if kind == "binding" else val) for kind, val in seq]
    alignment = difflib.SequenceMatcher(None, erase(a), erase(b), autojunk=False)
    pairs = collections.Counter()
    for block in alignment.get_matching_blocks():
        for i, j in zip(range(block.a, block.a + block.size), range(block.b, block.b + block.size)):
            if a[i][0] == b[j][0] == "binding":
                pairs[a[i][1], b[j][1]] += 1
    counts_a = collections.Counter(v for k, v in a if k == "binding")
    counts_b = collections.Counter(v for k, v in b if k == "binding")
    aligned = [{"source_binding": i, "output_binding": j, "source_name": left_names[i],
                "output_name": right_names[j], "exact_name": left_names[i] == right_names[j],
                "occurrences": count}
               for (i, j), count in sorted(pairs.items()) if counts_a[i] == counts_b[j] == count]
    exact = sum(row["exact_name"] for row in aligned)
    style_a = tokens(canonicalize(source, statement_style=True)[0])
    style_b = tokens(canonicalize(output, statement_style=True)[0])

    return {"model": "luau-ast-binding-v1", "status": "measured", "raw_structural_ratio": raw,
            "statement_initializer_normalized_ratio": difflib.SequenceMatcher(None, style_a, style_b, autojunk=False).ratio(),
            "style_normalization": "single-local-if-initializer-v1",
            "source_conditional_expressions": conditional_count(source),
            "output_conditional_expressions": conditional_count(output),
            "source_bindings": len(left_names), "output_bindings": len(right_names),
            "aligned_bindings": len(aligned), "exact_names": exact,
            "alignment_coverage": len(aligned) / len(left_names) if left_names else None,
            "exact_name_precision_on_aligned": exact / len(aligned) if aligned else None,
            "exact_name_recovery_lower_bound": exact / len(left_names) if left_names else None,
            "bindings": aligned,
            "type_syntax_equal": left_types == right_types}
