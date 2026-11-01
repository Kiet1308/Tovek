//! Keep an actual builtin call actual when recompiling the plain spelling at the
//! input's optimization level would not: Luau folds a builtin call whose
//! arguments are all constants, and turns a plain call it recognizes into a
//! FASTCALL. Either bypasses a builtin the environment replaced, so such a
//! call prints with a parenthesized callee (`(math.abs)(-17)`), which Luau
//! neither folds nor fast-calls. Every other call keeps its source spelling.
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{deinline_safety::CaptureSafety, Block, Call, LValue, Literal, RValue, Select, Statement, Traverse};

/// `input_fastcalls`: the chunk holds a FASTCALL, so its compiler fast-called
/// builtins and a plain call without one was not recognized as a builtin.
/// Run after callee/argument motion, before constant spelling (whose
/// constructor calls stay foldable on purpose).
pub fn preserve_builtin_calls(block: &mut Block, assume_standard_libraries: bool, input_fastcalls: bool) -> usize {
    if assume_standard_libraries { return 0; }
    let safety = CaptureSafety::writes_for_read_timing(block);
    let mut census = Census::default();
    census.block(block);
    census.finish();
    let mut state = State { safety: &safety, aliases: FxHashMap::default(), constants: FxHashSet::default(),
        parameters: FxHashMap::default(), visited: FxHashSet::default(), changed: 0, input_fastcalls, census };
    state.block(block);
    state.changed
}

/// What the whole chunk shows before any call is judged.
#[derive(Default)]
struct Census {
    /// Callee paths some call reaches through a FASTCALL: builtins of the
    /// input's compiler, including a configured vector constructor (`Vector3.new`).
    /// Whether the compiler folds it: only its vector constructor (builtin 54).
    fast_paths: FxHashMap<String, bool>,
    /// `local function f`, by local, and per function which parameters some
    /// call passes a literal: Luau -O2 inlines such a call and folds a builtin
    /// call of that parameter (`math.abs(p)`) the input's copy still made.
    /// Each with whether only de-inlined calls pass one ([`State::parameters`]).
    functions: FxHashMap<u64, usize>,
    calls: Vec<(u64, Vec<bool>, bool)>,
    literal_parameters: FxHashMap<usize, Vec<(usize, bool)>>,
    /// The functions of `functions`: only those Luau inlines, compiling the
    /// calls of their nested functions in the copy.
    declared: FxHashSet<usize>,
}

impl Census {
    fn literal(value: &RValue) -> bool {
        match value {
            RValue::Literal(_) => true,
            RValue::Unary(_) | RValue::Binary(_) => value.rvalues().into_iter().all(Self::literal),
            _ => false,
        }
    }

    fn call(&mut self, call: &Call) {
        if call.builtin_fastcall.is_some() && fast_builtin_shape(&call.value) {
            let vector = matches!(call.builtin_fastcall, Some((54, _)));
            *self.fast_paths.entry(call.value.to_string()).or_default() |= vector;
        }
        if let RValue::Local(local) = &*call.value {
            self.calls.push((local.stable_id(), call.arguments.iter().map(Self::literal).collect(), call.is_inferred()));
        }
    }

    fn value(&mut self, value: &RValue) {
        match value {
            RValue::Closure(closure) => self.block(&closure.function.lock().body),
            RValue::Call(call) | RValue::Select(Select::Call(call)) => self.call(call),
            _ => {}
        }
        value.visit_rvalues(&mut |child| { self.value(child); true });
    }

    fn block(&mut self, block: &Block) {
        for statement in &block.0 {
            if let Statement::Assign(assign) = statement
                && let ([LValue::Local(local)], [RValue::Closure(closure)]) = (assign.left.as_slice(), assign.right.as_slice())
            {
                self.functions.insert(local.stable_id(), (&*closure.function.0 as *const _) as usize);
            }
            if let Statement::Call(call) = statement { self.call(call); }
            statement.visit_lvalues(&mut |left| left.visit_rvalues(&mut |value| { self.value(value); true }));
            statement.visit_rvalues(&mut |value| { self.value(value); true });
            match statement {
                Statement::If(node) => {
                    self.block(&node.then_block.lock());
                    self.block(&node.else_block.lock());
                }
                Statement::While(node) => self.block(&node.block.lock()),
                Statement::Repeat(node) => self.block(&node.block.lock()),
                Statement::NumericFor(node) => self.block(&node.block.lock()),
                Statement::GenericFor(node) => self.block(&node.block.lock()),
                _ => {}
            }
        }
    }

