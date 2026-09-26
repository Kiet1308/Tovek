// Kiểm tra Worker đã build trong workerd cục bộ; không gọi deployment thật.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
const build = resolve(process.argv[2] ?? 'luau-worker/build');
const require = createRequire(resolve(process.argv[3] ?? '.', 'package.json'));
const { Miniflare, Log, LogLevel } = require('miniflare');
const secret = randomUUID();
const manifest = {
  mainModule: 'index.js',
  modules: {
    'index.js': { type: 'esm', contents: readFileSync(resolve(build, 'index.js'), 'utf8') },
    'index_bg.wasm': { type: 'wasm', contents: new Uint8Array(readFileSync(resolve(build, 'index_bg.wasm'))) },
  },
};
function runtime(env = {}) {
  return new Miniflare({
    workers: [{ config: { name: 'review-test', compatibilityDate: '2026-06-15', manifest, env } }],
    log: new Log(LogLevel.ERROR),
    telemetry: { enabled: false },
  });
}
function abc(op, a=0, b=0, c=0) { return (op | a<<8 | b<<16 | c<<24) >>> 0; }
function chunk(words, constants=[]) {
  const data = [...[6,3,0,0,1,3,0,0,0,0,0,words.length]];
  for (const word of words) data.push(word&255,(word>>>8)&255,(word>>>16)&255,word>>>24);
  data.push(constants.length,...constants.flat(),0,0,0,0,0,0);
  return Buffer.from(data).toString('base64');
}
function integer(n) {
  const bytes=[9,0];
  do { const low=Number(n&127n);n>>=7n;bytes.push(low|(n?128:0)); } while(n);
  return bytes;
}
const good=chunk([abc(4,0,7),abc(22,0,2)]);
// Missing numeric step deliberately reaches the lift invariant. The public
// try API must catch it on the actual unwind-enabled wasm build as well.
const panic=chunk([abc(56),abc(22,0,1)]);
const big=chunk([abc(5),abc(22,0,2)],[integer(4294967297n)]);
const body=JSON.stringify({key:1,scripts:[
  {id:'before',encoded_bytecode:good},
  {id:'panic',encoded_bytecode:panic},
  {id:'after',encoded_bytecode:good},
  {id:'integer',encoded_bytecode:big},
]});
const mf = runtime({ AUTH_SECRET: { type: 'text', value: secret } });
try {
  const unauthorized=await mf.dispatchFetch('http://localhost/decompile_batch',{method:'POST',body});
  assert.equal(unauthorized.status,403);
  for(let round=0;round<2;round++) {
    const response=await mf.dispatchFetch('http://localhost/decompile_batch',{
      method:'POST',headers:{Authorization:secret,'Content-Type':'application/json'},body,
    });
    assert.equal(response.status,200);
    const result=await response.json();
    assert.equal(result.count,4);assert.equal(result.ok_count,3);
    assert.deepEqual(result.results.map(item=>item.ok),[true,false,true,true]);
    assert.match(result.results[1].error,/panicked:.*FORNPREP/);
    assert.equal(result.results[2].decompilation.trim(),'return 7');
    assert.equal(result.results[3].decompilation.trim(),'return 4294967297');
    console.log(JSON.stringify({vong:round+1,...result}));
  }
} finally { await mf.dispose(); }
const missing = runtime();
try {
  const response=await missing.dispatchFetch('http://localhost/decompile_batch',{method:'POST',body});
  assert.equal(response.status,503);
} finally { await missing.dispose(); }
console.log('PASS: xác thực bằng binding, integer 64 bit và batch tiếp tục sau panic trên WASM.');
