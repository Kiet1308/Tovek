use std::fmt;

use crate::{has_side_effects, LocalRw, SideEffects, Traverse};

// TODO: Rc
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label(pub String);

impl SideEffects for Label {}

impl From<&str> for Label {
    fn from(str: &str) -> Self {
        Label(str.into())
    }
}

impl From<String> for Label {
    fn from(str: String) -> Self {
        Label(str)
    }
}

impl LocalRw for Label {}

impl Traverse for Label {}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "::{}::", self.0)
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Goto(pub Label);

impl Traverse for Goto {}

has_side_effects!(Goto);

impl Goto {
    pub fn new(label: Label) -> Self {
        Self(label)
    }
}

impl LocalRw for Goto {}

impl fmt::Display for Goto {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "goto {}", self.0 .0)
    }
}
