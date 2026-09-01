//! Conservative register-pressure reduction for generated source-like ASTs.
//!
//! SSA destruction deliberately gives every value a distinct `RcLocal`.  A
//! large type-dispatch function can therefore contain more source locals than
//! Luau's 255-register limit even though the original bytecode reuses those
//! registers on mutually-exclusive branches.  This pass coalesces only
//! unnamed, non-captured locals whose conservative lexical live ranges do not
//! overlap.  Loop bindings and locals crossing a loop boundary remain
//! untouched.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{Block, LocalRw, RValue, RcLocal, Statement, Traverse};

#[derive(Clone)]
struct Occurrence {
    position: usize,
    branches: Vec<(usize, bool)>,
    read: bool,
    written: bool,
}

#[derive(Clone)]
struct LocalInfo {
    local: RcLocal,
    occurrences: Vec<Occurrence>,
    first: usize,
    last: usize,
    loop_scope: Vec<usize>,
    blocked: bool,
}

/// A representative and every local already assigned to that storage slot.
///
/// Checking only the representative's range is unsound: two later locals can
/// each be disjoint from the first value while overlapping one another.  The
/// latter pair would then be incorrectly assigned the same source variable.
struct CoalesceGroup {
    representative: LocalInfo,
    members: Vec<LocalInfo>,
}

impl LocalInfo {
    fn new(local: RcLocal, occurrence: Occurrence, loop_scope: &[usize], blocked: bool) -> Self {
        Self {
            local,
            first: occurrence.position,
            last: occurrence.position,
            occurrences: vec![occurrence],
            loop_scope: loop_scope.to_vec(),
            blocked,
        }
    }

    fn add(&mut self, occurrence: Occurrence, loop_scope: &[usize], blocked: bool) {
        self.first = self.first.min(occurrence.position);
        self.last = self.last.max(occurrence.position);
        if self.loop_scope != loop_scope {
            self.blocked = true;
        }
        self.blocked |= blocked;
        self.occurrences.push(occurrence);
    }
}

/// Coalesce generated locals when the function is above the register-pressure
/// threshold.  The pass is intentionally a no-op for functions containing a
/// closure: closure capture lifetime is represented separately from the
/// shallow `LocalRw` view and must not be guessed here.
pub fn coalesce_generated_locals(block: &mut Block, protected: &FxHashSet<RcLocal>) {
    let mut position = 0;
    let mut branch_id = 0;
    let mut loop_id = 0;
    let mut has_closure = false;
    let mut infos: FxHashMap<RcLocal, LocalInfo> = FxHashMap::default();
    collect_block(
        block,
        &mut position,
        &mut branch_id,
        &mut loop_id,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut has_closure,
        &mut infos,
        protected,
    );
    if has_closure || infos.len() <= 240 {
        return;
    }

    // Do not split branch-private identities here.  That transformation needs
    // block/region liveness and definite-assignment facts; a local may be live
    // into or out of a nested loop even when a shallow sibling walk cannot see
    // the use.  Keeping the conservative interval coalescer is preferable to
    // manufacturing fresh, uninitialised cells.  Large functions that still
    // exceed the register limit are left for the certified fallback.

    let mut values = infos.into_values().collect::<Vec<_>>();
    values.sort_by_key(|info| (info.first, info.last, info.local.stable_id()));
    let mut groups: Vec<CoalesceGroup> = Vec::new();
    let mut replacements = FxHashMap::default();
    for info in values {
        if info.blocked || !is_unnamed(&info.local) {
            continue;
        }
        let Some(group) = groups.iter_mut().find(|group| can_join_group(group, &info)) else {
            groups.push(CoalesceGroup {
                representative: info.clone(),
                members: vec![info],
            });
            continue;
        };
        replacements.insert(info.local.clone(), group.representative.local.clone());
        group.members.push(info);
    }
    if !replacements.is_empty() {
        crate::replace_locals::replace_locals(block, &replacements);
    }
}

fn can_join_group(group: &CoalesceGroup, info: &LocalInfo) -> bool {
    !group.representative.blocked
        && group.representative.loop_scope == info.loop_scope
        && group
            .members
            .iter()
            .all(|member| !ranges_interfere(member, info))
}

