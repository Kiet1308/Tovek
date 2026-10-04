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
const MAX_SCRIPT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SCRIPT_NAME_BYTES: usize = 4 * 1024;
const MAX_ID_BYTES: usize = 1024;
const MAX_BATCH_ITEMS: usize = 50_000;
// The isolate has a smaller aggregate budget than the native server. These
// caps cover decoded input and the final JSON, including JSON string escaping.
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_DECODED_BATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESPONSE_METADATA_BYTES: usize = 4 * 1024 * 1024;
const RESPONSE_BUDGET_ERROR: &str = "batch response budget exceeded";

#[derive(Deserialize)]
struct DecompileMessage {
    id: String,
    #[serde(alias = "bytecode")]
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
    #[serde(alias = "bytecode")]
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
    decompile: impl FnMut(&[u8], Option<&str>) -> std::result::Result<String, String>,
) -> BatchResponse {
    decompile_batch_items_with_budget(scripts, reuse_results, decompile, MAX_SOURCE_BYTES, MAX_RESPONSE_BYTES)
}

fn decompile_batch_items_with_budget(
    scripts: Vec<BatchItem>, reuse_results: bool,
    mut decompile: impl FnMut(&[u8], Option<&str>) -> std::result::Result<String, String>,
    source_limit: usize, response_limit: usize,
) -> BatchResponse {
    use std::collections::{hash_map::Entry, HashMap};
    let mut contexts: HashMap<(String, Option<String>), usize> = HashMap::new();
    let mut key_bytes = 0usize;
    let mut results: Vec<BatchResultItem> = Vec::with_capacity(scripts.len());
    let fallback_sizes = scripts.iter().enumerate().map(|(index, item)| {
        json_length(&failed_item(index, item.id.clone().unwrap_or_else(|| index.to_string()), RESPONSE_BUDGET_ERROR))
            .saturating_add(1)
    }).collect::<Vec<_>>();
    let mut reserved = fallback_sizes.iter().sum::<usize>().saturating_add(128);
    let mut response_bytes = 0usize;
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
                    match (&previous.decompilation, &previous.error) {
                        (Some(source), _) => Ok(source.clone()),
                        (_, Some(error)) => Err(error.clone()),
                        _ => Err("decompile result unavailable".into()),
                    }
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
        let mut row = match outcome {
            Ok(source) if source.len() <= source_limit => BatchResultItem {
                index, id, ok: true, decompilation: Some(source.into_boxed_str().into_string()), error: None,
            },
            Ok(_) => failed_item(index, id, "decompiled source exceeds the output limit"),
            Err(reason) => {
                let mut reason = reason;
                truncate_text(&mut reason, 4096);
                failed_item(index, id, &reason)
            },
        };
        reserved = reserved.saturating_sub(fallback_sizes[index]);
        let mut bytes = json_length(&row).saturating_add(1);
        if bytes > response_limit.saturating_sub(response_bytes).saturating_sub(reserved) {
            row = failed_item(index, row.id, RESPONSE_BUDGET_ERROR);
            bytes = json_length(&row).saturating_add(1);
        }
        response_bytes = response_bytes.saturating_add(bytes);
        results.push(row);
    }
    BatchResponse {
        count: results.len(),
        ok_count: results.iter().filter(|result| result.ok).count(),
        results,
    }
}

fn failed_item(index: usize, id: String, error: &str) -> BatchResultItem {
    BatchResultItem { index, id, ok: false, decompilation: None, error: Some(error.into()) }
}

fn truncate_text(text: &mut String, maximum: usize) {
    if text.len() <= maximum { return; }
    let mut end = maximum;
    while !text.is_char_boundary(end) { end -= 1; }
    text.truncate(end);
}

fn json_length(value: &impl Serialize) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len()); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut counter = Counter(0);
    if serde_json::to_writer(&mut counter, value).is_err() { return usize::MAX; }
    counter.0
}

