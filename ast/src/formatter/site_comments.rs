//! Comments for the calls a de-inliner rebuilt. A rebuilt call carries its
//! `rebuilt` attribute instead of a marker statement, so later passes fold and
//! merge it like any other call. The formatter prints [`SITE_COMMENT`] at the
//! end of the line its statement ends on (a block statement's header line),
//! one comment per line however many calls it holds, and the exact number of
//! rebuilt calls in the final tree on the helper's definition line. One walk
//! before emission finds both, so emission only looks them up.

use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    Block, Call, GenericFor, If, LValue, NumericFor, RValue, Select, Statement, Traverse, While,
    call_origins::Kind,
};

/// Ends every line that holds an equivalent call a de-inliner inferred. It
/// describes the call Tovek printed, not where the source called the helper.
pub(crate) const SITE_COMMENT: &str = "inferred equivalent call";

#[derive(Clone, Copy, Default)]
struct HelperCalls {
    calls: usize,
    arithmetic: usize,
}

/// What the walk found: inferred calls per helper binding, and the nodes
/// (by address; the tree does not move while it prints) whose own values
/// hold one, a block statement's header node for its header.
#[derive(Default)]
struct Census {
    helpers: FxHashMap<u64, HelperCalls>,
    sites: FxHashSet<usize>,
}

/// The inferred calls of a whole tree, found before emission: a helper's
/// definition prints before its calls.
#[derive(Clone, Default)]
pub(crate) struct InferredCalls(Option<Rc<Census>>);

impl InferredCalls {
    /// One walk over the tree, in the order it prints: an arm block two arms
    /// share prints, and counts, twice.
    pub(crate) fn count(block: &Block) -> Self {
        let mut census = Census::default();
        count_block(block, &mut census);
        Self((!census.sites.is_empty()).then(|| Rc::new(census)))
    }

    /// The comment on the definition line of the helper bound to `helper`,
    /// when some inferred call names it.
    pub(crate) fn definition_comment(&self, helper: u64) -> Option<String> {
        let HelperCalls { calls, arithmetic } = *self.0.as_ref()?.helpers.get(&helper)?;
        let kind = if arithmetic == calls { "arithmetic " } else { "" };
        let plural = if calls == 1 { "" } else { "s" };
        Some(format!("{calls} equivalent {kind}call{plural} inferred from this helper"))
    }

    /// Whether the statement's own values (outside nested function bodies,
    /// whose calls end lines of their own) hold an inferred call.
    pub(crate) fn holds(&self, statement: &Statement) -> bool {
        self.holds_at(own_address(statement))
    }

    /// [`Self::holds`] for a block statement's header node.
    pub(crate) fn header_holds<T>(&self, header: &T) -> bool {
        self.holds_at(header as *const T as usize)
    }

    fn holds_at(&self, address: usize) -> bool {
        self.0.as_ref().is_some_and(|census| census.sites.contains(&address))
    }
}

