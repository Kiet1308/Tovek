// Act III. The release pages. A hard cut to white where "V2" fills the frame; V2.1 turns over like a
// page and races two timers across it; V2.1.1 fills the frame with damaged scripts; the field tiles
// itself into V2.5's deep blue, where 198 wrong outputs go out one by one; V2.5.1 sets up the file
// that V2.6 will rebuild.

import {
  ease, clamp, lerp, envelope, smoothstep, spring,
  layoutText, drawText, reveal, measure, metrics, setFont, drawCounter, drawOdometer, formatNumber,
  layoutCode, codeMorph, drawMorph, rgba, mix, hash01, pixel, roundRect,
} from '../../engine/index.js';
import {
  T, ERA, TY, TRACK, CODE, COL, COLW, BOX, CLIP, S, surface, hairline, vline, faded, clipRect, rollText,
  camAt, camFit, lerpCam, zoomCam, camTrack, drift, codeAt, edgeFade, recovered, settle, washIn, flowOf, drawFlow,
  drawLedger, label, usDate, stripDot,
} from './base.js';

let P = null;

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

  // the 1,500 damaged scripts: which ones V2 aborted on (a fixed scatter, as many as the notes say)
  const aborted = new Set();
  const n = S.N.v211;
  if (n) for (let i = 0; aborted.size < n.aborted && i < n.scripts * 4; i++) aborted.add(Math.floor(hash01(211, i) * n.scripts));
  // the 198 fuzz failures go out in a fixed shuffled order
  const fz = S.N.v25;
  const order = [];
  if (fz) {
    const idx = Array.from({ length: fz.from }, (_, i) => i);
    for (let i = idx.length - 1; i > 0; i--) { const j = Math.floor(hash01(25, i) * (i + 1)); [idx[i], idx[j]] = [idx[j], idx[i]]; }
    idx.forEach((cell, rank) => { order[cell] = rank; });
  }
  // V2.5's names from context: the second function, V2.1.1 against V2.5
  const j211 = ST['v2.1.1'].excerpts.join.text, j25 = ST['v2.5'].excerpts.join.text;
  const pJ = codeMorph(j211, j25);
  const ctxLine = pJ.b.lineOf(/^\s*\w+ = player\.Chatted/);
  const ctxTok = [...pJ.inserted].find((i) => pJ.b.tokens[i].line === ctxLine && pJ.b.tokens[i].k === 'id');
  const L251 = L('v2.5.1');
  P = {
    pV2, camV2From: camFit(for08 - 1, after08, BOX), camV2For: camAt(forLine('v2-v0.1') - 2), camV2While: camAt(whileLine('v2-v0.1') - 4),
    rV2: recovered(pV2, 'all'), aborted, order, pJ, ctxLine, ctxTok, rJ: recovered(pJ, 'all'),
    L251, cam251: camAt(Math.max(0, L251.lineOf(/^local function /) - 1)), ledger: ledgerEvents,
  };
}

export const pmeta = () => P;

// --------------------------------------------------------------------------------------------- V2

