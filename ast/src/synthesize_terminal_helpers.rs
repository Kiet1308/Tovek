//! Synthesize small local helpers for large, exactly duplicated terminal regions.
//!
//! Structured early-return lowering can clone a caller's terminal continuation
//! into many unrelated branches.  Local cross-jumping cannot always hoist those
//! copies back through every enclosing loop/conditional.  This pass performs the
//! safe source-level equivalent: a repeated terminal region becomes one captured
//! local function and every copy becomes `return helper()`.
//!
//! The admission policy is intentionally strict: at least four copies, a large
//! structured body, no closures/varargs/loop-control transfer, exact free-local
//! identity, and alpha-renaming only for locals declared inside the region.

use by_address::ByAddress;
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    deinline::{collect_declared_locals, collect_reads, collect_written, dbg_stmt_node_count},
    factor_common_tails::block_alpha_bindings_with_locals,
    Assign, Block, Call, Closure, Comment, Function, LValue, Literal, Local, LocalRw, RValue,
    RcLocal, Return, Select, Statement, Traverse, Upvalue,
};

const MIN_NODES: usize = 12;
const MAX_REGION_STATEMENTS: usize = 5;
const MAX_HELPERS_PER_SCOPE: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct CandidateKey {
    statements: usize,
    nodes: usize,
    first_tag: u8,
}

#[derive(Debug)]
struct Group {
    template: Vec<Statement>,
    localizable: FxHashSet<RcLocal>,
    occurrences: usize,
    nodes: usize,
}

/// Deduplicate terminal continuations in the module body and every pre-existing
/// closure body. Returns the number of synthesized helpers.
pub fn synthesize_terminal_helpers(body: &mut Block) -> usize {
    crate::factor_common_tails::unshare_blocks(body);
    synthesize_in_existing_closures(&mut body.0) + synthesize_scope(&mut body.0)
}

fn synthesize_in_existing_closures(stmts: &mut [Statement]) -> usize {
    let mut count = 0;
    for statement in stmts {
        count += synthesize_in_statement_children(statement);
        for value in crate::deinline::stmt_rvalues_mut(statement) {
            count += synthesize_in_rvalue(value);
        }
    }
    count
}

fn synthesize_in_statement_children(statement: &mut Statement) -> usize {
    match statement {
        Statement::If(node) => {
            synthesize_in_existing_closures(&mut node.then_block.lock().0)
                + synthesize_in_existing_closures(&mut node.else_block.lock().0)
        }
        Statement::While(node) => synthesize_in_existing_closures(&mut node.block.lock().0),
        Statement::Repeat(node) => synthesize_in_existing_closures(&mut node.block.lock().0),
        Statement::NumericFor(node) => synthesize_in_existing_closures(&mut node.block.lock().0),
        Statement::GenericFor(node) => synthesize_in_existing_closures(&mut node.block.lock().0),
        _ => 0,
    }
}

fn synthesize_in_rvalue(value: &mut RValue) -> usize {
    if let RValue::Closure(closure) = value {
        let mut function = closure.function.0.lock();
        let nested = synthesize_in_existing_closures(&mut function.body.0);
        return nested + synthesize_scope(&mut function.body.0);
    }
    value
        .rvalues_mut()
        .into_iter()
        .map(synthesize_in_rvalue)
        .sum()
}

