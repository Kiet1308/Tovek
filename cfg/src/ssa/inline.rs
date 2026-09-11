use crate::function::Function;
use ast::{LocalRw, Reduce, SideEffects, Traverse};
use indexmap::IndexMap;
use itertools::{Either, Itertools};
use petgraph::visit::EdgeRef;
use rustc_hash::{FxHashMap, FxHashSet};

mod facts;

/// Relational reversal changes both operand positions and the operator, leaving
/// the VM's ordered comparison intact. Equality has no reversed operator: its
/// __eq call must retain the original argument order. A primitive literal on
/// either side rules out an equality metamethod; type hints do not.
fn can_reverse_comparison(operation: ast::BinaryOperation, candidate: &ast::RValue) -> bool {
    match operation {
        ast::BinaryOperation::LessThan
        | ast::BinaryOperation::LessThanOrEqual
        | ast::BinaryOperation::GreaterThan
        | ast::BinaryOperation::GreaterThanOrEqual => true,
        ast::BinaryOperation::Equal | ast::BinaryOperation::NotEqual => matches!(
            candidate,
            ast::RValue::Literal(ast::Literal::Nil | ast::Literal::Boolean(_)
                | ast::Literal::Number(_) | ast::Literal::String(_))
        ),
        _ => false,
    }
}

/// Whether moving an inline candidate *past* this already-visited rvalue could
/// reorder an observable event. This includes runtime errors (for example a
/// dynamic table key evaluating to nil), not only explicit side effects.
fn rvalue_blocks_reorder(rvalue: &ast::RValue) -> bool {
    match rvalue {
        // These constructs either execute conditionally or can invoke a
        // metamethod/raise, so moving an effect across them can change order.
        ast::RValue::Call(_)
        | ast::RValue::MethodCall(_)
        | ast::RValue::Select(ast::Select::Call(_) | ast::Select::MethodCall(_))
        | ast::RValue::Index(_)
        | ast::RValue::Binary(_)
        | ast::RValue::IfExpression(_) => true,
        ast::RValue::Unary(unary) => {
            matches!(
                unary.operation,
                ast::UnaryOperation::Length | ast::UnaryOperation::Negate
            ) || ast::is_observable(rvalue)
        }
        // A table with a computed key is a barrier even though its constructor
        // itself normally has no SideEffects implementation; `is_observable`
        // catches the possible nil/NaN key error. Literal-keyed tables remain
        // reorderable, preserving the useful inlining optimization.
        ast::RValue::Table(_) => ast::is_observable(rvalue),
        // A missing global can invoke the environment's __index, which may
        // mutate state or throw. In particular, fetching a callee must not
        // move ahead of an earlier effect in one of its arguments.
        ast::RValue::Global(_) => true,
        _ => ast::is_observable(rvalue),
    }
}

/// Returns whether `read` occurs in a subexpression that may not be evaluated
/// on every execution of `rvalue`. A side-effecting definition cannot be moved
/// into such a position: `local x = effect(); return flag and x` must not become
/// `return flag and effect()`, which skips the call when `flag` is false.
fn local_is_conditionally_evaluated(
    rvalue: &ast::RValue,
    read: &ast::RcLocal,
    conditional: bool,
) -> bool {
    match rvalue {
        ast::RValue::Local(local) => conditional && local == read,
        ast::RValue::Binary(binary)
            if matches!(binary.operation, ast::BinaryOperation::And | ast::BinaryOperation::Or) =>
        {
            local_is_conditionally_evaluated(&binary.left, read, conditional)
                || local_is_conditionally_evaluated(&binary.right, read, true)
        }
        ast::RValue::IfExpression(if_expression) => {
            local_is_conditionally_evaluated(&if_expression.condition, read, conditional)
                || local_is_conditionally_evaluated(&if_expression.then_value, read, true)
                || local_is_conditionally_evaluated(&if_expression.else_value, read, true)
        }
        _ => rvalue
            .rvalues()
            .into_iter()
            .any(|child| local_is_conditionally_evaluated(child, read, conditional)),
    }
}

