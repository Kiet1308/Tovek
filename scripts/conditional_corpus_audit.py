#!/usr/bin/env python3
"""Check source/recorded-name/capture-proof identity and conditional-pass coverage.

Stream manifest-selected sidecars rather than load every historical sidecar.
This gate is intended for corpora where the final IR has no conditional to
lower, or explicitly refuses it. Changed source requires separate review and
cannot silently become a replacement baseline here.
"""
import argparse
import collections
import json
import pathlib

from benchmark_v2 import tree_hash
from naming_metadata_audit import recorded_contract
from provenance_audit import manifest, sidecar
from roadmap_v2 import sha256


def audit(before, after):
    _, left = manifest(before)
    _, right = manifest(after)
    if left.keys() != right.keys():
        raise ValueError("manifest script identities differ")
    rows = []
    for key in sorted(left):
        a, b = sidecar(before, left[key]), sidecar(after, right[key])
        original, current = a["source_recovery"], b["source_recovery"]
        bindings = {r["binding_id"]: r for r in current["bindings"]}
        recorded = a["bytecode_sha256"] == b["bytecode_sha256"] and \
            [recorded_contract(r) for r in original["records"]] == [recorded_contract(r) for r in current["records"]] and \
            all(bindings.get(r["binding_id"]) == r for r in original["bindings"] if r["origins"])
        captures = a["capture_effects"] == b["capture_effects"]
        a_source = sha256(before / left[key]["source_path"])
        b_source = sha256(after / right[key]["source_path"])
        report = b["conditional_lowering"]
        if report["model"] != "luau-v9-scalar-select-statements-v1":
            raise ValueError("unknown conditional model")
        for field in ("input_selects", "lowered_selects", "introduced_locals"):
            if type(report[field]) is not int or report[field] < 0:
                raise ValueError("invalid conditional count")
        if report["lowered_selects"] > report["input_selects"]:
            raise ValueError("lowering count exceeds inventory")
        rows.append(dict(script_path=key, status="passed" if recorded and captures and a_source == b_source else "changed",
                         source_sha256=b_source, source_unchanged=a_source == b_source,
                         recorded_contracts_equal=recorded, capture_certificates_equal=captures,
                         conditional_lowering=report))
    old_tree, old_count = tree_hash(before, "*.luau")
    new_tree, new_count = tree_hash(after, "*.luau")
    return dict(schema_version=1, before=str(before), after=str(after), rows=rows,
                before_tree=old_tree, after_tree=new_tree, source_files=new_count,
                complete_source_tree_equal=old_tree == new_tree and old_count == new_count,
                summary=dict(collections.Counter(r["status"] for r in rows)),
                lowering_counts={key: sum(r["conditional_lowering"][key] for r in rows)
                                 for key in ("input_selects", "lowered_selects", "introduced_locals", "budget_exhausted")},
                contract="All source bytes, recorded contracts and prior independently checked capture certificates must remain equal. "
                         "Full source-tree comparison includes empty inputs that have no analysis sidecar. "
                         "A budget exhaustion makes the conditional inventory unknown, not zero coverage.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("before", "after", "report"):
        parser.add_argument("--" + name, type=pathlib.Path, required=True)
    args = parser.parse_args()
    report = audit(args.before.resolve(strict=True), args.after.resolve(strict=True))
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({key: report[key] for key in ("summary", "lowering_counts", "complete_source_tree_equal")}))
    return int(not report["complete_source_tree_equal"] or any(r["status"] != "passed" for r in report["rows"]))


if __name__ == "__main__":
    raise SystemExit(main())
