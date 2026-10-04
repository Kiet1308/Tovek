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
//! fails the whole batch (that item carries `ok:false` + an `error`). Malformed
//! request framing and request-wide input limits are HTTP 4xx responses.
//!
//! Upload/response reservations and CPU jobs have separate bounded capacity.
//! Batch uploads cannot occupy every ingress slot, and batch CPU work yields
//! between bounded quanta. CPU permits survive requester cancellation.
mod service_cache;
mod batch_response;
mod batch_reuse;

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
    Extension, Router,
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

/// CPU work is admitted after bounded input parsing and optional cache lookup.
const MAX_CONCURRENT_JOBS: usize = 4;
const MAX_INGRESS_JOBS: usize = 8;
const MAX_BATCH_INGRESS: usize = 2;
const BATCH_QUANTUM: usize = 8;
const SOURCE_CACHE_ENV: &str = "TOVEK_SOURCE_CACHE_MIB";

/// Requests that may wait for a job slot. A waiting request holds no body
/// buffer: its upload stays in the socket until it is admitted.
const DEFAULT_QUEUE_LIMIT: usize = 1024;
const QUEUE_LIMIT_ENV: &str = "TOVEK_QUEUE_LIMIT";
/// How long a request may wait for a job slot before it is turned away.
const DEFAULT_QUEUE_TIMEOUT: Duration = Duration::from_secs(120);
const QUEUE_TIMEOUT_ENV: &str = "TOVEK_QUEUE_TIMEOUT_SECS";
/// Seconds a client that was turned away should wait before retrying.
const RETRY_AFTER_SECS: &str = "1";

// Batch framing and payload limits. Parser and engine working allocations are
// separate from these retained protocol-buffer bounds.
const MDB1_MAGIC: &[u8; 4] = b"MDB1";
const MDB1_VERSION: u8 = 1;
const MDB1_FLAG_DONT_REUSE_VAR: u8 = luau_lifter::DONT_REUSE_VAR as u8;
const MDB1_FLAG_STRICT_NO_SYNTHETIC_CONTROL: u8 = STRICT_NO_SYNTHETIC_CONTROL as u8;
const MDB1_SUPPORTED_FLAGS: u8 =
    MDB1_FLAG_DONT_REUSE_VAR | MDB1_FLAG_STRICT_NO_SYNTHETIC_CONTROL;
const MAX_ENTRIES: usize = 50_000;
const MAX_NAME_LEN: usize = 4 * 1024; // 4 KiB — a GetFullName() path.
const MAX_CODE_LEN: usize = 16 * 1024 * 1024; // 16 MiB — one script's bytecode.
const MAX_ID_LEN: usize = 1024;
const MAX_DECODED_BATCH: usize = 64 * 1024 * 1024;
const MAX_SOURCE_LEN: usize = 16 * 1024 * 1024;
const MAX_RESPONSE_LEN: usize = 64 * 1024 * 1024;
const MAX_RESPONSE_METADATA: usize = 16 * 1024 * 1024;
const RESPONSE_BUDGET_ERROR: &str = "batch response budget exceeded";

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
    #[error("payload too large: {0}")]
    TooLarge(String),
    #[error("{0}")]
    Unavailable(&'static str),
}
impl Clone for Error {
    fn clone(&self) -> Self {
        match self {
            Self::Io(error) => Self::Io(io::Error::new(error.kind(), error.to_string())),
            Self::Base64(error) => Self::Base64(error.clone()),
            Self::BadRequest(error) => Self::BadRequest(error.clone()),
            Self::TooLarge(error) => Self::TooLarge(error.clone()),
            Self::Unavailable(error) => Self::Unavailable(error),
        }
    }
}
impl Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Error::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Base64(_) => StatusCode::BAD_REQUEST,
            Error::BadRequest(_) => StatusCode::BAD_REQUEST,
            Error::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Error::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut response = Response::builder().status(self.status_code());
        if matches!(self, Self::Unavailable(_)) {
            response = response.header(axum::http::header::RETRY_AFTER, RETRY_AFTER_SECS);
        }
        response
            .body(Body::from(format!("{self}")))
            .expect("failed to build body")
    }
}

/// Shared server state.
#[derive(Clone)]
struct AppState {
    /// Shared by legacy, raw and batch CPU jobs.
    cpu_semaphore: Arc<Semaphore>,
    /// Reserve CPU admission for interactive requests even under batch load.
    batch_cpu_semaphore: Arc<Semaphore>,
    /// Holds input/response memory; an upload never occupies a CPU permit.
    ingress_semaphore: Arc<Semaphore>,
    batch_ingress_semaphore: Arc<Semaphore>,
    /// Bound queued batches before they can consume shared queue places.
    batch_queue_semaphore: Arc<Semaphore>,
    /// Running plus waiting requests: bounds the FIFO queue in front of
    /// `ingress_semaphore`. Tokio semaphores hand out permits in request order.
    queue_semaphore: Arc<Semaphore>,
    /// How long a request may wait for a job slot, its body still unread.
    queue_timeout: Duration,
    /// One deadline for the complete admitted upload, not for CPU execution.
    upload_timeout: Duration,
    source_cache: service_cache::SourceCache,
}

impl AppState {
    fn new(jobs: usize, queue_limit: usize, queue_timeout: Duration, upload_timeout: Duration) -> Self {
        Self {
            cpu_semaphore: Arc::new(Semaphore::new(jobs)),
            batch_cpu_semaphore: Arc::new(Semaphore::new(jobs.saturating_sub(1).max(1).min(2))),
            ingress_semaphore: Arc::new(Semaphore::new(jobs)),
            batch_ingress_semaphore: Arc::new(Semaphore::new(jobs.max(1))),
            batch_queue_semaphore: Arc::new(Semaphore::new(jobs + queue_limit)),
            queue_semaphore: Arc::new(Semaphore::new(jobs + queue_limit)),
            queue_timeout,
            upload_timeout,
            source_cache: service_cache::SourceCache::new(0),
        }
    }

    /// Configure only before the state is handed to a router.
    fn with_ingress(mut self, slots: usize, batch_slots: usize) -> Self {
        let queued = self.queue_semaphore.available_permits() - self.ingress_semaphore.available_permits();
        let batch_slots = batch_slots.min(slots).max(1);
        self.ingress_semaphore = Arc::new(Semaphore::new(slots));
        self.batch_ingress_semaphore = Arc::new(Semaphore::new(batch_slots));
        // Reserve both ingress and waiting capacity for interactive traffic.
        // Otherwise batches waiting for their lane can fill the outer queue
        // while every reserved interactive ingress slot is still idle.
        let batch_places = if batch_slots < slots { batch_slots + queued / 2 } else { slots + queued };
        self.batch_queue_semaphore = Arc::new(Semaphore::new(batch_places));
        self.queue_semaphore = Arc::new(Semaphore::new(slots + queued));
        self
    }
}

/// Retained through input processing, detached work and response body ownership.
struct Admission {
    _job: OwnedSemaphorePermit,
    _place: OwnedSemaphorePermit,
    _batch: Option<OwnedSemaphorePermit>,
    _batch_place: Option<OwnedSemaphorePermit>,
}

