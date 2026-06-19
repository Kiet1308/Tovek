use either::Either;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct Register(pub u8);

impl From<u8> for Register {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

#[derive(Debug, Copy, Clone)]
pub struct Constant(pub u32);

#[derive(Debug, Copy, Clone)]
pub struct RegisterOrConstant(pub Either<Register, Constant>);

impl From<u32> for RegisterOrConstant {
    fn from(value: u32) -> Self {
        Self(if value > 255 {
            Either::Right(Constant(value - 256))
        } else {
            Either::Left(Register(value as u8))
        })
    }
}

#[derive(Debug, Clone)]
pub struct Upvalue(pub u8);

#[derive(Debug, Clone)]
pub struct Function(pub u32);
