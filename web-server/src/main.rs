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
use std::time::Duration;

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, FromRequest, State},
    http::{header::CONTENT_TYPE, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
    Extension, Json, Router,
};
use base64::prelude::*;
use http_body::Body as _;
use luau_lifter::{
    decompile_batch_with_options as lib_decompile_batch_with_options, BatchInput, DecompileOptions,
    STRICT_NO_SYNTHETIC_CONTROL,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
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
const LEGACY_BODY_LIMIT: usize = 2 * 1024 * 1024; // Axum's unchanged default.
const REJECT_BODY_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_UPLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const UPLOAD_TIMEOUT_ENV: &str = "TOVEK_UPLOAD_TIMEOUT_SECS";

/// Cap CPU jobs across every route before buffering their bodies. Single
/// requests share capacity with batches instead of bypassing backpressure.
const MAX_CONCURRENT_JOBS: usize = 4;

// `MDB1` binary-batch framing limits. The body limit above transitively bounds
// total allocation; these are cheap early-outs and integrity checks.
const MDB1_MAGIC: &[u8; 4] = b"MDB1";
const MDB1_VERSION: u8 = 1;
const MDB1_FLAG_DONT_REUSE_VAR: u8 = luau_lifter::DONT_REUSE_VAR as u8;
const MDB1_FLAG_STRICT_NO_SYNTHETIC_CONTROL: u8 = STRICT_NO_SYNTHETIC_CONTROL as u8;
const MDB1_SUPPORTED_FLAGS: u8 =
    MDB1_FLAG_DONT_REUSE_VAR | MDB1_FLAG_STRICT_NO_SYNTHETIC_CONTROL;
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
    /// Shared by legacy, raw and batch CPU jobs.
    cpu_semaphore: Arc<Semaphore>,
    /// One deadline for the complete admitted upload, not for CPU execution.
    upload_timeout: Duration,
}

fn parse_upload_timeout(value: &str) -> io::Result<Duration> {
    value.parse::<u32>().ok().filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(u64::from(seconds)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
            format!("{UPLOAD_TIMEOUT_ENV} must be a positive 32-bit integer number of seconds")))
}

/// Use Axum's existing bounded Bytes extraction exactly once. Only body
/// ingestion is timed: a completed upload may start CPU work that must keep
/// its permit even after this deadline or cancellation of the HTTP future.
struct AdmittedBody(Bytes);

#[axum::async_trait]
impl FromRequest<AppState> for AdmittedBody {
    type Rejection = Response;

    async fn from_request(
        request: axum::extract::Request,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match tokio::time::timeout(state.upload_timeout, Bytes::from_request(request, state)).await {
            Ok(Ok(bytes)) => Ok(Self(bytes)),
            Ok(Err(rejection)) => Err(rejection.into_response()),
            Err(_) => Err((
                StatusCode::REQUEST_TIMEOUT,
                [(axum::http::header::CONNECTION, "close")],
                "request body upload timed out",
            ).into_response()),
        }
    }
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

    let upload_timeout = match std::env::var(UPLOAD_TIMEOUT_ENV) {
        Ok(value) => parse_upload_timeout(&value)?,
        Err(std::env::VarError::NotPresent) => DEFAULT_UPLOAD_TIMEOUT,
        Err(_) => return Err(io::Error::new(io::ErrorKind::InvalidInput,
            format!("{UPLOAD_TIMEOUT_ENV} must contain a valid integer"))),
    };
    let state = AppState {
        cpu_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_JOBS)),
        upload_timeout,
    };

    let app = app(state);

    // Run the web server
    let listener = TcpListener::bind(BIND_ADDR).await?;
    info!("🚀 Listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await
}

fn app(state: AppState) -> Router {
    // Build our application with the routes. Per-route `DefaultBodyLimit` layers
    // raise the 2 MiB default ONLY for the new routes; `/decompile` is untouched.
    Router::new()
        .route("/decompile", post(decompile))
        .route(
            "/decompile/raw",
            post(decompile_raw).layer(DefaultBodyLimit::max(RAW_BODY_LIMIT)),
        )
        .route(
            "/decompile/batch",
            post(decompile_batch)
                .layer(DefaultBodyLimit::max(BATCH_BODY_LIMIT)),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), admit_work))
        .with_state(state)
}