fn synthesize_scope(stmts: &mut Vec<Statement>) -> usize {
    let mut synthesized = 0;
    while synthesized < MAX_HELPERS_PER_SCOPE {
        if count_potential_sites(stmts, 2) < 2 {
            break;
        }
        let mut scope_declared = FxHashSet::default();
        collect_declared_locals(stmts, &mut scope_declared);
        let mut captured = FxHashSet::default();
        collect_captured_locals(stmts, &mut captured);
        let groups = collect_groups(stmts, &scope_declared, &captured);
        let Some((template, localizable, insertion, upvalues, name)) =
            choose_candidate(stmts, groups, &scope_declared, &captured)
        else {
            break;
        };

        let helper = RcLocal::new(Local::new(Some(unique_name(stmts, &name))));
        let replaced = replace_suffixes(
            &mut stmts[insertion..],
            &template,
            &localizable,
            &scope_declared,
            &captured,
            &helper,
        );
        // `choose_candidate` counted this exact, owner-local replacement domain.
        // Still keep declaration insertion transactional with any future matcher
        // change: once a call was emitted, its helper must always be declared.
        if replaced == 0 {
            break;
        }

        let function_name = helper.0 .0.lock().0.clone();
        // Candidate statements contain Arc-backed blocks. A deep copy prevents
        // later helper rewrites from mutating any surviving source owner.
        let mut helper_body = crate::simplify_gotos::dc_block(&Block(template)).0;
        if !localizable.is_empty() {
            let mut originals: Vec<RcLocal> = localizable.into_iter().collect();
            originals.sort();
            // These writes become lexical locals of a new function, not the
            // outer dead result cells they replace. Give them fresh identities
            // so usage/name/capture analysis cannot merge the two binders.
            let mut remap = FxHashMap::default();
            let mut locals = Vec::with_capacity(originals.len());
            for original in originals {
                let name = original.0 .0.lock().0.clone();
                let fresh = RcLocal::new(Local::new(name));
                remap.insert(original, fresh.clone());
                locals.push(fresh);
            }
            let mut helper_block = Block(helper_body);
            crate::replace_locals::replace_locals(&mut helper_block, &remap);
            helper_body = helper_block.0;
            helper_body.insert(
                0,
                Statement::Assign(Assign {
                    left: locals.into_iter().map(LValue::Local).collect(),
                    right: Vec::new(),
                    prefix: true,
                    parallel: false,
                }),
            );
        }
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(Function {
                name: function_name,
                parameters: Vec::new(),
                is_variadic: false,
                body: Block(helper_body),
            }))),
            // Ref is the conservative capture mode: reads observe the value at
            // call time and any local writes retain their original cell identity.
            upvalues: upvalues.into_iter().map(Upvalue::Ref).collect(),
        };
        let declaration = Statement::Assign(Assign {
            left: vec![LValue::Local(helper)],
            right: vec![RValue::Closure(closure)],
            prefix: true,
            parallel: false,
        });
        let marker = Statement::Comment(Comment::new(format!(
            "[DEDUP] synthesized from {replaced} duplicated terminal regions"
        )));
        stmts.splice(insertion..insertion, [marker, declaration]);
        synthesized += 1;
    }
    synthesized
}

/// Allocation-free census used by the overwhelmingly common no-op scope. A
/// candidate group needs at least two physical terminal regions regardless of
/// its size, so stop as soon as two possible sites are found.
fn count_potential_sites(stmts: &[Statement], limit: usize) -> usize {
    let mut count = usize::from(
        matches!(stmts.last(), Some(Statement::Return(_)))
            && (2..=MAX_REGION_STATEMENTS.min(stmts.len()))
                .any(|len| structured_head(&stmts[stmts.len() - len])),
    );
    if count >= limit {
        return count;
    }
    for statement in stmts {
        let child_count = match statement {
            Statement::If(node) => {
                let then_count = count_potential_sites(&node.then_block.lock().0, limit - count);
                if count + then_count >= limit {
                    then_count
                } else {
                    then_count
                        + count_potential_sites(
                            &node.else_block.lock().0,
                            limit - count - then_count,
                        )
                }
            }
            Statement::While(node) => count_potential_sites(&node.block.lock().0, limit - count),
            Statement::Repeat(node) => count_potential_sites(&node.block.lock().0, limit - count),
            Statement::NumericFor(node) => {
                count_potential_sites(&node.block.lock().0, limit - count)
            }
            Statement::GenericFor(node) => {
                count_potential_sites(&node.block.lock().0, limit - count)
            }
            _ => 0,
        };
        count += child_count;
        if count >= limit {
            break;
        }
    }
    count
}

fn collect_groups(
    stmts: &[Statement],
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
) -> Vec<Group> {
    let mut buckets: FxHashMap<CandidateKey, Vec<Group>> = FxHashMap::default();
    collect_candidates(stmts, scope_declared, captured, &mut buckets);
    buckets.into_values().flatten().collect()
}

