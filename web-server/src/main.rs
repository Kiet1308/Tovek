//! HTTP decompiler server for the executor → server workflow.
//!
//! Routes:
//!   * `POST /decompile`        — one script, base64-encoded body (legacy; unchanged).
//!   * `POST /decompile/raw`    — one script, RAW bytecode body (no base64).
//!   * `POST /decompile/batch`  — many scripts in one request; JSON (base64) or the
//!                                binary `MDB1` framing (raw, no base64). JSON results.
//!
//! The single-script routes return the decompiled source as `text/plain`. The
//! batch route returns a JSON array of per-item results — one bad script never
//! fails the whole batch (that item carries `ok:false` + an `error`); only a
//! malformed request framing is an HTTP 4xx.
use std::io;
use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{header::CONTENT_TYPE, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use base64::prelude::*;
use luau_lifter::{
    decompile_batch_with_options as lib_decompile_batch_with_options, BatchInput, DecompileOptions,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tracing::info;

// Global allocator for the server binary. The decompiler is allocation-bound, so
// mimalloc's per-thread free-lists noticeably cut wall time (see Cargo.toml). It
// lives here in the binary, never in the shared library (which the wasm worker
// links and cannot build mimalloc against).
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

const BIND_ADDR: &str = "127.0.0.1:3000";

/// Default decode key (`op = op * key % 256`). 203 is Roblox client bytecode —
/// the only thing the executor → server workflow produces. Overridable per
/// request via the `x-encode-key` header (single routes) / the JSON `key` field /
/// the `MDB1` header key byte (batch).
const DEFAULT_KEY: u8 = 203;

// Per-route body limits (axum's global default is only 2 MiB, which a batch — or
// even one large module — would silently 413 against).
const RAW_BODY_LIMIT: usize = 16 * 1024 * 1024; // 16 MiB: one raw script.
const BATCH_BODY_LIMIT: usize = 64 * 1024 * 1024; // 64 MiB: one whole batch.

/// Cap concurrent batch decompiles so a few large simultaneous uploads can't
/// exhaust memory (each batch buffers its body + holds every result string).
const MAX_CONCURRENT_BATCHES: usize = 4;

// `MDB1` binary-batch framing limits. The body limit above transitively bounds
// total allocation; these are cheap early-outs and integrity checks.
const MDB1_MAGIC: &[u8; 4] = b"MDB1";
const MDB1_VERSION: u8 = 1;
const MDB1_FLAG_DONT_REUSE_VAR: u8 = luau_lifter::DONT_REUSE_VAR as u8;
const MDB1_SUPPORTED_FLAGS: u8 = MDB1_FLAG_DONT_REUSE_VAR;
const MAX_ENTRIES: usize = 50_000;
const MAX_NAME_LEN: usize = 4 * 1024; // 4 KiB — a GetFullName() path.
const MAX_CODE_LEN: usize = 16 * 1024 * 1024; // 16 MiB — one script's bytecode.

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("there was an IO error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid base64 data recieved: {0}")]
    Base64(#[from] base64::DecodeError),
    /// Malformed request framing (bad JSON / bad `MDB1` frame / bad header). This
    /// is distinct from a single script failing to decompile, which is reported
    /// per-item inside a 200 response.
    #[error("bad request: {0}")]
    BadRequest(String),
}
impl Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Error::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Base64(_) => StatusCode::BAD_REQUEST,
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
        }
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        Response::builder()
            .status(self.status_code())
            .body(Body::from(format!("{self}")))
            .expect("failed to build body")
    }
}

/// Shared server state.
#[derive(Clone)]
struct AppState {
    /// Bounds concurrent batch decompiles (see [`MAX_CONCURRENT_BATCHES`]).
    batch_semaphore: Arc<Semaphore>,
}

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    // One process-global quiet panic hook, installed before any decompile work, so
    // the per-function/per-item `catch_unwind`s used by the batch path don't spam
    // stderr and don't race a per-call set_hook across threads.
    luau_lifter::install_quiet_panic_hook();

    // Setup the logger
    let subscriber = tracing_subscriber::fmt()
        .compact()
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_target(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to set global tracing subscriber");

    let state = AppState {
        batch_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_BATCHES)),
    };

    // Build our application with the routes. Per-route `DefaultBodyLimit` layers
    // raise the 2 MiB default ONLY for the new routes; `/decompile` is untouched.
    let app = Router::new()
        .route("/decompile", post(decompile))
        .route(
            "/decompile/raw",
            post(decompile_raw).layer(DefaultBodyLimit::max(RAW_BODY_LIMIT)),
        )
        .route(
            "/decompile/batch",
            post(decompile_batch).layer(DefaultBodyLimit::max(BATCH_BODY_LIMIT)),
        )
        .with_state(state);

    // Run the web server
    let listener = TcpListener::bind(BIND_ADDR).await?;
    info!("🚀 Listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await
}

