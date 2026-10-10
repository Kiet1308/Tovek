// Shared ground for the history film: the timeline, the look of each era, the film's data once it is
// prepared, and the drawing helpers every act uses. Everything here is either a constant, a cache
// keyed by its inputs, or a pure function of its arguments, so any frame can be drawn on its own.

import {
  ease, clamp, lerp, envelope, smoothstep,
  font, layoutText, drawText, measure, metrics, setFont,
  drawCounter, drawOdometer, formatNumber,
  layoutCode, codeMetrics, drawCode, roundRect, withCamera,
  rgba, mix, pixel,
} from '../../engine/index.js';

// ------------------------------------------------------------------------------------------ time
// Every beat of the film, in seconds. Acts read only these, so the cut can be re-timed here.

export const T = {
  // I. one script, as bytes: the dump, one byte, its opcode, the instruction, the line
  hex0: 1.0, focus: 4.2, dive0: 5.5, dive1: 8.0, opcode: 8.25, instr: 9.75, listing: 11.25, line: 12.1, open1: 13.5,
  // medal: the line becomes medal's, the camera pulls back, the title, the people, the goto
  medal0: 13.5, lineMorph0: 13.7, lineMorph1: 15.1, pull0: 15.0, pull1: 16.6, medalTitle: 16.2, credit: 19.6, gotoPush: 22.6, medal1: 25.4,
  // the date: medal's first commit to Tovek's first beta, to the minute
  date0: 25.4, date1: 29.0,
  // beta 0.1 in the terminal
  b01: 29.0, b01Morph0: 30.2, b01Morph1: 33.6, b011: 35.6,
  // four days, eight betas: title, slams, then a breath on the second function
  mont0: 35.6, slams0: 37.0, mont1: 45.4,
  breath0: 45.4, breath1: 55.0,
  // July: 0.7, the goto arcs, 0.8 snaps them into a loop, 0.9
  r07: 55.0, r07Morph0: 56.0, r07Morph1: 58.4, arcs0: 59.6, nogoto: 63.8, snap0: 65.4, snap1: 67.6, r08wide: 68.0, r09: 73.0, july1: 76.2,
  // the release pages
  v2: 76.2, v2settle: 77.0, v2Morph0: 77.8, v2Morph1: 81.6, v21: 86.0, v211: 92.4, v25: 97.6, fuzz0: 99.6, ctx25: 104.0, v251: 107.4,
  // V2.6: the flood, the copies come home, the constants, the numbers
  flood: 111.4, title26: 112.0, homing0: 115.0, homing1: 131.0, consts0: 131.0, nums: 137.2,
  // the end
  orig: 143.2, end: 151.0, duration: 160.0,
};

// --------------------------------------------------------------------------------------- the look

export const FAM = {
  inter: '"Inter", "Geist", system-ui, sans-serif',
  hanken: '"Hanken Grotesk", "Geist", system-ui, sans-serif',
  gsans: '"Google Sans", "Geist", system-ui, sans-serif',
};
// The typefaces of the V2, V2.1 and V2.5 release pages, for their eras (all on Google Fonts).
export const ERA_FONTS_CSS = 'https://fonts.googleapis.com/css2?family=Inter:wght@400..700&family=Hanken+Grotesk:wght@400..700&family=Google+Sans:wght@400..700&display=swap';

export const ERA = {
  dark: { surface: '#0d0d0c', ink: '#f2f0eb', ink2: '#a9a69c', ink3: '#8b8980', faint: '#3a3935', accent: '#ff4d1a' },
  v2: { surface: '#ffffff', ink: '#0b0b0b', ink2: '#505050', ink3: '#8c8c8c', faint: '#e6e6e6', accent: '#0b0b0b', marker: '#c6f135' },
  v21: { surface: '#faf9f5', ink: '#141413', ink2: '#5e5d59', ink3: '#87867f', faint: '#e5e4dd', accent: '#b0512f', fill: '#d97757' },
  v25: { surface: '#0b2a6b', ink: '#ffffff', ink2: '#bcd0f5', ink3: '#86a2d8', faint: '#24447f', accent: '#9dd2ff' },
  v26: { surface: '#f2f0eb', ink: '#121210', ink2: '#4a4943', ink3: '#8b8980', faint: '#dedbd3', accent: '#d63a0c', signal: '#ff4d1a' },
  end: { surface: '#0d0d0c', ink: '#f2f0eb', ink2: '#a9a69c', ink3: '#8b8980', faint: '#3a3935', accent: '#ff4d1a' },
};

