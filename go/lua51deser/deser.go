package lua51deser

import (
	"encoding/binary"
	"fmt"
	"math"
)

type Header struct {
	Version, Format, Endian, IntWidth, SizeTWidth, InstrWidth, NumWidth byte
	NumIntegral                                                         bool
}

func ParseHeader(b []byte) (Header, int, error) {
	var h Header
	if len(b) < 12 {
		return h, 0, fmt.Errorf("lua51deser: header too short: %d bytes", len(b))
	}
	if string(b[:4]) != "\x1bLua" {
		return h, 0, fmt.Errorf("lua51deser: bad magic %q", b[:4])
	}
	h.Version = b[4]
	h.Format = b[5]
	h.Endian = b[6]
	h.IntWidth = b[7]
	h.SizeTWidth = b[8]
	h.InstrWidth = b[9]
	h.NumWidth = b[10]
	switch b[11] {
	case 0:
	case 1:
		h.NumIntegral = true
	default:
		return h, 0, fmt.Errorf("lua51deser: bad integral flag %d", b[11])
	}
	if h.Version != 0x51 {
		return h, 0, fmt.Errorf("lua51deser: unsupported version %#x", h.Version)
	}
	if h.Format != 0 {
		return h, 0, fmt.Errorf("lua51deser: unsupported format %d", h.Format)
	}
	if h.Endian != 0 && h.Endian != 1 {
		return h, 0, fmt.Errorf("lua51deser: bad endian %d", h.Endian)
	}
	if h.IntWidth != 4 || h.SizeTWidth != 4 || h.InstrWidth != 4 || h.NumWidth != 8 {
		return h, 0, fmt.Errorf("lua51deser: bad widths int=%d size_t=%d instr=%d num=%d",
			h.IntWidth, h.SizeTWidth, h.InstrWidth, h.NumWidth)
	}
	if h.NumIntegral {
		return h, 0, fmt.Errorf("lua51deser: integral numbers not supported")
	}
	return h, 12, nil
}

type ValueKind int

const (
	Nil ValueKind = iota
	Bool
	Number
	String
)

type Value struct {
	Kind ValueKind
	Bool bool
	Num  float64
	Str  []byte
}

type Local struct {
	Name       []byte
	Begin, End uint32
}

type Layout int

const (
	BC Layout = iota
	BX
	BSx
)

type Opcode byte

const (
	Move Opcode = iota
	LoadConstant
	LoadBoolean
	LoadNil
	GetUpvalue
	GetGlobal
	GetIndex
	SetGlobal
	SetUpvalue
	SetIndex
	NewTable
	PrepMethodCall
	Add
	Subtract
	Multiply
	Divide
	Modulo
	Power
	Minus
	Not
	Length
	Concatenate
	Jump
	Equal
	LessThan
	LessThanOrEqual
	Test
	TestSet
	Call
	TailCall
	Return
	IterateNumericForLoop
	InitNumericForLoop
	IterateGenericForLoop
	SetList
	Close
	Closure
	VarArg
)

var opNames = [38]string{
	"Move", "LoadConstant", "LoadBoolean", "LoadNil", "GetUpvalue",
	"GetGlobal", "GetIndex", "SetGlobal", "SetUpvalue", "SetIndex",
	"NewTable", "PrepMethodCall", "Add", "Subtract", "Multiply",
	"Divide", "Modulo", "Power", "Minus", "Not", "Length",
	"Concatenate", "Jump", "Equal", "LessThan", "LessThanOrEqual",
	"Test", "TestSet", "Call", "TailCall", "Return",
	"IterateNumericForLoop", "InitNumericForLoop", "IterateGenericForLoop",
	"SetList", "Close", "Closure", "VarArg",
}

func (o Opcode) String() string {
	if int(o) < len(opNames) {
		return opNames[o]
	}
	return fmt.Sprintf("Opcode(%d)", byte(o))
}

const extraOp Opcode = 0xFF

type Insn struct {
	Op   Opcode
	A    byte
	B, C uint16
	Bx   uint32
	SBx  int32
}

func DecodeInsn(w uint32) Insn {
	return Insn{
		Op:  Opcode(w & 0x3F),
		A:   byte((w >> 6) & 0xFF),
		C:   uint16((w >> 14) & 0x1FF),
		B:   uint16((w >> 23) & 0x1FF),
		Bx:  (w >> 14) & 0x3FFFF,
		SBx: int32((w>>14)&0x3FFFF) - 131071,
	}
}

