"""Bounded private-table normalization within one RETURN execution segment.

Allocation timing is outside the dataflow model's contract; ordered template
keys/capacity, nil/dead-key history and externally observable stores stay exact.
Closure bodies have separate event identities and are never rebased here.
"""
def normalize_private_tables(events, terminal, offset, remaining):
    from bytecode_dataflow import Unknown
    if terminal[0] != 'return':
        raise Unknown('private table normalization requires a return segment')
    private = {}
    allocations = set()
    kept = []
    def spend():
        remaining[0] -= 1
        if remaining[0] < 0:
            raise Unknown('private table normalization budget')
    def references(value):
        spend()
        if not isinstance(value, tuple):
            return set()
        if len(value) == 2 and value[0] == 'value':
            return {value[1]}
        refs = set()
        for child in value:
            refs.update(references(child))
        return refs
    def args_without_body(event):
        return (event[1], event[3]) if event[0] in ('NEWCLOSURE', 'DUPCLOSURE') else event[1:]
    for index, event in enumerate(events):
        spend()
        identity = offset + index
        kind = event[0]
        if kind == 'DUPTABLE' and event[1][0] in ('table', 'tablek'):
            fields = event[1][1]
            remaining[0] -= len(fields)
            if remaining[0] < 0:
                raise Unknown('private table normalization budget')
            if all(key[0] == 'str' and value[0] == 'num' and value[1] == '0000000000000000' for key, value in fields):
                # Duplicate keys keep the last actual value, with unchanged template order.
                event = ('DUPTABLE', ('table', fields))
                private[identity] = dict(fields)
                allocations.add(identity)
        elif kind == 'NEWTABLE':
            private[identity] = {}
            allocations.add(identity)
        else:
            if kind == 'SETTABLE' and event[1][0] == 'value' and event[1][1] in private:
                table = event[1][1]
                key, value = event[2:]
                # No deletion/reinsertion normalization, even while the table is private.
                if value[0] in ('num', 'str', 'bool', 'vec') and private[table].get(key) == value:
                    continue
                escaped = references((key, value))
                if key[0] == 'str':
                    private[table][key] = value
                else:
                    escaped.add(table)
            else:
                escaped = references(args_without_body(event))
            for escaped_id in escaped:
                private.pop(escaped_id, None)
        kept.append((identity, event))
    kept = [x for x in kept if x[0] in allocations] + [x for x in kept if x[0] not in allocations]
    mapping = {old: offset + new for new, (old, _) in enumerate(kept)}
    def remap(value):
        spend()
        if not isinstance(value, tuple):
            return value
        if len(value) == 2 and value[0] == 'value':
            return ('value', mapping.get(value[1], value[1]))
        return tuple(remap(child) for child in value)
    def remap_event(event):
        if event[0] in ('NEWCLOSURE', 'DUPCLOSURE'):
            return (event[0], event[1], event[2], remap(event[3]))
        return remap(event)
    return tuple(remap_event(event) for _, event in kept), remap(terminal)
