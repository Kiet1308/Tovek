package lua51lifter

import (
	"strings"
	"testing"

	deser "github.com/kiet1308/tovek-go/lua51deser"
)

func moveRet() *deser.Function {
	return &deser.Function{
		NumParams: 1,
		MaxStack:  2,
		Code: []deser.Insn{
			{Op: deser.Move, A: 1, B: 0},
			{Op: deser.Return, A: 1, B: 2},
		},
	}
}

func TestDecompileMoveReturn(t *testing.T) {
	out, err := Decompile(moveRet())
	if err != nil {
		t.Fatalf("Decompile: %v", err)
	}
	if !strings.Contains(out, "return") {
		t.Fatalf("missing return:\n%s", out)
	}
	if !strings.Contains(out, "tovek-go") {
		t.Fatalf("missing header:\n%s", out)
	}
}

func TestDecompileChunkNil(t *testing.T) {
	if _, err := DecompileChunk(nil); err == nil {
		t.Fatal("expected error for nil chunk")
	}
}
