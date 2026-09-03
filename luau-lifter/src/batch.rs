//! Parallel folder decompiler — the native replacement for `decompile_folder.sh`.
//!
//! Walks SRC for `*.lua` files (each a UniversalSynSaveInstance text wrapper:
//! `--` comment header lines, then a base64 Luau-bytecode blob), decodes them
//! exactly like the bash pipeline `grep -v '^--' | tr -d ' \t\r\n' | base64 -d`,
//! decompiles in parallel with rayon, and mirrors the tree into OUT with every
//! `.lua` renamed to `.luau`.
//!
//! The decode/decompile/write logic lives in [`crate::decompile_core`] so it is
//! shared byte-for-byte with the `validate-folder` driver.

use crate::decompile_core::{
    AnalysisManifestEntry, AnalysisUnavailable, ExportManifestInventory, GeneratedSourceRecord,
    Outcome, acquire_output_generation_lock, atomic_write_contained,
    build_work_from_export_manifest, build_work_with_extension, invalidate_analysis_manifest,
    precreate_dirs, prepare_analysis_root, process_one, process_one_with_analysis, sha256_hex,
    size_pool, validate_analysis_root_for_write,
};
use luau_lifter::{DecompileDiagnostic, DecompileOptions};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Run the folder decompiler. Returns a process exit code (0 = no failures).
pub fn run(
    src: &Path,
    out: &Path,
    key: u8,
    threads: usize,
    verbose: bool,
    options: DecompileOptions,
    emit_upvalue_analysis: bool,
    output_extension: &str,
    export_manifest: Option<&Path>,
) -> i32 {
    let start = Instant::now();

    let (_src_root, out_root, work, inventory) = match export_manifest {
        Some(manifest) => {
            match build_work_from_export_manifest(src, out, output_extension, manifest) {
                Ok((src_root, out_root, work, inventory)) => (src_root, out_root, work, inventory),
                Err(code) => return code,
            }
        }
        None => match build_work_with_extension(src, out, output_extension) {
            Ok((src_root, out_root, work)) => {
                let total_scripts = work.len();
                (
                    src_root,
                    out_root,
                    work,
                    ExportManifestInventory {
                        total_scripts,
                        ..ExportManifestInventory::default()
                    },
                )
            }
            Err(code) => return code,
        },
    };

    if work.is_empty() && inventory.total_scripts == 0 {
        eprintln!("no .lua files found under {}", src.display());
    }

    let generation_lock = match acquire_output_generation_lock(&out_root) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("error: acquire output generation lock: {error}");
            return 2;
        }
    };
    let locked_out_root = generation_lock.output_root();
    if emit_upvalue_analysis || !work.is_empty() {
        if let Err(error) = invalidate_analysis_manifest(locked_out_root) {
            eprintln!("error: invalidate previous analysis manifest: {error}");
            return 2;
        }
    }
    let analysis_root = if emit_upvalue_analysis {
        match prepare_analysis_root(locked_out_root) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: prepare analysis directory: {error}");
                return 2;
            }
        }
    } else {
        locked_out_root.join(".tovek-analysis")
    };
    if let Err(code) = precreate_dirs(&work) {
        return code;
    }

    size_pool(threads);

    // Hash the exact sorted input set (relative path + bytes) once per run so
    // an analysis manifest can be reproduced and compared without relying on
    // stale output-directory contents.
    let corpus_sha256 = match hash_corpus_inputs(&work) {
        Ok(hash) => hash,
        Err(error) => {
            eprintln!("error: hash corpus inputs: {error}");
            return 2;
        }
    };

    // Decompile in parallel. map_init gives each worker a reusable base64 scratch
    // buffer so we don't reallocate it per file.
    let outcomes: Vec<(
        Outcome,
        Option<AnalysisManifestEntry>,
        Option<AnalysisUnavailable>,
        Option<GeneratedSourceRecord>,
    )> = work
        .par_iter()
        .map_init(Vec::<u8>::new, |b64, w| {
            if emit_upvalue_analysis {
                process_one_with_analysis(w, key, b64, verbose, options, &analysis_root)
            } else {
                (process_one(w, key, b64, verbose, options), None, None, None)
            }
        })
        .collect();

    // Tally on the main thread (collect() preserves input order, so the FAIL
    // list is deterministic).
    let (mut ok, mut skipped, mut fail) = (0usize, 0usize, inventory.failed_scripts);
    let mut entries = Vec::new();
    let mut diagnostics: Vec<FolderDiagnostic> = inventory
        .diagnostics
        .iter()
        .map(|diagnostic| FolderDiagnostic {
            export_id: diagnostic.export_id.clone(),
            script_path: diagnostic.script_path.clone(),
            status: if diagnostic.export_id.is_some() {
                "failed"
            } else {
                "manifest_issue"
            },
            code: diagnostic.code,
            message: diagnostic.message.clone(),
            evidence: None,
        })
        .collect();
    let mut unavailable = 0usize;
    let mut generated_sources = Vec::new();
    for (w, (o, entry, analysis_unavailable, source_record)) in work.iter().zip(&outcomes) {
        if let Some(source_record) = source_record {
            generated_sources.push(source_record.clone());
        }
        if let Some(entry) = entry {
            entries.push(entry.clone());
        }
        if let Some(analysis_unavailable) = analysis_unavailable {
            unavailable += 1;
            diagnostics.push(FolderDiagnostic {
                export_id: w
                    .volt_export
                    .as_ref()
                    .map(|metadata| metadata.export_id.clone()),
                script_path: w.rel.clone(),
                status: "analysis_unavailable",
                code: analysis_unavailable.code,
                message: analysis_unavailable.message.clone(),
                evidence: None,
            });
        }
        match o {
            Outcome::Ok => ok += 1,
            Outcome::Skipped => {
                skipped += 1;
                diagnostics.push(FolderDiagnostic {
                    export_id: w
                        .volt_export
                        .as_ref()
                        .map(|metadata| metadata.export_id.clone()),
                    script_path: w.rel.clone(),
                    status: "skipped",
                    code: "empty_bytecode_payload",
                    message: "Raw bytecode entry had no decoded payload".to_string(),
                    evidence: None,
                });
            }
            Outcome::Fail(reason) => {
                fail += 1;
                let (code, evidence) = classify_failure(&reason);
                diagnostics.push(FolderDiagnostic {
                    export_id: w
                        .volt_export
                        .as_ref()
                        .map(|metadata| metadata.export_id.clone()),
                    script_path: w.rel.clone(),
                    status: "failed",
                    code,
                    message: reason.clone(),
                    evidence,
                });
                eprintln!("FAIL {}\n      {reason}", w.rel);
            }
        }
    }

    if emit_upvalue_analysis {
        if let Err(error) = write_analysis_manifest_with_audit(
            &analysis_root,
            output_extension,
            options,
            &inventory,
            unavailable,
            skipped,
            fail,
            key,
            rayon::current_num_threads(),
            corpus_sha256,
            generated_sources,
            entries,
            diagnostics,
        ) {
            fail += 1;
            eprintln!("FAIL analysis manifest\n      {error}");
        }
    }

    let total = inventory.total_scripts;
    let elapsed = start.elapsed();
    let secs = elapsed.as_secs_f64();
    let per_ms = if total > 0 {
        secs * 1000.0 / total as f64
    } else {
        0.0
    };
    let fps = if secs > 0.0 { total as f64 / secs } else { 0.0 };

    eprintln!("----------------------------------------");
    eprintln!("Done: {ok} decompiled, {skipped} skipped (no bytecode), {fail} failed.");
    if inventory.manifest_issue_count > 0 {
        eprintln!(
            "Export manifest: {} consistency issue(s).",
            inventory.manifest_issue_count
        );
    }
    eprintln!("Output: {}", out_root.display());
    eprintln!(
        "Time: {secs:.2}s  ({total} files, {per_ms:.1} ms/file, {fps:.0} files/s, {} threads)",
        rayon::current_num_threads()
    );

    i32::from(fail > 0 || inventory.manifest_issue_count > 0)
}

