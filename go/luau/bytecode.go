package luau

import (
	"encoding/binary"
	"errors"
	"fmt"
	"math"
)

const (
	ConstNil = iota
	ConstBool
	ConstNumber
	ConstString
	ConstImport
	ConstTable
	ConstClosure
	ConstVector
	ConstTableWithConsts
	ConstInteger
	ConstClassShape
	ConstVectorD
)

type TablePair struct {
	Key   uint64
	Value int32
}

type Constant struct {
	Tag         byte
	Bool        bool
	Num         float64
	StrIdx      uint64
	Import      uint32
	TableIdx    []uint64
	ClosureIdx  uint64
	Vec         [4]float32
	VecD        [4]float64
	Int         int64
	TableConsts []TablePair
}

type DebugLocal struct {
	Name    string
	NameIdx uint64
	StartPC uint32
	EndPC   uint32
	Reg     byte
}

type Proto struct {
	MaxStack    byte
	NumParams   byte
	NumUpvals   byte
	Variadic    bool
	Flags       byte
	Insns       []Insn
	Consts      []Constant
	Children    []uint64
	LineDefined int
	FuncName    *string
	DebugLocals []DebugLocal
	DebugUpvals []string
}

type BytecodeChunk struct {
	Version      byte
	TypesVersion byte
	Strings      [][]byte
	Protos       []*Proto
	MainProto    uint64
}

type Insn struct {
	Op     Opcode
	A      byte
	B, C   byte
	D      int16
	E      int32
	Aux    uint32
	HasAux bool
	PC     int
}

func isABCForm(op Opcode) bool {
	switch op {
	case OpLOADNIL, OpLOADB, OpMOVE,
		OpGETUPVAL, OpSETUPVAL, OpCLOSEUPVALS,
		OpGETTABLE, OpSETTABLE,
		OpGETTABLEKS, OpSETTABLEKS,
		OpGETTABLEN, OpSETTABLEN,
		OpNAMECALL, OpCALL,
		OpADD, OpSUB, OpMUL, OpDIV, OpMOD, OpPOW,
		OpADDK, OpSUBK, OpMULK, OpDIVK, OpMODK, OpPOWK,
		OpAND, OpOR, OpANDK, OpORK, OpCONCAT,
		OpNOT, OpMINUS, OpLENGTH,
		OpNEWTABLE, OpSETLIST, OpFORGLOOP, OpLOADKX,
		OpFASTCALL, OpFASTCALL1, OpFASTCALL2, OpFASTCALL2K, OpFASTCALL3,
		OpNATIVECALL, OpGETVARARGS, OpPREPVARARGS, OpCAPTURE,
		OpSUBRK, OpDIVRK,
		OpGETUDATAKS, OpSETUDATAKS, OpNAMECALLUDATA, OpNEWCLASSMEMBER,
		OpCALLFB, OpFASTPCALL,
		OpGETGLOBAL, OpSETGLOBAL, OpGETIMPORT,
		OpJUMPIFEQ, OpJUMPIFLE, OpJUMPIFLT,
		OpJUMPIFNOTEQ, OpJUMPIFNOTLE, OpJUMPIFNOTLT,
		OpJUMPXEQKNIL, OpJUMPXEQKB, OpJUMPXEQKN, OpJUMPXEQKS:
		return true
	}
	return false
}

