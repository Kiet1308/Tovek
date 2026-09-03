#!/usr/bin/env python3
"""Bytecode round-trip oracle for the Tovek decompiler.

For every saved-bytecode input (UniversalSynSaveInstance text: optional `--`
comment header then one base64 blob of Luau bytecode) the script

1. decompiles the whole tree with `luau-lifter decompile-folder` (strict);
2. recompiles every emitted `.luau` with the official compiler
   (`luau-compile --binary -O2 -g1 --fflags=false`, bytecode version 9);
3. parses both bytecode chunks with an independent Python deserializer;
4. normalises every prototype (registers dropped, constants by value, jump
   targets as labels) and compares the original with the recompiled proto.

Tiers per prototype pair (see `docs/bytecode_roundtrip.md`):

* ``exact``  - normalised instruction stream identical;
* ``equiv``  - multiset of semantic instructions identical; only block order,
               branch polarity, register copies or unconditional jumps differ
               (guard <-> nesting, `and`/`or` reassociation, `+=`, renaming);
* ``differ`` - the semantic multiset differs; the report lists which
               instruction families were lost/added so the case can be
               triaged into acceptable / investigate / bug;
* ``missing``/``extra`` - prototype without a counterpart (helper synthesised
               by de-inlining, closure merged/split, ...).

Usage (corpus mode)::

    bytecode_roundtrip.py --lifter target/release/luau-lifter \
        --compiler luau-compile --corpus D:/corpus --key 203 \
        --report out.json --markdown out.md [--baseline base.json]

Usage (ground-truth mode - directory of real `.luau` sources)::

    bytecode_roundtrip.py --lifter ... --compiler ... --sources docs/failure_fixtures/semantic_roundtrip

In ground-truth mode every source is compiled at -O2 first, run through the
same pipeline, and a token-level "source likeness" ratio is reported as well.

Exit status is non-zero when any input fails to decompile/recompile/parse, or
when `--baseline` is given and the number of non-equivalent prototypes grew
(overall, or in any single file).
"""
from __future__ import annotations

import argparse
import base64
import collections
import concurrent.futures
import difflib
import json
import os
import pathlib
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import time

# --------------------------------------------------------------------------
# Luau bytecode deserialiser (versions 4..11, types 0..3), key-aware.
# --------------------------------------------------------------------------

OPCODES = (
    "NOP BREAK LOADNIL LOADB LOADN LOADK MOVE GETGLOBAL SETGLOBAL GETUPVAL SETUPVAL "
    "CLOSEUPVALS GETIMPORT GETTABLE SETTABLE GETTABLEKS SETTABLEKS GETTABLEN SETTABLEN "
    "NEWCLOSURE NAMECALL CALL RETURN JUMP JUMPBACK JUMPIF JUMPIFNOT JUMPIFEQ JUMPIFLE "
    "JUMPIFLT JUMPIFNOTEQ JUMPIFNOTLE JUMPIFNOTLT ADD SUB MUL DIV MOD POW ADDK SUBK MULK "
    "DIVK MODK POWK AND OR ANDK ORK CONCAT NOT MINUS LENGTH NEWTABLE DUPTABLE SETLIST "
    "FORNPREP FORNLOOP FORGLOOP FORGPREP_INEXT FASTCALL3 FORGPREP_NEXT NATIVECALL "
    "GETVARARGS DUPCLOSURE PREPVARARGS LOADKX JUMPX FASTCALL COVERAGE CAPTURE SUBRK DIVRK "
    "FASTCALL1 FASTCALL2 FASTCALL2K FORGPREP JUMPXEQKNIL JUMPXEQKB JUMPXEQKN JUMPXEQKS "
    "IDIV IDIVK GETUDATAKS SETUDATAKS NAMECALLUDATA NEWCLASSMEMBER CALLFB CMPPROTO"
).split()
OP_INDEX = {name: i for i, name in enumerate(OPCODES)}

AUX_OPS = {
    OP_INDEX[n]
    for n in (
        "GETGLOBAL SETGLOBAL GETIMPORT GETTABLEKS SETTABLEKS NAMECALL JUMPIFEQ JUMPIFLE "
        "JUMPIFLT JUMPIFNOTEQ JUMPIFNOTLE JUMPIFNOTLT NEWTABLE SETLIST FORGLOOP LOADKX "
        "FASTCALL2 FASTCALL2K FASTCALL3 JUMPXEQKNIL JUMPXEQKB JUMPXEQKN JUMPXEQKS "
        "GETUDATAKS SETUDATAKS NAMECALLUDATA NEWCLASSMEMBER CALLFB CMPPROTO"
    ).split()
}

# Operand layout per opcode: "abc", "ad", "e" (mirrors luau-lifter/src/instruction.rs).
_AD_OPS = set([4, 5, 12, 19] + list(range(23, 33)) + [54] + list(range(56, 60)) + [61, 64] + list(range(76, 81)) + [88])
_E_OPS = {67, 69}


class BytecodeError(Exception):
    pass


class Reader:
    __slots__ = ("data", "pos")

    def __init__(self, data: bytes):
        self.data = data
        self.pos = 0

    def u8(self) -> int:
        if self.pos >= len(self.data):
            raise BytecodeError("truncated (u8)")
        v = self.data[self.pos]
        self.pos += 1
        return v

    def u32(self) -> int:
        if self.pos + 4 > len(self.data):
            raise BytecodeError("truncated (u32)")
        v = struct.unpack_from("<I", self.data, self.pos)[0]
        self.pos += 4
        return v

    def varint(self) -> int:
        result = 0
        shift = 0
        while True:
            b = self.u8()
            result |= (b & 0x7F) << shift
            shift += 7
            if not b & 0x80:
                return result

    def bytes(self, n: int) -> bytes:
        if self.pos + n > len(self.data):
            raise BytecodeError("truncated (bytes)")
        v = self.data[self.pos : self.pos + n]
        self.pos += n
        return v

    def string(self) -> bytes:
        return self.bytes(self.varint())


class Proto:
    __slots__ = (
        "id", "max_stack", "num_params", "num_upvalues", "is_vararg", "code",
        "constants", "children", "line_defined", "name", "insns", "stream", "sig",
    )

    def __init__(self):
        self.insns = []  # list of (pc, op, a, b, c, d, e, aux)


class Chunk:
    __slots__ = ("version", "types_version", "strings", "protos", "main")


def _decode_insn(word: int, key: int):
    op = (word & 0xFF) * key & 0xFF
    a = (word >> 8) & 0xFF
    b = (word >> 16) & 0xFF
    c = (word >> 24) & 0xFF
    d = (word >> 16) & 0xFFFF
    if d >= 0x8000:
        d -= 0x10000
    e = word >> 8
    if e >= 0x800000:
        e -= 0x1000000
    return op, a, b, c, d, e


