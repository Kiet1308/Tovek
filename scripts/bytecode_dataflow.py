"""Bounded, register-independent instruction/dataflow comparison for Luau.

This is deliberately separate from the legacy opcode-multiset oracle. `proved`
means equality of ordered symbolic execution trees in this model, `different`
means different trees (not a concrete runtime counterexample), and `unknown`
means the model or its budget cannot handle the input. Never accept unknown.

The initial domain is acyclic bytecode, including ordered effects, branch
polarity, calls/varargs with result packs, and closures with value captures.
Reference/upvalue captures, CLOSE, loops, fastcalls and unmodelled opcodes refuse.
Debugging, resource exhaustion, stack locations and VM allocation timing are
outside the contract. No arithmetic identities or purity assumptions are used.
"""
from __future__ import annotations

import hashlib
import struct


MODEL = "luau-acyclic-use-def-v1"


class Unknown(Exception):
    pass


def symbolic_tree(chunk, proto, *, budget=20000, depth=0, _remaining=None, _templates=None):
    # Import lazily: bytecode_roundtrip also uses this module for its reports.
    from bytecode_roundtrip import OPCODES, AUX_OPS

    if depth > 32:
        raise Unknown("prototype depth budget")
    remaining = [budget] if _remaining is None else _remaining
    templates = {} if _templates is None else _templates
    instructions = {i[0]: i for i in proto.insns}

    def constant(index, seen=()):
        if index < 0 or index >= len(proto.constants) or index in seen:
            raise Unknown("invalid or recursive constant")
        k = proto.constants[index]
        tag = k[0]
        if tag in ("nil", "bool"):
            return k
        if tag == "num":
            return (tag, struct.pack("<d", k[1]).hex())
        if tag == "str":
            if not 0 < k[1] <= len(chunk.strings):
                raise Unknown("invalid string index")
            return (tag, chunk.strings[k[1] - 1].hex())
        if tag == "vec":
            return (tag, struct.pack("<4f", *k[1]).hex())
        if tag == "import":
            word = k[1]
            count = word >> 30
            if not 1 <= count <= 3:
                raise Unknown("invalid import path")
            return (tag, tuple(constant((word >> (20 - 10 * i)) & 1023,
                                        (*seen, index)) for i in range(count)))
        if tag == "table":
            return (tag, tuple(constant(i, (*seen, index)) for i in k[1]))
        if tag == "tablek":
            return (tag, tuple((constant(i, (*seen, index)),
                                constant(v, (*seen, index)) if v >= 0 else ("nil",))
                               for i, v in k[1]))
        raise Unknown(f"unsupported constant {tag}")

    def number(n):
        return ("num", struct.pack("<d", float(n)).hex())

    def execute(pc, registers, top, seen, event_count):
        events = []

        def read(register):
            if not 0 <= register < proto.max_stack:
                raise Unknown("register outside stack")
            if register in registers:
                return registers[register]
            if top is not None and register >= top[0]:
                return ("result", top[1], register - top[0])
            raise Unknown(f"read before definition at pc {pc}")

        def write(register, value):
            if not 0 <= register < proto.max_stack:
                raise Unknown("register outside stack")
            registers[register] = value

        def event(op, *args):
            value = ("value", event_count + len(events))
            events.append((op, *args))
            return value

        def pack(start, count):
            if count:
                return ("fixed", tuple(read(r) for r in range(start, start + count - 1)))
            if top is None or start > top[0]:
                raise Unknown("unresolved multret top")
            return ("open", tuple(read(r) for r in range(start, top[0])), top[1])

        while True:
            remaining[0] -= 1
            if remaining[0] < 0:
                raise Unknown("instruction/path budget")
            if pc in seen:
                raise Unknown("loop/back edge")
            if pc not in instructions:
                raise Unknown("invalid control-flow target")
            seen = seen | {pc}
            _, op, a, b, c, d, e, aux = instructions[pc]
            name = OPCODES[op]
            next_pc = pc + (2 if op in AUX_OPS else 1)
            if name in ("NOP", "COVERAGE"):
                pass
            elif name == "PREPVARARGS":
                if not proto.is_vararg or a != proto.num_params:
                    raise Unknown("invalid vararg preparation")
            elif name == "LOADNIL":
                write(a, ("nil",))
            elif name == "LOADB":
                write(a, ("bool", bool(b)))
                next_pc = pc + 1 + c
            elif name == "LOADN":
                write(a, number(d))
            elif name in ("LOADK", "LOADKX"):
                write(a, constant(aux if name == "LOADKX" else d))
            elif name == "MOVE":
                write(a, read(b))
            elif name in ("GETGLOBAL", "SETGLOBAL"):
                key = constant(aux)
                if name == "GETGLOBAL":
                    write(a, event(name, key))
                else:
                    event(name, key, read(a))
            elif name in ("GETUPVAL", "SETUPVAL"):
                if b >= proto.num_upvalues:
                    raise Unknown("invalid upvalue slot")
                if name == "GETUPVAL":
                    write(a, event(name, b))
                else:
                    event(name, b, read(a))
            elif name == "GETIMPORT":
                key = constant(d)
                if key != ("import", tuple(constant((aux >> (20 - 10 * i)) & 1023)
                                          for i in range(aux >> 30))):
                    raise Unknown("inconsistent import operands")
                write(a, event(name, key))
            elif name in ("GETTABLE", "GETTABLEKS", "GETTABLEN",
                          "SETTABLE", "SETTABLEKS", "SETTABLEN"):
                table = read(b)
                key = constant(aux) if name.endswith("KS") else (
                    number(c + 1) if name.endswith("N") else read(c))
                if name.startswith("GET"):
                    write(a, event("GETTABLE", table, key))
                else:
                    event("SETTABLE", table, key, read(a))
            elif name == "NAMECALL":
                receiver = read(b)
                write(a, event(name, receiver, constant(aux)))
                write(a + 1, receiver)
            elif name in ("ADD", "SUB", "MUL", "DIV", "MOD", "POW", "IDIV",
                          "AND", "OR", "ADDK", "SUBK", "MULK", "DIVK", "MODK",
                          "POWK", "IDIVK", "ANDK", "ORK", "SUBRK", "DIVRK"):
                if name.endswith("RK"):
                    left, right, operation = constant(b), read(c), name[:-2]
                else:
                    left = read(b)
                    right = constant(c) if name.endswith("K") else read(c)
                    operation = name.removesuffix("K")
                write(a, event(operation, left, right))
            elif name in ("NOT", "MINUS", "LENGTH"):
                write(a, event(name, read(b)))
            elif name == "CONCAT":
                write(a, event(name, tuple(read(r) for r in range(b, c + 1))))
            elif name == "NEWTABLE":
                write(a, event(name, b, aux))
            elif name == "DUPTABLE":
                write(a, event(name, constant(d)))
            elif name == "SETLIST":
                event(name, read(a), aux, pack(b, c))
                if c == 0:
                    top = None
            elif name in ("CALL", "GETVARARGS"):
                if name == "CALL":
                    result = event(name, read(a), pack(a + 1, b), c)
                    count = c
                else:
                    if not proto.is_vararg:
                        raise Unknown("varargs in fixed-arity function")
                    result = event(name, b)
                    count = b
                for register in list(registers):
                    if register >= a:
                        del registers[register]
                top = (a, result) if count == 0 else None
                for i in range(max(0, count - 1)):
                    write(a + i, ("result", result, i))
            elif name in ("NEWCLOSURE", "DUPCLOSURE"):
                if name == "NEWCLOSURE":
                    if not 0 <= d < len(proto.children):
                        raise Unknown("invalid child slot")
                    child_id = proto.children[d]
                else:
                    if not 0 <= d < len(proto.constants) or proto.constants[d][0] != "closure":
                        raise Unknown("invalid closure constant")
                    child_id = proto.constants[d][1]
                if not 0 <= child_id < len(chunk.protos):
                    raise Unknown("invalid child prototype")
                child = chunk.protos[child_id]
                captures = []
                for _ in range(child.num_upvalues):
                    capture = instructions.get(next_pc)
                    if capture is None or OPCODES[capture[1]] != "CAPTURE":
                        raise Unknown("missing capture")
                    if capture[2] != 0:
                        raise Unknown("reference/upvalue capture lifetime")
                    captures.append(read(capture[3]))
                    next_pc += 1
                body = symbolic_tree(chunk, child, depth=depth + 1,
                                     _remaining=remaining, _templates=templates)
                # Preserve sharing of DUPCLOSURE templates even for identical bodies.
                template = templates.setdefault((id(proto), d), len(templates)) if name == "DUPCLOSURE" else None
                write(a, event(name, template, body, tuple(captures)))
            elif name in ("JUMP", "JUMPX"):
                next_pc = pc + 1 + (e if name == "JUMPX" else d)
            elif name in ("JUMPIF", "JUMPIFNOT", "JUMPIFEQ", "JUMPIFNOTEQ",
                          "JUMPIFLE", "JUMPIFNOTLE", "JUMPIFLT", "JUMPIFNOTLT",
                          "JUMPXEQKNIL", "JUMPXEQKB", "JUMPXEQKN", "JUMPXEQKS"):
                invert = "NOT" in name
                if name in ("JUMPIF", "JUMPIFNOT"):
                    condition = ("truthy", read(a))
                elif name.startswith("JUMPX"):
                    invert = bool(aux >> 31)
                    kind = name.removeprefix("JUMPXEQK")
                    rhs = ("nil",) if kind == "NIL" else (
                        ("bool", bool(aux & 1)) if kind == "B" else constant(aux & 0xffffff))
                    condition = ("EQ", read(a), rhs)
                else:
                    condition = (name.removeprefix("JUMPIF").removeprefix("NOT"),
                                 read(a), read(aux))
                yes, no = pc + 1 + d, next_pc
                if invert:
                    yes, no = no, yes
                offset = event_count + len(events)
                branches = tuple(execute(target, registers.copy(), top, seen, offset)
                                 for target in (yes, no))
                return (tuple(events), ("branch", condition, *branches))
            elif name == "RETURN":
                return (tuple(events), ("return", pack(a, b)))
            else:
                raise Unknown(f"unsupported {name} at pc {pc}")
            pc = next_pc

    registers = {i: ("parameter", i) for i in range(proto.num_params)}
    return (proto.num_params, proto.num_upvalues, proto.is_vararg,
            execute(0, registers, None, frozenset(), 0))


