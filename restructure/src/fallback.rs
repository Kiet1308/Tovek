//! Semantics-preserving fallback for CFGs that are not reducible to the small
//! set of structured patterns handled by [`GraphStructurer`].
//!
//! The normal structurer deliberately aims for readable Lua and uses labels as
//! an intermediate representation when a graph has several entries.  A later
//! pass removes those labels in the common cases.  Some bytecode shapes (most
//! notably a generic-for containing a break that rejoins an outer generic-for)
//! are irreducible in that representation, however.  Falling through to the
//! label printer would either emit invalid Luau or require a semantics-changing
//! guess about which loop a jump belongs to.
//!
//! This module is a fail-closed fallback: it translates the *original,
//! post-SSA* CFG into a local state machine.  Every CFG edge becomes an explicit
//! state transition, so no edge is discarded and no source-level goto/label is
//! needed.  Generic-for VM markers are deliberately rejected here until the
//! CFG preserves VM prep-kind and edge-sensitive write metadata.  Treating
//! `FORGPREP` as an identity assignment is not equivalent for tables, `__iter`,
//! `__call`, or unsupported values, and treating `FORGLOOP` as a plain call can
//! get hidden-control exhaustion semantics wrong.  Returning `None` keeps this
//! fallback fail-closed instead of emitting plausible but incorrect Luau.

use ast::{
    Assign, Binary, BinaryOperation, Block, Continue, If, LValue, Literal, Local, LocalRw, RValue,
    RcLocal, Statement, Traverse, Upvalue, While,
};
use cfg::{block::BlockEdge, function::Function};
use petgraph::{stable_graph::NodeIndex, visit::EdgeRef};
use rustc_hash::{FxHashMap, FxHashSet};

/// Purpose of a local synthesized by the certified CFG fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntheticRole {
    ProgramCounter,
    DispatchSignal,
    DispatchExit,
}

#[derive(Clone, Debug)]
pub struct SyntheticLocal {
    pub local: RcLocal,
    pub role: SyntheticRole,
}

/// A fallback block together with identity-based metadata for every synthetic
/// control local it introduced.  Policies can inspect this without relying on
/// generated names such as `controlFlowState`.
#[derive(Debug)]
pub struct CertifiedFallback {
    pub block: Block,
    pub synthetic_locals: Vec<SyntheticLocal>,
}

/// Lower a post-SSA CFG to a valid Luau state machine.
///
/// `None` means that the CFG contains a construct for which this conservative
/// fallback does not yet have a proven lowering.  Callers must retain their
/// existing fail-closed behaviour in that case; this function never emits a
/// partial program.
pub fn lift(function: Function) -> Option<Block> {
    let locals_to_ignore = function
        .parameters
        .iter()
        .cloned()
        .collect::<FxHashSet<_>>();
    lift_with_ignored_locals(function, &locals_to_ignore)
}

/// Build a fallback state machine while preserving the lexical scope of
/// locals that are live across dispatcher iterations.
///
/// A `continue` in the generated dispatcher starts a new iteration of the
/// enclosing `while`.  Lua locals declared inside that loop are consequently
/// re-created (and reset to `nil`) on every transition.  Any value that flows
/// from one CFG state to another must therefore be declared outside the loop.
/// The normal local-declaration pass does not know that our synthetic loop is a
/// dispatcher, so we seed declarations here for exactly those values.  The
/// caller supplies parameters and upvalues in `locals_to_ignore`; declaring one
/// of those would shadow the function's actual inputs and change semantics.
pub fn lift_with_ignored_locals(
    function: Function,
    locals_to_ignore: &FxHashSet<RcLocal>,
) -> Option<Block> {
    lift_certified_with_ignored_locals(function, locals_to_ignore).map(|result| result.block)
}