fn rvalue_contains_closure(value: &RValue) -> bool {
    matches!(value, RValue::Closure(_)) || value.rvalues().into_iter().any(rvalue_contains_closure)
}

/// `Statement::rvalues()` intentionally reports only RHS expressions.  An
/// indexed assignment can put an arbitrary expression (including a closure)
/// in its LHS index, so closure detection for this pass must traverse both
/// lvalues and rvalues.
fn statement_contains_closure(statement: &Statement) -> bool {
    let mut copy = statement.clone();
    let mut found = false;
    copy.traverse_rvalues(&mut |value| {
        found |= rvalue_contains_closure(value);
    });
    found
}

fn is_unnamed(local: &RcLocal) -> bool {
    local.0.0.lock().0.is_none()
}

fn paths_are_exclusive(left: &[(usize, bool)], right: &[(usize, bool)]) -> bool {
    left.iter().any(|(id, branch)| {
        right
            .iter()
            .any(|(other_id, other_branch)| id == other_id && branch != other_branch)
    })
}

fn ranges_interfere(left: &LocalInfo, right: &LocalInfo) -> bool {
    if left.last < right.first || right.last < left.first {
        return false;
    }
    // If every pair of occurrences is on opposite arms of at least one
    // conditional, the values can never be live at the same time.
    left.occurrences.iter().any(|left_occurrence| {
        right.occurrences.iter().any(|right_occurrence| {
            !paths_are_exclusive(&left_occurrence.branches, &right_occurrence.branches)
                && !(left_occurrence.position < right_occurrence.position
                    && left.last < right_occurrence.position)
                && !(right_occurrence.position < left_occurrence.position
                    && right.last < left_occurrence.position)
        })
    })
}

fn record_statement(
    statement: &Statement,
    position: usize,
    branches: &[(usize, bool)],
    loop_scope: &[usize],
    blocked: bool,
    infos: &mut FxHashMap<RcLocal, LocalInfo>,
    protected: &FxHashSet<RcLocal>,
) {
    let reads = statement
        .values_read()
        .into_iter()
        .cloned()
        .collect::<FxHashSet<_>>();
    let writes = statement
        .values_written()
        .into_iter()
        .cloned()
        .collect::<FxHashSet<_>>();
    for local in reads.union(&writes) {
        let occurrence = Occurrence {
            position,
            branches: branches.to_vec(),
            read: reads.contains(local),
            written: writes.contains(local),
        };
        let local_blocked = blocked
            || protected.contains(local)
            || (occurrence.read && occurrence.written && !loop_scope.is_empty());
        if let Some(info) = infos.get_mut(local) {
            info.add(occurrence, loop_scope, local_blocked);
        } else {
            infos.insert(
                local.clone(),
                LocalInfo::new(local.clone(), occurrence, loop_scope, local_blocked),
            );
        }
    }
}

