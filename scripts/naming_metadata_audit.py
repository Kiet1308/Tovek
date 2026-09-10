#!/usr/bin/env python3
"""Audit inferred-name coverage and preservation of recorded binding mappings."""
import argparse
import collections
import json
import pathlib

from roadmap_v2 import sha256


def inventory(root):
    result = {}
    for path in sorted((root / ".tovek-analysis/scripts").glob("*.json")):
        data = json.loads(path.read_text(encoding="utf-8"))
        key = data["script_path"]
        if key in result:
            raise ValueError(f"duplicate script identity: {key}")
        result[key] = path, data
    return result


def recorded_contract(record):
    # Output spans and a method's local receiver spelling can change. The
    # recorded identifier, slot/origin, mapping identity and exactness cannot.
    bindings = [{key: value for key, value in binding.items() if key not in ("span", "emitted_name")}
                for binding in record["emitted_bindings"]]
    return {key: value for key, value in record.items() if key != "emitted_bindings"} | {"emitted_bindings": bindings}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("before", "after", "report"):
        parser.add_argument(f"--{name}", required=True, type=pathlib.Path)
    args = parser.parse_args()
    before, after = inventory(args.before), inventory(args.after)
    rows, renamed = [], []
    statuses, reasons, totals = (collections.Counter() for _ in range(3))
    for key in sorted(before.keys() | after.keys()):
        row = {"script_path": key, "status": "passed"}
        if key not in before or key not in after:
            rows.append(row | {"status": "missing_metadata"})
            continue
        first, a = before[key]
        second, b = after[key]
        row.update(before_sha256=sha256(first), after_sha256=sha256(second))
        left, right = a["source_recovery"], b["source_recovery"]
        if a["bytecode_sha256"] != b["bytecode_sha256"] or \
                [recorded_contract(r) for r in left["records"]] != [recorded_contract(r) for r in right["records"]]:
            row["status"] = "recorded_mapping_changed"
        by_id = lambda report: {r["binding_id"]: r for r in report["bindings"]}
        lbindings, rbindings = by_id(left), by_id(right)
        protected = {bid: value for bid, value in lbindings.items() if value["origins"]}
        if any(rbindings.get(bid) != value for bid, value in protected.items()):
            row["status"] = "protected_binding_changed"
        report = b["name_inference"]
        row.update(renamed=report["renamed"], conflicts=report["conflicts"],
                   budget_exhausted=report["budget_exhausted"], recorded_names=right["recorded_names"],
                   mapped_names=right["mapped_names"], protected_bindings=len(protected))
        for field in ("visited_nodes", "bindings", "renamed", "conflicts", "budget_exhausted", "unresolved_calls", "refused_edges"):
            totals[field] += report[field]
        for binding in report["rows"]:
            statuses[binding["status"]] += 1
            if binding["status"] == "renamed":
                priority = max(c["priority"] for c in binding["candidates"])
                winners = [c for c in binding["candidates"] if c["priority"] == priority]
                reasons.update({c["reason"] for c in winners})
                renamed.append({"script_path": key, **binding})
        rows.append(row)
    report = {"schema_version": 1, "summary": {"scripts": len(rows),
              "status": dict(collections.Counter(r["status"] for r in rows)), **totals,
              "binding_status": dict(statuses), "winning_rules": dict(reasons)},
              "rows": rows, "renamed_bindings": renamed,
              "limitations": "This preserves recorded mappings and reports inferred evidence; it is not an exact-name quality score. Lexical correctness is checked separately with the pinned parser's binding graph."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(report["summary"], indent=2))
    return int(not rows or any(r["status"] != "passed" for r in rows))


if __name__ == "__main__":
    raise SystemExit(main())