def parse_chunk(data: bytes, key: int) -> Chunk:
    r = Reader(data)
    version = r.u8()
    if version == 0:
        raise BytecodeError("compile error: " + data[1:].decode("utf-8", "replace")[:200])
    if not 4 <= version <= 11:
        raise BytecodeError(f"unsupported bytecode version {version}")
    ch = Chunk()
    ch.version = version
    ch.types_version = r.u8() if version >= 4 else 0
    if ch.types_version > 3:
        raise BytecodeError(f"unsupported types version {ch.types_version}")
    ch.strings = [r.string() for _ in range(r.varint())]
    if ch.types_version == 3:
        while True:
            idx = r.u8()
            if idx == 0:
                break
            r.varint()
    nprotos = r.varint()
    ch.protos = []
    for pid in range(nprotos):
        p = Proto()
        p.id = pid
        p.max_stack = r.u8()
        p.num_params = r.u8()
        p.num_upvalues = r.u8()
        p.is_vararg = r.u8() != 0
        if version >= 4:
            r.u8()  # flags
            r.bytes(r.varint())  # type info
        ncode = r.varint()
        code = [r.u32() for _ in range(ncode)]
        p.code = code
        p.constants = [_parse_constant(r, version) for _ in range(r.varint())]
        p.children = [r.varint() for _ in range(r.varint())]
        p.line_defined = r.varint()
        p.name = r.varint()
        if r.u8():  # line info
            gap = r.u8()
            r.bytes(ncode)
            r.bytes(4 * (((ncode - 1) >> gap) + 1))
        if r.u8():  # debug info
            for _ in range(r.varint()):
                r.varint(); r.varint(); r.varint(); r.u8()
            for _ in range(r.varint()):
                r.varint()
        if version >= 11:
            for _ in range(r.varint()):
                if r.u8() != 0:
                    raise BytecodeError("unknown feedback slot")
                r.varint()
        # decode instructions
        pc = 0
        insns = p.insns
        while pc < ncode:
            op, a, b, c, d, e = _decode_insn(code[pc], key)
            if op >= len(OPCODES):
                raise BytecodeError(f"bad opcode {op} at pc {pc} (wrong key?)")
            aux = 0
            length = 1
            if op in AUX_OPS:
                if pc + 1 >= ncode:
                    raise BytecodeError("missing aux word")
                aux = code[pc + 1]
                length = 2
            insns.append((pc, op, a, b, c, d, e, aux))
            pc += length
        ch.protos.append(p)
    ch.main = r.varint()
    return ch


def _parse_constant(r: Reader, version: int):
    tag = r.u8()
    if tag == 0:
        return ("nil",)
    if tag == 1:
        return ("bool", r.u8() != 0)
    if tag == 2:
        return ("num", struct.unpack("<d", r.bytes(8))[0])
    if tag == 3:
        return ("str", r.varint())
    if tag == 4:
        return ("import", r.u32())
    if tag == 5:
        return ("table", tuple(r.varint() for _ in range(r.varint())))
    if tag == 6:
        return ("closure", r.varint())
    if tag == 7:
        return ("vec", struct.unpack("<4f", r.bytes(16)))
    if tag == 8:
        pairs = []
        for _ in range(r.varint()):
            k = r.varint()
            v = struct.unpack("<i", r.bytes(4))[0]
            pairs.append((k, v))
        return ("tablek", tuple(pairs))
    if tag == 9:
        neg = r.u8()
        mag = r.varint()
        return ("num", float(-mag if neg else mag))
    if tag == 10:
        r.varint(); np_ = r.varint(); nm = r.varint()
        for _ in range(np_ + nm):
            r.varint()
        return ("class",)
    raise BytecodeError(f"unknown constant tag {tag}")


# --------------------------------------------------------------------------
# Normalisation
# --------------------------------------------------------------------------

_NUM_FMT = "%.17g"


def _fmt_num(x: float) -> str:
    if x != x:
        return "nan"
    if x in (float("inf"), float("-inf")):
        return "inf" if x > 0 else "-inf"
    if x == int(x) and abs(x) < 1e15:
        return str(int(x))
    return _NUM_FMT % x


def const_repr(ch: Chunk, p: Proto, idx: int, proto_map=None) -> str:
    if idx < 0 or idx >= len(p.constants):
        return f"?k{idx}"
    k = p.constants[idx]
    tag = k[0]
    if tag == "nil":
        return "nil"
    if tag == "bool":
        return "true" if k[1] else "false"
    if tag == "num":
        return _fmt_num(k[1])
    if tag == "str":
        s = ch.strings[k[1] - 1] if 0 < k[1] <= len(ch.strings) else b"?"
        return '"' + s.decode("utf-8", "backslashreplace") + '"'
    if tag == "import":
        idv = k[1]
        count = idv >> 30
        parts = []
        for i in range(count):
            ci = (idv >> (20 - 10 * i)) & 1023
            parts.append(const_repr(ch, p, ci).strip('"'))
        return "@" + ".".join(parts)
    if tag == "table":
        return "{" + ",".join(const_repr(ch, p, i) for i in k[1]) + "}"
    if tag == "tablek":
        return "{" + ",".join(f"{const_repr(ch, p, i)}={const_repr(ch, p, v) if v >= 0 else '_'}" for i, v in k[1]) + "}"
    if tag == "closure":
        return "fn"
    if tag == "vec":
        return "vec(" + ",".join(_fmt_num(v) for v in k[1]) + ")"
    return tag


# instruction families used for the ``equiv`` multiset.  Everything that only
# reflects register allocation / block layout is dropped.
_DROP_IN_SIG = {
    OP_INDEX[n]
    for n in (
        "NOP BREAK MOVE JUMP JUMPBACK JUMPX COVERAGE CLOSEUPVALS PREPVARARGS "
        "FASTCALL FASTCALL1 FASTCALL2 FASTCALL2K FASTCALL3 NATIVECALL"
    ).split()
}
_CANON = {
    "JUMPIF": "TEST", "JUMPIFNOT": "TEST",
    "JUMPIFEQ": "CMPEQ", "JUMPIFNOTEQ": "CMPEQ",
    "JUMPIFLE": "CMPLE", "JUMPIFNOTLE": "CMPLE",
    "JUMPIFLT": "CMPLT", "JUMPIFNOTLT": "CMPLT",
    "JUMPXEQKNIL": "CMPKNIL", "JUMPXEQKB": "CMPKB", "JUMPXEQKN": "CMPK", "JUMPXEQKS": "CMPK",
    "ADDK": "ADD", "SUBK": "SUB", "MULK": "MUL", "DIVK": "DIV", "MODK": "MOD", "POWK": "POW",
    "IDIVK": "IDIV", "SUBRK": "SUB", "DIVRK": "DIV", "ANDK": "AND", "ORK": "OR",
    "LOADKX": "LOADK", "LOADN": "LOADK", "LOADB": "LOADK", "DUPCLOSURE": "NEWCLOSURE",
    "FORGPREP_INEXT": "FORGPREP", "FORGPREP_NEXT": "FORGPREP",
}


