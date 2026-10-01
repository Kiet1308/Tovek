//! A multiple assignment to existing locals from one call.
//!
//! `x, y = f()` into locals or upvalues compiles to a call into fresh
//! registers followed by one store per target, which the lifter shows as
//! temporaries:
//!
//! ```lua
//! local v3, v4 = pcall(load)            -->  v, v2 = pcall(load)
//! v = v3
//! v2 = v4
//! ```
//!
//! When every temporary is generated and read only by its store, and the
//! stores directly follow the call, the multiple assignment does the same: the
//! call runs once, then every target takes its value, with nothing in between.
//! Folding first lets naming see the call's results land in the targets
//! (`success, result = pcall(load)`).
//!
//! A swap `a, b = b, a` is split the same way, through one temporary:
//!
//! ```lua
//! local v3 = left                       -->  left, right = right, left
//! left = right
//! right = v3
//! ```

use rustc_hash::FxHashMap;

use crate::{inline_temps::Usage, Block, LValue, RValue, RcLocal, Statement};

pub fn fold_tuple_copies(block: &mut Block) {
    // The shape is rare; count usage only when some block contains it.
    if has_shape(block) {
        let usage = crate::inline_temps::collect_usage(block);
        fold_block(block, &usage);
    }
}

fn has_shape(block: &Block) -> bool {
    block.0.windows(2).any(|pair| tuple_temporaries(&pair[0]).is_some_and(|temporaries| copied_from(&pair[1], &temporaries).is_some()))
        || block.0.windows(3).any(|triple| swap_shape(triple).is_some())
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
    while index < block.0.len() {
        if let Some(targets) = targets(&block.0, index, usage) {
            block.0.drain(index + 1..=index + targets.len());
            let Statement::Assign(assign) = &mut block.0[index] else { unreachable!() };
            assign.left = targets.into_iter().map(LValue::Local).collect();
            assign.prefix = false;
        } else if let Some((temporary, first, second)) = block.0.get(index..index + 3).and_then(swap_shape)
            && !temporary.has_source_binding()
            && !temporary.preserve_binding()
            && usage.get(temporary).is_some_and(|usage| usage.reads == 1 && usage.writes == 1 && !usage.captured)
        {
            let (first, second) = (first.clone(), second.clone());
            block.0.drain(index + 1..index + 3);
            let Statement::Assign(assign) = &mut block.0[index] else { unreachable!() };
            assign.left = vec![LValue::Local(first.clone()), LValue::Local(second.clone())];
            assign.right = vec![RValue::Local(second), RValue::Local(first)];
            assign.prefix = false;
        }
        index += 1;
    }
}

/// `local t = a; a = b; b = t` for distinct locals: (t, a, b).
fn swap_shape(statements: &[Statement]) -> Option<(&RcLocal, &RcLocal, &RcLocal)> {
    let [Statement::Assign(save), Statement::Assign(shift), Statement::Assign(restore)] = statements else {
        return None;
    };
    fn copy(assign: &crate::Assign) -> Option<(&RcLocal, &RcLocal)> {
        match (assign.left.as_slice(), assign.right.as_slice()) {
            ([LValue::Local(target)], [RValue::Local(source)]) if !assign.parallel => Some((target, source)),
            _ => None,
        }
    }
    let ((temporary, first), (shifted, second), (restored, saved)) = (copy(save)?, copy(shift)?, copy(restore)?);
    let distinct = first != second && temporary != first && temporary != second;
    (save.prefix && !shift.prefix && !restore.prefix && distinct && shifted == first && restored == second && saved == temporary)
        .then_some((temporary, first, second))
}

/// The locals `local t1, t2, ... = <call or ...>` declares, if it is that.
fn tuple_temporaries(statement: &Statement) -> Option<Vec<&RcLocal>> {
    let Statement::Assign(declaration) = statement else { return None };
    let [value] = declaration.right.as_slice() else { return None };
    if !declaration.prefix
        || declaration.parallel
        || declaration.left.len() < 2
        || !matches!(value, RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_))
    {
        return None;
    }
    declaration.left.iter().map(LValue::as_local).collect()
}

