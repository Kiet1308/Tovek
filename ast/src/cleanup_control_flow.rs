//! Final control-flow presentation, after helper matching and naming.
//!
//! Rewrites move existing statements/conditions without copying evaluations.
//! Direct declarations are never lifted out of their lexical block: names
//! have already been assigned, and extending a scope could shadow a later
//! local/global or exhaust Luau's register budget. Loop control is summarized
//! bottom-up; a nested loop's break/continue never belongs to its parent.

use crate::{Block, Literal, RValue, Repeat, Statement, Traverse};

#[derive(Default, Clone, Copy)]
struct Flow {
    breaks: usize,
    continues: bool,
    unstructured: bool,
}

impl Flow {
    fn include(&mut self, other: Self) {
        self.breaks += other.breaks;
        self.continues |= other.continues;
        self.unstructured |= other.unstructured;
    }
}

pub fn cleanup_control_flow(block: &mut Block) {
    cleanup_block(block, false);
}

fn trivia(statement: &Statement) -> bool {
    matches!(statement, Statement::Comment(_) | Statement::Empty(_))
}

fn terminates(block: &Block) -> bool {
    matches!(block.0.iter().rfind(|statement| !trivia(statement)),
        Some(Statement::Return(_) | Statement::Break(_) | Statement::Continue(_)))
}

fn declares_local(block: &Block) -> bool {
    block.0.iter().any(|statement| matches!(statement, Statement::Assign(assign) if assign.prefix))
}

fn singleton_if(block: &Block) -> bool {
    let mut real = block.0.iter().filter(|statement| !trivia(statement));
    matches!((real.next(), real.next()), (Some(Statement::If(_)), None))
}

fn break_guard(statement: &Statement) -> bool {
    let Statement::If(branch) = statement else { return false; };
    let then = branch.then_block.lock();
    let mut real = then.0.iter().filter(|statement| !trivia(statement));
    matches!((real.next(), real.next()), (Some(Statement::Break(_)), None))
        && branch.else_block.lock().0.iter().all(trivia)
}

/// `if c then return x end`: the `return`.
fn return_guard(statement: &Statement) -> Option<Statement> {
    let Statement::If(branch) = statement else { return None; };
    let then = branch.then_block.lock();
    let mut real = then.0.iter().filter(|statement| !trivia(statement));
    match (real.next(), real.next()) {
        (Some(ret @ Statement::Return(_)), None) if branch.else_block.lock().0.iter().all(trivia) => Some(ret.clone()),
        _ => None,
    }
}

fn cleanup_value(value: &mut RValue) {
    if let RValue::Closure(closure) = value {
        cleanup_block(&mut closure.function.lock().body, false);
    } else {
        value.visit_rvalues_mut(&mut |child| { cleanup_value(child); true });
    }
}

