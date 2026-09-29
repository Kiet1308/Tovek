package luau

import (
	"fmt"
	"sort"
	"strconv"
	"strings"
	"sync"

	"github.com/kiet1308/tovek-go/ast"
	"github.com/kiet1308/tovek-go/cfg"
	"github.com/kiet1308/tovek-go/restructure"
)

const (
	DontReuseVar              = BitDontReuseVar
	NoSynthHelpers            = BitNoSynthHelpers
	AssumeNoNaN               = BitAssumeNoNaN
	StrictNoSyntheticControl  = BitStrictNoSyntheticControl
	EmitBindingProvenance     = BitEmitBindingProvenance
	SynthesizeArithmeticLoops = BitSynthesizeArithmeticLoops
	CompactAnnotations        = BitCompactAnnotations
)

type BatchInput struct {
	Bytecode   []byte
	EncodeKey  byte
	ScriptName *string
}

type BatchResult struct {
	Source string
	Err    string
	OK     bool
}

func DecompileBatch(items []BatchInput, opts DecompileOptions) []BatchResult {
	out := make([]BatchResult, len(items))
	var wg sync.WaitGroup
	for i := range items {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			defer func() {
				if r := recover(); r != nil {
					out[i] = BatchResult{Err: fmt.Sprintf("panic: %v", r)}
				}
			}()
			src, err := TryDecompileBytecode(items[i].Bytecode, items[i].EncodeKey, items[i].ScriptName, opts)
			if err != nil {
				out[i] = BatchResult{Err: err.Error()}
				return
			}
			out[i] = BatchResult{Source: src, OK: true}
		}(i)
	}
	wg.Wait()
	return out
}

func DecompileBytecode(bc []byte, key byte, scriptName *string, opts DecompileOptions) string {
	src, err := TryDecompileBytecode(bc, key, scriptName, opts)
	if err != nil {
		return "-- decompile failed: " + err.Error()
	}
	return src
}

func TryDecompileBytecode(bc []byte, key byte, scriptName *string, opts DecompileOptions) (src string, err error) {
	defer func() {
		if r := recover(); r != nil {
			src = ""
			err = fmt.Errorf("decompile panic: %v", r)
		}
	}()
	chunk, err := ParseBytecode(bc, key)
	if err != nil {
		return "", err
	}
	if opts.StrictNoSyntheticControl {
		for pi, p := range chunk.Protos {
			for i, ins := range p.Insns {
				if d, ok := branchDest(p, i, ins); ok && d <= i {
					return "", fmt.Errorf("proto %d: synthetic control flow not allowed", pi)
				}
			}
		}
	}
	var sb strings.Builder
	name := ""
	if scriptName != nil {
		name = *scriptName
	}
	if !opts.CompactAnnotations {
		if name != "" {
			fmt.Fprintf(&sb, "-- decompiled chunk %q (%d protos)\n", name, len(chunk.Protos))
		} else {
			fmt.Fprintf(&sb, "-- decompiled chunk (%d protos)\n", len(chunk.Protos))
		}
	}
	for i, p := range chunk.Protos {
		if uint64(i) == chunk.MainProto {
			continue
		}
		sb.WriteString(renderProto(chunk, i, p, name, opts))
	}
	if int(chunk.MainProto) < len(chunk.Protos) {
		sb.WriteString(renderMain(chunk, chunk.Protos[chunk.MainProto], name, opts))
	}
	return sb.String(), nil
}

