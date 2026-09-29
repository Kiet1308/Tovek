package ast

type LocalID uint64

type Local struct {
	ID   LocalID
	Name string
}

func (l *Local) String() string {
	if l == nil {
		return "<nil>"
	}
	if l.Name != "" {
		return l.Name
	}
	return "<local>"
}

type RValue struct {
	Text  string
	Local *Local
}

func NewLocalRValue(l *Local) *RValue {
	return &RValue{Local: l}
}

func NewTextRValue(text string) *RValue {
	return &RValue{Text: text}
}

func (v *RValue) String() string {
	if v == nil {
		return "<nil>"
	}
	if v.Text != "" {
		return v.Text
	}
	return v.Local.String()
}

type Statement struct {
	Text string
}

func NewStatement(text string) Statement {
	return Statement{Text: text}
}

func (s Statement) String() string {
	return s.Text
}

type Block struct {
	Statements []Statement
}

func (b *Block) Add(s Statement) {
	if b != nil {
		b.Statements = append(b.Statements, s)
	}
}

func (b *Block) Len() int {
	if b == nil {
		return 0
	}
	return len(b.Statements)
}

func (b *Block) IsEmpty() bool {
	return b.Len() == 0
}

func (b *Block) Clone() *Block {
	if b == nil {
		return nil
	}
	out := &Block{Statements: make([]Statement, len(b.Statements))}
	copy(out.Statements, b.Statements)
	return out
}
