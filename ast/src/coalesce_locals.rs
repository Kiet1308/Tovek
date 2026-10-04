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
    /// Written by a table constructor (`t = {...}`).
    builds_table: bool,
    /// Read only as the table a store writes into (`t.k = v`).
    stores_into: bool,
}

#[derive(Clone)]
struct LocalInfo {
    local: RcLocal,
    occurrences: Vec<Occurrence>,
    first: usize,
    last: usize,
    loop_scope: Vec<usize>,
    branch_scope: Vec<(usize, bool)>,
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
            branch_scope: occurrence.branches.clone(),
            occurrences: vec![occurrence],
            loop_scope: loop_scope.to_vec(),
            blocked,
        }
    }

    fn add(&mut self, occurrence: Occurrence, loop_scope: &[usize], blocked: bool) {
        self.first = self.first.min(occurrence.position);
        self.last = self.last.max(occurrence.position);
        let common = self
            .branch_scope
            .iter()
            .zip(&occurrence.branches)
            .take_while(|(left, right)| left == right)
            .count();
        self.branch_scope.truncate(common);
        if self.loop_scope != loop_scope {
            self.blocked = true;
            let common = self
                .loop_scope
                .iter()
                .zip(loop_scope)
                .take_while(|(left, right)| left == right)
                .count();
            self.loop_scope.truncate(common);
        }
        self.blocked |= blocked;
        self.occurrences.push(occurrence);
    }
}

/// Coalesce generated locals above Luau's 200-local source limit. Captured
/// cells keep their identities for the full closure lifetime; unrelated local
/// values may still reuse storage after their last lexical occurrence.
pub fn coalesce_generated_locals(block: &mut Block, protected: &FxHashSet<RcLocal>) {
    coalesce_generated_locals_in_function(block, protected, &[], &[], Sharing::Eager);
}

/// The full occurrence collector is needed only above the source binding
/// limit. Count exactly its local identities first, without retaining local
/// owners, path vectors, capture sets or per-statement read/write sets.
struct PressureCensus {
    seen: FxHashSet<u64>,
    owned: usize,
    available: usize,
    statements: u64,
    operands: u64,
}

impl PressureCensus {
    fn local(&mut self, local: &RcLocal) -> bool {
        self.operands += 1;
        self.owned += usize::from(self.seen.insert(local.stable_id()));
        self.owned <= self.available
    }

    fn block(&mut self, block: &Block) -> bool {
        for statement in block.iter() {
            self.statements += 1;
            if !statement.visit_local_reads(&mut |local| self.local(local))
                || !statement.visit_local_writes(&mut |local| self.local(local))
            {
                return false;
            }
            // Match collect_block's frame boundary exactly: structured
            // blocks belong to this frame, closure bodies do not. Closure
            // capture operands (including indexed LHSs) were read above.
            let complete = match statement {
                Statement::If(branch) => {
                    // The two arms may deliberately share one Arc. Release
                    // its guard before acquiring the next arm's lock.
                    let then_complete = {
                        let block = branch.then_block.lock();
                        self.block(&block)
                    };
                    then_complete && {
                        let block = branch.else_block.lock();
                        self.block(&block)
                    }
                }
                Statement::While(node) => self.block(&node.block.lock()),
                Statement::Repeat(node) => self.block(&node.block.lock()),
                Statement::NumericFor(node) => self.block(&node.block.lock()),
                Statement::GenericFor(node) => self.block(&node.block.lock()),
                _ => true,
            };
            if !complete { return false; }
        }
        true
    }
}

fn pressure_within_limit(block: &Block, parameters: &[RcLocal], upvalues: &[RcLocal], limit: usize) -> bool {
    let Some(available) = limit.checked_sub(parameters.len()) else { return false; };
    let mut census = PressureCensus {
        // Preseeding excludes both incoming upvalues and parameter identities
        // from owned bindings; parameters.len still counts unused/duplicate
        // parameter slots exactly as the established pressure rule does.
        seen: parameters.iter().chain(upvalues).map(RcLocal::stable_id).collect(),
        owned: 0,
        available,
        statements: 0,
        operands: 0,
    };
    let within_limit = census.block(block);
    crate::telemetry::count("coalesce_pressure_census_statements", census.statements);
    crate::telemetry::count("coalesce_pressure_census_operands", census.operands);
    within_limit
}

#[cfg(test)]
thread_local! {
    static REFERENCE_PRESSURE_COLLECTOR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Locals a Luau function may declare at once.
pub const LOCAL_LIMIT: usize = 200;

/// How freely locals share storage in a function over [`LOCAL_LIMIT`]. A
/// decompile starts `Deferred` and retries with the next level while the
/// finished output still declares too many ([`declared_locals_exceed_limit`])
/// or needs too many registers
/// ([`crate::register_pressure::registers_exceed_limit`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sharing {
    /// Temporaries later passes fold into their use stay apart.
    #[default]
    Deferred,
    /// Every unnamed temporary, and source locals of one name (`do local a
    /// = ... end` repeated past the limit becomes one reassigned `a`).
    Eager,
    /// Source locals of any names too; a slot holding several names takes
    /// none of them (naming infers one), which no source spelled.
    AnyName,
}

