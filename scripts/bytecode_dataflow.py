"""Bounded, register-independent instruction/dataflow comparison for Luau.

This is deliberately separate from the legacy opcode-multiset oracle. `proved`
means equality of ordered symbolic execution trees in this model, `different`
means different trees (not a concrete runtime counterexample), and `unknown`
means the model or its budget cannot handle the input. Never accept unknown.

The symbolic domain is main-without-upvalues acyclic bytecode, including ordered
effects, branch polarity, calls/varargs with result packs, and value captures.
Forwarded parent-upvalue captures and repeated upvalue reads use a whole-chunk
main-without-upvalues/no-reference-captures proof. The same domain permits
identical truthiness arms to collapse; every observable event or effectful
comparison invalidates read reuse. Fastcalls retain both the native success
path and the complete fallback path; vararg frames require entry preparation.
Reference captures, CLOSE, loops and unmodelled opcodes refuse. Terminal event
segments normalize exact redundant stores to private table slots and move fresh
allocation within the excluded allocation-timing contract; table layout,
escaped stores and nil stores remain.
Implicit callbacks with a pending open result pack refuse because their stack
frames can overwrite otherwise private temporaries above the dynamic top.
Closed-domain NAMECALL is allowed below the pack: its lookup preserves that top.
Debugging, resource exhaustion, stack locations and VM allocation timing are
outside the contract. No arithmetic identities or purity assumptions are used.
"""
from __future__ import annotations

import hashlib
import struct


MODEL = "luau-acyclic-use-def-v3"


class Unknown(Exception):
    pass


