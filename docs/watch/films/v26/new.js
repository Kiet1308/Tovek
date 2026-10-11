// V. New in V2.6. The read head cuts the night open along the middle.
// VI. Helpers, rebuilt. Roblox's own ClickToMoveController: Luau -O2 pasted its DisconnectEvent helper
//     into DisconnectEvents eleven times. One copy, close up: the -g1 line of each of its lines points
//     at the helper's own lines. Then the camera pulls back over all eleven, and they fold into calls.
// VII. Constants, solved. task.wait(0.016666666666666666): its 64 bits, and 1 / 60 folded the way Luau
//      folds it, bit for bit. Then frames(1), and 2 / 3.

import {
  T, C, TY, S, f, fmt, surface, kicker, say, note, leader, numberCard, cameraTrack, frameAt, applyCamera, toScreen, TRACK, AROUND, headH, drift, mixQ,
} from './base.js';
import {
  ease, clamp, lerp, progress, envelope, layoutText, drawText, reveal, layoutCode, codeMetrics, drawCode,
  codeMorph, drawMorph, rgba, setFont, roundRect, mix,
} from '../../engine/index.js';

// ------------------------------------------------------------------------------------------ V2.6

/** The sting: V2.6 on the night; at T.split the head opens it along the middle onto `under`. */
export function drawSting(ctx, t, under) {
  const sp = progress(t, T.split, 1.0, ease.inOutCubic);
  if (sp > 0 && under) under(ctx, t);
  const gap = sp * 600;
  const half = (dir) => {
    ctx.save();
    ctx.translate(0, dir * gap);
    ctx.beginPath();
    if (dir < 0) ctx.rect(-10, -10, 1940, 550); else ctx.rect(-10, 540, 1940, 550);
    ctx.clip();
    surface(ctx, C.night);
    ctx.save();
    drift(ctx, t, T.sting, T.split + 1, 0.035, 960, 600);
    const L = layoutText('V2.6', TY.giant, { around: AROUND });
    reveal(ctx, L, 960, 790, t, { unit: 'glyph', start: T.sting + 0.35, stagger: 0.075, dur: 1.05, align: 'center', color: C.onNight, mask: true, rise: 1.0, tracking: TRACK.giant, trackingFrom: 0.04 });
    say(ctx, 'New in', 960, 262, t, T.sting + 0.15, { font: f(48, 'text', 500), color: C.onNight2, align: 'center', tracking: 0 });
    ctx.restore();
    ctx.restore();
  };
  half(-1);
  half(1);
  // the head lands on the middle just before it opens, then rides both edges out
  const ha = envelope(t, T.split - 0.4, T.split + 1.0, 0.25, 0.5);
  if (ha > 0) {
    if (sp <= 0) headH(ctx, 540, ha, { glow: 0.1, side: 1 });
    else {
      headH(ctx, 540 - gap, ha, { glow: 0.12, side: 1 });
      headH(ctx, 540 + gap, ha, { glow: 0.12, side: -1 });
    }
  }
}

// --------------------------------------------------------------------------------- VI. helpers

let HP = null;
const BASE = { size: 22, lineHeight: 1.5 };
const MACRO = { s: 2.0, x: 300, y: 418 };     // the close-up: copy 1's code at 44 px, its top-left at (x, y)
const SRC = { size: 27, lineHeight: 1.5, x: 1226, numX: 1196 };
const FOLD = { s: 1.0, head: 512, left: 236, dur: 0.46 };   // the fold: function scale, the head's y, x of col 0

/** When each pasted copy starts to fold: slow at first, faster into the last one, which lands at T.fold1. */
export function foldTimes(n) {
  return Array.from({ length: n }, (_, k) => T.fold0 + (T.fold1 - T.fold0 - FOLD.dur) * Math.pow(k / Math.max(1, n - 1), 1.3));
}
export const FOLD_DUR = FOLD.dur;

