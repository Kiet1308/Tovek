//! Value-exact return reconstruction, without expression-style conditionals.
//! Only terminal writes to unobserved inferred bindings may disappear. This is
//! not local coalescing: parameters and compiler-recorded bindings stay intact.
use rustc_hash::FxHashSet;

use crate::{Binary, BinaryOperation, Block, LValue, Literal, RValue, RcLocal,
    Return, Select, Statement, Traverse, Unary, UnaryOperation};

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 100_000;
const MAX_REWRITES: usize = 10_000;

#[derive(Default)]
struct Facts {
    captured: FxHashSet<RcLocal>,
    parameters: FxHashSet<RcLocal>,
    declared: FxHashSet<RcLocal>,
    nodes: usize,
}

/// Refuse the entire tree if capture/control inspection is incomplete. The
/// immutable capture set remains conservative as terminal statements disappear.
pub fn reconstruct_terminal_returns(block: &mut Block) {
    let mut facts = Facts::default();
    if !inspect_block(block, &mut facts, 0) { return; }
    let mut removed = FxHashSet::default();
    let mut budget = MAX_REWRITES;
    rewrite_block(block, &facts, &mut removed, &mut budget, 0);
    if removed.is_empty() { return; }
    let usage = crate::inline_temps::collect_usage(block);
    removed.retain(|local| usage.get(local).is_some_and(|u|
        u.reads == 0 && u.writes == 1 && !u.captured));
    remove_orphan_declarations(block, &removed);
}

fn inspect_block(block: &Block, facts: &mut Facts, depth: usize) -> bool {
    if depth > MAX_DEPTH { return false; }
    for statement in &block.0 {
        facts.nodes += 1;
        if facts.nodes > MAX_NODES || matches!(statement, Statement::Goto(_) | Statement::Label(_)) {
            return false;
        }
        for value in statement.rvalues() {
            if !inspect_value(value, facts, depth + 1) { return false; }
        }
        if let Statement::Assign(assign) = statement {
            if assign.prefix {
                facts.declared.extend(assign.left.iter().filter_map(|left| left.as_local().cloned()));
            }
            for left in &assign.left {
                for value in left.rvalues() {
                    if !inspect_value(value, facts, depth + 1) { return false; }
                }
            }
        }
        let mut valid = true;
        visit_blocks(statement, &mut |block| valid &= inspect_block(block, facts, depth + 1));
        if !valid { return false; }
    }
    true
}

fn inspect_value(value: &RValue, facts: &mut Facts, depth: usize) -> bool {
    facts.nodes += 1;
    if depth > MAX_DEPTH || facts.nodes > MAX_NODES { return false; }
    if let RValue::Closure(closure) = value {
        for upvalue in &closure.upvalues {
            let (crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local)) = upvalue;
            facts.captured.insert(local.clone());
        }
        let function = closure.function.lock();
        facts.parameters.extend(function.parameters.iter().cloned());
        if !inspect_block(&function.body, facts, depth + 1) { return false; }
    }
    value.rvalues().into_iter().all(|child| inspect_value(child, facts, depth + 1))
}

fn visit_blocks(statement: &Statement, f: &mut impl FnMut(&mut Block)) {
    match statement {
        Statement::If(v) => { f(&mut v.then_block.lock()); f(&mut v.else_block.lock()); }
        Statement::While(v) => f(&mut v.block.lock()),
        Statement::Repeat(v) => f(&mut v.block.lock()),
        Statement::NumericFor(v) => f(&mut v.block.lock()),
        Statement::GenericFor(v) => f(&mut v.block.lock()),
        _ => {}
    }
}

