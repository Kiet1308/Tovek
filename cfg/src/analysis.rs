//! Analysis validity for one explicitly managed CFG mutation session.
//!
//! A revision belongs to the session, not to the public `Function`: callers
//! must advance it from each pass's mutation report before querying a cached
//! analysis again. Raw graph/block edits outside that session require a fresh
//! session. This avoids pretending a counter can observe arbitrary `graph_mut`
//! or shared nested-block mutations.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Revision(u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Revisions {
    topology: Revision,
    operands: Revision,
    layout: Revision,
}

impl Revisions {
    /// Reachability, dominators and adjacency-only analyses depend on this.
    pub fn topology(&self) -> Revision { self.topology }

    /// Def-use, capture and expression facts depend on this. Removing an edge
    /// or statement can remove operand occurrences, so those edits advance it.
    pub fn operands(&self) -> Revision { self.operands }

    /// Statement-position indexes depend on this. Moving/replacing expressions
    /// within unchanged statement slots does not invalidate their positions.
    pub fn layout(&self) -> Revision { self.layout }

    pub fn advance(&mut self, topology: bool, operands: bool, layout: bool) {
        fn bump(revision: &mut Revision) {
            revision.0 = revision.0.checked_add(1).expect("CFG analysis revision exhausted");
        }
        if topology { bump(&mut self.topology); }
        if topology || operands || layout { bump(&mut self.operands); }
        if topology || layout { bump(&mut self.layout); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_keep_dominators_and_positions_across_expression_edits() {
        let mut revisions = Revisions::default();
        let initial = revisions;
        revisions.advance(false, false, false);
        assert_eq!(revisions, initial);
        revisions.advance(false, true, false);
        assert_eq!(revisions.topology(), initial.topology());
        assert_eq!(revisions.layout(), initial.layout());
        assert_ne!(revisions.operands(), initial.operands());
        let expressions = revisions;
        revisions.advance(false, false, true);
        assert_eq!(revisions.topology(), expressions.topology());
        assert_ne!(revisions.layout(), expressions.layout());
        assert_ne!(revisions.operands(), expressions.operands());
        let positions = revisions;
        revisions.advance(true, false, false);
        assert_ne!(revisions.topology(), positions.topology());
        assert_ne!(revisions.layout(), positions.layout());
        assert_ne!(revisions.operands(), positions.operands());
    }
}
