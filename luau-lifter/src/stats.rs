//! `--stats-json`: what the de-inliners rebuilt and refused, per script
//! ([`ast::reconstruction_stats::Stats`]). Each completed decompilation adds
//! its script; [`write_json`] writes them all, sorted by name, when the
//! process finishes. Off by default: an unrequested run computes nothing.

use std::{
    io::{self, BufWriter, Write},
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use ast::reconstruction_stats::Stats;

struct Sink {
    path: PathBuf,
    /// The name of a script decompiled without one (the single-file CLI).
    unnamed: String,
    scripts: Mutex<Vec<(String, Stats)>>,
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// Collect stats for every script this process decompiles, for `path`.
pub fn enable(path: PathBuf, unnamed: String) {
    let _ = SINK.set(Sink { path, unnamed, scripts: Mutex::default() });
}

pub fn enabled() -> bool {
    SINK.get().is_some()
}

pub(crate) fn record(script: Option<&str>, stats: Stats) {
    if let Some(sink) = SINK.get() {
        let name = script.unwrap_or(&sink.unnamed).to_string();
        sink.scripts.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push((name, stats));
    }
}

/// Write the collected scripts. Call after all workers joined.
pub fn write_json() -> io::Result<()> {
    let Some(sink) = SINK.get() else { return Ok(()) };
    let mut scripts = std::mem::take(&mut *sink.scripts.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
    scripts.sort_by(|a, b| a.0.cmp(&b.0));
    #[derive(serde::Serialize)]
    struct Script<'a> {
        script: &'a str,
        #[serde(flatten)]
        stats: &'a Stats,
    }
    let export = serde_json::json!({
        "schema": "tovek-stats/2",
        "contract": "reconstructed_calls counts the calls the de-inliners rebuilt in the final tree, per printed copy; calls_by_helper lists them per helper binding with the bytecode prototype its function was lifted from (null when unknown) and its printed name; refused_helpers counts helpers the statement de-inliner refused as targets and never rebuilt a call of; refused_sites counts refused site attempts (one site may be tried more than once).",
        "scripts": scripts.iter().map(|(script, stats)| Script { script, stats }).collect::<Vec<_>>(),
    });
    let mut writer = BufWriter::new(std::fs::File::create(&sink.path)?);
    serde_json::to_writer_pretty(&mut writer, &export)?;
    writer.write_all(b"\n")?;
    writer.flush()
}