fn collect_candidates(
    stmts: &[Statement],
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
    buckets: &mut FxHashMap<CandidateKey, Vec<Group>>,
) {
    for statement in stmts {
        match statement {
            Statement::If(node) => {
                let then_block = node.then_block.lock().0.clone();
                let else_block = node.else_block.lock().0.clone();
                collect_candidates(&then_block, scope_declared, captured, buckets);
                collect_candidates(&else_block, scope_declared, captured, buckets);
            }
            Statement::While(node) => collect_candidates(
                &node.block.lock().0.clone(),
                scope_declared,
                captured,
                buckets,
            ),
            Statement::Repeat(node) => collect_candidates(
                &node.block.lock().0.clone(),
                scope_declared,
                captured,
                buckets,
            ),
            Statement::NumericFor(node) => collect_candidates(
                &node.block.lock().0.clone(),
                scope_declared,
                captured,
                buckets,
            ),
            Statement::GenericFor(node) => collect_candidates(
                &node.block.lock().0.clone(),
                scope_declared,
                captured,
                buckets,
            ),
            _ => {}
        }
    }

    if !matches!(stmts.last(), Some(Statement::Return(_))) {
        return;
    }
    for len in 2..=MAX_REGION_STATEMENTS.min(stmts.len()) {
        let region = &stmts[stmts.len() - len..];
        if !structured_head(&region[0]) || !region_movable(region) {
            continue;
        }
        let nodes = region.iter().map(dbg_stmt_node_count).sum();
        if nodes < MIN_NODES {
            continue;
        }
        let key = CandidateKey {
            statements: len,
            nodes,
            first_tag: statement_tag(&region[0]),
        };
        let groups = buckets.entry(key).or_default();
        if let Some(group) = groups.iter_mut().find(|group| {
            candidate_matches(
                &group.template,
                region,
                &group.localizable,
                scope_declared,
                captured,
            )
        }) {
            group.occurrences += 1;
        } else {
            groups.push(Group {
                template: region.to_vec(),
                localizable: localizable_destinations(region, scope_declared, captured),
                occurrences: 1,
                nodes,
            });
        }
    }
}

fn choose_candidate(
    scope: &[Statement],
    groups: Vec<Group>,
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
) -> Option<(
    Vec<Statement>,
    FxHashSet<RcLocal>,
    usize,
    Vec<RcLocal>,
    String,
)> {
    let mut declared_anywhere = FxHashSet::default();
    collect_declared_locals(scope, &mut declared_anywhere);
    let root_declarations = root_declaration_positions(scope);

    let mut ranked: Vec<Group> = groups
        .into_iter()
        .filter(|group| group.occurrences >= minimum_occurrences(group.nodes))
        .collect();
    ranked.sort_by(|left, right| {
        candidate_score(right)
            .cmp(&candidate_score(left))
            .then_with(|| right.nodes.cmp(&left.nodes))
    });

    for group in ranked {
        let mut declared = FxHashSet::default();
        collect_declared_locals(&group.template, &mut declared);
        declared.extend(group.localizable.iter().cloned());
        let mut referenced = FxHashSet::default();
        collect_reads(&group.template, &mut referenced);
        collect_written(&group.template, &mut referenced);
        referenced.retain(|local| !declared.contains(local));

        let mut insertion = 0;
        let mut free: Vec<RcLocal> = referenced.into_iter().collect();
        free.sort();
        let mut scope_safe = true;
        for local in &free {
            if let Some(&position) = root_declarations.get(local) {
                insertion = insertion.max(position + 1);
            } else if declared_anywhere.contains(local) {
                // Declared only in a nested sibling/branch: no common lexical
                // placement can capture it safely from this scope.
                scope_safe = false;
                break;
            }
        }
        if !scope_safe || insertion >= scope.len() {
            continue;
        }
        let suffix_count = count_suffixes(
            &scope[insertion..],
            &group.template,
            &group.localizable,
            scope_declared,
            captured,
        );
        if suffix_count < minimum_occurrences(group.nodes) {
            continue;
        }
        let name = helper_name(&group.template);
        return Some((group.template, group.localizable, insertion, free, name));
    }
    None
}

