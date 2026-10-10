// Act III. The release pages, each a different kind of shot.
//
//   V2      a hard cut to white where "V2" fills the frame; it settles into a centred masthead; the
//           page scrolls and the code rises as a centred column, the dispatcher dissolving into
//           continue and break; the page scrolls again into one number filling the frame
//   V2.1    the page turns over to its name, huge; the name drops into the second of two bars that
//           race the full width of the frame
//   V2.1.1  the bars break up into a full-width field of 1,500 damaged scripts
//   V2.5    deep blue tiles itself out of the field; the name fills the frame and sinks into a window
//           in a field of 198 marks that go out one by one with the count; a centred code landscape
//           pushes into one new name
//   V2.5.1  the camera pulls back through that name into V2.5.1's file and pans up it to the helpers,
//           with the release's number on a type layer that moves less than the code (parallax)

import {
  ease, clamp, lerp, envelope, smoothstep,
  layoutText, drawText, reveal, measure, metrics, setFont, drawCounter, counterWidth, drawOdometer, formatNumber,
  layoutCode, codeMorph, drawMorph, codeFont, rgba, mix, hash01, pixel, roundRect,
} from '../../engine/index.js';
import {
  T, ERA, TY, TRACK, HPAIRS, CODE, COL, S, surface, hairline, vline, faded, clipRect, rollText,
  camFit, lerpCam, zoomCam, camTrack, drift, driftOffset, codeAt, edgeFade, recovered, settle, washIn, flowOf, drawFlow,
  drawLedger, label, usDate,
} from './base.js';

let P = null;

const FULL = { x: 0, y: 0, w: 1920, h: 1080 };

/** A camera that shows lines [first, last] of L as a column centred in the frame. */
function camCentre(L, first, last, { top = 112, h = 856, maxS = 1 } = {}) {
  const c = camFit(first, last, { x: 0, y: top, w: 1920, h }, maxS);
  c.x = 960 - (L.colsIn(first, last) * S.CM.cw * c.s) / 2;
  return c;
}
/** A camera at scale s with grid cell (line, col) at screen point (x, y). */
const camOn = (line, col, s, x = 960, y = 540) => ({ s, x: x - col * S.CM.cw * s, y: y - (line + 0.5) * S.CM.lh * s });

// ------------------------------------------------------------------------------------- prepare