#[derive(Serialize)]
struct FolderDiagnostic {
    #[serde(skip_serializing_if = "Option::is_none")]
    export_id: Option<String>,
    script_path: String,
    status: &'static str,
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence: Option<Vec<DecompileDiagnostic>>,
}

/// Convert the legacy string error into a stable top-level code and preserve
/// any structured per-function diagnostics appended by the lifter.  The
/// parser deliberately falls back to a coarse code for older/errors outside
/// the structuring pipeline, so mixed-version corpus manifests remain useful.
fn classify_failure(reason: &str) -> (&'static str, Option<Vec<DecompileDiagnostic>>) {
    const MARKER: &str = " | diagnostics=";
    if let Some((_, payload)) = reason.split_once(MARKER) {
        if let Ok(diagnostics) = serde_json::from_str::<Vec<DecompileDiagnostic>>(payload) {
            let code = diagnostics
                .first()
                .map(|diagnostic| match diagnostic.code.as_str() {
                    "residual_control_flow" => "residual_control_flow",
                    "panic" => "panic",
                    code if code.starts_with("source_like_unsafe_") => "source_like_rejection",
                    "source_like_unsupported" => "source_like_rejection",
                    _ => "processing_failed",
                })
                .unwrap_or("processing_failed");
            return (code, Some(diagnostics));
        }
    }
    let code = if reason.starts_with("deserialize:") {
        "deserialize"
    } else if reason.starts_with("base64:") {
        "base64"
    } else if reason.contains("residual goto/label") {
        "residual_control_flow"
    } else if reason.contains("unsupported certified fallback") {
        "certified_fallback_unavailable"
    } else if reason.starts_with("formatting failed") {
        "formatting"
    } else {
        "processing_failed"
    };
    (code, None)
}

#[derive(Serialize)]
struct FolderAnalysisManifest {
    schema_name: &'static str,
    schema_version: u32,
    generation_id: String,
    status: &'static str,
    tovek_version: &'static str,
    output_extension: String,
    decompile_option_bits: u32,
    total_scripts: usize,
    processed_scripts: usize,
    analyzed_scripts: usize,
    partial_analysis_scripts: usize,
    analysis_unavailable_scripts: usize,
    skipped_scripts: usize,
    failed_scripts: usize,
    manifest_issue_count: usize,
    generated_source_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volt_export_manifest_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volt_export_schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volt_export_schema_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volt_export_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volt_export_declared_counts: Option<crate::decompile_core::VoltExportCounts>,
    scripts: Vec<AnalysisManifestEntry>,
    diagnostics: Vec<FolderDiagnostic>,
    #[serde(flatten)]
    corpus_audit: CorpusAuditMetadata,
}

#[derive(Serialize)]
struct CorpusAuditMetadata {
    audit_schema: &'static str,
    repo_head: String,
    corpus_sha256: String,
    input_count: usize,
    command: Vec<String>,
    encode_key: u8,
    threads: usize,
    control_flow_policy: &'static str,
    tool_path: String,
    tool_version: &'static str,
    tool_sha256: String,
    results_sha256: String,
    parser_status: &'static str,
    parser_failures: Vec<String>,
}

fn hash_corpus_inputs(work: &[crate::decompile_core::Work]) -> Result<String, String> {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    for item in work {
        let bytes = std::fs::read(&item.input)
            .map_err(|error| format!("read {}: {error}", item.input.display()))?;
        let path = item.rel.as_bytes();
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path);
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

#[derive(Serialize)]
struct SourceWriteProvenance<'a> {
    schema_name: &'static str,
    schema_version: u32,
    generation_id: &'a str,
    publication_state: &'static str,
    tovek_version: &'static str,
    output_extension: &'a str,
    source_count: usize,
    generated_source_paths: &'a [String],
    sources: &'a [GeneratedSourceRecord],
    #[serde(skip_serializing_if = "Option::is_none")]
    volt_export_manifest_sha256: Option<&'a str>,
}

