#!/usr/bin/env python3
"""Group source-like structurer rejections from a serial verbose corpus run.

Set MEDAL_DEBUG_RESTRUCTURE=1 and run decompile-folder with --threads 1
--verbose, redirecting stdout and stderr to the same log. File completion
records delimit each input; prototype IDs are local to an input.
"""
import argparse
import collections
import json
import pathlib
import re
import shutil


def inventory(log):
    completed = re.search(r"Done: (\d+) decompiled, (\d+) skipped .*?, (\d+) failed\.", log)
    if not completed or not re.search(r"Time: .*\b1 threads\)", log):
        raise ValueError("inventory requires a completed --threads 1 corpus log")
    if sum(line.startswith("ok ") for line in log.splitlines()) != int(completed[1]):
        raise ValueError("log is missing verbose input completion records")
    pending = []
    traces = collections.defaultdict(list)
    rows = []
    for line in log.splitlines():
        trace = re.match(
            r"source-like unsupported id=(\d+) shared_tail=(true|false) "
            r"reason=(\S+) node=(\d+) stop=(.*)", line
        )
        if trace:
            proto, shared, reason, node, stop = trace.groups()
            traces[int(proto)].append(dict(
                shared_tail=shared == "true", reason=reason, node=int(node), stop=stop
            ))
        attempt = re.match(r"source-like (first attempt|retry) id=(\d+) -> (.*)", line)
        if attempt:
            phase, proto, result = attempt.groups()
            proto = int(proto)
            if phase == "retry" and result == "Unsupported":
                locations = traces.pop(proto, [])
                # Wrapper failures are propagated from the innermost failure.
                cause = next((t["reason"] for t in locations
                              if t["reason"] not in ("path", "conditional", "loop")),
                             locations[0]["reason"] if locations else "analysis-or-coverage")
                pending.append(dict(proto=proto, reason=cause, trace=locations))
            else:
                traces.pop(proto, None)
        if line.startswith("ok ") or line.startswith("FAIL "):
            path = line.split(" ", 1)[1]
            rows.extend(dict(file=path, **row) for row in pending)
            pending.clear()
            traces.clear()
    if pending:
        raise ValueError("log ends before the pending input's completion record")
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=pathlib.Path)
    parser.add_argument("--report", required=True, type=pathlib.Path)
    parser.add_argument("--corpus", type=pathlib.Path,
                        help="source corpus to copy rejected inputs from")
    parser.add_argument("--copy-to", type=pathlib.Path,
                        help="copy rejected inputs to this directory for focused runs")
    args = parser.parse_args()
    if bool(args.corpus) != bool(args.copy_to):
        parser.error("--corpus and --copy-to must be used together")
    rows = inventory(args.log.read_text(encoding="utf-8-sig"))
    summary = dict(events=len(rows), files=len({r["file"] for r in rows}),
                   reasons=dict(collections.Counter(r["reason"] for r in rows)))
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(dict(summary=summary, rejections=rows), indent=2)
                           + "\n", encoding="utf-8")
    if args.corpus:
        source = args.corpus.resolve()
        target = args.copy_to.resolve()
        for name in sorted({r["file"] for r in rows}):
            src, dst = (source / name).resolve(), (target / name).resolve()
            if not src.is_relative_to(source) or not dst.is_relative_to(target):
                raise ValueError(f"input path escapes corpus: {name}")
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src, dst)
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