impl Sharing {
    /// The next, freer level, if any.
    pub fn next(self) -> Option<Self> {
        match self {
            Sharing::Deferred => Some(Sharing::Eager),
            Sharing::Eager => Some(Sharing::AnyName),
            Sharing::AnyName => None,
        }
    }
}

/// Explicit frame ownership keeps unused parameters in the root pressure count
/// and excludes incoming upvalues, which do not consume local binding slots.
/// `sharing`: which locals may share storage ([`Sharing`]); short of
/// `Eager`, temporaries later passes fold into their use
/// ([`folds_into_its_use`]) stay apart, and the caller checks the finished
/// function ([`declared_locals_exceed_limit`]).
pub fn coalesce_generated_locals_in_function(
    block: &mut Block,
    protected: &FxHashSet<RcLocal>,
    parameters: &[RcLocal],
    upvalues: &[RcLocal],
    sharing: Sharing,
) {
    let _phase = crate::telemetry::Span::new("GENERATED_LOCAL_COALESCE");
    // A retry (`Eager` on) also comes when the finished output needed more
    // registers than Luau has ([`crate::register_pressure`]): the locals
    // then leave room for the widest statement's temporaries as well.
    let limit = match sharing {
        Sharing::Deferred => LOCAL_LIMIT,
        _ => LOCAL_LIMIT.min(
            crate::register_pressure::REGISTER_LIMIT
                .saturating_sub(crate::register_pressure::widest_statement(&block.0, upvalues)),
        ),
    };
    #[cfg(not(test))]
    let precheck = true;
    #[cfg(test)]
    let precheck = !REFERENCE_PRESSURE_COLLECTOR.with(std::cell::Cell::get);
    if precheck {
        if pressure_within_limit(block, parameters, upvalues, limit) {
            crate::telemetry::count("coalesce_pressure_census_accepted", 1);
            return;
        }
        crate::telemetry::count("coalesce_pressure_census_refused", 1);
    }
    let parameter_set: FxHashSet<_> = parameters.iter().cloned().collect();
    let external_set: FxHashSet<_> = upvalues.iter().cloned().collect();
    let mut position = 0;
    let mut branch_id = 0;
    let mut loop_id = 0;
    let mut captured = FxHashSet::default();
    let mut infos: FxHashMap<RcLocal, LocalInfo> = FxHashMap::default();
    collect_block(
        block,
        &mut position,
        &mut branch_id,
        &mut loop_id,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut captured,
        &mut infos,
        protected,
    );
    let owned_bindings = infos.keys().filter(|local|
        !parameter_set.contains(*local) && !external_set.contains(*local)).count();
    if owned_bindings.saturating_add(parameters.len()) <= limit { return; }
    for (local, info) in &mut infos {
        info.blocked |= captured.contains(local) || parameter_set.contains(local) || external_set.contains(local);
    }

    // Count declarations along a lexical scope chain, not across the entire
    // function. Hundreds of locals in separate dispatch arms never occupy
    // registers at the same time. Coalescing those functions needlessly makes
    // unrelated computations share identities and hides inline helper shapes.
    let mut scope_sizes: FxHashMap<(Vec<usize>, Vec<(usize, bool)>), usize> = FxHashMap::default();
    for info in infos.values().filter(|info|
        !parameter_set.contains(&info.local) && !external_set.contains(&info.local)) {
        *scope_sizes
            .entry((info.loop_scope.clone(), info.branch_scope.clone()))
            .or_default() += 1;
    }
    *scope_sizes.entry((Vec::new(), Vec::new())).or_default() += parameters.len();
    if !scope_pressure_exceeds(&scope_sizes, limit) { return; }

    // Do not split branch-private identities here.  That transformation needs
    // block/region liveness and definite-assignment facts; a local may be live
    // into or out of a nested loop even when a shallow sibling walk cannot see
    // the use.  Keeping the conservative interval coalescer is preferable to
    // manufacturing fresh, uninitialised cells.  Large functions that still
    // exceed the register limit are left for the certified fallback.

    let mut values = infos.into_values().collect::<Vec<_>>();
    values.sort_by_key(|info| (info.first, info.last, info.local.stable_id()));
    let (replacements, mixed) = coalesce_values(values, sharing);
    if !replacements.is_empty() {
        crate::replace_locals::replace_locals(block, &replacements);
    }
    for local in mixed {
        local.0.lock().0 = None;
    }
}

type ScopeKey = (Vec<usize>, Vec<(usize, bool)>);

fn scope_pressure_exceeds(sizes: &FxHashMap<ScopeKey, usize>, limit: usize) -> bool {
    let mut by_loops: FxHashMap<&[usize], FxHashMap<&[(usize, bool)], usize>> = FxHashMap::default();
    for ((loops, branches), &count) in sizes {
        by_loops.entry(loops).or_default().insert(branches, count);
    }
    sizes.keys().any(|(loops, branches)| {
        let mut pressure = 0;
        for loop_depth in 0..=loops.len() {
            if let Some(scopes) = by_loops.get(&loops[..loop_depth]) {
                for branch_depth in 0..=branches.len() {
                    pressure += scopes.get(&branches[..branch_depth]).copied().unwrap_or(0);
                    if pressure > limit { return true; }
                }
            }
        }
        false
    })
}

