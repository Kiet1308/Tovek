"""Private child-storage comparison after a bounded reference-frame proof.

Reference owners retain their complete storage layout and CFG. Only nonowners
may use private-storage normalization, after proving every possibly open owner
cell stays below descendant call frames. Owners with open packs are excluded:
implicit metamethod calls use the dynamic stack top. Main external upvalues are
also excluded. Debug/stack inspection and resource limits follow Graph's contract.
"""
from __future__ import annotations

import collections
import hashlib

from bytecode_graph import Graph, Unknown


MODEL = "luau-guarded-reference-context-v1"


def reference_graph(chunk, budget):
    from bytecode_roundtrip import OPCODES

    if chunk.protos[chunk.main].num_upvalues:
        raise Unknown("external main upvalue context")
    strict = Graph(chunk, budget)
    bodies, _ = strict.build()
    if not strict.exact_storage:
        raise Unknown("no reference cells to certify")
    owner_count = frame_checks = 0
    for original, body in zip(strict.originals, bodies):
        nodes = body[-1]
        owns_ref = any(name in ("NEWCLOSURE", "DUPCLOSURE") and
                       any(mode == 1 for mode, _ in operands[2])
                       for name, operands, _ in nodes)
        if not owns_ref:
            continue
        owner_count += 1
        # shortcut: reject open-pack owners until implicit call frames are tracked.
        for row in chunk.protos[original].insns:
            strict.spend()
            name = OPCODES[row[1]]
            if ((name == "CALL" and (row[3] == 0 or row[4] == 0)) or
                    (name in ("GETVARARGS", "RETURN") and row[3] == 0) or
                    (name == "SETLIST" and row[4] == 0)):
                raise Unknown("open-pack reference owner has untracked implicit call frame")

        incoming = [set() for _ in nodes]
        outgoing = [set() for _ in nodes]
        predecessors = [[] for _ in nodes]
        for index, (_, _, edges) in enumerate(nodes):
            for target in edges:
                predecessors[target].append(index)
        pending = collections.deque(range(len(nodes)))
        queued = set(pending)
        while pending:
            strict.spend()
            index = pending.popleft()
            queued.remove(index)
            active = set()
            for predecessor in predecessors[index]:
                strict.spend(len(outgoing[predecessor]))
                active.update(outgoing[predecessor])
            incoming[index] = active.copy()
            name, operands, edges = nodes[index]
            if name in ("NEWCLOSURE", "DUPCLOSURE"):
                active.update(source[1] for mode, source in operands[2] if mode == 1)
            elif name == "CLOSEUPVALS":
                active.difference_update(source[1] for source in operands[0])
            if active != outgoing[index]:
                outgoing[index] = active
                for target in edges:
                    if target not in queued:
                        pending.append(target)
                        queued.add(target)

        for index, (name, operands, _) in enumerate(nodes):
            strict.spend()
            active = incoming[index]
            if not active:
                continue
            if name == "CALL":
                base = operands[0][1]
            elif name == "FORGPREP":
                base = operands[0][0][1]
            elif name == "FORGLOOP":
                base = operands[0][0][1] + 3
            elif name.startswith("FASTCALL"):
                raise Unknown("active reference across uncertified fastcall")
            else:
                continue
            frame_checks += 1
            if any(register >= base for register in active):
                raise Unknown("open reference overlaps descendant call frame")

    # The private graph may relax nonowners only after every owner is certified.
    candidate = Graph(chunk, strict.remaining, private_copies=True)
    candidate._reference_context = True
    graph, stats = candidate.build()
    stats.update(reference_owner_contexts=owner_count, reference_frame_checks=frame_checks)
    return graph, stats


def compare_reference_graph(original, rebuilt, *, budget=20000):
    graphs, summaries, reasons = [], [], {}
    for label, chunk in (("original", original), ("rebuilt", rebuilt)):
        try:
            graph, summary = reference_graph(chunk, budget)
            graphs.append(graph)
            summaries.append(summary)
        except (Unknown, RecursionError) as error:
            reasons[label] = str(error) or "recursion budget"
    if reasons:
        return dict(model=MODEL, status="unknown", reasons=reasons)
    result = dict(model=MODEL, status="proved" if graphs[0] == graphs[1] else "unknown",
                  fingerprints=[hashlib.sha256(repr(graph).encode()).hexdigest() for graph in graphs],
                  summaries=summaries)
    if result["status"] == "unknown":
        result["reason"] = "no bisimulation after preserving certified reference-owner storage"
    return result
