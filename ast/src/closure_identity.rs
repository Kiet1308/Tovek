//! Keep the identity of a closure that one bytecode constant shares.
//!
//! Luau loads a closure whose captures never change with DUPCLOSURE: every
//! load of one constant yields the same object while the captured values
//! stay rawequal (`lvmexecute.cpp`, `LOP_DUPCLOSURE`). Luau's `-O2` inliner
//! compiles one function literal at every inlined call site against that one
//! constant, so `x.callback == y.callback` holds in the bytecode. Printed as
//! two literals, the copies would recompile to two constants and compare
//! unequal.
//!
//! When two or more copies escape (anything but a call reads them), they
//! become one binding, declared right before the first statement making a
//! copy, in the innermost block holding them all:
//!
//! ```lua
//! print(({ cb = function() return 2 end }).cb == ({ cb = function() return 2 end }).cb)
//! -- becomes
//! local function cb() return 2 end
//! print(({ cb = cb }).cb == ({ cb = cb }).cb)
//! ```
//!
//! The copies must capture the same locals, each visible there and never
//! written after its declaration, or an upvalue of the function (one cell for
//! every copy), so the one closure captures what each copy did. A recursive
//! `local function` copy captures itself, the shared closure: the first copy's
//! declaration becomes the binding and the others read it. Copies that only
//! calls read keep their literals: nothing can compare them. Escaping copies
//! that capture different variables stay separate: in the bytecode they are
//! one object only while the values happen to be equal, which no source
//! spells without the helper (README, "Output and validation").
//!
//! The constant also caches: the first run of any copy fills it with that
//! copy's captured values, and a later run reuses the closure only while its
//! own values stay rawequal to those, making a new closure otherwise. Copies
//! capturing different values are thus unlike separate literals, each with
//! a constant of its own. Where one copy runs before every other (it runs
//! whenever its statement does, and that statement comes first), another
//! copy whose captures can never equal the first one's values (bound to a
//! different constant, or to a literal of a different function) makes a new
//! closure on every run. [`LoadedConstants::mark_uncached_copies`] marks such
//! a copy made anew right after linking (its `closure_constant` cleared), so
//! every later pass treats it as the NEWCLOSURE it behaves as: the
//! de-inliner may rebuild it into a call of its helper, which makes a new
//! closure too, and [`crate::fresh_closures`] keeps it new where it stays a
//! literal.
//!
//! Most chunks have no copies at all. [`LoadedConstants`], filled while
//! lifting, tells so without walking the tree.

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    Assign, Block, Closure, Function, LValue, LocalRw, RValue, RcLocal, Select, Statement, Traverse, Upvalue,
    inline_temps::collect_closures_in_statement,
};

/// One constant of one prototype: `(prototype, constant slot)`.
type Key = (usize, usize);

/// A position in a function body: a statement index, and which block of that
/// statement the path continues into (`None`: the statement's own values).
type Step = (usize, Option<u8>);

/// The closures the chunk's DUPCLOSURE instructions load, recorded while
/// lifting. Two literals of one function body load one constant only when
/// its prototype loads it at two instructions, or when a pass copied one
/// literal: tail duplication shares the copied closure's function, and no
/// pass before [`share_closure_constants`] builds a function with a constant
/// slot of its own. With neither, the walk is skipped.
#[derive(Default)]
pub struct LoadedConstants {
    /// Some prototype loads one closure constant at two instructions.
    repeated: bool,
    /// Every closure a DUPCLOSURE loads. Besides this list, only the tree
    /// holds them once the lifter's own tables are gone.
    functions: Vec<Arc<Mutex<Function>>>,
}

impl LoadedConstants {
    /// Record the closures one prototype makes, one per closure instruction.
    pub fn record_prototype<'a>(&mut self, closures: impl Iterator<Item = &'a Arc<Mutex<Function>>>) {
        let mut slots = Vec::new();
        for function in closures {
            if let Some(slot) = function.lock().closure_constant {
                slots.push(slot);
                self.functions.push(function.clone());
            }
        }
        slots.sort_unstable();
        self.repeated |= slots.windows(2).any(|pair| pair[0] == pair[1]);
    }

    /// Whether two literals of one function body can load one constant: a
    /// repeated load, or a function that two places of the tree share.
    fn may_repeat(&self) -> bool {
        if self.repeated {
            crate::telemetry::count("closure_identity_repeated_loads", 1);
            return true;
        }
        let copied = self.functions.iter().any(|function| Arc::strong_count(function) > 2);
        crate::telemetry::count("closure_identity_copied_literals", u64::from(copied));
        copied
    }

    /// Clear the constant of each copy the constant's cache makes a new
    /// closure on every run of (module documentation), when some prototype
    /// loads one closure constant at two instructions. Returns how many.
    pub fn mark_uncached_copies(&self, block: &Block) -> usize {
        if !self.repeated {
            return 0;
        }
        let mut marked = 0;
        mark_in_function(block, &[], &mut FxHashSet::default(), &mut marked);
        crate::telemetry::count("closure_identity_uncached_copies", marked as u64);
        marked
    }

    /// [`share_closure_constants`], when some copies may exist. Consumes the
    /// list, so the passes after it see the tree's own sharing again.
    pub fn share(self, block: &mut Block) {
        if self.may_repeat() {
            drop(self);
            share_closure_constants(block);
        } else {
            drop(self);
            #[cfg(debug_assertions)]
            assert!(!repeats_a_constant(block, &mut FxHashSet::default()), "a closure constant repeats unrecorded");
        }
    }
}

