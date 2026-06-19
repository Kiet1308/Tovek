use enum_dispatch::enum_dispatch;

#[enum_dispatch]
pub trait SideEffects {
    fn has_side_effects(&self) -> bool {
        false
    }
}

macro_rules! has_side_effects {
    ($($name:ty),*) => {
        $(
            impl $crate::SideEffects for $name {
                fn has_side_effects(&self) -> bool {
                    true
                }
            }
        )*
    };
}

pub(crate) use has_side_effects;