/// Keep the ingress reservation until the response is consumed or dropped, so
/// slow readers cannot accumulate completed response buffers without a limit.
struct ReservedBody {
    body: Body,
    _admission: Arc<Admission>,
}
impl http_body::Body for ReservedBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>)
        -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, axum::Error>>> {
        std::pin::Pin::new(&mut self.body).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool { self.body.is_end_stream() }
    fn size_hint(&self) -> http_body::SizeHint { self.body.size_hint() }
}

fn parse_seconds(name: &str, value: &str) -> io::Result<Duration> {
    value.parse::<u32>().ok().filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(u64::from(seconds)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
            format!("{name} must be a positive 32-bit integer number of seconds")))
}

fn parse_queue_limit(value: &str) -> io::Result<usize> {
    value.parse::<u32>().map(|limit| limit as usize).map_err(|_| io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{QUEUE_LIMIT_ENV} must be a non-negative 32-bit integer")))
}

/// An unset variable means the default; a set but invalid one stops startup.
fn env_setting<T>(name: &str, default: T, parse: impl FnOnce(&str) -> io::Result<T>) -> io::Result<T> {
    match std::env::var(name) {
        Ok(value) => parse(&value),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(_) => Err(io::Error::new(io::ErrorKind::InvalidInput,
            format!("{name} must contain a valid integer"))),
    }
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

    let upload_timeout = env_setting(UPLOAD_TIMEOUT_ENV, DEFAULT_UPLOAD_TIMEOUT,
        |value| parse_seconds(UPLOAD_TIMEOUT_ENV, value))?;
    let queue_timeout = env_setting(QUEUE_TIMEOUT_ENV, DEFAULT_QUEUE_TIMEOUT,
        |value| parse_seconds(QUEUE_TIMEOUT_ENV, value))?;
    let queue_limit = env_setting(QUEUE_LIMIT_ENV, DEFAULT_QUEUE_LIMIT, parse_queue_limit)?;
    let cache_bytes = env_setting(SOURCE_CACHE_ENV, 0usize, |value| {
        value.parse::<usize>().ok().and_then(|mib| mib.checked_mul(1024 * 1024))
            .filter(|bytes| *bytes <= 1024 * 1024 * 1024)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
                format!("{SOURCE_CACHE_ENV} must be an integer from 0 to 1024 MiB")))
    })?;
    let mut state = AppState::new(MAX_CONCURRENT_JOBS, queue_limit, queue_timeout, upload_timeout)
        .with_ingress(MAX_INGRESS_JOBS, MAX_BATCH_INGRESS);
    state.source_cache = service_cache::SourceCache::new(cache_bytes);

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

/// Take a queue place subject to the batch quota, then wait within the request's
/// lane for ingress before AdmittedBody buffers the body with an upload deadline.
/// A full queue/quota or an expired wait is refused. On HTTP/1, dropping an unread upload can reset
/// the socket before the client sees 503, so a refused upload is discarded one
/// frame at a time, within route/time limits.
async fn admit_work(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let is_batch = request.uri().path() == "/decompile/batch";
    let batch_place = if is_batch {
        state.batch_queue_semaphore.clone().try_acquire_owned().map(Some)
    } else { Ok(None) };
    let place = batch_place.and_then(|batch_place| {
        state.queue_semaphore.clone().try_acquire_owned().map(|place| (place, batch_place))
    });
    let refusal = match place {
        Err(_) => "decompile queue full",
        Ok((place, batch_place)) => {
            let job = async {
                // Acquire the batch lane first: queued batches must not reserve
                // every general ingress slot while waiting for their own lane.
                let batch = if is_batch {
                    Some(state.batch_ingress_semaphore.clone().acquire_owned().await?)
                } else { None };
                let job = state.ingress_semaphore.clone().acquire_owned().await?;
                Ok::<_, tokio::sync::AcquireError>((job, batch))
            };
            match tokio::time::timeout(state.queue_timeout, job).await {
                Ok(Ok((job, batch))) => {
                    let admission = Arc::new(Admission {
                        _job: job, _place: place, _batch: batch, _batch_place: batch_place,
                    });
                    request.extensions_mut().insert(admission.clone());
                    let response = next.run(request).await;
                    return if response.status().is_success() && !response.body().is_end_stream() {
                        response.map(|body| Body::new(ReservedBody { body, _admission: admission }))
                    } else { response };
                }
                // The semaphore is never closed, so only the wait can fail.
                _ => "timed out waiting for a decompile slot",
            }
        }
    };
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
    (StatusCode::SERVICE_UNAVAILABLE, [(axum::http::header::RETRY_AFTER, RETRY_AFTER_SECS)], refusal)
        .into_response()
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
    State(state): State<AppState>,
    Extension(admission): Extension<Arc<Admission>>,
    headers: HeaderMap,
    AdmittedBody(body): AdmittedBody,
) -> Result<Response, Error> {
    let cache = state.source_cache.clone();
    let script = run_admitted_work(admission.clone(), move || {
        let bytecode = BASE64_STANDARD.decode(body)?.into();
        PreparedScript::new(bytecode, DEFAULT_KEY, header_string(&headers, "x-script-name"),
            parse_options_headers(&headers)?, &cache)
    }).await??;
    let source = decompile_prepared(state, admission, script).await?;
    Ok(([(CONTENT_TYPE, "text/plain; charset=utf-8")], source).into_response())
}

/// `POST /decompile/raw` — one script, RAW bytecode body (no base64). The script
/// name comes from `x-script-name`; an optional `x-encode-key` overrides the key.
async fn decompile_raw(
    State(state): State<AppState>,
    Extension(admission): Extension<Arc<Admission>>,
    headers: HeaderMap,
    AdmittedBody(body): AdmittedBody,
) -> Result<Response, Error> {
    let script_name = header_string(&headers, "x-script-name");
    let key = parse_key_header(&headers)?;
    let options = parse_options_headers(&headers)?;
    // With the optional cache disabled, raw input still moves without a copy.
    let cache = state.source_cache.clone();
    let script = if !cache.enabled() {
        PreparedScript::new(body, key, script_name, options, &cache)?
    } else {
        run_admitted_work(admission.clone(), move || {
            PreparedScript::new(body, key, script_name, options, &cache)
        }).await??
    };
    let source = decompile_prepared(state, admission, script).await?;
    Ok(([(CONTENT_TYPE, "text/plain; charset=utf-8")], source).into_response())
}

/// `POST /decompile/batch` — many scripts in one request.
///
/// `Content-Type: application/json` → JSON batch (base64 bytecode); anything else
/// (e.g. `application/octet-stream`) → the binary `MDB1` framing (raw bytecode).
/// Admitted batches respond 200 with a JSON results array; malformed framing
/// returns 4xx and the admission middleware returns 503 when the queue is full.
async fn decompile_batch(
    State(state): State<AppState>,
    Extension(admission): Extension<Arc<Admission>>,
    headers: HeaderMap,
    AdmittedBody(body): AdmittedBody,
) -> Result<Response, Error> {
    let items = run_admitted_work(admission.clone(), move || {
        let items = parse_batch_request(&headers, &body)?;
        batch_response::validate_metadata(&items)?;
        Ok::<_, Error>(items)
    }).await??;
    serve_parsed_batch(state, admission, items).await
}

