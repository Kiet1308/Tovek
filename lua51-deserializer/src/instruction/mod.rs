use nom::{
    error::{Error, ErrorKind, ParseError},
    Err, IResult,
};
use num_traits::ToPrimitive;

use argument::{Constant, Function, Register, RegisterOrConstant, Upvalue};
use layout::Layout;
use operation_code::OperationCode;

pub mod argument;
mod layout;
mod operation_code;
pub mod position;

#[derive(Debug)]
struct RawInstruction(OperationCode, Layout);

impl RawInstruction {
    pub fn parse(input: &[u8]) -> IResult<&[u8], Self> {
        // TODO: we read the operation code and then the instruction including the operation code
        // while parsing the layout.
        // we should instead read the instruction here and pass it to Layout::parse
        let operation_code = OperationCode::parse(input).map(|r| r.1)?;
        let (input, layout) = Layout::parse(input, operation_code.to_u8().unwrap())?;

        Ok((input, Self(operation_code, layout)))
    }
}

#[derive(Debug, Clone)]
pub enum Instruction {
    /// The raw word following SETLIST C=0. Keep its PC occupied so jumps and
    /// debug ranges remain aligned, but never decode it as an opcode.
    ExtraArgument,
    Move {
        destination: Register,
        source: Register,
    },
    LoadConstant {
        destination: Register,
        source: Constant,
    },
    LoadBoolean {
        destination: Register,
        value: bool,
        skip_next: bool,
    },
    LoadNil(Vec<Register>),
    GetUpvalue {
        destination: Register,
        upvalue: Upvalue,
    },
    GetGlobal {
        destination: Register,
        global: Constant,
    },
    GetIndex {
        destination: Register,
        object: Register,
        key: RegisterOrConstant,
    },
    SetGlobal {
        destination: Constant,
        value: Register,
    },
    SetUpvalue {
        destination: Upvalue,
        source: Register,
    },
    SetIndex {
        object: Register,
        key: RegisterOrConstant,
        value: RegisterOrConstant,
    },
    NewTable {
        destination: Register,
        array_size: u8,
        hash_size: u8,
    },
    PrepMethodCall {
        destination: Register,
        self_arg: Register,
        object: Register,
        method: RegisterOrConstant,
    },
    Add {
        destination: Register,
        lhs: RegisterOrConstant,
        rhs: RegisterOrConstant,
    },
    Sub {
        destination: Register,
        lhs: RegisterOrConstant,
        rhs: RegisterOrConstant,
    },
    Mul {
        destination: Register,
        lhs: RegisterOrConstant,
        rhs: RegisterOrConstant,
    },
    Div {
        destination: Register,
        lhs: RegisterOrConstant,
        rhs: RegisterOrConstant,
    },
    Mod {
        destination: Register,
        lhs: RegisterOrConstant,
        rhs: RegisterOrConstant,
    },
    Pow {
        destination: Register,
        lhs: RegisterOrConstant,
        rhs: RegisterOrConstant,
    },
    Minus {
        destination: Register,
        operand: Register,
    },
    Not {
        destination: Register,
        operand: Register,
    },
    Length {
        destination: Register,
        operand: Register,
    },
    Concatenate {
        destination: Register,
        operands: Vec<Register>,
    },
    Jump(i32),
    Equal {
        lhs: RegisterOrConstant,
        rhs: RegisterOrConstant,
        invert: bool,
    },
    LessThan {
        lhs: RegisterOrConstant,
        rhs: RegisterOrConstant,
        invert: bool,
    },
    LessThanOrEqual {
        lhs: RegisterOrConstant,
        rhs: RegisterOrConstant,
        invert: bool,
    },
    Test {
        value: Register,
        invert: bool,
    },
    TestSet {
        destination: Register,
        value: Register,
        invert: bool,
    },
    Call {
        function: Register,
        arguments: u8,
        return_values: u8,
    },
    TailCall {
        function: Register,
        arguments: u8,
    },
    Return(Register, u8),
    IterateNumericForLoop {
        // TODO: change to struct instead of vec
        // internal_counter, limit, step, external_counter
        control: Vec<Register>,
        skip: i32,
    },
    InitNumericForLoop {
        // TODO: change to struct instead of vec
        // internal_counter, limit, step, external_counter
        // the name "control" refers to just the counter
        control: Vec<Register>,
        skip: i32,
    },
    IterateGenericForLoop {
        // ex. `next` in `for i, v in next, {}, 5`
        generator: Register,
        // ex. `{}` in `for i, v in next, {}, 5`
        state: Register,
        // internal control variable
        // initial value ex. `5` in `for i, v in next, {}, 5`
        // assigned to external control (vars[0]) at the start of the loop body
        internal_control: Register,
        // variables returned by generator call, starting with the external control
        vars: Vec<Register>,
    },
    SetList {
        table: Register,
        number_of_elements: u8,
        block_number: u32,
    },
    Close(Register),
    Closure {
        destination: Register,
        function: Function,
    },
    VarArg(Register, u8),
}

