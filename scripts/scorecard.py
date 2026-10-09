#!/usr/bin/env python3
"""Readability scorecard: decompile the 51-sample research set and score it.

Usage::

    scorecard.py --lifter target/release/luau-lifter [--samples DIR] [--out DIR] [--json REPORT]

The sample set (``index.json``, ``corpus/<name>/input.lua`` saved with key
203, ``public/<name>/input.bin`` raw ``-O2 -g1`` bytecode with
``source.luau`` beside it) comes from the de-inline research. Every number is
deterministic:

* ``lines``: non-blank lines, without luacid's banner lines.
* ``calls``: calls the de-inliners rebuilt, from ``--stats-json`` (counted in
  the final tree, statement and expression sites alike). ``markers`` is the
  older count, from the per-site comments, which expression sites lack.
* ``gen_share``: declared bindings (locals, parameters, loop variables, local
  functions) whose name is generated: Tovek ``v``/``vN``/``p``/``pN``; luacid
  ``var``/``arg``/``tbl``/... plus digits.
* ``name_recall`` and ``token_sim``: public samples only, ``Fusion_Types``
  excluded (its source is types only). The share of source-declared names the
  output also declares (as a multiset), and the mean difflib ratio over token
  sequences.
* ``census``: inlined copies of named helpers, found from line info (Luau
  ``-O2`` keeps the callee's lines on inlined code). ``census_recall`` credits
  each helper with at most as many rebuilt calls as it has copies.

The luacid row scores the outputs stored with the samples.
"""
from __future__ import annotations

import argparse
import collections
import difflib
import json
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

from bytecode_roundtrip import OP_INDEX, BytecodeError, parse_chunk

DEFAULT_SAMPLES = pathlib.Path("D:/Medal/v22-work/tmp/deinline-research")

KEYWORDS = set(
    "and break do else elseif end false for function if in local nil not or repeat return then true until while continue".split()
)
# Long comments, line comments, long strings, quoted strings, backticks,
# identifiers, numbers, punctuation.
TOKEN = re.compile(
    r"--\[(=*)\[.*?\]\1\]"
    r"|--[^\n]*"
    r"|\[(=*)\[.*?\]\2\]"
    r'|"(?:\\.|[^"\\\n])*"'
    r"|'(?:\\.|[^'\\\n])*'"
    r"|`(?:\\.|[^`\\])*`"
    r"|[A-Za-z_][A-Za-z0-9_]*"
    r"|\d[\w.]*"
    r"|::|\.\.\.|\.\.|[=~<>]=|\S",
    re.S,
)
GENERATED = {
    "tovek": re.compile(r"^(v|p)\d*$"),
    "luacid": re.compile(r"^(var|arg|tbl|str|num|func|bool|vec|nil|cf|inst|any|ud|buf|thread|v|p)\d+$"),
}
BANNERS = ("-- [[ luacid.dev", "-- Luau bytecode version")


def tokens(text: str) -> list[str]:
    return [m.group(0) for m in TOKEN.finditer(text) if not m.group(0).startswith("--")]


def _identifier(token: str) -> bool:
    return bool(re.match(r"[A-Za-z_][A-Za-z0-9_]*$", token)) and token not in KEYWORDS


def _skip_type(ts: list[str], i: int) -> int:
    depth = 0
    while i < len(ts):
        t = ts[i]
        if t in "({[<":
            depth += 1
        elif t in ")}]>":
            if depth == 0:
                return i
            depth -= 1
        elif depth == 0 and (t in (",", "=", "in") or t in KEYWORDS and t != "nil"):
            return i
        i += 1
    return i


def declarations(text: str) -> list[tuple[str, str]]:
    """``(kind, name)`` of every binding the text declares: ``local`` names,
    ``local function`` names, parameters and ``for`` variables. As in the
    research scorecard the baselines come from, the parameters of a ``local
    function`` are not read, which keeps the numbers comparable."""
    ts = tokens(text)
    names = []
    i = 0
    while i < len(ts):
        t = ts[i]
        if t == "local" and i + 1 < len(ts) and ts[i + 1] == "function":
            names.append(("fn", ts[i + 2]))
            i += 3
        elif t in ("local", "for"):
            kind = "local" if t == "local" else "for"
            j = i + 1
            while j < len(ts) and _identifier(ts[j]):
                names.append((kind, ts[j]))
                j += 1
                if j < len(ts) and ts[j] == ":":
                    j = _skip_type(ts, j + 1)
                if j < len(ts) and ts[j] == ",":
                    j += 1
                    continue
                break
            i = j
        elif t == "function":
            j = i + 1
            while j < len(ts) and ts[j] != "(":
                j += 1
            j += 1
            while j < len(ts) and ts[j] != ")":
                if _identifier(ts[j]):
                    names.append(("param", ts[j]))
                if ts[j] == ":":
                    j = _skip_type(ts, j + 1)
                    continue
                j += 1
            i = j
        else:
            i += 1
    return names