pub fn lift_certified_with_ignored_locals(
    function: Function,
    locals_to_ignore: &FxHashSet<RcLocal>,
) -> Option<CertifiedFallback> {
    let entry = *function.entry().as_ref()?;
    let nodes = reachable_nodes(&function, entry);
    if nodes.is_empty() {
        return None;
    }

    // The dispatcher only has a proven lowering for the flat, post-SSA CFG
    // statements above.  A prestructured statement with an embedded block is
    // opaque to the CFG edge/liveness analysis below: `If::values_read` and
    // `Traverse` intentionally expose only the condition, while the nested
    // body can contain writes or reference captures.  Lowering such a block
    // as an ordinary state statement can therefore reset a captured local on
    // the next dispatcher iteration.  Reject it until nested-block effects
    // have a dedicated scope-aware analysis.
    if nodes.iter().any(|&node| {
        function.block(node).is_some_and(|block| {
            block.iter().any(statement_has_embedded_block)
        })
    }) {
        return None;
    }

    // A GenericForInit/GenericForNext pair is still a low-level VM protocol,
    // not a source-level iterator expression.  The current Function IR does
    // not retain the FORGPREP variant/AUX metadata needed to distinguish the
    // table fast path, `__iter`, `__call`, and callable-generator paths, nor
    // the path-specific result writes on exhaustion.  Refuse every reachable
    // marker until that provenance is available; callers then produce the
    // explicit unsupported-structuring sentinel rather than wrong source.
    if nodes.iter().any(|&node| {
        function.block(node).is_some_and(|block| {
            block.iter().any(|statement| {
                matches!(
                    statement,
                    Statement::GenericForInit(_) | Statement::GenericForNext(_)
                )
            })
        })
    }) {
        return None;
    }

    let states: FxHashMap<NodeIndex, usize> = nodes
        .iter()
        .enumerate()
        .map(|(state, &node)| (node, state))
        .collect();
    let state_local = RcLocal::new(Local::new(Some("controlFlowState".to_string())));

    let mut plans = Vec::with_capacity(nodes.len());
    let mut state_facts = Vec::with_capacity(nodes.len());
    let mut state_successors = Vec::with_capacity(nodes.len());
    for &node in &nodes {
        let statements = function.block(node)?.clone().0;
        let edges = function
            .edges(node)
            .map(|edge| (edge.target(), edge.weight().clone()))
            .collect::<Vec<_>>();
        state_successors.push(edges.iter().map(|(target, _)| *target).collect());
        state_facts.push(analyze_state(&statements, &edges));

        let body = lower_block(statements, &edges, &states, &state_local)?;
        plans.push(body);
    }

    // Build the dispatch ladder backwards so every branch has a structurally
    // valid else block.  Each state body ends in `continue`, `break`, or
    // `return`; the fallback never relies on accidental fall-through.
    let mut dispatch = Block(vec![Statement::Break(ast::Break {}).into()]);
    for (state, body) in plans.into_iter().enumerate().rev() {
        let condition = Binary::new(
            state_local.clone().into(),
            Literal::Number(state as f64).into(),
            BinaryOperation::Equal,
        )
        .into();
        dispatch = Block(vec![If::new(condition, body, dispatch).into()]);
    }

    let entry_state = *states.get(&entry)?;
    let mut root = Block::default();

    // Seed declarations before the synthetic `while`.  Keep ordering stable so
    // generated output remains deterministic regardless of hash iteration
    // order.  Locals that are initialized before every read remain scoped by
    // `LocalDeclarer`, preserving the readable output of the normal pipeline.
    let persistent_locals =
        persistent_locals(&state_facts, &state_successors, &states, locals_to_ignore);
    // A local captured by reference can represent a fresh per-iteration Lua
    // cell.  Hoisting that binding outside the synthetic dispatcher would make
    // closures from different iterations share one cell, changing observable
    // behaviour.  We do not have enough lexical-scope information in a flat
    // post-SSA CFG to split such cells soundly, so reject this one shape rather
    // than emit code with a plausible but incorrect capture lifetime.  By-value
    // captures are snapshots and remain safe to hoist; parameters/upvalues are
    // excluded from `persistent_locals` by the caller.
    if contains_ref_capture(
        &function,
        &nodes,
        &state_facts,
        &persistent_locals,
        locals_to_ignore,
    ) {
        return None;
    }
    if !persistent_locals.is_empty() {
        let mut declaration = Assign::new(
            persistent_locals.into_iter().map(LValue::Local).collect(),
            Vec::new(),
        );
        declaration.prefix = true;
        root.push(declaration.into());
    }
    root.push(
        Assign::new(vec![LValue::Local(state_local.clone())], vec![
            Literal::Number(entry_state as f64).into(),
        ])
        .into(),
    );
    root.push(While::new(Literal::Boolean(true).into(), dispatch).into());
    Some(CertifiedFallback {
        block: root,
        synthetic_locals: vec![SyntheticLocal {
            local: state_local,
            role: SyntheticRole::ProgramCounter,
        }],
    })
}

