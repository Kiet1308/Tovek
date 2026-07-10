use crate::function::Function;
use ast::{LocalRw, Reduce, SideEffects, Traverse};
use indexmap::IndexMap;
use itertools::{Either, Itertools};
use petgraph::visit::EdgeRef;
use rustc_hash::{FxHashMap, FxHashSet};

/// Whether moving a side-effecting inline candidate *past* this already-visited
/// rvalue (in evaluation order) could reorder two observable effects.
///
/// Only function/method calls are treated as hard, un-reorderable effects: a call
/// may read or write arbitrary state, so a call must never hop over another call.
/// Global reads, table indexing and arithmetic are treated as reorderable — in
/// real (non-adversarial) code they read library tables/fields that the candidate
/// will not mutate. This is what lets `local v = lib.fn(expr)` collapse back into
/// one expression instead of leaving a `local vN = expr` trail in front of it.
fn rvalue_blocks_reorder(rvalue: &ast::RValue) -> bool {
    matches!(
        rvalue,
        ast::RValue::Call(_)
            | ast::RValue::MethodCall(_)
            | ast::RValue::Select(ast::Select::Call(_) | ast::Select::MethodCall(_))
    )
}

/// A `game:GetService("X")` service handle or a `require(...)` module handle.
/// The SSA inliner refuses to fold these into their single use site so they
/// survive as named header locals — `local Players = game:GetService("Players")`,
/// `local AfkConfig = require(...)` — which is how the source is written and what
/// `name_locals` can give a meaningful name (GetService -> PascalCase service,
/// require -> module base name). Refusing to inline is always semantics-preserving:
/// the value is still computed once, read once, in the same position.
fn is_service_or_require_handle(rvalue: &ast::RValue) -> bool {
    match rvalue {
        ast::RValue::MethodCall(method_call)
        | ast::RValue::Select(ast::Select::MethodCall(method_call)) => {
            method_call.method == "GetService"
                && matches!(
                    method_call.arguments.first(),
                    Some(ast::RValue::Literal(ast::Literal::String(_)))
                )
        }
        ast::RValue::Call(call) | ast::RValue::Select(ast::Select::Call(call)) => {
            matches!(
                &*call.value,
                ast::RValue::Global(global) if global.0.as_slice() == b"require"
            )
        }
        _ => false,
    }
}

struct TraverseSelf<'a, T: Traverse>(&'a mut T);

impl<'a> Traverse for TraverseSelf<'a, ast::RValue> {
    fn rvalues_mut(&mut self) -> Vec<&mut ast::RValue> {
        vec![self.0]
    }

    fn rvalues(&self) -> Vec<&ast::RValue> {
        vec![self.0]
    }
}

struct Inliner<'a> {
    function: &'a mut Function,
    local_to_group: &'a FxHashMap<ast::RcLocal, usize>,
    upvalue_to_group: &'a IndexMap<ast::RcLocal, ast::RcLocal>,
    local_usages: &'a mut FxHashMap<ast::RcLocal, usize>,
}

impl<'a> Inliner<'a> {
    fn new(
        function: &'a mut Function,
        local_to_group: &'a FxHashMap<ast::RcLocal, usize>,
        upvalue_to_group: &'a IndexMap<ast::RcLocal, ast::RcLocal>,
        local_usages: &'a mut FxHashMap<ast::RcLocal, usize>,
    ) -> Self {
        Self {
            function,
            local_to_group,
            upvalue_to_group,
            local_usages,
        }
    }