// Test and compatibility helper retained for callers that only need the
// original analysis-manifest shape. Production folder runs use the audited
// variant below with corpus/tool hashes and command metadata.
#[allow(clippy::too_many_arguments)]
fn write_analysis_manifest(
    analysis_root: &Path,
    output_extension: &str,
    options: DecompileOptions,
    inventory: &ExportManifestInventory,
    analysis_unavailable_scripts: usize,
    skipped_scripts: usize,
    failed_scripts: usize,
    generated_sources: Vec<GeneratedSourceRecord>,
    scripts: Vec<AnalysisManifestEntry>,
    diagnostics: Vec<FolderDiagnostic>,
) -> Result<(), String> {
    write_analysis_manifest_with_audit(
        analysis_root,
        output_extension,
        options,
        inventory,
        analysis_unavailable_scripts,
        skipped_scripts,
        failed_scripts,
        1,
        rayon::current_num_threads(),
        "unknown".to_string(),
        generated_sources,
        scripts,
        diagnostics,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_analysis_manifest_with_audit(
    analysis_root: &Path,
    output_extension: &str,
    options: DecompileOptions,
    inventory: &ExportManifestInventory,
    analysis_unavailable_scripts: usize,
    skipped_scripts: usize,
    failed_scripts: usize,
    encode_key: u8,
    threads: usize,
    corpus_sha256: String,
    mut generated_sources: Vec<GeneratedSourceRecord>,
    mut scripts: Vec<AnalysisManifestEntry>,
    diagnostics: Vec<FolderDiagnostic>,
) -> Result<(), String> {
    validate_analysis_root_for_write(analysis_root)?;
    scripts.sort_by(|left, right| left.script_path.cmp(&right.script_path));
    generated_sources.sort_by(|left, right| left.path.cmp(&right.path));
    generated_sources.dedup_by(|left, right| left.path == right.path);
    let generated_source_paths = generated_sources
        .iter()
        .map(|source| source.path.clone())
        .collect();
    let mut diagnostics = diagnostics;
    diagnostics.sort_by(|left, right| {
        left.script_path
            .cmp(&right.script_path)
            .then_with(|| left.code.cmp(right.code))
            .then_with(|| left.export_id.cmp(&right.export_id))
            .then_with(|| left.message.cmp(&right.message))
    });
    let partial_analysis_scripts = scripts
        .iter()
        .filter(|script| script.status == luau_lifter::upvalue_analysis::AnalysisStatus::Partial)
        .count();
    let results_material = serde_json::to_vec(&(&generated_sources, &scripts, &diagnostics))
        .map_err(|error| error.to_string())?;
    let tool_path = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::canonicalize(path).ok())
        .unwrap_or_else(|| PathBuf::from("unknown"));
    let tool_sha256 = std::fs::read(&tool_path)
        .map(|bytes| sha256_hex(&bytes))
        .unwrap_or_else(|_| "unknown".to_string());
    let repo_head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|head| !head.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let control_flow_policy = if options.control_flow_policy
        == luau_lifter::ControlFlowOutputPolicy::StrictNoSyntheticControl
    {
        "StrictNoSyntheticControl"
    } else {
        "AllowCertifiedDispatcher"
    };
    let total_scripts = inventory.total_scripts;
    let export_status = inventory.manifest_status.as_deref();
    let status = if total_scripts == 0
        && inventory.manifest_issue_count == 0
        && export_status.map_or(true, |status| status == "complete")
    {
        "complete"
    } else if scripts.is_empty()
        && analysis_unavailable_scripts == 0
        && (failed_scripts > 0 || export_status == Some("failed"))
    {
        "failed"
    } else if skipped_scripts > 0
        || failed_scripts > 0
        || analysis_unavailable_scripts > 0
        || partial_analysis_scripts > 0
        || inventory.manifest_issue_count > 0
        || export_status.is_some_and(|status| status != "complete")
        || scripts.len() != total_scripts
    {
        "partial"
    } else {
        "complete"
    };
    let mut manifest = FolderAnalysisManifest {
        schema_name: "tovek-upvalue-analysis-manifest",
        schema_version: 1,
        generation_id: String::new(),
        status,
        tovek_version: env!("CARGO_PKG_VERSION"),
        output_extension: output_extension.to_string(),
        decompile_option_bits: options.bits(),
        total_scripts,
        processed_scripts: scripts.len() + analysis_unavailable_scripts + skipped_scripts,
        analyzed_scripts: scripts.len(),
        partial_analysis_scripts,
        analysis_unavailable_scripts,
        skipped_scripts,
        failed_scripts,
        manifest_issue_count: inventory.manifest_issue_count,
        generated_source_paths,
        volt_export_manifest_sha256: inventory.manifest_sha256.clone(),
        volt_export_schema: inventory.manifest_schema.clone(),
        volt_export_schema_version: inventory.manifest_schema_version,
        volt_export_status: inventory.manifest_status.clone(),
        volt_export_declared_counts: inventory.declared_counts.clone(),
        scripts,
        diagnostics,
        corpus_audit: CorpusAuditMetadata {
            audit_schema: "tovek-corpus-audit/v1",
            repo_head,
            corpus_sha256,
            input_count: total_scripts,
            command: std::env::args().collect(),
            encode_key,
            threads,
            control_flow_policy,
            tool_path: tool_path.to_string_lossy().replace('\\', "/"),
            tool_version: env!("CARGO_PKG_VERSION"),
            tool_sha256,
            results_sha256: format!("sha256:{}", sha256_hex(&results_material)),
            // The batch binary does not bundle the external official Luau
            // compiler. Keep an explicit status so an empty failure list is
            // never mistaken for a parser audit having run.
            parser_status: "not_run",
            parser_failures: Vec::new(),
        },
    };
    // Keep the publication identity stable across worker counts and invocation
    // metadata.  The audit envelope intentionally records command/thread/tool
    // provenance, but those fields must not perturb source sidecars or the
    // generation id used to bind them.  Results and output-affecting options
    // are the canonical identity inputs instead.
    let generation_material = serde_json::to_vec(&(
        &results_material,
        output_extension,
        options.bits(),
        &inventory.manifest_sha256,
        &inventory.manifest_schema,
        &inventory.manifest_schema_version,
        &inventory.manifest_status,
    ))
    .map_err(|error| error.to_string())?;
    manifest.generation_id = format!("sha256:{}", sha256_hex(&generation_material));
    let provenance = SourceWriteProvenance {
        schema_name: "tovek-source-write-provenance",
        schema_version: 2,
        generation_id: &manifest.generation_id,
        publication_state: "source_writes_committed",
        tovek_version: env!("CARGO_PKG_VERSION"),
        output_extension,
        source_count: manifest.generated_source_paths.len(),
        generated_source_paths: &manifest.generated_source_paths,
        sources: &generated_sources,
        volt_export_manifest_sha256: inventory.manifest_sha256.as_deref(),
    };
    let mut provenance_bytes =
        serde_json::to_vec_pretty(&provenance).map_err(|error| error.to_string())?;
    provenance_bytes.push(b'\n');
    let output_root = analysis_root.parent().ok_or_else(|| {
        format!(
            "analysis root {} has no output-root parent",
            analysis_root.display()
        )
    })?;
    atomic_write_contained(
        output_root,
        &analysis_root.join("source-write-provenance.json"),
        &provenance_bytes,
        true,
    )
    .map_err(|error| format!("publish source-write provenance: {error}"))?;

    let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    atomic_write_contained(
        output_root,
        &analysis_root.join("manifest.json"),
        &bytes,
        true,
    )
    .map_err(|error| format!("publish analysis manifest: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::prelude::*;
    use serde_json::{Value, json};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "tovek-{name}-{}-{}",
                std::process::id(),
                TEST_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry(
        export_id: &str,
        name: &str,
        path: Option<&str>,
        extraction_kind: &str,
        status: &str,
    ) -> Value {
        let raw = extraction_kind == "raw_bytecode";
        json!({
            "export_id": export_id,
            "ordinal_one_based": 1,
            "name": name,
            "class_name": "ModuleScript",
            "full_name": format!("Workspace.{name}"),
            "path_segments": [
                {"name": "Workspace", "class_name": "Workspace"},
                {"name": name, "class_name": "ModuleScript"}
            ],
            "dump_path": path.map(|path| format!("Dump/{path}")),
            "dump_relative_path": path,
            "legacy_candidate_dump_relative_path": path,
            "path_collision_resolved": false,
            "non_ascii_path_uniquified": false,
            "extraction_kind": extraction_kind,
            "is_bytecode": raw,
            "bytecode_analysis_eligible": raw,
            "payload_encoding": "base64",
            "status": status,
            "raw_size_bytes": 0,
            "payload_size_bytes": 0,
            "diagnostics": []
        })
    }

    fn write_manifest(path: &Path, scripts: Vec<Value>, counts: Value) {
        let saved = scripts
            .iter()
            .filter(|entry| entry["status"] == "saved")
            .count();
        let failed = scripts.len() - saved;
        let status = if failed == 0 {
            "complete"
        } else if saved > 0 {
            "partial"
        } else {
            "failed"
        };
        let manifest = json!({
            "schema": "volt-decompile-export-manifest",
            "schema_version": 1,
            "status": status,
            "root": "Dump",
            "manifest_path": "Dump/.volt-export-manifest.json",
            "payload_encoding": "base64",
            "counts": counts,
            "scripts": scripts
        });
        std::fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }

    #[test]
    fn mixed_export_manifest_is_partial_clean_and_deterministic() {
        let temp = TestDir::new("mixed-export-manifest");
        let src = temp.0.join("dump");
        let out = temp.0.join("out");
        std::fs::create_dir_all(src.join("Workspace")).unwrap();

        let bytecode = include_bytes!("../tests/fixtures/upvalue_analysis_nested_g0.luaubc");
        let encoded_bytecode = BASE64_STANDARD.encode(bytecode);
        std::fs::write(src.join("Workspace/Raw.lua"), &encoded_bytecode).unwrap();
        // Empty source is valid and must remain a clean zero-byte output.
        std::fs::write(src.join("Workspace/Fallback.lua"), b"").unwrap();

        let manifest_path = src.join(".volt-export-manifest.json");
        let mut raw_entry = entry(
            "volt-script-000001",
            "Raw",
            Some("Workspace/Raw.lua"),
            "raw_bytecode",
            "saved",
        );
        raw_entry["raw_size_bytes"] = json!(bytecode.len());
        raw_entry["payload_size_bytes"] = json!(encoded_bytecode.len());
        write_manifest(
            &manifest_path,
            vec![
                raw_entry,
                entry(
                    "volt-script-000002",
                    "Fallback",
                    Some("Workspace/Fallback.lua"),
                    "source_fallback",
                    "saved",
                ),
                entry("volt-script-000003", "Failure", None, "failure", "failed"),
            ],
            json!({
                "discovered": 3,
                "saved": 2,
                "failed": 1,
                "raw_bytecode": 1,
                "source_fallback": 1,
                "extraction_failure": 1
            }),
        );

        let invoke = || {
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                true,
                "lua",
                Some(&manifest_path),
            )
        };
        assert_eq!(invoke(), 1);
        assert_eq!(
            std::fs::read(out.join("Workspace/Fallback.lua")).unwrap(),
            b""
        );
        let first = std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap();
        let parsed: Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(parsed["status"], "partial");
        assert_eq!(parsed["total_scripts"], 3);
        assert_eq!(parsed["analyzed_scripts"], 1);
        assert_eq!(parsed["analysis_unavailable_scripts"], 1);
        assert_eq!(parsed["failed_scripts"], 1);
        assert_eq!(
            parsed["generated_source_paths"],
            json!(["Workspace/Fallback.lua", "Workspace/Raw.lua"])
        );
        let provenance: Value = serde_json::from_slice(
            &std::fs::read(out.join(".tovek-analysis/source-write-provenance.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(provenance["schema_name"], "tovek-source-write-provenance");
        assert_eq!(provenance["schema_version"], 2);
        assert_eq!(provenance["publication_state"], "source_writes_committed");
        assert_eq!(provenance["generation_id"], parsed["generation_id"]);
        assert_eq!(
            provenance["generated_source_paths"],
            parsed["generated_source_paths"]
        );
        assert_eq!(provenance["source_count"], 2);
        assert_eq!(provenance["sources"].as_array().unwrap().len(), 2);
        assert_eq!(
            provenance["volt_export_manifest_sha256"],
            parsed["volt_export_manifest_sha256"]
        );
        assert_eq!(parsed["scripts"][0]["export_id"], "volt-script-000001");
        assert_eq!(parsed["scripts"][0]["script_path"], "Workspace.Raw");
        assert_eq!(parsed["scripts"][0]["dump_path"], "Workspace/Raw.lua");
        assert_eq!(
            parsed["scripts"][0]["volt_export"]["full_name"],
            "Workspace.Raw"
        );
        let first_sidecar_rel = parsed["scripts"][0]["sidecar_path"]
            .as_str()
            .unwrap()
            .to_string();
        let first_sidecar = out.join(first_sidecar_rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let first_sidecar_bytes = std::fs::read(&first_sidecar).unwrap();
        assert_eq!(
            sha256_hex(&first_sidecar_bytes),
            parsed["scripts"][0]["sidecar_sha256"]
        );
        let first_sidecar_json: Value = serde_json::from_slice(&first_sidecar_bytes).unwrap();
        assert_eq!(first_sidecar_json["script_path"], "Workspace.Raw");
        assert_eq!(first_sidecar_json["dump_path"], "Workspace/Raw.lua");
        let raw_source = out.join("Workspace/Raw.lua");
        let raw_source_bytes = std::fs::read(&raw_source).unwrap();
        let raw_record = provenance["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["path"] == "Workspace/Raw.lua")
            .unwrap();
        assert_eq!(raw_record["byte_len"], raw_source_bytes.len());
        assert_eq!(raw_record["sha256"], sha256_hex(&raw_source_bytes));
        std::fs::write(&raw_source, b"-- user edit after interrupted generation\n").unwrap();
        let edited = std::fs::read(&raw_source).unwrap();
        assert!(
            raw_record["byte_len"] != edited.len() || raw_record["sha256"] != sha256_hex(&edited),
            "post-publication user edits must be distinguishable from committed bytes"
        );
        // Volt owns these exact prior-generation outputs and removes them
        // before asking Tovek to publish the next manifest generation.
        std::fs::remove_file(out.join("Workspace/Fallback.lua")).unwrap();
        std::fs::remove_file(out.join("Workspace/Raw.lua")).unwrap();
        assert_eq!(invoke(), 1);
        let second = std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap();
        assert_eq!(first, second);

        std::fs::remove_file(out.join("Workspace/Fallback.lua")).unwrap();
        std::fs::remove_file(out.join("Workspace/Raw.lua")).unwrap();
        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions {
                    dont_reuse_var: true,
                    ..DecompileOptions::default()
                },
                true,
                "lua",
                Some(&manifest_path),
            ),
            1
        );
        let changed: Value = serde_json::from_slice(
            &std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap(),
        )
        .unwrap();
        assert_ne!(changed["scripts"][0]["sidecar_path"], first_sidecar_rel);
        assert_eq!(std::fs::read(first_sidecar).unwrap(), first_sidecar_bytes);
    }

    #[test]
    fn bytecode_error_source_is_reported_as_analysis_unavailable() {
        let temp = TestDir::new("bytecode-error-analysis-unavailable");
        let src = temp.0.join("dump");
        let out = temp.0.join("out");
        std::fs::create_dir_all(src.join("Workspace")).unwrap();

        let mut bytecode = vec![0];
        bytecode.extend_from_slice(b"compile error: expected expression");
        let encoded = BASE64_STANDARD.encode(&bytecode);
        std::fs::write(src.join("Workspace/Error.lua"), &encoded).unwrap();
        let mut raw_entry = entry(
            "volt-script-error",
            "Error",
            Some("Workspace/Error.lua"),
            "raw_bytecode",
            "saved",
        );
        raw_entry["raw_size_bytes"] = json!(bytecode.len());
        raw_entry["payload_size_bytes"] = json!(encoded.len());
        let manifest_path = src.join(".volt-export-manifest.json");
        write_manifest(
            &manifest_path,
            vec![raw_entry],
            json!({
                "discovered": 1,
                "saved": 1,
                "failed": 0,
                "raw_bytecode": 1,
                "source_fallback": 0,
                "extraction_failure": 0
            }),
        );

        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                true,
                "lua",
                Some(&manifest_path),
            ),
            0
        );
        assert_eq!(
            std::fs::read(out.join("Workspace/Error.lua")).unwrap(),
            b"compile error: expected expression\n"
        );
        let parsed: Value = serde_json::from_slice(
            &std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["status"], "partial");
        assert_eq!(parsed["processed_scripts"], 1);
        assert_eq!(parsed["analyzed_scripts"], 0);
        assert_eq!(parsed["analysis_unavailable_scripts"], 1);
        assert_eq!(parsed["failed_scripts"], 0);
        assert_eq!(
            parsed["generated_source_paths"],
            json!(["Workspace/Error.lua"])
        );
        assert!(parsed["scripts"].as_array().unwrap().is_empty());
        assert_eq!(parsed["diagnostics"][0]["export_id"], "volt-script-error");
        assert_eq!(parsed["diagnostics"][0]["status"], "analysis_unavailable");
        assert_eq!(parsed["diagnostics"][0]["code"], "bytecode_error");

        std::fs::remove_file(out.join("Workspace/Error.lua")).unwrap();
        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                false,
                "lua",
                Some(&manifest_path),
            ),
            0
        );
        assert!(!out.join(".tovek-analysis/manifest.json").exists());
    }

    #[test]
    fn export_manifest_refuses_preexisting_raw_fallback_and_empty_destinations() {
        let temp = TestDir::new("manifest-preexisting-destinations");
        let src = temp.0.join("dump");
        let out = temp.0.join("out");
        std::fs::create_dir_all(src.join("Workspace")).unwrap();
        std::fs::create_dir_all(out.join("Workspace")).unwrap();

        let bytecode = include_bytes!("../tests/fixtures/upvalue_analysis_nested_g0.luaubc");
        let encoded = BASE64_STANDARD.encode(bytecode);
        std::fs::write(src.join("Workspace/Raw.lua"), &encoded).unwrap();
        std::fs::write(src.join("Workspace/Empty.lua"), b"").unwrap();
        let fallback_source = b"return 'fallback'";
        let fallback_encoded = BASE64_STANDARD.encode(fallback_source);
        std::fs::write(src.join("Workspace/Fallback.lua"), &fallback_encoded).unwrap();

        let sentinel = b"user-owned destination";
        for name in ["Raw.lua", "Empty.lua", "Fallback.lua"] {
            std::fs::write(out.join("Workspace").join(name), sentinel).unwrap();
        }

        let mut raw = entry(
            "volt-script-raw",
            "Raw",
            Some("Workspace/Raw.lua"),
            "raw_bytecode",
            "saved",
        );
        raw["raw_size_bytes"] = json!(bytecode.len());
        raw["payload_size_bytes"] = json!(encoded.len());
        let mut empty = entry(
            "volt-script-empty",
            "Empty",
            Some("Workspace/Empty.lua"),
            "raw_bytecode",
            "saved",
        );
        empty["raw_size_bytes"] = json!(0);
        empty["payload_size_bytes"] = json!(0);
        let mut fallback = entry(
            "volt-script-fallback",
            "Fallback",
            Some("Workspace/Fallback.lua"),
            "source_fallback",
            "saved",
        );
        fallback["raw_size_bytes"] = json!(fallback_source.len());
        fallback["payload_size_bytes"] = json!(fallback_encoded.len());
        let manifest_path = src.join(".volt-export-manifest.json");
        write_manifest(
            &manifest_path,
            vec![raw, empty, fallback],
            json!({
                "discovered": 3,
                "saved": 3,
                "failed": 0,
                "raw_bytecode": 2,
                "source_fallback": 1,
                "extraction_failure": 0
            }),
        );

        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                true,
                "lua",
                Some(&manifest_path),
            ),
            1
        );
        for name in ["Raw.lua", "Empty.lua", "Fallback.lua"] {
            assert_eq!(
                std::fs::read(out.join("Workspace").join(name)).unwrap(),
                sentinel
            );
        }
        let parsed: Value = serde_json::from_slice(
            &std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["failed_scripts"], 3);
        assert_eq!(parsed["generated_source_paths"], json!([]));
        let diagnostics = parsed["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 3);
        assert!(
            diagnostics
                .iter()
                .all(|item| item["code"] == "preexisting_output_destination")
        );
    }

    #[cfg(windows)]
    #[test]
    fn swapped_analysis_junction_cannot_redirect_manifest_or_provenance() {
        let temp = TestDir::new("analysis-parent-junction-swap");
        let out = std::fs::canonicalize(&temp.0).unwrap().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let out = std::fs::canonicalize(&out).unwrap();
        let analysis = prepare_analysis_root(&out).unwrap();
        let parked = out.join(".tovek-analysis-parked");
        let outside = temp.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::rename(&analysis, &parked).unwrap();
        let linked = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&analysis)
            .arg(&outside)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !linked {
            std::fs::rename(&parked, &analysis).unwrap();
            return;
        }

        let error = write_analysis_manifest(
            &analysis,
            "lua",
            DecompileOptions::default(),
            &ExportManifestInventory::default(),
            0,
            0,
            0,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap_err();
        assert!(error.contains("changed or now resolves") || error.contains("reparse point"));
        assert!(!outside.join("manifest.json").exists());
        assert!(!outside.join("source-write-provenance.json").exists());
        std::fs::remove_dir(&analysis).unwrap();
        std::fs::rename(&parked, &analysis).unwrap();
    }

    #[test]
    fn source_provenance_survives_final_manifest_publication_failure() {
        let temp = TestDir::new("provenance-survives-manifest-failure");
        let out = temp.0.join("out");
        let source = out.join("Workspace/Committed.lua");
        let analysis = out.join(".tovek-analysis");
        std::fs::create_dir_all(&analysis).unwrap();
        crate::decompile_core::atomic_write(&source, b"return 42\n").unwrap();
        // A directory at the final destination forces only manifest publication
        // to fail; the provenance snapshot must already be durable at that point.
        std::fs::create_dir(analysis.join("manifest.json")).unwrap();
        let analysis_root = std::fs::canonicalize(&analysis).unwrap();
        let inventory = ExportManifestInventory {
            total_scripts: 1,
            manifest_sha256: Some("export-manifest-sha256".to_string()),
            ..ExportManifestInventory::default()
        };

        let error = write_analysis_manifest(
            &analysis_root,
            "lua",
            DecompileOptions::default(),
            &inventory,
            0,
            0,
            0,
            vec![GeneratedSourceRecord {
                path: "Workspace/Committed.lua".to_string(),
                byte_len: 10,
                sha256: sha256_hex(b"return 42\n"),
            }],
            Vec::new(),
            Vec::new(),
        )
        .unwrap_err();

        assert!(error.contains("publish analysis manifest"));
        assert_eq!(std::fs::read(source).unwrap(), b"return 42\n");
        let provenance: Value = serde_json::from_slice(
            &std::fs::read(analysis.join("source-write-provenance.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(provenance["schema_name"], "tovek-source-write-provenance");
        assert_eq!(provenance["schema_version"], 2);
        assert_eq!(provenance["publication_state"], "source_writes_committed");
        assert_eq!(provenance["source_count"], 1);
        assert_eq!(
            provenance["generated_source_paths"],
            json!(["Workspace/Committed.lua"])
        );
        assert_eq!(provenance["sources"][0]["path"], "Workspace/Committed.lua");
        assert_eq!(provenance["sources"][0]["byte_len"], 10);
        assert_eq!(
            provenance["sources"][0]["sha256"],
            sha256_hex(b"return 42\n")
        );
        assert_eq!(
            provenance["volt_export_manifest_sha256"],
            "export-manifest-sha256"
        );
        assert!(
            provenance["generation_id"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }

    #[test]
    fn provenance_generation_id_binds_exact_committed_source_bytes() {
        let temp = TestDir::new("provenance-binds-source-bytes");
        let out = temp.0.join("out");
        let analysis = out.join(".tovek-analysis");
        std::fs::create_dir_all(&analysis).unwrap();
        let analysis_root = std::fs::canonicalize(&analysis).unwrap();
        let publish = |bytes: &[u8]| {
            write_analysis_manifest(
                &analysis_root,
                "lua",
                DecompileOptions::default(),
                &ExportManifestInventory::default(),
                0,
                0,
                0,
                vec![GeneratedSourceRecord {
                    path: "Workspace/Committed.lua".to_string(),
                    byte_len: bytes.len() as u64,
                    sha256: sha256_hex(bytes),
                }],
                Vec::new(),
                Vec::new(),
            )
            .unwrap();
            serde_json::from_slice::<Value>(
                &std::fs::read(analysis.join("source-write-provenance.json")).unwrap(),
            )
            .unwrap()
        };

        let first = publish(b"return 1\n");
        let second = publish(b"return 2\n");

        assert_ne!(first["generation_id"], second["generation_id"]);
        assert_ne!(
            first["sources"][0]["sha256"],
            second["sources"][0]["sha256"]
        );
        assert_eq!(
            first["sources"][0]["byte_len"],
            second["sources"][0]["byte_len"]
        );
    }

    #[test]
    fn analysis_request_publishes_fresh_manifest_for_empty_export() {
        let temp = TestDir::new("empty-export-manifest");
        let src = temp.0.join("dump");
        let out = temp.0.join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(out.join(".tovek-analysis")).unwrap();
        std::fs::write(out.join(".tovek-analysis/manifest.json"), b"stale").unwrap();
        let manifest_path = src.join(".volt-export-manifest.json");
        write_manifest(
            &manifest_path,
            vec![],
            json!({
                "discovered": 0,
                "saved": 0,
                "failed": 0,
                "raw_bytecode": 0,
                "source_fallback": 0,
                "extraction_failure": 0
            }),
        );

        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                true,
                "lua",
                Some(&manifest_path),
            ),
            0
        );
        let parsed: Value = serde_json::from_slice(
            &std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["status"], "complete");
        assert_eq!(parsed["total_scripts"], 0);
        assert!(
            parsed["generation_id"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(parsed["audit_schema"], "tovek-corpus-audit/v1");
        assert_eq!(parsed["input_count"], 0);
        assert_eq!(parsed["encode_key"], 1);
        assert!(
            parsed["corpus_sha256"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert!(
            parsed["results_sha256"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(parsed["parser_status"], "not_run");
        assert!(parsed["parser_failures"].as_array().unwrap().is_empty());
    }

    #[test]
    fn standard_decompile_creates_and_locks_a_fresh_output_root() {
        let temp = TestDir::new("standard-fresh-output-root");
        let src = temp.0.join("dump");
        let out = temp.0.join("new-output");
        std::fs::create_dir_all(&src).unwrap();
        let bytecode = include_bytes!("../tests/fixtures/upvalue_analysis_nested_g0.luaubc");
        std::fs::write(src.join("Script.lua"), BASE64_STANDARD.encode(bytecode)).unwrap();
        assert!(!out.exists());

        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                false,
                "luau",
                None,
            ),
            0
        );
        assert!(out.join("Script.luau").is_file());
        assert!(out.join(".tovek-output.lock").is_file());

        std::fs::create_dir_all(out.join(".tovek-analysis")).unwrap();
        std::fs::write(out.join(".tovek-analysis/manifest.json"), b"stale").unwrap();
        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                false,
                "luau",
                None,
            ),
            0
        );
        assert!(!out.join(".tovek-analysis/manifest.json").exists());
    }

    #[test]
    fn exporter_failure_with_zero_work_publishes_failed_manifest() {
        let temp = TestDir::new("failed-export-manifest");
        let src = temp.0.join("dump");
        let out = temp.0.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let manifest_path = src.join(".volt-export-manifest.json");
        write_manifest(
            &manifest_path,
            vec![entry(
                "volt-script-000001",
                "Failure",
                None,
                "failure",
                "failed",
            )],
            json!({
                "discovered": 1,
                "saved": 0,
                "failed": 1,
                "raw_bytecode": 0,
                "source_fallback": 0,
                "extraction_failure": 1
            }),
        );

        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                true,
                "lua",
                Some(&manifest_path),
            ),
            1
        );
        let parsed: Value = serde_json::from_slice(
            &std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["status"], "failed");
        assert_eq!(parsed["total_scripts"], 1);
        assert_eq!(parsed["failed_scripts"], 1);
        assert_eq!(parsed["diagnostics"][0]["code"], "exporter_entry_not_saved");
    }

    #[test]
    fn exporter_payload_sizes_are_checked_against_wrapper_and_decoded_bytes() {
        let temp = TestDir::new("payload-size-mismatch");
        let src = temp.0.join("dump");
        let out = temp.0.join("out");
        std::fs::create_dir_all(src.join("Workspace")).unwrap();
        let encoded = BASE64_STANDARD.encode(b"return 1");
        std::fs::write(src.join("Workspace/Fallback.lua"), &encoded).unwrap();
        let manifest_path = src.join(".volt-export-manifest.json");
        let mut fallback = entry(
            "volt-script-000001",
            "Fallback",
            Some("Workspace/Fallback.lua"),
            "source_fallback",
            "saved",
        );
        fallback["raw_size_bytes"] = json!(8);
        fallback["payload_size_bytes"] = json!(encoded.len() + 1);
        write_manifest(
            &manifest_path,
            vec![fallback],
            json!({
                "discovered": 1,
                "saved": 1,
                "failed": 0,
                "raw_bytecode": 0,
                "source_fallback": 1,
                "extraction_failure": 0
            }),
        );

        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                true,
                "lua",
                Some(&manifest_path),
            ),
            1
        );
        let parsed: Value = serde_json::from_slice(
            &std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["status"], "failed");
        assert!(
            parsed["generated_source_paths"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(!out.join("Workspace/Fallback.lua").exists());
        assert_eq!(parsed["diagnostics"][0]["code"], "processing_failed");
        assert!(
            parsed["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("payload_size_bytes mismatch")
        );

        let mut fallback = entry(
            "volt-script-000001",
            "Fallback",
            Some("Workspace/Fallback.lua"),
            "source_fallback",
            "saved",
        );
        fallback["raw_size_bytes"] = json!(9);
        fallback["payload_size_bytes"] = json!(encoded.len());
        write_manifest(
            &manifest_path,
            vec![fallback],
            json!({
                "discovered": 1,
                "saved": 1,
                "failed": 0,
                "raw_bytecode": 0,
                "source_fallback": 1,
                "extraction_failure": 0
            }),
        );
        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                true,
                "lua",
                Some(&manifest_path),
            ),
            1
        );
        let parsed: Value = serde_json::from_slice(
            &std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap(),
        )
        .unwrap();
        assert!(
            parsed["generated_source_paths"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(!out.join("Workspace/Fallback.lua").exists());
        assert!(
            parsed["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("raw_size_bytes mismatch")
        );
    }

    #[test]
    fn manifest_consistency_issues_return_failure_but_preserve_partial_artifacts() {
        let temp = TestDir::new("manifest-consistency-exit-status");
        let src = temp.0.join("dump");
        let out = temp.0.join("out");
        std::fs::create_dir_all(src.join("Workspace")).unwrap();
        let bytecode = include_bytes!("../tests/fixtures/upvalue_analysis_nested_g0.luaubc");
        let encoded = BASE64_STANDARD.encode(bytecode);
        std::fs::write(src.join("Workspace/Raw.lua"), &encoded).unwrap();
        let mut raw_entry = entry(
            "volt-script-000001",
            "Raw",
            Some("Workspace/Raw.lua"),
            "raw_bytecode",
            "saved",
        );
        raw_entry["raw_size_bytes"] = json!(bytecode.len());
        raw_entry["payload_size_bytes"] = json!(encoded.len());
        let manifest_path = src.join(".volt-export-manifest.json");
        write_manifest(
            &manifest_path,
            vec![raw_entry],
            json!({
                "discovered": 1,
                "saved": 1,
                "failed": 0,
                "raw_bytecode": 1,
                "source_fallback": 0,
                "extraction_failure": 0
            }),
        );
        let mut exporter_manifest: Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        exporter_manifest["status"] = json!("partial");
        exporter_manifest["counts"]["saved"] = json!(2);
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&exporter_manifest).unwrap(),
        )
        .unwrap();

        assert_eq!(
            run(
                &src,
                &out,
                1,
                0,
                false,
                DecompileOptions::default(),
                true,
                "lua",
                Some(&manifest_path),
            ),
            1
        );
        assert!(out.join("Workspace/Raw.lua").is_file());
        let parsed: Value = serde_json::from_slice(
            &std::fs::read(out.join(".tovek-analysis/manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed["status"], "partial");
        assert_eq!(parsed["analyzed_scripts"], 1);
        assert_eq!(parsed["failed_scripts"], 0);
        assert_eq!(parsed["manifest_issue_count"], 2);
        assert_eq!(
            parsed["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|diagnostic| diagnostic["status"] == "manifest_issue")
                .count(),
            2
        );
    }

    #[test]
    fn classify_failure_preserves_per_function_structuring_evidence() {
        let failure = luau_lifter::DecompileFailure {
            message: "control-flow structuring failed: residual goto/label would be invalid Luau"
                .to_string(),
            diagnostics: vec![DecompileDiagnostic {
                stage: "final_invariant".to_string(),
                code: "residual_control_flow".to_string(),
                function: "p3".to_string(),
                message: "source-like proof rejected: shared tail".to_string(),
            }],
        };
        let rendered = failure.to_string();
        let (code, evidence) = classify_failure(&rendered);
        assert_eq!(code, "residual_control_flow");
        let evidence = evidence.expect("typed diagnostics should survive legacy rendering");
        assert_eq!(evidence[0].function, "p3");
        assert_eq!(evidence[0].code, "residual_control_flow");
    }
}
