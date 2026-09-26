use luau_lifter::{BatchInput, decompile_batch, try_decompile_bytecode_with_options};

fn abc(op: u8, a: u8, b: u8, c: u8) -> u32 {
    u32::from(op) | (u32::from(a) << 8) | (u32::from(b) << 16) | (u32::from(c) << 24)
}

fn chunk(words: &[u32]) -> Vec<u8> {
    let mut bytes = vec![6, 3, 0, 0, 1, 3, 0, 0, 0, 0, 0, words.len() as u8];
    for word in words {
        bytes.extend(word.to_le_bytes());
    }
    bytes.extend([0; 7]);
    bytes
}

#[test]
fn loadb_does_not_execute_skipped_instructions() {
    let bytes = chunk(&[abc(3, 0, 1, 1), abc(4, 0, 5, 0), abc(22, 0, 2, 0)]);
    let source = try_decompile_bytecode_with_options(&bytes, 1, None, Default::default()).unwrap();
    assert_eq!(source.trim(), "return true");
}

#[test]
fn sparse_setlist_declines_constructor_fold_without_rejecting_input() {
    let bytes = chunk(&[
        abc(53, 0, 0, 0),
        0,
        abc(4, 1, 42, 0),
        abc(55, 0, 1, 2),
        5,
        abc(22, 0, 2, 0),
    ]);
    let source = try_decompile_bytecode_with_options(&bytes, 1, None, Default::default()).unwrap();
    assert!(source.contains("[5] = 42"), "{source}");
}

#[test]
fn invalid_operands_are_errors_and_do_not_abort_batch() {
    let good = chunk(&[abc(4, 0, 7, 0), abc(22, 0, 2, 0)]);
    let samples = [
        chunk(&[abc(23, 0, 100, 0), abc(4, 0, 5, 0), abc(22, 0, 2, 0)]),
        chunk(&[
            abc(53, 0, 0, 0),
            0,
            abc(4, 1, 42, 0),
            abc(55, 0, 1, 2),
            0,
            abc(22, 0, 2, 0),
        ]),
        chunk(&[abc(4, 255, 1, 0), abc(22, 0, 2, 0)]),
    ];
    for bad in samples {
        let result = std::panic::catch_unwind(|| {
            try_decompile_bytecode_with_options(&bad, 1, None, Default::default())
        });
        assert!(
            result
                .expect("try API must never unwind to its caller")
                .is_err()
        );
        let output = decompile_batch(&[
            BatchInput {
                bytecode: &bad,
                encode_key: 1,
                script_name: None,
            },
            BatchInput {
                bytecode: &good,
                encode_key: 1,
                script_name: None,
            },
        ]);
        assert!(output[0].is_err());
        assert_eq!(output[1].as_ref().unwrap().trim(), "return 7");
    }
}

#[test]
fn lift_panic_is_an_error_with_its_original_message() {
    let bad = chunk(&[abc(56, 0, 0, 0), abc(22, 0, 1, 0)]);
    let result = std::panic::catch_unwind(|| {
        try_decompile_bytecode_with_options(&bad, 1, None, Default::default())
    });
    let error = result.expect("try API leaked an unwind").unwrap_err();
    assert!(error.contains("panicked: FORNPREP:"), "{error}");
}