func branchDest(p *Proto, idx int, ins Insn) (int, bool) {
	n := len(p.Insns)
	dest := func(off int64) (int, bool) {
		d := idx + 1 + int(off)
		if d < 0 || d >= n {
			return 0, false
		}
		return d, true
	}
	switch ins.Op {
	case OpJUMP, OpJUMPBACK:
		if ins.D == 0 {
			return 0, false
		}
		return dest(int64(ins.D))
	case OpJUMPX:
		if ins.E == 0 {
			return 0, false
		}
		return dest(int64(ins.E))
	case OpJUMPIF, OpJUMPIFNOT,
		OpJUMPIFEQ, OpJUMPIFLE, OpJUMPIFLT,
		OpJUMPIFNOTEQ, OpJUMPIFNOTLE, OpJUMPIFNOTLT,
		OpJUMPXEQKNIL, OpJUMPXEQKB, OpJUMPXEQKN, OpJUMPXEQKS,
		OpFORNPREP, OpFORNLOOP, OpFORGLOOP,
		OpFORGPREP, OpFORGPREPINEXT, OpFORGPREPNEXT:
		return dest(int64(ins.D))
	case OpLOADB:
		if ins.C == 0 {
			return 0, false
		}
		return dest(int64(ins.C))
	}
	return 0, false
}

func isUnconditional(op Opcode) bool {
	switch op {
	case OpJUMP, OpJUMPBACK, OpJUMPX, OpFORNPREP, OpFORGPREP, OpFORGPREPINEXT, OpFORGPREPNEXT:
		return true
	}
	return false
}

type lifter struct {
	chunk *Proto
	strs  [][]byte
	opts  DecompileOptions
	vers  map[byte]int
}

func (l *lifter) reg(n byte) string {
	if l.opts.DontReuseVar {
		return fmt.Sprintf("r_%d_v%d", n, l.vers[n])
	}
	return fmt.Sprintf("r_%d", n)
}

func (l *lifter) def(n byte) string {
	if l.opts.DontReuseVar {
		l.vers[n]++
		return fmt.Sprintf("r_%d_v%d", n, l.vers[n])
	}
	return fmt.Sprintf("r_%d", n)
}

func luaQuote(s string) string {
	var sb strings.Builder
	sb.WriteByte('"')
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch c {
		case '"':
			sb.WriteString("\\\"")
		case '\\':
			sb.WriteString("\\\\")
		case '\n':
			sb.WriteString("\\n")
		case '\r':
			sb.WriteString("\\r")
		case '\t':
			sb.WriteString("\\t")
		default:
			if c < 0x20 || c == 0x7f {
				fmt.Fprintf(&sb, "\\x%02x", c)
			} else {
				sb.WriteByte(c)
			}
		}
	}
	sb.WriteByte('"')
	return sb.String()
}

func (l *lifter) strAt(idx uint64) string {
	if idx == 0 || idx-1 >= uint64(len(l.strs)) {
		return `""`
	}
	return luaQuote(string(l.strs[idx-1]))
}

func (l *lifter) konst(k Constant) string {
	switch k.Tag {
	case ConstNil:
		return "nil"
	case ConstBool:
		if k.Bool {
			return "true"
		}
		return "false"
	case ConstNumber:
		return strconv.FormatFloat(k.Num, 'g', -1, 64)
	case ConstInteger:
		return strconv.FormatInt(k.Int, 10)
	case ConstString:
		return l.strAt(k.StrIdx)
	case ConstImport:
		return fmt.Sprintf("import_%d", k.Import)
	case ConstClosure:
		return fmt.Sprintf("__proto_%d", k.ClosureIdx)
	case ConstTable:
		return "{}"
	case ConstTableWithConsts:
		return "{}"
	case ConstVector:
		return fmt.Sprintf("vector.create(%s, %s, %s)",
			strconv.FormatFloat(float64(k.Vec[0]), 'g', -1, 32),
			strconv.FormatFloat(float64(k.Vec[1]), 'g', -1, 32),
			strconv.FormatFloat(float64(k.Vec[2]), 'g', -1, 32))
	case ConstVectorD:
		return fmt.Sprintf("vector.create(%s, %s, %s)",
			strconv.FormatFloat(k.VecD[0], 'g', -1, 64),
			strconv.FormatFloat(k.VecD[1], 'g', -1, 64),
			strconv.FormatFloat(k.VecD[2], 'g', -1, 64))
	default:
		return "nil"
	}
}