fn collect_block(
    block: &mut Block,
    position: &mut usize,
    branch_id: &mut usize,
    loop_id: &mut usize,
    branches: &mut Vec<(usize, bool)>,
    loop_scope: &mut Vec<usize>,
    has_closure: &mut bool,
    infos: &mut FxHashMap<RcLocal, LocalInfo>,
    protected: &FxHashSet<RcLocal>,
) {
    for statement in block.iter_mut() {
        let current_position = *position;
        *position += 1;
        if statement_contains_closure(statement) {
            *has_closure = true;
        }
        record_statement(
            statement,
            current_position,
            branches,
            loop_scope,
            false,
            infos,
            protected,
        );
        match statement {
            Statement::If(if_statement) => {
                let id = *branch_id;
                *branch_id += 1;
                branches.push((id, true));
                collect_block(
                    &mut if_statement.then_block.lock(),
                    position,
                    branch_id,
                    loop_id,
                    branches,
                    loop_scope,
                    has_closure,
                    infos,
                    protected,
                );
                branches.pop();
                branches.push((id, false));
                collect_block(
                    &mut if_statement.else_block.lock(),
                    position,
                    branch_id,
                    loop_id,
                    branches,
                    loop_scope,
                    has_closure,
                    infos,
                    protected,
                );
                branches.pop();
            }
            Statement::While(while_statement) => {
                let id = *loop_id;
                *loop_id += 1;
                loop_scope.push(id);
                collect_block(
                    &mut while_statement.block.lock(),
                    position,
                    branch_id,
                    loop_id,
                    branches,
                    loop_scope,
                    has_closure,
                    infos,
                    protected,
                );
                loop_scope.pop();
            }
            Statement::Repeat(repeat_statement) => {
                let id = *loop_id;
                *loop_id += 1;
                loop_scope.push(id);
                collect_block(
                    &mut repeat_statement.block.lock(),
                    position,
                    branch_id,
                    loop_id,
                    branches,
                    loop_scope,
                    has_closure,
                    infos,
                    protected,
                );
                loop_scope.pop();
            }
            Statement::NumericFor(for_loop) => {
                let id = *loop_id;
                *loop_id += 1;
                mark_blocked(&for_loop.counter, infos);
                loop_scope.push(id);
                collect_block(
                    &mut for_loop.block.lock(),
                    position,
                    branch_id,
                    loop_id,
                    branches,
                    loop_scope,
                    has_closure,
                    infos,
                    protected,
                );
                loop_scope.pop();
            }
            Statement::GenericFor(for_loop) => {
                let id = *loop_id;
                *loop_id += 1;
                for result in &for_loop.res_locals {
                    mark_blocked(result, infos);
                }
                loop_scope.push(id);
                collect_block(
                    &mut for_loop.block.lock(),
                    position,
                    branch_id,
                    loop_id,
                    branches,
                    loop_scope,
                    has_closure,
                    infos,
                    protected,
                );
                loop_scope.pop();
            }
            _ => {}
        }
    }
}

