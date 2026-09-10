//! Export optional pass diagnostics after all workers finish.
use serde::{ser::SerializeSeq, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::io::{self, BufWriter, Write};

pub(crate) fn context(
    script_name: Option<&str>,
    bytecode: &[u8],
) -> Option<ast::telemetry::Context> {
    ast::telemetry::enabled().then(|| ast::telemetry::Context {
        script: script_name
            .map(String::from)
            .unwrap_or_else(|| format!("<bytecode:{:x}>", Sha256::digest(bytecode)))
            .into(),
        prototype: None,
    })
}

struct Rows<'a>(&'a ast::telemetry::Report);
impl Serialize for Rows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.rows_len()))?;
        for row in self.0.rows() {
            sequence.serialize_element(&row)?;
        }
        sequence.end()
    }
}

/// Explicitly call after joins and before process::exit; dropping a guard at
/// main scope is insufficient for folder mode, which exits without unwinding.
pub fn write_json() -> io::Result<()> {
    if !ast::telemetry::enabled() {
        return Ok(());
    }
    let path = std::env::var_os("MEDAL_PROFILE_JSON").ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "MEDAL_PROFILE_JSON is missing")
    })?;
    let report = ast::telemetry::take_report();
    let binary = std::env::current_exe()?;
    let binary_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&binary)?));
    let metadata = serde_json::json!({
        "schema_version": 1, "model": "tovek-pass-thread-wall-v1",
        "binary": binary, "binary_sha256": binary_sha256, "version": env!("CARGO_PKG_VERSION"),
        "command": std::env::args().collect::<Vec<_>>(),
        "logical_processors": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "row_limit": ast::telemetry::ROW_LIMIT, "node_limit": ast::telemetry::NODE_LIMIT,
        "rows_count": report.rows_len(),
        "timing_contract": "Nanosecond wall intervals; exclusive subtracts nested spans on the same thread only. Rayon workers overlap, and waiting is included. Sums are not process wall or CPU time. Profiling/aggregation overhead remains outside child intervals and can be charged to ancestors.",
        "node_contract": "Bounded statement/rvalue census at explicitly measured AST phases. Includes indexed assignment operands, excludes binder/type syntax and implicit storage. Closure bodies counted once only when owned. node_samples=0 means unmeasured, not empty. Before/after census is outside the measured phase interval.",
        "counter_contract": "Counts belong to the innermost measured phase. Missing counters are unmeasured or inapplicable, not zero. No cache hit is claimed unless a specifically identified cache is used.",
    });
    #[derive(Serialize)]
    struct Export<'a> {
        #[serde(flatten)]
        metadata: serde_json::Value,
        #[serde(flatten)]
        diagnostics: &'a ast::telemetry::Report,
        rows: Rows<'a>,
    }
    let mut writer = BufWriter::new(std::fs::File::create(path)?);
    serde_json::to_writer(
        &mut writer,
        &Export {
            metadata,
            diagnostics: &report,
            rows: Rows(&report),
        },
    )?;
    writer.write_all(b"\n")?;
    writer.flush()
}
