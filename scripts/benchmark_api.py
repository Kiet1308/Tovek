#!/usr/bin/env python3
"""Pin corpus workloads, then measure CLI, in-memory API and allocations separately.

No cache eviction or OS cache flushing is attempted. Input decode and source
hashes are excluded from API timings. CLI samples include process startup/I/O.
"""
import argparse
import base64
import hashlib
import json
import math
import os
import pathlib
import platform
import shutil
import statistics
import subprocess
import tempfile
import time

from benchmark_v2 import peak_rss_reader
from roadmap_v2 import sha256


NAMED = {
    "write": "ReplicatedStorage/Shared/Network/BufferEncoder/Write.lua",
    "lightning": "ReplicatedStorage/DivergentVFX/LightningCore.lua",
    "promise": "ReplicatedStorage/Shared/ForgeVFXForCutscenes/pkg/Promise.lua",
}
GROUPS = ("all", "small", "large", "write", "lightning", "promise", "ui")


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n",
                    encoding="utf-8", newline="\n")


def decoded_size(saved):
    compact = b"".join(line for line in saved.split(b"\n") if not line.startswith(b"--"))
    for whitespace in (b" ", b"\t", b"\r"):
        compact = compact.replace(whitespace, b"")
    return len(base64.b64decode(compact, validate=True))


def make_manifest(corpus, source_root, key):
    records = []
    # Preserve the baseline tree order in the manifest, including on other hosts.
    for source in sorted(source_root.rglob("*.luau")):
        relative = source.relative_to(source_root).with_suffix(".lua").as_posix()
        saved = (corpus / relative).read_bytes()
        records.append((dict(path=relative, input_sha256=hashlib.sha256(saved).hexdigest(),
                             source_sha256=sha256(source), groups=["all"]), decoded_size(saved)))
    if {p.relative_to(corpus).as_posix() for p in corpus.rglob("*.lua")} != {r[0]["path"] for r in records}:
        raise ValueError("baseline/corpus file sets differ")
    sizes = sorted(size for _, size in records if size)
    if not sizes:
        raise ValueError("empty corpus")
    small, large = sizes[int((len(sizes) - 1) * .5)], sizes[int((len(sizes) - 1) * .9)]
    ui = [(size, row["path"]) for row, size in records
          if "FusionPackage/Components/" in row["path"] and size]
    named = dict(NAMED, ui=max(ui)[1])
    paths = {r[0]["path"] for r in records}
    if not set(named.values()) <= paths:
        raise ValueError("representative input missing")
    for row, size in records:
        if 0 < size <= small:
            row["groups"].append("small")
        if size >= large:
            row["groups"].append("large")
        row["groups"].extend(group for group, path in named.items() if path == row["path"])
    manifest = dict(schema_version=1, decode_key=key, scripts=[r[0] for r in records])
    workloads = dict(schema_version=1, threshold_bytes=dict(small_at_most=small, large_at_least=large),
                     counts={g: sum(g in row["groups"] for row, _ in records) for g in GROUPS}, named=named,
                     selection="Fixed before timing; small/large use lower-sample p50/p90 nonempty decoded sizes "
                               "at index floor((n-1)*q); "
                               "UI is the largest decoded input within FusionPackage/Components.")
    return manifest, workloads


def run_process(command, log, timeout, env):
    peak = None
    started = time.perf_counter()
    with log.open("wb") as stream:
        process = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT, env=env)
        rss = peak_rss_reader(process)
        while process.poll() is None:
            value = rss()
            if value is not None:
                peak = max(peak or 0, value)
            if time.perf_counter() - started > timeout:
                process.kill()
                process.wait()
                raise TimeoutError(f"measurement timeout; see {log}")
            time.sleep(.005)
        value = rss()
        if value is not None:
            peak = max(peak or 0, value)
        code = process.wait()
    elapsed = time.perf_counter() - started
    if code:
        raise RuntimeError(f"measurement failed with code {code}; see {log}")
    return dict(command=command, process_wall_seconds=elapsed, process_peak_rss_bytes=peak)


def check_sources(root, scripts):
    digest = hashlib.sha256()
    expected = set()
    for row in scripts:
        relative = pathlib.PurePosixPath(row["path"]).with_suffix(".luau").as_posix()
        expected.add(relative)
        data = (root / relative).read_bytes()
        if hashlib.sha256(data).hexdigest() != row["source_sha256"]:
            raise ValueError(f"source differs from locked baseline: {relative}")
        for part in (relative.encode("utf-8"), data):
            digest.update(len(part).to_bytes(8, "little"))
            digest.update(part)
    if expected != {p.relative_to(root).as_posix() for p in root.rglob("*.luau")}:
        raise ValueError("unexpected/missing output files")
    return digest.hexdigest()


