//! Shared decompile core used by both `decompile-folder` (`batch.rs`) and
//! `validate-folder` (`validate.rs`).
//!
//! This is the single byte-for-byte decode/decompile/write path: the bash
//! pipeline `grep -v '^--' | tr -d ' \t\r\n' | base64 -d` followed by an
//! in-process decompile that appends one trailing `\n` (to match the legacy
//! single-file `println!` and the `decompile_folder.sh` baseline tree).
//!
//! Both drivers MUST go through here so they never drift apart.

use base64::prelude::*;
use luau_lifter::DecompileOptions;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static ATOMIC_WRITE_TEMP_ID: AtomicU64 = AtomicU64::new(0);
const MAX_EXPORT_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXPORT_MANIFEST_SCRIPTS: usize = 100_000;

/// One unit of work, fully resolved up front so the parallel closure never
/// touches `strip_prefix`/path math (and never allocates the output path twice).
pub(crate) struct Work {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Trusted output root used to keep every manifest-backed publication
    /// handle-relative and contained even if a parent path is attacked. Export
    /// manifest work always stores the canonical root.
    pub output_root: PathBuf,
    /// Path relative to SRC, forward-slashed, *including* the `.lua` extension —
    /// passed verbatim as `--script-name` to match the bash baseline.
    pub rel: String,
    /// Path relative to OUT, using the selected output extension.
    pub source_rel: String,
    pub kind: WorkKind,
    pub volt_export: Option<VoltExportMetadata>,
}

struct ManifestWorkCandidate {
    work: Work,
    canonical_output_key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkKind {
    RawBytecode,
    SourceFallback,
}

pub(crate) enum Outcome {
    Ok,
    /// Input had no base64 payload (the "Failed to get bytecode" files).
    Skipped,
    Fail(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct GeneratedSourceRecord {
    pub path: String,
    pub byte_len: u64,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub(crate) struct AnalysisUnavailable {
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AnalysisManifestEntry {
    pub export_id: String,
    pub script_path: String,
    pub dump_path: String,
    pub source_path: String,
    pub sidecar_path: String,
    pub sidecar_sha256: String,
    pub extraction_kind: &'static str,
    pub bytecode_sha256: String,
    pub source_sha256: String,
    pub bytecode_artifact_id: String,
    pub analysis_id: String,
    pub status: luau_lifter::upvalue_analysis::AnalysisStatus,
    pub function_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volt_export: Option<VoltExportMetadata>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct VoltPathSegment {
    pub name: String,
    pub class_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct VoltExportMetadata {
    pub export_id: String,
    pub ordinal_one_based: usize,
    pub name: String,
    pub class_name: String,
    pub full_name: Option<String>,
    pub path_segments: Vec<VoltPathSegment>,
    pub dump_path: Option<String>,
    pub dump_relative_path: Option<String>,
    pub legacy_candidate_dump_relative_path: Option<String>,
    pub path_collision_resolved: bool,
    pub non_ascii_path_uniquified: bool,
    pub extraction_kind: String,
    pub is_bytecode: bool,
    pub bytecode_analysis_eligible: bool,
    pub payload_encoding: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_size_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_size_bytes: Option<usize>,
    #[serde(default)]
    pub diagnostics: Vec<serde_json::Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExportManifestDiagnostic {
    pub export_id: Option<String>,
    pub script_path: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ExportManifestInventory {
    pub total_scripts: usize,
    pub failed_scripts: usize,
    pub manifest_issue_count: usize,
    pub manifest_sha256: Option<String>,
    pub manifest_schema: Option<String>,
    pub manifest_schema_version: Option<u32>,
    pub manifest_status: Option<String>,
    pub declared_counts: Option<VoltExportCounts>,
    pub diagnostics: Vec<ExportManifestDiagnostic>,
}

/// Discover every `*.lua` file under SRC (recursive, sorted), resolve the SRC/OUT
/// roots, and build the fully-resolved [`Work`] list. Returns a process exit code
/// (`Err`) on a fatal path error, mirroring the original `batch::run` behavior.
pub(crate) fn build_work(src: &Path, out: &Path) -> Result<(PathBuf, PathBuf, Vec<Work>), i32> {
    build_work_with_extension(src, out, "luau")
}

pub(crate) fn build_work_with_extension(
    src: &Path,
    out: &Path,
    output_extension: &str,
) -> Result<(PathBuf, PathBuf, Vec<Work>), i32> {
    // canonicalize SRC (must exist) so the walk + strip_prefix share one verbatim
    // form; OUT may not exist yet, so use `absolute` (no existence requirement,
    // and keeps long paths Windows-safe). This asymmetry is load-bearing — do not
    // "tidy" it to canonicalize both.
    let src_root = match std::fs::canonicalize(src) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot access SRC {}: {e}", src.display());
            return Err(2);
        }
    };
    let out_root = match std::path::absolute(out) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: invalid OUT {}: {e}", out.display());
            return Err(2);
        }
    };

    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(&src_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lua")))
        .collect();
    files.sort();

    let work: Vec<Work> = files
        .iter()
        .map(|input| {
            let rel_path = input.strip_prefix(&src_root).unwrap_or(input);
            let source_rel_path = rel_path.with_extension(output_extension);
            let output = out_root.join(&source_rel_path);
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            let source_rel = source_rel_path.to_string_lossy().replace('\\', "/");
            Work {
                input: input.clone(),
                output,
                output_root: out_root.clone(),
                rel,
                source_rel,
                kind: WorkKind::RawBytecode,
                volt_export: None,
            }
        })
        .collect();

    Ok((src_root, out_root, work))
}

#[derive(Deserialize)]
struct VoltExportManifest {
    schema: String,
    schema_version: u32,
    status: String,
    payload_encoding: String,
    counts: VoltExportCounts,
    #[serde(deserialize_with = "deserialize_bounded_manifest_scripts")]
    scripts: Vec<serde_json::Value>,
}

fn deserialize_bounded_manifest_scripts<'de, D>(
    deserializer: D,
) -> Result<Vec<serde_json::Value>, D::Error>
where
    D: Deserializer<'de>,
{
    struct BoundedScriptsVisitor;

    impl<'de> serde::de::Visitor<'de> for BoundedScriptsVisitor {
        type Value = Vec<serde_json::Value>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "at most {MAX_EXPORT_MANIFEST_SCRIPTS} exporter script entries"
            )
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|size| size > MAX_EXPORT_MANIFEST_SCRIPTS)
            {
                return Err(serde::de::Error::custom(format!(
                    "export manifest contains more than {MAX_EXPORT_MANIFEST_SCRIPTS} scripts"
                )));
            }
            let mut scripts = Vec::with_capacity(
                sequence
                    .size_hint()
                    .unwrap_or(0)
                    .min(MAX_EXPORT_MANIFEST_SCRIPTS),
            );
            while let Some(script) = sequence.next_element()? {
                if scripts.len() == MAX_EXPORT_MANIFEST_SCRIPTS {
                    return Err(serde::de::Error::custom(format!(
                        "export manifest contains more than {MAX_EXPORT_MANIFEST_SCRIPTS} scripts"
                    )));
                }
                scripts.push(script);
            }
            Ok(scripts)
        }
    }

    deserializer.deserialize_seq(BoundedScriptsVisitor)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct VoltExportCounts {
    pub discovered: usize,
    pub saved: usize,
    pub failed: usize,
    pub raw_bytecode: usize,
    pub source_fallback: usize,
    pub extraction_failure: usize,
}

pub(crate) fn build_work_from_export_manifest(
    src: &Path,
    out: &Path,
    output_extension: &str,
    manifest_path: &Path,
) -> Result<(PathBuf, PathBuf, Vec<Work>, ExportManifestInventory), i32> {
    let src_root = match std::fs::canonicalize(src) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: cannot access SRC {}: {error}", src.display());
            return Err(2);
        }
    };
    if let Err(error) = std::fs::create_dir_all(out) {
        eprintln!("error: cannot create OUT {}: {error}", out.display());
        return Err(2);
    }
    let out_root = match std::fs::canonicalize(out) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: invalid OUT {}: {error}", out.display());
            return Err(2);
        }
    };
    let bytes = match read_bounded_export_manifest(manifest_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!(
                "error: read export manifest {}: {error}",
                manifest_path.display()
            );
            return Err(2);
        }
    };
    let manifest: VoltExportManifest = match serde_json::from_slice(&bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!(
                "error: parse export manifest {}: {error}",
                manifest_path.display()
            );
            return Err(2);
        }
    };

    if manifest.schema != "volt-decompile-export-manifest" {
        eprintln!(
            "error: unsupported export manifest schema: {}",
            manifest.schema
        );
        return Err(2);
    }
    if manifest.schema_version != 1 {
        eprintln!(
            "error: unsupported export manifest schema version: {}",
            manifest.schema_version
        );
        return Err(2);
    }
    if manifest.payload_encoding != "base64" {
        eprintln!(
            "error: unsupported export manifest payload_encoding: {} (expected base64)",
            manifest.payload_encoding
        );
        return Err(2);
    }
    if !matches!(manifest.status.as_str(), "complete" | "partial" | "failed") {
        eprintln!(
            "error: unsupported export manifest status: {}",
            manifest.status
        );
        return Err(2);
    }
    if [
        manifest.counts.discovered,
        manifest.counts.saved,
        manifest.counts.failed,
        manifest.counts.raw_bytecode,
        manifest.counts.source_fallback,
        manifest.counts.extraction_failure,
    ]
    .into_iter()
    .any(|count| count > MAX_EXPORT_MANIFEST_SCRIPTS)
    {
        eprintln!(
            "error: export manifest declared count exceeds limit of {MAX_EXPORT_MANIFEST_SCRIPTS}"
        );
        return Err(2);
    }

    let mut inventory = ExportManifestInventory {
        total_scripts: manifest.scripts.len(),
        manifest_sha256: Some(sha256_hex(&bytes)),
        manifest_schema: Some(manifest.schema.clone()),
        manifest_schema_version: Some(manifest.schema_version),
        manifest_status: Some(manifest.status.clone()),
        declared_counts: Some(manifest.counts.clone()),
        ..ExportManifestInventory::default()
    };
    validate_declared_counts(&manifest, &mut inventory);

    // Duplicate IDs are a property of the raw exporter inventory, not only of
    // entries that survive schema/status/path validation. Reject every member
    // of a duplicate set so a failed or malformed entry cannot leave another
    // entry with an ambiguously queryable export_id.
    let mut raw_export_id_counts: HashMap<String, usize> = HashMap::new();
    for raw_script in &manifest.scripts {
        if let Some(export_id) = raw_script
            .get("export_id")
            .and_then(serde_json::Value::as_str)
        {
            *raw_export_id_counts
                .entry(export_id.to_string())
                .or_default() += 1;
        }
    }

    let mut candidates = Vec::new();
    for (index, raw_script) in manifest.scripts.into_iter().enumerate() {
        let fallback_id = raw_script
            .get("export_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("manifest-entry-{:06}", index + 1));
        if raw_export_id_counts.get(&fallback_id).copied().unwrap_or(0) > 1 {
            let diagnostic_path = raw_script
                .get("dump_relative_path")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    raw_script
                        .get("full_name")
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or(&fallback_id)
                .to_string();
            inventory.failed_scripts += 1;
            inventory.diagnostics.push(ExportManifestDiagnostic {
                export_id: Some(fallback_id.clone()),
                script_path: diagnostic_path,
                code: "duplicate_export_id",
                message: format!("Duplicate Volt export_id {fallback_id:?}"),
            });
            continue;
        }
        let script: VoltExportMetadata = match serde_json::from_value(raw_script) {
            Ok(script) => script,
            Err(error) => {
                inventory.failed_scripts += 1;
                inventory.diagnostics.push(ExportManifestDiagnostic {
                    export_id: Some(fallback_id.clone()),
                    script_path: fallback_id,
                    code: "malformed_export_entry",
                    message: error.to_string(),
                });
                continue;
            }
        };
        let diagnostic_path = script.dump_relative_path.clone().unwrap_or_else(|| {
            script
                .full_name
                .clone()
                .unwrap_or_else(|| script.export_id.clone())
        });
        if !matches!(script.status.as_str(), "saved" | "failed") {
            reject_script(
                &mut inventory,
                &script,
                "invalid_export_entry_status",
                format!("Unsupported Volt export entry status {:?}", script.status),
            );
            continue;
        }
        if script.status == "failed" {
            inventory.failed_scripts += 1;
            inventory.diagnostics.push(ExportManifestDiagnostic {
                export_id: Some(script.export_id.clone()),
                script_path: diagnostic_path,
                code: "exporter_entry_not_saved",
                message: format!(
                    "Volt exporter reported status {:?} with extraction_kind {:?}",
                    script.status, script.extraction_kind
                ),
            });
            continue;
        }
        if script.payload_encoding != "base64" {
            reject_script(
                &mut inventory,
                &script,
                "unsupported_entry_payload_encoding",
                format!(
                    "Entry payload_encoding {:?} does not match the supported manifest encoding base64",
                    script.payload_encoding
                ),
            );
            continue;
        }
        let kind = match script.extraction_kind.as_str() {
            "raw_bytecode" if script.bytecode_analysis_eligible && script.is_bytecode => {
                WorkKind::RawBytecode
            }
            "source_fallback" if !script.bytecode_analysis_eligible && !script.is_bytecode => {
                WorkKind::SourceFallback
            }
            _ => {
                reject_script(
                    &mut inventory,
                    &script,
                    "inconsistent_extraction_metadata",
                    format!(
                        "Saved entry has inconsistent extraction_kind/is_bytecode/bytecode_analysis_eligible metadata ({:?}, {}, {})",
                        script.extraction_kind,
                        script.is_bytecode,
                        script.bytecode_analysis_eligible
                    ),
                );
                continue;
            }
        };
        if script.raw_size_bytes.is_none() || script.payload_size_bytes.is_none() {
            reject_script(
                &mut inventory,
                &script,
                "missing_export_size_metadata",
                "Saved export entry must declare raw_size_bytes and payload_size_bytes".to_string(),
            );
            continue;
        }
        let Some(relative) = script.dump_relative_path.clone() else {
            reject_script(
                &mut inventory,
                &script,
                "missing_dump_relative_path",
                "Saved export entry has no dump_relative_path".to_string(),
            );
            continue;
        };
        let relative_path = Path::new(&relative);
        if !is_safe_relative_path(relative_path) {
            reject_script(
                &mut inventory,
                &script,
                "unsafe_dump_relative_path",
                format!(
                    "dump_relative_path is absolute, empty, contains non-normal components, or uses colon/Windows alternate-stream syntax: {relative:?}"
                ),
            );
            continue;
        }
        let unresolved_input = src_root.join(relative_path);
        let input = match std::fs::canonicalize(&unresolved_input) {
            Ok(input) if input.starts_with(&src_root) && input.is_file() => input,
            Ok(input) => {
                reject_script(
                    &mut inventory,
                    &script,
                    "dump_path_escaped_source_root",
                    format!(
                        "Canonical dump path {} is not a file contained by source root {}",
                        input.display(),
                        src_root.display()
                    ),
                );
                continue;
            }
            Err(error) => {
                reject_script(
                    &mut inventory,
                    &script,
                    "missing_dump_file",
                    format!(
                        "Cannot resolve dump file {}: {error}",
                        unresolved_input.display()
                    ),
                );
                continue;
            }
        };
        let source_rel_path = relative_path.with_extension(output_extension);
        let output = out_root.join(&source_rel_path);
        if let Err(message) = validate_existing_output_chain(&out_root, &output) {
            reject_script(&mut inventory, &script, "unsafe_output_parent", message);
            continue;
        }
        match std::fs::symlink_metadata(&output) {
            Ok(_) => {
                reject_script(
                    &mut inventory,
                    &script,
                    "preexisting_output_destination",
                    format!(
                        "Refusing to replace pre-existing export destination {}. Volt must remove only a previously owned, unchanged source before invoking Tovek",
                        output.display()
                    ),
                );
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                reject_script(
                    &mut inventory,
                    &script,
                    "unsafe_output_parent",
                    format!(
                        "Cannot inspect output destination {}: {error}",
                        output.display()
                    ),
                );
                continue;
            }
        }
        let canonical_output_key = match canonical_output_identity_key(&out_root, &output) {
            Ok(key) => key,
            Err(message) => {
                reject_script(&mut inventory, &script, "unsafe_output_parent", message);
                continue;
            }
        };
        candidates.push(ManifestWorkCandidate {
            work: Work {
                input,
                output,
                output_root: out_root.clone(),
                rel: relative.replace('\\', "/"),
                source_rel: source_rel_path.to_string_lossy().replace('\\', "/"),
                kind,
                volt_export: Some(script),
            },
            canonical_output_key,
        });
    }

    let mut output_paths: HashMap<String, usize> = HashMap::new();
    let mut canonical_output_paths: HashMap<String, usize> = HashMap::new();
    for candidate in &candidates {
        *output_paths
            .entry(candidate.work.source_rel.to_lowercase())
            .or_default() += 1;
        *canonical_output_paths
            .entry(candidate.canonical_output_key.clone())
            .or_default() += 1;
    }
    let mut work = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let metadata = candidate
            .work
            .volt_export
            .as_ref()
            .expect("manifest work has metadata");
        let colliding_output = output_paths[&candidate.work.source_rel.to_lowercase()] > 1;
        if colliding_output {
            reject_script(
                &mut inventory,
                metadata,
                "case_colliding_output_path",
                format!(
                    "Output path {:?} collides case-insensitively",
                    candidate.work.source_rel
                ),
            );
        } else if canonical_output_paths[&candidate.canonical_output_key] > 1 {
            reject_script(
                &mut inventory,
                metadata,
                "canonical_output_path_alias",
                format!(
                    "Output path {:?} resolves to the same destination as another export entry",
                    candidate.work.source_rel
                ),
            );
        } else {
            work.push(candidate.work);
        }
    }
    work.sort_by(|left, right| left.rel.cmp(&right.rel));
    inventory.diagnostics.sort_by(|left, right| {
        left.script_path
            .cmp(&right.script_path)
            .then_with(|| left.code.cmp(right.code))
            .then_with(|| left.export_id.cmp(&right.export_id))
    });
    Ok((src_root, out_root, work, inventory))
}

