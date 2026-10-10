// Act V. Beside the original source, then the numbers, then the mark. The two panels leave as their
// own lines are wiped into night one by one; the last card is given time to breathe.

import {
  ease, clamp, lerp, envelope, spring, progress,
  layoutText, drawText, reveal, typewriter, measure, drawCounter, drawOdometer, formatNumber,
  codeMorph, codeMetrics, drawCode, drawMark, markWidth, rgba, mix, pixel, roundRect,
} from '../../engine/index.js';
import { T, ERA, TY, TRACK, AROUND, S, surface, hairline, faded, label, word, cap } from './base.js';

let SIDE = null;

function runsOf(L, set) {
  const parts = [];
  for (const i of set) for (const p of L.tokens[i].parts) parts.push({ line: p.line, c0: p.col, c1: p.col + p.text.length });
  parts.sort((a, b) => a.line - b.line || a.c0 - b.c0);
  const out = [];
  for (const p of parts) {
    const last = out[out.length - 1];
    if (last && last.line === p.line && p.c0 - last.c1 <= 1) last.c1 = Math.max(last.c1, p.c1);
    else out.push({ ...p });
  }
  return out;
}

export function prepareEnd() {
  const v = S.V26.excerpts.shield.text, o = S.D.sample.focus_source.text;
  const plan = codeMorph(v, o);
  const kinds = new Set([...plan.removed.map((r) => r.k), ...[...plan.inserted].map((i) => plan.b.tokens[i].k)]);
  const onlyNames = [...kinds].every((k) => ['id', 'fn', 'prop', 'glob', 'num', 'com'].includes(k));
  const size = 15, lineHeight = 1.4;
  const cm = codeMetrics(size, lineHeight);
  const removedRuns = [];
  for (const r of plan.removed) {
    const last = removedRuns[removedRuns.length - 1];
    const c1 = r.col + r.text.length;
    if (last && last.line === r.line && r.col - last.c1 <= 1) last.c1 = Math.max(last.c1, c1);
    else removedRuns.push({ line: r.line, c0: r.col, c1 });
  }
  const wA = plan.b.cols * cm.cw, wB = plan.a.cols * cm.cw;
  const gap = 120;
  const x0 = Math.round(960 - (wA + gap + wB) / 2);
  SIDE = {
    plan, size, lineHeight, cm, onlyNames, kept: plan.stats.kept, total: plan.b.tokens.length, removedRuns, insertedRuns: runsOf(plan.b, plan.inserted),
    xs: [x0, x0 + wA + gap], top: 252, lines: Math.max(plan.a.lineCount, plan.b.lineCount),
  };
  return SIDE;
}

export const smeta = () => SIDE;

/** The time each panel line is wiped into night, top to bottom. */
const wipeAt = (l) => T.end - 0.15 + l * 0.022;

export function drawSide(ctx, t) {
  const e = ERA.v26, ty = TY.v26, S2 = SIDE;
  surface(ctx, e.surface);
  const a = ease.out(clamp((t - T.orig - 0.2) / 0.8));
  const { cw, lh } = S2.cm;
  const top = S2.top;
  const panels = [
    { x: S2.xs[0], L: S2.plan.b, label: `Original source  ·  ${S.D.sample.file}`, runs: S2.insertedRuns },
    { x: S2.xs[1], L: S2.plan.a, label: `Tovek ${S.V26.name}`, runs: S2.removedRuns },
  ];
  ctx.save();
  ctx.globalAlpha *= a;
  // the count of tokens that came back exactly
  const t0 = T.orig + 0.8;
  const kept = Math.round(lerp(0, S2.kept, ease.out(clamp((t - t0) / 1.6))));
  const statL = layoutText(`of ${S2.total} tokens come back exactly.`, ty.stat);
  const numW = measure(String(S2.kept), ty.stat) + 16;
  const total = numW + statL.width;
  drawCounter(ctx, kept, 960 - total / 2 + numW - 16, 150, ty.stat, { color: e.accent, align: 'right' });
  drawText(ctx, statL, 960 - total / 2 + numW, 150, { color: e.ink });
  ctx.restore();
  const washA = ease.out(clamp((t - T.orig - 2.4) / 0.6));
  panels.forEach((pn, i) => {
    const pa = ease.out(clamp((t - T.orig - 0.4 - i * 0.15) / 0.8));
    if (pa <= 0) return;
    ctx.save();
    ctx.globalAlpha *= pa;
    ctx.translate(0, (1 - pa) * 30);
    drawText(ctx, layoutText(pn.label.toUpperCase(), ty.side), pn.x, top - 30, { color: e.ink2, tracking: 0.1 });
    hairline(ctx, pn.x, top - 16, pn.L.cols * cw, e.ink, 0.16);
    ctx.save();
    ctx.translate(pn.x, top);
    if (washA > 0) {
      ctx.save();
      ctx.globalAlpha *= washA;
      ctx.fillStyle = rgba(e.ink, 0.085);
      for (const r of pn.runs) roundRect(ctx, r.c0 * cw - cw * 0.25, r.line * lh + lh * 0.1, (r.c1 - r.c0) * cw * ease.out(washA) + cw * 0.5, lh * 0.8, lh * 0.16);
      ctx.restore();
    }
    drawCode(ctx, pn.L, { size: S2.size, lineHeight: S2.lineHeight, palette: S.PAL.v26, lineAlpha: (l) => ease.out(clamp((t - T.orig - 0.5 - l * 0.012 - i * 0.15) / 0.5)) });
    ctx.restore();
    ctx.restore();
  });
}

