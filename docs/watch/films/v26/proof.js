// VIII. Checked against the bytecode. Black and silence; then every fuzz program as one cell, in the
//       order it ran, lit as the read head passes: decompiled, compiled again and run beside its own
//       bytecode. None came out wrong.
// IX. Four real games. Every script of each game as a waveform of its output's length, drawn by the
//     head in real time against the measured clock (scaled only if the run took longer than the shot).

import { T, C, TY, S, f, fmt, surface, kicker, say, note, numberCard, headV, drift } from './base.js';
import { ease, clamp, lerp, progress, envelope, layoutText, drawText, rgba, setFont } from '../../engine/index.js';

// ------------------------------------------------------------------------------------ VIII. checked

const GRID = { rows: 30, pitch: 16, cell: 11, x: 0, y: 440 };
let G = null;

export function prepareProof() {
  const fz = S.D.fuzz;
  const n = fz.programs;
  let st = fz.statuses;
  if (!st || st.length !== n) {
    // no per-program report: the counts, in a fixed spread
    const c = fz.counts;
    st = [...'p'.repeat(c.passed), ...'r'.repeat(c.refused), ...'i'.repeat(c.invalid), ...'f'.repeat(c.failed || 0)].join('');
  }
  const cols = Math.ceil(n / GRID.rows);
  GRID.x = 960 - (cols * GRID.pitch - (GRID.pitch - GRID.cell)) / 2;
  // running counts per column, so a frame reads them instead of counting 3,000 cells
  const cum = [];
  const acc = { p: 0, r: 0, i: 0, f: 0 };
  for (let c = 0; c < cols; c++) {
    for (let r = 0; r < GRID.rows; r++) {
      const i = c * GRID.rows + r;
      if (i < n) acc[st[i]]++;
    }
    cum.push({ ...acc });
  }
  G = { n, st, cols, cum, x1: GRID.x + cols * GRID.pitch };
  prepareScale();
}

const gridHead = (t) => lerp(GRID.x - 60, G.x1 + 40, progress(t, T.grid0, T.grid1 - T.grid0, ease.inOutSine));

export function drawChecked(ctx, t) {
  surface(ctx, C.night);
  if (t < T.say8 - 0.01) return; // a moment of black and silence
  ctx.save();
  drift(ctx, t, T.say8, T.scale, 0.03, 960, 600);
  say(ctx, 'Each output is compiled again', 140, 214, t, T.say8, { font: TY.say, out: T.zero - 0.2 });
  say(ctx, 'and run beside its bytecode.', 140, 214 + 100, t, T.say8 + 0.3, { font: TY.say, color: C.onNight2, out: T.zero - 0.12 });

  const dim = 1 - 0.82 * progress(t, T.zero, 0.7, ease.inOut);
  const hx = gridHead(t);
  ctx.save();
  ctx.globalAlpha *= dim * progress(t, T.grid0 - 0.4, 0.4);
  const base = ctx.globalAlpha;
  // the grid waits, faint, before the head reaches it
  ctx.fillStyle = C.onNight;
  for (let c = 0; c < G.cols; c++) {
    const x = GRID.x + c * GRID.pitch;
    const passed = hx >= x + GRID.cell;
    for (let r = 0; r < GRID.rows; r++) {
      const i = c * GRID.rows + r;
      if (i >= G.n) break;
      const y = GRID.y + r * GRID.pitch;
      if (!passed) {
        ctx.globalAlpha = base * 0.07;
        ctx.fillStyle = C.onNight;
        ctx.fillRect(x + GRID.cell / 2 - 1.5, y + GRID.cell / 2 - 1.5, 3, 3);
        continue;
      }
      const since = (hx - x) / 260; // a short flash just behind the head
      const flash = clamp(1 - since);
      const s = G.st[i];
      if (s === 'p') {
        ctx.globalAlpha = base * (0.72 + 0.28 * flash);
        ctx.fillStyle = C.onNight;
        ctx.fillRect(x, y, GRID.cell, GRID.cell);
      } else if (s === 'r') {
        ctx.globalAlpha = base * 0.9;
        ctx.strokeStyle = C.onNight2;
        ctx.lineWidth = 1.5;
        ctx.strokeRect(x + 0.75, y + 0.75, GRID.cell - 1.5, GRID.cell - 1.5);
      } else if (s === 'i') {
        ctx.globalAlpha = base * 0.55;
        ctx.fillStyle = C.onNight3;
        ctx.fillRect(x + GRID.cell / 2 - 1.5, y + GRID.cell / 2 - 1.5, 3, 3);
      } else {
        ctx.globalAlpha = base;
        ctx.fillStyle = C.signal;
        ctx.fillRect(x, y, GRID.cell, GRID.cell);
      }
    }
  }
  ctx.restore();
  // the head over the grid
  if (t >= T.grid0 && t < T.grid1 + 0.15) {
    headV(ctx, hx, envelope(t, T.grid0, T.grid1, 0.15, 0.2) * dim, { side: -1, y0: GRID.y - 36, y1: GRID.y + GRID.rows * GRID.pitch + 26 });
  }
  // the legend counts with the cells
  const la = progress(t, T.grid0, 0.5) * (1 - progress(t, T.zero, 0.5));
  if (la > 0) {
    const col = clamp(Math.floor((hx - GRID.x) / GRID.pitch), -1, G.cols - 1);
    const seen = col < 0 ? { p: 0, r: 0, i: 0, f: 0 } : G.cum[col];
    const y = GRID.y + GRID.rows * GRID.pitch + 66;
    const items = [
      ['p', 'same behaviour as the bytecode', C.onNight],
      ['r', 'refused (patched bytecode)', C.onNight2],
      ['i', 'not a valid program', C.onNight3],
      ['f', 'wrong', C.signal],
    ];
    let x = GRID.x;
    const FN = f(30, 'mono', 500), FL = f(21, 'text', 400);
    ctx.save();
    ctx.globalAlpha *= la;
    for (const [k, text, color] of items) {
      const num = fmt(seen[k]);
      drawText(ctx, layoutText(num, FN), x, y, { color: k === 'f' ? C.signal : C.onNight });
      const w = layoutText(num, FN).width;
      drawText(ctx, layoutText(text, FL), x + w + 12, y - 2, { color });
      x += w + 12 + layoutText(text, FL).width + 54;
    }
    ctx.restore();
  }
  ctx.restore();
  if (t >= T.zero) {
    const fz = S.D.fuzz;
    numberCard(ctx, t, T.zero + 0.15, {
      x: 140, y: 690, value: fmt(fz.counts.failed || 0), color: C.signal, sub: C.onNight, quiet: C.onNight2,
      text: `wrong outputs, in ${fmt(fz.programs)} fresh programs.`,
      note: `Compiled at ${fz.levels}, run on the same VM`, out: T.scale - 0.3, font: f(380, 'display', 640),
    });
  }
}

