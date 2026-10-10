// Tovek V2.6 release notes: one camera, six powers of ten.
// Native scroll drives a sticky stage. Every frame is a pure function of the scroll position t, so the same t always
// draws the same picture. All numbers and code come from ./data/*.json (written by the release data scripts).

const root = document.documentElement;
const reduceMQ = matchMedia('(prefers-reduced-motion: reduce)');
const coarse = matchMedia('(pointer: coarse)').matches;

// ------------------------------------------------------------------ small maths

const clamp = (x, a = 0, b = 1) => (x < a ? a : x > b ? b : x);
const lerp = (a, b, t) => a + (b - a) * t;
const smooth = (a, b, x) => { const t = clamp((x - a) / (b - a)); return t * t * (3 - 2 * t); };
const inOut = (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
const inOutSine = (t) => -(Math.cos(Math.PI * t) - 1) / 2;
const outCubic = (t) => 1 - Math.pow(1 - t, 3);
const outQuint = (t) => 1 - Math.pow(1 - t, 5);
const fmt = (n) => Math.round(n).toLocaleString('en-US');
const log10 = Math.log10;

// ------------------------------------------------------------------ the timeline (units of scroll)

const TL = [
  { id: 'hero', rest: 0, len: 0.75 },
  { id: 'l0', rest: 0, len: 1.05 },
  { dive: 0, len: 1.0 },
  { id: 'l1', rest: 1, len: 1.7 },
  { dive: 1, len: 1.05 },
  { id: 'l2', rest: 2, len: 1.7 },
  { dive: 2, len: 1.0 },
  { id: 'l3', rest: 3, len: 1.6 },
  { dive: 3, len: 1.0 },
  { id: 'l4', rest: 4, len: 1.5 },
  { dive: 4, len: 1.0 },
  { id: 'l5', rest: 5, len: 1.35 },
  { dive: 5, len: 0.9 },
  { id: 'l6', rest: 6, len: 1.25 },
  { rise: true, len: 2.1 },
  { id: 'lit', rest: 0, len: 1.7, finale: true },
];
{
  let acc = 0;
  for (const s of TL) { s.t0 = acc; acc += s.len; s.t1 = acc; }
}
const TOTAL = TL[TL.length - 1].t1;
const REST = Object.fromEntries(TL.filter((s) => s.id).map((s) => [s.id, s]));
const RISE = TL.find((s) => s.rise);
const FINALE = REST.lit;
const LEVEL_IDS = ['l0', 'l1', 'l2', 'l3', 'l4', 'l5', 'l6'];

// Levels that grow out of a window in their parent: the framed square opens, shows the level inside it, and grows
// until it is the view. The others (10^2 and 1 bit) are their parent's own picture, magnified, so they need none.
const WINDOWED = [false, true, false, true, true, true, false];
// how far the camera leans into the field when the visitor hovers "Scroll to dive" (in powers of ten)
const PEEK_Z = 0.2;
// the field's slow ping from the dive point: period and travel time in seconds
const PING_EVERY = 6.4, PING_TRAVEL = 3.0;

function stateAt(t) {
  let seg = TL[TL.length - 1];
  for (const s of TL) if (t < s.t1) { seg = s; break; }
  const u = clamp((t - seg.t0) / seg.len);
  const q = new Float32Array(7);
  for (let i = 0; i < 7; i++) {
    const s = REST[LEVEL_IDS[i]];
    q[i] = clamp((t - s.t0) / s.len);
  }
  let z, cap = null;
  if (seg.dive !== undefined) {
    z = seg.dive + inOut(u);
    cap = u < 0.3 ? LEVEL_IDS[seg.dive] : u > 0.7 ? LEVEL_IDS[seg.dive + 1] : null;
  } else if (seg.rise) {
    z = 6 * (1 - inOutSine(u));
    cap = u < 0.1 ? 'l6' : u > 0.9 ? 'lit' : null;
  } else {
    z = seg.rest;
    cap = seg.id;
  }
  return {
    t, z, q, cap,
    risen: t >= RISE.t0,
    fin: clamp((t - FINALE.t0) / (FINALE.len * 0.62)),
    rise: seg.rise ? u : t >= RISE.t1 ? 1 : 0,
  };
}

// ------------------------------------------------------------------ colours and type

let COL = {};
function readColors() {
  const cs = getComputedStyle(root);
  const v = (n, d) => cs.getPropertyValue(n).trim() || d;
  COL = {
    paper: v('--paper', '#f2f0eb'), sheet: v('--sheet', '#faf9f6'), ink: v('--ink', '#121210'),
    ink2: v('--ink-2', '#4a4943'), ink3: v('--ink-3', '#8b8980'), signal: v('--signal', '#ff4d1a'),
    signalDeep: v('--signal-deep', '#d63a0c'), paper2: v('--paper-2', '#e9e6df'),
  };
}
const MONO = '"Geist Mono", ui-monospace, Consolas, monospace';
const TEXT = '"Geist", system-ui, sans-serif';
const DISPLAY = '"Bricolage Grotesque", "Geist", sans-serif';
let ADV = 0.6; // Geist Mono advance per em, measured once fonts are in

// a tiny Luau tokenizer for canvas code: near-greyscale, the rebuilt-call seam in the accent
const KW = new Set('and break continue do else elseif end false for function if in local nil not or repeat return then true until while'.split(' '));
const TOKEN = /(--.*$)|("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')|(\b\d+(?:\.\d+)?\b)|([A-Za-z_]\w*)|(\s+)|(.)/g;
const tokCache = new Map();
function tokens(line) {
  let out = tokCache.get(line);
  if (out) return out;
  out = [];
  let col = 0;
  line = line.replace(/\t/g, '    ');
  for (const m of line.matchAll(TOKEN)) {
    const [s, com, str, num, id] = m;
    const k = com ? (com.includes('inferred') ? 'seam' : 'com') : str ? 'str' : num ? 'num' : id ? (KW.has(id) ? 'kw' : 'id') : 'p';
    if (s.trim()) out.push([col, s, k]);
    col += s.length;
  }
  tokCache.set(line, out);
  return out;
}
const TOKCOL = { kw: 'ink', id: 'ink2', str: 'ink2', num: 'ink', com: 'ink3', seam: 'signalDeep', p: 'ink2' };

// ------------------------------------------------------------------ data

const DATA = {};
async function loadData() {
  const names = ['field', 'script', 'script_text', 'dive', 'lines', 'expression', 'number'];
  const got = await Promise.all(names.map((n) => fetch(`data/${n}.json`).then((r) => {
    if (!r.ok) throw new Error(`${n}.json: ${r.status}`);
    return r.json();
  })));
  names.forEach((n, i) => { DATA[n] = got[i]; });
  prepare();
}

// derived, viewport-independent structures
const PRE = {};
function prepare() {
  const { field, script, script_text: text, dive } = DATA;

  // the field: four discs of dots, largest scripts at the centre of each
  const rows = field.rows;
  const games = [[], [], [], []];
  rows.forEach((r, idx) => games[r[0] - 1].push(idx));
  const maxLines = Math.max(...rows.map((r) => Math.max(r[3], r[4])));
  const litOf = (c) => (c > 0 ? 0.38 + 0.62 * clamp(Math.log2(1 + c) / 6) : 0);
  for (const g of games) g.sort((a, b) => (rows[b][4] - rows[a][4]) || (rows[b][3] - rows[a][3]) || a - b);
  PRE.field = { rows, games, maxLines, litOf };

  // the silhouette: union of both outputs in alignment order
  const A = script.silhouette.v251, B = script.silhouette.v26;
  const ent = [];
  for (const [tag, i1, i2, j1, j2] of dive.silhouette.order) {
    if (tag === 0) for (let k = 0; k < i2 - i1; k++) ent.push([i1 + k, j1 + k, 0]);
    else {
      for (let i = i1; i < i2; i++) ent.push([i, -1, 1]);
      for (let j = j1; j < j2; j++) ent.push([-1, j, 2]);
    }
  }
  const oldCopy = new Int8Array(A.length).fill(-1);
  dive.silhouette.copies.forEach(([f, e], k) => { for (let i = f; i < e; i++) oldCopy[i] = k; });
  const linesA = text.v251.split('\n'), linesB = text.v26.split('\n');
  const focus = new Uint8Array(B.length);
  const h0 = dive.helper.v26_line - 1;
  for (let j = h0; j < h0 + 5; j++) focus[j] = 1;
  for (let j = dive.function.v26[0] - 1; j < dive.function.v26[1]; j++) focus[j] = 1;
  PRE.sil = { A, B, ent, oldCopy, linesA, linesB, focus, copies: dive.silhouette.copies, n: ent.length };
}

// ------------------------------------------------------------------ scene layout (per viewport)

const R_ROWS = 280;      // silhouette rows per column
const N_COLS = 5;
const TAB = 4;

function makeScene(W, H, opt) {
  const mobile = opt.mobile;
  let box;
  if (opt.box) box = opt.box;
  else if (mobile) {
    const top = opt.topH + 50, bottom = H * 0.56 - 6;
    box = { x: 14, y: top, w: W - 28, h: Math.max(160, bottom - top) };
  } else {
    const left = opt.left, right = W - opt.right;
    box = { x: left, y: opt.topH + 14, w: Math.max(200, right - left), h: H - opt.topH - 38 };
  }
  box.cx = box.x + box.w / 2;
  box.cy = box.y + box.h / 2;
  const P = Math.floor(Math.min(box.w, box.h) * (mobile ? 0.98 : 0.94));
  const sc = { W, H, mobile, box, P, lv: [], mask: opt.mask || null, still: !!opt.still };
  // the room a level has around the centre of the box at rest, before the ruler or the captions cover it
  const m = sc.mask || {};
  const visL = m.left ? m.left + 4 : box.x, visR = m.right ? m.right - 30 : box.x + box.w;
  sc.visL = visL; sc.visR = visR;
  sc.half = Math.max(80, Math.min(box.cx - visL, visR - box.cx) - 6);

  sc.lv[0] = layField(sc);
  sc.lv[1] = laySil(sc);
  sc.lv[2] = layFn(sc);
  sc.lv[3] = layIns(sc);
  sc.lv[4] = layExpr(sc);
  sc.lv[5] = layBits(sc);
  sc.lv[6] = layBit(sc);

  // world: plate i has size 10^-i, its child sits at its target
  sc.C = [{ x: 0, y: 0 }];
  sc.S = [1];
  for (let i = 0; i < 6; i++) {
    const S = sc.S[i], T = sc.lv[i].target;
    sc.S[i + 1] = S / 10;
    sc.C[i + 1] = { x: sc.C[i].x + (T[0] * S) / P, y: sc.C[i].y + (T[1] * S) / P };
  }
  sc.rest = sc.lv.map((L, i) => {
    const off = L.off || [0, 0], dz = L.dz || 0;
    return { x: sc.C[i].x + (off[0] * sc.S[i]) / P, y: sc.C[i].y + (off[1] * sc.S[i]) / P, k: (P / sc.S[i]) * Math.pow(10, dz) };
  });
  return sc;
}

function camera(sc, z) {
  z = clamp(z, 0, 6);
  const i = Math.min(5, Math.floor(z)), f = z - i;
  const A = sc.rest[i], B = sc.rest[i + 1];
  if (f <= 1e-9) return A;
  if (f >= 1 - 1e-9) return B;
  const px = (B.k * B.x - A.k * A.x) / (B.k - A.k), py = (B.k * B.y - A.k * A.y) / (B.k - A.k);
  const k = A.k * Math.pow(B.k / A.k, f);
  return { x: px - ((px - A.x) * A.k) / k, y: py - ((py - A.y) * A.k) / k, k };
}

// --- 10^4: the field
function layField(sc) {
  const { rows, games, maxLines, litOf } = PRE.field;
  const P = sc.P;
  const n = rows.length;
  const discs = [
    { cx: -0.205, cy: -0.19, rot: 0.3 }, { cx: 0.232, cy: 0.214, rot: 1.9 },
    { cx: 0.292, cy: -0.282, rot: 4.1 }, { cx: -0.298, cy: 0.302, rot: 2.6 },
  ];
  const big = games[0].length;
  const R0 = 0.268;
  const x = new Float32Array(n), y = new Float32Array(n), r = new Float32Array(n);
  const l1 = new Float32Array(n), l2 = new Float32Array(n), dist = new Float32Array(n);
  const r1 = new Float32Array(n), r2 = new Float32Array(n);
  const spacing = R0 * Math.sqrt(Math.PI / big) * P;
  const GA = Math.PI * (3 - Math.sqrt(5));
  const lmax = Math.log(1 + maxLines);
  const callR = (c) => (c > 0 ? spacing * (0.1 + 0.34 * Math.sqrt(Math.min(c, 32) / 32)) : 0);
  const labels = [];
  games.forEach((g, gi) => {
    const D = discs[gi], rad = R0 * Math.sqrt(g.length / big) * P;
    g.forEach((idx, k) => {
      const rr = rad * Math.sqrt((k + 0.5) / g.length), a = k * GA + D.rot;
      const row = rows[idx];
      x[idx] = D.cx * P + rr * Math.cos(a);
      y[idx] = D.cy * P + rr * Math.sin(a);
      const sz = Math.log(1 + Math.max(row[3], row[4])) / lmax;
      r[idx] = spacing * (0.11 + 0.22 * sz * sz);
      r1[idx] = r[idx] + callR(row[1]);
      r2[idx] = r[idx] + callR(row[2]);
      l1[idx] = litOf(row[1]);
      l2[idx] = litOf(row[2]);
    });
    labels.push({ x: D.cx * P, y: D.cy * P + rad + spacing * 2.2, text: `GAME ${gi + 1}`, sub: `${fmt(g.length)} scripts` });
  });
  // the dot we dive into: a large script in game 1, off the very centre
  const g0 = games[0];
  let target = g0[Math.min(g0.length - 1, Math.round(g0.length * 0.02))];
  const tx = x[target], ty = y[target];
  let maxD = 0;
  for (let i = 0; i < n; i++) { dist[i] = Math.hypot(x[i] - tx, y[i] - ty); if (dist[i] > maxD) maxD = dist[i]; }
  for (let i = 0; i < n; i++) dist[i] /= maxD;
  return { x, y, r, r1, r2, l1, l2, dist, n, labels, spacing, maxD, targetIdx: target, target: [tx, ty] };
}

// --- 10^3: one script as a silhouette
function laySil(sc) {
  const P = sc.P;
  const p = (0.94 * P) / R_ROWS;
  const fs = 0.62 * p;
  const cw = fs * ADV;
  const colW = 100 * cw;
  const gap = 0.062 * P;
  const total = N_COLS * colW + (N_COLS - 1) * gap;
  const g = { p, fs, cw, colW, gap, x0: -total / 2, y0: -0.47 * P };
  // positions at m = 1 (V2.6 layout) for the function we dive into
  const ys = silPositions(1);
  const { dive } = DATA;
  const [f0, f1] = dive.function.v26;
  const yA = ys.newY[f0 - 1], yB = ys.newY[f1 - 1];
  const at = (yy) => { const col = Math.floor(yy / R_ROWS); return [g.x0 + col * (colW + gap), g.y0 + (yy - col * R_ROWS) * p, col]; };
  const [cx0, cyA] = at(yA), [, cyB] = at(yB);
  g.at = at;
  g.target = [cx0 + colW / 2, (cyA + cyB + p) / 2];
  return g;
}

// fractional line positions of every union entry at morph m; also index of new lines -> y
function silPositions(m) {
  const S = PRE.sil;
  const y = new Float32Array(S.n), h = new Float32Array(S.n);
  const newY = new Float32Array(S.B.length);
  let acc = 0;
  for (let k = 0; k < S.n; k++) {
    const e = S.ent[k];
    const hh = e[2] === 0 ? 1 : e[2] === 1 ? 1 - m : m;
    y[k] = acc; h[k] = hh;
    if (e[1] >= 0) newY[e[1]] = acc;
    acc += hh;
  }
  return { y, h, total: acc, newY };
}

// --- 10^2: one function (overlays on top of the zoomed silhouette)
function layFn(sc) {
  const P = sc.P, L1 = sc.lv[1];
  const p2 = L1.p * 10, fs2 = L1.fs * 10, cw2 = L1.cw * 10;
  // a still figure (the reduced-motion page) cannot pan, so it fits the lines whole instead
  const minFont = sc.still ? 5.5 : sc.mobile ? 9.6 : 12.2;
  const cardChars = Math.max(...PRE.sil.linesB.slice(DATA.dive.helper.v26_line - 1, DATA.dive.helper.v26_line + 4).map((l) => l.replace(/\t/g, '    ').length));
  const [fa, fb] = DATA.dive.function.v26;
  const fnChars = Math.max(...PRE.sil.linesB.slice(fa - 1, fb).map((l) => l.replace(/\t/g, '    ').length));
  const room = sc.visR - sc.box.x - 18;
  const fit = (sc.box.w - 28) / (cardChars * cw2 + cw2 * 3.2);
  // fit the function's own lines too, down to the smallest size that still reads
  const fitFn = room / (fnChars * cw2 + cw2 * 1.6);
  const dz = log10(Math.max(minFont / fs2, Math.min(1, fit, fitFn)));
  const scale = Math.pow(10, dz);
  const toL2 = (x, y) => [(x - L1.target[0]) * 10, (y - L1.target[1]) * 10];
  const ys = silPositions(1);
  const colLeft = toL2(L1.at(ys.newY[DATA.dive.function.v26[0] - 1])[0], 0)[0];
  const calls = PRE.sil.copies.map(([, , nl]) => {
    const [xx, yy] = L1.at(ys.newY[nl]);
    const [lx, ly] = toL2(xx, yy);
    return [lx + TAB * cw2, ly + p2 / 2];
  });
  const fnTop = toL2(0, L1.at(ys.newY[DATA.dive.function.v26[0] - 1])[1])[1];
  // the helper card, lifted above the function
  let cardLines = PRE.sil.linesB.slice(DATA.dive.helper.v26_line - 1, DATA.dive.helper.v26_line + 4);
  const visW = sc.box.w / scale;
  if (cardChars * cw2 + cw2 * 3.2 > visW) {
    const [head, ...rest] = cardLines[0].split(' --');
    if (rest.length) cardLines = [head, '--' + rest.join(' --')].concat(cardLines.slice(1));
  }
  const cardH = p2 * cardLines.length + p2 * 2.4;
  const pad = cw2 * 1.6;
  const longest = Math.max(...cardLines.map((l) => l.replace(/	/g, '    ').length));
  const card = { x: colLeft - pad, y: fnTop - cardH - p2 * 1.6, w: Math.min(longest * cw2 + pad * 2, visW * 0.98), h: cardH, lines: cardLines, pad };
  // rest view: the column's left edge just inside the box, card and function centred vertically
  const fnBottom = fnTop + p2 * (DATA.dive.function.v26[1] - DATA.dive.function.v26[0] + 1);
  const midY = (card.y + fnBottom) / 2;
  const visH = sc.box.h / scale;
  const offY = Math.min(midY, card.y - p2 * 0.8 + visH / 2);
  const off = [colLeft - pad - 10 / scale + visW / 2, offY];
  // Lines that still run past the edge at the smallest readable size are not cut silently: during the rest at this
  // level the camera pans right to show their ends, then back (see panAt), with an edge fade and a position thumb.
  const endPx = sc.box.x + 10 + (pad + fnChars * cw2) * scale;
  const pan = Math.max(0, endPx - (sc.visR - 10)) / scale;
  // target for 10^1: the start of the first rebuilt call
  const c0 = calls[0];
  return { p2, fs2, cw2, dz, off, calls, card, colLeft, pan, scale, target: [c0[0] + cw2 * 5, c0[1]] };
}

// --- 10^1: instructions and their line numbers
function layIns(sc) {
  // Side by side (bytecode, line, source) where it fits; stacked (source under its instructions) where it does not.
  // The type shrinks to fit, never below 7.4 px.
  const tries = sc.mobile ? [[9, true]] : [[12, false], [10.6, false], [12, true]];
  for (const [fs, stack] of tries) {
    const G = layInsAt(sc, fs, stack);
    if (G.right <= sc.half || stack) {
      if (G.right <= sc.half) return G;
      const k = Math.max(7.4 / fs, (sc.half - 2) / G.right * 0.98);
      return layInsAt(sc, fs * Math.min(1, k), stack);
    }
  }
  return layInsAt(sc, 9, true);
}

function layInsAt(sc, fs, m) {
  const P = sc.P;
  const lh = fs * (m ? 1.95 : 2.05);
  const cw = fs * ADV;
  const L = DATA.lines, D = DATA.dive.folded;
  const half = Math.min(P / 2, sc.half + 4);
  const callText = DATA.script.copies[0].v26_text.replace(/ --.*$/, '');
  const helperLines = new Set(L.copy.filter((r) => r.from === 'helper').map((r) => r.line));
  const aRows = L.copy.map((r) => ({ pc: r.pc, text: r.text, line: r.line, from: r.from }));
  let aSrc = [{ line: L.call_site.line, text: L.call_site.text.trim(), from: 'caller' }, { gap: true }]
    .concat(L.helper.source_lines.map((s) => ({ line: s.line, text: s.text.replace(/\t/g, '  '), from: helperLines.has(s.line) ? 'helper' : 'frame' })));
  const bRows = D.rows.map((r) => ({ pc: null, text: r.text, line: r.line, from: r.from }));
  let bSrc = [{ line: D.call_line.line, text: D.call_line.text.trim(), from: 'caller' }, { gap: true }]
    .concat(D.helper_lines.map((s) => ({ line: s.line, text: s.text.replace(/\t/g, '  '), from: s.line === D.rows[1].line ? 'helper' : 'frame' })));
  if (m) {
    // on a phone only the lines the instructions point at
    const keep = (src, rows) => src.filter((s) => !s.gap && rows.some((r) => r.line === s.line));
    aSrc = keep(aSrc, aRows);
    bSrc = keep(bSrc, bRows);
  }
  const insW = Math.max(...aRows.concat(bRows).map((r) => r.text.length));
  const x0 = -half + (m ? 4 : 10);
  const xi = x0 + (m ? 0 : cw * 3.5);
  const xt = xi + cw * (insW + 2);
  const tagW = cw * 4.6;
  const xs = m ? x0 + cw * 4.5 : xt + tagW + cw * 10;
  const geo = { fs, lh, cw, x0, xi, xt, xs, tagW };
  const blocks = [
    { eyebrow: 'ClickToMoveController · copy 1 of 12', title: callText, rows: aRows, src: aSrc },
    { eyebrow: 'SpawnShield.luau · a sample we wrote', title: DATA.expression.frames.call_source, rows: bRows, src: bSrc },
  ];
  const head = m ? 2.9 : 3.0;
  const hOf = (b) => head + (m ? b.rows.length + 0.4 + b.src.length : Math.max(b.rows.length, b.src.length));
  const gapB = m ? 1.1 : 1.9;
  const total = (hOf(blocks[0]) + gapB + hOf(blocks[1])) * lh;
  let y = -total / 2;
  for (const b of blocks) {
    b.y = y;
    b.rowY = b.rows.map((_, i) => y + lh * (head + i));
    const srcTop = m ? y + lh * (head + b.rows.length + 0.4) : y + lh * head;
    b.srcY = b.src.map((_, i) => srcTop + lh * i);
    y += (hOf(b) + gapB) * lh;
  }
  // target for 10^0: the folded constant in the LOADK row
  const bB = blocks[1];
  const k = bB.rows.findIndex((r) => r.text.startsWith('LOADK'));
  const t = bB.rows[k].text, bi = t.indexOf('[');
  // the rightmost ink, to choose the form that fits
  const srcW = Math.max(...blocks.flatMap((b) => b.src.filter((s) => !s.gap).map((s) => s.text.length)));
  const nRows = Math.max(...blocks.map((b) => b.rows.length));
  const right = m ? Math.max(xt + tagW + cw * (3.6 + 0.9 * (nRows - 1)), xs + cw * (srcW + 2.2)) : xs + cw * (0.6 + srcW);
  return { geo, blocks, stack: m, right, target: [xi + cw * (bi + (t.length - bi) / 2), bB.rowY[k]] };
}

// --- 10^0: one expression
function layExpr(sc) {
  // labels at the left and the seam after the call where that fits; labels above and the seam below where not
  const tries = sc.mobile ? [[12.6, 10.2, true]] : [[sc.P < 640 ? 19 : 23, 13.5, false], [17, 12.5, true]];
  let G = null;
  for (const [big, small, stack] of tries) {
    G = layExprAt(sc, big, small, stack);
    if (G.right <= sc.half) return G;
  }
  const k = Math.max(0.62, (sc.half - 2) / G.right);
  return layExprAt(sc, G.g.big * k, Math.max(9, G.g.small * k), true);
}

function layExprAt(sc, big, small, m) {
  const P = sc.P;
  const half = Math.min(P / 2, sc.half + 4);
  const x = -half + (m ? 6 : 28);
  const E = DATA.expression;
  const v26 = E.frames.v26.replace(/ --.*$/, '');
  const seam = (E.frames.v26.match(/--.*$/) || [''])[0];
  const lines = {
    a: E.frames.v251, b: v26, seam,
    fa: E.fraction.v251, fb: E.fraction.v26,
    helper: E.frames.helper_source.split('\n')[1].trim(),
  };
  const g = { big, small, x, lab: m ? null : x, codeX: m ? x : x + small * ADV * 8 };
  const rowGap = big * (m ? 3.6 : 2.5);
  const span = rowGap + rowGap * (m ? 1.3 : 1.15) + small * (m ? 5.4 : 4.4) + small * (m ? 3.2 : 2.1);
  const yRow1 = -span / 2 - (m ? big * 0.6 : big * 0.2);
  const yRow2 = yRow1 + rowGap;
  const yCheck = yRow2 + rowGap * (m ? 1.3 : 1.15);
  const yF1 = yCheck + small * (m ? 5.4 : 4.4);
  const yF2 = yF1 + small * (m ? 3.2 : 2.1);
  const constant = E.frames.bytecode.constant;
  // check line: frames(1) -> 1 / 60 -> constant
  const check = [`frames(1)`, `1 / 60`, constant];
  const cw = small * ADV;
  let cx = g.codeX;
  const pieces = check.map((s, i) => { const w = s.length * cw; const p = { s, x: cx, w }; cx += w + cw * 4; return p; });
  const tgt = pieces[2];
  const bigCw = big * ADV;
  const right = g.codeX + Math.max(lines.a.length * bigCw, m ? lines.b.length * bigCw : (lines.b.length + 2) * bigCw + seam.length * cw,
    m ? seam.length * cw : 0, cx - g.codeX - cw * 4, Math.max(lines.fa.length, lines.fb.length) * cw);
  return { g, lines, yRow1, yRow2, yCheck, yF1, yF2, pieces, stack: m, right, target: [tgt.x + tgt.w / 2, yCheck] };
}

// --- 64 bits
function layBits(sc) {
  const P = sc.P, m = sc.mobile;
  const N = DATA.number;
  const rows = [
    { label: 'in the bytecode', bits: N.folded_in_bytecode.bits },
    { label: 'Luau folds 1 / 60', bits: N.luau_folds_1_over_60.bits },
    { label: '1.0 / 60.0 in IEEE 754', bits: N.ieee_division_1_over_60.bits },
  ];
  const c = (P * (m ? 0.96 : 0.92)) / (64 + 1.6);
  const x0 = -(c * 65.6) / 2;
  const cellX = (i) => x0 + i * c + (i >= 1 ? c * 0.8 : 0) + (i >= 12 ? c * 0.8 : 0);
  const gapY = Math.max(c * 5.2, m ? 36 : 56);
  const big = m ? 17 : P < 600 ? 24 : 30;
  const y0 = -gapY * 0.55;
  const ys = rows.map((_, i) => y0 + i * gapY);
  const yNum = ys[0] - c * 4.2 - big * 1.6;
  const last = (cellX(58) + cellX(63) + c) / 2;
  return { rows, c, cellX, ys, sq: c * 0.8, big, yNum, value: N.folded_in_bytecode.value, target: [last, ys[1] + c * 0.4] };
}

// --- one bit
function layBit(sc) {
  return { target: [0, 0] };
}

// ------------------------------------------------------------------ drawing

function makeRenderer(canvas, glCanvas) {
  const glField = glCanvas ? makeGLField(glCanvas) : null;
  const ctx = canvas.getContext('2d', { alpha: !!glField });
  const R = { canvas, ctx, dpr: 1, W: 0, H: 0, fieldCam: '', lastFieldCam: '', layers: {}, glField };
  if (glField) {
    glCanvas.addEventListener('webglcontextlost', (e) => { e.preventDefault(); R.glField = null; glCanvas.hidden = true; kick(); });
  }
  R.resize = (W, H) => {
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    R.dpr = dpr; R.W = W; R.H = H;
    canvas.width = Math.round(W * dpr);
    canvas.height = Math.round(H * dpr);
    if (R.glField) R.glField.resize(canvas.width, canvas.height);
    R.fieldCam = ''; R.layers = {};
  };
  return R;
}

// The field on the GPU: one point per script. The wave of the finale runs in the vertex shader, so a moving camera
// or a running wave costs the same as a still frame.
function makeGLField(canvas) {
  let gl = null;
  try { gl = canvas.getContext('webgl', { alpha: false, antialias: false, premultipliedAlpha: true, depth: false, stencil: false }); } catch (e) { gl = null; }
  if (!gl) return null;
  const VS = `
    precision highp float;
    attribute vec2 a_pos; attribute vec3 a_rad; attribute vec3 a_lit;
    uniform float u_k; uniform vec2 u_off; uniform vec2 u_res; uniform float u_fin; uniform float u_minR; uniform float u_alpha; uniform float u_intro;
    uniform vec3 u_ink; uniform vec3 u_sig;
    uniform float u_time; uniform float u_live; uniform float u_ping;
    varying float v_r; varying vec4 v_col;
    void main() {
      float W = 0.14;
      float front = u_fin * (1.0 + W);
      float w = u_fin > 0.0 ? clamp((front - a_lit.z) / W, 0.0, 1.0) : 0.0;
      bool after = w >= 0.5;
      float lit = after ? a_lit.y : a_lit.x;
      float r = lit > 0.0 ? (after ? a_rad.z : a_rad.y) : a_rad.x;
      if (w > 0.0 && w < 1.0 && a_lit.y > 0.0) r *= 1.0 + 0.75 * sin(3.14159265 * w);
      float rd = max(u_minR, r * u_k);
      vec2 p = u_off + a_pos * u_k;
      gl_Position = vec4(p.x / u_res.x * 2.0 - 1.0, 1.0 - p.y / u_res.y * 2.0, 0.0, 1.0);
      gl_PointSize = 2.0 * rd + 2.0;
      v_r = rd;
      float a; vec3 c;
      if (lit <= 0.0) { a = 0.17; c = u_ink; }
      else if (!after) { a = 0.34 + 0.5 * lit; c = u_ink; }
      else { a = 0.5 + 0.5 * lit; c = u_sig; }
      if (u_live > 0.0) {
        // the field is alive while you look at it: every dot breathes on its own slow clock, deeper where Tovek
        // rebuilt more calls in that script
        float h = fract(sin(dot(a_pos, vec2(12.9898, 78.233))) * 43758.5453);
        float h2 = fract(h * 7.123 + 0.31);
        float breath = sin(u_time * (0.85 + 0.55 * h2) + h * 6.2831853);
        a *= 1.0 + u_live * (lit > 0.0 ? 0.14 + 0.3 * lit : 0.16) * breath;
        // a faint ring travels out from the dive point; the scripts V2.6 rebuilds catch it in the accent
        if (u_ping >= 0.0) {
          float ring = exp(-pow((a_lit.z - u_ping) / 0.075, 2.0)) * u_live;
          a += ring * (0.08 + 0.2 * a_lit.y);
          if (!after && a_lit.y > 0.0) c = mix(c, u_sig, ring * 0.5);
        }
        a = clamp(a, 0.0, 1.0);
      }
      // on load the field appears outward from the dot the camera will dive into
      float reveal = clamp((u_intro * 1.35 - a_lit.z) / 0.35, 0.0, 1.0);
      v_col = vec4(c, a * u_alpha * reveal * reveal * (3.0 - 2.0 * reveal));
    }`;
  const FS = `
    precision mediump float;
    varying float v_r; varying vec4 v_col;
    void main() {
      float d = length((gl_PointCoord - 0.5) * (2.0 * v_r + 2.0));
      float cov = clamp(v_r - d + 0.5, 0.0, 1.0);
      if (cov <= 0.0) discard;
      gl_FragColor = vec4(v_col.rgb * v_col.a * cov, v_col.a * cov);
    }`;
  const sh = (type, src) => { const o = gl.createShader(type); gl.shaderSource(o, src); gl.compileShader(o); if (!gl.getShaderParameter(o, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(o)); return o; };
  let prog;
  try {
    prog = gl.createProgram();
    gl.attachShader(prog, sh(gl.VERTEX_SHADER, VS));
    gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, FS));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog));
  } catch (e) { console.warn('V2.6 field: WebGL unavailable', e); return null; }
  const loc = (n) => gl.getUniformLocation(prog, n);
  const U = { intro: loc('u_intro'), k: loc('u_k'), off: loc('u_off'), res: loc('u_res'), fin: loc('u_fin'), minR: loc('u_minR'), alpha: loc('u_alpha'), ink: loc('u_ink'), sig: loc('u_sig'),
    time: loc('u_time'), live: loc('u_live'), ping: loc('u_ping') };
  const A = { pos: gl.getAttribLocation(prog, 'a_pos'), rad: gl.getAttribLocation(prog, 'a_rad'), lit: gl.getAttribLocation(prog, 'a_lit') };
  const buf = gl.createBuffer();
  const rgb = (c) => hex(c).map((v) => v / 255);
  let uploaded = null, count = 0;
  const G = {
    resize(w, h) { canvas.width = w; canvas.height = h; gl.viewport(0, 0, w, h); },
    clear() {
      const p = rgb(COL.paper);
      gl.clearColor(p[0], p[1], p[2], 1);
      gl.clear(gl.COLOR_BUFFER_BIT);
    },
    draw(F, pl, dpr, fin, alpha, intro = 1, life = null) {
      if (uploaded !== F) {
        // unlit first, so lit dots sit on top
        const order = Array.from({ length: F.n }, (_, i) => i).sort((a, b) => Math.max(F.l1[a], F.l2[a]) - Math.max(F.l1[b], F.l2[b]) || a - b);
        const data = new Float32Array(F.n * 8);
        order.forEach((i, j) => { data.set([F.x[i], F.y[i], F.r[i], F.r1[i], F.r2[i], F.l1[i], F.l2[i], F.dist[i]], j * 8); });
        gl.bindBuffer(gl.ARRAY_BUFFER, buf);
        gl.bufferData(gl.ARRAY_BUFFER, data, gl.STATIC_DRAW);
        uploaded = F; count = F.n;
      }
      gl.useProgram(prog);
      gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.enableVertexAttribArray(A.pos); gl.vertexAttribPointer(A.pos, 2, gl.FLOAT, false, 32, 0);
      gl.enableVertexAttribArray(A.rad); gl.vertexAttribPointer(A.rad, 3, gl.FLOAT, false, 32, 8);
      gl.enableVertexAttribArray(A.lit); gl.vertexAttribPointer(A.lit, 3, gl.FLOAT, false, 32, 20);
      gl.uniform1f(U.k, dpr * pl.s);
      gl.uniform2f(U.off, dpr * pl.X, dpr * pl.Y);
      gl.uniform2f(U.res, canvas.width, canvas.height);
      gl.uniform1f(U.fin, fin);
      gl.uniform1f(U.minR, 0.55 * dpr);
      gl.uniform1f(U.alpha, alpha);
      gl.uniform1f(U.intro, intro);
      gl.uniform1f(U.time, life ? life.clock % 3600 : 0);
      gl.uniform1f(U.live, life ? life.live : 0);
      gl.uniform1f(U.ping, life ? life.ping : -1);
      gl.uniform3fv(U.ink, rgb(COL.ink));
      gl.uniform3fv(U.sig, rgb(COL.signal));
      gl.enable(gl.BLEND);
      gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      gl.drawArrays(gl.POINTS, 0, count);
    },
  };
  return G;
}

