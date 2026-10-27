//! How vector constants print. Sources, compiled with Luau c2ec0d4e5ca5 at
//! `--binary -O2 -g1`:
//!
//! vector_spelling_proof (`--vector-lib=Vector3 --vector-ctor=new`): the
//! FASTCALL of the vector builtin in `offset` falls back to `Vector3.new`.
//!     local function offset(x) return Vector3.new(x, 1, 2) end
//!     local base = Vector3.new(4, 1, 2)
//!     local down = -Vector3.new(0, 10, 0)
//!     local vector = offset(3)
//!     print(base, down, vector)
//!
//! vector_spelling_roblox (same options, opcodes then encoded as a Roblox
//! client encodes them, op * 227): no call proves the constructor, the
//! encoding does.
//!     local base = Vector3.new(4, 1, 2)
//!     local down = -Vector3.new(0, 10, 0)
//!     print(base, down)
//!
//! vector_spelling_inline (no vector options; keeps the snapshot):
//!     local base = vector.create(4, 1, 2)
//!     local function lift(v) return v + vector.create(0, 0.5, 0) end
//!     print(lift(base))
//!
//! vector_single_constant (`--fflags=false --vector-lib=vector --vector-ctor=create`):
//!     return vector.create(1, 2, 3)

const PROOF: &[u8] = include_bytes!("fixtures/vector_spelling_proof.luaubc");
const ROBLOX: &[u8] = include_bytes!("fixtures/vector_spelling_roblox.luaubc");
const INLINE: &[u8] = include_bytes!("fixtures/vector_spelling_inline.luaubc");

fn decompile(bytecode: &[u8], key: u8) -> String {
    luau_lifter::try_decompile_bytecode_with_options(bytecode, key, None, Default::default())
        .expect("decompiles")
}

#[test]
fn a_vector3_new_fallback_spells_constants_as_vector3_new() {
    let source = decompile(PROOF, 1);
    assert!(source.contains("Vector3.new(4, 1, 2)"), "{source}");
    // (-0, -10, -0) is the negation the compiler folded.
    assert!(source.contains("-Vector3.new(0, 10, 0)"), "{source}");
    assert!(!source.contains("createVector") && !source.contains("vector.create"), "{source}");
}

#[test]
fn roblox_client_bytecode_spells_constants_as_vector3_new() {
    let source = decompile(ROBLOX, 203);
    assert!(source.contains("print(Vector3.new(4, 1, 2), -Vector3.new(0, 10, 0))"), "{source}");
    assert!(!source.contains("createVector"), "{source}");
}

#[test]
fn other_chunks_keep_the_constructor_snapshot() {
    // Code outside the chunk may replace a function's environment after it
    // loaded; a constant must not look the constructor up when it runs.
    let source = decompile(INLINE, 1);
    assert!(source.starts_with("local createVector = vector.create"), "{source}");
    assert!(source.contains("createVector(0, 0.5, 0)") && !source.contains("Vector3"), "{source}");
}

#[test]
fn vector_producer_records_name_retained_final_bindings() {
    let single = include_bytes!("fixtures/vector_single_constant.luaubc");
    for bytecode in [single.as_slice(), INLINE] {
        let plain = decompile(bytecode, 1);
        for detailed in [false, true] {
            let artifact = luau_lifter::try_decompile_bytecode_artifact_with_options(
                bytecode, 1, None, luau_lifter::DecompileOptions {
                    emit_binding_provenance: detailed,
                    ..Default::default()
                },
            ).unwrap();
            assert_eq!(artifact.source, plain);
            let analysis = artifact.upvalue_analysis.unwrap();
            let report = analysis.materialize_vectors.unwrap();
            assert_eq!(report["model"], "entry_constructor_snapshot_v1");
            assert_eq!(report["introduced_locals"], 1);
            if let Some(trace) = analysis.binding_provenance {
                let group = trace["local_producers"]["passes"].as_array().unwrap().iter()
                    .find(|group| group["pass"] == "materialize_vectors").unwrap();
                assert_eq!(group["records"], report["introduced_bindings"]["records"]);
                for record in group["records"].as_array().unwrap() {
                    let binding = trace["final_bindings"].as_array().unwrap().iter()
                        .find(|binding| binding["binding_id"] == record["binding_id"]).unwrap();
                    assert_eq!(binding["lineage"], serde_json::json!([]));
                    assert_eq!(binding["emitter_introduction"]["pass"], "materialize_vectors");
                }
            }
        }
    }
}