pub fn share_closure_constants(block: &mut Block) {
    share_in_function(block, &[], &mut FxHashSet::default());
}

fn key(closure: &Closure) -> Option<Key> {
    let function = closure.function.lock();
    Some((function.bytecode_proto_id?, function.closure_constant?))
}

/// The keyed closure literals of one function body (nested functions aside),
/// and the functions nested in it.
fn census(body: &Block) -> (FxHashMap<Key, usize>, Vec<by_address::ByAddress<Arc<Mutex<Function>>>>) {
    let mut nested = Vec::new();
    let mut counts = FxHashMap::<Key, usize>::default();
    walk(body, &mut Vec::new(), &mut |statement, _| {
        collect_closures_in_statement(statement, &mut |closure| {
            nested.push(closure.function.clone());
            if let Some(key) = key(closure) {
                *counts.entry(key).or_default() += 1;
            }
        });
    });
    (counts, nested)
}

/// Whether some function body in `body` holds two literals of one constant.
#[cfg(debug_assertions)]
fn repeats_a_constant(body: &Block, visited: &mut FxHashSet<usize>) -> bool {
    let (counts, nested) = census(body);
    counts.values().any(|&count| count >= 2)
        || nested.into_iter().any(|function| {
            visited.insert(Arc::as_ptr(&function.0) as usize) && repeats_a_constant(&function.lock().body, visited)
        })
}

fn share_in_function(body: &mut Block, parameters: &[RcLocal], visited: &mut FxHashSet<usize>) {
    let (counts, nested) = census(body);
    // De-inline copies can share one body: each is shared once.
    for function in nested {
        if visited.insert(Arc::as_ptr(&function.0) as usize) {
            let mut function = function.lock();
            let parameters = function.parameters.clone();
            share_in_function(&mut function.body, &parameters, visited);
        }
    }
    let mut keys = counts.into_iter().filter(|&(_, count)| count >= 2).map(|(key, _)| key).collect::<Vec<_>>();
    if keys.is_empty() {
        return;
    }
    keys.sort_unstable();
    let facts = FunctionFacts::new(body, parameters);
    for key in keys {
        share_copies(body, key, &facts);
    }
}

struct Site {
    path: Vec<Step>,
    upvalues: Vec<Upvalue>,
    /// The local of `f = function() ... end`, and whether the statement
    /// declares it (`local function f`).
    bound: Option<(RcLocal, bool)>,
}

impl Site {
    /// `local function f() ... f ... end`: the closure capturing its own
    /// binding, which in the bytecode is the shared closure itself.
    fn captures_itself(&self, upvalue: &Upvalue) -> bool {
        matches!((upvalue, &self.bound), (Upvalue::Copy(local), Some((bound, _))) if local == bound)
    }
}

fn share_copies(body: &mut Block, key: Key, facts: &FunctionFacts) {
    let mut copies = Vec::new();
    walk(body, &mut Vec::new(), &mut |statement, path| {
        let bound = match statement {
            Statement::Assign(assign) if assign.left.len() == 1 && matches!(assign.right.as_slice(), [RValue::Closure(_)]) => {
                assign.left[0].as_local().map(|local| (local.clone(), assign.prefix))
            }
            _ => None,
        };
        collect_closures_in_statement(statement, &mut |closure| {
            if key_matches(closure, key) {
                copies.push(Site { path: path.to_vec(), upvalues: closure.upvalues.clone(), bound: bound.clone() });
            }
        });
    });
    // A copy bound to a local whose every read calls it cannot be compared.
    let escaping = copies.iter().filter(|copy| copy.bound.as_ref().is_none_or(|(local, _)| facts.escapes(local))).count();
    if escaping < 2 {
        return;
    }
    let Some(position) = shared_position(&copies) else {
        crate::telemetry::count("closure_identity_unshared", 1);
        return;
    };
    let first = &copies[0];
    let visible = facts.visible_at(body, &position);
    let same_captures = copies.iter().all(|copy| {
        copy.upvalues.len() == first.upvalues.len()
            && copy.upvalues.iter().zip(&first.upvalues).all(|(upvalue, expected)| {
                match (copy.captures_itself(upvalue), first.captures_itself(expected)) {
                    (true, true) => true,
                    (false, false) => upvalue == expected,
                    _ => false,
                }
            })
    });
    let stable = first.upvalues.iter().all(|upvalue| first.captures_itself(upvalue) || match upvalue {
        Upvalue::Copy(local) => !facts.declared.contains(local)
            || visible.contains(local) && facts.writes(local) == 0,
        // A reference to a local of this function would see its writes.
        Upvalue::Ref(local) => !facts.declared.contains(local),
    });
    let captures_itself = first.upvalues.iter().any(|upvalue| first.captures_itself(upvalue));
    // A copy capturing itself: the first copy's definition moves up as the
    // shared binding, and its body keeps naming that binding.
    let definition = if captures_itself { definition_start(body, first, facts) } else { None };
    if !same_captures || !stable || captures_itself && definition.is_none() {
        crate::telemetry::count("closure_identity_unshared", 1);
        return;
    }
    crate::telemetry::count("closure_identity_shared", 1);
    let (block_path, mut index) = position;
    let (shared, declaration) = if let Some(start) = definition {
        let (first_block, last) = first.path.split_at(first.path.len() - 1);
        let mut definition = Vec::new();
        with_block(body, first_block, &mut |block| definition = block.0.drain(start..=last[0].0).collect());
        if first_block == block_path {
            index = start;
        }
        let shared = first.bound.as_ref().unwrap().0.clone();
        replace_copies(body, key, &shared, &mut None);
        (shared, definition)
    } else {
        let shared = RcLocal::default();
        let mut kept = None;
        replace_copies(body, key, &shared, &mut kept);
        let mut declaration = Assign::new(vec![LValue::Local(shared.clone())], vec![kept.expect("a copy was found")]);
        declaration.prefix = true;
        (shared, vec![declaration.into()])
    };
    let mut declaration = Some(declaration);
    with_block(body, &block_path, &mut |block| {
        block.0.splice(index..index, declaration.take().unwrap());
    });
    declare_copies(body, &shared);
}

