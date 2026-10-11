// Shared ground for the V2.6 launch film: the cut's timing, the type, the colours, the film's data
// once prepared, and the drawing helpers every shot uses. Everything here is a constant, a cache
// keyed by its inputs, or a pure function of its arguments, so any frame can be drawn on its own.
//
// The film's one device is the read head: a hairline of the accent that turns bytecode (night) into
// source (paper). It scans the page in the opening, sweeps the loop back and forth, splits V2.6
// open, runs over the fuzz grid and the four games, and underlines the address at the end.

import {
  ease, clamp, lerp, progress, envelope,
  font, layoutText, drawText, reveal, measure, metrics, setFont,
  layoutCode, codeMetrics, codePalettes,
  V26, rgba, mix, pixel,
} from '../../engine/index.js';

// ------------------------------------------------------------------------------------------ time
// The film runs at 96 beats a minute (a beat is 0.625 s, a bar 2.5 s). Sections start on bars.
// Shots read only these numbers, so the cut is re-timed here.

export const BEAT = 0.625;
export const BAR = 2.5;

export const T = {
  // I. bytes: macro on the bytes, then the camera pulls back until the whole file fills the frame
  first: 0.6, pull0: 1.2, line1: 2.1, line2: 2.75, textOut: 5.6, pull1: 8.1, label: 8.3,
  // II. back to source: the line, the bytes gather into the shape of the code, the read head scans it
  source: 10.0, sayOut: 12.2, gather0: 12.35, gather1: 14.85, scan0: 14.9, scan1: 17.1,
  // III. names: they drain away, then come back from their evidence one by one
  names: 20.0, drain0: 20.7, drain1: 21.4, n1: 21.8, n2: 23.5, n3: 25.75, n4: 27.75, rest0: 29.25, rest1: 29.95, namesNum: 30.35,
  // IV. loops: back to the bytecode's jumps, then forward into the loop's structure
  loops: 32.5, back0: 33.75, back1: 34.75, arcs0: 34.95, fwd0: 36.55, fwd1: 38.15, loopsNum: 39.6,
  // V. new in V2.6
  sting: 42.5, split: 45.55,
  // VI. helpers: one pasted copy and its line numbers, then all eleven fold into calls
  helpers: 46.45, evidence0: 48.2, hpull: 51.6, fold0: 52.75, fold1: 56.0, helpersNum: 57.35,
  // VII. constants: the folded number, its 64 bits, Luau's fold of 1 / 60 beside them, frames(1)
  consts: 60.0, bits0: 61.85, match0: 62.75, match1: 65.0, back: 65.55, frac0: 67.45,
  // VIII. checked: silence, then every program run again
  checked: 70.0, say8: 70.85, grid0: 73.0, grid1: 76.55, zero: 76.95,
  // IX. scale: four games in real time
  scale: 80.0, sweep0: 81.6, sweepMax: 6.0, total: 0,
  // X. free and open
  free: 90.0, os: 92.5, term0: 94.4, browser: 98.45,
  // XI. the end card
  end: 100.8, mark0: 101.5, word: 102.6, url: 103.6, credit: 104.8,
  duration: 112.5,
};

/** Fill the timings that depend on data (the measured time of the four games). */
export function timeFromData(D) {
  const W = D.scale.wall_seconds;
  T.sweepDur = Math.min(T.sweepMax, W);
  T.sweepRate = W / T.sweepDur; // 1 = real time
  T.total = T.sweep0 + T.sweepDur + 0.4;
}

// --------------------------------------------------------------------------------------- the look

export const C = {
  night: V26.night, night2: '#161614', paper: V26.paper, sheet: V26.sheet, paper2: V26.paper2,
  ink: V26.ink, ink2: V26.ink2, ink3: V26.ink3, rule: 'rgba(18,18,16,0.14)',
  onNight: V26.onNight, onNight2: V26.onNight2, onNight3: '#6d6b63', ruleNight: 'rgba(242,240,235,0.16)',
  signal: V26.signal, signalDeep: V26.signalDeep,
};

export const f = (size, family, weight = 400, stretch = 100) => font(size, { family, weight, stretch });