/// `POST /decompile` — one script, base64-encoded body. UNCHANGED legacy path.
async fn decompile(headers: HeaderMap, body: Bytes) -> Result<String, Error> {
    let mut bytecode = Vec::new();
    BASE64_STANDARD.decode_vec(body, &mut bytecode)?;
    let script_name = headers
        .get("x-script-name")
        .and_then(|value| value.to_str().ok());
    let options = parse_options_headers(&headers)?;
    let decompiled =
        luau_lifter::decompile_bytecode_with_options(&bytecode, 203, script_name, options);
    info!("Successfully decompiled bytecode.");
    Ok(decompiled)
}

/// `POST /decompile/raw` — one script, RAW bytecode body (no base64). The script
/// name comes from `x-script-name`; an optional `x-encode-key` overrides the key.
async fn decompile_raw(headers: HeaderMap, body: Bytes) -> Result<String, Error> {
    let script_name = header_string(&headers, "x-script-name");
    let key = parse_key_header(&headers)?;
    let options = parse_options_headers(&headers)?;
    // `Bytes` is already `'static + Send`; move it straight into the blocking task
    // (it derefs to `&[u8]`) so there's no extra copy of the bytecode. Route the
    // single item through `decompile_batch` so a deserialize error AND a lifter
    // panic both come back as `Err` (a clean 400) rather than a 500 — same
    // isolation the batch path gets.
    let result = run_blocking(move || {
        let inputs = [BatchInput {
            bytecode: &body[..],
            encode_key: key,
            script_name: script_name.as_deref(),
        }];
        lib_decompile_batch_with_options(&inputs, options)
            .pop()
            .expect("one input yields exactly one result")
    })
    .await?;
    // A decompile/deserialize failure on the single route has no per-item channel,
    // so surface it as a 400 with the reason (matches the existing client, which
    // turns any >=400 into a `-- decompile failed` comment).
    let decompiled = result.map_err(Error::BadRequest)?;
    info!("Successfully decompiled raw bytecode.");
    Ok(decompiled)
}

/// `POST /decompile/batch` — many scripts in one request.
///
/// `Content-Type: application/json` → JSON batch (base64 bytecode); anything else
/// (e.g. `application/octet-stream`) → the binary `MDB1` framing (raw bytecode).
/// Always responds 200 with a JSON results array; a malformed request framing is
/// the only 4xx.
async fn decompile_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    // Parse into per-item work. A framing/schema error is a whole-request 400; a
    // bad single item (e.g. un-decodable base64) becomes a deferred per-item error.
    let items = parse_batch_request(&headers, &body)?;

    // Bound concurrent batches to cap peak memory. The permit is held across the
    // blocking decompile below.
    let _permit = Arc::clone(&state.batch_semaphore)
        .acquire_owned()
        .await
        .map_err(|_| Error::Io(io::Error::new(io::ErrorKind::Other, "semaphore closed")))?;

    let results = run_blocking(move || decompile_parsed_batch(items)).await?;
    let ok_count = results.iter().filter(|r| r.ok).count();
    info!(
        "Batch decompiled {} scripts ({ok_count} ok).",
        results.len()
    );
    let response = BatchResponse {
        count: results.len(),
        ok_count,
        results,
    };
    Ok(Json(response).into_response())
}

// ---------------------------------------------------------------------------
// Batch request parsing
// ---------------------------------------------------------------------------

/// One parsed batch item: either ready to decompile, or already failed at parse
/// time (e.g. un-decodable base64) — kept so the result stays index-aligned.
enum ParsedItem {
    Ready {
        bytecode: Vec<u8>,
        key: u8,
        options: DecompileOptions,
        id: Option<String>,
        script_name: Option<String>,
    },
    Failed {
        id: Option<String>,
        script_name: Option<String>,
        error: String,
    },
}

