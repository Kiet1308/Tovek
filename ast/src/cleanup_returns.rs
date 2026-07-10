//! Remove redundant trailing void `return` statements from function bodies.
//!
//! The Luau lifter materialises the implicit return at the end of every function
//! as an explicit value-less `return`. Source code almost never writes that — a
//! function simply falls off its end — so the machine-shaped tail reads worse
//! than the original:
//!
//! ```luau
//! local function onDone()
//!     setState(false)
//!     return        -- <- redundant; the function returns nothing either way
//! end
//! ```
//!
//! This pass strips that trailing value-less return from every closure/function
//! body so the output matches idiomatic source.
//!
//! Scope: only the *function/chunk tail* — the last statement of a body. Returns
//! inside nested branches / loop tails are deliberately left untouched: a `return` there is a real
//!     early exit (it skips the rest of the enclosing function), not a
//!     fall-through no-op, so removing it would change control flow.
//!
//! Dropping a value-less tail return is always semantics-preserving: in Luau a
//! function ending in `return` (no values) returns exactly the same zero values
//! as one that simply runs off its end.

use crate::{Block, RValue, Statement, Traverse};

/// Entry point. Falling off the end of a chunk and `return` with zero values are
/// equivalent, so the root tail is cleaned just like every nested function tail.
pub fn cleanup_redundant_returns(block: &mut Block) {
    clean_same_function_block(block);
    strip_trailing_void_return(block);
}

/// Recurse through control-flow blocks belonging to one function without
/// mistaking a branch tail for the function tail.
fn clean_same_function_block(block: &mut Block) {
    truncate_after_unconditional_transfer(block);
    for statement in &mut block.0 {
        clean_nested_in_statement(statement);
    }
}

/// Luau's grammar requires `return` to be the last statement in its block, and
/// anything after an unconditional return/break/continue is unreachable anyway.
/// Preserve legacy low-level blocks containing labels: an external goto may
/// enter their suffix, so ordinary fallthrough reasoning is insufficient there.
fn truncate_after_unconditional_transfer(block: &mut Block) {
    let Some(terminal) = block.0.iter().position(|statement| {
        matches!(
            statement,
            Statement::Return(_) | Statement::Break(_) | Statement::Continue(_)
        )
    }) else {
        return;
    };
    if block.0[terminal + 1..]
        .iter()
        .any(|statement| matches!(statement, Statement::Label(_)))
    {
        return;
    }
    block.0.truncate(terminal + 1);
}