fn read_bounded_export_manifest(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let declared_len = file.metadata()?.len();
    if declared_len > MAX_EXPORT_MANIFEST_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "export manifest is {declared_len} bytes; limit is {MAX_EXPORT_MANIFEST_BYTES} bytes"
            ),
        ));
    }
    let capacity = usize::try_from(declared_len).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity);
    std::io::Read::by_ref(&mut file)
        .take(MAX_EXPORT_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_EXPORT_MANIFEST_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "export manifest grew beyond the {MAX_EXPORT_MANIFEST_BYTES}-byte limit while reading"
            ),
        ));
    }
    Ok(bytes)
}

fn reject_script(
    inventory: &mut ExportManifestInventory,
    script: &VoltExportMetadata,
    code: &'static str,
    message: String,
) {
    inventory.failed_scripts += 1;
    inventory.diagnostics.push(ExportManifestDiagnostic {
        export_id: Some(script.export_id.clone()),
        script_path: script.dump_relative_path.clone().unwrap_or_else(|| {
            script
                .full_name
                .clone()
                .unwrap_or_else(|| script.export_id.clone())
        }),
        code,
        message,
    });
}

fn validate_declared_counts(
    manifest: &VoltExportManifest,
    inventory: &mut ExportManifestInventory,
) {
    let actual_saved = manifest
        .scripts
        .iter()
        .filter(|entry| entry.get("status").and_then(serde_json::Value::as_str) == Some("saved"))
        .count();
    let actual_failed = manifest.scripts.len().saturating_sub(actual_saved);
    let actual_raw = manifest
        .scripts
        .iter()
        .filter(|entry| {
            entry
                .get("extraction_kind")
                .and_then(serde_json::Value::as_str)
                == Some("raw_bytecode")
        })
        .count();
    let actual_fallback = manifest
        .scripts
        .iter()
        .filter(|entry| {
            entry
                .get("extraction_kind")
                .and_then(serde_json::Value::as_str)
                == Some("source_fallback")
        })
        .count();
    let actual_extraction_failure = manifest.scripts.len() - actual_raw - actual_fallback;
    let actual = VoltExportCounts {
        discovered: manifest.scripts.len(),
        saved: actual_saved,
        failed: actual_failed,
        raw_bytecode: actual_raw,
        source_fallback: actual_fallback,
        extraction_failure: actual_extraction_failure,
    };
    if manifest.counts.discovered != actual.discovered
        || manifest.counts.saved != actual.saved
        || manifest.counts.failed != actual.failed
        || manifest.counts.raw_bytecode != actual.raw_bytecode
        || manifest.counts.source_fallback != actual.source_fallback
        || manifest.counts.extraction_failure != actual.extraction_failure
    {
        inventory.manifest_issue_count += 1;
        inventory.diagnostics.push(ExportManifestDiagnostic {
            export_id: None,
            script_path: "<export-manifest>".to_string(),
            code: "manifest_count_mismatch",
            message: format!(
                "Declared counts {:?} do not match actual counts {:?}",
                manifest.counts, actual
            ),
        });
    }
    let expected_status = if actual.failed == 0 {
        "complete"
    } else if actual.saved > 0 {
        "partial"
    } else {
        "failed"
    };
    if manifest.status != expected_status {
        inventory.manifest_issue_count += 1;
        inventory.diagnostics.push(ExportManifestDiagnostic {
            export_id: None,
            script_path: "<export-manifest>".to_string(),
            code: "manifest_status_mismatch",
            message: format!(
                "Declared status {:?} does not match entry status counts; expected {expected_status:?}",
                manifest.status
            ),
        });
    }
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| match component {
            std::path::Component::Normal(part) => !part.as_encoded_bytes().contains(&b':'),
            _ => false,
        })
}