/// What a capture holds whenever a copy runs, as far as telling copies apart
/// goes: a local declared with a constant, or with a literal of a function,
/// and never written.
#[derive(Clone)]
enum Held {
    Constant(crate::Literal),
    /// A closure of this bytecode prototype.
    Closure(usize),
}

impl Held {
    /// Whether no value `self` holds is ever rawequal to one `other` holds.
    fn never_equals(&self, other: &Held) -> bool {
        use crate::Literal::*;
        match (self, other) {
            (Held::Closure(a), Held::Closure(b)) => a != b,
            (Held::Constant(a), Held::Constant(b)) => match (a, b) {
                (Nil, Nil) => false,
                (Boolean(a), Boolean(b)) => a != b,
                // `0 == -0`, and NaN equals nothing, itself included.
                (Number(a), Number(b)) => a != b,
                (Integer(a), Integer(b)) => a != b,
                (String(a), String(b)) => a != b,
                (Vector(a, b, c), Vector(x, y, z)) => a != x || b != y || c != z,
                (VectorD(a, b, c), VectorD(x, y, z)) => a != x || b != y || c != z,
                // A number and an integer, or a vector in each encoding.
                (Number(_) | Integer(_), Number(_) | Integer(_)) | (Vector(..) | VectorD(..), Vector(..) | VectorD(..)) => {
                    false
                }
                _ => true,
            },
            _ => true,
        }
    }
}

fn mark_in_function(body: &Block, parameters: &[RcLocal], visited: &mut FxHashSet<usize>, marked: &mut usize) {
    let (counts, nested) = census(body);
    for function in nested {
        if visited.insert(Arc::as_ptr(&function.0) as usize) {
            let function = function.lock();
            mark_in_function(&function.body, &function.parameters, visited, marked);
        }
    }
    let mut keys = counts.into_iter().filter(|&(_, count)| count >= 2).map(|(key, _)| key).collect::<Vec<_>>();
    if keys.is_empty() {
        return;
    }
    keys.sort_unstable();
    // Which copy runs first is read off the nesting of blocks, which a
    // `goto` would bypass.
    let mut jumps = false;
    walk(body, &mut Vec::new(), &mut |statement, _| {
        jumps |= matches!(statement, Statement::Goto(_) | Statement::Label(_));
    });
    if jumps {
        return;
    }
    let facts = FunctionFacts::new(body, parameters);
    let mut held = FxHashMap::default();
    walk(body, &mut Vec::new(), &mut |statement, _| {
        let Statement::Assign(assign) = statement else { return };
        if !assign.prefix || assign.left.len() != assign.right.len() {
            return;
        }
        for (left, right) in assign.left.iter().zip(&assign.right) {
            let Some(local) = left.as_local().filter(|local| facts.writes(local) == 0) else { continue };
            let value = match right {
                RValue::Literal(literal) => Held::Constant(literal.clone()),
                RValue::Closure(closure) => match closure.function.lock().bytecode_proto_id {
                    Some(prototype) => Held::Closure(prototype),
                    None => continue,
                },
                _ => continue,
            };
            held.insert(local.clone(), value);
        }
    });
    for key in keys {
        mark_copies(body, key, &held, marked);
    }
}

/// One closure literal loading the constant, and whether it runs every time
/// its statement does (no `and`, `or` or `if` expression arm holds it).
struct Load {
    path: Vec<Step>,
    upvalues: Vec<Upvalue>,
    function: Arc<Mutex<Function>>,
    always: bool,
}

fn mark_copies(body: &Block, key: Key, held: &FxHashMap<RcLocal, Held>, marked: &mut usize) {
    let mut loads = Vec::new();
    walk(body, &mut Vec::new(), &mut |statement, path| {
        closures_with_guards(statement, &mut |closure, always| {
            if key_matches(closure, key) {
                loads.push(Load {
                    path: path.to_vec(),
                    upvalues: closure.upvalues.clone(),
                    function: closure.function.0.clone(),
                    always,
                });
            }
        });
    });
    let Some((first, rest)) = loads.split_first() else { return };
    if !first.always
        || rest.iter().all(|load| load.upvalues == first.upvalues)
        || !rest.iter().all(|load| runs_before(body, &first.path, &load.path))
    {
        return;
    }
    let holds = |upvalue: &Upvalue| match upvalue {
        Upvalue::Copy(local) => held.get(local),
        Upvalue::Ref(_) => None,
    };
    for load in rest {
        let never_cached = load.upvalues.iter().zip(&first.upvalues).any(|(upvalue, cached)| {
            matches!((holds(upvalue), holds(cached)), (Some(value), Some(cached)) if value.never_equals(cached))
        });
        if never_cached {
            load.function.lock().closure_constant = None;
            *marked += 1;
        }
    }
}

