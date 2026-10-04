//! Vector constants must not look up a mutable environment every time a
//! function runs. Source has no vector literal token, so capture the standard
//! runtime constructor once, before executing the recovered chunk. A chunk with
//! no local/register headroom left, or with a closure that has no room for one
//! more upvalue, calls `vector.create` inline instead, where Luau resolves that
//! path once at load time (the script never assigns `vector` and keeps its
//! environment): it then folds the call back into the constant.
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
    // A tree too large or deep to plan a binding for can still spell its
    // constants in place.
    let inventory = match crate::lower_conditionals::prepare_local_rewrite(body, |block| {
        block
            .iter()
            .flat_map(crate::deinline::stmt_rvalues)
            .any(contains_vector)
    }) {
        Ok(None) => return Ok(None),
        Ok(Some(inventory)) => Some(inventory),
        Err(_) => None,
    };
    let has_headroom = inventory.is_some()
        && crate::lower_conditionals::local_rewrite_frame_with_bound(body, &[], 0, register_bound)
            .headroom
            != 0
        && captures_fit(body);
    if !has_headroom && !constructor_fixed(body) {
        return Err("no local/register or upvalue headroom for the vector constructor binding");
    }
    let constructor = inventory.filter(|_| has_headroom).map(|mut inventory| {
        let name = crate::rehoist_constants::unique_name("createVector", &mut inventory.reserved);
        RcLocal::new(Local::new(Some(name)))
    });
    let mut context = Context {
        constructor,
        functions: FxHashMap::default(),
    };
    if !context.block(body) {
        return Ok(None);
    }
    let Some(constructor) = context.constructor else {
        return Ok(Some(crate::local_producers::Pass {
            pass: "materialize_vectors",
            rewrite_model: "inline_constructor_v1",
            introduced_locals: 0,
            ledger: Default::default(),
        }));
    };
    let mut declaration = Assign::new(
        vec![constructor.clone().into()],
        vec![vector_create()],
    );
    declaration.prefix = true;
    body.0.insert(0, declaration.into());
    let mut ledger = crate::local_producers::Ledger::default();
    ledger.record(&constructor, crate::local_producers::Role::VectorConstructor);
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

    /// 200 top-level locals (Luau's limit), then `return VECTOR`.
    fn full_frame(mut prefix: Vec<Statement>) -> Block {
        for i in 0..200 {
            let mut declaration = Assign::new(
                vec![RcLocal::new(Local::new(Some(format!("v{i}")))).into()],
                vec![Literal::Number(i as f64).into()],
            );
            declaration.prefix = true;
            prefix.push(declaration.into());
        }
        prefix.push(crate::Return::new(vec![Literal::Vector(0.1, -0.0, 1.0).into()]).into());
        Block(prefix)
    }

    #[test]
    fn without_headroom_a_load_time_constructor_is_called_in_place() {
        let mut body = full_frame(Vec::new());
        let report = materialize_vectors(&mut body).unwrap().unwrap();
        assert_eq!(report.introduced_locals, 0);
        let text = body.to_string();
        assert!(text.contains("return vector.create(0.1, -0, 1)"), "{text}");
        assert!(!text.contains("createVector"), "{text}");

        // The script replaces `vector`: no source spelling is exact.
        let assignment = Assign::new(vec![crate::LValue::Global(Global(b"vector".to_vec()))], vec![Literal::Nil.into()]);
        let mut body = full_frame(vec![assignment.into()]);
        assert!(materialize_vectors(&mut body).is_err());
    }
}

/// Luau's upvalue limit per function.
const MAX_UPVALUES: usize = 200;

/// Whether every closure the shared constructor binding would reach (one
/// using a vector constant, or enclosing one that does) can capture it
/// without exceeding [`MAX_UPVALUES`].
fn captures_fit(body: &Block) -> bool {
    fn visit(value: &RValue, functions: &mut FxHashMap<usize, bool>, fits: &mut bool) -> bool {
        match value {
            RValue::Literal(Literal::Vector(..) | Literal::VectorD(..)) => true,
            RValue::Closure(closure) => {
                let id = triomphe::Arc::as_ptr(&closure.function.0) as usize;
                let used = match functions.get(&id) {
                    Some(&used) => used,
                    None => {
                        functions.insert(id, false);
                        let used = block(&closure.function.lock().body, functions, fits);
                        functions.insert(id, used);
                        used
                    }
                };
                if used && closure.upvalues.len() >= MAX_UPVALUES {
                    *fits = false;
                }
                used
            }
            _ => {
                let mut used = false;
                value.visit_rvalues(&mut |child| {
                    used |= visit(child, functions, fits);
                    true
                });
                used
            }
        }
    }
    fn block(body: &Block, functions: &mut FxHashMap<usize, bool>, fits: &mut bool) -> bool {
        let mut used = false;
        for statement in &body.0 {
            crate::deinline::visit_stmt_rvalues(statement, &mut |rvalue| {
                used |= visit(rvalue, functions, fits);
                true
            });
            used |= match statement {
                Statement::If(node) => {
                    block(&node.then_block.lock(), functions, fits) | block(&node.else_block.lock(), functions, fits)
                }
                Statement::While(node) => block(&node.block.lock(), functions, fits),
                Statement::Repeat(node) => block(&node.block.lock(), functions, fits),
                Statement::NumericFor(node) => block(&node.block.lock(), functions, fits),
                Statement::GenericFor(node) => block(&node.block.lock(), functions, fits),
                _ => false,
            };
        }
        used
    }
    let mut fits = true;
    block(body, &mut FxHashMap::default(), &mut fits);
    fits
}