export const f = (size, family, weight = 400, stretch = 100) => font(size, { family, weight, stretch });

// Display tracking: large type is set a touch tight, never so tight that glyphs touch.
export const TRACK = { hero: -0.008, big: -0.01, title: -0.012 };
// Bricolage's display cut sets a period tight against its digits ("2.6"): open it a little.
export const AROUND = { '.': 0.032 };

export const TY = {
  dark: {
    hero: f(196, 'display', 600, 87.5), sub: f(34, 'text', 400), authors: f(44, 'text', 500), credit: f(30, 'text', 400), credit2: f(30, 'text', 500),
    label: f(17, 'mono', 500), prompt: f(21, 'mono', 400), title: f(96, 'mono', 600), mdate: f(40, 'mono', 400),
    head: f(27, 'mono', 500), chip: f(17, 'mono', 500), lab: f(18, 'mono', 400), val: f(19, 'mono', 500),
    hex: f(17, 'mono', 400), url: f(17, 'mono', 400), slam: f(520, 'mono', 600), slamBeta: f(44, 'mono', 500),
    clock: f(64, 'mono', 500), clockSmall: f(19, 'mono', 500), stamp: f(150, 'mono', 500), stampLab: f(20, 'mono', 500),
    byte: f(600, 'mono', 500), opcode: f(176, 'mono', 600), instr: f(56, 'mono', 400), bytes: f(64, 'mono', 400),
    small: f(18, 'mono', 500), line: f(40, 'mono', 400), nogoto: f(176, 'mono', 600), big: f(84, 'mono', 600),
    title2: f(150, 'display', 600, 87.5),
  },
  v2: {
    meta: f(22, FAM.inter, 500), tovek: f(44, FAM.inter, 600), title: f(250, FAM.inter, 700), head: f(52, FAM.inter, 500),
    giant: f(1000, FAM.inter, 700), num: f(76, FAM.inter, 600), unit: f(24, FAM.inter, 400), lab: f(18, FAM.inter, 400), val: f(19, FAM.inter, 600),
  },
  v21: {
    meta: f(17, 'mono', 500), tovek: f(44, FAM.hanken, 600), title: f(200, FAM.hanken, 600), head: f(64, FAM.hanken, 500),
    num: f(96, FAM.hanken, 600), unit: f(24, FAM.hanken, 400), lab: f(22, FAM.hanken, 500), big: f(64, FAM.hanken, 600),
  },
  v25: {
    meta: f(24, FAM.gsans, 400), tovek: f(44, FAM.gsans, 500), title: f(200, FAM.gsans, 500), head: f(50, FAM.gsans, 400),
    num: f(200, FAM.gsans, 500), unit: f(24, FAM.gsans, 400), lab: f(18, FAM.gsans, 400), val: f(19, FAM.gsans, 500),
    mid: f(120, FAM.gsans, 500),
  },
  v26: {
    meta: f(17, 'mono', 500), tovek: f(56, 'display', 600, 87.5), title: f(300, 'display', 650, 87.5), head: f(46, 'text', 500),
    num: f(220, 'display', 600, 87.5), unit: f(26, 'text', 400), lab: f(18, 'text', 400), val: f(19, 'mono', 500),
    small: f(17, 'mono', 500), corner: f(30, 'display', 600, 87.5), cornerHead: f(19, 'text', 500),
    stat: f(50, 'display', 600, 87.5), side: f(17, 'mono', 500), tally: f(16, 'mono', 500),
  },
  end: {
    num: f(150, 'display', 600, 87.5), unit: f(26, 'text', 400), card: f(160, 'display', 650, 87.5),
    credit: f(19, 'mono', 500), url: f(24, 'mono', 400),
  },
};