let fontCur = '';
function setFont(ctx, f) { if (f !== fontCur || ctx.font !== f) { ctx.font = f; fontCur = f; } }

function render(R, sc, st, extra = {}) {
  const { ctx, dpr, W, H } = R;
  fontCur = '';
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalAlpha = 1;
  if (R.glField) {
    R.glField.clear();
    ctx.clearRect(0, 0, R.canvas.width, R.canvas.height);
  } else {
    ctx.fillStyle = COL.paper;
    ctx.fillRect(0, 0, R.canvas.width, R.canvas.height);
  }
  if (!sc) return;
  // the lean of "Scroll to dive" never pulls the camera back once the dive itself is deeper
  const zc = clamp(Math.max(st.z, PEEK_Z * (extra.peek || 0)), 0, 6);
  const cam = { ...camera(sc, zc) }; // a copy: at rest camera() hands back the scene's own rest view
  // at 10^2, lines too long for the screen: the camera pans to their ends and back
  const pan = sc.lv[2].pan > 0 && st.z === 2 ? sc.lv[2].pan * panAt(st.q[2]) : 0;
  if (pan) cam.x += (pan * sc.S[2]) / sc.P;
  const intro = extra.intro === undefined ? 1 : extra.intro;
  const life = lifeAt(st, zc, extra);
  const plates = [];
  for (let i = 0; i < 7; i++) {
    const s = (cam.k * sc.S[i]) / sc.P;
    const X = sc.box.cx + (sc.C[i].x - cam.x) * cam.k;
    const Y = sc.box.cy + (sc.C[i].y - cam.y) * cam.k;
    plates.push({ i, s, d: log10(s), X, Y });
  }
  const view = (pl) => ({ x0: -pl.X / pl.s, y0: -pl.Y / pl.s, x1: (W - pl.X) / pl.s, y1: (H - pl.Y) / pl.s });
  const use = (pl) => ctx.setTransform(dpr * pl.s, 0, 0, dpr * pl.s, dpr * pl.X, dpr * pl.Y);
  const wins = plates.map((pl) => windowOf(sc, pl, zc));
  // a parent recedes (dims) while the window into its child grows out of it
  const recede = plates.map((pl, i) => (wins[i + 1] ? 1 - 0.4 * smooth(0.05, 0.6, wins[i + 1].u) : 1));

  // content, parent first; a windowed level is drawn into its window, and not at all before it opens
  const D = [drawField, drawSil, drawFn, drawIns, drawExpr, drawBits, drawBit];
  for (const pl of plates) {
    if (pl.d < -1.6 || pl.d > 2.3) continue;
    const w = wins[pl.i];
    if (WINDOWED[pl.i] && zc - (pl.i - 1) <= 0.002) continue;
    ctx.globalAlpha = 1;
    if (w) {
      ctx.save();
      drawWindow(R, w);
      ctx.beginPath();
      ctx.rect(w.x - w.h, w.y - w.h, w.h * 2, w.h * 2); // drawWindow left the transform in CSS pixels
      ctx.clip();
    }
    D[pl.i](R, sc, st, pl, use, view(pl), intro * recede[pl.i] * (w ? w.open : 1), life, intro);
    if (w) ctx.restore();
  }
  // frames and their labels, in screen space
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.globalAlpha = 1;
  for (const pl of plates) drawFrame(R, sc, st, pl, intro, life, wins[pl.i]);
  if (sc.lv[2].pan > 0 && st.z === 2) drawPanCue(R, sc, st, pan);
  drawMasks(R, sc);
}