    /// Index, per function identity, the parameters some call passes a literal.
    fn finish(&mut self) {
        let mut literal: FxHashMap<(usize, usize), bool> = FxHashMap::default();
        for (callee, literals, inferred) in std::mem::take(&mut self.calls) {
            if let Some(&identity) = self.functions.get(&callee) {
                for index in literals.iter().enumerate().filter(|(_, literal)| **literal).map(|(index, _)| index) {
                    *literal.entry((identity, index)).or_insert(inferred) &= inferred;
                }
            }
        }
        for ((identity, index), inferred_only) in literal {
            self.literal_parameters.entry(identity).or_default().push((index, inferred_only));
        }
        self.declared = self.functions.values().copied().collect();
    }
}

/// A global or a constant-key field chain (`Vector3.new`), as a FASTCALL fallback loads.
fn fast_builtin_shape(value: &RValue) -> bool {
    match value {
        RValue::Global(_) => true,
        RValue::Index(index) => matches!(&*index.right, RValue::Literal(Literal::String(_))) && fast_builtin_shape(&index.left),
        _ => false,
    }
}

/// Builtins the pinned compiler (Luau c2ec0d4, `Compiler/src/Builtins.cpp`)
/// fast-calls and may constant-fold.
fn fast_builtin(library: Option<&[u8]>, name: &[u8]) -> bool {
    match library {
        None => matches!(name, b"assert" | b"type" | b"typeof" | b"rawset" | b"rawget" | b"rawequal" | b"rawlen"
            | b"unpack" | b"select" | b"getmetatable" | b"setmetatable" | b"tonumber" | b"tostring"),
        Some(b"math") => matches!(name, b"abs" | b"acos" | b"asin" | b"atan2" | b"atan" | b"ceil" | b"cosh" | b"cos"
            | b"deg" | b"exp" | b"floor" | b"fmod" | b"frexp" | b"ldexp" | b"log10" | b"log" | b"max" | b"min"
            | b"modf" | b"pow" | b"rad" | b"sinh" | b"sin" | b"sqrt" | b"tanh" | b"tan" | b"clamp" | b"sign"
            | b"round" | b"lerp" | b"isnan" | b"isinf" | b"isfinite"),
        Some(b"bit32") => matches!(name, b"arshift" | b"band" | b"bnot" | b"bor" | b"bxor" | b"btest" | b"extract"
            | b"lrotate" | b"lshift" | b"replace" | b"rrotate" | b"rshift" | b"countlz" | b"countrz" | b"byteswap"),
        Some(b"string") => matches!(name, b"byte" | b"char" | b"len" | b"sub"),
        Some(b"table") => matches!(name, b"insert" | b"unpack"),
        Some(b"buffer") => matches!(name, b"readi8" | b"readu8" | b"writei8" | b"writeu8" | b"readi16" | b"readu16"
            | b"writei16" | b"writeu16" | b"readi32" | b"readu32" | b"writei32" | b"writeu32" | b"readf32"
            | b"writef32" | b"readf64" | b"writef64" | b"readinteger" | b"writeinteger"),
        Some(b"vector") => matches!(name, b"create" | b"magnitude" | b"normalize" | b"cross" | b"dot" | b"floor"
            | b"ceil" | b"abs" | b"sign" | b"clamp" | b"min" | b"max" | b"lerp"),
        Some(_) => false,
    }
}

/// A global path (`math.max`, `type`) the compiler may fast-call.
pub(crate) fn fast_builtin_path(value: &RValue) -> bool {
    match value {
        RValue::Global(global) => fast_builtin(None, &global.0),
        RValue::Index(index) => matches!((&*index.left, &*index.right),
            (RValue::Global(library), RValue::Literal(Literal::String(name))) if fast_builtin(Some(&library.0), name)),
        _ => false,
    }
}

/// Builtins the pinned compiler constant-folds (`Compiler/src/BuiltinFolding.cpp`).
fn foldable_builtin(library: Option<&[u8]>, name: &[u8]) -> bool {
    match library {
        None => matches!(name, b"type" | b"typeof"),
        Some(b"math") => !matches!(name, b"frexp" | b"modf"),
        Some(b"bit32") => !matches!(name, b"countlz" | b"countrz" | b"byteswap"),
        Some(b"string") => true,
        _ => false,
    }
}

const LIBRARIES: [&[u8]; 6] = [b"math", b"bit32", b"string", b"table", b"buffer", b"vector"];

