#!/usr/bin/env python3
"""Check every pinned residual-control fixture, with exact typed-refusal evidence."""
import argparse
import collections
import hashlib
import json
from pathlib import Path
import re
import subprocess

from bytecode_roundtrip import read_saved_bytecode
from expected_refusals import batch_refusals_match, refusal_outcome_matches


FIXTURE_ROOT = Path(__file__).resolve().parent.parent / "docs/failure_fixtures/residual_control_flow"
MANIFEST_PATH = FIXTURE_ROOT.with_name("residual_control_flow_manifest.json")
CONTROL_MARKER = re.compile(r"(^|[^\w])goto\s|controlFlowState|GenericFor(?:Init|Next)")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def fixture_manifest():
    """Bind both the saved wrapper and decoded bytecode to the complete reviewed inventory."""
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != 1 or type(manifest.get("key")) is not int or manifest["key"] != 203:
        raise ValueError("invalid residual fixture manifest")
    entries = {entry["file"]: entry for entry in manifest["files"]}
    inputs = {path.relative_to(FIXTURE_ROOT).as_posix(): path for path in FIXTURE_ROOT.rglob("*.lua")}
    if not entries or len(entries) != len(manifest["files"]) or entries.keys() != inputs.keys():
        raise ValueError("residual fixture inventory mismatch")
    for name, source in inputs.items():
        entry, raw = entries[name], read_saved_bytecode(source)
        if (source.is_symlink() or source.resolve() != source or not raw
                or sha256(source.read_bytes()) != entry["fixture_sha256"]
                or sha256(raw) != entry["bytecode_sha256"]
                or raw[0] != entry["bytecode_version"]):
            raise ValueError(f"residual fixture hash mismatch: {name}")
    return FIXTURE_ROOT, entries


def fixture_refusal_evidence(entry, raw_bytecode, key, mode, outcome):
    refusal = entry.get("expected_refusal", {})
    if (type(key) is not int or key != 203 or mode not in ("default", "strict")
            or mode not in refusal.get("modes", [])
            or sha256(raw_bytecode) != entry["bytecode_sha256"]):
        return None
    reason = refusal["reason"]
    diagnostic = f"prototype {refusal['prototype']}, pc {refusal['pc']}: {reason}"
    if (not refusal_outcome_matches(reason, outcome)
            or outcome["stderr"].replace("\r\n", "\n").removesuffix("\n") != diagnostic):
        return None
    return dict(fixture_sha256=entry["fixture_sha256"], bytecode_sha256=entry["bytecode_sha256"],
                key=key, mode=mode, reason=reason, prototype=refusal["prototype"], pc=refusal["pc"], **outcome)


def reviewed_refusals(report_path, output_root):
    """Recheck a complete run before downstream gates exclude any missing source."""
    root, entries = fixture_manifest()
    report = json.loads(Path(report_path).read_text(encoding="utf-8"))
    output_root = Path(output_root).resolve()
    mode = report.get("mode")
    if (report.get("schema_version") != 1 or report.get("kind") != "residual_control"
            or mode not in ("default", "strict") or type(report.get("key")) is not int or report["key"] != 203
            or report["fixtures_path"] != str(root) or report["output_root"] != str(output_root)
            or report["manifest_path"] != str(MANIFEST_PATH)
            or report["manifest_sha256"] != sha256(MANIFEST_PATH.read_bytes())
            or report["lifter_args"] != mode_arguments(mode)):
        raise ValueError("residual report context mismatch")
    rows = {row["file"]: row for row in report["files"]}
    if len(rows) != len(report["files"]) or rows.keys() != entries.keys():
        raise ValueError("residual report inventory mismatch")
    refused, failures, emitted = {}, {}, set()
    for name, entry in entries.items():
        row = rows[name]
        relative = Path(name).with_suffix(".luau").as_posix()
        output = output_root / relative
        if (row["fixture_sha256"] != entry["fixture_sha256"]
                or row["bytecode_sha256"] != entry["bytecode_sha256"]):
            raise ValueError("residual report input hash mismatch")
        if row["status"] == "emitted":
            if output.is_symlink() or output.resolve() != output or not output.is_file():
                raise ValueError("invalid residual output artifact")
            source = output.read_bytes()
            if (not source or row.get("refusal") or sha256(source) != row["output_sha256"]
                    or len(source) != row["output_bytes"] or CONTROL_MARKER.search(source.decode("utf-8"))):
                raise ValueError("residual output hash or control marker mismatch")
            emitted.add(relative)
        elif row["status"] == "expected_refusal":
            evidence = row["refusal"]
            checked = fixture_refusal_evidence(entry, read_saved_bytecode(root / name), report["key"], mode,
                                               {key: evidence[key] for key in ("exit_code", "stdout", "stderr")})
            if (checked is None or checked != evidence or output.exists() or output.is_symlink()
                    or row.get("output") or row.get("output_sha256") or row.get("output_bytes", 0)):
                raise ValueError("residual refusal evidence or artifact mismatch")
            refused[relative] = evidence
            failures[name] = evidence["stderr"]
        else:
            raise ValueError("unreviewed residual failure")
    if {path.relative_to(output_root).as_posix() for path in output_root.rglob("*.luau")} != emitted:
        raise ValueError("residual output inventory mismatch")
    if (report["summary"] != dict(collections.Counter(row["status"] for row in rows.values()))
            or not batch_refusals_match(report["batch"], failures, entries, str(output_root))):
        raise ValueError("residual batch diagnostics or counts mismatch")
    return refused


