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
  T, ERA, TY, TRACK, AROUND, CODE, COL, S, surface, hairline, vline, faded, clipRect, camAt, zoomCam, lerpCam, label,
  monthYear, usDate, stripDot, hexOf, word,
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
  // flights: in file order, one after another
  const first = T.homing0 + 2.0, every = 2.0;
  H.copies.forEach((c, i) => {
    c.depart = first + i * every;
    c.lift = 0.3; c.fly = 1.15; c.land = 0.25;
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

function drawTitle26(ctx, t) {
  const e = ERA.v26, ty = TY.v26, s = S.V26;
  const t0 = T.title26;
  const shrink = ease.inOutCubic(clamp((t - (T.homing0 - 0.9)) / 0.9));
  if (shrink >= 1) return;
  const a = 1 - shrink;
  ctx.save();
  ctx.globalAlpha *= a;
  const L = layoutText(s.name, ty.title, { around: AROUND });
  const k = lerp(1, 0.6, shrink);
  ctx.translate(960, lerp(560, 300, shrink));
  ctx.scale(k, k);
  reveal(ctx, L, 0, 0, t, { unit: 'glyph', start: t0 + 0.15, stagger: 0.06, dur: 0.9, color: e.ink, align: 'center', tracking: TRACK.title });
  ctx.restore();
  const meta = `${(s.released ? usDate(s.date) : monthYear(s.date)).toUpperCase()}  ·  RELEASE ${S.RIDX.get(s)} OF ${S.D.totals.releases}`;
  faded(ctx, a, () => {
    reveal(ctx, layoutText('Tovek', ty.tovek), 960, 300, t, { unit: 'line', start: t0, dur: 0.8, color: e.ink, align: 'center', tracking: -0.01 });
    reveal(ctx, layoutText(meta, ty.meta), 960, 220, t, { unit: 'line', start: t0, dur: 0.8, color: e.ink2, align: 'center', tracking: 0.12 });
    reveal(ctx, layoutText(s.headline, ty.head), 960, 680, t, { unit: 'word', start: t0 + 0.6, stagger: 0.07, dur: 0.8, color: e.ink2, align: 'center' });
  });
}

/** The quiet corner label that stays over the wide shot. */
function drawCorner(ctx, t) {
  const e = ERA.v26, ty = TY.v26, s = S.V26;
  const a = envelope(t, T.homing0 - 0.4, T.consts0 - 0.6, 0.6, 0.5);
  if (a <= 0) return;
  ctx.save();
  ctx.globalAlpha *= a;
  const L = layoutText(`Tovek ${s.name}`, ty.corner, { around: AROUND });
  drawText(ctx, L, 96, 66, { color: e.ink, tracking: -0.01 });
  drawText(ctx, layoutText(stripDot(s.headline), ty.cornerHead), 96 + L.width + 18, 64, { color: e.ink2 });
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
  for (const [k, gr] of plan.kept) {
    ctx.fillStyle = pal[k] || pal.id;
    ctx.globalAlpha = base * fadeIn;
    const nums = gr.nums, texts = gr.text, hix = H.keptHelper.get(k);
    for (let n = 0, q = 0; n < texts.length; n++, q += 5) {
      const al = nums[q + 1], ac = nums[q], bl = nums[q + 3], bc = nums[q + 2];
      let ax = atX(A, al, ac), ay = atY(A, al);
      const hi = hix[n];
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

  // 3. removed tokens: the copies glow, lift, fly home; the rest dissolve before the file closes up
  const select = (c) => ease.out(clamp((t - (T.homing0 + 0.3 + H.copies.indexOf(c) * 0.45)) / 0.5));
  for (const c of H.copies) {
    const q = t < c.depart ? 0 : clamp((t - c.depart) / (c.lift + c.fly));
    if (t >= c.arrive + c.land) continue;
    const lit = select(c);
    const [ax0, ay0] = posA(c.l0, c.c0);
    const [ax1, ay1] = posA(c.l1 + 1, c.c1);
    const cx = (ax0 + ax1) / 2, cy = (ay0 + ay1) / 2;
    const [hx, hy] = posA(c.helper.aLine, c.helper.aCol + c.helper.len / 2);
    const tx = hx, ty2 = hy + lh * A.s * 0.5;
    // the route: up and over, the long flights arc across the frame
    const dist = Math.hypot(tx - cx, ty2 - cy);
    const ctrl = [lerp(cx, tx, 0.5) + dist * 0.28 + 60, Math.min(cy, ty2) - dist * 0.32 - 90];
    const pt = (u) => [(1 - u) * (1 - u) * cx + 2 * (1 - u) * u * ctrl[0] + u * u * tx, (1 - u) * (1 - u) * cy + 2 * (1 - u) * u * ctrl[1] + u * u * ty2];
    const liftP = ease.out(clamp((t - c.depart) / c.lift));
    const flyP = ease.inOutCubic(clamp((t - c.depart - c.lift) / c.fly));
    const landP = clamp((t - c.arrive) / c.land);
    const [px, py] = flyP > 0 ? pt(flyP) : [cx, cy];
    const k = lerp(1, 0.18, ease.inQuad(flyP)) * (1 + 0.06 * liftP * (1 - flyP));
    const alpha = 1 - landP;
    // trail
    if (flyP > 0 && flyP < 1) {
      ctx.save();
      ctx.strokeStyle = e.signal;
      ctx.lineWidth = 2 / s;
      ctx.lineCap = 'round';
      const tail = Math.max(0, flyP - 0.45), steps = 22;
      let prev = pt(tail);
      for (let i = 1; i <= steps; i++) {
        const p2 = pt(tail + ((flyP - tail) * i) / steps);
        ctx.globalAlpha = base * 0.5 * (i / steps);
        ctx.beginPath();
        ctx.moveTo(prev[0] / s, prev[1] / s);
        ctx.lineTo(p2[0] / s, p2[1] / s);
        ctx.stroke();
        prev = p2;
      }
      ctx.restore();
    }
    // the card: a band behind the copy, its tokens in the accent
    ctx.save();
    ctx.globalAlpha = base * alpha * fadeIn;
    ctx.translate(px / s, py / s);
    ctx.scale(k, k);
    ctx.translate(-cx / s, -cy / s);
    const bw = ax1 - ax0 + cw * A.s * 1.6, bh = ay1 - ay0;
    if (liftP > 0) {
      ctx.fillStyle = rgba(e.ink, 0.07 * liftP);
      roundRect(ctx, (ax0 - cw * A.s * 0.8 + 6) / s, (ay0 + 9) / s, bw / s, bh / s, 8 / s);
    }
    ctx.fillStyle = mix(e.surface, e.signal, (0.12 + 0.06 * liftP) * lit);
    roundRect(ctx, (ax0 - cw * A.s * 0.8) / s, ay0 / s, bw / s, bh / s, 8 / s);
    ctx.fillStyle = e.accent;
    ctx.globalAlpha = base * alpha * fadeIn * lit;
    ctx.fillRect((ax0 - cw * A.s * 0.8) / s, ay0 / s, 3 / s, bh / s);
    ctx.globalAlpha = base * alpha * fadeIn;
    for (const i of c.removed) {
      const r = plan.removed[i];
      const [x, y] = posA(r.line, r.col);
      ctx.fillStyle = lit >= 1 ? e.accent : mix(hexOf(pal[r.k] || pal.id), e.accent, lit);
      ctx.fillText(r.text, x / s, (y + baseline * A.s) / s);
    }
    ctx.restore();
    // the helper's name beside the copy, before it leaves
    if (q <= 0) {
      const [nx, ny] = posA(c.l0, c.c1 + 3);
      ctx.save();
      ctx.globalAlpha = base * lit * fadeIn;
      ctx.fillStyle = e.accent;
      ctx.fillText(`→ ${c.helper.name}`, nx / s, (ny + baseline * A.s) / s);
      ctx.restore();
    }
  }
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
    // labels: what it was, and what it is
    const la = envelope(t, x.t0 + 0.3, x.t0 + 3.05, 0.4, 0.35);
    label(ctx, 'FOLDED BY LUAU', 960, 230, e.ink2, { alpha: la * (1 - clamp(pm * 2)), align: 'center' });
    label(ctx, 'SOLVED BACK', 960, 230, e.accent, { alpha: la * clamp(pm * 2 - 1), align: 'center' });
    if (x.comment) drawText(ctx, layoutText('-- ' + x.comment, TY.dark.line), 960, 820, { color: e.accent, align: 'center', alpha: la * clamp(pm * 2.2 - 1.2) });
  }
}

// --------------------------------------------------------------------------------- the numbers

function drawNumbers(ctx, t) {
  const e = ERA.v26, ty = TY.v26;
  const n = S.N.v26a, z = S.N.v26b;
  if (!n) return;
  const t0 = T.nums;
  const out = ease.inOut(clamp((t - (T.orig - 0.6)) / 0.6));
  const side = ease.inOutCubic(clamp((t - t0 - 3.0) / 0.8));
  ctx.save();
  ctx.globalAlpha *= 1 - out;
  ctx.translate(0, -out * 40);
  // rebuilt calls across four real games
  const x1 = lerp(960, 560, side);
  const k1 = lerp(1, 0.74, side);
  ctx.save();
  ctx.translate(x1, 560);
  ctx.scale(k1, k1);
  drawOdometer(ctx, t, { from: n.from ?? 0, to: n.to, start: t0 + 0.2, dur: 2.0, x: 0, y: 0, font: ty.num, align: 'center', color: e.ink, turns: 1 });
  ctx.restore();
  const lab1 = layoutText(n.label, ty.unit, { maxWidth: 600, lineHeight: 1.3 });
  faded(ctx, ease.out(clamp((t - t0 - 0.4) / 0.6)), () => {
    drawText(ctx, lab1, x1, 640, { color: e.ink2, align: 'center' });
    if (n.paren) drawText(ctx, layoutText(n.paren, ty.unit), x1, 640 + lab1.height + 34, { color: e.ink2, align: 'center' });
    label(ctx, `${S.ST['v2.5.1'].name.toUpperCase()}: ${formatNumber(n.from ?? 0)}`, x1, 380, e.ink3, { align: 'center' });
  });
  // fresh fuzzing: zero wrong
  if (z && side > 0) {
    const x2 = 1360;
    const q = ease.out(clamp((t - t0 - 3.4) / 0.6));
    faded(ctx, q, () => {
      ctx.save();
      ctx.translate(x2, 560);
      ctx.scale(0.74, 0.74);
      drawCounter(ctx, z.to, 0, 0, ty.num, { align: 'center', color: e.accent });
      ctx.restore();
      const lab2 = layoutText(z.label, ty.unit, { maxWidth: 520, lineHeight: 1.3 });
      drawText(ctx, lab2, x2, 640, { color: e.ink2, align: 'center' });
      if (z.paren) drawText(ctx, layoutText(z.paren, ty.unit), x2, 640 + lab2.height + 34, { color: e.ink2, align: 'center' });
    });
    vline(ctx, 960, 400, 360, e.ink, 0.12 * q);
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
    } else drawHoming(ctx, t);
    drawCorner(ctx, t);
    drawHomingStats(ctx, t);
  }
  if (t >= T.consts0 - 0.3 && t < T.nums + 0.3) drawConstants(ctx, t);
  if (t >= T.nums - 0.05) drawNumbers(ctx, t);
}