/// Whether `vector.create` read anywhere in the chunk is the library's: the
/// chunk assigns neither the global `vector` nor a member of it, and names
/// neither getfenv nor setfenv. Luau then resolves the path once at load
/// time. A work list, not recursion, so any depth is decided.
fn constructor_fixed(body: &Block) -> bool {
    fn rooted_at_vector(mut value: &RValue) -> bool {
        while let RValue::Index(index) = value {
            value = &index.left;
        }
        matches!(value, RValue::Global(global) if global.0 == b"vector")
    }
    let mut functions: Vec<triomphe::Arc<parking_lot::Mutex<crate::Function>>> = Vec::new();
    let mut seen = rustc_hash::FxHashSet::default();
    // Scans one function body; nested blocks are walked in place.
    let mut scan = |body: &Block, functions: &mut Vec<_>| -> bool {
        let mut blocks: Vec<triomphe::Arc<parking_lot::Mutex<Block>>> = Vec::new();
        let mut check = |statements: &[Statement], blocks: &mut Vec<_>, functions: &mut Vec<_>| -> bool {
            for statement in statements {
                if let Statement::Assign(assign) = statement
                    && assign.left.iter().any(|left| match left {
                        crate::LValue::Global(global) => global.0 == b"vector",
                        crate::LValue::Index(index) => rooted_at_vector(&index.left),
                        crate::LValue::Local(_) => false,
                    })
                {
                    return false;
                }
                let mut values: Vec<&RValue> = crate::deinline::stmt_rvalues(statement);
                while let Some(value) = values.pop() {
                    match value {
                        RValue::Global(global) if global.0 == b"getfenv" || global.0 == b"setfenv" => return false,
                        RValue::Closure(closure) => {
                            if seen.insert(triomphe::Arc::as_ptr(&closure.function.0) as usize) {
                                functions.push(closure.function.0.clone());
                            }
                        }
                        _ => values.extend(value.rvalues()),
                    }
                }
                match statement {
                    Statement::If(node) => blocks.extend([node.then_block.clone(), node.else_block.clone()]),
                    Statement::While(node) => blocks.push(node.block.clone()),
                    Statement::Repeat(node) => blocks.push(node.block.clone()),
                    Statement::NumericFor(node) => blocks.push(node.block.clone()),
                    Statement::GenericFor(node) => blocks.push(node.block.clone()),
                    _ => {}
                }
            }
            true
        };
        if !check(&body.0, &mut blocks, functions) {
            return false;
        }
        while let Some(block) = blocks.pop() {
            if !check(&block.lock().0, &mut blocks, functions) {
                return false;
            }
        }
        true
    };
    if !scan(body, &mut functions) {
        return false;
    }
    while let Some(function) = functions.pop() {
        if !scan(&function.lock().body, &mut functions) {
            return false;
        }
    }
    true
}

fn vector_create() -> RValue {
    Index::new(
        Global(b"vector".to_vec()).into(),
        Literal::String(b"create".to_vec()).into(),
    )
    .into()
}

struct Context {
    /// `None` when the chunk has no headroom for the shared binding.
    constructor: Option<RcLocal>,
    functions: FxHashMap<usize, bool>,
}

impl Context {
    fn value(&mut self, value: &mut RValue) -> bool {
        let components = match value {
            RValue::Literal(Literal::Vector(x, y, z)) => {
                // Keep the shortest round-tripping f32 spelling in source; a
                // NaN or infinity keeps its sign, which no spelling carries.
                Some([*x, *y, *z].map(|n| if n.is_finite() {
                    ryu::Buffer::new().format_finite(n).parse::<f64>().unwrap()
                } else {
                    f64::from(n)
                }))
            }
            RValue::Literal(Literal::VectorD(x, y, z)) => Some([*x, *y, *z]),
            _ => None,
        };
        if let Some(components) = components {
            *value = Call::new(
                self.constructor
                    .as_ref()
                    .map_or_else(vector_create, |local| local.clone().into()),
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
            if let (true, Some(constructor)) = (used, &self.constructor) {
                closure.upvalues.push(Upvalue::Copy(constructor.clone()));
            }
            return used;
        }
        let mut used = false;
        value.visit_rvalues_mut(&mut |child| {
            used |= self.value(child);
            true
        });
        used
    }

    fn block(&mut self, body: &mut Block) -> bool {
        let mut used = false;
        for statement in &mut body.0 {
            crate::deinline::visit_stmt_rvalues_mut(statement, &mut |value| {
                used |= self.value(value);
                true
            });
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
