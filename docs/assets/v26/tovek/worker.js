/* Tovek in a Web Worker.
 *
 * Loads tovek.wasm once and decompiles one request at a time, so the page never freezes. Nothing leaves the
 * browser: the bytes arrive by postMessage and the source goes back the same way.
 *
 * Messages in:  {type: 'init', wasmUrl, warm}
 *               {type: 'decompile', id, bytes: ArrayBuffer, name, form, key, flags, policy}
 * Messages out: {type: 'ready', info, loadMs} | {type: 'failed', error} | {type: 'warmed', ms, ok}
 *               {type: 'result', id, ok, source | error, stage, ms, key, form, memory}
 * See tovek.js for the meaning of every field.
 */
'use strict';

const ABI = 1;
/** After a job leaves the instance's memory above this, start a fresh instance (memory never shrinks). */
const FRESH_INSTANCE_BYTES = 256 * 1024 * 1024;
const STATUS = ['ok', 'decompile', 'input'];
const FORM = { 0: 'auto', 1: 'raw', 2: 'text' };

let wasmModule = null;
let exports = null;
let info = null;
let loading = null;
const utf8 = new TextDecoder('utf-8');

async function compile(url) {
  if (WebAssembly.compileStreaming) {
    try {
      return await WebAssembly.compileStreaming(fetch(url));
    } catch (error) {
      // Served without application/wasm, or streaming unsupported: compile from bytes instead.
      if (error instanceof WebAssembly.CompileError) throw error;
    }
  }
  const response = await fetch(url);
  if (!response.ok) throw new Error(`could not fetch tovek.wasm (HTTP ${response.status})`);
  return WebAssembly.compile(await response.arrayBuffer());
}

async function instantiate() {
  const instance = await WebAssembly.instantiate(wasmModule, {});
  exports = instance.exports;
}

function readBlock(pointer) {
  const head = new DataView(exports.memory.buffer, pointer, 16);
  const status = head.getUint32(0, true);
  const key = head.getUint32(4, true);
  const form = head.getUint32(8, true);
  const length = head.getUint32(12, true);
  const text = utf8.decode(new Uint8Array(exports.memory.buffer, pointer + 16, length));
  exports.tovek_free(pointer, 16 + length);
  return { status, key, form, text };
}

function copyIn(bytes) {
  const pointer = exports.tovek_alloc(bytes.length);
  if (!pointer) throw new Error('out of memory');
  new Uint8Array(exports.memory.buffer, pointer, bytes.length).set(bytes);
  return pointer;
}

async function load(wasmUrl) {
  const start = performance.now();
  try {
    wasmModule = await compile(wasmUrl);
  } catch (error) {
    const unsupported = error instanceof WebAssembly.CompileError;
    throw new Error(unsupported
      ? 'this browser cannot run Tovek\'s WebAssembly build (it needs WebAssembly exception handling)'
      : String(error && error.message || error));
  }
  await instantiate();
  info = JSON.parse(readBlock(exports.tovek_version()).text);
  if (info.abi !== ABI) throw new Error(`tovek.wasm speaks ABI ${info.abi}, this worker speaks ${ABI}`);
  return { info, loadMs: performance.now() - start };
}

function decompile(request) {
  const bytes = new Uint8Array(request.bytes);
  const name = new TextEncoder().encode(request.name || '');
  let input = 0;
  let namePointer = 0;
  const start = performance.now();
  let block;
  try {
    input = copyIn(bytes);
    namePointer = copyIn(name);
    block = readBlock(exports.tovek_decompile(input, bytes.length, request.form, request.key, request.flags,
      request.policy, namePointer, name.length));
    exports.tovek_free(input, bytes.length);
    exports.tovek_free(namePointer, name.length);
  } catch (error) {
    // A trap (for example running out of memory) leaves the instance unusable: the next request gets a fresh one.
    exports = null;
    return { ok: false, error: `the decompiler stopped: ${error && error.message || error}`, stage: 'crash',
      ms: performance.now() - start, key: request.key, form: FORM[request.form] };
  }
  const ms = performance.now() - start;
  const memory = exports.memory.buffer.byteLength;
  if (memory > FRESH_INSTANCE_BYTES) exports = null;
  const common = { ms, key: block.key, form: FORM[block.form], memory };
  return block.status === 0
    ? { ok: true, source: block.text, ...common }
    : { ok: false, error: block.text, stage: STATUS[block.status] || 'decompile', ...common };
}

// One request at a time, in arrival order.
let queue = Promise.resolve();
self.onmessage = (event) => {
  queue = queue.then(() => handle(event.data));
};

async function handle(message) {
  if (message.type === 'init') {
    loading = loading || load(message.wasmUrl);
    try {
      const { info: built, loadMs } = await loading;
      self.postMessage({ type: 'ready', info: built, loadMs });
    } catch (error) {
      self.postMessage({ type: 'failed', error: String(error && error.message || error) });
      return;
    }
    if (message.warm !== false) {
      // Decompile the built-in warm-up script once, so the engine compiles the common paths now, while the page
      // is being read, instead of during the visitor's first request (which waits in the queue behind this).
      const start = performance.now();
      try {
        const bytes = exports.tovek_warm();
        self.postMessage({ type: 'warmed', ms: performance.now() - start, ok: bytes > 0 });
      } catch (error) {
        exports = null;
        self.postMessage({ type: 'warmed', ms: performance.now() - start, ok: false });
      }
    }
    return;
  }
  if (message.type === 'decompile') {
    let reply;
    try {
      await loading;
      if (!exports) await instantiate();
      reply = decompile(message);
    } catch (error) {
      reply = { ok: false, error: String(error && error.message || error), stage: 'load', ms: 0 };
    }
    self.postMessage({ type: 'result', id: message.id, ...reply });
  }
}