fn eligible(local: &RcLocal, facts: &Facts) -> bool {
    // The lifter can invoke this pass on a child function by itself. An external
    // upvalue need not appear in any nested closure's capture list in that body.
    if !facts.declared.contains(local) || facts.captured.contains(local) || facts.parameters.contains(local) { return false; }
    let evidence = local.0.lock();
    if !evidence.2.is_empty() || evidence.4.parameter { return false; }
    // A conditional result is a presentation role, not a recorded source name.
    // Never merge it into a parameter; remove only its terminal, unobserved write.
    evidence.4.conditional_result || evidence.0.as_deref().is_some_and(|name|
        name == "v" || name.strip_prefix('v').is_some_and(|n|
            !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())))
}

fn rewrite_block(block: &mut Block, facts: &Facts, removed: &mut FxHashSet<RcLocal>,
    budget: &mut usize, depth: usize)
{
    if depth > MAX_DEPTH || *budget == 0 { return; }
    for statement in &block.0 {
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure|
            rewrite_block(&mut closure.function.lock().body, facts, removed, budget, depth + 1));
        visit_blocks(statement, &mut |child| rewrite_block(child, facts, removed, budget, depth + 1));
    }
    // Every successful iteration removes syntax; the additional per-block cap
    // prevents long generated chains from consuming the whole function budget.
    for _ in 0..32 {
        if *budget == 0 { break; }
        if direct_return(block, facts, removed, budget, depth) { continue; }
        if fold_guard(block) { *budget -= 1; continue; }
        break;
    }
}

fn tail_local(block: &Block) -> Option<RcLocal> {
    let Statement::Return(ret) = block.0.last()? else { return None; };
    let [RValue::Local(local)] = ret.values.as_slice() else { return None; };
    Some(local.clone())
}

fn writes_result(statement: &Statement, local: &RcLocal) -> bool {
    matches!(statement, Statement::Assign(a) if !a.parallel
        && matches!(a.left.as_slice(), [LValue::Local(target)] if target == local)
        && a.right.len() == 1)
}

fn scalar(value: RValue) -> RValue {
    match value {
        RValue::Call(call) => Select::Call(call).into(),
        RValue::MethodCall(call) => Select::MethodCall(call).into(),
        RValue::VarArg(value) => Select::VarArg(value).into(),
        value => value,
    }
}

fn take_assignment_value(statement: Statement) -> RValue {
    let Statement::Assign(mut assign) = statement else { unreachable!() };
    let mut value = scalar(assign.right.remove(0));
    crate::node_origins::inlined(&mut value);
    value
}

fn direct_return(block: &mut Block, facts: &Facts, removed: &mut FxHashSet<RcLocal>,
    budget: &mut usize, depth: usize) -> bool
{
    let Some(local) = tail_local(block) else { return false; };
    if block.0.len() < 2 || !eligible(&local, facts) { return false; }
    let index = block.0.len() - 2;
    if writes_result(&block.0[index], &local) {
        let value = take_assignment_value(block.0.remove(index));
        let Statement::Return(ret) = &mut block.0[index] else { unreachable!() };
        ret.values = vec![value];
        removed.insert(local);
        *budget -= 1;
        return true;
    }
    let Statement::If(branch) = &block.0[index] else { return false; };
    let (then_done, then_changed) = return_from_branch(&mut branch.then_block.lock(), &local, budget, depth + 1);
    let (else_done, else_changed) = return_from_branch(&mut branch.else_block.lock(), &local, budget, depth + 1);
    if then_changed || else_changed { removed.insert(local); }
    if then_done && else_done {
        block.0.pop();
        return true;
    }
    then_changed || else_changed
}

/// Propagate a return only into branch tails. A loop, close, or any intervening
/// statement stops propagation. Paths already returning keep their own arity.
fn return_from_branch(block: &mut Block, local: &RcLocal, budget: &mut usize, depth: usize) -> (bool, bool) {
    if depth > MAX_DEPTH || *budget == 0 { return (false, false); }
    let Some(last) = block.0.last() else { return (false, false); };
    if writes_result(last, local) {
        let value = take_assignment_value(block.0.pop().unwrap());
        block.0.push(Return::new(vec![value]).into());
        *budget -= 1;
        return (true, true);
    }
    match last {
        Statement::Return(_) => (true, false),
        Statement::If(branch) => {
            let (a, x) = return_from_branch(&mut branch.then_block.lock(), local, budget, depth + 1);
            let (b, y) = return_from_branch(&mut branch.else_block.lock(), local, budget, depth + 1);
            (a && b, x || y)
        }
        _ => (false, false),
    }
}

