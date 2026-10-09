use std::fmt;

use by_address::ByAddress;
use parking_lot::Mutex;
use triomphe::Arc;

use crate::{
    Block, Literal, LocalRw, RcLocal, Reduce, SideEffects, Traverse, Type,
    formatter::Formatter,
    type_system::{Infer, TypeSystem},
};

#[derive(Debug, PartialEq, Clone)]
pub enum Upvalue {
    Copy(RcLocal),
    Ref(RcLocal),
}

#[derive(Default, Debug, PartialEq, Clone)]
pub struct Function {
    /// Index of the Luau bytecode prototype this function was lifted from.
    ///
    /// The same prototype can be instantiated at several closure sites (most
    /// notably after `-O2` inlining), so pointer identity is not sufficient to
    /// recognise equivalent closure constructors. Keeping the prototype id lets
    /// the de-inliner compare those constructors exactly, together with their
    /// capture modes and mapped upvalues. Functions synthesized by AST cleanup
    /// passes deliberately leave this as `None`.
    pub bytecode_proto_id: Option<usize>,
    /// Deterministic static occurrence id inside one immutable analysis artifact.
    ///
    /// Unlike `bytecode_proto_id`, this distinguishes multiple closure sites that
    /// instantiate the same prototype. Synthetic functions leave it as `None`.
    pub bytecode_function_id: Option<String>,
    /// The parent prototype's constant a DUPCLOSURE loaded this closure from.
    /// Every load of one constant yields the same closure object while the
    /// captured values stay rawequal, so copies `-O2` inlining made of one
    /// literal are one object ([`crate::closure_identity`]). `None` for
    /// NEWCLOSURE (a new object each time) and synthesized functions.
    pub closure_constant: Option<usize>,
    /// Immutable bytecode hint: retain the local closure binder through SSA so
    /// the module reconstruction pass can inspect it after child bodies exist.
    /// This is only an inlining refusal, never a semantic equivalence proof.
    pub retain_for_reconstruction: bool,
    /// The SSA inliner proved this closure may move into the one store that
    /// reads its binder, a field or global of the function's own name (`M.F =
    /// F` -> `function M.F`), and left it in place: the statement de-inliner
    /// may still rebuild calls of `F` first. [`crate::fold_function_names`]
    /// makes the move afterwards where `F` gained no other read.
    pub named_store_fold: bool,
    /// Line info shows this prototype's code inside another function: Luau
    /// `-O2` inlined it there. Only then does the SSA inliner leave a
    /// function-name fold to [`crate::fold_function_names`]; an evidence
    /// hint, never a proof.
    pub inlined_by_compiler: bool,
    /// The source gave the function the `@native` attribute (the prototype's
    /// `LPF_NATIVE_FUNCTION` flag); printed back before its `function`.
    pub native: bool,
    pub name: Option<String>,
    pub parameters: Vec<RcLocal>,
    /// Source-recoverable Luau type annotation per parameter (aligned with
    /// `parameters`, `self` included), rendered from the compiler's bytecode
    /// type information.  `None` when the bytecode records no exact type.
    pub parameter_annotations: Vec<Option<String>>,
    /// Naming hint per parameter derived from its bytecode type (for example
    /// `cframe` for a tagged `CFrame`), consulted only when usage-based naming
    /// found nothing better.
    pub parameter_name_hints: Vec<Option<String>>,
    pub is_variadic: bool,
    pub body: Block,
}

#[derive(PartialEq, Clone)]
pub struct Closure {
    pub node_origin: crate::node_origins::Origin,
    pub function: ByAddress<Arc<Mutex<Function>>>,
    pub upvalues: Vec<Upvalue>,
}

impl Reduce for Closure {
    fn reduce(self) -> crate::RValue {
        self.into()
    }

    fn reduce_condition(self) -> crate::RValue {
        Literal::Boolean(true).into()
    }
}

impl Infer for Closure {
    fn infer<'a: 'b, 'b>(&'a mut self, _system: &mut TypeSystem<'b>) -> Type {
        todo!()
        // let return_values = system.analyze_block(&mut self.body);
        // let parameters = self
        //     .parameters
        //     .iter_mut()
        //     .map(|l| l.infer(system))
        //     .collect_vec();

        // Type::Function(parameters, return_values)
    }
}

impl fmt::Display for Closure {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Default::default(),
            position_query: None,
            closure_observer: None,
            emission_map: None,
            layout_budget: None,
            compact_annotations: false,
            inferred_calls: Default::default(),
        }
        .format_closure(self)
    }
}

impl LocalRw for Closure {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.upvalues.iter().all(|upvalue| match upvalue {
            Upvalue::Copy(local) | Upvalue::Ref(local) => visit(local),
        })
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.upvalues.iter_mut().all(|upvalue| match upvalue {
            Upvalue::Copy(local) | Upvalue::Ref(local) => visit(local),
        })
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }
}

impl SideEffects for Closure {}

impl Traverse for Closure {}

crate::node_origins::semantic_debug!(Closure; function,upvalues);