fn statement_has_embedded_block(statement: &Statement) -> bool {
    match statement {
        // An empty If is the CFG conditional terminator and is safe: its real
        // branches are represented by BlockEdges.  A non-empty branch is an
        // already-structured subgraph whose effects are not visible to the
        // fallback's flat-state analysis.
        Statement::If(if_statement) => {
            !if_statement.then_block.lock().is_empty()
                || !if_statement.else_block.lock().is_empty()
        }
        // These statements always own an embedded body.  They are not emitted
        // by the post-SSA CFG fallback input and must not be treated as opaque
        // state statements until their nested effects are analyzed.
        Statement::While(_)
        | Statement::Repeat(_)
        | Statement::NumericFor(_)
        | Statement::GenericFor(_) => true,
        _ => false,
    }
}

fn contains_ref_capture(
    function: &Function,
    nodes: &[NodeIndex],
    facts: &[StateFacts],
    persistent_locals: &[RcLocal],
    locals_to_ignore: &FxHashSet<RcLocal>,
) -> bool {
    // Build the set of local values carried by each outgoing edge.  The
    // destination (`param`) is a write that does not appear in a source
    // statement, while RHS reads can be hidden by a same-state definition in
    // `StateFacts`; both must participate in the ambiguity check below.
    let edge_values = nodes
        .iter()
        .map(|&node| {
            function
                .edges(node)
                .flat_map(|edge| {
                    edge.weight().arguments.iter().flat_map(|(param, value)| {
                        std::iter::once(param).chain(value.values_read().into_iter())
                    })
                })
                .cloned()
                .collect::<FxHashSet<_>>()
        })
        .collect::<Vec<_>>();

    let persistent = persistent_locals.iter().collect::<FxHashSet<_>>();
    for (capture_state, state) in facts.iter().enumerate() {
        for local in &state.captured_ref {
            // Parameters and already-linked upvalues have function scope, so
            // their cells are not recreated by the synthetic dispatcher.
            if locals_to_ignore.contains(local) {
                continue;
            }
            // Hoisting a reference capture is only safe when its cell is known
            // to live across all dispatcher iterations.  `persistent_locals`
            // is intentionally conservative, but it can miss a capture that
            // is followed solely by a write (the write kills normal liveness).
            // Such a shape is still ambiguous after CLOSEUPVALS was discarded,
            // so fail closed regardless of whether liveness selected it.
            if persistent.contains(local)
                || edge_values
                    .get(capture_state)
                    .is_some_and(|values| values.contains(local))
                || facts.iter().enumerate().any(|(other_state, other)| {
                    other_state != capture_state
                        && (other.captured_ref.contains(local)
                            || other.use_before_def.contains(local)
                            || other.defs.contains(local)
                            || edge_values
                                .get(other_state)
                                .is_some_and(|values| values.contains(local)))
                })
            {
                return true;
            }
        }
    }
    false
}

fn reachable_nodes(function: &Function, entry: NodeIndex) -> Vec<NodeIndex> {
    let mut seen = FxHashSet::default();
    let mut stack = vec![entry];
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        stack.extend(function.successor_blocks(node));
    }

    // NodeIndex order is stable for a given CFG and avoids making generated
    // source depend on traversal details of petgraph's adjacency storage.
    let mut nodes = seen.into_iter().collect::<Vec<_>>();
    nodes.sort_by_key(|node| node.index());
    nodes
}

#[derive(Default)]
struct StateFacts {
    /// Locals read before their first write in this state.  The set is
    /// deliberately conservative for complex expressions: `values_read` is
    /// evaluated before the statement's writes, matching parallel assignment
    /// semantics.
    use_before_def: FxHashSet<RcLocal>,
    /// Locals written by the state body.  Edge-copy destinations are not
    /// considered definite here: a conditional predecessor may provide a
    /// value on only one path, so treating them as source-state definitions
    /// would unsafely suppress a required hoist.
    defs: FxHashSet<RcLocal>,
    /// Locals captured by-reference by closures constructed in this state.
    /// Value captures (`Upvalue::Copy`) snapshot their input and do not impose
    /// a cell-lifetime constraint on the dispatcher.
    captured_ref: FxHashSet<RcLocal>,
}