// Code: one grid everywhere. Read shots draw at scale 1 (22 px); wide shots scale it down.
export const CODE = { size: 22, lineHeight: 1.55 };
export const COL = 120;            // text column left edge
export const COLW = 600;           // text column width
export const BOX = { x: 820, y: 112, w: 980, h: 840 };   // code in a split shot
export const CLIP = { x: 740, y: 80, w: 1140, h: 900 };  // where that code may draw

// --------------------------------------------------------------------------------------- state
// Filled once by prepare(); render() only reads it.

export const S = {
  D: null,          // story.json
  ST: null,         // stage by tag (+ 'last' for V2.6)
  ORDER: null,      // stages in order
  V26: null,        // the V2.6 stage, whatever its tag
  CM: null,         // code metrics at CODE.size
  PAL: null,        // code palette per era
  RIDX: null,       // stage -> release number (1..17)
  REPO: '',         // the Tovek repository, read from the release binaries' sources
  N: null,          // numbers parsed from the release notes
};

// ------------------------------------------------------------------------------------- helpers

const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
const WEEKDAYS = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'];
const WORDS = ['zero', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight', 'nine', 'ten', 'eleven', 'twelve',
  'thirteen', 'fourteen', 'fifteen', 'sixteen', 'seventeen', 'eighteen', 'nineteen', 'twenty'];
export const word = (n) => WORDS[n] ?? String(n);
export const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1);
export const lc = (s) => s.charAt(0).toLowerCase() + s.slice(1);
const ymd = (iso) => iso.slice(0, 10).split('-').map(Number);
export const longDate = (iso) => { const [y, m, d] = ymd(iso); return `${d} ${MONTHS[m - 1]} ${y}`; };
export const usDate = (iso) => { const [y, m, d] = ymd(iso); return `${MONTHS[m - 1]} ${d}, ${y}`; };
export const monthYear = (iso) => { const [y, m] = ymd(iso); return `${MONTHS[m - 1]} ${y}`; };
export const shortMonth = (iso) => MONTHS[ymd(iso)[1] - 1].slice(0, 3).toUpperCase();
export const weekday = (iso) => WEEKDAYS[new Date(iso.slice(0, 10) + 'T00:00:00Z').getUTCDay()];
export const num = (s) => Number(String(s).replace(/,/g, ''));
export const stripDot = (s) => s.replace(/\.$/, '');
/** '2026-06-19T17:51:41Z' -> '17:51' (UTC, as GitHub records it). */
export const hhmm = (iso) => iso.slice(11, 16);
export const stamp = (iso) => `${iso.slice(0, 10)} ${hhmm(iso)}`;

export function hexOf(css) {
  if (css.startsWith('#')) return css;
  const m = css.match(/\d+/g).map(Number);
  return '#' + m.slice(0, 3).map((v) => v.toString(16).padStart(2, '0')).join('');
}

/** A code palette for an era: the night palette's tone steps, mixed between surface and ink. */
export function makePalette(e, wash) {
  const W = { fn: 1, id: 0.94, glob: 0.88, prop: 0.82, num: 0.8, const: 0.74, str: 0.7, kw: 0.62, op: 0.56, punct: 0.5, com: 0.46 };
  const pal = {};
  for (const k in W) pal[k] = hexOf(mix(e.surface, e.ink, W[k]));
  pal.accent = e.accent;
  pal.wash = wash;
  pal.gutter = hexOf(mix(e.surface, e.ink, 0.3));
  pal.base = e.surface;
  return pal;
}

export function surface(ctx, color) {
  ctx.fillStyle = color;
  ctx.fillRect(-4, -4, 1928, 1088);
}

export function hairline(ctx, x, y, w, color, alpha = 1) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.fillStyle = color;
  ctx.fillRect(x, y, w, Math.max(1, pixel(ctx)));
  ctx.restore();
}

export function vline(ctx, x, y, h, color, alpha = 1) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.fillStyle = color;
  ctx.fillRect(x, y, Math.max(1, pixel(ctx)), h);
  ctx.restore();
}