def count_lines(text: str) -> int:
    return sum(1 for line in text.splitlines() if line.strip() and not line.startswith(BANNERS))


def marker_calls(text: str, who: str) -> int:
    """The comment-based call count: luacid's self-report, Tovek's per-site markers."""
    if who == "luacid":
        return sum(int(n) for n in re.findall(r"(\d+) call sites? recovered", text))
    # Site comments in every wording Tovek has printed: `-- inferred equivalent
    # call` per line since M1, `-- equivalent call inferred; ...` per call
    # before, `-- inferred call` in compact mode. A helper's definition line
    # (`-- 3 equivalent calls inferred from this helper`) is no site.
    return len(re.findall(r"equivalent calls? inferred(?! from this)|inferred (?:equivalent )?call\b", text))


# --------------------------------------------------------------------------
# Line-info census of inlined copies
# --------------------------------------------------------------------------

# Instructions a closure's definition emits on its own first line in the
# lexical parent; they are not an inlined copy.
DEFINITION_OPS = {OP_INDEX[n] for n in "NEWCLOSURE DUPCLOSURE CAPTURE PREPVARARGS SETTABLEKS SETGLOBAL MOVE".split()}


def parse_any_key(data: bytes):
    """Parse a chunk saved plain or with Roblox's encoded opcodes (key 203).
    A wrong key shows as a bad opcode or protos that do not end in RETURN."""
    ret = OP_INDEX["RETURN"]
    for key in (1, 203):
        try:
            chunk = parse_chunk(data, key)
        except BytecodeError:
            continue
        if all(not p.insns or p.insns[-1][1] == ret for p in chunk.protos):
            return chunk
    raise BytecodeError("neither plain nor key 203")


def census(chunk) -> collections.Counter:
    """Inlined copies per helper prototype, keyed ``name#id`` (``?`` when the
    prototype has no debug name).

    For each caller, each instruction is attributed to the innermost helper
    outside the caller's own lexical scope whose span ``[linedefined, last
    line]`` holds its line; a nested inlined helper's lines continue the outer
    copy. A copy starts when the helper changes, or when a copy that reached
    its last line starts again at its first (two copies back to back).
    """
    protos = chunk.protos
    own = {}  # helper id -> (first line, last line)
    full = {}  # helper id -> every line its own code has
    for p in protos:
        if p.lines and p.id != chunk.main:
            own[p.id] = (p.line_defined, max(p.lines))
            full[p.id] = set(p.lines)
    counts = collections.Counter()
    for caller in protos:
        if not caller.lines:
            continue
        if caller.id in own:
            first, last = own[caller.id]
            candidates = [h for h in own if h != caller.id and not (own[h][0] <= first and last <= own[h][1])]
        else:
            candidates = list(own)
        caller_span = own.get(caller.id, (-1, 1 << 30))
        current = None  # [helper, last own line reached]
        for pc, op, *_ in caller.insns:
            line = caller.lines[pc]
            if current is not None and line in full[current[0]]:
                a, b = own[current[0]]
                inside = [x for x in full[current[0]] if a <= x <= b]
                start = min(inside) if inside else a
                if not (a <= line <= b and line == start and current[1] == b and start != b):
                    if a <= line <= b:
                        current[1] = line
                    continue
                current = None  # a second copy back to back
            best = None
            for h in candidates:
                a, b = own[h]
                definition = caller_span[0] <= a and line == a and op in DEFINITION_OPS
                if a <= line <= b and not definition and (best is None or b - a < own[best][1] - own[best][0]):
                    best = h
            if best is None:
                current = None
            else:
                current = [best, line]
                counts[best] += 1
    names = {}
    for h, n in counts.items():
        name = chunk.strings[protos[h].name - 1].decode("utf-8", "replace") if protos[h].name else "?"
        names[f"{name}#{h}"] = n
    return collections.Counter(names)