fn validate_existing_output_chain(out_root: &Path, output: &Path) -> Result<(), String> {
    let relative = output.strip_prefix(out_root).map_err(|_| {
        format!(
            "Output {} is outside {}",
            output.display(),
            out_root.display()
        )
    })?;
    let mut current = out_root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(_) => {
                let canonical = std::fs::canonicalize(&current).map_err(|error| {
                    format!(
                        "Cannot resolve existing output path {}: {error}",
                        current.display()
                    )
                })?;
                if !canonical.starts_with(out_root) {
                    return Err(format!(
                        "Existing output path {} resolves outside output root {}",
                        current.display(),
                        out_root.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "Cannot inspect output path {}: {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}

fn canonical_output_identity_key(out_root: &Path, output: &Path) -> Result<String, String> {
    let relative = output.strip_prefix(out_root).map_err(|_| {
        format!(
            "Output {} is outside {}",
            output.display(),
            out_root.display()
        )
    })?;
    let mut resolved = out_root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let candidate = resolved.join(component.as_os_str());
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {
                resolved = std::fs::canonicalize(&candidate).map_err(|error| {
                    format!(
                        "Cannot resolve existing output path {}: {error}",
                        candidate.display()
                    )
                })?;
                if !resolved.starts_with(out_root) {
                    return Err(format!(
                        "Existing output path {} resolves outside output root {}",
                        candidate.display(),
                        out_root.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                for unresolved in &components[index..] {
                    resolved.push(unresolved.as_os_str());
                }
                break;
            }
            Err(error) => {
                return Err(format!(
                    "Cannot inspect output path {}: {error}",
                    candidate.display()
                ));
            }
        }
    }
    let key = resolved.to_string_lossy().into_owned();
    #[cfg(windows)]
    let key = key.to_lowercase();
    Ok(key)
}

pub(crate) fn prepare_analysis_root(out_root: &Path) -> Result<PathBuf, String> {
    let requested_root = out_root.join(".tovek-analysis");
    validate_existing_output_chain(out_root, &requested_root)?;
    #[cfg(windows)]
    windows_contained_fs::ensure_directory(out_root, &requested_root.join("scripts"))
        .map_err(|error| format!("create analysis directories: {error}"))?;
    #[cfg(not(windows))]
    std::fs::create_dir_all(requested_root.join("scripts"))
        .map_err(|error| format!("create analysis directories: {error}"))?;
    let analysis_root = std::fs::canonicalize(&requested_root)
        .map_err(|error| format!("resolve analysis directory: {error}"))?;
    if !analysis_root.starts_with(out_root) {
        return Err(format!(
            "Analysis directory {} resolves outside output root {}",
            requested_root.display(),
            out_root.display()
        ));
    }
    validate_analysis_scripts_root(&analysis_root)?;
    Ok(analysis_root)
}

pub(crate) struct GenerationLock {
    _file: File,
    // On Windows this handle denies delete-sharing for the output root, so the
    // path cannot be renamed and recreated around the held lock file.
    _root_directory: Option<File>,
    output_root: PathBuf,
}

impl GenerationLock {
    pub(crate) fn output_root(&self) -> &Path {
        &self.output_root
    }
}

pub(crate) fn acquire_output_generation_lock(out_root: &Path) -> Result<GenerationLock, String> {
    std::fs::create_dir_all(out_root)
        .map_err(|error| format!("create output root before locking: {error}"))?;
    let canonical_root = std::fs::canonicalize(out_root)
        .map_err(|error| format!("resolve output root before locking: {error}"))?;
    #[cfg(not(windows))]
    let lock_path = canonical_root.join(".tovek-output.lock");
    #[cfg(windows)]
    let (file, root_directory) = windows_contained_fs::open_generation_lock(&canonical_root)
        .map_err(|error| format!("open output generation lock: {error}"))?;
    #[cfg(not(windows))]
    let (file, root_directory) = {
        validate_existing_output_chain(&canonical_root, &lock_path)?;
        if let Ok(metadata) = std::fs::symlink_metadata(&lock_path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "Output generation lock {} is not a regular file",
                    lock_path.display()
                ));
            }
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&lock_path)
            .map_err(|error| format!("open output generation lock: {error}"))?;
        let canonical_lock = std::fs::canonicalize(&lock_path)
            .map_err(|error| format!("resolve output generation lock: {error}"))?;
        if !canonical_lock.starts_with(&canonical_root) {
            return Err(format!(
                "Output generation lock {} resolves outside output root {}",
                lock_path.display(),
                canonical_root.display()
            ));
        }
        (file, None)
    };
    fs2::FileExt::lock_exclusive(&file)
        .map_err(|error| format!("lock output generation: {error}"))?;
    #[cfg(not(windows))]
    validate_existing_output_chain(&canonical_root, &lock_path)?;
    Ok(GenerationLock {
        _file: file,
        _root_directory: root_directory,
        output_root: canonical_root,
    })
}

pub(crate) fn invalidate_analysis_manifest(out_root: &Path) -> Result<(), String> {
    // Remove provenance first: if invalidation is interrupted, consumers may
    // still use the old manifest, but can never mistake old recovery data for
    // the generation that is about to start writing sources.
    for (name, description) in [
        ("source-write-provenance.json", "source-write provenance"),
        ("manifest.json", "analysis manifest"),
    ] {
        let path = out_root.join(".tovek-analysis").join(name);
        #[cfg(windows)]
        {
            match windows_contained_fs::remove_regular_file(out_root, &path) {
                Ok(()) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "invalidate previous {description} {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        #[cfg(not(windows))]
        validate_existing_output_chain(out_root, &path)?;
        #[cfg(not(windows))]
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(format!(
                    "Existing {description} {} is not a regular file",
                    path.display()
                ));
            }
            Ok(_) => std::fs::remove_file(&path)
                .map_err(|error| format!("invalidate previous {description}: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect previous {description} {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn validate_analysis_scripts_root(analysis_root: &Path) -> Result<PathBuf, String> {
    validate_analysis_root_for_write(analysis_root)?;
    let requested_scripts = analysis_root.join("scripts");
    let scripts_root = std::fs::canonicalize(&requested_scripts)
        .map_err(|error| format!("resolve analysis scripts directory: {error}"))?;
    if !scripts_root.starts_with(analysis_root) {
        return Err(format!(
            "Analysis scripts directory {} resolves outside analysis root {}",
            requested_scripts.display(),
            analysis_root.display()
        ));
    }
    Ok(scripts_root)
}

pub(crate) fn validate_analysis_root_for_write(analysis_root: &Path) -> Result<(), String> {
    let canonical = std::fs::canonicalize(analysis_root)
        .map_err(|error| format!("resolve analysis root before write: {error}"))?;
    if canonical != analysis_root {
        return Err(format!(
            "Analysis root {} changed or now resolves to {}",
            analysis_root.display(),
            canonical.display()
        ));
    }
    Ok(())
}

/// Pre-create every unique parent directory single-threaded, so the parallel
/// phase only ever writes files (no concurrent `create_dir_all` race).
pub(crate) fn precreate_dirs(work: &[Work]) -> Result<(), i32> {
    let mut dirs: HashSet<(&Path, &Path)> = HashSet::new();
    for w in work {
        if let Some(parent) = w.output.parent() {
            dirs.insert((&w.output_root, parent));
        }
    }
    for (root, dir) in dirs {
        #[cfg(windows)]
        let result = windows_contained_fs::ensure_directory(root, dir);
        #[cfg(not(windows))]
        let result = std::fs::create_dir_all(dir);
        if let Err(e) = result {
            eprintln!("error: create dir {}: {e}", dir.display());
            return Err(2);
        }
    }
    Ok(())
}

/// Size the global rayon pool once, before the first parallel call. `threads == 0`
/// leaves rayon's default (= logical CPU count).
pub(crate) fn size_pool(threads: usize) {
    if threads != 0 {
        if let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
        {
            eprintln!("warning: could not set thread count ({e}); using default pool");
        }
    }
}

/// Decode + decompile + write one file. Never panics out: a panic anywhere in
/// the decompile pipeline is caught and reported as a `Fail`. Thin wrapper used
/// by `decompile-folder`, which discards the source string.
pub(crate) fn process_one(
    w: &Work,
    key: u8,
    b64: &mut Vec<u8>,
    verbose: bool,
    options: DecompileOptions,
) -> Outcome {
    decode_and_decompile(w, key, b64, true, false, verbose, options, None).0
}

pub(crate) fn process_one_with_analysis(
    w: &Work,
    key: u8,
    b64: &mut Vec<u8>,
    verbose: bool,
    options: DecompileOptions,
    analysis_root: &Path,
) -> (
    Outcome,
    Option<AnalysisManifestEntry>,
    Option<AnalysisUnavailable>,
    Option<GeneratedSourceRecord>,
) {
    let (outcome, _, entry, source_record) = decode_and_decompile(
        w,
        key,
        b64,
        true,
        false,
        verbose,
        options,
        Some(analysis_root),
    );
    let unavailable = if matches!(outcome, Outcome::Ok) && entry.is_none() {
        Some(match w.kind {
            WorkKind::SourceFallback => AnalysisUnavailable {
                code: "source_fallback",
                message:
                    "Source fallback was copied cleanly and was not interpreted as bytecode"
                        .to_string(),
            },
            WorkKind::RawBytecode => AnalysisUnavailable {
                code: "bytecode_error",
                message: "Decompiler produced source from a bytecode error payload, but no static upvalue analysis was available".to_string(),
            },
        })
    } else {
        None
    };
    (outcome, entry, unavailable, source_record)
}

/// Like [`process_one`] but returns the decompiled source on success so the
/// caller can compute line counts / `goto` flags without re-reading the file.
///
/// `write_skipped` controls whether an empty-payload input writes a zero-byte
/// output: `decompile-folder` passes `true` (mirror its baseline tree), while
/// `validate-folder` passes `false` (the bash validator `continue`s before any
/// write, so no file exists for skipped inputs).
pub(crate) fn process_one_capture(
    w: &Work,
    key: u8,
    b64: &mut Vec<u8>,
    write_skipped: bool,
    options: DecompileOptions,
) -> (Outcome, Option<String>) {
    let (outcome, source, _, _) =
        decode_and_decompile(w, key, b64, write_skipped, true, false, options, None);
    (outcome, source)
}

/// The actual decode/decompile/write logic shared by both entry points.
///
/// * `write_skipped` — write a zero-byte file for empty-payload inputs.
/// * `capture` — return `Some(source)` on success (otherwise `None`, and the
///   source is moved straight into the on-disk bytes with no clone).
/// * `verbose` — print `ok <rel>` to stderr on success (the `decompile-folder`
///   `-v` behavior; kept inside the closure so its timing/threading is
///   byte-for-byte unchanged).
fn decode_and_decompile(
    w: &Work,
    key: u8,
    b64: &mut Vec<u8>,
    write_skipped: bool,
    capture: bool,
    verbose: bool,
    options: DecompileOptions,
    analysis_root: Option<&Path>,
) -> (
    Outcome,
    Option<String>,
    Option<AnalysisManifestEntry>,
    Option<GeneratedSourceRecord>,
) {
    let text = match std::fs::read(&w.input) {
        Ok(t) => t,
        Err(e) => return (Outcome::Fail(format!("read: {e}")), None, None, None),
    };

    // Replicate `grep -v '^--' | tr -d ' \t\r\n'`: drop lines starting with
    // "--" (start-of-line anchor — no trim), keep all non-whitespace bytes.
    b64.clear();
    for line in text.split(|&b| b == b'\n') {
        if line.starts_with(b"--") {
            continue;
        }
        b64.extend(
            line.iter()
                .copied()
                .filter(|&b| b != b' ' && b != b'\t' && b != b'\r'),
        );
    }

    if let Some(metadata) = w.volt_export.as_ref() {
        let declared = metadata
            .payload_size_bytes
            .expect("saved manifest work has payload_size_bytes");
        if declared != b64.len() {
            return (
                Outcome::Fail(format!(
                    "export payload_size_bytes mismatch: declared {declared}, compact wrapper payload is {} bytes",
                    b64.len()
                )),
                None,
                None,
                None,
            );
        }
    }

    let bytecode = match BASE64_STANDARD.decode(b64.as_slice()) {
        Ok(b) => b,
        Err(e) => return (Outcome::Fail(format!("base64: {e}")), None, None, None),
    };
    if let Some(metadata) = w.volt_export.as_ref() {
        let declared = metadata
            .raw_size_bytes
            .expect("saved manifest work has raw_size_bytes");
        if declared != bytecode.len() {
            return (
                Outcome::Fail(format!(
                    "export raw_size_bytes mismatch: declared {declared}, decoded payload is {} bytes",
                    bytecode.len()
                )),
                None,
                None,
                None,
            );
        }
    }
    if bytecode.is_empty() && w.kind == WorkKind::RawBytecode {
        if write_skipped {
            let write_result = if analysis_root.is_some() || w.volt_export.is_some() {
                atomic_write_contained(&w.output_root, &w.output, b"", w.volt_export.is_none())
            } else {
                std::fs::write(&w.output, b"")
            };
            if let Err(e) = write_result {
                return (Outcome::Fail(format!("write: {e}")), None, None, None);
            }
        }
        return (
            Outcome::Skipped,
            None,
            None,
            write_skipped.then(|| generated_source_record(w, b"")),
        );
    }

    if w.kind == WorkKind::SourceFallback {
        let write_result = if analysis_root.is_some() || w.volt_export.is_some() {
            atomic_write_contained(
                &w.output_root,
                &w.output,
                &bytecode,
                w.volt_export.is_none(),
            )
        } else {
            std::fs::write(&w.output, &bytecode)
        };
        if let Err(error) = write_result {
            return (
                Outcome::Fail(format!("write source fallback: {error}")),
                None,
                None,
                None,
            );
        }
        let captured = capture.then(|| String::from_utf8_lossy(&bytecode).into_owned());
        if verbose {
            eprintln!("ok {} (source fallback)", w.rel);
        }
        return (
            Outcome::Ok,
            captured,
            None,
            Some(generated_source_record(w, &bytecode)),
        );
    }

    // catch_unwind is the backstop for deep panics in the lifter/ssa/restructure
    // passes. The common deserialize-failure path already comes back as Err.
    let (source, upvalue_analysis) = if analysis_root.is_some() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            luau_lifter::try_decompile_bytecode_artifact_with_options(
                &bytecode,
                key,
                Some(&w.rel),
                options,
            )
        }));
        let artifact = match result {
            Ok(Ok(artifact)) => artifact,
            Ok(Err(reason)) => return (Outcome::Fail(reason), None, None, None),
            Err(payload) => {
                return (Outcome::Fail(panic_message(payload)), None, None, None);
            }
        };
        (artifact.source, artifact.upvalue_analysis)
    } else {
        let result = catch_unwind(AssertUnwindSafe(|| {
            luau_lifter::try_decompile_bytecode_with_options(&bytecode, key, Some(&w.rel), options)
        }));
        let source = match result {
            Ok(Ok(source)) => source,
            Ok(Err(reason)) => return (Outcome::Fail(reason), None, None, None),
            Err(payload) => {
                return (Outcome::Fail(panic_message(payload)), None, None, None);
            }
        };
        (source, None)
    };

    // Append a trailing newline so output is byte-identical to the single-file
    // mode (which prints via `println!`). When `capture` is set we keep the
    // source string for the caller, so we must clone before consuming it.
    let (mut source_bytes, captured) = if capture {
        (source.as_bytes().to_vec(), Some(source))
    } else {
        (source.into_bytes(), None)
    };
    source_bytes.push(b'\n');
    let write_result = if analysis_root.is_some() || w.volt_export.is_some() {
        atomic_write_contained(
            &w.output_root,
            &w.output,
            &source_bytes,
            w.volt_export.is_none(),
        )
    } else {
        std::fs::write(&w.output, &source_bytes)
    };
    if let Err(e) = write_result {
        return (Outcome::Fail(format!("write: {e}")), None, None, None);
    }

    let analysis_entry = match (analysis_root, upvalue_analysis) {
        (Some(analysis_root), Some(analysis)) => {
            match write_analysis_sidecar(
                analysis_root,
                w,
                key,
                options,
                &bytecode,
                &source_bytes,
                &analysis,
            ) {
                Ok(entry) => Some(entry),
                Err(error) => {
                    return (
                        Outcome::Fail(format!("analysis sidecar: {error}")),
                        None,
                        None,
                        Some(generated_source_record(w, &source_bytes)),
                    );
                }
            }
        }
        _ => None,
    };
    if verbose {
        eprintln!("ok {}", w.rel);
    }
    (
        Outcome::Ok,
        captured,
        analysis_entry,
        Some(generated_source_record(w, &source_bytes)),
    )
}

fn generated_source_record(work: &Work, bytes: &[u8]) -> GeneratedSourceRecord {
    GeneratedSourceRecord {
        path: work.source_rel.clone(),
        byte_len: bytes.len() as u64,
        sha256: sha256_hex(bytes),
    }
}

#[derive(Serialize)]
struct ScriptSidecar<'a> {
    export_id: &'a str,
    bytecode_artifact_id: &'a str,
    analysis_id: &'a str,
    script_path: &'a str,
    dump_path: &'a str,
    source_path: &'a str,
    extraction_kind: &'static str,
    bytecode_sha256: &'a str,
    source_sha256: &'a str,
    tovek_version: &'static str,
    decompile_option_bits: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    volt_export: Option<&'a VoltExportMetadata>,
    #[serde(flatten)]
    analysis: &'a luau_lifter::upvalue_analysis::ScriptUpvalueAnalysis,
}

fn write_analysis_sidecar(
    analysis_root: &Path,
    work: &Work,
    key: u8,
    options: DecompileOptions,
    bytecode: &[u8],
    source_bytes: &[u8],
    analysis: &luau_lifter::upvalue_analysis::ScriptUpvalueAnalysis,
) -> Result<AnalysisManifestEntry, String> {
    let scripts_root = validate_analysis_scripts_root(analysis_root)?;
    let export_id = work
        .volt_export
        .as_ref()
        .map(|metadata| metadata.export_id.clone())
        .unwrap_or_else(|| sha256_hex(work.rel.as_bytes()));
    let bytecode_sha256 = sha256_hex(bytecode);
    let source_sha256 = sha256_hex(source_bytes);
    let mut artifact_hasher = Sha256::new();
    artifact_hasher.update(bytecode);
    artifact_hasher.update([key, analysis.bytecode_version]);
    let bytecode_artifact_id = format!("sha256:{}", hex_digest(artifact_hasher.finalize()));
    let mut analysis_hasher = Sha256::new();
    analysis_hasher.update(bytecode_artifact_id.as_bytes());
    analysis_hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
    analysis_hasher.update(analysis.schema_version.to_le_bytes());
    analysis_hasher.update(options.bits().to_le_bytes());
    let analysis_id = format!("sha256:{}", hex_digest(analysis_hasher.finalize()));
    let sidecar = ScriptSidecar {
        export_id: &export_id,
        bytecode_artifact_id: &bytecode_artifact_id,
        analysis_id: &analysis_id,
        script_path: work
            .volt_export
            .as_ref()
            .and_then(|metadata| metadata.full_name.as_deref())
            .unwrap_or(&work.rel),
        dump_path: &work.rel,
        source_path: &work.source_rel,
        extraction_kind: "raw_bytecode",
        bytecode_sha256: &bytecode_sha256,
        source_sha256: &source_sha256,
        tovek_version: env!("CARGO_PKG_VERSION"),
        decompile_option_bits: options.bits(),
        volt_export: work.volt_export.as_ref(),
        analysis,
    };
    let mut json = serde_json::to_vec_pretty(&sidecar).map_err(|error| error.to_string())?;
    json.push(b'\n');
    let sidecar_sha256 = sha256_hex(&json);
    let sidecar_rel = format!(".tovek-analysis/scripts/{sidecar_sha256}.json");
    let sidecar_path = scripts_root.join(format!("{sidecar_sha256}.json"));
    let output_root = analysis_root.parent().ok_or_else(|| {
        format!(
            "analysis root {} has no output-root parent",
            analysis_root.display()
        )
    })?;
    write_content_addressed_file_contained(output_root, &sidecar_path, &json, &sidecar_sha256)?;

    Ok(AnalysisManifestEntry {
        export_id,
        script_path: work
            .volt_export
            .as_ref()
            .and_then(|metadata| metadata.full_name.clone())
            .unwrap_or_else(|| work.rel.clone()),
        dump_path: work.rel.clone(),
        source_path: work.source_rel.clone(),
        sidecar_path: sidecar_rel,
        sidecar_sha256,
        extraction_kind: "raw_bytecode",
        bytecode_sha256,
        source_sha256,
        bytecode_artifact_id,
        analysis_id,
        status: analysis.status,
        function_count: analysis.functions.len(),
        volt_export: work.volt_export.clone(),
    })
}

#[cfg(test)]
fn write_content_addressed_file(
    path: &Path,
    bytes: &[u8],
    expected_sha256: &str,
) -> Result<(), String> {
    match std::fs::read(path) {
        Ok(existing) => {
            return verify_content_addressed_bytes(path, &existing, bytes, expected_sha256);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "read existing content-addressed artifact {}: {error}",
                path.display()
            ));
        }
    }

    static TEMP_ID: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| format!("content-addressed path {} has no parent", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    let temp_path = loop {
        let candidate = parent.join(format!(
            ".{file_name}.{}.{}.new",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                    drop(file);
                    let _ = std::fs::remove_file(&candidate);
                    return Err(format!(
                        "write content-addressed artifact temporary file {}: {error}",
                        candidate.display()
                    ));
                }
                break candidate;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "create content-addressed artifact temporary file {}: {error}",
                    candidate.display()
                ));
            }
        }
    };

    let publish_result = publish_new_file(&temp_path, path);
    let _ = std::fs::remove_file(&temp_path);
    match publish_result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(path).map_err(|read_error| {
                format!(
                    "read concurrently published content-addressed artifact {} after {error}: {read_error}",
                    path.display()
                )
            })?;
            verify_content_addressed_bytes(path, &existing, bytes, expected_sha256)
        }
        Err(error) => Err(format!(
            "publish content-addressed artifact {} without replacement: {error}",
            path.display()
        )),
    }
}

