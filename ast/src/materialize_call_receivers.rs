//! Restore a named local when a property-assignment receiver is a call.
//!
//! `factory(...).Parent = target` is legal Luau, but it hides the object whose
//! property is being mutated and is rarely how Roblox source is written. More
//! importantly, this shape can already exist before the late UI inliner runs,
//! so merely refusing to create new instances is insufficient.

use rustc_hash::FxHashSet;

use crate::{Assign, Block, LValue, Literal, Local, RValue, RcLocal, Select, Statement, Traverse};

const MAX_ACTIVE_LOCALS: usize = 200;

pub fn materialize_call_assignment_receivers(block: &mut Block) {
    let mut context = FunctionContext::new(block, 0);
    materialize_block(block, &mut context);
}

struct FunctionContext {
    local_headroom: usize,
    reserved: FxHashSet<String>,
}

impl FunctionContext {
    fn new(block: &mut Block, parameter_count: usize) -> Self {
        let mut reserved = FxHashSet::default();
        crate::rehoist_constants::collect_reserved_identifiers(block, &mut reserved);
        let declared = count_declared_locals(block);
        Self {
            local_headroom: MAX_ACTIVE_LOCALS.saturating_sub(parameter_count + declared),
            reserved,
        }
    }

    fn fresh_receiver(&mut self, hint: Option<String>) -> Option<RcLocal> {
        if self.local_headroom == 0 {
            return None;
        }
        self.local_headroom -= 1;
        let base = hint.as_deref().unwrap_or("instance");
        let name = crate::rehoist_constants::unique_name(base, &mut self.reserved);
        Some(RcLocal::new(Local::new(Some(name))))
    }
}

fn count_declared_locals(block: &Block) -> usize {
    block
        .0
        .iter()
        .map(|statement| {
            let direct = match statement {
                Statement::Assign(assign) if assign.prefix => assign
                    .left
                    .iter()
                    .filter(|left| matches!(left, LValue::Local(_)))
                    .count(),
                Statement::NumericFor(_) => 1,
                Statement::GenericFor(node) => node.res_locals.len(),
                _ => 0,
            };
            direct
                + match statement {
                    Statement::If(node) => {
                        count_declared_locals(&node.then_block.lock())
                            + count_declared_locals(&node.else_block.lock())
                    }
                    Statement::While(node) => count_declared_locals(&node.block.lock()),
                    Statement::Repeat(node) => count_declared_locals(&node.block.lock()),
                    Statement::NumericFor(node) => count_declared_locals(&node.block.lock()),
                    Statement::GenericFor(node) => count_declared_locals(&node.block.lock()),
                    _ => 0,
                }
        })
        .sum()
}

fn materialize_block(block: &mut Block, context: &mut FunctionContext) {
    let mut index = 0;
    while index < block.0.len() {
        materialize_nested_in_statement(&mut block.0[index], context);

        let Some(hint) = receiver_call(&block.0[index]).map(call_result_hint) else {
            index += 1;
            continue;
        };
        let Some(local) = context.fresh_receiver(hint) else {
            index += 1;
            continue;
        };
        let call = take_receiver_call(&mut block.0[index], &local)
            .expect("receiver shape was checked immediately before mutation");
        let mut declaration = Assign::new(vec![LValue::Local(local)], vec![call]);
        declaration.prefix = true;
        block.0.insert(index, declaration.into());
        index += 2;
    }
}

fn materialize_nested_in_statement(statement: &mut Statement, context: &mut FunctionContext) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |value| -> Option<()> {
        if let RValue::Closure(closure) = value {
            functions.push(closure.function.clone());
        }
        None
    });
    for function in functions {
        let mut function = function.lock();
        let parameter_count = function.parameters.len();
        let mut nested_context = FunctionContext::new(&mut function.body, parameter_count);
        materialize_block(&mut function.body, &mut nested_context);
    }

    match statement {
        Statement::If(node) => {
            materialize_block(&mut node.then_block.lock(), context);
            materialize_block(&mut node.else_block.lock(), context);
        }
        Statement::While(node) => materialize_block(&mut node.block.lock(), context),
        Statement::Repeat(node) => materialize_block(&mut node.block.lock(), context),
        Statement::NumericFor(node) => materialize_block(&mut node.block.lock(), context),
        Statement::GenericFor(node) => materialize_block(&mut node.block.lock(), context),
        _ => {}
    }
}

fn receiver_call(statement: &Statement) -> Option<&RValue> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if assign.prefix || assign.parallel || assign.left.len() != 1 {
        return None;
    }
    let LValue::Index(index) = &assign.left[0] else {
        return None;
    };
    call_root(&index.left)
}

