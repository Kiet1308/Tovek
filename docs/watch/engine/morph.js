// THE TOKEN MORPH. Code A becomes code B in place:
//   - tokens both versions share (longest common subsequence over tokens) glide to their new spot;
//   - tokens only A has fade, drift and blur away;
//   - tokens only B has type in, in the accent (they are what changed);
//   - when A and B have different sizes, a camera eases from a fit of A to a fit of B.
//
//   const plan = codeMorph(srcA, srcB);              // diffed once, cached per (A, B)
//   drawMorph(ctx, plan, progress(t, 8, 4), { box: { x: 160, y: 120, w: 1600, h: 840 } });
//
// The plan precomputes every position in typed arrays, so a frame is one pass of fillText per
// tone. ~80-line excerpts stay well inside a 60 fps budget.

import { layoutCode, codeMetrics, codePalettes } from './code.js';
import { setFont } from './text.js';
import { clamp, lerp } from './tween.js';
import { ease as E } from './ease.js';
import { mix } from './color.js';

const planCache = new Map();

/** LCS over two token lists (kind + text). Returns matched index pairs, ascending. */
function lcsPairs(a, b) {
  const eq = (x, y) => x.k === y.k && x.text === y.text;
  let s = 0;
  while (s < a.length && s < b.length && eq(a[s], b[s])) s++;
  let ea = a.length, eb = b.length;
  while (ea > s && eb > s && eq(a[ea - 1], b[eb - 1])) { ea--; eb--; }
  const pairs = [];
  for (let i = 0; i < s; i++) pairs.push([i, i]);
  const n = ea - s, m = eb - s;
  if (n > 0 && m > 0) {
    // intern tokens so the inner loop compares integers
    const ids = new Map();
    const id = (tk) => {
      const key = tk.k + '\u0000' + tk.text;
      let v = ids.get(key);
      if (v === undefined) { v = ids.size; ids.set(key, v); }
      return v;
    };
    const A = new Int32Array(n), B = new Int32Array(m);
    for (let i = 0; i < n; i++) A[i] = id(a[s + i]);
    for (let j = 0; j < m; j++) B[j] = id(b[s + j]);
    const W = m + 1;
    const T = n * W + W > 4e7 ? null : new Uint16Array((n + 1) * W);
    if (T) {
      // suffix table: T[i][j] = LCS(A[i..], B[j..]); then walk forward, preferring matches
      for (let i = n - 1; i >= 0; i--) {
        const row = i * W, below = row + W, ai = A[i];
        for (let j = m - 1; j >= 0; j--) {
          T[row + j] = ai === B[j] ? T[below + j + 1] + 1 : Math.max(T[below + j], T[row + j + 1]);
        }
      }
      let i = 0, j = 0;
      while (i < n && j < m) {
        if (A[i] === B[j] && T[i * W + j] === T[(i + 1) * W + j + 1] + 1) { pairs.push([s + i, s + j]); i++; j++; }
        else if (T[(i + 1) * W + j] >= T[i * W + j + 1]) i++;
        else j++;
      }
    }
  }
  for (let k = 0; ea + k < a.length; k++) pairs.push([ea + k, eb + k]);
  return pairs;
}

/**
 * Diff two Luau sources into a morph plan. Cached per (A, B, tabSize).
 * `plan.inserted` (Set of B token indices) is what to keep highlighting with drawCode afterwards.
 */
export function codeMorph(srcA, srcB, { tabSize = 4 } = {}) {
  const key = tabSize + '\u0001' + srcA + '\u0001' + srcB;
  const hit = planCache.get(key);
  if (hit) return hit;
  const A = layoutCode(srcA, { tabSize }), B = layoutCode(srcB, { tabSize });
  const pairs = lcsPairs(A.tokens, B.tokens);
  const keptA = new Uint8Array(A.tokens.length), keptB = new Uint8Array(B.tokens.length);
  for (const [i, j] of pairs) { keptA[i] = 1; keptB[j] = 1; }

  const maxLine = Math.max(1, B.lineCount - 1), maxLineA = Math.max(1, A.lineCount - 1);
  // kept: grouped by kind -> flat arrays [ax, ay, bx, by, delay] per token part (parts move together)
  const kept = new Map();
  for (const [i, j] of pairs) {
    const ta = A.tokens[i], tb = B.tokens[j];
    if (!kept.has(tb.k)) kept.set(tb.k, { text: [], nums: [] });
    const g = kept.get(tb.k);
    const delay = (tb.line / maxLine) * 0.6 + (ta.line / maxLineA) * 0.4;
    // parts pair up in order; a token that changes line count cannot be kept (its text differs)
    for (let p = 0; p < tb.parts.length; p++) {
      const pa = ta.parts[p] || ta.parts[ta.parts.length - 1], pb = tb.parts[p];
      g.text.push(pb.text);
      g.nums.push(pa.col, pa.line, pb.col, pb.line, delay);
    }
  }
  for (const g of kept.values()) g.nums = Float32Array.from(g.nums);

  const removed = [];
  A.tokens.forEach((tk) => { if (!keptA[tk.i]) for (const pt of tk.parts) removed.push({ k: tk.k, text: pt.text, col: pt.col, line: pt.line, wave: tk.line / maxLineA }); });

  // inserted tokens type in as runs: consecutive inserted tokens on one B line
  const inserted = new Set();
  const runs = [];
  let run = null;
  B.tokens.forEach((tk) => {
    if (keptB[tk.i]) { run = null; return; }
    inserted.add(tk.i);
    for (const pt of tk.parts) {
      if (!run || run.line !== pt.line) {
        run = { line: pt.line, pieces: [], chars: 0 };
        runs.push(run);
      }
      run.pieces.push({ k: tk.k, text: pt.text, col: pt.col, at: run.chars, i: tk.i });
      run.chars += pt.text.length + 1;
    }
  });
  for (const r of runs) r.wave = r.line / maxLine;

  const plan = {
    a: A, b: B, kept, removed, runs, inserted,
    stats: { kept: pairs.length, removed: A.tokens.length - pairs.length, inserted: inserted.size, linesA: A.lineCount, linesB: B.lineCount },
  };
  if (planCache.size > 64) planCache.clear();
  planCache.set(key, plan);
  return plan;
}