fn minimum_occurrences(nodes: usize) -> usize {
    if nodes >= 32 {
        2
    } else if nodes >= 20 {
        3
    } else {
        4
    }
}

fn candidate_score(group: &Group) -> usize {
    group
        .occurrences
        .saturating_sub(1)
        .saturating_mul(group.nodes.saturating_sub(2))
}

fn root_declaration_positions(stmts: &[Statement]) -> FxHashMap<RcLocal, usize> {
    let mut result = FxHashMap::default();
    for (index, statement) in stmts.iter().enumerate() {
        if let Statement::Assign(assign) = statement
            && assign.prefix
        {
            for left in &assign.left {
                if let LValue::Local(local) = left {
                    result.insert(local.clone(), index);
                }
            }
        }
    }
    result
}

fn count_suffixes(
    stmts: &[Statement],
    template: &[Statement],
    localizable: &FxHashSet<RcLocal>,
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
) -> usize {
    stmts
        .iter()
        .map(|statement| {
            count_statement_suffixes(statement, template, localizable, scope_declared, captured)
        })
        .sum()
}

fn count_statement_suffixes(
    statement: &Statement,
    template: &[Statement],
    localizable: &FxHashSet<RcLocal>,
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
) -> usize {
    match statement {
        Statement::If(node) => {
            count_suffix_vec(
                &node.then_block.lock().0,
                template,
                localizable,
                scope_declared,
                captured,
            ) + count_suffix_vec(
                &node.else_block.lock().0,
                template,
                localizable,
                scope_declared,
                captured,
            )
        }
        Statement::While(node) => count_suffix_vec(
            &node.block.lock().0,
            template,
            localizable,
            scope_declared,
            captured,
        ),
        Statement::Repeat(node) => count_suffix_vec(
            &node.block.lock().0,
            template,
            localizable,
            scope_declared,
            captured,
        ),
        Statement::NumericFor(node) => count_suffix_vec(
            &node.block.lock().0,
            template,
            localizable,
            scope_declared,
            captured,
        ),
        Statement::GenericFor(node) => count_suffix_vec(
            &node.block.lock().0,
            template,
            localizable,
            scope_declared,
            captured,
        ),
        _ => 0,
    }
}

fn count_suffix_vec(
    stmts: &[Statement],
    template: &[Statement],
    localizable: &FxHashSet<RcLocal>,
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
) -> usize {
    if suffix_matches(stmts, template, localizable, scope_declared, captured) {
        1
    } else {
        count_suffixes(stmts, template, localizable, scope_declared, captured)
    }
}

fn replace_suffixes(
    stmts: &mut [Statement],
    template: &[Statement],
    localizable: &FxHashSet<RcLocal>,
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
    helper: &RcLocal,
) -> usize {
    let mut count = 0;
    for statement in stmts.iter_mut() {
        count += match statement {
            Statement::If(node) => {
                replace_suffix_vec(
                    &mut node.then_block.lock().0,
                    template,
                    localizable,
                    scope_declared,
                    captured,
                    helper,
                ) + replace_suffix_vec(
                    &mut node.else_block.lock().0,
                    template,
                    localizable,
                    scope_declared,
                    captured,
                    helper,
                )
            }
            Statement::While(node) => replace_suffix_vec(
                &mut node.block.lock().0,
                template,
                localizable,
                scope_declared,
                captured,
                helper,
            ),
            Statement::Repeat(node) => replace_suffix_vec(
                &mut node.block.lock().0,
                template,
                localizable,
                scope_declared,
                captured,
                helper,
            ),
            Statement::NumericFor(node) => replace_suffix_vec(
                &mut node.block.lock().0,
                template,
                localizable,
                scope_declared,
                captured,
                helper,
            ),
            Statement::GenericFor(node) => replace_suffix_vec(
                &mut node.block.lock().0,
                template,
                localizable,
                scope_declared,
                captured,
                helper,
            ),
            _ => 0,
        };
    }
    count
}

