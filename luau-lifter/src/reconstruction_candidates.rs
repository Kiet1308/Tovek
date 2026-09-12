//! Preserve prospective scalar helper binders before parallel child lifting.
//! The later AST matcher independently proves every accepted reconstruction.
use crate::{deserializer::function::Function, instruction::Instruction, op_code::OpCode};

pub(crate) fn retain(function: &Function, name: Option<&str>) -> bool {
    name.is_some_and(|name| name.len() <= 256 && ast::valid_source_name(name))
        && !function.is_vararg
        && function.num_upvalues == 0
        && (1..=8).contains(&function.num_parameters)
        && scalar_body(&function.instructions)
}

fn scalar_body(instructions: &[Instruction]) -> bool {
    use OpCode::*;
    if instructions.len() > 128 { return false; }
    let mut operations = 0;
    let mut returns = 0;
    for instruction in instructions {
        let opcode = match instruction {
            Instruction::BC { op_code, .. } | Instruction::AD { op_code, .. }
            | Instruction::E { op_code, .. } => *op_code,
        };
        match opcode {
            LOP_RETURN => {
                if !matches!(instruction, Instruction::BC { b: 2, .. }) { return false; }
                returns += 1;
            }
            LOP_ADD | LOP_SUB | LOP_MUL | LOP_DIV | LOP_MOD | LOP_POW
            | LOP_ADDK | LOP_SUBK | LOP_MULK | LOP_DIVK | LOP_MODK | LOP_POWK
            | LOP_SUBRK | LOP_DIVRK | LOP_IDIV | LOP_IDIVK
            | LOP_AND | LOP_OR | LOP_ANDK | LOP_ORK | LOP_NOT | LOP_MINUS
            | LOP_JUMPIFEQ | LOP_JUMPIFLE | LOP_JUMPIFLT | LOP_JUMPIFNOTEQ
            | LOP_JUMPIFNOTLE | LOP_JUMPIFNOTLT | LOP_JUMPXEQKNIL
            | LOP_JUMPXEQKB | LOP_JUMPXEQKN | LOP_JUMPXEQKS => operations += 1,
            LOP_NOP | LOP_LOADNIL | LOP_LOADB | LOP_LOADN | LOP_LOADK
            | LOP_LOADKX | LOP_MOVE | LOP_JUMP | LOP_JUMPIF | LOP_JUMPIFNOT
            | LOP_JUMPX | LOP_COVERAGE => {}
            _ => return false,
        }
    }
    returns > 0 && operations >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bc(op_code: OpCode, b: u8) -> Instruction {
        Instruction::BC { op_code, a: 0, b, c: 0, aux: 0 }
    }

    #[test]
    fn scalar_candidate_requires_cost_and_fixed_return_arity() {
        use OpCode::*;
        let mut body = vec![bc(LOP_MULK, 0), bc(LOP_ADDK, 0), bc(LOP_MULK, 0), bc(LOP_RETURN, 2)];
        assert!(scalar_body(&body));
        for arity in [0, 1, 3] {
            body[3] = bc(LOP_RETURN, arity);
            assert!(!scalar_body(&body));
        }
        assert!(!scalar_body(&[bc(LOP_ADDK, 0), bc(LOP_RETURN, 2)]));
    }

    #[test]
    fn calls_captures_storage_loops_and_budget_refuse() {
        use OpCode::*;
        let base = vec![bc(LOP_MULK, 0), bc(LOP_ADDK, 0), bc(LOP_MULK, 0), bc(LOP_RETURN, 2)];
        for opcode in [LOP_CALL, LOP_GETUPVAL, LOP_GETTABLE, LOP_SETGLOBAL, LOP_NEWCLOSURE, LOP_FORNLOOP, LOP_JUMPBACK] {
            let mut body = base.clone();
            body.insert(0, bc(opcode, 0));
            assert!(!scalar_body(&body), "{opcode:?}");
        }
        let mut body = base;
        body.resize(129, bc(LOP_NOP, 0));
        assert!(!scalar_body(&body));
    }
}