/// Where a constant of an inlined copy comes from, strongest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source { Literal, Parameter, Inferred }

#[derive(Clone, Copy, PartialEq)]
enum Builtin { Library(&'static [u8]), Function { foldable: bool } }

struct State<'a> {
    safety: &'a CaptureSafety,
    /// Locals bound once to a builtin or library (`local max = math.max`), which
    /// Luau resolves like the global path.
    aliases: FxHashMap<u64, Builtin>,
    /// Locals declared with a constant and never assigned again, which Luau
    /// propagates into a call it then folds.
    constants: FxHashSet<u64>,
    /// Parameters some call passes a literal ([`Census::literal_parameters`]),
    /// constants of the copy Luau inlines for it, and whether only de-inlined
    /// calls pass one. Such a call's copy kept every builtin call of the
    /// constant the de-inliner's specialization does not fold (`math.abs(p)`
    /// matched as `math.abs(-3)`), and folded those it does
    /// (`math.pow(10, p)` matched as `100`), which recompiling folds alike.
    parameters: FxHashMap<u64, bool>,
    visited: FxHashSet<usize>,
    changed: usize,
    input_fastcalls: bool,
    census: Census,
}

impl State<'_> {
    fn builtin(&self, value: &RValue) -> Option<Builtin> {
        match value {
            RValue::Global(global) => {
                let name = global.0.as_slice();
                if let Some(library) = LIBRARIES.iter().find(|library| **library == name) {
                    Some(Builtin::Library(library))
                } else {
                    fast_builtin(None, name).then_some(Builtin::Function { foldable: foldable_builtin(None, name) })
                }
            }
            RValue::Local(local) => self.aliases.get(&local.stable_id()).copied(),
            RValue::Index(index) => match (self.builtin(&index.left), &*index.right) {
                (Some(Builtin::Library(library)), RValue::Literal(Literal::String(name)))
                    if fast_builtin(Some(library), name) => Some(Builtin::Function { foldable: foldable_builtin(Some(library), name) }),
                _ => None,
            },
            // Luau also resolves `local math = math or replacement` as the library.
            RValue::Binary(binary) if binary.operation == crate::BinaryOperation::Or
                && matches!(self.builtin(&binary.left), Some(Builtin::Library(_))) => self.builtin(&binary.left),
            _ => None,
        }
    }

    /// Whether `value` is constant in an inlined copy, and from what: the
    /// weakest source among the locals it reads.
    fn constant(&self, value: &RValue) -> Option<Source> {
        match value {
            RValue::Literal(_) => Some(Source::Literal),
            RValue::Local(local) if self.constants.contains(&local.stable_id()) => Some(Source::Literal),
            RValue::Local(local) => self.parameters.get(&local.stable_id())
                .map(|&inferred_only| if inferred_only { Source::Inferred } else { Source::Parameter }),
            RValue::Unary(_) | RValue::Binary(_) => {
                value.rvalues().into_iter().try_fold(Source::Literal, |weakest, child| Some(weakest.max(self.constant(child)?)))
            }
            _ => None,
        }
    }

    fn call(&mut self, call: &mut Call) {
        // The configured vector constructor keeps its own spelling policy.
        if call.group_callee || matches!(call.builtin_fastcall, Some((54, _))) { return; }
        let foldable = match self.builtin(&call.value) {
            Some(Builtin::Function { foldable }) => foldable,
            _ => match self.census.fast_paths.get(&call.value.to_string()) {
                Some(&foldable) => foldable,
                None => return,
            },
        };
        // Luau fast-calls `select` only as `select(n, ...)`.
        let fast_select = !matches!(&*call.value, RValue::Global(global) if global.0 == b"select")
            || matches!(call.arguments.as_slice(), [_, RValue::VarArg(_) | RValue::Select(Select::VarArg(_))]);
        let gains_fastcall = self.input_fastcalls && call.builtin_fastcall.is_none() && fast_select;
        let weakest = if crate::deinline::fold::specialization_folds(call) { Source::Parameter } else { Source::Inferred };
        let folds = foldable && !call.arguments.is_empty()
            && call.arguments.iter().all(|argument| self.constant(argument).is_some_and(|source| source <= weakest));
        if gains_fastcall || folds {
            call.group_callee = true;
            self.changed += 1;
        }
    }

