// Act IV. V2.6. The paper floods out from one vermilion point on the first pasted copy. A wide shot
// of the whole file, in two columns: Luau's pasted copies glow, lift off, fly home along arcs and
// snap into their helpers one after another; the file closes up and the calls type in. Then the
// constants Luau folded, each filling the frame; then the numbers.

import {
  ease, clamp, lerp, envelope, smoothstep, spring,
  layoutText, drawText, reveal, typewriter, measure, metrics, setFont, drawCounter, drawOdometer, formatNumber,
  layoutCode, codeMorph, drawMorph, codeMetrics, codeFont, rgba, mix, pixel, roundRect,
} from '../../engine/index.js';
import {
  T, ERA, TY, TRACK, AROUND, PAIRS, CODE, COL, S, surface, hairline, vline, faded, clipRect, camAt, zoomCam, lerpCam, label,
  monthYear, usDate, stripDot, hexOf, word, num,
} from './base.js';
import { cam251, pmeta } from './pages.js';

let H = null; // the homing analysis and the two-column layouts

// ------------------------------------------------------------------------------------- analysis
// Luau -O2 pasted helpers into their callers. From the token diff of V2.5.1's output and V2.6's:
// every edit hunk whose new text calls a helper defined in the file is a pasted copy of it. Its
// removed tokens fly home; the call types in where the copy was.