fn replace_suffix_vec(
    stmts: &mut Vec<Statement>,
    template: &[Statement],
    localizable: &FxHashSet<RcLocal>,
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
    helper: &RcLocal,
) -> usize {
    if suffix_matches(stmts, template, localizable, scope_declared, captured) {
        stmts.truncate(stmts.len() - template.len());
        stmts.push(Statement::Return(Return::new(vec![RValue::Call(
            Call::new(RValue::Local(helper.clone()), Vec::new()),
        )])));
        return 1;
    }
    replace_suffixes(
        stmts,
        template,
        localizable,
        scope_declared,
        captured,
        helper,
    )
}

fn suffix_matches(
    stmts: &[Statement],
    template: &[Statement],
    localizable: &FxHashSet<RcLocal>,
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
) -> bool {
    if stmts.len() < template.len() {
        return false;
    }
    let candidate = &stmts[stmts.len() - template.len()..];
    statement_tag(&candidate[0]) == statement_tag(&template[0])
        && candidate_matches(template, candidate, localizable, scope_declared, captured)
}

fn structured_head(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::If(_)
            | Statement::While(_)
            | Statement::Repeat(_)
            | Statement::NumericFor(_)
            | Statement::GenericFor(_)
    )
}

fn candidate_matches(
    template: &[Statement],
    candidate: &[Statement],
    template_localizable: &FxHashSet<RcLocal>,
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
) -> bool {
    let Some(bindings) =
        block_alpha_bindings_with_locals(template, candidate, template_localizable)
    else {
        return false;
    };
    if template_localizable.is_empty() {
        return true;
    }
    let candidate_localizable = localizable_destinations(candidate, scope_declared, captured);
    template_localizable.iter().all(|pattern| {
        // A shallow template copy may share an Arc-backed child with its own
        // source occurrence. `alpha_arc_eq` deliberately avoids recursively
        // locking that same Arc, so an otherwise identity mapping can remain
        // implicit; in that case validate the pattern cell itself.
        candidate_localizable.contains(bindings.local_binding(pattern).unwrap_or(pattern))
    })
}

fn region_movable(stmts: &[Statement]) -> bool {
    stmts.iter().all(statement_movable)
}

fn collect_captured_locals(stmts: &[Statement], captured: &mut FxHashSet<RcLocal>) {
    for statement in stmts {
        for value in crate::deinline::stmt_rvalues(statement) {
            collect_captured_rvalue(value, captured);
        }
        match statement {
            Statement::If(node) => {
                collect_captured_locals(&node.then_block.lock().0, captured);
                collect_captured_locals(&node.else_block.lock().0, captured);
            }
            Statement::While(node) => collect_captured_locals(&node.block.lock().0, captured),
            Statement::Repeat(node) => collect_captured_locals(&node.block.lock().0, captured),
            Statement::NumericFor(node) => collect_captured_locals(&node.block.lock().0, captured),
            Statement::GenericFor(node) => collect_captured_locals(&node.block.lock().0, captured),
            _ => {}
        }
    }
}

fn collect_captured_rvalue(value: &RValue, captured: &mut FxHashSet<RcLocal>) {
    if let RValue::Closure(closure) = value {
        captured.extend(closure.upvalues.iter().map(|upvalue| match upvalue {
            Upvalue::Copy(local) | Upvalue::Ref(local) => local.clone(),
        }));
        return;
    }
    for child in value.rvalues() {
        collect_captured_rvalue(child, captured);
    }
}

/// External locals whose incoming value is never observed before the region
/// overwrites them.  They can be declared inside the synthesized helper rather
/// than captured, allowing SSA-renamed copies of the same dead result lane to
/// share one helper.
fn localizable_destinations(
    stmts: &[Statement],
    scope_declared: &FxHashSet<RcLocal>,
    captured: &FxHashSet<RcLocal>,
) -> FxHashSet<RcLocal> {
    let mut definitely_written = FxHashSet::default();
    let mut written_anywhere = FxHashSet::default();
    let mut read_before_write = FxHashSet::default();
    analyze_kills(
        stmts,
        &mut definitely_written,
        &mut written_anywhere,
        &mut read_before_write,
    );
    let mut declared = FxHashSet::default();
    collect_declared_locals(stmts, &mut declared);
    written_anywhere.retain(|local| {
        scope_declared.contains(local)
            && !captured.contains(local)
            && !declared.contains(local)
            && !read_before_write.contains(local)
    });
    written_anywhere
}