fn analyze_state(statements: &[Statement], edges: &[(NodeIndex, BlockEdge)]) -> StateFacts {
    let mut facts = StateFacts::default();
    for statement in statements {
        for local in statement.values_read() {
            if !facts.defs.contains(local) {
                facts.use_before_def.insert(local.clone());
            }
        }
        facts
            .defs
            .extend(statement.values_written().into_iter().cloned());

        // Traverse all expression positions (including index l-values) while
        // stopping at the child function body.  Clone the statement because
        // this analysis runs before lowering and the traversal API is mutable.
        let mut statement_copy = statement.clone();
        statement_copy.traverse_rvalues(&mut |value| {
            if let RValue::Closure(closure) = value {
                for upvalue in &closure.upvalues {
                    if let Upvalue::Ref(local) = upvalue {
                        facts.captured_ref.insert(local.clone());
                    }
                }
            }
        });
    }

    // Edge copies are parallel and execute after the source block.  Reads on
    // their RHS therefore observe the source state's definitions.  Their
    // destinations are intentionally not added to `defs`; with multiple
    // conditional predecessors they are not definitely initialized at target
    // entry, and over-hoisting is safer than allowing a reset local to leak.
    for (_, edge) in edges {
        for (_, value) in &edge.arguments {
            for local in value.values_read() {
                if !facts.defs.contains(local) {
                    facts.use_before_def.insert(local.clone());
                }
            }
        }
    }
    facts
}