/// The node a statement's site comment is looked up by: the header node of a
/// block statement (what `format_if` and friends receive), else the statement.
fn own_address(statement: &Statement) -> usize {
    match statement {
        Statement::If(r#if) => r#if as *const If as usize,
        Statement::While(r#while) => r#while as *const While as usize,
        Statement::NumericFor(numeric_for) => &**numeric_for as *const NumericFor as usize,
        Statement::GenericFor(generic_for) => generic_for as *const GenericFor as usize,
        _ => statement as *const Statement as usize,
    }
}

fn count_block(block: &Block, census: &mut Census) {
    for statement in block.iter() {
        let mut holds = matches!(statement, Statement::Call(call) if count_call(call, census));
        visit_own_values(statement, &mut |value| holds |= count_value(value, census));
        if holds {
            census.sites.insert(own_address(statement));
        }
        match statement {
            Statement::If(r#if) => {
                count_block(&r#if.then_block.lock(), census);
                count_block(&r#if.else_block.lock(), census);
            }
            Statement::While(r#while) => count_block(&r#while.block.lock(), census),
            Statement::Repeat(repeat) => count_block(&repeat.block.lock(), census),
            Statement::NumericFor(numeric_for) => count_block(&numeric_for.block.lock(), census),
            Statement::GenericFor(generic_for) => count_block(&generic_for.block.lock(), census),
            _ => {}
        }
    }
}

/// Counts the inferred calls in `value`; whether one sits outside nested
/// function bodies.
fn count_value(value: &RValue, census: &mut Census) -> bool {
    let mut holds = match value {
        RValue::Call(call) | RValue::Select(Select::Call(call)) => count_call(call, census),
        RValue::Closure(closure) => {
            count_block(&closure.function.lock().body, census);
            return false;
        }
        _ => false,
    };
    value.visit_rvalues(&mut |child| {
        holds |= count_value(child, census);
        true
    });
    holds
}

/// Counts `call` on its helper when inferred; whether it is.
fn count_call(call: &Call, census: &mut Census) -> bool {
    if !call.is_inferred() {
        return false;
    }
    if let RValue::Local(helper) = call.value.as_ref() {
        let entry = census.helpers.entry(helper.stable_id()).or_default();
        entry.calls += 1;
        entry.arithmetic += usize::from(call.rebuilt == Some(Kind::ArithmeticDeinline));
    }
    true
}

/// Visits the values a statement evaluates itself, outside its nested blocks:
/// a block statement's header values, an assignment's target addresses too.
fn visit_own_values<'a>(statement: &'a Statement, visit: &mut dyn FnMut(&'a RValue)) {
    statement.visit_rvalues(&mut |value| {
        visit(value);
        true
    });
    if let Statement::Assign(assign) = statement {
        for target in &assign.left {
            if let LValue::Index(index) = target {
                visit(&index.left);
                visit(&index.right);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    use crate::{
        Assign, Binary, BinaryOperation, Block, Call, Closure, Function, Global, If, LValue, Literal, Local,
        RValue, RcLocal, Return, Statement, call_origins::Kind,
    };

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn closure(parameters: Vec<RcLocal>, body: Vec<Statement>) -> RValue {
        let function = Function { parameters, body: Block(body), ..Default::default() };
        RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: Vec::new(),
        })
    }

    fn definition(helper: &RcLocal, body: Vec<Statement>) -> Statement {
        let parameter = local("x");
        let mut body = body;
        if body.is_empty() {
            return Assign { prefix: true, ..Assign::new(vec![LValue::Local(helper.clone())], vec![closure(vec![], body)]) }.into();
        }
        body.push(Return::new(vec![RValue::Local(parameter.clone())]).into());
        Assign { prefix: true, ..Assign::new(vec![LValue::Local(helper.clone())], vec![closure(vec![parameter], body)]) }.into()
    }

    fn call(helper: &RcLocal, argument: f64, kind: Option<Kind>) -> Call {
        let mut call = Call::new(RValue::Local(helper.clone()), vec![Literal::Number(argument).into()]);
        call.rebuilt = kind;
        call
    }

    fn print(value: RValue) -> Statement {
        Call::new(RValue::Global(Global::from("print")), vec![value]).into()
    }

    /// Each line holding inferred calls ends with one comment, a block
    /// statement's on its header line; the definition line counts them all.
    #[test]
    fn site_comments_end_their_lines_and_the_definition_counts_every_call() {
        let helper = local("helper");
        let y = local("y");
        let sum = Binary::new(
            call(&helper, 2.0, Some(Kind::ExpressionDeinline)).into(),
            call(&helper, 3.0, Some(Kind::ExpressionDeinline)).into(),
            BinaryOperation::Add,
        );
        let branch = If::new(
            call(&helper, 4.0, Some(Kind::StatementDeinline)).into(),
            Block(vec![print(RValue::Local(y.clone()))]),
            Block::default(),
        );
        let block = Block(vec![
            definition(&helper, vec![]),
            call(&helper, 1.0, Some(Kind::StatementDeinline)).into(),
            Assign { prefix: true, ..Assign::new(vec![LValue::Local(y.clone())], vec![sum.into()]) }.into(),
            branch.into(),
        ]);
        assert_eq!(
            block.to_string(),
            "local function helper() end -- 4 equivalent calls inferred from this helper\n\n\
             helper(1) -- inferred equivalent call\n\
             local y = helper(2) + helper(3) -- inferred equivalent call\n\n\
             if helper(4) then -- inferred equivalent call\n\tprint(y)\nend"
        );
    }

    /// A call inside a function body ends that body's line, not the line
    /// of the statement holding the function.
    #[test]
    fn a_call_in_a_nested_function_marks_its_own_line() {
        let helper = local("helper");
        let callback = closure(vec![], vec![call(&helper, 1.0, Some(Kind::StatementDeinline)).into()]);
        let block = Block(vec![
            definition(&helper, vec![print(Literal::Number(0.0).into())]),
            Call::new(RValue::Global(Global::from("run")), vec![callback]).into(),
        ]);
        assert_eq!(
            block.to_string(),
            "local function helper(x) -- 1 equivalent call inferred from this helper\n\tprint(0)\n\treturn x\nend\n\n\
             run(function()\n\thelper(1) -- inferred equivalent call\nend)"
        );
    }

    /// Arithmetic matches keep their kind on the definition line; a call to
    /// a synthesized helper is no inference and carries no comment.
    #[test]
    fn arithmetic_helpers_say_so_and_synthesized_calls_stay_bare() {
        let helper = local("scale");
        let synthesized = local("tail");
        let block = Block(vec![
            definition(&helper, vec![print(Literal::Number(0.0).into())]),
            print(call(&helper, 1.0, Some(Kind::ArithmeticDeinline)).into()),
            call(&synthesized, 2.0, Some(Kind::TerminalSynthesis)).into(),
        ]);
        let text = block.to_string();
        assert!(text.starts_with("local function scale(x) -- 1 equivalent arithmetic call inferred from this helper\n"));
        assert!(text.ends_with("print(scale(1)) -- inferred equivalent call\ntail(2)"));
    }
}
