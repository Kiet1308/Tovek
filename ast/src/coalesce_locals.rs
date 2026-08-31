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

    // A single SSA identity can be reused by the bytecode on several
    // mutually-exclusive arms.  In that shape the ordinary interval pass
    // above cannot help: every arm occurrence belongs to the same local, so
    // its range spans the whole switch and LocalDeclarer hoists one large
    // declaration.  Split only branch-private identities first, then recollect
    // ranges so the newly-created cells can be coalesced with one another.
    split_branch_private_locals(block, protected);

    infos.clear();
    position = 0;
    branch_id = 0;
    loop_id = 0;
    has_closure = false;
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
    if has_closure {
        return;
    }

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

#[derive(Default)]
struct BranchUsage {
    refs: FxHashSet<RcLocal>,
    captured: FxHashSet<RcLocal>,
}

/// Clone a local into an `if` arm only when each arm defines it before any
/// read and the surrounding block never observes the original identity.  The
/// proof is deliberately syntactic and fail-closed; a local declared by a
/// loop/header or a closure capture is never split.
fn split_branch_private_locals(block: &mut Block, protected: &FxHashSet<RcLocal>) {
    let inherited_scope = FxHashSet::default();
    split_branch_block(block, protected, &inherited_scope);
}

fn split_branch_block(
    block: &mut Block,
    protected: &FxHashSet<RcLocal>,
    inherited_scope: &FxHashSet<RcLocal>,
) {
    let mut scope = inherited_scope.clone();
    scope.extend(scope_declarations(block));

    for index in 0..block.0.len() {
        let outside_usage = usage_outside(block, index);
        let Statement::If(if_statement) = &block.0[index] else {
            continue;
        };
        let then_block = if_statement.then_block.clone();
        let else_block = if_statement.else_block.clone();
        let condition = usage_of_condition(&if_statement.condition);
        let then_usage = usage_of_block(&then_block.lock());
        let else_usage = usage_of_block(&else_block.lock());
        let candidates = then_usage
            .refs
            .intersection(&else_usage.refs)
            .cloned()
            .collect::<Vec<_>>();
        let mut then_map = FxHashMap::default();
        let mut else_map = FxHashMap::default();
        for local in candidates {
            if protected.contains(&local)
                || scope.contains(&local)
                || condition.refs.contains(&local)
                || outside_usage.refs.contains(&local)
                || then_usage.captured.contains(&local)
                || else_usage.captured.contains(&local)
                || !first_is_write(&then_block.lock(), &local)
                || !first_is_write(&else_block.lock(), &local)
            {
                continue;
            }
            then_map.insert(local.clone(), RcLocal::default());
            else_map.insert(local, RcLocal::default());
        }
        if !then_map.is_empty() {
            crate::replace_locals::replace_locals(&mut then_block.lock(), &then_map);
            crate::replace_locals::replace_locals(&mut else_block.lock(), &else_map);
        }

        let Statement::If(if_statement) = &mut block.0[index] else {
            unreachable!();
        };
        split_branch_block(&mut if_statement.then_block.lock(), protected, &scope);
        split_branch_block(&mut if_statement.else_block.lock(), protected, &scope);
    }

    // Visit loops/closures that are direct children of this block.  `If` arms
    // were already visited above so they are skipped here.
    for statement in &mut block.0 {
        match statement {
            Statement::If(_) => {}
            _ => split_branch_nested_statement(statement, protected, &scope),
        }
    }
}

fn split_branch_nested_statement(
    statement: &mut Statement,
    protected: &FxHashSet<RcLocal>,
    scope: &FxHashSet<RcLocal>,
) {
    match statement {
        Statement::While(node) => split_branch_block(&mut node.block.lock(), protected, scope),
        Statement::Repeat(node) => split_branch_block(&mut node.block.lock(), protected, scope),
        Statement::NumericFor(node) => {
            let mut child_scope = scope.clone();
            child_scope.insert(node.counter.clone());
            split_branch_block(&mut node.block.lock(), protected, &child_scope);
        }
        Statement::GenericFor(node) => {
            let mut child_scope = scope.clone();
            child_scope.extend(node.res_locals.iter().cloned());
            split_branch_block(&mut node.block.lock(), protected, &child_scope);
        }
        _ => {}
    }
}