fn cleanup_block(block: &mut Block, else_arm: bool) -> Flow {
    // Keep an existing elseif chain together. Its children are still visited.
    let elseif = else_arm && singleton_if(block);
    let mut flow = Flow::default();
    let mut output = Vec::with_capacity(block.0.len());
    let last = block.0.iter().rposition(|statement| !trivia(statement));
    for (index, mut statement) in std::mem::take(&mut block.0).into_iter().enumerate() {
        crate::deinline::visit_stmt_rvalues_mut(&mut statement, &mut |value| {
            cleanup_value(value);
            true
        });
        let mut after_loop = None;
        match &mut statement {
            Statement::If(branch) => {
                let then_flow = cleanup_block(&mut branch.then_block.lock(), false);
                let else_flow = cleanup_block(&mut branch.else_block.lock(), true);
                flow.include(then_flow);
                flow.include(else_flow);
                let lift = !elseif && !then_flow.unstructured && !else_flow.unstructured
                    && terminates(&branch.then_block.lock())
                    && {
                        let otherwise = branch.else_block.lock();
                        !otherwise.0.iter().all(trivia)
                            && !singleton_if(&otherwise)
                            && !declares_local(&otherwise)
                            && (Some(index) == last || !terminates(&otherwise))
                    };
                if lift {
                    let tail = std::mem::take(&mut branch.else_block.lock().0);
                    output.push(statement);
                    output.extend(tail);
                    continue;
                }
            }
            Statement::While(node) => {
                let body_flow = cleanup_block(&mut node.block.lock(), false);
                flow.unstructured |= body_flow.unstructured;
                if matches!(node.condition, RValue::Literal(Literal::Boolean(true)))
                    && !body_flow.unstructured
                {
                    let mut body = node.block.lock();
                    if let Some(head) = body.0.iter().position(|statement| !trivia(statement))
                        && break_guard(&body.0[head])
                    {
                        let Statement::If(branch) = body.0.remove(head) else { unreachable!() };
                        node.condition = crate::flatten_guards::negate(branch.condition);
                        // Continues reevaluate this same head test, including its effects.
                        let comments = branch.then_block.lock().0.iter().filter(|s| trivia(s)).cloned()
                            .chain(branch.else_block.lock().0.iter().filter(|s| trivia(s)).cloned())
                            .collect::<Vec<_>>();
                        body.0.splice(head..head, comments);
                    } else if body_flow.breaks == 0
                        && let Some(head) = body.0.iter().position(|statement| !trivia(statement))
                        && let Some(ret) = return_guard(&body.0[head])
                    {
                        // Leaving only by returns, nothing after the loop runs
                        // but this exit: `while c do ... end; return x`.
                        let Statement::If(branch) = body.0.remove(head) else { unreachable!() };
                        node.condition = crate::flatten_guards::negate(branch.condition);
                        after_loop = Some(ret);
                    }
                }
                if matches!(node.condition, RValue::Literal(Literal::Boolean(true)))
                    && !body_flow.continues && !body_flow.unstructured
                {
                    let mut body = node.block.lock();
                    if let Some(end) = body.0.iter().rposition(|statement| !trivia(statement)) {
                        // The loop runs once. Flattening is safe only when no
                        // earlier exit would be retargeted to an enclosing loop.
                        if matches!(body.0[end], Statement::Break(_))
                            && body_flow.breaks == 1 && !declares_local(&body)
                        {
                            body.0.remove(end);
                            // A chain of nested one-shot wrappers can pass
                            // the same buffer outward without copying it at
                            // every nesting level.
                            if output.is_empty() {
                                output = std::mem::take(&mut body.0);
                            } else {
                                output.extend(std::mem::take(&mut body.0));
                            }
                            continue;
                        }
                        if break_guard(&body.0[end]) {
                            let Statement::If(branch) = body.0.remove(end) else { unreachable!() };
                            // Preserve comments belonging to the removed latch.
                            body.0.extend(branch.then_block.lock().0.iter().filter(|s| trivia(s)).cloned());
                            body.0.extend(branch.else_block.lock().0.iter().filter(|s| trivia(s)).cloned());
                            let replacement = Repeat::new(branch.condition, Block(std::mem::take(&mut body.0)));
                            drop(body);
                            output.push(replacement.into());
                            continue;
                        }
                    }
                }
            }
            Statement::Repeat(node) => {
                flow.unstructured |= cleanup_block(&mut node.block.lock(), false).unstructured;
            }
            Statement::NumericFor(node) => {
                flow.unstructured |= cleanup_block(&mut node.block.lock(), false).unstructured;
            }
            Statement::GenericFor(node) => {
                flow.unstructured |= cleanup_block(&mut node.block.lock(), false).unstructured;
            }
            Statement::Break(_) => flow.breaks += 1,
            Statement::Continue(_) => flow.continues = true,
            Statement::Goto(_) | Statement::Label(_) | Statement::Close(_) => flow.unstructured = true,
            _ => {}
        }
        output.push(statement);
        output.extend(after_loop);
    }
    block.0 = output;
    flow
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Break, Call, Continue, Global, If, Local, RcLocal, Return, While};

    fn call(name: &str) -> RValue {
        Call::new(Global::from(name).into(), vec![]).into()
    }

    fn effect() -> Statement {
        Call::new(Global::from("effect").into(), vec![]).into()
    }

    fn latch() -> Statement {
        If::new(call("done"), Block(vec![Break {}.into()]), Block::default()).into()
    }

    fn loop_block(statements: Vec<Statement>) -> Block {
        Block(vec![While::new(Literal::Boolean(true).into(), Block(statements)).into()])
    }

    #[test]
    fn removes_else_after_small_guard_without_reversing_condition() {
        let mut block = Block(vec![If::new(call("test"),
            Block(vec![Return::default().into()]), Block(vec![effect()])).into(), effect()]);
        cleanup_control_flow(&mut block);
        assert_eq!(block.0.len(), 3);
        let Statement::If(guard) = &block.0[0] else { panic!() };
        assert!(guard.else_block.lock().0.is_empty());
        assert!(matches!(guard.condition, RValue::Call(_)));
    }

    #[test]
    fn keeps_declarations_inside_their_scope() {
        let local = RcLocal::new(Local::new(Some("effect".into())));
        let mut declaration = Assign::new(vec![local.into()], vec![Literal::Nil.into()]);
        declaration.prefix = true;
        let mut block = Block(vec![If::new(call("test"),
            Block(vec![Return::default().into()]), Block(vec![declaration.clone().into()])).into(), effect()]);
        cleanup_control_flow(&mut block);
        let Statement::If(guard) = &block.0[0] else { panic!() };
        assert_eq!(guard.else_block.lock().0.len(), 1);
        let mut block = loop_block(vec![declaration.into(), Break {}.into()]);
        cleanup_control_flow(&mut block);
        assert!(matches!(block.0[0], Statement::While(_)));
    }

    #[test]
    fn preserves_an_existing_elseif_chain() {
        let second = If::new(call("second"), Block(vec![Return::default().into()]),
            Block(vec![effect()]));
        let first = If::new(call("first"), Block(vec![Return::default().into()]),
            Block(vec![second.into()]));
        let mut block = Block(vec![first.into(), effect()]);
        cleanup_control_flow(&mut block);
        let Statement::If(first) = &block.0[0] else { panic!() };
        {
            let otherwise = first.else_block.lock();
            let Statement::If(second) = &otherwise.0[0] else { panic!() };
            assert!(matches!(second.else_block.lock().0.as_slice(), [Statement::Call(_)]));
        }
        assert!(block.to_string().contains("elseif second() then"));
    }

    #[test]
    fn recovers_repeat_but_keeps_continue_evaluation_point() {
        let mut block = loop_block(vec![effect(), latch()]);
        cleanup_control_flow(&mut block);
        let Statement::Repeat(repeat) = &block.0[0] else { panic!() };
        assert_eq!(repeat.block.lock().0.len(), 1);
        assert!(matches!(repeat.condition, RValue::Call(_)));

        let mut block = loop_block(vec![If::new(call("skip"),
            Block(vec![Continue {}.into()]), Block::default()).into(), effect(), latch()]);
        cleanup_control_flow(&mut block);
        assert!(matches!(block.0[0], Statement::While(_)), "continue must skip done()");
    }

    #[test]
    fn recovers_head_test_even_with_continue() {
        let mut block = loop_block(vec![latch(), effect(), Continue {}.into()]);
        cleanup_control_flow(&mut block);
        let Statement::While(node) = &block.0[0] else { panic!() };
        assert_eq!(node.condition.to_string(), "not done()");
        assert_eq!(node.block.lock().0.len(), 2);
        assert_eq!(block.to_string().matches("done()").count(), 1);
        let once = block.to_string();
        cleanup_control_flow(&mut block);
        assert_eq!(block.to_string(), once);
    }

    #[test]
    fn head_test_does_not_flip_nan_sensitive_comparison() {
        let condition = crate::Binary {
            node_origin: Default::default(),
            left: Box::new(call("left")),
            right: Box::new(call("right")),
            operation: crate::BinaryOperation::LessThan,
        }.into();
        let guard = If::new(condition, Block(vec![Break {}.into()]), Block::default());
        let mut block = loop_block(vec![guard.into(), effect()]);
        cleanup_control_flow(&mut block);
        let Statement::While(node) = &block.0[0] else { panic!() };
        assert_eq!(node.condition.to_string(), "not (left() < right())");
    }

    #[test]
    fn keeps_effects_before_and_inside_exit_guard() {
        let mut block = loop_block(vec![effect(), latch(), effect()]);
        cleanup_control_flow(&mut block);
        let Statement::While(node) = &block.0[0] else { panic!() };
        assert_eq!(node.condition, Literal::Boolean(true).into());
        let guard = If::new(call("done"), Block(vec![effect(), Break {}.into()]), Block::default());
        let mut block = loop_block(vec![guard.into(), effect()]);
        cleanup_control_flow(&mut block);
        let Statement::While(node) = &block.0[0] else { panic!() };
        assert_eq!(node.condition, Literal::Boolean(true).into());
    }

    #[test]
    fn inner_loop_controls_do_not_block_outer_repeat() {
        let inner = While::new(call("inner"), Block(vec![Continue {}.into()]));
        let mut block = loop_block(vec![inner.into(), latch()]);
        cleanup_control_flow(&mut block);
        assert!(matches!(block.0[0], Statement::Repeat(_)));
    }

    #[test]
    fn flattens_only_a_single_unconditional_loop_exit() {
        let mut block = loop_block(vec![effect(), Break {}.into()]);
        cleanup_control_flow(&mut block);
        assert!(matches!(block.0.as_slice(), [Statement::Call(_)]));
        let mut block = loop_block(vec![If::new(call("exit"),
            Block(vec![Break {}.into()]), Block::default()).into(), effect(), Break {}.into()]);
        cleanup_control_flow(&mut block);
        assert!(matches!(block.0[0], Statement::While(_)), "an inner break must keep its target");
    }

    #[test]
    fn cleans_closures_in_assignment_targets() {
        let function = triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
            body: loop_block(vec![effect(), latch()]),
            ..Default::default()
        }));
        let closure = crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(function.clone()),
            upvalues: vec![],
        };
        let index = crate::Index::new(Global::from("table").into(),
            Call::new(closure.into(), vec![]).into());
        let mut block = Block(vec![Assign::new(vec![index.into()], vec![Literal::Nil.into()]).into()]);
        cleanup_control_flow(&mut block);
        assert!(matches!(function.lock().body.0[0], Statement::Repeat(_)));
    }
}
