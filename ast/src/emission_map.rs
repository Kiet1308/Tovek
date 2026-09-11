//! Optional final-text locations. These identify emitted syntax and binding
//! identities, not value origins, instruction ownership or rewrite proofs.
use crate::formatter::SourceSpan;

pub const OCCURRENCE_LIMIT: usize = 100_000;
pub const ANNOTATION_BYTE_LIMIT: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingOccurrence {
    pub binding_id: u64,
    pub role: &'static str,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnotationOccurrence {
    pub text: String,
    pub truncated: bool,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpaqueOccurrence {
    pub reason: &'static str,
    pub span: SourceSpan,
}

/// Collected during actual emission only; layout previews must never contribute.
/// No RcLocal owners are retained and no IDs are allocated by this collector.
#[derive(Default, Debug)]
pub struct EmissionMap {
    pub bindings: Vec<BindingOccurrence>,
    pub annotations: Vec<AnnotationOccurrence>,
    pub opaque_regions: Vec<OpaqueOccurrence>,
    pub omitted_occurrences: usize,
}

impl EmissionMap {
    fn room(&mut self) -> bool {
        if self.bindings.len() + self.annotations.len() + self.opaque_regions.len() < OCCURRENCE_LIMIT {
            true
        } else {
            self.omitted_occurrences += 1;
            false
        }
    }

    pub fn binding(&mut self, binding_id: u64, role: &'static str, span: SourceSpan) {
        if self.room() {
            self.bindings.push(BindingOccurrence { binding_id, role, span });
        }
    }

    pub fn annotation(&mut self, text: &str, span: SourceSpan) {
        if !self.room() { return; }
        let mut end = text.len().min(ANNOTATION_BYTE_LIMIT);
        while !text.is_char_boundary(end) { end -= 1; }
        self.annotations.push(AnnotationOccurrence {
            text: text[..end].to_owned(), truncated: end != text.len(), span,
        });
    }

    pub fn opaque(&mut self, reason: &'static str, span: SourceSpan) {
        if self.room() { self.opaque_regions.push(OpaqueOccurrence { reason, span }); }
    }

    pub fn sort(&mut self) {
        self.bindings.sort_by_key(|item| (item.span.start.byte_offset, item.span.end.byte_offset, item.binding_id));
        self.annotations.sort_by_key(|item| item.span.start.byte_offset);
        self.opaque_regions.sort_by_key(|item| item.span.start.byte_offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formatter::SourcePosition;

    #[test]
    fn bounds_and_unicode_truncation_are_explicit() {
        let start = SourcePosition { byte_offset: 0, line_one_based: 1, column_one_based: 1 };
        let span = SourceSpan { start, end: start };
        let mut map = EmissionMap::default();
        map.annotation(&"界".repeat(2000), span);
        assert!(map.annotations[0].truncated);
        assert_eq!(map.annotations[0].text.len(), 4095);
        for id in 0..OCCURRENCE_LIMIT { map.binding(id as u64, "read", span); }
        assert_eq!(map.bindings.len() + map.annotations.len(), OCCURRENCE_LIMIT);
        assert_eq!(map.omitted_occurrences, 1);
    }
}