fn write_content_addressed_file_contained(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    expected_sha256: &str,
) -> Result<(), String> {
    match atomic_write_contained(root, path, bytes, false) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(path).map_err(|read_error| {
                format!(
                    "read existing content-addressed artifact {} after {error}: {read_error}",
                    path.display()
                )
            })?;
            verify_content_addressed_bytes(path, &existing, bytes, expected_sha256)
        }
        Err(error) => Err(format!(
            "publish contained content-addressed artifact {} without replacement: {error}",
            path.display()
        )),
    }
}

#[cfg(not(windows))]
fn publish_new_file(temp_path: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::hard_link(temp_path, destination)
}

#[cfg(all(windows, test))]
fn publish_new_file(temp_path: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    let existing = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            replacement.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn verify_content_addressed_bytes(
    path: &Path,
    existing: &[u8],
    expected: &[u8],
    expected_sha256: &str,
) -> Result<(), String> {
    let existing_sha256 = sha256_hex(existing);
    if existing_sha256 != expected_sha256 || existing != expected {
        Err(format!(
            "existing content-addressed artifact {} does not match its expected SHA-256 {expected_sha256} (actual {existing_sha256})",
            path.display()
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(any(test, not(windows)))]
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let canonical_parent = std::fs::canonicalize(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    let (temp_path, mut file) = create_atomic_temp_file(parent, file_name, &ATOMIC_WRITE_TEMP_ID)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    drop(file);

    let validation = (|| {
        let current_parent = std::fs::canonicalize(parent)?;
        let temp_metadata = std::fs::symlink_metadata(&temp_path)?;
        let canonical_temp = std::fs::canonicalize(&temp_path)?;
        if current_parent != canonical_parent
            || temp_metadata.file_type().is_symlink()
            || !temp_metadata.is_file()
            || canonical_temp.parent() != Some(canonical_parent.as_path())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "atomic write temporary path escaped or changed parent directory",
            ));
        }
        Ok(())
    })();
    if let Err(error) = validation {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }

    let result = replace_file(&temp_path, path);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result?;
    sync_parent_directory(&canonical_parent)
}

/// Atomically publishes a file below a trusted canonical root. Manifest-backed
/// source writes pass `replace_existing = false`: a leftover user or untracked
/// file is never silently replaced.
pub(crate) fn atomic_write_contained(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    replace_existing: bool,
) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        windows_contained_fs::atomic_write(root, path, bytes, replace_existing)
    }
    #[cfg(not(windows))]
    {
        let relative = path.strip_prefix(root).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "publication path {} is outside {}",
                    path.display(),
                    root.display()
                ),
            )
        })?;
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "publication path contains a non-normal component",
            ));
        }
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
        })?;
        std::fs::create_dir_all(parent)?;
        let canonical_root = std::fs::canonicalize(root)?;
        let canonical_parent = std::fs::canonicalize(parent)?;
        if !canonical_parent.starts_with(&canonical_root) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "publication parent escaped the output root",
            ));
        }
        if !replace_existing {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("artifact");
            let (temp_path, mut file) =
                create_atomic_temp_file(parent, file_name, &ATOMIC_WRITE_TEMP_ID)?;
            if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                drop(file);
                let _ = std::fs::remove_file(&temp_path);
                return Err(error);
            }
            drop(file);
            let result = publish_new_file(&temp_path, path);
            let _ = std::fs::remove_file(&temp_path);
            result?;
            return sync_parent_directory(&canonical_parent);
        }
        atomic_write(path, bytes)
    }
}