/** Clip to a rectangle whose edges sit on whole device pixels (a scissor, never a coverage mask). */
export function clipRect(ctx, r, fn) {
  const px = pixel(ctx);
  const sn = (v) => Math.round(v / px) * px;
  const x0 = sn(r.x), y0 = sn(r.y), x1 = sn(r.x + r.w), y1 = sn(r.y + r.h);
  ctx.save();
  ctx.beginPath();
  ctx.rect(x0, y0, x1 - x0, y1 - y0);
  ctx.clip();
  fn();
  ctx.restore();
}

/** Alpha for a block: skips the call when it is invisible. */
export function faded(ctx, a, fn) {
  if (a <= 0) return;
  ctx.save();
  ctx.globalAlpha *= Math.min(1, a);
  fn();
  ctx.restore();
}

/** Characters that change roll vertically from `from` to `to` (monospace, left aligned). */
export function rollText(ctx, from, to, x, y, F, t, start, { dur = 0.55, stagger = 0.045, color = '#fff', alpha = 1 } = {}) {
  if (alpha <= 0) return;
  const cw = measure('0', F);
  const m = metrics(F);
  const n = Math.max(from.length, to.length);
  const rise = (m.ascent + m.descent) * 0.9;
  ctx.save();
  setFont(ctx, F);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  ctx.fillStyle = color;
  const base = ctx.globalAlpha * alpha;
  let k = 0;
  for (let i = 0; i < n; i++) {
    const a = from[i] || ' ', b = to[i] || ' ';
    const px = x + i * cw;
    if (a === b) {
      if (b !== ' ') { ctx.globalAlpha = base; ctx.fillText(b, px, y); }
      continue;
    }
    const p = ease.out(clamp((t - start - k * stagger) / dur));
    k++;
    ctx.save();
    ctx.beginPath();
    ctx.rect(px - 2, y - m.ascent * 1.05, cw + 4, (m.ascent + m.descent) * 1.1);
    ctx.clip();
    if (p < 1 && a !== ' ') { ctx.globalAlpha = base * (1 - p); ctx.fillText(a, px, y - p * rise); }
    if (p > 0 && b !== ' ') { ctx.globalAlpha = base * p; ctx.fillText(b, px, y + (1 - p) * rise); }
    ctx.restore();
  }
  ctx.restore();
}

/**
 * A ticker: a monospace string whose digits spin like an odometer from `from` to `to` (same length,
 * same separators), the right-hand columns turning more, settling left to right. Non-digits that
 * differ cross-fade. Used for timestamps racing between releases.
 */
export function ticker(ctx, from, to, x, y, F, t, start, dur, { color = '#fff', alpha = 1, turns = 1, align = 'left', fn = ease.inOut, dimFrom = null } = {}) {
  if (alpha <= 0) return;
  const cw = measure('0', F);
  const m = metrics(F);
  const n = Math.max(from.length, to.length);
  const capH = m.capHeight || F.size * 0.72;
  const pad = capH * 0.2, slot = capH + pad * 2;
  const ox = x - (align === 'center' ? (n * cw) / 2 : align === 'right' ? n * cw : 0);
  ctx.save();
  setFont(ctx, F);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  ctx.fillStyle = color;
  const base = ctx.globalAlpha * alpha;
  let digitIndex = 0;
  const digitsTotal = [...to].filter((c) => c >= '0' && c <= '9').length;
  for (let i = 0; i < n; i++) {
    const a = from[i] || ' ', b = to[i] || ' ';
    const px = ox + i * cw;
    const isDigit = a >= '0' && a <= '9' && b >= '0' && b <= '9';
    if (!isDigit) {
      if (a === b) { if (b !== ' ') { ctx.globalAlpha = base; ctx.fillText(b, px, y); } continue; }
      const p = fn(clamp((t - start) / dur));
      if (a !== ' ') { ctx.globalAlpha = base * (1 - p); ctx.fillText(a, px, y); }
      if (b !== ' ') { ctx.globalAlpha = base * p; ctx.fillText(b, px, y); }
      continue;
    }
    const place = digitsTotal - 1 - digitIndex++;
    const da = +a, db = +b;
    const extra = da === db ? 0 : Math.max(0, turns - Math.floor(place / 2));
    const steps = ((db - da + 10) % 10) + 10 * extra;
    const colDur = dur * (0.55 + 0.45 * (1 - place / Math.max(1, digitsTotal)));
    const p = fn(clamp((t - start) / colDur));
    const pos = da + steps * p;
    const whole = Math.floor(pos), frac = pos - whole;
    ctx.save();
    ctx.beginPath();
    ctx.rect(px - 2, y - capH - pad, cw + 4, slot);
    ctx.clip();
    for (let k = 0; k <= 1; k++) {
      const dy = (k - frac) * slot;
      const near = Math.pow(1 - Math.min(1, Math.abs(dy) / slot), 2.2);
      if (near <= 0.003) continue;
      ctx.globalAlpha = base * near;
      ctx.fillText(String((((whole + k) % 10) + 10) % 10), px, y - dy);
    }
    ctx.restore();
  }
  ctx.restore();
}

