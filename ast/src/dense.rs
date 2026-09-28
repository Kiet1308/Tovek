//! Dense per-function indices for locals, computed from their ids.
//!
//! A function's register locals are minted while it is lifted, so their ids
//! form one contiguous range; every version minted while decompiling it comes
//! from its own id segment (see [`crate::set_local_id_base`]). Facts about
//! those locals can therefore live in plain vectors instead of hash maps keyed
//! by [`RcLocal`]. Anything else (a captured parent local) gets the next index
//! of a small side table on first sight.

use std::ops::Range;

use rustc_hash::FxHashMap;

use crate::RcLocal;

/// Ids reserved for one function's minted locals (`base + func_idx * 2^40`).
const SEGMENT: u64 = 1 << 40;

/// Where a local's facts live.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Lifted(u32),
    Minted(u32),
    Other(u32),
}

#[derive(Clone, Debug, Default)]
pub struct LocalIndex {
    lifted: Range<u64>,
    minted: u64,
    others: FxHashMap<u64, u32>,
}

impl LocalIndex {
    /// `lifted` are the ids minted while lifting the function; `minted` is the
    /// first id of its segment.
    pub fn new(lifted: Range<u64>, minted: u64) -> Self {
        Self { lifted, minted, others: FxHashMap::default() }
    }

    fn classify(&self, id: u64) -> Option<Slot> {
        if self.lifted.contains(&id) {
            Some(Slot::Lifted((id - self.lifted.start) as u32))
        } else if id.wrapping_sub(self.minted) < SEGMENT {
            Some(Slot::Minted((id - self.minted) as u32))
        } else {
            self.others.get(&id).map(|&index| Slot::Other(index))
        }
    }

    /// The slot of `local`, registering an outside local on first sight.
    #[inline]
    pub fn slot(&mut self, local: &RcLocal) -> Slot {
        let id = local.stable_id();
        if let Some(slot) = self.classify(id) {
            return slot;
        }
        let next = self.others.len() as u32;
        self.others.insert(id, next);
        Slot::Other(next)
    }

    /// The slot of `local` if it is a register, a minted version or an
    /// already registered outside local.
    #[inline]
    pub fn find(&self, local: &RcLocal) -> Option<Slot> {
        self.classify(local.stable_id())
    }
}

/// A value per local slot; unset slots read as `fill`.
#[derive(Clone, Debug)]
pub struct LocalVec<T> {
    lifted: Vec<T>,
    minted: Vec<T>,
    others: Vec<T>,
    fill: T,
}

impl<T: Clone> LocalVec<T> {
    pub fn new(fill: T) -> Self {
        Self { lifted: Vec::new(), minted: Vec::new(), others: Vec::new(), fill }
    }

    fn part(&self, slot: Slot) -> (&Vec<T>, usize) {
        match slot {
            Slot::Lifted(index) => (&self.lifted, index as usize),
            Slot::Minted(index) => (&self.minted, index as usize),
            Slot::Other(index) => (&self.others, index as usize),
        }
    }

    #[inline]
    pub fn get(&self, slot: Slot) -> &T {
        let (values, index) = self.part(slot);
        values.get(index).unwrap_or(&self.fill)
    }

    #[inline]
    pub fn get_mut(&mut self, slot: Slot) -> &mut T {
        let (values, index) = match slot {
            Slot::Lifted(index) => (&mut self.lifted, index as usize),
            Slot::Minted(index) => (&mut self.minted, index as usize),
            Slot::Other(index) => (&mut self.others, index as usize),
        };
        if index >= values.len() {
            values.resize(index + 1, self.fill.clone());
        }
        &mut values[index]
    }

    /// Every stored value, in slot order (lifted, minted, others).
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.lifted.iter().chain(&self.minted).chain(&self.others)
    }
}