func (l *lifter) konstAt(idx int) string {
	if idx < 0 || idx >= len(l.chunk.Consts) {
		return "nil"
	}
	return l.konst(l.chunk.Consts[idx])
}

func (l *lifter) constStr(idx uint64) string {
	if idx >= uint64(len(l.chunk.Consts)) {
		return `"unk"`
	}
	if c := l.chunk.Consts[idx]; c.Tag == ConstString {
		return l.strAt(c.StrIdx)
	}
	return l.konst(l.chunk.Consts[idx])
}

func (l *lifter) importPath(aux uint32) string {
	count := (aux >> 30) & 3
	ids := []uint32{(aux >> 20) & 1023, (aux >> 10) & 1023, aux & 1023}
	var parts []string
	for i := uint32(0); i < count && int(i) < len(ids); i++ {
		parts = append(parts, l.importName(uint64(ids[i])))
	}
	if len(parts) == 0 {
		return "import"
	}
	out := parts[0]
	for _, p := range parts[1:] {
		if strings.HasPrefix(p, `"`) {
			out += "[" + p + "]"
		} else {
			out += "." + p
		}
	}
	return out
}

// importName resolves one import path segment to a bare global when it is a
// valid identifier (matching ast Global display), else a quoted string.
func (l *lifter) importName(idx uint64) string {
	if idx >= uint64(len(l.chunk.Consts)) {
		return `"unk"`
	}
	c := l.chunk.Consts[idx]
	if c.Tag != ConstString {
		return l.konst(c)
	}
	return globalName(l.strAt(c.StrIdx))
}

func globalName(quoted string) string {
	if len(quoted) >= 2 && quoted[0] == '"' && quoted[len(quoted)-1] == '"' {
		inner := quoted[1 : len(quoted)-1]
		if isValidName(inner) {
			return inner
		}
	}
	return quoted
}

func isValidName(s string) bool {
	if s == "" {
		return false
	}
	for i := 0; i < len(s); i++ {
		c := s[i]
		ok := c == '_' || c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || i > 0 && c >= '0' && c <= '9'
		if !ok {
			return false
		}
	}
	switch s {
	case "and", "break", "do", "else", "elseif", "end", "false", "for",
		"function", "if", "in", "local", "nil", "not", "or", "repeat",
		"return", "then", "true", "until", "while", "continue":
		return false
	}
	return true
}

func (l *lifter) closureTarget(d int16, children []uint64) string {
	if d < 0 || int(d) >= len(children) {
		return fmt.Sprintf("__proto_unk%d", d)
	}
	return fmt.Sprintf("__proto_%d", children[d])
}

func arithOp(op Opcode) string {
	switch op {
	case OpADD, OpADDK:
		return "+"
	case OpSUB, OpSUBK:
		return "-"
	case OpMUL, OpMULK:
		return "*"
	case OpDIV, OpDIVK:
		return "/"
	case OpIDIV, OpIDIVK:
		return "//"
	case OpMOD, OpMODK:
		return "%"
	case OpPOW, OpPOWK:
		return "^"
	case OpSUBRK:
		return "-"
	case OpDIVRK:
		return "/"
	}
	return "+"
}