/** Parse "A to B label (paren)" or "B label (paren)" from a release-note number. */
export function parseNumber(text) {
  let m = text.match(/^([\d,.]+) to ([\d,.]+) (.+?)(?: \((.+)\))?$/);
  if (m) return { from: num(m[1]), to: num(m[2]), label: m[3], paren: m[4] || '' };
  m = text.match(/^([\d,.]+) (.+?)(?: \((.+)\))?$/);
  if (m) return { to: num(m[1]), label: m[2], paren: m[3] || '' };
  return null;
}

// ------------------------------------------------------------------------------- code cameras

export const camAt = (top, s = 1, box = BOX) => ({ s, x: box.x, y: box.y - top * S.CM.lh * s });
export function camFit(first, last, box, maxS = 1) {
  const n = last - first + 1;
  const s = Math.min(maxS, box.h / (n * S.CM.lh));
  return { s, x: box.x, y: box.y + (box.h - n * S.CM.lh * s) / 2 - first * S.CM.lh * s };
}
export const lerpCam = (a, b, p) => ({ s: lerp(a.s, b.s, p), x: lerp(a.x, b.x, p), y: lerp(a.y, b.y, p) });
/**
 * Interpolate between two cameras so the zoom feels constant: scale moves in log space while the
 * anchor point P (design units) slides on screen from where A shows it to where B shows it.
 * Without P, the anchor is the point B shows at the frame centre.
 */
export function zoomCam(a, b, p, P = null, g = p) {
  const s = Math.exp(lerp(Math.log(a.s), Math.log(b.s), p));
  const px = P ? P.x : (960 - b.x) / b.s, py = P ? P.y : (540 - b.y) / b.s;
  const qx = lerp(a.s * px + a.x, b.s * px + b.x, g), qy = lerp(a.s * py + a.y, b.s * py + b.y, g);
  return { s, x: qx - s * px, y: qy - s * py };
}

/** A camera that moves through keyframes [[t, cam, ease?], ...]. */
export function camTrack(frames) {
  return (t) => {
    if (t <= frames[0][0]) return frames[0][1];
    for (let i = 1; i < frames.length; i++) {
      const [t1, c1, fn = ease.inOut] = frames[i];
      if (t <= t1) {
        const [t0, c0] = frames[i - 1];
        return lerpCam(c0, c1, fn(clamp((t - t0) / (t1 - t0 || 1))));
      }
    }
    return frames[frames.length - 1][1];
  };
}

/**
 * A slow camera drift for code-as-landscape shots: a push-in about (cx, cy) and a sideways slide.
 * `depth` < 1 moves a layer less (the type layer), so code and type separate in parallax.
 */
export function drift(cam, t, t0, t1, { zoom = 0.05, dx = -24, dy = -10, cx = 960, cy = 540, depth = 1 } = {}) {
  const e = ease.inOutSine(clamp((t - t0) / (t1 - t0)));
  const k = 1 + zoom * depth * e;
  return { s: cam.s * k, x: cx + (cam.x - cx) * k + dx * depth * e, y: cy + (cam.y - cy) * k + dy * depth * e };
}
/** The same drift as a translation for a type layer (parallax). */
export function driftOffset(t, t0, t1, { dx = -24, dy = -10, depth = 0.35 } = {}) {
  const e = ease.inOutSine(clamp((t - t0) / (t1 - t0)));
  return [dx * depth * e, dy * depth * e];
}

