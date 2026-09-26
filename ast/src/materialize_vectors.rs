//! Vector constants must not look up a mutable environment every time a
//! function runs. Source has no vector literal token, so capture the standard
//! runtime constructor once, before executing the recovered chunk.
use rustc_hash::FxHashMap;

use crate::{
    Assign, Block, Call, Global, Index, Literal, Local, RValue, RcLocal, Statement, Traverse,
    Upvalue,
};

pub fn materialize_vectors(
    body: &mut Block,
) -> Result<Option<crate::local_producers::Pass>, &'static str> {
    fn contains_vector(value: &RValue) -> bool {
        matches!(
            value,
            RValue::Literal(Literal::Vector(..) | Literal::VectorD(..))
        ) || value.rvalues().into_iter().any(contains_vector)
    }
    let Some(mut inventory) = crate::lower_conditionals::prepare_local_rewrite(body, |block| {
        block
            .iter()
            .flat_map(crate::deinline::stmt_rvalues)
            .any(contains_vector)
    })?
    else {
        return Ok(None);
    };
    if crate::lower_conditionals::local_rewrite_frame_with_bound(body, &[], 0, register_bound)
        .headroom
        == 0
    {
        return Err("no local/register headroom for the vector constructor binding");
    }
    let name = crate::rehoist_constants::unique_name("createVector", &mut inventory.reserved);
    let constructor = RcLocal::new(Local::new(Some(name)));
    let mut context = Context {
        constructor,
        functions: FxHashMap::default(),
    };
    if !context.block(body) {
        return Ok(None);
    }
    let mut declaration = Assign::new(
        vec![context.constructor.clone().into()],
        vec![
            Index::new(
                Global(b"vector".to_vec()).into(),
                Literal::String(b"create".to_vec()).into(),
            )
            .into(),
        ],
    );
    declaration.prefix = true;
    body.0.insert(0, declaration.into());
    let mut ledger = crate::local_producers::Ledger::default();
    ledger.record(
        &context.constructor,
        crate::local_producers::Role::VectorConstructor,
    );
    Ok(Some(crate::local_producers::Pass {
        pass: "materialize_vectors",
        rewrite_model: "entry_constructor_snapshot_v1",
        introduced_locals: 1,
        ledger,
    }))
}

/// Luau's table emitter reuses scratch for record fields and flushes list
/// fields in batches of 16. Sibling fields and closure captures are not all
/// live expression temporaries at once. Keep the conservative sum elsewhere.
fn register_bound(value: &RValue) -> usize {
    match value {
        RValue::Closure(_) => 1, // CAPTURE reads existing local/upvalue slots
        RValue::Literal(Literal::Vector(..) | Literal::VectorD(..)) => 5,
        RValue::Table(table) => {
            let array = table
                .0
                .iter()
                .filter(|(key, _)| key.is_none())
                .count()
                .min(16);
            let field = table
                .0
                .iter()
                .map(|(key, value)| key.as_ref().map_or(0, register_bound) + register_bound(value))
                .max()
                .unwrap_or(0);
            1 + array + field
        }
        _ => {
            1 + value
                .rvalues()
                .into_iter()
                .map(register_bound)
                .sum::<usize>()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_record_tables_reuse_registers_and_reserve_the_constructor_name() {
        let mut fields: Vec<_> = (0..1000)
            .map(|i| {
                (
                    Some(Literal::String(format!("field{i}").into_bytes()).into()),
                    Literal::Number(i as f64).into(),
                )
            })
            .collect();
        fields.push((
            Some(Literal::String(b"vector".to_vec()).into()),
            Literal::Vector(0.1, -0.0, 1.0).into(),
        ));
        fields.push((
            Some(Literal::String(b"global".to_vec()).into()),
            Global(b"createVector".to_vec()).into(),
        ));
        let mut body = Block(vec![
            crate::Return::new(vec![crate::Table::new(fields).into()]).into(),
        ]);
        let report = materialize_vectors(&mut body).unwrap().unwrap();
        assert_eq!(report.introduced_locals, 1);
        let text = body.to_string();
        assert!(
            text.starts_with("local createVector_2 = vector.create"),
            "{text}"
        );
        assert!(text.contains("createVector_2(0.1, -0, 1)"), "{text}");
        assert!(text.contains("global = createVector"), "{text}");
    }
}

struct Context {
    constructor: RcLocal,
    functions: FxHashMap<usize, bool>,
}

impl Context {
    fn value(&mut self, value: &mut RValue) -> bool {
        let components = match value {
            RValue::Literal(Literal::Vector(x, y, z)) => {
                // Keep the shortest round-tripping f32 spelling in source.
                Some([*x, *y, *z].map(|n| ryu::Buffer::new().format(n).parse::<f64>().unwrap()))
            }
            RValue::Literal(Literal::VectorD(x, y, z)) => Some([*x, *y, *z]),
            _ => None,
        };
        if let Some(components) = components {
            *value = Call::new(
                self.constructor.clone().into(),
                components
                    .into_iter()
                    .map(|n| Literal::Number(n).into())
                    .collect(),
            )
            .into();
            return true;
        }
        if let RValue::Closure(closure) = value {
            let id = triomphe::Arc::as_ptr(&closure.function.0) as usize;
            let used = if let Some(&used) = self.functions.get(&id) {
                used
            } else {
                self.functions.insert(id, false);
                let used = self.block(&mut closure.function.lock().body);
                self.functions.insert(id, used);
                used
            };
            if used {
                closure
                    .upvalues
                    .push(Upvalue::Copy(self.constructor.clone()));
            }
            return used;
        }
        let mut used = false;
        for child in value.rvalues_mut() {
            used |= self.value(child);
        }
        used
    }

    fn block(&mut self, body: &mut Block) -> bool {
        let mut used = false;
        for statement in &mut body.0 {
            for value in crate::deinline::stmt_rvalues_mut(statement) {
                used |= self.value(value);
            }
            used |= match statement {
                Statement::If(node) => {
                    self.block(&mut node.then_block.lock())
                        | self.block(&mut node.else_block.lock())
                }
                Statement::While(node) => self.block(&mut node.block.lock()),
                Statement::Repeat(node) => self.block(&mut node.block.lock()),
                Statement::NumericFor(node) => self.block(&mut node.block.lock()),
                Statement::GenericFor(node) => self.block(&mut node.block.lock()),
                _ => false,
            };
        }
        used
    }
}
