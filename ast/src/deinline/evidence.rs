//! Line evidence of inlined copies (plan E1).
//!
//! Luau `-O2` keeps a callee's own source lines on the code it inlines, so
//! a chunk's per-PC lines tell, for each function, how many copies of each
//! local helper its code holds. The lifter reads that census off the
//! bytecode ([`Copies`]); the de-inliners use it to *admit* a helper below
//! their readability floors in a function, all or nothing: every one of its
//! copies there must match, and nothing else may (the count oracle). It
//! never *authorizes* a rewrite: each rebuilt call is still proven by exact
//! unification, so forged or missing line info can only cost recall.
//! Without line info (`-g0`) nothing here applies.

use std::{cell::RefCell, marker::PhantomData, rc::Rc};

use rustc_hash::{FxHashMap, FxHashSet};

/// The inlined copies a chunk's line info shows, by prototype index.
#[derive(Default, Clone, Debug)]
pub struct Copies {
    /// The chunk's main prototype, which the chunk body is.
    pub main: usize,
    /// `(caller, helper)` -> the copies of the helper in the caller's
    /// code. A copy counts once however much it holds: a helper inlined
    /// inside another one's copy goes with that copy (it disappears into
    /// that copy's call when the outer one is rebuilt).
    pub copies: FxHashMap<(u32, u32), u32>,
    /// `(caller, helper)` where any of the caller's code is the helper's,
    /// inside another helper's copy or not.
    pub present: FxHashSet<(u32, u32)>,
    /// `(caller, outer, inner)`: a copy of `inner` inside a copy of `outer`
    /// in the caller's code.
    pub nested: FxHashSet<(u32, u32, u32)>,
    /// Fully folded copies (plan E2): `(caller, helper, value bits)` -> the
    /// copies of a one-line arithmetic helper that are one load of the
    /// number constant with those bits, its whole computation folded
    /// (`frames(13)` as `0.21666666666666667`): outside other copies, and
    /// inside a copy of another helper.
    pub constant_copies: FxHashMap<(u32, u32, u64), ConstantCopies>,
    /// `(caller, value bits)` -> the caller's references to that number
    /// constant, every instruction loading it, operating with it or
    /// comparing with it, and every value of a table template: only for
    /// callers with a [`Copies::constant_copies`] or
    /// [`Copies::copy_refs`] entry.
    pub constant_refs: FxHashMap<(u32, u64), u32>,
    /// `(caller, helper, value bits)` -> the caller's references to that
    /// number constant inside copies of the helper (or of a helper whose
    /// copy holds one of it): constants its copies' folding may have made
    /// ([`Copies::constant_in_copies`]).
    pub copy_refs: FxHashMap<(u32, u32, u64), u32>,
    /// The helpers with a copy somewhere, read off `copies` by [`enter`].
    inlined: FxHashSet<u32>,
    /// `(caller, helper)` whose copies refer to a number constant, read
    /// off `copy_refs` by [`enter`].
    folded: FxHashSet<(u32, u32)>,
    /// The helpers of `folded`.
    folded_helpers: FxHashSet<u32>,
}

/// The fully folded copies of one helper producing one constant in one
/// function ([`Copies::constant_copies`]).
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConstantCopies {
    /// Copies outside any other copy.
    pub outermost: u32,
    /// Copies inside a copy of another helper.
    pub nested: u32,
}

impl Copies {
    /// No copies yet, in the chunk whose main prototype is `main`.
    pub fn new(main: usize) -> Self {
        Self { main, ..Self::default() }
    }

    /// The copies of `helper` in the code of `caller` (`None`: the chunk).
    pub fn copies(&self, caller: Option<usize>, helper: usize) -> u32 {
        let caller = caller.unwrap_or(self.main);
        match (u32::try_from(caller), u32::try_from(helper)) {
            (Ok(caller), Ok(helper)) => self.copies.get(&(caller, helper)).copied().unwrap_or(0),
            _ => 0,
        }
    }

    /// Whether any code of `caller` (`None`: the chunk) is `helper`'s.
    pub fn present(&self, caller: Option<usize>, helper: usize) -> bool {
        let caller = caller.unwrap_or(self.main);
        match (u32::try_from(caller), u32::try_from(helper)) {
            (Ok(caller), Ok(helper)) => self.present.contains(&(caller, helper)),
            _ => false,
        }
    }

    /// Whether a copy of `inner` sits inside a copy of `outer` in the code
    /// of `caller` (`None`: the chunk).
    pub fn nested(&self, caller: Option<usize>, outer: usize, inner: usize) -> bool {
        let caller = caller.unwrap_or(self.main);
        match (u32::try_from(caller), u32::try_from(outer), u32::try_from(inner)) {
            (Ok(caller), Ok(outer), Ok(inner)) => self.nested.contains(&(caller, outer, inner)),
            _ => false,
        }
    }

    /// Whether copies of `helper` in the code of `caller` (`None`: the
    /// chunk) refer to the number constant with `bits` (plan E2): a literal
    /// of it may stand for what those copies folded.
    pub fn constant_in_copies(&self, caller: Option<usize>, helper: usize, bits: u64) -> bool {
        let caller = caller.unwrap_or(self.main);
        match (u32::try_from(caller), u32::try_from(helper)) {
            (Ok(caller), Ok(helper)) => self.copy_refs.contains_key(&(caller, helper, bits)),
            _ => false,
        }
    }

    /// Whether copies of `helper` in the code of `caller` (`None`: the
    /// chunk) refer to any number constant: only there may a constant-fold
    /// equation of it hold (plan E2).
    pub fn constants_in_copies(&self, caller: Option<usize>, helper: usize) -> bool {
        let caller = caller.unwrap_or(self.main);
        match (u32::try_from(caller), u32::try_from(helper)) {
            (Ok(caller), Ok(helper)) => self.folded.contains(&(caller, helper)),
            _ => false,
        }
    }