def compare_acyclic(original, rebuilt, *, budget=20000):
    """Compare the reachable chunk, refusing unsupported semantics on either side."""
    trees, reasons = [], {}
    for label, chunk in (("original", original), ("rebuilt", rebuilt)):
        try:
            tree = symbolic_tree(chunk, chunk.protos[chunk.main], budget=budget)
            trees.append(tree)
        except (Unknown, RecursionError) as error:
            reasons[label] = str(error) or "recursion budget"
    result = {"model": MODEL}
    if reasons:
        result.update(status="unknown", reasons=reasons)
    else:
        result.update(status="proved" if trees[0] == trees[1] else "different",
                      fingerprints=[hashlib.sha256(repr(t).encode()).hexdigest() for t in trees])
    return result


def compare_dataflow(original, rebuilt, *, budget=20000):
    """Try symbolic execution, then bounded register/CFG bisimulation.

    The second certificate preserves storage and capture lifetimes exactly;
    its mismatch stays unknown. It never overrides a differing symbolic tree.
    """
    result = compare_acyclic(original, rebuilt, budget=budget)
    if result['status'] != 'unknown':
        return result
    from bytecode_graph import compare_graph
    graph = compare_graph(original, rebuilt, budget=budget)
    if graph['status'] == 'proved':
        return {**graph, 'acyclic_unknown': result['reasons']}
    return {**result, 'graph': graph}
