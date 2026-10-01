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
//!
//! A local base is a snapshot too when the compound form reads it once: an
//! upvalue is loaded into a register, while a register local is used in place,
//! so it must be one no closure can rebind while `e` runs:
//!
//! ```lua
//! local v2 = counter                    -->  counter.DependentCount -= 1
//! v2.DependentCount -= 1                     (`counter` an upvalue here)
//! ```

use rustc_hash::FxHashMap;

use crate::{inline_temps::Usage, BinaryOperation, Block, LValue, RValue, RcLocal, Statement, Upvalue};

pub fn fold_compound_bases(block: &mut Block) {
    // The shape is rare; count usage only when some block contains it.
    if has_shape(block) {
        let usage = crate::inline_temps::collect_usage(block);
        fold_block(block, &usage, &[]);
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

/// `upvalues` are those of the function `block` belongs to.
fn fold_block(block: &mut Block, usage: &FxHashMap<RcLocal, Usage>, upvalues: &[Upvalue]) {
    for statement in &mut block.0 {
        let mut functions = Vec::new();
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
            functions.push((closure.function.clone(), closure.upvalues.clone()));
        });
        for (function, upvalues) in functions {
            fold_block(&mut function.lock().body, usage, &upvalues);
        }
        match statement {
            Statement::If(r#if) => {
                fold_block(&mut r#if.then_block.lock(), usage, upvalues);
                fold_block(&mut r#if.else_block.lock(), usage, upvalues);
            }
            Statement::While(r#while) => fold_block(&mut r#while.block.lock(), usage, upvalues),
            Statement::Repeat(repeat) => fold_block(&mut repeat.block.lock(), usage, upvalues),
            Statement::NumericFor(numeric_for) => fold_block(&mut numeric_for.block.lock(), usage, upvalues),
            Statement::GenericFor(generic_for) => fold_block(&mut generic_for.block.lock(), usage, upvalues),
            _ => {}
        }
    }
    let mut index = 0;
    while index + 1 < block.0.len() {
        if folds(&block.0[index], &block.0[index + 1], usage, upvalues) {
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
/// in the shape of [`shape`]. A local base must be an upvalue of the function
/// or captured by no closure.
fn folds(declaration: &Statement, update: &Statement, usage: &FxHashMap<RcLocal, Usage>, upvalues: &[Upvalue]) -> bool {
    shape(declaration, update).is_some_and(|temporary| {
        usage.get(temporary).is_some_and(|usage| usage.reads == 2 && usage.writes == 1 && !usage.captured)
    }) && match declaration {
        Statement::Assign(declaration) => match &declaration.right[0] {
            RValue::Local(base) => {
                usage.get(base).is_none_or(|usage| !usage.captured)
                    || upvalues.iter().any(|upvalue| matches!(upvalue, Upvalue::Copy(local) | Upvalue::Ref(local) if local == base))
            }
            _ => true,
        },
        _ => false,
    }
}

/// `local t = <index, global or local>` followed by `t.k = t.k op e` with a
/// repeatable key, for a generated `t`.
fn shape<'a>(declaration: &'a Statement, update: &Statement) -> Option<&'a RcLocal> {
    let (Statement::Assign(declaration), Statement::Assign(update)) = (declaration, update) else { return None };
    let ([LValue::Local(temporary)], [RValue::Index(_) | RValue::Global(_) | RValue::Local(_)]) =
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
    use crate::{Assign, Binary, BinaryOperation, Block, Global, Index, LValue, Literal, Local, RValue, RcLocal, Upvalue};

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

    fn snapshot(temporary: &RcLocal, base: &RcLocal) -> crate::Statement {
        let mut declaration = Assign::new(vec![LValue::Local(temporary.clone())], vec![RValue::Local(base.clone())]);
        declaration.prefix = true;
        declaration.into()
    }

    fn closure(body: Vec<crate::Statement>, upvalues: Vec<Upvalue>) -> RValue {
        crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
                body: Block(body),
                ..Default::default()
            }))),
            upvalues,
        }
        .into()
    }

    /// A local base is read once by the compound form when it is an upvalue
    /// (loaded into a register) or captured by no closure; a register local a
    /// closure may rebind keeps its snapshot.
    #[test]
    fn a_local_snapshot_folds_only_where_nothing_can_rebind_the_base() {
        let step = || RValue::Global(Global::from("step"));
        let plain = RcLocal::new(Local::new(Some("plain".into())));
        let temporary = RcLocal::new(Local::new(None));
        let mut block = Block(vec![snapshot(&temporary, &plain), update(&temporary, step())]);
        fold_compound_bases(&mut block);
        assert_eq!(block.to_string(), "plain.Offset += step");

        // Captured and rebound by `swap`, read in place by its declaring function.
        let target = RcLocal::new(Local::new(Some("target".into())));
        let swap = RcLocal::new(Local::new(Some("swap".into())));
        let temporary = RcLocal::new(Local::new(None));
        let rebind = Assign::new(vec![LValue::Local(target.clone())], vec![RValue::Global(Global::from("other"))]);
        let mut install = Assign::new(vec![LValue::Local(swap.clone())], vec![closure(vec![rebind.into()], vec![Upvalue::Ref(target.clone())])]);
        install.prefix = true;
        let mut block = Block(vec![install.into(), snapshot(&temporary, &target), update(&temporary, step())]);
        fold_compound_bases(&mut block);
        assert!(block.to_string().contains("local "), "{block}");
        assert!(!block.to_string().contains("target.Offset +="), "{block}");

        // The same base read as an upvalue inside a closure folds.
        let inner = RcLocal::new(Local::new(None));
        let mut reader = Assign::new(vec![LValue::Local(RcLocal::new(Local::new(Some("reader".into()))))],
            vec![closure(vec![snapshot(&inner, &target), update(&inner, step())], vec![Upvalue::Ref(target.clone())])]);
        reader.prefix = true;
        let mut block = Block(vec![reader.into()]);
        fold_compound_bases(&mut block);
        assert!(block.to_string().contains("target.Offset += step"), "{block}");
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