fn one_scalar_return(block: &Block) -> Option<&RValue> {
    let [Statement::Return(ret)] = block.0.as_slice() else { return None; };
    let [value] = ret.values.as_slice() else { return None; };
    (!matches!(value, RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_))).then_some(value)
}

enum Chain { Condition, Not, And, Or, NotAnd, NotOr, LocalAnd, LocalOr }

fn chain_kind(condition: &RValue, yes: &RValue, no: &RValue) -> Option<Chain> {
    // Prefer the direct local idiom over a boolean-coercion spelling such as
    // `not input and input`. Both are exact, but `input and false` reads better.
    let guard = match condition {
        RValue::Local(local) => Some((local, false)),
        RValue::Unary(v) if v.operation == UnaryOperation::Not => v.value.as_local().map(|local| (local, true)),
        _ => None,
    };
    if let Some((local, negated)) = guard {
        if yes.as_local() == Some(local) {
            return Some(if negated { Chain::LocalAnd } else { Chain::LocalOr });
        }
        if no.as_local() == Some(local) {
            return Some(if negated { Chain::LocalOr } else { Chain::LocalAnd });
        }
    }
    let is_bool = |v: &RValue, b| matches!(v, RValue::Literal(Literal::Boolean(x)) if *x == b);
    let falsy = |v: &RValue| matches!(v, RValue::Literal(Literal::Nil | Literal::Boolean(false)));
    // `not condition and nil` hides the deliberate false/nil distinction.
    if falsy(yes) && falsy(no) { return None; }
    if is_bool(yes, false) && is_bool(no, true) { return Some(Chain::Not); }
    if is_bool(yes, false) { return Some(Chain::NotAnd); }
    if is_bool(no, true) { return Some(Chain::NotOr); }
    if crate::binary::is_boolean(condition) {
        if is_bool(yes, true) && is_bool(no, false) { return Some(Chain::Condition); }
        if is_bool(no, false) { return Some(Chain::And); }
        if is_bool(yes, true) { return Some(Chain::Or); }
    }
    None
}

fn compact_chain(condition: &RValue, yes: &RValue, no: &RValue) -> bool {
    use std::fmt::Write;
    struct Preview { remaining: usize }
    impl std::fmt::Write for Preview {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            if s.contains(['\n', '\r']) || s.len() > self.remaining { return Err(std::fmt::Error); }
            self.remaining -= s.len();
            Ok(())
        }
    }
    // Read-only, bounded preview: stop formatting at the first newline or width
    // overflow. Leave room for return, operators and parentheses. This is a
    // presentation refusal, never an effect/type certificate.
    write!(&mut Preview { remaining: 100 }, "{condition} {yes} {no}").is_ok()
}

fn take_return(block: &mut Block) -> RValue {
    let Statement::Return(mut ret) = block.0.pop().unwrap() else { unreachable!() };
    ret.values.remove(0)
}

fn build_chain(kind: Chain, condition: RValue, yes: RValue, no: RValue) -> RValue {
    let not = |value| Unary::new(value, UnaryOperation::Not).into();
    let binary = |left, right, op| Binary::new(left, right, op).into();
    match kind {
        Chain::Condition => condition,
        Chain::Not => not(condition),
        Chain::And => binary(condition, yes, BinaryOperation::And),
        Chain::Or => binary(condition, no, BinaryOperation::Or),
        Chain::NotAnd => binary(not(condition), no, BinaryOperation::And),
        Chain::NotOr => binary(not(condition), yes, BinaryOperation::Or),
        Chain::LocalAnd | Chain::LocalOr => {
            let local = match condition { RValue::Unary(v) => *v.value, v => v };
            let other = if yes.as_local() == local.as_local() { no } else { yes };
            binary(local, other, if matches!(kind, Chain::LocalAnd) { BinaryOperation::And } else { BinaryOperation::Or })
        }
    }
}

