#!/usr/bin/env python3
"""Recreate the frozen lexical queue, then audit prototype/binding occurrences.

Lexical absence selects the original research queue only; recovery is decided
from compiler-origin binding metadata and formatter occurrence identities.
"""
import argparse
import collections
import concurrent.futures
import json
import pathlib
import re

from roadmap_v2 import sha256
from source_fidelity import parse_ast


def read(path):
    return json.loads(path.read_text(encoding="utf-8"))


def occurrences(sidecar, prototype):
    return [o for f in sidecar["functions"] if f["proto_id"] == prototype for o in f["occurrences"]]


def function_contexts(tree):
    contexts = []

    def walk(value, parent=None, field=None):
        if isinstance(value, dict):
            if value.get("type") == "AstExprFunction":
                kind = parent.get("type") if parent else None
                key = parent.get("key", {}) if kind == "AstExprTableItem" else {}
                field_name = key.get("value") if key.get("type") == "AstExprConstantString" else None
                contexts.append((value["location"], kind, field, field_name))
            for key, item in value.items():
                walk(item, value, key)
        elif isinstance(value, list):
            for item in value:
                walk(item, parent, field)
    walk(tree)
    return contexts


def context_of(occurrence, contexts):
    end = occurrence["span"]["end"]
    target = f"{end['line_one_based'] - 1},{end['column_one_based'] - 1}"
    matches = [(parent, field, name) for location, parent, field, name in contexts if location.split(" - ")[-1] == target]
    if len(matches) != 1:
        return "unknown_ast_context"
    parent, field, name = matches[0]
    if name is not None:
        return f"table_field:{name}"
    if parent == "AstStatReturn":
        return "anonymous_return"
    if parent == "AstExprCall" and field == "args":
        return "callback_argument"
    return f"{parent}.{field}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=pathlib.Path, required=True, help="frozen binary folder output with analysis")
    parser.add_argument("--current", type=pathlib.Path, required=True, help="new folder output with source_recovery analysis")
    parser.add_argument("--ast", type=pathlib.Path, required=True)
    parser.add_argument("--expected-queue", type=int, default=457)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()
    old_manifest = read(args.baseline / ".tovek-analysis/manifest.json")
    new_manifest = read(args.current / ".tovek-analysis/manifest.json")
    new_scripts = {s["script_path"]: s for s in new_manifest["scripts"]}
    rows, jobs, totals = [], [], collections.Counter()
    for script in old_manifest["scripts"]:
        if not script.get("sidecar_path"):
            continue
        old = read(args.baseline / script["sidecar_path"])
        new_script = new_scripts[script["script_path"]]
        new = read(args.current / new_script["sidecar_path"])
        if script["bytecode_sha256"] != new_script["bytecode_sha256"]:
            raise ValueError(f"bytecode changed: {script['script_path']}")
        for root, item in ((args.baseline, script), (args.current, new_script)):
            if sha256(root / item["source_path"]) != item["source_sha256"]:
                raise ValueError(f"stale source metadata: {item['source_path']}")
        records = {r["origin"]["prototype"]: r for r in new["source_recovery"]["records"]
                   if r["origin"]["kind"] == "function_name"}
        totals.update(r["status"] for r in records.values())
        source = args.baseline / script["source_path"]
        text = source.read_text(encoding="utf-8")
        # Exactly the research queue's lexical selector, not a name metric.
        masked = re.sub(r'--[^\n]*|"(?:\\.|[^"\\])*"|\x27(?:\\.|[^\x27\\])*\x27',
                        lambda m: " " * len(m[0]), text)
        identifiers = set(re.findall(r"\b[A-Za-z_]\w*\b", masked))
        selected = []
        for proto in old["prototypes"]:
            name = proto.get("debug_name")
            if not name or not re.fullmatch(r"[A-Za-z_]\w*", name) or name in identifiers:
                continue
            pid = proto["proto_id"]
            record = records[pid]
            emitted = record["emitted_bindings"]
            exact = any(b.get("exact_spelling") for b in emitted)
            category = "mapped_exact_name" if exact else "mapped_renamed_or_conflicting" if emitted else record["status"]
            row = {"file": script["script_path"], "prototype": pid, "recorded_name": name,
                   "bytecode_sha256": script["bytecode_sha256"],
                   "baseline_source_sha256": script["source_sha256"],
                   "current_source_sha256": new_script["source_sha256"],
                   "baseline_occurrences": occurrences(old, pid),
                   "current_occurrences": occurrences(new, pid),
                   "recovery": record, "category": category}
            selected.append(row)
            rows.append(row)
        if selected:
            jobs.append((source, selected))
    if len(rows) != args.expected_queue:
        raise ValueError(f"frozen queue changed: expected {args.expected_queue}, got {len(rows)}")

    def classify(job):
        source, subset = job
        contexts = function_contexts(parse_ast(args.ast.resolve(), source, 30))
        for row in subset:
            row["baseline_contexts"] = [context_of(o, contexts) if o["syntax_kind"] == "anonymous"
                                        else o["syntax_kind"] for o in row["baseline_occurrences"]]
            if not row["baseline_contexts"]:
                row["baseline_contexts"] = ["prototype_not_emitted"]
            if f"table_field:{row['recorded_name']}" in row["baseline_contexts"]:
                row["baseline_name_visible_as_field_key"] = True
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(classify, jobs))
    report = {"schema_version": 1, "baseline_tool_sha256": old_manifest.get("tool_sha256"),
              "current_tool_sha256": new_manifest.get("tool_sha256"),
              "ast_sha256": sha256(args.ast), "all_recorded_function_names": dict(totals),
              "queue_size": len(rows), "queue_categories": dict(collections.Counter(r["category"] for r in rows)),
              "baseline_contexts": dict(collections.Counter(c for r in rows for c in r["baseline_contexts"])),
              "rows": rows,
              "limitations": "Original lexical queue can miss losses masked by same-spelled identifiers. Binding metadata determines recovery; AST context is descriptive. Missing mappings remain unrecovered."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({k: v for k, v in report.items() if k != "rows"}, indent=2))


if __name__ == "__main__":
    main()
