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
        block_number: u8,
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
                    control: (a..=a + 4).map(Register).collect(),
                    skip: b_sx,
                }
            }
            RawInstruction(OperationCode::InitNumericForLoop, Layout::BSx { a, b_sx }) => {
                Self::InitNumericForLoop {
                    control: (a..=a + 4).map(Register).collect(),
                    skip: b_sx,
                }
            }
            RawInstruction(OperationCode::IterateGenericForLoop, Layout::BC { a, c, .. }) => {
                let res = Self::IterateGenericForLoop {
                    generator: Register(a),
                    state: Register(a + 1),
                    internal_control: Register(a + 2),
                    vars: (a + 3..a + 3 + c as u8).map(Register).collect(),
                };
                // must have at least external control variable
                assert!(match &res {
                    Self::IterateGenericForLoop { vars, .. } => !vars.is_empty(),
                    _ => unreachable!(),
                });
                res
            }
            RawInstruction(OperationCode::SetList, Layout::BC { a, b, c }) => Self::SetList {
                table: Register(a),
                number_of_elements: b as u8,
                block_number: c as u8,
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
