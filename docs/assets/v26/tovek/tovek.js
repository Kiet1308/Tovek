// Tovek in the browser. Nothing is uploaded: the decompiler runs here, in a Web Worker, from tovek.wasm.
//
//   import { load, decompile } from './assets/v26/tovek/tovek.js';
//   await load();                                   // fetch + compile once; resolves the build info
//   const result = await decompile(bytes, { name: 'Promise.lua' });
//   if (result.source !== undefined) show(result.source); else showError(result.error);
//
// decompile(input, options) never throws. It resolves
//   { source, ms, version, key, form, memory }       on success
//   { error, stage, ms, version, key, form, memory } on failure (stage: input | decompile | load | crash | timeout |
//                                                    cancelled; load/timeout/cancelled carry only error, stage, ms)
// key: 1 plain Luau bytecode, 203 Roblox client bytecode. form: 'raw' | 'text'. ms: time inside the worker.
// memory: bytes of WebAssembly memory the worker holds after the call (it never shrinks; past 256 MB the worker
// starts a fresh instance for the next call).
// The source is byte-for-byte what the native CLI prints (without its trailing newline):
//   raw bytecode -> `luau-lifter <file> [--script-name <name>]` (Roblox client bytecode: `-e`)
//   saved text   -> `luau-lifter decompile-folder` on a folder holding that file, named <name>

const HERE = import.meta.url;

// Option bits of luau_lifter::DecompileOptions::from_flag_bits.
const FLAG = {
  dontReuseVar: 1 << 0,
  noSynthHelpers: 1 << 1,
  assumeNoNan: 1 << 2,
  synthesizeArithmeticLoops: 1 << 5,
  compactStyle: 1 << 7,
  assumeStandardLibraries: 1 << 8,
};
const FORM = { auto: 0, raw: 1, text: 2 };
const KEY = { auto: 0, plain: 1, client: 203 };
const POLICY = { cli: 0, dispatcher: 1, strict: 2 };

let worker = null;
let ready = null;
let info = null;
let nextId = 1;
const pending = new Map();
const urls = { wasm: new URL('./tovek.wasm', HERE).href, worker: new URL('./worker.js', HERE).href };

function settleAll(error, stage) {
  for (const { resolve, timer } of pending.values()) {
    clearTimeout(timer);
    resolve({ error, stage, ms: 0, version: info?.version });
  }
  pending.clear();
}

function stop(error, stage) {
  if (worker) worker.terminate();
  worker = null;
  ready = null;
  settleAll(error, stage);
}

/**
 * Start the worker and compile tovek.wasm. Safe to call many times; later calls share the first.
 * Resolves { version, commit, crate, abi, loadMs } (version is the CLI's --version, e.g. "V2.6").
 * options.warm (default true): right after loading, decompile a small built-in script once so the first real
 * request does not pay for the engine compiling the decompiler's code (about 0.1-0.2 s, off the main thread).
 * options.wasmUrl / options.workerUrl: other locations for tovek.wasm / worker.js.
 * Rejects with an Error when the browser cannot run it (no Web Workers or no WebAssembly exception handling).
 */
export function load(options = {}) {
  const base = globalThis.document?.baseURI ?? HERE;
  if (options.wasmUrl) urls.wasm = new URL(options.wasmUrl, base).href;
  if (options.workerUrl) urls.worker = new URL(options.workerUrl, base).href;
  if (ready) return ready;
  ready = new Promise((resolve, reject) => {
    if (typeof Worker === 'undefined' || typeof WebAssembly === 'undefined') {
      reject(new Error('this browser cannot run Tovek here (no Web Workers or WebAssembly)'));
      return;
    }
    const started = performance.now();
    worker = new Worker(urls.worker);
    worker.onmessage = ({ data }) => {
      if (data.type === 'ready') {
        info = { ...data.info, loadMs: performance.now() - started, compileMs: data.loadMs };
        resolve(info);
      } else if (data.type === 'warmed') {
        if (info) info.warmMs = data.ms;
      } else if (data.type === 'failed') {
        reject(new Error(data.error));
        stop(data.error, 'load');
      } else if (data.type === 'result') {
        const job = pending.get(data.id);
        if (!job) return;
        pending.delete(data.id);
        clearTimeout(job.timer);
        const { ok, source, error, stage, ms, key, form, memory } = data;
        job.resolve(ok
          ? { source, ms, version: info?.version, key, form, memory }
          : { error, stage, ms, version: info?.version, key, form, memory });
      }
    };
    worker.onerror = (event) => {
      event.preventDefault();
      const message = event.message || 'the decompiler worker could not start';
      reject(new Error(message));
      stop(message, 'load');
    };
    worker.postMessage({ type: 'init', wasmUrl: urls.wasm, warm: options.warm !== false });
  });
  ready.catch(() => {});
  return ready;
}

