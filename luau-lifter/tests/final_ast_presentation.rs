//! Sources are semantic-roundtrip fixtures, pinned with Luau c2ec0d4e5ca50796ba174a7565298f59aa572268.
//! Compile with --binary --fflags=false and the O/g profile in each filename.

fn decompile(bytes: &[u8]) -> String {
    luau_lifter::try_decompile_bytecode_with_options(bytes, 1, None, Default::default())
        .expect("final-AST presentation fixture decompiles")
}

#[test]
fn return_annotations_prove_types_and_pack_arity_without_trusting_hints_or_libraries() {
    for bytes in [include_bytes!("fixtures/proven_return_types_O0g1.luaubc").as_slice(),
        include_bytes!("fixtures/proven_return_types_O2g1.luaubc").as_slice()] {
        let source = decompile(bytes);
        let header = |name: &str| source.lines().find(|line| line.contains(&format!("function {name}(")))
            .unwrap_or_else(|| panic!("missing {name}: {source}"));
        for (name, annotation) in [("booleanValue", "boolean"), ("choice", "number | string"), ("tuple", "(number, string, nil)")] {
            assert!(header(name).contains(&format!("): {annotation}")), "{source}");
        }
        for name in ["maybe", "differentArity", "unknownOperator", "unknownLibrary"] {
            assert!(!header(name).contains("): "), "unproven {name}: {source}");
        }
        assert!(source.contains("function(): string"), "anonymous returns: {source}");
        assert!(source.lines().any(|line| line.contains("function ") && line.contains("test(") && line.contains("): boolean")), "method returns: {source}");
    }
}

#[test]
fn available_lines_keep_gaps_and_stripped_lines_use_the_existing_fallback() {
    let located = decompile(include_bytes!("fixtures/source_spacing_O2g1.luaubc"));
    let stripped = decompile(include_bytes!("fixtures/source_spacing_O2g0.luaubc"));
    assert!(located.contains("print(\"first\")\n\tprint(\"adjacent\")"), "{located}");
    assert!(located.contains("print(\"adjacent\")\n\n\tprint(\"separated\")"), "{located}");
    assert!(located.contains("print(\"nested-first\")\n\n\t\tprint(\"nested-separated\")"), "{located}");
    assert!(stripped.contains("print(\"adjacent\")\n\tprint(\"separated\")"), "{stripped}");
}

#[test]
fn readable_conditionals_use_final_names_for_the_width_limit() {
    for bytes in [include_bytes!("fixtures/readable_if_final_names_O0g1.luaubc").as_slice(),
        include_bytes!("fixtures/readable_if_final_names_O2g1.luaubc").as_slice()] {
        let source = decompile(bytes);
        assert!(source.contains("selectedCharacterTransparencyOverrideValue"), "{source}");
        assert!(source.contains("\n\tif "), "long final name must retain branches: {source}");
        for line in source.lines().filter(|line| line.contains("= if ") || line.contains("return if ")) {
            let columns = line.chars().map(|ch| if ch == '\t' { 4 } else { 1 }).sum::<usize>();
            assert!(columns <= 72, "{columns} columns: {line}");
        }
    }
}