#[derive(Deserialize)]
struct JsonBatchRequest {
    /// Decode key applied to every script (default [`DEFAULT_KEY`]).
    #[serde(default)]
    key: Option<u8>,
    /// Optional decompiler flags. Currently supports `DONT_REUSE_VAR`.
    #[serde(default)]
    flags: Option<String>,
    #[serde(default, alias = "dontReuseVar")]
    dont_reuse_var: Option<bool>,
    scripts: Vec<JsonBatchItem>,
}

#[derive(Deserialize)]
struct JsonBatchItem {
    /// Client-chosen correlation token, echoed back verbatim.
    #[serde(default)]
    id: Option<String>,
    #[serde(default, alias = "scriptName")]
    script_name: Option<String>,
    /// base64-encoded bytecode.
    bytecode: String,
}

#[derive(Serialize)]
struct BatchResponse {
    count: usize,
    ok_count: usize,
    results: Vec<BatchResultItem>,
}

#[derive(Serialize)]
struct BatchResultItem {
    /// Zero-based position in the request — the universal correlation key.
    index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    script_name: Option<String>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    decompilation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn parse_batch_request(headers: &HeaderMap, body: &Bytes) -> Result<Vec<ParsedItem>, Error> {
    let header_options = parse_options_headers(headers)?;
    if is_json_content_type(headers) {
        parse_json_batch(body, header_options)
    } else {
        parse_mdb1_batch(body, header_options)
    }
}

/// Essence-based, parameter-tolerant `application/json` detection (so
/// `application/json; charset=utf-8` still routes to the JSON parser).
fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false)
}

fn parse_json_batch(
    body: &Bytes,
    header_options: DecompileOptions,
) -> Result<Vec<ParsedItem>, Error> {
    let req: JsonBatchRequest = serde_json::from_slice(body)
        .map_err(|e| Error::BadRequest(format!("invalid JSON batch: {e}")))?;
    if req.scripts.len() > MAX_ENTRIES {
        return Err(Error::BadRequest(format!(
            "too many scripts: {} (max {MAX_ENTRIES})",
            req.scripts.len()
        )));
    }
    let key = req.key.unwrap_or(DEFAULT_KEY);
    let body_options = parse_json_options(req.flags.as_deref(), req.dont_reuse_var)?;
    let options = header_options.union(body_options);
    let mut out = Vec::with_capacity(req.scripts.len().min(1024));
    for item in req.scripts {
        // A bad base64 payload is bad *data* for one script, not a malformed
        // request — defer it as a per-item failure so it can't sink the batch.
        match BASE64_STANDARD.decode(item.bytecode.as_bytes()) {
            Ok(bytecode) => out.push(ParsedItem::Ready {
                bytecode,
                key,
                options,
                id: item.id,
                script_name: item.script_name,
            }),
            Err(e) => out.push(ParsedItem::Failed {
                id: item.id,
                script_name: item.script_name,
                error: format!("base64: {e}"),
            }),
        }
    }
    Ok(out)
}

