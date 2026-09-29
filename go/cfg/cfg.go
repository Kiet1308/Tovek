package cfg

import (
	"sort"

	"github.com/kiet1308/tovek-go/ast"
)

type BranchType int

const (
	BranchUnconditional BranchType = iota
	BranchThen
	BranchElse
)

type PhiArg struct {
	Target *ast.Local
	Value  *ast.RValue
}

type BlockEdge struct {
	Branch BranchType
	Args   []PhiArg
}

func (e BlockEdge) Clone() BlockEdge {
	out := BlockEdge{Branch: e.Branch}
	if e.Args != nil {
		out.Args = append([]PhiArg(nil), e.Args...)
	}
	return out
}

type BlockID int

type edgeKey struct {
	from BlockID
	to   BlockID
}

type Function struct {
	ID       int
	Name     *string
	Params   []*ast.Local
	Variadic bool

	blocks   map[BlockID]*ast.Block
	order    []BlockID
	free     []BlockID
	next     BlockID
	entry    BlockID
	hasEntry bool

	succ  map[BlockID][]BlockID
	pred  map[BlockID][]BlockID
	edges map[edgeKey]*BlockEdge
}

func NewFunction(id int) *Function {
	return &Function{
		ID:     id,
		blocks: make(map[BlockID]*ast.Block),
		succ:   make(map[BlockID][]BlockID),
		pred:   make(map[BlockID][]BlockID),
		edges:  make(map[edgeKey]*BlockEdge),
	}
}

func (f *Function) NewBlock() BlockID {
	if f == nil {
		return BlockID(-1)
	}
	var id BlockID
	if len(f.free) > 0 {
		id = f.free[len(f.free)-1]
		f.free = f.free[:len(f.free)-1]
	} else {
		id = f.next
		f.next++
	}
	if f.blocks == nil {
		f.blocks = make(map[BlockID]*ast.Block)
	}
	f.blocks[id] = &ast.Block{}
	f.order = append(f.order, id)
	return id
}

func (f *Function) RemoveBlock(id BlockID) {
	if f == nil {
		return
	}
	if _, ok := f.blocks[id]; !ok {
		return
	}
	for _, to := range append([]BlockID(nil), f.succ[id]...) {
		f.RemoveEdge(id, to)
	}
	for _, from := range append([]BlockID(nil), f.pred[id]...) {
		f.RemoveEdge(from, id)
	}
	delete(f.blocks, id)
	delete(f.succ, id)
	delete(f.pred, id)
	order := f.order[:0]
	for _, kept := range f.order {
		if kept != id {
			order = append(order, kept)
		}
	}
	f.order = order
	f.free = append(f.free, id)
	if f.hasEntry && f.entry == id {
		f.hasEntry = false
	}
}

func (f *Function) HasBlock(id BlockID) bool {
	if f == nil {
		return false
	}
	_, ok := f.blocks[id]
	return ok
}

func (f *Function) SetEntry(id BlockID) {
	if f == nil || !f.HasBlock(id) {
		return
	}
	f.entry = id
	f.hasEntry = true
}

func (f *Function) Entry() (BlockID, bool) {
	if f == nil || !f.hasEntry {
		return 0, false
	}
	return f.entry, true
}

func (f *Function) SetBlock(id BlockID, b *ast.Block) {
	if f == nil || !f.HasBlock(id) {
		return
	}
	if b == nil {
		f.blocks[id] = &ast.Block{}
		return
	}
	f.blocks[id] = b
}

func (f *Function) Block(id BlockID) *ast.Block {
	if f == nil {
		return nil
	}
	return f.blocks[id]
}

func (f *Function) Blocks() []BlockID {
	if f == nil {
		return nil
	}
	out := append([]BlockID(nil), f.order...)
	kept := out[:0]
	for _, id := range out {
		if _, ok := f.blocks[id]; ok {
			kept = append(kept, id)
		}
	}
	return kept
}

func (f *Function) BlockCount() int {
	if f == nil {
		return 0
	}
	return len(f.blocks)
}

func removeID(list []BlockID, id BlockID) []BlockID {
	out := list[:0]
	for _, kept := range list {
		if kept != id {
			out = append(out, kept)
		}
	}
	return out
}