/// `target = t` for one of `temporaries`: (target, position of `t`).
fn copied_from<'a>(statement: &'a Statement, temporaries: &[&RcLocal]) -> Option<(&'a RcLocal, usize)> {
    let Statement::Assign(copy) = statement else { return None };
    let ([LValue::Local(target)], [RValue::Local(source)]) = (copy.left.as_slice(), copy.right.as_slice()) else {
        return None;
    };
    if copy.prefix || copy.parallel || temporaries.contains(&target) {
        return None;
    }
    Some((target, temporaries.iter().position(|temporary| *temporary == source)?))
}

/// The targets, in declaration order, of a tuple declaration at `index` whose
/// generated temporaries are each stored once into a distinct local by the
/// statements right after it and read nowhere else.
fn targets(stmts: &[Statement], index: usize, usage: &FxHashMap<RcLocal, Usage>) -> Option<Vec<RcLocal>> {
    let temporaries = tuple_temporaries(&stmts[index])?;
    let copies = stmts.get(index + 1..=index + temporaries.len())?;
    let mut targets: Vec<Option<&RcLocal>> = vec![None; temporaries.len()];
    for copy in copies {
        let (target, slot) = copied_from(copy, &temporaries)?;
        if targets[slot].is_some() || targets.contains(&Some(target)) {
            return None;
        }
        targets[slot] = Some(target);
    }
    let single_use = temporaries.iter().all(|temporary| {
        !temporary.has_source_binding()
            && !temporary.preserve_binding()
            && usage.get(*temporary).is_some_and(|usage| usage.reads == 1 && usage.writes == 1 && !usage.captured)
    });
    single_use.then(|| targets.into_iter().map(|target| target.unwrap().clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::fold_tuple_copies;
    use crate::{Assign, Block, Call, Global, LValue, Local, RValue, RcLocal};

    fn declare(temporaries: &[&RcLocal]) -> crate::Statement {
        let mut declaration = Assign::new(
            temporaries.iter().map(|temporary| LValue::Local((*temporary).clone())).collect(),
            vec![Call::new(RValue::Global(Global::from("pcall")), vec![RValue::Global(Global::from("load"))]).into()],
        );
        declaration.prefix = true;
        declaration.into()
    }

    fn copy(target: &RcLocal, source: &RcLocal) -> crate::Statement {
        Assign::new(vec![LValue::Local(target.clone())], vec![RValue::Local(source.clone())]).into()
    }

    #[test]
    fn stores_right_after_a_tuple_call_become_one_assignment() {
        let (ready, store) = (RcLocal::new(Local::new(Some("ready".into()))), RcLocal::new(Local::new(Some("store".into()))));
        let (first, second) = (RcLocal::new(Local::new(None)), RcLocal::new(Local::new(None)));
        let mut block = Block(vec![declare(&[&first, &second]), copy(&store, &second), copy(&ready, &first)]);
        fold_tuple_copies(&mut block);
        assert_eq!(block.to_string(), "ready, store = pcall(load)");
    }

    #[test]
    fn a_swap_through_a_temporary_becomes_one_assignment() {
        let (left, right) = (RcLocal::new(Local::new(Some("left".into()))), RcLocal::new(Local::new(Some("right".into()))));
        let temporary = RcLocal::new(Local::new(None));
        let mut save = Assign::new(vec![LValue::Local(temporary.clone())], vec![RValue::Local(left.clone())]);
        save.prefix = true;
        let mut block = Block(vec![save.into(), copy(&left, &right), copy(&right, &temporary)]);
        fold_tuple_copies(&mut block);
        assert_eq!(block.to_string(), "left, right = right, left");
    }

    #[test]
    fn a_temporary_read_elsewhere_keeps_the_declaration() {
        let (ready, store) = (RcLocal::new(Local::new(Some("ready".into()))), RcLocal::new(Local::new(Some("store".into()))));
        let (first, second) = (RcLocal::new(Local::new(None)), RcLocal::new(Local::new(None)));
        let mut block = Block(vec![
            declare(&[&first, &second]),
            copy(&ready, &first),
            copy(&store, &second),
            Call::new(RValue::Global(Global::from("print")), vec![RValue::Local(second.clone())]).into(),
        ]);
        fold_tuple_copies(&mut block);
        assert!(block.to_string().starts_with("local "), "{block}");
    }
}