impl Instruction {
    pub fn parse(input: &[u8]) -> IResult<&[u8], Self> {
        let (input, instruction) = RawInstruction::parse(input)?;
        // B/C are nine-bit fields. Validate before narrowing to register/count
        // bytes, and before constructing ranges with byte-sized endpoints.
        let operands_fit = match &instruction {
            RawInstruction(OperationCode::Move | OperationCode::GetUpvalue | OperationCode::SetUpvalue
                | OperationCode::GetIndex | OperationCode::Minus | OperationCode::Not
                | OperationCode::Length, Layout::BC { b, .. }) => *b <= u8::MAX as u16,
            RawInstruction(OperationCode::LoadNil, Layout::BC { a, b, .. }) =>
                *b <= u8::MAX as u16 && *b >= u16::from(*a),
            RawInstruction(OperationCode::LoadBoolean, Layout::BC { b, c, .. }) => *b <= 1 && *c <= 1,
            RawInstruction(OperationCode::PrepMethodCall, Layout::BC { a, b, .. }) =>
                a.checked_add(1).is_some() && *b <= u8::MAX as u16,
            RawInstruction(OperationCode::Concatenate, Layout::BC { b, c, .. }) =>
                *b < *c && *c <= u8::MAX as u16,
            RawInstruction(OperationCode::Equal | OperationCode::LessThan | OperationCode::LessThanOrEqual,
                Layout::BC { a, .. }) => *a <= 1,
            RawInstruction(OperationCode::Test, Layout::BC { c, .. }) => *c <= 1,
            RawInstruction(OperationCode::TestSet, Layout::BC { b, c, .. }) => *b <= u8::MAX as u16 && *c <= 1,
            RawInstruction(OperationCode::Call, Layout::BC { b, c, .. }) => *b <= u8::MAX as u16 && *c <= u8::MAX as u16,
            RawInstruction(OperationCode::TailCall | OperationCode::Return | OperationCode::SetList | OperationCode::VarArg,
                Layout::BC { b, .. }) => *b <= u8::MAX as u16,
            RawInstruction(OperationCode::IterateNumericForLoop | OperationCode::InitNumericForLoop,
                Layout::BSx { a, .. }) => a.checked_add(3).is_some(),
            RawInstruction(OperationCode::IterateGenericForLoop, Layout::BC { a, c, .. }) =>
                *c != 0 && usize::from(*a) + 2 + usize::from(*c) <= usize::from(u8::MAX),
            _ => true,
        };
        if !operands_fit {
            return Err(Err::Failure(Error::from_error_kind(input, ErrorKind::Verify)));
        }
        let instruction = match instruction {
            RawInstruction(OperationCode::Move, Layout::BC { a, b, .. }) => Self::Move {
                destination: Register(a),
                source: Register(b as u8),
            },
            RawInstruction(OperationCode::LoadConstant, Layout::BX { a, b_x }) => {
                Self::LoadConstant {
                    destination: Register(a),
                    source: Constant(b_x),
                }
            }
            RawInstruction(OperationCode::LoadBoolean, Layout::BC { a, b, c }) => {
                Self::LoadBoolean {
                    destination: Register(a),
                    value: b == 1,
                    skip_next: c == 1,
                }
            }
            RawInstruction(OperationCode::LoadNil, Layout::BC { a, b, .. }) => {
                Self::LoadNil((a..=b as u8).map(Register).collect())
            }
            RawInstruction(OperationCode::GetUpvalue, Layout::BC { a, b, .. }) => {
                Self::GetUpvalue {
                    destination: Register(a),
                    upvalue: Upvalue(b as u8),
                }
            }
            RawInstruction(OperationCode::GetGlobal, Layout::BX { a, b_x }) => Self::GetGlobal {
                destination: Register(a),
                global: Constant(b_x),
            },
            RawInstruction(OperationCode::GetIndex, Layout::BC { a, b, c }) => Self::GetIndex {
                destination: Register(a),
                object: Register(b as u8),
                key: RegisterOrConstant::from(c as u32),
            },
            RawInstruction(OperationCode::SetGlobal, Layout::BX { a, b_x }) => Self::SetGlobal {
                destination: Constant(b_x),
                value: Register(a),
            },
            RawInstruction(OperationCode::SetUpvalue, Layout::BC { a, b, .. }) => {
                Self::SetUpvalue {
                    destination: Upvalue(b as u8),
                    source: Register(a),
                }
            }
            RawInstruction(OperationCode::SetIndex, Layout::BC { a, b, c }) => Self::SetIndex {
                object: Register(a),
                key: RegisterOrConstant::from(b as u32),
                value: RegisterOrConstant::from(c as u32),
            },
            RawInstruction(OperationCode::NewTable, Layout::BC { a, b, c }) => Self::NewTable {
                destination: Register(a),
                array_size: b as u8,
                hash_size: c as u8,
            },
            RawInstruction(OperationCode::PrepMethodCall, Layout::BC { a, b, c }) => {
                Self::PrepMethodCall {
                    destination: Register(a),
                    self_arg: Register(a + 1),
                    object: Register(b as u8),
                    method: RegisterOrConstant::from(c as u32),
                }
            }
            RawInstruction(OperationCode::Add, Layout::BC { a, b, c }) => Self::Add {
                destination: Register(a),
                lhs: RegisterOrConstant::from(b as u32),
                rhs: RegisterOrConstant::from(c as u32),
            },
            RawInstruction(OperationCode::Subtract, Layout::BC { a, b, c }) => Self::Sub {
                destination: Register(a),
                lhs: RegisterOrConstant::from(b as u32),
                rhs: RegisterOrConstant::from(c as u32),
            },
            RawInstruction(OperationCode::Multiply, Layout::BC { a, b, c }) => Self::Mul {
                destination: Register(a),
                lhs: RegisterOrConstant::from(b as u32),
                rhs: RegisterOrConstant::from(c as u32),
            },
            RawInstruction(OperationCode::Divide, Layout::BC { a, b, c }) => Self::Div {
                destination: Register(a),
                lhs: RegisterOrConstant::from(b as u32),
                rhs: RegisterOrConstant::from(c as u32),
            },
            RawInstruction(OperationCode::Modulo, Layout::BC { a, b, c }) => Self::Mod {
                destination: Register(a),
                lhs: RegisterOrConstant::from(b as u32),
                rhs: RegisterOrConstant::from(c as u32),
            },
            RawInstruction(OperationCode::Power, Layout::BC { a, b, c }) => Self::Pow {
                destination: Register(a),
                lhs: RegisterOrConstant::from(b as u32),
                rhs: RegisterOrConstant::from(c as u32),
            },
            RawInstruction(OperationCode::Minus, Layout::BC { a, b, .. }) => Self::Minus {
                destination: Register(a),
                operand: Register(b as u8),
            },
            RawInstruction(OperationCode::Not, Layout::BC { a, b, c: _ }) => Self::Not {
                destination: Register(a),
                operand: Register(b as u8),
            },
            RawInstruction(OperationCode::Length, Layout::BC { a, b, c: _ }) => Self::Length {
                destination: Register(a),
                operand: Register(b as u8),
            },
            RawInstruction(OperationCode::Concatenate, Layout::BC { a, b, c }) => {
                Self::Concatenate {
                    destination: Register(a),
                    operands: (b..=c).map(|r| Register(r as u8)).collect(),
                }
            }
            RawInstruction(OperationCode::Jump, Layout::BSx { b_sx, .. }) => Self::Jump(b_sx),
            RawInstruction(OperationCode::Equal, Layout::BC { a, b, c }) => Self::Equal {
                lhs: RegisterOrConstant::from(b as u32),
                rhs: RegisterOrConstant::from(c as u32),
                invert: a != 1,
            },
            RawInstruction(OperationCode::LessThan, Layout::BC { a, b, c }) => Self::LessThan {
                lhs: RegisterOrConstant::from(b as u32),
                rhs: RegisterOrConstant::from(c as u32),
                invert: a != 1,
            },
            RawInstruction(OperationCode::LessThanOrEqual, Layout::BC { a, b, c }) => {
                Self::LessThanOrEqual {
                    lhs: RegisterOrConstant::from(b as u32),
                    rhs: RegisterOrConstant::from(c as u32),
                    invert: a != 1,
                }
            }
            RawInstruction(OperationCode::Test, Layout::BC { a, c, .. }) => Self::Test {
                value: Register(a),
                invert: c != 1,
            },
            RawInstruction(OperationCode::TestSet, Layout::BC { a, b, c }) => Self::TestSet {
                destination: Register(a),
                value: Register(b as u8),
                invert: c != 1,
            },
            RawInstruction(OperationCode::Call, Layout::BC { a, b, c }) => Self::Call {
                function: Register(a),
                arguments: b as u8,
                return_values: c as u8,
            },
            RawInstruction(OperationCode::TailCall, Layout::BC { a, b, .. }) => Self::TailCall {
                function: Register(a),
                arguments: b as u8,
            },
            RawInstruction(OperationCode::Return, Layout::BC { a, b, .. }) => {
                Self::Return(Register(a), b as u8)
            }
            RawInstruction(OperationCode::IterateNumericForLoop, Layout::BSx { a, b_sx }) => {
                Self::IterateNumericForLoop {
                    control: (a..=a + 3).map(Register).collect(),
                    skip: b_sx,
                }
            }
            RawInstruction(OperationCode::InitNumericForLoop, Layout::BSx { a, b_sx }) => {
                Self::InitNumericForLoop {
                    control: (a..=a + 3).map(Register).collect(),
                    skip: b_sx,
                }
            }
            RawInstruction(OperationCode::IterateGenericForLoop, Layout::BC { a, c, .. }) => {
                let res = Self::IterateGenericForLoop {
                    generator: Register(a),
                    state: Register(a + 1),
                    internal_control: Register(a + 2),
                    vars: (usize::from(a) + 3..usize::from(a) + 3 + usize::from(c))
                        .map(|register| Register(register as u8)).collect(),
                };
                res
            }
            RawInstruction(OperationCode::SetList, Layout::BC { a, b, c }) => Self::SetList {
                table: Register(a),
                number_of_elements: b as u8,
                block_number: c as u32,
            },
            RawInstruction(OperationCode::Close, Layout::BC { a, .. }) => Self::Close(Register(a)),
            RawInstruction(OperationCode::Closure, Layout::BX { a, b_x }) => Self::Closure {
                destination: Register(a),
                function: Function(b_x),
            },
            RawInstruction(OperationCode::VarArg, Layout::BC { a, b, .. }) => {
                Self::VarArg(Register(a), b as u8)
            }
            _ => {
                return Err(Err::Failure(Error::from_error_kind(
                    input,
                    ErrorKind::Switch,
                )))
            }
        };

        Ok((input, instruction))
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn operand_widths_and_loop_ranges_fail_before_narrowing_or_overflow() {
        let abc = |op: u32, a: u32, b: u32, c: u32| (op | (a << 6) | (c << 14) | (b << 23)).to_le_bytes();
        for bytes in [
            abc(0, 0, 256, 0), // MOVE with a nine-bit register
            abc(11, 255, 0, 0), // SELF needs A+1
            abc(21, 0, 2, 1), // CONCAT has a reversed range
            abc(28, 0, 256, 1), // CALL count would truncate
            abc(33, 0, 0, 0), // TFORLOOP needs an external result
            abc(33, 254, 0, 1), // TFORLOOP register range overflows
            abc(3, 10, 9, 0), // LOADNIL reversed range
        ] { assert!(Instruction::parse(&bytes).is_err(), "{bytes:?}"); }
        let numeric = 31u32 | (252 << 6);
        assert!(matches!(Instruction::parse(&numeric.to_le_bytes()),
            Ok(([], Instruction::IterateNumericForLoop { control, .. })) if control.len() == 4));
        let overflow = 31u32 | (253 << 6);
        assert!(Instruction::parse(&overflow.to_le_bytes()).is_err());
    }
}