/// Reserve capacity before AdmittedBody buffers the body with an upload deadline. On HTTP/1,
/// dropping an unread upload can reset the socket before the client sees 503.
/// Discard rejected uploads one frame at a time, within route/time limits.
async fn admit_work(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Ok(permit) = Arc::clone(&state.cpu_semaphore).try_acquire_owned() else {
        let limit = match request.uri().path() {
            "/decompile/raw" => RAW_BODY_LIMIT,
            "/decompile/batch" => BATCH_BODY_LIMIT,
            _ => LEGACY_BODY_LIMIT,
        };
        if let Err(status) = discard_rejected_body(request.into_body(), limit, REJECT_BODY_TIMEOUT).await {
            // An incomplete/oversized upload cannot be reused as another HTTP
            // request. These failures are distinct from a completed rejection.
            return (status, [(axum::http::header::CONNECTION, "close")]).into_response();
        }
        return (StatusCode::SERVICE_UNAVAILABLE, "decompile capacity exhausted").into_response();
    };
    let permit = Arc::new(permit);
    request.extensions_mut().insert(permit.clone());
    next.run(request).await
}

async fn discard_rejected_body(mut body: Body, limit: usize, timeout: Duration) -> Result<(), StatusCode> {
    tokio::time::timeout(timeout, async {
        let mut remaining = limit;
        while let Some(frame) = std::future::poll_fn(|cx| {
            std::pin::Pin::new(&mut body).poll_frame(cx)
        }).await {
            let frame = frame.map_err(|_| StatusCode::BAD_REQUEST)?;
            if let Ok(data) = frame.into_data() {
                remaining = remaining.checked_sub(data.len()).ok_or(StatusCode::PAYLOAD_TOO_LARGE)?;
            }
        }
        Ok(())
    }).await.map_err(|_| StatusCode::REQUEST_TIMEOUT)?
}

/// `POST /decompile` — retain the legacy base64 protocol and decode key.
async fn decompile(
    Extension(permit): Extension<Arc<OwnedSemaphorePermit>>,
    headers: HeaderMap,
    AdmittedBody(body): AdmittedBody,
) -> Result<String, Error> {
    run_admitted_work(permit, move || {
        let bytecode = BASE64_STANDARD.decode(body)?;
        let script_name = headers.get("x-script-name").and_then(|value| value.to_str().ok());
        let options = parse_options_headers(&headers)?;
        let source = luau_lifter::try_decompile_bytecode_with_options(&bytecode, 203, script_name, options)
            .map_err(Error::BadRequest)?;
        info!("Successfully decompiled bytecode.");
        Ok(source)
    }).await?
}