// how far the 10^2 pan has gone, by progress through that level's rest: out after the copies are home, then back
// before the camera dives into the first call
const panAt = (q) => inOutSine(smooth(0.74, 0.86, q)) * (1 - inOutSine(smooth(0.93, 1.0, q)));

// The field's life: breathing and the ping only near the top of the dive (and calmer in the lit finale).
function lifeAt(st, zc, extra) {
  if (extra.clock === undefined) return null;
  const near = 1 - smooth(0.05, 0.5, zc);
  const live = near * (st.risen ? 0.6 * smooth(0.9, 1, st.fin) : 1) * (extra.live === undefined ? 1 : extra.live);
  if (live <= 0.001) return null;
  const since = (extra.clock - 1.2) % PING_EVERY;
  const ping = since >= 0 && since < PING_TRAVEL + 0.4 ? (since / PING_TRAVEL) * 1.25 - 0.06 : -1;
  return { clock: extra.clock, live, ping, since: since >= 0 ? since : 99 };
}

// The window a framed square opens into its child: paper fills the square and the child shows inside it; as the
// camera dives the window grows, and near the end it lets go of its edges so the child becomes the whole view.
// u runs from 0 (the parent at rest) to 1 (this level at rest) with the camera, in both directions.
function windowOf(sc, pl, zc) {
  const u = zc - (pl.i - 1);
  if (!WINDOWED[pl.i] || u <= 0.002 || u >= 1) return null;
  const open = smooth(0.002, 0.13, u);
  const half = (sc.P * pl.s) / 2;
  const grow = 1 + 2.4 * Math.pow(smooth(0.68, 1, u), 1.5);
  return { open, u, shade: open * (1 - smooth(0.5, 0.85, u)), x: pl.X, y: pl.Y, half, h: half * grow };
}

function drawWindow(R, w) {
  const { ctx, dpr } = R;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const x0 = w.x - w.h, y0 = w.y - w.h, s = w.h * 2;
  // the parent stays faintly visible behind the window for a moment
  ctx.globalAlpha = w.open * 0.94;
  ctx.fillStyle = COL.paper;
  ctx.fillRect(x0, y0, s, s);
  // a soft shade inside the edge: the next level lies below the page
  const shade = w.shade;
  if (shade > 0.01 && s > 12) {
    ctx.strokeStyle = COL.ink;
    ctx.lineWidth = 2;
    const steps = [0.075, 0.045, 0.025, 0.012];
    for (let k = 0; k < steps.length; k++) {
      ctx.globalAlpha = shade * steps[k];
      ctx.strokeRect(x0 + 1 + k * 2, y0 + 1 + k * 2, s - 2 - k * 4, s - 2 - k * 4);
    }
  }
  ctx.globalAlpha = 1;
}