#[cfg(windows)]
mod windows_contained_fs {
    use super::*;
    #[cfg(test)]
    use std::cell::RefCell;
    use std::ffi::{OsStr, c_void};
    use std::io;
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
    use std::ptr::null_mut;

    type Handle = *mut c_void;
    type NtStatus = i32;

    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const DELETE: u32 = 0x0001_0000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_LIST_DIRECTORY: u32 = 0x0000_0001;
    const FILE_ADD_FILE: u32 = 0x0000_0002;
    const FILE_ADD_SUBDIRECTORY: u32 = 0x0000_0004;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const FILE_CREATE: u32 = 2;
    const FILE_OPEN_IF: u32 = 3;
    const FILE_DIRECTORY_FILE: u32 = 0x1;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    const FILE_ATTRIBUTE_TAG_INFO_CLASS: i32 = 9;
    const FILE_DISPOSITION_INFO_CLASS: i32 = 4;
    const ERROR_ALREADY_EXISTS: i32 = 183;
    const ERROR_FILE_EXISTS: i32 = 80;

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }

    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: Handle,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut c_void,
        security_quality_of_service: *mut c_void,
    }

    #[repr(C)]
    struct IoStatusBlock {
        status: isize,
        information: usize,
    }

    #[repr(C)]
    struct FileAttributeTagInfo {
        file_attributes: u32,
        reparse_tag: u32,
    }

    #[repr(C)]
    struct FileRenameInfoLayout {
        replace_if_exists: u8,
        root_directory: Handle,
        file_name_length: u32,
        file_name: [u16; 1],
    }

    #[repr(C)]
    struct FileDispositionInfo {
        delete_file: u8,
    }

    unsafe extern "system" {
        fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: Handle,
        ) -> Handle;
        fn GetFinalPathNameByHandleW(
            file: Handle,
            path: *mut u16,
            path_length: u32,
            flags: u32,
        ) -> u32;
        fn GetFileInformationByHandleEx(
            file: Handle,
            info_class: i32,
            info: *mut c_void,
            size: u32,
        ) -> i32;
        fn SetFileInformationByHandle(
            file: Handle,
            info_class: i32,
            info: *const c_void,
            size: u32,
        ) -> i32;
        fn RtlNtStatusToDosError(status: NtStatus) -> u32;
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtCreateFile(
            file_handle: *mut Handle,
            desired_access: u32,
            object_attributes: *mut ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            allocation_size: *mut i64,
            file_attributes: u32,
            share_access: u32,
            create_disposition: u32,
            create_options: u32,
            ea_buffer: *mut c_void,
            ea_length: u32,
        ) -> NtStatus;
        fn NtSetInformationFile(
            file_handle: Handle,
            io_status_block: *mut IoStatusBlock,
            file_information: *const c_void,
            length: u32,
            file_information_class: u32,
        ) -> NtStatus;
    }

    struct DirectoryChain {
        handles: Vec<File>,
    }

    #[cfg(test)]
    thread_local! {
        static BEFORE_RENAME_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
        static BEFORE_DELETE_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }

    impl DirectoryChain {
        fn parent(&self) -> &File {
            self.handles.last().expect("directory chain contains root")
        }
    }

    pub(super) fn atomic_write(
        root: &Path,
        path: &Path,
        bytes: &[u8],
        replace_existing: bool,
    ) -> io::Result<()> {
        let (components, file_name) = split_contained_path(root, path)?;
        let chain = open_directory_chain(root, &components, true)
            .map_err(|error| contextual("open contained parent", error))?;
        let parent = chain.parent();
        let (mut temp, _) = create_temp(parent, file_name)
            .map_err(|error| contextual("create contained temporary file", error))?;
        if let Err(error) = temp.write_all(bytes).and_then(|()| temp.sync_all()) {
            let _ = delete_open_file(&temp);
            return Err(error);
        }
        #[cfg(test)]
        BEFORE_RENAME_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        if let Err(error) = rename_open_file(&temp, parent, file_name, replace_existing) {
            let _ = delete_open_file(&temp);
            return Err(contextual("rename contained temporary file", error));
        }
        drop(temp);
        drop(chain);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn set_before_rename_hook(hook: impl FnOnce() + 'static) {
        BEFORE_RENAME_HOOK.with(|slot| {
            assert!(
                slot.borrow().is_none(),
                "publication hook already installed"
            );
            *slot.borrow_mut() = Some(Box::new(hook));
        });
    }

    fn contextual(context: &str, error: io::Error) -> io::Error {
        io::Error::new(error.kind(), format!("{context}: {error}"))
    }

    pub(super) fn ensure_directory(root: &Path, directory: &Path) -> io::Result<()> {
        let relative = contained_components(root, directory)?;
        let chain = open_directory_chain(root, &relative, true)?;
        drop(chain);
        Ok(())
    }

    pub(super) fn open_generation_lock(root: &Path) -> io::Result<(File, Option<File>)> {
        let mut chain = open_directory_chain(root, &[], false)?;
        let root_directory = chain.handles.pop().expect("directory chain contains root");
        let lock = nt_create_relative(
            &root_directory,
            OsStr::new(".tovek-output.lock"),
            GENERIC_READ | GENERIC_WRITE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN_IF,
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
        )?;
        reject_reparse(&lock)?;
        Ok((lock, Some(root_directory)))
    }

    pub(super) fn remove_regular_file(root: &Path, path: &Path) -> io::Result<()> {
        let (components, file_name) = split_contained_path(root, path)?;
        let chain = open_directory_chain(root, &components, false)
            .map_err(|error| contextual("open contained parent for deletion", error))?;
        #[cfg(test)]
        BEFORE_DELETE_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let file = nt_create_relative(
            chain.parent(),
            file_name,
            DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            1, // FILE_OPEN
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
        )?;
        reject_reparse(&file)?;
        delete_open_file(&file)?;
        drop(file);
        drop(chain);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn set_before_delete_hook(hook: impl FnOnce() + 'static) {
        BEFORE_DELETE_HOOK.with(|slot| {
            assert!(slot.borrow().is_none(), "deletion hook already installed");
            *slot.borrow_mut() = Some(Box::new(hook));
        });
    }

    fn split_contained_path<'a>(
        root: &Path,
        path: &'a Path,
    ) -> io::Result<(Vec<&'a OsStr>, &'a OsStr)> {
        let mut components = contained_components(root, path)?;
        let file_name = components.pop().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "publication path has no file name",
            )
        })?;
        Ok((components, file_name))
    }

    fn contained_components<'a>(root: &Path, path: &'a Path) -> io::Result<Vec<&'a OsStr>> {
        let relative = path.strip_prefix(root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "publication path {} is outside {}",
                    path.display(),
                    root.display()
                ),
            )
        })?;
        let mut result = Vec::new();
        for component in relative.components() {
            match component {
                std::path::Component::Normal(name) if !name.as_encoded_bytes().contains(&b':') => {
                    result.push(name)
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "publication path contains an unsafe component",
                    ));
                }
            }
        }
        Ok(result)
    }

    fn open_directory_chain(
        root: &Path,
        components: &[&OsStr],
        create: bool,
    ) -> io::Result<DirectoryChain> {
        let canonical_root = std::fs::canonicalize(root)?;
        if path_key(&canonical_root) != path_key(root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "trusted output root {} changed to {}",
                    root.display(),
                    canonical_root.display()
                ),
            ));
        }
        let mut root_wide = root
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let raw = unsafe {
            CreateFileW(
                root_wide.as_mut_ptr(),
                FILE_LIST_DIRECTORY
                    | FILE_ADD_FILE
                    | FILE_ADD_SUBDIRECTORY
                    | FILE_READ_ATTRIBUTES
                    | SYNCHRONIZE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            )
        };
        if raw as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        let root_file = unsafe { File::from_raw_handle(raw as RawHandle) };
        let final_root = final_path(&root_file)?;
        if path_key(&final_root) != path_key(root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "trusted output root handle resolved to {}",
                    final_root.display()
                ),
            ));
        }
        reject_reparse(&root_file)?;

        let mut handles = vec![root_file];
        for component in components {
            let child = nt_open_relative_directory(
                handles.last().expect("root exists"),
                component,
                create,
            )?;
            reject_reparse(&child)?;
            handles.push(child);
        }
        Ok(DirectoryChain { handles })
    }

    fn nt_open_relative_directory(parent: &File, name: &OsStr, create: bool) -> io::Result<File> {
        nt_create_relative(
            parent,
            name,
            FILE_LIST_DIRECTORY
                | FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_READ_ATTRIBUTES
                | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            if create { FILE_OPEN_IF } else { 1 },
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
        )
    }

    fn create_temp(parent: &File, destination_name: &OsStr) -> io::Result<(File, Vec<u16>)> {
        let stem = destination_name.to_string_lossy();
        loop {
            let name = format!(
                ".{stem}.{}.{}.tmp",
                std::process::id(),
                ATOMIC_WRITE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
            );
            let wide = OsStr::new(&name).encode_wide().collect::<Vec<_>>();
            match nt_create_relative(
                parent,
                OsStr::new(&name),
                GENERIC_WRITE | DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_CREATE,
                FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            ) {
                Ok(file) => return Ok((file, wide)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn nt_create_relative(
        parent: &File,
        name: &OsStr,
        desired_access: u32,
        share_access: u32,
        disposition: u32,
        options: u32,
    ) -> io::Result<File> {
        let mut wide = name.encode_wide().collect::<Vec<_>>();
        let byte_len = wide
            .len()
            .checked_mul(2)
            .and_then(|len| u16::try_from(len).ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "relative file name is too long",
                )
            })?;
        let mut unicode = UnicodeString {
            length: byte_len,
            maximum_length: byte_len,
            buffer: wide.as_mut_ptr(),
        };
        let mut attributes = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as u32,
            root_directory: parent.as_raw_handle() as Handle,
            object_name: &mut unicode,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: null_mut(),
            security_quality_of_service: null_mut(),
        };
        let mut io_status = IoStatusBlock {
            status: 0,
            information: 0,
        };
        let mut handle: Handle = null_mut();
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                desired_access,
                &mut attributes,
                &mut io_status,
                null_mut(),
                FILE_ATTRIBUTE_NORMAL,
                share_access,
                disposition,
                options,
                null_mut(),
                0,
            )
        };
        if status < 0 {
            let code = unsafe { RtlNtStatusToDosError(status) } as i32;
            let error = io::Error::from_raw_os_error(code);
            return if matches!(code, ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS) {
                Err(io::Error::new(io::ErrorKind::AlreadyExists, error))
            } else {
                Err(error)
            };
        }
        Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
    }

    fn rename_open_file(
        file: &File,
        parent: &File,
        destination_name: &OsStr,
        replace_existing: bool,
    ) -> io::Result<()> {
        let wide = destination_name.encode_wide().collect::<Vec<_>>();
        let name_offset = offset_of!(FileRenameInfoLayout, file_name);
        // Windows validates the full trailing WCHAR member even though
        // FileNameLength excludes the terminator.
        let mut buffer = vec![0u8; name_offset + (wide.len() + 1) * 2];
        unsafe {
            let layout = buffer.as_mut_ptr() as *mut FileRenameInfoLayout;
            (*layout).replace_if_exists = u8::from(replace_existing);
            (*layout).root_directory = parent.as_raw_handle() as Handle;
            (*layout).file_name_length = (wide.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(
                wide.as_ptr() as *const u8,
                buffer.as_mut_ptr().add(name_offset),
                wide.len() * 2,
            );
        }
        let mut io_status = IoStatusBlock {
            status: 0,
            information: 0,
        };
        let status = unsafe {
            NtSetInformationFile(
                file.as_raw_handle() as Handle,
                &mut io_status,
                buffer.as_ptr() as *const c_void,
                buffer.len() as u32,
                10, // FileRenameInformation
            )
        };
        if status < 0 {
            let code = unsafe { RtlNtStatusToDosError(status) } as i32;
            let error = io::Error::from_raw_os_error(code);
            if matches!(code, ERROR_ALREADY_EXISTS | ERROR_FILE_EXISTS) {
                Err(io::Error::new(io::ErrorKind::AlreadyExists, error))
            } else {
                Err(error)
            }
        } else {
            Ok(())
        }
    }

    fn delete_open_file(file: &File) -> io::Result<()> {
        let disposition = FileDispositionInfo { delete_file: 1 };
        let result = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as Handle,
                FILE_DISPOSITION_INFO_CLASS,
                &disposition as *const _ as *const c_void,
                size_of::<FileDispositionInfo>() as u32,
            )
        };
        if result == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn reject_reparse(file: &File) -> io::Result<()> {
        let mut info = FileAttributeTagInfo {
            file_attributes: 0,
            reparse_tag: 0,
        };
        let result = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as Handle,
                FILE_ATTRIBUTE_TAG_INFO_CLASS,
                &mut info as *mut _ as *mut c_void,
                size_of::<FileAttributeTagInfo>() as u32,
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        if info.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "publication directory is a reparse point",
            ));
        }
        Ok(())
    }

    fn final_path(file: &File) -> io::Result<PathBuf> {
        let needed =
            unsafe { GetFinalPathNameByHandleW(file.as_raw_handle() as Handle, null_mut(), 0, 0) };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u16; needed as usize + 1];
        let written = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle() as Handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        };
        if written == 0 || written as usize >= buffer.len() {
            return Err(io::Error::last_os_error());
        }
        Ok(PathBuf::from(String::from_utf16_lossy(
            &buffer[..written as usize],
        )))
    }

    fn path_key(path: &Path) -> String {
        let text = path.to_string_lossy().replace('/', "\\");
        let text = text
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
            .unwrap_or(text);
        text.trim_end_matches('\\').to_lowercase()
    }
}

