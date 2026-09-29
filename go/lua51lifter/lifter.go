package lua51lifter

import (
	"fmt"
	"strconv"
	"strings"

	deser "github.com/kiet1308/tovek-go/lua51deser"
)

func DecompileChunk(c *deser.Chunk) (string, error) {
	if c == nil || c.Func == nil {
		return "", fmt.Errorf("lua51lifter: nil chunk")
	}
	return Decompile(c.Func)
}

func Decompile(f *deser.Function) (string, error) {
	if f == nil {
		return "", fmt.Errorf("lua51lifter: nil function")
	}
	var sb strings.Builder
	sb.WriteString("-- decompiled by tovek-go (lua51)\n")
	l := &lifter{sb: &sb}
	l.writeFunc(f, 0, true)
	return sb.String(), nil
}

type lifter struct {
	sb *strings.Builder
}

func (l *lifter) writeFunc(f *deser.Function, depth int, top bool) {
	pad := strings.Repeat("  ", depth)
	if !top {
		params := make([]string, f.NumParams)
		for i := range params {
			params[i] = fmt.Sprintf("arg%d", i+1)
		}
		if f.IsVariadic() {
			params = append(params, "...")
		}
		l.printf("%sfunction(%s)\n", pad, strings.Join(params, ", "))
		pad = strings.Repeat("  ", depth+1)
	} else if f.IsVariadic() {
		l.printf("%slocal ... = ...\n", pad)
	}
	for i := byte(0); i < f.NumParams; i++ {
		l.printf("%slocal r_%d = arg%d\n", pad, i, i+1)
	}
	for _, ins := range f.Code {
		l.writeInsn(f, ins, pad, depth)
	}
	if !top {
		l.printf("%send\n", strings.Repeat("  ", depth))
	}
}

func (l *lifter) printf(format string, args ...any) {
	fmt.Fprintf(l.sb, format, args...)
}

func reg(n byte) string  { return fmt.Sprintf("r_%d", n) }
func up(n uint16) string { return fmt.Sprintf("up_%d", n) }
func rk(v uint16) string { return fmt.Sprintf("k_%d", v-256) }

func (l *lifter) operand(f *deser.Function, v uint16) string {
	if v > 255 {
		if int(v-256) < len(f.Consts) {
			return constStr(f.Consts[v-256])
		}
		return rk(v)
	}
	return reg(byte(v))
}

func constStr(v deser.Value) string {
	switch v.Kind {
	case deser.Nil:
		return "nil"
	case deser.Bool:
		return strconv.FormatBool(v.Bool)
	case deser.Number:
		return strconv.FormatFloat(v.Num, 'g', -1, 64)
	case deser.String:
		return strconv.Quote(string(v.Str))
	}
	return "nil"
}