type Function struct {
	Name                                     []byte
	LineDefined, LastLineDefined             uint32
	NumUpvalues, NumParams, Vararg, MaxStack byte
	Code                                     []Insn
	Consts                                   []Value
	Closures                                 []*Function
	Positions                                []uint32
	Locals                                   []Local
	Upvalues                                 [][]byte
}

func (f *Function) IsVariadic() bool {
	return f.Vararg&2 != 0
}

func (f *Function) NeedsArgTable() bool {
	return f.Vararg&4 != 0
}

type Chunk struct {
	Func *Function
}

type reader struct {
	b   []byte
	off int
}

func (r *reader) remain() int { return len(r.b) - r.off }

func (r *reader) u8() (byte, error) {
	if r.remain() < 1 {
		return 0, fmt.Errorf("lua51deser: unexpected EOF at offset %d", r.off)
	}
	v := r.b[r.off]
	r.off++
	return v, nil
}

func (r *reader) u32() (uint32, error) {
	if r.remain() < 4 {
		return 0, fmt.Errorf("lua51deser: unexpected EOF at offset %d", r.off)
	}
	v := binary.LittleEndian.Uint32(r.b[r.off:])
	r.off += 4
	return v, nil
}

func (r *reader) f64() (float64, error) {
	if r.remain() < 8 {
		return 0, fmt.Errorf("lua51deser: unexpected EOF at offset %d", r.off)
	}
	v := math.Float64frombits(binary.LittleEndian.Uint64(r.b[r.off:]))
	r.off += 8
	return v, nil
}

func (r *reader) bytes(n uint32) ([]byte, error) {
	if uint64(n) > uint64(r.remain()) {
		return nil, fmt.Errorf("lua51deser: unexpected EOF at offset %d (need %d bytes)", r.off, n)
	}
	v := r.b[r.off : r.off+int(n)]
	r.off += int(n)
	return v, nil
}

func (r *reader) str() ([]byte, error) {
	n, err := r.u32()
	if err != nil {
		return nil, err
	}
	raw, err := r.bytes(n)
	if err != nil {
		return nil, err
	}
	if len(raw) > 0 {
		raw = raw[:len(raw)-1]
	}
	return raw, nil
}

func ParseChunk(b []byte) (*Chunk, error) {
	h, off, err := ParseHeader(b)
	if err != nil {
		return nil, err
	}
	if h.Endian != 1 {
		return nil, fmt.Errorf("lua51deser: only little-endian chunks supported")
	}
	r := &reader{b: b, off: off}
	fn, err := parseFunction(r)
	if err != nil {
		return nil, err
	}
	return &Chunk{Func: fn}, nil
}

func parseFunction(r *reader) (*Function, error) {
	f := &Function{}
	var err error
	if f.Name, err = r.str(); err != nil {
		return nil, fmt.Errorf("lua51deser: function name: %w", err)
	}
	if f.LineDefined, err = r.u32(); err != nil {
		return nil, fmt.Errorf("lua51deser: line defined: %w", err)
	}
	if f.LastLineDefined, err = r.u32(); err != nil {
		return nil, fmt.Errorf("lua51deser: last line defined: %w", err)
	}
	if f.NumUpvalues, err = r.u8(); err != nil {
		return nil, fmt.Errorf("lua51deser: upvalue count: %w", err)
	}
	if f.NumParams, err = r.u8(); err != nil {
		return nil, fmt.Errorf("lua51deser: param count: %w", err)
	}
	if f.Vararg, err = r.u8(); err != nil {
		return nil, fmt.Errorf("lua51deser: vararg: %w", err)
	}
	if f.MaxStack, err = r.u8(); err != nil {
		return nil, fmt.Errorf("lua51deser: max stack: %w", err)
	}
	if f.Code, err = parseCode(r); err != nil {
		return nil, err
	}
	if f.Consts, err = parseConsts(r); err != nil {
		return nil, err
	}
	var nclosures uint32
	if nclosures, err = r.u32(); err != nil {
		return nil, fmt.Errorf("lua51deser: closure count: %w", err)
	}
	for i := uint32(0); i < nclosures; i++ {
		sub, err := parseFunction(r)
		if err != nil {
			return nil, fmt.Errorf("lua51deser: closure %d: %w", i, err)
		}
		f.Closures = append(f.Closures, sub)
	}
	if r.remain() > 0 {
		if f.Positions, err = parsePositions(r); err != nil {
			return nil, err
		}
	}
	if r.remain() > 0 {
		if f.Locals, err = parseLocals(r); err != nil {
			return nil, err
		}
	}
	if r.remain() > 0 {
		if f.Upvalues, err = parseUpvalues(r); err != nil {
			return nil, err
		}
	}
	return f, nil
}