export function prepareHelpers() {
  const h = S.D.helper;
  const pasted = h.function.pasted.replace(/\r/g, '').split('\n');
  const rebuilt = h.function.rebuilt.replace(/\r/g, '').split('\n');
  // the pasted function in chunks: the header, one chunk per copy (from its `local` to the next), the end
  const starts = [];
  pasted.forEach((l, i) => { if (/^\tlocal \w+ = self\.\w+$/.test(l)) starts.push(i); });
  const chunks = starts.map((s, k) => {
    const e = k + 1 < starts.length ? starts[k + 1] : pasted.length - 1;
    return { lines: pasted.slice(s, e), call: rebuilt[k + 1] };
  });
  if (chunks.length !== rebuilt.length - 2) throw new Error('helpers: copies and calls do not pair up');
  // each chunk's own instructions with their -g1 lines (in program order, one GETTABLEKS starts a copy)
  const perCopy = [];
  for (const i of h.instructions) {
    if (i.op === 'GETTABLEKS' && i.from === 'caller') perCopy.push([]);
    if (perCopy.length) perCopy[perCopy.length - 1].push(i);
  }
  // the -g1 line of each line of a copy: local (the caller's line), if, the call
  const evidence = perCopy.map((ins) => ({
    0: [...new Set(ins.filter((i) => i.from === 'caller').map((i) => i.line))],
    2: [...new Set(ins.filter((i) => i.op === 'JUMPIFNOT').map((i) => i.line))],
    3: [...new Set(ins.filter((i) => i.from === 'helper' && i.op !== 'JUMPIFNOT').map((i) => i.line))],
  }));
  const M = codeMetrics(BASE.size, BASE.lineHeight);
  HP = {
    header: pasted[0], footer: pasted[pasted.length - 1], chunks, evidence, M,
    maxCols: Math.max(...pasted.map((l) => l.replace(/\t/g, '    ').length)),
    callCols: Math.max(...rebuilt.map((l) => l.replace(/\t/g, '    ').length)),
    foldAt: foldTimes(chunks.length),
    helperLines: h.helper_lines.source_lines, helperCode: new Set(h.helper_lines.code_lines),
  };
}

const foldP = (t, k) => ease.inOutCubic(clamp((t - HP.foldAt[k]) / FOLD.dur));

/**
 * The fold front in function space: the boundary between the calls already made and the copies still
 * pasted. Each copy adds one line (its call) as it folds, so the front is the header plus every
 * copy's progress. The camera keeps the front on the read head: the function rises through it.
 */
function front(t) {
  let n = 1;
  for (let k = 0; k < HP.chunks.length; k++) n += foldP(t, k);
  return n * HP.M.lh;
}

/** Function space → screen: the close-up, the pull back onto the head, the folds, the rebuilt function, the step aside. */
function helperCam(t) {
  const { cw, lh } = HP.M;
  // close-up: copy 1's first line (function row 1, col 4) at MACRO.(x, y)
  const close = frameAt(4 * cw, 1 * lh, MACRO.x, MACRO.y, MACRO.s);
  const fold = frameAt(0, front(t), FOLD.left, FOLD.head, FOLD.s);
  const n = HP.chunks.length + 2;
  const sEnd = Math.min(1.32, 1560 / (HP.callCols * cw));
  const done = frameAt(0, (n * lh) / 2, 196, 560, sEnd);
  const right = frameAt(0, (n * lh) / 2, 940, 560, 0.76);
  let cam = close;
  const p = progress(t, T.hpull, 1.0, ease.inOutCubic);
  if (p > 0) cam = blendCam(close, fold, p);
  const q = progress(t, T.fold1 + FOLD.dur * 0.2, 0.9, ease.inOutCubic);
  if (q > 0) cam = blendCam(cam, done, q);
  const r = progress(t, T.helpersNum - 0.15, 1.0, ease.inOutCubic);
  if (r > 0) cam = blendCam(cam, right, r);
  return cam;
}