    fn try_inline(
        traversible: &mut impl Traverse,
        read: &ast::RcLocal,
        new_rvalue: &mut Option<ast::RValue>,
        new_rvalue_has_side_effects: bool,
    ) -> bool {
        traversible
            .traverse_values(&mut |p, v| {
                match p {
                    ast::PreOrPost::Pre => {
                        if let Either::Right(rvalue) = v {
                            match rvalue {
                                ast::RValue::Binary(ast::Binary {
                                    left,
                                    right,
                                    operation,
                                }) if operation.is_comparator()
                                    && left.has_side_effects()
                                    && let ast::RValue::Local(local) = right.as_ref()
                                    && local == read =>
                                {
                                    *right = std::mem::replace(
                                        left,
                                        Box::new(new_rvalue.take().unwrap()),
                                    );
                                    *operation = match *operation {
                                        // TODO: __eq metamethod?
                                        ast::BinaryOperation::Equal => ast::BinaryOperation::Equal,
                                        ast::BinaryOperation::NotEqual => {
                                            ast::BinaryOperation::NotEqual
                                        }
                                        ast::BinaryOperation::LessThanOrEqual => {
                                            ast::BinaryOperation::GreaterThanOrEqual
                                        }
                                        ast::BinaryOperation::GreaterThanOrEqual => {
                                            ast::BinaryOperation::LessThanOrEqual
                                        }
                                        ast::BinaryOperation::LessThan => {
                                            ast::BinaryOperation::GreaterThan
                                        }
                                        ast::BinaryOperation::GreaterThan => {
                                            ast::BinaryOperation::LessThan
                                        }
                                        _ => unreachable!(),
                                    };
                                    return Some(true);
                                }
                                _ => {}
                            }
                        }
                    }
                    ast::PreOrPost::Post => {
                        if let Either::Right(rvalue) = v {
                            match rvalue {
                                ast::RValue::Local(local) if local == read => {
                                    *rvalue = new_rvalue.take().unwrap();
                                    // success!
                                    return Some(true);
                                }
                                _ => {}
                            }
                            if new_rvalue_has_side_effects && rvalue_blocks_reorder(rvalue) {
                                // failure :(
                                return Some(false);
                            }
                        }
                    }
                }
                // keep searching
                None
            })
            .unwrap_or(false)
    }