// Display type is Bricolage Grotesque at its normal width; text is Geist; code and labels Geist Mono.
export const TY = {
  hero: f(150, 'display', 640),        // the opening lines
  title: f(112, 'display', 640),       // a section's name
  giant: f(600, 'display', 650),       // V2.6
  num: f(280, 'display', 640),         // a section's number
  word: f(176, 'display', 650),        // free / open source / MIT
  os: f(96, 'display', 620),
  mark: f(200, 'display', 650),        // the wordmark on the end card
  say: f(92, 'display', 640),          // a full sentence in display type
  lead: f(40, 'text', 400),            // a sentence under a number
  note: f(29, 'text', 400),            // a margin note in a shot
  kicker: f(19, 'mono', 500),          // small caps labels
  clock: f(132, 'mono', 500),
  url: f(36, 'mono', 400),
  credit: f(22, 'text', 400),
};
export const TRACK = { hero: -0.006, title: -0.006, word: -0.01, giant: -0.02, num: -0.02 };
// Bricolage sets a period tight against its digits ("2.6"): open it a little. Its kerning lets "ov"
// touch at display sizes: open that one pair (checked by work-launch/glyphcheck_v26.py).
export const AROUND = { '.': 0.03 };
export const PAIRS = { ov: 0.03 };

// --------------------------------------------------------------------------------------- state
// Filled once by prepare(); render() only reads it.

export const S = {
  D: null,        // launch.json
  hero: null,     // the hero script's layouts and token model
};

// ------------------------------------------------------------------------------------- helpers

export const fmt = (n) => String(Math.round(n)).replace(/\B(?=(\d{3})+(?!\d))/g, ',');

/** Fill the frame. */
export function surface(ctx, color, alpha = 1) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.fillStyle = color;
  ctx.fillRect(-4, -4, 1928, 1088);
  ctx.restore();
}

/** A small mono label in capitals with open tracking. */
export function kicker(ctx, text, x, y, { color = C.ink2, align = 'left', alpha = 1, tracking = 0.12, size } = {}) {
  if (alpha <= 0) return;
  const L = layoutText(text.toUpperCase(), size ? f(size, 'mono', 500) : TY.kicker);
  drawText(ctx, L, x, y, { color, align, tracking, alpha });
}

/** A margin note: a short line of text that rises in and leaves. */
export function note(ctx, text, x, y, t, start, out, { color = C.ink2, align = 'left', font: F = TY.note, maxWidth } = {}) {
  if (t < start || t > out + 0.6) return;
  const L = layoutText(text, F, { maxWidth, lineHeight: 1.3 });
  reveal(ctx, L, x, y, t, { unit: 'line', start, dur: 0.6, stagger: 0.08, align, color, mask: true, out: { start: out, dur: 0.45, stagger: 0.04 } });
}

/**
 * A line of display type that rises into place word by word and leaves the same way.
 * `out` is the time it starts to leave (Infinity to stay).
 */
export function say(ctx, text, x, y, t, start, { font: F = TY.hero, color = C.onNight, align = 'left', out = Infinity, stagger = 0.075, dur = 0.85, colorOf, maxWidth, lineHeight = 1.04, tracking } = {}) {
  const L = layoutText(text, F, { maxWidth, lineHeight, around: AROUND, pairs: PAIRS });
  if (t < start) return L;
  reveal(ctx, L, x, y, t, {
    unit: 'word', start, stagger, dur, align, color, colorOf, mask: true, rise: 1.0,
    tracking: tracking ?? TRACK.hero,
    out: out < Infinity ? { start: out, stagger: 0.035, dur: 0.55, rise: 1.0 } : undefined,
  });
  return L;
}

