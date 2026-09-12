//! Display labels for known emitter annotations. The original text remains in
//! metadata; matching a label does not establish an original source/proof claim.
pub fn compact_text(text: &str) -> Option<&'static str> {
    match text {
        " equivalent calls inferred from this helper; original call sites unknown" => Some("inferred helper"),
        "equivalent call inferred; original call site unknown"
        | " equivalent call inferred; original call site unknown" => Some("inferred call"),
        " [-O2 INLINED, UNHOOKABLE] reconstructed definition;" => Some("inferred helper"),
        "inlined by Luau -O2 (UNHOOKABLE)"
        | " [-O2 INLINED, UNHOOKABLE] reconstructed call" => Some("inferred call"),
        " equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown" => Some("inferred arithmetic helper"),
        _ if text.starts_with("[DEDUP] synthesized from ") => Some("synthesized helper"),
        _ if text == crate::reroll_arithmetic::MARKER => Some("synthesized arithmetic loop"),
        _ => None,
    }
}