/// A `game:GetService("X")` service handle or a `require(...)` module handle.
/// The SSA inliner refuses to fold these into their single use site so they
/// survive as named header locals — `local Players = game:GetService("Players")`,
/// `local AfkConfig = require(...)` — which is how the source is written and what
/// `name_locals` can give a meaningful name (GetService -> PascalCase service,
/// require -> module base name). Refusing to inline is always semantics-preserving:
/// the value is still computed once, read once, in the same position.
/// Never forward a table constructor into the BASE of an index write
/// (`local t = {}; t.k = v` -> `({}).k = v`): the write would land on a
/// temporary nobody can read, deleting the field (a dead `Handlers.Formatter =
/// function … end` table lost its closures this way). The
/// `t = {} t.a = 1` -> `t = { a = 1 }` fold reconstructs the constructor instead.
fn forwards_table_into_index_write(
    new_rvalue: &ast::RValue,
    use_stat: &ast::Statement,
    local: &ast::RcLocal,
) -> bool {
    matches!(new_rvalue, ast::RValue::Table(_))
        && use_stat.as_assign().is_some_and(|a| {
            a.left.iter().any(|l| {
                matches!(l, ast::LValue::Index(i)
                    if matches!(i.left.as_ref(), ast::RValue::Local(x) if x == local))
            })
        })
}

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
        if new_rvalue_has_side_effects
            && traversible
                .rvalues()
                .into_iter()
                .any(|rvalue| local_is_conditionally_evaluated(rvalue, read, false))
        {
            return false;
        }
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
                                }) if can_reverse_comparison(*operation, new_rvalue.as_ref().unwrap())
                                    && left.has_side_effects()
                                    && let ast::RValue::Local(local) = right.as_ref()
                                    && local == read =>
                                {
                                    *right = std::mem::replace(
                                        left,
                                        Box::new(new_rvalue.take().unwrap()),
                                    );
                                    *operation = match *operation {
                                        // Equality reaches this path only with a
                                        // primitive literal, excluding __eq.
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
        let mut fact_statistics = facts::Statistics::default();
        let node_indices = self.function.graph().node_indices().collect::<Vec<_>>();
        for node in node_indices {
            let block = self.function.block_mut(node).unwrap();
            let mut facts = facts::Cache::new(block.len(), self.local_to_group, self.upvalue_to_group);

            // TODO: rename values_read to locals_read
            let mut stat_to_values_read = Vec::with_capacity(block.len());
            for stat in &block.0 {
                stat_to_values_read.push(
                    stat.values_read()
                        .into_iter()
                        .filter(|&l| {
                            self.local_usages[l] == 1 && !self.upvalue_to_group.contains_key(l)
                                && (!l.has_source_binding() || ast::assignment_preserves_function_name(stat, l))
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
                    let statement_facts = facts.get(stat_index, &block[stat_index]);
                    if statement_facts.writes_upvalue {
                        allow_side_effects = false;
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
                    if statement_facts.read_groups.iter()
                        .any(|g| groups_written.contains(g))
                    {
                        // We are stepping OVER this statement without inlining it
                        // (it reads a group written by a later statement). If it has
                        // an observable effect, any still-earlier side-effecting def
                        // we go on to inline would hop PAST it and reorder effects
                        // (C9: `c1=A(); m=B(a); … return c1+m` inlined A() past B()).
                        // Close the side-effect window here, exactly as the
                        // fall-through path at the bottom of the loop does.
                        allow_side_effects &= !statement_facts.observable;
                        continue;
                    }

                    if let ast::Statement::Assign(assign) = &block[stat_index]
                        && let Ok(new_rvalue) = assign.right.iter().exactly_one()
                    {
                        let new_rvalue_has_side_effects = statement_facts.single_rhs_observable.unwrap();
                        if (!new_rvalue_has_side_effects || allow_side_effects)
                            && !is_service_or_require_handle(new_rvalue)
                        {
                            if let Ok(ast::LValue::Local(local)) = &assign.left.iter().exactly_one()
                                && !forwards_table_into_index_write(new_rvalue, &block[index], local)
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
                                    facts.invalidate(stat_index);
                                    facts.invalidate(index);
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
                                        if ast::is_observable(expr) {
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
                                    facts.invalidate(stat_index);
                                    facts.invalidate(index);
                                    continue 'w;
                                }
                            }
                        }
                    }
                    groups_written.extend(statement_facts.write_groups.iter().copied());
                    allow_side_effects &= !statement_facts.observable;
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
                        let statement_facts = facts.get(stat_index, &block[stat_index]);
                        if statement_facts.writes_upvalue {
                            index += 1;
                            continue 'w;
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
                        if statement_facts.read_groups.iter()
                            .any(|g| groups_written.contains(g))
                        {
                            continue;
                        }

                        if let ast::Statement::Assign(assign) = &block[stat_index]
                            && assign.right.len() == 1
                        {
                            let new_rvalue_has_side_effects = statement_facts.single_rhs_observable.unwrap();
                            if !new_rvalue_has_side_effects
                                && let Ok(ast::LValue::Local(local)) =
                                    &assign.left.iter().exactly_one()
                                && !local.has_source_binding()
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
                                    facts.invalidate(stat_index);
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
                        groups_written.extend(statement_facts.write_groups.iter().copied());
                    }
                    index += 1;
                }
            }
            fact_statistics.add(facts.statistics());
        }
        fact_statistics.record();
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

/// Index of the `local t = {...}` declaration that a SETLIST at `set_list_index`
/// can be folded into once the declaration is moved directly before it.
///
/// The move is only legal when no statement in between reads or writes `t`
/// (closure captures count as reads), and when every entry already in the
/// constructor is total-pure and reads no local written in between (moving the
/// constructor later must not change what those entries evaluate to).
fn movable_table_declaration(
    block: &ast::Block,
    set_list_index: usize,
    object_local: &ast::RcLocal,
) -> Option<usize> {
    let mut written_between: Vec<ast::RcLocal> = Vec::new();
    for j in (0..set_list_index).rev() {
        let statement = &block[j];
        if let Some(assign) = statement.as_assign()
            && table_constructor_local(assign).as_ref() == Some(object_local)
        {
            let table = assign.right[0].as_table().unwrap();
            let entries_movable = table.0.iter().all(|(key, value)| {
                key.iter().chain(std::iter::once(value)).all(|rvalue| {
                    ast::is_total_pure(rvalue)
                        && rvalue
                            .values_read()
                            .iter()
                            .all(|local| !written_between.contains(*local))
                })
            });
            return entries_movable.then_some(j);
        }
        if statement.values_read().contains(&object_local)
            || statement.values_written().contains(&object_local)
        {
            return None;
        }
        written_between.extend(statement.values_written().into_iter().cloned());
    }
    None
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
    upvalue_to_group: &IndexMap<ast::RcLocal, ast::RcLocal>,
) -> bool {
    let mut changed = false;
    let mut i = 0;
    while i < block.len() {
        let Some(object_local) = block[i].as_assign().and_then(table_constructor_local) else {
            i += 1;
            continue;
        };
        // A callback may observe the table through this cell before a later
        // field value is evaluated. Folding would postpone the cell assignment.
        if upvalue_to_group.contains_key(&object_local) {
            i += 1;
            continue;
        }

        let table_index = i;
        let initial_len = block[table_index].as_assign().unwrap().right[0]
            .as_table()
            .unwrap()
            .0
            .len();
        i += 1;
        while i < block.len() {
            let Some((key, value)) = block[i]
                .as_assign()
                .and_then(|assign| field_assignment_parts(assign, &object_local))
            else {
                break;
            };

            let table = block[table_index].as_assign().unwrap().right[0]
                .as_table()
                .unwrap();
            if table.0.last().is_some_and(|(key, value)| {
                key.is_none()
                    && matches!(
                        value,
                        ast::RValue::Call(_) | ast::RValue::MethodCall(_) | ast::RValue::VarArg(_)
                    )
            }) || !can_fold_table_field_assignment(key, value, &object_local)
            {
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
            // Replacing a nil placeholder moves this evaluation across the
            // rest of the constructor. Cross only total fields without
            // mutable-cell snapshots; otherwise append in the original order.
            match table
                .0
                .iter()
                .take(initial_len)
                .position(|(k, _)| k.as_ref() == Some(&new_key))
            {
                Some(p)
                    if matches!(&table.0[p].1, ast::RValue::Literal(ast::Literal::Nil))
                        && table.0[p..initial_len].iter().all(|(key, value)| {
                            key.as_ref().is_some_and(ast::is_total_table_key)
                                && ast::is_total_pure(value)
                                && !value
                                    .values_read()
                                    .iter()
                                    .any(|read| upvalue_to_group.contains_key(*read))
                        }) =>
                {
                    decrement_rvalue_usages(local_usages, &table.0[p].1);
                    table.0[p].1 = new_value;
                }
                Some(p)
                    if matches!(&table.0[p].1, ast::RValue::Literal(ast::Literal::Nil))
                        && ast::is_total_table_key(&new_key) =>
                {
                    table.0.remove(p);
                    table.0.push((Some(new_key), new_value));
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
                            // A dead NON-EMPTY table constructor is kept for fidelity:
                            // a config/enum table (`local ASSETS = { Image = "rbxassetid://…" }`)
                            // or a table of closures the source never used still
                            // carries strings, numbers and function bodies the reader
                            // wants (ROADMAP C5; oracle class `dropped-const-table`).
                            // Only the empty `{}` stays disposable.
                            let keep_const_table =
                                matches!(rvalue, ast::RValue::Table(t) if !t.0.is_empty());
                            if !keep_named_closure && !keep_can_raise && !keep_const_table {
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
            changed |= fold_table_constructor_field_assignments(
                block,
                &mut local_usages,
                upvalue_to_group,
            );

            // if the first statement is a set_list, we cant inline it anyway
            for i in 1..block.len() {
                if let ast::Statement::SetList(set_list) = &block[i] {
                    let object_local = set_list.object_local.clone();
                    if upvalue_to_group.contains_key(&object_local) {
                        continue;
                    }
                    // `local t = {}` may sit several statements above the SETLIST
                    // when the array items needed temporaries (nested table
                    // constructors, closures, calls). Allocating the empty table
                    // later is unobservable, so move the declaration next to the
                    // SETLIST when nothing in between references `t` and the
                    // constructor's own entries stay pure and unaffected.
                    if block[i - 1]
                        .as_assign()
                        .is_none_or(|assign| assign.left != [object_local.clone().into()])
                        && let Some(decl_index) = movable_table_declaration(block, i, &object_local)
                    {
                        let decl = block.remove(decl_index);
                        block.insert(i - 1, decl);
                        changed = true;
                    }
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
    use super::{
        fold_table_constructor_field_assignments, inline, local_is_conditionally_evaluated,
        rvalue_blocks_reorder,
    };
    use crate::function::Function;
    use ast::{
        Assign, Binary, Block, Global, Index, LValue, Literal, Local, RValue, RcLocal, Return,
        Statement, Table,
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
        fold_table_constructor_field_assignments(block, &mut FxHashMap::default(), &IndexMap::new())
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
    fn field_fold_preserves_effectful_suffix_order_and_captured_table_cell() {
        let config = local("config");
        let mut block = Block(vec![
            table_decl(&config),
            field_assign(
                &config,
                string("first"),
                ast::Call::new(global("first"), vec![]).into(),
            ),
        ]);
        block[0].as_assign_mut().unwrap().right[0] = Table(vec![
            (Some(string("first")), Literal::Nil.into()),
            (
                Some(string("second")),
                ast::Call::new(global("second"), vec![]).into(),
            ),
        ])
        .into();
        assert!(fold_fields(&mut block));
        let output = block.to_string();
        assert!(
            output.find("second()").unwrap() < output.find("first()").unwrap(),
            "{output}"
        );

        let mut captured = Block(vec![
            table_decl(&config),
            field_assign(
                &config,
                string("value"),
                ast::Call::new(global("observe"), vec![]).into(),
            ),
        ]);
        let before = captured.to_string();
        let protected = IndexMap::from_iter([(config.clone(), config.clone())]);
        assert!(!fold_table_constructor_field_assignments(
            &mut captured,
            &mut FxHashMap::default(),
            &protected
        ));
        assert_eq!(captured.to_string(), before);
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

    #[test]
    fn dynamic_table_constructor_blocks_effect_reordering() {
        let key = local("key");
        let table = RValue::Table(Table(vec![(
            Some(local_value(&key)),
            number(1.0),
        )]));
        assert!(
            rvalue_blocks_reorder(&table),
            "a nil/NaN-capable table key must stay before side effects"
        );
        assert!(rvalue_blocks_reorder(&RValue::Binary(Binary::new(
            local_value(&key),
            number(1.0),
            ast::BinaryOperation::Add,
        ))));
    }

    #[test]
    fn global_callee_lookup_stays_after_an_earlier_call() {
        let value = local("value");
        let mut block = inline_block(Block(vec![
            Assign::new(
                vec![LValue::Local(value.clone())],
                vec![ast::Call::new(global("fetch"), vec![]).into()],
            )
            .into(),
            ast::Call::new(global("sink"), vec![local_value(&value)]).into(),
        ]));
        remove_empty(&mut block);
        assert_eq!(block.len(), 2, "{block}");
        assert!(matches!(&block[0], Statement::Assign(_)), "{block}");
        assert!(matches!(&block[1], Statement::Call(call)
            if call.arguments == vec![local_value(&value)]), "{block}");
    }

    #[test]
    fn equality_inlining_preserves_metamethod_operand_order_despite_type_hints() {
        for operation in [ast::BinaryOperation::Equal, ast::BinaryOperation::NotEqual] {
            let right = local("right_value");
            right.0.lock().1 = Some("number".into());
            let lhs: RValue = ast::Call::new(global("left"), vec![]).into();
            let mut block = inline_block(Block(vec![
                Assign::new(vec![right.clone().into()], vec![ast::Call::new(global("right"), vec![]).into()]).into(),
                Return::new(vec![Binary::new(lhs.clone(), local_value(&right), operation).into()]).into(),
            ]));
            remove_empty(&mut block);
            assert_eq!(block.len(), 2, "{block}");
            let comparison = block[1].as_return().unwrap().values[0].as_binary().unwrap();
            assert_eq!(*comparison.left, lhs);
            assert_eq!(*comparison.right, local_value(&right));
            assert_eq!(comparison.operation, operation);
        }
    }

    #[test]
    fn comparison_reversal_retains_relational_and_primitive_equality_cases() {
        for (operation, candidate, reversed) in [
            (ast::BinaryOperation::LessThan, ast::Call::new(global("right"), vec![]).into(), ast::BinaryOperation::GreaterThan),
            (ast::BinaryOperation::Equal, number(7.0), ast::BinaryOperation::Equal),
        ] {
            let right = local("right_value");
            let lhs: RValue = ast::Call::new(global("left"), vec![]).into();
            let mut block = inline_block(Block(vec![
                Assign::new(vec![right.clone().into()], vec![candidate.clone()]).into(),
                Return::new(vec![Binary::new(lhs.clone(), local_value(&right), operation).into()]).into(),
            ]));
            remove_empty(&mut block);
            assert_eq!(block.len(), 1, "{block}");
            let comparison = block[0].as_return().unwrap().values[0].as_binary().unwrap();
            assert_eq!(*comparison.left, candidate);
            assert_eq!(*comparison.right, lhs);
            assert_eq!(comparison.operation, reversed);
        }
    }

    #[test]
    fn global_barrier_still_allows_total_values_and_local_callees() {
        for (callee, candidate) in [
            (global("sink"), number(7.0)),
            (
                local_value(&local("sink")),
                ast::Call::new(global("fetch"), vec![]).into(),
            ),
        ] {
            let value = local("value");
            let mut block = inline_block(Block(vec![
                Assign::new(vec![LValue::Local(value.clone())], vec![candidate]).into(),
                ast::Call::new(callee, vec![local_value(&value)]).into(),
            ]));
            remove_empty(&mut block);
            assert_eq!(block.len(), 1, "{block}");
            assert!(matches!(&block[0], Statement::Call(_)), "{block}");
        }
    }

    #[test]
    fn side_effecting_value_is_not_moved_into_short_circuit_rhs() {
        let flag = local("flag");
        let value = local("value");
        let expression = RValue::Binary(Binary::new(
            local_value(&flag),
            local_value(&value),
            ast::BinaryOperation::And,
        ));
        assert!(local_is_conditionally_evaluated(&expression, &value, false));
        assert!(!local_is_conditionally_evaluated(&expression, &flag, false));
    }
}

#[cfg(test)]
mod set_list_fold_through_tests {
    use super::movable_table_declaration;
    use ast::{Assign, Block, Call, Global, LValue, Literal, Local, RValue, RcLocal, SetList, Statement, Table};

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn table_decl(local: &RcLocal, entries: Vec<(Option<RValue>, RValue)>) -> Statement {
        let mut assign = Assign::new(
            vec![LValue::Local(local.clone())],
            vec![RValue::Table(Table(entries))],
        );
        assign.prefix = true;
        assign.into()
    }

    fn call_assign(target: &RcLocal, callee: &str, arguments: Vec<RValue>) -> Statement {
        let mut assign = Assign::new(
            vec![LValue::Local(target.clone())],
            vec![Call::new(RValue::Global(Global::from(callee)), arguments).into()],
        );
        assign.prefix = true;
        assign.into()
    }

    fn set_list(object: &RcLocal, values: Vec<RValue>, tail: Option<RValue>) -> Statement {
        SetList::new(object.clone(), 1, values, tail).into()
    }

    #[test]
    fn declaration_moves_past_unrelated_temporaries() {
        let t = local("t");
        let x = local("x");
        let y = local("y");
        let block = Block(vec![
            table_decl(&t, Vec::new()),
            call_assign(&x, "f", Vec::new()),
            call_assign(&y, "g", vec![RValue::Local(x.clone())]),
            set_list(&t, vec![RValue::Local(x.clone()), RValue::Local(y.clone())], None),
        ]);
        assert_eq!(movable_table_declaration(&block, 3, &t), Some(0));
    }

    #[test]
    fn declaration_stays_when_the_table_is_referenced_in_between() {
        let t = local("t");
        let x = local("x");
        let block = Block(vec![
            table_decl(&t, Vec::new()),
            call_assign(&x, "f", vec![RValue::Local(t.clone())]),
            set_list(&t, vec![RValue::Local(x.clone())], None),
        ]);
        assert_eq!(movable_table_declaration(&block, 2, &t), None);
    }

    #[test]
    fn declaration_stays_when_an_entry_reads_a_local_written_in_between() {
        let t = local("t");
        let x = local("x");
        let block = Block(vec![
            table_decl(
                &t,
                vec![(
                    Some(Literal::String(b"n".to_vec()).into()),
                    RValue::Local(x.clone()),
                )],
            ),
            call_assign(&x, "f", Vec::new()),
            set_list(&t, vec![RValue::Local(x.clone())], None),
        ]);
        assert_eq!(movable_table_declaration(&block, 2, &t), None);
    }

    #[test]
    fn pure_entries_move_with_the_declaration() {
        let t = local("t");
        let x = local("x");
        let block = Block(vec![
            table_decl(
                &t,
                vec![(
                    Some(Literal::String(b"n".to_vec()).into()),
                    Literal::Number(1.0).into(),
                )],
            ),
            call_assign(&x, "f", Vec::new()),
            set_list(&t, vec![RValue::Local(x.clone())], Some(Call::new(RValue::Global(Global::from("g")), Vec::new()).into())),
        ]);
        assert_eq!(movable_table_declaration(&block, 2, &t), Some(0));
    }

    #[test]
    fn unfolded_multret_tail_is_not_truncated() {
        let t = local("t");
        let x = local("x");
        let statement = set_list(
            &t,
            vec![RValue::Local(x.clone())],
            Some(Call::new(RValue::Global(Global::from("g")), Vec::new()).into()),
        );
        let text = statement.to_string();
        assert!(text.starts_with("do local _values = table.pack(x, g()); for _k = 1, _values.n do t[_k] = _values[_k] end end"), "{text}");
        let tail_only = set_list(&t, Vec::new(), Some(RValue::VarArg(ast::VarArg {})));
        assert_eq!(
            tail_only.to_string(),
            "do local _values = table.pack(...); for _k = 1, _values.n do t[_k] = _values[_k] end end"
        );
    }
}