    // TODO: dont clone rvalues
    // TODO: REFACTOR: move to ssa module?
    // TODO: inline into block arguments
    fn inline_rvalues(self) {
        let node_indices = self.function.graph().node_indices().collect::<Vec<_>>();
        for node in node_indices {
            let block = self.function.block_mut(node).unwrap();

            // TODO: rename values_read to locals_read
            let mut stat_to_values_read = Vec::with_capacity(block.len());
            for stat in &block.0 {
                stat_to_values_read.push(
                    stat.values_read()
                        .into_iter()
                        .filter(|&l| {
                            self.local_usages[l] == 1 && !self.upvalue_to_group.contains_key(l)
                        })
                        .cloned()
                        .map(Some)
                        .collect_vec(),
                );
            }

            // visit all statements that read at least one local with only one usage,
            // this is the statement we will inline into
            // then seek backwards from the previous statement to the start of the block
            // until we find a statement that assigns to a single-use local that
            // is used in the statement we are inlining into.
            // TODO: push multiple use local assignments forward to their first use
            let mut index = 0;
            'w: while index < block.len() {
                let mut groups_written = FxHashSet::default();
                let mut allow_side_effects = true;
                for stat_index in (0..index).rev() {
                    let mut values_read = stat_to_values_read[index]
                        .iter_mut()
                        .filter(|l| l.is_some())
                        .peekable();
                    if values_read.peek().is_none() {
                        index += 1;
                        continue 'w;
                    }
                    // we cant inline across upvalue writes because an inlining candidate with side effects,
                    // for ex. a non-local function call, might access the upvalue
                    for value_written in block[stat_index].values_written() {
                        if self.upvalue_to_group.contains_key(value_written) {
                            // TODO: set allow_side_effects to false instead
                            allow_side_effects = false;
                        }
                    }

                    /*
                    -- we dont want to inline `tostring(a)` into `print(b)`
                    local print = print
                    local a = 1
                    while true do
                        local b = tostring(a)
                        a = 1
                        print(b)
                    end
                    */
                    if block[stat_index]
                        .values_read()
                        .into_iter()
                        .filter_map(|l| self.local_to_group.get(l))
                        .any(|g| groups_written.contains(g))
                    {
                        // We are stepping OVER this statement without inlining it
                        // (it reads a group written by a later statement). If it has
                        // an observable effect, any still-earlier side-effecting def
                        // we go on to inline would hop PAST it and reorder effects
                        // (C9: `c1=A(); m=B(a); … return c1+m` inlined A() past B()).
                        // Close the side-effect window here, exactly as the
                        // fall-through path at the bottom of the loop does.
                        allow_side_effects &= !block[stat_index].has_side_effects();
                        continue;
                    }

                    if let ast::Statement::Assign(assign) = &block[stat_index]
                        && let Ok(new_rvalue) = assign.right.iter().exactly_one()
                    {
                        let new_rvalue_has_side_effects = new_rvalue.has_side_effects()
                            || new_rvalue
                                .values_read()
                                .iter()
                                .any(|v| self.upvalue_to_group.contains_key(*v));
                        if (!new_rvalue_has_side_effects || allow_side_effects)
                            && !is_service_or_require_handle(new_rvalue)
                        {
                            if let Ok(ast::LValue::Local(local)) = &assign.left.iter().exactly_one()
                                && let Some(read) = stat_to_values_read[index]
                                    .iter_mut()
                                    .find(|l| l.as_ref() == Some(local))
                            {
                                let mut new_rvalue = Some(
                                    block[stat_index]
                                        .as_assign_mut()
                                        .unwrap()
                                        .right
                                        .pop()
                                        .unwrap(),
                                );
                                if Self::try_inline(
                                    &mut block[index],
                                    read.as_ref().unwrap(),
                                    &mut new_rvalue,
                                    new_rvalue_has_side_effects,
                                ) {
                                    assert!(new_rvalue.is_none());

                                    // TODO: PERF: this is probably inefficient
                                    for rvalue in block[index].rvalues_mut() {
                                        *rvalue =
                                            std::mem::replace(rvalue, ast::Literal::Nil.into())
                                                .reduce();
                                    }

                                    // TODO: PERF: remove `local_usages[l] == 1` filter in stat_to_values_read
                                    // and use stat_to_values_read here
                                    for local in block[stat_index].values_read() {
                                        let local_usage_count =
                                            self.local_usages.get_mut(local).unwrap();
                                        *local_usage_count = local_usage_count.saturating_sub(1);
                                    }
                                    // we dont need to update local usages because tracking usages for a local
                                    // with no declarations serves no purpose
                                    block[stat_index] = ast::Empty {}.into();
                                    *read = None;
                                    continue 'w;
                                } else {
                                    block[stat_index]
                                        .as_assign_mut()
                                        .unwrap()
                                        .right
                                        .push(new_rvalue.unwrap());
                                }
                            } else if let Some(generic_for_init) =
                                block[index].as_generic_for_init()
                                && generic_for_init
                                    .0
                                    .right
                                    .iter()
                                    .rev()
                                    .map_while(|r| r.as_local())
                                    .eq_by(assign.left.iter().rev(), |a, b| Some(a) == b.as_local())
                                && assign.left.iter().all(|l| {
                                    l.as_local().is_some_and(|l| {
                                        stat_to_values_read[index]
                                            .iter_mut()
                                            .any(|r| r.as_ref() == Some(l))
                                    })
                                })
                            {
                                let start_index =
                                    generic_for_init.0.right.len() - assign.left.len();
                                let has_leading_side_effects = || {
                                    let mut leading_side_effects = false;
                                    for expr in generic_for_init.0.right.iter().take(start_index) {
                                        if expr.has_side_effects() {
                                            leading_side_effects = true;
                                            break;
                                        }
                                    }
                                    leading_side_effects
                                };

                                if !new_rvalue_has_side_effects || !has_leading_side_effects() {
                                    let new_rvalue = block[stat_index]
                                        .as_assign_mut()
                                        .unwrap()
                                        .right
                                        .pop()
                                        .unwrap();

                                    let generic_for_init =
                                        block[index].as_generic_for_init_mut().unwrap();
                                    let old_locals = generic_for_init
                                        .0
                                        .right
                                        .drain(start_index..)
                                        .map(|r| r.as_local().unwrap().clone())
                                        .collect_vec();
                                    generic_for_init.0.right.push(new_rvalue);

                                    // TODO: PERF: remove `local_usages[l] == 1` filter in stat_to_values_read
                                    // and use stat_to_values_read here
                                    for local in block[stat_index].values_read() {
                                        let local_usage_count =
                                            self.local_usages.get_mut(local).unwrap();
                                        *local_usage_count = local_usage_count.saturating_sub(1);
                                    }
                                    // we dont need to update local usages because tracking usages for a local
                                    // with no declarations serves no purpose
                                    block[stat_index] = ast::Empty {}.into();
                                    for old_local in old_locals {
                                        *stat_to_values_read[index]
                                            .iter_mut()
                                            .find(|l| l.as_ref() == Some(&old_local))
                                            .unwrap() = None;
                                    }
                                    continue 'w;
                                }
                            }
                        }
                    }
                    groups_written.extend(
                        block[stat_index]
                            .values_written()
                            .into_iter()
                            .filter_map(|l| self.local_to_group.get(l))
                            .cloned(),
                    );
                    allow_side_effects &= !block[stat_index].has_side_effects();
                }
                index += 1;
            }
            // we cant inline anything with side effects or anything that depends on other params
            // because block params are executed in parallel.
            for edge in self.function.edges(node).map(|e| e.id()).collect_vec() {
                // TODO: rename values_read to locals_read
                let mut arg_to_values_read = self
                    .function
                    .graph()
                    .edge_weight(edge)
                    .unwrap()
                    .arguments
                    .iter()
                    .map(|(_, a)| {
                        a.values_read()
                            .into_iter()
                            .filter(|&l| {
                                self.local_usages[l] == 1 && !self.upvalue_to_group.contains_key(l)
                            })
                            .cloned()
                            .map(Some)
                            .collect_vec()
                    })
                    .collect_vec();

                let mut index = 0;
                'w: while index < arg_to_values_read.len() {
                    let mut groups_written = FxHashSet::default();
                    for stat_index in (0..self.function.block(node).unwrap().len()).rev() {
                        let mut values_read = arg_to_values_read[index]
                            .iter_mut()
                            .filter(|l| l.is_some())
                            .peekable();
                        if values_read.peek().is_none() {
                            index += 1;
                            continue 'w;
                        }
                        let block = self.function.block_mut(node).unwrap();
                        // we cant inline across upvalue writes because an inlining candidate with side effects,
                        // for ex. a non-local function call, might access the upvalue
                        for value_written in block[stat_index].values_written() {
                            if self.upvalue_to_group.contains_key(value_written) {
                                // TODO: set allow_side_effects to false instead
                                index += 1;
                                continue 'w;
                            }
                        }

                        /*
                        -- we dont want to inline `tostring(a)` into `print(b)`
                        local print = print
                        local a = 1
                        while true do
                            local b = tostring(a)
                            a = 1
                            print(b)
                        end
                        */
                        if block[stat_index]
                            .values_read()
                            .into_iter()
                            .filter_map(|l| self.local_to_group.get(l))
                            .any(|g| groups_written.contains(g))
                        {
                            continue;
                        }

                        if let ast::Statement::Assign(assign) = &block[stat_index]
                            && let Ok(new_rvalue) = assign.right.iter().exactly_one()
                        {
                            let new_rvalue_has_side_effects = new_rvalue.has_side_effects()
                                || new_rvalue
                                    .values_read()
                                    .iter()
                                    .any(|v| self.upvalue_to_group.contains_key(*v));
                            if !new_rvalue_has_side_effects
                                && let Ok(ast::LValue::Local(local)) =
                                    &assign.left.iter().exactly_one()
                                && let Some(read) = arg_to_values_read[index]
                                    .iter_mut()
                                    .find(|l| l.as_ref() == Some(local))
                            {
                                let mut new_rvalue = Some(
                                    block[stat_index]
                                        .as_assign_mut()
                                        .unwrap()
                                        .right
                                        .pop()
                                        .unwrap(),
                                );
                                if Self::try_inline(
                                    &mut TraverseSelf(
                                        &mut self
                                            .function
                                            .graph_mut()
                                            .edge_weight_mut(edge)
                                            .unwrap()
                                            .arguments[index]
                                            .1,
                                    ),
                                    read.as_ref().unwrap(),
                                    &mut new_rvalue,
                                    new_rvalue_has_side_effects,
                                ) {
                                    assert!(new_rvalue.is_none());
                                    let block = self.function.block_mut(node).unwrap();

                                    // TODO: PERF: remove `local_usages[l] == 1` filter in stat_to_values_read
                                    // and use stat_to_values_read here
                                    for local in block[stat_index].values_read() {
                                        let local_usage_count =
                                            self.local_usages.get_mut(local).unwrap();
                                        *local_usage_count = local_usage_count.saturating_sub(1);
                                    }
                                    // we dont need to update local usages because tracking usages for a local
                                    // with no declarations serves no purpose

                                    block[stat_index] = ast::Empty {}.into();
                                    *read = None;
                                    continue 'w;
                                } else {
                                    let block = self.function.block_mut(node).unwrap();

                                    block[stat_index]
                                        .as_assign_mut()
                                        .unwrap()
                                        .right
                                        .push(new_rvalue.unwrap());
                                }
                            }
                        }
                        let block = self.function.block(node).unwrap();

                        groups_written.extend(
                            block[stat_index]
                                .values_written()
                                .into_iter()
                                .filter_map(|l| self.local_to_group.get(l))
                                .cloned(),
                        );
                    }
                    index += 1;
                }
            }
        }
    }
}