/// Parse the binary `MDB1` raw-batch framing. Every length is bounds-checked
/// against the remaining buffer before slicing, so a hostile/truncated body can
/// never panic or read out of bounds.
///
/// Layout (little-endian):
///   header: `MDB1`(4) | version u8 | key u8 | flags u8 | reserved u8(=0) | count u32
///   entry × count: name_len u32 | name bytes | code_len u32 | code bytes
fn parse_mdb1_batch(
    body: &[u8],
    header_options: DecompileOptions,
) -> Result<Vec<ParsedItem>, Error> {
    let mut pos = 0usize;

    let header = take(body, &mut pos, 12)
        .ok_or_else(|| Error::BadRequest("MDB1: truncated header".into()))?;
    if &header[0..4] != MDB1_MAGIC {
        return Err(Error::BadRequest(
            "MDB1: bad magic (expected an MDB1 batch body; send Content-Type: application/json for a JSON batch)".into(),
        ));
    }
    let version = header[4];
    if version != MDB1_VERSION {
        return Err(Error::BadRequest(format!(
            "MDB1: unsupported version {version} (this server speaks {MDB1_VERSION})"
        )));
    }
    let key = header[5];
    let flags = header[6];
    let reserved = header[7];
    if flags & !MDB1_SUPPORTED_FLAGS != 0 {
        return Err(Error::BadRequest(format!(
            "MDB1: unsupported flags byte 0x{flags:02X}"
        )));
    }
    if reserved != 0 {
        return Err(Error::BadRequest(
            "MDB1: reserved byte must be zero in v1".into(),
        ));
    }
    let mdb1_options =
        DecompileOptions::from_flag_bits(flags as u32).expect("unsupported MDB1 flags rejected");
    let options = header_options.union(mdb1_options);
    let count = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
    if count > MAX_ENTRIES {
        return Err(Error::BadRequest(format!(
            "MDB1: too many entries {count} (max {MAX_ENTRIES})"
        )));
    }

    // `count` is attacker-influenced; use it only as a capped capacity hint, and
    // verify it against what we actually parse below.
    let mut out = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let name_len = read_u32(body, &mut pos)
            .ok_or_else(|| Error::BadRequest("MDB1: truncated (name length)".into()))?
            as usize;
        if name_len > MAX_NAME_LEN {
            return Err(Error::BadRequest(format!(
                "MDB1: name too large {name_len} (max {MAX_NAME_LEN})"
            )));
        }
        let name = take(body, &mut pos, name_len)
            .ok_or_else(|| Error::BadRequest("MDB1: truncated (name)".into()))?;

        let code_len = read_u32(body, &mut pos)
            .ok_or_else(|| Error::BadRequest("MDB1: truncated (code length)".into()))?
            as usize;
        if code_len > MAX_CODE_LEN {
            return Err(Error::BadRequest(format!(
                "MDB1: code too large {code_len} (max {MAX_CODE_LEN})"
            )));
        }
        let code = take(body, &mut pos, code_len)
            .ok_or_else(|| Error::BadRequest("MDB1: truncated (code)".into()))?;

        // A non-UTF-8 or empty name degrades to "no name" (matches the header path).
        let script_name = std::str::from_utf8(name)
            .ok()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        out.push(ParsedItem::Ready {
            bytecode: code.to_vec(),
            key,
            options,
            id: None,
            script_name,
        });
    }

    // A well-formed body ends exactly after the last declared entry.
    if pos != body.len() {
        return Err(Error::BadRequest(format!(
            "MDB1: {} trailing byte(s) after {count} entries",
            body.len() - pos
        )));
    }
    Ok(out)
}

/// Read a little-endian u32, advancing `pos`. `None` if fewer than 4 bytes remain.
fn read_u32(buf: &[u8], pos: &mut usize) -> Option<u32> {
    let s = take(buf, pos, 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Borrow `n` bytes from `buf` at `*pos`, advancing `pos`. `None` (never a panic)
/// if `n` would run past the end; `checked_add` rules out length overflow.
fn take<'a>(buf: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
    let end = pos.checked_add(n)?;
    if end > buf.len() {
        return None;
    }
    let slice = &buf[*pos..end];
    *pos = end;
    Some(slice)
}

// ---------------------------------------------------------------------------
// Batch decompilation
// ---------------------------------------------------------------------------

/// Decompile the ready items in parallel (via the library's deterministic,
/// order-preserving batch path) and weave the already-failed items
/// back in, producing one index-aligned result per input.
fn decompile_parsed_batch(items: Vec<ParsedItem>) -> Vec<BatchResultItem> {
    let n = items.len();
    let mut results: Vec<Option<BatchResultItem>> = (0..n).map(|_| None).collect();

    // Owned storage for the ready items, so we can borrow `&[u8]` / `&str` into
    // `BatchInput` for the library call.
    struct Ready {
        idx: usize,
        bytecode: Vec<u8>,
        key: u8,
        options: DecompileOptions,
        id: Option<String>,
        script_name: Option<String>,
    }
    let mut ready: Vec<Ready> = Vec::new();

    for (idx, item) in items.into_iter().enumerate() {
        match item {
            ParsedItem::Failed {
                id,
                script_name,
                error,
            } => {
                results[idx] = Some(BatchResultItem {
                    index: idx,
                    id,
                    script_name,
                    ok: false,
                    decompilation: None,
                    error: Some(error),
                });
            }
            ParsedItem::Ready {
                bytecode,
                key,
                options,
                id,
                script_name,
            } => ready.push(Ready {
                idx,
                bytecode,
                key,
                options,
                id,
                script_name,
            }),
        }
    }

    // Scope `inputs` so its borrow of `ready` ends before we move out of `ready`.
    let outcomes = {
        let options = ready
            .first()
            .map(|r| r.options)
            .unwrap_or_else(DecompileOptions::default);
        let inputs: Vec<BatchInput> = ready
            .iter()
            .map(|r| BatchInput {
                bytecode: &r.bytecode,
                encode_key: r.key,
                script_name: r.script_name.as_deref(),
            })
            .collect();
        lib_decompile_batch_with_options(&inputs, options)
    };

    for (r, outcome) in ready.into_iter().zip(outcomes) {
        let idx = r.idx;
        results[idx] = Some(match outcome {
            Ok(source) => BatchResultItem {
                index: idx,
                id: r.id,
                script_name: r.script_name,
                ok: true,
                decompilation: Some(source),
                error: None,
            },
            Err(reason) => BatchResultItem {
                index: idx,
                id: r.id,
                script_name: r.script_name,
                ok: false,
                decompilation: None,
                error: Some(reason),
            },
        });
    }

    // Every slot was filled (failed at parse, or decompiled above).
    results.into_iter().map(Option::unwrap).collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run CPU-bound decompile work on tokio's blocking pool so it never stalls an
/// async worker. A panic in `f` surfaces as a 500.
async fn run_blocking<F, T>(f: F) -> Result<T, Error>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|join_err| {
        Error::Io(io::Error::new(
            io::ErrorKind::Other,
            format!("decompile task failed: {join_err}"),
        ))
    })
}