    fn value(&mut self, value: &mut RValue) {
        if let RValue::Closure(closure) = value {
            let identity = (&*closure.function.0 as *const _) as usize;
            if self.visited.insert(identity) {
                // A function Luau never inlines is compiled once, before any
                // copy: it reads an enclosing parameter as an upvalue, which
                // no constant argument reaches.
                let enclosing = (!self.census.declared.contains(&identity)).then(|| std::mem::take(&mut self.parameters));
                let mut function = closure.function.lock();
                for &(index, inferred_only) in self.census.literal_parameters.get(&identity).into_iter().flatten() {
                    if let Some(parameter) = function.parameters.get(index)
                        && (self.safety.never_reassigned(parameter) || !self.safety.complete())
                    {
                        self.parameters.insert(parameter.stable_id(), inferred_only);
                    }
                }
                self.block(&mut function.body);
                if let Some(enclosing) = enclosing {
                    self.parameters = enclosing;
                }
            }
            return;
        }
        if let RValue::Call(call) | RValue::Select(Select::Call(call)) = value { self.call(call); }
        value.visit_rvalues_mut(&mut |child| { self.value(child); true });
    }

    fn block(&mut self, block: &mut Block) {
        for statement in &mut block.0 {
            if let Statement::Assign(assign) = &*statement {
                for (left, right) in assign.left.iter().zip(&assign.right) {
                    let LValue::Local(local) = left else { continue };
                    // An unproven census (exhausted budget) counts as constant: grouping is always exact.
                    let unchanged = self.safety.never_reassigned(local) || !self.safety.complete();
                    if let Some(builtin) = self.builtin(right) && unchanged {
                        self.aliases.insert(local.stable_id(), builtin);
                    }
                    // A constant computed from a parameter is that parameter's.
                    match self.constant(right).filter(|_| assign.prefix && unchanged) {
                        Some(Source::Literal) => { self.constants.insert(local.stable_id()); }
                        Some(source) => { self.parameters.insert(local.stable_id(), source == Source::Inferred); }
                        None => {}
                    }
                }
            }
            if let Statement::Call(call) = statement { self.call(call); }
            statement.visit_lvalues_mut(&mut |left| {
                left.visit_rvalues_mut(&mut |value| { self.value(value); true }); true
            });
            statement.visit_rvalues_mut(&mut |value| { self.value(value); true });
            match statement {
                Statement::If(node) => {
                    self.block(&mut node.then_block.lock());
                    self.block(&mut node.else_block.lock());
                }
                Statement::While(node) => self.block(&mut node.block.lock()),
                Statement::Repeat(node) => self.block(&mut node.block.lock()),
                Statement::NumericFor(node) => self.block(&mut node.block.lock()),
                Statement::GenericFor(node) => self.block(&mut node.block.lock()),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Global, Index, Local, RcLocal, Return};

    fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
    fn number(n: f64) -> RValue { Literal::Number(n).into() }
    fn member(library: &str, name: &str) -> RValue {
        Index::new(Global::from(library).into(), Literal::String(name.as_bytes().to_vec()).into()).into()
    }
    fn declaration(local: &RcLocal, value: RValue) -> Statement {
        let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
        assign.prefix = true;
        assign.into()
    }
    fn fast(mut call: Call) -> Call {
        call.builtin_fastcall = Some((1, 2));
        call
    }

    #[test]
    fn groups_only_calls_recompilation_would_fold_or_fast_call() {
        let constant = local("constant");
        let changed = local("changed");
        let value = local("value");
        let mut body = Block(vec![
            declaration(&constant, number(-17.0)),
            declaration(&changed, number(1.0)),
            Assign::new(vec![changed.clone().into()], vec![number(2.0)]).into(),
            Return::new(vec![
                fast(Call::new(member("math", "abs"), vec![number(-17.0)])).into(),
                fast(Call::new(member("math", "abs"), vec![constant.clone().into()])).into(),
                fast(Call::new(member("math", "abs"), vec![changed.clone().into()])).into(),
                fast(Call::new(member("math", "sin"), vec![value.clone().into()])).into(),
                Call::new(Global::from("type").into(), vec![value.clone().into()]).into(),
                Call::new(member("string", "format"), vec![Literal::String(b"%d".to_vec()).into(), number(1.0)]).into(),
                Call::new(member("math", "random"), vec![number(1.0), number(6.0)]).into(),
                Call::new(Global::from("select").into(), vec![number(1.0), value.clone().into()]).into(),
                Call::new(member("buffer", "readstring"), vec![value.clone().into(), number(0.0), number(4.0)]).into(),
            ]).into(),
        ]);
        assert_eq!(preserve_builtin_calls(&mut body, false, true), 3);
        let output = body.to_string();
        for expected in ["(math.abs)(-17)", "(math.abs)(constant)", "math.abs(changed)", "math.sin(value)",
            "(type)(value)", "string.format(\"%d\", 1)", "math.random(1, 6)", "select(1, value)",
            "buffer.readstring(value, 0, 4)"] {
            assert!(output.contains(expected), "{expected} in {output}");
        }
        assert_eq!(preserve_builtin_calls(&mut body, false, true), 0);
    }

    #[test]
    fn literal_arguments_reach_their_helper_and_observed_fast_paths_count() {
        let (helper, parameter, value) = (local("helper"), local("p"), local("value"));
        let mut function = crate::Function::default();
        function.parameters = vec![parameter.clone()];
        function.body = Block(vec![Return::new(vec![fast(Call::new(member("math", "abs"), vec![parameter.into()])).into()]).into()]);
        let closure = crate::Closure { node_origin: Default::default(), upvalues: vec![],
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(function))) };
        let mut body = Block(vec![
            declaration(&helper, closure.into()),
            Return::new(vec![
                Call::new(helper.into(), vec![number(-3.0)]).into(),
                fast(Call::new(member("Vector3", "new"), vec![value.clone().into()])).into(),
                Call::new(member("Vector3", "new"), vec![value.into()]).into(),
            ]).into(),
        ]);
        assert_eq!(preserve_builtin_calls(&mut body, false, true), 2);
        let output = body.to_string();
        assert!(output.contains("return (math.abs)(p)"), "{output}");
        assert!(output.contains("Vector3.new(value), (Vector3.new)(value)"), "{output}");
    }