/** Underline a span (design coordinates) as it is read: a short accent rule that draws left to right. */
export function underline(ctx, x, y, w, p, { color = C.signalDeep, width = 3, alpha = 1 } = {}) {
  if (p <= 0 || alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.fillStyle = color;
  ctx.fillRect(x, y, w * clamp(p), width);
  ctx.restore();
}

/**
 * A hairline leader from a to b along a quadratic curve through `ctrl` (or bent by `bend`), drawn up
 * to `p` (0..1), with a dot at its start and an optional arrowhead at its end.
 */
export function leader(ctx, a, b, p, { color = C.signalDeep, width = 2, bend = -60, dot = true, alpha = 1, head = false, ctrl = null } = {}) {
  if (p <= 0 || alpha <= 0) return;
  const mx = (a.x + b.x) / 2, my = (a.y + b.y) / 2;
  const dx = b.x - a.x, dy = b.y - a.y, len = Math.hypot(dx, dy) || 1;
  const cx = ctrl ? ctrl.x : mx + (-dy / len) * bend, cy = ctrl ? ctrl.y : my + (dx / len) * bend;
  const N = 48, end = Math.max(1, Math.round(N * clamp(p)));
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.strokeStyle = color;
  ctx.fillStyle = color;
  ctx.lineWidth = width;
  ctx.lineCap = 'round';
  ctx.beginPath();
  let lx = a.x, ly = a.y, px = a.x, py = a.y;
  for (let i = 0; i <= end; i++) {
    const q = Math.min(i / N, clamp(p));
    const x = (1 - q) * (1 - q) * a.x + 2 * (1 - q) * q * cx + q * q * b.x;
    const y = (1 - q) * (1 - q) * a.y + 2 * (1 - q) * q * cy + q * q * b.y;
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
    px = lx; py = ly; lx = x; ly = y;
  }
  ctx.stroke();
  if (dot) {
    ctx.beginPath();
    ctx.arc(a.x, a.y, width * 2.2, 0, Math.PI * 2);
    ctx.fill();
  }
  if (head && p > 0.05) arrowHead(ctx, lx, ly, Math.atan2(ly - py, lx - px), width * 4.5);
  ctx.restore();
}

export function arrowHead(ctx, x, y, ang, s) {
  ctx.beginPath();
  ctx.moveTo(x + Math.cos(ang) * 1.5, y + Math.sin(ang) * 1.5);
  ctx.lineTo(x - Math.cos(ang - 0.45) * s, y - Math.sin(ang - 0.45) * s);
  ctx.lineTo(x - Math.cos(ang + 0.45) * s, y - Math.sin(ang + 0.45) * s);
  ctx.closePath();
  ctx.fill();
}

/** A polyline drawn up to p of its length, with rounded joins and an optional arrowhead. */
export function strokePath(ctx, pts, p, { color, width = 2, head = true, dot = false } = {}) {
  const seg = [];
  let total = 0;
  for (let i = 1; i < pts.length; i++) {
    const l = Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]);
    seg.push(l);
    total += l;
  }
  if (total <= 0.5 || p <= 0) return;
  let left = total * clamp(p);
  ctx.strokeStyle = color;
  ctx.fillStyle = color;
  ctx.lineWidth = width;
  ctx.lineJoin = 'round';
  ctx.lineCap = 'round';
  ctx.beginPath();
  ctx.moveTo(pts[0][0], pts[0][1]);
  let ex = pts[0][0], ey = pts[0][1], dx = 1, dy = 0;
  for (let i = 1; i < pts.length && left > 0; i++) {
    const l = seg[i - 1];
    const q = Math.min(1, left / (l || 1));
    ex = pts[i - 1][0] + (pts[i][0] - pts[i - 1][0]) * q;
    ey = pts[i - 1][1] + (pts[i][1] - pts[i - 1][1]) * q;
    if (l > 0) { dx = (pts[i][0] - pts[i - 1][0]) / l; dy = (pts[i][1] - pts[i - 1][1]) / l; }
    ctx.lineTo(ex, ey);
    left -= l;
  }
  ctx.stroke();
  if (dot) {
    ctx.beginPath();
    ctx.arc(pts[0][0], pts[0][1], width * 2, 0, Math.PI * 2);
    ctx.fill();
  }
  if (head && p > 0.12) arrowHead(ctx, ex, ey, Math.atan2(dy, dx), width * 4.2);
}

/** A number card: a big number, one line under it, and a quiet source line. Returns the number's width. */
export function numberCard(ctx, t, start, { x = 140, y = 600, value, text, note: quietNote, color = C.ink, sub = C.ink2, quiet = C.ink3, font: F = TY.num, out = Infinity, align = 'left', maxWidth = 1500 } = {}) {
  if (t < start) return 0;
  const L = layoutText(value, F, { around: AROUND });
  reveal(ctx, L, x, y, t, {
    unit: 'glyph', start, stagger: 0.035, dur: 0.9, align, color, mask: true, rise: 1.0, tracking: TRACK.num,
    out: out < Infinity ? { start: out, stagger: 0.02, dur: 0.5 } : undefined,
  });
  const gy = y + F.size * 0.3;
  let ny = gy + 40;
  if (text) {
    const T1 = layoutText(text, TY.lead, { maxWidth, lineHeight: 1.25 });
    reveal(ctx, T1, x, gy + 40, t, { unit: 'line', start: start + 0.35, dur: 0.8, stagger: 0.08, align, color: sub, mask: true,
      out: out < Infinity ? { start: out, dur: 0.45 } : undefined });
    ny = gy + 40 + T1.height + 62;
  }
  if (quietNote) {
    const a = progress(t, start + 0.75, 0.6, ease.out) * (out < Infinity ? 1 - progress(t, out, 0.4) : 1);
    kicker(ctx, quietNote, x, ny, { color: quiet, align, alpha: a, tracking: 0.08 });
  }
  return L.width;
}