function analyzeHoming(plan) {
  const A = plan.a, B = plan.b;
  const removedByLine = new Map();
  plan.removed.forEach((r, i) => { if (!removedByLine.has(r.line)) removedByLine.set(r.line, []); removedByLine.get(r.line).push(i); });
  const keptA = new Map(); // A line -> Set of B lines
  const keptB = new Map(); // B line -> Set of A lines
  for (const g of plan.kept.values()) {
    for (let q = 0; q < g.nums.length; q += 5) {
      const la = g.nums[q + 1], lb = g.nums[q + 3];
      if (!keptA.has(la)) keptA.set(la, new Set());
      keptA.get(la).add(lb);
      if (!keptB.has(lb)) keptB.set(lb, new Set());
      keptB.get(lb).add(la);
    }
  }
  const insertedLines = new Set(plan.runs.map((r) => r.line));
  const helpers = new Map();
  B.lines.forEach((s, i) => {
    const m = s.match(/^local function (\w+)\(/);
    if (!m) return;
    let end = i;
    for (let j = i + 1; j < B.lines.length; j++) if (B.lines[j] === 'end') { end = j; break; }
    helpers.set(m[1], { name: m[1], line: i, end, col: s.indexOf(m[1]), len: m[1].length, copies: [], arrive: Infinity });
  });
  // the helper's definition in A (same name), where the copies land before the file closes up
  for (const h of helpers.values()) {
    const la = A.lines.findIndex((s) => s.startsWith(`local function ${h.name}(`));
    h.aLine = la >= 0 ? la : h.line;
    let end = h.aLine;
    for (let j = h.aLine + 1; j < A.lines.length; j++) if (A.lines[j] === 'end') { end = j; break; }
    h.aEnd = end;
    h.aCol = la >= 0 ? A.lines[la].indexOf(h.name) : h.col;
  }

  const hunks = [];
  let cur = null, prevAnchorB = -1;
  const closeHunk = (nextAnchorB) => {
    if (cur) { cur.b0 = prevAnchorB + 1; cur.b1 = nextAnchorB - 1; hunks.push(cur); cur = null; }
  };
  for (let la = 0; la < A.lineCount; la++) {
    const rem = removedByLine.get(la);
    const kb = keptA.get(la);
    if (!rem && !kb) continue;
    const unchanged = !rem && kb && kb.size === 1 && !insertedLines.has([...kb][0]);
    if (unchanged) { closeHunk([...kb][0]); prevAnchorB = [...kb][0]; continue; }
    if (!cur) cur = { lines: [], removed: [] };
    cur.lines.push(la);
    if (rem) cur.removed.push(...rem);
  }
  closeHunk(B.lineCount);

  const copies = [], exprs = [];
  for (const h of hunks) {
    if (!h.removed.length) continue;
    const runs = plan.runs.filter((r) => r.line >= h.b0 && r.line <= h.b1);
    let helper = null;
    for (const r of runs) {
      for (let k = 0; k < r.pieces.length; k++) {
        const pc = r.pieces[k];
        if (helpers.has(pc.text) && r.pieces[k + 1]?.text === '(') { helper = helpers.get(pc.text); break; }
      }
      if (helper) break;
    }
    for (const i of h.removed) {
      const r = plan.removed[i];
      if (r.k !== 'num' || r.text.length < 8) continue;
      const kb = keptA.get(r.line);
      if (!kb || kb.size !== 1) continue;
      const lb = [...kb][0];
      const lineText = B.lines[lb];
      const call = [...helpers.values()].map((hh) => lineText.match(new RegExp(`\\b${hh.name}\\([^()]*\\)`))).find(Boolean);
      const m = call || lineText.match(/\b\d+(?:\.\d+)? \/ \d+(?:\.\d+)?\b/);
      if (m) exprs.push({ from: r.text, to: m[0], line: lb, aLine: r.line, aText: A.lines[r.line], bText: lineText });
    }
    if (!helper) continue;
    const parts = h.removed.map((i) => plan.removed[i]);
    const c0 = Math.min(...parts.map((p) => p.col)), c1 = Math.max(...parts.map((p) => p.col + p.text.length));
    const l0 = Math.min(...parts.map((p) => p.line)), l1 = Math.max(...parts.map((p) => p.line));
    const copy = { helper, removed: new Set(h.removed), l0, l1, c0, c1, callLine: runs.find((r) => r.pieces.some((p) => p.text === helper.name))?.line };
    copies.push(copy);
    helper.copies.push(copy);
  }
  exprs.sort((a, b) => a.line - b.line);
  copies.sort((a, b) => a.l0 - b.l0);
  const copyOf = new Map();
  copies.forEach((c) => c.removed.forEach((i) => copyOf.set(i, c)));
  const helperList = [...helpers.values()].filter((h) => h.copies.length);
  // tokens on a helper's own lines (its parameter renamed, its comment) change when its copies land
  const helperOfALine = (l) => helperList.find((h) => l >= h.aLine && l <= h.aEnd) || null;
  const helperOfBLine = (l) => helperList.find((h) => l >= h.line && l <= h.end) || null;
  return { plan, copies, copyOf, helpers: helperList, exprs, helperOfALine, helperOfBLine };
}

// ---------------------------------------------------------------------------- two-column layout

/** Split a file into two columns at the blank line nearest its middle. */
function columns(L, size) {
  const n = L.lineCount;
  let split = Math.ceil(n / 2);
  for (let d = 0; d < 8; d++) {
    if (!L.lines[split + d]?.trim()) { split = split + d + 1; break; }
    if (!L.lines[split - d]?.trim()) { split = split - d + 1; break; }
  }
  const rows = Math.max(split, n - split);
  const { cw, lh } = S.CM;
  const s = size;
  const w0 = L.colsIn(0, split - 1) * cw * s, w1 = L.colsIn(split, n - 1) * cw * s;
  const gap = 110;
  const total = w0 + gap + w1;
  const x0 = Math.round(960 - total / 2), x1 = Math.round(x0 + w0 + gap);
  const top = Math.round(540 - (rows * lh * s) / 2) + 6;
  return { split, s, x: [x0, x1], top, rows };
}

export function prepareClimax() {
  const plan = codeMorph(S.ST['v2.5.1'].output, S.V26.output);
  H = analyzeHoming(plan);
  const { lh } = S.CM;
  // one scale per side: each fits its file in the frame height; the shorter file comes closer
  const rowsA = Math.ceil(plan.a.lineCount / 2) + 1, rowsB = Math.ceil(plan.b.lineCount / 2) + 1;
  H.colA = columns(plan.a, Math.min(0.66, 930 / (rowsA * lh)));
  H.colB = columns(plan.b, Math.min(0.8, 930 / (rowsB * lh)));
  // flights: in file order, one after another, each given room: a lift, a long arc, a snap
  const first = T.homing0 + 2.4, every = 2.6;
  H.copies.forEach((c, i) => {
    c.depart = first + i * every;
    c.lift = 0.5; c.fly = 1.6; c.land = 0.45;
    c.arrive = c.depart + c.lift + c.fly;
    c.helper.arrive = Math.min(c.helper.arrive, c.arrive);
    c.helper.lastArrive = Math.max(c.helper.lastArrive ?? 0, c.arrive);
  });
  // for every kept token, the helper whose lines it sits on in B (-1 for none)
  H.keptHelper = new Map();
  for (const [k, gr] of plan.kept) {
    const idx = new Int8Array(gr.text.length);
    for (let n = 0, q = 0; n < gr.text.length; n++, q += 5) {
      const bl = gr.nums[q + 3];
      idx[n] = H.helpers.findIndex((h) => bl >= h.line && bl <= h.end);
    }
    H.keptHelper.set(k, idx);
  }
  const lastArrive = Math.max(...H.copies.map((c) => c.arrive), T.homing0 + 4);
  H.dissolve0 = lastArrive + 0.5;
  H.glide0 = lastArrive + 0.7; H.glide1 = H.glide0 + 2.2;
  H.type0 = H.glide1 - 0.2; H.type1 = H.type0 + 1.9;
  // the folded constants, each filling the frame in turn
  H.exprs.forEach((x, i) => {
    x.t0 = T.consts0 + i * 3.1;
    // the call's comment is shown as a label under the close-up, so the expression can fill the frame
    const bare = x.bText.trim().replace(/\s*--\s*(.*)$/, '');
    x.comment = (x.bText.match(/--\s*(.*)$/) || [])[1] || '';
    x.plan = codeMorph(x.aText.trim(), bare);
  });
  // where the paper floods from: the first token of the first pasted copy, as V2.5.1's shot shows it
  const c0 = H.copies[0];
  if (c0) {
    const first = Math.min(...[...c0.removed]);
    const r = plan.removed[first];
    H.floodToken = { line: r.line, col: r.col, text: r.text };
  }
  return H;
}

export const hmeta = () => H;

/** Design position of grid cell (line, col) in a two-column layout. */
function at(C, line, col) {
  return [atX(C, line, col), atY(C, line)];
}
const atX = (C, line, col) => C.x[line >= C.split ? 1 : 0] + col * S.CM.cw * C.s;
const atY = (C, line) => C.top + (line >= C.split ? line - C.split : line) * S.CM.lh * C.s;

// ---------------------------------------------------------------------------------- the flood

function floodOrigin(t) {
  const P = pmeta();
  const f = H.floodToken;
  if (!f || !P) return [1300, 420];
  const cam = cam251(t);
  const { cw, lh } = S.CM;
  return [cam.x + (f.col + f.text.length / 2) * cw * cam.s, cam.y + (f.line + 0.5) * lh * cam.s];
}

/** Paper floods out of one vermilion point. No clip: a growing disc, the page drawn once it is full. */
function drawFlood(ctx, t) {
  const e = ERA.v26;
  const [ox, oy] = floodOrigin(T.flood);
  const dot = ease.out(clamp((t - T.flood) / 0.3));
  const grow = ease.inOutCubic(clamp((t - T.flood - 0.35) / 0.95));
  const far = Math.max(Math.hypot(ox, oy), Math.hypot(1920 - ox, oy), Math.hypot(ox, 1080 - oy), Math.hypot(1920 - ox, 1080 - oy)) + 4;
  if (grow >= 1) return true;
  ctx.save();
  if (grow > 0) {
    ctx.fillStyle = e.surface;
    ctx.beginPath();
    ctx.arc(ox, oy, far * grow, 0, Math.PI * 2);
    ctx.fill();
    // the accent rides the front of the flood
    ctx.strokeStyle = e.signal;
    ctx.lineWidth = lerp(6, 1.5, grow);
    ctx.globalAlpha = 1 - grow * 0.6;
    ctx.beginPath();
    ctx.arc(ox, oy, far * grow, 0, Math.PI * 2);
    ctx.stroke();
  }
  ctx.globalAlpha = 1;
  ctx.fillStyle = e.signal;
  const r = 7 * dot * (1 + 0.6 * Math.sin(clamp((t - T.flood) / 0.35) * Math.PI));
  if (r > 0) { ctx.beginPath(); ctx.arc(ox, oy, r, 0, Math.PI * 2); ctx.fill(); }
  ctx.restore();
  return false;
}

// ----------------------------------------------------------------------------------- the title

// The title fills the frame; when the wide shot opens, each of its lines flies into the corner
// label that stays over the wide shot ("Tovek V2.6  Inlined copies come home"), at that label's size.
const FLY = 1.0;
function cornerGeom() {
  const ty = TY.v26, s = S.V26;
  const kT = ty.corner.size / ty.tovek.size, kN = ty.corner.size / ty.giant.size;
  const Lt = layoutText('Tovek', ty.tovek, { pairs: PAIRS });
  const space = measure('Tovek ', ty.tovek) - measure('Tovek', ty.tovek);
  const nameX = 96 + (Lt.width - 0.01 * ty.tovek.size * 4 + space) * kT;
  const Ln = layoutText(s.name, ty.giant, { around: AROUND });
  const headX = nameX + (Ln.width - 0.01 * ty.giant.size * (s.name.length - 1)) * kN + 18;
  return { tovek: { x: 96, y: 66, k: kT }, name: { x: nameX, y: 66, k: kN }, head: { x: headX, y: 64, k: ty.cornerHead.size / ty.head.size } };
}

function drawTitle26(ctx, t) {
  const e = ERA.v26, ty = TY.v26, s = S.V26;
  const t0 = T.title26;
  const fly = ease.inOutCubic(clamp((t - (T.homing0 - FLY)) / FLY));
  if (fly >= 1) return;
  const C = cornerGeom();
  const G = ty.giant;
  const Lg = layoutText(s.name, G, { around: AROUND });
  const wg = Lg.width + TRACK.title * G.size * (s.name.length - 1);
  const Lt = layoutText('Tovek', ty.tovek, { pairs: PAIRS });
  const head = stripDot(s.headline);
  const Lh = layoutText(head, ty.head);
  const dot = s.headline.endsWith('.') ? layoutText('.', ty.head) : null;
  const wh = Lh.width + (dot ? dot.width : 0);
  // big positions (left edge, baseline), centred in the frame
  const big = { name: { x: 960 - wg / 2, y: 724 }, tovek: { x: 960 - Lt.width / 2, y: 284 }, head: { x: 960 - wh / 2, y: 846 } };
  const at = (b, c) => ({ x: lerp(b.x, c.x, fly), y: lerp(b.y, c.y, fly), k: lerp(1, c.k, fly) });
  const place = (p, fn) => { ctx.save(); ctx.translate(p.x, p.y); ctx.scale(p.k, p.k); fn(); ctx.restore(); };
  // the name: glyph by glyph, then whole as it flies
  place(at(big.name, C.name), () => {
    if (fly <= 0) reveal(ctx, Lg, 0, 0, t, { unit: 'glyph', start: t0 + 0.15, stagger: 0.07, dur: 0.95, color: e.ink, tracking: TRACK.title });
    else drawText(ctx, Lg, 0, 0, { color: e.ink, tracking: lerp(TRACK.title, -0.01, fly) });
  });
  place(at(big.tovek, C.tovek), () => {
    if (fly <= 0) reveal(ctx, Lt, 0, 0, t, { unit: 'line', start: t0, dur: 0.8, color: e.ink, tracking: -0.01 });
    else drawText(ctx, Lt, 0, 0, { color: e.ink, tracking: -0.01 });
  });
  // the headline flies too but fades before it lands: over the wide shot the corner keeps only the
  // name, clear of the first line of code
  const headA = 1 - smoothstep(0.35, 0.85, fly);
  if (headA > 0) place(at(big.head, C.head), () => {
    const col = e.ink2;
    ctx.globalAlpha *= headA;
    if (fly <= 0) {
      reveal(ctx, Lh, 0, 0, t, { unit: 'word', start: t0 + 0.7, stagger: 0.07, dur: 0.8, color: col });
      if (dot) reveal(ctx, dot, Lh.width, 0, t, { unit: 'line', start: t0 + 0.7 + 0.07 * Lh.wordCount, dur: 0.8, color: col });
    } else {
      drawText(ctx, Lh, 0, 0, { color: col });
      if (dot) drawText(ctx, dot, Lh.width, 0, { color: col, alpha: 1 - fly });
    }
  });
  const meta = `${(s.released ? usDate(s.date) : monthYear(s.date)).toUpperCase()}  ·  RELEASE ${S.RIDX.get(s)} OF ${S.D.totals.releases}`;
  faded(ctx, 1 - ease.out(clamp(fly * 2.5)), () => {
    reveal(ctx, layoutText(meta, ty.meta), 960, 200, t, { unit: 'line', start: t0, dur: 0.8, color: e.ink2, align: 'center', tracking: 0.12 });
  });
}

/** The quiet corner label that stays over the wide shot: exactly where the title's flight ends. */
function drawCorner(ctx, t, ox = 0, oy = 0) {
  const e = ERA.v26, ty = TY.v26, s = S.V26;
  if (t < T.homing0) return;
  const a = 1 - ease.inOut(clamp((t - (T.consts0 - 1.1)) / 0.5));
  if (a <= 0) return;
  const C = cornerGeom();
  const place = (p, fn) => { ctx.save(); ctx.translate(p.x, p.y); ctx.scale(p.k, p.k); fn(); ctx.restore(); };
  ctx.save();
  ctx.globalAlpha *= a;
  ctx.translate(ox, oy);
  place(C.tovek, () => drawText(ctx, layoutText('Tovek', ty.tovek, { pairs: PAIRS }), 0, 0, { color: e.ink, tracking: -0.01 }));
  place(C.name, () => drawText(ctx, layoutText(s.name, ty.giant, { around: AROUND }), 0, 0, { color: e.ink, tracking: -0.01 }));
  ctx.restore();
}

// --------------------------------------------------------------------------------- the climax

function drawHoming(ctx, t) {
  const e = ERA.v26, plan = H.plan, pal = S.PAL.v26;
  const { cw, lh, baseline } = S.CM;
  const A = H.colA, B = H.colB;
  const g = ease.inOutCubic(clamp((t - H.glide0) / (H.glide1 - H.glide0)));
  const s = lerp(A.s, B.s, g);
  const fontPx = CODE.size;
  const toCam = (x, y) => [x / s, y / s];
  ctx.save();
  ctx.scale(s, s);
  setFont(ctx, codeFont(fontPx));
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  const base = ctx.globalAlpha;
  const fadeIn = ease.out(clamp((t - (T.homing0 - 0.55)) / 1.0));
  const posA = (l, c) => at(A, l, c);
  const posB = (l, c) => at(B, l, c);

  // 1. kept tokens hold in A. A helper's own lines settle into their new columns when its first copy
  //    lands; then everything glides to B as the gaps close and the columns reflow. Across the glide
  //    x moves together (so words keep their spacing at the changing scale); y ripples by line.
  const raw = clamp((t - H.glide0) / (H.glide1 - H.glide0));
  const spread = 0.3;
  const helperIn = H.helpers.map((h) => ease.inOut(clamp((t - h.arrive - 0.05) / 0.45)));
  const inv = 1 / s, by0 = baseline * s;
  // while the copies fly, the rest of the file steps back; the helpers they fly to stay lit
  const focus = envelope(t, T.homing0 + 0.4, H.glide0 + 0.3, 0.9, 0.7, ease.inOut, ease.inOut);
  const dimA = base * fadeIn * (1 - 0.6 * focus), litA = base * fadeIn;
  for (const [k, gr] of plan.kept) {
    ctx.fillStyle = pal[k] || pal.id;
    const nums = gr.nums, texts = gr.text, hix = H.keptHelper.get(k);
    for (let n = 0, q = 0; n < texts.length; n++, q += 5) {
      const al = nums[q + 1], ac = nums[q], bl = nums[q + 3], bc = nums[q + 2];
      let ax = atX(A, al, ac), ay = atY(A, al);
      const hi = hix[n];
      ctx.globalAlpha = hi >= 0 ? litA : dimA;
      if (hi >= 0 && helperIn[hi] > 0) {
        const h = H.helpers[hi], hl = h.aLine + (bl - h.line), e2 = helperIn[hi];
        ax += (atX(A, hl, bc) - ax) * e2; ay += (atY(A, hl) - ay) * e2;
      }
      const d = nums[q + 4] * spread;
      const ey = raw <= 0 ? 0 : raw >= 1 ? 1 : ease.inOut(clamp((raw - d) / (1 - spread)));
      const x = ax + (atX(B, bl, bc) - ax) * g, y = ay + (atY(B, bl) - ay) * ey + by0;
      ctx.fillText(texts[n], x * inv, y * inv);
    }
  }

  // 2. the helpers' definitions: outlined as destinations, flashing as each copy lands
  for (const h of H.helpers) {
    const ready = ease.out(clamp((t - T.homing0 - 0.4) / 0.6)) * (1 - ease.inOut(clamp((t - H.glide0 + 0.4) / 0.6)));
    let flash = 0;
    for (const c of h.copies) flash = Math.max(flash, envelope(t, c.arrive - 0.05, c.arrive + 0.9, 0.05, 0.8));
    if (ready > 0 || flash > 0) {
      const [x0, y0] = posA(h.aLine, 0);
      const wLine = Math.max(...Array.from({ length: h.aEnd - h.aLine + 1 }, (_, i) => plan.a.lineCols[h.aLine + i] || 0));
      const bw = (wLine + 1.5) * cw * A.s, bh = (h.aEnd - h.aLine + 1) * lh * A.s;
      ctx.save();
      ctx.globalAlpha = base * Math.max(ready * 0.9, flash);
      ctx.fillStyle = rgba(e.signal, 0.05 + 0.12 * flash);
      roundRect(ctx, (x0 - cw * A.s * 0.8) / s, (y0 - lh * A.s * 0.1) / s, (bw + cw * A.s * 0.8) / s, (bh + lh * A.s * 0.2) / s, 6 / s);
      ctx.fillStyle = e.accent;
      ctx.fillRect((x0 - cw * A.s * 0.8) / s, (y0 - lh * A.s * 0.1) / s, 2.5 / s, (bh + lh * A.s * 0.2) / s);
      // each arrival sends a ring out from the definition
      for (const c of h.copies) {
        const w = clamp((t - c.arrive) / 0.85);
        if (w <= 0 || w >= 1) continue;
        const g = 4 + 52 * ease.out(w);
        ctx.globalAlpha = base * (1 - w) * 0.9;
        ctx.strokeStyle = e.signal;
        ctx.lineWidth = lerp(3, 1, w) / s;
        const rx = x0 - cw * A.s * 0.8 - g, ry = y0 - lh * A.s * 0.1 - g;
        const rw = bw + cw * A.s * 0.8 + g * 2, rh = bh + lh * A.s * 0.2 + g * 2;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(rx / s, ry / s, rw / s, rh / s, (6 + g * 0.5) / s);
        else ctx.rect(rx / s, ry / s, rw / s, rh / s);
        ctx.stroke();
      }
      ctx.restore();
      // a tally of the copies that came home, in the margin beside the definition
      const home = h.copies.filter((c) => t >= c.arrive).length;
      if (home > 0) {
        const [tx, ty] = posA(h.aLine, 0);
        ctx.save();
        ctx.globalAlpha = base * (1 - ease.inOut(clamp((t - H.glide0 + 0.4) / 0.6)));
        setFont(ctx, TY.v26.tally);
        ctx.fillStyle = e.accent;
        ctx.textAlign = 'right';
        ctx.fillText(`${home} of ${h.copies.length} home`, (tx - cw * A.s * 2) / s, (ty + baseline * A.s) / s);
        ctx.restore();
        setFont(ctx, codeFont(fontPx));
      }
    }
  }

  // 3. the copies: each glows, a route is drawn ahead of it, it lifts off its line, flies home along
  //    the arc and snaps into its helper's definition (a ring goes out from there, see above)
  H.copies.forEach((c, ci) => {
    if (t >= c.arrive + c.land) return;
    const lit = ease.out(clamp((t - (T.homing0 + 0.3 + ci * 0.45)) / 0.5));
    const [ax0, ay0] = posA(c.l0, c.c0);
    const [ax1, ay1] = posA(c.l1 + 1, c.c1);
    const pad = cw * A.s * 0.8;
    const cardX = ax0 - pad, cardY = ay0, cardW = ax1 - ax0 + pad * 2, cardH = ay1 - ay0;
    const cx = cardX + cardW / 2, cy = cardY + cardH / 2;
    // the destination: the helper's body as V2.5.1 wrote it
    const h = c.helper;
    const [hx0, hy0] = posA(h.aLine, 0);
    let wLine = 0;
    for (let l = h.aLine; l <= h.aEnd; l++) wLine = Math.max(wLine, plan.a.lineCols[l] || 0);
    const boxW = (wLine + 1.5) * cw * A.s + pad, boxH = (h.aEnd - h.aLine + 1) * lh * A.s;
    const tx = hx0 - pad + boxW / 2, ty2 = hy0 + boxH / 2;
    // the route: one arc up and over, its top kept inside the frame; a copy right below its helper
    // swings out to the side so the jump still reads as a flight
    const dist = Math.hypot(tx - cx, ty2 - cy);
    const side = Math.abs(tx - cx) < 260 ? 340 : 0;
    const ctrl = [lerp(cx, tx, 0.5) + side, Math.min(cy, ty2) - clamp(dist * 0.45, 160, 420)];
    // the apex of the arc, (cy + 2 ctrl + ty2) / 4, stays low enough that the lifted card is in frame
    const apex = Math.max(70, cardH * 0.56 + 36);
    ctrl[1] = Math.max(ctrl[1], (4 * apex - cy - ty2) / 2);
    const pt = (u) => [(1 - u) * (1 - u) * cx + 2 * (1 - u) * u * ctrl[0] + u * u * tx, (1 - u) * (1 - u) * cy + 2 * (1 - u) * u * ctrl[1] + u * u * ty2];
    const liftP = ease.out(clamp((t - c.depart) / c.lift));
    const flyP = ease.inOutCubic(clamp((t - c.depart - c.lift) / c.fly));
    const landT = t - c.arrive, landP = clamp(landT / c.land);
    // a polyline along the route from u0 to u1, each segment with its own alpha
    const route = (u0, u1, steps, alphaOf) => {
      let prev = pt(u0);
      for (let i = 1; i <= steps; i++) {
        const p2 = pt(u0 + ((u1 - u0) * i) / steps);
        ctx.globalAlpha = base * alphaOf(i / steps);
        ctx.beginPath();
        ctx.moveTo(prev[0] / s, prev[1] / s);
        ctx.lineTo(p2[0] / s, p2[1] / s);
        ctx.stroke();
        prev = p2;
      }
    };
    ctx.save();
    ctx.strokeStyle = e.signal;
    ctx.lineCap = 'round';
    // the flight plan: a dashed arc drawn out from the copy that flies next
    const plan0 = ease.inOut(clamp((t - Math.max(T.homing0 + 0.9, c.depart - 1.6)) / 1.0));
    const planA = plan0 * (1 - ease.inOut(clamp((t - c.depart - c.lift) / (c.fly * 0.7)))) * 0.5 * fadeIn;
    if (planA > 0) {
      ctx.lineWidth = 1.6 / s;
      ctx.setLineDash([6 / s, 7 / s]);
      ctx.lineDashOffset = (-t * 18) / s;
      route(0, plan0, 40, () => planA);
      ctx.setLineDash([]);
    }
    // the trail: a solid stroke behind the card, brightening toward it
    if (flyP > 0 && flyP < 1) {
      const tail = Math.max(0, flyP - 0.55);
      ctx.lineWidth = 3.2 / s;
      route(tail, flyP, 28, (q) => 0.75 * q * q);
    }
    ctx.restore();
    // the card's pose: lifted, flown, then folded into the helper's own box (its width on the way,
    // its lines closing up into the helper's lines at the end), then a short pop as it snaps in
    const [px, py] = flyP > 0 ? pt(flyP) : [cx, cy];
    const lifted = 1 + 0.07 * liftP;
    let kx = lerp(lifted, Math.min(1.25, boxW / cardW), ease.inOutCubic(flyP));
    let ky = lerp(lifted, Math.min(1.25, boxH / cardH), ease.inQuart(flyP));
    if (landT > 0) { const pop = 1 + 0.06 * Math.sin(Math.PI * clamp(landT / 0.3)); kx *= pop; ky *= pop; }
    const alpha = (1 - ease.inQuad(landP)) * fadeIn;
    const wait = t < c.depart ? 0.78 + 0.22 * Math.sin((t - T.homing0) * 4.4 + ci * 1.9) : 1 - 0.4 * flyP;
    const glow = lit * wait;
    ctx.save();
    ctx.globalAlpha = base * alpha;
    ctx.translate(px / s, py / s);
    ctx.scale(kx, ky);
    ctx.translate(-cx / s, -cy / s);
    const rr = (x, y, w, hh, r) => roundRect(ctx, x / s, y / s, w / s, hh / s, r / s);
    // its shadow, deeper as it lifts
    if (liftP > 0) {
      for (let j = 1; j <= 3; j++) {
        ctx.fillStyle = rgba(e.ink, 0.04 * liftP * (1 - flyP * 0.5));
        const o = (4 + 5 * j) * liftP;
        rr(cardX + o * 0.6, cardY + o, cardW, cardH, 8 + j * 2);
      }
    }
    // its glow: soft layers of the signal colour around the card
    if (glow > 0) {
      for (let j = 9; j >= 1; j--) {
        const g = j * 3.6;
        ctx.fillStyle = rgba(e.signal, 0.022 * glow);
        rr(cardX - g, cardY - g, cardW + g * 2, cardH + g * 2, 8 + g);
      }
    }
    ctx.fillStyle = mix(e.surface, e.signal, (0.13 + 0.09 * liftP) * lit);
    rr(cardX, cardY, cardW, cardH, 8);
    ctx.fillStyle = e.accent;
    ctx.globalAlpha = base * alpha * lit;
    ctx.fillRect(cardX / s, cardY / s, 3 / s, cardH / s);
    ctx.globalAlpha = base * alpha;
    for (const i of c.removed) {
      const r = plan.removed[i];
      const [x, y] = posA(r.line, r.col);
      ctx.fillStyle = lit >= 1 ? e.accent : mix(hexOf(pal[r.k] || pal.id), e.accent, lit);
      ctx.fillText(r.text, x / s, (y + baseline * A.s) / s);
    }
    ctx.restore();
    // the helper's name beside the copy, until it leaves
    if (t < c.depart + 0.2) {
      const [nx, ny] = posA(c.l0, c.c1 + 3);
      ctx.save();
      ctx.globalAlpha = base * lit * fadeIn * (1 - clamp((t - c.depart) / 0.2));
      ctx.fillStyle = e.accent;
      ctx.fillText(`→ ${c.helper.name}`, nx / s, (ny + baseline * A.s) / s);
      ctx.restore();
    }
  });
  // other removed tokens dissolve once every copy is home; helper-line tokens change as copies land
  for (let i = 0; i < plan.removed.length; i++) {
    if (H.copyOf.has(i)) continue;
    const r = plan.removed[i];
    const h = H.helperOfALine(r.line);
    const t0 = h ? h.arrive + 0.05 : H.dissolve0;
    const q = ease.inOut(clamp((t - t0) / 0.5));
    if (q >= 1) continue;
    const [x, y] = posA(r.line, r.col);
    ctx.globalAlpha = base * (1 - q) * fadeIn;
    ctx.fillStyle = pal[r.k] || pal.id;
    ctx.fillText(r.text, x / s, (y + baseline * A.s - q * lh * A.s * 0.35) / s);
  }

  // 4. new text: on a helper's lines as its copies land (its comment counts them), the calls after the glide
  for (const run of plan.runs) {
    const h = H.helperOfBLine(run.line);
    const comment = run.pieces.every((pc) => pc.k === 'com');
    const start = h ? (comment ? h.lastArrive + 0.2 : h.arrive + 0.2) : H.type0 + (run.line / Math.max(1, plan.b.lineCount)) * 0.9;
    const rate = h ? 30 : 46;
    const typed = (t - start) * rate;
    if (typed <= 0) continue;
    for (const pc of run.pieces) {
      const visible = typed - pc.at;
      if (visible <= 0) break;
      // on helper lines the text rides the glide exactly like the kept text of its line
      const bx = atX(B, run.line, pc.col), by = atY(B, run.line);
      let x = bx, y = by;
      if (h) {
        const al = h.aLine + (run.line - h.line);
        const d = ((run.line / Math.max(1, plan.b.lineCount - 1)) * 0.6 + (al / Math.max(1, plan.a.lineCount - 1)) * 0.4) * spread;
        const ey = raw <= 0 ? 0 : raw >= 1 ? 1 : ease.inOut(clamp((raw - d) / (1 - spread)));
        const ax = atX(A, al, pc.col), ay = atY(A, al);
        x = ax + (bx - ax) * g; y = ay + (by - ay) * ey;
      }
      y += baseline * s;
      ctx.fillStyle = e.accent;
      const whole = Math.min(pc.text.length, Math.floor(visible));
      ctx.globalAlpha = base;
      if (whole > 0) ctx.fillText(pc.text.slice(0, whole), x / s, y / s);
      if (whole < pc.text.length) {
        ctx.globalAlpha = base * (visible - whole);
        ctx.fillText(pc.text[whole], (x + whole * cw * s) / s, y / s);
      }
    }
  }
  ctx.restore();
}

/** The stats under the closed-up file: lines, and the calls rebuilt. */
function drawHomingStats(ctx, t) {
  const e = ERA.v26, ty = TY.v26;
  const a = envelope(t, H.type1 - 0.2, T.consts0 - 0.4, 0.6, 0.4);
  if (a <= 0) return;
  const v = S.V26, p = S.ST['v2.5.1'];
  const txt = `${p.lines} → ${v.lines} lines  ·  ${v.features.rebuilt_calls} calls rebuilt`;
  label(ctx, txt.toUpperCase(), 1824, 66, e.ink2, { alpha: a, align: 'right', tracking: 0.1, font: ty.small });
}

// ------------------------------------------------------------------------------- the constants

function drawConstants(ctx, t) {
  const e = ERA.v26, pal = S.PAL.v26;
  const { cw, lh } = S.CM;
  for (const x of H.exprs) {
    if (t < x.t0 - 0.3 || t > x.t0 + 3.15) continue;
    const pl = x.plan;
    const a = 1;
    // A: the whole line, then a push until the folded constant fills the frame
    const iA = pl.a.lines[0].indexOf(x.from);
    const iB = pl.b.lines[0].indexOf(x.to);
    const first = x === H.exprs[0];
    const lineCam = { s: 2.6, x: 960 - (first ? iA + x.from.length / 2 : pl.a.cols / 2) * cw * 2.6, y: 540 - 0.5 * lh * 2.6 };
    const sA = Math.min(7.2, 1600 / (x.from.length * cw));
    const camA = { s: sA, x: 960 - (iA + x.from.length / 2) * cw * sA, y: 540 - 0.5 * lh * sA };
    const sB = Math.min(12, 1100 / (x.to.length * cw));
    const camB = { s: sB, x: 960 - (iB + x.to.length / 2) * cw * sB, y: 540 - 0.5 * lh * sB };
    const push = ease.inOutCubic(clamp((t - x.t0 - 0.55) / 0.7));
    const pm = clamp((t - x.t0 - 1.55) / 0.85);
    ctx.save();
    if (pm <= 0) {
      const cam = zoomCam(lineCam, camA, push);
      const typed = first ? Infinity : clamp((t - x.t0) / 0.45) * pl.a.src.length;
      ctx.globalAlpha *= first ? ease.out(clamp((t - x.t0 + 0.25) / 0.35)) : 1;
      ctx.translate(cam.x, cam.y);
      ctx.scale(cam.s, cam.s);
      setFont(ctx, codeFont(CODE.size));
      ctx.textBaseline = 'alphabetic';
      for (const tk of pl.a.tokens) {
        const vis = typed - tk.col;
        if (vis <= 0) continue;
        ctx.fillStyle = pal[tk.k] || pal.id;
        ctx.fillText(tk.text.slice(0, Math.ceil(vis)), tk.col * cw, S.CM.baseline);
      }
      ctx.restore();
      // the strike through the folded constant
      const strike = ease.inOut(clamp((t - x.t0 - 1.2) / 0.35));
      if (strike > 0) {
        const xa = cam.x + iA * cw * cam.s, w = x.from.length * cw * cam.s;
        ctx.fillStyle = e.ink2;
        ctx.fillRect(xa, cam.y + lh * cam.s * 0.52, w * strike, Math.max(2, cam.s * 0.9));
      }
    } else {
      drawMorph(ctx, pl, pm, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, cameraA: camA, cameraB: camB, blur: 8, timing: { out: [0, 0.45], move: [0.1, 0.9], in: [0.45, 1] } });
      ctx.restore();
    }
    // soft frame edges: the close-up runs off the frame into the paper
    for (const [x0, x1] of [[0, 220], [1920, 1700]]) {
      const g = ctx.createLinearGradient(x0, 0, x1, 0);
      g.addColorStop(0, rgba(e.surface, 1));
      g.addColorStop(1, rgba(e.surface, 0));
      ctx.fillStyle = g;
      ctx.fillRect(Math.min(x0, x1), 0, 220, 1080);
    }
    // labels: what it was, and what it is
    const la = envelope(t, x.t0 + 0.3, x.t0 + 3.05, 0.4, 0.35);
    label(ctx, 'FOLDED BY LUAU', 960, 230, e.ink2, { alpha: la * (1 - clamp(pm * 2)), align: 'center' });
    label(ctx, 'SOLVED BACK', 960, 230, e.accent, { alpha: la * clamp(pm * 2 - 1), align: 'center' });
    if (x.comment) drawText(ctx, layoutText('-- ' + x.comment, TY.dark.line), 960, 820, { color: e.accent, align: 'center', alpha: la * clamp(pm * 2.2 - 1.2) });
  }
}