/// Every closure literal of a statement's own values, and whether it is
/// evaluated whenever the statement is: neither the right operand of `and`
/// or `or` nor an arm of an `if` expression holds it.
fn closures_with_guards(statement: &Statement, visit: &mut impl FnMut(&Closure, bool)) {
    fn values(value: &RValue, always: bool, visit: &mut impl FnMut(&Closure, bool)) {
        match value {
            RValue::Closure(closure) => visit(closure, always),
            RValue::Binary(binary)
                if matches!(binary.operation, crate::BinaryOperation::And | crate::BinaryOperation::Or) =>
            {
                values(&binary.left, always, visit);
                values(&binary.right, false, visit);
            }
            RValue::IfExpression(expression) => {
                values(&expression.condition, always, visit);
                values(&expression.then_value, false, visit);
                values(&expression.else_value, false, visit);
            }
            _ => {
                value.visit_rvalues(&mut |child| {
                    values(child, always, visit);
                    true
                });
            }
        }
    }
    statement.visit_lvalues(&mut |lvalue| {
        lvalue.visit_rvalues(&mut |value| {
            values(value, true, visit);
            true
        })
    });
    statement.visit_rvalues(&mut |value| {
        values(value, true, visit);
        true
    });
}

/// Whether the copy at `first` runs before the one at `other` can, on every
/// path: `first` runs whenever its statement does, and either that statement
/// comes first in the innermost block holding both, reached through blocks
/// that always run (a `repeat` body, a numeric `for` that makes a trip, a
/// `while true`) with no `break` or `continue` before it, or it is the
/// condition (or the loop's values) of the statement holding `other` in a
/// block.
fn runs_before(body: &Block, first: &[Step], other: &[Step]) -> bool {
    let common = first.iter().zip(other).take_while(|(a, b)| a == b && a.1.is_some()).count();
    let (Some(&(at, child)), Some(&(other_at, other_child))) = (first.get(common), other.get(common)) else {
        return false;
    };
    let mut before = false;
    with_block_ref(body, &first[..common], &mut |block| {
        before = if at < other_at {
            always_reaches(block, &first[common..])
        } else {
            at == other_at
                && child.is_none()
                && other_child.is_some()
                && first.len() == common + 1
                && !matches!(block.0[at], Statement::Repeat(_))
        };
    });
    before
}

/// Whether running the statement `steps[0]` names in `block` always reaches
/// the copy `steps` lead to.
fn always_reaches(block: &Block, steps: &[Step]) -> bool {
    let Some((&(index, child), rest)) = steps.split_first() else { return false };
    let statement = &block.0[index];
    let Some(child) = child else {
        // The copy is in this statement's own values. A `repeat` reads its
        // condition only after a body that may break out.
        return rest.is_empty() && !matches!(statement, Statement::Repeat(_));
    };
    let enters = match statement {
        Statement::Repeat(_) => true,
        Statement::While(node) => matches!(node.condition, RValue::Literal(crate::Literal::Boolean(true))),
        Statement::NumericFor(node) => match (&node.initial, &node.limit, &node.step) {
            (
                RValue::Literal(crate::Literal::Number(initial)),
                RValue::Literal(crate::Literal::Number(limit)),
                RValue::Literal(crate::Literal::Number(step)),
            ) => (*step > 0.0 && initial <= limit) || (*step < 0.0 && initial >= limit),
            _ => false,
        },
        _ => false,
    };
    let Some(nested) = child_block(statement, child).filter(|_| enters) else { return false };
    let nested = nested.lock();
    let next = rest.first().map_or(0, |&(index, _)| index);
    !nested.0[..next].iter().any(leaves_loop) && always_reaches(&nested, rest)
}

/// Whether a statement can leave the loop around it other than by leaving
/// the function: a `break` or `continue` outside any inner loop (or a goto).
fn leaves_loop(statement: &Statement) -> bool {
    match statement {
        Statement::Break(_) | Statement::Continue(_) | Statement::Goto(_) => true,
        Statement::If(node) => {
            node.then_block.lock().0.iter().any(leaves_loop) || node.else_block.lock().0.iter().any(leaves_loop)
        }
        _ => false,
    }
}

/// Where the first copy's definition starts: its own statement for `local
/// function f`, or the bare `local f` right before `f = function`. `None`
/// when `f` has another write, which a copy reading `f` could see.
fn definition_start(body: &Block, site: &Site, facts: &FunctionFacts) -> Option<usize> {
    let (local, declares) = site.bound.as_ref()?;
    let (block_path, last) = site.path.split_at(site.path.len() - 1);
    let index = last[0].0;
    if *declares {
        return (facts.writes(local) == 0).then_some(index);
    }
    let mut start = None;
    with_block_ref(body, block_path, &mut |block| {
        if index > 0 && is_bare_declaration(&block.0[index - 1], local) && facts.writes(local) == 1 {
            start = Some(index - 1);
        }
    });
    start
}

