package luau

import (
	"encoding/binary"
	"math"
	"strings"
	"testing"
)

// encOp returns the file byte decoding to op under key.
func encOp(op Opcode, key byte) byte {
	for enc := 0; enc < 256; enc++ {
		if byte(uint32(enc)*uint32(key)%256) == byte(op) {
			return byte(enc)
		}
	}
	panic("no encoding")
}

func uleb(v uint64) []byte {
	var out []byte
	for {
		b := byte(v & 0x7f)
		v >>= 7
		if v != 0 {
			b |= 0x80
		}
		out = append(out, b)
		if v == 0 {
			return out
		}
	}
}

func word(op byte, a, b, c byte) []byte {
	return []byte{op, a, b, c}
}

func wordD(op byte, a byte, d int16) []byte {
	out := []byte{op, a, 0, 0}
	binary.LittleEndian.PutUint16(out[2:], uint16(d))
	return out
}

// printHelloChunk builds `print("hello")` as real Luau bytecode, version 6:
// GETIMPORT r0, import("print"); LOADK r1, "hello"; CALL r0, 1 arg, 0 rets;
// RETURN r0, 0 (bare return).
func printHelloChunk(t *testing.T, key byte) []byte {
	t.Helper()
	strs := []string{"print", "hello"}
	var out []byte
	out = append(out, 6, 0) // version, types version
	out = append(out, uleb(uint64(len(strs)))...)
	for _, s := range strs {
		out = append(out, uleb(uint64(len(s)))...)
		out = append(out, s...)
	}
	out = append(out, uleb(1)...) // one proto

	var proto []byte
	proto = append(proto, 2, 0, 0, 1) // maxstack, params, upvals, vararg
	proto = append(proto, 0)          // flags
	proto = append(proto, uleb(0)...) // no type info

	var code []byte
	// GETIMPORT r0 aux(count=1, id0=0 for "print"): aux = (1<<30)|(0<<20)|(0<<10)|0.
	code = append(code, word(encOp(OpGETIMPORT, key), 0, 0, 0)...)
	aux := make([]byte, 4)
	binary.LittleEndian.PutUint32(aux, 1<<30)
	code = append(code, aux...)
	// LOADK r1 const[1] ("hello").
	code = append(code, wordD(encOp(OpLOADK, key), 1, 1)...)
	// CALL r0 nargs=2 (r0,r1) nrets=1.
	code = append(code, word(encOp(OpCALL, key), 0, 2, 1)...)
	// RETURN r0, 0 rets.
	code = append(code, word(encOp(OpRETURN, key), 0, 0, 0)...)
	proto = append(proto, uleb(uint64(len(code)/4))...)
	proto = append(proto, code...)

	// Constants: [0] string idx 1 ("print"), [1] string idx 2 ("hello").
	proto = append(proto, uleb(2)...)
	proto = append(proto, 3) // tag string
	proto = append(proto, uleb(1)...)
	proto = append(proto, 3)
	proto = append(proto, uleb(2)...)

	proto = append(proto, uleb(0)...) // no children
	proto = append(proto, uleb(0)...) // line defined
	proto = append(proto, uleb(0)...) // no name
	proto = append(proto, 0, 0)       // no line info, no debug info
	out = append(out, proto...)
	out = append(out, uleb(0)...) // main proto 0
	return out
}

func TestDecompilePrintHello(t *testing.T) {
	bc := printHelloChunk(t, 1)
	src, err := TryDecompileBytecode(bc, 1, nil, DecompileOptions{})
	if err != nil {
		t.Fatalf("decompile: %v", err)
	}
	t.Logf("source:\n%s", src)
	// Single-element imports render as bare globals, like the Rust lifter.
	for _, want := range []string{`= print`, `"hello"`, "return"} {
		if !strings.Contains(src, want) {
			t.Fatalf("source missing %q:\n%s", want, src)
		}
	}
}

func TestDecompileIntegerAndNumberConsts(t *testing.T) {
	var out []byte
	out = append(out, 6, 0)
	out = append(out, uleb(0)...)
	out = append(out, uleb(1)...)
	var proto []byte
	proto = append(proto, 3, 0, 0, 1, 0)
	proto = append(proto, uleb(0)...)
	var code []byte
	code = append(code, wordD(encOp(OpLOADK, 1), 0, 0)...)
	code = append(code, wordD(encOp(OpLOADK, 1), 1, 1)...)
	code = append(code, wordD(encOp(OpLOADK, 1), 2, 2)...)
	code = append(code, word(encOp(OpRETURN, 1), 0, 1, 0)...)
	proto = append(proto, uleb(uint64(len(code)/4))...)
	proto = append(proto, code...)
	proto = append(proto, uleb(3)...)
	proto = append(proto, 9, 0)
	proto = append(proto, uleb(42)...) // integer 42
	proto = append(proto, 9, 1)
	proto = append(proto, uleb(7)...) // integer -7
	num := make([]byte, 8)
	binary.LittleEndian.PutUint64(num, math.Float64bits(2.5))
	proto = append(proto, 2) // tag number
	proto = append(proto, num...)
	proto = append(proto, uleb(0)...)
	proto = append(proto, uleb(0)...)
	proto = append(proto, uleb(0)...)
	proto = append(proto, 0, 0)
	out = append(out, proto...)
	out = append(out, uleb(0)...)
	src, err := TryDecompileBytecode(out, 1, nil, DecompileOptions{})
	if err != nil {
		t.Fatalf("decompile: %v", err)
	}
	t.Logf("source:\n%s", src)
	for _, want := range []string{"42", "-7", "2.5"} {
		if !strings.Contains(src, want) {
			t.Fatalf("source missing %q:\n%s", want, src)
		}
	}
}

func TestDecompileEncodedKey203(t *testing.T) {
	bc := printHelloChunk(t, 203)
	src, err := TryDecompileBytecode(bc, 203, nil, DecompileOptions{})
	if err != nil {
		t.Fatalf("decompile: %v", err)
	}
	if !strings.Contains(src, `= print`) || !strings.Contains(src, `"hello"`) {
		t.Fatalf("source:\n%s", src)
	}
	if _, err := TryDecompileBytecode(bc, 1, nil, DecompileOptions{}); err == nil {
		t.Fatal("wrong key should fail or misparse; got success")
	}
}
