//! A call's last argument keeps all of its values unless parenthesized:
//! `f(g())` passes every result of `g`, `f((g()))` only the first. Where
//! the callee drops the extra values anyway, the parentheses say nothing:
//! `emit((x:FindFirstChild("Beams")))` reads as `emit(x:FindFirstChild("Beams"))`.
//!
//! A callee drops them when the argument sits at or after its last
//! parameter and it takes no `...`: a local function declared once, or a
//! Luau library function whose global the script cannot have replaced
//! (`deinline_safety::CaptureSafety::constant_import`).
use rustc_hash::FxHashMap;

use crate::{Block, LValue, RValue, RcLocal, Select, Statement, Traverse};

pub fn untruncate_arguments(body: &mut Block) {
    let census = crate::deinline_safety::CaptureSafety::new(body);
    if !census.complete() {
        return;
    }
    let mut writes = FxHashMap::default();
    crate::expr_deinline::collect_write_counts(&body.0, &mut writes);
    let mut parameters = FxHashMap::default();
    local_functions(&body.0, &writes, &mut parameters);
    let arity = |callee: &RValue| match callee {
        RValue::Local(local) => parameters.get(local).copied(),
        _ if census.constant_import(callee) => library_arity(callee),
        _ => None,
    };
    block(&mut body.0, &arity);
}

/// The parameter count of every non-variadic local function whose binder
/// is written only by its declaration.
fn local_functions(stmts: &[Statement], writes: &FxHashMap<RcLocal, usize>, out: &mut FxHashMap<RcLocal, usize>) {
    for statement in stmts {
        if let Statement::Assign(assign) = statement
            && assign.prefix
            && let [LValue::Local(local)] = assign.left.as_slice()
            && let [RValue::Closure(closure)] = assign.right.as_slice()
            && writes.get(local) == Some(&1)
        {
            let function = closure.function.0.lock();
            if !function.is_variadic {
                out.insert(local.clone(), function.parameters.len());
            }
        }
        for_each_block(statement, &mut |nested| local_functions(nested, writes, out));
        crate::deinline::visit_stmt_rvalues(statement, &mut |value| {
            closures(value, &mut |body| local_functions(&body.0, writes, out));
            true
        });
    }
}

fn block(stmts: &mut [Statement], arity: &impl Fn(&RValue) -> Option<usize>) {
    for statement in stmts {
        match statement {
            Statement::Call(call) => untruncate(call, arity),
            _ => {}
        }
        statement.traverse_rvalues(&mut |value| match value {
            RValue::Call(call) | RValue::Select(Select::Call(call)) => untruncate(call, arity),
            RValue::Closure(closure) => block(&mut closure.function.0.lock().body.0, arity),
            _ => {}
        });
        for_each_block_mut(statement, &mut |nested| block(nested, arity));
    }
}

fn untruncate(call: &mut crate::Call, arity: &impl Fn(&RValue) -> Option<usize>) {
    let Some(last) = call.arguments.len().checked_sub(1) else { return };
    if !matches!(call.arguments[last], RValue::Select(_)) {
        return;
    }
    if arity(&call.value).is_some_and(|parameters| last + 1 >= parameters) {
        let value = std::mem::replace(&mut call.arguments[last], RValue::Literal(crate::Literal::Nil));
        call.arguments[last] = crate::untruncated(value);
    }
}

/// Parameters of a Luau library function that takes no `...` and ignores
/// arguments after them. Not `math.random` or `table.insert`, which refuse
/// extra arguments, nor `string.rep`/`string.gmatch` past their Lua 5.4
/// optional third one.
fn library_arity(callee: &RValue) -> Option<usize> {
    fn name(value: &RValue) -> Option<&str> {
        match value {
            RValue::Global(global) => std::str::from_utf8(&global.0).ok(),
            _ => None,
        }
    }
    let (library, function) = match callee {
        RValue::Global(_) => (None, name(callee)?),
        RValue::Index(index) => {
            let RValue::Literal(crate::Literal::String(key)) = &*index.right else { return None };
            (Some(name(&index.left)?), std::str::from_utf8(key).ok()?)
        }
        _ => return None,
    };
    let arity = match (library, function) {
        (None, "type" | "typeof" | "tostring" | "rawlen" | "getmetatable" | "ipairs" | "pairs") => 1,
        (None, "tonumber" | "rawget" | "rawequal" | "setmetatable" | "next" | "error") => 2,
        (None, "rawset") => 3,
        (Some("math"), "abs" | "acos" | "asin" | "atan" | "ceil" | "cos" | "cosh" | "deg" | "exp"
            | "floor" | "frexp" | "log10" | "modf" | "rad" | "sin" | "sinh" | "sqrt" | "tan" | "tanh"
            | "sign" | "round") => 1,
        (Some("math"), "atan2" | "fmod" | "ldexp" | "log" | "pow") => 2,
        (Some("math"), "clamp" | "noise" | "lerp") => 3,
        (Some("math"), "map") => 5,
        (Some("string"), "len" | "lower" | "upper" | "reverse") => 1,
        (Some("string"), "split") => 2,
        (Some("string"), "byte" | "sub" | "match" | "rep" | "gmatch") => 3,
        (Some("string"), "find" | "gsub") => 4,
        (Some("table"), "clear" | "clone" | "freeze" | "isfrozen" | "maxn" | "getn") => 1,
        (Some("table"), "sort" | "remove" | "create") => 2,
        (Some("table"), "find") => 3,
        (Some("table"), "concat") => 4,
        (Some("table"), "move") => 5,
        _ => return None,
    };
    Some(arity)
}

