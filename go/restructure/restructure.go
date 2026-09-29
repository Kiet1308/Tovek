package restructure

import (
	"fmt"

	"github.com/kiet1308/tovek-go/ast"
	"github.com/kiet1308/tovek-go/cfg"
)

type UnsafeReason string

const (
	UnsafeCapturedCellReorder          UnsafeReason = "captured-cell-reorder"
	UnsafeCapturedLoopResultRef        UnsafeReason = "captured-loop-result-ref"
	UnsafeLiveBranchRewrite            UnsafeReason = "live-branch-rewrite"
	UnsafeForInitSuffixOrder           UnsafeReason = "for-init-suffix-order"
	UnsafeForOriginMissing             UnsafeReason = "for-origin-missing"
	UnsafeForOriginMismatch            UnsafeReason = "for-origin-mismatch"
	UnsafeForOriginDuplicate           UnsafeReason = "for-origin-duplicate"
	UnsafeForOriginPrepKindUnsupported UnsafeReason = "for-origin-prep-kind-unsupported"
	UnsafeForProtocolEdgeTransfer      UnsafeReason = "for-protocol-edge-transfer"
	UnsafeForInitEdgeTransferOrder     UnsafeReason = "for-init-edge-transfer-order"
	UnsafeUnmodeledClose               UnsafeReason = "unmodeled-close"
	UnsafeUnmodeledControl             UnsafeReason = "unmodeled-control"
)

type AttemptKind int

const (
	Structured AttemptKind = iota
	Unsupported
	Unsafe
)

type Attempt struct {
	Kind   AttemptKind
	Block  *ast.Block
	Reason UnsafeReason
}

func hasBackEdge(f *cfg.Function) bool {
	if f == nil {
		return false
	}
	entry, ok := f.Entry()
	if !ok {
		return false
	}
	visited := make(map[cfg.BlockID]bool)
	inStack := make(map[cfg.BlockID]bool)
	var visit func(id cfg.BlockID) bool
	visit = func(id cfg.BlockID) bool {
		visited[id] = true
		inStack[id] = true
		for _, next := range f.Successors(id) {
			if inStack[next] {
				return true
			}
			if !visited[next] {
				if visit(next) {
					return true
				}
			}
		}
		inStack[id] = false
		return false
	}
	return visit(entry)
}

func reachableFromEntry(f *cfg.Function) map[cfg.BlockID]bool {
	seen := make(map[cfg.BlockID]bool)
	if f == nil {
		return seen
	}
	entry, ok := f.Entry()
	if !ok {
		return seen
	}
	queue := []cfg.BlockID{entry}
	seen[entry] = true
	for len(queue) > 0 {
		id := queue[0]
		queue = queue[1:]
		for _, next := range f.Successors(id) {
			if !seen[next] {
				seen[next] = true
				queue = append(queue, next)
			}
		}
	}
	return seen
}

func LiftAttempt(f *cfg.Function, protected map[ast.LocalID]bool) Attempt {
	_ = protected
	if f == nil {
		return Attempt{Kind: Unsupported}
	}
	if f.BlockCount() == 1 && !hasBackEdge(f) {
		return Attempt{Kind: Structured, Block: f.Linearize()}
	}
	return Attempt{Kind: Unsupported}
}

func Lift(f *cfg.Function) *ast.Block {
	attempt := LiftAttempt(f, nil)
	if attempt.Kind != Structured {
		return nil
	}
	return attempt.Block
}

func LiftFallback(f *cfg.Function) *ast.Block {
	if f == nil {
		return &ast.Block{}
	}
	out := f.Linearize()
	if out == nil {
		return &ast.Block{}
	}
	if f.BlockCount() <= 1 {
		return out
	}
	labeled := &ast.Block{}
	entry, hasEntry := f.Entry()
	seen := make(map[cfg.BlockID]bool)
	for _, id := range f.Blocks() {
		if hasEntry && id == entry {
			continue
		}
		if seen[id] {
			continue
		}
		seen[id] = true
		labeled.Add(ast.NewStatement(fmt.Sprintf("::block_%d::", int(id))))
		if b := f.Block(id); b != nil {
			labeled.Statements = append(labeled.Statements, b.Statements...)
		}
	}
	if hasEntry {
		head := &ast.Block{}
		head.Add(ast.NewStatement(fmt.Sprintf("::block_%d::", int(entry))))
		if b := f.Block(entry); b != nil {
			head.Statements = append(head.Statements, b.Statements...)
		}
		head.Statements = append(head.Statements, labeled.Statements...)
		return head
	}
	return labeled
}

func LiftCertified(f *cfg.Function) (*ast.Block, []string) {
	if f == nil {
		return &ast.Block{}, nil
	}
	reachable := reachableFromEntry(f)
	if len(reachable) == 0 {
		return f.Linearize(), nil
	}
	synthetic := []string{"__tovek_pc", "__tovek_dispatch"}
	return LiftFallback(f), synthetic
}
