//! Link closure inputs in lexical order. Constructor captures are rewritten
//! before entering their function, so each incoming local maps directly to its
//! final cell rather than requiring a rewrite of every descendant afterwards.

use by_address::ByAddress;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use triomphe::Arc;

use crate::{Block, Function, LocalRw, RValue, RcLocal, Statement, Traverse, Upvalue};

type Inputs = FxHashMap<ByAddress<Arc<Mutex<Function>>>, Vec<RcLocal>>;

pub fn link_upvalues(body: &mut Block, upvalues: &Inputs) {
    link_block(body, upvalues, &FxHashMap::default());
}

fn link_block(body: &mut Block, upvalues: &Inputs, locals: &FxHashMap<RcLocal, RcLocal>) {
    for statement in &mut body.0 {
        if !locals.is_empty() {
            statement.visit_local_reads_mut(&mut |local| {
                replace(local, locals);
                true
            });
            statement.visit_local_writes_mut(&mut |local| {
                replace(local, locals);
                true
            });
        }
        statement.traverse_rvalues(&mut |value| {
            if let RValue::Closure(closure) = value {
                let inputs = &upvalues[&closure.function];
                let locals = inputs.iter().zip(&closure.upvalues).map(|(old, capture)| {
                    let (Upvalue::Copy(new) | Upvalue::Ref(new)) = capture;
                    (old.clone(), new.clone())
                }).collect();
                link_block(&mut closure.function.lock().body, upvalues, &locals);
            }
        });
        visit_blocks(statement, |block| link_block(block, upvalues, locals));
    }
}

fn replace(local: &mut RcLocal, locals: &FxHashMap<RcLocal, RcLocal>) {
    if let Some(new) = locals.get(local) {
        new.inherit_source_bindings(local);
        *local = new.clone();
    }
}

fn visit_blocks(statement: &mut Statement, mut visit: impl FnMut(&mut Block)) {
    match statement {
        Statement::If(value) => {
            visit(&mut value.then_block.lock());
            visit(&mut value.else_block.lock());
        }
        Statement::While(value) => visit(&mut value.block.lock()),
        Statement::Repeat(value) => visit(&mut value.block.lock()),
        Statement::NumericFor(value) => visit(&mut value.block.lock()),
        Statement::GenericFor(value) => visit(&mut value.block.lock()),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BindingOrigin, Closure, If, Literal, Local, Return, SourceBinding};

    // Exact old bottom-up scheduling, including propagation through descendant
    // closures and merging diagnostic evidence on every replaced occurrence.
    fn reference(body: &mut Block, upvalues: &Inputs) {
        for statement in &mut body.0 {
            statement.traverse_rvalues(&mut |value| {
                if let RValue::Closure(closure) = value {
                    let locals = upvalues[&closure.function].iter().zip(&closure.upvalues)
                        .map(|(old, capture)| {
                            let (Upvalue::Copy(new) | Upvalue::Ref(new)) = capture;
                            (old.clone(), new.clone())
                        }).collect::<FxHashMap<_, _>>();
                    let mut function = closure.function.lock();
                    reference(&mut function.body, upvalues);
                    crate::replace_locals::replace_locals(&mut function.body, &locals);
                }
            });
            visit_blocks(statement, |block| reference(block, upvalues));
        }
    }

    fn fixture(depth: usize, captures: bool) -> (Block, Inputs) {
        fn descend(level: usize, depth: usize, capture: &RcLocal, inputs: &mut Inputs, captures: bool) -> RValue {
            let incoming = RcLocal::default();
            incoming.0.lock().add_source_binding(SourceBinding {
                origin: BindingOrigin::DebugUpvalue { prototype: level, slot: 0 },
                name: format!("capture{level}"),
            });
            incoming.record_definition_lineage();
            for offset in 0..8 {
                incoming.0.lock().3.as_mut().unwrap().add(10_000 + level as u64 * 8 + offset);
            }
            incoming.0.lock().4.separate_from_parameter = level % 2 == 1;
            let mut values = vec![incoming.clone().into()];
            if level < depth {
                values.push(descend(level + 1, depth, &incoming, inputs, captures));
            }
            let body = vec![If::new(Literal::Boolean(true).into(),
                vec![Return::new(values).into()].into(), Block::default()).into()].into();
            let function = ByAddress(Arc::new(Mutex::new(Function { body, ..Default::default() })));
            inputs.insert(function.clone(), if captures { vec![incoming] } else { vec![] });
            Closure { node_origin: Default::default(), function,
                upvalues: if !captures { vec![] } else if level % 2 == 0 {
                    vec![Upvalue::Copy(capture.clone())]
                } else { vec![Upvalue::Ref(capture.clone())] } }.into()
        }
        crate::reset_local_ids();
        let cell = RcLocal::default();
        cell.0.lock().0 = Some("cell".into());
        cell.0.lock().4.parameter = true;
        cell.record_definition_lineage();
        let mut inputs = Inputs::default();
        let closure = descend(0, depth, &cell, &mut inputs, captures);
        (vec![Return::new(vec![cell.into(), closure.clone(), closure]).into()].into(), inputs)
    }

    fn snapshot(body: &mut Block) -> Vec<(u64, Local, usize)> {
        let mut result = Vec::new();
        for statement in &mut body.0 {
            for local in statement.values_read().into_iter().chain(statement.values_written()) {
                result.push((local.stable_id(), local.0.lock().clone(), Arc::count(&local.0.0)));
            }
            statement.traverse_rvalues(&mut |value| {
                if let RValue::Closure(closure) = value {
                    result.extend(snapshot(&mut closure.function.lock().body));
                }
            });
            visit_blocks(statement, |block| result.extend(snapshot(block)));
        }
        result
    }

    #[test]
    fn top_down_linking_matches_bottom_up_cells_evidence_owners_and_format() {
        for captures in [false, true] {
            for depth in [0, 1, 3, 20, 40] {
                let (mut old, old_inputs) = fixture(depth, captures);
                reference(&mut old, &old_inputs);
                let expected = snapshot(&mut old);
                let expected_text = old.to_string();
                let (mut new, new_inputs) = fixture(depth, captures);
                link_upvalues(&mut new, &new_inputs);
                assert_eq!(snapshot(&mut new), expected, "depth={depth} captures={captures}");
                assert_eq!(new.to_string(), expected_text);
            }
        }
    }
}