/**
 * The camera for one side: scale and offset that fit `L` (or only its lines `lines: [first, last]`)
 * into `box`. fit: 'contain' (default) | 'width' | 'none'. align: 'top' | 'center'.
 * The scale stays within [minScale, maxScale]. Returns `{ s, x, y }`: draw at
 * translate(x, y) · scale(s) with the code's top-left at the origin.
 */
export function fitCamera(L, box, { size = 24, lineHeight = 1.55, fit = 'contain', align = 'center', maxScale = 1, minScale = 0.2, lines } = {}) {
  const { cw, lh } = codeMetrics(size, lineHeight);
  const l0 = lines ? Math.max(0, lines[0]) : 0;
  const l1 = lines ? Math.min(L.lineCount - 1, lines[1]) : L.lineCount - 1;
  const w = Math.max(1, lines ? L.colsIn(l0, l1) : L.cols) * cw, h = Math.max(1, l1 - l0 + 1) * lh;
  let s = fit === 'none' ? 1 : fit === 'width' ? box.w / w : Math.min(box.w / w, box.h / h);
  s = clamp(s, minScale ?? 0.2, maxScale ?? 1);
  const x = box.x;
  const y = (align === 'top' ? box.y : box.y + (box.h - h * s) / 2) - l0 * lh * s;
  return { s, x, y };
}

/** Run `fn` with the context moved into a camera from fitCamera / drawMorph. */
export function withCamera(ctx, cam, fn) {
  ctx.save();
  ctx.translate(cam.x, cam.y);
  ctx.scale(cam.s, cam.s);
  fn();
  ctx.restore();
}

// six taps around a circle: the soft copies of a leaving token
const TAPS = [0, 1, 2, 3, 4, 5].map((k) => [Math.cos((k * Math.PI) / 3), Math.sin((k * Math.PI) / 3)]);

/**
 * Draw the morph at progress `p` (0 = exactly A, 1 = exactly B).
 *
 * Options:
 *   box            { x, y, w, h } the code must fit in (design units)
 *   size, lineHeight, palette ('night' | 'paper' | object)
 *   fit, align, maxScale, minScale     camera fit for each side (see fitCamera)
 *   linesA, linesB [first, last] line window each camera frames (default: everything)
 *   clip           clip drawing to the box (use with line windows)
 *   timing         { out: [a, b], move: [a, b], in: [a, b] } windows inside 0..1
 *   wave           0..1, how much the motion ripples down the lines (default 0.35)
 *   blur           how far leaving tokens soften (design px, default 6; 0 for none)
 *   inserted       'accent' (default) | 'base': tone of typed-in tokens at the end
 *   insertedMix    0..1, accent amount for inserted tokens (animate it to let the accent settle)
 *   highlight      optional Set of B token indices (or (index) => bool): only these inserted tokens
 *                  take the accent, the rest type in in their base tone (default: every inserted token)
 *   cameraA, cameraB  override either camera ({ s, x, y })
 */