/// Minimum eligible position over source-ordered groups. A group containing a
/// member first used directly in its declaration scope certainly interferes
/// until that member's last use: its first occurrence shares the scope prefix
/// with every later candidate. Other branch-sensitive groups still undergo the
/// unchanged all-member proof; the index only skips certified conflicts.
#[derive(Default)]
struct GroupAvailability {
    size: usize,
    len: usize,
    minimum: Vec<usize>,
}

impl GroupAvailability {
    fn push(&mut self, ready: usize) {
        if self.len == self.size {
            let size = (self.size * 2).max(1);
            let mut minimum = vec![usize::MAX; size * 2];
            for index in 0..self.len { minimum[size + index] = self.minimum[self.size + index]; }
            for node in (1..size).rev() { minimum[node] = minimum[node * 2].min(minimum[node * 2 + 1]); }
            self.minimum = minimum;
            self.size = size;
        }
        let index = self.len;
        self.len += 1;
        self.set(index, ready);
    }

    fn set(&mut self, index: usize, ready: usize) {
        let mut node = self.size + index;
        self.minimum[node] = ready;
        while node > 1 {
            node /= 2;
            self.minimum[node] = self.minimum[node * 2].min(self.minimum[node * 2 + 1]);
        }
    }

    fn first_ready(&self, start: usize, position: usize) -> Option<usize> {
        if self.len == 0 { return None; }
        self.search(1, 0, self.size, start, position)
    }

    fn search(&self, node: usize, left: usize, right: usize, start: usize, position: usize) -> Option<usize> {
        if right <= start || self.minimum[node] > position { return None; }
        if right - left == 1 { return Some(left); }
        let middle = (left + right) / 2;
        self.search(node * 2, left, middle, start, position)
            .or_else(|| self.search(node * 2 + 1, middle, right, start, position))
    }
}

#[derive(Default)]
struct ScopeGroups {
    groups: Vec<CoalesceGroup>,
    available: GroupAvailability,
}

/// Whether some function in `block` declares more locals at once than Luau
/// accepts: a scope's declarations stay live to its end, a nested scope adds
/// its own, and a closure starts from its parameters.
pub fn declared_locals_exceed_limit(block: &Block) -> bool {
    fn scope(statements: &[Statement], mut active: usize) -> bool {
        for statement in statements {
            let mut exceeded = false;
            statement.traverse_rvalues_ref(&mut |value| {
                if !exceeded && let RValue::Closure(closure) = value {
                    let function = closure.function.0.lock();
                    exceeded = scope(&function.body.0, function.parameters.len());
                }
            });
            exceeded = exceeded || match statement {
                Statement::Assign(assign) if assign.prefix => {
                    active += assign.left.len();
                    false
                }
                Statement::If(node) => scope(&node.then_block.lock().0, active) || scope(&node.else_block.lock().0, active),
                Statement::While(node) => scope(&node.block.lock().0, active),
                Statement::Repeat(node) => scope(&node.block.lock().0, active),
                Statement::NumericFor(node) => scope(&node.block.lock().0, active + 1),
                Statement::GenericFor(node) => scope(&node.block.lock().0, active + node.res_locals.len()),
                _ => false,
            };
            if exceeded || active > LOCAL_LIMIT {
                return true;
            }
        }
        false
    }
    scope(&block.0, 0)
}

/// A temporary later passes fold into its one use, which a storage slot
/// shared with other temporaries would forbid, leaving one local reassigned
/// throughout (`Players = game:GetService("ReplicatedStorage")`): a value
/// the very next statement reads (`t = game:GetService("X"); services.X =
/// t`), or a table built by stores into it and then read once (`t = {...};
/// t.X = x; ...; Services = t`).
fn folds_into_its_use(info: &LocalInfo) -> bool {
    match info.occurrences.as_slice() {
        [definition, use_] if use_.position == definition.position + 1 => {
            definition.written && !definition.read && use_.read && !use_.written
                && use_.branches == definition.branches
        }
        [definition, stores @ .., use_] => {
            definition.builds_table && !definition.read
                && stores.iter().all(|store| store.stores_into && store.branches == definition.branches)
                && use_.read && !use_.written && use_.branches == definition.branches
        }
        _ => false,
    }
}