fn analyze_kills(
    stmts: &[Statement],
    definitely_written: &mut FxHashSet<RcLocal>,
    written_anywhere: &mut FxHashSet<RcLocal>,
    read_before_write: &mut FxHashSet<RcLocal>,
) {
    for statement in stmts {
        for value in crate::deinline::stmt_rvalues(statement) {
            for local in value.values_read() {
                if !definitely_written.contains(local) {
                    read_before_write.insert(local.clone());
                }
            }
        }

        match statement {
            Statement::Assign(assign) => {
                for left in &assign.left {
                    if let LValue::Local(local) = left {
                        written_anywhere.insert(local.clone());
                        definitely_written.insert(local.clone());
                    }
                }
            }
            Statement::If(node) => {
                let mut then_written = definitely_written.clone();
                let mut else_written = definitely_written.clone();
                analyze_kills(
                    &node.then_block.lock().0,
                    &mut then_written,
                    written_anywhere,
                    read_before_write,
                );
                analyze_kills(
                    &node.else_block.lock().0,
                    &mut else_written,
                    written_anywhere,
                    read_before_write,
                );
                then_written.retain(|local| else_written.contains(local));
                *definitely_written = then_written;
            }
            // These loops may execute zero times. Analyze their bodies for
            // unsafe reads, but do not propagate a body write to the exit.
            Statement::While(node) => {
                let mut body_written = definitely_written.clone();
                analyze_kills(
                    &node.block.lock().0,
                    &mut body_written,
                    written_anywhere,
                    read_before_write,
                );
            }
            Statement::NumericFor(node) => {
                let mut body_written = definitely_written.clone();
                body_written.insert(node.counter.clone());
                analyze_kills(
                    &node.block.lock().0,
                    &mut body_written,
                    written_anywhere,
                    read_before_write,
                );
            }
            Statement::GenericFor(node) => {
                let mut body_written = definitely_written.clone();
                body_written.extend(node.res_locals.iter().cloned());
                analyze_kills(
                    &node.block.lock().0,
                    &mut body_written,
                    written_anywhere,
                    read_before_write,
                );
            }
            // Conservatively treat Repeat's condition as an entry read (recorded
            // above); body writes therefore never justify localization across it.
            Statement::Repeat(node) => {
                let mut body_written = definitely_written.clone();
                analyze_kills(
                    &node.block.lock().0,
                    &mut body_written,
                    written_anywhere,
                    read_before_write,
                );
            }
            _ => {}
        }
    }
}

fn statement_movable(statement: &Statement) -> bool {
    if matches!(
        statement,
        Statement::Break(_)
            | Statement::Continue(_)
            | Statement::Goto(_)
            | Statement::Label(_)
            | Statement::Close(_)
            | Statement::NumForInit(_)
            | Statement::NumForNext(_)
            | Statement::GenericForInit(_)
            | Statement::GenericForNext(_)
            | Statement::Comment(_)
    ) {
        return false;
    }
    if crate::deinline::stmt_rvalues(statement)
        .into_iter()
        .any(|value| !rvalue_movable(value))
    {
        return false;
    }
    match statement {
        Statement::If(node) => {
            region_movable(&node.then_block.lock().0) && region_movable(&node.else_block.lock().0)
        }
        Statement::While(node) => region_movable(&node.block.lock().0),
        Statement::Repeat(node) => region_movable(&node.block.lock().0),
        Statement::NumericFor(node) => region_movable(&node.block.lock().0),
        Statement::GenericFor(node) => region_movable(&node.block.lock().0),
        _ => true,
    }
}