/** The wipe into night: each code line is covered by a band of night, sweeping left to right. */
export function drawWipe(ctx, t) {
  const S2 = SIDE;
  const { lh } = S2.cm;
  const e = ERA.end;
  const px = pixel(ctx);
  const sn = (v) => Math.round(v / px) * px;
  let full = true;
  ctx.fillStyle = e.surface;
  // the bands cover the panels' lines, then the rest of the frame in wide strips
  const rows = Math.ceil(1080 / lh) + 1;
  for (let r = 0; r < rows; r++) {
    const y0 = sn(r * lh - (S2.top % lh)), y1 = sn((r + 1) * lh - (S2.top % lh));
    const q = ease.inOutCubic(clamp((t - wipeAt(r) ) / 0.55));
    if (q < 1) full = false;
    if (q <= 0) continue;
    ctx.fillRect(-2, y0, sn(1924 * q), y1 - y0 + px);
  }
  return full;
}

export function drawEnd(ctx, t) {
  const e = ERA.end, ty = TY.end;
  const full = drawWipe(ctx, t);
  if (!full && t < T.end + 1.4) return;
  surface(ctx, e.surface);
  const t0 = T.end + 0.6;
  const tt = S.D.totals;
  const items = [
    { v: tt.releases, label: 'releases' },
    { v: tt.days_since_first_beta, label: `days since ${S.ST['v0.1.0-beta'].name}` },
    { v: tt.commits, label: 'commits' },
  ];
  const outT = T.end + 3.6;
  items.forEach((it, i) => {
    const x = 960 + (i - 1) * 470;
    const st = t0 + i * 0.22;
    const a = ease.out(clamp((t - st) / 0.6)) * (1 - ease.inOut(clamp((t - outT - i * 0.06) / 0.5)));
    if (a <= 0) return;
    const rise = (1 - ease.out(clamp((t - st) / 0.8))) * 24 + ease.inOut(clamp((t - outT - i * 0.06) / 0.5)) * -30;
    ctx.save();
    ctx.globalAlpha *= a;
    ctx.translate(0, rise);
    drawOdometer(ctx, t, { from: 0, to: it.v, start: st, dur: 1.6, x, y: 560, font: ty.num, align: 'center', color: e.ink, turns: 1 });
    drawText(ctx, layoutText(it.label, ty.unit), x, 616, { color: e.ink2, align: 'center' });
    ctx.restore();
  });
  // the mark lands with its eight bits, then the name; then nothing moves but the caret
  const mt = outT + 0.6;
  if (t > mt - 0.1) {
    const size = 120 * (0.94 + 0.06 * spring(t - mt, { freq: 1.1, damping: 0.6 }));
    drawMark(ctx, 960 - markWidth(size) / 2, 300 + (120 - size) / 2, size, { color: e.ink, bitColor: e.accent, body: progress(t, mt, 0.8, ease.out), bits: progress(t, mt + 0.25, 1.3) });
    reveal(ctx, layoutText(`Tovek ${S.V26.name}`, ty.card, { around: AROUND }), 960, 610, t, { unit: 'word', start: mt + 0.5, stagger: 0.14, dur: 1.1, align: 'center', color: e.ink, tracking: TRACK.hero });
    const url = layoutText(S.REPO, ty.url);
    const typed = clamp((t - mt - 1.3) * 28, 0, url.glyphCount);
    const caretOn = typed < url.glyphCount || Math.floor((t - mt) * 1.7) % 2 === 0;
    typewriter(ctx, url, 960 - url.width / 2, 700, typed, { color: e.ink2, caret: t > mt + 1.2, caretAlpha: caretOn ? 0.8 : 0 });
    const credit = layoutText(`Built on medal by ${S.D.medal_history.authors.join(' and ')}`.toUpperCase(), ty.credit);
    drawText(ctx, credit, 960, 880, { color: e.ink2, align: 'center', tracking: 0.12, alpha: ease.out(clamp((t - mt - 1.8) / 0.9)) });
  }
}
