use futures_util::StreamExt;
extern crate console_error_panic_hook;

use base64::prelude::*;
use luau_lifter::{
    decompile_bytecode_with_script_name, try_decompile_bytecode_with_script_name,
};
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
                        let bytecode = BASE64_STANDARD
                            .decode(msg.encoded_bytecode)
                            .expect("bytecode must be base64 encoded");
                        let resp = DecompileResponse {
                            id: msg.id,
                            decompilation: decompile_bytecode_with_script_name(
                                &bytecode,
                                1,
                                msg.script_name.as_deref(),
                            ),
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
            match try_decompile_bytecode_with_script_name(
                &bytecode,
                CLIENT_KEY,
                script_name.as_deref(),
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
                        try_decompile_bytecode_with_script_name(
                            &bytecode,
                            key,
                            item.script_name.as_deref(),
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
