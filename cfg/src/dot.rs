use std::{
    borrow::{Borrow, Cow},
    cell::RefCell,
    io::Write,
};

use ast::LocalRw;
use dot::{GraphWalk, LabelText, Labeller};

use itertools::Itertools;
use petgraph::{
    stable_graph::{EdgeIndex, NodeIndex},
    visit::{Bfs, Walker},
};

use crate::function::Function;

fn arguments(args: &Vec<(ast::RcLocal, ast::RValue)>) -> String {
    let mut s = String::new();
    for (i, (local, new_local)) in args.iter().enumerate() {
        use std::fmt::Write;
        write!(s, "{} -> {}", local, new_local).unwrap();
        if i + 1 != args.len() {
            s.push('\n');
        }
    }
    s
}

struct FunctionLabeller<'a> {
    function: &'a Function,
    counter: RefCell<usize>,
}

impl<'a> Labeller<'a, NodeIndex, EdgeIndex> for FunctionLabeller<'a> {
    fn graph_id(&'a self) -> dot::Id<'a> {
        dot::Id::new("cfg").unwrap()
    }

    fn node_label<'b>(&'b self, n: &NodeIndex) -> dot::LabelText<'b> {
        let block = self.function.block(*n).unwrap();
        let prefix = if self.function.entry() == &Some(*n) {
            "entry"
        } else {
            ""
        };
        dot::LabelText::LabelStr(
            block
                .iter()
                .map(|s| {
                    for local in s.values() {
                        let name = &mut local.0 .0.lock().0;
                        if name.is_none() {
                            // TODO: ugly
                            *name = Some(format!("v{}", self.counter.borrow()));
                            *self.counter.borrow_mut() += 1;
                        }
                    }
                    s
                })
                .join("\n")
                .into(),
        )
        .prefix_line(dot::LabelText::LabelStr(
            format!("{} {}", n.index(), prefix).into(),
        ))
    }

    fn edge_label<'b>(&'b self, e: &EdgeIndex) -> dot::LabelText<'b> {
        let edge = self.function.graph().edge_weight(*e).unwrap();
        match edge.branch_type {
            crate::block::BranchType::Unconditional => {
                dot::LabelText::LabelStr(arguments(&edge.arguments).into())
            }
            crate::block::BranchType::Then => {
                let arguments = arguments(&edge.arguments);
                if !arguments.is_empty() {
                    dot::LabelText::LabelStr(format!("t\n{}", arguments).into())
                } else {
                    dot::LabelText::LabelStr("t".into())
                }
            }
            crate::block::BranchType::Else => {
                let arguments = arguments(&edge.arguments);
                if !arguments.is_empty() {
                    dot::LabelText::LabelStr(format!("e\n{}", arguments).into())
                } else {
                    dot::LabelText::LabelStr("e".into())
                }
            }
        }
    }

    fn node_id(&'a self, n: &NodeIndex) -> dot::Id<'a> {
        dot::Id::new(format!("N{}", n.index())).unwrap()
    }

    fn node_shape(&'a self, _n: &NodeIndex) -> Option<LabelText<'a>> {
        Some(LabelText::LabelStr("rect".into()))
    }
}

impl<'a> GraphWalk<'a, NodeIndex, EdgeIndex> for FunctionLabeller<'a> {
    fn nodes(&'a self) -> dot::Nodes<'a, NodeIndex> {
        Cow::Owned(
            Bfs::new(self.function.graph(), self.function.entry().unwrap())
                .iter(self.function.graph())
                .collect::<Vec<_>>(),
        )
    }

    fn edges(&'a self) -> dot::Edges<'a, EdgeIndex> {
        Cow::Owned(self.function.graph().edge_indices().collect())
    }

    fn source(&self, e: &EdgeIndex) -> NodeIndex {
        self.function.graph().edge_endpoints(*e).unwrap().0
    }

    fn target(&self, e: &EdgeIndex) -> NodeIndex {
        self.function.graph().edge_endpoints(*e).unwrap().1
    }
}

pub fn render_to<W: Write>(function: &Function, output: &mut W) -> std::io::Result<()> {
    dot::render(
        &FunctionLabeller {
            function,
            counter: RefCell::new(1),
        },
        output,
    )
}