/** Ease a camera track: keyframes of [t, {x, y, s}, ease?] (focus point in page space, scale). */
export function cameraTrack(frames) {
  return (t) => {
    if (t <= frames[0][0]) return frames[0][1];
    for (let i = 1; i < frames.length; i++) {
      const [t1, b, fn] = frames[i];
      const [t0, a] = frames[i - 1];
      if (t <= t1) {
        const p = (fn || ease.inOut)(clamp((t - t0) / (t1 - t0)));
        // scale moves in log space, so a zoom feels even
        const s = Math.exp(lerp(Math.log(a.s), Math.log(b.s), p));
        // the focus point follows the scale, so the point under the frame centre does not drift
        // sideways while the zoom runs (a straight dolly in the zoom's own space)
        const k = Math.abs(b.s - a.s) > 1e-6 ? (1 / s - 1 / a.s) / (1 / b.s - 1 / a.s) : p;
        return { x: lerp(a.x, b.x, k), y: lerp(a.y, b.y, k), s };
      }
    }
    return frames[frames.length - 1][1];
  };
}

/** A camera key that puts page point (px, py) at screen point (qx, qy) at scale s. */
export const frameAt = (px, py, qx, qy, s) => ({ x: px + (960 - qx) / s, y: py + (540 - qy) / s, s });

/** Apply a camera: the focus point (x, y) in page space lands on screen at (960, 540), scaled by s. */
export function applyCamera(ctx, cam) {
  ctx.translate(960 - cam.x * cam.s, 540 - cam.y * cam.s);
  ctx.scale(cam.s, cam.s);
}

/** Screen position of a page point under a camera. */
export const toScreen = (cam, x, y) => ({ x: 960 + (x - cam.x) * cam.s, y: 540 + (y - cam.y) * cam.s });

/** A slow push for a held frame: scale 1 → 1 + amount around (cx, cy) over [t0, t1]. */
export function drift(ctx, t, t0, t1, amount = 0.03, cx = 960, cy = 540) {
  const s = 1 + amount * ease.inOutSine(clamp((t - t0) / (t1 - t0)));
  ctx.translate(cx, cy);
  ctx.scale(s, s);
  ctx.translate(-cx, -cy);
}

// ----------------------------------------------------------------------------- the hero script page
// The hero script's V2.6 output, set in one column. The same page is the target of the opening's
// bytes, the ground of the names shot, and the source of the loop.

export const PAGE = { size: 26, lineHeight: 1.5 };

/** Build the token model of the hero page from the named output and its nameless twin. */
export function buildHero(D) {
  const named = layoutCode(D.hero.output);
  const plain = layoutCode(D.hero.output_nameless);
  if (named.tokens.length !== plain.tokens.length) throw new Error('hero: nameless output does not line up');
  const { cw, lh, baseline } = codeMetrics(PAGE.size, PAGE.lineHeight);
  const toks = named.tokens.map((tb, k) => {
    const ta = plain.tokens[k];
    const isVar = ta.text !== tb.text;
    return {
      k, kind: tb.k, line: tb.line, colB: tb.col, colA: ta.col, text: tb.text, plain: ta.text,
      v: isVar ? (+ta.text.slice(1) - 1) : -1,
    };
  });
  // var tokens before each token on its line (for the reflow)
  const byLine = new Map();
  for (const tk of toks) {
    if (!byLine.has(tk.line)) byLine.set(tk.line, []);
    byLine.get(tk.line).push(tk);
  }
  for (const list of byLine.values()) {
    const vars = [];
    for (const tk of list) {
      tk.before = vars.slice();
      if (tk.v >= 0) vars.push(tk);
    }
  }
  const tokAt = (line, col) => toks.find((tk) => tk.line === line && tk.colB === col) || null;
  const width = named.cols * cw, height = named.lineCount * lh;
  return {
    named, plain, toks, byLine, nVars: D.hero.declared.length, cw, lh, baseline, tokAt, width, height,
    lineY: (l) => l * lh,
    lineW: (l) => named.lineCols[l] * cw,
  };
}

/** Column (in cells, fractional) of a token under per-variable naming progress `named(v)`. */
export function tokenCol(tk, named) {
  let c = tk.colB;
  for (const vt of tk.before) {
    const p = named(vt.v);
    if (p < 1) c -= (vt.text.length - vt.plain.length) * (1 - p);
  }
  return c;
}

/** Page position (top-left of the cell) of a token. */
export function tokenXY(H, tk, named = () => 1) {
  return { x: tokenCol(tk, named) * H.cw, y: H.lineY(tk.line) };
}

