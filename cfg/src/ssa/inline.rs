use crate::function::Function;
use ast::{LocalRw, Reduce, SideEffects, Traverse};
use ast::FxIndexMap as IndexMap;
use itertools::{Either, Itertools};
use petgraph::visit::EdgeRef;
use rustc_hash::{FxHashMap, FxHashSet};

mod facts;
mod schedule;

/// How many times each local is read, in dense slots (see [`ast::dense`]).
/// A local that is never read counts zero.
#[derive(Default)]
pub(crate) struct Usages {
    index: ast::dense::LocalIndex,
    counts: ast::dense::LocalVec<usize>,
}

impl Usages {
    fn census(function: &Function) -> Self {
        let mut usages = Self { index: function.local_index(), counts: ast::dense::LocalVec::new(0) };
        for node in function.graph().node_indices() {
            let mut record = |read: &ast::RcLocal| {
                *usages.counts.get_mut(usages.index.slot(read)) += 1;
                true
            };
            for statement in function.block(node).unwrap().iter() {
                statement.visit_local_reads(&mut record);
            }
            for edge in function.edges(node) {
                for (_, argument) in &edge.weight().arguments {
                    argument.visit_local_reads(&mut record);
                }
            }
        }
        usages
    }

    #[inline]
    fn get(&self, local: &ast::RcLocal) -> usize {
        self.index.find(local).map_or(0, |slot| *self.counts.get(slot))
    }

    /// Remove one read; false when the count was already zero.
    #[inline]
    fn decrement(&mut self, local: &ast::RcLocal) -> bool {
        let slot = self.index.slot(local);
        let count = self.counts.get_mut(slot);
        let previous = *count;
        *count = previous.saturating_sub(1);
        *count != previous
    }

    #[cfg(test)]
    fn counts(&self) -> Vec<usize> {
        self.counts.values().copied().collect()
    }
}

/// Candidate definitions cannot move during one block visit: successful
/// substitution only empties a producer or rewrites a consumer's expressions.
/// Remember the first assignment that could produce each local, including
/// multi-destination result packs. An obsolete entry after emptying is merely
/// conservative. This also preserves behavior for hand-built, non-SSA blocks
/// containing more than one definition of the same local.
struct ProducerIndex(FxHashMap<u64, usize>);

impl ProducerIndex {
    fn new(block: &ast::Block) -> Self {
        let mut definitions = FxHashMap::default();
        for (index, statement) in block.iter().enumerate() {
            if let ast::Statement::Assign(assign) = statement
                && assign.right.len() == 1
            {
                for local in assign.left.iter().filter_map(|left| left.as_local()) {
                    definitions.entry(local.stable_id()).or_insert(index);
                }
            }
        }
        Self(definitions)
    }

    fn first(&self, reads: &[Option<ast::RcLocal>], before: usize) -> Option<usize> {
        #[cfg(test)]
        if tests::REFERENCE_SCAN.with(std::cell::Cell::get) {
            return (before != 0).then_some(0);
        }
        reads.iter().flatten()
            .filter_map(|local| self.0.get(&local.stable_id()).copied())
            .filter(|&index| index < before)
            .min()
    }
}

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

/// Whether evaluating `rvalue` later could write a captured cell. A builtin
/// or a fixed library member (`error`, `debug.traceback`) the chunk cannot
/// have replaced is fetched without running Lua code, so it cannot; a script
/// table's field (`t.x`) may run `__index`, and any call, operator or other
/// index might.
fn may_write_capture_when_moved(rvalue: &ast::RValue, globals: &ast::ChunkGlobals) -> bool {
    !globals.fixed_library_import(rvalue) && ast::effects::may_write_capture(rvalue)
}

/// A global or a constant-key field chain on one (`table.insert`): what
/// GETIMPORT fetches.
fn is_import_path(rvalue: &ast::RValue) -> bool {
    match rvalue {
        ast::RValue::Global(_) => true,
        ast::RValue::Index(index) => {
            matches!(index.right.as_ref(), ast::RValue::Literal(ast::Literal::String(_)))
                && is_import_path(&index.left)
        }
        _ => false,
    }
}

/// Every node of the import-path callees of `Call`s lifted from a FASTCALL or
/// FASTPCALL fallback. The bytecode fetches such a callee after evaluating the
/// arguments, so no part of it is an ordering barrier for them.
fn late_global_callees(statement: &ast::Statement) -> Vec<*const ast::RValue> {
    fn mark(call: &ast::Call, out: &mut Vec<*const ast::RValue>) {
        if !call.callee_after_arguments || !is_import_path(&call.value) {
            return;
        }
        let mut node = call.value.as_ref();
        loop {
            out.push(node as *const _);
            let ast::RValue::Index(index) = node else { break };
            out.push(index.right.as_ref() as *const _);
            node = index.left.as_ref();
        }
    }
    fn visit(rvalue: &ast::RValue, out: &mut Vec<*const ast::RValue>) {
        if let ast::RValue::Call(call) | ast::RValue::Select(ast::Select::Call(call)) = rvalue {
            mark(call, out);
        }
        rvalue.visit_rvalues(&mut |child| { visit(child, out); true });
    }
    let mut out = Vec::new();
    if let ast::Statement::Call(call) = statement {
        mark(call, &mut out);
    }
    statement.visit_rvalues(&mut |rvalue| { visit(rvalue, &mut out); true });
    out
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
        _ => !rvalue.visit_rvalues(&mut |child| {
            !local_is_conditionally_evaluated(child, read, conditional)
        }),
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

/// Never forward a single-result call into the iterator of generalized
/// iteration (`local t = f(); for k, v in t do`). The two nils after it are
/// VM protocol, not source, and are dropped when structuring; `for k, v in
/// f() do` would then spread every result of `f` into the iterator triple.
fn forwards_call_into_generalized_iteration(
    new_rvalue: &ast::RValue,
    use_stat: &ast::Statement,
    local: &ast::RcLocal,
) -> bool {
    let ast::Statement::GenericForInit(ast::GenericForInit(init, Some(origin))) = use_stat else {
        return false;
    };
    matches!(new_rvalue, ast::RValue::Select(_))
        && origin.prep_kind == ast::ForPrepKind::Generic
        && !origin.explicit_nil_args
        && matches!(init.right.first(), Some(ast::RValue::Local(first)) if first == local)
}

/// Never forward a register cell a closure writes, as a bare local, to where
/// Luau hands the register straight to an instruction that runs after a value
/// which may write the cell ([`ast::evaluation_order::late_operand_conflict`],
/// [`ast::evaluation_order::late_store_conflict`]):
/// the copy keeps the value from before that write. `local before = x; return
/// change() < before` must not become `return x > change()`, nor `local key =
/// x; t[key] = change()` become `t[x] = change()`. An incoming upvalue is
/// fetched where it stands (GETUPVAL), and any other expression is evaluated
/// into a temporary there.
fn forwards_cell_into_late_read(
    new_rvalue: &ast::RValue,
    use_stat: &ast::Statement,
    local: &ast::RcLocal,
    shared_register: impl Fn(&ast::RcLocal) -> bool,
) -> bool {
    matches!(new_rvalue, ast::RValue::Local(cell) if shared_register(cell))
        && (ast::evaluation_order::late_operand_conflict(use_stat, local, &ast::effects::may_write_capture)
            || ast::evaluation_order::late_store_conflict(use_stat, local, &ast::effects::may_write_capture))
}

/// The operand of `rvalue` that Luau reads from its register only when the
/// operation itself runs. The compiler hands a register local straight to an
/// arithmetic or comparison instruction and to GETTABLE, so in `v + f()`,
/// `v < f()` and `v[f()]` the call runs first and `v` is read afterwards
/// (`CALL`, then `ADD Rv Rv Rcall`). An effect moved into the other operand
/// therefore still precedes the read, exactly as in the bytecode. Incoming
/// upvalues are fetched eagerly (GETUPVAL), and `..` copies its operands into
/// consecutive registers first, so neither qualifies.
fn late_register_read<'r>(rvalue: &'r ast::RValue, incoming_upvalue_ids: &FxHashSet<u64>) -> Option<&'r ast::RValue> {
    let operand = match rvalue {
        ast::RValue::Binary(binary) if matches!(
            binary.operation,
            ast::BinaryOperation::Add
                | ast::BinaryOperation::Sub
                | ast::BinaryOperation::Mul
                | ast::BinaryOperation::Div
                | ast::BinaryOperation::IDiv
                | ast::BinaryOperation::Mod
                | ast::BinaryOperation::Pow
                | ast::BinaryOperation::Equal
                | ast::BinaryOperation::NotEqual
                | ast::BinaryOperation::LessThan
                | ast::BinaryOperation::LessThanOrEqual
                | ast::BinaryOperation::GreaterThan
                | ast::BinaryOperation::GreaterThanOrEqual
        ) => binary.left.as_ref(),
        ast::RValue::Index(index) => index.left.as_ref(),
        _ => return None,
    };
    matches!(operand, ast::RValue::Local(local) if !incoming_upvalue_ids.contains(&local.stable_id()))
        .then_some(operand)
}