func (l *lifter) liftInsn(p *Proto, idx int, ins Insn) []string {
	A := func() string { return l.reg(ins.A) }
	D := func() string { return l.def(ins.A) }
	dest, hasDest := branchDest(p, idx, ins)
	gl := ""
	if hasDest {
		gl = fmt.Sprintf("goto L%d", dest)
	}
	switch ins.Op {
	case OpNOP, OpCOVERAGE:
		return nil
	case OpBREAK:
		return []string{"-- break"}
	case OpLOADNIL:
		return []string{D() + " = nil"}
	case OpLOADB:
		s := []string{D() + fmt.Sprintf(" = %v", ins.B != 0)}
		if hasDest {
			s = append(s, gl)
		}
		return s
	case OpLOADN:
		return []string{D() + fmt.Sprintf(" = %d", ins.D)}
	case OpLOADK:
		return []string{D() + " = " + l.konstAt(int(ins.D))}
	case OpLOADKX:
		return []string{D() + " = " + l.konstAt(int(ins.Aux))}
	case OpMOVE:
		return []string{D() + " = " + l.reg(ins.B)}
	case OpGETGLOBAL:
		return []string{D() + " = " + l.constStr(uint64(ins.Aux))}
	case OpSETGLOBAL:
		return []string{l.constStr(uint64(ins.Aux)) + " = " + A()}
	case OpGETUPVAL:
		return []string{D() + fmt.Sprintf(" = uv_%d", ins.B)}
	case OpSETUPVAL:
		return []string{fmt.Sprintf("uv_%d = %s", ins.B, A())}
	case OpCLOSEUPVALS:
		return []string{fmt.Sprintf("-- close upvalues from %s", A())}
	case OpGETIMPORT:
		return []string{D() + " = " + l.importPath(ins.Aux)}
	case OpGETTABLE:
		return []string{D() + fmt.Sprintf(" = %s[%s]", l.reg(ins.B), l.reg(ins.C))}
	case OpSETTABLE:
		return []string{fmt.Sprintf("%s[%s] = %s", l.reg(ins.B), l.reg(ins.C), A())}
	case OpGETTABLEKS:
		return []string{D() + fmt.Sprintf(" = %s.%s", l.reg(ins.B), l.constStr(uint64(ins.Aux)))}
	case OpSETTABLEKS:
		return []string{fmt.Sprintf("%s.%s = %s", l.reg(ins.B), l.constStr(uint64(ins.Aux)), A())}
	case OpGETUDATAKS:
		return []string{D() + fmt.Sprintf(" = %s.%s", l.reg(ins.B), l.constStr(uint64(ins.Aux&0xffff)))}
	case OpSETUDATAKS:
		return []string{fmt.Sprintf("%s.%s = %s", l.reg(ins.B), l.constStr(uint64(ins.Aux&0xffff)), A())}
	case OpGETTABLEN:
		return []string{D() + fmt.Sprintf(" = %s[%d]", l.reg(ins.B), int(ins.C)+1)}
	case OpSETTABLEN:
		return []string{fmt.Sprintf("%s[%d] = %s", l.reg(ins.B), int(ins.C)+1, A())}
	case OpNEWCLOSURE:
		return []string{D() + " = " + l.closureTarget(ins.D, p.Children)}
	case OpDUPCLOSURE:
		return []string{D() + " = " + l.konstAt(int(ins.D))}
	case OpNAMECALL:
		m := l.constStr(uint64(ins.Aux))
		return []string{fmt.Sprintf("%s, %s = %s:%s, %s", D(), l.def(ins.A+1), l.reg(ins.B), strings.Trim(m, `"`), l.reg(ins.B))}
	case OpNAMECALLUDATA:
		m := l.constStr(uint64(ins.Aux & 0xffff))
		return []string{fmt.Sprintf("%s, %s = %s:%s, %s", D(), l.def(ins.A+1), l.reg(ins.B), strings.Trim(m, `"`), l.reg(ins.B))}
	case OpCALL, OpCALLFB:
		return []string{l.liftCall(ins, false)}
	case OpRETURN:
		if ins.B == 0 {
			return []string{fmt.Sprintf("return %s, ...", A())}
		}
		if ins.B == 1 {
			return []string{"return"}
		}
		parts := make([]string, 0, ins.B-1)
		for i := byte(0); i < ins.B-1; i++ {
			parts = append(parts, l.reg(ins.A+i))
		}
		return []string{"return " + strings.Join(parts, ", ")}
	case OpJUMP, OpJUMPBACK, OpJUMPX:
		if !hasDest {
			return []string{"-- jump"}
		}
		return []string{gl}
	case OpJUMPIF:
		return []string{fmt.Sprintf("if %s then %s end", A(), gl)}
	case OpJUMPIFNOT:
		return []string{fmt.Sprintf("if not %s then %s end", A(), gl)}
	case OpJUMPIFEQ:
		return []string{fmt.Sprintf("if %s == %s then %s end", A(), l.reg(byte(ins.Aux)), gl)}
	case OpJUMPIFLE:
		return []string{fmt.Sprintf("if %s <= %s then %s end", A(), l.reg(byte(ins.Aux)), gl)}
	case OpJUMPIFLT:
		return []string{fmt.Sprintf("if %s < %s then %s end", A(), l.reg(byte(ins.Aux)), gl)}
	case OpJUMPIFNOTEQ:
		return []string{fmt.Sprintf("if %s ~= %s then %s end", A(), l.reg(byte(ins.Aux)), gl)}
	case OpJUMPIFNOTLE:
		return []string{fmt.Sprintf("if %s > %s then %s end", A(), l.reg(byte(ins.Aux)), gl)}
	case OpJUMPIFNOTLT:
		return []string{fmt.Sprintf("if %s >= %s then %s end", A(), l.reg(byte(ins.Aux)), gl)}
	case OpJUMPXEQKNIL:
		cond := fmt.Sprintf("%s == nil", A())
		if ins.Aux&2 != 0 {
			cond = fmt.Sprintf("%s ~= nil", A())
		}
		return []string{fmt.Sprintf("if %s then %s end", cond, gl)}
	case OpJUMPXEQKB:
		val := "false"
		if ins.Aux&1 != 0 {
			val = "true"
		}
		op := "=="
		if ins.Aux&2 != 0 {
			op = "~="
		}
		return []string{fmt.Sprintf("if %s %s %s then %s end", A(), op, val, gl)}
	case OpJUMPXEQKN, OpJUMPXEQKS:
		op := "=="
		if ins.Aux&(1<<31) != 0 {
			op = "~="
		}
		return []string{fmt.Sprintf("if %s %s %s then %s end", A(), op, l.konstAt(int(ins.Aux&0xffffff)), gl)}
	case OpADD, OpSUB, OpMUL, OpDIV, OpMOD, OpPOW, OpIDIV:
		return []string{fmt.Sprintf("%s = %s %s %s", D(), l.reg(ins.B), arithOp(ins.Op), l.reg(ins.C))}
	case OpADDK, OpSUBK, OpMULK, OpDIVK, OpMODK, OpPOWK, OpIDIVK:
		return []string{fmt.Sprintf("%s = %s %s %s", D(), l.reg(ins.B), arithOp(ins.Op), l.konstAt(int(ins.C)))}
	case OpSUBRK, OpDIVRK:
		return []string{fmt.Sprintf("%s = %s %s %s", D(), l.konstAt(int(ins.C)), arithOp(ins.Op), l.reg(ins.B))}
	case OpAND, OpOR:
		op := "and"
		if ins.Op == OpOR {
			op = "or"
		}
		return []string{fmt.Sprintf("%s = %s %s %s", D(), l.reg(ins.B), op, l.reg(ins.C))}
	case OpANDK, OpORK:
		op := "and"
		if ins.Op == OpORK {
			op = "or"
		}
		return []string{fmt.Sprintf("%s = %s %s %s", D(), l.reg(ins.B), op, l.konstAt(int(ins.C)))}
	case OpCONCAT:
		parts := make([]string, 0, int(ins.C)-int(ins.B)+1)
		for r := ins.B; ; r++ {
			parts = append(parts, l.reg(r))
			if r == ins.C {
				break
			}
		}
		return []string{D() + " = " + strings.Join(parts, " .. ")}
	case OpNOT:
		return []string{D() + " = not " + l.reg(ins.B)}
	case OpMINUS:
		return []string{D() + " = -" + l.reg(ins.B)}
	case OpLENGTH:
		return []string{D() + " = #" + l.reg(ins.B)}
	case OpNEWTABLE:
		return []string{D() + " = {}"}
	case OpDUPTABLE:
		return []string{D() + " = " + l.konstAt(int(ins.D)) + " -- duptable"}
	case OpSETLIST:
		return []string{fmt.Sprintf("-- setlist %s from %s count %d aux %d", A(), l.reg(ins.B), ins.C, ins.Aux)}
	case OpFORNPREP, OpFORNLOOP:
		if hasDest {
			return []string{fmt.Sprintf("%s -- fornum %s", gl, A())}
		}
		return []string{fmt.Sprintf("-- fornum %s", A())}
	case OpFORGLOOP, OpFORGPREP, OpFORGPREPINEXT, OpFORGPREPNEXT:
		if hasDest {
			return []string{fmt.Sprintf("%s -- forgen %s", gl, A())}
		}
		return []string{fmt.Sprintf("-- forgen %s", A())}
	case OpGETVARARGS:
		if ins.B == 0 {
			return []string{fmt.Sprintf("%s, ... = ...", D())}
		}
		parts := make([]string, 0, ins.B-1)
		for i := byte(0); i < ins.B-1; i++ {
			parts = append(parts, l.def(ins.A+i))
		}
		return []string{strings.Join(parts, ", ") + " = ..."}
	case OpPREPVARARGS:
		return []string{fmt.Sprintf("-- prepvarargs %s", A())}
	case OpFASTCALL, OpFASTCALL1, OpFASTCALL2, OpFASTCALL2K, OpFASTCALL3, OpFASTPCALL:
		return []string{fmt.Sprintf("-- fastcall %d", ins.A)}
	case OpCAPTURE:
		return []string{fmt.Sprintf("-- capture %d %s", ins.A, l.reg(ins.B))}
	case OpNATIVECALL:
		return []string{"-- nativecall"}
	case OpNEWCLASSMEMBER:
		return []string{fmt.Sprintf("-- newclassmember %s.%s", A(), l.constStr(uint64(ins.Aux)))}
	case OpCMPPROTO:
		return []string{fmt.Sprintf("-- cmpproto %s aux %d", A(), ins.Aux)}
	default:
		return []string{fmt.Sprintf("-- op %s", ins.Op)}
	}
}

