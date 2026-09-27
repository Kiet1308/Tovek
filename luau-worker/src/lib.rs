use futures_util::StreamExt;
extern crate console_error_panic_hook;

use base64::prelude::*;
use luau_lifter::{
    try_decompile_bytecode_with_options, ControlFlowOutputPolicy, DecompileOptions,
};
use serde::{Deserialize, Serialize};
use worker::*;


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

const MAX_BATCH_REUSE_ENTRIES: usize = 1024;
const MAX_BATCH_REUSE_KEY_BYTES: usize = 4 * 1024 * 1024;

/// Retain only bounded, request-local input keys. Results already belong to the
/// response; duplicate entries copy their payload from the original result.
fn decompile_batch_items(
    scripts: Vec<BatchItem>,
    reuse_results: bool,
    mut decompile: impl FnMut(&[u8], Option<&str>) -> std::result::Result<String, String>,
) -> BatchResponse {
    use std::collections::{hash_map::Entry, HashMap};
    let mut contexts: HashMap<(String, Option<String>), usize> = HashMap::new();
    let mut key_bytes = 0usize;
    let mut results: Vec<BatchResultItem> = Vec::with_capacity(scripts.len());
    let mut execute = |encoded: &str, script_name: Option<&str>| {
        BASE64_STANDARD.decode(encoded.as_bytes())
            .map_err(|error| format!("base64: {error}"))
            .and_then(|bytecode| decompile(&bytecode, script_name))
    };
    for (index, item) in scripts.into_iter().enumerate() {
        let id = item.id.unwrap_or_else(|| index.to_string());
        let outcome = if reuse_results {
            let retained_bytes = item.encoded_bytecode.capacity()
                .saturating_add(item.script_name.as_ref().map_or(0, String::capacity));
            let can_retain = contexts.len() < MAX_BATCH_REUSE_ENTRIES
                && retained_bytes <= MAX_BATCH_REUSE_KEY_BYTES.saturating_sub(key_bytes);
            // Exact encoded text and the entire script name are conservative
            // context keys. Decode key and options are shared by this batch.
            // Hash collisions still compare both strings; nothing is interned
            // or retained beyond this request.
            match contexts.entry((item.encoded_bytecode, item.script_name)) {
                Entry::Occupied(entry) => {
                    let previous = &results[*entry.get()];
                    results.push(BatchResultItem {
                        index, id, ok: previous.ok,
                        decompilation: previous.decompilation.clone(),
                        error: previous.error.clone(),
                    });
                    continue;
                }
                Entry::Vacant(entry) => {
                    let (encoded, script_name) = entry.key();
                    let outcome = execute(encoded, script_name.as_deref());
                    if can_retain {
                        entry.insert(index);
                        key_bytes += retained_bytes;
                    }
                    outcome
                }
            }
        } else {
            execute(&item.encoded_bytecode, item.script_name.as_deref())
        };
        results.push(match outcome {
            Ok(source) => BatchResultItem {
                index, id, ok: true, decompilation: Some(source), error: None,
            },
            Err(reason) => BatchResultItem {
                index, id, ok: false, decompilation: None, error: Some(reason),
            },
        });
    }
    BatchResponse {
        count: results.len(),
        ok_count: results.iter().filter(|result| result.ok).count(),
        results,
    }
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
            "STRICT_NO_SYNTHETIC_CONTROL" => {
                options.control_flow_policy = ControlFlowOutputPolicy::StrictNoSyntheticControl;
            }
            _ => return Err(format!("unsupported decompile flag: {token}")),
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use luau_lifter::{ControlFlowOutputPolicy, STRICT_NO_SYNTHETIC_CONTROL};

    #[test]
    fn worker_accepts_strict_control_policy_by_name_and_bits() {
        let named = parse_flags_text("strict-no-synthetic-control").unwrap();
        assert_eq!(
            named.control_flow_policy,
            ControlFlowOutputPolicy::StrictNoSyntheticControl
        );
        let numeric = parse_flags_text(&STRICT_NO_SYNTHETIC_CONTROL.to_string()).unwrap();
        assert_eq!(numeric, named);
    }

    fn item(bytes: &[u8], name: Option<&str>, id: Option<&str>) -> BatchItem {
        BatchItem {
            id: id.map(str::to_owned),
            encoded_bytecode: BASE64_STANDARD.encode(bytes),
            script_name: name.map(str::to_owned),
        }
    }

    #[test]
    fn batch_reuse_preserves_order_ids_errors_and_complete_context() {
        let inputs = || vec![
            item(&[1], Some("Folder/A"), Some("first")),
            item(&[1], Some("Folder/A"), None),
            item(&[1], Some("Other/A"), Some("same module hint, distinct key")),
            item(&[1], None, None),
            item(&[1], Some(""), None),
            item(&[2], Some("Folder/A"), Some("failed")),
            item(&[2], Some("Folder/A"), Some("failed duplicate")),
            BatchItem { id: None, encoded_bytecode: "!".into(), script_name: None },
            BatchItem { id: Some("bad base64".into()), encoded_bytecode: "!".into(), script_name: None },
            item(&[1], Some("Folder/A"), Some("last")),
        ];
        let outcome = |bytes: &[u8], name: Option<&str>| {
            if bytes == [2] { Err("decompiler error".to_string()) }
            else { Ok(format!("{bytes:?}/{name:?}")) }
        };
        let mut fresh_calls = 0;
        let fresh = decompile_batch_items(inputs(), false, |bytes, name| {
            fresh_calls += 1;
            outcome(bytes, name)
        });
        let mut reused_calls = 0;
        let reused = decompile_batch_items(inputs(), true, |bytes, name| {
            reused_calls += 1;
            outcome(bytes, name)
        });
        assert_eq!(fresh_calls, 8);
        assert_eq!(reused_calls, 5);
        assert_eq!(serde_json::to_value(&fresh).unwrap(), serde_json::to_value(&reused).unwrap());
        assert_eq!(reused.count, 10);
        assert_eq!(reused.ok_count, 6);
        assert_eq!(reused.results[1].id, "1");
        assert_eq!(reused.results[6].id, "failed duplicate");
        assert_eq!(reused.results[9].index, 9);
        assert_eq!(reused.results[9].id, "last");
    }

    #[test]
    fn batch_reuse_is_bounded_by_entries_and_keeps_admitted_hits() {
        let mut inputs = (0..=MAX_BATCH_REUSE_ENTRIES)
            .map(|index| item(&index.to_le_bytes(), None, None)).collect::<Vec<_>>();
        inputs.push(item(&0usize.to_le_bytes(), None, None));
        inputs.push(item(&MAX_BATCH_REUSE_ENTRIES.to_le_bytes(), None, None));
        let mut calls = 0;
        let response = decompile_batch_items(inputs, true, |bytes, _| {
            calls += 1;
            Ok(BASE64_STANDARD.encode(bytes))
        });
        assert_eq!(calls, MAX_BATCH_REUSE_ENTRIES + 2);
        assert_eq!(response.count, MAX_BATCH_REUSE_ENTRIES + 3);
        assert_eq!(response.ok_count, response.count);
    }

    #[test]
    fn batch_reuse_budgets_retained_capacity_and_drops_keys_between_requests() {
        let reserved = |byte, capacity| {
            let mut input = item(&[byte], None, None);
            let mut encoded = String::with_capacity(capacity);
            encoded.push_str(&input.encoded_bytecode);
            input.encoded_bytecode = encoded;
            input
        };
        // These short keys reserve far more than their lengths. One admitted
        // key consumes half the byte budget; the second exceeds the remainder.
        let inputs = vec![
            reserved(1, MAX_BATCH_REUSE_KEY_BYTES / 2),
            reserved(2, MAX_BATCH_REUSE_KEY_BYTES / 2 + 1),
            item(&[1], None, None),
            reserved(2, MAX_BATCH_REUSE_KEY_BYTES / 2 + 1),
        ];
        let mut calls = 0;
        let response = decompile_batch_items(inputs, true, |_, _| { calls += 1; Ok("ok".into()) });
        assert_eq!(calls, 3);
        assert_eq!(response.ok_count, 4);
        let oversized = vec![reserved(3, MAX_BATCH_REUSE_KEY_BYTES + 1), reserved(3, MAX_BATCH_REUSE_KEY_BYTES + 1)];
        decompile_batch_items(oversized, true, |_, _| { calls += 1; Ok("ok".into()) });
        assert_eq!(calls, 5);
        decompile_batch_items(vec![item(&[1], None, None), item(&[1], None, None)], true,
            |_, _| { calls += 1; Ok("new request".into()) });
        assert_eq!(calls, 6);
        decompile_batch_items(vec![item(&[1], None, None), item(&[1], None, None)], false,
            |_, _| { calls += 1; Ok("fresh diagnostic execution".into()) });
        assert_eq!(calls, 8);
    }
}