export function drawV2(ctx, t) {
  const e = ERA.v2, ty = TY.v2, s = S.ST['v2-v0.1'];
  surface(ctx, e.surface);
  // the giant V2, then it settles into the page's title
  const G = ty.giant;
  const Lg = layoutText(s.name, G);
  const capG = metrics(G).capHeight || G.size * 0.72;
  const settleP = ease.inOutCubic(clamp((t - T.v2settle) / 0.95));
  const push = 1 + 0.025 * ease.out(clamp((t - T.v2) / (T.v2settle - T.v2)));
  const k = lerp(push, TY.v2.title.size / G.size, settleP);
  const x = lerp(960 - (Lg.width * push) / 2, COL - 8, settleP);
  const y = lerp(540 + (capG * push) / 2, 420, settleP);
  ctx.save();
  ctx.translate(x, y);
  ctx.scale(k, k);
  drawText(ctx, Lg, 0, 0, { color: e.ink, tracking: TRACK.title });
  ctx.restore();

  // the page around it
  const pa = ease.out(clamp((t - T.v2settle - 0.6) / 0.6));
  const meta = `${usDate(s.date)}  ·  Release ${S.RIDX.get(s)} of ${S.D.totals.releases}`;
  if (pa > 0) {
    reveal(ctx, layoutText(meta, ty.meta), COL, 150, t, { unit: 'line', start: T.v2settle + 0.6, dur: 0.8, color: e.ink2 });
    reveal(ctx, layoutText('Tovek', ty.tovek), COL - 2, 214, t, { unit: 'line', start: T.v2settle + 0.6, dur: 0.8, color: e.ink, tracking: -0.01 });
    reveal(ctx, layoutText(s.headline, ty.head), COL, 506, t, { unit: 'word', start: T.v2settle + 0.9, stagger: 0.06, dur: 0.8, color: e.ink2 });
  }
  // anonymous bindings across the corpus, from the release notes
  const n = S.N.v2;
  if (n) {
    const t0 = T.v2settle + 1.5;
    faded(ctx, ease.out(clamp((t - t0) / 0.6)), () => {
      drawOdometer(ctx, t, { from: n.from, to: n.to, start: t0 + 0.3, dur: 2.2, x: COL - 4, y: 680, font: ty.num, color: e.ink, turns: 1 });
      drawText(ctx, layoutText(`${n.label}, down from ${formatNumber(n.from)}`, ty.unit), COL, 724, { color: e.ink2 });
    });
  }
  // the code: 0.9's dispatcher dissolves into continue and break
  const pal = S.PAL.v2;
  const ca = ease.out(clamp((t - T.v2Morph0 + 0.6) / 0.6));
  faded(ctx, ca, () => clipRect(ctx, CLIP, () => {
    const p = clamp((t - T.v2Morph0) / (T.v2Morph1 - T.v2Morph0));
    if (p <= 0) codeAt(ctx, P.pV2.a, P.camV2From, pal);
    else if (p < 1) drawMorph(ctx, P.pV2, p, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, cameraA: P.camV2From, cameraB: P.camV2For, blur: 5, highlight: P.rV2 });
    else {
      const cam = camTrack([[T.v21 - 2.2, P.camV2For], [T.v21 - 0.6, P.camV2While]])(t);
      codeAt(ctx, P.pV2.b, cam, pal, { highlight: P.rV2, mix: settle(T.v2Morph1, 1.6)(t), washAlpha: washIn(T.v2Morph1, 1.6)(t), washColor: e.marker });
      const fa = envelope(t, T.v2Morph1 - 0.01, T.v21, 0.5, 0.4);
      if (fa > 0) drawFlow(ctx, P.pV2.b, cam, t, e.ink2, { gotoA: 0, loopA: fa });
    }
    edgeFade(ctx, CLIP, e.surface);
  }));
  drawLedger(ctx, t, P.ledger, 'v2', COL, 828, ease.out(clamp((t - T.v2Morph0) / 0.8)));
}

// ------------------------------------------------------------------------------------------- V2.1

/** The page turn: the next page slides in from the right with a hard edge on whole device pixels. */
function turnEdge(ctx, t, t0, dur = 1.15) {
  const p = ease.inOutCubic(clamp((t - t0) / dur));
  const px = pixel(ctx);
  return Math.round(lerp(1922, -2, p) / px) * px;
}

export function drawV21(ctx, t) {
  const e = ERA.v21, ty = TY.v21;
  const edge = turnEdge(ctx, t, T.v21);
  clipRect(ctx, { x: edge, y: 0, w: 1920 - edge + 4, h: 1080 }, () => {
    surface(ctx, e.surface);
    drawV21Page(ctx, t);
  });
  if (edge > 0) vline(ctx, edge, 0, 1080, e.ink, 0.22);
}