    /// Whether copies of `helper` refer to a number constant somewhere.
    pub fn constants_in_copies_anywhere(&self, helper: usize) -> bool {
        u32::try_from(helper).is_ok_and(|helper| self.folded_helpers.contains(&helper))
    }

    /// Whether some function holds a copy of `helper`.
    pub fn inlined_anywhere(&self, helper: usize) -> bool {
        u32::try_from(helper).is_ok_and(|helper| self.inlined.contains(&helper))
    }
}

/// What a probe counted for one pair of a function and a helper below the
/// floors: the matches of the helper there, and the calls of it some
/// de-inliner rebuilt there before. Prototypes are `None` where unknown.
pub(crate) struct Probed {
    pub(crate) caller: Option<usize>,
    pub(crate) helper: Option<usize>,
    pub(crate) hits: usize,
    pub(crate) rebuilt: usize,
}

/// The count oracle: for each probed pair, whether its helper is rebuilt in
/// that function now. Only where its matches, with the calls already
/// rebuilt there, are exactly its copies there: then each match is a copy
/// and each copy a match. Any other count refuses them all (a copy the
/// structurer cloned or merged, one that does not unify, code written like
/// the helper by hand). Outermost first: a helper whose copies sit inside
/// copies of another one admitted in the same function waits for a later
/// round, after that one's calls took in the code they hold.
pub(crate) fn admitted(copies: &Copies, probed: &[Probed]) -> Vec<bool> {
    let agrees: Vec<bool> = probed
        .iter()
        .map(|p| match (p.caller, p.helper) {
            (Some(caller), Some(helper)) => p.hits > 0 && p.hits + p.rebuilt == copies.copies(Some(caller), helper) as usize,
            _ => false,
        })
        .collect();
    let mut outers: FxHashMap<usize, Vec<usize>> = FxHashMap::default();
    for (p, &agrees) in probed.iter().zip(&agrees) {
        if let (true, Some(caller), Some(helper)) = (agrees, p.caller, p.helper) {
            outers.entry(caller).or_default().push(helper);
        }
    }
    probed
        .iter()
        .zip(agrees)
        .map(|(p, agrees)| {
            agrees
                && !matches!((p.caller, p.helper), (Some(caller), Some(inner))
                    if outers[&caller].iter().any(|&outer| outer != inner && copies.nested(Some(caller), outer, inner)))
        })
        .collect()
}

thread_local! {
    static STATE: RefCell<Option<Rc<Copies>>> = const { RefCell::new(None) };
}

/// Holds the census of the chunk being decompiled on this thread until it
/// drops, then restores what was there before.
pub struct Scope(Option<Rc<Copies>>, PhantomData<Rc<()>>);

impl Drop for Scope {
    fn drop(&mut self) {
        let previous = self.0.take();
        STATE.with(|state| *state.borrow_mut() = previous);
    }
}

/// Makes `copies` the census of the chunk decompiled on this thread
/// (`None`: no line info, so no evidence).
pub fn enter(copies: Option<Copies>) -> Scope {
    let copies = copies.map(|mut copies| {
        copies.inlined = copies.copies.keys().map(|&(_, helper)| helper).collect();
        copies.folded = copies.copy_refs.keys().map(|&(caller, helper, _)| (caller, helper)).collect();
        copies.folded_helpers = copies.folded.iter().map(|&(_, helper)| helper).collect();
        Rc::new(copies)
    });
    let previous = STATE.with(|state| state.replace(copies));
    Scope(previous, PhantomData)
}

/// The census of the chunk being decompiled, if it has line info.
pub(crate) fn current() -> Option<Rc<Copies>> {
    STATE.with(|state| state.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_are_read_per_caller_and_the_chunk_is_its_main_prototype() {
        let mut copies = Copies::new(3);
        copies.copies.insert((3, 1), 2);
        copies.copies.insert((2, 1), 1);
        copies.present.insert((2, 0));
        assert_eq!(copies.copies(None, 1), 2);
        assert_eq!(copies.copies(Some(2), 1), 1);
        assert_eq!(copies.copies(Some(2), 0), 0);
        assert!(copies.present(Some(2), 0));
        assert!(!copies.present(None, 0));
        {
            let _scope = enter(Some(copies));
            let copies = current().unwrap();
            assert_eq!(copies.copies(None, 1), 2);
            assert!(copies.inlined_anywhere(1));
            assert!(!copies.inlined_anywhere(0));
            {
                let _inner = enter(None);
                assert!(current().is_none());
            }
            assert!(current().is_some());
        }
        assert!(current().is_none());
    }

    #[test]
    fn the_count_oracle_admits_exact_counts_outermost_first() {
        let mut copies = Copies::new(0);
        copies.copies.insert((1, 10), 2);
        copies.copies.insert((1, 11), 1);
        copies.copies.insert((2, 10), 1);
        copies.nested.insert((1, 10, 11));
        let probe = |caller, helper, hits, rebuilt| Probed { caller: Some(caller), helper: Some(helper), hits, rebuilt };
        let admitted = admitted(&copies, &[
            probe(1, 10, 1, 1),  // one match, one call rebuilt before: two copies
            probe(1, 11, 1, 0),  // inside copies of 10, admitted in this round: waits
            probe(2, 10, 2, 0),  // two matches, one copy: refused
            Probed { caller: None, helper: Some(10), hits: 1, rebuilt: 0 },
        ]);
        assert_eq!(admitted, vec![true, false, false, false]);
    }
}