fn persistent_locals(
    facts: &[StateFacts],
    successors: &[Vec<NodeIndex>],
    states: &FxHashMap<NodeIndex, usize>,
    locals_to_ignore: &FxHashSet<RcLocal>,
) -> Vec<RcLocal> {
    let mut live_in = facts
        .iter()
        .map(|state| state.use_before_def.clone())
        .collect::<Vec<_>>();

    // The graph is finite and each update only adds locals to a set, so this
    // monotone fixed point terminates after at most |states|*|locals| inserts.
    loop {
        let mut changed = false;
        for state in (0..facts.len()).rev() {
            let mut live_out = FxHashSet::default();
            for target in &successors[state] {
                let Some(&target_state) = states.get(target) else {
                    continue;
                };
                live_out.extend(live_in[target_state].iter().cloned());
            }

            let mut next = facts[state].use_before_def.clone();
            next.extend(
                live_out
                    .into_iter()
                    .filter(|local| !facts[state].defs.contains(local)),
            );
            if next != live_in[state] {
                live_in[state] = next;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut result = live_in
        .into_iter()
        .flatten()
        .filter(|local| !locals_to_ignore.contains(local))
        .collect::<FxHashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    result.sort();
    result
}

fn lower_block(
    mut statements: Vec<Statement>,
    edges: &[(NodeIndex, BlockEdge)],
    states: &FxHashMap<NodeIndex, usize>,
    state_local: &RcLocal,
) -> Option<Block> {
    // The CFG lifter represents a conditional jump as an empty If terminator;
    // its real branches live on the two outgoing BlockEdges.  Refuse to
    // reinterpret a non-empty If here because doing so could execute a nested
    // branch twice.
    if matches!(statements.last(), Some(Statement::If(_))) {
        if edges.len() != 2 {
            return None;
        }
        let condition = {
            let statement = statements.pop()?.into_if().ok()?;
            if !statement.then_block.lock().is_empty() || !statement.else_block.lock().is_empty() {
                return None;
            }
            statement.condition
        };
        let then_edge = edge_of_type(edges, cfg::block::BranchType::Then)?;
        let else_edge = edge_of_type(edges, cfg::block::BranchType::Else)?;
        statements.push(
            If::new(
                condition,
                transition(then_edge, states, state_local)?,
                transition(else_edge, states, state_local)?,
            )
            .into(),
        );
        return Some(statements.into());
    }

    let mut lowered = Vec::with_capacity(statements.len() + 2);
    for statement in statements {
        match statement {
            // Never print low-level protocol markers into source.  The early
            // reachable-node guard normally handles these; retaining the
            // match here makes the helper fail closed even if it is reused
            // independently in the future.
            Statement::GenericForInit(_)
            | Statement::GenericForNext(_)
            | Statement::NumForInit(_)
            | Statement::NumForNext(_)
            | Statement::Goto(_)
            | Statement::Label(_) => return None,
            other => lowered.push(other),
        }
    }

    match edges {
        [] => {
            if !matches!(lowered.last(), Some(Statement::Return(_))) {
                lowered.push(Statement::Break(ast::Break {}).into());
            }
        }
        [edge] if edge.1.branch_type == cfg::block::BranchType::Unconditional => {
            lowered.extend(transition(edge, states, state_local)?.0)
        }
        _ => return None,
    }
    Some(lowered.into())
}

fn edge_of_type(
    edges: &[(NodeIndex, BlockEdge)],
    kind: cfg::block::BranchType,
) -> Option<&(NodeIndex, BlockEdge)> {
    edges.iter().find(|(_, edge)| edge.branch_type == kind)
}

fn transition(
    edge: &(NodeIndex, BlockEdge),
    states: &FxHashMap<NodeIndex, usize>,
    state_local: &RcLocal,
) -> Option<Block> {
    let (target, edge) = edge;
    let state = *states.get(target)?;
    let mut body = Block::default();

    if !edge.arguments.is_empty() {
        let mut assign = Assign::new(
            edge.arguments
                .iter()
                .map(|(local, _)| LValue::Local(local.clone()))
                .collect(),
            edge.arguments
                .iter()
                .map(|(_, value)| value.clone())
                .collect(),
        );
        assign.parallel = true;
        body.push(assign.into());
    }
    body.push(
        Assign::new(vec![LValue::Local(state_local.clone())], vec![
            Literal::Number(state as f64).into(),
        ])
        .into(),
    );
    body.push(Continue {}.into());
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ast::{
        Closure, Function as AstFunction, GenericForInit, GenericForNext, NumForNext, Upvalue,
    };
    use cfg::block::BlockEdge;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn edge(
        function: &mut Function,
        from: NodeIndex,
        to: NodeIndex,
        branch_type: cfg::block::BranchType,
    ) {
        function
            .graph_mut()
            .add_edge(from, to, BlockEdge::new(branch_type));
    }

    #[test]
    fn rejects_generic_for_without_vm_protocol_metadata() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let next = function.new_block();
        let body = function.new_block();
        let exit = function.new_block();
        function.set_entry(entry);

        let generator = local("generator");
        let state = local("state");
        let control = local("control");
        let result = local("result");

        function
            .block_mut(entry)
            .unwrap()
            .push(GenericForInit::new(generator.clone(), state.clone(), control.clone()).into());
        function.block_mut(next).unwrap().push(
            GenericForNext::new(
                vec![result.clone()],
                generator.clone().into(),
                state.clone(),
                control.clone(),
            )
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(ast::Return::new(Vec::new()).into());
        edge(
            &mut function,
            entry,
            next,
            cfg::block::BranchType::Unconditional,
        );
        edge(&mut function, next, body, cfg::block::BranchType::Then);
        edge(&mut function, next, exit, cfg::block::BranchType::Else);
        edge(
            &mut function,
            body,
            next,
            cfg::block::BranchType::Unconditional,
        );

        // The fallback must not pretend that a tuple assignment implements
        // Luau's FORGPREP/FORGLOOP protocol.  Without prep-kind/AUX metadata,
        // emitting a callable iterator would be wrong for plain tables,
        // __iter/__call values, and path-specific exhaustion writes.
        assert!(lift(function).is_none());
    }

    #[test]
    fn reports_synthetic_control_by_local_identity() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);
        function
            .block_mut(entry)
            .unwrap()
            .push(ast::Return::new(Vec::new()).into());

        let fallback = lift_certified_with_ignored_locals(function, &FxHashSet::default()).unwrap();
        assert_eq!(fallback.synthetic_locals.len(), 1);
        assert_eq!(
            fallback.synthetic_locals[0].role,
            SyntheticRole::ProgramCounter
        );
        let rendered = fallback.block.to_string();
        assert!(rendered.contains("while true do"), "{rendered}");
    }

    #[test]
    fn predeclares_values_crossing_dispatcher_iterations() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let use_state = function.new_block();
        function.set_entry(entry);

        let value = local("value");
        let sink = local("sink");
        function.block_mut(entry).unwrap().push(
            Assign::new(vec![LValue::Local(value.clone())], vec![
                Literal::Number(1.0).into(),
            ])
            .into(),
        );
        function
            .block_mut(use_state)
            .unwrap()
            .push(Assign::new(vec![LValue::Local(sink)], vec![value.clone().into()]).into());
        edge(
            &mut function,
            entry,
            use_state,
            cfg::block::BranchType::Unconditional,
        );

        let output = lift(function).unwrap().to_string();
        let declaration = output.find("local value").expect("missing declaration");
        let dispatcher = output.find("while true do").expect("missing dispatcher");
        assert!(declaration < dispatcher, "{output}");
        assert!(!output[dispatcher..].contains("local value"), "{output}");
    }

    #[test]
    fn predeclares_values_reused_by_a_self_loop() {
        let mut function = Function::new(0);
        let loop_node = function.new_block();
        function.set_entry(loop_node);

        let value = local("value");
        function.block_mut(loop_node).unwrap().push(
            Assign::new(vec![LValue::Local(value.clone())], vec![
                Binary::new(
                    value.clone().into(),
                    Literal::Number(1.0).into(),
                    BinaryOperation::Add,
                )
                .into(),
            ])
            .into(),
        );
        edge(
            &mut function,
            loop_node,
            loop_node,
            cfg::block::BranchType::Unconditional,
        );

        let output = lift(function).unwrap().to_string();
        let declaration = output.find("local value").expect("missing declaration");
        let dispatcher = output.find("while true do").expect("missing dispatcher");
        assert!(declaration < dispatcher, "{output}");
        assert!(!output[dispatcher..].contains("local value"), "{output}");
    }

    #[test]
    fn predeclares_values_across_a_multi_state_cycle() {
        let mut function = Function::new(0);
        let producer = function.new_block();
        let consumer = function.new_block();
        function.set_entry(producer);

        let value = local("value");
        let sink = local("sink");
        function.block_mut(producer).unwrap().push(
            Assign::new(vec![LValue::Local(value.clone())], vec![
                Literal::Number(1.0).into(),
            ])
            .into(),
        );
        function
            .block_mut(consumer)
            .unwrap()
            .push(Assign::new(vec![LValue::Local(sink)], vec![value.clone().into()]).into());
        edge(
            &mut function,
            producer,
            consumer,
            cfg::block::BranchType::Unconditional,
        );
        edge(
            &mut function,
            consumer,
            producer,
            cfg::block::BranchType::Unconditional,
        );

        let output = lift(function).unwrap().to_string();
        let declaration = output.find("local value").expect("missing declaration");
        let dispatcher = output.find("while true do").expect("missing dispatcher");
        assert!(declaration < dispatcher, "{output}");
        assert!(!output[dispatcher..].contains("local value"), "{output}");
    }

    #[test]
    fn does_not_shadow_ignored_function_inputs() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let use_state = function.new_block();
        function.set_entry(entry);

        let parameter = local("parameter");
        function.parameters.push(parameter.clone());
        function.block_mut(entry).unwrap().push(
            Assign::new(vec![LValue::Local(local("unused"))], vec![
                Literal::Number(1.0).into(),
            ])
            .into(),
        );
        function
            .block_mut(use_state)
            .unwrap()
            .push(ast::Return::new(vec![parameter.clone().into()]).into());
        edge(
            &mut function,
            entry,
            use_state,
            cfg::block::BranchType::Unconditional,
        );

        let mut ignored = FxHashSet::default();
        ignored.insert(parameter.clone());
        let output = lift_with_ignored_locals(function, &ignored).unwrap();
        assert!(
            !matches!(
                output.first(),
                Some(Statement::Assign(assign))
                    if assign.prefix
                        && assign.left.iter().any(|left| {
                            matches!(left, LValue::Local(local) if local == &parameter)
                        })
            ),
            "function parameters must not be redeclared:\n{output}"
        );
    }

    #[test]
    fn refuses_unlowered_numeric_loop_markers() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);
        let counter = local("counter");
        function.block_mut(entry).unwrap().push(
            NumForNext::new(
                counter,
                Literal::Number(1.0).into(),
                Literal::Number(1.0).into(),
            )
            .into(),
        );
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_prestructured_nested_blocks_until_scope_analysis() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);

        let captured = local("captured");
        let callback = local("callback");
        let closure = Closure {
            function: by_address::ByAddress(Arc::new(Mutex::new(AstFunction::default()))),
            upvalues: vec![Upvalue::Ref(captured)],
        };
        let nested = If::new(
            Literal::Boolean(true).into(),
            Block(vec![
                Assign::new(vec![LValue::Local(callback)], vec![RValue::Closure(closure)])
                    .into(),
            ]),
            Block::default(),
        );
        function.block_mut(entry).unwrap().push(nested.into());

        // `If` bodies are not visible to the flat CFG liveness/capture pass.
        // Keep the fallback certified by refusing this prestructured shape
        // until nested-block scope effects have a dedicated analysis.
        assert!(lift(function).is_none());
    }

    #[test]
    fn refuses_ref_capture_that_would_cross_dispatcher_iterations() {
        let mut function = Function::new(0);
        let capture = function.new_block();
        let write = function.new_block();
        function.set_entry(capture);

        let captured = local("captured");
        let callback = local("callback");
        let closure = Closure {
            function: by_address::ByAddress(Arc::new(Mutex::new(AstFunction::default()))),
            upvalues: vec![Upvalue::Ref(captured.clone())],
        };
        function.block_mut(capture).unwrap().push(
            Assign::new(vec![LValue::Local(callback)], vec![RValue::Closure(
                closure,
            )])
            .into(),
        );
        function.block_mut(write).unwrap().push(
            Assign::new(vec![LValue::Local(captured.clone())], vec![
                Literal::Number(1.0).into(),
            ])
            .into(),
        );
        edge(
            &mut function,
            capture,
            write,
            cfg::block::BranchType::Unconditional,
        );
        edge(
            &mut function,
            write,
            capture,
            cfg::block::BranchType::Unconditional,
        );

        assert!(
            lift(function).is_none(),
            "fallback must fail closed when a Ref capture would be hoisted"
        );
    }

    #[test]
    fn allows_ref_capture_of_function_input() {
        let mut function = Function::new(0);
        let capture = function.new_block();
        let use_input = function.new_block();
        function.set_entry(capture);

        let parameter = local("parameter");
        let callback = local("callback");
        function.parameters.push(parameter.clone());
        let closure = Closure {
            function: by_address::ByAddress(Arc::new(Mutex::new(AstFunction::default()))),
            upvalues: vec![Upvalue::Ref(parameter.clone())],
        };
        function.block_mut(capture).unwrap().push(
            Assign::new(vec![LValue::Local(callback)], vec![RValue::Closure(
                closure,
            )])
            .into(),
        );
        function
            .block_mut(use_input)
            .unwrap()
            .push(ast::Return::new(vec![parameter.clone().into()]).into());
        edge(
            &mut function,
            capture,
            use_input,
            cfg::block::BranchType::Unconditional,
        );
        edge(
            &mut function,
            use_input,
            capture,
            cfg::block::BranchType::Unconditional,
        );

        let ignored = [parameter].into_iter().collect();
        assert!(
            lift_with_ignored_locals(function, &ignored).is_some(),
            "function-scoped inputs must remain valid capture cells"
        );
    }

    #[test]
    fn refuses_ref_capture_followed_by_cross_state_write_even_when_liveness_kills_read() {
        // The capture reads `captured` only after this state's assignment, so a
        // plain use-before-def liveness scan would not hoist it.  A subsequent
        // state writes the same local; after CLOSEUPVALS metadata has been
        // discarded, either sharing or splitting that cell is possible.  The
        // fallback must therefore reject the shape rather than silently pick
        // one closure lifetime.
        let mut function = Function::new(0);
        let capture = function.new_block();
        let write = function.new_block();
        function.set_entry(capture);

        let captured = local("captured");
        let callback = local("callback");
        function.block_mut(capture).unwrap().extend([
            Assign::new(vec![LValue::Local(captured.clone())], vec![
                Literal::Number(0.0).into(),
            ])
            .into(),
            Assign::new(vec![LValue::Local(callback)], vec![RValue::Closure(
                Closure {
                    function: by_address::ByAddress(Arc::new(Mutex::new(AstFunction::default()))),
                    upvalues: vec![Upvalue::Ref(captured.clone())],
                },
            )])
            .into(),
        ]);
        function.block_mut(write).unwrap().push(
            Assign::new(vec![LValue::Local(captured)], vec![
                Literal::Number(1.0).into(),
            ])
            .into(),
        );
        edge(
            &mut function,
            capture,
            write,
            cfg::block::BranchType::Unconditional,
        );
        edge(
            &mut function,
            write,
            capture,
            cfg::block::BranchType::Unconditional,
        );

        assert!(
            lift(function).is_none(),
            "cross-state writes after a Ref capture are ambiguous without Close metadata"
        );
    }
}