// the deliberate horizontal scroller at 10^2: a fade where lines continue, and a thumb that shows where you are
function drawPanCue(R, sc, st, pan) {
  const { ctx, dpr, W } = R;
  const G = sc.lv[2];
  const a = smooth(0.0, 0.12, st.q[2]) * (1 - smooth(0.97, 1, st.q[2]));
  if (a <= 0.01) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const paper = COL.paper, p = hex(paper), clear = `rgba(${p[0]},${p[1]},${p[2]},0)`;
  const f = pan / G.pan;
  const fade = (x0, x1, alpha) => {
    if (alpha <= 0.01) return;
    const g = ctx.createLinearGradient(x0, 0, x1, 0);
    g.addColorStop(0, clear); g.addColorStop(1, paper);
    ctx.globalAlpha = alpha;
    ctx.fillStyle = g;
    ctx.fillRect(Math.min(x0, x1), sc.box.y - 30, Math.abs(x1 - x0), sc.box.h + 60);
  };
  const right = Math.min(W, sc.visR + 6);
  fade(right - 44, right, a * (1 - f));
  fade(sc.box.x + 30, sc.box.x - 10, a * f);
  // the thumb, under the code
  const tw = Math.min(140, sc.box.w * 0.4), tx = sc.box.x + sc.box.w - tw - 2, ty = sc.box.y + sc.box.h - 3;
  const visW = sc.box.w / G.scale, thumb = tw * visW / (visW + G.pan);
  ctx.globalAlpha = a * 0.9;
  ctx.fillStyle = COL.paper;
  ctx.fillRect(tx - 8, ty - 12, tw + 10, 18);
  ctx.globalAlpha = a * 0.5;
  ctx.fillStyle = COL.ink3;
  ctx.fillRect(tx, ty, tw, 1);
  ctx.globalAlpha = a;
  ctx.fillStyle = COL.ink;
  ctx.fillRect(tx + (tw - thumb) * f, ty - 1, thumb, 3);
  // and an arrow while there is more to the right
  ctx.globalAlpha = a * (1 - f) * 0.9;
  setFont(ctx, `500 10px ${MONO}`);
  ctx.textBaseline = 'middle';
  ctx.textAlign = 'right';
  ctx.fillStyle = COL.ink2;
  ctx.fillText('LINE ENDS →', tx - 10, ty);
  ctx.textAlign = 'left';
  ctx.globalAlpha = 1;
}

// envelope of a level's content by its zoom d (0 = at rest)
const env = (d, inA = -0.85, inB = -0.3, outA = 0.3, outB = 0.85) => smooth(inA, inB, d) * (1 - smooth(outA, outB, d));

const LABELS = [
  ['10', '4', 'scripts'], ['10', '3', 'lines'], ['10', '2', 'lines'], ['10', '1', 'instructions'],
  ['10', '0', 'expression'], ['64', '', 'bits'], ['1', '', 'bit'],
];

function drawFrame(R, sc, st, pl, intro, life, win) {
  const { ctx } = R;
  const i = pl.i, d = pl.d;
  if (d < -1.35 || d > 1.0) return;
  let a = smooth(-1.32, -1.0, d) * (1 - smooth(0.45, 0.95, d));
  // the child frame is drawn on while its parent is at rest; the first one is already there when the page opens
  let draw = 1;
  if (i > 0) {
    const parentQ = st.q[i - 1];
    if (st.z < i - 1 + 0.001 && !st.risen) draw = clamp(parentQ * 2.6 - 0.15);
    if (i === 1 && !st.risen) draw = Math.max(draw, clamp((intro - 0.45) / 0.55));
    // on the way back out the frames stay (each level shrinks into its square), the last one leaves for the finale
    if (i === 1 && st.risen) a *= 1 - smooth(0.8, 1, st.rise);
  }
  a *= intro;
  if (a <= 0.003 || draw <= 0) return;
  const half = (sc.P * pl.s) / 2;
  // while a window is open the square is its edge, and follows it as it lets go
  const edge = win ? win.h : half;
  const x0 = pl.X - edge, y0 = pl.Y - edge, sz = edge * 2;
  if (x0 > R.W + 40 || y0 > R.H + 40 || x0 + sz < -40 || y0 + sz < -40) return;
  const full = (1 - smooth(-0.55, -0.12, d)) * a;
  const crop = sc.lv[i].off ? 0 : smooth(-0.55, -0.12, d) * a;
  ctx.lineWidth = 1;
  // at the top of the page the first frame breathes with the field's ping, and a ring marks the one script inside
  let pulse = 1;
  if (i === 1 && life && d < -0.8) {
    const p = Math.exp(-life.since * 1.6);
    pulse = 1 + 0.45 * p * life.live;
    const g = life.since / 1.5;
    if (g < 1 && full > 0.01) {
      const grow = sz * (1 + 0.5 * outCubic(g));
      ctx.globalAlpha = full * 0.34 * (1 - g) * life.live * draw;
      ctx.strokeStyle = COL.ink;
      ctx.strokeRect(Math.round(pl.X - grow / 2) + 0.5, Math.round(pl.Y - grow / 2) + 0.5, Math.round(grow), Math.round(grow));
    }
  }
  if (i === 1 && !win && d < -0.75) {
    const F0 = sc.lv[0], k = F0.targetIdx;
    const rr = Math.max(F0.r[k], F0.r1[k]) * pl.s * 10 + 3.5;
    ctx.globalAlpha = a * draw * 0.6 * (1 - smooth(-0.95, -0.78, d)) * Math.min(1.3, pulse);
    ctx.strokeStyle = COL.ink;
    ctx.beginPath();
    ctx.arc(pl.X, pl.Y, rr, 0, Math.PI * 2);
    ctx.stroke();
  }
  if (full > 0.003) {
    ctx.globalAlpha = Math.min(1, full * 0.62 * pulse);
    ctx.strokeStyle = COL.ink;
    const per = sz * 4, len = per * draw;
    ctx.beginPath();
    // trace the square clockwise from the top-left corner, as far as `draw` allows
    const pts = [[x0, y0], [x0 + sz, y0], [x0 + sz, y0 + sz], [x0, y0 + sz], [x0, y0]];
    let left = len;
    ctx.moveTo(Math.round(x0) + 0.5, Math.round(y0) + 0.5);
    for (let k = 0; k < 4 && left > 0; k++) {
      const [ax, ay] = pts[k], [bx, by] = pts[k + 1];
      const f = Math.min(1, left / sz);
      ctx.lineTo(Math.round(ax + (bx - ax) * f) + 0.5, Math.round(ay + (by - ay) * f) + 0.5);
      left -= sz;
    }
    ctx.stroke();
  }
  if (crop > 0.003) {
    ctx.globalAlpha = crop * 0.5;
    ctx.strokeStyle = COL.ink;
    const L = Math.min(14, sz * 0.06), o = 6;
    ctx.beginPath();
    for (const [cx, cy, sx, sy] of [[x0, y0, -1, -1], [x0 + sz, y0, 1, -1], [x0 + sz, y0 + sz, 1, 1], [x0, y0 + sz, -1, 1]]) {
      const X = Math.round(cx) + 0.5, Y = Math.round(cy) + 0.5;
      ctx.moveTo(X + sx * o, Y); ctx.lineTo(X + sx * (o + L), Y);
      ctx.moveTo(X, Y + sy * o); ctx.lineTo(X, Y + sy * (o + L));
    }
    ctx.stroke();
  }
  // label: power of ten and unit, constant size
  const la = (sc.mobile && crop > full ? 0 : Math.max(full, crop)) * (i === 0 ? 1 : draw);
  if (la > 0.01 && sz > 18) {
    const [b, e, unit] = LABELS[i];
    const asCrop = crop > full;
    const lx = asCrop ? x0 + 12 : x0, ly = asCrop ? y0 - 9 : y0 - 7;
    if (ly > -10 && ly < R.H + 10 && lx < R.W - 40 && lx > -200) {
      ctx.textBaseline = 'alphabetic';
      setFont(ctx, `600 11px ${DISPLAY}`);
      const wb = ctx.measureText(b).width;
      setFont(ctx, `600 7.5px ${DISPLAY}`);
      const we = e ? ctx.measureText(e).width + 1 : 0;
      setFont(ctx, `500 9.5px ${MONO}`);
      const wu = ctx.measureText(unit.toUpperCase()).width;
      // a paper tab behind the label keeps it legible over whatever it sits on
      ctx.globalAlpha = la * 0.92;
      ctx.fillStyle = COL.paper;
      ctx.fillRect(lx - 3, ly - 12, wb + we + wu + 11, 16);
      ctx.globalAlpha = la * 0.95;
      ctx.fillStyle = COL.ink2;
      setFont(ctx, `600 11px ${DISPLAY}`);
      ctx.fillText(b, lx, ly);
      if (e) { setFont(ctx, `600 7.5px ${DISPLAY}`); ctx.fillText(e, lx + wb + 0.5, ly - 4.5); }
      setFont(ctx, `500 9.5px ${MONO}`);
      ctx.fillStyle = COL.ink3;
      ctx.fillText(unit.toUpperCase(), lx + wb + we + 5, ly);
    }
  }
}

// keep the world out from under the header, the ruler and the captions
function drawMasks(R, sc) {
  const m = sc.mask;
  if (!m) return;
  const { ctx, dpr, W, H } = R;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.globalAlpha = 1;
  const paper = COL.paper;
  const rgb = hex(paper);
  const clear = `rgba(${rgb[0]},${rgb[1]},${rgb[2]},0)`;
  const band = (x0, y0, x1, y1, dir) => {
    const g = dir === 'x' ? ctx.createLinearGradient(x0, 0, x1, 0) : ctx.createLinearGradient(0, y0, 0, y1);
    g.addColorStop(0, clear);
    g.addColorStop(1, paper);
    return g;
  };
  if (m.right) {
    ctx.fillStyle = band(m.right - 34, 0, m.right - 4, 0, 'x');
    ctx.fillRect(m.right - 34, 0, 30, H);
    ctx.fillStyle = paper;
    ctx.fillRect(m.right - 4, 0, W - m.right + 4, H);
  }
  if (m.top) {
    ctx.fillStyle = paper;
    ctx.fillRect(0, 0, W, m.top - 14);
    ctx.fillStyle = band(0, m.top + 10, 0, m.top - 14, 'y');
    ctx.fillRect(0, m.top - 14, W, 24);
  }
  if (m.left) {
    ctx.fillStyle = paper;
    ctx.fillRect(0, 0, m.left - 22, H);
    ctx.fillStyle = band(m.left, 0, m.left - 22, 0, 'x');
    ctx.fillRect(m.left - 22, 0, 22, H);
  }
}

// --- 10^4
// The dots of each state are built once into a few Path2D objects in plate space (one per colour and alpha step),
// so a moving camera costs one transform and a dozen fills. While the camera rests, each state is kept as a bitmap.
// During the finale the wave front is a circle around the dive point: the old bitmap shows outside it, the new one
// inside, and only the dots on the front are drawn live with their small pop.
const BK = 6;
const FILLS = [['ink', 0.17]].concat(
  Array.from({ length: BK }, (_, b) => ['ink', 0.34 + 0.5 * ((b + 0.5) / BK)]),
  Array.from({ length: BK }, (_, b) => ['signal', 0.5 + 0.5 * ((b + 0.5) / BK)]));

function dotGroup(F, i, after) {
  const lit = after ? F.l2[i] : F.l1[i];
  if (lit <= 0) return 0;
  return (after ? 1 + BK : 1) + Math.min(BK - 1, Math.floor(lit * BK - 0.0001));
}
function dotRadius(F, i, after) {
  const lit = after ? F.l2[i] : F.l1[i];
  return lit > 0 ? (after ? F.r2[i] : F.r1[i]) : F.r[i];
}

function fieldPaths(F, after) {
  const key = after ? 'pNew' : 'pOld';
  if (F[key]) return F[key];
  const paths = FILLS.map(() => new Path2D());
  const minR = F.spacing * 0.09;
  for (let i = 0; i < F.n; i++) {
    const g = dotGroup(F, i, after), rr = Math.max(minR, dotRadius(F, i, after));
    paths[g].moveTo(F.x[i] + rr, F.y[i]);
    paths[g].arc(F.x[i], F.y[i], rr, 0, Math.PI * 2);
  }
  F[key] = paths;
  return paths;
}

function fillField(x, R, pl, F, after) {
  x.setTransform(R.dpr * pl.s, 0, 0, R.dpr * pl.s, R.dpr * pl.X, R.dpr * pl.Y);
  const paths = fieldPaths(F, after);
  for (let g = 0; g < FILLS.length; g++) {
    x.fillStyle = COL[FILLS[g][0]];
    x.globalAlpha = FILLS[g][1];
    x.fill(paths[g]);
  }
  x.globalAlpha = 1;
}

function fieldLayer(R, pl, F, after) {
  const c = document.createElement('canvas');
  c.width = R.canvas.width; c.height = R.canvas.height;
  fillField(c.getContext('2d'), R, pl, F, after);
  return c;
}

function waveDots(x, R, pl, F, front, W) {
  const paths = FILLS.map(() => null);
  for (let i = 0; i < F.n; i++) {
    const w = (front - F.dist[i]) / W;
    if (w < 0.5 || w >= 1) continue;
    const g = dotGroup(F, i, true);
    const rr = dotRadius(F, i, true) * (F.l2[i] > 0 ? 1 + 0.75 * Math.sin(Math.PI * w) : 1);
    const p = paths[g] || (paths[g] = new Path2D());
    p.moveTo(F.x[i] + rr, F.y[i]);
    p.arc(F.x[i], F.y[i], rr, 0, Math.PI * 2);
  }
  x.setTransform(R.dpr * pl.s, 0, 0, R.dpr * pl.s, R.dpr * pl.X, R.dpr * pl.Y);
  for (let g = 0; g < FILLS.length; g++) {
    if (!paths[g]) continue;
    x.fillStyle = COL[FILLS[g][0]];
    x.globalAlpha = FILLS[g][1];
    x.fill(paths[g]);
  }
  x.globalAlpha = 1;
}