func decodeInsn(word uint32, key byte) (Insn, error) {
	opByte := byte(uint32(byte(word)) * uint32(key) % 256)
	var ins Insn
	if opByte == 97 {
		ins.Op = OpNOP
		ins.PC = -1
		return ins, nil
	}
	op, ok := OpcodeFromByte(opByte)
	if !ok {
		return ins, fmt.Errorf("instruction: unknown opcode %d", opByte)
	}
	ins.Op = op
	a := byte(word >> 8)
	b := byte(word >> 16)
	c := byte(word >> 24)
	switch {
	case op == OpJUMPX || op == OpCOVERAGE:
		ins.E = int32(word) >> 8
	case op == OpLOADNIL || op == OpLOADB || op == OpMOVE ||
		op == OpGETUPVAL || op == OpSETUPVAL || op == OpCLOSEUPVALS ||
		op == OpGETTABLE || op == OpSETTABLE ||
		op == OpGETTABLEKS || op == OpSETTABLEKS ||
		op == OpGETTABLEN || op == OpSETTABLEN ||
		op == OpNAMECALL || op == OpCALL || op == OpRETURN ||
		op == OpADD || op == OpSUB || op == OpMUL || op == OpDIV || op == OpMOD || op == OpPOW ||
		op == OpADDK || op == OpSUBK || op == OpMULK || op == OpDIVK || op == OpMODK || op == OpPOWK ||
		op == OpAND || op == OpOR || op == OpANDK || op == OpORK || op == OpCONCAT ||
		op == OpNOT || op == OpMINUS || op == OpLENGTH ||
		op == OpNEWTABLE || op == OpSETLIST || op == OpFORGLOOP || op == OpLOADKX ||
		op == OpFASTCALL || op == OpFASTCALL1 || op == OpFASTCALL2 || op == OpFASTCALL2K || op == OpFASTCALL3 ||
		op == OpNATIVECALL || op == OpGETVARARGS || op == OpPREPVARARGS || op == OpCAPTURE ||
		op == OpSUBRK || op == OpDIVRK ||
		op == OpGETGLOBAL || op == OpSETGLOBAL || op == OpGETIMPORT ||
		op == OpJUMPIFEQ || op == OpJUMPIFLE || op == OpJUMPIFLT ||
		op == OpJUMPIFNOTEQ || op == OpJUMPIFNOTLE || op == OpJUMPIFNOTLT ||
		op == OpJUMPXEQKNIL || op == OpJUMPXEQKB || op == OpJUMPXEQKN || op == OpJUMPXEQKS ||
		op == OpGETUDATAKS || op == OpSETUDATAKS || op == OpNAMECALLUDATA || op == OpNEWCLASSMEMBER ||
		op == OpCALLFB || op == OpFASTPCALL:
		ins.A, ins.B, ins.C = a, b, c
		ins.D = int16(word >> 16)
	default:
		ins.A = a
		ins.D = int16(word >> 16)
		ins.B = b
		ins.C = c
	}
	_ = isABCForm
	return ins, nil
}

type cursor struct {
	b   []byte
	pos int
}

func (c *cursor) rest() []byte { return c.b[c.pos:] }

func (c *cursor) take(n int, what string) ([]byte, error) {
	if n < 0 || len(c.rest()) < n {
		return nil, fmt.Errorf("%s: truncated", what)
	}
	out := c.b[c.pos : c.pos+n]
	c.pos += n
	return out, nil
}

func (c *cursor) u8(what string) (byte, error) {
	b, err := c.take(1, what)
	if err != nil {
		return 0, err
	}
	return b[0], nil
}

func (c *cursor) u32le(what string) (uint32, error) {
	b, err := c.take(4, what)
	if err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint32(b), nil
}

func (c *cursor) f32le(what string) (float32, error) {
	v, err := c.u32le(what)
	if err != nil {
		return 0, err
	}
	return math.Float32frombits(v), nil
}

func (c *cursor) f64le(what string) (float64, error) {
	b, err := c.take(8, what)
	if err != nil {
		return 0, err
	}
	return math.Float64frombits(binary.LittleEndian.Uint64(b)), nil
}

func (c *cursor) uleb(what string) (uint64, error) {
	v, n, err := ReadULEB128(c.rest())
	if err != nil {
		return 0, fmt.Errorf("%s: %w", what, err)
	}
	c.pos += n
	return v, nil
}

func (c *cursor) str(what string) ([]byte, error) {
	s, n, err := ReadString(c.rest())
	if err != nil {
		return nil, fmt.Errorf("%s: %w", what, err)
	}
	c.pos += n
	return s, nil
}