fn call_root(mut value: &RValue) -> Option<&RValue> {
    loop {
        match value {
            RValue::Index(index) => value = &index.left,
            RValue::Call(_)
            | RValue::MethodCall(_)
            | RValue::Select(Select::Call(_) | Select::MethodCall(_)) => return Some(value),
            _ => return None,
        }
    }
}

fn take_receiver_call(statement: &mut Statement, local: &RcLocal) -> Option<RValue> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    let LValue::Index(index) = &mut assign.left[0] else {
        return None;
    };
    let mut value = index.left.as_mut();
    loop {
        match value {
            RValue::Index(index) => value = index.left.as_mut(),
            RValue::Call(_)
            | RValue::MethodCall(_)
            | RValue::Select(Select::Call(_) | Select::MethodCall(_)) => {
                return Some(std::mem::replace(value, RValue::Local(local.clone())))
            }
            _ => return None,
        }
    }
}

fn call_result_hint(value: &RValue) -> Option<String> {
    let raw = match value {
        RValue::Call(call) => callee_name(&call.value),
        RValue::MethodCall(call) => Some(call.method.clone()),
        RValue::Select(Select::Call(call)) => callee_name(&call.value),
        RValue::Select(Select::MethodCall(call)) => Some(call.method.clone()),
        _ => None,
    }?;
    sanitize_hint(&raw)
}

fn callee_name(value: &RValue) -> Option<String> {
    match value {
        RValue::Local(local) => local.0 .0.lock().0.clone(),
        RValue::Global(global) => std::str::from_utf8(&global.0).ok().map(str::to_owned),
        RValue::Index(index) => match index.right.as_ref() {
            RValue::Literal(Literal::String(bytes)) => {
                std::str::from_utf8(bytes).ok().map(str::to_owned)
            }
            _ => None,
        },
        RValue::Call(call) => callee_name(&call.value),
        _ => None,
    }
}

fn sanitize_hint(raw: &str) -> Option<String> {
    const KEYWORDS: &[&str] = &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
        "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    let mut name = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>();
    if name.is_empty() {
        return None;
    }
    if name.as_bytes()[0].is_ascii_digit() {
        name.insert(0, '_');
    }
    if let Some(first) = name.get_mut(0..1) {
        first.make_ascii_lowercase();
    }
    if name == "_" || name == "self" || KEYWORDS.contains(&name.as_str()) {
        None
    } else {
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::{materialize_call_assignment_receivers, MAX_ACTIVE_LOCALS};
    use crate::{
        Assign, Block, Call, Global, Index, LValue, Literal, Local, RValue, RcLocal, Statement,
        Table,
    };

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn local_value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    #[test]
    fn restores_named_local_for_call_rooted_property_assignment() {
        let theme = local("theme");
        let image_label = local("imageLabel");
        let call = Call::new(
            RValue::Index(Index::new(local_value(&theme), string("titleText"))),
            vec![RValue::Table(Table::default())],
        );
        let mut block = Block(vec![Assign::new(
            vec![LValue::Index(Index::new(
                RValue::Call(call),
                string("Parent"),
            ))],
            vec![local_value(&image_label)],
        )
        .into()]);

        materialize_call_assignment_receivers(&mut block);

        assert_eq!(
            block.to_string(),
            "local titleText = theme.titleText({})\ntitleText.Parent = imageLabel"
        );
        let once = block.to_string();
        materialize_call_assignment_receivers(&mut block);
        assert_eq!(block.to_string(), once);
    }

    #[test]
    fn does_not_add_receiver_local_without_register_headroom() {
        let mut statements = (0..MAX_ACTIVE_LOCALS)
            .map(|index| {
                let local = local(&format!("value{index}"));
                let mut declaration = Assign::new(
                    vec![LValue::Local(local)],
                    vec![RValue::Literal(Literal::Number(index as f64))],
                );
                declaration.prefix = true;
                declaration.into()
            })
            .collect::<Vec<_>>();
        statements.push(
            Assign::new(
                vec![LValue::Index(Index::new(
                    Call::new(RValue::Global(Global::from("makeInstance")), vec![]).into(),
                    string("Parent"),
                ))],
                vec![RValue::Literal(Literal::Nil)],
            )
            .into(),
        );
        let mut block = Block(statements);

        materialize_call_assignment_receivers(&mut block);

        assert_eq!(block.0.len(), MAX_ACTIVE_LOCALS + 1);
        let Statement::Assign(assign) = block.0.last().unwrap() else {
            panic!()
        };
        let LValue::Index(index) = &assign.left[0] else {
            panic!()
        };
        assert!(matches!(index.left.as_ref(), RValue::Call(_)));
    }
}
