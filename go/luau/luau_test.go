package luau

import (
	"strings"
	"testing"
)

func leb128Enc(v uint64) []byte {
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

func TestOpcodeRoundtrip(t *testing.T) {
	if OpCount != 90 {
		t.Fatalf("OpCount = %d, want 90", OpCount)
	}
	for b := 0; b < 90; b++ {
		op, ok := OpcodeFromByte(byte(b))
		if !ok {
			t.Fatalf("byte %d rejected", b)
		}
		if int(op) != b {
			t.Fatalf("byte %d decoded to %d", b, int(op))
		}
		if op.String() == "UNKNOWN" {
			t.Fatalf("opcode %d has no name", b)
		}
	}
	if _, ok := OpcodeFromByte(90); ok {
		t.Fatal("byte 90 should be invalid")
	}
	if _, ok := OpcodeFromByte(97); ok {
		t.Fatal("byte 97 should be invalid")
	}
	names := map[Opcode]string{
		OpNOP: "NOP", OpFASTPCALL: "FASTPCALL", OpIDIVK: "IDIVK",
		OpGETUDATAKS: "GETUDATAKS", OpCMPPROTO: "CMPPROTO",
		OpFORGPREPINEXT: "FORGPREP_INEXT", OpFORGPREPNEXT: "FORGPREP_NEXT",
	}
	for op, want := range names {
		if op.String() != want {
			t.Fatalf("op %d name = %q, want %q", int(op), op.String(), want)
		}
	}
	auxOps := []Opcode{OpGETGLOBAL, OpSETGLOBAL, OpGETIMPORT, OpGETTABLEKS, OpSETTABLEKS,
		OpNAMECALL, OpJUMPIFEQ, OpJUMPIFLE, OpJUMPIFLT, OpJUMPIFNOTEQ, OpJUMPIFNOTLE, OpJUMPIFNOTLT,
		OpNEWTABLE, OpSETLIST, OpFORGLOOP, OpLOADKX, OpFASTCALL2, OpFASTCALL2K, OpFASTCALL3,
		OpJUMPXEQKNIL, OpJUMPXEQKB, OpJUMPXEQKN, OpJUMPXEQKS,
		OpGETUDATAKS, OpSETUDATAKS, OpNAMECALLUDATA, OpNEWCLASSMEMBER, OpCALLFB, OpCMPPROTO}
	for _, op := range auxOps {
		if !HasAux(op) {
			t.Fatalf("op %s should have aux", op)
		}
	}
	for _, op := range []Opcode{OpNOP, OpCALL, OpRETURN, OpJUMP, OpADD, OpLOADK, OpFASTCALL1, OpIDIV} {
		if HasAux(op) {
			t.Fatalf("op %s should not have aux", op)
		}
	}
}

func TestLEB128(t *testing.T) {
	for _, v := range []uint64{0, 1, 127, 128, 255, 300, 16384, 1<<32 - 1, 1 << 63} {
		enc := leb128Enc(v)
		got, n, err := ReadULEB128(enc)
		if err != nil || got != v || n != len(enc) {
			t.Fatalf("ULEB128(%d) = %d,%d,%v", v, got, n, err)
		}
	}
	if _, _, err := ReadULEB128([]byte{0x80}); err == nil {
		t.Fatal("truncated LEB128 should fail")
	}
	enc := append(leb128Enc(3), []byte("abc")...)
	s, n, err := ReadString(enc)
	if err != nil || string(s) != "abc" || n != 4 {
		t.Fatalf("ReadString = %q,%d,%v", s, n, err)
	}
	if _, _, err := ReadString([]byte{5, 'a'}); err == nil {
		t.Fatal("truncated string should fail")
	}
	if _, _, err := ReadListCount([]byte{0x80}); err == nil {
		t.Fatal("truncated count should fail")
	}
}

func TestParseBytecodeRejectsBadVersion(t *testing.T) {
	for _, tc := range [][]byte{{}, {3}, {15}, {0, 'x'}} {
		if _, err := ParseBytecode(tc, 1); err == nil {
			t.Fatalf("input %v should fail", tc)
		}
	}
	if _, err := ParseBytecode([]byte{4, 9}, 1); err == nil {
		t.Fatal("bad types version should fail")
	}
}

func minimalChunk(t *testing.T, key byte) []byte {
	t.Helper()
	enc := func(op byte) byte { return byte(uint32(op) * uint32(key) % 256) }
	var out []byte
	out = append(out, 6, 0)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(1)...)
	out = append(out, 2, 0, 0, 0, 0)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(2)...)
	out = append(out, enc(byte(OpNOP)), 0, 0, 0)
	out = append(out, enc(byte(OpRETURN)), 0, 1, 0)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(0)...)
	out = append(out, leb128Enc(0)...)
	out = append(out, 0, 0)
	out = append(out, leb128Enc(0)...)
	return out
}

func TestParseAndDecompileMinimal(t *testing.T) {
	bc := minimalChunk(t, 1)
	chunk, err := ParseBytecode(bc, 1)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if len(chunk.Protos) != 1 || chunk.MainProto != 0 {
		t.Fatalf("chunk = %+v", chunk)
	}
	src, err := TryDecompileBytecode(bc, 1, nil, DecompileOptions{})
	if err != nil {
		t.Fatalf("decompile: %v", err)
	}
	if !strings.Contains(src, "return") {
		t.Fatalf("source missing return: %q", src)
	}
}

func TestTryDecompileErrorOnEmpty(t *testing.T) {
	if _, err := TryDecompileBytecode(nil, 1, nil, DecompileOptions{}); err == nil {
		t.Fatal("empty input should fail")
	}
	if s := DecompileBytecode(nil, 1, nil, DecompileOptions{}); !strings.HasPrefix(s, "-- decompile failed:") {
		t.Fatalf("fallback = %q", s)
	}
}

func TestOptionsBits(t *testing.T) {
	o := DecompileOptions{DontReuseVar: true, AssumeNoNaN: true, CompactAnnotations: true}
	if o.Bits() != (1<<0 | 1<<2 | 1<<6) {
		t.Fatalf("bits = %x", o.Bits())
	}
	back := OptionsFromBits(o.Bits())
	if back != o {
		t.Fatalf("roundtrip = %+v", back)
	}
}

func TestDecompileBatchOrder(t *testing.T) {
	good := minimalChunk(t, 1)
	name := "scriptA"
	items := []BatchInput{
		{Bytecode: good, EncodeKey: 1, ScriptName: &name},
		{Bytecode: []byte{9}, EncodeKey: 1},
		{Bytecode: good, EncodeKey: 1},
		{Bytecode: nil, EncodeKey: 1},
	}
	res := DecompileBatch(items, DecompileOptions{})
	if len(res) != len(items) {
		t.Fatalf("len = %d", len(res))
	}
	if !res[0].OK || !strings.Contains(res[0].Source, "scriptA") {
		t.Fatalf("item 0 = %+v", res[0])
	}
	if res[1].OK || res[1].Err == "" {
		t.Fatalf("item 1 should fail: %+v", res[1])
	}
	if !res[2].OK || res[2].Source != res[0].Source && !strings.Contains(res[2].Source, "return") {
		t.Fatalf("item 2 = %+v", res[2])
	}
	if res[3].OK {
		t.Fatal("item 3 should fail")
	}
	if res[0].Source != DecompileBytecode(good, 1, &name, DecompileOptions{}) {
		t.Fatal("batch output differs from single decompile")
	}
}
