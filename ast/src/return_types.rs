//! Final-AST return annotations. Unknown calls, cells, metatables and result
//! packs stay unannotated; naming and bytecode parameter hints are not proofs.
use crate::{BinaryOperation, Block, Literal, RValue, Statement, UnaryOperation};

const NIL: u8 = 1;
const BOOLEAN: u8 = 2;
const NUMBER: u8 = 4;
const STRING: u8 = 8;

fn value_type(value: &RValue) -> Option<u8> {
    if crate::is_boolean(value) { return Some(BOOLEAN); }
    match value {
        RValue::Literal(Literal::Nil) => Some(NIL),
        RValue::Literal(Literal::Number(_)) => Some(NUMBER),
        RValue::Literal(Literal::String(_)) => Some(STRING),
        RValue::Unary(unary) => match unary.operation {
            UnaryOperation::Negate if value_type(&unary.value) == Some(NUMBER) => Some(NUMBER),
            UnaryOperation::Length if value_type(&unary.value) == Some(STRING)
                || matches!(unary.value.as_ref(), RValue::Table(_)) => Some(NUMBER),
            _ => None,
        },
        RValue::Binary(binary) => {
            let left = value_type(&binary.left)?;
            let right = value_type(&binary.right)?;
            match binary.operation {
                BinaryOperation::And | BinaryOperation::Or => Some(left | right),
                BinaryOperation::Concat if (left | right) & !(STRING | NUMBER) == 0 => Some(STRING),
                BinaryOperation::Add | BinaryOperation::Sub | BinaryOperation::Mul
                | BinaryOperation::Div | BinaryOperation::IDiv | BinaryOperation::Mod
                | BinaryOperation::Pow if left == NUMBER && right == NUMBER => Some(NUMBER),
                _ => None,
            }
        }
        RValue::IfExpression(select) => Some(value_type(&select.then_value)? | value_type(&select.else_value)?),
        _ => None,
    }
}

/// A successful evaluation cannot yield nil, without trusting calls or hints.
pub(crate) fn is_proven_non_nil(value: &RValue) -> bool {
    match value {
        RValue::Literal(literal) => !matches!(literal, Literal::Nil),
        RValue::Table(_) | RValue::Closure(_) => true,
        _ => value_type(value).is_some_and(|ty| ty & NIL == 0),
    }
}

fn merge(pack: &mut Option<Vec<u8>>, next: Vec<u8>) -> Option<()> {
    if let Some(previous) = pack {
        if previous.len() != next.len() { return None; }
        for (previous, next) in previous.iter_mut().zip(next) { *previous |= next; }
    } else {
        *pack = Some(next);
    }
    Some(())
}

/// Whether the end is reachable. Loops may execute zero times or break;
/// return packs inside them still contribute without proving termination.
fn returns(block: &Block, pack: &mut Option<Vec<u8>>) -> Option<bool> {
    for statement in &block.0 {
        match statement {
            Statement::Return(ret) => {
                merge(pack, ret.values.iter().map(value_type).collect::<Option<_>>()?)?;
                return Some(false);
            }
            Statement::Break(_) | Statement::Continue(_) => return Some(false),
            Statement::If(branch) => {
                let yes = returns(&branch.then_block.lock(), pack)?;
                let no = returns(&branch.else_block.lock(), pack)?;
                if !yes && !no { return Some(false); }
            }
            Statement::While(node) => { returns(&node.block.lock(), pack)?; }
            Statement::Repeat(node) => { returns(&node.block.lock(), pack)?; }
            Statement::NumericFor(node) => { returns(&node.block.lock(), pack)?; }
            Statement::GenericFor(node) => { returns(&node.block.lock(), pack)?; }
            Statement::Goto(_) | Statement::Label(_) | Statement::NumForInit(_)
            | Statement::NumForNext(_) | Statement::GenericForInit(_) | Statement::GenericForNext(_) => return None,
            _ => {}
        }
    }
    Some(true)
}

fn annotation(block: &Block) -> Option<String> {
    let mut pack = None;
    if returns(block, &mut pack)? { merge(&mut pack, Vec::new())?; }
    let types: Vec<_> = pack?.into_iter().map(|ty| {
        [(NIL, "nil"), (BOOLEAN, "boolean"), (NUMBER, "number"), (STRING, "string")]
            .into_iter().filter_map(|(bit, name)| (ty & bit != 0).then_some(name))
            .collect::<Vec<_>>().join(" | ")
    }).collect();
    Some(if types.len() == 1 { types[0].clone() } else { format!("({})", types.join(", ")) })
}

pub fn annotate(block: &mut Block) {
    fn walk(block: &mut Block, seen: &mut rustc_hash::FxHashSet<usize>) {
        for statement in &mut block.0 {
            let mut functions = Vec::new();
            crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
                functions.push(closure.function.clone());
            });
            for function in functions {
                if !seen.insert(triomphe::Arc::as_ptr(&function.0) as usize) { continue; }
                let mut function = function.lock();
                function.return_annotation = annotation(&function.body);
                walk(&mut function.body, seen);
            }
            match statement {
                Statement::If(branch) => {
                    walk(&mut branch.then_block.lock(), seen);
                    walk(&mut branch.else_block.lock(), seen);
                }
                Statement::While(node) => walk(&mut node.block.lock(), seen),
                Statement::Repeat(node) => walk(&mut node.block.lock(), seen),
                Statement::NumericFor(node) => walk(&mut node.block.lock(), seen),
                Statement::GenericFor(node) => walk(&mut node.block.lock(), seen),
                _ => {}
            }
        }
    }
    walk(block, &mut Default::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Binary, Call, Global, If, Return};
    fn returned(values: Vec<RValue>) -> Block { Block(vec![Return::new(values).into()]) }
    fn number() -> RValue { Literal::Number(1.0).into() }
    fn flag() -> RValue { Global::from("flag").into() }

    #[test]
    fn joins_proven_types_only_when_every_path_has_the_same_arity() {
        let branch = |yes, no| Block(vec![If::new(flag(), yes, no).into()]);
        let body = branch(returned(vec![number()]), returned(vec![Literal::String(b"one".to_vec()).into()]));
        assert_eq!(annotation(&body).as_deref(), Some("number | string"));
        assert_eq!(annotation(&returned(vec![number(), Literal::Nil.into()])).as_deref(), Some("(number, nil)"));
        assert_eq!(annotation(&Block::default()).as_deref(), Some("()"));
        assert!(annotation(&branch(returned(vec![number()]), Block::default())).is_none());
        assert!(annotation(&branch(returned(vec![Literal::Nil.into()]), returned(vec![]))).is_none());
    }

    #[test]
    fn operators_do_not_prove_types_from_names_or_metamethods() {
        let unknown = crate::RcLocal::new(crate::Local::with_type_hint("number".into()));
        let binary = |op| Binary::new(unknown.clone().into(), number(), op).into();
        assert_eq!(annotation(&returned(vec![binary(BinaryOperation::Equal)])).as_deref(), Some("boolean"));
        assert!(annotation(&returned(vec![binary(BinaryOperation::Add)])).is_none());
        assert!(annotation(&returned(vec![Call::new(Global::from("tonumber").into(), vec![number()]).into()])).is_none());
    }

    #[test]
    fn loops_do_not_hide_fallthrough_or_other_returns() {
        let mut body = Block(vec![crate::While::new(flag(), returned(vec![number()])).into()]);
        assert!(annotation(&body).is_none());
        body.0.push(Return::new(vec![Literal::Boolean(false).into()]).into());
        assert_eq!(annotation(&body).as_deref(), Some("boolean | number"));
    }
}