func (l *lifter) liftCall(ins Insn, _ bool) string {
	var args []string
	if ins.B == 0 {
		args = []string{l.reg(ins.A + 1), "..."}
	} else {
		for i := byte(1); i < ins.B; i++ {
			args = append(args, l.reg(ins.A+i))
		}
	}
	call := fmt.Sprintf("%s(%s)", l.reg(ins.A), strings.Join(args, ", "))
	switch {
	case ins.C == 0:
		return fmt.Sprintf("%s, ... = %s", l.def(ins.A), call)
	case ins.C == 1:
		return call
	default:
		rets := make([]string, 0, ins.C-1)
		for i := byte(0); i < ins.C-1; i++ {
			rets = append(rets, l.def(ins.A+i))
		}
		return strings.Join(rets, ", ") + " = " + call
	}
}

func paramsOf(p *Proto) string {
	parts := make([]string, 0, p.NumParams+1)
	for i := byte(0); i < p.NumParams; i++ {
		parts = append(parts, fmt.Sprintf("arg%d", i+1))
	}
	if p.Variadic {
		parts = append(parts, "...")
	}
	return strings.Join(parts, ", ")
}

func buildCFG(chunk *BytecodeChunk, pi int, p *Proto, opts DecompileOptions) *cfg.Function {
	n := len(p.Insns)
	starts := map[int]bool{0: true}
	isBranch := make([]bool, n)
	for i, ins := range p.Insns {
		if ins.Op == OpNOP && !ins.HasAux {
			if i > 0 {
				if _, ok := branchDest(p, i-1, p.Insns[i-1]); ok {
					continue
				}
			}
		}
		d, ok := branchDest(p, i, ins)
		if ok {
			starts[d] = true
			isBranch[i] = true
			if i+1 < n {
				starts[i+1] = true
			}
			continue
		}
		if ins.Op == OpRETURN {
			isBranch[i] = true
			if i+1 < n {
				starts[i+1] = true
			}
		}
	}
	bounds := make([]int, 0, len(starts))
	for s := range starts {
		if s >= 0 && s < n {
			bounds = append(bounds, s)
		}
	}
	sort.Ints(bounds)
	blockOf := make(map[int]int, n)
	for bi, s := range bounds {
		e := n
		if bi+1 < len(bounds) {
			e = bounds[bi+1]
		}
		for i := s; i < e; i++ {
			blockOf[i] = bi
		}
	}
	f := cfg.NewFunction(pi)
	l := &lifter{chunk: p, strs: chunk.Strings, opts: opts, vers: map[byte]int{}}
	ids := make([]cfg.BlockID, len(bounds))
	blocks := make([]*ast.Block, len(bounds))
	for bi, s := range bounds {
		e := n
		if bi+1 < len(bounds) {
			e = bounds[bi+1]
		}
		ids[bi] = f.NewBlock()
		b := &ast.Block{}
		if bi > 0 || s == 0 {
			b.Add(ast.NewStatement(fmt.Sprintf("::L%d::", s)))
		}
		for i := s; i < e; i++ {
			ins := p.Insns[i]
			if ins.Op == OpNOP {
				continue
			}
			for _, st := range l.liftInsn(p, i, ins) {
				b.Add(ast.NewStatement(st))
			}
		}
		blocks[bi] = b
		f.SetBlock(ids[bi], b)
	}
	f.SetEntry(ids[0])
	edge := func(from, to int) {
		fb, ok1 := blockOf[from]
		tb, ok2 := blockOf[to]
		if !ok1 || !ok2 || fb == tb {
			if ok1 && ok2 && fb == tb {
				return
			}
			if !ok1 || !ok2 {
				return
			}
		}
		f.AddEdge(ids[fb], ids[tb], cfg.BlockEdge{Branch: cfg.BranchUnconditional})
	}
	for i, ins := range p.Insns {
		if ins.Op == OpNOP {
			continue
		}
		if d, ok := branchDest(p, i, ins); ok {
			edge(i, d)
			if !isUnconditional(ins.Op) && i+1 < n {
				edge(i, i+1)
			}
			continue
		}
		if ins.Op == OpRETURN {
			continue
		}
		if i+1 < n {
			edge(i, i+1)
		}
	}
	return f
}