func (f *Function) AddEdge(from, to BlockID, e BlockEdge) {
	if f == nil || !f.HasBlock(from) || !f.HasBlock(to) {
		return
	}
	if f.succ == nil {
		f.succ = make(map[BlockID][]BlockID)
	}
	if f.pred == nil {
		f.pred = make(map[BlockID][]BlockID)
	}
	if f.edges == nil {
		f.edges = make(map[edgeKey]*BlockEdge)
	}
	key := edgeKey{from: from, to: to}
	if _, ok := f.edges[key]; !ok {
		f.succ[from] = append(f.succ[from], to)
		f.pred[to] = append(f.pred[to], from)
	}
	edge := e.Clone()
	f.edges[key] = &edge
}

func (f *Function) Successors(id BlockID) []BlockID {
	if f == nil {
		return nil
	}
	return append([]BlockID(nil), f.succ[id]...)
}

func (f *Function) Predecessors(id BlockID) []BlockID {
	if f == nil {
		return nil
	}
	return append([]BlockID(nil), f.pred[id]...)
}

func (f *Function) Edge(from, to BlockID) *BlockEdge {
	if f == nil {
		return nil
	}
	return f.edges[edgeKey{from: from, to: to}]
}

func (f *Function) RemoveEdge(from, to BlockID) {
	if f == nil {
		return
	}
	key := edgeKey{from: from, to: to}
	if _, ok := f.edges[key]; !ok {
		return
	}
	delete(f.edges, key)
	f.succ[from] = removeID(f.succ[from], to)
	f.pred[to] = removeID(f.pred[to], from)
}

func (f *Function) Linearize() *ast.Block {
	out := &ast.Block{}
	if f == nil {
		return out
	}
	for _, id := range f.Blocks() {
		if b := f.blocks[id]; b != nil {
			out.Statements = append(out.Statements, b.Statements...)
		}
	}
	return out
}

func Construct(f *Function) {
}

func Destruct(f *Function) {
}

func StructureJumps(f *Function) bool {
	return false
}

func StructureConditionals(f *Function) bool {
	return false
}

func Inline(f *Function) {
}

func RemoveUnnecessaryParams(f *Function) bool {
	return false
}

func Dominators(f *Function) map[BlockID]map[BlockID]bool {
	result := make(map[BlockID]map[BlockID]bool)
	if f == nil {
		return result
	}
	ids := f.Blocks()
	if len(ids) == 0 {
		return result
	}
	all := make(map[BlockID]bool, len(ids))
	for _, id := range ids {
		all[id] = true
	}
	entry, ok := f.Entry()
	if !ok {
		entry = ids[0]
	}
	for _, id := range ids {
		if id == entry {
			result[id] = map[BlockID]bool{id: true}
			continue
		}
		dom := make(map[BlockID]bool, len(all))
		for other := range all {
			dom[other] = true
		}
		result[id] = dom
	}
	changed := true
	for changed {
		changed = false
		for _, id := range ids {
			if id == entry {
				continue
			}
			preds := f.Predecessors(id)
			var next map[BlockID]bool
			if len(preds) == 0 {
				next = map[BlockID]bool{id: true}
			} else {
				first := true
				for _, p := range preds {
					dom, ok := result[p]
					if !ok {
						continue
					}
					if first {
						next = make(map[BlockID]bool, len(dom)+1)
						for other := range dom {
							next[other] = true
						}
						first = false
						continue
					}
					for other := range next {
						if !dom[other] {
							delete(next, other)
						}
					}
				}
				if next == nil {
					next = make(map[BlockID]bool)
				}
				next[id] = true
			}
			if !equalSets(result[id], next) {
				result[id] = next
				changed = true
			}
		}
	}
	return result
}

func equalSets(a, b map[BlockID]bool) bool {
	if len(a) != len(b) {
		return false
	}
	for k := range a {
		if !b[k] {
			return false
		}
	}
	return true
}

func SortedIDs(ids []BlockID) []BlockID {
	out := append([]BlockID(nil), ids...)
	sort.Slice(out, func(i, j int) bool { return out[i] < out[j] })
	return out
}
