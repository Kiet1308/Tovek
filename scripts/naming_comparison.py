#!/usr/bin/env python3
"""Compare binding-aligned name recovery on two locked public-source reports.

Only complete, unchanged source/output binding alignments contribute to name
precision. Unknown alignments stay in the denominator of configuration coverage.
This report measures spelling, not semantic role quality or rename correctness;
the independent alpha-equivalence audit is required for the latter.
"""
import argparse
import collections
import json
import pathlib

from roadmap_v2 import sha256


def compare(first, second):
    row = {key: second[key] for key in ("repo", "file", "opt", "split", "source_sha256")}
    row["groups"] = second.get("groups", [])
    a, b = first["source_fidelity"], second["source_fidelity"]
    if first["source_sha256"] != second["source_sha256"]:
        return row | {"status": "source_changed"}
    if a.get("status") != "measured" or b.get("status") != "measured":
        return row | {"status": "unknown_alignment", "before_status": a.get("status"), "after_status": b.get("status")}
    keyed = lambda report: {(r["source_binding"], r["output_binding"], r["source_name"], r["occurrences"]): r
                            for r in report["bindings"]}
    left, right = keyed(a), keyed(b)
    if left.keys() != right.keys() or a["source_bindings"] != b["source_bindings"]:
        return row | {"status": "alignment_changed"}
    changes = [{"source_binding": key[0], "output_binding": key[1], "source_name": key[2],
                "before": left[key]["output_name"], "after": right[key]["output_name"],
                "before_exact": left[key]["exact_name"], "after_exact": right[key]["exact_name"]}
               for key in left if left[key]["output_name"] != right[key]["output_name"]]
    return row | {"status": "measured", "source_bindings": a["source_bindings"], "aligned_bindings": len(left),
                  "before_exact": a["exact_names"], "after_exact": b["exact_names"], "changes": changes}


def summarize(rows):
    measured = [r for r in rows if r["status"] == "measured"]
    changes = [c for r in measured for c in r["changes"]]
    aligned = sum(r["aligned_bindings"] for r in measured)
    source = sum(r["source_bindings"] for r in measured)
    before, after = (sum(r[f"{side}_exact"] for r in measured) for side in ("before", "after"))
    return {"configurations": len(rows), "status": dict(collections.Counter(r["status"] for r in rows)),
            "source_bindings_in_measured_configurations": source, "aligned_bindings": aligned,
            "before_exact_names": before, "after_exact_names": after,
            "before_exact_precision_on_aligned": before / aligned if aligned else None,
            "after_exact_precision_on_aligned": after / aligned if aligned else None,
            "changed_aligned_names": len(changes), "changes_to_exact": sum(c["after_exact"] for c in changes),
            "changes_from_exact": sum(c["before_exact"] for c in changes),
            "changed_name_exact_precision": sum(c["after_exact"] for c in changes) / len(changes) if changes else None}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("before", "after", "report"):
        parser.add_argument(f"--{name}", required=True, type=pathlib.Path)
    args = parser.parse_args()
    before, after = (json.loads(path.read_text(encoding="utf-8")) for path in (args.before, args.after))
    if before["manifest_sha256"] != after["manifest_sha256"]:
        parser.error("different source manifests")
    keyed = lambda report: {(r["repo"], r["file"], r["opt"]): r for r in report["rows"]}
    left, right = keyed(before), keyed(after)
    if left.keys() != right.keys():
        parser.error("different configuration sets")
    rows = [compare(left[key], right[key]) for key in sorted(left)]
    report = {"schema_version": 1, "before_sha256": sha256(args.before), "after_sha256": sha256(args.after),
              "manifest_sha256": before["manifest_sha256"], "summary": summarize(rows),
              "splits": {split: summarize([r for r in rows if r["split"] == split]) for split in sorted({r["split"] for r in rows})},
              "groups": {group: summarize([r for r in rows if group in r["groups"]]) for group in sorted({g for r in rows for g in r["groups"]})},
              "rows": rows,
              "limitations": "O0/O1/O2 are separate configurations, not independent sources. Exact spelling is not human-assessed role quality. Unaligned bindings and unknown configurations have no name-quality score. Requires a separate full alpha-equivalence audit."}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps(report["splits"], indent=2))
    return int(any(r["status"] not in ("measured", "unknown_alignment") for r in rows))


if __name__ == "__main__":
    raise SystemExit(main())