export function visibleLines(L, cam, clip) {
  const step = S.CM.lh * cam.s;
  const first = Math.max(0, Math.floor((clip.y - cam.y) / step) - 1);
  const last = Math.min(L.lineCount - 1, Math.ceil((clip.y + clip.h - cam.y) / step) + 1);
  return [first, last];
}

/** Draw a code layout through a camera, culled to the clip box. */
export function codeAt(ctx, L, cam, pal, { clip = CLIP, highlight = null, mix: k = 1, lineAlpha, washAlpha = 0, washColor } = {}) {
  withCamera(ctx, cam, () => {
    const [first, last] = visibleLines(L, cam, clip);
    if (highlight && washAlpha > 0) drawWashes(ctx, washRuns(L, highlight), washAlpha, washColor || pal.wash);
    drawCode(ctx, L, {
      size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, lines: [first, last], y: first * S.CM.lh,
      highlight: highlight && k > 0 ? highlight : null, highlightMix: k, lineAlpha,
    });
  });
}

/** Soft top and bottom edges where code runs under the edge of its area. */
export function edgeFade(ctx, clip, color, h = 46) {
  for (const [y0, y1] of [[clip.y, clip.y + h], [clip.y + clip.h, clip.y + clip.h - h]]) {
    const g = ctx.createLinearGradient(0, y0, 0, y1);
    g.addColorStop(0, rgba(color, 1));
    g.addColorStop(1, rgba(color, 0));
    ctx.fillStyle = g;
    ctx.fillRect(clip.x - 2, Math.min(y0, y1), clip.w + 4, h);
  }
}

// Washes behind a set of tokens, merged into one rounded run per line.
const washCache = new WeakMap();
export function washRuns(L, set) {
  let byL = washCache.get(set);
  if (byL) return byL;
  const parts = [];
  for (const i of set) for (const p of L.tokens[i].parts) parts.push({ line: p.line, c0: p.col, c1: p.col + p.text.length });
  parts.sort((a, b) => a.line - b.line || a.c0 - b.c0);
  byL = [];
  for (const p of parts) {
    const last = byL[byL.length - 1];
    if (last && last.line === p.line && p.c0 - last.c1 <= 1) last.c1 = Math.max(last.c1, p.c1);
    else byL.push({ ...p });
  }
  washCache.set(set, byL);
  return byL;
}

export function drawWashes(ctx, runs, alpha, color, sweep = 1) {
  if (alpha <= 0 || !runs.length) return;
  const { cw, lh } = S.CM;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.fillStyle = color;
  for (const r of runs) {
    const w = (r.c1 - r.c0) * cw * sweep + cw * 0.5;
    roundRect(ctx, r.c0 * cw - cw * 0.25, r.line * lh + lh * 0.13, w, lh * 0.74, lh * 0.14);
  }
  ctx.restore();
}

// Generated names (v, v2, p, p3, v_u_2) are never a recovery, so they never take the accent.
const GENERATED = /^(?:[vp](?:_u)?_?\d*|_)$/;
const NAME_KINDS = new Set(['id', 'fn', 'prop', 'glob']);
/**
 * The inserted tokens of a morph that show what the release recovered. `focus`: 'names' (new real
 * names only), 'all' (every new token except generated names), or 'none'.
 */
export function recovered(plan, focus) {
  const out = new Set();
  if (focus === 'none') return out;
  const toks = plan.b.tokens;
  for (const i of plan.inserted) {
    const tk = toks[i];
    if (GENERATED.test(tk.text)) continue;
    // goto and its labels are jumps, not recoveries (Luau has no goto keyword, so they lex as names)
    if (tk.text === 'goto' || toks[i - 1]?.text === 'goto' || toks[i - 1]?.text === '::' || tk.text === '::') continue;
    if (focus === 'names' && !NAME_KINDS.has(tk.k)) continue;
    out.add(i);
  }
  return out;
}