def mode_arguments(mode):
    return ["--strict-no-synthetic-control"] if mode == "strict" else []


def outcome(process):
    return dict(exit_code=process.returncode, stdout=process.stdout.decode("utf-8"),
                stderr=process.stderr.decode("utf-8"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lifter", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    args = parser.parse_args()
    try:
        root, entries = fixture_manifest()
        work, lifter = args.work.resolve(), args.lifter.resolve()
        work.mkdir(parents=True, exist_ok=True)
        for mode in ("default", "strict"):
            output = work / f"residual-control-{mode}"
            batch = subprocess.run([str(lifter), "decompile-folder", str(root), str(output), "--key", "203",
                                    "--threads", "1", "--emit-upvalue-analysis", "--verbose", *mode_arguments(mode)],
                                   capture_output=True, timeout=120)
            (work / f"residual-control-{mode}.log").write_bytes(batch.stdout + batch.stderr)
            rows = []
            for name, entry in entries.items():
                target = output / Path(name).with_suffix(".luau")
                row = dict(file=name, fixture_sha256=entry["fixture_sha256"],
                           bytecode_sha256=entry["bytecode_sha256"], status="failed")
                if target.is_file() and not target.is_symlink():
                    source = target.read_bytes()
                    row.update(status="emitted", output_sha256=sha256(source), output_bytes=len(source))
                elif not target.exists() and not target.is_symlink():
                    raw = read_saved_bytecode(root / name)
                    binary = work / "residual-inputs" / Path(name).with_suffix(".luaubc")
                    binary.parent.mkdir(parents=True, exist_ok=True)
                    binary.write_bytes(raw)
                    single = subprocess.run([str(lifter), str(binary), "-e", *mode_arguments(mode)],
                                            capture_output=True, timeout=120)
                    result = outcome(single)
                    evidence = fixture_refusal_evidence(entry, raw, 203, mode, result)
                    if evidence:
                        row.update(status="expected_refusal", refusal=evidence)
                    else:
                        row["decompile"] = result
                rows.append(row)
            report = dict(schema_version=1, kind="residual_control", fixtures_path=str(root),
                          output_root=str(output), manifest_path=str(MANIFEST_PATH),
                          manifest_sha256=sha256(MANIFEST_PATH.read_bytes()), mode=mode, key=203,
                          lifter_args=mode_arguments(mode), lifter=dict(path=str(lifter), sha256=sha256(lifter.read_bytes())),
                          batch=outcome(batch), files=rows,
                          summary=dict(collections.Counter(row["status"] for row in rows)))
            path = work / f"residual-control-{mode}.json"
            path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
            reviewed_refusals(path, output)
            print(f"residual {mode}: {json.dumps(report['summary'], sort_keys=True)}")
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"residual control: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