fn rvalue_movable(value: &RValue) -> bool {
    if matches!(
        value,
        RValue::Closure(_) | RValue::VarArg(_) | RValue::Select(Select::VarArg(_))
    ) {
        return false;
    }
    value.rvalues().into_iter().all(rvalue_movable)
}

fn helper_name(template: &[Statement]) -> String {
    for statement in template {
        if let Some(name) = find_first_child_name(statement) {
            return format!("find{name}");
        }
    }
    "deduplicatedTail".to_string()
}

fn find_first_child_name(statement: &Statement) -> Option<String> {
    for value in statement.rvalues() {
        if let Some(name) = find_first_child_name_rvalue(value) {
            return Some(name);
        }
    }
    match statement {
        Statement::If(node) => find_first_child_name_block(&node.then_block.lock().0)
            .or_else(|| find_first_child_name_block(&node.else_block.lock().0)),
        Statement::While(node) => find_first_child_name_block(&node.block.lock().0),
        Statement::Repeat(node) => find_first_child_name_block(&node.block.lock().0),
        Statement::NumericFor(node) => find_first_child_name_block(&node.block.lock().0),
        Statement::GenericFor(node) => find_first_child_name_block(&node.block.lock().0),
        _ => None,
    }
}

fn find_first_child_name_block(stmts: &[Statement]) -> Option<String> {
    stmts.iter().find_map(find_first_child_name)
}

fn find_first_child_name_rvalue(value: &RValue) -> Option<String> {
    if matches!(value, RValue::Literal(Literal::String(bytes)) if bytes == b"Template") {
        return Some("Template".to_string());
    }
    if let RValue::MethodCall(call) = value
        && call.method == "FindFirstChild"
        && let Some(RValue::Literal(Literal::String(bytes))) = call.arguments.first()
        && let Ok(text) = std::str::from_utf8(bytes)
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !text.is_empty()
    {
        let mut chars = text.chars();
        let first = chars.next()?.to_ascii_uppercase();
        return Some(format!("{first}{}", chars.as_str()));
    }
    value
        .rvalues()
        .into_iter()
        .find_map(find_first_child_name_rvalue)
}