export function preparePages(ledgerEvents) {
  const ST = S.ST;
  const L = (tag) => layoutCode(ST[tag].output);
  const shieldTop = (tag) => ST[tag].excerpts.shield.first_line - 1;
  const forLine = (tag) => L(tag).lineOf(/^\s*for _, /, shieldTop(tag));
  const whileLine = (tag) => L(tag).lineOf(/^\s*while os\.clock/, shieldTop(tag));
  const plan = (a, b) => codeMorph(ST[a].output, ST[b].output);
  const L08 = L('v0.8');
  const for08 = forLine('v0.8');
  const inner08 = flowOf(L08).loops.find((lp) => lp.inner && lp.from > for08);
  const after08 = inner08 ? inner08.to + 2 : L08.lineOf(/^\s*local \w+ = os\.clock\(\)/, for08) + 1;
  const pV2 = plan('v0.9.0-beta', 'v2-v0.1');
  const LV2 = pV2.b;
  const fV2 = forLine('v2-v0.1'), wV2 = whileLine('v2-v0.1');
  const rows = Math.floor(856 / S.CM.lh);

  // the 1,500 damaged scripts: which ones V2 aborted on (a fixed scatter, as many as the notes say)
  const aborted = new Set();
  const n = S.N.v211;
  if (n) for (let i = 0; aborted.size < n.aborted && i < n.scripts * 4; i++) aborted.add(Math.floor(hash01(211, i) * n.scripts));
  // the fuzz failures go out in a fixed shuffled order
  const fz = S.N.v25;
  const order = [];
  if (fz) {
    const idx = Array.from({ length: fz.from }, (_, i) => i);
    for (let i = idx.length - 1; i > 0; i--) { const j = Math.floor(hash01(25, i) * (i + 1)); [idx[i], idx[j]] = [idx[j], idx[i]]; }
    idx.forEach((cell, rank) => { order[cell] = rank; });
  }
  // V2.5's names from context: the second function, V2.1.1 against V2.5
  const pJ = codeMorph(ST['v2.1.1'].excerpts.join.text, ST['v2.5'].excerpts.join.text);
  const ctxLine = pJ.b.lineOf(/^\s*\w+ = player\.Chatted/);
  const ctxTok = [...pJ.inserted].find((i) => pJ.b.tokens[i].line === ctxLine && pJ.b.tokens[i].k === 'id');
  const tk = ctxTok != null ? pJ.b.tokens[ctxTok] : null;
  // V2.5.1's file: the same name, so the camera can pull back through it into the whole file
  const L251 = L('v2.5.1');
  let line251 = tk ? L251.lines.findIndex((s) => s.trim() === pJ.b.lines[tk.line].trim()) : -1;
  const tk251 = line251 >= 0 ? L251.tokens.find((x) => x.line === line251 && x.text === tk.text) : null;
  if (!tk251) line251 = -1;
  const col251 = tk251 ? tk251.col : 0;
  const join251 = [ST['v2.5.1'].excerpts.join.first_line - 1, ST['v2.5.1'].excerpts.join.last_line - 1];
  const helpers251 = Math.max(0, L251.lineOf(/^local function /) - 1);
  const shield251 = ST['v2.5.1'].excerpts.shield.first_line - 1;

  P = {
    pV2, aborted, order, pJ, ctxLine, ctxTok, tk, rJ: recovered(pJ, 'all'), rV2: recovered(pV2, 'all'),
    // V2's centred column: 0.8's dispatcher, then V2's for loop, then its while loop
    camV2From: camCentre(pV2.a, for08 - 1, after08),
    camV2For: camCentre(LV2, fV2 - 2, fV2 - 2 + rows - 1),
    camV2While: camCentre(LV2, wV2 - 6, wV2 - 6 + rows - 1),
    L251, line251, col251, join251, helpers251, shield251,
    ledger: ledgerEvents,
  };
  // V2.5.1's cameras: the name close up, the second function, then the helpers the flood starts from
  const { cw } = S.CM;
  const sZ = tk ? Math.min(4.6, 1500 / (tk.text.length * cw)) : 1;
  P.sZ = sZ;
  P.c251Name = line251 >= 0 ? camOn(line251, col251 + tk.text.length / 2, sZ) : null;
  P.c251Join = camCentre(L251, join251[0], join251[1], { top: 90, h: 900, maxS: 1.05 });
  P.c251Top = camCentre(L251, helpers251, Math.min(L251.lineCount - 1, shield251 + 22), { top: 70, h: 940, maxS: 1.15 });
}

export const pmeta = () => P;

// --------------------------------------------------------------------------------------------- V2