func parseCode(r *reader) ([]Insn, error) {
	n, err := r.u32()
	if err != nil {
		return nil, fmt.Errorf("lua51deser: code length: %w", err)
	}
	if uint64(n)*4 > uint64(r.remain()) {
		return nil, fmt.Errorf("lua51deser: code truncated: %d words", n)
	}
	words := make([]uint32, n)
	for i := range words {
		words[i] = binary.LittleEndian.Uint32(r.b[r.off:])
		r.off += 4
	}
	code := make([]Insn, 0, n)
	for i := 0; i < len(words); i++ {
		ins := DecodeInsn(words[i])
		if ins.Op > VarArg {
			return nil, fmt.Errorf("lua51deser: unknown opcode %d at pc %d", byte(ins.Op), len(code))
		}
		if ins.Op == SetList && ins.C == 0 {
			if i+1 >= len(words) {
				return nil, fmt.Errorf("lua51deser: SETLIST at pc %d missing extended block", len(code))
			}
			ext := words[i+1]
			if ext == 0 {
				return nil, fmt.Errorf("lua51deser: SETLIST at pc %d has zero extended block", len(code))
			}
			ins.Bx = ext
			ins.C = uint16(ext)
			code = append(code, ins, Insn{Op: extraOp})
			i++
			continue
		}
		code = append(code, ins)
	}
	return code, nil
}

func parseConsts(r *reader) ([]Value, error) {
	n, err := r.u32()
	if err != nil {
		return nil, fmt.Errorf("lua51deser: const count: %w", err)
	}
	var out []Value
	for i := uint32(0); i < n; i++ {
		kind, err := r.u8()
		if err != nil {
			return nil, fmt.Errorf("lua51deser: const %d kind: %w", i, err)
		}
		switch kind {
		case 0:
			out = append(out, Value{Kind: Nil})
		case 1:
			v, err := r.u8()
			if err != nil {
				return nil, fmt.Errorf("lua51deser: const %d bool: %w", i, err)
			}
			out = append(out, Value{Kind: Bool, Bool: v != 0})
		case 3:
			v, err := r.f64()
			if err != nil {
				return nil, fmt.Errorf("lua51deser: const %d number: %w", i, err)
			}
			out = append(out, Value{Kind: Number, Num: v})
		case 4:
			sn, err := r.u32()
			if err != nil {
				return nil, fmt.Errorf("lua51deser: const %d string length: %w", i, err)
			}
			if sn == 0 {
				return nil, fmt.Errorf("lua51deser: const %d empty string", i)
			}
			raw, err := r.bytes(sn)
			if err != nil {
				return nil, fmt.Errorf("lua51deser: const %d string: %w", i, err)
			}
			out = append(out, Value{Kind: String, Str: raw[:len(raw)-1]})
		default:
			return nil, fmt.Errorf("lua51deser: const %d bad kind %d", i, kind)
		}
	}
	return out, nil
}

func parsePositions(r *reader) ([]uint32, error) {
	n, err := r.u32()
	if err != nil {
		return nil, fmt.Errorf("lua51deser: position count: %w", err)
	}
	if uint64(n)*4 > uint64(r.remain()) {
		return nil, fmt.Errorf("lua51deser: positions truncated: %d entries", n)
	}
	out := make([]uint32, n)
	for i := range out {
		out[i] = binary.LittleEndian.Uint32(r.b[r.off:])
		r.off += 4
	}
	return out, nil
}

func parseLocals(r *reader) ([]Local, error) {
	n, err := r.u32()
	if err != nil {
		return nil, fmt.Errorf("lua51deser: local count: %w", err)
	}
	var out []Local
	for i := uint32(0); i < n; i++ {
		name, err := r.str()
		if err != nil {
			return nil, fmt.Errorf("lua51deser: local %d name: %w", i, err)
		}
		begin, err := r.u32()
		if err != nil {
			return nil, fmt.Errorf("lua51deser: local %d begin: %w", i, err)
		}
		end, err := r.u32()
		if err != nil {
			return nil, fmt.Errorf("lua51deser: local %d end: %w", i, err)
		}
		out = append(out, Local{Name: name, Begin: begin, End: end})
	}
	return out, nil
}

func parseUpvalues(r *reader) ([][]byte, error) {
	n, err := r.u32()
	if err != nil {
		return nil, fmt.Errorf("lua51deser: upvalue count: %w", err)
	}
	var out [][]byte
	for i := uint32(0); i < n; i++ {
		name, err := r.str()
		if err != nil {
			return nil, fmt.Errorf("lua51deser: upvalue %d: %w", i, err)
		}
		out = append(out, name)
	}
	return out, nil
}