fn mark_blocked(local: &RcLocal, infos: &mut FxHashMap<RcLocal, LocalInfo>) {
    if let Some(info) = infos.get_mut(local) {
        info.blocked = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Assign, Call, Closure, Function, GenericFor, Global, If, Index, LValue, Literal,
        NumericFor, RValue, Upvalue, While,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn synthetic_info(first: usize, last: usize) -> LocalInfo {
        let local = RcLocal::default();
        let occurrence = Occurrence {
            position: first,
            branches: Vec::new(),
            read: true,
            written: false,
        };
        let mut info = LocalInfo::new(local, occurrence, &[], false);
        if last > first {
            info.add(
                Occurrence {
                    position: last,
                    branches: Vec::new(),
                    read: true,
                    written: false,
                },
                &[],
                false,
            );
        }
        info
    }

    #[test]
    fn coalesce_group_rejects_overlap_between_nonrepresentatives() {
        // The first value is disjoint from both later values, but the later
        // values overlap one another.  A representative-only check would
        // incorrectly put all three in one source local.
        let representative = synthetic_info(0, 0);
        let first_later = synthetic_info(2, 10);
        let overlapping_later = synthetic_info(5, 6);
        let group = CoalesceGroup {
            representative: representative.clone(),
            members: vec![representative, first_later.clone()],
        };

        let representative_only = CoalesceGroup {
            representative: synthetic_info(0, 0),
            members: vec![synthetic_info(0, 0)],
        };
        assert!(can_join_group(&representative_only, &first_later));
        assert!(can_join_group(&representative_only, &overlapping_later));
        assert!(!can_join_group(&group, &overlapping_later));
    }

    #[test]
    fn coalescer_keeps_overlapping_values_distinct() {
        let representative = RcLocal::default();
        let first_later = RcLocal::default();
        let overlapping_later = RcLocal::default();
        let mut statements = vec![
            Assign::new(
                vec![LValue::Local(representative.clone())],
                vec![RValue::Literal(Literal::Number(0.0))],
            )
            .into(),
            Assign::new(
                vec![LValue::Local(first_later.clone())],
                vec![RValue::Literal(Literal::Number(1.0))],
            )
            .into(),
            Assign::new(
                vec![LValue::Local(overlapping_later.clone())],
                vec![RValue::Literal(Literal::Number(2.0))],
            )
            .into(),
            Call::new(
                RValue::Global(Global::from("sink")),
                vec![RValue::Local(first_later.clone())],
            )
            .into(),
            Call::new(
                RValue::Global(Global::from("sink")),
                vec![RValue::Local(overlapping_later.clone())],
            )
            .into(),
        ];
        // Trigger the pressure pass without changing the three-value shape.
        for _ in 0..240 {
            statements.push(
                Assign::new(
                    vec![LValue::Local(RcLocal::default())],
                    vec![RValue::Literal(Literal::Number(3.0))],
                )
                .into(),
            );
        }
        let mut block = Block(statements);
        coalesce_generated_locals(&mut block, &FxHashSet::default());

        let first_after = block
            .0
            .iter()
            .find_map(|statement| match statement {
                Statement::Assign(assign)
                    if assign.right.iter().any(
                        |value| matches!(value, RValue::Literal(Literal::Number(n)) if *n == 1.0),
                    ) =>
                {
                    assign.left[0].as_local().cloned()
                }
                _ => None,
            })
            .expect("first value assignment");
        let overlap_after = block
            .0
            .iter()
            .find_map(|statement| match statement {
                Statement::Assign(assign)
                    if assign.right.iter().any(
                        |value| matches!(value, RValue::Literal(Literal::Number(n)) if *n == 2.0),
                    ) =>
                {
                    assign.left[0].as_local().cloned()
                }
                _ => None,
            })
            .expect("overlapping value assignment");
        assert_ne!(first_after, overlap_after);
    }

    #[test]
    fn closure_in_indexed_lhs_disables_pressure_rewrite() {
        let captured = RcLocal::default();
        let value = RcLocal::default();
        let closure = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(captured)],
        });
        let indexed_store = Assign::new(
            vec![LValue::Index(Index::new(
                RValue::Global(Global::from("targets")),
                closure,
            ))],
            vec![RValue::Literal(Literal::Number(1.0))],
        );
        let mut statements = vec![
            Assign::new(
                vec![LValue::Local(value.clone())],
                vec![RValue::Literal(Literal::Number(0.0))],
            )
            .into(),
            indexed_store.into(),
        ];
        let pressure_local = RcLocal::default();
        statements.push(
            Assign::new(
                vec![LValue::Local(pressure_local.clone())],
                vec![RValue::Literal(Literal::Number(2.0))],
            )
            .into(),
        );
        for _ in 0..240 {
            statements.push(
                Assign::new(
                    vec![LValue::Local(RcLocal::default())],
                    vec![RValue::Literal(Literal::Number(2.0))],
                )
                .into(),
            );
        }
        let mut block = Block(statements);
        coalesce_generated_locals(&mut block, &FxHashSet::default());

        let first = match &block.0[0] {
            Statement::Assign(assign) => assign.left[0].as_local().cloned(),
            _ => None,
        };
        assert_eq!(first, Some(value));
        let pressure = match &block.0[2] {
            Statement::Assign(assign) => assign.left[0].as_local().cloned(),
            _ => None,
        };
        assert_eq!(pressure, Some(pressure_local));
    }

    #[test]
    fn keeps_local_live_across_nested_loop_branch() {
        let value = RcLocal::default();
        let counter = RcLocal::default();
        let branch = If::new(
            RValue::Global(Global::from("flag")),
            Block::from(vec![
                Assign::new(
                    vec![LValue::Local(value.clone())],
                    vec![RValue::Literal(Literal::Number(1.0))],
                )
                .into(),
            ]),
            Block::from(vec![
                Assign::new(
                    vec![LValue::Local(value.clone())],
                    vec![RValue::Literal(Literal::Number(2.0))],
                )
                .into(),
            ]),
        );
        let loop_statement = NumericFor::new(
            RValue::Literal(Literal::Number(1.0)),
            // A zero-trip loop must not be treated as a definite write before
            // the value is consumed after the enclosing branch.
            RValue::Literal(Literal::Number(0.0)),
            RValue::Literal(Literal::Number(1.0)),
            counter,
            Block::from(vec![branch.into()]),
        );
        let mut statements = vec![
            Assign::new(
                vec![LValue::Local(value.clone())],
                vec![RValue::Literal(Literal::Number(0.0))],
            )
            .into(),
            loop_statement.into(),
            Call::new(
                RValue::Global(Global::from("sink")),
                vec![RValue::Local(value.clone())],
            )
            .into(),
        ];
        for _ in 0..241 {
            statements.push(
                Assign::new(
                    vec![LValue::Local(RcLocal::default())],
                    vec![RValue::Literal(Literal::Number(3.0))],
                )
                .into(),
            );
        }
        let mut block = Block(statements);
        coalesce_generated_locals(&mut block, &FxHashSet::default());

        let Statement::NumericFor(numeric) = &block.0[1] else {
            panic!("expected numeric loop");
        };
        let Statement::If(if_statement) = &numeric.block.lock().0[0] else {
            panic!("expected branch");
        };
        for arm in [&if_statement.then_block, &if_statement.else_block] {
            let Statement::Assign(assign) = &arm.lock().0[0] else {
                panic!("expected arm assignment");
            };
            assert_eq!(assign.left[0].as_local(), Some(&value));
        }
    }

    #[test]
    fn keeps_local_live_across_zero_trip_generic_loop_branch() {
        let value = RcLocal::default();
        let make_arm = |number| {
            Block::from(vec![
                GenericFor::new(
                    Vec::new(),
                    vec![RValue::Global(Global::from("empty_iterator"))],
                    Block::from(vec![
                        Assign::new(
                            vec![LValue::Local(value.clone())],
                            vec![RValue::Literal(Literal::Number(number))],
                        )
                        .into(),
                    ]),
                )
                .into(),
                Call::new(
                    RValue::Global(Global::from("sink")),
                    vec![RValue::Local(value.clone())],
                )
                .into(),
            ])
        };
        let mut block = Block::from(vec![
            If::new(
                RValue::Global(Global::from("flag")),
                make_arm(1.0),
                make_arm(2.0),
            )
            .into(),
        ]);
        for _ in 0..241 {
            block.0.push(
                Assign::new(
                    vec![LValue::Local(RcLocal::default())],
                    vec![RValue::Literal(Literal::Number(4.0))],
                )
                .into(),
            );
        }

        coalesce_generated_locals(&mut block, &FxHashSet::default());

        let Statement::If(if_statement) = &block.0[0] else {
            panic!("expected branch");
        };
        for arm in [&if_statement.then_block, &if_statement.else_block] {
            let arm = arm.lock();
            let Statement::GenericFor(loop_node) = &arm.0[0] else {
                panic!("expected generic loop");
            };
            let body = loop_node.block.lock();
            let Statement::Assign(assign) = &body.0[0] else {
                panic!("expected loop write");
            };
            assert_eq!(assign.left[0].as_local(), Some(&value));
            let call = arm.0.last().expect("expected arm read");
            assert!(call.values_read().into_iter().any(|local| local == &value));
        }
    }

    #[test]
    fn keeps_local_live_across_zero_trip_while_branch() {
        let value = RcLocal::default();
        let make_arm = |number| {
            Block::from(vec![
                While::new(
                    RValue::Global(Global::from("never")),
                    Block::from(vec![
                        Assign::new(
                            vec![LValue::Local(value.clone())],
                            vec![RValue::Literal(Literal::Number(number))],
                        )
                        .into(),
                    ]),
                )
                .into(),
                Call::new(
                    RValue::Global(Global::from("sink")),
                    vec![RValue::Local(value.clone())],
                )
                .into(),
            ])
        };
        let mut block = Block::from(vec![
            If::new(
                RValue::Global(Global::from("flag")),
                make_arm(1.0),
                make_arm(2.0),
            )
            .into(),
        ]);
        for _ in 0..241 {
            block.0.push(
                Assign::new(
                    vec![LValue::Local(RcLocal::default())],
                    vec![RValue::Literal(Literal::Number(5.0))],
                )
                .into(),
            );
        }

        coalesce_generated_locals(&mut block, &FxHashSet::default());

        let Statement::If(if_statement) = &block.0[0] else {
            panic!("expected branch");
        };
        for arm in [&if_statement.then_block, &if_statement.else_block] {
            let arm = arm.lock();
            let Statement::While(loop_node) = &arm.0[0] else {
                panic!("expected while loop");
            };
            let body = loop_node.block.lock();
            let Statement::Assign(assign) = &body.0[0] else {
                panic!("expected loop write");
            };
            assert_eq!(assign.left[0].as_local(), Some(&value));
            let call = arm.0.last().expect("expected arm read");
            assert!(call.values_read().into_iter().any(|local| local == &value));
        }
    }
}