def normalise(ch: Chunk, p: Proto, child_pos):
    """Fill `p.stream` (ordered, labelled) and `p.sig` (Counter of semantic ops).

    `child_pos(proto_id)` maps a child prototype id to a stable position token.
    """
    insns = p.insns
    pcs = {ins[0]: i for i, ins in enumerate(insns)}
    # collect jump targets
    targets = set()
    for pc, op, a, b, c, d, e, aux in insns:
        name = OPCODES[op]
        if name in ("JUMP", "JUMPBACK", "JUMPIF", "JUMPIFNOT", "JUMPIFEQ", "JUMPIFLE", "JUMPIFLT",
                    "JUMPIFNOTEQ", "JUMPIFNOTLE", "JUMPIFNOTLT", "FORNPREP", "FORNLOOP", "FORGLOOP",
                    "FORGPREP_INEXT", "FORGPREP_NEXT", "FORGPREP", "JUMPXEQKNIL", "JUMPXEQKB",
                    "JUMPXEQKN", "JUMPXEQKS"):
            targets.add(pc + 1 + d)
        elif name == "JUMPX":
            targets.add(pc + 1 + e)
        elif name == "LOADB" and c:
            targets.add(pc + 1 + c)
    labels = {t: i for i, t in enumerate(sorted(targets))}

    stream = []
    sig_tokens = []
    K = lambda i: const_repr(ch, p, i)
    builtin_call_pc = None  # set while a FASTCALL* waits for its paired CALL
    for pc, op, a, b, c, d, e, aux in insns:
        if pc in labels:
            stream.append(f"L{labels[pc]}:")
        name = OPCODES[op]
        target = None
        ops = ()
        if name in ("LOADK",):
            ops = (K(d),)
        elif name == "LOADKX":
            ops = (K(aux),)
        elif name == "LOADN":
            ops = (str(d),)
        elif name == "LOADB":
            ops = ("true" if b else "false",)
            if c:
                target = pc + 1 + c
        elif name in ("GETGLOBAL", "SETGLOBAL", "GETTABLEKS", "SETTABLEKS", "NAMECALL",
                      "GETUDATAKS", "SETUDATAKS", "NAMECALLUDATA"):
            ops = (K(aux),)
        elif name == "GETIMPORT":
            ops = (K(d),)
        elif name in ("GETTABLEN", "SETTABLEN"):
            ops = (str(c + 1),)
        elif name in ("ADDK", "SUBK", "MULK", "DIVK", "MODK", "POWK", "IDIVK", "ANDK", "ORK"):
            ops = (K(c),)
        elif name in ("SUBRK", "DIVRK"):
            ops = (K(b), "rk")
        elif name == "CALL":
            ops = (str(b - 1) if b else "*", str(c - 1) if c else "*")
        elif name == "RETURN":
            ops = (str(b - 1) if b else "*",)
        elif name == "GETVARARGS":
            ops = (str(b - 1) if b else "*",)
        elif name == "NEWTABLE":
            ops = (str(aux),)
        elif name == "DUPTABLE":
            ops = (K(d),)
        elif name == "SETLIST":
            ops = (str(c - 1) if c else "*", str(aux))
        elif name in ("NEWCLOSURE",):
            cid = p.children[d] if d < len(p.children) else -1
            ops = (child_pos(cid),)
        elif name == "DUPCLOSURE":
            k = p.constants[d] if d < len(p.constants) else None
            cid = k[1] if k and k[0] == "closure" else -1
            ops = (child_pos(cid),)
        elif name == "CAPTURE":
            ops = (("val", "ref", "upval")[a] if a < 3 else str(a),)
        elif name == "CONCAT":
            ops = (str(c - b + 1),)
        elif name in ("JUMPXEQKN", "JUMPXEQKS"):
            ops = (K(aux & 0xFFFFFF), "not" if aux >> 31 else "")
            target = pc + 1 + d
        elif name == "JUMPXEQKB":
            ops = ("true" if aux & 1 else "false", "not" if aux >> 31 else "")
            target = pc + 1 + d
        elif name == "JUMPXEQKNIL":
            ops = ("not" if aux >> 31 else "",)
            target = pc + 1 + d
        elif name in ("JUMP", "JUMPBACK", "JUMPIF", "JUMPIFNOT", "JUMPIFEQ", "JUMPIFLE", "JUMPIFLT",
                      "JUMPIFNOTEQ", "JUMPIFNOTLE", "JUMPIFNOTLT", "FORNPREP", "FORNLOOP", "FORGLOOP",
                      "FORGPREP_INEXT", "FORGPREP_NEXT", "FORGPREP"):
            target = pc + 1 + d
            if name == "FORGLOOP":
                ops = (str(aux & 0xFF),)
        elif name == "JUMPX":
            target = pc + 1 + e
        elif name in ("FASTCALL", "FASTCALL1", "FASTCALL2", "FASTCALL2K", "FASTCALL3"):
            ops = (str(c),)
            # the paired CALL is the next CALL instruction
            builtin_call_pc = "next"
        if name == "GETIMPORT":
            # The Roblox compiler and the pinned upstream compiler disagree on
            # which `a.b.c` chains become one import; expand every chain into
            # the root import plus field reads so both sides agree.
            parts = ops[0][1:].split(".")
            expanded = [f'GETIMPORT(@{parts[0]})'] + [f'GETTABLEKS("{f}")' for f in parts[1:]]
            stream.extend(expanded)
            sig_tokens.extend(expanded)
            continue
        item = name + ("(" + ",".join(ops) + ")" if ops else "")
        if target is not None:
            item += f" ->L{labels.get(target, '?')}"
        stream.append(item)
        if op not in _DROP_IN_SIG:
            cname = _CANON.get(name, name)
            if cname in ("CMPK", "CMPKB", "CMPKNIL"):
                ops = tuple(o for o in ops if o not in ("not", ""))
            if cname == "SUB" and ops and ops[-1] == "rk":
                ops = ops[:-1]
            if cname == "CALL":
                # an unused result (`f(x)` vs `local _ = f(x)`) is value-neutral
                ops = ops[:1]
                if builtin_call_pc == "next":
                    # a call the compiler recognised as a builtin (FASTCALL
                    # fast path); the pinned compiler truncates a multret last
                    # argument for fixed-arity builtins, the Roblox one does not
                    cname = "BCALL"
                    builtin_call_pc = None
            if cname == "FORGLOOP":
                # `for k, _ in` vs `for k in`: the unused variable count is value-neutral
                ops = ()
            if cname in ("ADD", "SUB", "MUL", "DIV", "MOD", "POW", "IDIV") and ops:
                # `x * K` (MULK) == LOADK K; MUL
                sig_tokens.append(f"LOADK({ops[0]})")
                ops = ()
            if cname == "DUPTABLE":
                # `{k = v}` (DUPTABLE template, with or without constant values -
                # the upstream compiler folds constants into the template, the
                # Roblox one does not) == `{}` + field stores.
                # Keys without a folded constant are still stored by explicit
                # SETTABLEKS instructions, so only constant entries are expanded.
                sig_tokens.append("NEWTABLE(0)")
                k = p.constants[d] if d < len(p.constants) else None
                if k and k[0] == "tablek":
                    for i, v in k[1]:
                        # a `nil` template value is still stored explicitly
                        if v >= 0 and K(v) != "nil":
                            sig_tokens.append(f"SETTABLEKS({K(i)})")
                            sig_tokens.append(f"LOADK({K(v)})")
                continue
            sig_tokens.append(cname + ("(" + ",".join(ops) + ")" if ops else ""))
    p.stream = stream
    p.sig = collections.Counter(_canonical_sequence(sig_tokens))


# Sequence-level rewrites applied before the multiset is built (both sides).
_SEQ_REWRITES = [
    # CFrame.new() == CFrame.identity (a single value, so `return CFrame.new()` is `return` of 1)
    (["GETIMPORT(@CFrame)", 'GETTABLEKS("new")', "CALL(0)", "RETURN(*)"], ["GETIMPORT(@CFrame)", 'GETTABLEKS("identity")', "RETURN(1)"]),
    (["GETIMPORT(@CFrame)", 'GETTABLEKS("new")', "CALL(0)"], ["GETIMPORT(@CFrame)", 'GETTABLEKS("identity")']),
    # Vector3.zero == folded Vector3.new(0, 0, 0)
    (["GETIMPORT(@Vector3)", 'GETTABLEKS("zero")'], ["LOADK(vec(0,0,0,0))"]),
    (["GETIMPORT(@Vector3)", 'GETTABLEKS("one")'], ["LOADK(vec(1,1,1,0))"]),
    (["GETIMPORT(@Vector3)", 'GETTABLEKS("xAxis")'], ["LOADK(vec(1,0,0,0))"]),
    (["GETIMPORT(@Vector3)", 'GETTABLEKS("yAxis")'], ["LOADK(vec(0,1,0,0))"]),
    (["GETIMPORT(@Vector3)", 'GETTABLEKS("zAxis")'], ["LOADK(vec(0,0,1,0))"]),
    # Vector2.new(0, 0) == Vector2.zero, Vector2.new(1, 1) == Vector2.one
    (["GETIMPORT(@Vector2)", 'GETTABLEKS("new")', "LOADK(0)", "LOADK(0)", "CALL(2)"], ["GETIMPORT(@Vector2)", 'GETTABLEKS("zero")']),
    (["GETIMPORT(@Vector2)", 'GETTABLEKS("new")', "LOADK(1)", "LOADK(1)", "CALL(2)"], ["GETIMPORT(@Vector2)", 'GETTABLEKS("one")']),
]