func parseConstant(c *cursor, version byte) (Constant, error) {
	var k Constant
	tag, err := c.u8("constant tag")
	if err != nil {
		return k, err
	}
	k.Tag = tag
	switch tag {
	case ConstNil:
	case ConstBool:
		v, err := c.u8("bool constant")
		if err != nil {
			return k, err
		}
		k.Bool = v != 0
	case ConstNumber:
		v, err := c.f64le("number constant")
		if err != nil {
			return k, err
		}
		k.Num = v
	case ConstString:
		v, err := c.uleb("string constant")
		if err != nil {
			return k, err
		}
		k.StrIdx = v
	case ConstImport:
		v, err := c.u32le("import constant")
		if err != nil {
			return k, err
		}
		k.Import = v
	case ConstTable:
		n, err := c.uleb("table constant count")
		if err != nil {
			return k, err
		}
		if n > uint64(len(c.rest())) {
			return k, errors.New("table constant: count exceeds input")
		}
		k.TableIdx = make([]uint64, 0, n)
		for i := uint64(0); i < n; i++ {
			v, err := c.uleb("table constant key")
			if err != nil {
				return k, err
			}
			k.TableIdx = append(k.TableIdx, v)
		}
	case ConstClosure:
		v, err := c.uleb("closure constant")
		if err != nil {
			return k, err
		}
		k.ClosureIdx = v
	case ConstVector:
		for i := 0; i < 4; i++ {
			v, err := c.f32le("vector constant")
			if err != nil {
				return k, err
			}
			k.Vec[i] = v
		}
	case ConstTableWithConsts:
		n, err := c.uleb("table-with-consts count")
		if err != nil {
			return k, err
		}
		if n > uint64(len(c.rest())) {
			return k, errors.New("table-with-consts: count exceeds input")
		}
		for i := uint64(0); i < n; i++ {
			key, err := c.uleb("table-with-consts key")
			if err != nil {
				return k, err
			}
			raw, err := c.u32le("table-with-consts value")
			if err != nil {
				return k, err
			}
			k.TableConsts = append(k.TableConsts, TablePair{Key: key, Value: int32(raw)})
		}
	case ConstInteger:
		neg, err := c.u8("integer sign")
		if err != nil {
			return k, err
		}
		rest := c.rest()
		i := 0
		for i < len(rest) && i < 9 && rest[i]&0x80 != 0 {
			i++
		}
		if i == 9 && len(rest) > 9 && rest[9] > 1 {
			return k, errors.New("integer constant: overflow")
		}
		mag, err := c.uleb("integer magnitude")
		if err != nil {
			return k, err
		}
		limit := uint64(math.MaxInt64)
		if neg != 0 {
			limit++
		}
		if mag > limit {
			return k, errors.New("integer constant: out of range")
		}
		if neg != 0 {
			k.Int = int64(mag)
			k.Int = -k.Int
			if mag == 1<<63 {
				k.Int = math.MinInt64
			}
		} else {
			k.Int = int64(mag)
		}
	case ConstClassShape:
		classID, err := c.uleb("class name id")
		if err != nil {
			return k, err
		}
		_ = classID
		np, err := c.uleb("class property count")
		if err != nil {
			return k, err
		}
		nm, err := c.uleb("class method count")
		if err != nil {
			return k, err
		}
		total := np + nm
		if total < np {
			return k, errors.New("class shape: count overflow")
		}
		if total > uint64(len(c.rest())) {
			return k, errors.New("class shape: count exceeds input")
		}
		for i := uint64(0); i < total; i++ {
			if _, err := c.uleb("class member"); err != nil {
				return k, err
			}
		}
	case ConstVectorD:
		if version < 13 {
			return k, fmt.Errorf("vectorD constant: unsupported in version %d", version)
		}
		for i := 0; i < 4; i++ {
			v, err := c.f64le("vectorD constant")
			if err != nil {
				return k, err
			}
			k.VecD[i] = v
		}
	default:
		return k, fmt.Errorf("constant: unknown tag %d", tag)
	}
	return k, nil
}