fn is_bare_declaration(statement: &Statement, local: &RcLocal) -> bool {
    matches!(statement, Statement::Assign(assign) if assign.prefix && assign.right.is_empty()
        && matches!(assign.left.as_slice(), [LValue::Local(declared)] if declared == local))
}

/// `local f` then `f = shared`, where a copy was, as `local f = shared`.
fn declare_copies(block: &mut Block, shared: &RcLocal) {
    let mut index = 1;
    while index < block.0.len() {
        let copied = match &block.0[index] {
            Statement::Assign(assign) if !assign.prefix && matches!(assign.right.as_slice(), [RValue::Local(local)] if local == shared) => {
                assign.left.first().and_then(LValue::as_local).filter(|_| assign.left.len() == 1).cloned()
            }
            _ => None,
        };
        if let Some(local) = copied
            && is_bare_declaration(&block.0[index - 1], &local)
        {
            block.0.remove(index - 1);
            block.0[index - 1].as_assign_mut().unwrap().prefix = true;
        } else {
            index += 1;
        }
    }
    for statement in &block.0 {
        let mut child = 0;
        while let Some(nested) = child_block(statement, child) {
            declare_copies(&mut nested.lock(), shared);
            child += 1;
        }
    }
}

fn key_matches(closure: &Closure, key: Key) -> bool {
    self::key(closure) == Some(key)
}

/// The innermost block holding every copy, and the index of the first
/// statement in it that makes one.
fn shared_position(copies: &[Site]) -> Option<(Vec<Step>, usize)> {
    let first = &copies.first()?.path;
    let mut depth = 0;
    while first[depth].1.is_some() && copies.iter().all(|copy| copy.path.get(depth) == Some(&first[depth])) {
        depth += 1;
    }
    let index = copies.iter().map(|copy| copy.path[depth].0).min()?;
    Some((first[..depth].to_vec(), index))
}

/// What one function declares and writes, nested functions' writes included.
struct FunctionFacts {
    parameters: Vec<RcLocal>,
    /// Declared in this function (parameters, `local`, loop variables).
    declared: FxHashSet<RcLocal>,
    /// Writes by statements other than a declaration, here and in nested
    /// functions.
    writes: FxHashMap<RcLocal, usize>,
    /// Read as anything but a call's function.
    escaping_reads: FxHashMap<RcLocal, usize>,
}

impl FunctionFacts {
    fn new(body: &Block, parameters: &[RcLocal]) -> Self {
        let mut facts = Self {
            parameters: parameters.to_vec(),
            declared: parameters.iter().cloned().collect(),
            writes: FxHashMap::default(),
            escaping_reads: FxHashMap::default(),
        };
        walk(body, &mut Vec::new(), &mut |statement, _| {
            facts.declared.extend(declarations(statement));
        });
        facts.census(body, &mut FxHashSet::default());
        facts
    }

    /// Writes and non-call reads in `body` and every function nested in it.
    fn census(&mut self, body: &Block, visited: &mut FxHashSet<usize>) {
        let mut nested = Vec::new();
        walk(body, &mut Vec::new(), &mut |statement, _| {
            let declared = declarations(statement);
            statement.visit_local_writes(&mut |local| {
                if !declared.contains(local) {
                    *self.writes.entry(local.clone()).or_default() += 1;
                }
                true
            });
            // Reads minus the captures (the closure body's reads count) and
            // minus calls of a local.
            statement.visit_local_reads(&mut |local| {
                *self.escaping_reads.entry(local.clone()).or_default() += 1;
                true
            });
            collect_closures_in_statement(statement, &mut |closure| {
                for upvalue in &closure.upvalues {
                    let (Upvalue::Copy(local) | Upvalue::Ref(local)) = upvalue;
                    self.uncount(local);
                }
                nested.push(closure.function.clone());
            });
            if let Statement::Call(call) = statement
                && let RValue::Local(local) = &*call.value
            {
                self.uncount(local);
            }
            statement.traverse_rvalues_ref(&mut |value| {
                if let RValue::Call(call) | RValue::Select(Select::Call(call)) = value
                    && let RValue::Local(local) = &*call.value
                {
                    self.uncount(local);
                }
            });
        });
        for function in nested {
            if visited.insert(Arc::as_ptr(&function.0) as usize) {
                self.census(&function.lock().body, visited);
            }
        }
    }

    fn uncount(&mut self, local: &RcLocal) {
        if let Some(count) = self.escaping_reads.get_mut(local) {
            *count -= 1;
        }
    }

    fn writes(&self, local: &RcLocal) -> usize {
        self.writes.get(local).copied().unwrap_or(0)
    }

    fn escapes(&self, local: &RcLocal) -> bool {
        self.escaping_reads.get(local).is_some_and(|&count| count > 0)
    }