fn rvalue_reads_local(rvalue: &ast::RValue, local: &ast::RcLocal) -> bool {
    rvalue.values_read().into_iter().any(|read| read == local)
}

fn decrement_local_usage(local_usages: &mut FxHashMap<ast::RcLocal, usize>, local: &ast::RcLocal) {
    if let Some(usage) = local_usages.get_mut(local) {
        *usage = usage.saturating_sub(1);
    }
}

fn decrement_rvalue_usages(
    local_usages: &mut FxHashMap<ast::RcLocal, usize>,
    rvalue: &ast::RValue,
) {
    for local in rvalue.values_read() {
        decrement_local_usage(local_usages, local);
    }
}

fn table_constructor_local(assign: &ast::Assign) -> Option<ast::RcLocal> {
    if assign.left.len() == 1
        && assign.right.len() == 1
        && assign.right[0].as_table().is_some()
        && let ast::LValue::Local(object_local) = &assign.left[0]
    {
        Some(object_local.clone())
    } else {
        None
    }
}

fn field_assignment_parts<'a>(
    assign: &'a ast::Assign,
    object_local: &ast::RcLocal,
) -> Option<(&'a ast::RValue, &'a ast::RValue)> {
    if assign.left.len() == 1
        && assign.right.len() == 1
        && let ast::LValue::Index(ast::Index {
            left: box ast::RValue::Local(local),
            right,
        }) = &assign.left[0]
        && local == object_local
    {
        Some((right, &assign.right[0]))
    } else {
        None
    }
}