func parseProto(c *cursor, key byte, version byte, strings [][]byte) (*Proto, error) {
	p := &Proto{}
	maxStack, err := c.u8("max stack size")
	if err != nil {
		return nil, err
	}
	p.MaxStack = maxStack
	numParams, err := c.u8("num params")
	if err != nil {
		return nil, err
	}
	p.NumParams = numParams
	numUpvals, err := c.u8("num upvalues")
	if err != nil {
		return nil, err
	}
	p.NumUpvals = numUpvals
	vararg, err := c.u8("vararg")
	if err != nil {
		return nil, err
	}
	p.Variadic = vararg != 0
	flags, err := c.u8("proto flags")
	if err != nil {
		return nil, err
	}
	p.Flags = flags
	typeLen, err := c.uleb("type info length")
	if err != nil {
		return nil, err
	}
	if _, err := c.take(int(typeLen), "type info"); err != nil {
		return nil, err
	}
	nIns, err := c.uleb("instruction count")
	if err != nil {
		return nil, err
	}
	if nIns == 0 {
		return nil, errors.New("proto: empty instruction stream")
	}
	if nIns > uint64(len(c.rest())/4) {
		return nil, errors.New("proto: instruction count exceeds input")
	}
	raw, err := c.take(int(nIns)*4, "instructions")
	if err != nil {
		return nil, err
	}
	words := make([]uint32, nIns)
	for i := range words {
		words[i] = binary.LittleEndian.Uint32(raw[i*4:])
	}
	insns := make([]Insn, 0, nIns)
	pc := 0
	for i := 0; i < len(words); i++ {
		ins, err := decodeInsn(words[i], key)
		if err != nil {
			return nil, err
		}
		ins.PC = pc
		pc++
		if HasAux(ins.Op) {
			i++
			if i >= len(words) {
				return nil, fmt.Errorf("proto: truncated aux word for %s", ins.Op)
			}
			ins.Aux = words[i]
			ins.HasAux = true
			insns = append(insns, ins)
			auxPad := Insn{Op: OpNOP, PC: pc}
			pc++
			insns = append(insns, auxPad)
		} else {
			insns = append(insns, ins)
		}
	}
	p.Insns = insns
	nConsts, err := c.uleb("constant count")
	if err != nil {
		return nil, err
	}
	if nConsts > uint64(len(c.rest())) {
		return nil, errors.New("proto: constant count exceeds input")
	}
	for i := uint64(0); i < nConsts; i++ {
		k, err := parseConstant(c, version)
		if err != nil {
			return nil, err
		}
		p.Consts = append(p.Consts, k)
	}
	nChildren, err := c.uleb("child proto count")
	if err != nil {
		return nil, err
	}
	if nChildren > uint64(len(c.rest())) {
		return nil, errors.New("proto: child count exceeds input")
	}
	for i := uint64(0); i < nChildren; i++ {
		v, err := c.uleb("child proto")
		if err != nil {
			return nil, err
		}
		p.Children = append(p.Children, v)
	}
	lineDef, err := c.uleb("line defined")
	if err != nil {
		return nil, err
	}
	p.LineDefined = int(lineDef)
	nameIdx, err := c.uleb("function name")
	if err != nil {
		return nil, err
	}
	if nameIdx != 0 && nameIdx-1 < uint64(len(strings)) {
		s := string(strings[nameIdx-1])
		p.FuncName = &s
	}
	hasLine, err := c.u8("line info flag")
	if err != nil {
		return nil, err
	}
	if hasLine != 0 {
		gap, err := c.u8("line gap")
		if err != nil {
			return nil, err
		}
		if gap >= 64 {
			return nil, errors.New("proto: invalid line gap")
		}
		if _, err := c.take(int(nIns), "line info"); err != nil {
			return nil, err
		}
		absCount := (int(nIns)-1)>>gap + 1
		if _, err := c.take(absCount*4, "abs line info"); err != nil {
			return nil, err
		}
	}
	hasDebug, err := c.u8("debug info flag")
	if err != nil {
		return nil, err
	}
	if hasDebug != 0 {
		nLocals, err := c.uleb("debug local count")
		if err != nil {
			return nil, err
		}
		if nLocals > uint64(len(c.rest())) {
			return nil, errors.New("proto: debug local count exceeds input")
		}
		for i := uint64(0); i < nLocals; i++ {
			ni, err := c.uleb("debug local name")
			if err != nil {
				return nil, err
			}
			sp, err := c.uleb("debug local start")
			if err != nil {
				return nil, err
			}
			ep, err := c.uleb("debug local end")
			if err != nil {
				return nil, err
			}
			reg, err := c.u8("debug local reg")
			if err != nil {
				return nil, err
			}
			dl := DebugLocal{NameIdx: ni, StartPC: uint32(sp), EndPC: uint32(ep), Reg: reg}
			if ni != 0 && ni-1 < uint64(len(strings)) {
				dl.Name = string(strings[ni-1])
			}
			p.DebugLocals = append(p.DebugLocals, dl)
		}
		nUp, err := c.uleb("debug upvalue count")
		if err != nil {
			return nil, err
		}
		if nUp > uint64(len(c.rest())) {
			return nil, errors.New("proto: debug upvalue count exceeds input")
		}
		for i := uint64(0); i < nUp; i++ {
			ni, err := c.uleb("debug upvalue name")
			if err != nil {
				return nil, err
			}
			name := ""
			if ni != 0 && ni-1 < uint64(len(strings)) {
				name = string(strings[ni-1])
			}
			p.DebugUpvals = append(p.DebugUpvals, name)
		}
	}
	if version >= 11 {
		nFb, err := c.uleb("feedback count")
		if err != nil {
			return nil, err
		}
		if nFb > uint64(len(c.rest())) {
			return nil, errors.New("proto: feedback count exceeds input")
		}
		for i := uint64(0); i < nFb; i++ {
			t, err := c.u8("feedback slot type")
			if err != nil {
				return nil, err
			}
			if t != 0 {
				return nil, fmt.Errorf("proto: unknown feedback slot type %d", t)
			}
			if _, err := c.uleb("feedback pc"); err != nil {
				return nil, err
			}
		}
	}
	if version >= 12 && p.Flags&(1<<3) != 0 {
		if _, err := c.uleb("inline cost"); err != nil {
			return nil, err
		}
	}
	return p, nil
}