/** Blend two cameras the way cameraTrack does (log scale, focus kept steady through the zoom). */
function blendCam(a, b, p) {
  const s = Math.exp(lerp(Math.log(a.s), Math.log(b.s), p));
  const k = Math.abs(b.s - a.s) > 1e-6 ? (1 / s - 1 / a.s) / (1 / b.s - 1 / a.s) : p;
  return { x: lerp(a.x, b.x, k), y: lerp(a.y, b.y, k), s };
}

/** The read head of the fold: it draws in across the frame after the pull back and leaves after the last fold. */
const foldHeadAlpha = (t) => envelope(t, T.hpull + 0.75, T.fold1 + FOLD.dur + 0.5, 0.3, 0.45);
const foldHeadP = (t) => progress(t, T.hpull + 0.7, 0.45, ease.inOutCubic);

export function drawHelpers(ctx, t) {
  surface(ctx, C.paper);
  const h = S.D.helper;
  const { lh } = HP.M;
  const cam = helperCam(t);
  const L = (text) => layoutCode(text);
  const lit = (k) => 1 - progress(t, HP.foldAt[k] + FOLD.dur + 0.6, 1.4);   // a fresh call glows, then settles

  // the function, copy by copy, in function space
  ctx.save();
  applyCamera(ctx, cam);
  ctx.globalAlpha *= progress(t, T.helpers + 0.35, 0.6, ease.out) * (1 - progress(t, T.consts - 0.45, 0.4, ease.inOut));
  // the context around copy 1 stays back during the close-up
  const ctxA = progress(t, T.hpull + 0.1, 0.7);
  let y = 0;
  ctx.save();
  ctx.globalAlpha *= ctxA;
  drawCode(ctx, L(HP.header), { x: 0, y, size: BASE.size, lineHeight: BASE.lineHeight, palette: 'paper' });
  ctx.restore();
  y += lh;
  const marks = [];
  HP.chunks.forEach((c, k) => {
    const p = foldP(t, k);
    const rows = lerp(c.lines.length, 1, p);
    const hgt = rows * lh;
    const a = k === 0 ? 1 : ctxA;
    if (p < 1) {
      // the pasted copy is pressed into its call's line as it crosses the head
      ctx.save();
      ctx.globalAlpha *= a * (1 - ease.inQuad(p));
      ctx.translate(0, y);
      ctx.scale(1, lerp(1, 1 / c.lines.length, p));
      drawCode(ctx, L(c.lines.join('\n')), { x: 0, y: 0, size: BASE.size, lineHeight: BASE.lineHeight, palette: 'paper' });
      ctx.restore();
      marks.push({ k, y, p, c, a });
    }
    if (p > 0) {
      ctx.save();
      ctx.globalAlpha *= ease.outQuad(p);
      const callEnd = c.call.indexOf(' --');
      const isCall = (tk) => tk.offset < (callEnd < 0 ? c.call.length : callEnd);
      drawCode(ctx, L(c.call), { x: 0, y, size: BASE.size, lineHeight: BASE.lineHeight, palette: 'paper', highlight: isCall, highlightMix: lit(k) });
      ctx.restore();
    }
    y += hgt;
  });
  ctx.save();
  ctx.globalAlpha *= ctxA;
  drawCode(ctx, L(HP.footer), { x: 0, y, size: BASE.size, lineHeight: BASE.lineHeight, palette: 'paper' });
  ctx.restore();
  ctx.restore();

  drawGutters(ctx, t, cam, marks);
  drawHelperSource(ctx, t, cam);
  // the head the copies cross
  const ha = foldHeadAlpha(t);
  if (ha > 0) {
    const hp = foldHeadP(t);
    ctx.save();
    ctx.beginPath();
    ctx.rect(-4, -4, 1928 * hp, 1088);
    ctx.clip();
    headH(ctx, FOLD.head, ha, { side: -1, glow: 0.12 });
    ctx.restore();
  }

  // the title, and whose code this is
  const tOut = T.hpull + 0.05;
  say(ctx, 'Helpers, rebuilt.', 140, 196, t, T.helpers + 0.3, { font: TY.title, color: C.ink, out: tOut, tracking: TRACK.title });
  const ka = envelope(t, T.helpers + 0.65, tOut + 0.45, 0.4, 0.4);
  if (ka > 0) kicker(ctx, `Roblox's own ${h.script}`, 144, 258, { color: C.ink2, alpha: ka });

  // the tally while they fold
  // the tally rides on the head
  const ta = envelope(t, T.fold0 - 0.35, T.fold1 + FOLD.dur + 0.75, 0.3, 0.4);
  if (ta > 0) {
    let done = 0;
    HP.chunks.forEach((_, k) => { if (foldP(t, k) >= 0.5) done++; });
    const all = done === HP.chunks.length;
    drawText(ctx, layoutText(`${done} / ${HP.chunks.length}`, f(76, 'display', 640), { around: AROUND }), 1780, FOLD.head - 22, { color: all ? C.signalDeep : C.ink, align: 'right', alpha: ta, tracking: -0.01 });
    kicker(ctx, 'pasted copies rebuilt as calls', 1780, FOLD.head + 38, { color: C.ink2, alpha: ta, size: 16, align: 'right' });
  }

  if (t >= T.helpersNum) {
    numberCard(ctx, t, T.helpersNum + 0.4, {
      x: 140, y: 590, value: fmt(S.D.field.rebuilt_calls), maxWidth: 760,
      text: 'helper calls rebuilt,\nin four real games.', note: 'Each one marked as an inferred equivalent call', out: T.consts - 0.4,
    });
  }
}