function drawField(R, sc, st, pl, use, vw, intro, life, rawIntro = intro) {
  // the field stays around the window into the script until the window is nearly the whole view
  const a = env(pl.d, -0.9, -0.35, 0.62, 0.98) * intro;
  if (a <= 0.003) return;
  const { ctx } = R;
  const F = sc.lv[0];
  const fin = st.fin;
  if (R.glField) {
    R.glField.draw(F, pl, R.dpr, fin, a / Math.max(rawIntro, 1e-3), rawIntro, life);
    drawDiscLabels(R, pl, F, a);
    return;
  }
  const camKey = `${pl.X.toFixed(2)}|${pl.Y.toFixed(2)}|${pl.s.toFixed(6)}|${R.canvas.width}x${R.canvas.height}`;
  const still = R.lastFieldCam === camKey;
  R.lastFieldCam = camKey;
  if (!still && R.fieldCam !== camKey) {
    // the camera is moving: fill the cached paths under the new transform
    ctx.save();
    ctx.globalAlpha = a;
    const paths = fieldPaths(F, fin >= 0.5);
    ctx.setTransform(R.dpr * pl.s, 0, 0, R.dpr * pl.s, R.dpr * pl.X, R.dpr * pl.Y);
    for (let g = 0; g < FILLS.length; g++) {
      ctx.fillStyle = COL[FILLS[g][0]];
      ctx.globalAlpha = FILLS[g][1] * a;
      ctx.fill(paths[g]);
    }
    ctx.restore();
    drawDiscLabels(R, pl, F, a);
    return;
  }
  if (R.fieldCam !== camKey) { R.fieldCam = camKey; R.layers = {}; }
  const layer = (after) => (R.layers[after ? 1 : 0] || (R.layers[after ? 1 : 0] = fieldLayer(R, pl, F, after)));
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalAlpha = a;
  if (fin <= 0) ctx.drawImage(layer(false), 0, 0);
  else if (fin >= 1) ctx.drawImage(layer(true), 0, 0);
  else {
    const W = 0.14, front = fin * (1 + W);
    const k = R.dpr * pl.s;
    const cx = R.dpr * pl.X + F.target[0] * k, cy = R.dpr * pl.Y + F.target[1] * k;
    const rIn = Math.max(0, (front - W) * F.maxD * k);
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, R.canvas.width, R.canvas.height);
    ctx.arc(cx, cy, rIn, 0, Math.PI * 2);
    ctx.clip('evenodd');
    ctx.drawImage(layer(false), 0, 0);
    ctx.restore();
    if (rIn > 0) {
      ctx.save();
      ctx.beginPath();
      ctx.arc(cx, cy, rIn, 0, Math.PI * 2);
      ctx.clip();
      ctx.drawImage(layer(true), 0, 0);
      ctx.restore();
    }
    ctx.save();
    ctx.globalAlpha = a;
    waveDots(ctx, R, pl, F, front, W);
    ctx.restore();
  }
  ctx.globalAlpha = 1;
  drawDiscLabels(R, pl, F, a);
}

function drawDiscLabels(R, pl, F, a) {
  const la = smooth(-0.45, -0.05, pl.d) * (1 - smooth(0.05, 0.3, pl.d)) * a;
  if (la <= 0.01) return;
  const { ctx, dpr } = R;
  ctx.setTransform(dpr * pl.s, 0, 0, dpr * pl.s, dpr * pl.X, dpr * pl.Y);
  ctx.globalAlpha = la;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'top';
  ctx.fillStyle = COL.ink3;
  setFont(ctx, `500 9.5px ${MONO}`);
  for (const L of F.labels) ctx.fillText(`${L.text} · ${L.sub}`, L.x, L.y);
  ctx.textAlign = 'left';
  ctx.globalAlpha = 1;
}

// --- 10^3 (and the code you read at 10^2)
function drawSil(R, sc, st, pl, use, vw, intro) {
  const d = pl.d;
  const a = smooth(-1.6, -1.5, d) * (1 - smooth(1.55, 1.98, d)) * intro; // in: its window opens it
  if (a <= 0.003) return;
  const { ctx } = R;
  const G = sc.lv[1], S = PRE.sil;
  const m = outCubic(clamp((st.q[1] - 0.12) / 0.66));
  const pos = silPositions(m);
  const { p, cw, colW, gap, x0, y0 } = G;
  const screenPitch = p * pl.s;
  // bars resolve into text in place: the text comes in over the bars before they go, so neither is ever faint alone
  const textA = smooth(4.8, 8.6, screenPitch);
  const barA = 1 - smooth(6.8, 10.8, screenPitch);
  const dim = smooth(0.35, 0.85, d);
  const extraWash = callGlow(sc, st);
  use(pl);
  // visible rows per column
  const colX = (col) => x0 + col * (colW + gap);
  const rowY = (row) => y0 + row * p;
  const ranges = [];
  for (let col = 0; col < N_COLS; col++) {
    const cx = colX(col);
    if (cx > vw.x1 || cx + colW < vw.x0) { ranges.push(null); continue; }
    const r0 = Math.max(0, Math.floor((vw.y0 - y0) / p) - 1), r1 = Math.min(R_ROWS, Math.ceil((vw.y1 - y0) / p) + 1);
    ranges.push(r1 > r0 ? [col * R_ROWS + r0, col * R_ROWS + r1] : null);
  }
  const inView = (yy) => { const col = Math.floor(yy / R_ROWS); const rg = ranges[col]; return rg && yy >= rg[0] - 1 && yy <= rg[1]; };

  // copy bands, washed while they are still pasted
  if (barA > 0.01 && m < 1) {
    ctx.globalAlpha = a * barA * 0.07 * (1 - m);
    ctx.fillStyle = COL.ink;
    let k = 0;
    while (k < S.n) {
      const e = S.ent[k];
      if (e[0] >= 0 && e[1] < 0 && S.oldCopy[e[0]] >= 0) {
        const start = pos.y[k];
        let end = start;
        while (k < S.n && S.ent[k][0] >= 0 && S.ent[k][1] < 0 && S.oldCopy[S.ent[k][0]] >= 0) { end = pos.y[k] + pos.h[k]; k++; }
        const col = Math.floor(start / R_ROWS);
        if (ranges[col]) ctx.fillRect(colX(col) - cw * 1.5, rowY(start - col * R_ROWS), colW + cw * 3, (end - start) * p);
      } else k++;
    }
  }

  if (barA > 0.01) {
    const pN = new Path2D(), pD = new Path2D(), pS = new Path2D();
    for (let k = 0; k < S.n; k++) {
      const hh = pos.h[k];
      if (hh < 0.03) continue;
      const yy = pos.y[k];
      if (!inView(yy)) continue;
      const e = S.ent[k];
      const shape = e[1] >= 0 ? S.B[e[1]] : S.A[e[0]];
      if (shape[2] === 1) continue;
      const col = Math.floor(yy / R_ROWS);
      const ind = Math.min(shape[0] * TAB, 60);
      const w = Math.min(shape[1], 100 - ind) * cw;
      const bh = p * 0.56 * hh;
      const bx = colX(col) + ind * cw, by = rowY(yy - col * R_ROWS) + (p * hh - bh) / 2;
      const isCopy = e[1] < 0 && S.oldCopy[e[0]] >= 0;
      (shape[2] === 2 ? pS : isCopy ? pD : pN).rect(bx, by, Math.max(w, cw), bh);
    }
    ctx.fillStyle = COL.ink;
    ctx.globalAlpha = a * barA * 0.36 * (1 - dim * 0.6);
    ctx.fill(pN);
    ctx.globalAlpha = a * barA * 0.78;
    ctx.fill(pD);
    ctx.fillStyle = COL.signal;
    ctx.globalAlpha = a * barA;
    ctx.fill(pS);
  }

  // the end of the script, a marker that moves as it shortens
  const endA = a * smooth(-0.4, -0.05, d) * (1 - smooth(0.15, 0.4, d));
  if (endA > 0.01) {
    const yy = pos.total;
    const col = Math.min(N_COLS - 1, Math.floor(yy / R_ROWS));
    const ex = colX(col), ey = rowY(yy - col * R_ROWS) + p * 1.5;
    ctx.globalAlpha = endA;
    ctx.fillStyle = COL.ink;
    ctx.fillRect(ex, ey, colW * 0.18, Math.max(1 / pl.s, p * 0.35));
    setFont(ctx, `500 ${9.5 / pl.s}px ${MONO}`);
    ctx.textBaseline = 'top';
    ctx.fillStyle = COL.ink2;
    ctx.fillText(`${fmt(yy)} lines`, ex, ey + p * 2.2);
  }

  // text, once a line is tall enough to read
  if (textA > 0.01) {
    setFont(ctx, `400 ${G.fs}px ${MONO}`);
    ctx.textBaseline = 'middle';
    for (let col = 0; col < N_COLS; col++) {
      const rg = ranges[col];
      if (!rg) continue;
      // entries are sorted by y: find the first in range
      let lo = 0, hi = S.n;
      while (lo < hi) { const mid = (lo + hi) >> 1; if (pos.y[mid] < rg[0] - 1) lo = mid + 1; else hi = mid; }
      for (let k = lo; k < S.n && pos.y[k] <= rg[1]; k++) {
        const hh = pos.h[k];
        if (hh < 0.5) continue;
        const e = S.ent[k];
        const line = e[1] >= 0 ? S.linesB[e[1]] : S.linesA[e[0]];
        if (!line || !line.trim()) continue;
        const yy = pos.y[k];
        const c = Math.floor(yy / R_ROWS);
        if (c !== col) continue;
        const focus = e[1] >= 0 && S.focus[e[1]];
        const la = a * textA * (focus ? 1 : 1 - 0.78 * dim);
        if (la < 0.02) continue;
        const tx = colX(col), ty = rowY(yy - col * R_ROWS) + p * 0.5;
        const shape = e[1] >= 0 ? S.B[e[1]] : S.A[e[0]];
        if (shape[2] === 2) {
          const t = line.replace(/	/g, '    ');
          const ind = t.length - t.trimStart().length;
          const boost = extraWash ? extraWash(e[1]) : 0;
          ctx.globalAlpha = la * (0.075 + 0.17 * boost);
          ctx.fillStyle = COL.signal;
          roundRect(ctx, tx + (ind - 0.6) * cw, ty - p * 0.47, (Math.min(100, t.length) - ind + 1.2) * cw, p * 0.94, p * 0.18);
          ctx.fill();
        }
        for (const [ci, s, kd] of tokens(line)) {
          if (ci >= 100) break;
          ctx.globalAlpha = la * (kd === 'seam' ? 1 : 0.96);
          ctx.fillStyle = COL[TOKCOL[kd]];
          ctx.fillText(ci + s.length > 100 ? s.slice(0, 100 - ci) : s, tx + ci * cw, ty);
        }
      }
    }
  }
  ctx.globalAlpha = 1;
}

// --- 10^2 overlays: the helper card, the copies flying home, the tally
const arrival = (q, k) => clamp((q - 0.1 - k * 0.044) / 0.17);
function callGlow(sc, st) {
  const q = st.q[2];
  if (q <= 0.1 || q >= 0.8 || st.risen) return null;
  const idx = new Map(PRE.sil.copies.map(([, , nl], k) => [nl, k]));
  return (nl) => {
    const k = idx.get(nl);
    if (k === undefined) return 0;
    const f = arrival(q, k);
    return f > 0 && f < 1 ? Math.sin(Math.PI * Math.min(1, f * 1.6)) : 0;
  };
}

function drawFn(R, sc, st, pl, use, vw, intro) {
  const d = pl.d;
  const a = env(d, -0.6, -0.15, 0.55, 0.97) * intro;
  if (a <= 0.003) return;
  const { ctx } = R;
  const G = sc.lv[2], S = PRE.sil;
  const q = st.risen ? 1 : st.q[2];
  const { p2, fs2, cw2, card } = G;
  use(pl);
  const show = smooth(0.0, 0.12, q) * a;
  if (show <= 0.003) return;
  const lift = (1 - outCubic(smooth(0.0, 0.14, q))) * p2 * 0.8;
  const cy = card.y + lift;
  // card
  ctx.fillStyle = COL.ink;
  for (const [dy, grow, al] of [[0.5, 0.35, 0.025], [0.28, 0.16, 0.035], [0.1, 0.04, 0.05]]) {
    ctx.globalAlpha = show * al;
    roundRect(ctx, card.x - p2 * grow, cy - p2 * grow * 0.5 + p2 * dy, card.w + p2 * grow * 2, card.h + p2 * grow, p2 * (0.42 + grow));
    ctx.fill();
  }
  ctx.globalAlpha = show;
  ctx.fillStyle = COL.sheet;
  roundRect(ctx, card.x, cy, card.w, card.h, p2 * 0.42);
  ctx.fill();
  ctx.lineWidth = 1 / pl.s;
  ctx.strokeStyle = 'rgba(18,18,16,0.14)';
  ctx.stroke();
  const padX = card.pad;
  setFont(ctx, `400 ${fs2}px ${MONO}`);
  ctx.textBaseline = 'middle';
  const lines = card.lines;
  const maxCh = Math.floor((card.w - padX * 2) / cw2);
  for (let i = 0; i < lines.length; i++) {
    const ty = cy + p2 * (1.0 + i);
    for (const [ci, s, kd] of tokens(lines[i])) {
      if (ci >= maxCh) break;
      ctx.fillStyle = COL[TOKCOL[kd]];
      ctx.fillText(ci + s.length > maxCh ? s.slice(0, maxCh - ci - 1) + '…' : s, card.x + padX + ci * cw2, ty);
    }
  }
  setFont(ctx, `500 ${fs2 * 0.72}px ${MONO}`);
  ctx.fillStyle = COL.ink3;
  // tally: one square per copy in the bytecode, filled when its call is rebuilt
  const n = G.calls.length;
  const ty = cy + card.h - p2 * 0.95;
  const sq = p2 * 0.4;
  let done = 0;
  for (let k = 0; k < n; k++) if (arrival(q, k) >= 1) done++;
  const label = 'COPIES IN THE BYTECODE';
  ctx.fillText(label, card.x + padX, ty);
  let sx = card.x + padX + ctx.measureText(label).width + cw2 * 1.2;
  for (let k = 0; k < n; k++) {
    const f = arrival(q, k);
    ctx.lineWidth = 1 / pl.s;
    ctx.strokeStyle = 'rgba(18,18,16,0.32)';
    ctx.strokeRect(sx, ty - sq / 2, sq, sq);
    if (f >= 1) { ctx.fillStyle = COL.signal; ctx.fillRect(sx, ty - sq / 2, sq, sq); }
    sx += sq * 1.5;
  }
  ctx.fillStyle = done === n ? COL.signalDeep : COL.ink2;
  const doneText = `${done} OF ${n} REBUILT`;
  ctx.fillText(doneText, sx + cw2 * 0.6, ty);
  // the helper's real line, when there is room for it
  const lineText = `LINE ${DATA.dive.helper.v26_line}`;
  if (sx + cw2 * 0.6 + ctx.measureText(doneText).width + cw2 * 2 + ctx.measureText(lineText).width < card.x + card.w - padX) {
    ctx.fillStyle = COL.ink3;
    ctx.textAlign = 'right';
    ctx.fillText(lineText, card.x + card.w - padX, ty);
    ctx.textAlign = 'left';
  }
  // each copy lifts out of its call and flies back into the helper
  const tx = card.x + padX + cw2 * 18, tyC = cy + p2 * 2.4;
  for (let k = n - 1; k >= 0; k--) {
    const f = arrival(q, k);
    if (f <= 0 || f >= 1) continue;
    const [sx0, sy0] = G.calls[k];
    const e = easeFly(f);
    const c1x = sx0 + cw2 * 46, c1y = sy0;
    const c2x = tx + cw2 * 30, c2y = tyC + p2 * 2;
    const u = 1 - e;
    const bx = u * u * u * sx0 + 3 * u * u * e * c1x + 3 * u * e * e * c2x + e * e * e * tx;
    const by = u * u * u * sy0 + 3 * u * u * e * c1y + 3 * u * e * e * c2y + e * e * e * tyC;
    const scl = lerp(0.74, 0.34, e);
    const ga = smooth(0, 0.14, f) * (1 - smooth(0.78, 1, f)) * a;
    const [f0, f1] = S.copies[k];
    const body = [];
    for (let i = f0; i < f1; i++) if (S.linesA[i].trim()) body.push(S.linesA[i].replace(/^\t/, ''));
    const wCh = Math.max(...body.map((l) => l.replace(/\t/g, '    ').length)) + 2;
    ctx.save();
    ctx.translate(bx, by);
    ctx.scale(scl, scl);
    ctx.globalAlpha = ga;
    ctx.fillStyle = COL.sheet;
    roundRect(ctx, -cw2, -p2 * 0.75, cw2 * wCh, p2 * (body.length + 0.5), p2 * 0.3);
    ctx.fill();
    ctx.lineWidth = 1 / (pl.s * scl);
    ctx.strokeStyle = 'rgba(214,58,12,0.5)';
    ctx.stroke();
    setFont(ctx, `400 ${fs2}px ${MONO}`);
    body.forEach((line, i) => {
      for (const [ci, s2, kd] of tokens(line)) { ctx.fillStyle = COL[TOKCOL[kd]]; ctx.fillText(s2, ci * cw2, p2 * i); }
    });
    ctx.restore();
  }
  ctx.globalAlpha = 1;
}
const easeFly = (t) => (t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2);