export function drawV2(ctx, t) {
  const e = ERA.v2, ty = TY.v2, s = S.ST['v2-v0.1'];
  surface(ctx, e.surface);
  // the page scrolls twice: masthead up and the code in; then the code up and the number in
  const scroll = ease.inOutCubic(clamp((t - T.v2scroll) / 0.85));
  const scroll2 = ease.inOutCubic(clamp((t - T.v2num) / 0.85));
  const H = 1080;

  // 1. the giant V2, then it settles into a centred masthead
  if (scroll < 1) {
    ctx.save();
    ctx.translate(0, -scroll * H);
    const G = ty.giant;
    const Lg = layoutText(s.name, G);
    const capG = metrics(G).capHeight || G.size * 0.72;
    const settleP = ease.inOutCubic(clamp((t - T.v2settle) / 0.95));
    const push = 1 + 0.025 * ease.out(clamp((t - T.v2) / (T.v2settle - T.v2)));
    const k = lerp(push, ty.title.size / G.size, settleP);
    const wG = Lg.width + TRACK.title * G.size * (s.name.length - 1);
    ctx.save();
    ctx.translate(960, lerp(540 + (capG * push) / 2, 470, settleP));
    ctx.scale(k, k);
    drawText(ctx, Lg, -wG / 2, 0, { color: e.ink, tracking: TRACK.title });
    ctx.restore();
    const st = T.v2settle + 0.55;
    const meta = `${usDate(s.published_at || s.date)}  ·  Release ${S.RIDX.get(s)} of ${S.D.totals.releases}`;
    reveal(ctx, layoutText(meta, ty.meta), 960, 196, t, { unit: 'line', start: st, dur: 0.8, color: e.ink2, align: 'center' });
    reveal(ctx, layoutText('Tovek', ty.tovek), 960, 262, t, { unit: 'line', start: st, dur: 0.8, color: e.ink, tracking: -0.01, align: 'center' });
    reveal(ctx, layoutText(s.headline, ty.head), 960, 590, t, { unit: 'word', start: st + 0.3, stagger: 0.06, dur: 0.8, color: e.ink2, align: 'center' });
    ctx.restore();
  }

  // 2. the code as a centred column: 0.9's dispatcher dissolves into continue and break
  const pal = S.PAL.v2;
  const dy = (1 - scroll) * H - scroll2 * H;
  if (scroll > 0 && scroll2 < 1) {
    const off = (c) => ({ s: c.s, x: c.x, y: c.y + dy });
    const p = clamp((t - T.v2Morph0) / (T.v2Morph1 - T.v2Morph0));
    if (p <= 0) codeAt(ctx, P.pV2.a, off(P.camV2From), pal, { clip: FULL });
    else if (p < 1) drawMorph(ctx, P.pV2, p, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, cameraA: off(P.camV2From), cameraB: off(P.camV2For), blur: 5, highlight: P.rV2 });
    else {
      const cam = off(drift(camTrack([[T.v2Morph1 + 0.2, P.camV2For], [T.v2num - 0.1, P.camV2While]])(t), t, T.v2Morph1, T.v2num + 1, { zoom: 0.03, dx: 0, dy: -8 }));
      codeAt(ctx, P.pV2.b, cam, pal, { clip: FULL, highlight: P.rV2, mix: settle(T.v2Morph1, 1.6)(t), washAlpha: washIn(T.v2Morph1, 1.6)(t), washColor: e.marker });
      const fa = envelope(t, T.v2Morph1 - 0.01, T.v2num + 0.6, 0.5, 0.4);
      if (fa > 0) drawFlow(ctx, P.pV2.b, cam, t, e.ink2, { gotoA: 0, loopA: fa });
    }
    edgeFade(ctx, FULL, e.surface, 90);
  }
  drawLedger(ctx, t, P.ledger, 'v2', COL, 828, ease.out(clamp((t - T.v2Morph0) / 0.8)) * (1 - scroll2));

  // 3. one number fills the frame: anonymous bindings across the corpus
  const n = S.N.v2;
  if (n && scroll2 > 0) {
    ctx.save();
    ctx.translate(0, (1 - scroll2) * H);
    drawOdometer(ctx, t, { from: n.from, to: n.to, start: T.v2num + 0.45, dur: 1.9, x: 960, y: 640, font: ty.ecu, color: e.ink, align: 'center', turns: 1 });
    drawText(ctx, layoutText(`${n.label}, down from ${formatNumber(n.from)}`, ty.ecuLab), 960, 746, { color: e.ink2, align: 'center' });
    ctx.restore();
  }
}

// ------------------------------------------------------------------------------------------- V2.1

/** The page turn: the next page slides in from the right with a hard edge on whole device pixels. */
function turnEdge(ctx, t, t0, dur = 1.15) {
  const p = ease.inOutCubic(clamp((t - t0) / dur));
  const px = pixel(ctx);
  return Math.round(lerp(1922, -2, p) / px) * px;
}

export function drawV21(ctx, t) {
  const e = ERA.v21;
  const edge = turnEdge(ctx, t, T.v21);
  clipRect(ctx, { x: edge, y: 0, w: 1920 - edge + 4, h: 1080 }, () => {
    surface(ctx, e.surface);
    drawV21Page(ctx, t);
  });
  if (edge > 0) vline(ctx, edge, 0, 1080, e.ink, 0.22);
}

// the race: two bars the full width of the frame
const BARS = { y: [452, 628], h: 132, labelX: 120 };
// the field of damaged scripts: a wide band, one cell per script
function fieldGeom(n) {
  const cols = 60, rows = Math.ceil(n / cols);
  const px = 28, py = Math.min(17, 440 / rows);
  return { cols, rows, px, py, x0: 960 - ((cols - 1) * px) / 2, y0: 380 };
}