def symbolic_tree(chunk, proto, *, budget=20000, depth=0, _remaining=None, _templates=None, _capture_domain=None):
    # Import lazily: bytecode_roundtrip also uses this module for its reports.
    from bytecode_roundtrip import OPCODES, AUX_OPS, OP_INDEX

    if chunk.protos[chunk.main].num_upvalues:
        raise Unknown("external main upvalues require exact storage")
    if depth > 32:
        raise Unknown("prototype depth budget")
    remaining = [budget] if _remaining is None else _remaining
    templates = {} if _templates is None else _templates
    capture_domain = [None] if _capture_domain is None else _capture_domain

    def closed_captures():
        if capture_domain[0] is None:
            capture_domain[0] = chunk.protos[chunk.main].num_upvalues == 0
            for body in chunk.protos:
                remaining[0] -= len(body.insns)
                if remaining[0] < 0:
                    raise Unknown("capture-domain budget")
                if any(insn[1] == OP_INDEX["CAPTURE"] and insn[2] == 1 for insn in body.insns):
                    capture_domain[0] = False
        return capture_domain[0]

    instructions = {i[0]: i for i in proto.insns}
    if proto.is_vararg and (0 not in instructions or
                            OPCODES[instructions[0][1]] != "PREPVARARGS" or
                            instructions[0][2] != proto.num_params):
        raise Unknown("vararg frame requires entry preparation")

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
            return (tag, tuple((constant(i, (*seen, index)), number(0)) for i in k[1]))
        if tag == "tablek":
            # Keep serialized entries/order (including explicit nil), but the
            # VM initializes every negative value index to +0, never nil.
            return (tag, tuple((constant(i, (*seen, index)),
                                constant(v, (*seen, index)) if v >= 0 else number(0))
                               for i, v in k[1]))
        raise Unknown(f"unsupported constant {tag}")

    def number(n):
        return ("num", struct.pack("<d", float(n)).hex())

    def execute(pc, registers, top, seen, event_count, upvalue_reads=None):
        events = []
        upvalue_reads = {} if upvalue_reads is None else upvalue_reads

        def read(register):
            if not 0 <= register < proto.max_stack:
                raise Unknown("register outside stack")
            if register in registers:
                return registers[register]
            if top is not None and register >= top[0]:
                raise Unknown("fixed read from an open result pack")
            raise Unknown(f"read before definition at pc {pc}")

        def write(register, value):
            if not 0 <= register < proto.max_stack:
                raise Unknown("register outside stack")
            if top is not None and register >= top[0]:
                raise Unknown("write into an unresolved open result pack")
            registers[register] = value

        def event(op, *args):
            if top is not None and op not in (
                    "GETUPVAL", "SETUPVAL", "NEWTABLE", "DUPTABLE", "NEWCLOSURE",
                    "DUPCLOSURE", "NOT", "AND", "OR", "CALL", "GETVARARGS", "SETLIST"):
                # NAMECALL's lookup restores the saved dynamic top on every path.
                if op != "NAMECALL" or not closed_captures():
                    raise Unknown("implicit callback with an open frame top")
            if op != "GETUPVAL":
                upvalue_reads.clear()
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
                if not proto.is_vararg or pc != 0 or a != proto.num_params:
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
                    if b not in upvalue_reads or not closed_captures():
                        upvalue_reads[b] = event(name, b)
                    write(a, upvalue_reads[b])
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
                call = instructions.get(next_pc)
                if call is None or call[1] != OP_INDEX["CALL"]:
                    raise Unknown("NAMECALL without CALL")
                if top is not None and max(a + 1, b) >= top[0]:
                    raise Unknown("namecall registers overlap an open result pack")
                receiver = read(b)
                write(a, event(name, receiver, constant(aux)))
                write(a + 1, receiver)
            elif name in ("ADD", "SUB", "MUL", "DIV", "MOD", "POW", "IDIV",
                          "AND", "OR", "ADDK", "SUBK", "MULK", "DIVK", "MODK",
                          "POWK", "IDIVK", "ANDK", "ORK", "SUBRK", "DIVRK"):
                if name in ("SUBRK", "DIVRK"):
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
            elif name in ("FASTCALL", "FASTCALL1", "FASTCALL2", "FASTCALL2K", "FASTCALL3"):
                call_pc = pc + 1 + c
                call = instructions.get(call_pc)
                if call is None or OPCODES[call[1]] != "CALL" or call_pc < next_pc or a == 0:
                    raise Unknown("invalid fastcall fallback")
                call_a, call_b, call_c = call[2:5]
                if not 0 <= call_a < proto.max_stack or call_a + max(0, call_c - 1) > proto.max_stack:
                    raise Unknown("invalid fastcall result registers")
                if name == "FASTCALL":
                    arguments = pack(call_a + 1, call_b)
                elif name == "FASTCALL1":
                    arguments = ("fixed", (read(b),))
                elif name == "FASTCALL2":
                    arguments = ("fixed", (read(b), read(aux)))
                elif name == "FASTCALL2K":
                    arguments = ("fixed", (read(b), constant(aux)))
                else:
                    if aux >> 16:
                        raise Unknown("invalid FASTCALL3 register payload")
                    arguments = ("fixed", (read(b), read(aux & 255), read((aux >> 8) & 255)))
                if top is not None and (name == "FASTCALL3" or (name != "FASTCALL" and call_c != 0)):
                    raise Unknown("fastcall frame aliases an open pack")
                offset = event_count + len(events)
                native_result = ("value", offset)
                native_registers = {r: value for r, value in registers.items() if r < call_a}
                for index in range(max(0, call_c - 1)):
                    native_registers[call_a + index] = ("result", native_result, index)
                native_top = (call_a, native_result) if call_c == 0 else None
                # Retain both VM outcomes; the fallback lookup/call is never erased.
                native_events, native_end = execute(call_pc + 1, native_registers, native_top, seen, offset + 1, {})
                native = ((("FASTCALL_SUCCESS", name, a, arguments, call_b, call_c), *native_events), native_end)
                fallback = execute(next_pc, registers.copy(), top, seen, offset, {})
                return (tuple(events), ("fastcall", name, a, arguments, call_b, call_c, native, fallback))
            elif name in ("CALL", "GETVARARGS"):
                if name == "CALL":
                    result = event(name, read(a), pack(a + 1, b), c)
                    count = c
                else:
                    if not proto.is_vararg:
                        raise Unknown("varargs in fixed-arity function")
                    if b != 0 and top is not None:
                        raise Unknown("fixed varargs preserve an unresolved frame top")
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
                if name == "NEWCLOSURE":
                    # The VM stores the new closure before reading self captures.
                    write(a, ("value", event_count + len(events)))
                captures = []
                for _ in range(child.num_upvalues):
                    capture = instructions.get(next_pc)
                    if capture is None or OPCODES[capture[1]] != "CAPTURE":
                        raise Unknown("missing capture")
                    if capture[2] == 0:
                        if name == "DUPCLOSURE" and capture[3] == a:
                            raise Unknown("shared self-capture identity")
                        captures.append(read(capture[3]))
                    elif capture[2] == 2 and closed_captures():
                        if capture[3] >= proto.num_upvalues:
                            raise Unknown("invalid captured upvalue slot")
                        captures.append(("parent-upvalue", capture[3]))
                    else:
                        raise Unknown("reference/upvalue capture lifetime")
                    next_pc += 1
                body = symbolic_tree(chunk, child, depth=depth + 1,
                                     _remaining=remaining, _templates=templates,
                                     _capture_domain=capture_domain)
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
                if condition[0] != "truthy":
                    if top is not None:
                        raise Unknown("implicit comparison callback with an open frame top")
                    upvalue_reads.clear()
                branches = tuple(execute(target, registers.copy(), top, seen, offset, upvalue_reads.copy())
                                 for target in (yes, no))
                if condition[0] == "truthy" and branches[0] == branches[1] and closed_captures():
                    return (tuple(events) + branches[0][0], branches[0][1])
                return (tuple(events), ("branch", condition, *branches))
            elif name == "RETURN":
                terminal = ("return", pack(a, b))
                if any(event[0] in ("NEWTABLE", "DUPTABLE") for event in events) and closed_captures():
                    from bytecode_private_tables import normalize_private_tables
                    return normalize_private_tables(tuple(events), terminal, event_count, remaining)
                return tuple(events), terminal
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
    """Try symbolic execution, exact CFG, then guarded storage CFGs.

    Graph certificates retain capture lifetimes; their mismatch stays
    unknown. Neither overrides a differing symbolic tree.
    """
    result = compare_acyclic(original, rebuilt, budget=budget)
    if result['status'] != 'unknown':
        return result
    from bytecode_graph import (compare_graph, compare_private_graph,
                                compare_adjacent_pack_graph, compare_closed_read_graph)
    graph = compare_graph(original, rebuilt, budget=budget)
    if graph['status'] == 'proved':
        return {**graph, 'acyclic_unknown': result['reasons']}
    private = compare_private_graph(original, rebuilt, budget=budget)
    if private['status'] == 'proved':
        return {**private, 'acyclic_unknown': result['reasons'], 'structural_unknown': graph}
    from bytecode_reference_graph import compare_reference_graph
    reference = compare_reference_graph(original, rebuilt, budget=budget)
    if reference['status'] == 'proved':
        return {**reference, 'acyclic_unknown': result['reasons'],
                'structural_unknown': graph, 'private_storage_unknown': private}
    adjacent = compare_adjacent_pack_graph(original, rebuilt, budget=budget)
    if adjacent['status'] == 'proved':
        return {**adjacent, 'acyclic_unknown': result['reasons'], 'structural_unknown': graph,
                'private_storage_unknown': private, 'reference_context_unknown': reference}
    closed = compare_closed_read_graph(original, rebuilt, budget=budget)
    if closed['status'] == 'proved':
        return {**closed, 'acyclic_unknown': result['reasons'], 'structural_unknown': graph,
                'private_storage_unknown': private, 'reference_context_unknown': reference,
                'adjacent_packs_unknown': adjacent}
    return {**result, 'graph': graph, 'private_storage': private,
            'reference_context': reference, 'adjacent_packs': adjacent, 'closed_reads': closed}