fn parse_bool(raw: &str, field: &str) -> std::result::Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{field} must be a boolean (true/false)")),
    }
}

fn authorize(req: &Request, env: &Env) -> Result<Option<Response>> {
    let secret = match env.secret("AUTH_SECRET") {
        Ok(secret) => secret.to_string(),
        Err(_) => return Response::error("authentication is not configured", 503).map(Some),
    };
    if secret.is_empty() {
        return Response::error("authentication is not configured", 503).map(Some);
    }
    let provided = req.headers().get("Authorization")?.unwrap_or_default();
    if provided != secret {
        return Response::error("invalid license", 403).map(Some);
    }
    Ok(None)
}

#[event(fetch, respond_with_errors)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();
    router
        .get_async("/decompile_ws", |req, ctx| async move {
            if let Some(response) = authorize(&req, &ctx.env)? {
                return Ok(response);
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
        .post_async("/decompile", |mut req, ctx| async move {
            if let Some(response) = authorize(&req, &ctx.env)? {
                return Ok(response);
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
        .post_async("/decompile_batch", |mut req, ctx| async move {
            if let Some(response) = authorize(&req, &ctx.env)? {
                return Ok(response);
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
            let reuse_results = request.scripts.len() > 1
                && !luau_lifter::requires_fresh_decompilation();
            let response = decompile_batch_items(request.scripts, reuse_results, |bytecode, script_name| {
                try_decompile_bytecode_with_options(bytecode, key, script_name, options)
            });
            Response::from_json(&response)
        })
        .run(req, env)
        .await
}
#[cfg(all(target_arch = "wasm32", panic = "abort"))]
compile_error!("Build the Worker with worker-build --panic-unwind; abort cannot isolate batch items.");