fn clean_nested_in_statement(statement: &mut Statement) {
    // 1. Closures defined directly in this statement's rvalues (assign RHS, call
    //    arguments, table values, ...) are full function bodies of their own:
    //    strip their tail and recurse. `post_traverse_rvalues` stops at the
    //    closure boundary, so nested-in-nested closures are handled by the
    //    recursive `cleanup_redundant_returns` call inside `clean_function_body`.
    let mut functions = Vec::new();
    for rvalue in crate::deinline::stmt_rvalues_mut(statement) {
        rvalue.post_traverse_rvalues(&mut |nested| -> Option<()> {
            if let RValue::Closure(closure) = nested {
                functions.push(closure.function.clone());
            }
            None
        });
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
    }
    for function in functions {
        clean_function_body(&mut function.lock().body);
    }

    // 2. Nested control-flow blocks belong to the SAME function, so their tails
    //    are NOT function tails — recurse to reach deeper closures, but never
    //    strip a return there (phase 1: function-tail only).
    match statement {
        Statement::If(r#if) => {
            clean_same_function_block(&mut r#if.then_block.lock());
            clean_same_function_block(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => clean_same_function_block(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => clean_same_function_block(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => {
            clean_same_function_block(&mut numeric_for.block.lock())
        }
        Statement::GenericFor(generic_for) => {
            clean_same_function_block(&mut generic_for.block.lock())
        }
        _ => {}
    }
}

/// Clean one function/closure body: strip its trailing value-less return, then
/// recurse to clean every closure nested within it.
fn clean_function_body(body: &mut Block) {
    clean_same_function_block(body);
    strip_trailing_void_return(body);
}

fn strip_trailing_void_return(body: &mut Block) {
    // Skip trailing `Empty` placeholders (they render to nothing) to find the
    // real last statement; if it is a value-less return, drop it.
    if let Some(pos) = body
        .0
        .iter()
        .rposition(|s| !matches!(s, Statement::Empty(_)))
        && matches!(&body.0[pos], Statement::Return(r) if r.values.is_empty())
    {
        body.0.remove(pos);
    }
}

#[cfg(test)]
mod tests {
    use super::cleanup_redundant_returns;
    use crate::{
        Assign, Block, Call, Closure, Empty, Function, Global, If, Index, LValue, Label, Literal,
        RValue, Return, Statement,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn global(name: &str) -> RValue {
        RValue::Global(Global(name.as_bytes().to_vec()))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn call(name: &str) -> Statement {
        Call::new(global(name), vec![]).into()
    }

    fn void_return() -> Statement {
        Return::new(vec![]).into()
    }

    fn function(body: Vec<Statement>) -> Arc<Mutex<Function>> {
        Arc::new(Mutex::new(Function {
            body: Block(body),
            ..Function::default()
        }))
    }

    fn closure(function: &Arc<Mutex<Function>>) -> RValue {
        RValue::Closure(Closure {
            function: ByAddress(function.clone()),
            upvalues: vec![],
        })
    }

    #[test]
    fn strips_trailing_void_return_in_closure() {
        let f = function(vec![call("setState"), void_return()]);
        let mut block = Block(vec![Call::new(global("use"), vec![closure(&f)]).into()]);

        cleanup_redundant_returns(&mut block);

        assert_eq!(f.lock().body.to_string(), "setState()");
    }

    #[test]
    fn keeps_value_return_in_closure() {
        let f = function(vec![Return::new(vec![string("x")]).into()]);
        let mut block = Block(vec![Call::new(global("use"), vec![closure(&f)]).into()]);

        cleanup_redundant_returns(&mut block);

        assert_eq!(f.lock().body.to_string(), "return \"x\"");
    }

    #[test]
    fn strips_main_chunk_tail_void_return() {
        let mut block = Block(vec![call("init"), void_return()]);

        cleanup_redundant_returns(&mut block);

        assert_eq!(block.to_string(), "init()");
    }

    #[test]
    fn preserves_return_inside_nested_branch() {
        // The early `return` inside the `if` is a real exit, not a tail no-op.
        let f = function(vec![
            If::new(global("cond"), Block(vec![void_return()]), Block(vec![])).into(),
            call("after"),
        ]);
        let mut block = Block(vec![Call::new(global("use"), vec![closure(&f)]).into()]);

        cleanup_redundant_returns(&mut block);

        assert_eq!(
            f.lock().body.to_string(),
            "if cond then\n\treturn\nend\n\nafter()"
        );
    }

    #[test]
    fn strips_function_tail_but_keeps_inner_branch_return() {
        let f = function(vec![
            If::new(global("cond"), Block(vec![void_return()]), Block(vec![])).into(),
            call("after"),
            void_return(),
        ]);
        let mut block = Block(vec![Call::new(global("use"), vec![closure(&f)]).into()]);

        cleanup_redundant_returns(&mut block);

        // tail `return` gone; the early `return` inside the `if` is preserved.
        assert_eq!(
            f.lock().body.to_string(),
            "if cond then\n\treturn\nend\n\nafter()"
        );
    }

    #[test]
    fn cleans_nested_closures() {
        let inner = function(vec![call("inner"), void_return()]);
        let outer = function(vec![
            Call::new(global("use"), vec![closure(&inner)]).into(),
            void_return(),
        ]);
        let mut block = Block(vec![Call::new(global("use"), vec![closure(&outer)]).into()]);

        cleanup_redundant_returns(&mut block);

        assert_eq!(
            outer.lock().body.to_string(),
            "use(function()\n\tinner()\nend)"
        );
        assert_eq!(inner.lock().body.to_string(), "inner()");
    }

    #[test]
    fn strips_void_return_before_trailing_empty_placeholder() {
        let f = function(vec![
            call("setState"),
            void_return(),
            Statement::Empty(Empty {}),
        ]);
        let mut block = Block(vec![Call::new(global("use"), vec![closure(&f)]).into()]);

        cleanup_redundant_returns(&mut block);

        // The value-less return before the trailing `Empty` placeholder is gone.
        let body = f.lock();
        assert!(!body
            .body
            .0
            .iter()
            .any(|s| matches!(s, Statement::Return(_))));
        assert!(matches!(&body.body.0[0], Statement::Call(_)));
    }

    #[test]
    fn strips_void_return_leaving_empty_function_body() {
        let f = function(vec![void_return()]);
        let mut block = Block(vec![Call::new(global("use"), vec![closure(&f)]).into()]);

        cleanup_redundant_returns(&mut block);

        assert!(f.lock().body.0.is_empty());
    }

    #[test]
    fn cleans_closure_defined_inside_if_body() {
        // The closure lives inside an `if` block of the *current* function, so it
        // is reached only by the control-flow recursion (case 2), not by the
        // top-level rvalue scan — its own tail return must still be stripped.
        let f = function(vec![call("inner"), void_return()]);
        let mut block = Block(vec![If::new(
            global("cond"),
            Block(vec![Call::new(global("use"), vec![closure(&f)]).into()]),
            Block(vec![]),
        )
        .into()]);

        cleanup_redundant_returns(&mut block);

        assert_eq!(f.lock().body.to_string(), "inner()");
    }

    #[test]
    fn keeps_void_return_followed_by_trailing_label() {
        // A `::label::` after the tail return may be a backward-`goto` target, so
        // the void return is not actually the tail — the pass conservatively
        // refuses to strip when the last non-`Empty` statement is not the return.
        let f = function(vec![
            call("step"),
            void_return(),
            Statement::Label(Label::from("done")),
        ]);
        let mut block = Block(vec![Call::new(global("use"), vec![closure(&f)]).into()]);

        cleanup_redundant_returns(&mut block);

        let body = f.lock();
        assert_eq!(body.body.0.len(), 3);
        assert!(body
            .body
            .0
            .iter()
            .any(|s| matches!(s, Statement::Return(r) if r.values.is_empty())));
    }

    #[test]
    fn cleans_closure_in_return_value_position() {
        // The closure is reached through a `return <closure>` rvalue — verifies
        // `post_traverse_rvalues` covers return-value position.
        let inner = function(vec![call("inner"), void_return()]);
        let outer = function(vec![Return::new(vec![closure(&inner)]).into()]);
        let mut block = Block(vec![Call::new(global("use"), vec![closure(&outer)]).into()]);

        cleanup_redundant_returns(&mut block);

        // outer's tail `return <closure>` is a value return -> preserved;
        // the returned closure's own void tail -> stripped.
        assert!(matches!(&outer.lock().body.0[0], Statement::Return(r) if r.values.len() == 1));
        assert_eq!(inner.lock().body.to_string(), "inner()");
    }

    #[test]
    fn cleans_main_chunk_tail_and_nested_closure() {
        let f = function(vec![call("setState"), void_return()]);
        let mut block = Block(vec![
            Call::new(global("use"), vec![closure(&f)]).into(),
            void_return(),
        ]);

        cleanup_redundant_returns(&mut block);

        assert_eq!(f.lock().body.to_string(), "setState()");
        assert_eq!(block.to_string(), "use(function()\n\tsetState()\nend)");
    }

    #[test]
    fn removes_unreachable_statements_after_value_return() {
        let mut block = Block(vec![Return::new(vec![string("done")]).into(), call("dead")]);

        cleanup_redundant_returns(&mut block);

        assert_eq!(block.0.len(), 1);
        assert_eq!(block.to_string(), "return \"done\"");
    }

    #[test]
    fn removes_unreachable_return_after_return_in_nested_branch() {
        let mut block = Block(vec![If::new(
            global("cond"),
            Block(vec![
                Return::new(vec![string("done")]).into(),
                void_return(),
            ]),
            Block::default(),
        )
        .into()]);

        cleanup_redundant_returns(&mut block);

        let Statement::If(node) = &block.0[0] else {
            panic!()
        };
        assert_eq!(node.then_block.lock().0.len(), 1);
    }

    #[test]
    fn cleans_closure_nested_in_indexed_lhs() {
        let inner = function(vec![Return::new(vec![string("done")]).into(), call("dead")]);
        let mut block = Block(vec![Statement::Assign(Assign {
            left: vec![LValue::Index(Index::new(
                global("targets"),
                closure(&inner),
            ))],
            right: vec![string("value")],
            prefix: false,
            parallel: false,
        })]);

        cleanup_redundant_returns(&mut block);

        assert_eq!(inner.lock().body.0.len(), 1);
    }
}