def _canonical_sequence(tokens):
    out = []
    i = 0
    n = len(tokens)
    while i < n:
        for pat, repl in _SEQ_REWRITES:
            L = len(pat)
            if tokens[i : i + L] == pat:
                out.extend(repl)
                i += L
                break
        else:
            out.append(tokens[i])
            i += 1
    return out


def proto_header(p: Proto) -> str:
    return f"params={p.num_params} vararg={int(p.is_vararg)}"


# --------------------------------------------------------------------------
# Comparison
# --------------------------------------------------------------------------

def _prepare(ch: Chunk):
    """Normalise all protos in tree order; returns list of (path, proto)."""
    order = []

    def walk(pid, path):
        p = ch.protos[pid]
        order.append((path, p))
        for i, cid in enumerate(p.children):
            walk(cid, path + (i,))

    walk(ch.main, ())
    # Child prototypes are compared on their own; the closure site only records
    # that a closure is created (an opaque token keeps de-inlined helpers from
    # shifting every sibling index).
    for _, p in order:
        normalise(ch, p, lambda cid: "f")
    return order


# Inlining specialises constants (parameter substitution) and re-plumbs
# upvalues, so those tokens do not decide whether a body "lives elsewhere".
_CONTAINMENT_IGNORED = {"LOADK", "LOADNIL", "LOADB", "GETUPVAL", "SETUPVAL", "CAPTURE", "NEWCLOSURE", "CALL", "BCALL"}


def _remap_subtrees(n_by_path, remaps):
    """Re-key permuted sibling subtrees so descendants are addressed by the
    original-side path."""
    moved = {}
    for src, dst in remaps:
        for path in [p for p in n_by_path if p[:len(src)] == src]:
            moved[dst + path[len(src):]] = n_by_path.pop(path)
    n_by_path.update(moved)


def _similarity(a: collections.Counter, b: collections.Counter) -> float:
    if not a and not b:
        return 1.0
    inter = sum((a & b).values())
    union = sum((a | b).values())
    return inter / union if union else 0.0


def compare_chunks(orig: Chunk, new: Chunk):
    """Return per-proto results and a match summary."""
    o_list = _prepare(orig)
    n_list = _prepare(new)
    o_by_path = {path: p for path, p in o_list}
    n_by_path = {path: p for path, p in n_list}

    pairs = []
    used_n = set()
    matched_o = set()
    # 1. children of a matched parent with the same child count: pair by
    #    identical fingerprint first (table fields / sibling closures may be
    #    emitted in a different order), then by position, then by similarity
    pairs.append(((), o_by_path[()], n_by_path[()]))
    used_n.add(id(n_by_path[()]))
    matched_o.add(id(o_by_path[()]))
    queue = [()]
    while queue:
        parent = queue.pop(0)
        po = o_by_path.get(parent)
        pn = n_by_path.get(parent)
        if po is None or pn is None or len(po.children) != len(pn.children):
            continue
        n_kids = list(range(len(pn.children)))
        o_kids = list(range(len(po.children)))
        kid_pairs = []
        for oi in list(o_kids):
            okid = o_by_path[parent + (oi,)]
            for ni in n_kids:
                nkid = n_by_path[parent + (ni,)]
                if okid.sig == nkid.sig and proto_header(okid) == proto_header(nkid):
                    kid_pairs.append((oi, ni))
                    n_kids.remove(ni)
                    o_kids.remove(oi)
                    break
        # remaining siblings: best mutual similarity first (a permuted table of
        # closures), falling back to the same position
        cands = sorted(
            ((_similarity(o_by_path[parent + (oi,)].sig, n_by_path[parent + (ni,)].sig), oi, ni)
             for oi in o_kids for ni in n_kids),
            key=lambda t: (-t[0], t[1] != t[2], t[1], t[2]),
        )
        for sim, oi, ni in cands:
            if oi in o_kids and ni in n_kids and (sim >= 0.5 or oi == ni):
                kid_pairs.append((oi, ni))
                o_kids.remove(oi)
                n_kids.remove(ni)
        for oi in list(o_kids):
            if oi in n_kids:
                kid_pairs.append((oi, oi))
                n_kids.remove(oi)
                o_kids.remove(oi)
        for oi in list(o_kids):
            if n_kids:
                ni = n_kids.pop(0)
                kid_pairs.append((oi, ni))
                o_kids.remove(oi)
        remaps = []
        for oi, ni in kid_pairs:
            opath = parent + (oi,)
            okid = o_by_path[opath]
            nkid = n_by_path[parent + (ni,)]
            pairs.append((opath, okid, nkid))
            used_n.add(id(nkid))
            matched_o.add(id(okid))
            if oi != ni:
                remaps.append((parent + (ni,), opath))
            queue.append(opath)
        if remaps:
            _remap_subtrees(n_by_path, remaps)
    # 2. fingerprint match for the rest (de-inline / closure re-shaping)
    rest_o = [(path, p) for path, p in o_list if id(p) not in matched_o]
    rest_n = [(path, p) for path, p in n_list if id(p) not in used_n]
    for path, op_ in rest_o:
        best = None
        best_s = 0.0
        for npath, np_ in rest_n:
            if id(np_) in used_n:
                continue
            s = _similarity(op_.sig, np_.sig)
            if s > best_s:
                best_s, best = s, (npath, np_)
        if best is not None and best_s >= 0.5:
            pairs.append((path, op_, best[1]))
            used_n.add(id(best[1]))
            matched_o.add(id(op_))
    missing = [path for path, p in o_list if id(p) not in matched_o]
    extra = [path for path, p in n_list if id(p) not in used_n]

    o_union = collections.Counter()
    n_union = collections.Counter()
    for _, p in o_list:
        o_union.update(p.sig)
    for _, p in n_list:
        n_union.update(p.sig)

    results = []
    for path, op_, np_ in pairs:
        if op_.stream == np_.stream and proto_header(op_) == proto_header(np_):
            tier = "exact"
            delta = None
        elif op_.sig == np_.sig and proto_header(op_) == proto_header(np_):
            tier = "equiv"
            delta = None
        else:
            tier = "differ"
            lost = op_.sig - np_.sig
            added = np_.sig - op_.sig
            # Does the lost code live in another recompiled proto (outlined
            # into a helper), or does the added code come from another original
            # proto (helper inlined by the recompile)?
            others_n = n_union - np_.sig
            others_o = o_union - op_.sig
            delta = {
                "lost": dict(lost.most_common()),
                "added": dict(added.most_common()),
                "common": dict((op_.sig & np_.sig).most_common()),
                "lost_elsewhere": bool(lost) and all(k in others_n for k in lost if _base(k) not in _CONTAINMENT_IGNORED),
                "added_elsewhere": bool(added) and all(k in others_o for k in added if _base(k) not in _CONTAINMENT_IGNORED),
                "header": None if proto_header(op_) == proto_header(np_) else [proto_header(op_), proto_header(np_)],
            }
        results.append({
            "path": ".".join(map(str, path)) or "main",
            "tier": tier,
            "insns": len(op_.insns),
            "delta": delta,
        })
    return results, [".".join(map(str, m)) or "main" for m in missing], [".".join(map(str, m)) or "main" for m in extra]