fn scope_declarations(block: &Block) -> FxHashSet<RcLocal> {
    let mut declarations = FxHashSet::default();
    for statement in &block.0 {
        match statement {
            Statement::Assign(assign) if assign.prefix => {
                declarations.extend(
                    assign
                        .left
                        .iter()
                        .filter_map(|left| left.as_local().cloned()),
                );
            }
            Statement::NumericFor(node) => {
                declarations.insert(node.counter.clone());
            }
            Statement::GenericFor(node) => declarations.extend(node.res_locals.iter().cloned()),
            _ => {}
        }
    }
    declarations
}

fn usage_outside(block: &Block, skip: usize) -> BranchUsage {
    let mut usage = BranchUsage::default();
    for (index, statement) in block.0.iter().enumerate() {
        if index != skip {
            collect_usage(statement, &mut usage);
        }
    }
    usage
}

fn usage_of_block(block: &Block) -> BranchUsage {
    let mut usage = BranchUsage::default();
    for statement in &block.0 {
        collect_usage(statement, &mut usage);
    }
    usage
}

fn usage_of_condition(condition: &RValue) -> BranchUsage {
    let mut usage = BranchUsage::default();
    usage
        .refs
        .extend(condition.values_read().into_iter().cloned());
    collect_rvalue_captures(condition, &mut usage.captured);
    usage
}

fn collect_usage(statement: &Statement, usage: &mut BranchUsage) {
    usage
        .refs
        .extend(statement.values_read().into_iter().cloned());
    usage
        .refs
        .extend(statement.values_written().into_iter().cloned());
    for value in statement.rvalues() {
        collect_rvalue_captures(value, &mut usage.captured);
    }
    match statement {
        Statement::If(node) => {
            for child in node.then_block.lock().iter() {
                collect_usage(child, usage);
            }
            for child in node.else_block.lock().iter() {
                collect_usage(child, usage);
            }
        }
        Statement::While(node) => {
            for child in node.block.lock().iter() {
                collect_usage(child, usage);
            }
        }
        Statement::Repeat(node) => {
            for child in node.block.lock().iter() {
                collect_usage(child, usage);
            }
        }
        Statement::NumericFor(node) => {
            for child in node.block.lock().iter() {
                collect_usage(child, usage);
            }
        }
        Statement::GenericFor(node) => {
            for child in node.block.lock().iter() {
                collect_usage(child, usage);
            }
        }
        _ => {}
    }
}

fn collect_rvalue_captures(value: &RValue, captured: &mut FxHashSet<RcLocal>) {
    if let RValue::Closure(closure) = value {
        captured.extend(closure.upvalues.iter().map(|upvalue| match upvalue {
            crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local) => local.clone(),
        }));
    }
    for nested in value.rvalues() {
        collect_rvalue_captures(nested, captured);
    }
}

fn rvalue_contains_closure(value: &RValue) -> bool {
    matches!(value, RValue::Closure(_)) || value.rvalues().into_iter().any(rvalue_contains_closure)
}

fn first_is_write(block: &Block, local: &RcLocal) -> bool {
    for statement in &block.0 {
        if statement
            .values_read()
            .into_iter()
            .any(|read| read == local)
        {
            return false;
        }
        if statement
            .values_written()
            .into_iter()
            .any(|written| written == local)
        {
            return true;
        }
        match statement {
            Statement::If(node) => {
                // A nested conditional cannot prove a definition on every
                // path without a full definite-assignment analysis.
                return first_is_write(&node.then_block.lock(), local)
                    && first_is_write(&node.else_block.lock(), local);
            }
            Statement::While(node) => return first_is_write(&node.block.lock(), local),
            Statement::Repeat(node) => return first_is_write(&node.block.lock(), local),
            Statement::NumericFor(node) => return first_is_write(&node.block.lock(), local),
            Statement::GenericFor(node) => return first_is_write(&node.block.lock(), local),
            _ => {}
        }
    }
    false
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
        if statement
            .rvalues()
            .into_iter()
            .any(|value| rvalue_contains_closure(value))
        {
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
    use crate::{Assign, Call, Global, LValue, Literal, RValue};

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
}