function drawV21Page(ctx, t) {
  const e = ERA.v21, ty = TY.v21;
  const s1 = S.ST['v2.1'], s2 = S.ST['v2.1.1'];
  const n = S.N.v21, d = S.N.v211;
  const two = t >= T.v211;
  const raceIn = ease.out(clamp((t - T.v21race + 0.3) / 0.6));
  const breakP = ease.inOut(clamp((t - T.v211) / 0.7)); // the bars break up into the field

  // meta: the release notes' kicker, rolling from V2.1 to V2.1.1
  const metaOf = (st) => `${st.name}  ·  RELEASE NOTES  ·  ${usDate(st.published_at || st.date).toUpperCase()}`;
  const ma = ease.out(clamp((t - T.v21 - 0.5) / 0.6));
  if (ma > 0) {
    const a = metaOf(s1), b = metaOf(s2), w = Math.max(a.length, b.length);
    rollText(ctx, a.padEnd(w), (two ? b : a).padEnd(w), COL, 132, TY.v21.meta, t, T.v211, { color: e.accent, alpha: ma, dur: 0.45, stagger: 0.012 });
  }
  // the headline, large, across the top; it turns over for V2.1.1
  const HF = ty.headline;
  reveal(ctx, layoutText(s1.headline, HF, { pairs: HPAIRS }), COL - 4, 320, t, { unit: 'word', start: T.v21race - 0.75, stagger: 0.08, dur: 0.8, color: e.ink, tracking: TRACK.big, out: { start: T.v211 - 0.3, stagger: 0.03, dur: 0.4 } });
  reveal(ctx, layoutText(s2.headline, HF, { pairs: HPAIRS }), COL - 4, 320, t, { unit: 'word', start: T.v211 + 0.25, stagger: 0.07, dur: 0.8, color: e.ink, tracking: TRACK.big });

  // 1. the name, huge, then it drops into its bar
  const drop = ease.inOutCubic(clamp((t - (T.v21race - 0.9)) / 0.85));
  const G = ty.giant, BF = ty.bar;
  const capG = metrics(G).capHeight || G.size * 0.72, capB = metrics(BF).capHeight || BF.size * 0.72;
  const by2 = BARS.y[1] + BARS.h / 2 + capB / 2;
  if (!two) {
    const k = lerp(1, BF.size / G.size, drop);
    const x = lerp(COL - 12, BARS.labelX, drop), y = lerp(540 + capG / 2, by2, drop);
    const LG = layoutText(s1.name, G);
    ctx.save();
    ctx.translate(x, y);
    ctx.scale(k, k);
    if (drop <= 0) reveal(ctx, LG, 0, 0, t, { unit: 'glyph', start: T.v21 + 0.3, stagger: 0.06, dur: 0.9, color: e.ink, tracking: TRACK.title });
    else if (drop < 1) drawText(ctx, LG, 0, 0, { color: e.ink, tracking: lerp(TRACK.title, 0, drop) });
    ctx.restore();
  }

  // 2. the race: V2 against V2.1 on one game, one thread
  if (n && raceIn > 0 && breakP < 1) {
    const t0 = T.v21race + 0.35, race = 2.8;
    const elapsed = clamp((t - t0) / race) * n.v2;
    const rows = [{ label: 'V2', secs: n.v2, fill: e.ink3 }, { label: s1.name, secs: n.v21, fill: e.fill }];
    ctx.save();
    ctx.globalAlpha *= raceIn;
    rows.forEach((r, i) => {
      const y = BARS.y[i];
      const h = BARS.h * (1 - breakP);
      const yc = y + BARS.h / 2;
      const run = Math.min(elapsed, r.secs), done = elapsed >= r.secs;
      const head = (1920 * run) / n.v2;
      ctx.fillStyle = rgba(e.ink, 0.06);
      ctx.fillRect(-2, yc - h / 2, 1924, h);
      ctx.fillStyle = r.fill;
      ctx.fillRect(-2, yc - h / 2, head + 2, h);
      if (breakP > 0) return;
      // the label reads in ink on the track and in the page colour where the bar has filled
      const ly = y + BARS.h / 2 + capB / 2;
      const lw = measure(r.label, BF) + 4;
      // (the second bar's label is the name itself, still dropping into place until `drop` is 1)
      if (i === 0 || drop >= 1) {
        clipRect(ctx, { x: head, y, w: 1920, h: BARS.h }, () => drawText(ctx, layoutText(r.label, BF), BARS.labelX, ly, { color: e.ink }));
        if (head > BARS.labelX) clipRect(ctx, { x: 0, y, w: Math.min(head, BARS.labelX + lw), h: BARS.h }, () => drawText(ctx, layoutText(r.label, BF), BARS.labelX, ly, { color: e.surface }));
      }
      // the timer rides the head of the bar; at the far edge it tucks inside
      const TF = ty.timer;
      const w = counterWidth(run, TF, { decimals: 1, suffix: ' s' });
      const outside = head + 28 + w < 1824;
      const tx = outside ? head + 28 : head - 28 - w;
      const color = outside ? (done && i === 1 ? e.accent : e.ink) : e.surface;
      if (t > t0 - 0.05) drawCounter(ctx, run, tx, yc + (metrics(TF).capHeight || TF.size * 0.7) / 2, TF, { decimals: 1, suffix: ' s', color });
    });
    ctx.restore();
    const lab = `${formatNumber(n.scripts)}-script game, one thread`;
    drawText(ctx, layoutText(lab, ty.note), COL, 872, { color: e.ink2, alpha: raceIn * (1 - breakP) });
  }

  // 3. V2.1.1: the bars break into 1,500 damaged scripts; V2 aborted on some, V2.1.1 on none
  if (d && two) {
    const g = fieldGeom(d.scripts);
    const fixed = ease.inOut(clamp((t - T.v211 - 2.0) / 0.9));
    const barY = BARS.y.map((y) => y + BARS.h / 2);
    ctx.save();
    for (let i = 0; i < d.scripts; i++) {
      const c = i % g.cols, r = Math.floor(i / g.cols);
      const cx = g.x0 + c * g.px, cy = g.y0 + r * g.py;
      // each cell lands from the nearer bar, the cells nearest the bars first
      const near = Math.min(Math.abs(cy - barY[0]), Math.abs(cy - barY[1]));
      const q = ease.out(clamp((t - T.v211 - 0.05 - near / 900 - hash01(7, i) * 0.12) / 0.45));
      if (q <= 0) continue;
      const bad = P.aborted.has(i);
      const from = cy < (barY[0] + barY[1]) / 2 ? barY[0] : barY[1];
      const y = lerp(from, cy, q);
      ctx.globalAlpha = q * (bad ? lerp(1, 0.42, fixed) : 0.42);
      ctx.fillStyle = bad ? mix(e.accent, e.ink3, fixed) : e.ink3;
      const sz = bad ? lerp(7.5, 4.5, fixed) : 4.5;
      ctx.fillRect(cx - sz, y - sz, sz * 2, sz * 2);
    }
    ctx.restore();
    const a = ease.out(clamp((t - T.v211 - 0.4) / 0.6));
    faded(ctx, a, () => {
      const msgA = `V2 aborted ${formatNumber(d.aborted)} times.`, msgB = `${s2.name}: never.`;
      const swap = clamp((t - T.v211 - 2.0) / 0.5);
      const MF = ty.statement;
      const yb = 900;
      if (swap < 1) drawText(ctx, layoutText(msgA, MF), COL - 4, yb - swap * 30, { color: e.ink, alpha: 1 - swap, tracking: TRACK.big });
      if (swap > 0) drawText(ctx, layoutText(msgB, MF), COL - 4, yb + (1 - swap) * 30, { color: e.accent, alpha: swap, tracking: TRACK.big });
      drawText(ctx, layoutText(`${formatNumber(d.scripts)} damaged scripts`, ty.note), 1800, yb - 6, { color: e.ink2, align: 'right' });
    });
  }
}