/** The -g1 line of each line of a copy, in a gutter just left of the code. Large in the close-up. */
function drawGutters(ctx, t, cam, marks) {
  const { lh, cw } = HP.M;
  const close = 1 - progress(t, T.hpull, 0.5);
  const wide = progress(t, T.hpull + 0.6, 0.5) * (1 - progress(t, T.fold1 + FOLD.dur, 0.4));
  if (close <= 0 && wide <= 0) return;
  const F = f(Math.max(9, 15 * cam.s), 'mono', 500);
  const gx = toScreen(cam, 2.6 * cw, 0).x;   // right edge of the gutter: just left of the copy's indent
  ctx.save();
  setFont(ctx, F);
  ctx.textBaseline = 'middle';
  ctx.textAlign = 'right';
  const base = ctx.globalAlpha;
  for (const m of marks) {
    const ev = HP.evidence[m.k] || {};
    const first = m.k === 0;
    const a = (first ? Math.max(close, wide) : wide) * (1 - m.p);
    if (a <= 0) continue;
    for (const row of [0, 2, 3]) {
      const nums = ev[row] || [];
      if (!nums.length) continue;
      // in the close-up the numbers type in one by one
      const at = T.evidence0 + [0, 0, 0.32, 0.6][row];
      const ra = first && close > 0 ? progress(t, at, 0.35) : 1;
      if (ra <= 0) continue;
      const yy = toScreen(cam, 0, m.y + (row + 0.5) * lh * lerp(1, 1 / m.c.lines.length, m.p)).y;
      if (yy < -40 || yy > 1120) continue;
      const fromHelper = nums.some((n) => HP.helperCode.has(n));
      ctx.globalAlpha = base * a * ra;
      ctx.fillStyle = fromHelper ? C.signalDeep : C.ink3;
      ctx.fillText(nums.join(' '), gx, yy + 1);
    }
  }
  // what the numbers are, over the gutter in the close-up
  if (close > 0) kicker(ctx, 'line', gx, MACRO.y - 28, { color: C.ink3, align: 'right', alpha: close * progress(t, T.evidence0, 0.4), size: 16 });
  ctx.restore();
}

