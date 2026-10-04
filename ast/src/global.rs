use derive_more::From;
use std::fmt;

use crate::{formatter::Formatter, LocalRw, SideEffects, Traverse};

#[derive(Debug, From, PartialEq, Eq, PartialOrd)]
#[cfg_attr(not(feature = "byte-storage-trace"), derive(Clone))]
pub struct Global(pub Vec<u8>);

#[cfg(feature = "byte-storage-trace")]
impl Clone for Global {
    fn clone(&self) -> Self {
        crate::telemetry::count("byte_global_clone_calls", 1);
        crate::telemetry::count("byte_global_clone_bytes", self.0.len() as u64);
        crate::telemetry::count("byte_global_clone_nonempty", u64::from(!self.0.is_empty()));
        Self(self.0.clone())
    }
}

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
            // A global no identifier spells: the field of the running
            // function's environment, which GETGLOBAL reads.
            write!(
                f,
                "getfenv(1)[\"{}\"]",
                Formatter::<fmt::Formatter>::escape_string(&self.0)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Global;

    #[test]
    fn global_clone_keeps_independent_arbitrary_byte_storage() {
        for bytes in [Vec::new(), b"validName".to_vec(), (0..=255).collect()] {
            let original = Global(bytes);
            let mut cloned = original.clone();
            assert_eq!(cloned, original);
            assert_eq!(cloned.to_string(), original.to_string());
            if !original.0.is_empty() {
                assert_ne!(cloned.0.as_ptr(), original.0.as_ptr());
                cloned.0[0] ^= 0xff;
                assert_ne!(cloned.0[0], original.0[0]);
            }
            cloned.0.push(0);
            assert_eq!(cloned.0.len(), original.0.len() + 1);
            cloned.clone_from(&original);
            assert_eq!(cloned, original);
        }
    }
}