// ------------------------------------------------------------------------------------------- V2.5

/** The blue page assembles out of the field: square tiles grow from the centre out, then merge. */
function tileFlood(ctx, t, t0, color) {
  const cols = 24, rows = 14, w = 1920 / cols, h = 1080 / rows;
  const px = pixel(ctx);
  const sn = (v) => Math.round(v / px) * px;
  let full = true;
  ctx.fillStyle = color;
  for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) {
    const cx = (c + 0.5) * w, cy = (r + 0.5) * h;
    const d = Math.hypot((cx - 960) / 960, (cy - 540) / 540) / Math.SQRT2;
    const q = ease.inOutCubic(clamp((t - t0 - d * 0.55) / 0.45));
    if (q < 1) full = false;
    if (q <= 0) continue;
    const sw = w * q, sh = h * q;
    const x = sn(cx - sw / 2 - 0.5), y = sn(cy - sh / 2 - 0.5);
    ctx.fillRect(x, y, sn(cx + sw / 2 + 0.5) - x, sn(cy + sh / 2 + 0.5) - y);
  }
  return full;
}

export function drawV25(ctx, t) {
  const e = ERA.v25;
  const full = tileFlood(ctx, t, T.v25, e.surface);
  if (!full && t < T.v25 + 1.1) return;
  if (full) surface(ctx, e.surface);
  drawV25Page(ctx, t);
}