fn fold_guard(block: &mut Block) -> bool {
    let len = block.0.len();
    if len == 0 { return false; }
    let (index, outer) = if matches!(block.0.last(), Some(Statement::Return(_))) && len >= 2 {
        (len - 2, true)
    } else { (len - 1, false) };
    let Statement::If(branch) = &block.0[index] else { return false; };
    let kind = {
        let then_block = branch.then_block.lock();
        let else_block = branch.else_block.lock();
        let Some(yes) = one_scalar_return(&then_block) else { return false; };
        let no = if outer && else_block.0.is_empty() {
            let Statement::Return(ret) = &block.0[len - 1] else { unreachable!() };
            let [value] = ret.values.as_slice() else { return false; };
            if matches!(value, RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_)) { return false; }
            value
        } else if !outer {
            let Some(value) = one_scalar_return(&else_block) else { return false; };
            value
        } else { return false; };
        let Some(kind) = chain_kind(&branch.condition, yes, no) else { return false; };
        if !compact_chain(&branch.condition, yes, no) { return false; }
        kind
    };
    let outer_value = outer.then(|| take_return(block));
    let Statement::If(branch) = block.0.pop().unwrap() else { unreachable!() };
    let yes = take_return(&mut branch.then_block.lock());
    let no = outer_value.unwrap_or_else(|| take_return(&mut branch.else_block.lock()));
    block.0.push(Return::new(vec![build_chain(kind, branch.condition, yes, no)]).into());
    true
}