async function toBytes(input) {
  if (typeof input === 'string') return new TextEncoder().encode(input);
  if (input instanceof ArrayBuffer) return new Uint8Array(input.slice(0));
  if (ArrayBuffer.isView(input)) return new Uint8Array(input.buffer.slice(input.byteOffset, input.byteOffset + input.byteLength));
  if (typeof Blob !== 'undefined' && input instanceof Blob) return new Uint8Array(await input.arrayBuffer());
  throw new TypeError('decompile() takes a Uint8Array, ArrayBuffer, Blob/File or string');
}

function flagBits(options) {
  let bits = 0;
  for (const [name, bit] of Object.entries(FLAG)) if (options[name]) bits |= bit;
  if (options.style === 'compact') bits |= FLAG.compactStyle;
  return bits;
}

/**
 * Decompile Luau bytecode.
 *
 * input   Uint8Array | ArrayBuffer | Blob/File | string
 *         - raw bytecode (luau-compile --binary, or Roblox client bytecode),
 *         - or text: base64, optionally after `--` comment lines (the saved-script form decompile-folder reads).
 * options name     script name hint, as the CLI's --script-name (a File's own name by default), e.g. 'Promise.lua'
 *         key      'auto' (default) | 'plain' (1) | 'client' (203) | a number 1-255
 *         form     'auto' (default) | 'raw' | 'text'
 *         control  'cli' (default: raw like single-file mode, text strict like decompile-folder)
 *                  | 'dispatcher' | 'strict'
 *         style    'default' | 'compact'
 *         dontReuseVar, assumeNoNan, assumeStandardLibraries, synthesizeArithmeticLoops, noSynthHelpers  booleans
 *         timeout  ms; past it the worker is stopped and the result is { error, stage: 'timeout' }
 */
export async function decompile(input, options = {}) {
  let bytes;
  try {
    bytes = await toBytes(input);
  } catch (error) {
    return { error: error.message, stage: 'input', ms: 0, version: info?.version };
  }
  try {
    await load();
  } catch (error) {
    return { error: error.message, stage: 'load', ms: 0 };
  }
  const name = options.name ?? (typeof File !== 'undefined' && input instanceof File ? input.name : '');
  const key = typeof options.key === 'number' ? options.key : KEY[options.key ?? 'auto'];
  const form = FORM[options.form ?? 'auto'];
  const policy = POLICY[options.control ?? 'cli'];
  if (key === undefined || form === undefined || policy === undefined) {
    return { error: 'unknown key, form or control option', stage: 'input', ms: 0, version: info?.version };
  }
  const id = nextId++;
  return new Promise((resolve) => {
    // cancel() (or a timed-out request) may have stopped the worker while this call waited for load().
    if (!worker) {
      resolve({ error: 'cancelled', stage: 'cancelled', ms: 0, version: info?.version });
      return;
    }
    const job = { resolve, timer: 0 };
    if (options.timeout > 0) {
      job.timer = setTimeout(() => {
        pending.delete(id);
        resolve({ error: `stopped after ${options.timeout} ms`, stage: 'timeout', ms: options.timeout, version: info?.version });
        stop('stopped because an earlier request ran too long', 'cancelled');
      }, options.timeout);
    }
    pending.set(id, job);
    worker.postMessage({ type: 'decompile', id, bytes: bytes.buffer, name, form, key, flags: flagBits(options), policy },
      [bytes.buffer]);
  });
}

/** Stop whatever is running (pending calls resolve { error, stage: 'cancelled' }). The next call loads again. */
export function cancel() {
  stop('cancelled', 'cancelled');
}

/** The build info once loaded ({ version, commit, crate, abi, loadMs }), else null. */
export function buildInfo() {
  return info;
}

/**
 * The bundled samples: public scripts and our own SpawnShield, with the native CLI's output hash for each.
 * Each row gets absolute `url` (the input) and `sourceUrl` (the original source) fields.
 */
export async function samples() {
  const base = new URL('./samples/', HERE);
  const response = await fetch(new URL('index.json', base));
  if (!response.ok) throw new Error(`could not fetch the samples (HTTP ${response.status})`);
  const index = await response.json();
  return index.samples.map((row) => ({
    ...row,
    url: new URL(row.file, base).href,
    sourceUrl: new URL(row.source_file, base).href,
  }));
}

/** Fetch one sample row's input bytes (as decompile() takes them). */
export async function sampleBytes(row) {
  const response = await fetch(row.url);
  if (!response.ok) throw new Error(`could not fetch ${row.file} (HTTP ${response.status})`);
  return new Uint8Array(await response.arrayBuffer());
}
