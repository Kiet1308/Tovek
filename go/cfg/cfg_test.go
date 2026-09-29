package cfg

import (
	"testing"

	"github.com/kiet1308/tovek-go/ast"
)

func TestDiamondSuccessorsAndPreds(t *testing.T) {
	f := NewFunction(1)
	entry := f.NewBlock()
	then := f.NewBlock()
	els := f.NewBlock()
	join := f.NewBlock()
	f.SetEntry(entry)

	f.SetBlock(entry, &ast.Block{Statements: []ast.Statement{ast.NewStatement("if c then")}})

	f.AddEdge(entry, then, BlockEdge{Branch: BranchThen})
	f.AddEdge(entry, els, BlockEdge{Branch: BranchElse})
	f.AddEdge(then, join, BlockEdge{Branch: BranchUnconditional})
	f.AddEdge(els, join, BlockEdge{Branch: BranchUnconditional})

	if got := f.Successors(entry); len(got) != 2 {
		t.Fatalf("entry successors = %v, want 2", got)
	}
	if got := f.Successors(then); len(got) != 1 || got[0] != join {
		t.Fatalf("then successors = %v, want [%v]", got, join)
	}
	if got := f.Predecessors(join); len(got) != 2 {
		t.Fatalf("join predecessors = %v, want 2", got)
	}
	if f.Edge(entry, then) == nil || f.Edge(entry, then).Branch != BranchThen {
		t.Fatalf("missing then edge")
	}
	if f.Edge(entry, els) == nil || f.Edge(entry, els).Branch != BranchElse {
		t.Fatalf("missing else edge")
	}
	if f.BlockCount() != 4 {
		t.Fatalf("block count = %d, want 4", f.BlockCount())
	}
	if id, ok := f.Entry(); !ok || id != entry {
		t.Fatalf("entry = %v,%v, want %v,true", id, ok, entry)
	}
	if out := f.Linearize(); out == nil || out.Len() == 0 {
		t.Fatalf("linearize must be non-empty for diamond with entry statement")
	}
}