struct PreparedScript {
    bytecode: Bytes,
    key: u8,
    script_name: Option<String>,
    options: DecompileOptions,
    cache_key: Option<service_cache::Key>,
}
impl PreparedScript {
    /// Called only by a blocking parser task, including bytecode key hashing.
    fn new(bytecode: Bytes, key: u8, script_name: Option<String>, options: DecompileOptions,
           cache: &service_cache::SourceCache) -> Result<Self, Error> {
        check_item_limits(bytecode.len(), script_name.as_deref(), None)?;
        let cache_key = if cache.accepts_key(bytecode.len(), script_name.as_ref().map_or(0, String::len))
            && !luau_lifter::requires_fresh_decompilation()
        {
            Some(service_cache::Key::new(&bytecode, key, options.bits(), script_name.as_deref(),
                std::env::var_os("MEDAL_NO_SHARED_TAIL").is_some()))
        } else { None };
        Ok(Self { bytecode, key, script_name, options, cache_key })
    }
}

async fn cpu_permits(state: &AppState, batch: bool)
    -> Result<(OwnedSemaphorePermit, Option<OwnedSemaphorePermit>), Error> {
    tokio::time::timeout(state.queue_timeout, async {
        let batch = if batch { Some(state.batch_cpu_semaphore.clone().acquire_owned().await) }
            else { None };
        let batch = batch.transpose().map_err(|_| Error::Unavailable("CPU scheduler stopped"))?;
        let cpu = state.cpu_semaphore.clone().acquire_owned().await
            .map_err(|_| Error::Unavailable("CPU scheduler stopped"))?;
        Ok((cpu, batch))
    }).await.map_err(|_| Error::Unavailable("timed out waiting for CPU capacity"))?
}

fn source_bytes(source: String) -> Result<Bytes, Error> {
    if source.len() > MAX_SOURCE_LEN {
        return Err(Error::TooLarge(format!("decompiled source exceeds {MAX_SOURCE_LEN} bytes")));
    }
    // Trim spare String capacity before retaining it in a response or cache.
    Ok(Bytes::from(source.into_bytes().into_boxed_slice()))
}

async fn compute_script(state: AppState, admission: Arc<Admission>, script: PreparedScript) -> service_cache::Outcome {
    let permits = cpu_permits(&state, false).await?;
    run_admitted_work((admission, permits), move || {
        luau_lifter::try_decompile_bytecode_with_options(&script.bytecode, script.key,
            script.script_name.as_deref(), script.options).map_err(Error::BadRequest).and_then(source_bytes)
    }).await?
}

async fn decompile_prepared(state: AppState, admission: Arc<Admission>, mut script: PreparedScript) -> service_cache::Outcome {
    let lookup = script.cache_key.take().map(|key| state.source_cache.claim(key))
        .unwrap_or(service_cache::Lookup::Bypass);
    match lookup {
        service_cache::Lookup::Ready(source) => Ok(source),
        service_cache::Lookup::Wait(receiver) => service_cache::wait(receiver).await,
        service_cache::Lookup::Lead(receiver, completion) => {
            // The first caller may disappear while waiting for CPU. The owner
            // task, completion guard and ingress reservation live independently.
            tokio::spawn(async move { completion.finish(compute_script(state, admission, script).await); });
            service_cache::wait(receiver).await
        }
        service_cache::Lookup::Bypass => compute_script(state, admission, script).await,
    }
}

type PreparedRow = (BatchResultItem, Option<PreparedScript>);
enum PendingResult {
    Cached(service_cache::Receiver),
    Direct(tokio::sync::oneshot::Receiver<service_cache::Outcome>),
}
struct BatchWork {
    script: PreparedScript,
    completion: Option<service_cache::Completion>,
    direct: Option<tokio::sync::oneshot::Sender<service_cache::Outcome>>,
}

async fn compute_batch_quantum(state: AppState, admission: Arc<Admission>, work: Vec<BatchWork>) {
    let (scripts, targets): (Vec<_>, Vec<_>) = work.into_iter()
        .map(|work| (work.script, (work.completion, work.direct))).unzip();
    let outcomes = match cpu_permits(&state, true).await {
        Err(error) => Err(error),
        Ok(permits) => run_admitted_work((admission, permits), move || {
            let options = scripts.first().map_or_else(DecompileOptions::default, |script| script.options);
            let inputs = scripts.iter().map(|script| BatchInput {
                bytecode: &script.bytecode, encode_key: script.key, script_name: script.script_name.as_deref(),
            }).collect::<Vec<_>>();
            // Retain native outer Rayon parallelism and its exact-context dedup;
            // no cache or async coordination runs inside Rayon.
            lib_decompile_batch_with_options(&inputs, options).into_iter()
                .map(|outcome| outcome.map_err(Error::BadRequest).and_then(source_bytes)).collect::<Vec<_>>()
        }).await,
    };
    let outcomes = match outcomes {
        Ok(outcomes) => outcomes,
        Err(error) => (0..targets.len()).map(|_| Err(error.clone())).collect(),
    };
    for ((completion, direct), outcome) in targets.into_iter().zip(outcomes) {
        if let Some(completion) = completion { completion.finish(outcome); }
        else if let Some(direct) = direct { let _ = direct.send(outcome); }
    }
}

async fn execute_quantum(state: AppState, admission: Arc<Admission>, rows: Vec<PreparedRow>)
    -> (Vec<BatchResultItem>, Vec<bool>) {
    let mut results = Vec::with_capacity(rows.len());
    let mut reusable = vec![true; rows.len()];
    let mut waiting = Vec::new();
    let mut work = Vec::new();
    for (position, (mut result, script)) in rows.into_iter().enumerate() {
        if let Some(mut script) = script {
            let lookup = script.cache_key.take().map(|key| state.source_cache.claim(key))
                .unwrap_or(service_cache::Lookup::Bypass);
            match lookup {
                service_cache::Lookup::Ready(source) => {
                    result.ok = true; result.decompilation = Some(source);
                }
                service_cache::Lookup::Wait(receiver) => waiting.push((position, PendingResult::Cached(receiver))),
                service_cache::Lookup::Lead(receiver, completion) => {
                    waiting.push((position, PendingResult::Cached(receiver)));
                    work.push(BatchWork { script, completion: Some(completion), direct: None });
                }
                service_cache::Lookup::Bypass => {
                    let (sender, receiver) = tokio::sync::oneshot::channel();
                    waiting.push((position, PendingResult::Direct(receiver)));
                    work.push(BatchWork { script, completion: None, direct: Some(sender) });
                }
            }
        }
        results.push(result);
    }
    if !work.is_empty() {
        // Own all completion guards before a caller can cancel. They wake every
        // follower even if this background task is dropped during shutdown.
        tokio::spawn(compute_batch_quantum(state, admission, work));
    }
    for (position, receiver) in waiting {
        let outcome = match receiver {
            PendingResult::Cached(receiver) => service_cache::wait(receiver).await,
            PendingResult::Direct(receiver) => receiver.await.unwrap_or_else(|_| {
                Err(Error::Io(io::Error::other("batch worker stopped")))
            }),
        };
        let row = &mut results[position];
        match outcome {
            Ok(source) => { row.ok = true; row.decompilation = Some(source); }
            Err(error) => {
                reusable[position] = !matches!(error, Error::Unavailable(_) | Error::Io(_));
                row.error = Some(error.to_string());
            }
        }
    }
    (results, reusable)
}