# --------------------------------------------------------------------------
# Triage tags for `differ` prototypes
# --------------------------------------------------------------------------

_FAMILY = {}
for _n in ("LOADNIL", "LOADB", "LOADN", "LOADK", "NOT", "AND", "OR", "MINUS", "LENGTH", "CONCAT",
           "GETUPVAL", "SETUPVAL", "CAPTURE", "GETVARARGS", "NEWTABLE", "DUPTABLE", "SETLIST"):
    _FAMILY[_n] = _n
for _n in ("ADD", "SUB", "MUL", "DIV", "MOD", "POW", "IDIV"):
    _FAMILY[_n] = "ARITH"
for _n in ("TEST", "CMPEQ", "CMPLE", "CMPLT", "CMPK", "CMPKB", "CMPKNIL"):
    _FAMILY[_n] = "CMP"
for _n in ("GETTABLEKS", "SETTABLEKS", "GETTABLE", "SETTABLE", "GETTABLEN", "SETTABLEN",
           "GETGLOBAL", "SETGLOBAL", "GETIMPORT", "GETUDATAKS", "SETUDATAKS"):
    _FAMILY[_n] = "TABLE"
for _n in ("CALL", "NAMECALL", "NAMECALLUDATA", "RETURN", "NEWCLOSURE", "FORNPREP", "FORNLOOP",
           "FORGLOOP", "FORGPREP"):
    _FAMILY[_n] = _n


def _fam(sig: str) -> str:
    op = sig.split("(", 1)[0]
    return _FAMILY.get(op, op)


def _base(sig: str) -> str:
    return sig.split("(", 1)[0]


# Known value-neutral rewrites: (lost bag, added bag).  Each rule is cancelled
# as many times as it fits before the families are examined.
_NEUTRAL_REWRITES = [
    # CFrame.new() == CFrame.identity (decompiler readability rewrite)
    ({'GETIMPORT(@CFrame)': 1, 'GETTABLEKS("new")': 1, 'CALL(0)': 1},
     {'GETIMPORT(@CFrame)': 1, 'GETTABLEKS("identity")': 1}),
    ({'GETIMPORT(@CFrame)': 1, 'GETTABLEKS("identity")': 1},
     {'GETIMPORT(@CFrame)': 1, 'GETTABLEKS("new")': 1, 'CALL(0)': 1}),
    # Vector3.new(0,0,0) folded to a vector constant == Vector3.zero
    ({'LOADK(vec(0,0,0,0))': 1}, {'GETIMPORT(@Vector3)': 1, 'GETTABLEKS("zero")': 1}),
    ({'GETIMPORT(@Vector3)': 1, 'GETTABLEKS("zero")': 1}, {'LOADK(vec(0,0,0,0))': 1}),
    # an explicit trailing `return` added or dropped by the structurer
    ({}, {'RETURN(0)': 1}),
    ({'RETURN(0)': 1}, {}),
]
_ADDED_ONLY_NEUTRAL_BASES = {"RETURN"}  # shared-tail duplication (`return x` copied into both arms)


