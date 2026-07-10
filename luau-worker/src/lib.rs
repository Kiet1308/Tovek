use futures_util::StreamExt;
extern crate console_error_panic_hook;

use base64::prelude::*;
use luau_lifter::{try_decompile_bytecode_with_options, DecompileOptions};
use serde::{Deserialize, Serialize};
use worker::*;

const AUTH_SECRET: &str = "ymjKH2O3BbO3bDSsKmpo3ek3vHxIWYLQfj0";

/// Roblox client bytecode decode key (`op = op * key % 256`).
const CLIENT_KEY: u8 = 203;

#[derive(Deserialize)]
struct DecompileMessage {
    id: String,
    encoded_bytecode: String,
    #[serde(default, alias = "scriptName")]
    script_name: Option<String>,
    #[serde(default, alias = "dontReuseVar")]
    dont_reuse_var: bool,
    #[serde(default)]
    flags: Option<String>,
}

#[derive(Serialize)]
struct DecompileResponse {
    id: String,
    decompilation: String,
}

/// One script in a `POST /decompile_batch` request.
#[derive(Deserialize)]
struct BatchItem {
    /// Optional client-chosen correlation token, echoed back (defaults to the index).
    #[serde(default)]
    id: Option<String>,
    /// base64-encoded bytecode.
    encoded_bytecode: String,
    #[serde(default, alias = "scriptName")]
    script_name: Option<String>,
}

#[derive(Deserialize)]
struct BatchRequest {
    /// Decode key for every script (default [`CLIENT_KEY`]).
    #[serde(default)]
    key: Option<u8>,
    #[serde(default, alias = "dontReuseVar")]
    dont_reuse_var: bool,
    #[serde(default)]
    flags: Option<String>,
    scripts: Vec<BatchItem>,
}

#[derive(Serialize)]
struct BatchResultItem {
    /// Zero-based input position (matches the web-server's batch schema, so a
    /// client can correlate results by index regardless of backend).
    index: usize,
    id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    decompilation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct BatchResponse {
    count: usize,
    ok_count: usize,
    results: Vec<BatchResultItem>,
}

/// Essence-based `application/octet-stream` detection (tolerates `; charset=...`).
fn is_octet_stream(req: &Request) -> bool {
    req.headers()
        .get("Content-Type")
        .ok()
        .flatten()
        .map(|ct| {
            ct.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/octet-stream")
        })
        .unwrap_or(false)
}

fn request_options(req: &Request) -> std::result::Result<DecompileOptions, String> {
    let mut options = DecompileOptions::default();
    if let Some(flags) = req.headers().get("X-Decompile-Flags").ok().flatten() {
        options = options.union(parse_flags_text(&flags)?);
    }
    if let Some(value) = req.headers().get("X-Dont-Reuse-Var").ok().flatten() {
        if parse_bool(&value, "X-Dont-Reuse-Var")? {
            options.dont_reuse_var = true;
        }
    }
    Ok(options)
}

fn body_options(
    flags: Option<&str>,
    dont_reuse_var: bool,
) -> std::result::Result<DecompileOptions, String> {
    let mut options = match flags {
        Some(flags) => parse_flags_text(flags)?,
        None => DecompileOptions::default(),
    };
    if dont_reuse_var {
        options.dont_reuse_var = true;
    }
    Ok(options)
}

fn parse_flags_text(raw: &str) -> std::result::Result<DecompileOptions, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(DecompileOptions::default());
    }
    if let Ok(bits) = raw.parse::<u32>() {
        return DecompileOptions::from_flag_bits(bits)
            .ok_or_else(|| format!("unsupported decompile flag bits: {bits}"));
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
            _ => return Err(format!("unsupported decompile flag: {token}")),
        }
    }
    Ok(options)
}

fn parse_bool(raw: &str, field: &str) -> std::result::Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{field} must be a boolean (true/false)")),
    }
}