function drawV21Page(ctx, t) {
  const e = ERA.v21, ty = TY.v21;
  const s1 = S.ST['v2.1'], s2 = S.ST['v2.1.1'];
  const two = t >= T.v211;
  const cur = two ? s2 : s1;
  // meta, Tovek, the name (rolling from V2.1 to V2.1.1), the headline beside it
  const meta = (st) => `RELEASE NOTES  ·  ${usDate(st.published_at || st.date).toUpperCase()}`;
  label(ctx, meta(cur), COL, 132, e.accent, { tracking: 0.12, alpha: ease.out(clamp((t - T.v21 - 0.4) / 0.6)) });
  drawText(ctx, layoutText('Tovek', ty.tovek), COL - 2, 196, { color: e.ink, tracking: -0.01, alpha: ease.out(clamp((t - T.v21 - 0.4) / 0.6)) });
  const TF = ty.title;
  if (!two) reveal(ctx, layoutText(s1.name, TF), COL - 8, 384, t, { unit: 'glyph', start: T.v21 + 0.5, stagger: 0.05, dur: 0.85, color: e.ink, tracking: TRACK.title });
  else {
    // V2.1 -> V2.1.1: the new characters rise in
    const L1 = layoutText(s1.name, TF), L2 = layoutText(s2.name, TF);
    drawText(ctx, L1, COL - 8, 384, { color: e.ink, tracking: TRACK.title });
    const tail = layoutText(s2.name.slice(s1.name.length), TF);
    const q = ease.out(clamp((t - T.v211) / 0.6));
    clipRect(ctx, { x: COL - 8 + L1.width, y: 384 - TF.size, w: 900, h: TF.size * 1.15 }, () => {
      drawText(ctx, tail, COL - 8 + L1.width + TRACK.title * TF.size * s1.name.length, 384 + (1 - q) * TF.size * 0.9, { color: e.ink, tracking: TRACK.title });
    });
  }
  const titleW = measure(cur.name, TF) + TRACK.title * TF.size * (cur.name.length - 1);
  const hx = Math.max(COL + titleW + 70, 980);
  const HF = ty.head;
  const hl = layoutText(s1.headline, HF, { maxWidth: 1824 - hx, lineHeight: 1.08 });
  const hl2 = layoutText(s2.headline, HF, { maxWidth: 1824 - hx, lineHeight: 1.08 });
  reveal(ctx, hl, hx, 384 - hl.height - 6, t, { unit: 'word', start: T.v21 + 0.9, stagger: 0.07, dur: 0.8, color: e.ink2, out: { start: T.v211 - 0.35, stagger: 0.02, dur: 0.35 } });
  reveal(ctx, hl2, hx, 384 - hl2.height - 6, t, { unit: 'word', start: T.v211 + 0.2, stagger: 0.07, dur: 0.8, color: e.ink2 });
  hairline(ctx, COL, 438, 1824 - COL, e.ink, 0.12 * ease.out(clamp((t - T.v21 - 0.6) / 0.8)));

  // V2 against V2.1 on one game, one thread: two timers racing across the page
  const n = S.N.v21;
  const fieldIn = clamp((t - T.v211 - 0.1) / 0.8);
  if (n && fieldIn < 1) {
    const t0 = T.v21 + 1.6, race = 2.8;
    const a = ease.out(clamp((t - t0) / 0.6)) * (1 - ease.inOut(fieldIn));
    faded(ctx, a, () => {
      drawText(ctx, layoutText(`${formatNumber(n.scripts)}-script game, one thread`, ty.unit), COL, 540, { color: e.ink2 });
      const x0 = COL + 200, x1 = 1460;
      const rows = [{ label: 'V2', secs: n.v2, color: e.ink3 }, { label: S.ST['v2.1'].name, secs: n.v21, color: e.fill }];
      rows.forEach((r, i) => {
        const y = 640 + i * 132;
        const elapsed = clamp((t - t0 - 0.4) / race) * n.v2;
        const run = Math.min(elapsed, r.secs);
        const done = elapsed >= r.secs;
        drawText(ctx, layoutText(r.label, ty.big), COL, y + 22, { color: e.ink });
        ctx.fillStyle = rgba(e.ink, 0.07);
        ctx.fillRect(x0, y - 20, x1 - x0, 44);
        // the bar collapses into a row of the field when V2.1.1 arrives
        const w = ((x1 - x0) * run) / n.v2;
        ctx.fillStyle = r.color;
        ctx.fillRect(x0, y - 20, w, 44);
        drawCounter(ctx, run, 1824, y + 30, ty.num, { align: 'right', decimals: 1, suffix: ' s', color: done && i === 1 ? e.accent : e.ink });
      });
    });
  }
  // V2.1.1: 1,500 damaged scripts fill the page; V2 aborted on some, V2.1.1 on none
  const d = S.N.v211;
  if (d && t > T.v211) {
    const a = ease.out(fieldIn);
    const cols = 75, rows = Math.ceil(d.scripts / cols), step = 20.6, x0 = 960 - ((cols - 1) * step) / 2, y0 = 520;
    const fixed = ease.inOut(clamp((t - T.v211 - 2.0) / 0.9));
    const rr = 3.4;
    ctx.save();
    for (let i = 0; i < d.scripts; i++) {
      const c = i % cols, r = Math.floor(i / cols);
      const appear = ease.out(clamp((t - T.v211 - 0.1 - (c / cols) * 0.5 - (r / rows) * 0.2) / 0.4));
      if (appear <= 0) continue;
      const bad = P.aborted.has(i);
      const cx = x0 + c * step, cy = y0 + r * step;
      ctx.globalAlpha = a * appear * (bad ? lerp(1, 0.4, fixed) : 0.4);
      ctx.fillStyle = bad ? mix(e.accent, e.ink3, fixed) : e.ink3;
      const s = bad ? lerp(rr * 1.7, rr, fixed) : rr;
      ctx.fillRect(cx - s, cy - s, s * 2, s * 2);
    }
    ctx.restore();
    const yb = y0 + rows * step + 26;
    faded(ctx, a, () => {
      drawText(ctx, layoutText(`${formatNumber(d.scripts)} damaged scripts`, ty.unit), x0 - rr, yb, { color: e.ink2 });
      const msg = fixed < 0.5 ? `V2 aborted ${formatNumber(d.aborted)} times` : `${S.ST['v2.1.1'].name}: never`;
      const MF = ty.big;
      drawText(ctx, layoutText(msg, MF), x0 + (cols - 1) * step + rr, 486, { color: fixed < 0.5 ? e.ink : e.accent, align: 'right' });
    });
  }
}

