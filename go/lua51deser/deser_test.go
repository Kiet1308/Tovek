package lua51deser

import (
	"encoding/binary"
	"testing"
)

func validHeader() []byte {
	return []byte{0x1b, 'L', 'u', 'a', 0x51, 0, 1, 4, 4, 4, 8, 0}
}

func TestParseHeader(t *testing.T) {
	h, n, err := ParseHeader(validHeader())
	if err != nil {
		t.Fatalf("ParseHeader: %v", err)
	}
	if n != 12 {
		t.Fatalf("offset = %d, want 12", n)
	}
	if h.Version != 0x51 || h.Endian != 1 || h.NumIntegral {
		t.Fatalf("bad header: %+v", h)
	}
}

func TestDecodeInsnMove(t *testing.T) {
	w := uint32(0) | uint32(1)<<6 | uint32(2)<<23 | uint32(0)<<14
	ins := DecodeInsn(w)
	if ins.Op != Move || ins.A != 1 || ins.B != 2 {
		t.Fatalf("bad decode: %+v", ins)
	}
	if ins.Bx != uint32(2)<<9 || ins.SBx != int32(ins.Bx)-131071 {
		t.Fatalf("bad Bx/SBx: %+v", ins)
	}
}

func TestParseChunkRejectsBadMagic(t *testing.T) {
	bad := append([]byte{0x1b, 'L', 'u', 'X'}, make([]byte, 8)...)
	if _, err := ParseChunk(bad); err == nil {
		t.Fatal("expected error for bad magic")
	}
}

func TestParseChunkMinimal(t *testing.T) {
	b := validHeader()
	b = append(b, 0, 0, 0, 0)
	b = binary.LittleEndian.AppendUint32(b, 0)
	b = binary.LittleEndian.AppendUint32(b, 0)
	b = append(b, 0, 0, 0, 2)
	b = binary.LittleEndian.AppendUint32(b, 0)
	b = binary.LittleEndian.AppendUint32(b, 0)
	b = binary.LittleEndian.AppendUint32(b, 0)
	c, err := ParseChunk(b)
	if err != nil {
		t.Fatalf("ParseChunk: %v", err)
	}
	if c.Func == nil || c.Func.MaxStack != 2 {
		t.Fatalf("bad chunk: %+v", c.Func)
	}
}