/** The helper as Roblox wrote it, right of the close-up, with leaders from the copy's lines. */
function drawHelperSource(ctx, t, cam) {
  const a = envelope(t, T.evidence0 + 0.6, T.hpull + 0.4, 0.45, 0.4);
  if (a <= 0) return;
  const h = S.D.helper;
  const { lh } = HP.M;
  const sm = codeMetrics(SRC.size, SRC.lineHeight);
  // align the helper's first code line (851) with the copy's `if` line
  const rowY = (r) => toScreen(cam, 0, 1 * lh + (r + 0.5) * lh).y;   // copy 1's rows on screen
  const k851 = HP.helperLines.findIndex((hl) => hl.line === [...HP.helperCode][0]);
  const top = rowY(2) - (k851 + 0.5) * sm.lh;
  const srcY = (k) => top + (k + 0.5) * sm.lh;
  ctx.save();
  ctx.globalAlpha *= a;
  kicker(ctx, `${h.name}, in Roblox's source`, SRC.numX - 40, top - 34, { color: C.ink2, size: 17 });
  HP.helperLines.forEach((hl, k) => {
    const ra = progress(t, T.evidence0 + 0.6 + k * 0.06, 0.45, ease.out);
    if (ra <= 0) return;
    const isLit = HP.helperCode.has(hl.line) && k < HP.helperLines.length - 1;
    const wa = isLit ? progress(t, T.evidence0 + 1.25, 0.5) : 0;
    ctx.save();
    ctx.globalAlpha *= ra;
    if (wa > 0) {
      ctx.save();
      ctx.globalAlpha *= wa;
      ctx.fillStyle = 'rgba(255,77,26,0.10)';
      ctx.fillRect(SRC.x - 14, srcY(k) - sm.lh * 0.42, (hl.text.replace(/\t/g, '    ').length + 1.2) * sm.cw, sm.lh * 0.84);
      ctx.restore();
    }
    drawText(ctx, layoutText(String(hl.line), f(SRC.size - 4, 'mono', isLit ? 500 : 400)), SRC.numX, srcY(k) + 8, { color: isLit ? mix(C.ink3, C.signalDeep, wa) : C.ink3, align: 'right' });
    drawCode(ctx, layoutCode(hl.text), { x: SRC.x, y: srcY(k) - sm.lh / 2, size: SRC.size, lineHeight: SRC.lineHeight, palette: 'paper' });
    ctx.restore();
  });
  // leaders: from the end of each copy line whose line number is the helper's, to that line of the helper
  const ev = HP.evidence[0];
  let n = 0;
  for (const row of [2, 3]) {
    const line = (ev[row] || [])[0];
    const k = HP.helperLines.findIndex((hl) => hl.line === line);
    if (k < 0) continue;
    const text = HP.chunks[0].lines[row].replace(/\t/g, '    ');
    const x0 = toScreen(cam, (text.length + 1.2) * HP.M.cw, 0).x;
    const lp = progress(t, T.evidence0 + 1.35 + n * 0.3, 0.65, ease.inOut);
    leader(ctx, { x: x0, y: rowY(row) }, { x: SRC.numX - 58, y: srcY(k) }, lp, { width: 2.5, bend: n ? 26 : -26, head: true });
    n++;
  }
  ctx.restore();
}

// ------------------------------------------------------------------------------- VII. constants

const EXPR = { size: 72, lineHeight: 1.4, y: 430 };
const BITS = { pitch: 25, cell: 19, gap: 18, rowA: 556, rowB: 642 };