def named_copies(copies: collections.Counter) -> collections.Counter:
    """Copies per named helper prototype (``name#id`` keys kept), anonymous
    helpers left out."""
    return collections.Counter({key: n for key, n in copies.items() if key.rsplit("#", 1)[0] != "?"})


def rebuilt_calls(calls_by_helper) -> tuple[collections.Counter, bool]:
    """Rebuilt calls per helper prototype id (``tovek-stats/2`` lists each
    helper with its ``proto``; one without cannot match a copy), and False;
    or, from an older lifter's dict of printed name to calls, per name, and
    True (helpers that print alike merge there)."""
    if isinstance(calls_by_helper, dict):
        return collections.Counter(calls_by_helper), True
    calls = collections.Counter()
    for entry in calls_by_helper:
        if entry.get("proto") is not None:
            calls[entry["proto"]] += entry["calls"]
    return calls, False


def census_hits(copies: collections.Counter, calls: collections.Counter, by_name: bool = False) -> int:
    """Rebuilt calls the census accounts for: per helper, at most its copies,
    joined on the prototype id (or on the name, see ``rebuilt_calls``)."""
    joined = collections.Counter()
    for key, n in copies.items():
        name, proto = key.rsplit("#", 1)
        joined[name if by_name else int(proto)] += n
    return sum(min(n, calls.get(key, 0)) for key, n in joined.items())


# --------------------------------------------------------------------------
# Decompiling the samples
# --------------------------------------------------------------------------

def supports_stats(lifter: str) -> bool:
    """Whether the lifter writes ``--stats-json`` (older builds do not)."""
    run = subprocess.run([lifter, "decompile-folder", "--help"], capture_output=True)
    return b"--stats-json" in run.stdout


def decompile(lifter: str, samples: pathlib.Path, index: list, out: pathlib.Path) -> dict | None:
    """Write ``out/<name>.luau`` for every sample; return each one's stats,
    or ``None`` for a lifter without ``--stats-json``."""
    out.mkdir(parents=True, exist_ok=True)
    with_stats = supports_stats(lifter)
    stats = {}
    with tempfile.TemporaryDirectory(prefix="scorecard-") as scratch:
        scratch = pathlib.Path(scratch)
        corpus = [x["name"] for x in index if x["kind"] == "corpus"]
        if corpus:
            (scratch / "in").mkdir()
            for name in corpus:
                shutil.copy(samples / "corpus" / name / "input.lua", scratch / "in" / f"{name}.lua")
            report = ["--stats-json", scratch / "corpus.json"] if with_stats else []
            subprocess.run([lifter, "decompile-folder", scratch / "in", scratch / "out", "--key", "203", *report],
                           capture_output=True)
            if (scratch / "corpus.json").exists():
                for script in json.loads((scratch / "corpus.json").read_text(encoding="utf-8"))["scripts"]:
                    stats[pathlib.PurePath(script["script"]).stem] = script
            for name in corpus:
                produced = scratch / "out" / f"{name}.luau"
                text = produced.read_text(encoding="utf-8") if produced.exists() else "-- tovek: no output\n"
                (out / f"{name}.luau").write_text(text, encoding="utf-8", newline="")
        for x in index:
            if x["kind"] != "public":
                continue
            name = x["name"]
            report = scratch / f"{name}.json"
            run = subprocess.run([lifter, samples / "public" / name / "input.bin",
                                  *(["--stats-json", report] if with_stats else [])], capture_output=True)
            text = run.stdout.decode("utf-8", "replace").replace("\r\n", "\n")
            if run.returncode != 0:
                text += "-- tovek: failed\n"
            (out / f"{name}.luau").write_text(text, encoding="utf-8", newline="")
            if report.exists():
                stats[name] = json.loads(report.read_text(encoding="utf-8"))["scripts"][0]
    return stats if with_stats else None