fn for_each_block(statement: &Statement, visit: &mut impl FnMut(&Block)) {
    match statement {
        Statement::If(branch) => {
            visit(&branch.then_block.lock());
            visit(&branch.else_block.lock());
        }
        Statement::While(node) => visit(&node.block.lock()),
        Statement::Repeat(node) => visit(&node.block.lock()),
        Statement::NumericFor(node) => visit(&node.block.lock()),
        Statement::GenericFor(node) => visit(&node.block.lock()),
        _ => {}
    }
}

fn for_each_block_mut(statement: &mut Statement, visit: &mut impl FnMut(&mut [Statement])) {
    match statement {
        Statement::If(branch) => {
            visit(&mut branch.then_block.lock().0);
            visit(&mut branch.else_block.lock().0);
        }
        Statement::While(node) => visit(&mut node.block.lock().0),
        Statement::Repeat(node) => visit(&mut node.block.lock().0),
        Statement::NumericFor(node) => visit(&mut node.block.lock().0),
        Statement::GenericFor(node) => visit(&mut node.block.lock().0),
        _ => {}
    }
}

fn closures(value: &RValue, visit: &mut impl FnMut(&Block)) {
    if let RValue::Closure(closure) = value {
        visit(&closure.function.0.lock().body);
        return;
    }
    value.visit_rvalues(&mut |child| {
        closures(child, visit);
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Call, Closure, Function, Global, Index, Literal, Local};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn field(library: &str, name: &str) -> RValue {
        Index::new(global(library), RValue::Literal(Literal::String(name.as_bytes().to_vec()))).into()
    }

    fn one_result() -> RValue {
        RValue::Select(Select::Call(Call::new(global("g"), Vec::new())))
    }

    fn function(name: &RcLocal, parameters: usize, is_variadic: bool) -> Statement {
        let function = Function {
            parameters: (0..parameters).map(|i| local(&format!("p{i}"))).collect(),
            is_variadic,
            ..Function::default()
        };
        let closure = Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: Vec::new(),
        };
        Assign::new(vec![LValue::Local(name.clone())], vec![RValue::Closure(closure)]).into()
    }

    fn declare(statement: Statement) -> Statement {
        let Statement::Assign(mut assign) = statement else { unreachable!() };
        assign.prefix = true;
        assign.into()
    }

    fn call(callee: RValue, mut arguments: Vec<RValue>) -> Statement {
        arguments.push(one_result());
        Call::new(callee, arguments).into()
    }

    fn run(mut statements: Vec<Statement>) -> String {
        let mut block = Block(std::mem::take(&mut statements));
        untruncate_arguments(&mut block);
        block.to_string()
    }

    #[test]
    fn parentheses_go_where_the_callee_drops_extra_values() {
        let emit = local("emit");
        let pair = local("pair");
        let rest = local("rest");
        let output = run(vec![
            declare(function(&emit, 1, false)),
            declare(function(&pair, 2, false)),
            declare(function(&rest, 1, true)),
            call(RValue::Local(emit.clone()), Vec::new()),
            call(RValue::Local(pair.clone()), Vec::new()),
            call(RValue::Local(pair.clone()), vec![global("a")]),
            call(RValue::Local(rest.clone()), Vec::new()),
            call(global("type"), Vec::new()),
            call(field("math", "abs"), Vec::new()),
            call(global("tonumber"), Vec::new()),
            call(field("math", "random"), vec![global("a")]),
            call(field("math", "max"), vec![global("a")]),
        ]);
        let calls: Vec<&str> = output.lines().filter(|line| line.contains("g()")).collect();
        assert_eq!(
            calls,
            [
                "emit(g())",
                "pair((g()))",
                "pair(a, g())",
                "rest((g()))",
                "type(g())",
                "math.abs(g())",
                "tonumber((g()))",
                "math.random(a, (g()))",
                "math.max(a, (g()))",
            ],
            "{output}"
        );
    }

    #[test]
    fn parentheses_stay_where_the_callee_may_be_replaced() {
        let emit = local("emit");
        let output = run(vec![
            declare(function(&emit, 1, false)),
            // `emit` is written again: the call may reach another function.
            function(&emit, 3, false),
            call(RValue::Local(emit.clone()), Vec::new()),
            // The script assigns `type`.
            Assign::new(vec![LValue::Global(Global::from("type"))], vec![global("print")]).into(),
            call(global("type"), Vec::new()),
        ]);
        assert!(output.contains("emit((g()))"), "{output}");
        assert!(output.contains("type((g()))"), "{output}");
    }
}
