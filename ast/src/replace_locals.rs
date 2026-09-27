use std::collections::HashMap;

use itertools::Either;

use crate::{Block, LocalRw, RValue, RcLocal, Statement, Traverse};

pub fn replace_locals<H: std::hash::BuildHasher>(
    block: &mut Block,
    map: &HashMap<RcLocal, RcLocal, H>,
) {
    for statement in &mut block.0 {
        replace_locals_in_statement(statement, map);
    }
}

/// Rewrite one indexed statement, including its nested blocks and closures.
/// Callers with occurrence information need not revisit unrelated statements.
pub(crate) fn replace_locals_in_statement<H: std::hash::BuildHasher>(
    statement: &mut Statement,
    map: &HashMap<RcLocal, RcLocal, H>,
) {
        statement.visit_local_reads_mut(&mut |local| {
            if let Some(new_local) = map.get(local) {
                new_local.inherit_source_bindings(local);
                *local = new_local.clone();
            }
            true
        });
        for local in statement.values_written_mut() {
            if let Some(new_local) = map.get(local) {
                new_local.inherit_source_bindings(local);
                *local = new_local.clone();
            }
        }
        // TODO: traverse_values
        statement.post_traverse_values(&mut |value| -> Option<()> {
            if let Either::Right(RValue::Closure(closure)) = value {
                replace_locals(&mut closure.function.lock().body, map)
            };
            None
        });
        match statement {
            Statement::If(r#if) => {
                replace_locals(&mut r#if.then_block.lock(), map);
                replace_locals(&mut r#if.else_block.lock(), map);
            }
            Statement::While(r#while) => {
                replace_locals(&mut r#while.block.lock(), map);
            }
            Statement::Repeat(repeat) => {
                replace_locals(&mut repeat.block.lock(), map);
            }
            Statement::NumericFor(numeric_for) => {
                replace_locals(&mut numeric_for.block.lock(), map);
            }
            Statement::GenericFor(generic_for) => {
                replace_locals(&mut generic_for.block.lock(), map);
            }
            _ => {}
        }
}