function roundRect(ctx, x, y, w, h, r) {
  ctx.beginPath();
  if (ctx.roundRect) ctx.roundRect(x, y, w, h, r);
  else ctx.rect(x, y, w, h);
}

// --- 10^1
function drawIns(R, sc, st, pl, use, vw, intro) {
  const d = pl.d;
  const a = env(d, -1.6, -1.5, 0.55, 0.97) * intro;
  if (a <= 0.003) return;
  const { ctx } = R;
  const G = sc.lv[3], g = G.geo, m = G.stack;
  const q = st.risen ? 1 : st.q[3];
  use(pl);
  ctx.textBaseline = 'middle';
  const hair = 1 / pl.s;
  G.blocks.forEach((b, bi) => {
    // the second block arrives once the first has made its point
    const rev = bi === 0 ? clamp(q / 0.5) : clamp((q - 0.42) / 0.36);
    const A = a * (bi === 0 ? 1 : smooth(0.0, 0.2, rev));
    if (A <= 0.003) return;
    ctx.globalAlpha = A;
    setFont(ctx, `500 ${g.fs * 0.8}px ${MONO}`);
    ctx.fillStyle = COL.ink3;
    ctx.fillText(b.eyebrow.toUpperCase(), g.x0, b.y);
    setFont(ctx, `500 ${g.fs * 1.2}px ${MONO}`);
    ctx.fillStyle = COL.ink;
    ctx.fillText(b.title, g.x0, b.y + g.lh * 0.95);
    // column heads
    const hy = b.y + g.lh * (m ? 1.95 : 2.05);
    setFont(ctx, `500 ${g.fs * 0.72}px ${MONO}`);
    ctx.fillStyle = COL.ink3;
    ctx.fillText(m ? 'BYTECODE' : 'BYTECODE  -g1', g.xi, hy);
    ctx.textAlign = 'center';
    ctx.fillText('LINE', g.xt + g.tagW / 2, hy);
    ctx.textAlign = 'left';
    if (!m) ctx.fillText('SOURCE', g.xs + g.cw * 0.6, hy);
    ctx.globalAlpha = A * 0.6;
    ctx.fillRect(g.x0, hy + g.lh * 0.34, (m ? g.xt + g.tagW : g.xs + g.cw * 42) - g.x0, hair);
    // instruction rows
    b.rows.forEach((r, k) => {
      const y = b.rowY[k];
      const ra = bi === 0 ? A : A * smooth(k * 0.06, 0.1 + k * 0.06, rev); // the first copy is there on arrival
      if (ra <= 0.003) return;
      ctx.globalAlpha = ra;
      if (r.pc !== null && !m) { setFont(ctx, `400 ${g.fs}px ${MONO}`); ctx.fillStyle = COL.ink3; ctx.textAlign = 'right'; ctx.fillText(String(r.pc), g.xi - g.cw * 1.2, y); ctx.textAlign = 'left'; }
      const op = r.text.split(' ')[0];
      setFont(ctx, `500 ${g.fs}px ${MONO}`);
      ctx.fillStyle = COL.ink;
      ctx.fillText(op, g.xi, y);
      setFont(ctx, `400 ${g.fs}px ${MONO}`);
      ctx.fillStyle = COL.ink2;
      ctx.fillText(r.text.slice(op.length), g.xi + op.length * g.cw, y);
      // the line tag
      const isH = r.from === 'helper';
      const lit = smooth(0.22 + k * 0.07, 0.42 + k * 0.07, rev);
      const tagH = g.lh * 0.74;
      ctx.lineWidth = hair;
      roundRect(ctx, g.xt, y - tagH / 2, g.tagW, tagH, tagH * 0.22);
      if (isH && lit > 0) { ctx.globalAlpha = ra * lit * 0.12; ctx.fillStyle = COL.signal; ctx.fill(); }
      ctx.globalAlpha = ra * (isH ? 0.4 + 0.6 * lit : 0.8);
      ctx.strokeStyle = isH ? COL.signalDeep : 'rgba(18,18,16,0.3)';
      ctx.stroke();
      ctx.globalAlpha = ra;
      ctx.fillStyle = isH ? lerpColor(COL.ink2, COL.signalDeep, lit) : COL.ink2;
      setFont(ctx, `500 ${g.fs}px ${MONO}`);
      ctx.textAlign = 'center';
      ctx.fillText(String(r.line), g.xt + g.tagW / 2, y + 0.5 * hair);
      ctx.textAlign = 'left';
      // the wire to its source line
      const si = b.src.findIndex((s) => s.line === r.line);
      if (si < 0 || lit <= 0) return;
      const sy = b.srcY[si];
      ctx.globalAlpha = ra * (isH ? 0.9 : 0.45);
      ctx.strokeStyle = isH ? COL.signalDeep : COL.ink3;
      ctx.lineWidth = (isH ? 1.25 : 1) * hair;
      ctx.beginPath();
      const e = outCubic(lit);
      if (!m) {
        const x1 = g.xt + g.tagW + g.cw * 0.5, x2 = g.xs - g.cw * 4.2;
        const xm = (x1 + x2) / 2;
        ctx.moveTo(x1, y);
        bezierPartial(ctx, x1, y, xm, y, xm, sy, x2, sy, e);
      } else {
        const x1 = g.xt + g.tagW, xr = x1 + g.cw * (1.6 + k * 0.9);
        const x2 = g.xs + g.cw * (b.src[si].text.length + 1.6);
        ctx.moveTo(x1, y);
        bezierPartial(ctx, x1, y, xr, y, xr + g.cw * 2, sy, x2, sy, e);
      }
      ctx.stroke();
    });
    // source lines
    b.src.forEach((s, k) => {
      if (s.gap) {
        if (!m) { ctx.globalAlpha = A * 0.6; ctx.fillStyle = COL.ink3; setFont(ctx, `400 ${g.fs}px ${MONO}`); ctx.fillText('\u22ee', g.xs + g.cw * 0.6, b.srcY[k]); }
        return;
      }
      const y = b.srcY[k];
      const sa = A * (bi === 0 ? 0.55 + 0.45 * smooth(0.1, 0.3, rev) : smooth(0.1, 0.3, rev));
      if (sa <= 0.003) return;
      const isH = s.from === 'helper';
      const lit = isH ? smooth(0.3, 0.6, rev) : 0;
      ctx.globalAlpha = sa;
      setFont(ctx, `500 ${g.fs * 0.86}px ${MONO}`);
      ctx.fillStyle = isH ? lerpColor(COL.ink3, COL.signalDeep, lit) : COL.ink3;
      ctx.textAlign = 'right';
      ctx.fillText(String(s.line), g.xs - g.cw * 0.4, y);
      ctx.textAlign = 'left';
      setFont(ctx, `400 ${g.fs}px ${MONO}`);
      ctx.globalAlpha = sa * (s.from === 'frame' ? 0.55 : 1);
      for (const [ci, t, kd] of tokens(s.text)) { ctx.fillStyle = COL[TOKCOL[kd]]; ctx.fillText(t, g.xs + g.cw * 0.6 + ci * g.cw, y); }
    });
  });
  ctx.globalAlpha = 1;
}

function bezierPartial(ctx, x0, y0, x1, y1, x2, y2, x3, y3, t) {
  if (t >= 0.999) { ctx.bezierCurveTo(x1, y1, x2, y2, x3, y3); return; }
  // de Casteljau split at t
  const ax = lerp(x0, x1, t), ay = lerp(y0, y1, t), bx = lerp(x1, x2, t), by = lerp(y1, y2, t), cx = lerp(x2, x3, t), cy = lerp(y2, y3, t);
  const dx = lerp(ax, bx, t), dy = lerp(ay, by, t), ex = lerp(bx, cx, t), ey = lerp(by, cy, t);
  const fx = lerp(dx, ex, t), fy = lerp(dy, ey, t);
  ctx.bezierCurveTo(ax, ay, dx, dy, fx, fy);
}

function lerpColor(a, b, t) {
  const pa = hex(a), pb = hex(b);
  return `rgb(${Math.round(lerp(pa[0], pb[0], t))},${Math.round(lerp(pa[1], pb[1], t))},${Math.round(lerp(pa[2], pb[2], t))})`;
}
const hexCache = new Map();
function hex(c) {
  let v = hexCache.get(c);
  if (!v) { const h = c.replace('#', ''); v = [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)]; hexCache.set(c, v); }
  return v;
}

// --- 10^0
function drawExpr(R, sc, st, pl, use, vw, intro) {
  const d = pl.d;
  const a = env(d, -1.6, -1.5, 0.55, 0.97) * intro;
  if (a <= 0.003) return;
  const { ctx } = R;
  const G = sc.lv[4], g = G.g, L = G.lines;
  const q = st.risen ? 1 : st.q[4];
  use(pl);
  ctx.textBaseline = 'middle';
  const bigCw = g.big * ADV, smCw = g.small * ADV;
  const label = (txt, y) => {
    setFont(ctx, `500 ${g.small * 0.8}px ${MONO}`);
    ctx.fillStyle = COL.ink3;
    if (g.lab !== null) ctx.fillText(txt, g.lab, y + 1);
    else ctx.fillText(txt, g.codeX, y - g.big * 1.15);
  };
  const code = (txt, x, y, size, alpha = 1, upto = Infinity) => {
    setFont(ctx, `400 ${size}px ${MONO}`);
    const cw = size * ADV;
    for (const [ci, s, kd] of tokens(txt)) {
      if (ci >= upto) break;
      ctx.globalAlpha = a * alpha;
      ctx.fillStyle = COL[TOKCOL[kd]];
      ctx.fillText(ci + s.length > upto ? s.slice(0, upto - ci) : s, x + ci * cw, y);
    }
  };
  // V2.5.1
  ctx.globalAlpha = a;
  label('V2.5.1', G.yRow1);
  const constStart = L.a.indexOf('(') + 1, constEnd = L.a.lastIndexOf(')');
  const solve = smooth(0.06, 0.34, q);
  code(L.a, g.codeX, G.yRow1, g.big, 1 - 0.45 * solve);
  // underline the folded constant
  ctx.globalAlpha = a * (0.25 + 0.5 * (1 - solve));
  ctx.fillStyle = COL.ink3;
  ctx.fillRect(g.codeX + constStart * bigCw, G.yRow1 + g.big * 0.72, (constEnd - constStart) * bigCw, Math.max(1 / pl.s, 1));
  // V2.6, typed in
  const typed = Math.floor(lerp(0, L.b.length, smooth(0.08, 0.32, q)) + 0.0001);
  ctx.globalAlpha = a * smooth(0.04, 0.1, q);
  label('V2.6', G.yRow2);
  code(L.b, g.codeX, G.yRow2, g.big, 1, typed);
  if (typed > 0 && typed < L.b.length) {
    ctx.globalAlpha = a;
    ctx.fillStyle = COL.ink;
    ctx.fillRect(g.codeX + typed * bigCw + 1, G.yRow2 - g.big * 0.6, Math.max(1.5, g.big * 0.08), g.big * 1.2);
  }
  // the rebuilt call itself, marked
  const callStart = L.b.indexOf('(') + 1, callEnd = L.b.lastIndexOf(')');
  const mark = smooth(0.32, 0.4, q);
  if (mark > 0) {
    ctx.globalAlpha = a * mark;
    ctx.fillStyle = COL.signal;
    ctx.fillRect(g.codeX + callStart * bigCw, G.yRow2 + g.big * 0.72, (callEnd - callStart) * bigCw * mark, Math.max(2 / pl.s, 2));
  }
  const seamA = smooth(0.34, 0.42, q);
  if (seamA > 0 && L.seam) {
    const sy = G.stack ? G.yRow2 + g.big * 1.55 : G.yRow2;
    const sx = G.stack ? g.codeX : g.codeX + (L.b.length + 2) * bigCw;
    setFont(ctx, `400 ${g.small}px ${MONO}`);
    ctx.globalAlpha = a * seamA;
    ctx.fillStyle = COL.signalDeep;
    ctx.fillText(L.seam, sx, sy + (G.stack ? 0 : 1));
  }
  // the check: frames(1) folds to the same double
  const ck = smooth(0.42, 0.5, q);
  if (ck > 0) {
    ctx.globalAlpha = a * ck;
    label('CHECK', G.yCheck);
    const arrows = [];
    G.pieces.forEach((pc, i) => {
      const pa = smooth(0.42 + i * 0.045, 0.5 + i * 0.045, q);
      if (pa <= 0) return;
      code(pc.s, pc.x, G.yCheck, g.small, pa);
      if (i < G.pieces.length - 1) {
        ctx.globalAlpha = a * pa;
        ctx.fillStyle = COL.ink3;
        setFont(ctx, `400 ${g.small}px ${TEXT}`);
        ctx.fillText('→', pc.x + pc.w + smCw * 1.3, G.yCheck);
      }
    });
    const eq = smooth(0.55, 0.62, q);
    if (eq > 0) {
      const last = G.pieces[2];
      ctx.globalAlpha = a * eq;
      ctx.fillStyle = COL.signalDeep;
      setFont(ctx, `500 ${g.small * 0.82}px ${MONO}`);
      const msg = G.stack ? '= the constant, bit for bit' : '= the folded constant, bit for bit';
      if (G.stack) ctx.fillText(msg, g.codeX, G.yCheck + g.small * 1.8);
      else ctx.fillText(msg, g.codeX, G.yCheck + g.small * 1.9);
    }
  }
  // exact fractions
  const fr = smooth(0.6, 0.72, q);
  if (fr > 0) {
    ctx.globalAlpha = a * fr;
    label('V2.5.1', G.yF1);
    code(L.fa, g.codeX, G.yF1, g.small, fr * 0.7);
    ctx.globalAlpha = a * fr;
    label('V2.6', G.yF2);
    code(L.fb, g.codeX, G.yF2, g.small, fr);
    const i0 = L.fb.indexOf('=') + 2;
    ctx.globalAlpha = a * fr;
    ctx.fillStyle = COL.signal;
    ctx.fillRect(g.codeX + i0 * smCw, G.yF2 + g.small * 0.75, (L.fb.length - i0) * smCw, Math.max(1.5 / pl.s, 1.5));
  }
  ctx.globalAlpha = 1;
}