fn unique_name(stmts: &[Statement], base: &str) -> String {
    let mut used = FxHashSet::default();
    collect_declared_locals(stmts, &mut used);
    let names: FxHashSet<String> = used
        .into_iter()
        .filter_map(|local| local.0 .0.lock().0.clone())
        .collect();
    if !names.contains(base) {
        return base.to_string();
    }
    for suffix in 2.. {
        let candidate = format!("{base}{suffix}");
        if !names.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn statement_tag(statement: &Statement) -> u8 {
    match statement {
        Statement::If(_) => 0,
        Statement::While(_) => 1,
        Statement::Repeat(_) => 2,
        Statement::NumericFor(_) => 3,
        Statement::GenericFor(_) => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GenericFor, If, Index, MethodCall};

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn terminal_search(frames: &RcLocal, item: RcLocal, result: &RcLocal) -> Vec<Statement> {
        vec![
            Statement::GenericFor(GenericFor::new(
                vec![item.clone()],
                vec![RValue::Local(frames.clone())],
                Block(vec![
                    Statement::Assign(Assign {
                        left: vec![LValue::Local(result.clone())],
                        right: vec![RValue::MethodCall(MethodCall::new(
                            RValue::Local(item),
                            "FindFirstChild".to_string(),
                            vec![RValue::Literal(Literal::from("Template"))],
                        ))],
                        prefix: false,
                        parallel: false,
                    }),
                    Statement::If(If::new(
                        RValue::Local(result.clone()),
                        Block(vec![Statement::Return(Return::new(vec![RValue::Local(
                            result.clone(),
                        )]))]),
                        Block::default(),
                    )),
                ]),
            )),
            Statement::Return(Return::new(vec![RValue::Literal(Literal::Nil)])),
        ]
    }

    #[test]
    fn synthesizes_large_strict_terminal_duplicates() {
        let frames = local("frames");
        let result = local("result");
        let mut arms = Vec::new();
        for index in 0..4 {
            arms.push(Statement::If(If::new(
                RValue::Literal(Literal::Boolean(index % 2 == 0)),
                Block(terminal_search(&frames, local("item"), &result)),
                Block::default(),
            )));
        }
        let mut body = Block(vec![
            Statement::Assign(Assign {
                left: vec![LValue::Local(frames)],
                right: vec![RValue::Table(crate::Table::default())],
                prefix: true,
                parallel: false,
            }),
            Statement::Assign(Assign {
                left: vec![LValue::Local(result.clone())],
                right: Vec::new(),
                prefix: true,
                parallel: false,
            }),
        ]);
        body.0.extend(arms);

        assert_eq!(synthesize_terminal_helpers(&mut body), 1);
        assert!(body.0.iter().any(|statement| matches!(statement,
            Statement::Assign(assign) if matches!(assign.right.as_slice(), [RValue::Closure(_)])
        )));
        let closure = body
            .0
            .iter()
            .find_map(|statement| match statement {
                Statement::Assign(assign) => match assign.right.as_slice() {
                    [RValue::Closure(closure)] => Some(closure),
                    _ => None,
                },
                _ => None,
            })
            .expect("synthesized helper declaration");
        let helper = closure.function.0.lock();
        let Statement::Assign(local_declaration) = &helper.body.0[0] else {
            panic!("localized helper result must be declared")
        };
        let [LValue::Local(helper_result)] = local_declaration.left.as_slice() else {
            panic!("expected one localized result")
        };
        assert_ne!(helper_result, &result);
    }

    #[test]
    fn refuses_different_live_free_local_identity() {
        let mut body = Block::default();
        for index in 0..4 {
            let different_frames = local(&format!("frames{index}"));
            let different_result = local(&format!("result{index}"));
            body.0.push(Statement::If(If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block(terminal_search(
                    &different_frames,
                    local("item"),
                    &different_result,
                )),
                Block::default(),
            )));
        }
        assert_eq!(synthesize_terminal_helpers(&mut body), 0);
    }

    #[test]
    fn candidate_mapped_destination_must_be_safe_too() {
        let frames = local("frames");
        let template_result = local("templateResult");
        let captured_result = local("capturedResult");
        let template = terminal_search(&frames, local("itemA"), &template_result);
        let candidate = terminal_search(&frames, local("itemB"), &captured_result);
        let scope_declared =
            FxHashSet::from_iter([frames, template_result.clone(), captured_result.clone()]);
        let captured = FxHashSet::from_iter([captured_result]);
        let template_localizable = FxHashSet::from_iter([template_result]);

        assert!(!candidate_matches(
            &template,
            &candidate,
            &template_localizable,
            &scope_declared,
            &captured,
        ));
    }

    #[test]
    fn indexed_lhs_closure_is_seen_by_capture_and_mobility_checks() {
        let captured_local = local("captured");
        let closure = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(captured_local.clone())],
        });
        let statement = Statement::Assign(Assign {
            left: vec![LValue::Index(Index::new(global("table"), closure))],
            right: vec![RValue::Literal(Literal::Boolean(true))],
            prefix: false,
            parallel: false,
        });
        let mut captured = FxHashSet::default();
        collect_captured_locals(std::slice::from_ref(&statement), &mut captured);

        assert!(captured.contains(&captured_local));
        assert!(!statement_movable(&statement));
    }

    #[test]
    fn counting_and_replacement_share_child_only_domain() {
        let frames = local("frames");
        let result = local("result");
        let template = terminal_search(&frames, local("item"), &result);
        let scope_declared = FxHashSet::from_iter([frames, result]);
        let captured = FxHashSet::default();
        let localizable = FxHashSet::default();

        assert_eq!(
            count_suffixes(
                &template,
                &template,
                &localizable,
                &scope_declared,
                &captured,
            ),
            0,
            "the scope root is not a replacement target"
        );
        assert_eq!(
            count_suffix_vec(
                &template,
                &template,
                &localizable,
                &scope_declared,
                &captured,
            ),
            1,
            "an explicitly owned child block is a replacement target"
        );
    }

    fn global(name: &str) -> RValue {
        RValue::Global(crate::Global::from(name))
    }
}