// --------------------------------------------------------------------------------- the numbers

// Two numbers, each filling the frame. The first rolls up from V2.5.1's count; then the frame
// scrolls on like an odometer drum and the second counts V2.5.1's wrong outputs down to zero.
function drawNumbers(ctx, t) {
  const e = ERA.v26, ty = TY.v26;
  const n = S.N.v26a, z = S.N.v26b;
  if (!n) return;
  const t0 = T.nums;
  const out = ease.inOut(clamp((t - (T.orig - 0.6)) / 0.6));
  const turn = z ? ease.inOutCubic(clamp((t - t0 - 2.8) / 0.7)) : 0;
  const F = ty.ecu, capF = metrics(F).capHeight || F.size * 0.72;
  const yNum = 540 + capF / 2 - 10;
  ctx.save();
  ctx.globalAlpha *= 1 - out;
  // 1. rebuilt calls across four real games
  if (turn < 1) {
    ctx.save();
    ctx.translate(0, -turn * 1080);
    const a = ease.out(clamp((t - t0) / 0.4));
    faded(ctx, a, () => {
      label(ctx, `${S.ST['v2.5.1'].name.toUpperCase()}: ${formatNumber(n.from ?? 0)}`, 960, yNum - capF - 70, e.ink3, { align: 'center' });
      drawOdometer(ctx, t, { from: n.from ?? 0, to: n.to, start: t0 + 0.25, dur: 2.0, x: 960, y: yNum, font: F, align: 'center', color: e.ink, turns: 1 });
      drawText(ctx, layoutText(n.label, ty.ecuLab), 960, yNum + 92, { color: e.ink2, align: 'center' });
      if (n.paren) drawText(ctx, layoutText(n.paren, ty.ecuNote), 960, yNum + 138, { color: e.ink3, align: 'center' });
    });
    ctx.restore();
  }
  // 2. fresh fuzzing: V2.5.1's wrong outputs, counted down to none
  if (z && turn > 0) {
    const m = z.label.match(/^(wrong outputs) in (.+)$/);
    const was = (z.paren || '').match(/^(.+?): ([\d,]+)$/);
    const from = was ? num(was[2]) : z.to;
    const start = t0 + 3.25, dur = 1.4;
    const landed = clamp((t - start - dur) / 0.4);
    ctx.save();
    ctx.translate(0, (1 - turn) * 1080);
    if (was) label(ctx, `${was[1].toUpperCase()}: ${was[2]}`, 960, yNum - capF - 70, e.ink3, { align: 'center' });
    drawOdometer(ctx, t, { from, to: z.to, start, dur, x: 960, y: yNum, font: F, align: 'center', color: mix(e.ink, e.accent, landed), exact: true, ease: ease.inOut });
    drawText(ctx, layoutText(m ? m[1] : z.label, ty.ecuLab), 960, yNum + 92, { color: e.ink2, align: 'center' });
    if (m) drawText(ctx, layoutText(`in ${m[2]}`, ty.ecuNote), 960, yNum + 138, { color: e.ink3, align: 'center' });
    ctx.restore();
  }
  ctx.restore();
}