export function drawMorph(ctx, plan, p, o = {}) {
  const size = o.size ?? 24, lineHeight = o.lineHeight ?? 1.55;
  const pal = (typeof o.palette === 'string' ? codePalettes[o.palette] : o.palette) || codePalettes.night;
  const { f, cw, lh, baseline } = codeMetrics(size, lineHeight);
  const box = o.box || { x: 0, y: 0, w: 1920, h: 1080 };
  const camOpts = { size, lineHeight, fit: o.fit, align: o.align, maxScale: o.maxScale, minScale: o.minScale };
  const camA = o.cameraA || fitCamera(plan.a, box, { ...camOpts, lines: o.linesA });
  const camB = o.cameraB || fitCamera(plan.b, box, { ...camOpts, lines: o.linesB });
  const tm = o.timing || {};
  const [o0, o1] = tm.out || [0, 0.42];
  const [m0, m1] = tm.move || [0.12, 0.84];
  const [i0, i1] = tm.in || [0.5, 1];
  const wave = o.wave ?? 0.35;
  const P = clamp(p);
  const pm = clamp((P - m0) / (m1 - m0));
  const camP = E.inOut(pm);
  const cam = { s: lerp(camA.s, camB.s, camP), x: lerp(camA.x, camB.x, camP), y: lerp(camA.y, camB.y, camP) };

  ctx.save();
  if (o.clip) {
    ctx.beginPath();
    ctx.rect(box.x, box.y, box.w, box.h);
    ctx.clip();
  }
  const parentAlpha = ctx.globalAlpha;
  ctx.translate(cam.x, cam.y);
  ctx.scale(cam.s, cam.s);
  setFont(ctx, f);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';

  // 1. leaving tokens fade, drift up and soften. The softening is a few offset copies of each
  //    token, not ctx.filter: once a canvas has drawn through a filter, Chrome rasterises later
  //    frames slightly differently, so the same t would no longer give the same pixels.
  const po = clamp((P - o0) / (o1 - o0));
  if (po < 1 && plan.removed.length) {
    const blurMax = o.blur ?? 6;
    const rad = blurMax > 0 ? E.inQuad(po) * blurMax * 0.8 : 0; // code units: it scales with the camera
    const soft = clamp(rad / 1.5);
    let lastStyle = '';
    for (const r of plan.removed) {
      const q = E.inOut(clamp((po - r.wave * wave * 0.5) / (1 - wave * 0.5)));
      if (q >= 1) continue;
      const color = pal[r.k] || pal.id;
      if (color !== lastStyle) { ctx.fillStyle = color; lastStyle = color; }
      const a = parentAlpha * (1 - q);
      const x = r.col * cw, y = r.line * lh + baseline - q * lh * 0.35;
      ctx.globalAlpha = a * lerp(1, 0.34, soft);
      ctx.fillText(r.text, x, y);
      if (soft > 0) {
        ctx.globalAlpha = a * 0.2 * soft;
        for (const [dx, dy] of TAPS) ctx.fillText(r.text, x + dx * rad, y + dy * rad);
      }
    }
  }

  // 2. tokens both versions share
  const spread = wave;
  for (const [k, g] of plan.kept) {
    ctx.fillStyle = pal[k] || pal.id;
    ctx.globalAlpha = parentAlpha;
    const nums = g.nums, texts = g.text;
    for (let n = 0, q = 0; n < texts.length; n++, q += 5) {
      const d = nums[q + 4] * spread;
      const e = pm <= 0 ? 0 : pm >= 1 ? 1 : E.inOut(clamp((pm - d) / (1 - spread)));
      const cx = nums[q] + (nums[q + 2] - nums[q]) * e;
      const cy = nums[q + 1] + (nums[q + 3] - nums[q + 1]) * e;
      ctx.fillText(texts[n], cx * cw, cy * lh + baseline);
    }
  }

  // 3. arriving tokens type in
  const pi = clamp((P - i0) / (i1 - i0));
  if (pi > 0 && plan.runs.length) {
    const tone = o.inserted || 'accent';
    const amt = tone === 'base' ? 0 : o.insertedMix ?? 1;
    const hl = o.highlight;
    const lit = !hl ? null : typeof hl === 'function' ? hl : (i) => hl.has(i);
    const step = 0.028;
    for (const r of plan.runs) {
      const startAt = r.wave * 0.45;
      const avail = Math.max(0.05, 1 - startAt);
      const st = Math.min(step, avail / Math.max(1, r.chars));
      const typed = (pi - startAt) / st; // characters typed so far in this run
      if (typed <= 0) continue;
      for (const pc of r.pieces) {
        const visible = typed - pc.at;
        if (visible <= 0) break;
        const base = pal[pc.k] || pal.id;
        const a = lit && !lit(pc.i) ? 0 : amt;
        ctx.fillStyle = a >= 1 ? pal.accent : a <= 0 ? base : mix(base, pal.accent, a);
        const whole = Math.min(pc.text.length, Math.floor(visible));
        const px = pc.col * cw, py = r.line * lh + baseline;
        ctx.globalAlpha = parentAlpha;
        if (whole > 0) ctx.fillText(pc.text.slice(0, whole), px, py);
        if (whole < pc.text.length) {
          ctx.globalAlpha = parentAlpha * (visible - whole);
          ctx.fillText(pc.text[whole], px + whole * cw, py);
        }
      }
    }
  }
  ctx.restore();
  return cam;
}