/// The replacements, and the slots that took locals of other names.
fn coalesce_values(values: Vec<LocalInfo>, sharing: Sharing) -> (FxHashMap<RcLocal, RcLocal>, Vec<RcLocal>) {
    let mut scopes: FxHashMap<(ScopeKey, Option<String>), ScopeGroups> = FxHashMap::default();
    let mut replacements = FxHashMap::default();
    let mut mixed = Vec::new();
    for info in values {
        if info.blocked || (sharing < Sharing::Eager && folds_into_its_use(&info)) { continue; }
        // Storage is shared only between locals of one spelling: unnamed
        // temporaries, or, when eager, source locals of the same name (`do
        // local a = ... end` repeated past the limit, flattened into one
        // scope, becomes one reassigned `a`); at `AnyName`, any.
        let Some(spelling) = storage_spelling(&info.local, sharing) else { continue; };
        let spelling = if sharing == Sharing::AnyName { None } else { spelling };
        let ready = if info.occurrences.iter().any(|occurrence|
            occurrence.position == info.first && occurrence.branches == info.branch_scope)
        { info.last + 1 } else { 0 };
        let scope = scopes.entry(((info.loop_scope.clone(), info.branch_scope.clone()), spelling)).or_default();
        let mut start = 0;
        let mut selected = None;
        while let Some(index) = scope.available.first_ready(start, info.first) {
            let group = &mut scope.groups[index];
            // First positions are monotone. Expired intervals cannot interfere
            // with this or any later candidate, even on conditional branches.
            group.members.retain(|member| member.last >= info.first);
            if can_join_group(group, &info) { selected = Some(index); break; }
            start = index + 1;
        }
        if let Some(index) = selected {
            let group = &mut scope.groups[index];
            if sharing == Sharing::AnyName
                && storage_spelling(&info.local, sharing) != storage_spelling(&group.representative.local, sharing)
                && !mixed.contains(&group.representative.local)
            {
                mixed.push(group.representative.local.clone());
            }
            replacements.insert(info.local.clone(), group.representative.local.clone());
            group.members.push(info);
            scope.available.set(index, ready.max(scope.available.minimum[scope.available.size + index]));
        } else {
            scope.groups.push(CoalesceGroup { representative: info.clone(), members: vec![info] });
            scope.available.push(ready);
        }
    }
    (replacements, mixed)
}

fn can_join_group(group: &CoalesceGroup, info: &LocalInfo) -> bool {
    !group.representative.blocked
        && group.representative.loop_scope == info.loop_scope
        // Separate lexical arms already reuse VM registers when compiled.
        // Merging their source locals forces the declaration into their common
        // parent, *increasing* live register pressure and preventing de-inline
        // matching. Only reuse storage within the same declaration scope.
        && group.representative.branch_scope == info.branch_scope
        && group
            .members
            .iter()
            .all(|member| !ranges_interfere(member, info))
}

/// Capture records are reads at closure construction, but their cells can
/// remain live after the last shallow read. Protect Copy and Ref captures and
/// include closures in indexed assignment targets; never descend into a child
/// function's body as though that body executed in this lexical scope.
fn collect_statement_captures(statement: &Statement, captured: &mut FxHashSet<RcLocal>) {
    statement.traverse_rvalues_ref(&mut |value| {
        if let RValue::Closure(closure) = value {
            captured.extend(closure.upvalues.iter().map(|upvalue| match upvalue {
                crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local) => local.clone(),
            }));
        }
    });
}

fn is_unnamed(local: &RcLocal) -> bool {
    !local.preserve_binding() && local.0.0.lock().0.is_none()
}