// --- 64 bits
function drawBits(R, sc, st, pl, use, vw, intro) {
  const d = pl.d;
  const a = env(d, -1.6, -1.5, 0.45, 0.95) * intro;
  if (a <= 0.003) return;
  const { ctx } = R;
  const G = sc.lv[5];
  const q = st.risen ? 1 : st.q[5];
  use(pl);
  const { c, sq, cellX, ys, rows } = G;
  const scan = clamp((q - 0.08) / 0.52) * 64;
  ctx.textBaseline = 'alphabetic';
  // field names over the first row
  setFont(ctx, `500 ${Math.max(7.5, c * 0.95)}px ${MONO}`);
  ctx.globalAlpha = a * 0.9;
  ctx.fillStyle = COL.ink3;
  const top = ys[0] - c * 3.6;
  // the number itself
  ctx.globalAlpha = a;
  setFont(ctx, `400 ${G.big}px ${MONO}`);
  ctx.fillStyle = COL.ink;
  ctx.fillText(G.value, cellX(0), G.yNum);
  setFont(ctx, `500 ${Math.max(8, c * 0.95)}px ${MONO}`);
  ctx.fillStyle = COL.ink3;
  ctx.fillText(sc.mobile ? 'FRAMES(1), AS LUAU STORES IT' : 'FRAMES(1), AS THE BYTECODE STORES IT: ONE DOUBLE, 64 BITS', cellX(0), G.yNum + G.big * 1.05);
  ctx.globalAlpha = a * 0.9;
  const groups = [[0, 1, 'sign'], [1, 12, 'exponent'], [12, 64, 'mantissa']];
  for (const [g0, g1, name] of groups) {
    const x0 = cellX(g0), x1 = cellX(g1 - 1) + sq;
    ctx.fillRect(x0, top + c * 0.5, x1 - x0, 1 / pl.s);
    ctx.fillRect(x0, top + c * 0.5, 1 / pl.s, c * 0.5);
    ctx.fillRect(x1 - 1 / pl.s, top + c * 0.5, 1 / pl.s, c * 0.5);
    if (name === 'sign') { ctx.textAlign = 'right'; ctx.fillText(sc.mobile ? 's' : 'sign', x1, top); ctx.textAlign = 'left'; }
    else ctx.fillText(name, x0, top);
  }
  rows.forEach((row, ri) => {
    const y = ys[ri];
    ctx.globalAlpha = a;
    setFont(ctx, `500 ${Math.max(8, c * 1.0)}px ${MONO}`);
    ctx.fillStyle = COL.ink2;
    ctx.fillText(row.label, cellX(0), y - Math.max(5, c * 0.75));
    for (let b = 0; b < 64; b++) {
      const x = cellX(b);
      const one = row.bits[b] === '1';
      if (one) { ctx.fillStyle = COL.ink; ctx.fillRect(x, y, sq, sq); }
      else { ctx.strokeStyle = 'rgba(18,18,16,0.32)'; ctx.lineWidth = 1 / pl.s; ctx.strokeRect(x + 0.5 / pl.s, y + 0.5 / pl.s, sq - 1 / pl.s, sq - 1 / pl.s); }
    }
  });
  // the comparison sweep
  const yb = ys[2] + sq + c * 0.9;
  ctx.globalAlpha = a;
  ctx.fillStyle = COL.signal;
  const done = Math.floor(scan);
  for (let b = 0; b < done; b++) ctx.fillRect(cellX(b), yb, sq, Math.max(c * 0.22, 1.5 / pl.s));
  if (scan > 0 && scan < 64) {
    const x = cellX(Math.min(63, done)) + sq / 2;
    ctx.globalAlpha = a * 0.9;
    ctx.fillRect(x - 0.75 / pl.s, ys[0] - c * 0.4, 1.5 / pl.s, ys[2] - ys[0] + sq + c * 0.8);
  }
  setFont(ctx, `500 ${Math.max(8, c * 1.0)}px ${MONO}`);
  ctx.globalAlpha = a;
  ctx.fillStyle = done >= 64 ? COL.signalDeep : COL.ink2;
  ctx.textAlign = 'right';
  ctx.fillText(`${done} of 64 bits equal`, cellX(63) + sq, yb + c * 2.6);
  ctx.textAlign = 'left';
  // what the bits say
  const N = DATA.number.folded_in_bytecode;
  const info = smooth(0.58, 0.7, q);
  if (info > 0) {
    ctx.globalAlpha = a * info;
    ctx.fillStyle = COL.ink3;
    setFont(ctx, `400 ${Math.max(7.5, c * 0.95)}px ${MONO}`);
    const y2 = yb + c * 2.6;
    ctx.fillText(`${N.hex}`, cellX(0), y2);
    const y3 = y2 + c * 2.2;
    ctx.fillText(`exponent ${N.exponent_value} − 1023 = ${N.exponent_unbiased}`, cellX(0), y3);
    if (!sc.mobile) ctx.fillText(`exactly ${N.exact_decimal}`, cellX(0), y3 + c * 2.2);
  }
  ctx.globalAlpha = 1;
}

// --- one bit
function drawBit(R, sc, st, pl, use, vw, intro) {
  const d = pl.d;
  const a = env(d, -0.7, -0.18, 0.3, 0.8) * intro;
  if (a <= 0.003) return;
  const { ctx } = R;
  const B5 = sc.lv[5];
  const q = st.risen ? 1 : st.q[6];
  use(pl);
  const N = DATA.number;
  const T = B5.target;
  const toL6 = (x, y) => [(x - T[0]) * 10, (y - T[1]) * 10];
  const c = B5.c * 10, sq = B5.sq * 10;
  const yMid = toL6(0, B5.ys[1])[1];
  const rows = [
    { bits: N.neighbours.next_up.bits, value: N.neighbours.next_up.value, label: 'one bit up', off: -1 },
    { bits: N.luau_folds_1_over_60.bits, value: N.luau_folds_1_over_60.value, label: 'Luau folds 1 / 60', off: 0 },
    { bits: N.neighbours.next_down.bits, value: N.neighbours.next_down.value, label: 'one bit down', off: 1 },
  ];
  const spread = outQuint(smooth(0.06, 0.4, q));
  const fsz = Math.max(10, c * 0.13);
  rows.forEach((row) => {
    const ra = row.off === 0 ? a : a * spread * 0.9;
    if (ra <= 0.003) return;
    const y = yMid + row.off * sq * 1.45 * spread;
    for (let b = 58; b < 64; b++) {
      const [x] = toL6(B5.cellX(b), 0);
      const one = row.bits[b] === '1';
      const differs = row.off !== 0 && row.bits[b] !== rows[1].bits[b];
      ctx.globalAlpha = ra * (row.off === 0 ? 1 : 0.55);
      if (one) { ctx.fillStyle = COL.ink; roundRect(ctx, x, y, sq, sq, sq * 0.08); ctx.fill(); }
      else { ctx.strokeStyle = 'rgba(18,18,16,0.38)'; ctx.lineWidth = 1 / pl.s; roundRect(ctx, x + 0.5 / pl.s, y + 0.5 / pl.s, sq - 1 / pl.s, sq - 1 / pl.s, sq * 0.08); ctx.stroke(); }
      setFont(ctx, `500 ${sq * 0.42}px ${DISPLAY}`);
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillStyle = one ? COL.paper : COL.ink3;
      ctx.fillText(one ? '1' : '0', x + sq / 2, y + sq * 0.53);
      if (differs) {
        ctx.globalAlpha = ra;
        ctx.strokeStyle = COL.ink;
        ctx.lineWidth = 2 / pl.s;
        roundRect(ctx, x - sq * 0.08, y - sq * 0.08, sq * 1.16, sq * 1.16, sq * 0.14);
        ctx.stroke();
      }
      if (row.off === 0 && b === 63) {
        const glow = smooth(0.4, 0.56, q);
        if (glow > 0) {
          ctx.globalAlpha = a * glow;
          ctx.strokeStyle = COL.signal;
          ctx.lineWidth = 3 / pl.s;
          roundRect(ctx, x - sq * 0.12, y - sq * 0.12, sq * 1.24, sq * 1.24, sq * 0.18);
          ctx.stroke();
        }
      }
    }
    ctx.textAlign = 'left';
    ctx.textBaseline = 'alphabetic';
    // label and value on the left edge of the row
    const [lx] = toL6(B5.cellX(58), 0);
    ctx.globalAlpha = ra;
    setFont(ctx, `500 ${fsz * 0.85}px ${MONO}`);
    ctx.fillStyle = row.off === 0 ? COL.ink2 : COL.ink3;
    ctx.fillText(row.label.toUpperCase(), lx, y - fsz * 0.7);
    setFont(ctx, `400 ${fsz}px ${MONO}`);
    ctx.fillStyle = row.off === 0 ? COL.ink : COL.ink3;
    ctx.textAlign = 'right';
    const [rx] = toL6(B5.cellX(63), 0);
    ctx.fillText(row.value, rx + sq, y - fsz * 0.7);
    ctx.textAlign = 'left';
  });
  // the verdict
  const v = smooth(0.5, 0.64, q);
  if (v > 0) {
    const [lx] = toL6(B5.cellX(58), 0);
    const y = yMid + sq * 1.45 * 2 + fsz * 1.2;
    ctx.globalAlpha = a * v;
    setFont(ctx, `500 ${fsz * 0.95}px ${MONO}`);
    ctx.fillStyle = COL.signalDeep;
    ctx.fillText('frames(1) folds to this one, and only this one', lx, y);
  }
  ctx.globalAlpha = 1;
}

// ------------------------------------------------------------------ the page

const els = {
  dive: document.getElementById('dive'), stage: document.getElementById('stage'), world: document.getElementById('world'),
  ruler: document.getElementById('ruler'), fill: document.getElementById('ruler-fill'), head: document.getElementById('ruler-head'),
  readout: document.getElementById('readout'), cue: document.getElementById('cue'), top: document.getElementById('top'),
  caps: [...document.querySelectorAll('.cap')], litCount: document.getElementById('lit-count'),
  links: [...document.querySelectorAll('.ruler a[data-level]')], rail: document.querySelector('.ruler-rail'),
};
let railLen = 0; // measured once per layout, so the frame loop never reads layout
const capById = Object.fromEntries(els.caps.map((c) => [c.dataset.seg, c]));

let main = null, scene = null, mode = 'motion';
let unitPx = 600, diveTop = 0, diveH = 0, stageH = 0;
let tCur = 0, tTarget = 0, lastFrame = 0, running = false;
let introStart = 0, ready = false;
let lastCap = 'hero', lastLevel = -1, lastCount = -1, lastMag = '';

function cssNum(name, fallback) { const v = parseFloat(getComputedStyle(root).getPropertyValue(name)); return Number.isFinite(v) ? v : fallback; }

function layout() {
  const W = els.stage.clientWidth, H = els.stage.clientHeight;
  stageH = H;
  const mobile = W <= 700;
  unitPx = Math.round(H * (mobile ? 0.6 : 0.64));
  diveH = Math.round(TOTAL * unitPx + H);
  els.dive.style.setProperty('--dive-h', `${diveH}px`);
  const r = els.dive.getBoundingClientRect();
  diveTop = r.top + scrollY;
  main.resize(W, H);
  railLen = 0;
  const topH = cssNum('--top-h', 64);
  if (ready) {
    const cap = document.querySelector('.captions');
    const capRect = cap.getBoundingClientRect();
    const mask = mobile ? { top: topH + 44 } : { top: topH + 8, right: capRect.left - 2, left: 150 };
    scene = makeScene(W, H, { mobile, topH, left: 168, right: W - capRect.left + 44, mask });
  }
  placeMinorTicks(mobile);
}

let minorFor = null;
function placeMinorTicks(mobile) {
  if (minorFor === mobile) return;
  minorFor = mobile;
  const rail = els.ruler.querySelector('.ruler-rail');
  rail.querySelectorAll('.minor').forEach((n) => n.remove());
  const frag = document.createDocumentFragment();
  for (let dec = 0; dec < 6; dec++) {
    for (let k = 2; k <= 9; k++) {
      const f = (dec + log10(k)) / 6;
      const s = document.createElement('span');
      s.className = 'minor';
      if (mobile) s.style.left = `${(f * 100).toFixed(3)}%`;
      else s.style.top = `${(f * 100).toFixed(3)}%`;
      if (k === 5) s.style[mobile ? 'height' : 'width'] = '7px';
      frag.appendChild(s);
    }
  }
  rail.appendChild(frag);
}

function tFromScroll() { return clamp((scrollY - diveTop) / unitPx, 0, TOTAL); }

