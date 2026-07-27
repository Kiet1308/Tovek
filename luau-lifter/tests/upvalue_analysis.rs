//! End-to-end raw provenance fixture compiled by the real Luau compiler.

use base64::Engine;
use luau_lifter::upvalue_analysis::{AnalysisStatus, CaptureKind, UpvalueAccessKind};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const BYTECODE: &[u8] = include_bytes!("fixtures/upvalue_analysis_nested.luaubc");
const BYTECODE_G0: &[u8] = include_bytes!("fixtures/upvalue_analysis_nested_g0.luaubc");

#[test]
fn maps_nested_capture_slots_from_compiled_bytecode() {
    let analysis = luau_lifter::analyze_upvalues_raw(BYTECODE, 1)
        .expect("compiled fixture must deserialize and analyze");

    assert_eq!(analysis.status, AnalysisStatus::Complete);
    assert!(analysis
        .functions
        .iter()
        .any(|function| function.lexical_depth >= 3));

    let captures = analysis
        .prototypes
        .iter()
        .flat_map(|prototype| &prototype.closure_sites)
        .flat_map(|site| &site.captures)
        .collect::<Vec<_>>();
    assert!(captures
        .iter()
        .any(|capture| capture.kind == CaptureKind::Value));
    assert!(captures
        .iter()
        .any(|capture| capture.kind == CaptureKind::Reference));
    assert!(captures
        .iter()
        .any(|capture| capture.kind == CaptureKind::ParentUpvalue));

    for site in analysis
        .prototypes
        .iter()
        .flat_map(|prototype| &prototype.closure_sites)
    {
        for (slot, capture) in site.captures.iter().enumerate() {
            assert_eq!(capture.target_slot_zero_based, slot);
            assert_eq!(capture.ordinal_one_based, slot + 1);
        }
    }

    let accesses = analysis
        .prototypes
        .iter()
        .flat_map(|prototype| &prototype.accesses)
        .collect::<Vec<_>>();
    assert!(accesses
        .iter()
        .any(|access| access.kind == UpvalueAccessKind::Read));
    assert!(accesses
        .iter()
        .any(|access| access.kind == UpvalueAccessKind::Write));
}

#[test]
fn artifact_reconciles_names_without_changing_source() {
    let source_only = luau_lifter::try_decompile_bytecode_with_script_name(BYTECODE, 1, None)
        .expect("source-only decompile must succeed");
    let artifact = luau_lifter::try_decompile_bytecode_artifact(BYTECODE, 1, None)
        .expect("artifact decompile must succeed");

    assert_eq!(artifact.source, source_only);
    let analysis = artifact
        .upvalue_analysis
        .expect("compiled bytecode must produce static analysis");
    assert!(analysis.functions.len() >= 4);
    assert!(analysis
        .functions
        .iter()
        .flat_map(|function| &function.upvalues)
        .all(|upvalue| !upvalue.name.text.is_empty()));
    assert!(analysis
        .functions
        .iter()
        .skip(1)
        .all(|function| function.function_id.starts_with("root:p")));
    assert!(analysis
        .functions
        .iter()
        .skip(1)
        .all(|function| function.emitted && !function.occurrences.is_empty()));
    for function in analysis.functions.iter().skip(1) {
        for occurrence in &function.occurrences {
            let emitted = &artifact.source
                [occurrence.span.start.byte_offset..occurrence.span.end.byte_offset];
            assert!(
                emitted.starts_with("function"),
                "unexpected span: {emitted:?}"
            );
            assert!(emitted.ends_with("end"), "unexpected span: {emitted:?}");
        }
    }
    assert!(analysis
        .functions
        .iter()
        .flat_map(|function| &function.upvalues)
        .all(|upvalue| upvalue.name.provenance
            != luau_lifter::upvalue_analysis::NameProvenance::Fallback));

    let first = serde_json::to_vec_pretty(&analysis).expect("analysis must serialize");
    let second = serde_json::to_vec_pretty(&analysis).expect("analysis must serialize twice");
    assert_eq!(
        first, second,
        "analysis serialization must be deterministic"
    );
}

#[test]
fn stripped_debug_deep_forwarding_uses_emitted_bindings() {
    let artifact = luau_lifter::try_decompile_bytecode_artifact(BYTECODE_G0, 1, None)
        .expect("stripped-debug artifact decompile must succeed");
    let analysis = artifact
        .upvalue_analysis
        .expect("stripped-debug bytecode must produce static analysis");

    let deepest = analysis
        .functions
        .iter()
        .max_by_key(|function| function.lexical_depth)
        .expect("fixture must contain nested functions");
    assert!(deepest.lexical_depth >= 3);
    assert!(deepest
        .upvalues
        .iter()
        .any(|upvalue| upvalue.capture_chain.len() >= 2));
    assert!(deepest.upvalues.iter().all(|upvalue| {
        upvalue.name.provenance == luau_lifter::upvalue_analysis::NameProvenance::DecompilerBinding
            && !upvalue.name.text.starts_with("upvalue_")
    }));
    assert!(deepest.upvalues.iter().all(|upvalue| {
        !upvalue.slot_binding_id.is_empty()
            && upvalue.capture_source_binding.is_some()
            && !upvalue.emitted_bindings.is_empty()
    }));

    let closure_site_ids = analysis
        .functions
        .iter()
        .filter_map(|function| function.closure_site_id.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(closure_site_ids.len(), analysis.functions.len() - 1);
    assert!(closure_site_ids
        .iter()
        .all(|site_id| site_id.starts_with("root:p")));
    assert!(analysis
        .functions
        .iter()
        .flat_map(|function| &function.occurrences)
        .all(|occurrence| !occurrence.human_path.is_empty()));
}

#[test]
fn folder_artifacts_are_identical_across_thread_counts() {
    let root = std::env::temp_dir().join(format!(
        "tovek-upvalue-thread-determinism-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let input = root.join("input");
    let single = root.join("single");
    let parallel = root.join("parallel");
    fs::create_dir_all(&input).unwrap();
    fs::write(
        input.join("Fixture.lua"),
        base64::engine::general_purpose::STANDARD.encode(BYTECODE),
    )
    .unwrap();

    run_folder_decompile(&input, &single, 1);
    run_folder_decompile(&input, &parallel, 4);

    assert_eq!(collect_files(&single), collect_files(&parallel));
    let _ = fs::remove_dir_all(root);
}

fn run_folder_decompile(input: &Path, output: &Path, threads: usize) {
    let result = Command::new(env!("CARGO_BIN_EXE_luau-lifter"))
        .arg("decompile-folder")
        .arg(input)
        .arg(output)
        .arg("--key")
        .arg("1")
        .arg("--threads")
        .arg(threads.to_string())
        .arg("--emit-upvalue-analysis")
        .arg("--output-extension")
        .arg("lua")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "folder decompile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn collect_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}