// --------------------------------------------------------------------------------------- the act

export function drawV26(ctx, t) {
  const e = ERA.v26;
  const full = t >= T.flood + 1.3 ? true : drawFlood(ctx, t);
  if (!full) return;
  surface(ctx, e.surface);
  if (t < T.homing0) drawTitle26(ctx, t);
  if (t > T.homing0 - 1.3 && t < T.consts0 + 0.2) {
    // the push into the first constant at the end of the wide shot
    const push = ease.inOutCubic(clamp((t - (T.consts0 - 1.3)) / 1.3));
    if (push > 0 && H.exprs[0]) {
      const x = H.exprs[0];
      const [px, py] = at(H.colB, x.line, (H.plan.b.lines[x.line].indexOf(x.to) + x.to.length / 2));
      const k = Math.exp(lerp(0, Math.log(2.6 / H.colB.s), push));
      ctx.save();
      // scale about the constant, moving it to the centre
      const tx = lerp(px, 960, push), ty = lerp(py, 540, push);
      ctx.translate(tx, ty);
      ctx.scale(k, k);
      ctx.translate(-px, -py);
      ctx.globalAlpha *= 1 - smoothstep(0.7, 1, push);
      drawHoming(ctx, t);
      ctx.restore();
    } else {
      // a slow push and slide while the copies fly home, back to rest as the file closes up
      const u = clamp((t - T.homing0) / (H.glide1 - T.homing0));
      const bump = (1 - Math.cos(2 * Math.PI * u)) / 2;
      const k = 1 + 0.045 * bump;
      ctx.save();
      ctx.translate(960 - 22 * bump, 540 - 8 * bump);
      ctx.scale(k, k);
      ctx.translate(-960, -540);
      drawHoming(ctx, t);
      ctx.restore();
    }
    {
      const u = clamp((t - T.homing0) / (H.glide1 - T.homing0));
      const bump = (1 - Math.cos(2 * Math.PI * u)) / 2;
      drawCorner(ctx, t, -22 * bump * 0.3, -8 * bump * 0.3);
    }
    drawHomingStats(ctx, t);
  }
  if (t >= T.consts0 - 0.3 && t < T.nums + 0.3) drawConstants(ctx, t);
  if (t >= T.nums - 0.05) drawNumbers(ctx, t);
}
