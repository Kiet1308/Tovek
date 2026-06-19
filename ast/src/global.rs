use derive_more::From;
use std::fmt;

use crate::{formatter::Formatter, LocalRw, SideEffects, Traverse};

#[derive(Debug, From, PartialEq, Eq, PartialOrd, Clone)]
pub struct Global(pub Vec<u8>);

impl Global {
    pub fn new(name: Vec<u8>) -> Self {
        Self(name)
    }
}

impl LocalRw for Global {}

impl SideEffects for Global {
    fn has_side_effects(&self) -> bool {
        true
    }
}

impl Traverse for Global {}

impl<'a> From<&'a str> for Global {
    fn from(name: &'a str) -> Self {
        Self::new(name.into())
    }
}

impl fmt::Display for Global {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if Formatter::<fmt::Formatter>::is_valid_name(&self.0) {
            write!(f, "{}", std::str::from_utf8(&self.0).unwrap())
        } else {
            write!(
                f,
                "__FENV[\"{}\"]",
                Formatter::<fmt::Formatter>::escape_string(&self.0)
            )
        }
    }
}
