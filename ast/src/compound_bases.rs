//! Compound assignment through a base that must be evaluated once.
//!
//! `t[i].k += e` compiles to one evaluation of `t[i]`, then a read of `k`,
//! `e`, the operator and the store. The lifter sees that register as a
//! temporary:
//!
//! ```lua
//! local v2 = textures[i]                -->  textures[i].OffsetStudsV += step
//! v2.OffsetStudsV += step
//! ```
//!
//! When a generated temporary is read by that one statement only, the compound
//! form evaluates the same things in the same order. The expanded form
//! `t[i].k = t[i].k + e` would evaluate `t[i]` twice, so the assignment carries
//! [`crate::Assign::compound`] and the formatter always prints `op=`.

use rustc_hash::FxHashMap;

use crate::{inline_temps::Usage, BinaryOperation, Block, LValue, RValue, RcLocal, Statement};

pub fn fold_compound_bases(block: &mut Block) {
    // The shape is rare; count usage only when some block contains it.
    if has_shape(block) {
        let usage = crate::inline_temps::collect_usage(block);
        fold_block(block, &usage);
    }
}

fn has_shape(block: &Block) -> bool {
    block.0.windows(2).any(|pair| shape(&pair[0], &pair[1]).is_some())
        || block.0.iter().any(|statement| {
            let mut found = false;
            crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
                found = found || has_shape(&closure.function.lock().body);
            });
            found || match statement {
                Statement::If(r#if) => has_shape(&r#if.then_block.lock()) || has_shape(&r#if.else_block.lock()),
                Statement::While(r#while) => has_shape(&r#while.block.lock()),
                Statement::Repeat(repeat) => has_shape(&repeat.block.lock()),
                Statement::NumericFor(numeric_for) => has_shape(&numeric_for.block.lock()),
                Statement::GenericFor(generic_for) => has_shape(&generic_for.block.lock()),
                _ => false,
            }
        })
}

fn fold_block(block: &mut Block, usage: &FxHashMap<RcLocal, Usage>) {
    for statement in &mut block.0 {
        let mut functions = Vec::new();
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
            functions.push(closure.function.clone());
        });
        for function in functions {
            fold_block(&mut function.lock().body, usage);
        }
        match statement {
            Statement::If(r#if) => {
                fold_block(&mut r#if.then_block.lock(), usage);
                fold_block(&mut r#if.else_block.lock(), usage);
            }
            Statement::While(r#while) => fold_block(&mut r#while.block.lock(), usage),
            Statement::Repeat(repeat) => fold_block(&mut repeat.block.lock(), usage),
            Statement::NumericFor(numeric_for) => fold_block(&mut numeric_for.block.lock(), usage),
            Statement::GenericFor(generic_for) => fold_block(&mut generic_for.block.lock(), usage),
            _ => {}
        }
    }
    let mut index = 0;
    while index + 1 < block.0.len() {
        if folds(&block.0[index], &block.0[index + 1], usage) {
            let Statement::Assign(declaration) = block.0.remove(index) else { unreachable!() };
            let base = declaration.right.into_iter().next().unwrap();
            let Statement::Assign(update) = &mut block.0[index] else { unreachable!() };
            let (LValue::Index(target), RValue::Binary(binary)) = (&mut update.left[0], &mut update.right[0]) else {
                unreachable!()
            };
            let RValue::Index(read) = binary.left.as_mut() else { unreachable!() };
            *read.left = base.clone();
            *target.left = base;
            update.compound = true;
        } else {
            index += 1;
        }
    }
}

/// A generated, uncaptured temporary written once and read exactly twice,
/// in the shape of [`shape`].
fn folds(declaration: &Statement, update: &Statement, usage: &FxHashMap<RcLocal, Usage>) -> bool {
    shape(declaration, update).is_some_and(|temporary| {
        usage.get(temporary).is_some_and(|usage| usage.reads == 2 && usage.writes == 1 && !usage.captured)
    })
}

/// `local t = <index or global>` followed by `t.k = t.k op e` with a
/// repeatable key, for a generated `t`.
fn shape<'a>(declaration: &'a Statement, update: &Statement) -> Option<&'a RcLocal> {
    let (Statement::Assign(declaration), Statement::Assign(update)) = (declaration, update) else { return None };
    let ([LValue::Local(temporary)], [RValue::Index(_) | RValue::Global(_)]) =
        (declaration.left.as_slice(), declaration.right.as_slice())
    else {
        return None;
    };
    let is_temporary = |value: &RValue| matches!(value, RValue::Local(local) if local == temporary);
    let matches = declaration.prefix && !declaration.parallel
        && !temporary.has_source_binding() && !temporary.preserve_binding()
        && !update.prefix && !update.parallel
        && matches!(
            (update.left.as_slice(), update.right.as_slice()),
            ([LValue::Index(target)], [RValue::Binary(binary)])
                if is_temporary(&target.left)
                    && matches!(target.right.as_ref(), RValue::Local(_) | RValue::Literal(_))
                    && compound_operation(binary.operation)
                    && matches!(binary.left.as_ref(), RValue::Index(read)
                        if is_temporary(&read.left) && read.right == target.right)
        );
    matches.then_some(temporary)
}

fn compound_operation(operation: BinaryOperation) -> bool {
    matches!(
        operation,
        BinaryOperation::Add
            | BinaryOperation::Sub
            | BinaryOperation::Mul
            | BinaryOperation::Div
            | BinaryOperation::IDiv
            | BinaryOperation::Mod
            | BinaryOperation::Pow
            | BinaryOperation::Concat
    )
}

#[cfg(test)]
mod tests {
    use super::fold_compound_bases;
    use crate::{Assign, Binary, BinaryOperation, Block, Global, Index, LValue, Literal, Local, RValue, RcLocal};

    fn string(text: &str) -> RValue {
        RValue::Literal(Literal::String(text.as_bytes().to_vec()))
    }

    fn update(object: &RcLocal, step: RValue) -> crate::Statement {
        let field = || Index::new(RValue::Local(object.clone()), string("Offset"));
        Assign::new(vec![LValue::Index(field())],
            vec![Binary::new(field().into(), step, BinaryOperation::Add).into()]).into()
    }

    #[test]
    fn a_once_evaluated_base_becomes_a_compound_target() {
        let textures = RcLocal::new(Local::new(Some("textures".into())));
        let i = RcLocal::new(Local::new(Some("i".into())));
        let temporary = RcLocal::new(Local::new(None));
        let mut declaration = Assign::new(vec![LValue::Local(temporary.clone())],
            vec![Index::new(RValue::Local(textures), RValue::Local(i)).into()]);
        declaration.prefix = true;
        let mut block = Block(vec![declaration.into(), update(&temporary, RValue::Global(Global::from("step")))]);
        fold_compound_bases(&mut block);
        assert_eq!(block.to_string(), "textures[i].Offset += step");
    }

    #[test]
    fn a_temporary_read_again_keeps_its_declaration() {
        let temporary = RcLocal::new(Local::new(None));
        let mut declaration = Assign::new(vec![LValue::Local(temporary.clone())],
            vec![Index::new(RValue::Global(Global::from("parts")), string("Main")).into()]);
        declaration.prefix = true;
        let mut block = Block(vec![
            declaration.into(),
            update(&temporary, RValue::Local(temporary.clone())),
        ]);
        fold_compound_bases(&mut block);
        assert!(block.to_string().starts_with("local "), "{block}");
    }
}
