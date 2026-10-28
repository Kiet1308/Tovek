"""Bounded transition-graph identity, including loops and captured storage.

This proves only a structural bisimulation under a consistent register/prototype
bijection. It does not equate different loop algorithms, reorder effects, drop
instructions or forget capture modes/CLOSE partitions. A mismatch is unknown,
not a runtime counterexample. Debug locations and resource limits are excluded.
"""
from __future__ import annotations

import collections
import hashlib
import struct

MODEL = 'luau-register-cfg-bisimulation-v2'
PRIVATE_MODEL = 'luau-private-storage-cfg-v1'
ADJACENT_PACK_MODEL = 'luau-private-adjacent-pack-cfg-v1'


class Unknown(Exception):
    pass


class Graph:
    def __init__(self, chunk, budget, *, private_copies=False):
        self.private_copies = private_copies
        self.chunk = chunk
        self.remaining = budget
        self.prototypes = {}
        self.bodies = []
        self.originals = []
        self.templates = {}
        self.exact_storage = False
        self._reference_context = False
        self._adjacent_packs = False
        self.stats = collections.Counter()

    def spend(self, count=1):
        self.remaining -= count
        if self.remaining < 0:
            raise Unknown('graph/analysis budget')

    def prototype(self, index):
        if not 0 <= index < len(self.chunk.protos):
            raise Unknown('invalid prototype')
        if index not in self.prototypes:
            self.spend()
            self.prototypes[index] = len(self.prototypes)
            self.bodies.append(None)
            self.originals.append(index)
        return self.prototypes[index]

    def constant(self, proto, index, seen=()):
        self.spend()
        if not 0 <= index < len(proto.constants) or index in seen or len(seen) > 32:
            raise Unknown('invalid/recursive constant')
        value = proto.constants[index]
        kind = value[0]
        if kind in ('nil', 'bool'):
            return value
        if kind == 'num':
            return kind, struct.pack('<d', value[1]).hex()
        if kind == 'int':
            return kind, str(value[1])
        if kind == 'vec':
            return kind, struct.pack('<4f', *value[1]).hex()
        if kind == 'str':
            if not 0 < value[1] <= len(self.chunk.strings):
                raise Unknown('invalid string')
            payload = self.chunk.strings[value[1] - 1]
            self.spend((len(payload) + 31) // 32)
            return kind, payload.hex()
        if kind == 'closure':
            template = self.templates.setdefault((id(proto), index), len(self.templates))
            return kind, self.prototype(value[1]), template
        if kind == 'import':
            word = value[1]
            size = word >> 30
            if not 1 <= size <= 3:
                raise Unknown('invalid import')
            return kind, tuple(self.constant(proto, (word >> (20 - 10 * i)) & 1023,
                                             (*seen, index)) for i in range(size))
        if kind == 'table':
            return kind, tuple((self.constant(proto, k, (*seen, index)),
                                ('num', struct.pack('<d', 0.0).hex())) for k in value[1])
        if kind == 'tablek':
            # Retain template layout; only resolve the VM's implicit +0 values.
            return kind, tuple((self.constant(proto, k, (*seen, index)),
                                ('num', struct.pack('<d', 0.0).hex()) if v < 0
                                else self.constant(proto, v, (*seen, index)))
                               for k, v in value[1])
        raise Unknown('unsupported constant ' + kind)

    def function(self, proto):
        from bytecode_roundtrip import OPCODES, AUX_OPS
        if not 0 <= proto.num_params <= proto.max_stack <= 255:
            raise Unknown('invalid stack/parameters')
        code = {i[0]: i for i in proto.insns}
        if len(code) != len(proto.insns) or 0 not in code:
            raise Unknown('duplicate/missing instruction')
        # CAPTURE words belong to a closure creation, never independent CFG nodes.
        captures, payload = {}, set()
        referenced = set()
        open_packs = False
        for pc, op, a, b, c, d, e, aux in proto.insns:
            self.spend()
            if not 0 <= op < len(OPCODES):
                raise Unknown('invalid opcode')
            name = OPCODES[op]
            if name in ('NEWCLOSURE', 'DUPCLOSURE'):
                if name == 'NEWCLOSURE':
                    if not 0 <= d < len(proto.children):
                        raise Unknown('invalid child')
                    child = proto.children[d]
                else:
                    if not 0 <= d < len(proto.constants) or proto.constants[d][0] != 'closure':
                        raise Unknown('invalid closure constant')
                    child = proto.constants[d][1]
                self.prototype(child)
                count = self.chunk.protos[child].num_upvalues
                sites = []
                for offset in range(count):
                    q = pc + 1 + offset
                    row = code.get(q)
                    if row is None or OPCODES[row[1]] != 'CAPTURE' or q in payload:
                        raise Unknown('missing/overlapping capture')
                    mode, source = row[2:4]
                    if mode not in (0, 1, 2) or (name == 'DUPCLOSURE' and mode == 1):
                        raise Unknown('unsupported capture mode')
                    if mode == 2:
                        if source >= proto.num_upvalues:
                            raise Unknown('invalid captured upvalue')
                    elif source >= proto.max_stack:
                        raise Unknown('invalid captured register')
                    if mode == 1:
                        referenced.add(source)
                    sites.append((mode, source))
                    payload.add(q)
                captures[pc] = child, sites
            open_packs |= ((name == 'CALL' and (b == 0 or c == 0))
                           or (name in ('RETURN', 'GETVARARGS') and b == 0)
                           or (name == 'SETLIST' and c == 0))
        for pc, row in code.items():
            if OPCODES[row[1]] == 'CAPTURE' and pc not in payload:
                raise Unknown('orphan capture')

        if proto.is_vararg:
            if OPCODES[code[0][1]] != 'PREPVARARGS' or sum(
                    OPCODES[row[1]] == 'PREPVARARGS' for row in code.values()) != 1:
                raise Unknown('graph requires one entry vararg preparation')

        # Open result packs address an unbounded suffix. Keep the exact register
        # layout in these functions; finite fixed-arity functions may alpha-rename.
        exact_storage = open_packs or (self.exact_storage and (not self._reference_context or bool(referenced)))
        registers = {i: ('parameter', i) for i in range(proto.num_params)}

        def reg(index):
            if not 0 <= index < proto.max_stack:
                raise Unknown('register outside stack')
            if exact_storage:
                return 'register', index
            if index not in registers:
                registers[index] = ('temporary', len(registers) - proto.num_params)
            return registers[index]

        def span(start, size):
            return tuple(reg(i) for i in range(start, start + size))

        labels, queue, nodes, facts = {0: 0}, collections.deque([0]), [], []
        pcs = []

        def label(pc):
            if pc not in code or pc in payload:
                raise Unknown('invalid CFG target')
            if pc not in labels:
                labels[pc] = len(labels)
                queue.append(pc)
            return labels[pc]

        while queue:
            self.spend()
            pc = queue.popleft()
            _, op, a, b, c, d, e, aux = code[pc]
            name = OPCODES[op]
            next_pc = pc + (2 if op in AUX_OPS else 1)
            successors = [next_pc]
            reads, writes, kills, edge_writes = set(), set(), set(), {}
            edge_kills, edge_tops = {}, {}
            top_write, open_read = 'preserve', None
            operands = ()

            def read(r):
                reads.add(r)
                return reg(r)

            def write(r):
                writes.add(r)
                return reg(r)

            def read_span(start, size):
                return tuple(read(r) for r in range(start, start + size))

            def write_span(start, size):
                return tuple(write(r) for r in range(start, start + size))

            def pack(start, size):
                nonlocal open_read
                if size:
                    return 'fixed', read_span(start, size - 1)
                if not 0 <= start <= proto.max_stack:
                    raise Unknown('invalid open pack start')
                open_read = start
                self.stats['open_packs'] += 1
                return 'open', start

            constant = lambda k: self.constant(proto, k)
            if name in ('NOP', 'COVERAGE'):
                pass
            elif name == 'PREPVARARGS':
                if not proto.is_vararg or a != proto.num_params:
                    raise Unknown('invalid vararg preparation')
                operands = (a,)
            elif name == 'LOADNIL':
                operands = (write(a),)
            elif name == 'LOADB':
                if b not in (0, 1):
                    raise Unknown('invalid boolean')
                operands = write(a), bool(b)
                successors = [pc + 1 + c]
            elif name == 'LOADN':
                operands = write(a), d
            elif name in ('LOADK', 'LOADKX', 'DUPTABLE'):
                operands = write(a), constant(aux if name == 'LOADKX' else d)
            elif name == 'MOVE':
                operands = write(a), read(b)
            elif name in ('GETGLOBAL', 'SETGLOBAL'):
                operands = (write(a) if name == 'GETGLOBAL' else read(a)), constant(aux)
            elif name in ('GETUPVAL', 'SETUPVAL'):
                if b >= proto.num_upvalues:
                    raise Unknown('invalid upvalue')
                operands = (write(a) if name == 'GETUPVAL' else read(a)), ('upvalue', b)
            elif name == 'CLOSEUPVALS':
                if a > proto.max_stack:
                    raise Unknown('invalid close boundary')
                # Preserve exactly which potentially captured cells are closed,
                # including cells opened only on another predecessor/iteration.
                operands = (tuple(sorted(reg(r) for r in referenced if r >= a)),)
                self.stats['close_sites'] += 1
            elif name == 'GETIMPORT':
                key = constant(d)
                size = aux >> 30
                if not 1 <= size <= 3 or key != ('import', tuple(
                        constant((aux >> (20 - 10 * i)) & 1023) for i in range(size))):
                    raise Unknown('inconsistent import')
                operands = write(a), key
            elif name in ('GETTABLE', 'SETTABLE', 'GETTABLEKS', 'SETTABLEKS',
                          'GETTABLEN', 'SETTABLEN', 'GETUDATAKS', 'SETUDATAKS'):
                key = (constant(aux & 0xffff if 'UDATA' in name else aux) if name.endswith('KS')
                       else c + 1 if name.endswith('N') else read(c))
                operands = (write(a) if name.startswith('GET') else read(a)), read(b), key
            elif name in ('NAMECALL', 'NAMECALLUDATA'):
                if next_pc not in code or OPCODES[code[next_pc][1]] != 'CALL':
                    raise Unknown('NAMECALL without CALL')
                operands = write_span(a, 2), read(b), constant(aux & 0xffff if 'UDATA' in name else aux)
            elif name in ('ADD', 'SUB', 'MUL', 'DIV', 'MOD', 'POW', 'IDIV', 'AND', 'OR'):
                operands = write(a), read(b), read(c)
            elif name in ('ADDK', 'SUBK', 'MULK', 'DIVK', 'MODK', 'POWK', 'IDIVK', 'ANDK', 'ORK'):
                operands = write(a), read(b), constant(c)
            elif name in ('SUBRK', 'DIVRK'):
                operands = write(a), constant(b), read(c)
            elif name in ('NOT', 'MINUS', 'LENGTH'):
                operands = write(a), read(b)
            elif name == 'CONCAT':
                if b > c:
                    raise Unknown('invalid concatenation range')
                operands = write(a), read_span(b, c - b + 1)
            elif name == 'NEWTABLE':
                operands = write(a), b, aux
            elif name == 'SETLIST':
                operands = read(a), pack(b, c), aux
                if c == 0:
                    top_write = None
            elif name in ('FASTCALL', 'FASTCALL1', 'FASTCALL2', 'FASTCALL2K', 'FASTCALL3'):
                call_pc = pc + 1 + c
                call = code.get(call_pc)
                if call is None or OPCODES[call[1]] != 'CALL' or call_pc < next_pc or a == 0:
                    raise Unknown('invalid fastcall fallback')
                call_a, call_b, call_c = call[2:5]
                if name == 'FASTCALL':
                    arguments = pack(call_a + 1, call_b)
                elif name == 'FASTCALL1':
                    arguments = (read(b),)
                elif name == 'FASTCALL2':
                    arguments = read(b), read(aux)
                elif name == 'FASTCALL2K':
                    arguments = read(b), constant(aux)
                else:
                    if aux >> 16:
                        raise Unknown('invalid FASTCALL3 register payload')
                    arguments = read(b), read(aux & 255), read((aux >> 8) & 255)
                results = span(call_a, max(0, call_c - 1))
                operands = a, arguments, call_b, call_c, results
                if call_c == 0:
                    operands += (('open_result', call_a),)
                successors = [call_pc + 1, next_pc]
                edge_kills[0] = set(range(call_a, proto.max_stack))
                edge_writes[0] = set(range(call_a, call_a + max(0, call_c - 1)))
                edge_tops[0] = call_a if call_c == 0 else None if name == 'FASTCALL' else 'preserve'
                self.stats['fastcall_sites'] += 1
            elif name in ('CALL', 'GETVARARGS'):
                if name == 'CALL':
                    operands = read(a), pack(a + 1, b), c
                    count = c
                else:
                    if not proto.is_vararg:
                        raise Unknown('varargs in fixed function')
                    operands, count = (b,), b
                kills.update(range(a, proto.max_stack))
                operands += (write_span(a, max(0, count - 1)),)
                top_write = a if count == 0 else None
                if count == 0:
                    operands += (('open_result', a),)
            elif name in ('NEWCLOSURE', 'DUPCLOSURE'):
                child, sites = captures[pc]
                encoded = []
                for mode, source in sites:
                    encoded.append((mode, ('upvalue', source) if mode == 2 else reg(source)))
                    if mode in (0, 1) and source != a:
                        reads.add(source)
                    if mode == 1:
                        self.stats['reference_captures'] += 1
                identity = constant(d) if name == 'DUPCLOSURE' else self.prototype(child)
                operands = write(a), identity, tuple(encoded)
                successors = [next_pc + len(sites)]
            elif name in ('JUMP', 'JUMPBACK', 'JUMPX'):
                successors = [pc + 1 + (e if name == 'JUMPX' else d)]
            elif name in ('JUMPIF', 'JUMPIFNOT'):
                operands = (read(a),)
                successors = [pc + 1 + d, next_pc]
            elif name in ('JUMPIFEQ', 'JUMPIFNOTEQ', 'JUMPIFLT', 'JUMPIFNOTLT', 'JUMPIFLE', 'JUMPIFNOTLE'):
                operands = read(a), read(aux)
                successors = [pc + 1 + d, next_pc]
            elif name.startswith('JUMPXEQK'):
                kind = name.removeprefix('JUMPXEQK')
                if kind not in ('NIL', 'B', 'N', 'S'):
                    raise Unknown('unknown constant branch')
                value = ('nil',) if kind == 'NIL' else (('bool', bool(aux & 1)) if kind == 'B' else constant(aux & 0xffffff))
                operands = read(a), value, bool(aux >> 31)
                successors = [pc + 1 + d, next_pc]
            elif name in ('FORNPREP', 'FORNLOOP'):
                operands = read_span(a, 3), write_span(a, 3) if name == 'FORNPREP' else (write(a + 2),)
                successors = [pc + 1 + d, next_pc]
            elif name in ('FORGPREP', 'FORGPREP_INEXT', 'FORGPREP_NEXT'):
                target = pc + 1 + d
                if target not in code or OPCODES[code[target][1]] != 'FORGLOOP' or code[target][2] != a:
                    raise Unknown('invalid generic iterator target')
                operands = read_span(a, 3), write_span(a, 3)
                kills.update(range(a, proto.max_stack))
                successors = [target]
            elif name == 'FORGLOOP':
                count = aux & 255
                if count == 0 or aux & 0x7fffff00:
                    raise Unknown('invalid iterator arity/flags')
                operands = read_span(a, 3), span(a + 3, count), bool(aux >> 31)
                successors = [pc + 1 + d, next_pc]
                kills.update(range(a + 3, proto.max_stack))
                edge_writes[0] = set(range(a + 3, a + 3 + count))
            elif name == 'RETURN':
                operands = (pack(a, b),)
                successors = []
            else:
                raise Unknown('unsupported opcode ' + name)
            edges = tuple(label(q) for q in successors)
            self.stats['back_edges'] += sum(q <= pc for q in successors)
            pcs.append(pc)
            nodes.append((name, operands, edges))
            facts.append((reads, writes, kills, edge_writes, top_write, open_read, edge_kills, edge_tops))

        if proto.is_vararg and any(0 in edges for _, _, edges in nodes):
            raise Unknown('graph cannot re-enter vararg preparation')
        self.validate_definitions(proto, nodes, facts)
        if self._adjacent_packs and open_packs and not self.exact_storage:
            rewritten = self.adjacent_pack_graph(proto, nodes, facts, pcs, code)
            if rewritten is not None:
                nodes, facts = rewritten
                exact_storage = False
                registers = {r: ('register', r) for r in range(proto.max_stack)}
        if self.private_copies and not exact_storage:
            nodes = self.private_copy_graph(nodes, facts, pcs, code, registers, proto.num_params)
        self.stats['instructions'] += len(nodes)
        self.stats['edges'] += sum(len(node[2]) for node in nodes)
        self.stats['prototypes'] += 1
        return (proto.num_params, proto.num_upvalues, bool(proto.is_vararg),
                proto.max_stack if exact_storage else None, tuple(nodes))

    def adjacent_pack_graph(self, proto, nodes, facts, pcs, code):
        """Keep opaque private packs only across a unique immediate consumer.

        Definition validation must precede this: an open producer leaves its
        suffix undefined for subsequent fixed reads, even after consumption.
        """
        predecessors = [[] for _ in nodes]
        for index, (_, _, edges) in enumerate(nodes):
            for target in edges:
                predecessors[target].append(index)
        producers, consumers = {}, {}
        for index, (name, _, edges) in enumerate(nodes):
            self.spend()
            _, _, a, b, c, _, _, _ = code[pcs[index]]
            if any(type(top) is int for top in facts[index][7].values()):
                return None  # FASTCALL success can bypass the ordinary producer.
            if (name == 'CALL' and c == 0) or (name == 'GETVARARGS' and b == 0):
                if len(edges) != 1 or predecessors[edges[0]] != [index] or not 0 <= a < proto.max_stack:
                    return None
                consumer = edges[0]
                if (nodes[consumer][0] not in ('CALL', 'RETURN', 'SETLIST')
                        or facts[consumer][5] is None or pcs[consumer] != pcs[index] + 1):
                    return None
                start = facts[consumer][5]
                if not 0 <= start <= a:
                    return None
                producers[index] = a
                consumers[consumer] = a
        if not producers or {i for i, fact in enumerate(facts) if fact[5] is not None} != set(consumers):
            return None
        result, updated = [], []
        for index, (name, operands, edges) in enumerate(nodes):
            self.spend()
            prefix = tuple(range(facts[index][5], consumers[index])) if index in consumers else ()
            self.spend(len(prefix))
            def rewrite(value):
                self.spend()
                if type(value) is tuple:
                    if len(value) == 2 and value[0] == 'open':
                        return ('open', tuple(('register', r) for r in prefix))
                    if len(value) == 2 and value[0] == 'open_result':
                        return ('open_result',)
                    return tuple(rewrite(item) for item in value)
                return value
            result.append((name, rewrite(operands), edges))
            reads, writes, kills, edge_writes, top_write, _, edge_kills, edge_tops = facts[index]
            updated.append((reads | set(prefix), writes, kills, edge_writes, top_write, None, edge_kills, edge_tops))
        self.stats['adjacent_pack_pairs'] += len(producers)
        return result, updated

    def private_copy_graph(self, nodes, facts, pcs, code, registers, parameters):
        """Private storage only: preserve effects and normalize dead copy slots.

        The caller certifies private storage and any adjacent open packs. Copies
        have one predecessor and a dead source; lookup reads/results are split.
        """
        predecessors = [[] for _ in nodes]
        for source, (_, _, edges) in enumerate(nodes):
            for target in edges:
                predecessors[target].append(source)
        mask = lambda values: sum(1 << r for r in values)
        incoming = [0] * len(nodes)
        outgoing = [0] * len(nodes)
        pending, queued = collections.deque(range(len(nodes))), set(range(len(nodes)))
        while pending:
            self.spend()
            index = pending.popleft()
            queued.remove(index)
            reads, writes, kills, edge_writes, _, _, edge_kills, _ = facts[index]
            after = 0
            for edge, target in enumerate(nodes[index][2]):
                after |= incoming[target] & ~mask(edge_writes.get(edge, ())) & ~mask(edge_kills.get(edge, ()))
            before = mask(reads) | (after & ~mask(writes) & ~mask(kills))
            outgoing[index] = after
            if before != incoming[index]:
                incoming[index] = before
                for predecessor in predecessors[index]:
                    if predecessor not in queued:
                        pending.append(predecessor)
                        queued.add(predecessor)
        removed = set()
        nodes = list(nodes)
        facts = list(facts)
        for index, (name, operands, edges) in enumerate(nodes):
            self.spend()
            if name not in ('GETGLOBAL', 'GETIMPORT', 'GETUPVAL', 'GETTABLE', 'GETTABLEKS', 'GETTABLEN') or len(edges) != 1:
                continue
            move = edges[0]
            if nodes[move][0] != 'MOVE' or predecessors[move] != [index] or len(nodes[move][2]) != 1:
                continue
            source = code[pcs[index]][2]
            destination, copied = code[pcs[move]][2:4]
            if source != copied or source == destination or outgoing[move] & (1 << source):
                continue
            nodes[index] = name, (nodes[move][1][0], *operands[1:]), edges
            facts[index] = (facts[index][0], {destination}, *facts[index][2:])
            removed.add(move)
        self.stats['private_copies_removed'] += len(removed)
        labels, queue, output, output_facts = {0: 0}, collections.deque([0]), [], []
        def label(index):
            while index in removed:
                self.spend()
                index = nodes[index][2][0]
            if index not in labels:
                labels[index] = len(labels)
                queue.append(index)
            return labels[index]
        while queue:
            self.spend()
            old = queue.popleft()
            name, operands, edges = nodes[old]
            output.append((name, operands, tuple(label(edge) for edge in edges)))
            output_facts.append(facts[old])
        # Canonicalize private storage lifetimes, not allocation of dead slots.
        nodes, facts = output, output_facts
        incoming = [0] * len(nodes)
        changed = True
        while changed:
            changed = False
            for index in reversed(range(len(nodes))):
                self.spend()
                reads, writes, kills, edge_writes, _, _, edge_kills, _ = facts[index]
                after = 0
                for edge, target in enumerate(nodes[index][2]):
                    after |= incoming[target] & ~mask(edge_writes.get(edge, ())) & ~mask(edge_kills.get(edge, ()))
                before = mask(reads) | (after & ~mask(writes) & ~mask(kills))
                if before != incoming[index]:
                    incoming[index] = before
                    changed = True
        parent = {}
        def find(cell):
            self.spend()
            path = []
            while parent.setdefault(cell, cell) != cell:
                self.spend()
                path.append(cell)
                cell = parent[cell]
            for child in path:
                parent[child] = cell
            return cell
        lookups = {'GETGLOBAL', 'GETIMPORT', 'GETUPVAL', 'GETTABLE', 'GETTABLEKS', 'GETTABLEN'}
        def after_cell(index, register):
            return (index, register, 'result') if nodes[index][0] in lookups and register in facts[index][1] else (index, register)
        for index, (_, _, edges) in enumerate(nodes):
            for target in edges:
                bits = incoming[target]
                while bits:
                    bit = bits & -bits
                    register = bit.bit_length() - 1
                    left, right = find(after_cell(index, register)), find((target, register))
                    parent[right] = left
                    bits -= bit
        names = {find((0, r)): ('parameter', r) for r in range(parameters) if incoming[0] & (1 << r)}
        originals = {value: key for key, value in registers.items()}
        next_name = 0
        def rename(index, value, output=False):
            nonlocal next_name
            self.spend()
            if type(value) is tuple:
                if len(value) == 2 and value[0] in ('temporary', 'parameter', 'register') and type(value[1]) is int:
                    root = find(after_cell(index, originals[value]) if output else (index, originals[value]))
                    if root not in names:
                        names[root] = ('temporary', next_name)
                        next_name += 1
                    return names[root]
                return tuple(rename(index, item) for item in value)
            return value
        return [(name, tuple(rename(index, operand, name in lookups and slot == 0) for slot, operand in enumerate(operands)), edges)
                for index, (name, operands, edges) in enumerate(nodes)]

    def validate_definitions(self, proto, nodes, facts):
        """Must-definition analysis; joins intersect definitions, loops converge.

        Captured-cell reads remain explicit VM operations in the certificate.
        This check rejects uninitialized register uses; it is not a purity proof.
        """
        predecessors = [[] for _ in nodes]
        for source, (_, _, edges) in enumerate(nodes):
            for edge, target in enumerate(edges):
                predecessors[target].append((source, edge))
        self.stats['joins'] += sum(len(p) > 1 for p in predecessors)
        all_regs = (1 << proto.max_stack) - 1
        incoming = [all_regs] * len(nodes)
        outgoing = [[all_regs] * len(node[2]) for node in nodes]
        unseen, conflict = object(), object()
        tops = [unseen] * len(nodes)
        top_out = [[unseen] * len(node[2]) for node in nodes]
        pending, queued = collections.deque(range(len(nodes))), set(range(len(nodes)))
        mask = lambda values: sum(1 << r for r in values)
        while pending:
            self.spend()
            node = pending.popleft()
            queued.remove(node)
            definitions = (1 << proto.num_params) - 1 if node == 0 else all_regs
            states = [None] if node == 0 else []
            for source, edge in predecessors[node]:
                definitions &= outgoing[source][edge]
                if top_out[source][edge] is not unseen:
                    states.append(top_out[source][edge])
            top = unseen if not states else states[0] if all(v == states[0] for v in states) else conflict
            reads, writes, kills, edge_writes, top_write, _, edge_kills, edge_tops = facts[node]
            after = (definitions & ~mask(kills)) | mask(writes)
            new = [(after & ~mask(edge_kills.get(edge, ()))) | mask(edge_writes.get(edge, ()))
                   for edge in range(len(nodes[node][2]))]
            base_top = top if top_write == 'preserve' else top_write
            new_top = [base_top if edge_tops.get(edge, 'preserve') == 'preserve' else edge_tops[edge]
                       for edge in range(len(nodes[node][2]))]
            incoming[node], tops[node] = definitions, top
            if new != outgoing[node] or new_top != top_out[node]:
                outgoing[node], top_out[node] = new, new_top
                for target in nodes[node][2]:
                    if target not in queued:
                        queued.add(target)
                        pending.append(target)
        for index, (reads, _, _, _, _, open_read, _, _) in enumerate(facts):
            required = set(reads)
            if open_read is not None:
                top = tops[index]
                if type(top) is not int or open_read > top:
                    raise Unknown('unresolved open result pack')
                required.update(range(open_read, top))
            if mask(required) & ~incoming[index]:
                raise Unknown('register read without a reaching definition')

    def build(self):
        if self.chunk.version != 9:
            raise Unknown('unsupported bytecode version')
        self.prototype(self.chunk.main)
        from bytecode_roundtrip import OP_INDEX
        # A ref can alias a caller stack slot during a nested call. Preserve the
        # entire chunk's physical layout, including frame sizes, in that domain.
        # This avoids assuming that untrusted bytecode obeys compiler allocation
        # discipline merely because its explicit register operands look alike.
        self.exact_storage = bool(self.chunk.protos[self.chunk.main].num_upvalues)
        for proto in self.chunk.protos:
            for row in proto.insns:
                self.spend()
                if row[1] == OP_INDEX['CAPTURE'] and row[2] == 1:
                    self.exact_storage = True
        done = 0
        # Queue prototype identities instead of recursive expansion. Sharing,
        # recursive constant references and DUPCLOSURE identity remain explicit.
        while done < len(self.prototypes):
            original = self.originals[done]
            self.bodies[done] = self.function(self.chunk.protos[original])
            done += 1
        return tuple(self.bodies), dict(self.stats)


def compare_graph(original, rebuilt, *, budget=20000):
    return _compare_graph(original, rebuilt, budget=budget, private_copies=False)


def compare_private_graph(original, rebuilt, *, budget=20000):
    """Bisimulation up to adjacent lookup copies and private storage lifetimes.

    REF/main-upvalue and open-pack functions retain the exact-storage contract.
    Unsupported transitions and exhausted analysis fuel still return unknown.
    """
    return _compare_graph(original, rebuilt, budget=budget, private_copies=True)


def compare_adjacent_pack_graph(original, rebuilt, *, budget=20000):
    """Private storage with uniquely paired immediate open-pack transitions.

    Entire chunks with REF captures or main upvalues retain exact storage.
    Nonadjacent/ambiguous packs retain the existing exact function contract.
    """
    return _compare_graph(original, rebuilt, budget=budget, private_copies=True, adjacent_packs=True)


def _compare_graph(original, rebuilt, *, budget, private_copies, adjacent_packs=False):
    graphs, summaries, reasons = [], [], {}
    for label, chunk in [('original', original), ('rebuilt', rebuilt)]:
        try:
            builder = Graph(chunk, budget, private_copies=private_copies)
            builder._adjacent_packs = adjacent_packs
            graph, summary = builder.build()
            graphs.append(graph)
            summaries.append(summary)
        except (Unknown, RecursionError) as error:
            reasons[label] = str(error) or 'recursion budget'
    model = ADJACENT_PACK_MODEL if adjacent_packs else PRIVATE_MODEL if private_copies else MODEL
    result = dict(model=model, status='unknown')
    if reasons:
        result['reasons'] = reasons
    else:
        result.update(status='proved' if graphs[0] == graphs[1] else 'unknown',
                      fingerprints=[hashlib.sha256(repr(g).encode()).hexdigest() for g in graphs],
                      summaries=summaries)
        if graphs[0] != graphs[1]:
            result['reason'] = ('no bisimulation under bounded private-storage lifetimes' if private_copies
                                else 'no structural bisimulation under the bounded register mapping')
    return result