/** The highlight of a morph's new tokens after it lands: in, hold, then settle. */
export const settle = (t1, hold = 1.8, out = 1.4) => (t) => (t < t1 ? 0 : 1 - clamp((t - t1 - hold) / out));
export const washIn = (t1, hold = 1.8, out = 1.4) => (t) => (t < t1 ? 0 : Math.min(ease.out(clamp((t - t1) / 0.45)), 1 - clamp((t - t1 - hold) / out)));

// ---------------------------------------------------------------------------- flow arrows
// The jumps of the code drawn in its own indentation: goto arcs (dashed) and loops (solid).

const flowCache = new Map();
const indentOf = (s) => { let c = 0; for (const ch of s) { if (ch === '\t') c += 4; else if (ch === ' ') c++; else break; } return c; };
export function flowOf(L) {
  let F = flowCache.get(L.src);
  if (F) return F;
  const lines = L.lines;
  const labels = new Map();
  lines.forEach((s, i) => { const m = s.match(/::(\w+)::/); if (m) labels.set(m[1], i); });
  const gotos = [];
  lines.forEach((s, i) => {
    const m = s.match(/\bgoto (\w+)/);
    if (!m || !labels.has(m[1])) return;
    const to = labels.get(m[1]);
    let cMin = Math.min(indentOf(s), indentOf(lines[to]));
    for (let j = Math.min(i, to) + 1; j < Math.max(i, to); j++) if (lines[j].trim()) cMin = Math.min(cMin, indentOf(lines[j]));
    const endCol = indentOf(s) + s.trim().length;
    const labelEnd = indentOf(lines[to]) + lines[to].trim().length;
    gotos.push({ from: i, to, c0: indentOf(s), c1: indentOf(lines[to]), cMin, endCol, labelEnd, label: m[1] });
  });
  const loops = [];
  lines.forEach((s, i) => {
    if (!/^\s*(for .* do|while .* do)$/.test(s)) return;
    const ind = indentOf(s);
    for (let j = i + 1; j < lines.length; j++) {
      if (indentOf(lines[j]) === ind && lines[j].trim() === 'end') { loops.push({ from: i, to: j, c: ind, inner: /while true do/.test(s) }); break; }
    }
  });
  F = { gotos, loops };
  flowCache.set(L.src, F);
  return F;
}

export function arrowHead(ctx, x, y, dir, lw, size) {
  const s = size ?? 4 + lw;
  ctx.beginPath();
  ctx.moveTo(x - s * dir, y - s * 0.75);
  ctx.lineTo(x, y);
  ctx.lineTo(x - s * dir, y + s * 0.75);
  ctx.stroke();
}

/** Arrows in the indentation: gotos dashed and flowing, loops solid up the left side. */
export function drawFlow(ctx, L, cam, t, color, { gotoA = 1, loopA = 1, innerA = 1 } = {}) {
  const F = flowOf(L);
  if (!F.gotos.length && !F.loops.length) return;
  const { cw, lh } = S.CM;
  const y = (l) => cam.y + (l + 0.5) * lh * cam.s;
  const x = (c) => cam.x + c * cw * cam.s;
  const lw = Math.max(1.4, 1.6 * Math.min(1, cam.s + 0.2));
  ctx.save();
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  ctx.strokeStyle = color;
  ctx.fillStyle = color;
  ctx.lineWidth = lw;
  for (const lp of F.loops) {
    const a = lp.inner ? loopA * innerA : loopA;
    if (a <= 0) continue;
    ctx.globalAlpha = a * 0.75;
    const x0 = x(lp.c) - 10, bend = 16 + Math.min(18, (lp.to - lp.from) * 0.4);
    ctx.beginPath();
    ctx.moveTo(x0, y(lp.to));
    ctx.bezierCurveTo(x0 - bend, y(lp.to), x0 - bend, y(lp.from), x0, y(lp.from));
    ctx.stroke();
    arrowHead(ctx, x0, y(lp.from), 1, lw);
  }
  if (gotoA > 0) {
    ctx.globalAlpha = gotoA * 0.95;
    ctx.setLineDash([lw * 3, lw * 3]);
    ctx.lineDashOffset = -t * 26;
    for (const g of F.gotos) {
      const xa = x(g.c0) - 8, xb = x(g.c1) - 8;
      const bend = 26 + Math.min(40, Math.abs(g.to - g.from) * 6) * Math.max(0.5, cam.s);
      const near = Math.min(xa, xb);
      const xm = Math.min(near - bend, near + (x(g.cMin) - 12 - near) / 0.75);
      ctx.beginPath();
      ctx.moveTo(xa, y(g.from));
      ctx.bezierCurveTo(xm, y(g.from), xm, y(g.to), xb, y(g.to));
      ctx.stroke();
    }
    ctx.setLineDash([]);
    for (const g of F.gotos) arrowHead(ctx, x(g.c1) - 8, y(g.to), 1, lw);
  }
  ctx.restore();
}