/** Where V2.1.1's marks sit, so V2.5's tiles can grow out of them. */
export function fieldCells() {
  const d = S.N.v211;
  if (!d) return null;
  const cols = 75, step = 20.6, x0 = 960 - ((cols - 1) * step) / 2, y0 = 520;
  return { cols, rows: Math.ceil(d.scripts / cols), step, x0, y0 };
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

function drawV25Page(ctx, t) {
  const e = ERA.v25, ty = TY.v25;
  const s1 = S.ST['v2.5'], s2 = S.ST['v2.5.1'];
  const two = t >= T.v251;
  const head0 = T.v25 + 0.7;
  // title block, top left
  const cur = two ? s2 : s1;
  label(ctx, usDate(cur.published_at || cur.date), COL, 140, e.ink2, { tracking: 0, font: ty.meta, alpha: ease.out(clamp((t - head0) / 0.6)) });
  drawText(ctx, layoutText('Tovek', ty.tovek), COL - 2, 204, { color: e.ink, tracking: -0.01, alpha: ease.out(clamp((t - head0) / 0.6)) });
  const TF = ty.title;
  const L1 = layoutText(s1.name, TF);
  if (!two) reveal(ctx, L1, COL - 8, 392, t, { unit: 'glyph', start: head0 + 0.1, stagger: 0.05, dur: 0.85, color: e.ink, tracking: TRACK.title });
  else {
    drawText(ctx, L1, COL - 8, 392, { color: e.ink, tracking: TRACK.title });
    const q = ease.out(clamp((t - T.v251) / 0.6));
    const tail = layoutText(s2.name.slice(s1.name.length), TF);
    const tx = COL - 8 + L1.width + TRACK.title * TF.size * s1.name.length;
    clipRect(ctx, { x: tx, y: 392 - TF.size, w: 600, h: TF.size * 1.15 }, () => drawText(ctx, tail, tx, 392 + (1 - q) * TF.size * 0.9, { color: e.ink, tracking: TRACK.title }));
  }
  const H1 = layoutText(s1.headline, ty.head, { maxWidth: 620 }), H2 = layoutText(s2.headline, ty.head, { maxWidth: 620 });
  reveal(ctx, H1, COL, 470, t, { unit: 'word', start: head0 + 0.5, stagger: 0.06, dur: 0.8, color: e.ink2, out: { start: T.v251 - 0.35, stagger: 0.02, dur: 0.35 } });
  reveal(ctx, H2, COL, 470, t, { unit: 'word', start: T.v251 + 0.2, stagger: 0.06, dur: 0.8, color: e.ink2 });

  // semantic fuzzing: a field of 198 wrong outputs, going out one by one with the count
  const fz = S.N.v25;
  if (fz && t < T.ctx25 + 0.8) {
    const leave = ease.inOut(clamp((t - T.ctx25 + 0.2) / 0.7));
    const a = ease.out(clamp((t - T.fuzz0 + 0.6) / 0.7)) * (1 - leave);
    const start = T.fuzz0 + 0.3, dur = 3.2;
    const fn = ease.inOut;
    const current = lerp(fz.from, fz.to, fn(clamp((t - start) / dur)));
    const outCount = fz.from - current;
    const cols = 18, rows = Math.ceil(fz.from / cols), cell = 54, size = 38;
    const gx0 = 1824 - cols * cell + (cell - size), gy0 = 330;
    faded(ctx, a, () => {
      for (let i = 0; i < fz.from; i++) {
        const c = i % cols, r = Math.floor(i / cols);
        const rank = P.order[i];
        // a mark goes out when the count passes its rank: a short fade, and it sinks a little
        const goneP = clamp(outCount - rank);
        const appear = ease.out(clamp((t - T.fuzz0 + 0.5 - (c + r) * 0.012) / 0.4));
        if (appear <= 0) continue;
        const x = gx0 + c * cell, y = gy0 + r * cell;
        ctx.globalAlpha = a * appear;
        if (goneP < 1) {
          ctx.fillStyle = mix(e.accent, e.faint, goneP);
          const k = lerp(1, 0.55, goneP);
          roundRect(ctx, x + (size * (1 - k)) / 2, y + (size * (1 - k)) / 2, size * k, size * k, 6 * k);
        } else {
          ctx.fillStyle = e.faint;
          roundRect(ctx, x + size * 0.225, y + size * 0.225, size * 0.55, size * 0.55, 3.3);
        }
      }
      const landed = clamp((t - start - dur) / 0.5);
      drawOdometer(ctx, t, { from: fz.from, to: fz.to, start, dur, x: COL - 6, y: 790, font: ty.num, color: landed > 0 ? mix(e.ink, e.accent, landed) : e.ink, turns: 1, ease: fn });
      drawText(ctx, layoutText('wrong or missing outputs in semantic fuzzing', ty.unit), COL, 842, { color: e.ink2 });
      drawText(ctx, layoutText(`${fz.fromLabel} → ${s1.name}`, ty.unit), COL, 876, { color: e.ink2, alpha: 0.85 });
    });
  }

  // names from context: the second function, V2.1.1 against V2.5, then a push into the new name
  if (t > T.ctx25 - 0.3 && t < T.v251 + 0.9) {
    const pal = S.PAL.v25;
    const a = ease.out(clamp((t - T.ctx25) / 0.5)) * (1 - ease.inOut(clamp((t - T.v251) / 0.6)));
    const pJ = P.pJ;
    const { cw, lh } = S.CM;
    const s0 = Math.min(1.25, 1040 / (pJ.b.cols * cw), 860 / (pJ.b.lineCount * lh));
    const wide = { s: s0, x: 1300 - (pJ.b.cols * cw * s0) / 2, y: 560 - (pJ.b.lineCount * lh * s0) / 2 };
    const tk = P.ctxTok != null ? pJ.b.tokens[P.ctxTok] : null;
    const sZ = Math.min(4.6, 1000 / ((tk ? tk.text.length : 10) * cw));
    const close = tk ? { s: sZ, x: 1300 - (tk.col + tk.text.length / 2) * cw * sZ, y: 560 - (tk.line + 0.5) * lh * sZ } : wide;
    const pm = clamp((t - T.ctx25 - 0.2) / 1.6);
    const zoom = ease.inOutCubic(clamp((t - T.ctx25 - 2.0) / 1.1));
    faded(ctx, a, () => clipRect(ctx, { x: 700, y: 40, w: 1220, h: 1000 }, () => {
      if (pm < 1) drawMorph(ctx, pJ, pm, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, cameraA: wide, cameraB: wide, blur: 5, highlight: P.rJ });
      else codeAt(ctx, pJ.b, zoomCam(wide, close, zoom), pal, { clip: { x: 700, y: 0, w: 1220, h: 1080 }, highlight: P.rJ, mix: 1, lineAlpha: (l) => (tk && l === tk.line ? 1 : 1 - 0.88 * zoom) });
    }));
  }

  // V2.5.1: helper calls across four games, and the file V2.6 will rebuild
  const c = S.N.v251;
  if (two) {
    const a = ease.out(clamp((t - T.v251 - 0.5) / 0.6));
    if (c) faded(ctx, a, () => {
      drawOdometer(ctx, t, { from: c.from ?? c.to, to: c.to, start: T.v251 + 0.8, dur: 1.8, x: COL - 6, y: 790, font: ty.num, color: e.ink, turns: 1 });
      drawText(ctx, layoutText(c.label, ty.unit, { maxWidth: COLW }), COL, 842, { color: e.ink2 });
    });
    const pal = S.PAL.v25;
    faded(ctx, ease.out(clamp((t - T.v251 - 0.3) / 0.7)), () => clipRect(ctx, CLIP, () => {
      codeAt(ctx, P.L251, cam251(t), pal);
      edgeFade(ctx, CLIP, e.surface);
    }));
  }
}

/** V2.5.1's camera: helpers and the top of shield, drifting gently. */
export function cam251(t) {
  return drift(P.cam251, t, T.v251, T.flood + 1.2, { zoom: 0.03, dx: -10, dy: -6, cx: 1300 });
}