export function drawConstants(ctx, t) {
  surface(ctx, C.paper);
  const k = S.D.constant;
  const fr = k.frames;
  ctx.save();
  drift(ctx, t, T.consts, T.checked, 0.025, 960, EXPR.y);
  // the title
  say(ctx, 'Constants, solved.', 140, 196, t, T.consts + 0.1, { font: TY.title, color: C.ink, out: T.bits0 - 0.25, tracking: TRACK.title });

  // the expression as Luau left it, then (after the bits) the call it came from
  const before = fr.v251, after = fr.v26;
  const cm = codeMetrics(EXPR.size, EXPR.lineHeight);
  const lift = progress(t, T.bits0, 0.8, ease.inOutCubic) * (1 - progress(t, T.back, 0.8, ease.inOutCubic));
  const exprY = lerp(EXPR.y + 40, 330, lift);
  const morph = codeMorph(before, after);
  const mp = progress(t, T.back + 0.35, 1.3);
  const fracOut = progress(t, T.frac0, 0.6, ease.inOut);
  const ea = progress(t, T.consts, 0.35, ease.out);
  const fracY = 640;
  const exprShift = lerp(0, -150, ease.inOutCubic(fracOut));
  if (ea > 0) {
    ctx.save();
    ctx.globalAlpha *= ea;
    const wB = layoutCode(after).cols * cm.cw, wA = layoutCode(before).cols * cm.cw;
    const sB = Math.min(1, 1640 / wB);
    const y0 = exprY + exprShift - cm.lh / 2;
    drawMorph(ctx, morph, mp, {
      box: { x: -400, y: y0, w: 3000, h: cm.lh }, fit: 'none', size: EXPR.size, lineHeight: EXPR.lineHeight, palette: 'paper',
      cameraA: { s: 1, x: 960 - wA / 2, y: y0 }, cameraB: { s: sB, x: 960 - (wB * sB) / 2, y: y0 + (cm.lh * (1 - sB)) / 2 },
      highlight: (i) => !/^--/.test(morph.b.tokens[i].text), insertedMix: 1,
    });
    // "folded by Luau" over the number while it is the number
    const na = envelope(t, T.consts + 0.95, T.back + 0.35, 0.4, 0.4);
    if (na > 0) {
      const numCol = before.indexOf('0.');
      kicker(ctx, 'Folded by Luau', 960 - wA / 2 + numCol * cm.cw, y0 - 18, { color: C.ink2, alpha: na, size: 20 });
    }
    // where frames(1) came from, once it is back
    const sa = envelope(t, T.back + 1.0, T.checked - 0.2, 0.6, 0.35);
    if (sa > 0) {
      const src = k.frames.helper_source.replace(/\n\s*/g, ' ');
      drawText(ctx, layoutText(src, f(26, 'mono', 400)), 960, y0 + cm.lh + 46, { color: C.ink3, align: 'center', alpha: sa });
    }
    ctx.restore();
  }

  drawBits(ctx, t, exprY);

  // 2 / 3
  if (t >= T.frac0) {
    const fz = k.fraction;
    const m2 = codeMorph(fz.v251, fz.v26);
    const p2 = progress(t, T.frac0 + 0.75, 1.1);
    const a2 = progress(t, T.frac0 + 0.25, 0.45) * (1 - progress(t, T.checked - 0.3, 0.28));
    const fm = codeMetrics(56, 1.4);
    const wA = layoutCode(fz.v251).cols * fm.cw, wB = layoutCode(fz.v26).cols * fm.cw;
    ctx.save();
    ctx.globalAlpha *= a2;
    drawMorph(ctx, m2, p2, {
      box: { x: -400, y: fracY - fm.lh / 2, w: 3000, h: fm.lh }, fit: 'none', size: 56, lineHeight: 1.4, palette: 'paper',
      cameraA: { s: 1, x: 960 - wA / 2, y: fracY - fm.lh / 2 }, cameraB: { s: 1, x: 960 - wB / 2, y: fracY - fm.lh / 2 },
      insertedMix: 1,
    });
    ctx.restore();
    const fa = envelope(t, T.frac0 + 0.45, T.checked - 0.2, 0.4, 0.3);
    if (fa > 0) kicker(ctx, 'Exact fractions, bit for bit', 960, fracY - fm.lh / 2 - 22, { color: C.ink2, align: 'center', alpha: fa, size: 20 });
  }
  ctx.restore();
}