def timing_summary(values):
    ordered = sorted(values)
    return dict(samples=len(values), median_seconds=statistics.median(values),
                p95_nearest_rank_seconds=ordered[math.ceil(.95 * len(values)) - 1],
                min_seconds=min(values), max_seconds=max(values))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="mode", required=True)
    pin = sub.add_parser("pin", help="generate workload manifest before any timing")
    pin.add_argument("--corpus", type=pathlib.Path, required=True)
    pin.add_argument("--source-root", type=pathlib.Path, required=True)
    pin.add_argument("--key", type=int, default=203)
    pin.add_argument("--manifest", type=pathlib.Path, required=True)
    pin.add_argument("--workloads", type=pathlib.Path, required=True)
    run = sub.add_parser("run")
    run.add_argument("--corpus", type=pathlib.Path, required=True)
    run.add_argument("--manifest", type=pathlib.Path, required=True)
    run.add_argument("--api", type=pathlib.Path, required=True)
    run.add_argument("--counts", type=pathlib.Path, required=True)
    run.add_argument("--cli", type=pathlib.Path, required=True)
    run.add_argument("--groups", nargs="+", choices=GROUPS, default=list(GROUPS))
    run.add_argument("--threads", type=int, nargs="+", default=[1, 16])
    run.add_argument("--rounds", type=int, default=7)
    run.add_argument("--timeout", type=float, default=1800)
    run.add_argument("--keep", type=pathlib.Path, required=True)
    run.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if args.mode == "pin":
        manifest, workloads = make_manifest(args.corpus.resolve(), args.source_root.resolve(), args.key)
        write_json(args.manifest, manifest)
        write_json(args.workloads, workloads)
        print(json.dumps(workloads))
        return 0
    if not 3 <= args.rounds <= 50 or any(not 1 <= t <= 64 for t in args.threads):
        parser.error("use 3..50 rounds and 1..64 threads")
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    if manifest["schema_version"] != 1:
        raise ValueError("unsupported manifest")
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="api-", dir=args.keep)).resolve()
    corpus = args.corpus.resolve(strict=True)
    # The executable validates again; refuse drift before staging CLI input too.
    for row in manifest["scripts"]:
        relative = pathlib.PurePosixPath(row["path"])
        if relative.is_absolute() or any(p in (".", "..") for p in relative.parts) or "\\" in row["path"] or ":" in row["path"]:
            raise ValueError("invalid manifest path")
        path = (corpus / row["path"]).resolve(strict=True)
        if not path.is_relative_to(corpus) or sha256(path) != row["input_sha256"]:
            raise ValueError("input escapes corpus or hash differs")
    binaries = {k: getattr(args, k).resolve(strict=True) for k in ("api", "counts", "cli")}
    env = {k: v for k, v in os.environ.items()
           if not k.upper().startswith("MEDAL_") and k.upper() != "DEINLINE_ANCHOR_TRACE"}
    groups, inputs, hashes = {}, {}, {}
    for group in args.groups:
        rows = [r for r in manifest["scripts"] if group in r["groups"]]
        if not rows:
            raise ValueError(f"empty group: {group}")
        groups[group] = rows
        if group == "all" and len(rows) == len(manifest["scripts"]):
            inputs[group] = corpus
        else:
            inputs[group] = work / "input" / group
            for row in rows:
                dest = inputs[group] / row["path"]
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(corpus / row["path"], dest)
    report = dict(schema_version=1, manifest_sha256=sha256(args.manifest),
                  manifest=manifest, tools={k: dict(path=str(p), sha256=sha256(p)) for k, p in binaries.items()},
                  system=dict(platform=platform.platform(), cpu=os.environ.get("PROCESSOR_IDENTIFIER"),
                              logical_processors=os.cpu_count()), option_bits=8,
                  api_runs=[], allocation_runs=[], cli_rows=[], summary=[],
                  contract="API: first call plus repeated calls in one process, explicit Rayon pool; no OS cold-cache claim. "
                           "CLI: warm-up at each thread count, then interleaved group/thread rounds; reused outputs. "
                           "Allocation build runs separately at one thread; its timings are not speed samples. "
                           "RSS is Windows process-lifetime PeakWorkingSetSize sampled at 5ms, includes preload/hashing. "
                           "API retains all result strings until the call returns; CLI writes/drops each output. "
                           "Finite samples; p95 equals max at seven rounds. No simultaneous heavy workload.")

    def checkpoint():
        write_json(args.report, report)

    for group in args.groups:
        for threads in args.threads:
            result_path = work / f"api-{group}-{threads}.json"
            command = [str(binaries["api"]), "--manifest", str(args.manifest.resolve()), "--input-root", str(corpus),
                       "--report", str(result_path), "--group", group, "--threads", str(threads), "--rounds", str(args.rounds)]
            process = run_process(command, result_path.with_suffix(".log"), args.timeout, env)
            result = json.loads(result_path.read_text(encoding="utf-8"))
            if (result["allocation_instrumented"] or result["manifest_sha256"] != report["manifest_sha256"]
                    or result["executable_sha256"] != report["tools"]["api"]["sha256"]):
                raise ValueError("wrong timing binary/manifest")
            hashes.setdefault(group, result["rows"][0]["output_tree_hash"])
            if any(r["output_tree_hash"] != hashes[group] for r in result["rows"]):
                raise ValueError("API output drift")
            result.update(process)
            report["api_runs"].append(result)
            summary = dict(kind="api", group=group, threads=threads,
                           **timing_summary([r["seconds"] for r in result["rows"] if not r["first_call"]]),
                           first_call_seconds=result["rows"][0]["seconds"], process_peak_rss_bytes=process["process_peak_rss_bytes"])
            report["summary"].append(summary)
            print(json.dumps(summary), flush=True)
            checkpoint()
    for index in range(-1, args.rounds):
        pairs = [(g, t) for g in args.groups for t in args.threads]
        if index % 2:
            pairs.reverse()
        for group, threads in pairs:
            output = work / "output" / group
            command = [str(binaries["cli"]), "decompile-folder", str(inputs[group]), str(output),
                       "--key", str(manifest["decode_key"]), "--threads", str(threads), "--strict-no-synthetic-control"]
            row = run_process(command, work / f"cli-{group}-{threads}-{index}.log", args.timeout, env)
            row.update(group=group, threads=threads, round=index, warmup=index == -1,
                       output_tree_hash=check_sources(output, groups[group]))
            if row["output_tree_hash"] != hashes[group]:
                raise ValueError("CLI/API output differs")
            report["cli_rows"].append(row)
            print(f"CLI {group} threads={threads} round={index} seconds={row['process_wall_seconds']:.6f}", flush=True)
        checkpoint()
    for group in args.groups:
        for threads in args.threads:
            rows = [r for r in report["cli_rows"] if r["group"] == group and r["threads"] == threads and not r["warmup"]]
            peaks = [r["process_peak_rss_bytes"] for r in rows if r["process_peak_rss_bytes"] is not None]
            report["summary"].append(dict(kind="cli", group=group, threads=threads,
                                          **timing_summary([r["process_wall_seconds"] for r in rows]),
                                          median_process_peak_rss_bytes=statistics.median(peaks) if peaks else None))
    checkpoint()
    for group in args.groups:
        result_path = work / f"counts-{group}.json"
        command = [str(binaries["counts"]), "--manifest", str(args.manifest.resolve()), "--input-root", str(corpus),
                   "--report", str(result_path), "--group", group, "--threads", "1", "--rounds", "1"]
        process = run_process(command, result_path.with_suffix(".log"), args.timeout, env)
        result = json.loads(result_path.read_text(encoding="utf-8"))
        if (not result["allocation_instrumented"] or result["manifest_sha256"] != report["manifest_sha256"]
                or result["executable_sha256"] != report["tools"]["counts"]["sha256"]):
            raise ValueError("wrong allocation binary/manifest")
        if any(r["output_tree_hash"] != hashes[group] or r["allocations"]["failed_allocations"] for r in result["rows"]):
            raise ValueError("instrumented output drift/allocation failure")
        result.update(process)
        report["allocation_runs"].append(result)
        print(f"ALLOC {group}: {json.dumps(result['rows'][1]['allocations'])}", flush=True)
        checkpoint()
    if any(sha256(path) != report["tools"][name]["sha256"] for name, path in binaries.items()):
        raise ValueError("a measurement executable changed during the run")
    report["complete"] = True
    report["output_hashes"] = hashes
    checkpoint()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
