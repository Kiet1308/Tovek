//! Source-informed spacing where compiler line metadata survived rewriting.
//! Missing locations use formatter heuristics; line gaps cannot recover comments.
use crate::{RValue, Statement, Traverse};

#[derive(Clone, Copy, Default)]
pub struct SourceLines { first: u32, last: u32 }

impl SourceLines {
    pub fn at(line: u32) -> Self { Self { first: line, last: line } }
    pub fn of(lines: impl IntoIterator<Item = u32>) -> Self {
        let mut result = Self::default();
        for line in lines { result.merge(Self::at(line)); }
        result
    }
    pub fn known(self) -> bool { self.first != 0 }
    pub fn merge(&mut self, other: Self) {
        if !other.known() { return; }
        if !self.known() { *self = other; }
        else { self.first = self.first.min(other.first); self.last = self.last.max(other.last); }
    }
    pub fn gap_after(self, previous: Self) -> Option<bool> {
        (self.known() && previous.known() && self.first >= previous.last)
            .then(|| self.first > previous.last.saturating_add(1))
    }
}

impl PartialEq for SourceLines { fn eq(&self, _: &Self) -> bool { true } }
impl Eq for SourceLines {}
impl std::fmt::Debug for SourceLines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("SourceLines") }
}

/// Seed new syntax only. A pending multret call keeps its earlier CALL line.
pub fn seed_value(value: &mut RValue, lines: SourceLines) {
    if let Some(origin) = crate::node_origins::value_mut(value) {
        if !origin.1.known() { origin.1 = lines; }
    }
    value.visit_rvalues_mut(&mut |child| { seed_value(child, lines); true });
}

pub fn seed_statement(statement: &mut Statement, lines: SourceLines) {
    if let Some(origin) = crate::node_origins::statement_mut(statement) { origin.1 = lines; }
    statement.visit_lvalues_mut(&mut |left| {
        left.visit_rvalues_mut(&mut |value| { seed_value(value, lines); true })
    });
    statement.visit_rvalues_mut(&mut |value| { seed_value(value, lines); true });
}

pub fn statement(statement: &Statement) -> SourceLines {
    // Fresh control syntax has no reliable header/closing location. Its body
    // cannot establish where the surrounding source statement began or ended.
    if matches!(statement, Statement::If(_) | Statement::While(_) | Statement::Repeat(_)
        | Statement::NumericFor(_) | Statement::GenericFor(_)) { return SourceLines::default(); }
    let mut result = crate::node_origins::statement(statement).map_or(SourceLines::default(), |origin| origin.1);
    fn value(item: &RValue, result: &mut SourceLines) {
        if let Some(origin) = crate::node_origins::value(item) { result.merge(origin.1); }
        if let RValue::Closure(closure) = item { result.merge(closure.function.lock().source_lines); }
        item.visit_rvalues(&mut |child| { value(child, result); true });
    }
    statement.visit_lvalues(&mut |left| left.visit_rvalues(&mut |item| { value(item, &mut result); true }));
    statement.visit_rvalues(&mut |item| { value(item, &mut result); true });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Call, Global};

    #[test]
    fn gaps_survive_copies_without_changing_semantic_equality() {
        let mut first: Statement = Call::new(Global::from("first").into(), vec![]).into();
        let original = first.clone();
        seed_statement(&mut first, SourceLines::at(2));
        assert_eq!(first, original);
        assert_eq!(format!("{first:?}"), format!("{original:?}"));
        let mut next: Statement = Call::new(Global::from("next").into(), vec![]).into();
        seed_statement(&mut next, SourceLines::at(5));
        let block = Block(vec![first.clone(), next.clone()]);
        assert_eq!(block.to_string(), "first()\n\nnext()");
        seed_statement(&mut next, SourceLines::at(3));
        assert_eq!(Block(vec![first, next]).to_string(), "first()\nnext()");
    }

    #[test]
    fn origin_merge_keeps_lines_even_without_diagnostic_ancestry() {
        let mut origin = crate::node_origins::Origin::default();
        origin.1 = SourceLines::at(2);
        let mut other = crate::node_origins::Origin::default();
        other.1 = SourceLines::at(8);
        origin.merge(&other);
        assert_eq!(SourceLines::at(9).gap_after(origin.1), Some(false));
        assert_eq!(SourceLines::at(10).gap_after(origin.1), Some(true));
        assert_eq!(SourceLines::at(10).gap_after(SourceLines::default()), None);
        origin.set_input(crate::node_origins::Input { function: "p0".into(), block: 0, statement: 0, value: None });
        assert_eq!(SourceLines::at(9).gap_after(origin.clone().1), Some(false));
    }
}