/** When the fuzz count runs, from T.fuzz0 (the score's ticks follow it). */
export const FUZZ = { start: 1.0, dur: 2.5 };

/**
 * The fuzz field: a grid that fills the frame with a window in the middle for the count. The marks
 * are the grid cells outside the window, in reading order; there are exactly as many as failures.
 */
function fuzzGeom(count) {
  const hole = { c: 13, r: 6 };
  const cols = 23;
  const rows = Math.max(12, Math.ceil((count + hole.c * hole.r) / cols));
  const pitch = Math.min(80, 1000 / rows);
  const x0 = 960 - (cols * pitch) / 2, y0 = 540 - (rows * pitch) / 2;
  const hc0 = Math.floor((cols - hole.c) / 2), hr0 = Math.floor((rows - hole.r) / 2);
  const cells = [];
  for (let r = 0; r < rows && cells.length < count; r++) for (let c = 0; c < cols && cells.length < count; c++) {
    if (c >= hc0 && c < hc0 + hole.c && r >= hr0 && r < hr0 + hole.r) continue;
    // distance from the window, in cells: the field ripples out from it
    const dc = c < hc0 ? hc0 - c : c >= hc0 + hole.c ? c - (hc0 + hole.c - 1) : 0;
    const dr = r < hr0 ? hr0 - r : r >= hr0 + hole.r ? r - (hr0 + hole.r - 1) : 0;
    cells.push({ x: x0 + (c + 0.5) * pitch, y: y0 + (r + 0.5) * pitch, d: Math.max(dc, dr) });
  }
  const win = { x: x0 + hc0 * pitch, y: y0 + hr0 * pitch, w: hole.c * pitch, h: hole.r * pitch };
  return { cells, pitch, win };
}