/// The spelling a local brings to a shared storage slot: `Some(None)` for an
/// unnamed temporary, `Some(Some(name))` from `Eager` on for a source local
/// whose debug intervals all name it the same, `None` when it keeps its own.
fn storage_spelling(local: &RcLocal, sharing: Sharing) -> Option<Option<String>> {
    if is_unnamed(local) {
        return Some(None);
    }
    if sharing < Sharing::Eager {
        return None;
    }
    let inner = local.0.lock();
    if inner.4.conditional_result || inner.4.parameter
        || !inner.2.iter().all(|binding| matches!(binding.origin, crate::BindingOrigin::DebugLocal { .. }))
    {
        return None;
    }
    let name = inner.source_name()?;
    inner.0.as_deref().is_none_or(|own| own == name).then(|| Some(name.to_owned()))
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

/// `local = {...}`.
fn builds_table(statement: &Statement, local: &RcLocal) -> bool {
    matches!(statement, Statement::Assign(assign)
        if matches!(assign.left.as_slice(), [crate::LValue::Local(written)] if written == local)
            && matches!(assign.right.as_slice(), [RValue::Table(_)]))
}

/// `local.k = v` / `local[k] = v` reading `local` nowhere else.
fn stores_into(statement: &Statement, local: &RcLocal) -> bool {
    let Statement::Assign(assign) = statement else { return false };
    let mut base = false;
    for left in &assign.left {
        match left {
            crate::LValue::Index(index) => {
                if index.right.any_local_read(&mut |read| read == local) {
                    return false;
                }
                match index.left.as_ref() {
                    RValue::Local(read) if read == local => base = true,
                    other if other.any_local_read(&mut |read| read == local) => return false,
                    _ => {}
                }
            }
            crate::LValue::Local(written) if written == local => return false,
            _ => {}
        }
    }
    base && !assign.right.iter().any(|value| value.any_local_read(&mut |read| read == local))
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
            builds_table: builds_table(statement, local),
            stores_into: stores_into(statement, local),
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
    captured: &mut FxHashSet<RcLocal>,
    infos: &mut FxHashMap<RcLocal, LocalInfo>,
    protected: &FxHashSet<RcLocal>,
) {
    for statement in block.iter_mut() {
        let current_position = *position;
        *position += 1;
        collect_statement_captures(statement, captured);
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
                    captured,
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
                    captured,
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
                    captured,
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
                    captured,
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
                    captured,
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
                    captured,
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

    fn legacy_pressure(block: &mut Block, parameters: &[RcLocal], upvalues: &[RcLocal]) -> usize {
        let mut infos = FxHashMap::default();
        collect_block(block, &mut 0, &mut 0, &mut 0, &mut Vec::new(), &mut Vec::new(),
            &mut FxHashSet::default(), &mut infos, &FxHashSet::default());
        let parameters_set: FxHashSet<_> = parameters.iter().cloned().collect();
        let external_set: FxHashSet<_> = upvalues.iter().cloned().collect();
        infos.keys().filter(|local| !parameters_set.contains(*local) && !external_set.contains(*local))
            .count().saturating_add(parameters.len())
    }

    #[test]
    fn pressure_census_matches_collector_without_owners_or_nested_function_reads() {
        for parameter_count in [0, 1, 180, 200, 201] {
            for local_count in [0, 1, 19, 20, 21, 199, 200, 201, 205] {
                let mut parameters: Vec<_> = (0..parameter_count).map(|_| RcLocal::default()).collect();
                // Retain the old count of declared slots even if a hand-built
                // parameter vector contains repeated identities.
                if parameters.len() > 1 { parameters[1] = parameters[0].clone(); }
                let upvalues: Vec<_> = (0..3).map(|_| RcLocal::default()).collect();
                let locals: Vec<_> = (0..local_count).map(|_| RcLocal::default()).collect();
                let mut body = Block(locals.iter().map(|local| Assign::new(
                    vec![local.clone().into(), local.clone().into()],
                    vec![upvalues[0].clone().into(), upvalues[0].clone().into()],
                ).into()).collect());
                let child = ByAddress(Arc::new(Mutex::new(Function {
                    body: Block((0..250).map(|_| Assign::new(vec![RcLocal::default().into()],
                        vec![Literal::Nil.into()]).into()).collect()), ..Function::default()
                })));
                body.push(Assign::new(vec![Index::new(Global::from("targets").into(), Closure {
                    node_origin: Default::default(), function: child.clone(),
                    upvalues: vec![Upvalue::Copy(upvalues[1].clone()), Upvalue::Ref(upvalues[2].clone())],
                }.into()).into()], vec![Literal::Nil.into()]).into());
                let shared = Arc::new(Mutex::new(body));
                let mut block = Block(vec![If {
                    node_origin: Default::default(), condition: Literal::Boolean(true).into(),
                    then_block: shared.clone(), else_block: shared.clone(),
                }.into()]);
                let snapshot = || parameters.iter().chain(&upvalues).chain(&locals)
                    .map(|local| (local.stable_id(), local.0.lock().clone(), Arc::count(&local.0.0)))
                    .collect::<Vec<_>>();
                let before = snapshot();
                let body_owners = Arc::strong_count(&shared);
                let closure_owners = Arc::strong_count(&child.0);
                let ids = crate::current_local_id();
                let actual = pressure_within_limit(&block, &parameters, &upvalues, 200);
                assert_eq!(snapshot(), before);
                assert_eq!(Arc::strong_count(&shared), body_owners);
                assert_eq!(Arc::strong_count(&child.0), closure_owners);
                assert_eq!(crate::current_local_id(), ids);
                assert_eq!(actual, legacy_pressure(&mut block, &parameters, &upvalues) <= 200,
                    "parameters={parameter_count}, locals={local_count}");
                assert_eq!(snapshot(), before);
            }
        }
    }

    #[test]
    fn pressure_gate_preserves_full_coalescing_metadata_captures_and_loop_binders() {
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) { REFERENCE_PRESSURE_COLLECTOR.with(|flag| flag.set(self.0)); }
        }
        type OriginView = Option<(Vec<crate::node_origins::Input>, bool, bool, Option<&'static str>, bool)>;
        fn origins(block: &Block, out: &mut Vec<OriginView>) {
            let mut record = |origin: Option<&crate::node_origins::Origin>| {
                out.push(origin.and_then(|origin| origin.0.as_ref()).map(|data| (
                    data.inputs.iter().map(|input| (**input).clone()).collect(), data.inlined,
                    data.cloned, data.synthesized, data.incomplete,
                )));
            };
            for statement in block.iter() {
                record(crate::node_origins::statement(statement));
                statement.traverse_rvalues_ref(&mut |value| record(crate::node_origins::value(value)));
            }
            for statement in block.iter() {
                match statement {
                    Statement::If(branch) => {
                        origins(&branch.then_block.lock(), out);
                        origins(&branch.else_block.lock(), out);
                    }
                    Statement::While(node) => origins(&node.block.lock(), out),
                    Statement::Repeat(node) => origins(&node.block.lock(), out),
                    Statement::NumericFor(node) => origins(&node.block.lock(), out),
                    Statement::GenericFor(node) => origins(&node.block.lock(), out),
                    _ => {}
                }
            }
        }
        for count in [0, 3, 195, 196, 197, 198, 199, 200, 201, 240] {
            let locals: Vec<_> = (0..count).map(|_| RcLocal::default()).collect();
            let [parameter, upvalue, counter, result, captured] = std::array::from_fn(|_| RcLocal::default());
            captured.0.lock().add_source_binding(crate::SourceBinding {
                origin: crate::BindingOrigin::DebugLocal { prototype: 1, register: 0, start_pc: 0, end_pc: 1 },
                name: "captured".into(),
            });
            let child = ByAddress(Arc::new(Mutex::new(Function::default())));
            let mut statements = Vec::new();
            for (index, local) in locals.iter().enumerate() {
                let mut assign = Assign::new(vec![local.clone().into()], vec![Literal::Number(index as f64).into()]);
                assign.node_origin = crate::node_origins::Origin::input(crate::node_origins::Input {
                    function: "pressure-oracle".into(), block: 0, statement: index, value: None,
                });
                statements.push(assign.into());
            }
            statements.extend([
                Assign::new(vec![captured.clone().into()], vec![parameter.clone().into()]).into(),
                Call::new(Global::from("save").into(), vec![Closure {
                    node_origin: Default::default(), function: child.clone(),
                    upvalues: vec![Upvalue::Ref(captured.clone()), Upvalue::Copy(upvalue.clone())],
                }.into()]).into(),
                NumericFor::new(Literal::Number(1.0).into(), Literal::Number(2.0).into(),
                    Literal::Number(1.0).into(), counter.clone(), Block::default()).into(),
                GenericFor::new(vec![result.clone()], vec![upvalue.clone().into()], Block::default()).into(),
            ]);
            let block = Block(statements);
            let protected = FxHashSet::from_iter([captured.clone()]);
            let parameters = vec![parameter.clone()];
            let upvalues = vec![upvalue.clone()];
            let all = locals.iter().chain([&parameter, &upvalue, &counter, &result, &captured]).collect::<Vec<_>>();
            let metadata = all.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>();
            let ids = crate::current_local_id();
            let mut expected = crate::simplify_gotos::deep_clone_block(&block);
            {
                let _restore = Restore(REFERENCE_PRESSURE_COLLECTOR.with(|flag| flag.replace(true)));
                coalesce_generated_locals_in_function(&mut expected, &protected, &parameters, &upvalues, Sharing::Eager);
            }
            let expected_metadata = all.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>();
            for (local, saved) in all.iter().zip(&metadata) { *local.0.lock() = saved.clone(); }
            let mut actual = crate::simplify_gotos::deep_clone_block(&block);
            coalesce_generated_locals_in_function(&mut actual, &protected, &parameters, &upvalues, Sharing::Eager);
            assert_eq!(actual.to_string(), expected.to_string(), "count={count}");
            let mut actual_origins = Vec::new(); let mut expected_origins = Vec::new();
            origins(&actual, &mut actual_origins); origins(&expected, &mut expected_origins);
            assert_eq!(actual_origins, expected_origins);
            assert_eq!(all.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>(), expected_metadata);
            assert_eq!(crate::current_local_id(), ids);
            assert!(child.lock().body.is_empty(), "a capture census must not enter child functions");
            let mut source = block.clone();
            assert_eq!(pressure_within_limit(&source, &parameters, &upvalues, 200),
                legacy_pressure(&mut source, &parameters, &upvalues) <= 200);
        }
    }

    #[test]
    fn indexed_groups_match_all_members_first_fit() {
        for seed in 1..100u64 {
            let mut random = seed;
            let mut next = || { random ^= random << 13; random ^= random >> 7; random ^= random << 17; random as usize };
            let mut values = Vec::new();
            for _ in 0..150 {
                let first = next() % 100;
                let last = first + next() % 40;
                let path = |choice: usize| match choice {
                    0 => vec![],
                    1 => vec![(0, true)],
                    2 => vec![(0, false)],
                    3 => vec![(0, true), (1, true)],
                    4 => vec![(0, true), (1, false)],
                    _ => vec![(0, false), (2, true)],
                };
                let mut info = LocalInfo::new(RcLocal::default(), Occurrence {
                    position: first, branches: path(next() % 6), read: true, written: false,
                    builds_table: false, stores_into: false,
                }, &[], false);
                info.add(Occurrence { position: last, branches: path(next() % 6), read: true, written: false,
                    builds_table: false, stores_into: false }, &[], false);
                values.push(info);
            }
            values.sort_by_key(|info| (info.first, info.last, info.local.stable_id()));
            let mut expected = FxHashMap::default();
            let mut groups: Vec<CoalesceGroup> = Vec::new();
            for info in values.iter().cloned() {
                if let Some(group) = groups.iter_mut().find(|group| can_join_group(group, &info)) {
                    expected.insert(info.local.clone(), group.representative.local.clone());
                    group.members.push(info);
                } else { groups.push(CoalesceGroup { representative: info.clone(), members: vec![info] }); }
            }
            assert_eq!(coalesce_values(values, Sharing::Eager).0, expected, "seed {seed}");
        }
    }

    #[test]
    fn indexed_scope_pressure_matches_full_prefix_scan() {
        let mut sizes = FxHashMap::default();
        for index in 0..200 {
            let loops = if index % 3 == 0 { vec![] } else { vec![index % 5] };
            let branches = if index % 4 == 0 { vec![] } else { vec![(index % 8, index % 2 == 0)] };
            *sizes.entry((loops, branches)).or_default() += 1;
            for limit in [0, 10, 30, 80, 240] {
                let expected = sizes.keys().any(|(loops, branches)| sizes.iter()
                    .filter(|((parent_loops, parent_branches), _)| loops.starts_with(parent_loops) && branches.starts_with(parent_branches))
                    .map(|(_, count)| count).sum::<usize>() > limit);
                assert_eq!(scope_pressure_exceeds(&sizes, limit), expected);
            }
        }
    }

    fn synthetic_info(first: usize, last: usize) -> LocalInfo {
        let local = RcLocal::default();
        let occurrence = Occurrence {
            position: first,
            branches: Vec::new(),
            read: true,
            written: false,
            builds_table: false,
            stores_into: false,
        };
        let mut info = LocalInfo::new(local, occurrence, &[], false);
        if last > first {
            info.add(
                Occurrence {
                    position: last,
                    branches: Vec::new(),
                    read: true,
                    written: false,
                    builds_table: false,
                    stores_into: false,
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
    fn reuses_storage_inside_arms_without_hoisting_their_declarations() {
        let arm = || {
            Block(
                (0..241)
                    .map(|n| {
                        Assign::new(vec![LValue::Local(RcLocal::default())], vec![
                            RValue::Literal(Literal::Number(n as f64)),
                        ])
                        .into()
                    })
                    .collect(),
            )
        };
        let mut block = Block(vec![
            If::new(Global::from("flag").into(), arm(), arm()).into(),
        ]);
        coalesce_generated_locals(&mut block, &FxHashSet::default());
        let branch = block[0].as_if().unwrap();
        let locals_in = |block: &Block| {
            block
                .iter()
                .flat_map(|statement| statement.values_written())
                .cloned()
                .collect::<FxHashSet<_>>()
        };
        let then_locals = locals_in(&branch.then_block.lock());
        let else_locals = locals_in(&branch.else_block.lock());
        assert_eq!(then_locals.len(), 1);
        assert_eq!(else_locals.len(), 1);
        assert!(then_locals.is_disjoint(&else_locals));
        let root = Arc::new(Mutex::new(block));
        crate::local_declarations::LocalDeclarer::default()
            .declare_locals(root.clone(), &FxHashSet::default());
        let root = root.lock();
        assert_eq!(root.len(), 1, "no declaration should escape its branch");
        let branch = root[0].as_if().unwrap();
        assert!(branch.then_block.lock()[0].as_assign().unwrap().prefix);
        assert!(branch.else_block.lock()[0].as_assign().unwrap().prefix);
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
    fn source_binding_pressure_counts_parameters_and_excludes_upvalues() {
        fn locals_block(count: usize) -> Block {
            Block((0..count).map(|index| Assign::new(vec![RcLocal::default().into()],
                vec![Literal::Number(index as f64).into()]).into()).collect())
        }
        let parameters: Vec<_> = (0..180).map(|_| RcLocal::default()).collect();
        let mut at_limit = locals_block(20);
        let unchanged = at_limit.clone();
        coalesce_generated_locals_in_function(&mut at_limit, &FxHashSet::default(), &parameters, &[], Sharing::Eager);
        assert_eq!(at_limit, unchanged, "200 bindings are legal even with unused parameters");
        let mut over_limit = locals_block(21);
        coalesce_generated_locals_in_function(&mut over_limit, &FxHashSet::default(), &parameters, &[], Sharing::Eager);
        let written: FxHashSet<_> = over_limit.iter().flat_map(|statement| statement.values_written()).collect();
        assert_eq!(written.len(), 1, "the 201st binding triggers conservative reuse");
        let upvalues: Vec<_> = (0..100).map(|_| RcLocal::default()).collect();
        let mut block = locals_block(150);
        block.push(Call::new(Global::from("use").into(), upvalues.iter().cloned().map(RValue::from).collect()).into());
        let unchanged = block.clone();
        coalesce_generated_locals_in_function(&mut block, &FxHashSet::default(), &[], &upvalues, Sharing::Eager);
        assert_eq!(block, unchanged, "incoming upvalues are not local binding slots");
    }

    #[test]
    fn copy_and_ref_capture_cells_outlive_last_shallow_occurrence() {
        let copy = RcLocal::default(); let reference = RcLocal::default();
        let child_local = RcLocal::default();
        let function = ByAddress(Arc::new(Mutex::new(Function { body: Block(vec![
            Assign::new(vec![child_local.clone().into()], vec![Literal::Number(7.0).into()]).into(),
            crate::Return::new(vec![copy.clone().into(), reference.clone().into()]).into(),
        ]), ..Function::default() })));
        let closure = Closure { node_origin: Default::default(), function: function.clone(),
            upvalues: vec![Upvalue::Copy(copy.clone()), Upvalue::Ref(reference.clone())] };
        let mut block = Block(vec![
            Assign::new(vec![copy.clone().into()], vec![Literal::Number(1.0).into()]).into(),
            Assign::new(vec![reference.clone().into()], vec![Literal::Number(2.0).into()]).into(),
            Call::new(Global::from("save").into(), vec![closure.into()]).into(),
        ]);
        for index in 0..205 {
            let temp = RcLocal::default();
            block.push(Assign::new(vec![temp.clone().into()], vec![Literal::Number(index as f64).into()]).into());
            block.push(Call::new(Global::from("callback").into(), vec![temp.into()]).into());
        }
        let child_before = function.lock().body.clone();
        coalesce_generated_locals(&mut block, &FxHashSet::default());
        let written: FxHashSet<_> = block.iter().flat_map(|statement| statement.values_written()).cloned().collect();
        assert!(written.contains(&copy) && written.contains(&reference));
        assert_eq!(written.len(), 3, "only the uncaptured sequential temporaries share a third cell");
        assert_eq!(function.lock().body, child_before, "child function locals are a separate execution frame");
        let captures = &block[2].as_call().unwrap().arguments[0].as_closure().unwrap().upvalues;
        assert_eq!(captures, &vec![Upvalue::Copy(copy), Upvalue::Ref(reference)]);
    }

    #[test]
    fn temporaries_folded_into_their_use_keep_their_identity_unless_eager() {
        // `t = n; callback(t)` and a table built by stores then read once: a
        // shared slot would leave one local reassigned throughout; the caller
        // falls back to eager sharing when the finished function overflows.
        let build = || {
            let table = RcLocal::default();
            let mut block = Block(vec![
                Assign::new(vec![table.clone().into()], vec![RValue::Table(crate::Table::new(Vec::new()))]).into(),
                Assign::new(vec![LValue::Index(Index::new(table.clone().into(), Literal::String(b"k".to_vec()).into()))],
                    vec![Literal::Number(1.0).into()]).into(),
                Assign::new(vec![LValue::Global(Global::from("Services"))], vec![table.clone().into()]).into(),
            ]);
            let mut temps = Vec::new();
            for index in 0..205 {
                let temp = RcLocal::default();
                block.push(Assign::new(vec![temp.clone().into()], vec![Literal::Number(index as f64).into()]).into());
                block.push(Call::new(Global::from("callback").into(), vec![temp.clone().into()]).into());
                temps.push(temp);
            }
            (block, table, temps)
        };
        let written = |block: &Block| block.iter().flat_map(|statement| statement.values_written()).cloned().collect::<FxHashSet<_>>();
        let (mut block, table, temps) = build();
        coalesce_generated_locals_in_function(&mut block, &FxHashSet::default(), &[], &[], Sharing::Deferred);
        let kept = written(&block);
        assert!(kept.contains(&table) && temps.iter().all(|temp| kept.contains(temp)));
        let (mut block, _, _) = build();
        coalesce_generated_locals_in_function(&mut block, &FxHashSet::default(), &[], &[], Sharing::Eager);
        assert_eq!(written(&block).len(), 1, "eager sharing gives the temporaries one slot with the table");
    }

    #[test]
    fn declared_local_pressure_counts_scopes_and_closures() {
        let declare = |count: usize| (0..count).map(|index| {
            Assign { prefix: true, ..Assign::new(vec![RcLocal::default().into()], vec![Literal::Number(index as f64).into()]) }.into()
        }).collect::<Vec<Statement>>();
        assert!(!declared_locals_exceed_limit(&Block(declare(LOCAL_LIMIT))));
        assert!(declared_locals_exceed_limit(&Block(declare(LOCAL_LIMIT + 1))));
        let nested = Block(declare(LOCAL_LIMIT - 1));
        let mut outer = Block(declare(2));
        outer.push(crate::While::new(Literal::Boolean(true).into(), nested).into());
        assert!(declared_locals_exceed_limit(&outer));
        let function = Function { body: Block(declare(LOCAL_LIMIT + 1)), ..Function::default() };
        let closure = Closure { node_origin: Default::default(), function: ByAddress(Arc::new(Mutex::new(function))), upvalues: Vec::new() };
        assert!(declared_locals_exceed_limit(&Block(vec![Call::new(Global::from("run").into(), vec![closure.into()]).into()])));
    }

    #[test]
    fn closure_in_indexed_lhs_protects_capture_without_disabling_unrelated_reuse() {
        let captured = RcLocal::default();
        let value = RcLocal::default();
        let closure = RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(captured.clone())],
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
        assert_eq!(first, Some(value.clone()));
        let pressure = match &block.0[2] {
            Statement::Assign(assign) => assign.left[0].as_local().cloned(),
            _ => None,
        };
        assert_eq!(pressure, Some(value.clone()));
        let closure = block[1].as_assign().unwrap().left[0].as_index().unwrap().right.as_closure().unwrap();
        assert_eq!(closure.upvalues, vec![Upvalue::Ref(captured.clone())]);
        assert_ne!(value, captured);
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