func (l *lifter) writeInsn(f *deser.Function, ins deser.Insn, pad string, depth int) {
	r := func(n byte) string { return reg(n) }
	switch ins.Op {
	case deser.Move:
		l.printf("%slocal %s = %s\n", pad, r(ins.A), r(byte(ins.B)))
	case deser.LoadConstant:
		l.printf("%slocal %s = %s\n", pad, r(ins.A), l.constAt(f, ins.Bx))
	case deser.LoadBoolean:
		l.printf("%slocal %s = %s\n", pad, r(ins.A), strconv.FormatBool(ins.B != 0))
		if ins.C != 0 {
			l.printf("%sgoto skip_%d\n", pad, ins.A)
		}
	case deser.LoadNil:
		var regs []string
		for n := ins.A; uint16(n) <= ins.B; n++ {
			regs = append(regs, r(n))
		}
		l.printf("%slocal %s = nil\n", pad, strings.Join(regs, ", "))
	case deser.GetUpvalue:
		l.printf("%slocal %s = %s\n", pad, r(ins.A), up(ins.B))
	case deser.GetGlobal:
		l.printf("%slocal %s = %s\n", pad, r(ins.A), l.constAt(f, ins.Bx))
	case deser.GetIndex:
		l.printf("%slocal %s = %s[%s]\n", pad, r(ins.A), r(byte(ins.B)), l.operand(f, ins.C))
	case deser.SetGlobal:
		l.printf("%s%s = %s\n", pad, l.constAt(f, ins.Bx), r(ins.A))
	case deser.SetUpvalue:
		l.printf("%s%s = %s\n", pad, up(ins.B), r(ins.A))
	case deser.SetIndex:
		l.printf("%s%s[%s] = %s\n", pad, r(ins.A), l.operand(f, ins.B), l.operand(f, ins.C))
	case deser.NewTable:
		l.printf("%slocal %s = {}\n", pad, r(ins.A))
	case deser.PrepMethodCall:
		l.printf("%slocal %s = %s[%s]\n", pad, r(ins.A), r(byte(ins.B)), l.operand(f, ins.C))
		l.printf("%slocal %s = %s\n", pad, r(ins.A+1), r(byte(ins.B)))
	case deser.Add, deser.Subtract, deser.Multiply, deser.Divide, deser.Modulo, deser.Power:
		op := map[deser.Opcode]string{
			deser.Add: "+", deser.Subtract: "-", deser.Multiply: "*",
			deser.Divide: "/", deser.Modulo: "%", deser.Power: "^",
		}[ins.Op]
		l.printf("%slocal %s = %s %s %s\n", pad, r(ins.A), l.operand(f, ins.B), op, l.operand(f, ins.C))
	case deser.Minus:
		l.printf("%slocal %s = -%s\n", pad, r(ins.A), r(byte(ins.B)))
	case deser.Not:
		l.printf("%slocal %s = not %s\n", pad, r(ins.A), r(byte(ins.B)))
	case deser.Length:
		l.printf("%slocal %s = #%s\n", pad, r(ins.A), r(byte(ins.B)))
	case deser.Concatenate:
		var parts []string
		for n := ins.B; n <= ins.C; n++ {
			parts = append(parts, r(byte(n)))
		}
		l.printf("%slocal %s = %s\n", pad, r(ins.A), strings.Join(parts, " .. "))
	case deser.Jump:
		l.printf("%sgoto pc_%d\n", pad, int32(ins.SBx))
	case deser.Equal:
		l.printf("%sif (%s == %s) == %s then end\n", pad, l.operand(f, ins.B), l.operand(f, ins.C), strconv.FormatBool(ins.A != 0))
	case deser.LessThan:
		l.printf("%sif (%s < %s) == %s then end\n", pad, l.operand(f, ins.B), l.operand(f, ins.C), strconv.FormatBool(ins.A != 0))
	case deser.LessThanOrEqual:
		l.printf("%sif (%s <= %s) == %s then end\n", pad, l.operand(f, ins.B), l.operand(f, ins.C), strconv.FormatBool(ins.A != 0))
	case deser.Test:
		l.printf("%sif (not %s) == %s then end\n", pad, r(ins.A), strconv.FormatBool(ins.C != 1))
	case deser.TestSet:
		l.printf("%sif (not %s) == %s then %s = %s end\n", pad, r(byte(ins.B)), strconv.FormatBool(ins.C != 1), r(ins.A), r(byte(ins.B)))
	case deser.Call:
		call := l.callStr(ins.A, ins.B)
		if ins.C == 0 {
			l.printf("%slocal %s = %s\n", pad, r(ins.A), call)
		} else if ins.C == 1 {
			l.printf("%s%s\n", pad, call)
		} else {
			var dsts []string
			for i := uint16(0); i < ins.C-1; i++ {
				dsts = append(dsts, r(ins.A+byte(i)))
			}
			l.printf("%slocal %s = %s\n", pad, strings.Join(dsts, ", "), call)
		}
	case deser.TailCall:
		l.printf("%sreturn %s\n", pad, l.callStr(ins.A, ins.B))
	case deser.Return:
		if ins.B == 0 {
			l.printf("%sreturn\n", pad)
		} else if ins.B == 1 {
			l.printf("%sreturn\n", pad)
		} else {
			var vals []string
			for i := uint16(0); i < ins.B-1; i++ {
				vals = append(vals, r(ins.A+byte(i)))
			}
			l.printf("%sreturn %s\n", pad, strings.Join(vals, ", "))
		}
	case deser.IterateNumericForLoop, deser.InitNumericForLoop:
		l.printf("%sfor %s in numeric_for(%s) do end\n", pad, r(ins.A), r(ins.A))
	case deser.IterateGenericForLoop:
		l.printf("%sfor _ in %s(%s, %s) do end\n", pad, r(ins.A), r(ins.A+1), r(ins.A+2))
	case deser.SetList:
		l.printf("%ssetlist(%s, %d, %d)\n", pad, r(ins.A), ins.B, ins.Bx)
	case deser.Close:
		l.printf("%sclose(%s)\n", pad, r(ins.A))
	case deser.Closure:
		if int(ins.Bx) < len(f.Closures) {
			sub := f.Closures[ins.Bx]
			params := make([]string, sub.NumParams)
			for i := range params {
				params[i] = fmt.Sprintf("arg%d", i+1)
			}
			if sub.IsVariadic() {
				params = append(params, "...")
			}
			l.printf("%slocal %s = function(%s)\n", pad, r(ins.A), strings.Join(params, ", "))
			inner := &lifter{sb: &strings.Builder{}}
			inner.writeFunc(sub, depth+1, false)
			for _, line := range strings.Split(strings.TrimSuffix(inner.sb.String(), "\n"), "\n") {
				if strings.HasPrefix(line, strings.Repeat("  ", depth+1)+"function(") ||
					strings.TrimSpace(line) == "end" {
					continue
				}
				l.printf("%s\n", line)
			}
			l.printf("%send\n", pad)
		} else {
			l.printf("%slocal %s = function() end\n", pad, r(ins.A))
		}
	case deser.VarArg:
		if ins.B == 0 {
			l.printf("%slocal %s = ...\n", pad, r(ins.A))
		} else {
			var dsts []string
			for i := uint16(0); i < ins.B-1; i++ {
				dsts = append(dsts, r(ins.A+byte(i)))
			}
			l.printf("%slocal %s = ...\n", pad, strings.Join(dsts, ", "))
		}
	default:
		l.printf("%s-- pc: %s\n", pad, ins.Op)
	}
}

func (l *lifter) constAt(f *deser.Function, bx uint32) string {
	if int(bx) < len(f.Consts) {
		return constStr(f.Consts[bx])
	}
	return fmt.Sprintf("k_%d", bx)
}

func (l *lifter) callStr(base byte, b uint16) string {
	fn := reg(base)
	if b == 0 {
		return fmt.Sprintf("%s(...)", fn)
	}
	var parts []string
	for i := 0; i < int(b)-1; i++ {
		parts = append(parts, reg(base+1+byte(i)))
	}
	return fmt.Sprintf("%s(%s)", fn, strings.Join(parts, ", "))
}