def score(samples: pathlib.Path, index: list, outputs: dict, stats: dict | None, who: str) -> dict:
    """Totals over the samples. ``outputs`` maps a sample name to its text."""
    total = collections.Counter()
    # Summed over the samples' stats: calls by kind, refusals by reason.
    kinds, refused_helpers, refused_sites = collections.Counter(), collections.Counter(), collections.Counter()
    similarities = []
    for x in index:
        name, kind = x["name"], x["kind"]
        text = outputs[name]
        decls = declarations(text)
        total["lines"] += count_lines(text)
        total["markers"] += marker_calls(text, who)
        if stats is not None:
            script = stats.get(name, {})
            kinds.update(script.get("reconstructed_calls", {}))
            refused_helpers.update(script.get("refused_helpers", {}))
            refused_sites.update(script.get("refused_sites", {}))
            total["missing_stats"] += name not in stats
        total["decls"] += len(decls)
        total["generated"] += sum(1 for _, n in decls if GENERATED[who].match(n))
        if kind == "public" and name != "Fusion_Types":
            source = (samples / "public" / name / "source.luau").read_text(encoding="utf-8", errors="replace")
            wanted = collections.Counter(n for _, n in declarations(source) if n != "_")
            got = collections.Counter(n for _, n in decls if n != "_")
            total["name_hit"] += sum((wanted & got).values())
            total["name_total"] += sum(wanted.values())
            similarities.append(difflib.SequenceMatcher(None, tokens(source), tokens(text), autojunk=False).ratio())
    result = dict(
        lines=total["lines"],
        calls=kinds["total"] if stats is not None else total["markers"],
        markers=total["markers"],
        generated=total["generated"], decls=total["decls"],
        gen_share=total["generated"] / total["decls"],
        name_hit=total["name_hit"], name_total=total["name_total"],
        name_recall=total["name_hit"] / total["name_total"],
        token_sim=sum(similarities) / len(similarities),
    )
    if stats is not None:
        result.update(missing_stats=total["missing_stats"], calls_by_kind=dict(kinds),
                      refused_helpers=dict(refused_helpers), refused_sites=dict(refused_sites))
    return result


def census_report(samples: pathlib.Path, index: list, stats: dict) -> dict:
    copies_total = hits = 0
    per_sample = {}
    for x in index:
        chunk = parse_any_key((samples / x["kind"] / x["name"] / "input.bin").read_bytes())
        copies = named_copies(census(chunk))
        calls, by_name = rebuilt_calls(stats.get(x["name"], {}).get("calls_by_helper", []))
        sample_hits = census_hits(copies, calls, by_name)
        copies_total += sum(copies.values())
        hits += sample_hits
        per_sample[x["name"]] = dict(copies=sum(copies.values()), hits=sample_hits)
    return dict(census=copies_total, census_hits=hits, census_recall=hits / copies_total if copies_total else 0.0,
                per_sample=per_sample)


def row(who: str, s: dict) -> str:
    text = (f"{who:7s} lines {s['lines']:6d}  calls {s['calls']:4d}  gen_share {s['gen_share']:.3f} "
            f"({s['generated']}/{s['decls']})  name_recall {s['name_recall']:.3f} ({s['name_hit']}/{s['name_total']})  "
            f"token_sim {s['token_sim']:.3f}")
    if "census_recall" in s:
        text += f"  markers {s['markers']}  census_recall {s['census_recall']:.3f} ({s['census_hits']}/{s['census']})"
    return text


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--lifter", required=True)
    parser.add_argument("--samples", type=pathlib.Path, default=DEFAULT_SAMPLES)
    parser.add_argument("--out", type=pathlib.Path, help="keep the decompiled samples here")
    parser.add_argument("--json", type=pathlib.Path, help="write the full report here")
    args = parser.parse_args()
    index = json.loads((args.samples / "index.json").read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix="scorecard-out-") as default_out:
        out = args.out or pathlib.Path(default_out)
        stats = decompile(args.lifter, args.samples, index, out)
        outputs = {x["name"]: (out / f"{x['name']}.luau").read_text(encoding="utf-8", errors="replace") for x in index}
    tovek = score(args.samples, index, outputs, stats, "tovek")
    if stats is None:
        print("note: the lifter has no --stats-json; calls are its per-site markers", file=sys.stderr)
    else:
        tovek.update(census_report(args.samples, index, stats))
    luacid_outputs = {x["name"]: (args.samples / x["kind"] / x["name"] / "luacid.luau").read_text(encoding="utf-8", errors="replace")
                      for x in index}
    luacid = score(args.samples, index, luacid_outputs, None, "luacid")
    print(row("tovek", tovek))
    print(row("luacid", luacid))
    if stats is not None:
        print("tovek calls by kind", {k: v for k, v in sorted(tovek["calls_by_kind"].items()) if k != "total"},
              "refused helpers", dict(sorted(tovek["refused_helpers"].items())),
              "refused sites", dict(sorted(tovek["refused_sites"].items())))
    if tovek.get("missing_stats"):
        print(f"warning: {tovek['missing_stats']} samples have no stats", file=sys.stderr)
    if args.json:
        args.json.write_text(json.dumps(dict(tovek=tovek, luacid=luacid), indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
