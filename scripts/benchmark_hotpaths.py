#!/usr/bin/env python3
"""Reproducible native A/B probes with cross-binary output hash gates.

Build identical benchmark_hotpaths/benchmark_api examples in clean baseline and
candidate trees using the same release toolchain/features. `prepare` stages
already-compiled regression fixtures and/or generates single-function folder
inputs. `pin` locks baseline source hashes for the existing API example. Timing
commands alternate process order and record all samples, never best-of-N.
Linux only (taskset affinity and wait4 per-process resource accounting).
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import shutil
import statistics
import subprocess
import tempfile
import threading
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def environment():
    return {k: v for k, v in os.environ.items()
            if not k.upper().startswith("MEDAL_") and k.upper() != "DEINLINE_ANCHOR_TRACE"}


def command(argv, timeout=120, cpus=None):
    argv = [str(x) for x in argv]
    if cpus:
        argv = ["taskset", "-c", cpus, *argv]
    # File capture prevents pipe backpressure while wait4 blocks without
    # polling. Its rusage belongs to this child, not cumulative prior children.
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        start = time.perf_counter()
        process = subprocess.Popen(argv, stdout=stdout, stderr=stderr, env=environment())
        expired = threading.Event()
        def expire():
            expired.set()
            # Popen.kill() may poll/reap the child and race wait4's accounting.
            try:
                os.kill(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        timer = threading.Timer(timeout, expire)
        timer.start()
        try:
            _, status, usage = os.wait4(process.pid, 0)
            process.returncode = os.waitstatus_to_exitcode(status)
            elapsed = time.perf_counter() - start
        except BaseException:
            try:
                os.kill(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                _, status, _ = os.wait4(process.pid, 0)
                process.returncode = os.waitstatus_to_exitcode(status)
            except ChildProcessError:
                pass
            raise
        finally:
            timer.cancel()
            timer.join()
        stdout.seek(0)
        stderr.seek(0)
        result = subprocess.CompletedProcess(argv, process.returncode, stdout.read(), stderr.read())
    if expired.is_set():
        raise TimeoutError(f"measurement exceeded {timeout}s: {argv}")
    if result.returncode:
        raise RuntimeError(f"exit {result.returncode}: {argv}\n{result.stderr.decode(errors='replace')}")
    return result, dict(command=argv, process_wall_seconds=elapsed,
                       process_cpu_seconds=usage.ru_utime + usage.ru_stime,
                       process_peak_rss_bytes=usage.ru_maxrss * 1024)


def tree_hash(root):
    digest = hashlib.sha256()
    paths = sorted(root.rglob("*.luau"))
    if not paths:
        raise ValueError(f"empty output tree: {root}")
    for path in paths:
        for part in (path.relative_to(root).as_posix().encode(), path.read_bytes()):
            digest.update(len(part).to_bytes(8, "little"))
            digest.update(part)
    return digest.hexdigest()


def prepare(args):
    if args.output.exists():
        raise ValueError("use a new --output directory to preserve the frozen workload")
    args.output.mkdir(parents=True)
    records = []
    if args.fixture_root:
        for path in sorted(args.fixture_root.rglob("input.bc")):
            relative = path.relative_to(args.fixture_root).with_suffix(".lua")
            target = args.output / "fixtures" / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(base64.b64encode(path.read_bytes()) + b"\n")
            records.append(dict(group="fixtures", path=relative.as_posix(), bytecode_sha256=digest(path)))
    if args.compiler:
        for index in range(args.files):
            source = args.output / "sources" / f"case{index}.luau"
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text("local value = seed\n" + "".join(
                f"value = step(value, {index + 1}, {j + 1})\n" for j in range(args.statements)) + "return value\n")
            result, _ = command([args.compiler.resolve(), "--binary", "-O1", "-g1", source.resolve()])
            if not result.stdout or result.stdout[0] == 0:
                raise ValueError(f"compiler did not produce bytecode: {source}")
            target = args.output / "unique" / f"Zone{index:04d}" / "Script.lua"
            target.parent.mkdir(parents=True)
            target.write_bytes(base64.b64encode(result.stdout) + b"\n")
            records.append(dict(group="unique", path=target.relative_to(args.output / "unique").as_posix(),
                                source_sha256=digest(source), bytecode_sha256=hashlib.sha256(result.stdout).hexdigest()))
        first = args.output / "unique" / "Zone0000" / "Script.lua"
        for index in range(args.files):
            target = args.output / "duplicate" / f"Zone{index:04d}" / "Script.lua"
            target.parent.mkdir(parents=True)
            shutil.copyfile(first, target)
    save(args.output / "workload.json", dict(schema_version=1, records=records,
         compiler_sha256=digest(args.compiler) if args.compiler else None, files=args.files, statements=args.statements))


def prepare_tables(args):
    if args.output.exists():
        raise ValueError("use a new --output directory to preserve the frozen workload")
    records = []
    for mode in ("calls", "values"):
        for size in args.sizes:
            name = f"table-{mode}-{size:05d}"
            source = args.output / "sources" / f"{name}.luau"
            source.parent.mkdir(parents=True, exist_ok=True)
            fields = [f"    key{i} = " + (f"f({i})" if mode == "calls" else f"seed + {i}") + ",\n" for i in range(size)]
            source.write_text("return {\n" + "".join(fields) + "}\n")
            process, _ = command([args.compiler.resolve(), "--binary", "-O1", "-g1", source.resolve()])
            if not process.stdout or process.stdout[0] == 0:
                raise ValueError(f"compiler did not produce bytecode: {source}")
            target = args.output / "inputs" / f"{name}.lua"
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(base64.b64encode(process.stdout) + b"\n")
            records.append(dict(mode=mode, size=size, path=target.name, source_sha256=digest(source),
                                bytecode_sha256=hashlib.sha256(process.stdout).hexdigest()))
    save(args.output / "workload.json", dict(schema_version=1, compiler_sha256=digest(args.compiler),
                                           flags=["--binary", "-O1", "-g1"], records=records))


def pin(args):
    # Baseline output is produced immediately before pinning, never inferred
    # from historical checked-in source or from the candidate binary.
    args.output.mkdir(parents=True, exist_ok=True)
    source_root = args.output / "locked-source"
    if source_root.exists():
        raise ValueError("baseline output already exists; use a new --output directory")
    process, metadata = command([args.baseline.resolve(), "decompile-folder", args.corpus.resolve(),
                                source_root.resolve(), "--key", args.key, "--threads", args.threads,
                                "--strict-no-synthetic-control"], args.timeout, args.cpus)
    (args.output / "baseline.log").write_bytes(process.stdout + process.stderr)
    scripts = []
    for path in sorted(args.corpus.rglob("*.lua")):
        relative = path.relative_to(args.corpus)
        source = source_root / relative.with_suffix(".luau")
        scripts.append(dict(path=relative.as_posix(), input_sha256=digest(path), source_sha256=digest(source),
                            groups=["all", relative.with_suffix("").as_posix()]))
    expected = {Path(s["path"]).with_suffix(".luau").as_posix() for s in scripts}
    actual = {p.relative_to(source_root).as_posix() for p in source_root.rglob("*.luau")}
    if expected != actual:
        raise ValueError("baseline source file set differs from input corpus")
    save(args.output / "manifest.json", dict(schema_version=1, decode_key=args.key, scripts=scripts))
    save(args.output / "baseline.json", dict(binary_sha256=digest(args.baseline), source_tree_sha256=tree_hash(source_root), **metadata))


def run(args):
    args.output.mkdir(parents=True, exist_ok=True)
    binaries = dict(baseline=args.baseline.resolve(), candidate=args.candidate.resolve())
    expected = None
    rows = []
    cases = args.case if args.mode == "hotpaths" else [None]
    report = dict(schema_version=1, mode=args.mode, binaries={k: dict(path=str(v), sha256=digest(v)) for k, v in binaries.items()},
                  platform=platform.platform(), affinity=args.cpus, allowed_cpus=sorted(os.sched_getaffinity(0)),
                  rounds=args.rounds, rows=rows, scope="Synthetic production AST paths" if args.mode == "hotpaths" else "Full native pipeline",
                  cache_note="Artifact cache state only; filesystem caches are not flushed.")
    if args.mode != "hotpaths":
        report.update(threads=args.threads, decode_key=args.key, corpus=str(args.corpus.resolve()),
                      inputs={p.relative_to(args.corpus).as_posix(): digest(p) for p in sorted(args.corpus.rglob("*.lua"))})
    if args.mode == "api":
        report["manifest_sha256"] = digest(args.manifest)
        report["decode_key"] = json.loads(args.manifest.read_text())["decode_key"]
        report["group"] = args.group
    if args.mode == "folder":
        report["artifact_cache_state"] = args.cache
    for case in cases:
        expected = None
        for round_index in range(args.rounds):
            for label in (["baseline", "candidate"] if round_index % 2 == 0 else ["candidate", "baseline"]):
                sample = args.output / (f"{case or args.mode}-{round_index}-{label}")
                if args.mode == "hotpaths":
                    mode, size = case.rsplit(":", 1)
                    process, metadata = command([binaries[label], mode, size, args.iterations], args.timeout, args.cpus)
                    result = json.loads(process.stdout)
                    output_hash = result["output_sha256"]
                    seconds = statistics.median(r["nanoseconds"] for r in result["rows"] if not r["warmup"]) / 1e9
                elif args.mode == "api":
                    process, metadata = command([binaries[label], "--manifest", args.manifest.resolve(), "--input-root", args.corpus.resolve(),
                        "--report", sample.with_suffix(".json").resolve(), "--threads", args.threads, "--rounds", args.iterations,
                        "--group", args.group], args.timeout, args.cpus)
                    result = json.loads(sample.with_suffix(".json").read_text())
                    output_hash = result["rows"][0]["output_tree_hash"]
                    seconds = statistics.median(r["seconds"] for r in result["rows"] if not r["first_call"])
                else:
                    source_root = sample / "source"
                    cache = sample / "cache"
                    if sample.exists():
                        raise ValueError(f"sample already exists; choose a fresh output directory: {sample}")
                    argv = [binaries[label], "decompile-folder", args.corpus.resolve(), source_root.resolve(),
                            "--key", args.key, "--threads", args.threads, "--strict-no-synthetic-control"]
                    if args.cache != "none":
                        argv += ["--cache-dir", cache.resolve()]
                    if args.cache == "warm":
                        command(argv, args.timeout, args.cpus)
                        # Both cold and warm samples write a new output tree.
                        shutil.rmtree(source_root)
                    process, metadata = command(argv, args.timeout, args.cpus)
                    result = dict(stderr=process.stderr.decode(errors="replace"))
                    output_hash = tree_hash(source_root)
                    seconds = metadata["process_wall_seconds"]
                if expected is not None and output_hash != expected:
                    raise ValueError(f"cross-binary/nondeterministic source mismatch: {case} {label} round {round_index}")
                expected = output_hash
                row = dict(case=case, round=round_index, label=label, seconds=seconds, output_sha256=output_hash,
                           measurement=result, **metadata)
                rows.append(row)
                print(f"{case or args.mode} {label} round={round_index} seconds={seconds:.6f}", flush=True)
                save(args.output / "report.json", report)
    summary = []
    for case in cases:
        values = {label: [r["seconds"] for r in rows if r["case"] == case and r["label"] == label] for label in binaries}
        medians = {label: statistics.median(v) for label, v in values.items()}
        paired = [a / b for a, b in zip(values["baseline"], values["candidate"])]
        summary.append(dict(case=case, median_seconds=medians, ratio_of_medians=medians["baseline"] / medians["candidate"],
                            median_paired_speedup=statistics.median(paired), min_paired_speedup=min(paired), max_paired_speedup=max(paired)))
    report["summary"] = summary
    save(args.output / "report.json", report)
    print(json.dumps(summary, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="mode", required=True)
    stage = sub.add_parser("prepare")
    stage.add_argument("--fixture-root", type=Path)
    stage.add_argument("--compiler", type=Path)
    stage.add_argument("--files", type=int, default=64)
    stage.add_argument("--statements", type=int, default=160)
    stage.add_argument("--output", type=Path, required=True)
    tables = sub.add_parser("prepare-tables")
    tables.add_argument("--compiler", type=Path, required=True)
    tables.add_argument("--sizes", type=int, nargs="+", default=[128, 512, 2048])
    tables.add_argument("--output", type=Path, required=True)
    pin_parser = sub.add_parser("pin")
    timing = []
    for mode in ("hotpaths", "api", "folder"):
        item = sub.add_parser(mode)
        item.add_argument("--candidate", type=Path, required=True)
        item.add_argument("--rounds", type=int, default=7)
        item.add_argument("--iterations", type=int, default=5)
        timing.append(item)
        if mode == "hotpaths":
            item.add_argument("--case", action="append", required=True, help="MODE:SIZE; repeat for multiple cases")
        elif mode == "api":
            item.add_argument("--manifest", type=Path, required=True)
            item.add_argument("--group", default="all")
        else:
            item.add_argument("--cache", choices=("none", "cold", "warm"), default="none")
    for item in [pin_parser, *timing]:
        item.add_argument("--baseline", type=Path, required=True)
        item.add_argument("--output", type=Path, required=True)
        item.add_argument("--cpus", help="taskset CPU list; defaults to the first permitted CPU")
        item.add_argument("--timeout", type=float, default=120)
        if item.prog.split()[-1] != "hotpaths":
            item.add_argument("--corpus", type=Path, required=True)
            item.add_argument("--key", type=int, default=1)
            item.add_argument("--threads", type=int, default=1)
    args = parser.parse_args()
    if args.mode == "prepare-tables":
        if any(size < 1 for size in args.sizes):
            parser.error("positive table sizes required")
        prepare_tables(args)
    elif args.mode == "prepare":
        if not args.fixture_root and not args.compiler:
            parser.error("prepare requires --fixture-root and/or --compiler")
        if args.files < 1 or args.statements < 1:
            parser.error("positive workload sizes required")
        prepare(args)
    else:
        args.cpus = args.cpus or str(min(os.sched_getaffinity(0)))
        if args.mode == "pin":
            pin(args)
        else:
            if args.rounds < 1 or args.iterations < 1:
                parser.error("positive sample counts required")
            # Every sample needs fresh destination/cache state.
            if (args.output / "report.json").exists():
                parser.error("report already exists; use a new --output directory")
            run(args)


if __name__ == "__main__":
    main()
