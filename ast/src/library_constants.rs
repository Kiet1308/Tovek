//! Friendly spellings for constants the compiler folded out of a library
//! (opt-in: `--assume-standard-libraries`).
//!
//! At `-O2` the Luau compiler turns `math.pi` and `math.huge` into number
//! constants, and with Roblox's vector options `Vector3.new(1, 2, 3)` into a
//! vector constant. It does so only while the script never writes that global
//! and never calls `getfenv`/`setfenv`. When the bytecode shows the same holds
//! for the whole chunk, the library spelling compiles back to the identical
//! constant under the compiler's own assumption, so it replaces the raw value.
//! The spelling still reads the environment when it runs, which another
//! script can replace (`getfenv(f).math = ...`), so the exact default keeps
//! the literal:
//!
//! ```lua
//! local angle = 3.141592653589793 * t     -->  local angle = math.pi * t
//! part.Size = createVector(4, 1, 2)       -->  part.Size = Vector3.new(4, 1, 2)
//! ```
//!
//! Otherwise the exact literal (`1e999`, `vector.create`) stays.

use crate::{Block, Call, Global, Index, LValue, Literal, LocalRw, RValue, Statement, Traverse, Unary, UnaryOperation};

/// Libraries whose folded constants may be spelled through the library.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Libraries {
    /// `math.pi` and `math.huge`.
    pub math: bool,
    /// `Vector3.new(x, y, z)` for vector constants.
    pub vector3: bool,
}

impl Libraries {
    pub fn any(self) -> bool {
        self.math || self.vector3
    }
}

/// Rewrite folded constants in every function of the tree. A local that
/// already carries the library's name would shadow the global at some site,
/// so the library is left alone then.
pub fn spell_library_constants(body: &mut Block, mut libraries: Libraries) {
    if !libraries.any() {
        return;
    }
    let mut shadowed = |name: &str| {
        let mut found = false;
        visit_locals(body, &mut |local| {
            found |= local.0.lock().0.as_deref() == Some(name);
        });
        found
    };
    libraries.math &= !shadowed("math");
    libraries.vector3 &= !shadowed("Vector3");
    if libraries.any() {
        rewrite_block(body, libraries);
    }
}

fn library(name: &str, member: &str) -> RValue {
    Index::new(Global(name.as_bytes().to_vec()).into(), Literal::String(member.as_bytes().to_vec()).into()).into()
}

fn spelled(literal: &Literal, libraries: Libraries) -> Option<RValue> {
    let negate = |value: RValue| -> RValue { Unary::new(value, UnaryOperation::Negate).into() };
    match *literal {
        Literal::Number(number) if libraries.math => {
            let magnitude = if number == std::f64::consts::PI || number == -std::f64::consts::PI {
                library("math", "pi")
            } else if number.is_infinite() {
                library("math", "huge")
            } else {
                return None;
            };
            Some(if number.is_sign_negative() { negate(magnitude) } else { magnitude })
        }
        // A single-precision component is written in its shortest decimal
        // form, which the compiler narrows back to the same float.
        Literal::Vector(x, y, z) if libraries.vector3 => Some(vector3([x, y, z].map(|c| {
            let wide = if c.is_finite() { c.to_string().parse().unwrap_or(f64::from(c)) } else { f64::from(c) };
            spelled(&Literal::Number(wide), libraries).unwrap_or_else(|| Literal::Number(wide).into())
        }))),
        Literal::VectorD(x, y, z) if libraries.vector3 => Some(vector3([x, y, z].map(|c| {
            spelled(&Literal::Number(c), libraries).unwrap_or_else(|| Literal::Number(c).into())
        }))),
        _ => None,
    }
}

fn vector3(components: [RValue; 3]) -> RValue {
    Call::new(library("Vector3", "new"), components.into()).into()
}

fn rewrite_value(value: &mut RValue, libraries: Libraries) {
    if let RValue::Literal(literal) = value {
        if let Some(replacement) = spelled(literal, libraries) {
            *value = replacement;
        }
        return;
    }
    if let RValue::Closure(closure) = value {
        rewrite_block(&mut closure.function.lock().body, libraries);
    }
    value.visit_rvalues_mut(&mut |child| {
        rewrite_value(child, libraries);
        true
    });
}

fn rewrite_block(block: &mut Block, libraries: Libraries) {
    for statement in &mut block.0 {
        statement.visit_rvalues_mut(&mut |value| {
            rewrite_value(value, libraries);
            true
        });
        statement.visit_lvalues_mut(&mut |target| {
            if let LValue::Index(index) = target {
                rewrite_value(&mut index.left, libraries);
                rewrite_value(&mut index.right, libraries);
            }
            true
        });
        for_each_child_block(statement, &mut |child| rewrite_block(child, libraries));
    }
}

fn visit_locals(block: &Block, visit: &mut impl FnMut(&crate::RcLocal)) {
    for statement in &block.0 {
        statement.visit_local_reads(&mut |local| { visit(local); true });
        statement.visit_local_writes(&mut |local| { visit(local); true });
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
            let function = closure.function.lock();
            for parameter in &function.parameters {
                visit(parameter);
            }
            visit_locals(&function.body, visit);
        });
        for_each_child_block(statement, &mut |child| visit_locals(child, visit));
    }
}

fn for_each_child_block(statement: &Statement, f: &mut impl FnMut(&mut Block)) {
    match statement {
        Statement::If(r#if) => {
            f(&mut r#if.then_block.lock());
            f(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => f(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => f(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => f(&mut numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => f(&mut generic_for.block.lock()),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{spell_library_constants, Libraries};
    use crate::{Assign, Block, LValue, Literal, Local, RValue, RcLocal, Return};

    fn spelled(values: Vec<RValue>, libraries: Libraries) -> String {
        let mut block = Block(vec![Return::new(values).into()]);
        spell_library_constants(&mut block, libraries);
        block.to_string()
    }

    const ALL: Libraries = Libraries { math: true, vector3: true };

    #[test]
    fn folded_library_constants_use_their_library() {
        let pi = std::f64::consts::PI;
        let values = vec![
            Literal::Number(pi).into(),
            Literal::Number(-pi).into(),
            Literal::Number(f64::INFINITY).into(),
            Literal::Number(f64::NEG_INFINITY).into(),
            Literal::Vector(0.1, f32::INFINITY, 2.0).into(),
            Literal::Number(2.0 * pi).into(),
        ];
        assert_eq!(
            spelled(values, ALL),
            "return math.pi, -math.pi, math.huge, -math.huge, Vector3.new(0.1, math.huge, 2), 6.283185307179586"
        );
    }

    #[test]
    fn a_touched_library_keeps_exact_literals() {
        let values = vec![Literal::Number(std::f64::consts::PI).into(), Literal::Vector(1.0, 2.0, 3.0).into()];
        assert_eq!(spelled(values.clone(), Libraries::default()), "return 3.141592653589793, vector.create(1, 2, 3)");
        assert_eq!(spelled(values, Libraries { math: false, vector3: true }), "return 3.141592653589793, Vector3.new(1, 2, 3)");
    }

    #[test]
    fn a_local_named_like_the_library_keeps_exact_literals() {
        let math = RcLocal::new(Local::new(Some("math".into())));
        let mut declaration = Assign::new(vec![LValue::Local(math.clone())], vec![Literal::Nil.into()]);
        declaration.prefix = true;
        let mut block = Block(vec![declaration.into(), Return::new(vec![Literal::Number(std::f64::consts::PI).into()]).into()]);
        spell_library_constants(&mut block, ALL);
        assert!(block.to_string().ends_with("return 3.141592653589793"), "{block}");
    }
}