// --------------------------------------------------------------------------------------- IX. scale

let W = null;
const BAND = { x0: 440, x1: 1780, h: 64, y: [452, 590, 728, 866] };

function prepareScale() {
  const sc = S.D.scale, field = S.D.field;
  const games = sc.games.map((g, k) => {
    const sizes = field.sizes[k] || [];
    const max = Math.max(1, ...sizes);
    // one path per game: the length of every script's output, as a mirrored waveform (square root, so
    // the small scripts still show)
    return { ...g, sizes, max, norm: sizes.map((n) => Math.sqrt(n / max)) };
  });
  // the games ran one after another: each band runs for its own measured time
  let t0 = 0;
  for (const g of games) {
    g.start = t0;
    g.dur = g.wall_seconds_median;
    t0 += g.dur;
  }
  // each game's waveform is drawn once into a bitmap (a filled path of thousands of points is slow to
  // rasterise every frame); a frame shows as much of it as the run has covered
  const K = 2; // bitmap pixels per design unit (sharp up to a 3840 px stage)
  const bw = Math.ceil((BAND.x1 - BAND.x0) * K), bh = Math.ceil(BAND.h * 1.6 * K) + 4;
  for (const g of games) {
    if (typeof OffscreenCanvas === 'undefined') break;
    const cv = new OffscreenCanvas(bw, bh);
    const c = cv.getContext('2d', { willReadFrequently: true });
    // one mark per script: a bar as tall as its output is long (up), half that below the axis
    const n = g.norm.length, dx = (BAND.x1 - BAND.x0) / n, mid = (BAND.h + 1) * K;
    const barW = Math.max(1, Math.min(dx * 0.55, 3) * K);
    c.fillStyle = C.onNight;
    for (let i = 0; i < n; i++) {
      const hgt = Math.max(1.5, g.norm[i] * BAND.h) * K;
      c.fillRect(Math.round((i + 0.5) * dx * K - barW / 2), mid - hgt, barW, hgt + hgt * 0.5);
    }
    g.bitmap = { cv, K, mid, bw, bh };
  }
  W = { games, total: t0 };
}