// accent mixes, cached on a 1/48 grid of the amount (drawHero asks for one per lit token per frame)
const MIX = new Map();
export function mixQ(a, b, p) {
  const q = Math.round(clamp(p) * 48);
  const key = a + b + q;
  let v = MIX.get(key);
  if (v === undefined) {
    v = mix(a, b, q / 48);
    if (MIX.size > 4000) MIX.clear();
    MIX.set(key, v);
  }
  return v;
}

/**
 * Draw the hero page in page space. Options:
 *   named(v)      0..1 per variable (1 = its name, 0 = its placeholder v1, v2 ...)
 *   accent(tk)    0..1 accent mix per token
 *   lineAlpha(l)  0..1 per line
 *   lines         [first, last] to draw
 *   palette       'paper' | 'night'
 */
export function drawHero(ctx, H, o = {}) {
  const pal = codePalettes[o.palette || 'paper'];
  const named = o.named || (() => 1);
  const accent = o.accent;
  const lineAlpha = o.lineAlpha;
  const [first, last] = o.lines || [0, H.named.lineCount - 1];
  const { f: F } = codeMetrics(PAGE.size, PAGE.lineHeight);
  ctx.save();
  setFont(ctx, F);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  const base = ctx.globalAlpha;
  const ghost = o.palette === 'night' ? '#8a877e' : '#8b8980';
  const hl = o.palette === 'night' ? C.signal : C.signalDeep;
  for (const tk of H.toks) {
    if (tk.line < first || tk.line > last) continue;
    const la = lineAlpha ? lineAlpha(tk.line) : 1;
    if (la <= 0.003) continue;
    const x = tokenCol(tk, named) * H.cw;
    const y = H.lineY(tk.line) + H.baseline;
    const color = pal[tk.kind] || pal.id;
    const a = accent ? accent(tk) : 0;
    if (tk.v >= 0) {
      const p = named(tk.v);
      const mid = p > 0 && p < 1;
      if (mid) {
        // the slot is as wide as its text at this moment: both words are cut to it
        ctx.save();
        ctx.beginPath();
        ctx.rect(x - 1, y - H.lh, lerp(tk.plain.length, tk.text.length, p) * H.cw + 2, H.lh * 1.5);
        ctx.clip();
      }
      // the old word goes, the slot changes width, the new word comes: never both at once
      if (p < 0.5) {
        ctx.globalAlpha = base * la * clamp((0.5 - p) * 2.6);
        ctx.fillStyle = a > 0 ? mixQ(ghost, hl, a * 0.6) : ghost;
        ctx.fillText(tk.plain, x, y);
      }
      if (p > 0.5) {
        ctx.globalAlpha = base * la * clamp((p - 0.5) * 2.6);
        ctx.fillStyle = a > 0 ? mixQ(color, hl, a) : color;
        ctx.fillText(tk.text, x, y);
      }
      if (mid) ctx.restore();
    } else {
      ctx.globalAlpha = base * la;
      ctx.fillStyle = a > 0 ? mixQ(color, hl, a) : color;
      ctx.fillText(tk.text, x, y);
    }
  }
  ctx.restore();
}

// ------------------------------------------------------------------------------------ the read head

/** A horizontal read head at screen y: the accent hairline with a soft glow on the side it has read. */
export function headH(ctx, y, alpha = 1, { glow = 0.16, side = -1, width = 2.5 } = {}) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  const h = 34;
  const g = ctx.createLinearGradient(0, y + side * h, 0, y);
  g.addColorStop(0, rgba(C.signal, 0));
  g.addColorStop(1, rgba(C.signal, glow));
  ctx.fillStyle = g;
  ctx.fillRect(-4, Math.min(y, y + side * h), 1928, h);
  ctx.fillStyle = C.signal;
  ctx.fillRect(-4, y - width / 2, 1928, width);
  ctx.restore();
}

/** A vertical read head at screen x; `side` is the side it has read (-1 left, 1 right). */
export function headV(ctx, x, alpha = 1, { glow = 0.16, side = -1, width = 2.5, y0 = -4, y1 = 1084 } = {}) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  const w = 34;
  const g = ctx.createLinearGradient(x + side * w, 0, x, 0);
  g.addColorStop(0, rgba(C.signal, 0));
  g.addColorStop(1, rgba(C.signal, glow));
  ctx.fillStyle = g;
  ctx.fillRect(Math.min(x, x + side * w), y0, w, y1 - y0);
  ctx.fillStyle = C.signal;
  ctx.fillRect(x - width / 2, y0, width, y1 - y0);
  ctx.restore();
}

export { ease, clamp, lerp, progress, envelope, layoutText, drawText, reveal, measure, metrics, setFont, rgba, mix, pixel };