#[event(fetch, respond_with_errors)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();
    router
        .get_async("/decompile_ws", |req, _ctx| async move {
            // A missing/invalid Authorization header must be a clean 403, not a
            // panicked 500 — so read it without `.expect()`.
            let license = req
                .headers()
                .get("Authorization")
                .ok()
                .flatten()
                .unwrap_or_default();

            if license != AUTH_SECRET {
                return Response::error("invalid license", 403);
            }

            let header_options = match request_options(&req) {
                Ok(options) => options,
                Err(e) => return Response::error(e, 400),
            };

            let pair = WebSocketPair::new()?;
            let server = pair.server;
            server.accept()?;

            wasm_bindgen_futures::spawn_local(async move {
                let mut event_stream = server.events().expect("could not open stream");
                while let Some(event) = event_stream.next().await {
                    if let WebsocketEvent::Message(msg) =
                        event.expect("received error in websocket")
                    {
                        let msg = msg
                            .json::<DecompileMessage>()
                            .expect("malformed decompile message");
                        let message_options =
                            match body_options(msg.flags.as_deref(), msg.dont_reuse_var) {
                                Ok(options) => options,
                                Err(e) => {
                                    let resp = DecompileResponse {
                                        id: msg.id,
                                        decompilation: format!("-- decompile failed: {e}"),
                                    };
                                    server
                                        .send_with_str(serde_json::to_string(&resp).unwrap())
                                        .unwrap();
                                    continue;
                                }
                            };
                        let options = header_options.union(message_options);
                        let bytecode = BASE64_STANDARD
                            .decode(msg.encoded_bytecode)
                            .expect("bytecode must be base64 encoded");
                        let decompilation = try_decompile_bytecode_with_options(
                            &bytecode,
                            1,
                            msg.script_name.as_deref(),
                            options,
                        )
                        .unwrap_or_else(|reason| format!("-- decompile failed: {reason}"));
                        let resp = DecompileResponse {
                            id: msg.id,
                            decompilation,
                        };
                        server
                            .send_with_str(serde_json::to_string(&resp).unwrap())
                            .unwrap();
                    }
                }
            });

            Response::from_websocket(pair.client)
        })
        .post_async("/decompile", |mut req, _ctx| async move {
            // A missing/invalid Authorization header must be a clean 403, not a
            // panicked 500 — so read it without `.expect()`.
            let license = req
                .headers()
                .get("Authorization")
                .ok()
                .flatten()
                .unwrap_or_default();

            if license != AUTH_SECRET {
                return Response::error("invalid license", 403);
            }

            let script_name = req.headers().get("X-Script-Name").ok().flatten();
            let options = match request_options(&req) {
                Ok(options) => options,
                Err(e) => return Response::error(e, 400),
            };
            // RAW: when the caller declares octet-stream, the body IS the bytecode
            // (no base64). Otherwise decode base64 as before. Either way, key 203.
            let raw = is_octet_stream(&req);
            let body = req.bytes().await?;
            let bytecode = if raw {
                body
            } else {
                match BASE64_STANDARD.decode(body) {
                    Ok(bytecode) => bytecode,
                    Err(_) => return Response::error("invalid bytecode", 400),
                }
            };
            // `try_*` so malformed bytecode is a clean 422, not a panicked 500.
            match try_decompile_bytecode_with_options(
                &bytecode,
                CLIENT_KEY,
                script_name.as_deref(),
                options,
            ) {
                Ok(source) => Response::ok(source),
                Err(reason) => Response::error(format!("decompile failed: {reason}"), 422),
            }
        })
        .post_async("/decompile_batch", |mut req, _ctx| async move {
            // A missing/invalid Authorization header must be a clean 403, not a
            // panicked 500 — so read it without `.expect()`.
            let license = req
                .headers()
                .get("Authorization")
                .ok()
                .flatten()
                .unwrap_or_default();

            if license != AUTH_SECRET {
                return Response::error("invalid license", 403);
            }

            let body = req.bytes().await?;
            let request: BatchRequest = match serde_json::from_slice(&body) {
                Ok(request) => request,
                Err(e) => return Response::error(format!("invalid JSON batch: {e}"), 400),
            };
            let key = request.key.unwrap_or(CLIENT_KEY);
            let header_options = match request_options(&req) {
                Ok(options) => options,
                Err(e) => return Response::error(e, 400),
            };
            let body_options = match body_options(request.flags.as_deref(), request.dont_reuse_var)
            {
                Ok(options) => options,
                Err(e) => return Response::error(e, 400),
            };
            let options = header_options.union(body_options);

            // Single-threaded wasm: decompile sequentially. One bad script becomes a
            // per-item error (via the `try_*` Result path) rather than aborting the
            // whole batch.
            let mut results = Vec::with_capacity(request.scripts.len());
            for (index, item) in request.scripts.into_iter().enumerate() {
                let id = item.id.unwrap_or_else(|| index.to_string());
                let outcome = BASE64_STANDARD
                    .decode(item.encoded_bytecode.as_bytes())
                    .map_err(|e| format!("base64: {e}"))
                    .and_then(|bytecode| {
                        try_decompile_bytecode_with_options(
                            &bytecode,
                            key,
                            item.script_name.as_deref(),
                            options,
                        )
                    });
                results.push(match outcome {
                    Ok(source) => BatchResultItem {
                        index,
                        id,
                        ok: true,
                        decompilation: Some(source),
                        error: None,
                    },
                    Err(reason) => BatchResultItem {
                        index,
                        id,
                        ok: false,
                        decompilation: None,
                        error: Some(reason),
                    },
                });
            }

            let ok_count = results.iter().filter(|r| r.ok).count();
            Response::from_json(&BatchResponse {
                count: results.len(),
                ok_count,
                results,
            })
        })
        .run(req, env)
        .await
}