    /// Locals in scope right before statement `index` of the block at `path`.
    fn visible_at(&self, body: &Block, (path, index): &(Vec<Step>, usize)) -> FxHashSet<RcLocal> {
        let mut visible = self.parameters.iter().cloned().collect::<FxHashSet<_>>();
        let mut block_path = Vec::new();
        for &(step_index, child) in path.iter().chain([&(*index, None)]) {
            with_block_ref(body, &block_path, &mut |block| {
                for statement in &block.0[..step_index] {
                    visible.extend(declarations(statement));
                }
                // Entering a loop body brings its variables into scope.
                if child.is_some() {
                    match &block.0[step_index] {
                        Statement::NumericFor(numeric_for) => {
                            visible.insert(numeric_for.counter.clone());
                        }
                        Statement::GenericFor(generic_for) => visible.extend(generic_for.res_locals.iter().cloned()),
                        _ => {}
                    }
                }
            });
            block_path.push((step_index, child));
        }
        visible
    }
}

/// The locals a statement declares in its block (`local`), or a loop's
/// variables, which its body declares.
fn declarations(statement: &Statement) -> Vec<RcLocal> {
    match statement {
        Statement::Assign(assign) if assign.prefix => {
            assign.left.iter().filter_map(LValue::as_local).cloned().collect()
        }
        Statement::NumericFor(numeric_for) => vec![numeric_for.counter.clone()],
        Statement::GenericFor(generic_for) => generic_for.res_locals.clone(),
        _ => Vec::new(),
    }
}

/// Every statement of a function body (not of nested functions), with its path.
fn walk(block: &Block, path: &mut Vec<Step>, visit: &mut impl FnMut(&Statement, &[Step])) {
    for (index, statement) in block.0.iter().enumerate() {
        path.push((index, None));
        visit(statement, path);
        let mut child = 0;
        while let Some(nested) = child_block(statement, child) {
            path.last_mut().unwrap().1 = Some(child);
            walk(&nested.lock(), path, visit);
            child += 1;
        }
        path.pop();
    }
}