function drawV25Page(ctx, t) {
  const e = ERA.v25, ty = TY.v25;
  const s1 = S.ST['v2.5'], s2 = S.ST['v2.5.1'];
  const fz = S.N.v25;

  // A. the name fills the frame, then sinks into the window of the field
  const head0 = T.v25 + 0.6;
  const sink = ease.inOutCubic(clamp((t - T.fuzz0 + 0.1) / 0.8));
  if (sink < 1 && t < T.fuzz0 + 1) {
    const G = ty.giant, capG = metrics(G).capHeight || G.size * 0.72;
    ctx.save();
    ctx.globalAlpha *= 1 - sink;
    ctx.translate(960, 540);
    ctx.scale(lerp(1, 0.28, sink), lerp(1, 0.28, sink));
    ctx.translate(-960, -540);
    label(ctx, usDate(s1.published_at || s1.date), 960, 300, e.ink2, { tracking: 0, font: ty.meta, align: 'center', alpha: ease.out(clamp((t - head0) / 0.6)) });
    reveal(ctx, layoutText(s1.name, G), 960, 540 + capG / 2, t, { unit: 'glyph', start: head0 + 0.05, stagger: 0.06, dur: 0.9, color: e.ink, tracking: TRACK.title, align: 'center' });
    reveal(ctx, layoutText(s1.headline, ty.head), 960, 800, t, { unit: 'word', start: head0 + 0.5, stagger: 0.07, dur: 0.8, color: e.ink2, align: 'center' });
    ctx.restore();
  }

  // B. semantic fuzzing: a field of failures going out one by one with the count in its window
  if (fz && t > T.fuzz0 - 0.3 && t < T.ctx25 + 0.8) {
    const g = fuzzGeom(fz.from);
    const leave = ease.inOut(clamp((t - T.ctx25 + 0.15) / 0.7));
    const start = T.fuzz0 + FUZZ.start, dur = FUZZ.dur, fn = ease.inOut;
    const current = lerp(fz.from, fz.to, fn(clamp((t - start) / dur)));
    const outCount = fz.from - current;
    const size = g.pitch * 0.58, r0 = g.pitch * 0.1;
    ctx.save();
    for (let i = 0; i < g.cells.length; i++) {
      const c = g.cells[i];
      const appear = ease.out(clamp((t - T.fuzz0 - 0.2 - c.d * 0.05 - hash01(26, i) * 0.08) / 0.45));
      if (appear <= 0) continue;
      // a mark goes out when the count passes its rank: it flashes, shrinks and dims
      const rank = P.order[i];
      const goneP = clamp((outCount - rank) / Math.min(5, fz.from - rank)); // the last is out as the count lands
      const k = lerp(1, 0.26, ease.out(goneP)) * lerp(0.4, 1, appear) * (1 - 0.6 * leave);
      const sz = size * k;
      ctx.globalAlpha = appear * (1 - leave * 0.9) * lerp(1, 0.55, goneP);
      ctx.fillStyle = goneP <= 0 ? e.accent : goneP < 0.35 ? mix(e.accent, '#ffffff', 1 - goneP / 0.35) : mix(e.accent, e.faint, (goneP - 0.35) / 0.65);
      roundRect(ctx, c.x - sz / 2, c.y - sz / 2 + leave * 30, sz, sz, r0 * k);
    }
    ctx.restore();
    // the window: the count, exact at every moment, and what it counts
    const wa = ease.out(clamp((t - T.fuzz0 - 0.35) / 0.6)) * (1 - ease.out(clamp((t - T.ctx25 + 0.45) / 0.35)));
    faded(ctx, wa, () => {
      const wy = g.win.y, wh = g.win.h;
      label(ctx, `SEMANTIC FUZZING  ·  ${fz.fromLabel} → ${s1.name}`, 960, wy + 54, e.ink2, { align: 'center', tracking: 0.12 });
      const CF = ty.count, capC = metrics(CF).capHeight || CF.size * 0.72;
      const landed = clamp((t - start - dur) / 0.5);
      drawOdometer(ctx, t, { from: fz.from, to: fz.to, start, dur, x: 960, y: wy + wh / 2 + capC / 2 + 4, font: CF, align: 'center', color: landed > 0 ? mix(e.ink, e.accent, landed) : e.ink, ease: fn, exact: true });
      drawText(ctx, layoutText('wrong or missing outputs', ty.countLab), 960, wy + wh - 36, { color: e.ink2, align: 'center' });
    });
  }

  // C. names from context: the second function as a centred landscape, then a push into the name
  const pal = S.PAL.v25;
  const { cw, lh } = S.CM;
  if (t > T.ctx25 - 0.2 && t < T.v251 + 0.05) {
    const pJ = P.pJ, tk = P.tk;
    const a = ease.out(clamp((t - T.ctx25 + 0.1) / 0.4));
    // the count's window opens out to the whole frame, and the code is inside it
    const open = fz ? ease.inOutCubic(clamp((t - (T.ctx25 - 0.15)) / 0.85)) : 1;
    const win = fz ? fuzzGeom(fz.from).win : FULL;
    const R = { x: lerp(win.x, -2, open), y: lerp(win.y, -2, open), w: lerp(win.w, 1924, open), h: lerp(win.h, 1084, open) };
    const s0 = Math.min(1.25, 1640 / (pJ.b.cols * cw), 880 / (pJ.b.lineCount * lh));
    const wide = { s: s0, x: 960 - (pJ.b.cols * cw * s0) / 2, y: 548 - (pJ.b.lineCount * lh * s0) / 2 };
    const close = tk ? camOn(tk.line, tk.col + tk.text.length / 2, P.sZ) : wide;
    const pm = clamp((t - T.ctx25 - 0.15) / 1.5);
    const zoom = ease.inOutCubic(clamp((t - T.ctx25 - 1.9) / 1.3));
    clipRect(ctx, R, () => {
      if (open < 1) { ctx.fillStyle = e.surface; ctx.fillRect(R.x, R.y, R.w, R.h); }
      faded(ctx, a, () => {
        if (pm < 1) drawMorph(ctx, pJ, pm, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, cameraA: wide, cameraB: wide, blur: 5, highlight: P.rJ });
        else codeAt(ctx, pJ.b, zoomCam(wide, close, zoom), pal, { clip: FULL, highlight: P.rJ, mix: 1, lineAlpha: (l) => (tk && l === tk.line ? 1 : 1 - 0.9 * zoom) });
      });
    });
    if (open > 0 && open < 1) {
      ctx.save();
      ctx.strokeStyle = e.accent;
      ctx.globalAlpha *= 0.6 * (1 - open);
      ctx.lineWidth = 1.5;
      ctx.strokeRect(R.x, R.y, R.w, R.h);
      ctx.restore();
    }
  }

  // D. V2.5.1: back out through the name into V2.5.1's whole file, up to its helpers
  if (t >= T.v251) {
    const tk = P.tk;
    const back = ease.inOutCubic(clamp((t - T.v251) / 1.5));
    const lineA = (l) => (l === P.line251 ? 1 : lerp(0.1, 1, back));
    codeAt(ctx, P.L251, cam251(t), pal, { clip: FULL, highlight: null, lineAlpha: P.line251 >= 0 ? lineA : undefined });
    edgeFade(ctx, FULL, e.surface, 80);
    // the name stays lit in the accent until the camera is wide
    if (tk && P.line251 >= 0 && back < 1) {
      const cam = cam251(t);
      ctx.save();
      ctx.globalAlpha *= 1 - back;
      ctx.translate(cam.x, cam.y);
      ctx.scale(cam.s, cam.s);
      setFont(ctx, codeFont(CODE.size));
      ctx.textBaseline = 'alphabetic';
      ctx.fillStyle = e.surface;
      const x = P.col251 * cw, y = P.line251 * lh;
      ctx.fillRect(x - 2, y + 2, tk.text.length * cw + 4, lh - 4);
      ctx.fillStyle = e.accent;
      ctx.fillText(tk.text, x, y + S.CM.baseline);
      ctx.restore();
    }
    drawV251Type(ctx, t);
  }
}