fn remove_orphan_declarations(block: &mut Block, removed: &FxHashSet<RcLocal>) {
    block.0.retain(|s| !matches!(s, Statement::Assign(a) if a.prefix && !a.parallel
        && matches!(a.left.as_slice(), [LValue::Local(local)] if removed.contains(local))
        && (a.right.is_empty() || matches!(a.right.as_slice(), [RValue::Literal(Literal::Nil)]))));
    for statement in &block.0 {
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure|
            remove_orphan_declarations(&mut closure.function.lock().body, removed));
        visit_blocks(statement, &mut |child| remove_orphan_declarations(child, removed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, BindingOrigin, Call, Closure, Function, Global, If, Index,
        Local, SourceBinding, Upvalue, VarArg};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.to_string()))) }
    fn value(local: &RcLocal) -> RValue { local.clone().into() }
    fn call() -> RValue { Call::new(Global(b"probe".to_vec()).into(), vec![]).into() }
    fn ret(value: RValue) -> Statement { Return::new(vec![value]).into() }
    fn assign(local: &RcLocal, value: RValue, prefix: bool) -> Statement {
        let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
        assign.prefix = prefix;
        assign.into()
    }
    fn branch(condition: RValue, yes: Vec<Statement>, no: Vec<Statement>) -> Statement {
        If::new(condition, Block(yes), Block(no)).into()
    }
    fn closure(local: &RcLocal, by_ref: bool) -> RValue {
        Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function {
                body: Block(vec![ret(value(local))]), ..Default::default()
            }))),
            upvalues: vec![if by_ref { Upvalue::Ref(local.clone()) } else { Upvalue::Copy(local.clone()) }],
        }.into()
    }

    #[test]
    fn terminal_assignment_preserves_one_value_for_calls_methods_and_varargs() {
        let v = local("v");
        for rhs in [call(), crate::MethodCall::new(value(&local("object")), "probe".into(), vec![]).into(), VarArg.into()] {
            let mut block = Block(vec![assign(&v, rhs, true), ret(value(&v))]);
            reconstruct_terminal_returns(&mut block);
            assert!(matches!(block.0.as_slice(), [Statement::Return(r)] if matches!(r.values.as_slice(), [RValue::Select(_)])));
        }
    }

    #[test]
    fn inferred_conditional_cell_can_disappear_without_merging_into_parameter() {
        let parameter = local("p"); parameter.0.lock().4.parameter = true;
        let result = local("result"); result.0.lock().4.conditional_result = true;
        let mut block = Block(vec![
            assign(&result, Literal::Nil.into(), true),
            branch(value(&parameter), vec![assign(&result, value(&parameter), false)],
                vec![assign(&result, Literal::Boolean(false).into(), false)]),
            ret(value(&result)),
        ]);
        reconstruct_terminal_returns(&mut block);
        assert_eq!(block.0.len(), 1);
        assert!(parameter.0.lock().4.parameter);
        assert_ne!(parameter, result);
        // `p and p` would return nil rather than false for nil input.
        assert_eq!(block.to_string(), "return p or false");
        assert!(!block.to_string().contains("result"));
    }

    #[test]
    fn source_parameter_capture_and_meaningful_bindings_remain_visible() {
        for mode in 0..6 {
            let v = local(if mode == 5 { "elapsed" } else { "v" });
            if mode == 0 { v.0.lock().add_source_binding(SourceBinding {
                origin: BindingOrigin::DebugLocal { prototype: 0, register: 0, start_pc: 0, end_pc: 10 }, name: "source".into()
            }); }
            if mode == 1 { v.0.lock().4.parameter = true; }
            let mut block = Block(vec![]);
            if (2..=4).contains(&mode) {
                let observing = closure(&v, mode != 2);
                if mode == 4 {
                    block.0.push(Assign::new(vec![Index::new(observing, Literal::String(b"field".to_vec()).into()).into()], vec![Literal::Nil.into()]).into());
                } else { block.0.push(Call::new(Global(b"keep".to_vec()).into(), vec![observing]).into()); }
            }
            block.0.extend([assign(&v, call(), false), ret(value(&v))]);
            let before = block.to_string();
            reconstruct_terminal_returns(&mut block);
            assert_eq!(block.to_string(), before, "mode {mode}");
        }
    }

    #[test]
    fn guards_preserve_false_nil_and_scalar_arity_in_all_four_directions() {
        for (negated, returned_guard, op) in [(false, false, "and"), (true, false, "or"), (false, true, "or"), (true, true, "and")] {
            let v = local("input");
            let condition = if negated { Unary::new(value(&v), UnaryOperation::Not).into() } else { value(&v) };
            let other = Literal::Nil.into();
            let (yes, no) = if returned_guard { (value(&v), other) } else { (other, value(&v)) };
            let mut block = Block(vec![branch(condition, vec![ret(yes)], vec![]), ret(no)]);
            reconstruct_terminal_returns(&mut block);
            assert_eq!(block.to_string(), format!("return input {op} nil"));
        }
        for expanded in [call(), VarArg.into()] {
            let v = local("input");
            let mut block = Block(vec![branch(value(&v), vec![ret(value(&v))], vec![]), ret(expanded)]);
            let before = block.to_string();
            reconstruct_terminal_returns(&mut block);
            assert_eq!(block.to_string(), before);
        }
    }

    #[test]
    fn branch_effects_and_nonterminal_paths_do_not_move() {
        let v = local("v"); let condition = local("condition");
        let mut block = Block(vec![
            assign(&v, Literal::Nil.into(), true),
            branch(value(&condition), vec![call().as_call().unwrap().clone().into(), assign(&v, call(), false)], vec![]),
            ret(value(&v)),
        ]);
        reconstruct_terminal_returns(&mut block);
        let output = block.to_string();
        assert!(output.contains("probe()\n\treturn (probe())"), "{output}");
        assert!(output.starts_with("local v = nil"));
        assert!(output.ends_with("return v"));
        let mut blocked = Block(vec![assign(&v, call(), true), Call::new(Global(b"between".to_vec()).into(), vec![]).into(), ret(value(&v))]);
        let before = blocked.to_string();
        reconstruct_terminal_returns(&mut blocked);
        assert_eq!(blocked.to_string(), before);
    }

    #[test]
    fn nan_sensitive_condition_is_negated_without_complementing_comparator() {
        let comparison = Binary::new(value(&local("a")), value(&local("b")), BinaryOperation::LessThan).into();
        let mut block = Block(vec![branch(comparison,
            vec![ret(Literal::Boolean(false).into())], vec![ret(Literal::Boolean(true).into())])]);
        reconstruct_terminal_returns(&mut block);
        assert_eq!(block.to_string(), "return not (a < b)");
    }

    #[test]
    fn self_reads_and_non_nil_initializers_are_not_discarded() {
        let v = local("v");
        let mut block = Block(vec![assign(&v, Literal::Nil.into(), true),
            assign(&v, Index::new(value(&v), Literal::String(b"X".to_vec()).into()).into(), false), ret(value(&v))]);
        reconstruct_terminal_returns(&mut block);
        assert_eq!(block.to_string(), "local v = nil\nreturn v.X");
        let mut effectful = Block(vec![assign(&v, call(), true), assign(&v, Literal::Boolean(true).into(), false), ret(value(&v))]);
        reconstruct_terminal_returns(&mut effectful);
        assert_eq!(effectful.to_string(), "local v = probe()\nreturn true");
    }

    #[test]
    fn moved_call_retains_input_origin_without_fabricated_return_origin() {
        let v = local("v");
        let mut rhs = call();
        *crate::node_origins::value_mut(&mut rhs).unwrap() = crate::node_origins::Origin::input(crate::node_origins::Input {
            function: "root:p1".into(), block: 0, statement: 1, value: Some(0)
        });
        let mut block = Block(vec![assign(&v, rhs, true), ret(value(&v))]);
        reconstruct_terminal_returns(&mut block);
        let Statement::Return(ret) = &block.0[0] else { panic!() };
        let data = crate::node_origins::value(&ret.values[0]).unwrap().0.as_ref().unwrap();
        assert!(data.inlined && !data.cloned);
        assert_eq!(data.inputs.len(), 1);
        assert!(ret.node_origin.0.is_none());
    }

    #[test]
    fn external_upvalue_write_is_not_treated_as_an_unobserved_temporary() {
        let upvalue = local("v");
        let mut block = Block(vec![assign(&upvalue, call(), false), ret(value(&upvalue))]);
        let before = block.to_string();
        reconstruct_terminal_returns(&mut block);
        assert_eq!(block.to_string(), before);
    }

    #[test]
    fn false_nil_distinction_and_long_guards_stay_as_readable_statements() {
        for condition in [value(&local("condition")), value(&local(&"LongGuardName".repeat(12)))] {
            let mut block = Block(vec![branch(condition,
                vec![ret(Literal::Boolean(false).into())], vec![ret(Literal::Nil.into())])]);
            let before = block.to_string();
            reconstruct_terminal_returns(&mut block);
            assert_eq!(block.to_string(), before);
        }
        let mut long = Block(vec![branch(value(&local(&"LongGuardName".repeat(12))),
            vec![ret(Literal::Boolean(false).into())], vec![ret(Literal::Boolean(true).into())])]);
        let before = long.to_string();
        reconstruct_terminal_returns(&mut long);
        assert_eq!(long.to_string(), before);
        let input = local("input");
        let mut short = Block(vec![branch(value(&input),
            vec![ret(Literal::Boolean(false).into())], vec![ret(value(&input))])]);
        reconstruct_terminal_returns(&mut short);
        assert_eq!(short.to_string(), "return input and false");
    }
}
