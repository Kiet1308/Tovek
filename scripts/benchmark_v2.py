#!/usr/bin/env python3
"""Interleaved CLI benchmarks with tool/input hashes and output determinism.

Measures warm filesystem cache and repeated output directories. Hashing and
validation are outside the timed interval. RSS is sampled Windows peak working
set where supported; allocations and a true cold-cache run are not claimed.
"""
import argparse
import collections
import ctypes
import hashlib
import json
import math
import os
import pathlib
import platform
import statistics
import subprocess
import tempfile
import time

from roadmap_v2 import sha256


def tree_hash(root, pattern):
    digest, count = hashlib.sha256(), 0
    for path in sorted(root.rglob(pattern)):
        relative = path.relative_to(root).as_posix().encode("utf-8")
        data = path.read_bytes()
        for value in (relative, data):
            digest.update(len(value).to_bytes(8, "little"))
            digest.update(value)
        count += 1
    return digest.hexdigest(), count


def peak_rss_reader(process):
    if os.name != "nt":
        return lambda: None
    from ctypes import wintypes
    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
            (name, ctypes.c_size_t) for name in (
                "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage",
                "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage")]
    query = ctypes.WinDLL("psapi").GetProcessMemoryInfo
    query.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    query.restype = wintypes.BOOL
    def read():
        counters = Counters()
        counters.cb = ctypes.sizeof(counters)
        if query(int(process._handle), ctypes.byref(counters), counters.cb):
            return counters.PeakWorkingSetSize
        return None
    return read


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lifter", action="append", required=True, metavar="LABEL=EXE")
    parser.add_argument("--analysis", action="append", default=[], metavar="LABEL=upvalues|provenance",
                        help="opt in a labelled binary to metadata generation; source hashing still excludes metadata")
    parser.add_argument("--lifter-arg", action="append", default=[], metavar="LABEL=ARG")
    parser.add_argument("--corpus", type=pathlib.Path, required=True)
    parser.add_argument("--key", type=int, default=203)
    parser.add_argument("--threads", type=int, nargs="+", default=[1, 16])
    parser.add_argument("--rounds", type=int, default=7)
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument("--keep", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()
    if args.rounds < 3 or any(t < 1 for t in args.threads):
        parser.error("use at least three rounds and positive thread counts")
    binaries = {}
    for spec in args.lifter:
        label, separator, path = spec.partition("=")
        if not separator or not label or any(c in label for c in "/\\:") or label in binaries:
            parser.error("--lifter needs unique simple labels")
        binaries[label] = pathlib.Path(path).resolve(strict=True)
    lifter_args = {label: [] for label in binaries}
    for spec in args.lifter_arg:
        label, separator, argument = spec.partition('=')
        if not separator or label not in binaries or not argument:
            parser.error('--lifter-arg needs a known label and nonempty argument')
        lifter_args[label].append(argument)
    analysis_modes = {}
    for spec in args.analysis:
        label, separator, mode = spec.partition('=')
        if not separator or label not in binaries or label in analysis_modes or mode not in ('upvalues', 'provenance'):
            parser.error('--analysis needs a known unique label and upvalues/provenance mode')
        analysis_modes[label] = mode
    args.keep.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="bench-", dir=args.keep)).resolve()
    corpus_hash, input_count = tree_hash(args.corpus, "*.lua")
    rows, expected = [], {}
    env = {k: v for k, v in os.environ.items() if not k.startswith("MEDAL_")}

    def sample(label, threads, index, warmup=False):
        output = work / label
        log = work / f"{label}-{threads}-{index}{'-warmup' if warmup else ''}.log"
        command = [str(binaries[label]), "decompile-folder", str(args.corpus.resolve()), str(output),
                   "--key", str(args.key), "--threads", str(threads), "--strict-no-synthetic-control", *lifter_args[label]]
        if label in analysis_modes:
            command.append('--emit-upvalue-analysis' if analysis_modes[label] == 'upvalues' else '--emit-binding-provenance')
        peak = None
        started = time.perf_counter()
        with log.open("wb") as stream:
            process = subprocess.Popen(command, stdout=stream, stderr=subprocess.STDOUT, env=env)
            rss = peak_rss_reader(process)
            while process.poll() is None:
                value = rss()
                if value is not None:
                    peak = max(peak or 0, value)
                if time.perf_counter() - started > args.timeout:
                    process.kill()
                    process.wait()
                    raise TimeoutError(f"benchmark timeout: {label}")
                time.sleep(0.01)
            exit_code = process.wait()
        elapsed = time.perf_counter() - started
        if exit_code:
            raise RuntimeError(f"benchmark failed: {label}; see {log}")
        output_hash, output_count = tree_hash(output, "*.luau")
        expected.setdefault(label, (output_hash, output_count))
        deterministic = (output_hash, output_count) == expected[label]
        row = dict(label=label, threads=threads, round=index, warmup=warmup, seconds=elapsed,
                   peak_rss_bytes=peak, output_hash=output_hash, output_count=output_count,
                   deterministic=deterministic, command=command)
        rows.append(row)
        print(f"{label} threads={threads} round={index} warmup={warmup}: {elapsed:.3f}s RSS={peak} deterministic={deterministic}", flush=True)

    for label in binaries:
        sample(label, max(args.threads), 0, True)
    for index in range(args.rounds):
        pairs = [(label, threads) for threads in args.threads for label in binaries]
        if index % 2:
            pairs.reverse()
        for label, threads in pairs:
            sample(label, threads, index)
    summary = []
    for label in binaries:
        for threads in args.threads:
            samples = [r for r in rows if not r["warmup"] and r["label"] == label and r["threads"] == threads]
            timings = sorted(r["seconds"] for r in samples)
            memories = [r["peak_rss_bytes"] for r in samples if r["peak_rss_bytes"] is not None]
            summary.append(dict(label=label, threads=threads, samples=len(timings),
                median_seconds=statistics.median(timings), p95_nearest_rank_seconds=timings[math.ceil(.95 * len(timings)) - 1],
                min_seconds=min(timings), max_seconds=max(timings),
                median_peak_rss_bytes=statistics.median(memories) if memories else None,
                max_peak_rss_bytes=max(memories) if memories else None))
    report = dict(schema_version=1, tools={label: dict(path=str(path), sha256=sha256(path)) for label, path in binaries.items()},
                  analysis_modes=analysis_modes, lifter_args=lifter_args,
                  corpus=str(args.corpus.resolve()), corpus_hash=corpus_hash, input_count=input_count,
                  system=dict(platform=platform.platform(), cpu=os.environ.get("PROCESSOR_IDENTIFIER"), logical_processors=os.cpu_count()),
                  rss_contract="Windows PeakWorkingSetSize sampled at 10 ms; unavailable on other platforms. Includes monitor overhead in CLI wall time.",
                  cache_contract="Warm input cache, output directory reused after warmup; no simultaneous benchmark workload.",
                  rows=rows, summary=summary, deterministic=all(r["deterministic"] for r in rows),
                  limitations="Finite samples; nearest-rank p95 equals maximum for seven rounds. No allocations, cold-cache or in-memory claim.")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    return int(not report["deterministic"])


if __name__ == "__main__":
    raise SystemExit(main())