fn can_fold_table_field_assignment(
    key: &ast::RValue,
    value: &ast::RValue,
    object_local: &ast::RcLocal,
) -> bool {
    !key.has_side_effects()
        && !rvalue_reads_local(key, object_local)
        && !rvalue_reads_local(value, object_local)
}

fn fold_table_constructor_field_assignments(
    block: &mut ast::Block,
    local_usages: &mut FxHashMap<ast::RcLocal, usize>,
) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < block.len() {
        let Some(object_local) = block[i].as_assign().and_then(table_constructor_local) else {
            i += 1;
            continue;
        };

        let table_index = i;
        i += 1;
        while i < block.len() {
            let Some((key, value)) = block[i]
                .as_assign()
                .and_then(|assign| field_assignment_parts(assign, &object_local))
            else {
                break;
            };

            if !can_fold_table_field_assignment(key, value, &object_local) {
                break;
            }

            decrement_local_usage(local_usages, &object_local);
            let field_assign = std::mem::replace(&mut block[i], ast::Empty {}.into())
                .into_assign()
                .unwrap();
            let new_key = Box::into_inner(
                field_assign
                    .left
                    .into_iter()
                    .next()
                    .unwrap()
                    .into_index()
                    .unwrap()
                    .right,
            );
            let new_value = field_assign.right.into_iter().next().unwrap();
            let table = block[table_index].as_assign_mut().unwrap().right[0]
                .as_table_mut()
                .unwrap();
            // Overwrite a `nil`/literal placeholder key (from a DUPTABLE
            // template) in place to preserve template key order and avoid
            // emitting a duplicate key. Only overwrite when the existing
            // value has no side effects; otherwise fall back to pushing so we
            // never drop a side-effectful initializer.
            match table
                .0
                .iter()
                .position(|(k, _)| k.as_ref() == Some(&new_key))
            {
                Some(p) if !table.0[p].1.has_side_effects() => {
                    decrement_rvalue_usages(local_usages, &table.0[p].1);
                    table.0[p].1 = new_value;
                }
                _ => {
                    table.0.push((Some(new_key), new_value));
                }
            }
            changed = true;
            i += 1;
        }
    }

    changed
}