fn prepare_quantum(quantum: Vec<ParsedItem>, offset: usize, cache: &service_cache::SourceCache,
                   reuse: &batch_reuse::Plan) -> (Vec<PreparedRow>, Vec<Option<usize>>) {
    let mut aliases = Vec::with_capacity(quantum.len());
    let mut representatives: Vec<(usize, usize)> = Vec::new();
    let rows = quantum.into_iter().enumerate().map(|(position, item)| {
        let index = offset + position;
        aliases.push(None);
        match item {
            ParsedItem::Failed { id, script_name, error } => (
                BatchResultItem { index, id, script_name, ok: false, decompilation: None, error: Some(error) }, None,
            ),
            ParsedItem::Ready { bytecode, key, options, id, script_name } => {
                let mut row = BatchResultItem { index, id, script_name: script_name.clone(),
                    ok: false, decompilation: None, error: None };
                if reuse.apply(&mut row) { return (row, None); }
                if let Some(slot) = reuse.slot(index) {
                    if let Some(&(_, representative)) = representatives.iter().find(|&&(seen, _)| seen == slot) {
                        aliases[position] = Some(representative);
                        return (row, None);
                    }
                    representatives.push((slot, position));
                }
                let script = match PreparedScript::new(bytecode, key, script_name, options, cache) {
                    Ok(script) => Some(script),
                    Err(error) => { row.error = Some(error.to_string()); None }
                };
                (row, script)
            }
        }
    }).collect();
    (rows, aliases)
}

fn finish_quantum(results: &mut [BatchResultItem], mut reusable: Vec<bool>, aliases: Vec<Option<usize>>,
                  reuse: &mut batch_reuse::Plan) {
    for (position, alias) in aliases.into_iter().enumerate() {
        if let Some(representative) = alias {
            results[position].ok = results[representative].ok;
            results[position].decompilation = results[representative].decompilation.clone();
            results[position].error = results[representative].error.clone();
            reusable[position] = reusable[representative];
        }
        // Apply an existing request-wide result without changing this row's
        // correlation metadata; memo exhaustion leaves current output intact.
        reuse.apply(&mut results[position]);
        reuse.finish_row(&mut results[position], reusable[position]);
    }
}

async fn serve_parsed_batch(state: AppState, admission: Arc<Admission>, items: Vec<ParsedItem>) -> Result<Response, Error> {
    let (items, mut writer, mut reuse) = run_admitted_work(admission.clone(), move || {
        let writer = batch_response::Writer::new(&items)?;
        let reuse = batch_reuse::Plan::new(&items);
        Ok::<_, Error>((items, writer, reuse))
    }).await??;
    let mut items = items.into_iter();
    let mut offset = 0;
    loop {
        let quantum = items.by_ref().take(BATCH_QUANTUM).collect::<Vec<_>>();
        if quantum.is_empty() { break; }
        let count = quantum.len();
        let cache = state.source_cache.clone();
        let (prepared, aliases, next_reuse) = run_admitted_work(admission.clone(), move || {
            let (prepared, aliases) = prepare_quantum(quantum, offset, &cache, &reuse);
            (prepared, aliases, reuse)
        }).await?;
        let (mut results, reusable) = execute_quantum(state.clone(), admission.clone(), prepared).await;
        let (next_writer, next_reuse) = run_admitted_work(admission.clone(), move || {
            let mut reuse = next_reuse;
            finish_quantum(&mut results, reusable, aliases, &mut reuse);
            writer.append(results)?;
            Ok::<_, Error>((writer, reuse))
        }).await??;
        writer = next_writer;
        reuse = next_reuse;
        offset += count;
        tokio::task::yield_now().await;
    }
    let bytes = run_admitted_work(admission, move || writer.finish()).await??;
    info!("Batch completed {offset} scripts.");
    Ok(([(CONTENT_TYPE, "application/json")], bytes).into_response())
}

async fn run_admitted_work<P, F, T>(permit: P, work: F) -> Result<T, Error>
where
    P: Send + 'static,
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
struct BatchResultItem {
    /// Zero-based position in the request — the universal correlation key.
    index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    script_name: Option<String>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "serialize_source")]
    decompilation: Option<Bytes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn serialize_source<S: serde::Serializer>(source: &Option<Bytes>, serializer: S) -> Result<S::Ok, S::Error> {
    match source {
        Some(source) => serializer.serialize_str(std::str::from_utf8(source)
            .map_err(serde::ser::Error::custom)?),
        None => serializer.serialize_none(),
    }
}