fn encoded_decoded_size(encoded: &str) -> usize {
    let padding = encoded.as_bytes().iter().rev().take_while(|&&byte| byte == b'=').take(2).count();
    encoded.len().div_ceil(4).saturating_mul(3).saturating_sub(padding)
}

fn validate_item(encoded: &str, name: Option<&str>, id: Option<&str>) -> std::result::Result<usize, String> {
    if name.is_some_and(|name| name.len() > MAX_SCRIPT_NAME_BYTES) {
        return Err(format!("script name exceeds {MAX_SCRIPT_NAME_BYTES} bytes"));
    }
    if id.is_some_and(|id| id.len() > MAX_ID_BYTES) {
        return Err(format!("script id exceeds {MAX_ID_BYTES} bytes"));
    }
    let decoded = encoded_decoded_size(encoded);
    if decoded > MAX_SCRIPT_BYTES { return Err(format!("bytecode exceeds {MAX_SCRIPT_BYTES} bytes")); }
    Ok(decoded)
}

fn validate_batch(scripts: &[BatchItem]) -> std::result::Result<(), String> {
    validate_batch_with_limits(scripts, MAX_DECODED_BATCH_BYTES, MAX_RESPONSE_METADATA_BYTES)
}

fn validate_batch_with_limits(scripts: &[BatchItem], decoded_limit: usize, metadata_limit: usize) -> std::result::Result<(), String> {
    if scripts.len() > MAX_BATCH_ITEMS { return Err(format!("too many scripts (max {MAX_BATCH_ITEMS})")); }
    let mut decoded = 0usize;
    let mut metadata = 128usize;
    for (index, item) in scripts.iter().enumerate() {
        decoded = decoded.saturating_add(validate_item(&item.encoded_bytecode, item.script_name.as_deref(), item.id.as_deref())?);
        if decoded > decoded_limit { return Err(format!("decoded batch exceeds {decoded_limit} bytes")); }
        metadata = metadata.saturating_add(json_length(&failed_item(index,
            item.id.clone().unwrap_or_else(|| index.to_string()), RESPONSE_BUDGET_ERROR))).saturating_add(1);
        if metadata > metadata_limit {
            return Err(format!("batch response metadata exceeds {metadata_limit} bytes"));
        }
    }
    Ok(())
}

