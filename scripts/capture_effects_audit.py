#!/usr/bin/env python3
"""Check immutable-slot certificates against independently parsed input bytecode."""
import argparse
import collections
import hashlib
import json
import pathlib

from bytecode_roundtrip import OPCODES, parse_chunk, read_saved_bytecode
from provenance_audit import manifest, sidecar


def verify(proof, chunk):
    if proof.get("schema_version") != 1 or proof.get("model") != "luau-v9-val-upval-immutability-v1":
        raise ValueError("unknown capture proof model")
    if proof["status"] == "refused":
        if proof["readonly_slots"] or proof["analyzed_slots"] is not None or not proof["refusal"]:
            raise ValueError("refusal carries a certificate")
        return dict(analysis_status="refused", readonly_slots=0, unknown_slots=None)
    if proof["status"] != "complete" or proof["refusal"] is not None or chunk is None or chunk.version != 9:
        raise ValueError("invalid completed proof/profile")
    if len(chunk.protos) > 50_000 or sum(len(p.code) for p in chunk.protos) > 1_000_000:
        raise ValueError("proof exceeds input budget")
    nodes = {(p.id, slot) for p in chunk.protos for slot in range(p.num_upvalues)}
    if len(nodes) > 200_000 or proof["analyzed_slots"] != len(nodes):
        raise ValueError("slot count/budget differs")
    claims = set()
    if len(proof["readonly_slots"]) > 50_000:
        raise ValueError("certificate row budget")
    for row in proof["readonly_slots"]:
        if type(row["prototype"]) is not int or len(row["slots"]) > 255:
            raise ValueError("invalid certificate row")
        for slot in row["slots"]:
            if type(slot) is not int:
                raise ValueError("invalid certificate slot")
            key = row["prototype"], slot
            if key not in nodes or key in claims or key[0] == chunk.main:
                raise ValueError("invalid, duplicate or external root certificate")
            claims.add(key)
    incoming = collections.defaultdict(list)
    parents = collections.defaultdict(set)
    writes = set()
    edges = 0
    for proto in chunk.protos:
        index = 0
        while index < len(proto.insns):
            pc, op, a, b, c, d, e, aux = proto.insns[index]
            opcode = OPCODES[op]
            if opcode == "SETUPVAL":
                target = proto.id, b
                if target not in nodes:
                    raise ValueError("invalid input write slot")
                writes.add(target)
            elif opcode == "CAPTURE":
                raise ValueError("orphan input capture")
            elif opcode in ("NEWCLOSURE", "DUPCLOSURE"):
                if d < 0:
                    raise ValueError("negative constructor index")
                if opcode == "NEWCLOSURE":
                    child_id = proto.children[d]
                else:
                    constant = proto.constants[d]
                    if constant[0] != "closure":
                        raise ValueError("non-closure constructor constant")
                    child_id = constant[1]
                child = chunk.protos[child_id]
                for ordinal in range(child.num_upvalues):
                    item = proto.insns[index + ordinal + 1]
                    if item[0] != pc + ordinal + 1 or OPCODES[item[1]] != "CAPTURE":
                        raise ValueError("non-contiguous constructor captures")
                    mode, source = item[2:4]
                    target = child_id, ordinal
                    edges += 1
                    if edges > 200_000:
                        raise ValueError("capture edge budget")
                    if mode in (0, 1):
                        if source >= proto.max_stack:
                            raise ValueError("invalid capture register")
                        incoming[target].append(("copy" if mode == 0 else "ref", None))
                    elif mode == 2:
                        parent = proto.id, source
                        if parent not in nodes:
                            raise ValueError("invalid parent slot")
                        incoming[target].append(("upvalue", parent))
                        parents[target].add(parent)
                    else:
                        raise ValueError("unknown input capture mode")
                index += child.num_upvalues
            index += 1
    # A claimed ancestor must also exclude writes in non-claimed descendants.
    todo = list(writes)
    while todo:
        child = todo.pop()
        for parent in parents[child] - writes:
            writes.add(parent)
            todo.append(parent)
    if claims & writes:
        raise ValueError("certificate has a direct/forwarded write")
    dependencies, consumers = {}, collections.defaultdict(set)
    for node in claims:
        sources = incoming[node]
        if not sources or any(kind == "ref" for kind, _ in sources):
            raise ValueError("certificate has a missing/REF constructor")
        dependencies[node] = {parent for kind, parent in sources if kind == "upvalue"}
        if not dependencies[node] <= claims:
            raise ValueError("immutable ancestor certificate missing")
        for parent in dependencies[node]:
            consumers[parent].add(node)
    ready = [node for node, deps in dependencies.items() if not deps]
    established = set()
    while ready:
        node = ready.pop()
        established.add(node)
        for child in consumers[node]:
            dependencies[child].remove(node)
            if not dependencies[child]:
                ready.append(child)
    if established != claims:
        raise ValueError("self-justifying capture certificate cycle")
    return dict(analysis_status="complete", readonly_slots=len(claims), unknown_slots=len(nodes) - len(claims))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("root", "input", "report"):
        parser.add_argument("--" + name, type=pathlib.Path, required=True)
    parser.add_argument("--key", type=int, default=1)
    args = parser.parse_args()
    input_root = args.input.resolve(strict=True)
    _, entries = manifest(args.root)
    rows = []
    for name, entry in sorted(entries.items()):
        record = sidecar(args.root, entry)
        input_path = (input_root / name).resolve(strict=True)
        if not input_path.is_relative_to(input_root) or input_path.stat().st_size > 16 * 1024 * 1024:
            raise ValueError("input path/byte budget")
        raw = read_saved_bytecode(input_path)
        if hashlib.sha256(raw).hexdigest() != record["bytecode_sha256"]:
            raise ValueError("certificate input hash differs")
        proof = record["capture_effects"]
        result = verify(proof, None if proof["status"] == "refused" else parse_chunk(raw, args.key))
        rows.append(dict(script_path=name, input_sha256=record["bytecode_sha256"], status="passed", **result))
    report = dict(schema_version=1, rows=rows, scripts=len(rows),
                  readonly_slots=sum(r["readonly_slots"] for r in rows),
                  analysis_status=dict(collections.Counter(r["analysis_status"] for r in rows)),
                  contract="Certificates verified from original bytecode by the independent Python parser, including every constructor and descendant UPVAL write. No alias/purity/source-binding claim.")
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({k: v for k, v in report.items() if k != "rows"}))


if __name__ == "__main__":
    main()