    #[test]
    fn de_inlined_literals_group_what_their_copies_kept_and_never_reach_anonymous_functions() {
        use crate::call_origins::Kind;
        let helper = |body: Vec<Statement>, parameter: &RcLocal| {
            let mut function = crate::Function::default();
            function.parameters = vec![parameter.clone()];
            function.body = Block(body);
            RValue::from(crate::Closure { node_origin: Default::default(), upvalues: vec![],
                function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(function))) })
        };
        let (abs, pow, nested, written) = (local("abs"), local("pow"), local("nested"), local("written"));
        let (p, q, r, s) = (local("p"), local("q"), local("r"), local("s"));
        let returns = |call: Call| -> Vec<Statement> { vec![Return::new(vec![call.into()]).into()] };
        let anonymous = helper(returns(fast(Call::new(member("math", "abs"), vec![r.clone().into()]))), &local("unused"));
        let mut body = Block(vec![
            // A copy matched with `math.abs(-3)` kept the call: grouped.
            declaration(&abs, helper(returns(fast(Call::new(member("math", "abs"), vec![p.clone().into()]))), &p)),
            // A copy matched with `100` folded it, as recompiling does.
            declaration(&pow, helper(returns(fast(Call::new(member("math", "pow"), vec![number(10.0), q.clone().into()]))), &q)),
            // A function expression is compiled once: `r` stays an upvalue.
            declaration(&nested, helper(vec![Call::new(Global::from("pcall").into(), vec![anonymous]).into()], &r)),
            // A plain call passing a literal groups even a power.
            declaration(&written, helper(returns(fast(Call::new(member("math", "pow"), vec![number(10.0), s.clone().into()]))), &s)),
            Return::new(vec![
                Call::new(abs.into(), vec![number(-3.0)]).reconstructed(Kind::StatementDeinline).into(),
                Call::new(pow.into(), vec![number(2.0)]).reconstructed(Kind::StatementDeinline).into(),
                Call::new(nested.into(), vec![number(-3.0)]).into(),
                Call::new(written.into(), vec![number(2.0)]).into(),
            ]).into(),
        ]);
        assert_eq!(preserve_builtin_calls(&mut body, false, true), 2);
        let output = body.to_string();
        for expected in ["return (math.abs)(p)", "return math.pow(10, q)", "return math.abs(r)", "return (math.pow)(10, s)"] {
            assert!(output.contains(expected), "{expected} in {output}");
        }
    }

    #[test]
    fn plain_calls_of_a_chunk_without_fastcalls_stay_plain() {
        let value = local("value");
        let mut body = Block(vec![Return::new(vec![Call::new(Global::from("type").into(), vec![value.into()]).into()]).into()]);
        assert_eq!(preserve_builtin_calls(&mut body, false, false), 0);
        assert_eq!(preserve_builtin_calls(&mut body, true, true), 0);
        assert!(body.to_string().contains("type(value)"));
    }
}
