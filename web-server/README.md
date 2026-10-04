# Native HTTP server

The server listens on `127.0.0.1:3000`. It supports the existing plaintext
single-script routes and the JSON/MDB1 batch protocol described in the root
README. These adapter changes improve overload behavior, duplicate throughput
and memory accounting. A cache hit or a batch throughput gain is not evidence
of faster decompilation of one fresh script.

## Admission and cancellation

Uploads and completed response buffers share eight ingress reservations. At
most two reservations belong to batch requests. Waiting requests leave their
request body unread; the shared queue contains at most eight running requests
plus `TOVEK_QUEUE_LIMIT` waiting places. Batches may occupy two running places
plus half of the configured waiting places, preserving waiting capacity for
interactive requests as well as ingress capacity.

CPU admission is separate: up to four decompiler jobs run at once, and at most
two of those jobs are batch quanta. A quantum contains at most eight scripts;
the batch returns to the CPU queue between quanta. This reserves job admission
for interactive traffic. It does not reserve physical cores: decompiler jobs
still share the process-wide Rayon pool, and an individual script can use
parallel core passes.

Whole-request duplicate reuse remains active with the optional process cache
disabled. The core computes its existing bytecode/key/normalized module-context
plan once; later quanta reuse representative results while keeping each input's
id, name and position. Saved payloads are released at their last use and have a
64 MiB text budget. A result that cannot fit is returned normally without being
saved; a later duplicate may recompute. Diagnostics bypass this plan, and transient CPU admission or
worker failures are not saved for later quanta.

Input decoding, optional cache-key preparation and bounded JSON encoding run on
blocking workers. An upload or a cache follower consumes no CPU permit. A
blocking job retains its reservations if the HTTP requester disconnects, and
a successful response retains its ingress reservation until its body is
consumed or dropped. A disconnected singleflight leader can finish for its
remaining followers. This prevents cancellation from launching unaccounted CPU
work or freeing a completed response buffer's reservation too early.

| Setting | Default | Meaning |
| --- | ---: | --- |
| `TOVEK_QUEUE_LIMIT` | 1024 | Extra waiting places; zero disables waiting beyond available ingress places. |
| `TOVEK_QUEUE_TIMEOUT_SECS` | 120 | Maximum wait for ingress, and separately for CPU admission. |
| `TOVEK_UPLOAD_TIMEOUT_SECS` | 30 | Total deadline for an admitted request body, excluding CPU execution. |
| `TOVEK_SOURCE_CACHE_MIB` | 0 | Optional ready-source cache budget, from 0 to 1024 MiB. Zero disables the cache and singleflight. |

A full queue or an expired admission wait returns `503` with `Retry-After: 1`.
A body deadline returns `408` with `Connection: close`. These are admission and
upload deadlines, not preemptive CPU execution deadlines. Batch CPU admission
failures appear as per-item errors in the batch response, and are not cached.

## Source cache and singleflight

Set, for example, `TOVEK_SOURCE_CACHE_MIB=64` before starting the process to
enable the cache. It is disabled by default so the fresh raw-script route pays
no key-copy or hashing cost. The cache is process-local; restarting or replacing
the executable discards every entry.

A key includes exact bytecode, the decode key, every decompiler option, the
complete optional script name, and `MEDAL_NO_SHARED_TAIL`. Diagnostics and
profiling requests that require a fresh engine execution bypass both cached
results and shared pending work. Environment settings should be configured
before startup.

Only successful source results are retained. Errors and CPU admission failures
wake current followers without entering the ready cache. Concurrent identical
misses share one asynchronous completion; no cache mutex is held across an
await or decompiler/Rayon work. Raw, legacy and batch routes share the same
source namespace while preserving their own response framing and correlation
ids.

The ready cache has at most 1024 entries, each at most 16 MiB including its key
and accounting allowance. Pending work has a separate cap of 64 keys and at
most `min(configured cache budget, 64 MiB)` of key accounting. Oversized entries
and a full pending map bypass sharing. Ready-cache eviction uses recent access
order. The configured budget covers ready entries; it is not a total process
memory limit.

## Payload limits

All lengths are bytes. JSON limits include string escaping where indicated.

| Resource | Limit |
| --- | ---: |
| Legacy `/decompile` request body | 2 MiB |
| Raw `/decompile/raw` request body | 16 MiB |
| JSON or MDB1 batch request body | 64 MiB |
| Scripts per batch | 50,000 |
| One decoded script | 16 MiB |
| One script name | 4 KiB |
| One JSON correlation id | 1 KiB |
| Aggregate decoded scripts in a batch | 64 MiB |
| One generated source | 16 MiB |
| Saved source/error text for whole-request duplicate reuse | 64 MiB |
| Batch metadata, including escaped ids/names and error envelopes | 16 MiB |
| Complete serialized batch response | 64 MiB |

JSON and MDB1 enforce the same per-script and name bounds. Limit violations
reject the request before engine work where the input permits this; malformed
base64 or bytecode inside an otherwise valid batch remains an item error.
Oversized source becomes an item error. When a source would exhaust the final
JSON budget, that row becomes `ok: false` with
`"error": "batch response budget exceeded"`; space remains reserved for all
later rows, preserving input order, ids and a valid complete JSON document.

These are protocol and retained-buffer limits. JSON parsing has additional
allocation overhead, and engine AST/formatter working memory is separate. The
source limit is checked after the core produces its string; it does not itself
bound peak formatter allocation.

## Verification

Run `cargo test -p web-server`. Unit and route tests cover cache identity and
hash collisions, bounded eviction/pending keys, failure and leader cancellation,
cache lookup before busy CPU admission, cross-route sharing, response buffer
ownership, batch/interactive queue reservations, upload deadlines, cancellation,
whole-request reuse across CPU quanta with the process cache disabled, and
escaped JSON budget rollback. The existing real-HTTP tests also exercise
overload response delivery and queued uploads.