func ParseBytecode(input []byte, encodeKey byte) (*BytecodeChunk, error) {
	if len(input) == 0 {
		return nil, errors.New("bytecode: empty input")
	}
	c := &cursor{b: input}
	status, err := c.u8("status")
	if err != nil {
		return nil, err
	}
	switch {
	case status == 0:
		return nil, fmt.Errorf("compiler error: %s", string(c.rest()))
	case status >= 4 && status <= 14:
	default:
		return nil, fmt.Errorf("bytecode: unsupported version %d", status)
	}
	version := status
	chunk := &BytecodeChunk{Version: version}
	typesVer := byte(0)
	if version >= 4 {
		typesVer, err = c.u8("types version")
		if err != nil {
			return nil, err
		}
		if typesVer > 3 {
			return nil, fmt.Errorf("bytecode: unsupported types version %d", typesVer)
		}
	}
	chunk.TypesVersion = typesVer
	nStr, err := c.uleb("string count")
	if err != nil {
		return nil, err
	}
	if nStr > uint64(len(c.rest())) {
		return nil, errors.New("bytecode: string count exceeds input")
	}
	for i := uint64(0); i < nStr; i++ {
		s, err := c.str("string entry")
		if err != nil {
			return nil, err
		}
		cp := make([]byte, len(s))
		copy(cp, s)
		chunk.Strings = append(chunk.Strings, cp)
	}
	if typesVer == 3 {
		for {
			idx, err := c.u8("userdata type index")
			if err != nil {
				return nil, err
			}
			if idx == 0 {
				break
			}
			si, err := c.uleb("userdata type name")
			if err != nil {
				return nil, err
			}
			_ = si
		}
	}
	nProtos, err := c.uleb("proto count")
	if err != nil {
		return nil, err
	}
	if nProtos > uint64(len(c.rest())) {
		return nil, errors.New("bytecode: proto count exceeds input")
	}
	for i := uint64(0); i < nProtos; i++ {
		if version >= 12 {
			size, err := c.uleb("proto size")
			if err != nil {
				return nil, err
			}
			body, err := c.take(int(size), "proto body")
			if err != nil {
				return nil, err
			}
			sub := &cursor{b: body}
			p, err := parseProto(sub, encodeKey, version, chunk.Strings)
			if err != nil {
				return nil, fmt.Errorf("proto %d: %w", i, err)
			}
			chunk.Protos = append(chunk.Protos, p)
		} else {
			p, err := parseProto(c, encodeKey, version, chunk.Strings)
			if err != nil {
				return nil, fmt.Errorf("proto %d: %w", i, err)
			}
			chunk.Protos = append(chunk.Protos, p)
		}
	}
	main, err := c.uleb("main proto")
	if err != nil {
		return nil, err
	}
	if main >= uint64(len(chunk.Protos)) {
		return nil, fmt.Errorf("bytecode: main proto %d out of range", main)
	}
	chunk.MainProto = main
	return chunk, nil
}