const clockAt = (t) => clamp((t - T.sweep0) * T.sweepRate, 0, W.total);

export function drawScale(ctx, t) {
  surface(ctx, C.night);
  const sc = S.D.scale;
  ctx.save();
  drift(ctx, t, T.scale, T.free, 0.025, 960, 640);
  say(ctx, 'Four real games.', 140, 214, t, T.scale + 0.05, { font: TY.title, out: T.total + 0.1 });
  const ka = envelope(t, T.scale + 0.6, T.total + 0.4, 0.5, 0.4);
  if (ka > 0) kicker(ctx, `Every script with bytecode, decompiled · one mark per script, as tall as its output is long`, 144, 290, { color: C.onNight2, alpha: ka, size: 17 });
  const elapsed = clockAt(t);
  // the waveforms are drawn from bitmaps: let the GPU take them in before the clock starts
  if (t < T.sweep0) {
    ctx.save();
    ctx.globalAlpha = 1 / 1024;
    for (const g of W.games) if (g.bitmap) ctx.drawImage(g.bitmap.cv, 0, 0, 4, 4);
    ctx.restore();
  }
  const outA = 1 - 0.92 * progress(t, T.total + 0.25, 0.6, ease.inOut);
  // the bands
  W.games.forEach((g, k) => {
    const y = BAND.y[k];
    const a = progress(t, T.scale + 0.5 + k * 0.12, 0.5) * outA;
    if (a <= 0) return;
    const done = clamp((elapsed - g.start) / g.dur);
    ctx.save();
    ctx.globalAlpha *= a;
    ctx.fillStyle = C.ruleNight;
    ctx.fillRect(BAND.x0, y, BAND.x1 - BAND.x0, 1.5);
    if (done > 0 && g.bitmap) {
      const n = g.norm.length;
      const upto = Math.max(1, Math.floor(n * done));
      const dx = (BAND.x1 - BAND.x0) / n;
      const b = g.bitmap;
      const w = upto * dx;
      ctx.save();
      ctx.globalAlpha *= done >= 1 ? 0.62 : 0.95;
      ctx.drawImage(b.cv, 0, 0, Math.max(1, w * b.K), b.bh, BAND.x0, y - b.mid / b.K, w, b.bh / b.K);
      ctx.restore();
      if (done < 1) headV(ctx, BAND.x0 + w, 1, { side: -1, y0: y - BAND.h - 10, y1: y + BAND.h * 0.6 + 10, glow: 0.22 });
    }
    // labels: the game and its scripts; its time once done
    kicker(ctx, `Game ${g.id}`, 144, y - 10, { color: C.onNight2, size: 18 });
    drawText(ctx, layoutText(`${fmt(g.decompiled)} scripts`, f(26, 'text', 400)), 144, y + 28, { color: C.onNight });
    if (done >= 1) {
      const da = progress(t, T.sweep0 + (g.start + g.dur) / T.sweepRate, 0.35);
      drawText(ctx, layoutText(`${g.dur.toFixed(2)} s`, f(24, 'mono', 500)), BAND.x1, y + 40, { color: C.onNight2, alpha: da, align: 'right' });
    }
    ctx.restore();
  });
  // the clock
  const ca = progress(t, T.sweep0 - 0.5, 0.4) * outA;
  if (ca > 0) {
    ctx.save();
    ctx.globalAlpha *= ca;
    const running = elapsed < W.total;
    drawText(ctx, layoutText(elapsed.toFixed(2), TY.clock), 1780, 236, { color: running ? C.onNight : C.onNight2, align: 'right' });
    kicker(ctx, T.sweepRate > 1.001 ? `seconds · shown ${T.sweepRate.toFixed(1)}× faster` : 'seconds · real time', 1780, 290, { color: C.onNight2, align: 'right', size: 17 });
    ctx.restore();
  }
  ctx.restore();
  // the total
  if (t >= T.total) {
    const m = sc.machine || {};
    const cpu = m.cpu ? `${m.cpu}${m.kind ? ' ' + m.kind : ''}` : 'one computer';
    numberCard(ctx, t, T.total + 0.55, {
      x: 140, y: 600, value: fmt(sc.scripts), color: C.onNight, sub: C.onNight, quiet: C.onNight2,
      text: `scripts in ${sc.wall_seconds.toFixed(1)} seconds. ${sc.failed === 0 ? 'None failed.' : fmt(sc.failed) + ' failed.'}`,
      note: `${cpu} · ${sc.threads || m.logical} threads · ${fmt(sc.lines)} lines of Luau`, out: T.free - 0.3,
    });
  }
}