/** V2.5.1's type layer: a scrim and the release's number, moving less than the code behind it. */
function drawV251Type(ctx, t) {
  const e = ERA.v25, ty = TY.v25, s2 = S.ST['v2.5.1'];
  const c = S.N.v251;
  const a = ease.out(clamp((t - T.v251 - 0.9) / 0.7));
  if (a <= 0) return;
  const [ox, oy] = driftOffset(t, T.v251, T.flood + 1.2, { dx: -10, dy: -6, depth: 0.35 });
  ctx.save();
  ctx.globalAlpha *= a;
  // the scrim: the page colour rising from the bottom, a band for the type to sit on
  const g = ctx.createLinearGradient(0, 1080, 0, 520);
  g.addColorStop(0, rgba(e.surface, 0.97));
  g.addColorStop(0.62, rgba(e.surface, 0.9));
  g.addColorStop(1, rgba(e.surface, 0));
  ctx.fillStyle = g;
  ctx.fillRect(0, 520, 1920, 560);
  ctx.translate(ox, oy);
  label(ctx, usDate(s2.published_at || s2.date), COL, 636, e.ink2, { tracking: 0, font: ty.meta });
  reveal(ctx, layoutText(s2.name, ty.name), COL - 4, 740, t, { unit: 'glyph', start: T.v251 + 0.9, stagger: 0.05, dur: 0.8, color: e.ink, tracking: TRACK.title });
  const nameW = measure(s2.name, ty.name) + 34;
  reveal(ctx, layoutText(s2.headline, ty.head), COL + nameW, 726, t, { unit: 'word', start: T.v251 + 1.2, stagger: 0.06, dur: 0.8, color: e.ink2 });
  if (c) {
    drawOdometer(ctx, t, { from: c.from ?? c.to, to: c.to, start: T.v251 + 1.4, dur: 1.8, x: COL - 6, y: 932, font: ty.calls, color: e.ink, turns: 1 });
    const w = counterWidth(c.to, ty.calls);
    drawText(ctx, layoutText(c.label, ty.callsLab, { maxWidth: 1824 - COL - w - 40, lineHeight: 1.25 }), COL - 6 + w + 34, 924, { color: e.ink2 });
  }
  ctx.restore();
}

/** V2.5.1's camera: close on the name, back to the second function, then up the file to the helpers. */
export function cam251(t) {
  if (!P.c251Name) return drift(P.c251Top, t, T.v251, T.flood + 1.2, { zoom: 0.03, dx: -10, dy: -6, cx: 960 });
  const t0 = T.v251;
  if (t < t0 + 1.5) {
    const A = P.c251Name, B = P.c251Join;
    const anchor = { x: (P.col251 + P.tk.text.length / 2) * S.CM.cw, y: (P.line251 + 0.5) * S.CM.lh };
    return zoomCam(A, B, ease.inOutCubic(clamp((t - t0) / 1.5)), anchor);
  }
  if (t < t0 + 2.9) return lerpCam(P.c251Join, P.c251Top, ease.inOutCubic(clamp((t - t0 - 1.5) / 1.4)));
  return drift(P.c251Top, t, t0 + 2.9, T.flood + 1.2, { zoom: 0.03, dx: -10, dy: -6, cx: 960 });
}