/// Owned copy of a request header value, if present and valid UTF-8.
fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn parse_options_headers(headers: &HeaderMap) -> Result<DecompileOptions, Error> {
    let mut options = DecompileOptions::default();
    if let Some(value) = headers.get("x-decompile-flags") {
        let value = value
            .to_str()
            .map_err(|_| Error::BadRequest("x-decompile-flags must be valid UTF-8".into()))?;
        options = options.union(parse_flags_text(value)?);
    }
    if let Some(value) = headers.get("x-dont-reuse-var") {
        let value = value
            .to_str()
            .map_err(|_| Error::BadRequest("x-dont-reuse-var must be valid UTF-8".into()))?;
        if parse_bool(value, "x-dont-reuse-var")? {
            options.dont_reuse_var = true;
        }
    }
    Ok(options)
}

fn parse_json_options(
    flags: Option<&str>,
    dont_reuse_var: Option<bool>,
) -> Result<DecompileOptions, Error> {
    let mut options = match flags {
        Some(flags) => parse_flags_text(flags)?,
        None => DecompileOptions::default(),
    };
    if dont_reuse_var.unwrap_or(false) {
        options.dont_reuse_var = true;
    }
    Ok(options)
}

fn parse_flags_text(raw: &str) -> Result<DecompileOptions, Error> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(DecompileOptions::default());
    }
    if let Ok(bits) = raw.parse::<u32>() {
        return DecompileOptions::from_flag_bits(bits)
            .ok_or_else(|| Error::BadRequest(format!("unsupported decompile flag bits: {bits}")));
    }

    let mut options = DecompileOptions::default();
    for token in raw
        .split(|c: char| c == ',' || c == '|' || c == ';' || c.is_ascii_whitespace())
        .filter(|token| !token.is_empty())
    {
        let normalized = token.trim().replace('-', "_").to_ascii_uppercase();
        match normalized.as_str() {
            "NONE" => {}
            "DONT_REUSE_VAR" => options.dont_reuse_var = true,
            _ => {
                return Err(Error::BadRequest(format!(
                    "unsupported decompile flag: {token}"
                )));
            }
        }
    }
    Ok(options)
}

fn parse_bool(raw: &str, field: &str) -> Result<bool, Error> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(Error::BadRequest(format!(
            "{field} must be a boolean (true/false)"
        ))),
    }
}

/// Parse the optional `x-encode-key` header as a `u8`, defaulting to [`DEFAULT_KEY`].
fn parse_key_header(headers: &HeaderMap) -> Result<u8, Error> {
    match headers.get("x-encode-key") {
        None => Ok(DEFAULT_KEY),
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
            .ok_or_else(|| Error::BadRequest("x-encode-key must be an integer 0..=255".into())),
    }
}