// ----------------------------------------------------------------------------------- the ledger
// A small readout of the file as each release decompiles it. Values roll when they change; a value
// that got better glows in the era's accent for a moment. Regressions are shown as they are.

export const LEDGER = [
  { label: 'non-blank lines', get: (s) => s.nonblank_lines, better: -1 },
  { label: 'goto', get: (s) => s.features.goto, better: -1 },
  { label: 'dispatcher states', get: (s) => s.features.dispatcher_states, better: -1 },
  { label: 'generated names', get: (s) => s.features.generated_names, better: -1 },
  { label: 'rebuilt calls', get: (s) => s.features.rebuilt_calls, better: 1 },
];

/**
 * The ledger at (x, y): `events` are [{ t, stage }] sorted by t; `era` sets the colours.
 * `alpha` lets a shot bring it in and out.
 */
export function drawLedger(ctx, t, events, era, x, y, alpha = 1, { width = 420 } = {}) {
  if (alpha <= 0 || !events.length) return;
  const e = ERA[era], ty = TY[era === 'dark' ? 'dark' : era] || TY.dark;
  let i = 0;
  while (i + 1 < events.length && events[i + 1].t <= t) i++;
  const cur = events[i], prev = events[Math.max(0, i - 1)];
  const gap = 26, xr = x + width;
  const valFont = ty.val || TY.dark.val, labFont = ty.lab || TY.dark.lab;
  ctx.save();
  ctx.globalAlpha *= alpha;
  drawText(ctx, layoutText(S.D.sample.file.toUpperCase(), TY.dark.label), x, y - 8, { color: e.ink2, tracking: 0.1 });
  hairline(ctx, x, y + 4, width, e.ink, 0.18);
  LEDGER.forEach((row, k) => {
    const yy = y + 32 + k * gap;
    const v = row.get(cur.stage), pv = row.get(prev.stage);
    drawText(ctx, layoutText(row.label, labFont), x, yy, { color: e.ink2 });
    const since = t - cur.t;
    const improved = i > 0 && (v - pv) * row.better > 0;
    const glow = improved && era !== 'v2' ? envelope(since, 0, 2.6, 0.3, 1.2) : 0;
    const color = glow > 0 ? mix(e.ink, e.accent, glow) : e.ink;
    if (i > 0 && v !== pv && since < 1.3 && since >= 0) drawOdometer(ctx, t, { from: pv, to: v, start: cur.t, dur: 1.1, x: xr, y: yy, font: valFont, align: 'right', color, turns: 0 });
    else drawCounter(ctx, v, xr, yy, valFont, { align: 'right', color });
  });
  ctx.restore();
}

/** A one-line version of the ledger for wide shots: label value · label value ... */
export function ledgerLine(stage) {
  return LEDGER.map((r) => `${formatNumber(r.get(stage))} ${r.label}`).join('  ·  ');
}

// --------------------------------------------------------------------------------- big type

/** Kicker + value pairs and other small mono labels, tracked out. */
export function label(ctx, text, x, y, color, { alpha = 1, align = 'left', tracking = 0.12, font: F = TY.dark.label } = {}) {
  drawText(ctx, layoutText(text, F), x, y, { color, alpha, align, tracking });
}

/** A smooth 0..1 for a block that comes in at `a`, holds, and leaves at `b`. */
export const span = (t, a, b, fin = 0.5, fout = 0.5) => envelope(t, a, b, fin, fout, ease.out, ease.inOut);

export { smoothstep, formatNumber };