/// The blocks of a statement, by position: an `if`'s then and else blocks,
/// a loop's body.
fn child_block(statement: &Statement, child: u8) -> Option<&Arc<Mutex<Block>>> {
    match (statement, child) {
        (Statement::If(r#if), 0) => Some(&r#if.then_block),
        (Statement::If(r#if), 1) => Some(&r#if.else_block),
        (Statement::While(r#while), 0) => Some(&r#while.block),
        (Statement::Repeat(repeat), 0) => Some(&repeat.block),
        (Statement::NumericFor(numeric_for), 0) => Some(&numeric_for.block),
        (Statement::GenericFor(generic_for), 0) => Some(&generic_for.block),
        _ => None,
    }
}

fn with_block_ref(block: &Block, path: &[Step], visit: &mut impl FnMut(&Block)) {
    match path.split_first() {
        None => visit(block),
        Some((&(index, child), rest)) => {
            let nested = child_block(&block.0[index], child.unwrap()).unwrap();
            with_block_ref(&nested.lock(), rest, visit);
        }
    }
}

fn with_block(block: &mut Block, path: &[Step], visit: &mut impl FnMut(&mut Block)) {
    match path.split_first() {
        None => visit(block),
        Some((&(index, child), rest)) => {
            let nested = child_block(&block.0[index], child.unwrap()).unwrap().clone();
            with_block(&mut nested.lock(), rest, visit);
        }
    }
}

/// Every copy in the function body reads `shared`; `kept` takes the first.
fn replace_copies(block: &mut Block, key: Key, shared: &RcLocal, kept: &mut Option<RValue>) {
    for statement in &mut block.0 {
        statement.traverse_rvalues(&mut |value| {
            if let RValue::Closure(closure) = value
                && key_matches(closure, key)
            {
                let copy = std::mem::replace(value, RValue::Local(shared.clone()));
                kept.get_or_insert(copy);
            }
        });
        let mut child = 0;
        while let Some(nested) = child_block(statement, child) {
            replace_copies(&mut nested.lock(), key, shared, kept);
            child += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use by_address::ByAddress;
    use crate::{Call, Function, Global, Literal, Return, Table};

    fn closure(constant: Option<usize>, upvalues: Vec<Upvalue>) -> RValue {
        let function = Function { bytecode_proto_id: Some(1), closure_constant: constant, ..Function::default() };
        Closure { node_origin: Default::default(), function: ByAddress(Arc::new(Mutex::new(function))), upvalues }.into()
    }

    fn table(value: RValue) -> RValue {
        Table::new(vec![(Some(Literal::String(b"cb".to_vec()).into()), value)]).into()
    }

    fn print(arguments: Vec<RValue>) -> Statement {
        Call::new(RValue::Global(Global::from("print")), arguments).into()
    }

    /// `print({cb = function() end}, {cb = function() end})`: one closure.
    #[test]
    fn escaping_copies_of_one_constant_share_one_binding() {
        let mut block = Block(vec![print(vec![table(closure(Some(3), vec![])), table(closure(Some(3), vec![]))])]);
        share_closure_constants(&mut block);
        let text = block.to_string();
        assert_eq!(text.matches("function").count(), 1, "{text}");
        assert!(text.starts_with("local function"), "{text}");
    }

    /// A NEWCLOSURE copy is a new object each time, as two literals are.
    #[test]
    fn fresh_closures_stay_literals() {
        let mut block = Block(vec![print(vec![table(closure(None, vec![])), table(closure(None, vec![]))])]);
        share_closure_constants(&mut block);
        assert_eq!(block.to_string().matches("function").count(), 2);
    }

    /// Two inlined `local function f() ... f() ... end` copies, only called.
    #[test]
    fn copies_only_called_keep_their_literals() {
        let (first, second) = (RcLocal::default(), RcLocal::default());
        let define = |local: &RcLocal| -> Statement {
            let mut assign = Assign::new(vec![local.clone().into()], vec![closure(Some(0), vec![Upvalue::Copy(local.clone())])]);
            assign.prefix = true;
            assign.into()
        };
        let call = |local: &RcLocal| -> Statement { Call::new(local.clone().into(), vec![]).into() };
        // `t = f()`: a one-result call is a `Select`.
        let store = |local: &RcLocal| -> Statement {
            Assign::new(vec![RcLocal::default().into()], vec![Select::Call(Call::new(local.clone().into(), vec![])).into()]).into()
        };
        let mut block = Block(vec![define(&first), call(&first), store(&first), define(&second), store(&second)]);
        share_closure_constants(&mut block);
        assert_eq!(block.0.len(), 5);
        assert_eq!(block.to_string().matches("function").count(), 2);
    }

    /// Copies capturing different locals are one object only while the values
    /// are equal: they stay literals.
    #[test]
    fn copies_capturing_different_values_stay_literals() {
        let (a, b) = (RcLocal::default(), RcLocal::default());
        let declare = |local: &RcLocal, value: f64| -> Statement {
            let mut assign = Assign::new(vec![local.clone().into()], vec![Literal::Number(value).into()]);
            assign.prefix = true;
            assign.into()
        };
        let mut block = Block(vec![
            declare(&a, 1.0),
            declare(&b, 2.0),
            Return::new(vec![
                table(closure(Some(3), vec![Upvalue::Copy(a.clone())])),
                table(closure(Some(3), vec![Upvalue::Copy(b.clone())])),
            ]).into(),
        ]);
        share_closure_constants(&mut block);
        assert_eq!(block.to_string().matches("function").count(), 2);
    }

    /// The same capture, declared before both copies and never written: the
    /// binding goes after that declaration, in the block holding both copies.
    #[test]
    fn shared_binding_follows_its_captures() {
        let value = RcLocal::default();
        let mut declaration = Assign::new(vec![value.clone().into()], vec![Literal::Number(1.0).into()]);
        declaration.prefix = true;
        let copy = || table(closure(Some(3), vec![Upvalue::Copy(value.clone())]));
        let mut block = Block(vec![
            declaration.into(),
            crate::If::new(Literal::Boolean(true).into(), Block(vec![print(vec![copy()])]), Block(vec![print(vec![copy()])])).into(),
        ]);
        share_closure_constants(&mut block);
        assert_eq!(block.0.len(), 3);
        assert!(matches!(&block.0[1], Statement::Assign(assign) if assign.prefix && matches!(assign.right[0], RValue::Closure(_))));
        assert_eq!(block.to_string().matches("function").count(), 1);
    }

    /// Two inlined `local function f() ... f ... end` copies that escape: the
    /// first definition is the shared closure, the second copy reads it. The
    /// lifter writes such a definition as `local f` then `f = function`.
    #[test]
    fn escaping_recursive_copies_share_the_first_binding() {
        for separate_declaration in [false, true] {
            let (first, second) = (RcLocal::default(), RcLocal::default());
            let define = |local: &RcLocal| -> Vec<Statement> {
                let mut assign = Assign::new(vec![local.clone().into()], vec![closure(Some(0), vec![Upvalue::Copy(local.clone())])]);
                if !separate_declaration {
                    assign.prefix = true;
                    return vec![assign.into()];
                }
                let mut declaration = Assign::new(vec![local.clone().into()], vec![]);
                declaration.prefix = true;
                vec![declaration.into(), assign.into()]
            };
            let mut block = Block([define(&first), vec![print(vec![first.clone().into()])], define(&second),
                vec![print(vec![second.clone().into()])]].concat());
            share_closure_constants(&mut block);
            let text = block.to_string();
            assert_eq!(text.matches("function").count(), 1, "{text}");
            // The second copy is `local second = first`, right before its use.
            let copy = block.0.iter().position(|statement| matches!(statement, Statement::Assign(assign)
                if assign.prefix && assign.right == [RValue::Local(first.clone())])).expect(&text);
            assert!(matches!(&block.0[copy + 1], Statement::Call(_)), "{text}");
            assert_eq!(block.0.len(), copy + 2, "{text}");
        }
    }

    /// The gate: a prototype loading one constant twice, or one literal two
    /// places of the tree share (a pass copied it), lets the walk run; a
    /// constant loaded once and held once skips it.
    #[test]
    fn loaded_constants_skip_the_walk_only_without_copies() {
        let function = || Arc::new(Mutex::new(Function { bytecode_proto_id: Some(1), closure_constant: Some(3), ..Function::default() }));
        let literal = |function: &Arc<Mutex<Function>>| -> RValue {
            Closure { node_origin: Default::default(), function: ByAddress(function.clone()), upvalues: vec![] }.into()
        };
        let (first, second) = (function(), function());
        let mut loaded = LoadedConstants::default();
        loaded.record_prototype([&first, &second].into_iter());
        assert!(loaded.may_repeat(), "one prototype loads slot 3 twice");

        // The test's own handle is dropped: only the list and the tree hold
        // a function, as in the pipeline.
        let copied = function();
        let mut loaded = LoadedConstants::default();
        loaded.record_prototype(std::iter::once(&copied));
        let mut block = Block(vec![print(vec![table(literal(&copied)), table(literal(&copied))])]);
        drop(copied);
        assert!(loaded.may_repeat(), "two places of the tree share one literal's function");
        loaded.share(&mut block);
        assert_eq!(block.to_string().matches("function").count(), 1, "{block}");

        let once = function();
        let mut loaded = LoadedConstants::default();
        loaded.record_prototype(std::iter::once(&once));
        let mut block = Block(vec![print(vec![table(literal(&once))])]);
        drop(once);
        assert!(!loaded.may_repeat());
        loaded.share(&mut block);
        assert_eq!(block.0.len(), 1);
    }

    /// A capture written after its declaration may differ between copies.
    #[test]
    fn written_capture_stays_literals() {
        let value = RcLocal::default();
        let mut declaration = Assign::new(vec![value.clone().into()], vec![Literal::Number(1.0).into()]);
        declaration.prefix = true;
        let copy = || table(closure(Some(3), vec![Upvalue::Copy(value.clone())]));
        let mut block = Block(vec![
            declaration.into(),
            print(vec![copy()]),
            Assign::new(vec![value.clone().into()], vec![Literal::Number(2.0).into()]).into(),
            print(vec![copy()]),
        ]);
        share_closure_constants(&mut block);
        assert_eq!(block.to_string().matches("function").count(), 2);
    }

    /// `local function h() end` for a prototype, then `t = <copy of K
    /// capturing h>`: one loop body of `bindk(h)` -O2 inlined.
    fn bound_copy(prototype: usize) -> (Block, Arc<Mutex<Function>>) {
        let h = RcLocal::default();
        let bound = Function { bytecode_proto_id: Some(prototype), ..Function::default() };
        let bound = Closure { node_origin: Default::default(), function: ByAddress(Arc::new(Mutex::new(bound))), upvalues: vec![] };
        let mut declaration = Assign::new(vec![h.clone().into()], vec![bound.into()]);
        declaration.prefix = true;
        let copy = Function { bytecode_proto_id: Some(1), closure_constant: Some(3), ..Function::default() };
        let copy = Arc::new(Mutex::new(copy));
        let literal = Closure { node_origin: Default::default(), function: ByAddress(copy.clone()), upvalues: vec![Upvalue::Copy(h)] };
        (Block(vec![declaration.into(), print(vec![literal.into()])]), copy)
    }

    fn numeric_for(limit: RValue, body: Block) -> Statement {
        crate::NumericFor::new(Literal::Number(1.0).into(), limit, Literal::Number(1.0).into(), RcLocal::default(), body).into()
    }

    /// `for i = 1, 2 do <bindk(h1)> end; repeat <bindk(h2)> until true`: the
    /// first copy fills the constant's cache with `h1`; `h2`, a closure of
    /// another function, never equals it, so the second copy is made anew.
    #[test]
    fn a_copy_another_copy_cached_differently_is_made_anew() {
        let loaded = LoadedConstants { repeated: true, functions: Vec::new() };
        let (first_body, first) = bound_copy(2);
        let (second_body, second) = bound_copy(3);
        let block = Block(vec![
            numeric_for(Literal::Number(2.0).into(), first_body),
            crate::Repeat::new(Literal::Boolean(true).into(), second_body).into(),
        ]);
        assert_eq!(loaded.mark_uncached_copies(&block), 1);
        assert_eq!(first.lock().closure_constant, Some(3));
        assert_eq!(second.lock().closure_constant, None);
    }

    /// Unmarked: the first copy in a loop that may make no trip (no copy is
    /// known to run first), copies of one prototype's closures, or no
    /// repeated load at all.
    #[test]
    fn a_copy_may_share_when_the_first_run_or_the_values_are_unknown() {
        let unknown_trips = || {
            let (first_body, _) = bound_copy(2);
            let (second_body, second) = bound_copy(3);
            let block = Block(vec![numeric_for(RcLocal::default().into(), first_body), second_body.0[0].clone(), second_body.0[1].clone()]);
            (block, second)
        };
        let loaded = LoadedConstants { repeated: true, functions: Vec::new() };
        let (block, second) = unknown_trips();
        assert_eq!(loaded.mark_uncached_copies(&block), 0);
        assert_eq!(second.lock().closure_constant, Some(3));

        let (first_body, _) = bound_copy(2);
        let (second_body, second) = bound_copy(2);
        let block = Block(vec![numeric_for(Literal::Number(2.0).into(), first_body), numeric_for(Literal::Number(2.0).into(), second_body)]);
        assert_eq!(loaded.mark_uncached_copies(&block), 0);
        assert_eq!(second.lock().closure_constant, Some(3));

        let (first_body, _) = bound_copy(2);
        let (second_body, second) = bound_copy(3);
        let block = Block(vec![numeric_for(Literal::Number(2.0).into(), first_body), numeric_for(Literal::Number(2.0).into(), second_body)]);
        assert_eq!(LoadedConstants::default().mark_uncached_copies(&block), 0);
        assert_eq!(second.lock().closure_constant, Some(3));
    }
}