function frame(now) {
  running = false;
  const dt = Math.min(64, now - (lastFrame || now));
  lastFrame = now;
  tTarget = tFromScroll();
  const tau = coarse ? 0 : 75;
  if (tau > 0 && Math.abs(tTarget - tCur) > 0.0004) tCur += (tTarget - tCur) * (1 - Math.exp(-dt / tau));
  else tCur = tTarget;
  // while the parts list is read the stage is off screen: nothing to draw
  if (ready && scrollY > diveTop + diveH + 40 && Math.abs(tTarget - tCur) <= 0.0004) return;
  peekSchedule(now);
  const intro = introAt(now);
  const live = draw(tCur, intro, now);
  // keep drawing while the camera settles, the field is alive on screen, or the camera leans in or out
  if (Math.abs(tTarget - tCur) > 0.0004 || (ready && intro < 1) || now - peek.t0 < peek.dur || peek.cycle
      || (live && scrollY < diveTop + diveH - stageH * 0.3)) kick();
}

function kick() { if (!running && mode === 'motion') { running = true; requestAnimationFrame(frame); } }

const introAt = (now) => (ready ? (introStart ? clamp((now - introStart) / 1500) : 0) : 0);

// Returns whether the field is alive in this frame (it breathes only near the top and in the lit finale).
function draw(t, intro, now = performance.now()) {
  const st = stateAt(t);
  const perf = window.__divePerf;
  const t0 = perf ? performance.now() : 0;
  const pk = peekAt(now);
  const clock = typeof window.__diveClock === 'number' ? window.__diveClock : now / 1000;
  render(main, ready ? scene : null, st, { intro: outCubic(intro), peek: pk, clock, live: mode === 'motion' && !window.__diveStill ? 1 : 0 });
  ui(st, t);
  if (perf) { perf.push([t, performance.now() - t0]); if (window.__diveMarks) performance.mark(`t=${t.toFixed(2)}`); }
  return ready && Math.max(st.z, PEEK_Z * pk) < 0.5 && (!st.risen || st.fin > 0.9);
}

// "Scroll to dive": the camera leans a little way into the field, so the page shows it is a dive. It does so on
// hover or focus of the cue, and on its own a few times while the visitor waits at the top.
const peek = { from: 0, to: 0, t0: 0, dur: 1, hover: false, cycle: 0, next: 0, count: 0 };
function peekAt(now) { return lerp(peek.from, peek.to, inOutSine(clamp((now - peek.t0) / peek.dur))); }
function peekTo(v, dur) {
  const now = performance.now();
  if (peek.to === v) return;
  peek.from = peekAt(now); peek.to = v; peek.t0 = now; peek.dur = dur;
  kick();
}
function peekSchedule(now) {
  if (!ready || !introStart || mode !== 'motion') return;
  if (tTarget > 0.02) {
    if (peek.to !== 0 && !flight) peekTo(0, 420);
    peek.cycle = 0; peek.next = now + 8000;
    return;
  }
  if (peek.hover) return;
  if (!peek.cycle && peek.count < 3 && now >= Math.max(peek.next, introStart + 3400)) { peek.cycle = now; peek.count++; peekTo(1, 1500); }
  else if (peek.cycle && now - peek.cycle > 2200 && peek.to === 1) peekTo(0, 1700);
  else if (peek.cycle && now - peek.cycle > 4000) { peek.cycle = 0; peek.next = now + 7000; }
}
function bindCue() {
  const cue = els.cue;
  if (!cue) return;
  const lean = (on) => { peek.hover = on; if (mode === 'motion' && tTarget <= 0.02) peekTo(on ? 1 : 0, on ? 1100 : 1300); };
  cue.addEventListener('pointerenter', () => lean(true));
  cue.addEventListener('pointerleave', () => lean(false));
  cue.addEventListener('focus', () => lean(true));
  cue.addEventListener('blur', () => lean(false));
  cue.addEventListener('click', (e) => {
    e.preventDefault();
    peek.hover = false;
    peekTo(0, 1400); // hands over to the dive as it starts
    if (mode === 'motion') flyTo('l1');
    else document.getElementById('l0')?.scrollIntoView();
  });
}

function ui(st, t) {
  // ruler
  const f = st.z / 6;
  const mobile = scene ? scene.mobile : els.stage.clientWidth <= 700;
  const len = railLen || (railLen = mobile ? els.rail.clientWidth : els.rail.clientHeight);
  els.head.style.transform = mobile ? `translate3d(${(f * len - 4.5).toFixed(1)}px,0,0)` : `translate3d(0,${(f * len - 4.5).toFixed(1)}px,0)`;
  els.fill.style.transform = mobile ? `scaleX(${f.toFixed(4)})` : `scaleY(${f.toFixed(4)})`;
  const level = st.cap === 'lit' ? 'lit' : Math.round(st.z);
  if (level !== lastLevel) {
    lastLevel = level;
    els.links.forEach((a) => { if (String(a.dataset.level) === String(level)) a.setAttribute('aria-current', 'step'); else a.removeAttribute('aria-current'); });
  }
  const mag = `×${fmt(Math.pow(10, st.z))}`;
  if (mag !== lastMag) { lastMag = mag; els.readout.textContent = mag; }
  // captions
  const cap = st.cap;
  if (cap !== lastCap) {
    if (lastCap && capById[lastCap]) capById[lastCap].classList.remove('is-on');
    if (cap && capById[cap]) capById[cap].classList.add('is-on');
    lastCap = cap;
  }
  // finale count
  if (ready && els.litCount) {
    const F = DATA.field.totals;
    const v = Math.round(lerp(F.v251.rebuilt_calls, F.v26.rebuilt_calls, outCubic(st.fin)));
    if (v !== lastCount) { lastCount = v; els.litCount.textContent = fmt(v); }
  }
  els.cue.classList.toggle('is-gone', t > 0.12);
}

function onScroll() {
  kick();
  const past = scrollY > diveTop + diveH - stageH - 8;
  els.top.classList.toggle('is-solid', past);
}

// fly the camera to a level: a scripted native scroll, cancelled by any input
let flight = 0;
function cancelFlight() { flight = 0; }
function levelT(id) {
  const s = REST[id];
  if (!s) return 0;
  if (id === 'hero') return 0;
  return s.t0 + s.len * (id === 'lit' ? 0.8 : 0.78);
}
function flyTo(id) {
  const t = levelT(id);
  const y = Math.round(diveTop + t * unitPx);
  if (reduceMQ.matches || mode === 'static') { scrollTo(0, y); return; }
  const y0 = scrollY, dist = y - y0;
  const dur = clamp(Math.abs(t - tCur) * 230, 450, 1600);
  const me = ++flight;
  const start = performance.now();
  const step = (now) => {
    if (me !== flight) return;
    const p = clamp((now - start) / dur);
    scrollTo(0, Math.round(y0 + dist * inOut(p)));
    if (p < 1) requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}
['wheel', 'touchstart', 'pointerdown'].forEach((ev) => addEventListener(ev, cancelFlight, { passive: true }));
addEventListener('keydown', (e) => { if (!(e.target.closest && e.target.closest('.ruler'))) cancelFlight(); });

function bindRuler() {
  els.ruler.addEventListener('click', (e) => {
    const a = e.target.closest('a[data-level]');
    if (!a || mode !== 'motion') return;
    e.preventDefault();
    const id = a.dataset.level === 'lit' ? 'lit' : `l${a.dataset.level}`;
    history.replaceState(null, '', `#${id}`);
    flyTo(id);
  });
  els.ruler.addEventListener('keydown', (e) => {
    const list = els.links.filter((a) => a.closest('.ruler-ticks'));
    const i = list.indexOf(document.activeElement);
    if (i < 0) return;
    let j = -1;
    if (e.key === 'ArrowDown' || e.key === 'ArrowRight') j = Math.min(list.length - 1, i + 1);
    else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') j = Math.max(0, i - 1);
    else if (e.key === 'Home') j = 0;
    else if (e.key === 'End') j = list.length - 1;
    if (j < 0) return;
    e.preventDefault();
    list[j].focus();
    list[j].click();
  });
  // in-page links into the dive (#l3, #lit, #intro) fly instead of jumping to the stacked captions
  document.addEventListener('click', (e) => {
    const a = e.target.closest('a[href^="#"]');
    if (!a || mode !== 'motion' || a.closest('.ruler')) return;
    const id = a.getAttribute('href').slice(1);
    if (REST[id] || id === 'intro') { e.preventDefault(); history.replaceState(null, '', `#${id}`); flyTo(id === 'intro' ? 'hero' : id); }
  });
}

// The parts list marks the Changes theme you are reading, in the side index and in the chip bar on narrow screens.
function bindScrollspy() {
  const themes = [...document.querySelectorAll('.theme[id]')];
  if (!themes.length || !('IntersectionObserver' in window)) return;
  const links = [...document.querySelectorAll('.block-index a[href^="#theme-"]')];
  let current = '';
  const mark = (id) => {
    if (id === current) return;
    current = id;
    for (const a of links) {
      const on = a.getAttribute('href') === `#${id}`;
      if (on) a.setAttribute('aria-current', 'true'); else a.removeAttribute('aria-current');
      // keep the chip in view inside its own bar, without moving the page
      const bar = on && a.closest('.block-chips .block-index');
      if (bar && bar.offsetParent) bar.scrollTo({ left: Math.max(0, a.offsetLeft - 16), behavior: reduceMQ.matches ? 'auto' : 'smooth' });
    }
  };
  // a theme is current while it crosses a thin band a third of the way down the screen
  const io = new IntersectionObserver((entries) => {
    for (const e of entries) if (e.isIntersecting) mark(e.target.id);
  }, { rootMargin: '-32% 0px -64% 0px' });
  themes.forEach((t) => io.observe(t));
}

function jumpToHash() {
  const id = location.hash.slice(1);
  if (!(REST[id] || id === 'intro')) return;
  scrollTo(0, Math.round(diveTop + levelT(id === 'intro' ? 'hero' : id) * unitPx));
  tCur = tTarget = tFromScroll();
}

// ------------------------------------------------------------------ the static version (reduced motion)

// Draw every level once off screen while the page is idle, so the first real pass finds its glyphs, shadows and
// paths already prepared and does not stutter.
function warmUp() {
  const samples = [];
  for (const s of TL) samples.push(s.t0 + s.len * 0.5, s.t0 + s.len * 0.92);
  const cv = document.createElement('canvas');
  const R = makeRenderer(cv);
  R.resize(main.W, main.H);
  let i = 0;
  const idle = window.requestIdleCallback || ((f) => setTimeout(() => f({ timeRemaining: () => 8 }), 60));
  const step = (dl) => {
    while (i < samples.length && dl.timeRemaining() > 6) {
      render(R, scene, stateAt(samples[i++]), { intro: 1 });
    }
    if (i < samples.length) idle(step);
    else { cv.width = cv.height = 0; }
  };
  idle(step);
}

function renderStatic() {
  const figs = [...document.querySelectorAll('.plate-fig')];
  for (const fig of figs) {
    const cv = fig.querySelector('canvas');
    const w = fig.clientWidth, h = fig.clientHeight || w;
    if (!w) continue;
    const R = makeRenderer(cv);
    R.resize(w, h);
    const sc = makeScene(w, h, { mobile: w < 520, still: true, box: { x: 0, y: 0, w, h } });
    const which = fig.dataset.plate;
    const z = which === 'lit' ? 0 : Number(which);
    const q = new Float32Array(7);
    for (let i = 0; i < 7; i++) q[i] = i < z ? 1 : i === z ? 1 : 0;
    const st = { t: 0, z, q, cap: null, risen: which === 'lit', fin: which === 'lit' ? 1 : 0, rise: which === 'lit' ? 1 : 0 };
    if (which === '0') st.risen = false;
    render(R, sc, st, { intro: 1 });
  }
}

// ------------------------------------------------------------------ start

function setMode() {
  mode = reduceMQ.matches ? 'static' : 'motion';
  root.classList.toggle('static', mode === 'static');
  // in the moving version every caption sits in the same place, so the browser must not jump to them by itself
  for (const c of els.caps) {
    if (mode === 'motion' && c.id) { c.dataset.id = c.id; c.removeAttribute('id'); }
    else if (mode === 'static' && c.dataset.id) c.id = c.dataset.id;
  }
}

async function start() {
  readColors();
  setMode();
  const glCanvas = document.createElement('canvas');
  glCanvas.className = 'world world-gl';
  glCanvas.setAttribute('aria-hidden', 'true');
  els.world.before(glCanvas);
  main = makeRenderer(els.world, glCanvas);
  if (!main.glField) glCanvas.remove();
  if (mode === 'motion') { layout(); draw(0, 0); }
  bindRuler();
  bindCue();
  bindScrollspy();
  try {
    await Promise.all([loadData(), document.fonts.load(`400 16px ${MONO}`), document.fonts.load(`500 16px ${MONO}`), document.fonts.load(`600 16px ${DISPLAY}`)]);
    await document.fonts.ready;
  } catch (err) {
    console.warn('V2.6 dive: data did not load', err);
    root.classList.add('static');
    return;
  }
  const probe = document.createElement('canvas').getContext('2d');
  probe.font = `400 100px ${MONO}`;
  ADV = probe.measureText('0000000000').width / 1000 || 0.6;
  ready = true;
  if (mode === 'static') { renderStatic(); }
  else {
    layout();
    jumpToHash();
    introStart = performance.now();
    kick();
    setTimeout(warmUp, 1300);
  }
  addEventListener('scroll', onScroll, { passive: true });
  let rz = 0;
  addEventListener('resize', () => {
    cancelAnimationFrame(rz);
    rz = requestAnimationFrame(() => {
      if (mode === 'static') { renderStatic(); return; }
      const before = tCur;
      layout();
      scrollTo(0, Math.round(diveTop + before * unitPx));
      tCur = tTarget = tFromScroll();
      kick();
    });
  });
  addEventListener('hashchange', () => {
    const id = location.hash.slice(1);
    if (mode === 'motion' && (REST[id] || id === 'intro')) flyTo(id === 'intro' ? 'hero' : id);
  });
  reduceMQ.addEventListener('change', () => {
    setMode();
    if (mode === 'static') renderStatic();
    else { layout(); kick(); }
  });
  onScroll();
}

start();

// for headless checks: draw any t and read the frame state
window.__dive = {
  stateAt, TOTAL, REST, TL, levelT: (id) => levelT(id), unit: () => unitPx, top: () => diveTop, ready: () => ready, scene: () => scene,
  // put the dive exactly at t now (screenshots and frame sequences)
  seek(t) { cancelFlight(); scrollTo(0, Math.round(diveTop + t * unitPx)); tTarget = tCur = t; draw(t, introAt(performance.now())); },
  // hold the camera's lean at v (0..1) and stop the automatic leans
  peek(v) { peek.from = peek.to = v; peek.t0 = 0; peek.dur = 1; peek.count = 99; peek.cycle = 0; kick(); },
};