def _cancel_neutral_rewrites(lost: collections.Counter, added: collections.Counter) -> None:
    for l_bag, a_bag in _NEUTRAL_REWRITES:
        while True:
            n_l = min((lost[k] // v for k, v in l_bag.items()), default=None)
            n_a = min((added[k] // v for k, v in a_bag.items()), default=None)
            n = min(x for x in (n_l, n_a) if x is not None)
            if n <= 0:
                break
            for k, v in l_bag.items():
                lost[k] -= v * n
            for k, v in a_bag.items():
                added[k] -= v * n
            if not l_bag or not a_bag:
                break
    for k in [k for k in added if _base(k) in _ADDED_ONLY_NEUTRAL_BASES]:
        del added[k]
    # builtin fast path: multret last argument truncated to the builtin's arity
    # (pinned compiler) vs kept (Roblox compiler); alias `local clamp = math.clamp`
    # turns BCALL into a plain CALL of the same arity
    for src, dst in ((lost, added), (added, lost)):
        while src["BCALL(*)"] > 0:
            cand = [k for k in dst if k.startswith("BCALL(") and k != "BCALL(*)" and dst[k] > 0]
            if not cand:
                break
            src["BCALL(*)"] -= 1
            dst[cand[0]] -= 1
        for k in [k for k in src if k.startswith("BCALL(")]:
            plain = "CALL" + k[len("BCALL"):]
            n = min(src[k], dst[plain])
            if n > 0:
                src[k] -= n
                dst[plain] -= n
    # decompiler fallback for an unfoldable multret SETLIST tail:
    #   `for _k, _v in next, { f() } do t[n + _k] = _v end`
    # (`next` may be read through an upvalue alias instead of an import)
    n = min(added.get(k, 0) for k in ("FORGPREP", "FORGLOOP", "SETTABLE"))
    n = min(n, added.get("GETIMPORT(@next)", 0) + added.get("GETUPVAL", 0))
    if n > 0:
        for k in ("FORGPREP", "FORGLOOP", "SETTABLE"):
            added[k] -= n
        via_import = min(n, added.get("GETIMPORT(@next)", 0))
        added["GETIMPORT(@next)"] -= via_import
        added["GETUPVAL"] -= n - via_import
        for k in ("LOADNIL", "ADD", "SETLIST(*,1)"):
            added[k] -= min(n, added.get(k, 0))
        for k in [k for k in lost if _base(k) == "SETLIST"]:
            del lost[k]
    # `local KEY = "x" ... t[KEY]`: the pinned compiler folds the constant
    # upvalue into GETTABLEKS/SETTABLEKS, the Roblox one reads the upvalue
    for src, dst, get, set_ in ((lost, added, "GETTABLE", "SETTABLE"), (added, lost, "GETTABLE", "SETTABLE")):
        for plain, keyed in ((get, "GETTABLEKS"), (set_, "SETTABLEKS")):
            n = min(src.get(plain, 0), src.get("GETUPVAL", 0), sum(v for k, v in dst.items() if _base(k) == keyed))
            if n > 0:
                src[plain] -= n
                src["GETUPVAL"] -= n
                for k in [k for k in dst if _base(k) == keyed]:
                    take = min(n, dst[k])
                    dst[k] -= take
                    n -= take
    # `{k = nil}`: the Roblox compiler only pre-shapes the key, the pinned
    # compiler also stores nil explicitly
    if lost.get("LOADNIL", 0) == 0:
        nil_fields = [k for k in added if _base(k) == "SETTABLEKS"]
        n = sum(added[k] for k in nil_fields)
        if n and added.get("LOADNIL", 0) >= n and not lost:
            added["LOADNIL"] -= n
            for k in nil_fields:
                del added[k]
    # `{a, b, c}` (SETLIST n) == `t[1], t[2], t[3] = a, b, c` (SETTABLEN 1..n)
    for src, dst in ((lost, added), (added, lost)):
        for k in [k for k in src if k.startswith("SETLIST(") and k.endswith(",1)")]:
            n = k[len("SETLIST("):-3]
            if not n.isdigit():
                continue
            n = int(n)
            while src[k] > 0 and all(dst[f"SETTABLEN({i})"] > 0 for i in range(1, n + 1)):
                src[k] -= 1
                for i in range(1, n + 1):
                    dst[f"SETTABLEN({i})"] -= 1
    for c in (lost, added):
        for k in [k for k, v in c.items() if v <= 0]:
            del c[k]


_CONST_OPS = {"LOADN", "LOADK", "LOADNIL", "LOADB"}


def _is_const_fold(lost: collections.Counter, added: collections.Counter) -> bool:
    """`0 + 7 + 7` recompiled as `21`: arithmetic on constants disappears."""
    lost_ops = {_base(s) for s in lost}
    added_ops = {_base(s) for s in added}
    return (
        lost_ops <= _CONST_OPS | {"ADD", "SUB", "MUL", "DIV", "MOD", "POW", "IDIV", "MINUS", "NOT", "CONCAT", "LENGTH"}
        and added_ops <= _CONST_OPS
    )


def classify_delta(delta) -> str:
    """Map a `differ` delta to a triage class.

    * ``accept``      - only value-neutral shape changes (constant materialisation,
                        boolean materialisation, `and`/`or` <-> branch, table
                        constructor shape, upvalue/capture re-plumbing);
    * ``reduced``     - every lost instruction is still present, just fewer times
                        (de-inline dedup, shared-tail merge, compound assignment);
    * ``duplicated``  - every added instruction was already present (shared-tail
                        duplication, helper re-inlined at more sites);
    * ``outlined``/``inlined`` - the lost/added code lives in another prototype
                        (de-inlined helper, or a local function the recompile
                        inlined);
    * ``dropped-const-table`` - a constant-only table constructor vanished
                        (dead `local t = {...}` dropped: fidelity, not behaviour);
    * ``investigate`` - semantic families changed in count but every constant
                        and call target is still present on both sides;
    * ``suspect``     - a call, table access, or constant present on one side
                        is absent on the other (candidate real bug).
    """
    if delta is None:
        return "accept"
    if delta.get("header"):
        return "suspect"
    lost = collections.Counter(delta["lost"])
    added = collections.Counter(delta["added"])
    _cancel_neutral_rewrites(lost, added)
    delta["lost"] = dict(lost.most_common())
    delta["added"] = dict(added.most_common())
    if not lost and not added:
        return "accept"
    lost_f = collections.Counter(_fam(s) for s in lost)
    added_f = collections.Counter(_fam(s) for s in added)
    neutral = {"LOADNIL", "LOADB", "LOADN", "LOADK", "NOT", "AND", "OR", "CAPTURE", "GETUPVAL",
               "SETUPVAL", "CMP", "MINUS", "NEWTABLE", "DUPTABLE", "SETLIST", "GETVARARGS"}
    if set(lost_f) <= neutral and set(added_f) <= neutral:
        return "accept"
    if _is_const_fold(lost, added):
        return "accept"
    common = delta.get("common", {})
    neutral_added = set(added_f) <= neutral
    neutral_lost = set(lost_f) <= neutral
    if neutral_added and all(k in common for k in lost):
        # every lost instruction still occurs in the recompiled proto, only fewer
        # times: de-inline dedup, shared-tail merge, `t[k].x = t[k].x + 1` -> `+=`
        return "reduced"
    if neutral_lost and all(k in common for k in added):
        # every added instruction already occurs in the original: shared-tail
        # duplication / helper re-inlined at more sites
        return "duplicated"
    calls = ("CALL", "BCALL", "NEWCLOSURE", "CAPTURE")
    lost_call = any(_base(k) in calls for k in lost)
    added_call = any(_base(k) in calls for k in added)
    if delta.get("lost_elsewhere") and (neutral_added or (added_call and all(_base(k) in neutral or _base(k) in calls for k in added))):
        # the lost instructions all exist in other recompiled protos: the
        # decompiler outlined an inlined body into a helper
        return "outlined"
    lookup = ("GETTABLEKS", "GETTABLE", "GETIMPORT")
    if delta.get("added_elsewhere") and (neutral_lost or (lost_call and all(_base(k) in neutral or _base(k) in calls or _base(k) in lookup for k in lost))):
        # the added instructions all exist in other original protos: the
        # recompile inlined a decompiler-emitted local function
        return "inlined"
    if neutral_added and {_base(k) for k in lost} <= {"NEWTABLE", "SETTABLEKS", "SETLIST", "SETTABLEN", "LOADK", "LOADB", "LOADNIL", "NEWCLOSURE", "CAPTURE"}:
        # a constant table constructor vanished: usually a dead `local t = {...}`
        # the decompiler dropped (fidelity loss, not a behaviour change)
        return "dropped-const-table"
    # every base op with a constant on the lost side must still exist on the new side in
    # some form (e.g. GETTABLEKS("x") lost 2, still 1 present) -> count change only
    if set(lost) <= {"RETURN(*)"} and all(_base(k) == "RETURN" for k in added):
        # `return f(x)` recompiled as RETURN 1: the pinned compiler infers the
        # result count of a known callee (function in a never-mutated local
        # table); indistinguishable from a real multret truncation, so keep it
        # visible for manual triage instead of accepting it
        return "investigate"
    # A named token (call target, field, import, loop, return) that exists on
    # one side only is the signature of dropped or invented code.
    named = ("CALL", "NAMECALL", "GETTABLEKS", "SETTABLEKS", "GETIMPORT", "GETGLOBAL", "SETGLOBAL",
             "GETTABLE", "SETTABLE", "NEWCLOSURE", "FORGPREP", "FORNPREP", "RETURN", "CONCAT", "LENGTH")
    lost_named = {s for s in lost if _base(s) in named and s not in common}
    added_named = {s for s in added if _base(s) in named and s not in common}
    if not lost_named and not added_named:
        return "investigate"
    return "suspect"


# --------------------------------------------------------------------------
# Pipeline
# --------------------------------------------------------------------------

def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, **kw)


def read_saved_bytecode(path: pathlib.Path) -> bytes | None:
    text = path.read_text(encoding="utf-8", errors="replace")
    body = "".join(line.strip() for line in text.splitlines() if not line.lstrip().startswith("--"))
    if not body:
        return None
    try:
        return base64.b64decode(body, validate=False)
    except Exception:
        return None


_short_counter = [0]


def compile_source(compiler: str, src: pathlib.Path, opt: str = "2", short_dir: pathlib.Path | None = None) -> tuple[bytes | None, str]:
    if short_dir is not None:
        # Deep corpus trees exceed MAX_PATH on Windows; compile a short-named copy.
        _short_counter[0] += 1
        tmp = short_dir / f"{_short_counter[0]}.luau"
        tmp.write_bytes(src.read_bytes())
        src = tmp
    r = run([compiler, "--binary", f"-O{opt}", "-g1", "--fflags=false",
             "--vector-lib=Vector3", "--vector-ctor=new", "--vector-type=Vector3", str(src)])
    if r.returncode != 0 or not r.stdout:
        return None, r.stderr.decode("utf-8", "replace")[:400]
    return r.stdout, ""


_TOKEN_RE = re.compile(r'"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'|\[\[.*?\]\]|--[^\n]*|\d+\.?\d*(?:[eE][+-]?\d+)?|[A-Za-z_]\w*|[^\sA-Za-z_0-9]+', re.S)
_KEYWORDS = set("and break do else elseif end false for function if in local nil not or repeat return then true until while continue".split())


def source_tokens(text: str):
    out = []
    for m in _TOKEN_RE.finditer(text):
        t = m.group(0)
        if t.startswith("--"):
            continue
        if t[0] == '"' or t[0] == "'" or t.startswith("[["):
            out.append("S" + t.strip('"\'[]'))
        elif t[0].isdigit():
            out.append("N" + t)
        elif t[0].isalpha() or t[0] == "_":
            out.append(t if t in _KEYWORDS else "ID")
        else:
            out.append(t)
    return out


def source_likeness(a: str, b: str) -> float:
    ta, tb = source_tokens(a), source_tokens(b)
    if not ta and not tb:
        return 1.0
    return difflib.SequenceMatcher(None, ta, tb, autojunk=False).ratio()


def process_file(args, rel: str, orig_raw: bytes, key: int, decompiled: pathlib.Path):
    """Compare one input; returns a result dict."""
    res = {"file": rel, "status": "ok", "protos": 0, "exact": 0, "equiv": 0, "differ": 0,
           "missing": [], "extra": [], "differs": [], "tags": []}
    try:
        orig = parse_chunk(orig_raw, key)
    except BytecodeError as e:
        res["status"] = "orig-parse-fail"
        res["error"] = str(e)
        return res
    res["protos"] = len(orig.protos)
    if not decompiled.exists():
        res["status"] = "no-output"
        return res
    text = decompiled.read_text(encoding="utf-8", errors="replace")
    for marker in ("controlFlowState", "GenericForInit", "GenericForNext", "NumForInit", "goto "):
        if marker in text:
            res["status"] = "marker:" + marker.strip()
            return res
    new_raw, err = compile_source(args.compiler, decompiled, short_dir=args._short_dir)
    if new_raw is None:
        res["status"] = "recompile-fail"
        res["error"] = err
        return res
    try:
        new = parse_chunk(new_raw, 1)
    except BytecodeError as e:
        res["status"] = "new-parse-fail"
        res["error"] = str(e)
        return res
    results, missing, extra = compare_chunks(orig, new)
    res["orig_protos"] = len(orig.protos)
    res["new_protos"] = len(new.protos)
    for r in results:
        res[r["tier"]] += 1
        if r["tier"] == "differ":
            r["raw_delta"] = json.loads(json.dumps(r["delta"]))
            r["class"] = classify_delta(r["delta"])
            res["differs"].append(r)
    res["missing"] = missing
    res["extra"] = extra
    tags = set()
    if "-- inlined by Luau -O2" in text:
        tags.add("deinline")
    if len(new.protos) != len(orig.protos):
        tags.add("proto-count")
    res["tags"] = sorted(tags)
    res["tags"] = _file_tags(res)
    res["nonequiv"] = res["differ"] + len(missing) + len(extra)
    return res


def collect_inputs(args, work: pathlib.Path):
    """Return (input_dir, key, [(rel, orig_bytes, source_text|None)])."""
    if args.sources:
        src_root = pathlib.Path(args.sources)
        inp = work / "in"
        inp.mkdir(parents=True, exist_ok=True)
        items = []
        for src in sorted(src_root.rglob("*.luau")):
            rel = src.relative_to(src_root).with_suffix("").as_posix()
            raw, err = compile_source(args.compiler, src, args.source_opt)
            if raw is None:
                print(f"[COMPILE FAIL] {rel}: {err}")
                continue
            target = inp / (rel + ".lua")
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(base64.b64encode(raw).decode())
            items.append((rel, raw, src.read_text(encoding="utf-8", errors="replace")))
        return inp, 1, items
    root = pathlib.Path(args.corpus)
    items = []
    for src in sorted(root.rglob("*.lua")):
        rel = src.relative_to(root).with_suffix("").as_posix()
        raw = read_saved_bytecode(src)
        if raw is None:
            continue
        items.append((rel, raw, None))
    return root, args.key, items


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lifter", required=True)
    ap.add_argument("--compiler", required=True, help="official luau-compile")
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--corpus", help="folder of saved-bytecode .lua files")
    g.add_argument("--sources", help="folder of ground-truth .luau sources (compiled at --source-opt)")
    ap.add_argument("--source-opt", default="2", choices=["0", "1", "2"])
    ap.add_argument("--key", type=int, default=203, help="opcode key for --corpus inputs (203 Roblox, 1 plain)")
    ap.add_argument("--threads", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--work", help="keep the work tree here")
    ap.add_argument("--report", help="write the JSON report here")
    ap.add_argument("--markdown", help="write a Markdown summary here")
    ap.add_argument("--baseline", help="baseline JSON (full report or --write-baseline output) to gate against: "
                                       "no input may regress in status and no input may gain non-equivalent protos")
    ap.add_argument("--write-baseline", help="write a compact per-file baseline {file: {status, nonequiv, protos}} here")
    ap.add_argument("--min-equiv", type=float, default=None, help="fail when exact+equiv ratio is below this")
    ap.add_argument("--limit", type=int, default=0, help="only process the first N inputs (debug)")
    ap.add_argument("--filter", help="substring filter on input path (debug)")
    ap.add_argument("--reclassify", help="re-run triage on an existing JSON report (no decompile/recompile)")
    args = ap.parse_args()
    if args.reclassify:
        return reclassify(args)
    args.lifter = str(pathlib.Path(args.lifter).resolve())
    args.compiler = str(pathlib.Path(args.compiler).resolve())

    work = pathlib.Path(args.work) if args.work else pathlib.Path(tempfile.mkdtemp(prefix="tovek_bc_rt_"))
    if args.work and work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True, exist_ok=True)

    t0 = time.time()
    inp, key, items = collect_inputs(args, work)
    if args.filter:
        items = [it for it in items if args.filter in it[0]]
    if args.limit:
        items = items[: args.limit]
    if not items:
        print("no inputs")
        return 1

    args._short_dir = work / "cc"
    args._short_dir.mkdir(parents=True, exist_ok=True)
    out = work / "out"
    dec = run([args.lifter, "decompile-folder", str(inp), str(out), "--key", str(key),
               "--threads", str(args.threads), "--strict-no-synthetic-control", "--verbose"])
    dec_log = (dec.stdout + dec.stderr).decode("utf-8", "replace")
    fails = [l for l in dec_log.splitlines() if l.startswith("FAIL")]
    for l in fails[:50]:
        print(l)
    t1 = time.time()

    results = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.threads) as ex:
        futs = {}
        for rel, raw, src_text in items:
            decompiled = out / (rel + ".luau")
            futs[ex.submit(process_file, args, rel, raw, key, decompiled)] = (rel, src_text, decompiled)
        for fut in concurrent.futures.as_completed(futs):
            rel, src_text, decompiled = futs[fut]
            r = fut.result()
            if src_text is not None and decompiled.exists():
                r["source_likeness"] = round(source_likeness(src_text, decompiled.read_text(encoding="utf-8", errors="replace")), 4)
            results.append(r)
    results.sort(key=lambda r: r["file"])
    t2 = time.time()

    # ---- summary
    tot = collections.Counter()
    status = collections.Counter(r["status"] for r in results)
    class_count = collections.Counter()
    tag_count = collections.Counter()
    for r in results:
        if r["status"] != "ok":
            continue
        tot["protos"] += r["exact"] + r["equiv"] + r["differ"] + len(r["missing"]) + len(r["extra"])
        tot["exact"] += r["exact"]
        tot["equiv"] += r["equiv"]
        tot["differ"] += r["differ"]
        tot["missing"] += len(r["missing"])
        tot["extra"] += len(r["extra"])
        for d in r["differs"]:
            class_count[d["class"]] += 1
        for t in r["tags"]:
            tag_count[t] += 1
        if r["nonequiv"] == 0:
            tot["files_equiv"] += 1
    n_ok = status["ok"]
    protos = tot["protos"] or 1
    equiv_ratio = (tot["exact"] + tot["equiv"]) / protos
    summary = {
        "inputs": len(results),
        "status": dict(status),
        "protos": tot["protos"],
        "exact": tot["exact"],
        "equiv": tot["equiv"],
        "differ": tot["differ"],
        "missing": tot["missing"],
        "extra": tot["extra"],
        "equiv_ratio": round(equiv_ratio, 5),
        "files_fully_equiv": tot["files_equiv"],
        "differ_classes": dict(class_count),
        "tags": dict(tag_count),
        "decompile_fail_lines": len(fails),
        "seconds": {"decompile": round(t1 - t0, 1), "compare": round(t2 - t1, 1)},
    }
    if any("source_likeness" in r for r in results):
        vals = [r["source_likeness"] for r in results if "source_likeness" in r]
        summary["source_likeness_mean"] = round(sum(vals) / len(vals), 4)

    print(f"inputs={len(results)} ok={n_ok} status={dict(status)}")
    print(f"protos={tot['protos']} exact={tot['exact']} equiv={tot['equiv']} differ={tot['differ']} "
          f"missing={tot['missing']} extra={tot['extra']}  equiv_ratio={equiv_ratio:.4%}")
    print(f"differ classes={dict(class_count)} tags={dict(tag_count)} files_fully_equiv={tot['files_equiv']}/{n_ok}")
    if "source_likeness_mean" in summary:
        print(f"source likeness (token ratio) mean={summary['source_likeness_mean']}")
    print(f"time: decompile {t1 - t0:.1f}s, recompile+compare {t2 - t1:.1f}s")

    report = {"summary": summary, "files": results}
    if args.write_baseline:
        compact = {
            "summary": {k: summary[k] for k in ("inputs", "protos", "exact", "equiv", "differ", "missing", "extra", "equiv_ratio")},
            "files": [
                {"file": r["file"], "status": r["status"], "nonequiv": r.get("nonequiv", 0),
                 "protos": r.get("protos", 0)}
                for r in results
            ],
        }
        pathlib.Path(args.write_baseline).write_text(json.dumps(compact, indent=1), encoding="utf-8")
    if args.report:
        pathlib.Path(args.report).write_text(json.dumps(report, indent=1), encoding="utf-8")
    if args.markdown:
        write_markdown(pathlib.Path(args.markdown), report)

    rc = 0
    bad_status = sum(v for k, v in status.items() if k != "ok")
    if bad_status:
        print(f"FAIL: {bad_status} inputs did not round-trip (see status)")
        rc = 1
    if args.min_equiv is not None and equiv_ratio < args.min_equiv:
        print(f"FAIL: equiv ratio {equiv_ratio:.4%} < {args.min_equiv:.4%}")
        rc = 1
    if args.baseline:
        base = json.loads(pathlib.Path(args.baseline).read_text(encoding="utf-8"))
        bfiles = {f["file"]: f for f in base["files"]}
        b_nonequiv = sum(f.get("nonequiv", 0) for f in base["files"] if f["status"] == "ok")
        regressions = []
        for r in results:
            b = bfiles.get(r["file"])
            if b is None:
                continue
            if b["status"] == "ok" and r["status"] != "ok":
                regressions.append(f"{r['file']}: status {b['status']} -> {r['status']}")
            elif r["status"] == "ok" and b["status"] == "ok" and r["nonequiv"] > b.get("nonequiv", 0):
                regressions.append(f"{r['file']}: non-equivalent protos {b.get('nonequiv', 0)} -> {r['nonequiv']}")
        cur_nonequiv = tot["differ"] + tot["missing"] + tot["extra"]
        print(f"baseline: non-equivalent protos {b_nonequiv} -> {cur_nonequiv}; per-file regressions={len(regressions)}")
        for line in regressions[:50]:
            print("  REGRESSION " + line)
        if regressions or cur_nonequiv > b_nonequiv:
            rc = 1
    if not args.work:
        shutil.rmtree(work, ignore_errors=True)
    return rc


def _file_tags(r, text_has_marker: bool | None = None):
    tags = set(t for t in r.get("tags", []) if t in ("deinline", "proto-count"))
    classes = {d["class"] for d in r["differs"]}
    for c in ("suspect", "investigate", "dropped-const-table", "duplicated", "reduced", "outlined", "inlined"):
        if c in classes:
            tags.add(c)
            break
    return sorted(tags)


def reclassify(args) -> int:
    report = json.loads(pathlib.Path(args.reclassify).read_text(encoding="utf-8"))
    class_count = collections.Counter()
    tag_count = collections.Counter()
    for r in report["files"]:
        for d in r.get("differs", []):
            raw = d.get("raw_delta", d["delta"])
            d["raw_delta"] = raw
            d["delta"] = json.loads(json.dumps(raw))
            d["class"] = classify_delta(d["delta"])
            class_count[d["class"]] += 1
        if r["status"] == "ok":
            r["tags"] = _file_tags(r)
            for t in r["tags"]:
                tag_count[t] += 1
    report["summary"]["differ_classes"] = dict(class_count)
    report["summary"]["tags"] = dict(tag_count)
    print(f"differ classes={dict(class_count)} tags={dict(tag_count)}")
    out = args.report or args.reclassify
    pathlib.Path(out).write_text(json.dumps(report, indent=1), encoding="utf-8")
    if args.markdown:
        write_markdown(pathlib.Path(args.markdown), report)
    return 0


def write_markdown(path: pathlib.Path, report):
    s = report["summary"]
    lines = ["# Bytecode round-trip report", ""]
    lines.append(f"- inputs: {s['inputs']} (status: {s['status']})")
    lines.append(f"- prototypes: {s['protos']} — exact {s['exact']}, equiv {s['equiv']}, differ {s['differ']}, "
                 f"missing {s['missing']}, extra {s['extra']}")
    lines.append(f"- equivalent ratio: **{s['equiv_ratio']:.2%}**; files fully equivalent: {s['files_fully_equiv']}")
    lines.append(f"- differ classes: {s['differ_classes']}; tags: {s['tags']}")
    if "source_likeness_mean" in s:
        lines.append(f"- source likeness mean: {s['source_likeness_mean']}")
    lines.append("")
    lines.append("## Non-equivalent files")
    lines.append("")
    lines.append("| file | protos | exact | equiv | differ | missing | extra | classes | tags |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|---|---|")
    for f in report["files"]:
        if f["status"] != "ok":
            lines.append(f"| `{f['file']}` | — | — | — | — | — | — | **{f['status']}** | {f.get('error', '')[:80]} |")
            continue
        if f["nonequiv"] == 0:
            continue
        classes = collections.Counter(d["class"] for d in f["differs"])
        lines.append(f"| `{f['file']}` | {f['protos']} | {f['exact']} | {f['equiv']} | {f['differ']} | "
                     f"{len(f['missing'])} | {len(f['extra'])} | {dict(classes)} | {','.join(f['tags'])} |")
    lines.append("")
    lines.append("## Suspect / investigate details")
    lines.append("")
    for f in report["files"]:
        for d in f.get("differs", []):
            if d["class"] in ("accept", "reduced", "duplicated", "outlined", "inlined"):
                continue
            lines.append(f"- `{f['file']}` proto `{d['path']}` ({d['insns']} insns) **{d['class']}**: "
                         f"lost {json.dumps(d['delta']['lost'], ensure_ascii=False)[:300]} / "
                         f"added {json.dumps(d['delta']['added'], ensure_ascii=False)[:300]}"
                         + (f" / header {d['delta']['header']}" if d['delta'].get('header') else ""))
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    sys.exit(main())