func renderProto(chunk *BytecodeChunk, pi int, p *Proto, _ string, opts DecompileOptions) string {
	var sb strings.Builder
	if p.FuncName != nil && *p.FuncName != "" && !opts.CompactAnnotations {
		fmt.Fprintf(&sb, "-- proto %d: %s\n", pi, *p.FuncName)
	}
	f := buildCFG(chunk, pi, p, opts)
	body, _ := restructure.LiftCertified(f)
	fmt.Fprintf(&sb, "local function __proto_%d(%s)\n", pi, paramsOf(p))
	for _, st := range body.Statements {
		t := st.Text
		if strings.HasPrefix(t, "::L") || t == "" {
			continue
		}
		sb.WriteString("  " + t + "\n")
	}
	sb.WriteString("end\n")
	return sb.String()
}

func renderMain(chunk *BytecodeChunk, p *Proto, _ string, opts DecompileOptions) string {
	var sb strings.Builder
	if !opts.NoSynthHelpers && !opts.CompactAnnotations {
		sb.WriteString("-- main chunk\n")
	}
	f := buildCFG(chunk, int(chunk.MainProto), p, opts)
	body, _ := restructure.LiftCertified(f)
	for _, st := range body.Statements {
		t := st.Text
		if strings.HasPrefix(t, "::L") || t == "" {
			continue
		}
		sb.WriteString(t + "\n")
	}
	return sb.String()
}
