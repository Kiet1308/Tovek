# Worker adapter

Build and authentication instructions are in the root README. The Worker keeps
the existing routes and output schemas:

| Route | Input | Decode key | Output |
| --- | --- | ---: | --- |
| `POST /decompile` | Raw bytecode with `application/octet-stream`, otherwise base64 | 203 | Plaintext source |
| `POST /decompile_batch` | JSON `scripts` array, accepting `encoded_bytecode` or `bytecode` | Request `key`, default 203 | Ordered per-item JSON results |
| `GET /decompile_ws` | WebSocket JSON text messages | 1 | `{ "id", "decompilation" }` messages |

`AUTH_SECRET` and the `Authorization` header retain their existing behavior.
Header options and message/batch options are combined as before. Script names
remain part of decompilation context. The WebSocket key is intentionally the
existing key 1, while the HTTP default is the Roblox client key 203.

## Malformed WebSocket messages

Malformed JSON, invalid base64, unsupported flags, binary frames and oversized
messages receive an error comment in the existing `decompilation` field. A
valid id is echoed when the message can be parsed; malformed JSON or an
oversized id uses the empty id. A subsequent valid message on the same socket
can still succeed. Transport or send failure stops that connection's loop
without unwrapping or panicking.

## Input and output budgets

The Worker uses the same per-script, name and id limits as the native server,
with smaller aggregate budgets for the isolate. Lengths are bytes.

| Resource | Limit |
| --- | ---: |
| HTTP request body or one WebSocket text message | 16 MiB |
| Scripts per batch | 50,000 |
| One decoded script | 16 MiB |
| One script name | 4 KiB |
| One correlation id | 1 KiB |
| Aggregate decoded batch input | 16 MiB |
| One generated source | 16 MiB |
| Batch metadata and fallback error envelopes | 4 MiB |
| Serialized batch or WebSocket JSON response | 32 MiB |

HTTP bodies are read incrementally with a byte limit, including when the
request lacks `Content-Length`. The encoded request limit also applies to
base64/JSON, so the maximum raw script is larger than the maximum script that
fits in a base64 envelope. Invalid framing returns `400`; input limit
violations return `413`. Invalid bytecode in the single HTTP route returns
`422`, and invalid items inside a valid batch produce item errors.

Batch output accounting includes JSON string escaping and reserves a minimal
error row for every later input. A source that exceeds its individual or the
remaining aggregate output budget becomes an item error while preserving
neighboring results and order. Request-local reuse still avoids reprocessing
identical scripts with identical naming context, subject to its existing
bounded input-key map; diagnostics that need fresh execution bypass reuse.
No cross-request source cache is installed in the Worker.

These limits cover protocol payloads and retained response text, not all
runtime memory. Parsing, final response serialization and the engine's AST or
formatter may allocate additional working buffers. Execution remains sequential
within a Worker batch; native server CPU admission and Rayon scheduling do not
apply here.

## Verification

Run `cargo test -p luau-worker` for pure Rust protocol helpers, reuse and budget
tests. After an unwind-enabled Worker build, run the root
`scripts/check_worker_runtime.mjs` with its documented build/tool arguments. It
uses local workerd through Miniflare to check authorization, malformed input,
an isolated decompiler panic, integer output and a valid–malformed–valid
WebSocket sequence. It does not call a deployed Worker.