fn check_item_limits(code_len: usize, name: Option<&str>, id: Option<&str>) -> Result<(), Error> {
    if code_len > MAX_CODE_LEN { return Err(Error::TooLarge(format!("bytecode exceeds {MAX_CODE_LEN} bytes"))); }
    if name.is_some_and(|name| name.len() > MAX_NAME_LEN) {
        return Err(Error::TooLarge(format!("script name exceeds {MAX_NAME_LEN} bytes")));
    }
    if id.is_some_and(|id| id.len() > MAX_ID_LEN) {
        return Err(Error::TooLarge(format!("script id exceeds {MAX_ID_LEN} bytes")));
    }
    Ok(())
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
    let mut decoded_bytes = 0usize;
    for item in req.scripts {
        check_item_limits(0, item.script_name.as_deref(), item.id.as_deref())?;
        if item.bytecode.len() > MAX_CODE_LEN.div_ceil(3) * 4 {
            return Err(Error::TooLarge(format!("encoded bytecode exceeds the {MAX_CODE_LEN}-byte decoded limit")));
        }
        // A bad base64 payload is bad *data* for one script, not a malformed
        // request — defer it as a per-item failure so it can't sink the batch.
        match BASE64_STANDARD.decode(item.bytecode.as_bytes()) {
            Ok(bytecode) => {
                check_item_limits(bytecode.len(), item.script_name.as_deref(), item.id.as_deref())?;
                decoded_bytes = decoded_bytes.saturating_add(bytecode.len());
                if decoded_bytes > MAX_DECODED_BATCH {
                    return Err(Error::TooLarge(format!("decoded batch exceeds {MAX_DECODED_BATCH} bytes")));
                }
                out.push(ParsedItem::Ready {
                    bytecode: bytecode.into(), key, options, id: item.id, script_name: item.script_name,
                });
            }
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
    let mut decoded_bytes = 0usize;
    for _ in 0..count {
        let name_len = read_u32(body, &mut pos)
            .ok_or_else(|| Error::BadRequest("MDB1: truncated (name length)".into()))?
            as usize;
        if name_len > MAX_NAME_LEN {
            return Err(Error::TooLarge(format!(
                "MDB1: name too large {name_len} (max {MAX_NAME_LEN})"
            )));
        }
        let name = take(body, &mut pos, name_len)
            .ok_or_else(|| Error::BadRequest("MDB1: truncated (name)".into()))?;

        let code_len = read_u32(body, &mut pos)
            .ok_or_else(|| Error::BadRequest("MDB1: truncated (code length)".into()))?
            as usize;
        if code_len > MAX_CODE_LEN {
            return Err(Error::TooLarge(format!(
                "MDB1: code too large {code_len} (max {MAX_CODE_LEN})"
            )));
        }
        decoded_bytes = decoded_bytes.saturating_add(code_len);
        if decoded_bytes > MAX_DECODED_BATCH {
            return Err(Error::TooLarge(format!("decoded batch exceeds {MAX_DECODED_BATCH} bytes")));
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
                    decompilation: Some(Bytes::from(source.into_bytes().into_boxed_slice())),
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
            "COMPACT_STYLE" => options.compact_style = true,
            "ASSUME_STANDARD_LIBRARIES" => options.assume_standard_libraries = true,
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
        let state = AppState::new(1, 0, DEFAULT_QUEUE_TIMEOUT, DEFAULT_UPLOAD_TIMEOUT);
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
    fn timeout_configuration_rejects_disabled_or_invalid_deadlines() {
        for name in [UPLOAD_TIMEOUT_ENV, QUEUE_TIMEOUT_ENV] {
            assert_eq!(parse_seconds(name, "1").unwrap(), Duration::from_secs(1));
            assert_eq!(parse_seconds(name, "120").unwrap(), Duration::from_secs(120));
            for invalid in ["", "0", "-1", "1.5", "never", "4294967296"] {
                let error = parse_seconds(name, invalid).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
                assert!(error.to_string().contains(name));
            }
        }
    }

    #[test]
    fn queue_limit_configuration_allows_zero_and_rejects_invalid_values() {
        assert_eq!(parse_queue_limit("0").unwrap(), 0);
        assert_eq!(parse_queue_limit("1024").unwrap(), 1024);
        for invalid in ["", "-1", "1.5", "many", "4294967296"] {
            assert_eq!(parse_queue_limit(invalid).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }

    /// Occupy one job slot the way a running request does: queue place and job.
    async fn hold_job(state: &AppState) -> Admission {
        let place = state.queue_semaphore.clone().try_acquire_owned().expect("no queue place free");
        let job = state.ingress_semaphore.clone().acquire_owned().await.unwrap();
        Admission { _job: job, _place: place, _batch: None, _batch_place: None }
    }

    /// Resolve once exactly `free` queue places remain, i.e. requests have queued.
    async fn until_free_places(state: &AppState, free: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.queue_semaphore.available_permits() != free { tokio::task::yield_now().await; }
        }).await.expect("requests never reached the queue");
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_and_trickled_admitted_bodies_have_one_deadline_on_every_route() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        for path in ROUTES {
            for trickle in [false, true] {
                let state = AppState::new(1, 0, DEFAULT_QUEUE_TIMEOUT, Duration::from_secs(10));
                let (frames, receiver) = tokio::sync::mpsc::unbounded_channel();
                let polled = Arc::new(AtomicUsize::new(0));
                let dropped = Arc::new(AtomicBool::new(false));
                let body = UploadBody { frames: receiver, polled: polled.clone(), dropped: dropped.clone() };
                let request = axum::http::Request::post(path)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::new(body)).unwrap();
                let deadline = tokio::time::Instant::now() + state.upload_timeout;
                let response = tokio::spawn(app(state.clone()).oneshot(request));
                while polled.load(Ordering::SeqCst) == 0 {
                    assert!(!response.is_finished(), "admitted body was never polled");
                    tokio::task::yield_now().await;
                }
                assert_eq!(state.ingress_semaphore.available_permits(), 0);

                for _ in 0..2 {
                    tokio::time::advance(Duration::from_secs(4)).await;
                    if trickle {
                        let previous_polls = polled.load(Ordering::SeqCst);
                        frames.send(Bytes::from_static(b"x")).unwrap();
                        while polled.load(Ordering::SeqCst) == previous_polls {
                            assert!(!response.is_finished(), "upload expired before its deadline");
                            tokio::task::yield_now().await;
                        }
                    }
                    assert!(!response.is_finished());
                    assert_eq!(state.ingress_semaphore.available_permits(), 0);
                }
                tokio::time::advance(Duration::from_secs(2)).await;
                // Tokio's timer rounds to milliseconds. Bound the join so a
                // missing/reset deadline cannot pass by auto-advancing virtual time.
                let timer_slack = Duration::from_millis(2);
                let response = tokio::time::timeout(timer_slack, response).await
                    .expect("upload deadline was missing or extended by incoming frames")
                    .unwrap().unwrap();
                assert!(tokio::time::Instant::now() <= deadline + timer_slack,
                    "incoming frames must not extend the original upload deadline");
                assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
                assert_eq!(response.headers()[axum::http::header::CONNECTION], "close");
                assert!(dropped.load(Ordering::SeqCst));
                assert_eq!(state.ingress_semaphore.available_permits(), 1);
                drop(frames);

                // A completed upload on the same route is admitted immediately.
                tokio::time::resume();
                let request = axum::http::Request::post(path).header(CONTENT_TYPE, "application/json")
                    .body(Body::from(valid_body(path))).unwrap();
                assert_eq!(app(state.clone()).oneshot(request).await.unwrap().status(), StatusCode::OK);
                assert_eq!(state.ingress_semaphore.available_permits(), 1);
                tokio::time::pause();
            }
        }
    }

    #[tokio::test]
    async fn cancelled_admitted_upload_drops_its_body_and_permit() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let state = AppState::new(1, 0, DEFAULT_QUEUE_TIMEOUT, DEFAULT_UPLOAD_TIMEOUT);
        let (_frames, receiver) = tokio::sync::mpsc::unbounded_channel();
        let polled = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let request = axum::http::Request::post("/decompile/raw").body(Body::new(UploadBody {
            frames: receiver, polled: polled.clone(), dropped: dropped.clone(),
        })).unwrap();
        let response = tokio::spawn(app(state.clone()).oneshot(request));
        while polled.load(Ordering::SeqCst) == 0 {
            assert!(!response.is_finished(), "admitted body was never polled");
            tokio::task::yield_now().await;
        }
        response.abort();
        assert!(response.await.unwrap_err().is_cancelled());
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(state.ingress_semaphore.available_permits(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn multi_frame_uploads_completed_before_the_deadline_still_succeed() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        for path in ROUTES {
            let state = AppState::new(1, 0, DEFAULT_QUEUE_TIMEOUT, Duration::from_secs(10));
            let (frames, receiver) = tokio::sync::mpsc::unbounded_channel();
            let polled = Arc::new(AtomicUsize::new(0));
            let body = valid_body(path);
            frames.send(body.slice(..body.len() / 2)).unwrap();
            let request = axum::http::Request::post(path).header(CONTENT_TYPE, "application/json")
                .body(Body::new(UploadBody {
                    frames: receiver, polled: polled.clone(), dropped: Arc::new(AtomicBool::new(false)),
                })).unwrap();
            let response = tokio::spawn(app(state.clone()).oneshot(request));
            while polled.load(Ordering::SeqCst) < 2 {
                assert!(!response.is_finished(), "upload ended before the remaining body arrived");
                tokio::task::yield_now().await;
            }
            tokio::time::advance(Duration::from_secs(9)).await;
            assert!(!response.is_finished());
            tokio::time::resume();
            frames.send(body.slice(body.len() / 2..)).unwrap();
            drop(frames);
            assert_eq!(response.await.unwrap().unwrap().status(), StatusCode::OK);
            assert_eq!(state.ingress_semaphore.available_permits(), 1);
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
        let state = AppState::new(1, 0, DEFAULT_QUEUE_TIMEOUT, DEFAULT_UPLOAD_TIMEOUT);
        for (path, limit) in ROUTES.into_iter().zip([LEGACY_BODY_LIMIT, RAW_BODY_LIMIT, BATCH_BODY_LIMIT]) {
            for (body, expected) in [
                (Body::from(vec![0u8; limit + 1]), StatusCode::PAYLOAD_TOO_LARGE),
                (Body::new(BrokenBody), StatusCode::BAD_REQUEST),
            ] {
                let request = axum::http::Request::post(path).body(body).unwrap();
                assert_eq!(app(state.clone()).oneshot(request).await.unwrap().status(), expected);
                assert_eq!(state.ingress_semaphore.available_permits(), 1);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn completed_upload_does_not_time_out_running_cpu_work() {
        let state = AppState::new(1, 0, DEFAULT_QUEUE_TIMEOUT, Duration::from_secs(1));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let control = Arc::new((std::sync::Mutex::new(Some(started_tx)), std::sync::Mutex::new(release_rx)));
        let router = Router::new().route("/work", post(move |
            Extension(permit): Extension<Arc<Admission>>,
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
        assert_eq!(state.ingress_semaphore.available_permits(), 0);
        tokio::time::resume();
        release_tx.send(()).unwrap();
        assert_eq!(response.await.unwrap().unwrap().status(), StatusCode::OK);
        assert_eq!(state.ingress_semaphore.available_permits(), 1);
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
        let state = AppState::new(1, 0, DEFAULT_QUEUE_TIMEOUT, DEFAULT_UPLOAD_TIMEOUT);
        let held = hold_job(&state).await;
        for path in ["/decompile", "/decompile/raw", "/decompile/batch"] {
            let polled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let request = axum::http::Request::post(path)
                .header("content-type", "application/json")
                .body(Body::new(CountedBody { remaining: 16, polled: polled.clone() }))
                .unwrap();
            let response = app(state.clone()).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[axum::http::header::RETRY_AFTER], RETRY_AFTER_SECS);
            assert_eq!(polled.load(std::sync::atomic::Ordering::Relaxed), 16);
            assert_eq!(state.ingress_semaphore.available_permits(), 0);
        }
        drop(held);
        let request = axum::http::Request::post("/decompile/batch")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"key":1,"scripts":[]}"#))
            .unwrap();
        let response = app(state.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
        assert_eq!(state.ingress_semaphore.available_permits(), 1);
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
        let state = AppState::new(1, 0, DEFAULT_QUEUE_TIMEOUT, DEFAULT_UPLOAD_TIMEOUT);
        let held = hold_job(&state).await;
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
                assert_eq!(body, b"decompile queue full");
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
        let state = AppState::new(MAX_CONCURRENT_JOBS, 0, DEFAULT_QUEUE_TIMEOUT, Duration::from_secs(2));
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
            while state.ingress_semaphore.available_permits() != 0 { tokio::task::yield_now().await; }
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
        assert_eq!(state.ingress_semaphore.available_permits(), MAX_CONCURRENT_JOBS);
        for path in ROUTES {
            let (status, _) = tokio::task::spawn_blocking(move || {
                socket_post(address, path, &valid_body(path))
            }).await.unwrap();
            assert_eq!(status, 200, "expired uploads must not starve {path}");
        }
        assert_eq!(state.ingress_semaphore.available_permits(), MAX_CONCURRENT_JOBS);
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

    #[tokio::test]
    async fn queued_requests_wait_unread_and_run_in_arrival_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let state = AppState::new(1, 8, DEFAULT_QUEUE_TIMEOUT, DEFAULT_UPLOAD_TIMEOUT);
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = order.clone();
        let router = Router::new().route("/work", post(move |
            Extension(admission): Extension<Arc<Admission>>,
            headers: HeaderMap,
            AdmittedBody(_body): AdmittedBody,
        | {
            let recorded = recorded.clone();
            async move {
                let id = header_string(&headers, "x-id").unwrap();
                run_admitted_work(admission, move || recorded.lock().unwrap().push(id)).await.unwrap();
                StatusCode::OK
            }
        })).route_layer(middleware::from_fn_with_state(state.clone(), admit_work)).with_state(state.clone());

        let held = hold_job(&state).await;
        let mut responses = Vec::new();
        let mut polls = Vec::new();
        for id in 0..5 {
            let polled = Arc::new(AtomicUsize::new(0));
            let body = CountedBody { remaining: 1, polled: polled.clone() };
            let request = axum::http::Request::post("/work").header("x-id", id.to_string())
                .body(Body::new(body)).unwrap();
            responses.push(tokio::spawn(router.clone().oneshot(request)));
            // Wait for each request to queue before sending the next.
            until_free_places(&state, 7 - id).await;
            polls.push(polled);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(responses.iter().all(|response| !response.is_finished()), "queued requests must wait");
        assert!(polls.iter().all(|polled| polled.load(Ordering::SeqCst) == 0), "queued bodies must stay unread");

        drop(held);
        for response in responses {
            assert_eq!(response.await.unwrap().unwrap().status(), StatusCode::OK);
        }
        assert_eq!(*order.lock().unwrap(), ["0", "1", "2", "3", "4"]);
        assert_eq!(state.ingress_semaphore.available_permits(), 1);
        assert_eq!(state.queue_semaphore.available_permits(), 9);
    }

    #[tokio::test]
    async fn full_queue_refuses_new_requests_until_a_place_frees() {
        let state = AppState::new(1, 1, DEFAULT_QUEUE_TIMEOUT, DEFAULT_UPLOAD_TIMEOUT);
        let held = hold_job(&state).await;
        let request = |body: Bytes| axum::http::Request::post("/decompile/raw").body(Body::from(body)).unwrap();
        let queued = tokio::spawn(app(state.clone()).oneshot(request(valid_body("/decompile/raw"))));
        until_free_places(&state, 0).await;

        let refused = app(state.clone()).oneshot(request(valid_body("/decompile/raw"))).await.unwrap();
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(refused.headers()[axum::http::header::RETRY_AFTER], RETRY_AFTER_SECS);
        let body = axum::body::to_bytes(refused.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"decompile queue full");

        drop(held);
        assert_eq!(queued.await.unwrap().unwrap().status(), StatusCode::OK);
        let retried = app(state.clone()).oneshot(request(valid_body("/decompile/raw"))).await.unwrap();
        assert_eq!(retried.status(), StatusCode::OK);
        drop(retried);
        assert_eq!(state.queue_semaphore.available_permits(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn expired_queue_wait_is_refused_and_frees_its_place() {
        let state = AppState::new(1, 4, Duration::from_secs(5), DEFAULT_UPLOAD_TIMEOUT);
        let held = hold_job(&state).await;
        for path in ROUTES {
            let request = axum::http::Request::post(path).header(CONTENT_TYPE, "application/json")
                .body(Body::from(valid_body(path))).unwrap();
            let response = tokio::spawn(app(state.clone()).oneshot(request));
            until_free_places(&state, 3).await;
            tokio::time::advance(Duration::from_secs(4)).await;
            assert!(!response.is_finished(), "{path} gave up before its queue deadline");
            tokio::time::advance(Duration::from_secs(1)).await;
            let response = tokio::time::timeout(Duration::from_millis(2), response).await
                .expect("queue deadline was missing").unwrap().unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[axum::http::header::RETRY_AFTER], RETRY_AFTER_SECS);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], b"timed out waiting for a decompile slot");
            assert_eq!(state.queue_semaphore.available_permits(), 4);
        }
        drop(held);
        assert_eq!(state.ingress_semaphore.available_permits(), 1);
        assert_eq!(state.queue_semaphore.available_permits(), 5);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn burst_over_real_http_connections_queues_instead_of_failing() {
        let state = AppState::new(1, 32, DEFAULT_QUEUE_TIMEOUT, DEFAULT_UPLOAD_TIMEOUT);
        let held = hold_job(&state).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app(state)).await.unwrap() });
        let mut requests = Vec::new();
        for index in 0..12 {
            let path = ROUTES[index % ROUTES.len()];
            requests.push(tokio::task::spawn_blocking(move || socket_post(address, path, &valid_body(path))));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(requests.iter().all(|request| !request.is_finished()), "the burst should be waiting");
        drop(held);
        for request in requests {
            assert_eq!(request.await.unwrap().0, 200);
        }
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
}

#[cfg(test)]
mod serving_tests {
    use super::*;
    use tower::ServiceExt;

    fn state(cache_bytes: usize) -> AppState {
        let mut state = AppState::new(2, 8, Duration::from_secs(5), DEFAULT_UPLOAD_TIMEOUT).with_ingress(4, 1);
        state.source_cache = service_cache::SourceCache::new(cache_bytes);
        state
    }

    fn raw(name: &str) -> axum::http::Request<Body> {
        axum::http::Request::post("/decompile/raw").header("x-script-name", name)
            .body(Body::from(super::tests::bytecode())).unwrap()
    }

    async fn until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !condition() { tokio::task::yield_now().await; }
        }).await.expect("state did not become ready");
    }

    #[tokio::test]
    async fn cached_response_bypasses_busy_cpu_but_cold_context_waits() {
        let state = state(1024 * 1024);
        let response = app(state.clone()).oneshot(raw("Widget")).await.unwrap();
        let expected = axum::body::to_bytes(response.into_body(), MAX_SOURCE_LEN).await.unwrap();
        assert_eq!(state.source_cache.counts(), (1, 0));
        let cpu = state.cpu_semaphore.clone().acquire_many_owned(2).await.unwrap();
        let hit = tokio::time::timeout(Duration::from_secs(1), app(state.clone()).oneshot(raw("Widget")))
            .await.expect("cache hit waited for CPU").unwrap();
        assert_eq!(axum::body::to_bytes(hit.into_body(), MAX_SOURCE_LEN).await.unwrap(), expected);
        let cold = tokio::spawn(app(state.clone()).oneshot(raw("Gadget")));
        until(|| state.source_cache.counts().1 == 1).await;
        assert!(!cold.is_finished());
        drop(cpu);
        assert_eq!(cold.await.unwrap().unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn cancelled_singleflight_leader_keeps_ingress_and_completes_for_its_follower() {
        let state = state(1024 * 1024);
        let cpu = state.cpu_semaphore.clone().acquire_many_owned(2).await.unwrap();
        let first = tokio::spawn(app(state.clone()).oneshot(raw("Widget")));
        until(|| state.source_cache.counts().1 == 1).await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(state.ingress_semaphore.available_permits(), 3, "detached owner retains input reservation");
        let second = tokio::spawn(app(state.clone()).oneshot(raw("Widget")));
        until(|| state.ingress_semaphore.available_permits() == 2).await;
        assert_eq!(state.source_cache.counts(), (0, 1));
        drop(cpu);
        let response = second.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let source = axum::body::to_bytes(response.into_body(), MAX_SOURCE_LEN).await.unwrap();
        assert_eq!(std::str::from_utf8(&source).unwrap().trim(), "return 7");
        assert_eq!(state.source_cache.counts(), (1, 0));
        assert_eq!(state.ingress_semaphore.available_permits(), 4);
    }

    #[tokio::test]
    async fn batch_and_single_routes_share_successful_contexts_and_preserve_request_ids() {
        let state = state(1024 * 1024);
        let encoded = BASE64_STANDARD.encode(super::tests::bytecode());
        let body = serde_json::json!({"scripts": [
            {"id":"first","script_name":"Widget","bytecode":encoded},
            {"id":"second","script_name":"Widget","bytecode":encoded},
        ]});
        let response = app(state.clone()).oneshot(axum::http::Request::post("/decompile/batch")
            .header(CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), MAX_RESPONSE_LEN).await.unwrap();
        let output: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(output["ok_count"], 2);
        assert_eq!(output["results"][0]["id"], "first");
        assert_eq!(output["results"][1]["id"], "second");
        assert_eq!(state.source_cache.counts(), (1, 0));
        let _cpu = state.cpu_semaphore.clone().acquire_many_owned(2).await.unwrap();
        let response = tokio::time::timeout(Duration::from_secs(1), app(state.clone()).oneshot(raw("Widget")))
            .await.expect("cross-route hit waited for CPU").unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn request_local_reuse_spans_quanta_with_process_cache_disabled() {
        let state = state(0);
        let admission = Arc::new(Admission {
            _job: state.ingress_semaphore.clone().try_acquire_owned().unwrap(),
            _place: state.queue_semaphore.clone().try_acquire_owned().unwrap(),
            _batch: None, _batch_place: None,
        });
        let count = BATCH_QUANTUM * 2 + 1;
        let items = (0..count).map(|index| ParsedItem::Ready {
            bytecode: super::tests::bytecode().into(), key: DEFAULT_KEY,
            options: DecompileOptions::default(), id: Some(index.to_string()),
            script_name: Some(format!("parent{index}.Widget")),
        }).collect::<Vec<_>>();
        let mut reuse = batch_reuse::Plan::new(&items);
        let mut writer = batch_response::Writer::new(&items).unwrap();
        let mut inputs = items.into_iter();
        let mut busy_cpu = None;
        for offset in (0..count).step_by(BATCH_QUANTUM) {
            let quantum = inputs.by_ref().take(BATCH_QUANTUM).collect();
            let (prepared, aliases) = prepare_quantum(quantum, offset, &state.source_cache, &reuse);
            assert_eq!(prepared.iter().filter(|(_, script)| script.is_some()).count(), usize::from(offset == 0));
            let (mut results, reusable) = tokio::time::timeout(Duration::from_secs(1),
                execute_quantum(state.clone(), admission.clone(), prepared))
                .await.expect("a later duplicate quantum waited for CPU");
            finish_quantum(&mut results, reusable, aliases, &mut reuse);
            writer.append(results).unwrap();
            if offset == 0 { busy_cpu = Some(state.cpu_semaphore.clone().acquire_many_owned(2).await.unwrap()); }
        }
        let response: serde_json::Value = serde_json::from_slice(&writer.finish().unwrap()).unwrap();
        assert_eq!(response["ok_count"], count);
        for index in 0..count {
            let row = &response["results"][index];
            assert_eq!(row["id"], index.to_string());
            assert_eq!(row["script_name"], format!("parent{index}.Widget"));
            assert_eq!(row["decompilation"].as_str().unwrap().trim(), "return 7");
        }
        assert_eq!(state.source_cache.counts(), (0, 0));
        drop(busy_cpu);
    }

    #[tokio::test]
    async fn failed_decompilations_do_not_fill_the_source_cache() {
        let state = state(1024 * 1024);
        for _ in 0..2 {
            let response = app(state.clone()).oneshot(axum::http::Request::post("/decompile/raw")
                .body(Body::from(vec![99u8])).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(state.source_cache.counts(), (0, 0));
        }
    }

    #[tokio::test]
    async fn response_buffer_retains_ingress_capacity_until_dropped_but_releases_cpu() {
        let state = AppState::new(1, 0, DEFAULT_QUEUE_TIMEOUT, DEFAULT_UPLOAD_TIMEOUT);
        let response = app(state.clone()).oneshot(raw("Widget")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.cpu_semaphore.available_permits(), 1);
        assert_eq!(state.ingress_semaphore.available_permits(), 0);
        let refused = app(state.clone()).oneshot(raw("Widget")).await.unwrap();
        assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
        drop(response);
        assert_eq!(state.ingress_semaphore.available_permits(), 1);
        assert_eq!(app(state.clone()).oneshot(raw("Widget")).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn batch_cpu_lane_cannot_reserve_all_interactive_job_capacity() {
        let state = state(0);
        let first_batch = cpu_permits(&state, true).await.unwrap();
        let next_state = state.clone();
        let second_batch = tokio::spawn(async move { cpu_permits(&next_state, true).await });
        tokio::task::yield_now().await;
        assert!(!second_batch.is_finished());
        let interactive = tokio::time::timeout(Duration::from_secs(1), cpu_permits(&state, false))
            .await.expect("queued batch held the interactive reservation").unwrap();
        drop(interactive);
        drop(first_batch);
        drop(second_batch.await.unwrap().unwrap());
        assert_eq!(state.cpu_semaphore.available_permits(), 2);
    }

    #[tokio::test]
    async fn stalled_batch_upload_does_not_reserve_cpu_or_block_an_interactive_upload() {
        struct Stalled(Arc<std::sync::atomic::AtomicBool>);
        impl http_body::Body for Stalled {
            type Data = Bytes;
            type Error = std::convert::Infallible;
            fn poll_frame(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>)
                -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                std::task::Poll::Pending
            }
        }
        let state = state(0);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stalled = tokio::spawn(app(state.clone()).oneshot(axum::http::Request::post("/decompile/batch")
            .body(Body::new(Stalled(polled.clone()))).unwrap()));
        until(|| polled.load(std::sync::atomic::Ordering::SeqCst)).await;
        assert_eq!(state.cpu_semaphore.available_permits(), 2);
        let queued = tokio::spawn(app(state.clone()).oneshot(axum::http::Request::post("/decompile/batch")
            .header(CONTENT_TYPE, "application/json").body(Body::from(r#"{"scripts":[]}"#)).unwrap()));
        let response = tokio::time::timeout(Duration::from_secs(1), app(state.clone()).oneshot(raw("Widget")))
            .await.expect("interactive request blocked behind batch upload").unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        drop(response);
        assert!(!queued.is_finished());
        stalled.abort();
        assert!(stalled.await.unwrap_err().is_cancelled());
        assert_eq!(queued.await.unwrap().unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_full_batch_waiting_lane_leaves_interactive_queue_capacity() {
        let state = AppState::new(2, 2, Duration::from_secs(5), DEFAULT_UPLOAD_TIMEOUT)
            .with_ingress(4, 1);
        let batch_lane = state.batch_ingress_semaphore.clone().acquire_owned().await.unwrap();
        let batch_places = state.batch_queue_semaphore.available_permits();
        assert_eq!(batch_places, 2);
        let batch_request = || axum::http::Request::post("/decompile/batch")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"scripts":[]}"#)).unwrap();
        let mut waiting = Vec::new();
        for _ in 0..batch_places {
            waiting.push(tokio::spawn(app(state.clone()).oneshot(batch_request())));
        }
        until(|| state.batch_queue_semaphore.available_permits() == 0).await;
        assert_eq!(state.ingress_semaphore.available_permits(), 4);
        assert_eq!(state.queue_semaphore.available_permits(), 4);
        let excess = app(state.clone()).oneshot(batch_request()).await.unwrap();
        assert_eq!(excess.status(), StatusCode::SERVICE_UNAVAILABLE);
        let interactive = tokio::time::timeout(Duration::from_secs(1), app(state.clone()).oneshot(raw("Widget")))
            .await.expect("batch waiters consumed the interactive queue reservation").unwrap();
        assert_eq!(interactive.status(), StatusCode::OK);
        drop(interactive);
        for task in &waiting { task.abort(); }
        for task in waiting { assert!(task.await.unwrap_err().is_cancelled()); }
        assert_eq!(state.batch_queue_semaphore.available_permits(), batch_places);
        assert_eq!(state.queue_semaphore.available_permits(), 6);
        drop(batch_lane);
        assert_eq!(app(state.clone()).oneshot(batch_request()).await.unwrap().status(), StatusCode::OK);
    }

    #[test]
    fn json_and_binary_envelopes_enforce_the_same_script_name_limit() {
        let name = "x".repeat(MAX_NAME_LEN + 1);
        let body = serde_json::json!({"scripts":[{"script_name":name,"bytecode":"AQ=="}]}).to_string();
        assert!(matches!(parse_json_batch(&Bytes::from(body), DecompileOptions::default()), Err(Error::TooLarge(_))));
        let mut binary = b"MDB1".to_vec();
        binary.extend([1, 203, 0, 0]);
        binary.extend(1u32.to_le_bytes());
        binary.extend(((MAX_NAME_LEN + 1) as u32).to_le_bytes());
        assert!(matches!(parse_mdb1_batch(&Bytes::from(binary), DecompileOptions::default()), Err(Error::TooLarge(_))));
        let body = serde_json::json!({"scripts":[{"id":"x".repeat(MAX_ID_LEN + 1),"bytecode":"AQ=="}]}).to_string();
        assert!(matches!(parse_json_batch(&Bytes::from(body), DecompileOptions::default()), Err(Error::TooLarge(_))));
    }
}
