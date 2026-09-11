import concurrent.futures
import pathlib
import struct
import tempfile
import unittest

from bytecode_roundtrip import AUX_OPS, OP_INDEX, BytecodeError
from source_fingerprint import execution_image, fingerprint
from source_registry import write_once, validate_profile, read_input, store


def varint(value):
    output = bytearray()
    while value >= 128:
        output.append((value & 127) | 128)
        value >>= 7
    return bytes(output) + bytes([value])


def chunk(words=None, constants=(), *, flags=0, type_info=b'', line=0, debug=b'\0', key=1):
    if words is None:
        words = [OP_INDEX['LOADN'] | (i << 16) for i in range(7)] + [OP_INDEX['RETURN'] | (2 << 16)]
    encoded = list(words)
    pc = 0
    while pc < len(words):
        opcode = words[pc] & 255
        encoded[pc] = words[pc] & ~255 | (opcode * pow(key, -1, 256) & 255)
        pc += 2 if opcode in AUX_OPS else 1
    return (b'\x09\x03\x00\x00\x01' + bytes([4, 2, 0, 0, flags]) + varint(len(type_info)) + type_info
            + varint(len(words)) + b''.join(struct.pack('<I', w) for w in encoded)
            + varint(len(constants)) + b''.join(constants) + b'\x00' + varint(line) + b'\x00\x00' + debug + b'\x00')


class SourceRegistryTests(unittest.TestCase):
    def test_opcode_encoding_is_normalized_but_aux_and_registers_are_exact(self):
        words = [OP_INDEX['GETGLOBAL'], 0x12345678, OP_INDEX['RETURN'] | (2 << 16)]
        self.assertEqual(fingerprint(chunk(words))[0], fingerprint(chunk(words, key=203), 203)[0])
        mutated = list(words)
        mutated[1] ^= 1 << 31
        self.assertNotEqual(fingerprint(chunk(words))[0], fingerprint(chunk(mutated))[0])
        mutated = list(words)
        mutated[0] |= 1 << 8
        self.assertNotEqual(fingerprint(chunk(words))[0], fingerprint(chunk(mutated))[0])

    def test_order_operand_and_arity_changes_never_match(self):
        base = [OP_INDEX['SUB'] | (1 << 8) | (2 << 16) | (3 << 24),
                OP_INDEX['CALL'] | (1 << 8) | (2 << 16) | (2 << 24), OP_INDEX['RETURN'] | (2 << 16)]
        first = fingerprint(chunk(base))[0]
        reverse_operands = [OP_INDEX['SUB'] | (1 << 8) | (3 << 16) | (2 << 24), *base[1:]]
        arity = [base[0], base[1] ^ (1 << 24), base[2]]
        for words in (list(reversed(base)), reverse_operands, arity):
            self.assertNotEqual(first, fingerprint(chunk(words))[0])

    def test_constant_bits_native_flags_and_types_are_retained(self):
        zero = b'\x02' + struct.pack('<Q', 0)
        negative_zero = b'\x02' + struct.pack('<Q', 1 << 63)
        nan1 = b'\x02' + struct.pack('<Q', 0x7ff8000000000001)
        nan2 = b'\x02' + struct.pack('<Q', 0x7ff8000000000002)
        self.assertNotEqual(fingerprint(chunk(constants=[zero]))[0], fingerprint(chunk(constants=[negative_zero]))[0])
        self.assertNotEqual(fingerprint(chunk(constants=[nan1]))[0], fingerprint(chunk(constants=[nan2]))[0])
        original = fingerprint(chunk())[0]
        self.assertNotEqual(original, fingerprint(chunk(flags=1))[0])
        self.assertNotEqual(original, fingerprint(chunk(type_info=b'\x00\x00\x00'))[0])

    def test_only_declared_debug_metadata_and_container_trailer_are_ignored(self):
        raw = chunk()
        self.assertEqual(fingerprint(raw)[0], fingerprint(chunk(line=300))[0])
        self.assertEqual(fingerprint(raw)[0], fingerprint(raw, trailer_bytes=24)[0])
        with self.assertRaises(BytecodeError):
            fingerprint(raw + b'x' * 24)
        self.assertEqual(fingerprint(raw)[0], fingerprint(raw + b'x' * 24, trailer_bytes=24)[0])
        self.assertEqual(fingerprint(raw + b'x' * 24, trailer_bytes=24)[1]['opaque_trailer_bytes'], 24)
        with self.assertRaises(BytecodeError):
            fingerprint(raw + b'x' * 23, trailer_bytes=24)

    def test_trivial_code_and_unsupported_input_refuse(self):
        words = [OP_INDEX['LOADNIL'], OP_INDEX['RETURN'] | (2 << 16)]
        self.assertTrue(fingerprint(chunk(words))[1]['low_information'])
        self.assertFalse(fingerprint(chunk())[1]['low_information'])
        for raw in (b'', b'\x08' + chunk()[1:], chunk()[:-1], chunk(constants=[b'\x0a']), b'\x09\x03' + b'\x80' * 20):
            with self.assertRaises(BytecodeError):
                execution_image(raw)

    def test_content_addressed_writes_are_concurrent_idempotent_and_do_not_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / 'artifact'
            payload = b'example' * 10000
            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                list(pool.map(lambda _: write_once(path, payload), range(20)))
            self.assertEqual(path.read_bytes(), payload)
            with self.assertRaises(ValueError):
                write_once(path, b'different')
            self.assertEqual(path.read_bytes(), payload)
            fresh = pathlib.Path(temporary) / 'new-registry'
            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                paths = list(pool.map(lambda i: store(fresh, 'bytecode', payload + bytes([i % 3]), '.bin'), range(40)))
            self.assertEqual(len(set(paths)), 3)
            self.assertTrue(all((fresh / path).is_file() for path in paths))

    def test_profile_cannot_add_arbitrary_code_or_compiler_flags(self):
        profile = dict(id='o2', opt=2, debug=1, type_info=0, source_preamble='', flags=['--fflags=false'])
        validate_profile(profile)
        for patch in (dict(source_preamble='print("changed")\n'), dict(flags=['--fflags=true']), dict(debug=0)):
            with self.assertRaises(ValueError):
                validate_profile(profile | patch)

    def test_saved_input_distinguishes_empty_dump_from_bad_base64(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / 'input.lua'
            path.write_bytes(b'-- empty source\n')
            self.assertEqual(read_input(path, saved=True)[0], b'')
            path.write_bytes(b'-- valid dump\nCQMAAA==\n')
            self.assertEqual(read_input(path, saved=True)[0], b'\x09\x03\x00\x00')
            path.write_bytes(b'not a base64 dump!')
            with self.assertRaises(ValueError):
                read_input(path, saved=True)


if __name__ == '__main__':
    unittest.main()
