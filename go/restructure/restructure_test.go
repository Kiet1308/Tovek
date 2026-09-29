package restructure

import (
	"testing"

	"github.com/kiet1308/tovek-go/ast"
	"github.com/kiet1308/tovek-go/cfg"
)

func TestSingleBlockLiftStructured(t *testing.T) {
	f := cfg.NewFunction(1)
	entry := f.NewBlock()
	f.SetEntry(entry)
	f.SetBlock(entry, &ast.Block{Statements: []ast.Statement{ast.NewStatement("return 1")}})

	attempt := LiftAttempt(f, nil)
	if attempt.Kind != Structured {
		t.Fatalf("single block attempt = %v, want structured", attempt.Kind)
	}
	if block := Lift(f); block == nil || block.Len() != 1 {
		t.Fatalf("single block lift must return one statement")
	}
	if Lift(nil) != nil {
		t.Fatalf("lift of nil must return nil")
	}
}

func TestMultiBlockFallbackNonNil(t *testing.T) {
	f := cfg.NewFunction(2)
	a := f.NewBlock()
	b := f.NewBlock()
	f.SetEntry(a)
	f.SetBlock(a, &ast.Block{Statements: []ast.Statement{ast.NewStatement("x = 1")}})
	f.SetBlock(b, &ast.Block{Statements: []ast.Statement{ast.NewStatement("y = 2")}})
	f.AddEdge(a, b, cfg.BlockEdge{Branch: cfg.BranchUnconditional})

	if Lift(f) != nil {
		t.Fatalf("multi-block lift without structure must return nil")
	}
	fallback := LiftFallback(f)
	if fallback == nil || fallback.Len() == 0 {
		t.Fatalf("multi-block fallback must be non-nil and non-empty")
	}
	certified, names := LiftCertified(f)
	if certified == nil || certified.Len() == 0 {
		t.Fatalf("certified fallback must be non-nil and non-empty")
	}
	if len(names) == 0 {
		t.Fatalf("certified fallback must report synthetic locals")
	}
}