/// `POST /decompile/raw` — one script, RAW bytecode body (no base64). The script
/// name comes from `x-script-name`; an optional `x-encode-key` overrides the key.
async fn decompile_raw(
    Extension(permit): Extension<Arc<OwnedSemaphorePermit>>,
    headers: HeaderMap,
    AdmittedBody(body): AdmittedBody,
) -> Result<String, Error> {
    let script_name = header_string(&headers, "x-script-name");
    let key = parse_key_header(&headers)?;
    let options = parse_options_headers(&headers)?;
    // `Bytes` is already `'static + Send`; move it straight into the blocking task
    // (it derefs to `&[u8]`) so there's no extra copy of the bytecode. The try
    // API catches parsing, lifting and formatting panics as per-item errors.
    let result = run_admitted_work(permit, move || {
        luau_lifter::try_decompile_bytecode_with_options(&body, key, script_name.as_deref(), options)
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
/// Admitted batches respond 200 with a JSON results array; malformed framing
/// returns 4xx and the admission middleware returns 503 when capacity is full.
async fn decompile_batch(
    Extension(permit): Extension<Arc<OwnedSemaphorePermit>>,
    headers: HeaderMap,
    AdmittedBody(body): AdmittedBody,
) -> Result<Response, Error> {
    run_admitted_work(permit, move || {
        // Parsing/base64 and JSON response encoding can be large CPU jobs too;
        // keep the complete job off Tokio's asynchronous executor.
        let items = parse_batch_request(&headers, &body)?;
        let results = decompile_parsed_batch(items);
        let ok_count = results.iter().filter(|r| r.ok).count();
        info!("Batch decompiled {} scripts ({ok_count} ok).", results.len());
        let response = BatchResponse { count: results.len(), ok_count, results };
        Ok(Json(response).into_response())
    }).await?
}

async fn run_admitted_work<F, T>(permit: Arc<OwnedSemaphorePermit>, work: F) -> Result<T, Error>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_blocking(move || {
        // spawn_blocking continues even if the HTTP future is cancelled.
        // The worker must retain admission until it releases its input/work.
        let _permit = permit;
        work()
    }).await
}

// ---------------------------------------------------------------------------
// Batch request parsing
// ---------------------------------------------------------------------------

/// One parsed batch item: either ready to decompile, or already failed at parse
/// time (e.g. un-decodable base64) — kept so the result stays index-aligned.
enum ParsedItem {
    Ready {
        bytecode: Bytes,
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
    /// Optional decompiler flags. Supports `DONT_REUSE_VAR` and
    /// `STRICT_NO_SYNTHETIC_CONTROL`.
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
                bytecode: bytecode.into(),
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
    body: &Bytes,
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
        let code_start = pos;
        take(body, &mut pos, code_len)
            .ok_or_else(|| Error::BadRequest("MDB1: truncated (code)".into()))?;

        // A non-UTF-8 or empty name degrades to "no name" (matches the header path).
        let script_name = std::str::from_utf8(name)
            .ok()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        out.push(ParsedItem::Ready {
            bytecode: body.slice(code_start..pos),
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
        bytecode: Bytes,
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
            "STRICT_NO_SYNTHETIC_CONTROL" => {
                options.control_flow_policy =
                    luau_lifter::ControlFlowOutputPolicy::StrictNoSyntheticControl;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use luau_lifter::{ControlFlowOutputPolicy, STRICT_NO_SYNTHETIC_CONTROL};
    use tower::ServiceExt;

    // v9 compiler layout for `return 7`, with Roblox's opcode decode key.
    pub(super) fn bytecode() -> Vec<u8> {
        let mut bytes = vec![9, 1, 0, 1, 1, 0, 0, 0, 0, 0, 2];
        bytes.extend([4u8.wrapping_mul(227), 0, 7, 0]);
        bytes.extend([22u8.wrapping_mul(227), 0, 2, 0]);
        bytes.extend([0; 7]);
        bytes
    }

    fn mdb1(codes: &[&[u8]]) -> Bytes {
        let mut bytes = b"MDB1".to_vec();
        bytes.extend([1, DEFAULT_KEY, 0, 0]);
        bytes.extend((codes.len() as u32).to_le_bytes());
        for code in codes {
            bytes.extend(6u32.to_le_bytes());
            bytes.extend(b"Widget");
            bytes.extend((code.len() as u32).to_le_bytes());
            bytes.extend(*code);
        }
        bytes.into()
    }

    #[test]
    fn binary_batch_borrows_code_and_rejects_truncation_or_trailing_bytes() {
        let good = bytecode();
        let body = mdb1(&[&good, &[99, 0, 0]]);
        let items = parse_mdb1_batch(&body, DecompileOptions::default()).unwrap();
        for (item, expected) in items.iter().zip([good.as_slice(), &[99, 0, 0]]) {
            let ParsedItem::Ready { bytecode, script_name, .. } = item else { panic!("ready"); };
            assert_eq!(&bytecode[..], expected);
            assert_eq!(script_name.as_deref(), Some("Widget"));
            let offset = bytecode.as_ptr() as usize - body.as_ptr() as usize;
            assert!(offset < body.len() && offset + bytecode.len() <= body.len());
        }
        for end in 0..body.len() {
            assert!(parse_mdb1_batch(&body.slice(..end), DecompileOptions::default()).is_err());
        }
        let mut trailing = body.to_vec();
        trailing.push(0);
        assert!(parse_mdb1_batch(&trailing.into(), DecompileOptions::default()).is_err());
        drop(body);
        let results = decompile_parsed_batch(items);
        assert!(results[0].ok);
        assert!(!results[1].ok);
    }

    #[tokio::test]
    async fn transports_preserve_source_status_item_order_and_permit_release() {
        let state = AppState {
            cpu_semaphore: Arc::new(Semaphore::new(1)),
            upload_timeout: DEFAULT_UPLOAD_TIMEOUT,
        };
        let good = bytecode();
        let expected = luau_lifter::try_decompile_bytecode_with_options(
            &good, DEFAULT_KEY, Some("Widget"), DecompileOptions::default()).unwrap();
        let encoded = BASE64_STANDARD.encode(&good);
        let json = serde_json::json!({"scripts": [
            {"id":"bad-base64", "bytecode":"!"},
            {"id":"good", "bytecode":encoded, "script_name":"Widget"},
            {"id":"bad-code", "bytecode":BASE64_STANDARD.encode([99])}
        ]});
        for (path, content_type, body, batch) in [
            ("/decompile", "text/plain", Bytes::from(encoded), false),
            ("/decompile/raw", "application/octet-stream", Bytes::from(good.clone()), false),
            ("/decompile/batch", "application/json; charset=utf-8", Bytes::from(json.to_string()), true),
            ("/decompile/batch", "application/octet-stream", mdb1(&[&[99], &good, &[99]]), true),
        ] {
            let request = axum::http::Request::post(path).header(CONTENT_TYPE, content_type)
                .header("x-script-name", "Widget").body(Body::from(body)).unwrap();
            let response = app(state.clone()).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            if batch {
                let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(response["count"], 3);
                assert_eq!(response["ok_count"], 1);
                for (i, item) in response["results"].as_array().unwrap().iter().enumerate() {
                    assert_eq!(item["index"], i);
                    assert_eq!(item["ok"], i == 1);
                }
                assert_eq!(response["results"][1]["decompilation"], expected);
            } else {
                assert_eq!(&body[..], expected.as_bytes());
            }
            assert_eq!(state.cpu_semaphore.available_permits(), 1);
        }
        for (path, body) in [("/decompile", b"!".as_slice()),
            ("/decompile/raw", &[99]), ("/decompile/batch", b"MDB1")] {
            let response = app(state.clone()).oneshot(axum::http::Request::post(path)
                .body(Body::from(body)).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(state.cpu_semaphore.available_permits(), 1);
        }
    }

    #[test]
    fn web_flags_accept_strict_control_policy_by_name_and_bits() {
        let named = parse_flags_text("strict-no-synthetic-control").unwrap();
        assert_eq!(
            named.control_flow_policy,
            ControlFlowOutputPolicy::StrictNoSyntheticControl
        );
        let numeric = parse_flags_text(&STRICT_NO_SYNTHETIC_CONTROL.to_string()).unwrap();
        assert_eq!(numeric, named);
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use std::{
        convert::Infallible,
        pin::Pin,
        task::{Context, Poll},
    };
    use tower::ServiceExt;

    struct CountedBody {
        remaining: usize,
        polled: Arc<std::sync::atomic::AtomicUsize>,
    }

    const ROUTES: [&str; 3] = ["/decompile", "/decompile/raw", "/decompile/batch"];

    fn valid_body(path: &str) -> Bytes {
        match path {
            "/decompile" => BASE64_STANDARD.encode(super::tests::bytecode()).into(),
            "/decompile/raw" => super::tests::bytecode().into(),
            "/decompile/batch" => Bytes::from_static(br#"{"scripts":[]}"#),
            _ => unreachable!(),
        }
    }

    struct UploadBody {
        frames: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
        polled: Arc<std::sync::atomic::AtomicUsize>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Drop for UploadBody {
        fn drop(&mut self) {
            self.dropped.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl http_body::Body for UploadBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
            -> Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
            self.polled.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.frames.poll_recv(cx).map(|frame| frame.map(|bytes| Ok(http_body::Frame::data(bytes))))
        }
    }

    #[test]
    fn upload_timeout_configuration_rejects_disabled_or_invalid_deadlines() {
        assert_eq!(parse_upload_timeout("1").unwrap(), Duration::from_secs(1));
        assert_eq!(parse_upload_timeout("120").unwrap(), Duration::from_secs(120));
        for invalid in ["", "0", "-1", "1.5", "never", "4294967296"] {
            assert_eq!(parse_upload_timeout(invalid).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_and_trickled_admitted_bodies_have_one_deadline_on_every_route() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        for path in ROUTES {
            for trickle in [false, true] {
                let state = AppState {
                    cpu_semaphore: Arc::new(Semaphore::new(1)),
                    upload_timeout: Duration::from_secs(10),
                };
                let (frames, receiver) = tokio::sync::mpsc::unbounded_channel();
                let polled = Arc::new(AtomicUsize::new(0));
                let dropped = Arc::new(AtomicBool::new(false));
                let body = UploadBody { frames: receiver, polled: polled.clone(), dropped: dropped.clone() };
                let request = axum::http::Request::post(path)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::new(body)).unwrap();
                let response = tokio::spawn(app(state.clone()).oneshot(request));
                while polled.load(Ordering::SeqCst) == 0 { tokio::task::yield_now().await; }
                assert_eq!(state.cpu_semaphore.available_permits(), 0);

                for _ in 0..2 {
                    tokio::time::advance(Duration::from_secs(4)).await;
                    if trickle {
                        let previous_polls = polled.load(Ordering::SeqCst);
                        frames.send(Bytes::from_static(b"x")).unwrap();
                        while polled.load(Ordering::SeqCst) == previous_polls {
                            tokio::task::yield_now().await;
                        }
                    }
                    assert!(!response.is_finished());
                    assert_eq!(state.cpu_semaphore.available_permits(), 0);
                }
                tokio::time::advance(Duration::from_secs(2)).await;
                let response = response.await.unwrap().unwrap();
                assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
                assert_eq!(response.headers()[axum::http::header::CONNECTION], "close");
                assert!(dropped.load(Ordering::SeqCst));
                assert_eq!(state.cpu_semaphore.available_permits(), 1);
                drop(frames);

                // A completed upload on the same route is admitted immediately.
                tokio::time::resume();
                let request = axum::http::Request::post(path).header(CONTENT_TYPE, "application/json")
                    .body(Body::from(valid_body(path))).unwrap();
                assert_eq!(app(state.clone()).oneshot(request).await.unwrap().status(), StatusCode::OK);
                assert_eq!(state.cpu_semaphore.available_permits(), 1);
                tokio::time::pause();
            }
        }
    }

    #[tokio::test]
    async fn cancelled_admitted_upload_drops_its_body_and_permit() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let state = AppState {
            cpu_semaphore: Arc::new(Semaphore::new(1)),
            upload_timeout: DEFAULT_UPLOAD_TIMEOUT,
        };
        let (_frames, receiver) = tokio::sync::mpsc::unbounded_channel();
        let polled = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let request = axum::http::Request::post("/decompile/raw").body(Body::new(UploadBody {
            frames: receiver, polled: polled.clone(), dropped: dropped.clone(),
        })).unwrap();
        let response = tokio::spawn(app(state.clone()).oneshot(request));
        while polled.load(Ordering::SeqCst) == 0 { tokio::task::yield_now().await; }
        response.abort();
        assert!(response.await.unwrap_err().is_cancelled());
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(state.cpu_semaphore.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn multi_frame_uploads_completed_before_the_deadline_still_succeed() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        for path in ROUTES {
            let state = AppState {
                cpu_semaphore: Arc::new(Semaphore::new(1)),
                upload_timeout: Duration::from_secs(10),
            };
            let (frames, receiver) = tokio::sync::mpsc::unbounded_channel();
            let polled = Arc::new(AtomicUsize::new(0));
            let body = valid_body(path);
            frames.send(body.slice(..body.len() / 2)).unwrap();
            let request = axum::http::Request::post(path).header(CONTENT_TYPE, "application/json")
                .body(Body::new(UploadBody {
                    frames: receiver, polled: polled.clone(), dropped: Arc::new(AtomicBool::new(false)),
                })).unwrap();
            let response = tokio::spawn(app(state.clone()).oneshot(request));
            while polled.load(Ordering::SeqCst) < 2 { tokio::task::yield_now().await; }
            tokio::time::advance(Duration::from_secs(9)).await;
            assert!(!response.is_finished());
            tokio::time::resume();
            frames.send(body.slice(body.len() / 2..)).unwrap();
            drop(frames);
            assert_eq!(response.await.unwrap().unwrap().status(), StatusCode::OK);
            assert_eq!(state.cpu_semaphore.available_permits(), 1);
            tokio::time::pause();
        }
    }

    #[tokio::test]
    async fn admitted_body_keeps_route_size_limits_and_body_read_errors() {
        struct BrokenBody;
        impl http_body::Body for BrokenBody {
            type Data = Bytes;
            type Error = io::Error;
            fn poll_frame(self: Pin<&mut Self>, _: &mut Context<'_>)
                -> Poll<Option<Result<http_body::Frame<Bytes>, io::Error>>> {
                Poll::Ready(Some(Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete upload"))))
            }
        }
        let state = AppState {
            cpu_semaphore: Arc::new(Semaphore::new(1)),
            upload_timeout: DEFAULT_UPLOAD_TIMEOUT,
        };
        for (path, limit) in ROUTES.into_iter().zip([LEGACY_BODY_LIMIT, RAW_BODY_LIMIT, BATCH_BODY_LIMIT]) {
            for (body, expected) in [
                (Body::from(vec![0u8; limit + 1]), StatusCode::PAYLOAD_TOO_LARGE),
                (Body::new(BrokenBody), StatusCode::BAD_REQUEST),
            ] {
                let request = axum::http::Request::post(path).body(body).unwrap();
                assert_eq!(app(state.clone()).oneshot(request).await.unwrap().status(), expected);
                assert_eq!(state.cpu_semaphore.available_permits(), 1);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn completed_upload_does_not_time_out_running_cpu_work() {
        let state = AppState {
            cpu_semaphore: Arc::new(Semaphore::new(1)),
            upload_timeout: Duration::from_secs(1),
        };
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let control = Arc::new((std::sync::Mutex::new(Some(started_tx)), std::sync::Mutex::new(release_rx)));
        let router = Router::new().route("/work", post(move |
            Extension(permit): Extension<Arc<OwnedSemaphorePermit>>,
            AdmittedBody(_body): AdmittedBody,
        | {
            let control = control.clone();
            async move {
                run_admitted_work(permit, move || {
                    control.0.lock().unwrap().take().unwrap().send(()).unwrap();
                    control.1.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
                }).await.unwrap();
                StatusCode::OK
            }
        })).route_layer(middleware::from_fn_with_state(state.clone(), admit_work)).with_state(state.clone());
        let request = axum::http::Request::post("/work").body(Body::empty()).unwrap();
        let response = tokio::spawn(router.oneshot(request));
        started_rx.await.unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(!response.is_finished(), "the upload deadline must not wrap CPU execution");
        assert_eq!(state.cpu_semaphore.available_permits(), 0);
        tokio::time::resume();
        release_tx.send(()).unwrap();
        assert_eq!(response.await.unwrap().unwrap().status(), StatusCode::OK);
        assert_eq!(state.cpu_semaphore.available_permits(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admitted_work_leaves_the_async_executor_available() {
        let semaphore = Arc::new(Semaphore::new(1));
        let permit = Arc::new(semaphore.acquire_owned().await.unwrap());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let request = tokio::spawn(run_admitted_work(permit, move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            7
        }));
        started_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        release_tx.send(()).unwrap();
        assert_eq!(request.await.unwrap().unwrap(), 7);
    }
    impl http_body::Body for CountedBody {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
            if self.remaining == 0 {
                return Poll::Ready(None);
            }
            self.remaining -= 1;
            self.polled.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Invalid input on every route: rejection must not parse/decompile it.
            Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from_static(&[b'!'; 1024])))))
        }
    }

    #[tokio::test]
    async fn excess_capacity_discards_frames_without_parsing_or_decompiling() {
        let state = AppState {
            cpu_semaphore: Arc::new(Semaphore::new(1)),
            upload_timeout: DEFAULT_UPLOAD_TIMEOUT,
        };
        let held = state.cpu_semaphore.clone().acquire_owned().await.unwrap();
        for path in ["/decompile", "/decompile/raw", "/decompile/batch"] {
            let polled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let request = axum::http::Request::post(path)
                .header("content-type", "application/json")
                .body(Body::new(CountedBody { remaining: 16, polled: polled.clone() }))
                .unwrap();
            let response = app(state.clone()).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(polled.load(std::sync::atomic::Ordering::Relaxed), 16);
            assert_eq!(state.cpu_semaphore.available_permits(), 0);
        }
        drop(held);
        let request = axum::http::Request::post("/decompile/batch")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"key":1,"scripts":[]}"#))
            .unwrap();
        let response = app(state.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.cpu_semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn rejected_body_discard_obeys_byte_and_time_limits() {
        let polled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let body = Body::new(CountedBody { remaining: 16, polled: polled.clone() });
        assert_eq!(discard_rejected_body(body, 3 * 1024, Duration::from_secs(1)).await,
            Err(StatusCode::PAYLOAD_TOO_LARGE));
        assert_eq!(polled.load(std::sync::atomic::Ordering::Relaxed), 4);

        struct StalledBody;
        impl http_body::Body for StalledBody {
            type Data = Bytes;
            type Error = Infallible;
            fn poll_frame(self: Pin<&mut Self>, _: &mut Context<'_>)
                -> Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
                Poll::Pending
            }
        }
        assert_eq!(discard_rejected_body(Body::new(StalledBody), 1024, Duration::from_millis(2)).await,
            Err(StatusCode::REQUEST_TIMEOUT));
    }

    /// Match real clients: finish sending the upload before reading the response,
    /// with HTTP/1.1's default keep-alive rather than an in-process Body service.
    fn socket_post(address: std::net::SocketAddr, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
        use std::io::{BufRead, Read, Write};
        let mut socket = std::net::TcpStream::connect(address).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(socket, "POST {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
        socket.write_all(body).unwrap();
        let mut reader = std::io::BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut length = None;
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" { break; }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
        }
        let mut response = vec![0; length.expect("response Content-Length")];
        reader.read_exact(&mut response).unwrap();
        (status, response)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn overload_uploads_receive_503_over_real_http_connections() {
        let state = AppState {
            cpu_semaphore: Arc::new(Semaphore::new(1)),
            upload_timeout: DEFAULT_UPLOAD_TIMEOUT,
        };
        let held = state.cpu_semaphore.clone().acquire_owned().await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app(state)).await.unwrap() });
        for _ in 0..3 {
            let mut requests = Vec::new();
            for index in 0..8 {
                requests.push(tokio::task::spawn_blocking(move || {
                    let (path, size) = match index % 3 {
                        0 => ("/decompile", 110_608),
                        1 => ("/decompile/raw", 562),
                        _ => ("/decompile/batch", 512 * 1024),
                    };
                    socket_post(address, path, &vec![b'!'; size])
                }));
            }
            for request in requests {
                let (status, body) = request.await.unwrap();
                assert_eq!(status, 503);
                assert_eq!(body, b"decompile capacity exhausted");
            }
        }
        drop(held);
        let (status, _) = tokio::task::spawn_blocking(move || {
            socket_post(address, "/decompile/batch", br#"{"key":1,"scripts":[]}"#)
        }).await.unwrap();
        assert_eq!(status, 200);
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_http_uploads_release_all_shared_capacity_at_the_deadline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let state = AppState {
            cpu_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_JOBS)),
            upload_timeout: Duration::from_secs(2),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move { axum::serve(listener, app(server_state)).await.unwrap() });
        let mut uploads = Vec::new();
        for index in 0..MAX_CONCURRENT_JOBS {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            let path = ROUTES[index % ROUTES.len()];
            socket.write_all(format!(
                "POST {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: 4\r\n\r\n"
            ).as_bytes()).await.unwrap();
            uploads.push(socket);
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.cpu_semaphore.available_permits() != 0 { tokio::task::yield_now().await; }
        }).await.expect("uploads did not acquire all shared permits");

        for path in ROUTES {
            let (status, _) = tokio::task::spawn_blocking(move || {
                socket_post(address, path, &valid_body(path))
            }).await.unwrap();
            assert_eq!(status, 503, "stalled uploads should currently occupy every slot");
        }
        for mut socket in uploads {
            let mut response = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut response))
                .await.expect("admitted upload was never timed out").unwrap();
            let response = String::from_utf8(response).unwrap();
            assert!(response.starts_with("HTTP/1.1 408"), "{response}");
            assert!(response.to_ascii_lowercase().contains("connection: close\r\n"));
        }
        assert_eq!(state.cpu_semaphore.available_permits(), MAX_CONCURRENT_JOBS);
        for path in ROUTES {
            let (status, _) = tokio::task::spawn_blocking(move || {
                socket_post(address, path, &valid_body(path))
            }).await.unwrap();
            assert_eq!(status, 200, "expired uploads must not starve {path}");
        }
        assert_eq!(state.cpu_semaphore.available_permits(), MAX_CONCURRENT_JOBS);
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn cancelled_request_keeps_capacity_until_blocking_work_finishes() {
        let semaphore = Arc::new(Semaphore::new(1));
        let permit = Arc::new(semaphore.clone().acquire_owned().await.unwrap());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let request = tokio::spawn(run_admitted_work(permit, move || {
            started_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }));
        started_rx.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert_eq!(semaphore.available_permits(), 0);
        release_tx.send(()).unwrap();
        let _released =
            tokio::time::timeout(std::time::Duration::from_secs(5), semaphore.acquire())
                .await
                .expect("worker did not release admission")
                .unwrap();
    }
}
