"""Bounded exact execution-image fingerprints for Luau v9/type-info v3.

Only opcode encoding and debug line/local metadata are normalized. All storage
operands, AUX bits, constant bytes, string order, flags, type payloads and child
IDs remain exact. This deliberately refuses other versions and unknown tags.
"""
import collections
import hashlib
import struct

from bytecode_roundtrip import AUX_OPS, OPCODES, BytecodeError, Reader

MODEL = 'luau-v9-exact-image-v1'
MAX_BYTES = 16 * 1024 * 1024
MAX_ITEMS = 1_000_000
MAX_PROTOS = 50_000


class BoundedReader(Reader):
    def varint(self):
        result = 0
        for index in range(10):
            byte = self.u8()
            result |= (byte & 127) << (7 * index)
            if not byte & 128:
                if result >= 1 << 64:
                    raise BytecodeError('varint exceeds uint64')
                return result
        raise BytecodeError('unterminated varint')

    def count(self, limit=MAX_ITEMS):
        count = self.varint()
        if count > limit:
            raise BytecodeError('fingerprint count budget exceeded')
        return count


def execution_image(data, key=1, trailer_bytes=0):
    if not 1 <= key <= 255 or key % 2 != 1:
        raise BytecodeError('opcode decode key must be odd and in 1..255')
    if not data or len(data) > MAX_BYTES:
        raise BytecodeError('fingerprint byte budget exceeded or empty input')
    if trailer_bytes not in (0, 24):
        raise BytecodeError('unsupported explicit container trailer size')
    reader = BoundedReader(data)
    if reader.u8() != 9 or reader.u8() != 3:
        raise BytecodeError('exact registry supports only bytecode v9/type-info v3')
    image = bytearray(b'luau-v9-exact-image-v1\0')
    def part(value):
        image.extend(struct.pack('<I', len(value)))
        image.extend(value)
    strings = [reader.string() for _ in range(reader.count())]
    image.extend(struct.pack('<I', len(strings)))
    for value in strings:
        part(value)
    start = reader.pos
    remappings = 0
    while reader.u8():
        string = reader.varint()
        if not 1 <= string <= len(strings):
            raise BytecodeError('invalid userdata type string')
        remappings += 1
        if remappings > 255:
            raise BytecodeError('userdata remapping budget exceeded')
    part(data[start:reader.pos])
    prototypes = reader.count(MAX_PROTOS)
    image.extend(struct.pack('<I', prototypes))
    instruction_count, kinds, proto_debug = 0, collections.Counter(), []
    for _ in range(prototypes):
        part(reader.bytes(5))  # maxstack, parameters, upvalues, vararg, flags
        part(reader.bytes(reader.count(MAX_BYTES)))  # exact type payload
        count = reader.count()
        if count * 4 > len(data) - reader.pos:
            raise BytecodeError('truncated instruction array')
        words = [reader.u32() for _ in range(count)]
        pc = 0
        while pc < count:
            opcode = (words[pc] & 255) * key & 255
            if opcode >= len(OPCODES):
                raise BytecodeError('unknown opcode in exact image')
            words[pc] = (words[pc] & ~255) | opcode
            kinds[OPCODES[opcode]] += 1
            instruction_count += 1
            if instruction_count > MAX_ITEMS:
                raise BytecodeError('instruction budget exceeded')
            pc += 2 if opcode in AUX_OPS else 1
        if pc != count:
            raise BytecodeError('missing AUX word')
        part(b''.join(struct.pack('<I', word) for word in words))
        constants = reader.count()
        start = reader.pos
        for _ in range(constants):
            tag = reader.u8()
            if tag == 0:
                pass
            elif tag == 1:
                reader.bytes(1)
            elif tag == 2:
                reader.bytes(8)  # retain NaN payloads and negative zero
            elif tag in (3, 6):
                reader.varint()
            elif tag == 4:
                reader.bytes(4)
            elif tag == 5:
                for _ in range(reader.count()):
                    reader.varint()
            elif tag == 7:
                reader.bytes(16)  # retain all four vector lanes and their bits
            elif tag == 8:
                for _ in range(reader.count()):
                    reader.varint()
                    reader.bytes(4)
            elif tag == 9:
                reader.bytes(1)
                reader.varint()
            else:
                raise BytecodeError(f'unsupported constant tag {tag}')
        image.extend(struct.pack('<I', constants))
        part(data[start:reader.pos])
        children = [reader.varint() for _ in range(reader.count(MAX_PROTOS))]
        if any(child >= prototypes for child in children):
            raise BytecodeError('invalid child prototype')
        part(b''.join(struct.pack('<I', child) for child in children))
        line_defined, name = reader.varint(), reader.varint()
        if name > len(strings):
            raise BytecodeError('invalid debug name')
        # Function names can be observed through debug.info; retain their identity.
        image.extend(struct.pack('<I', name))
        line_start = reader.pos
        if reader.u8():
            gap = reader.u8()
            if gap > 31 or not count:
                raise BytecodeError('invalid line-info layout')
            reader.bytes(count)
            reader.bytes(4 * (((count - 1) >> gap) + 1))
        line_bytes = data[line_start:reader.pos]
        debug_start = reader.pos
        if reader.u8():
            for _ in range(reader.count()):
                reader.varint(); reader.varint(); reader.varint(); reader.u8()
            for _ in range(reader.count()):
                reader.varint()
        proto_debug.append(dict(line_defined=line_defined, line_sha256=hashlib.sha256(line_bytes).hexdigest(),
                                locals_sha256=hashlib.sha256(data[debug_start:reader.pos]).hexdigest()))
    main = reader.varint()
    remaining = len(data) - reader.pos
    if main >= prototypes or remaining not in (0, trailer_bytes):
        raise BytecodeError('invalid main prototype or trailing bytecode')
    image.extend(struct.pack('<I', main))
    substantive = sum(count for op, count in kinds.items() if op not in ('NOP', 'PREPVARARGS', 'RETURN', 'LOADNIL', 'COVERAGE'))
    # A conservative admission threshold, not a statistical entropy estimate.
    # Refuse trivial chunks and small constant-return collisions before lookup.
    low_information = substantive < 4 or instruction_count < 8
    return bytes(image), dict(model=MODEL, bytecode_version=9, types_version=3, prototypes=prototypes,
                             instruction_count=instruction_count, substantive_instructions=substantive,
                             low_information=low_information, debug_metadata=proto_debug,
                             consumed_prefix_bytes=reader.pos, opaque_trailer_bytes=remaining,
                             opaque_trailer_sha256=hashlib.sha256(data[reader.pos:]).hexdigest() if remaining else None)


def fingerprint(data, key=1, trailer_bytes=0):
    image, facts = execution_image(data, key, trailer_bytes)
    return hashlib.sha256(image).hexdigest(), facts