pub fn inline(
    function: &mut Function,
    local_to_group: &FxHashMap<ast::RcLocal, usize>,
    upvalue_to_group: &IndexMap<ast::RcLocal, ast::RcLocal>,
) {
    let mut local_usages = FxHashMap::default();
    for node in function.graph().node_indices() {
        for read in function.values_read(node) {
            *local_usages.entry(read.clone()).or_insert(0usize) += 1;
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        Inliner::new(
            function,
            local_to_group,
            upvalue_to_group,
            &mut local_usages,
        )
        .inline_rvalues();

        // remove unused locals
        for block in function.blocks_mut() {
            for stat_index in 0..block.len() {
                if let ast::Statement::Assign(assign) = &block[stat_index]
                    && assign.left.len() == 1
                    && assign.right.len() == 1
                    && let ast::LValue::Local(local) = &assign.left[0]
                {
                    let rvalue = &assign.right[0];
                    let has_side_effects = rvalue.has_side_effects();
                    // TODO: REFACTOR: is_some_and
                    if !upvalue_to_group.contains_key(local)
                        && local_usages.get(local).map_or(true, |&u| u == 0)
                    {
                        if has_side_effects {
                            // TODO: PERF: dont clone
                            let new_stat = match rvalue {
                                ast::RValue::Call(call)
                                | ast::RValue::Select(ast::Select::Call(call)) => {
                                    Some(call.clone().into())
                                }
                                ast::RValue::MethodCall(method_call)
                                | ast::RValue::Select(ast::Select::MethodCall(method_call)) => {
                                    Some(method_call.clone().into())
                                }
                                _ => None,
                            };
                            if let Some(new_stat) = new_stat {
                                block[stat_index] = new_stat;
                                changed = true;
                            }
                        } else {
                            // Preserve a closure bound to a *named* local function even
                            // when its only call sites were inlined away by the Luau -O2
                            // compiler (leaving the binding unused). We recover it as a
                            // marked `local function` definition + reconstructed calls
                            // instead of deleting it. Anonymous dead closures are still
                            // removed as before.
                            let keep_named_closure = matches!(
                                rvalue,
                                ast::RValue::Closure(c) if c.function.lock().name.is_some()
                            );
                            // An unused binding whose RHS can RAISE must NOT be deleted:
                            // evaluating e.g. `a < b` (type error), `t.x` (index nil) or
                            // `#x` is observable, even though `has_side_effects` reports it
                            // pure (it is modelled pure only so single-use temps can inline
                            // back). Deleting it would silently swallow that runtime error
                            // (bug C11). This is safe — a single-use def that was inlined is
                            // already emptied by the inliner above (it moves the expression
                            // to the use site, where the raise still occurs), so a def that
                            // still reaches here with zero uses is genuinely never evaluated
                            // elsewhere; keeping it does not double-evaluate.
                            let keep_can_raise = !ast::is_total_pure(rvalue);
                            if !keep_named_closure && !keep_can_raise {
                                block[stat_index] = ast::Empty {}.into();
                                changed = true;
                            }
                        }
                    }
                }
            }
        }

        for block in function.blocks_mut() {
            // we check block.ast.len() elsewhere and do `i - ` here and elsewhere so we need to get rid of empty statements
            // TODO: fix ^
            block.retain(|s| s.as_empty().is_none());

            // `t = {} t.a = 1` -> `t = { a = 1 }`
            changed |= fold_table_constructor_field_assignments(block, &mut local_usages);

            // if the first statement is a set_list, we cant inline it anyway
            for i in 1..block.len() {
                if let ast::Statement::SetList(set_list) = &block[i] {
                    let object_local = set_list.object_local.clone();
                    if let Some(assign) = block[i - 1].as_assign_mut()
                        && assign.left == [object_local.into()]
                    {
                        let set_list = std::mem::replace(&mut block[i], ast::Empty {}.into())
                            .into_set_list()
                            .unwrap();
                        *local_usages.get_mut(&set_list.object_local).unwrap() -= 1;
                        let assign = block.get_mut(i - 1).unwrap().as_assign_mut().unwrap();
                        let table = assign.right[0].as_table_mut().unwrap();
                        assert!(
                            table.0.iter().filter(|(k, _)| k.is_none()).count()
                                == set_list.index - 1
                        );
                        for value in set_list.values {
                            table.0.push((None, value));
                        }
                        // table already has tail?
                        // TODO: REFACTOR: is_some_and
                        assert!(!table.0.last().map_or(false, |(k, v)| k.is_none()
                            && matches!(
                                v,
                                ast::RValue::VarArg(_)
                                    | ast::RValue::Call(_)
                                    | ast::RValue::MethodCall(_)
                            )));
                        if let Some(tail) = set_list.tail {
                            table.0.push((None, tail));
                        }
                        changed = true;
                    }
                    // todo: only inline in changed blocks
                    //cfg::dot::render_to(function, &mut std::io::stdout());
                    //break 'outer;
                }
            }
        }
    }
    // we check block.ast.len() elsewhere and do `i - ` here and elsewhere so we need to get rid of empty statements
    // TODO: fix ^
    for block in function.blocks_mut() {
        block.retain(|s| s.as_empty().is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::{fold_table_constructor_field_assignments, inline};
    use crate::function::Function;
    use ast::{
        Assign, Block, Global, Index, LValue, Literal, Local, RValue, RcLocal, Return, Statement,
        Table,
    };
    use indexmap::IndexMap;
    use rustc_hash::FxHashMap;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn local_value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn string(value: &str) -> RValue {
        Literal::String(value.as_bytes().to_vec()).into()
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn number(value: f64) -> RValue {
        Literal::Number(value).into()
    }

    fn boolean(value: bool) -> RValue {
        Literal::Boolean(value).into()
    }

    fn table_decl(local: &RcLocal) -> Statement {
        let mut assign = Assign::new(
            vec![LValue::Local(local.clone())],
            vec![RValue::Table(Table::default())],
        );
        assign.prefix = true;
        assign.into()
    }

    fn field_assign(object: &RcLocal, key: RValue, value: RValue) -> Statement {
        Assign::new(
            vec![Index::new(local_value(object), key).into()],
            vec![value],
        )
        .into()
    }

    fn return_local(local: &RcLocal) -> Statement {
        Return::new(vec![local_value(local)]).into()
    }

    fn remove_empty(block: &mut Block) {
        block.retain(|statement| statement.as_empty().is_none());
    }

    fn first_table(block: &Block) -> &Table {
        block[0].as_assign().unwrap().right[0].as_table().unwrap()
    }

    fn fold_fields(block: &mut Block) -> bool {
        fold_table_constructor_field_assignments(block, &mut FxHashMap::default())
    }

    fn inline_block(block: Block) -> Block {
        let mut function = Function::new(0);
        let entry = function.new_block();
        *function.block_mut(entry).unwrap() = block;
        function.set_entry(entry);

        inline(&mut function, &FxHashMap::default(), &IndexMap::new());

        function.block(entry).unwrap().clone()
    }

    #[test]
    fn folds_consecutive_field_assignments_into_constructor() {
        let config = local("config");
        let mut block = Block(vec![
            table_decl(&config),
            field_assign(&config, string("Enabled"), boolean(true)),
            field_assign(&config, string("Range"), number(20.0)),
            return_local(&config),
        ]);

        assert!(fold_fields(&mut block));
        remove_empty(&mut block);

        assert_eq!(
            block.to_string(),
            "local config = {\n\tEnabled = true,\n\tRange = 20\n}\nreturn config"
        );
    }

    #[test]
    fn preserves_order_and_dynamic_local_keys() {
        let config = local("config");
        let key = local("key");
        let mut block = Block(vec![
            table_decl(&config),
            field_assign(&config, string("Enabled"), boolean(true)),
            field_assign(&config, local_value(&key), number(20.0)),
            return_local(&config),
        ]);

        assert!(fold_fields(&mut block));
        remove_empty(&mut block);

        let table = first_table(&block);
        assert_eq!(
            table.0,
            vec![
                (Some(string("Enabled")), boolean(true)),
                (Some(local_value(&key)), number(20.0)),
            ]
        );
    }

    #[test]
    fn stops_at_non_consecutive_statement() {
        let config = local("config");
        let other = local("other");
        let mut block = Block(vec![
            table_decl(&config),
            field_assign(&config, string("Enabled"), boolean(true)),
            Assign::new(vec![LValue::Local(other.clone())], vec![number(1.0)]).into(),
            field_assign(&config, string("Range"), number(20.0)),
            return_local(&config),
        ]);

        assert!(fold_fields(&mut block));
        remove_empty(&mut block);

        assert_eq!(
            first_table(&block).0,
            vec![(Some(string("Enabled")), boolean(true))]
        );
        assert!(matches!(&block[2], Statement::Assign(assign)
            if matches!(&assign.left[0], LValue::Index(index)
                if index.left.as_ref() == &local_value(&config)
                    && index.right.as_ref() == &string("Range"))));
    }

    #[test]
    fn does_not_fold_key_that_reads_constructed_table() {
        let config = local("config");
        let self_key = Index::new(local_value(&config), string("Name")).into();
        let mut block = Block(vec![
            table_decl(&config),
            field_assign(&config, self_key, number(1.0)),
            return_local(&config),
        ]);

        assert!(!fold_fields(&mut block));
        assert!(first_table(&block).0.is_empty());
        assert!(matches!(&block[1], Statement::Assign(_)));
    }

    #[test]
    fn does_not_fold_value_that_reads_constructed_table() {
        let config = local("config");
        let mut block = Block(vec![
            table_decl(&config),
            field_assign(&config, string("Self"), local_value(&config)),
            return_local(&config),
        ]);

        assert!(!fold_fields(&mut block));
        assert!(first_table(&block).0.is_empty());
        assert!(matches!(&block[1], Statement::Assign(_)));
    }

    #[test]
    fn does_not_fold_side_effectful_key() {
        let config = local("config");
        let mut block = Block(vec![
            table_decl(&config),
            field_assign(&config, global("dynamicKey"), number(1.0)),
            return_local(&config),
        ]);

        assert!(!fold_fields(&mut block));
        assert!(first_table(&block).0.is_empty());
        assert!(matches!(&block[1], Statement::Assign(_)));
    }

    #[test]
    fn full_inline_updates_usage_after_removed_field_assignment() {
        let config = local("config");
        let block = inline_block(Block(vec![
            Assign::new(
                vec![LValue::Local(config.clone())],
                vec![RValue::Table(Table::default())],
            )
            .into(),
            field_assign(&config, string("Enabled"), boolean(true)),
            return_local(&config),
        ]));

        assert_eq!(block.to_string(), "return {\n\tEnabled = true\n}");
    }
}