async fn bounded_body(req: &mut Request, maximum: usize) -> std::result::Result<Vec<u8>, (String, u16)> {
    if req.headers().get("Content-Length").ok().flatten()
        .and_then(|length| length.parse::<u64>().ok()).is_some_and(|length| length > maximum as u64)
    { return Err((format!("request body exceeds {maximum} bytes"), 413)); }
    let mut stream = req.stream().map_err(|error| (error.to_string(), 400))?;
    let mut body = Vec::new();
    while let Some(frame) = stream.next().await {
        let frame = frame.map_err(|error| (error.to_string(), 400))?;
        let required = body.len().checked_add(frame.len()).filter(|length| *length <= maximum)
            .ok_or_else(|| (format!("request body exceeds {maximum} bytes"), 413))?;
        if required > body.capacity() {
            let capacity = body.capacity().saturating_mul(2).max(required).min(maximum);
            body.reserve_exact(capacity - body.len());
        }
        body.extend_from_slice(&frame);
    }
    Ok(body)
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

    #[test]
    fn websocket_bad_json_base64_and_flags_do_not_prevent_later_good_messages() {
        let good = |id: &str| serde_json::json!({"id": id, "encoded_bytecode": "AQ==", "script_name": "Widget"}).to_string();
        let mut calls = 0;
        let mut decompile = |bytes: &[u8], name: Option<&str>, options: DecompileOptions| {
            calls += 1;
            assert_eq!(bytes, [1]);
            assert_eq!(name, Some("Widget"));
            assert!(options.dont_reuse_var);
            Ok("return 7".to_string())
        };
        let options = DecompileOptions { dont_reuse_var: true, ..DecompileOptions::default() };
        let texts = [good("before"), "{bad".into(),
            r#"{"id":"bad-base64","encoded_bytecode":"!"}"#.into(),
            r#"{"id":"bad-flags","encoded_bytecode":"AQ==","flags":"unknown"}"#.into(), good("after")];
        let results = texts.iter().map(|text| decompile_ws_text(text, options, &mut decompile)).collect::<Vec<_>>();
        assert_eq!(calls, 2);
        assert_eq!(results[0].decompilation, "return 7");
        assert_eq!(results[1].id, "");
        assert_eq!(results[2].id, "bad-base64");
        assert_eq!(results[3].id, "bad-flags");
        assert_eq!(results[4].id, "after");
        assert_eq!(results[4].decompilation, "return 7");
        assert!(results[1..4].iter().all(|result| result.decompilation.starts_with("-- decompile failed:")));
    }

    #[test]
    fn envelope_caps_cover_names_ids_padding_and_aggregate_decoded_input() {
        for count in 0..32 {
            let encoded = BASE64_STANDARD.encode(vec![1; count]);
            assert_eq!(encoded_decoded_size(&encoded), count);
        }
        assert!(validate_item("AQ==", Some(&"x".repeat(MAX_SCRIPT_NAME_BYTES + 1)), None).is_err());
        assert!(validate_item("AQ==", None, Some(&"x".repeat(MAX_ID_BYTES + 1))).is_err());
        let inputs = vec![item(&[1, 2], None, None), item(&[1, 2], None, None)];
        assert!(validate_batch_with_limits(&inputs, 4, 1024).is_ok());
        assert!(validate_batch_with_limits(&inputs, 3, 1024).unwrap_err().contains("decoded"));
        assert!(validate_batch_with_limits(&inputs, 4, 128).unwrap_err().contains("metadata"));
    }

    #[test]
    fn response_budget_counts_escaping_and_duplicate_results_without_losing_good_neighbours() {
        let input = || vec![item(&[1], None, None), item(&[2], None, None), item(&[1], None, None)];
        let mut calls = 0;
        let response = decompile_batch_items_with_budget(input(), true, |bytes, _| {
            calls += 1;
            Ok(if bytes == [2] { "\n".repeat(1000) } else { "return 7".into() })
        }, 4096, 500);
        assert_eq!(calls, 2);
        assert!(json_length(&response) <= 500);
        assert_eq!(response.ok_count, 2);
        assert_eq!(response.results[1].error.as_deref(), Some(RESPONSE_BUDGET_ERROR));
        assert_eq!(response.results[2].decompilation.as_deref(), Some("return 7"));
        let response = decompile_batch_items_with_budget(input(), false, |_, _| Ok("x".repeat(17)), 16, 500);
        assert_eq!(response.ok_count, 0);
        assert!(response.results.iter().all(|result| result.error.as_ref().unwrap().contains("output limit")));
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

fn ws_failure(id: String, reason: impl AsRef<str>) -> DecompileResponse {
    let mut reason = reason.as_ref().to_string();
    truncate_text(&mut reason, 4096);
    DecompileResponse { id, decompilation: format!("-- decompile failed: {reason}") }
}

/// Transport errors stay inside one message. With malformed JSON there is no
/// trustworthy correlation id, so the error uses the empty id; later messages
/// on the socket remain usable.
fn decompile_ws_text(
    text: &str, header_options: DecompileOptions,
    mut decompile: impl FnMut(&[u8], Option<&str>, DecompileOptions) -> std::result::Result<String, String>,
) -> DecompileResponse {
    if text.len() > MAX_REQUEST_BYTES { return ws_failure(String::new(), "websocket message exceeds the request limit"); }
    let message: DecompileMessage = match serde_json::from_str(text) {
        Ok(message) => message,
        Err(error) => return ws_failure(String::new(), format!("invalid JSON message: {error}")),
    };
    if message.id.len() > MAX_ID_BYTES { return ws_failure(String::new(), "script id exceeds the limit"); }
    if let Err(error) = validate_item(&message.encoded_bytecode, message.script_name.as_deref(), Some(&message.id)) {
        return ws_failure(message.id, error);
    }
    let options = match body_options(message.flags.as_deref(), message.dont_reuse_var) {
        Ok(options) => header_options.union(options),
        Err(error) => return ws_failure(message.id, error),
    };
    let bytecode = match BASE64_STANDARD.decode(&message.encoded_bytecode) {
        Ok(bytecode) => bytecode,
        Err(error) => return ws_failure(message.id, format!("base64: {error}")),
    };
    let source = match decompile(&bytecode, message.script_name.as_deref(), options) {
        Ok(source) if source.len() <= MAX_SOURCE_BYTES => source,
        Ok(_) => return ws_failure(message.id, "decompiled source exceeds the output limit"),
        Err(error) => return ws_failure(message.id, error),
    };
    let response = DecompileResponse { id: message.id, decompilation: source };
    if json_length(&response) > MAX_RESPONSE_BYTES { return ws_failure(response.id, "JSON response exceeds the output limit"); }
    response
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
                let mut event_stream = match server.events() {
                    Ok(stream) => stream,
                    Err(_) => { let _ = server.close(Some(1011), Some("could not open stream")); return; }
                };
                while let Some(event) = event_stream.next().await {
                    let event = match event { Ok(event) => event, Err(_) => break };
                    if let WebsocketEvent::Message(msg) = event {
                        let response = match msg.text() {
                            Some(text) => decompile_ws_text(&text, header_options, |bytes, name, options| {
                                // Preserve the existing WebSocket decode-key contract.
                                try_decompile_bytecode_with_options(bytes, 1, name, options)
                            }),
                            None => ws_failure(String::new(), "websocket message must be JSON text"),
                        };
                        let encoded = match serde_json::to_string(&response) { Ok(encoded) => encoded, Err(_) => break };
                        if server.send_with_str(encoded).is_err() { break; }
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
            if let Err(error) = validate_item("", script_name.as_deref(), None) {
                return Response::error(error, 413);
            }
            let options = match request_options(&req) {
                Ok(options) => options,
                Err(e) => return Response::error(e, 400),
            };
            // RAW: when the caller declares octet-stream, the body IS the bytecode
            // (no base64). Otherwise decode base64 as before. Either way, key 203.
            let raw = is_octet_stream(&req);
            let body = match bounded_body(&mut req, MAX_REQUEST_BYTES).await {
                Ok(body) => body, Err((error, status)) => return Response::error(error, status),
            };
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
                Ok(source) if source.len() <= MAX_SOURCE_BYTES => Response::ok(source),
                Ok(_) => Response::error("decompiled source exceeds the output limit", 413),
                Err(reason) => Response::error(format!("decompile failed: {reason}"), 422),
            }
        })
        .post_async("/decompile_batch", |mut req, ctx| async move {
            if let Some(response) = authorize(&req, &ctx.env)? {
                return Ok(response);
            }

            let body = match bounded_body(&mut req, MAX_REQUEST_BYTES).await {
                Ok(body) => body, Err((error, status)) => return Response::error(error, status),
            };
            let request: BatchRequest = match serde_json::from_slice(&body) {
                Ok(request) => request,
                Err(e) => return Response::error(format!("invalid JSON batch: {e}"), 400),
            };
            if let Err(error) = validate_batch(&request.scripts) { return Response::error(error, 413); }
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
            if json_length(&response) > MAX_RESPONSE_BYTES {
                return Response::error("batch response exceeds the output limit", 413);
            }
            Response::from_json(&response)
        })
        .run(req, env)
        .await
}
#[cfg(all(target_arch = "wasm32", panic = "abort"))]
compile_error!("Build the Worker with worker-build --panic-unwind; abort cannot isolate batch items.");
