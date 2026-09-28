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

fn ad(op: u8, a: u8, d: i16) -> u32 {
    u32::from(op) | (u32::from(a) << 8) | (u32::from(d as u16) << 16)
}

/// One prototype of a hand-built v6 module: 8 registers, no debug info.
struct Proto {
    upvalues: u8,
    code: Vec<u32>,
    constants: Vec<Vec<u8>>,
    children: Vec<u8>,
}

/// A v6 module whose main prototype is the last one.
fn module(strings: &[&[u8]], protos: &[Proto]) -> Vec<u8> {
    let mut bytes = vec![6, 3, strings.len() as u8];
    for string in strings {
        bytes.push(string.len() as u8);
        bytes.extend_from_slice(string);
    }
    bytes.push(0); // no userdata type names
    bytes.push(protos.len() as u8);
    for proto in protos {
        bytes.extend([8, 0, proto.upvalues, 0, 0, 0, proto.code.len() as u8]);
        for word in &proto.code {
            bytes.extend(word.to_le_bytes());
        }
        bytes.push(proto.constants.len() as u8);
        for constant in &proto.constants {
            bytes.extend(constant);
        }
        bytes.push(proto.children.len() as u8);
        bytes.extend(&proto.children);
        bytes.extend([0, 0, 0, 0]); // line defined, name, no line info, no debug info
    }
    bytes.push(protos.len() as u8 - 1);
    bytes
}

fn main_only(strings: &[&[u8]], code: Vec<u32>, constants: Vec<Vec<u8>>) -> Vec<u8> {
    module(strings, &[Proto { upvalues: 0, code, constants, children: vec![] }])
}

fn decompile(bytes: &[u8]) -> Result<String, String> {
    try_decompile_bytecode_with_options(bytes, 1, None, Default::default())
}

/// `t:<method>()` on a fresh table, with the method name as string constant 1.
fn method_call(method: &[u8]) -> Vec<u8> {
    main_only(&[method], vec![abc(53, 0, 0, 0), 0, abc(20, 1, 0, 0), 0, abc(21, 1, 2, 1), abc(22, 0, 1, 0)],
        vec![vec![3, 1]])
}

#[test]
fn namecall_keys_that_are_not_identifiers_become_index_calls() {
    assert!(decompile(&method_call(b"Timeout")).unwrap().contains(":Timeout()"));
    let spaced = decompile(&method_call(b"foo bar")).unwrap();
    assert!(spaced.contains("[\"foo bar\"]("), "{spaced}");
    assert!(!spaced.contains(":foo bar"), "{spaced}");
    // A byte string that is not UTF-8 is still a valid table key.
    let latin1 = decompile(&method_call(b"T\xe9meout")).unwrap();
    assert!(latin1.contains("]("), "{latin1}");
    assert!(decompile(&method_call(b"end")).unwrap().contains("[\"end\"]("));
}

#[test]
fn namecall_must_be_followed_by_its_call() {
    let constants = || vec![vec![3, 1]];
    for code in [
        vec![abc(20, 1, 0, 0), 0, abc(22, 0, 1, 0)],
        vec![abc(53, 0, 0, 0), 0, abc(20, 1, 0, 0), 0, abc(21, 2, 2, 1), abc(22, 0, 1, 0)],
        // A branch into the CALL separates it from its NAMECALL.
        vec![abc(53, 0, 0, 0), 0, ad(23, 0, 2), abc(20, 1, 0, 0), 0, abc(21, 1, 2, 1), abc(22, 0, 1, 0)],
    ] {
        let error = decompile(&main_only(&[b"Run"], code, constants())).unwrap_err();
        assert!(error.contains("invalid method call"), "{error}");
    }
}

#[test]
fn closures_must_carry_exactly_their_captures() {
    let child = || Proto { upvalues: 1, code: vec![abc(9, 0, 0, 0), abc(22, 0, 2, 0)], constants: vec![], children: vec![] };
    let main = |code: Vec<u32>| Proto { upvalues: 0, code, constants: vec![], children: vec![0] };
    let with = |code: Vec<u32>| decompile(&module(&[], &[child(), main(code)]));

    let source = with(vec![ad(4, 1, 5), ad(19, 0, 0), abc(70, 0, 1, 0), abc(22, 0, 2, 0)]).unwrap();
    assert!(source.contains("return function()"), "{source}");
    for (code, expected) in [
        (vec![ad(19, 0, 0), abc(22, 0, 2, 0)], "invalid closure capture list"),
        (vec![ad(19, 0, 0), abc(70, 0, 1, 0), abc(70, 0, 1, 0), abc(22, 0, 2, 0)], "capture outside a closure"),
        (vec![ad(23, 0, 1), ad(19, 0, 0), abc(70, 0, 1, 0), abc(22, 0, 2, 0)], "invalid closure capture list"),
        (vec![abc(70, 0, 1, 0), abc(22, 0, 1, 0)], "capture outside a closure"),
    ] {
        let error = with(code).unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
}