function drawBits(ctx, t, exprY) {
  const k = S.D.constant;
  const a = progress(t, T.bits0, 0.4) * (1 - progress(t, T.back - 0.1, 0.6, ease.inOut));
  if (a <= 0) return;
  const A = k.bits.bits, Bb = k.luau_fold.bits;
  const L = k.layout; // 1 / 11 / 52
  const groupOf = (i) => (i < L.sign_bits ? 0 : i < L.sign_bits + L.exponent_bits ? 1 : 2);
  const width = 64 * BITS.pitch + 2 * BITS.gap - (BITS.pitch - BITS.cell);
  const x0 = 960 - width / 2;
  const xOf = (i) => x0 + i * BITS.pitch + groupOf(i) * BITS.gap;
  // the number's own position: the bits come out of it
  const cm = codeMetrics(EXPR.size, EXPR.lineHeight);
  const wA = layoutCode(k.frames.v251).cols * cm.cw;
  const numCol = k.frames.v251.indexOf('0.');
  const numX = 960 - wA / 2 + numCol * cm.cw, numW = k.bits.value.length * cm.cw;
  ctx.save();
  ctx.globalAlpha *= a;
  // row A: the constant in the bytecode, out of the number
  for (let i = 0; i < 64; i++) {
    const p = ease.out(clamp((t - T.bits0 - 0.08 - i * 0.007) / 0.7));
    if (p <= 0) continue;
    const x = lerp(numX + (i / 63) * numW, xOf(i), p), y = lerp(exprY, BITS.rowA, p);
    cell(ctx, x, y, A[i] === '1', C.ink, p);
  }
  // row B: 1 / 60 folded by Luau's rules, landing bit by bit; each turns the accent where it matches
  let matched = 0;
  for (let i = 0; i < 64; i++) {
    const at = T.match0 + 0.2 + ease.inOutQuad(i / 63) * (T.match1 - T.match0 - 0.45);
    const p = ease.out(clamp((t - at) / 0.3));
    if (p <= 0) continue;
    if (Bb[i] === A[i] && p >= 0.5) matched++;
    cell(ctx, xOf(i), BITS.rowB - (1 - p) * 24, Bb[i] === '1', Bb[i] === A[i] ? C.signalDeep : C.ink, p);
  }
  // labels
  const la = progress(t, T.bits0 + 0.45, 0.5);
  kicker(ctx, 'sign', xOf(0) + BITS.cell, BITS.rowA - 30, { color: C.ink3, alpha: la, size: 15, align: 'right' });
  kicker(ctx, 'exponent', xOf(1), BITS.rowA - 30, { color: C.ink3, alpha: la, size: 15 });
  kicker(ctx, 'mantissa', xOf(12), BITS.rowA - 30, { color: C.ink3, alpha: la, size: 15 });
  kicker(ctx, `${k.bits.hex} · in the bytecode`, x0 + width, BITS.rowA - 30, { color: C.ink3, alpha: la, size: 15, align: 'right' });
  const lb = progress(t, T.match0, 0.5);
  kicker(ctx, '1 / 60, folded by Tovek the way Luau folds it', x0, BITS.rowB + 48, { color: C.ink2, alpha: lb, size: 17 });
  // the count
  if (t >= T.match0) {
    const F = f(76, 'display', 640);
    const done = matched >= 64;
    drawText(ctx, layoutText(`${matched} / 64`, F), x0 + width, BITS.rowB + 150, { color: done ? C.signalDeep : C.ink, align: 'right', tracking: -0.01 });
    const ba = progress(t, T.match1 - 0.1, 0.5);
    if (ba > 0) kicker(ctx, 'bits the same', x0 + width, BITS.rowB + 192, { color: C.ink2, align: 'right', alpha: ba });
  }
  ctx.restore();
}

function cell(ctx, x, y, one, color, p = 1) {
  const s = BITS.cell * (0.7 + 0.3 * p);
  const o = (BITS.cell - s) / 2;
  if (one) {
    ctx.fillStyle = color;
    roundRect(ctx, x + o, y - s / 2, s, s, 4);
  } else {
    ctx.strokeStyle = color === C.ink ? rgba(C.ink, 0.42) : color;
    ctx.lineWidth = 1.6;
    ctx.beginPath();
    if (ctx.roundRect) ctx.roundRect(x + o + 0.8, y - s / 2 + 0.8, s - 1.6, s - 1.6, 4); else ctx.rect(x + o, y - s / 2, s, s);
    ctx.stroke();
  }
}