fn is_service_or_require_handle(rvalue: &ast::RValue) -> bool {
    ast::inline_temps::is_service_or_require_handle(rvalue)
}

struct TraverseSelf<'a, T: Traverse>(&'a mut T);

impl<'a> Traverse for TraverseSelf<'a, ast::RValue> {
    fn visit_rvalues<'b>(&'b self, visit: &mut dyn FnMut(&'b ast::RValue) -> bool) -> bool {
        visit(self.0)
    }

    fn visit_rvalues_mut<'b>(&'b mut self, visit: &mut dyn FnMut(&'b mut ast::RValue) -> bool) -> bool {
        visit(self.0)
    }

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
    local_usages: &'a mut Usages,
    readonly_capture_ids: &'a FxHashSet<u64>,
    incoming_upvalue_ids: Option<&'a FxHashSet<u64>>,
}

impl<'a> Inliner<'a> {
    fn new(
        function: &'a mut Function,
        local_to_group: &'a FxHashMap<ast::RcLocal, usize>,
        upvalue_to_group: &'a IndexMap<ast::RcLocal, ast::RcLocal>,
        local_usages: &'a mut Usages,
        readonly_capture_ids: &'a FxHashSet<u64>,
        incoming_upvalue_ids: Option<&'a FxHashSet<u64>>,
    ) -> Self {
        Self {
            function,
            local_to_group,
            upvalue_to_group,
            local_usages,
            readonly_capture_ids,
            incoming_upvalue_ids,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn try_inline(
        traversible: &mut impl Traverse,
        read: &ast::RcLocal,
        new_rvalue: &mut Option<ast::RValue>,
        new_rvalue_has_side_effects: bool,
        upvalue_to_group: &IndexMap<ast::RcLocal, ast::RcLocal>,
        readonly_capture_ids: &FxHashSet<u64>,
        incoming_upvalue_ids: Option<&FxHashSet<u64>>,
        late_callees: &[*const ast::RValue],
    ) -> bool {
        let candidate_may_write_capture = new_rvalue_has_side_effects
            && ast::effects::may_write_capture(new_rvalue.as_ref().unwrap());
        // Register reads the VM performs only when their operation runs, after
        // the candidate's position has been evaluated (see `late_register_read`).
        let mut late_reads: Vec<*const ast::RValue> = Vec::new();
        if new_rvalue_has_side_effects
            && !traversible.visit_rvalues(&mut |rvalue| {
                !local_is_conditionally_evaluated(rvalue, read, false)
            })
        {
            return false;
        }
        traversible
            .traverse_values(&mut |p, v| {
                match p {
                    ast::PreOrPost::Pre => {
                        // A store into a register local's field (`t.k = v`,
                        // `t[k] = v`) reads `t` and `k` when SETTABLE runs,
                        // after every value the statement assigns.
                        if candidate_may_write_capture
                            && let Some(incoming) = incoming_upvalue_ids
                            && let Either::Left(ast::LValue::Index(index)) = &v
                        {
                            for operand in [index.left.as_ref(), index.right.as_ref()] {
                                if matches!(operand, ast::RValue::Local(local) if !incoming.contains(&local.stable_id())) {
                                    late_reads.push(operand as *const ast::RValue);
                                }
                            }
                        }
                        if let Either::Right(rvalue) = v {
                            match rvalue {
                                ast::RValue::Binary(ast::Binary {
                                    left,
                                    right,
                                    operation,
                                    ..
                                }) if can_reverse_comparison(*operation, new_rvalue.as_ref().unwrap())
                                    && left.has_side_effects()
                                    && let ast::RValue::Local(local) = right.as_ref()
                                    && local == read =>
                                {
                                    ast::node_origins::inlined(new_rvalue.as_mut().unwrap());
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
                            if candidate_may_write_capture
                                && let Some(incoming) = incoming_upvalue_ids
                                && let Some(operand) = late_register_read(rvalue, incoming)
                            {
                                late_reads.push(operand as *const ast::RValue);
                            }
                        }
                    }
                    ast::PreOrPost::Post => {
                        if let Either::Right(rvalue) = v {
                            match rvalue {
                                ast::RValue::Local(local) if local == read => {
                                    ast::node_origins::inlined(new_rvalue.as_mut().unwrap());
                                    *rvalue = new_rvalue.take().unwrap();
                                    // success!
                                    return Some(true);
                                }
                                _ => {}
                            }
                            if new_rvalue_has_side_effects
                                && !late_callees.contains(&(rvalue as *const ast::RValue))
                                && !late_reads.contains(&(rvalue as *const ast::RValue))
                                && (rvalue_blocks_reorder(rvalue)
                                    || (candidate_may_write_capture
                                        && ast::effects::intrinsic(rvalue, &|local| upvalue_to_group.contains_key(local)
                                            && !readonly_capture_ids.contains(&local.stable_id()))
                                            .contains(ast::effects::Effects::CAPTURE_READ)))
                            {
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
    fn inline_rvalues(self, schedule: &mut schedule::Schedule) {
        let mut origin_events = Vec::new();
        let mut omitted_inline_events = 0;
        let trace_origins = self.function.provenance.is_some();
        let mut fact_statistics = facts::Statistics::default();
        for node_index in 0..schedule.nodes.len() {
            let node = schedule.nodes[node_index];
            if !schedule.visit(node, self.function.block(node).unwrap().len()) { continue; }
            let globals = self.function.globals.clone();
            let block = self.function.block_mut(node).unwrap();
            let mut facts = facts::Cache::new(
                block.len(), self.local_to_group, self.upvalue_to_group, self.readonly_capture_ids);
            let producers = ProducerIndex::new(block);

            // TODO: rename values_read to locals_read
            let mut stat_to_values_read = Vec::with_capacity(block.len());
            for stat in &block.0 {
                stat_to_values_read.push(eligible_reads(stat, |local| {
                    self.local_usages.get(local) == 1 && !self.upvalue_to_group.contains_key(local)
                        && (!local.preserve_binding() || ast::assignment_preserves_function_name(stat, local))
                }));
            }

            // visit all statements that read at least one local with only one usage,
            // this is the statement we will inline into
            // then seek backwards from the previous statement to the start of the block
            // until we find a statement that assigns to a single-use local that
            // is used in the statement we are inlining into.
            // TODO: push multiple use local assignments forward to their first use
            let mut index = 0;
            'w: while index < block.len() {
                let Some(first_producer) = producers.first(&stat_to_values_read[index], index) else {
                    index += 1;
                    continue;
                };
                let mut groups_written = FxHashSet::default();
                let mut allow_side_effects = true;
                // A candidate that may write a captured cell must not move
                // below a stepped-over read of such a cell: `local r = f()`
                // before `x = x + g(r)` would read `x` before `f` runs.
                let mut crossed_capture_read = false;
                for stat_index in (first_producer..index).rev() {
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
                        crossed_capture_read |= statement_facts.reads_capture;
                        continue;
                    }

                    if let ast::Statement::Assign(assign) = &block[stat_index]
                        && let Ok(new_rvalue) = assign.right.iter().exactly_one()
                    {
                        let new_rvalue_has_side_effects = statement_facts.single_rhs_observable.unwrap();
                        if (!new_rvalue_has_side_effects || allow_side_effects)
                            && !(crossed_capture_read && new_rvalue_has_side_effects
                                && may_write_capture_when_moved(new_rvalue, &globals))
                            && !is_service_or_require_handle(new_rvalue)
                            && !matches!(new_rvalue, ast::RValue::Closure(c)
                                if c.function.lock().retain_for_reconstruction)
                        {
                            if let Ok(ast::LValue::Local(local)) = &assign.left.iter().exactly_one()
                                && !forwards_table_into_index_write(new_rvalue, &block[index], local)
                                && !forwards_call_into_generalized_iteration(new_rvalue, &block[index], local)
                                && !forwards_cell_into_late_read(new_rvalue, &block[index], local, |cell| {
                                    self.upvalue_to_group.contains_key(cell)
                                        && !self.readonly_capture_ids.contains(&cell.stable_id())
                                        && self.incoming_upvalue_ids.is_none_or(|ids| !ids.contains(&cell.stable_id()))
                                })
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
                                // A FASTPCALL callee is an import fetched after the
                                // arguments (`Call::callee_after_arguments`), so an
                                // argument definition does not move across it.
                                let late_callees = late_global_callees(&block[index]);
                                if Self::try_inline(
                                    &mut block[index],
                                    read.as_ref().unwrap(),
                                    &mut new_rvalue,
                                    new_rvalue_has_side_effects,
                                    self.upvalue_to_group,
                                    self.readonly_capture_ids,
                                    self.incoming_upvalue_ids,
                                    &late_callees,
                                ) {
                                    assert!(new_rvalue.is_none());
                                    schedule.changed(node);

                                    // TODO: PERF: this is probably inefficient
                                    reduce_statement_roots(&mut block[index]);

                                    // TODO: PERF: remove `local_usages[l] == 1` filter in stat_to_values_read
                                    // and use stat_to_values_read here
                                    block[stat_index].visit_local_reads(&mut |local| {
                                        if self.local_usages.decrement(local) { schedule.usage_changed(local); }
                                        true
                                    });
                                    // we dont need to update local usages because tracking usages for a local
                                    // with no declarations serves no purpose
                                    block[stat_index] = ast::Empty {}.into();
                                    if trace_origins && origin_events.len() < crate::provenance::RECORD_LIMIT {
                                        origin_events.push(crate::provenance::InlineEvent {
                                            phase: "ssa_inline", producer: read.as_ref().unwrap().stable_id(),
                                            consumer_bindings: block[index].values_written()
                                                .into_iter().map(ast::RcLocal::stable_id).collect(),
                                            site_kind: "statement",
                                        });
                                    } else if trace_origins { omitted_inline_events += 1; }
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
                                // The pack fills the init's trailing slots (`next,
                                // t:GetChildren()` packs state and control); the
                                // leading values stay where they are.
                                && !assign.left.is_empty()
                                && generic_for_init.0.right.len() >= assign.left.len()
                                && generic_for_init.0.right[generic_for_init.0.right.len() - assign.left.len()..]
                                    .iter()
                                    .zip(&assign.left)
                                    .all(|(r, l)| r.as_local().is_some_and(|r| Some(r) == l.as_local()))
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
                                // The leading values are now evaluated before the
                                // pack: none may run code or read a cell it may write.
                                let has_leading_side_effects = || {
                                    generic_for_init.0.right.iter().take(start_index).any(|expr| {
                                        ast::is_observable(expr)
                                            || expr.any_local_read(&mut |local| {
                                                self.upvalue_to_group.contains_key(local)
                                                    && !self.readonly_capture_ids.contains(&local.stable_id())
                                            })
                                    })
                                };

                                if !new_rvalue_has_side_effects || !has_leading_side_effects() {
                                    schedule.changed(node);
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
                                    block[stat_index].visit_local_reads(&mut |local| {
                                        if self.local_usages.decrement(local) { schedule.usage_changed(local); }
                                        true
                                    });
                                    // we dont need to update local usages because tracking usages for a local
                                    // with no declarations serves no purpose
                                    block[stat_index] = ast::Empty {}.into();
                                    for old_local in old_locals {
                                        if trace_origins && origin_events.len() < crate::provenance::RECORD_LIMIT {
                                            origin_events.push(crate::provenance::InlineEvent {
                                                phase: "ssa_inline", producer: old_local.stable_id(),
                                                consumer_bindings: Vec::new(), site_kind: "generic_for_pack",
                                            });
                                        } else if trace_origins { omitted_inline_events += 1; }
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
                    crossed_capture_read |= statement_facts.reads_capture;
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
                    .map(|(_, argument)| eligible_reads(argument, |local| {
                        self.local_usages.get(local) == 1 && !self.upvalue_to_group.contains_key(local)
                    }))
                    .collect_vec();

                let mut index = 0;
                'w: while index < arg_to_values_read.len() {
                    let end = self.function.block(node).unwrap().len();
                    let Some(first_producer) = producers.first(&arg_to_values_read[index], end) else {
                        index += 1;
                        continue;
                    };
                    let mut groups_written = FxHashSet::default();
                    for stat_index in (first_producer..end).rev() {
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
                                && !local.preserve_binding()
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
                                    self.upvalue_to_group,
                                    self.readonly_capture_ids,
                                    None,
                                    &[],
                                ) {
                                    assert!(new_rvalue.is_none());
                                    schedule.changed(node);
                                    let block = self.function.block_mut(node).unwrap();

                                    // TODO: PERF: remove `local_usages[l] == 1` filter in stat_to_values_read
                                    // and use stat_to_values_read here
                                    block[stat_index].visit_local_reads(&mut |local| {
                                        if self.local_usages.decrement(local) { schedule.usage_changed(local); }
                                        true
                                    });
                                    // we dont need to update local usages because tracking usages for a local
                                    // with no declarations serves no purpose

                                    block[stat_index] = ast::Empty {}.into();
                                    if trace_origins && origin_events.len() < crate::provenance::RECORD_LIMIT {
                                        origin_events.push(crate::provenance::InlineEvent {
                                            phase: "ssa_inline", producer: read.as_ref().unwrap().stable_id(),
                                            consumer_bindings: vec![self.function.graph().edge_weight(edge).unwrap().arguments[index].0.stable_id()],
                                            site_kind: "phi_argument",
                                        });
                                    } else if trace_origins { omitted_inline_events += 1; }
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
        if let Some(trace) = &mut self.function.provenance {
            trace.dropped_records += omitted_inline_events;
            for event in origin_events { trace.inline_event(event); }
        }
    }
}

/// Reduce exactly the same direct expression roots after each substitution.
/// Replacing one root cannot change the surrounding statement's root slots;
/// neither sibling reductions nor successive reductions may be skipped.
fn reduce_statement_roots(statement: &mut ast::Statement) {
    #[cfg(test)]
    if tests::REFERENCE_SCHEDULE.with(std::cell::Cell::get) {
        for value in statement.rvalues_mut() {
            *value = std::mem::replace(value, ast::Literal::Nil.into()).reduce();
        }
        return;
    }
    statement.visit_rvalues_mut(&mut |value| {
        *value = std::mem::replace(value, ast::Literal::Nil.into()).reduce();
        true
    });
}

/// Keep the old per-statement/edge eligibility snapshot and duplicate operand
/// order, while collecting only the retained handles. The mutable inliner
/// later marks consumed slots with None without admitting newly exposed reads.
fn eligible_reads(value: &impl LocalRw, mut eligible: impl FnMut(&ast::RcLocal) -> bool) -> Vec<Option<ast::RcLocal>> {
    let mut reads = Vec::new();
    value.visit_local_reads(&mut |local| {
        if eligible(local) { reads.push(Some(local.clone())); }
        true
    });
    #[cfg(test)]
    assert_eq!(reads, value.values_read().into_iter().filter(|local| eligible(local))
        .cloned().map(Some).collect::<Vec<_>>());
    reads
}

fn rvalue_reads_local(rvalue: &ast::RValue, local: &ast::RcLocal) -> bool {
    rvalue.any_local_read(&mut |read| read == local)
}

fn decrement_local_usage(
    local_usages: &mut Usages,
    local: &ast::RcLocal,
    usage_changed: &mut impl FnMut(&ast::RcLocal),
) {
    if local_usages.decrement(local) { usage_changed(local); }
}

fn decrement_rvalue_usages(
    local_usages: &mut Usages,
    rvalue: &ast::RValue,
    usage_changed: &mut impl FnMut(&ast::RcLocal),
) {
    rvalue.visit_local_reads(&mut |local| {
        decrement_local_usage(local_usages, local, usage_changed);
        true
    });
}

/// Index of the `local t = {...}` declaration that a SETLIST at `set_list_index`
/// can be folded into once the declaration is moved directly before it.
///
/// The move is only legal when no statement in between reads or writes `t`
/// (closure captures count as reads), and when every entry already in the
/// constructor is total-pure and reads no local written in between (moving the
/// constructor later must not change what those entries evaluate to). A call
/// in between writes no local syntactically but may write a captured cell
/// (`cell`): an entry reading one stays before any statement that can run
/// code (`{a = x, obj:Get()}` reads `x` before the call).
fn movable_table_declaration(
    block: &ast::Block,
    set_list_index: usize,
    object_local: &ast::RcLocal,
    cell: impl Fn(&ast::RcLocal) -> bool,
) -> Option<usize> {
    let mut written_between: Vec<ast::RcLocal> = Vec::new();
    let mut runs_code_between = false;
    for j in (0..set_list_index).rev() {
        let statement = &block[j];
        if let Some(assign) = statement.as_assign()
            && table_constructor_local(assign).as_ref() == Some(object_local)
        {
            let table = assign.right[0].as_table().unwrap();
            let entries_movable = table.0.iter().all(|(key, value)| {
                key.iter().chain(std::iter::once(value)).all(|rvalue| {
                    ast::is_total_pure(rvalue)
                        && !rvalue.any_local_read(&mut |local| {
                            written_between.contains(local) || (runs_code_between && cell(local))
                        })
                })
            });
            return entries_movable.then_some(j);
        }
        if statement.any_local_read(&mut |local| local == object_local)
            || statement.any_local_write(&mut |local| local == object_local)
        {
            return None;
        }
        runs_code_between |= ast::statement_is_observable(statement);
        statement.visit_local_writes(&mut |local| { written_between.push(local.clone()); true });
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
            ..
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

/// Where a contiguous field store lands in the constructor being folded.
enum FieldSlot {
    Replace(usize),
    MoveToEnd(usize),
    Append,
}

fn fold_table_constructor_field_assignments(
    block: &mut ast::Block,
    local_usages: &mut Usages,
    upvalue_to_group: &IndexMap<ast::RcLocal, ast::RcLocal>,
    usage_changed: &mut impl FnMut(&ast::RcLocal),
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
        let mut listed = None;
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
            // Replacing a nil/zero template field moves this evaluation across the
            // rest of the constructor. Cross only total fields without
            // mutable-cell snapshots; otherwise append in the original order. A
            // key the constructor already lists stays a statement: the later
            // store is a mutation, and `{ k = a, k = b }` is never how a table
            // is written.
            // The last listed entry for the key's slot is the one the store
            // overwrites (`{[0] = 0, [-0] = 10}` keeps 10 in slot 0).
            let slot = match table.0[..initial_len.min(table.0.len())]
                .iter()
                .rposition(|(k, _)| k.as_ref().is_some_and(|k| ast::same_table_key(k, key)))
            {
                Some(p)
                    if ast::is_inert_entry_value(&table.0[p].1)
                        && table.0[p..].iter().all(|(key, value)| {
                            key.as_ref().is_some_and(ast::is_total_table_key)
                                && ast::is_total_pure(value)
                                && !value.any_local_read(&mut |read| upvalue_to_group.contains_key(read))
                        }) => FieldSlot::Replace(p),
                Some(p) if ast::is_template_placeholder(&table.0[p].1) && ast::is_total_table_key(key) => {
                    FieldSlot::MoveToEnd(p)
                }
                _ if listed.get_or_insert_with(|| ast::ListedKeys::new(table)).lists(table, key) => break,
                _ => FieldSlot::Append,
            };

            decrement_local_usage(local_usages, &object_local, usage_changed);
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
            match slot {
                FieldSlot::Replace(p) => {
                    decrement_rvalue_usages(local_usages, &table.0[p].1, usage_changed);
                    table.0[p].1 = new_value;
                }
                FieldSlot::MoveToEnd(p) => {
                    table.0.remove(p);
                    table.0.push((Some(new_key), new_value));
                }
                FieldSlot::Append => {
                    if let Some(listed) = &mut listed {
                        listed.add(&new_key);
                    }
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
    inline_with_readonly_captures(function, local_to_group, upvalue_to_group, &FxHashSet::default(), None);
}

/// `readonly_capture_ids` must come from immutable input-cell evidence mapped
/// through this SSA construction's incoming groups. Names/types are not proof;
/// absent evidence uses `inline` and protects every captured destination read.
///
/// `incoming_upvalue_ids` lists every SSA version of this function's incoming
/// upvalues; every other local lives in a register. With it, the inliner uses
/// Luau's late register reads (see `late_register_read`); `None` keeps every
/// captured read an ordering barrier.
pub fn inline_with_readonly_captures(
    function: &mut Function,
    local_to_group: &FxHashMap<ast::RcLocal, usize>,
    upvalue_to_group: &IndexMap<ast::RcLocal, ast::RcLocal>,
    readonly_capture_ids: &FxHashSet<u64>,
    incoming_upvalue_ids: Option<&FxHashSet<u64>>,
) {
    let census_timer = ast::prof::Timer::new(&ast::prof::I_CENSUS);
    let mut local_usages = Usages::census(function);
    drop(census_timer);
    #[cfg(not(test))]
    let dirty_scheduling = !cfg!(feature = "reference-inline-sweeps");
    #[cfg(test)]
    let dirty_scheduling = !tests::REFERENCE_SCHEDULE.with(std::cell::Cell::get);
    let mut schedule = schedule::Schedule::new(function, dirty_scheduling);
    let mut changed = true;
    while changed {
        changed = false;
        schedule.begin_sweep();
        let inline_timer = ast::prof::Timer::new(&ast::prof::I_INLINE);
        Inliner::new(
            function,
            local_to_group,
            upvalue_to_group,
            &mut local_usages,
            readonly_capture_ids,
            incoming_upvalue_ids,
        )
        .inline_rvalues(&mut schedule);
        drop(inline_timer);
        let dead_timer = ast::prof::Timer::new(&ast::prof::I_DEAD);

        // remove unused locals
        for node_index in 0..schedule.nodes.len() {
            let node = schedule.nodes[node_index];
            let block = function.block_mut(node).unwrap();
            for stat_index in 0..block.len() {
                if let ast::Statement::Assign(assign) = &block[stat_index]
                    && assign.left.len() == 1
                    && assign.right.len() == 1
                    && let ast::LValue::Local(local) = &assign.left[0]
                {
                    let rvalue = &assign.right[0];
                    // TODO: REFACTOR: is_some_and
                    if !upvalue_to_group.contains_key(local)
                        && local_usages.get(local) == 0
                    {
                        if rvalue.has_side_effects() {
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
                                schedule.changed(node);
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
                                schedule.changed(node);
                            }
                        }
                    }
                }
            }
        }

        drop(dead_timer);
        let _tables_timer = ast::prof::Timer::new(&ast::prof::I_TABLES);
        for node_index in 0..schedule.nodes.len() {
            let node = schedule.nodes[node_index];
            let block = function.block_mut(node).unwrap();
            // we check block.ast.len() elsewhere and do `i - ` here and elsewhere so we need to get rid of empty statements
            // TODO: fix ^
            let old_len = block.len();
            block.retain(|s| s.as_empty().is_none());
            if block.len() != old_len { schedule.changed(node); }

            // `t = {} t.a = 1` -> `t = { a = 1 }`
            let folded = fold_table_constructor_field_assignments(
                block,
                &mut local_usages,
                upvalue_to_group,
                &mut |local| schedule.usage_changed(local),
            );
            if folded {
                changed = true;
                schedule.changed(node);
            }

            // if the first statement is a set_list, we cant inline it anyway
            for i in 1..block.len() {
                if let ast::Statement::SetList(set_list) = &block[i] {
                    let object_local = set_list.object_local.clone();
                    let expected_entries = set_list.index.checked_sub(1);
                    let has_multret_value = set_list.values.last().is_some_and(|value|
                        matches!(value, ast::RValue::VarArg(_) | ast::RValue::Call(_) | ast::RValue::MethodCall(_)));
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
                        && let Some(decl_index) = movable_table_declaration(block, i, &object_local, |local| {
                            upvalue_to_group.contains_key(local) && !readonly_capture_ids.contains(&local.stable_id())
                        })
                    {
                        let decl = block.remove(decl_index);
                        block.insert(i - 1, decl);
                        changed = true;
                        schedule.changed(node);
                    }
                    if let Some(assign) = block[i - 1].as_assign()
                        && assign.left == [object_local.into()]
                        && assign.right.len() == 1
                        && assign.right[0].as_table().is_some_and(|table| {
                            expected_entries
                                == Some(table.0.iter().filter(|(key, _)| key.is_none()).count())
                                && !table.0.last().is_some_and(|(key, value)| key.is_none()
                                    && matches!(value, ast::RValue::VarArg(_) | ast::RValue::Call(_) | ast::RValue::MethodCall(_)))
                                && !has_multret_value
                        })
                    {
                        let set_list = std::mem::replace(&mut block[i], ast::Empty {}.into())
                            .into_set_list()
                            .unwrap();
                        let decremented = local_usages.decrement(&set_list.object_local);
                        debug_assert!(decremented, "a SETLIST reads its table");
                        schedule.usage_changed(&set_list.object_local);
                        let assign = block.get_mut(i - 1).unwrap().as_assign_mut().unwrap();
                        let table = assign.right[0].as_table_mut().unwrap();
                        for value in set_list.values {
                            table.0.push((None, value));
                        }
                        if let Some(tail) = set_list.tail {
                            table.0.push((None, tail));
                        }
                        changed = true;
                        schedule.changed(node);
                    }
                    // todo: only inline in changed blocks
                    //cfg::dot::render_to(function, &mut std::io::stdout());
                    //break 'outer;
                }
            }
        }
        #[cfg(test)]
        tests::record_sweep(function, &local_usages);
    }
    schedule.record();
    #[cfg(test)]
    tests::SCHEDULE_STATISTICS.with(|statistics| statistics.set(schedule.statistics));
    // we check block.ast.len() elsewhere and do `i - ` here and elsewhere so we need to get rid of empty statements
    // TODO: fix ^
    for block in function.blocks_mut() {
        block.retain(|s| s.as_empty().is_none());
    }
}

#[cfg(test)]
mod tests {
    thread_local! {
        // Run the unchanged full-prefix search as a differential oracle. The
        // flag is thread-local so other unit tests cannot see the override.
        pub(super) static REFERENCE_SCAN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        pub(super) static REFERENCE_SCHEDULE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        pub(super) static SCHEDULE_STATISTICS: std::cell::Cell<super::schedule::Statistics> = Default::default();
        static SWEEPS: std::cell::RefCell<Option<Vec<Sweep>>> = const { std::cell::RefCell::new(None) };
    }

    #[derive(Debug, PartialEq)]
    struct Sweep {
        blocks: Vec<(usize, Block)>,
        edges: Vec<(usize, usize, crate::block::BranchType, Vec<(RcLocal, RValue)>)>,
        usages: Vec<usize>,
        origins: Vec<Option<(Vec<ast::node_origins::Input>, bool, bool, Option<&'static str>, bool)>>,
        inline_events: Vec<(u64, Vec<u64>, &'static str)>,
    }

    pub(super) fn record_sweep(function: &Function, usages: &super::Usages) {
        use ast::Traverse;
        SWEEPS.with(|sweeps| {
            let mut sweeps = sweeps.borrow_mut();
            let Some(sweeps) = sweeps.as_mut() else { return; };
            let mut origins = Vec::new();
            let mut origin = |value: Option<&ast::node_origins::Origin>| {
                origins.push(value.and_then(|origin| origin.0.as_ref()).map(|data| (
                    data.inputs.iter().map(|input| (**input).clone()).collect(),
                    data.inlined, data.cloned, data.synthesized, data.incomplete,
                )));
            };
            let mut edges = Vec::new();
            for (node, block) in function.blocks() {
                for statement in block.iter() {
                    origin(ast::node_origins::statement(statement));
                    statement.traverse_rvalues_ref(&mut |value| origin(ast::node_origins::value(value)));
                }
                for edge in function.edges(node) {
                    for (_, value) in &edge.weight().arguments {
                        origin(ast::node_origins::value(value));
                        value.traverse_rvalues_ref(&mut |value| origin(ast::node_origins::value(value)));
                    }
                    edges.push((node.index(), edge.target().index(), edge.weight().branch_type.clone(), edge.weight().arguments.clone()));
                }
            }
            sweeps.push(Sweep {
                blocks: function.blocks().map(|(node, block)| (node.index(), block.clone())).collect(),
                edges,
                usages: usages.counts(),
                origins,
                inline_events: function.provenance.as_ref().map(|trace| trace.inlines.iter()
                    .map(|event| (event.producer, event.consumer_bindings.clone(), event.site_kind)).collect()).unwrap_or_default(),
            });
        });
    }

    fn run_scheduled(
        function: &mut Function,
        groups: &FxHashMap<RcLocal, usize>,
        captures: &IndexMap<RcLocal, RcLocal>,
        reference: bool,
    ) -> (Vec<Sweep>, super::schedule::Statistics) {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                REFERENCE_SCHEDULE.with(|flag| flag.set(false));
                SWEEPS.with(|sweeps| { sweeps.borrow_mut().take(); });
            }
        }
        let _reset = Reset;
        REFERENCE_SCHEDULE.with(|flag| flag.set(reference));
        SWEEPS.with(|sweeps| *sweeps.borrow_mut() = Some(Vec::new()));
        let ids = ast::current_local_id();
        inline(function, groups, captures);
        assert_eq!(ast::current_local_id(), ids, "inlining must not mint identities");
        (SWEEPS.with(|sweeps| sweeps.borrow_mut().take().unwrap()), SCHEDULE_STATISTICS.with(std::cell::Cell::get))
    }

    struct ReferenceScan;
    impl ReferenceScan {
        fn enter() -> Self {
            REFERENCE_SCAN.with(|flag| assert!(!flag.replace(true)));
            Self
        }
    }
    impl Drop for ReferenceScan {
        fn drop(&mut self) { REFERENCE_SCAN.with(|flag| flag.set(false)); }
    }
    use super::{
        fold_table_constructor_field_assignments, inline, local_is_conditionally_evaluated,
        rvalue_blocks_reorder,
    };
    use crate::function::Function;
    use ast::{
        Assign, Binary, Block, Global, Index, LValue, Literal, Local, RValue, RcLocal, Return,
        Statement, Table, LocalRw,
    };
    use ast::FxIndexMap as IndexMap;
    use petgraph::visit::EdgeRef;
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
        fold_table_constructor_field_assignments(block, &mut super::Usages::default(), &IndexMap::default(), &mut |_| {})
    }

    fn inline_block(block: Block) -> Block {
        let mut function = Function::new(0);
        let entry = function.new_block();
        *function.block_mut(entry).unwrap() = block;
        function.set_entry(entry);

        inline(&mut function, &FxHashMap::default(), &IndexMap::default());

        function.block(entry).unwrap().clone()
    }

    #[test]
    fn dirty_schedule_skips_stable_blocks_without_extending_the_fixed_point() {
        let mut function = Function::new(0);
        let nodes: Vec<_> = (0..129).map(|_| function.new_block()).collect();
        function.set_entry(nodes[0]);
        for &node in &nodes[..128] {
            for _ in 0..16 {
                function.block_mut(node).unwrap().push(ast::Call::new(global("effect"), Vec::new()).into());
            }
        }
        function.block_mut(nodes[128]).unwrap().push(Assign::new(
            vec![RcLocal::default().into()], vec![number(1.0)],
        ).into());
        let mut reference = function.clone();
        let (actual, statistics) = run_scheduled(&mut function, &FxHashMap::default(), &IndexMap::default(), false);
        let (expected, legacy) = run_scheduled(&mut reference, &FxHashMap::default(), &IndexMap::default(), true);
        assert_eq!(actual, expected);
        assert_eq!(statistics.sweeps, 2, "only the dead assignment requests another sweep");
        assert_eq!(statistics.sweeps, legacy.sweeps);
        assert_eq!(statistics.blocks_skipped, 128);
        assert_eq!(statistics.usage_invalidations, 0);
        assert_eq!(legacy.statement_visits - statistics.statement_visits, 128 * 16);
    }

    #[test]
    fn global_usage_revision_keeps_table_changes_in_the_legacy_phase_order() {
        let mut function = Function::new(0);
        let first = function.new_block();
        let last = function.new_block();
        function.set_entry(first);
        function.block_mut(first).unwrap().push(ast::Call::new(global("effect"), Vec::new()).into());
        let table = RcLocal::default();
        function.block_mut(last).unwrap().extend([
            table_decl(&table), field_assign(&table, string("value"), number(1.0)), return_local(&table),
        ]);
        let mut reference = function.clone();
        let (actual, statistics) = run_scheduled(&mut function, &FxHashMap::default(), &IndexMap::default(), false);
        let (expected, legacy) = run_scheduled(&mut reference, &FxHashMap::default(), &IndexMap::default(), true);
        assert_eq!(actual, expected);
        assert_eq!(statistics.sweeps, 2, "the last inline move must not start a third sweep");
        assert_eq!(statistics.sweeps, legacy.sweeps);
        assert!(statistics.usage_invalidations > 0);
        assert_eq!(statistics.blocks_skipped, 0, "a global usage change conservatively revisits all blocks");
    }

    #[test]
    fn usage_changes_reactivate_other_blocks_and_outgoing_argument_candidates() {
        for edge_use in [false, true] {
            for changed_first in [false, true] {
                let mut function = Function::new(0);
                let nodes: Vec<_> = (0..4).map(|_| function.new_block()).collect();
                function.set_entry(nodes[0]);
                let (candidate, changing) = if changed_first { (nodes[2], nodes[0]) } else { (nodes[0], nodes[2]) };
                let table = RcLocal::default();
                function.block_mut(candidate).unwrap().push(table_decl(&table));
                if edge_use {
                    function.set_edges(candidate, vec![(nodes[3], crate::block::BlockEdge {
                        arguments: vec![(RcLocal::default(), table.clone().into())], ..Default::default()
                    })]);
                } else {
                    function.block_mut(candidate).unwrap().push(return_local(&table));
                }
                // Public hand-built CFGs can redefine the same identity. The
                // second declaration's field fold changes the first block's
                // global eligibility from two reads to exactly one.
                function.block_mut(changing).unwrap().extend([
                    table_decl(&table), field_assign(&table, string("field"), number(1.0)),
                ]);
                function.block_mut(nodes[1]).unwrap().push(ast::Call::new(global("untouched"), Vec::new()).into());
                let mut reference = function.clone();
                let (actual, _) = run_scheduled(&mut function, &FxHashMap::default(), &IndexMap::default(), false);
                let (expected, _) = run_scheduled(&mut reference, &FxHashMap::default(), &IndexMap::default(), true);
                assert_eq!(actual, expected, "edge_use={edge_use}, changed_first={changed_first}");
                assert!(function.block(candidate).unwrap().iter().all(|statement| statement.as_assign().is_none()));
            }
        }
    }

    #[test]
    fn scheduled_sweeps_match_reference_with_captures_origins_tables_and_parallel_edges() {
        use ast::Traverse;
        for seed in 0..192usize {
            let mut random = seed + 1;
            let mut next = || {
                random = random.wrapping_mul(1664525).wrapping_add(1013904223);
                random >> 8
            };
            let mut function = Function::new(0);
            let nodes: Vec<_> = (0..8).map(|_| function.new_block()).collect();
            function.set_entry(nodes[0]);
            function.remove_block(nodes[5]); // StableGraph holes must not become scheduled blocks.
            let mut locals: Vec<_> = (0..5).map(|_| RcLocal::default()).collect();
            function.parameters = locals.clone();
            for &node in &nodes {
                if node == nodes[5] { continue; }
                for _ in 0..(2 + next() % 14) {
                    let source = locals[next() % locals.len()].clone();
                    let target = if next() % 7 == 0 { source.clone() } else { RcLocal::default() };
                    let value = match next() % 7 {
                        0 => number((next() % 8) as f64),
                        1 => source.clone().into(),
                        2 => ast::Call::new(global("effect"), vec![source.clone().into()]).into(),
                        3 => Index::new(source.clone().into(), string("field")).into(),
                        4 => Binary::new(source.clone().into(), number(1.0), ast::BinaryOperation::Add).into(),
                        5 => RValue::Table(Table::default()),
                        _ => ast::Closure {
                            node_origin: Default::default(), function: Default::default(),
                            upvalues: vec![ast::Upvalue::Copy(source.clone()), ast::Upvalue::Ref(source.clone())],
                        }.into(),
                    };
                    function.block_mut(node).unwrap().push(Assign::new(vec![target.clone().into()], vec![value]).into());
                    locals.push(target);
                    if next() % 4 == 0 {
                        let table = RcLocal::default();
                        function.block_mut(node).unwrap().extend([
                            table_decl(&table), field_assign(&table, string("item"), source.clone().into()),
                            return_local(&table),
                        ]);
                        locals.push(table);
                    }
                    if next() % 5 == 0 {
                        let table = RcLocal::default();
                        function.block_mut(node).unwrap().extend([
                            table_decl(&table), ast::SetList::new(table.clone(), 1, vec![source.into()], None).into(),
                            return_local(&table),
                        ]);
                        locals.push(table);
                    }
                }
                let argument = locals[next() % locals.len()].clone();
                for _ in 0..next() % 3 {
                    function.graph_mut().add_edge(node, nodes[7], crate::block::BlockEdge {
                        arguments: vec![(RcLocal::default(), argument.clone().into())], ..Default::default()
                    });
                }
            }
            for (node, block) in function.blocks().map(|(node, block)| (node, block.len())).collect::<Vec<_>>() {
                for index in 0..block {
                    let statement = &mut function.block_mut(node).unwrap()[index];
                    let input = ast::node_origins::Input {
                        function: "schedule-test".into(), block: node.index(), statement: index, value: None,
                    };
                    if let Some(origin) = ast::node_origins::statement_mut(statement) {
                        *origin = ast::node_origins::Origin::input(input.clone());
                    }
                    statement.traverse_rvalues(&mut |value| {
                        if let Some(origin) = ast::node_origins::value_mut(value) {
                            *origin = ast::node_origins::Origin::input(input.clone());
                        }
                    });
                }
            }
            function.provenance = Some(Box::new(crate::provenance::FunctionTrace::new(0, "schedule-test".into())));
            let groups = locals.iter().enumerate().map(|(index, local)| (local.clone(), index % 5)).collect();
            let captures = locals.iter().enumerate().filter(|(index, _)| index % 13 == seed % 13)
                .map(|(_, local)| (local.clone(), local.clone())).collect();
            let mut actual = function.clone();
            let mut reference = function.clone(); // Both variants receive equal clone-origin flags.
            let (actual_sweeps, statistics) = run_scheduled(&mut actual, &groups, &captures, false);
            let (reference_sweeps, legacy) = run_scheduled(&mut reference, &groups, &captures, true);
            assert_eq!(actual_sweeps, reference_sweeps, "seed={seed}");
            assert_eq!(statistics.sweeps, legacy.sweeps, "seed={seed}");
        }
    }

    #[test]
    fn indexed_search_matches_full_prefix_with_effects_groups_and_edge_arguments() {
        for seed in 0..96usize {
            let mut random = seed + 1;
            let mut next = || {
                random = random.wrapping_mul(1664525).wrapping_add(1013904223);
                random
            };
            let mut function = Function::new(0);
            let entry = function.new_block();
            let exit = function.new_block();
            function.set_entry(entry);
            let mut locals: Vec<_> = (0..4).map(|_| RcLocal::default()).collect();
            function.parameters = locals.clone();
            for _ in 0..40 {
                let read = locals[next() % locals.len()].clone();
                let rhs = match next() % 6 {
                    0 => number((next() % 8) as f64),
                    1 => read.clone().into(),
                    2 => ast::Call::new(global("effect"), vec![read.clone().into()]).into(),
                    3 => Index::new(read.clone().into(), string("field")).into(),
                    4 => Binary::new(read.clone().into(), number(1.0), ast::BinaryOperation::Add).into(),
                    _ => RValue::Table(Table::default()),
                };
                // Repeated definitions are intentional: public hand-built
                // CFGs need the same conservative behavior as the old scan.
                let target = if next() % 7 == 0 { read } else { RcLocal::default() };
                function.block_mut(entry).unwrap().push(Assign::new(vec![target.clone().into()], vec![rhs]).into());
                locals.push(target);
                if next() % 3 == 0 {
                    let arg = locals[next() % locals.len()].clone();
                    function.block_mut(entry).unwrap().push(ast::Call::new(global("barrier"), vec![arg.into()]).into());
                }
            }
            let params = [RcLocal::default(), RcLocal::default()];
            let arguments = params.iter().map(|param| (param.clone(), locals[next() % locals.len()].clone().into())).collect();
            function.set_edges(entry, vec![(exit, crate::block::BlockEdge { arguments, ..Default::default() })]);
            function.block_mut(exit).unwrap().push(Return::new(params.into_iter().map(RValue::from).collect()).into());
            let groups = locals.iter().enumerate().map(|(index, local)| (local.clone(), index % 5)).collect();
            let captures = locals.iter().enumerate().filter(|(index, _)| index % 11 == seed % 11)
                .map(|(_, local)| (local.clone(), local.clone())).collect();
            let mut reference = function.clone();
            inline(&mut function, &groups, &captures);
            {
                let _reference = ReferenceScan::enter();
                inline(&mut reference, &groups, &captures);
            }
            for (node, block) in function.blocks() {
                assert_eq!(block, reference.block(node).unwrap(), "seed={seed}, node={node:?}");
                let actual: Vec<_> = function.edges(node).map(|edge| edge.weight().arguments.clone()).collect();
                let expected: Vec<_> = reference.edges(node).map(|edge| edge.weight().arguments.clone()).collect();
                assert_eq!(actual, expected, "edge arguments, seed={seed}");
            }
        }
    }

    #[test]
    fn indexed_scan_stays_local_for_long_effect_barrier_blocks() {
        let mut block = Block::default();
        let mut reads = Vec::new();
        for _ in 0..6_000 {
            let value = RcLocal::default();
            block.push(Assign::new(vec![value.clone().into()], vec![ast::Call::new(global("f"), vec![]).into()]).into());
            block.push(ast::Call::new(global("g"), vec![]).into());
            block.push(ast::Call::new(global("h"), vec![value.clone().into()]).into());
            reads.push(value);
        }
        let producers = super::ProducerIndex::new(&block);
        let scans: usize = reads.iter().enumerate().map(|(index, local)| {
            let consumer = 3 * index + 2;
            consumer - producers.first(&[Some(local.clone())], consumer).unwrap()
        }).sum();
        assert_eq!(scans, 12_000, "two barriers per consumer, independent of prefix length");
        let output = inline_block(block);
        assert_eq!(output.len(), 18_000, "all effectful statements retain their order");
    }

    #[test]
    fn method_lookup_runs_after_arguments_in_pinned_luau() {
        for shape in 0..3 {
            let argument = local("argument");
            let object = local("object");
            let method = ast::MethodCall::new(local_value(&object), "consume".into(), vec![local_value(&argument)]);
            let use_statement = match shape {
                0 => ast::Statement::MethodCall(method),
                1 => Return::new(vec![RValue::MethodCall(method)]).into(),
                _ => Return::new(vec![RValue::Select(ast::Select::MethodCall(method))]).into(),
            };
            let block = Block(vec![
                Assign::new(vec![LValue::Local(argument.clone())],
                    vec![ast::Call::new(global("fetch"), vec![]).into()]).into(),
                use_statement,
            ]);
            let mut result = inline_block(block);
            remove_empty(&mut result);
            // Compiler.cpp emits argument code before NAMECALL. Unlike a dot
            // call, colon syntax does not fetch the method before arguments.
            assert_eq!(result.len(), 1, "shape {shape} unnecessarily keeps the argument temporary");
        }
    }

    #[test]
    fn method_lookup_allows_literal_argument_and_effectful_receiver() {
        for receiver in [false, true] {
            let temporary = local("temporary");
            let object = local("object");
            let value = if receiver { ast::Call::new(global("fetch"), vec![]).into() } else { number(7.0) };
            let method = ast::MethodCall::new(
                if receiver { local_value(&temporary) } else { local_value(&object) },
                "consume".into(), if receiver { vec![] } else { vec![local_value(&temporary)] });
            let mut result = inline_block(Block(vec![
                Assign::new(vec![LValue::Local(temporary)], vec![value]).into(),
                Return::new(vec![RValue::MethodCall(method)]).into(),
            ]));
            remove_empty(&mut result);
            assert_eq!(result.len(), 1, "safe receiver/literal case should still inline");
        }
    }

    #[test]
    fn captured_callee_read_does_not_move_before_argument_callback() {
        for captured in [false, true] {
            let argument = local("argument");
            let callee = local("callee");
            let mut function = Function::new(0);
            let entry = function.new_block();
            *function.block_mut(entry).unwrap() = Block(vec![
                Assign::new(vec![LValue::Local(argument.clone())],
                    vec![ast::Call::new(global("fetch"), vec![]).into()]).into(),
                Return::new(vec![ast::Call::new(local_value(&callee), vec![local_value(&argument)]).into()]).into(),
            ]);
            function.set_entry(entry);
            let captures = if captured { IndexMap::from_iter([(callee.clone(), callee)]) } else { IndexMap::default() };
            inline(&mut function, &FxHashMap::default(), &captures);
            let result = function.block_mut(entry).unwrap();
            remove_empty(result);
            assert_eq!(result.len(), if captured { 2 } else { 1 });
        }
    }

    #[test]
    fn register_operand_is_read_after_the_call_in_its_other_operand() {
        // `local t = fetch(); return value + t` becomes `return value + fetch()`:
        // Luau reads the register `value` when ADD runs, after the call, so a
        // captured `value` is no barrier. An incoming upvalue is fetched before
        // the call (GETUPVAL) and `..` copies its operands first, so those stay.
        for (shape, incoming, inlined) in [
            ("add", false, true),
            ("less_than", false, true),
            ("index", false, true),
            ("add", true, false),
            ("concat", false, false),
        ] {
            let value = local("value");
            let result = local("result");
            let operand = |operation| Binary::new(local_value(&value), local_value(&result), operation).into();
            let returned: RValue = match shape {
                "add" => operand(ast::BinaryOperation::Add),
                "less_than" => operand(ast::BinaryOperation::LessThan),
                "concat" => operand(ast::BinaryOperation::Concat),
                _ => Index::new(local_value(&value), local_value(&result)).into(),
            };
            let mut function = Function::new(0);
            let entry = function.new_block();
            *function.block_mut(entry).unwrap() = Block(vec![
                Assign::new(vec![LValue::Local(result.clone())],
                    vec![ast::Call::new(global("fetch"), vec![]).into()]).into(),
                Return::new(vec![returned]).into(),
            ]);
            function.set_entry(entry);
            let captures = IndexMap::from_iter([(value.clone(), value.clone())]);
            let incoming_ids = if incoming {
                rustc_hash::FxHashSet::from_iter([value.stable_id()])
            } else {
                rustc_hash::FxHashSet::default()
            };
            super::inline_with_readonly_captures(&mut function, &FxHashMap::default(), &captures,
                &Default::default(), Some(&incoming_ids));
            let block = function.block_mut(entry).unwrap();
            remove_empty(block);
            assert_eq!(block.len(), if inlined { 1 } else { 2 }, "{shape} incoming={incoming}");
        }
    }

    #[test]
    fn cell_snapshot_stays_where_luau_reads_the_register_late() {
        // `local before = cell` with `change()` able to write `cell`: as the
        // operand of a comparison (reversed or not), the base or key of a
        // store, a method receiver or a constructor key, the register `cell`
        // would be read after the call. An incoming upvalue is fetched where
        // it stands, and with nothing that may write the cell nothing moves.
        for (shape, incoming, inlined) in [
            ("compare", false, false),
            ("store_key", false, false),
            ("store_base", false, false),
            ("receiver", false, false),
            ("constructor_key", false, false),
            ("store_key", true, true),
            ("store_literal", false, true),
        ] {
            let cell = local("cell");
            let before = local("before");
            let target = local("target");
            let change = || -> RValue { ast::Call::new(global("change"), vec![]).into() };
            let store = |base: RValue, key: RValue, value: RValue| -> Statement {
                Assign::new(vec![Index::new(base, key).into()], vec![value]).into()
            };
            let use_statement = match shape {
                "compare" => Return::new(vec![
                    Binary::new(change(), local_value(&before), ast::BinaryOperation::LessThan).into(),
                ]).into(),
                "store_key" => store(local_value(&target), local_value(&before), change()),
                "store_literal" => store(local_value(&target), local_value(&before), number(1.0)),
                "store_base" => store(local_value(&before), string("x"), change()),
                "receiver" => Return::new(vec![
                    ast::MethodCall::new(local_value(&before), "m".into(), vec![change()]).into(),
                ]).into(),
                _ => Return::new(vec![Table::new(vec![(Some(local_value(&before)), change())]).into()]).into(),
            };
            let mut function = Function::new(0);
            let entry = function.new_block();
            *function.block_mut(entry).unwrap() = Block(vec![
                Assign::new(vec![LValue::Local(before.clone())], vec![local_value(&cell)]).into(),
                use_statement,
            ]);
            function.set_entry(entry);
            let captures = IndexMap::from_iter([(cell.clone(), cell.clone())]);
            let incoming_ids = if incoming {
                rustc_hash::FxHashSet::from_iter([cell.stable_id()])
            } else {
                rustc_hash::FxHashSet::default()
            };
            super::inline_with_readonly_captures(&mut function, &FxHashMap::default(), &captures,
                &Default::default(), Some(&incoming_ids));
            let block = function.block_mut(entry).unwrap();
            remove_empty(block);
            assert_eq!(block.len(), if inlined { 1 } else { 2 }, "{shape} incoming={incoming}: {block}");
        }
    }

    #[test]
    fn captured_callee_and_argument_reads_can_commute() {
        let argument = local("argument");
        let callee = local("callee");
        let captured_value = local("captured_value");
        let mut function = Function::new(0);
        let entry = function.new_block();
        *function.block_mut(entry).unwrap() = Block(vec![
            Assign::new(vec![LValue::Local(argument.clone())], vec![local_value(&captured_value)]).into(),
            Return::new(vec![ast::Call::new(local_value(&callee), vec![local_value(&argument)]).into()]).into(),
        ]);
        function.set_entry(entry);
        let captures = IndexMap::from_iter([(callee.clone(), callee), (captured_value.clone(), captured_value)]);
        inline(&mut function, &FxHashMap::default(), &captures);
        let result = function.block_mut(entry).unwrap();
        remove_empty(result);
        assert_eq!(result.len(), 1);
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
        block[0].as_assign_mut().unwrap().right[0] = Table::new(vec![
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
            &mut super::Usages::default(),
            &protected,
            &mut |_| {},
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
        let table = RValue::Table(Table::new(vec![(
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
    fn reconstruction_candidate_survives_until_child_bodies_exist() {
        for retain in [false, true] {
            let helper = local("adjust");
            let closure = ast::Closure { node_origin: Default::default(), function: Default::default(), upvalues: vec![] };
            closure.function.lock().retain_for_reconstruction = retain;
            let mut block = inline_block(Block(vec![
                Assign::new(vec![helper.clone().into()], vec![closure.into()]).into(),
                Return::new(vec![local_value(&helper)]).into(),
            ]));
            remove_empty(&mut block);
            assert_eq!(block.len(), if retain { 2 } else { 1 }, "{block}");
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
            vec![RValue::Table(Table::new(entries))],
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
        assert_eq!(movable_table_declaration(&block, 3, &t, |_| false), Some(0));
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
        assert_eq!(movable_table_declaration(&block, 2, &t, |_| false), None);
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
        assert_eq!(movable_table_declaration(&block, 2, &t, |_| false), None);
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
        assert_eq!(movable_table_declaration(&block, 2, &t, |_| false), Some(0));
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