#[cfg(any(test, not(windows)))]
fn create_atomic_temp_file(
    parent: &Path,
    file_name: &str,
    temp_id: &AtomicU64,
) -> std::io::Result<(PathBuf, File)> {
    loop {
        let candidate = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            temp_id.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(all(not(unix), any(test, not(windows))))]
fn sync_parent_directory(_parent: &Path) -> std::io::Result<()> {
    // MoveFileExW uses MOVEFILE_WRITE_THROUGH in replace_file on Windows.
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, destination)
}

#[cfg(all(windows, test))]
fn replace_file(temp_path: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }
    let existing = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            replacement.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        let error = std::io::Error::last_os_error();
        let _ = std::fs::remove_file(temp_path);
        Err(error)
    } else {
        Ok(())
    }
}

/// Extract a human-readable message from a caught panic payload.
pub(crate) fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(s) => format!("panicked: {s}"),
        Err(p) => match p.downcast::<&'static str>() {
            Ok(s) => format!("panicked: {s}"),
            Err(_) => "panicked: <non-string payload>".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "tovek-core-{name}-{}-{}",
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

    fn entry(export_id: &str, relative: &str) -> Value {
        json!({
            "export_id": export_id,
            "ordinal_one_based": 1,
            "name": "Script",
            "class_name": "ModuleScript",
            "full_name": "Workspace.Script",
            "path_segments": [{"name": "Workspace", "class_name": "Workspace"}],
            "dump_path": format!("Dump/{relative}"),
            "dump_relative_path": relative,
            "legacy_candidate_dump_relative_path": relative,
            "path_collision_resolved": false,
            "non_ascii_path_uniquified": false,
            "extraction_kind": "source_fallback",
            "is_bytecode": false,
            "bytecode_analysis_eligible": false,
            "payload_encoding": "base64",
            "status": "saved",
            "raw_size_bytes": 0,
            "payload_size_bytes": 0,
            "diagnostics": []
        })
    }

    fn manifest(scripts: Vec<Value>) -> Value {
        json!({
            "schema": "volt-decompile-export-manifest",
            "schema_version": 1,
            "status": "complete",
            "payload_encoding": "base64",
            "counts": {
                "discovered": scripts.len(),
                "saved": scripts.len(),
                "failed": 0,
                "raw_bytecode": 0,
                "source_fallback": scripts.len(),
                "extraction_failure": 0
            },
            "scripts": scripts
        })
    }

    #[test]
    fn malformed_and_case_colliding_saved_entries_are_rejected_deterministically() {
        let temp = TestDir::new("collisions");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(src.join("Folder")).unwrap();
        std::fs::write(src.join("Folder/A.lua"), b"").unwrap();
        std::fs::write(src.join("Folder/a.LUA"), b"").unwrap();
        let mut malformed = entry("volt-script-000003", "Folder/Missing.lua");
        malformed
            .as_object_mut()
            .unwrap()
            .remove("payload_encoding");
        let value = manifest(vec![
            entry("volt-script-000001", "Folder/A.lua"),
            entry("volt-script-000002", "Folder/a.LUA"),
            malformed,
        ]);
        let manifest_path = src.join(".volt-export-manifest.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();

        let (_, _, work, inventory) =
            build_work_from_export_manifest(&src, &out, "lua", &manifest_path).unwrap();
        assert!(work.is_empty());
        assert_eq!(inventory.total_scripts, 3);
        assert_eq!(inventory.failed_scripts, 3);
        assert_eq!(
            inventory
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            vec![
                "case_colliding_output_path",
                "case_colliding_output_path",
                "malformed_export_entry"
            ]
        );
    }

    #[test]
    fn canonical_output_directory_aliases_are_rejected_before_parallel_writes() {
        let temp = TestDir::new("canonical-output-alias");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        let real_output_dir = out.join("Real");
        let alias_output_dir = out.join("Alias");
        std::fs::create_dir_all(src.join("Real")).unwrap();
        std::fs::create_dir_all(src.join("Alias")).unwrap();
        std::fs::create_dir_all(&real_output_dir).unwrap();
        std::fs::write(src.join("Real/A.lua"), b"").unwrap();
        std::fs::write(src.join("Alias/A.lua"), b"").unwrap();

        #[cfg(windows)]
        let linked = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&alias_output_dir)
            .arg(&real_output_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&real_output_dir, &alias_output_dir).is_ok();
        if !linked {
            return;
        }

        let manifest_path = src.join(".volt-export-manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest(vec![
                entry("volt-script-real", "Real/A.lua"),
                entry("volt-script-alias", "Alias/A.lua"),
            ]))
            .unwrap(),
        )
        .unwrap();

        let (_, _, work, inventory) =
            build_work_from_export_manifest(&src, &out, "lua", &manifest_path).unwrap();
        assert!(work.is_empty());
        assert_eq!(inventory.failed_scripts, 2);
        assert!(
            inventory
                .diagnostics
                .iter()
                .all(|diagnostic| { diagnostic.code == "canonical_output_path_alias" })
        );
        assert!(!real_output_dir.join("A.lua").exists());
    }

    #[cfg(windows)]
    #[test]
    fn parent_junction_swap_cannot_redirect_raw_fallback_or_empty_publication() {
        let temp = TestDir::new("source-parent-junction-swap");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        let output_parent = out.join("Workspace");
        let outside = temp.0.join("outside");
        std::fs::create_dir_all(src.join("Workspace")).unwrap();
        std::fs::create_dir_all(&output_parent).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let bytecode = include_bytes!("../tests/fixtures/upvalue_analysis_nested_g0.luaubc");
        let raw_payload = BASE64_STANDARD.encode(bytecode);
        let fallback_source = b"return 'fallback'";
        let fallback_payload = BASE64_STANDARD.encode(fallback_source);
        std::fs::write(src.join("Workspace/Raw.lua"), &raw_payload).unwrap();
        std::fs::write(src.join("Workspace/Empty.lua"), b"").unwrap();
        std::fs::write(src.join("Workspace/Fallback.lua"), &fallback_payload).unwrap();

        let mut raw = entry("volt-script-raw", "Workspace/Raw.lua");
        raw["extraction_kind"] = json!("raw_bytecode");
        raw["is_bytecode"] = json!(true);
        raw["bytecode_analysis_eligible"] = json!(true);
        raw["raw_size_bytes"] = json!(bytecode.len());
        raw["payload_size_bytes"] = json!(raw_payload.len());
        let mut empty = entry("volt-script-empty", "Workspace/Empty.lua");
        empty["extraction_kind"] = json!("raw_bytecode");
        empty["is_bytecode"] = json!(true);
        empty["bytecode_analysis_eligible"] = json!(true);
        let mut fallback = entry("volt-script-fallback", "Workspace/Fallback.lua");
        fallback["raw_size_bytes"] = json!(fallback_source.len());
        fallback["payload_size_bytes"] = json!(fallback_payload.len());
        let mut value = manifest(vec![raw, empty, fallback]);
        value["counts"]["raw_bytecode"] = json!(2);
        value["counts"]["source_fallback"] = json!(1);
        let manifest_path = src.join(".volt-export-manifest.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();

        // Manifest parsing validates this genuine parent. Swap it only after
        // work construction to exercise the publication boundary itself.
        let (_, _, work, inventory) =
            build_work_from_export_manifest(&src, &out, "lua", &manifest_path).unwrap();
        assert_eq!(inventory.failed_scripts, 0);
        assert_eq!(work.len(), 3);
        let parked = out.join("Workspace-parked");
        std::fs::rename(&output_parent, &parked).unwrap();
        let linked = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&output_parent)
            .arg(&outside)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !linked {
            std::fs::rename(&parked, &output_parent).unwrap();
            return;
        }

        for work in &work {
            let outcome = process_one(work, 1, &mut Vec::new(), false, DecompileOptions::default());
            assert!(
                matches!(outcome, Outcome::Fail(_)),
                "{} unexpectedly published through swapped parent",
                work.rel
            );
        }
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        std::fs::remove_dir(&output_parent).unwrap();
        std::fs::rename(&parked, &output_parent).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn held_parent_handles_block_a_swap_between_temp_creation_and_rename() {
        use std::sync::{Arc, Mutex};

        let temp = TestDir::new("mid-publication-parent-swap");
        let root = std::fs::canonicalize(&temp.0).unwrap();
        let parent = root.join("Workspace");
        let parked = root.join("Workspace-parked");
        std::fs::create_dir(&parent).unwrap();
        let swap_result = Arc::new(Mutex::new(None));
        let captured_result = Arc::clone(&swap_result);
        let captured_parent = parent.clone();
        let captured_parked = parked.clone();
        windows_contained_fs::set_before_rename_hook(move || {
            *captured_result.lock().unwrap() =
                Some(std::fs::rename(&captured_parent, &captured_parked));
        });

        let destination = parent.join("Result.lua");
        atomic_write_contained(&root, &destination, b"return 42\n", false).unwrap();
        let attempted_swap = swap_result.lock().unwrap().take().unwrap();
        assert!(
            attempted_swap.is_err(),
            "an output parent was renamed while its publication handle was held"
        );
        assert_eq!(std::fs::read(destination).unwrap(), b"return 42\n");
        assert!(!parked.exists());
    }

    #[cfg(windows)]
    #[test]
    fn contained_no_replace_rejects_a_destination_created_after_temp_write() {
        let temp = TestDir::new("contained-late-destination");
        let root = std::fs::canonicalize(&temp.0).unwrap();
        let parent = root.join("Workspace");
        std::fs::create_dir(&parent).unwrap();
        let destination = parent.join("Result.lua");
        let hook_destination = destination.clone();
        windows_contained_fs::set_before_rename_hook(move || {
            std::fs::write(hook_destination, b"late user file").unwrap();
        });

        let error = atomic_write_contained(&root, &destination, b"generated", false).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&destination).unwrap(), b"late user file");
        assert!(std::fs::read_dir(&parent).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn schema_version_and_payload_encoding_are_authoritative() {
        let temp = TestDir::new("schema-validation");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let manifest_path = src.join(".volt-export-manifest.json");
        let mut value = manifest(vec![]);
        value["payload_encoding"] = json!("raw");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(build_work_from_export_manifest(&src, &out, "lua", &manifest_path).is_err());

        value["payload_encoding"] = json!("base64");
        value["schema_version"] = json!(2);
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(build_work_from_export_manifest(&src, &out, "lua", &manifest_path).is_err());

        value["schema_version"] = json!(1);
        value["status"] = json!("unknown");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(build_work_from_export_manifest(&src, &out, "lua", &manifest_path).is_err());
    }

    #[test]
    fn path_escape_and_duplicate_export_ids_are_rejected() {
        let temp = TestDir::new("escape-and-duplicate-id");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(src.join("Folder")).unwrap();
        std::fs::write(src.join("Folder/A.lua"), b"").unwrap();
        std::fs::write(src.join("Folder/B.lua"), b"").unwrap();
        let value = manifest(vec![
            entry("volt-script-duplicate", "Folder/A.lua"),
            entry("volt-script-duplicate", "Folder/B.lua"),
            entry("volt-script-escape", "../outside.lua"),
        ]);
        let manifest_path = src.join(".volt-export-manifest.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();

        let (_, _, work, inventory) =
            build_work_from_export_manifest(&src, &out, "lua", &manifest_path).unwrap();
        assert!(work.is_empty());
        assert_eq!(inventory.failed_scripts, 3);
        assert_eq!(
            inventory
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            vec![
                "unsafe_dump_relative_path",
                "duplicate_export_id",
                "duplicate_export_id"
            ]
        );
    }

    #[test]
    fn manifest_relative_paths_reject_windows_alternate_stream_syntax() {
        assert!(is_safe_relative_path(Path::new("Folder/A.lua")));
        assert!(!is_safe_relative_path(Path::new("Folder/A.lua:payload")));
        assert!(!is_safe_relative_path(Path::new("Folder/A.lua::$DATA")));
        assert!(!is_safe_relative_path(Path::new("Folder:payload/A.lua")));
        assert!(!is_safe_relative_path(Path::new("C:relative.lua")));
    }

    #[test]
    fn export_manifest_rejects_alternate_stream_path_before_filesystem_resolution() {
        let temp = TestDir::new("reject-ads-manifest-path");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let manifest_path = src.join(".volt-export-manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest(vec![entry(
                "volt-script-ads",
                "Folder/A.lua::$DATA",
            )]))
            .unwrap(),
        )
        .unwrap();

        let (_, _, work, inventory) =
            build_work_from_export_manifest(&src, &out, "lua", &manifest_path).unwrap();
        assert!(work.is_empty());
        assert_eq!(inventory.failed_scripts, 1);
        assert_eq!(inventory.diagnostics.len(), 1);
        assert_eq!(inventory.diagnostics[0].code, "unsafe_dump_relative_path");
    }

    #[cfg(windows)]
    #[test]
    fn existing_ntfs_alternate_stream_is_rejected_without_creating_output() {
        let temp = TestDir::new("reject-existing-ads");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(src.join("Folder")).unwrap();
        std::fs::write(src.join("Folder/A.lua"), b"ordinary file").unwrap();
        let stream = src.join("Folder/A.lua:tovek-test");
        if std::fs::write(&stream, b"alternate stream").is_err() {
            return;
        }
        assert_eq!(std::fs::read(&stream).unwrap(), b"alternate stream");
        let manifest_path = src.join(".volt-export-manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest(vec![entry(
                "volt-script-existing-ads",
                "Folder/A.lua:tovek-test",
            )]))
            .unwrap(),
        )
        .unwrap();

        let (_, _, work, inventory) =
            build_work_from_export_manifest(&src, &out, "lua", &manifest_path).unwrap();
        assert!(work.is_empty());
        assert_eq!(inventory.failed_scripts, 1);
        assert_eq!(inventory.diagnostics[0].code, "unsafe_dump_relative_path");
        assert!(!out.join("Folder/A.lua").exists());
    }

    #[test]
    fn duplicate_export_ids_include_failed_and_malformed_raw_entries() {
        let temp = TestDir::new("duplicate-id-before-filtering");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(src.join("Folder")).unwrap();
        std::fs::write(src.join("Folder/A.lua"), b"").unwrap();

        let mut failed = entry("volt-script-duplicate", "Folder/B.lua");
        failed["status"] = json!("failed");
        let mut malformed = entry("volt-script-duplicate", "Folder/C.lua");
        malformed.as_object_mut().unwrap().remove("class_name");
        let mut value = manifest(vec![
            entry("volt-script-duplicate", "Folder/A.lua"),
            failed,
            malformed,
        ]);
        value["status"] = json!("partial");
        value["counts"]["saved"] = json!(2);
        value["counts"]["failed"] = json!(1);
        let manifest_path = src.join(".volt-export-manifest.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();

        let (_, _, work, inventory) =
            build_work_from_export_manifest(&src, &out, "lua", &manifest_path).unwrap();
        assert!(work.is_empty());
        assert_eq!(inventory.failed_scripts, 3);
        assert_eq!(inventory.diagnostics.len(), 3);
        let duplicate_diagnostics = inventory
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "duplicate_export_id")
            .collect::<Vec<_>>();
        assert_eq!(duplicate_diagnostics.len(), 3);
        assert!(duplicate_diagnostics.iter().all(|diagnostic| {
            diagnostic.export_id.as_deref() == Some("volt-script-duplicate")
        }));
    }

    #[test]
    fn existing_content_addressed_file_is_verified_without_replacement() {
        let temp = TestDir::new("content-addressed-no-replace");
        let path = temp.0.join("sidecar.json");
        let bytes = b"{\"schema_version\":2}\n";
        let digest = sha256_hex(bytes);
        write_content_addressed_file(&path, bytes, &digest).unwrap();

        let original_permissions = std::fs::metadata(&path).unwrap().permissions();
        let mut readonly_permissions = original_permissions.clone();
        readonly_permissions.set_readonly(true);
        std::fs::set_permissions(&path, readonly_permissions).unwrap();
        write_content_addressed_file(&path, bytes, &digest).unwrap();
        assert!(std::fs::metadata(&path).unwrap().permissions().readonly());

        std::fs::set_permissions(&path, original_permissions).unwrap();
        std::fs::write(&path, b"corrupt").unwrap();
        let error = write_content_addressed_file(&path, bytes, &digest).unwrap_err();
        assert!(error.contains("does not match its expected SHA-256"));
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
    }

    #[test]
    fn atomic_temp_creation_skips_a_planted_hardlink_without_following_it() {
        let temp = TestDir::new("atomic-temp-hardlink");
        let target = temp.0.join("attacker-target");
        std::fs::write(&target, b"attacker").unwrap();
        let planted = temp
            .0
            .join(format!(".destination.json.{}.0.tmp", std::process::id()));
        std::fs::hard_link(&target, &planted).unwrap();
        let temp_id = AtomicU64::new(0);

        let (created, mut file) =
            create_atomic_temp_file(&temp.0, "destination.json", &temp_id).unwrap();
        file.write_all(b"ours").unwrap();
        file.sync_all().unwrap();
        drop(file);

        assert_ne!(created, planted);
        assert_eq!(std::fs::read(&target).unwrap(), b"attacker");
        assert_eq!(std::fs::read(&planted).unwrap(), b"attacker");
        assert_eq!(std::fs::read(&created).unwrap(), b"ours");
    }

    #[test]
    fn atomic_temp_creation_skips_a_planted_symlink_without_following_it() {
        let temp = TestDir::new("atomic-temp-symlink");
        let target = temp.0.join("attacker-target");
        std::fs::write(&target, b"attacker").unwrap();
        let planted = temp
            .0
            .join(format!(".destination.json.{}.0.tmp", std::process::id()));
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&target, &planted).is_ok();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, &planted).is_ok();
        if !linked {
            return;
        }
        let temp_id = AtomicU64::new(0);

        let (created, mut file) =
            create_atomic_temp_file(&temp.0, "destination.json", &temp_id).unwrap();
        file.write_all(b"ours").unwrap();
        file.sync_all().unwrap();
        drop(file);

        assert_ne!(created, planted);
        assert_eq!(std::fs::read(&target).unwrap(), b"attacker");
        assert_eq!(std::fs::read(&planted).unwrap(), b"attacker");
        assert_eq!(std::fs::read(&created).unwrap(), b"ours");
    }

    #[test]
    fn analysis_empty_raw_output_replaces_a_hardlink_without_truncating_its_target() {
        let temp = TestDir::new("analysis-empty-raw-hardlink");
        let input = temp.0.join("empty-bytecode.lua");
        let output = temp.0.join("out/empty.lua");
        let target = temp.0.join("hardlink-target.lua");
        std::fs::write(&input, b"").unwrap();
        std::fs::write(&target, b"attacker-owned content").unwrap();
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::hard_link(&target, &output).unwrap();
        let mut original_target = File::open(&target).unwrap();
        let work = Work {
            input,
            output: output.clone(),
            output_root: temp.0.join("out"),
            rel: "empty-bytecode.lua".to_string(),
            source_rel: "empty.lua".to_string(),
            kind: WorkKind::RawBytecode,
            volt_export: None,
        };

        let (outcome, entry, unavailable, source_written) = process_one_with_analysis(
            &work,
            1,
            &mut Vec::new(),
            false,
            DecompileOptions::default(),
            &temp.0.join("out/.tovek-analysis"),
        );

        assert!(
            matches!(outcome, Outcome::Skipped),
            "unexpected outcome: {}",
            match &outcome {
                Outcome::Ok => "ok".to_string(),
                Outcome::Skipped => "skipped".to_string(),
                Outcome::Fail(error) => format!("fail: {error}"),
            }
        );
        assert!(entry.is_none());
        assert!(unavailable.is_none());
        assert!(source_written.is_some());
        assert_eq!(std::fs::read(&output).unwrap(), b"");
        assert_eq!(std::fs::read(&target).unwrap(), b"attacker-owned content");
        let mut original_content = String::new();
        std::io::Read::read_to_string(&mut original_target, &mut original_content).unwrap();
        assert_eq!(original_content, "attacker-owned content");
    }

    #[test]
    fn analysis_empty_raw_output_replaces_a_symlink_without_truncating_its_target() {
        let temp = TestDir::new("analysis-empty-raw-symlink");
        let input = temp.0.join("empty-bytecode.lua");
        let output = temp.0.join("out/empty.lua");
        let target = temp.0.join("symlink-target.lua");
        std::fs::write(&input, b"").unwrap();
        std::fs::write(&target, b"attacker-owned content").unwrap();
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&target, &output).is_ok();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, &output).is_ok();
        if !linked {
            return;
        }
        let mut original_target = File::open(&target).unwrap();
        let work = Work {
            input,
            output: output.clone(),
            output_root: temp.0.join("out"),
            rel: "empty-bytecode.lua".to_string(),
            source_rel: "empty.lua".to_string(),
            kind: WorkKind::RawBytecode,
            volt_export: None,
        };

        let (outcome, entry, unavailable, source_written) = process_one_with_analysis(
            &work,
            1,
            &mut Vec::new(),
            false,
            DecompileOptions::default(),
            &temp.0.join("out/.tovek-analysis"),
        );

        assert!(matches!(outcome, Outcome::Skipped));
        assert!(entry.is_none());
        assert!(unavailable.is_none());
        assert!(source_written.is_some());
        assert!(
            !std::fs::symlink_metadata(&output)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"");
        assert_eq!(std::fs::read(&target).unwrap(), b"attacker-owned content");
        let mut original_content = String::new();
        std::io::Read::read_to_string(&mut original_target, &mut original_content).unwrap();
        assert_eq!(original_content, "attacker-owned content");
    }

    #[test]
    fn content_addressed_publication_is_concurrently_no_clobber() {
        let temp = TestDir::new("content-addressed-concurrent-publish");
        let destination = temp.0.join("winner.json");
        let first = temp.0.join("first.new");
        let second = temp.0.join("second.new");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let spawn = |candidate: PathBuf| {
            let barrier = barrier.clone();
            let destination = destination.clone();
            std::thread::spawn(move || {
                barrier.wait();
                publish_new_file(&candidate, &destination)
            })
        };
        let first_publish = spawn(first);
        let second_publish = spawn(second);
        barrier.wait();
        let results = [
            first_publish.join().unwrap(),
            second_publish.join().unwrap(),
        ];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result
                        .as_ref()
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists)
                })
                .count(),
            1
        );
        assert!(matches!(
            std::fs::read(destination).unwrap().as_slice(),
            b"first" | b"second"
        ));
    }

    #[test]
    fn generation_lock_child_process() {
        let (Ok(root), Ok(started), Ok(acquired)) = (
            std::env::var("TOVEK_TEST_GENERATION_LOCK_ROOT"),
            std::env::var("TOVEK_TEST_GENERATION_LOCK_STARTED"),
            std::env::var("TOVEK_TEST_GENERATION_LOCK_ACQUIRED"),
        ) else {
            return;
        };
        std::fs::write(started, b"started").unwrap();
        let _lock = acquire_output_generation_lock(Path::new(&root)).unwrap();
        std::fs::write(acquired, b"acquired").unwrap();
    }

    #[test]
    fn generation_lock_serializes_independent_processes() {
        let temp = TestDir::new("generation-cross-process-lock");
        let out = temp.0.join("out");
        let started = temp.0.join("child-started");
        let acquired = temp.0.join("child-acquired");
        std::fs::create_dir_all(&out).unwrap();
        let out_root = std::fs::canonicalize(&out).unwrap();
        let first_lock = acquire_output_generation_lock(&out_root).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "decompile_core::tests::generation_lock_child_process",
                "--nocapture",
            ])
            .env("TOVEK_TEST_GENERATION_LOCK_ROOT", &out_root)
            .env("TOVEK_TEST_GENERATION_LOCK_STARTED", &started)
            .env("TOVEK_TEST_GENERATION_LOCK_ACQUIRED", &acquired)
            .spawn()
            .unwrap();
        for _ in 0..200 {
            if started.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            started.exists(),
            "child process did not reach lock acquisition"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !acquired.exists(),
            "child bypassed the held generation lock"
        );
        drop(first_lock);
        assert!(child.wait().unwrap().success());
        assert!(acquired.exists());
    }

    #[test]
    fn generation_lock_creates_a_fresh_output_root() {
        let temp = TestDir::new("generation-lock-fresh-output");
        let out = temp.0.join("new-output");
        assert!(!out.exists());

        let _lock = acquire_output_generation_lock(&out).unwrap();

        assert!(out.is_dir());
        assert!(out.join(".tovek-output.lock").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn generation_lock_file_and_root_cannot_be_replaced_while_held() {
        let temp = TestDir::new("generation-lock-replacement");
        let out = temp.0.join("out");
        let parked_lock = temp.0.join("parked.lock");
        let parked_root = temp.0.join("parked-root");
        let lock = acquire_output_generation_lock(&out).unwrap();

        assert!(
            std::fs::rename(out.join(".tovek-output.lock"), &parked_lock).is_err(),
            "held lock file must deny rename/recreate bypasses"
        );
        assert!(
            std::fs::rename(&out, &parked_root).is_err(),
            "held output-root handle must deny path replacement"
        );
        assert!(!parked_lock.exists());
        assert!(!parked_root.exists());
        drop(lock);
    }

    #[test]
    fn non_analysis_writer_can_invalidate_manifest_without_deleting_sidecars() {
        let temp = TestDir::new("invalidate-analysis-manifest");
        let out = temp.0.join("out");
        let analysis = out.join(".tovek-analysis");
        let sidecar = analysis.join("scripts/sidecar.json");
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(analysis.join("manifest.json"), b"manifest").unwrap();
        std::fs::write(analysis.join("source-write-provenance.json"), b"provenance").unwrap();
        std::fs::write(&sidecar, b"sidecar").unwrap();
        let out_root = std::fs::canonicalize(&out).unwrap();
        let _lock = acquire_output_generation_lock(&out_root).unwrap();

        invalidate_analysis_manifest(&out_root).unwrap();

        assert!(!analysis.join("manifest.json").exists());
        assert!(!analysis.join("source-write-provenance.json").exists());
        assert_eq!(std::fs::read(sidecar).unwrap(), b"sidecar");
    }

    #[cfg(windows)]
    #[test]
    fn invalidation_holds_parent_handles_across_deletion() {
        use std::sync::{Arc, Mutex};

        let temp = TestDir::new("invalidation-parent-swap");
        let out = temp.0.join("out");
        let analysis = out.join(".tovek-analysis");
        let parked = out.join("analysis-parked");
        std::fs::create_dir_all(&analysis).unwrap();
        std::fs::write(analysis.join("source-write-provenance.json"), b"old").unwrap();
        std::fs::write(analysis.join("manifest.json"), b"old").unwrap();
        let out_root = std::fs::canonicalize(&out).unwrap();
        let result = Arc::new(Mutex::new(None));
        let captured_result = Arc::clone(&result);
        let captured_analysis = analysis.clone();
        let captured_parked = parked.clone();
        windows_contained_fs::set_before_delete_hook(move || {
            *captured_result.lock().unwrap() =
                Some(std::fs::rename(&captured_analysis, &captured_parked));
        });

        invalidate_analysis_manifest(&out_root).unwrap();

        assert!(result.lock().unwrap().take().unwrap().is_err());
        assert!(!analysis.join("source-write-provenance.json").exists());
        assert!(!analysis.join("manifest.json").exists());
        assert!(!parked.exists());
    }

    #[cfg(windows)]
    #[test]
    fn invalidation_does_not_follow_a_reparse_file() {
        let temp = TestDir::new("invalidation-reparse-file");
        let out = temp.0.join("out");
        let analysis = out.join(".tovek-analysis");
        let outside = temp.0.join("outside.json");
        std::fs::create_dir_all(&analysis).unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        if std::os::windows::fs::symlink_file(
            &outside,
            analysis.join("source-write-provenance.json"),
        )
        .is_err()
        {
            return;
        }
        let out_root = std::fs::canonicalize(&out).unwrap();

        assert!(invalidate_analysis_manifest(&out_root).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"outside");
    }

    #[test]
    fn source_fallback_without_analysis_uses_direct_write() {
        let temp = TestDir::new("fallback-direct-write");
        let input = temp.0.join("input.lua");
        let output = temp.0.join("output.lua");
        let hard_link = temp.0.join("output-hard-link.lua");
        let source = b"return 42";
        std::fs::write(&input, BASE64_STANDARD.encode(source)).unwrap();
        std::fs::write(&output, b"old").unwrap();
        std::fs::hard_link(&output, &hard_link).unwrap();
        let work = Work {
            input,
            output,
            output_root: temp.0.clone(),
            rel: "input.lua".to_string(),
            source_rel: "output.lua".to_string(),
            kind: WorkKind::SourceFallback,
            volt_export: None,
        };

        assert!(matches!(
            process_one(
                &work,
                1,
                &mut Vec::new(),
                false,
                DecompileOptions::default()
            ),
            Outcome::Ok
        ));
        assert_eq!(std::fs::read(&work.output).unwrap(), source);
        assert_eq!(std::fs::read(hard_link).unwrap(), source);
    }

    #[test]
    fn declared_status_and_counts_are_checked() {
        let temp = TestDir::new("status-and-counts");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let mut value = manifest(vec![]);
        value["status"] = json!("partial");
        value["counts"]["saved"] = json!(1);
        let manifest_path = src.join(".volt-export-manifest.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();

        let (_, _, work, inventory) =
            build_work_from_export_manifest(&src, &out, "lua", &manifest_path).unwrap();
        assert!(work.is_empty());
        assert_eq!(inventory.manifest_issue_count, 2);
        assert_eq!(
            inventory
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            vec!["manifest_count_mismatch", "manifest_status_mismatch"]
        );
    }

    #[test]
    fn export_manifest_rejects_oversized_file_before_allocation() {
        let temp = TestDir::new("oversized-export-manifest");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let manifest_path = src.join(".volt-export-manifest.json");
        let file = File::create(&manifest_path).unwrap();
        file.set_len(MAX_EXPORT_MANIFEST_BYTES + 1).unwrap();

        assert!(build_work_from_export_manifest(&src, &out, "lua", &manifest_path).is_err());
    }

    #[test]
    fn export_manifest_rejects_excessive_script_count_during_parse() {
        let temp = TestDir::new("excessive-export-script-count");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let manifest_path = src.join(".volt-export-manifest.json");
        let mut bytes = br#"{"schema":"volt-decompile-export-manifest","schema_version":1,"status":"complete","payload_encoding":"base64","counts":{"discovered":0,"saved":0,"failed":0,"raw_bytecode":0,"source_fallback":0,"extraction_failure":0},"scripts":["#.to_vec();
        for index in 0..=MAX_EXPORT_MANIFEST_SCRIPTS {
            if index != 0 {
                bytes.push(b',');
            }
            bytes.extend_from_slice(b"{}");
        }
        bytes.extend_from_slice(b"]}");
        assert!((bytes.len() as u64) < MAX_EXPORT_MANIFEST_BYTES);
        std::fs::write(&manifest_path, bytes).unwrap();

        assert!(build_work_from_export_manifest(&src, &out, "lua", &manifest_path).is_err());
    }

    #[test]
    fn export_manifest_rejects_excessive_declared_counts() {
        let temp = TestDir::new("excessive-export-declared-count");
        let src = temp.0.join("src");
        let out = temp.0.join("out");
        std::fs::create_dir_all(&src).unwrap();
        let mut value = manifest(vec![]);
        value["counts"]["discovered"] = json!(MAX_EXPORT_MANIFEST_SCRIPTS + 1);
        let manifest_path = src.join(".volt-export-manifest.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&value).unwrap()).unwrap();

        assert!(build_work_from_export_manifest(&src, &out, "lua", &manifest_path).is_err());
    }

    #[test]
    fn analysis_directory_symlink_escape_is_rejected_when_supported() {
        let temp = TestDir::new("analysis-symlink-escape");
        let out = temp.0.join("out");
        let outside = temp.0.join("outside");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = out.join(".tovek-analysis");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, &link).is_ok();
        if !linked {
            return;
        }
        let out_root = std::fs::canonicalize(&out).unwrap();
        assert!(prepare_analysis_root(&out_root).is_err());

        std::fs::remove_dir(&link).unwrap();
        std::fs::create_dir_all(&link).unwrap();
        let scripts_link = link.join("scripts");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &scripts_link).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &scripts_link).unwrap();
        assert!(prepare_analysis_root(&out_root).is_err());
    }
}
