// "One function, seventeen releases": the history film.
//
// One small script, compiled once to Luau bytecode, decompiled by medal and by every Tovek release.
// The code on screen is each release's real output; dates, headlines, notes and numbers come from the
// release notes. Everything is read from ../data/story.json (written by v26/data/story.py), so the
// film follows the data when the final V2.6 binary regenerates it.
//
// Grammar: the code on the right is the same file throughout. Between releases it morphs in place
// (kept tokens glide, removed tokens dissolve, new tokens type in). Each era is drawn in the
// typographic mood of its own release page: the betas as a terminal, V2 stark black on white, V2.1
// ivory and clay, V2.5 deep blue, V2.6 the V2.6 identity. A new era turns over like a page.

import {
  ease, progress, envelope, clamp, lerp, smoothstep, spring,
  font, layoutText, drawText, reveal, typewriter, measure, metrics, setFont, fitSize,
  drawCounter, drawOdometer, formatNumber,
  layoutCode, codeMetrics, drawCode, codePalettes, codeMorph, drawMorph, withCamera, roundRect,
  defineScore, note, ticks, drawMark, markWidth, V26, rgba, mix, hash01, pixel,
} from '../engine/index.js';

// ------------------------------------------------------------------------------------------ time

// Every beat, in seconds. The release heads are when each release's name lands; MORPH is when the
// code changes from the previous release's output to this one's.
const T = {
  hex0: 1.0, focus: 5.0, decode0: 6.6, decode1: 8.6,
  medal: 8.4, medalOut: 18.6, ledger: 11.5,
  cal: 26.2, calOut: 44.8,
  homing0: 99.8, homing1: 109.6,
  expr: 110.4, nums: 115.8,
  orig: 121.0,
  end: 129.6,
  duration: 136.6,
};

const HEAD = {
  'medal': 8.4,
  'v0.1.0-beta': 19.2, 'v0.2.0-beta': 27.8, 'v0.3.0-beta': 30.8, 'v0.4.0-beta': 32.8,
  'v0.5.0-beta': 35.8, 'v0.5.1-beta': 38.8, 'v0.5.2-beta': 40.4, 'v0.6.0-beta': 42.2,
  'v0.7.0': 45.6, 'v0.8': 51.0, 'v0.9.0-beta': 60.2,
  'v2-v0.1': 64.8, 'v2.1': 73.8, 'v2.1.1': 78.8, 'v2.5': 84.0, 'v2.5.1': 92.6,
  'last': 98.0, // V2.6, whatever its tag
};

const MORPH = {
  'v0.1.0-beta': [20.4, 24.6], 'v0.2.0-beta': [28.4, 30.0], 'v0.4.0-beta': [33.4, 35.0],
  'v0.5.0-beta': [36.4, 37.8], 'v0.7.0': [46.6, 49.2], 'v0.8': [53.4, 57.8],
  'v2-v0.1': [65.8, 70.0], 'v2.1': [74.4, 75.8], 'v2.5': [89.0, 91.4],
};

// Eras and the page turns between them (the overlap of two spans is the turn).
const ERA_SPANS = [
  { id: 'dark', t0: 0, t1: 64.6 },
  { id: 'v2', t0: 63.4, t1: 73.6 },
  { id: 'v21', t0: 72.4, t1: 83.8 },
  { id: 'v25', t0: 82.6, t1: 97.8 },
  { id: 'v26', t0: 96.6, t1: 129.6 },
  { id: 'end', t0: 128.6, t1: T.duration + 1 },
];

// --------------------------------------------------------------------------------------- the look

const FAM = {
  inter: '"Inter", "Geist", system-ui, sans-serif',
  hanken: '"Hanken Grotesk", "Geist", system-ui, sans-serif',
  gsans: '"Google Sans", "Geist", system-ui, sans-serif',
};
// The typefaces of the V2, V2.1 and V2.5 release pages, for their eras (all on Google Fonts).
const ERA_FONTS_CSS = 'https://fonts.googleapis.com/css2?family=Inter:wght@400..700&family=Hanken+Grotesk:wght@400..700&family=Google+Sans:wght@400..700&display=swap';

const ERA = {
  dark: { surface: '#0d0d0c', ink: '#f2f0eb', ink2: '#a9a69c', ink3: '#8b8980', faint: '#3a3935', accent: '#ff4d1a' },
  v2: { surface: '#ffffff', ink: '#0b0b0b', ink2: '#505050', ink3: '#8c8c8c', faint: '#e6e6e6', accent: '#0b0b0b', marker: '#c6f135' },
  v21: { surface: '#faf9f5', ink: '#141413', ink2: '#5e5d59', ink3: '#87867f', faint: '#e5e4dd', accent: '#b0512f', fill: '#d97757' },
  v25: { surface: '#0b2a6b', ink: '#ffffff', ink2: '#bcd0f5', ink3: '#86a2d8', faint: '#24447f', accent: '#9dd2ff' },
  v26: { surface: '#f2f0eb', ink: '#121210', ink2: '#4a4943', ink3: '#8b8980', faint: '#dedbd3', accent: '#d63a0c' },
  end: { surface: '#0d0d0c', ink: '#f2f0eb', ink2: '#a9a69c', ink3: '#8b8980', faint: '#3a3935', accent: '#ff4d1a' },
};

const f = (size, family, weight = 400, stretch = 100) => font(size, { family, weight, stretch });

// Type per era. `title` is the release name, `meta` the date line, `head` the release headline.
const TY = {
  dark: {
    hero: f(112, 'display', 600, 87.5), date: f(40, 'text', 500), credit: f(30, 'text', 400), credit2: f(30, 'text', 500),
    label: f(17, 'mono', 500), prompt: f(21, 'mono', 400), title: f(96, 'mono', 600), mdate: f(40, 'mono', 400),
    head: f(27, 'mono', 500), cal: f(26, 'mono', 500), day: f(50, 'mono', 500), chip: f(17, 'mono', 500),
    wday: f(15, 'mono', 500), big: f(84, 'mono', 600), lab: f(18, 'mono', 400), val: f(19, 'mono', 500),
    hex: f(17, 'mono', 400), url: f(17, 'mono', 400),
  },
  v2: {
    meta: f(22, FAM.inter, 500), tovek: f(44, FAM.inter, 600), title: f(260, FAM.inter, 700), head: f(52, FAM.inter, 500),
    num: f(76, FAM.inter, 600), unit: f(24, FAM.inter, 400), lab: f(18, FAM.inter, 400), val: f(19, FAM.inter, 600),
  },
  v21: {
    meta: f(17, 'mono', 500), tovek: f(44, FAM.hanken, 600), title: f(200, FAM.hanken, 600), head: f(50, FAM.hanken, 500),
    num: f(30, FAM.hanken, 600), unit: f(22, FAM.hanken, 400), lab: f(18, FAM.hanken, 400), val: f(19, FAM.hanken, 600),
  },
  v25: {
    meta: f(24, FAM.gsans, 400), tovek: f(44, FAM.gsans, 500), title: f(200, FAM.gsans, 500), head: f(50, FAM.gsans, 400),
    num: f(140, FAM.gsans, 500), unit: f(24, FAM.gsans, 400), lab: f(18, FAM.gsans, 400), val: f(19, FAM.gsans, 500),
  },
  v26: {
    meta: f(17, 'mono', 500), tovek: f(48, 'display', 600, 87.5), title: f(220, 'display', 650, 87.5), head: f(46, 'text', 500),
    num: f(120, 'display', 600, 87.5), unit: f(24, 'text', 400), lab: f(18, 'text', 400), val: f(19, 'mono', 500),
    small: f(17, 'mono', 500), list: f(26, 'mono', 500), expA: f(26, 'mono', 400), expB: f(56, 'mono', 600),
    stat: f(50, 'display', 600, 87.5), side: f(17, 'mono', 500),
  },
  end: {
    num: f(150, 'display', 600, 87.5), unit: f(26, 'text', 400), card: f(160, 'display', 650, 87.5),
    credit: f(19, 'mono', 500), url: f(24, 'mono', 400),
  },
};

// Code: the same grid everywhere. Read shots draw at scale 1 (22 px); wide shots scale down.
const CODE = { size: 22, lineHeight: 1.55 };
const BOX = { x: 820, y: 112, w: 980, h: 840 };            // read shots
const MACRO = { x: 780, y: 64, w: 1040, h: 952 };          // V2.6's wide shot
const CLIP = { x: 740, y: 96, w: 1140, h: 872 };            // code area (left edge leaves room for flow arrows)
const CLIP26 = { x: 676, y: 52, w: 1204, h: 980 };
const COL = 120;                                             // text column left edge
const COLW = 600;                                            // text column width

// --------------------------------------------------------------------------------------- state
// Built once in prepare(); render() only reads it.

let D = null;            // story.json
let ST = null;           // stage by tag (+ 'last' for V2.6)
let ORDER = null;        // stages in order
let CM = null;           // code metrics
let PAL = null;          // code palette per era
let SHOTS = null;        // the code track
let LEDGER_EVENTS = null;
let HOMING = null;       // V2.6 copies-come-home analysis
let SIDE = null;         // the side-by-side with the original
let HEX = null;          // the cold open's hex dump
let CAL = null;          // the beta calendar
let N = null;            // parsed release numbers
let V26_STAGE = null;

// ------------------------------------------------------------------------------------- helpers

const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
const WEEKDAYS = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'];
const WORDS = ['zero', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight', 'nine', 'ten', 'eleven', 'twelve',
  'thirteen', 'fourteen', 'fifteen', 'sixteen', 'seventeen', 'eighteen', 'nineteen', 'twenty'];
const word = (n) => WORDS[n] ?? String(n);
const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1);
const lc = (s) => s.charAt(0).toLowerCase() + s.slice(1);
const ymd = (iso) => iso.slice(0, 10).split('-').map(Number);
const longDate = (iso) => { const [y, m, d] = ymd(iso); return `${d} ${MONTHS[m - 1]} ${y}`; };
const usDate = (iso) => { const [y, m, d] = ymd(iso); return `${MONTHS[m - 1]} ${d}, ${y}`; };
const monthYear = (iso) => { const [y, m] = ymd(iso); return `${MONTHS[m - 1]} ${y}`; };
const weekday = (iso) => WEEKDAYS[new Date(iso.slice(0, 10) + 'T00:00:00Z').getUTCDay()];
const num = (s) => Number(String(s).replace(/,/g, ''));
const stripDot = (s) => s.replace(/\.$/, '');

function hexOf(css) {
  if (css.startsWith('#')) return css;
  const m = css.match(/\d+/g).map(Number);
  return '#' + m.slice(0, 3).map((v) => v.toString(16).padStart(2, '0')).join('');
}

/** A code palette for an era: the night palette's tone steps, mixed between surface and ink. */
function makePalette(e, wash) {
  const W = { fn: 1, id: 0.94, glob: 0.88, prop: 0.82, num: 0.8, const: 0.74, str: 0.7, kw: 0.62, op: 0.56, punct: 0.5, com: 0.46 };
  const pal = {};
  for (const k in W) pal[k] = hexOf(mix(e.surface, e.ink, W[k]));
  pal.accent = e.accent;
  pal.wash = wash;
  pal.gutter = hexOf(mix(e.surface, e.ink, 0.3));
  pal.base = e.surface;
  return pal;
}

function hairline(ctx, x, y, w, color, alpha = 1) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.fillStyle = color;
  ctx.fillRect(x, y, w, Math.max(1, pixel(ctx)));
  ctx.restore();
}

/** Clip to a rectangle whose edges sit on whole device pixels (a scissor, never a coverage mask). */
function clipRect(ctx, r, fn) {
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

/** Characters that change roll vertically from `from` to `to` (monospace, left aligned). */
function rollText(ctx, from, to, x, y, F, t, start, { dur = 0.55, stagger = 0.045, color = '#fff', alpha = 1 } = {}) {
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

/** Parse "A to B label (paren)" or "B label (paren)" from a release-note number. */
function parseNumber(text) {
  let m = text.match(/^([\d,.]+) to ([\d,.]+) (.+?)(?: \((.+)\))?$/);
  if (m) return { from: num(m[1]), to: num(m[2]), label: m[3], paren: m[4] || '' };
  m = text.match(/^([\d,.]+) (.+?)(?: \((.+)\))?$/);
  if (m) return { to: num(m[1]), label: m[2], paren: m[3] || '' };
  return null;
}

// ------------------------------------------------------------------------------- code cameras

const camAt = (top, s = 1, box = BOX) => ({ s, x: box.x, y: box.y - top * CM.lh * s });
function camFit(first, last, box, maxS = 1) {
  const n = last - first + 1;
  const s = Math.min(maxS, box.h / (n * CM.lh));
  return { s, x: box.x, y: box.y + (box.h - n * CM.lh * s) / 2 - first * CM.lh * s };
}
const lerpCam = (a, b, p) => ({ s: lerp(a.s, b.s, p), x: lerp(a.x, b.x, p), y: lerp(a.y, b.y, p) });

/** A camera that moves through keyframes [[t, cam], ...] with an ease per segment. */
function camTrack(frames) {
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

function visibleLines(L, cam, clip) {
  const step = CM.lh * cam.s;
  const first = Math.max(0, Math.floor((clip.y - cam.y) / step) - 1);
  const last = Math.min(L.lineCount - 1, Math.ceil((clip.y + clip.h - cam.y) / step) + 1);
  return [first, last];
}

// Washes behind a set of tokens, merged into one rounded run per line.
const washCache = new WeakMap();
function washRuns(L, set) {
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

function drawWashes(ctx, runs, alpha, color, sweep = 1) {
  if (alpha <= 0 || !runs.length) return;
  const { cw, lh } = CM;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.fillStyle = color;
  for (const r of runs) {
    const w = (r.c1 - r.c0) * cw * sweep + cw * 0.5;
    roundRect(ctx, r.c0 * cw - cw * 0.25, r.line * lh + lh * 0.13, w, lh * 0.74, lh * 0.14);
  }
  ctx.restore();
}

// ---------------------------------------------------------------------------- flow arrows
// The jumps of the code drawn in its own indentation: goto arcs (dashed, flowing toward the label)
// and loops (solid, up the left side). Computed from the text of each output.

const flowCache = new Map();
function flowOf(L) {
  let F = flowCache.get(L.src);
  if (F) return F;
  const lines = L.lines;
  const indent = (s) => { let c = 0; for (const ch of s) { if (ch === '\t') c += 4; else if (ch === ' ') c++; else break; } return c; };
  const labels = new Map();
  lines.forEach((s, i) => { const m = s.match(/::(\w+)::/); if (m) labels.set(m[1], i); });
  const gotos = [];
  lines.forEach((s, i) => {
    const m = s.match(/\bgoto (\w+)/);
    if (m && labels.has(m[1])) gotos.push({ from: i, to: labels.get(m[1]), c0: indent(s), c1: indent(lines[labels.get(m[1])]) });
  });
  const loops = [];
  lines.forEach((s, i) => {
    if (!/^\s*(for .* do|while .* do)$/.test(s)) return;
    const ind = indent(s);
    for (let j = i + 1; j < lines.length; j++) {
      if (indent(lines[j]) === ind && lines[j].trim() === 'end') { loops.push({ from: i, to: j, c: ind, inner: /while true do/.test(s) }); break; }
    }
  });
  F = { gotos, loops };
  flowCache.set(L.src, F);
  return F;
}

function drawFlow(ctx, L, cam, t, color, { gotoA = 1, loopA = 1, innerA = 1 } = {}) {
  const F = flowOf(L);
  if (!F.gotos.length && !F.loops.length) return;
  const { cw, lh } = CM;
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
      const xm = Math.min(xa, xb) - bend;
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

function arrowHead(ctx, x, y, dir, lw) {
  const s = 4 + lw;
  ctx.beginPath();
  ctx.moveTo(x - s * dir, y - s * 0.75);
  ctx.lineTo(x, y);
  ctx.lineTo(x - s * dir, y + s * 0.75);
  ctx.stroke();
}

// --------------------------------------------------------------------------- the code track
// A list of shots that tile the film's code from the medal decode to the side-by-side. Each one
// draws the code for its window of time in whichever era's palette the page is in.

function holdShot(t0, t1, L, cam, opts = {}) {
  return { t0, t1, draw(ctx, t, era) {
    const c = typeof cam === 'function' ? cam(t) : cam;
    const pal = PAL[era];
    withCamera(ctx, c, () => {
      if (opts.highlight) {
        const k = opts.mix ? opts.mix(t) : 0;
        const wash = opts.wash ? opts.wash(t) : 0;
        if (wash > 0) drawWashes(ctx, washRuns(L, opts.highlight), wash, era === 'v2' ? ERA.v2.marker : rgba(pal.accent, era === 'dark' || era === 'v25' ? 0.2 : 0.14));
        const [first, last] = visibleLines(L, c, clipFor(era));
        drawCode(ctx, L, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, lines: [first, last], y: first * CM.lh, highlight: k > 0 ? opts.highlight : null, highlightMix: k, lineAlpha: opts.lineAlpha ? (l) => opts.lineAlpha(t, l) : undefined });
      } else {
        const [first, last] = visibleLines(L, c, clipFor(era));
        drawCode(ctx, L, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, lines: [first, last], y: first * CM.lh, lineAlpha: opts.lineAlpha ? (l) => opts.lineAlpha(t, l) : undefined });
      }
    });
    if (opts.flow) {
      const fa = opts.flow(t);
      if (fa.gotoA > 0 || fa.loopA > 0) drawFlow(ctx, L, c, t, ERA[era].ink2, fa);
    }
  } };
}

function morphShot(t0, t1, plan, camA, camB, opts = {}) {
  return { t0, t1, draw(ctx, t, era) {
    const p = (t - t0) / (t1 - t0);
    drawMorph(ctx, plan, p, { size: CODE.size, lineHeight: CODE.lineHeight, palette: PAL[era], cameraA: camA, cameraB: camB, blur: 5, ...opts });
    if (opts.flowA || opts.flowB) {
      const out = 1 - clamp(p / 0.25), inn = clamp((p - 0.85) / 0.15);
      if (opts.flowA && out > 0) drawFlow(ctx, plan.a, camA, t, ERA[era].ink2, { gotoA: out * (opts.flowA.gotoA ?? 1), loopA: out * (opts.flowA.loopA ?? 1) });
      if (opts.flowB && inn > 0) drawFlow(ctx, plan.b, camB, t, ERA[era].ink2, { gotoA: inn * (opts.flowB.gotoA ?? 1), loopA: inn * (opts.flowB.loopA ?? 1) });
    }
  } };
}

const clipFor = (era) => (era === 'v26' ? CLIP26 : CLIP);

// Generated names (v, v2, p, p3, v_u_2) are never a recovery, so they never take the accent.
const GENERATED = /^(?:[vp](?:_u)?_?\d*|_)$/;
const NAME_KINDS = new Set(['id', 'fn', 'prop', 'glob']);
/**
 * The inserted tokens of a morph that show what the release recovered. `focus`: 'names' (new real
 * names only), 'all' (every new token except generated names), or 'none' (nothing was recovered:
 * a generated name was only renumbered).
 */
function recovered(plan, focus) {
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

/** The highlight of a morph's new tokens after it lands: wash in, hold, then settle. */
const settle = (t1, hold = 1.8, out = 1.4) => (t) => (t < t1 ? 0 : 1 - clamp((t - t1 - hold) / out));
const washIn = (t1, hold = 1.8, out = 1.4) => (t) => (t < t1 ? 0 : Math.min(ease.out(clamp((t - t1) / 0.45)), 1 - clamp((t - t1 - hold) / out)));

function buildShots() {
  const S = (tag) => ST[tag];
  const L = (tag) => layoutCode(S(tag).output);
  const shieldTop = (tag) => S(tag).excerpts.shield.first_line - 1;
  const shieldEnd = (tag) => S(tag).excerpts.shield.last_line - 1;
  const joinTop = (tag) => S(tag).excerpts.join.first_line - 1;
  const forLine = (tag) => L(tag).lineOf(/^\s*for _, /, shieldTop(tag));
  const whileLine = (tag) => L(tag).lineOf(/^\s*while os\.clock/, shieldTop(tag));
  const forTop = (tag) => forLine(tag) - 2;
  const whileTop = (tag) => whileLine(tag) - 4;
  const plan = (a, b) => codeMorph(S(a).output, S(b).output);

  const m = MORPH;
  const camMedalTop = camAt(shieldTop('medal'));
  const camMedalFor = camAt(forTop('medal'));
  const camB01For = camAt(forTop('v0.1.0-beta'));
  const camB01Join = camAt(joinTop('v0.1.0-beta'));
  const camB06For = camAt(forTop('v0.6.0-beta'));
  const cam07For = camAt(forTop('v0.7.0'));
  // the wide shot of 0.8's loop: from the for line to the first statement after it
  const L08 = L('v0.8');
  const for08 = forLine('v0.8');
  const inner08 = flowOf(L08).loops.find((lp) => lp.inner && lp.from > for08);
  const after08 = inner08 ? inner08.to + 2 : L08.lineOf(/^\s*local \w+ = os\.clock\(\)/, for08) + 1;
  const cam08Wide = camFit(for08 - 1, after08, BOX);
  const camV2For = camAt(forTop('v2-v0.1'));
  const camV2While = camAt(whileTop('v2-v0.1'));
  const camV21While = camAt(whileTop('v2.1.1'));
  const camV25Join = camAt(joinTop('v2.5'));

  const flowOn = (t0, t1, fadeIn = 0.6, fadeOut = 0.5) => (t) => {
    const a = envelope(t, t0, t1, fadeIn, fadeOut);
    return { gotoA: a, loopA: a };
  };

  const pB01 = plan('medal', 'v0.1.0-beta');
  const pB02 = plan('v0.1.0-beta', 'v0.2.0-beta');
  const pB04 = plan('v0.3.0-beta', 'v0.4.0-beta');
  const pB05 = plan('v0.4.0-beta', 'v0.5.0-beta');
  const p07 = plan('v0.6.0-beta', 'v0.7.0');
  const p08 = plan('v0.7.0', 'v0.8');
  const pV2 = plan('v0.9.0-beta', 'v2-v0.1');
  const pV21 = plan('v2-v0.1', 'v2.1');
  const pV25 = plan('v2.1.1', 'v2.5');
  const R = {
    b01: recovered(pB01, 'names'), b02: recovered(pB02, 'names'), b04: recovered(pB04, 'names'),
    b05: recovered(pB05, 'all'), r07: recovered(p07, 'names'), v2: recovered(pV2, 'all'),
    v21: recovered(pV21, 'none'), v25: recovered(pV25, 'all'),
  };

  const last = V26_STAGE;
  const v251 = S('v2.5.1');
  const L251 = layoutCode(v251.output), L26 = layoutCode(last.output);
  const helperTop = Math.max(0, L26.lineOf(/^local function /) - 1);
  const macroA = camFit(helperTop, v251.excerpts.shield.last_line - 1, MACRO);
  const macroB = camFit(helperTop, last.excerpts.shield.last_line - 1, MACRO);
  const read26 = camAt(last.excerpts.shield.first_line - 1);
  const exprLines = HOMING.exprs.map((e) => e.line);
  // the second close-up frames the last expression (frames(1)) near the bottom of the window
  const read26b = camAt(Math.max(last.excerpts.shield.first_line - 1, (exprLines[exprLines.length - 1] ?? 40) - 18));
  const exprFocus = (t, l) => {
    for (const e of HOMING.exprs) {
      const a = envelope(t, e.t0 - 0.2, e.t0 + 2.4, 0.4, 0.5);
      if (a > 0) return l === e.line ? 1 : 1 - 0.55 * a;
    }
    return 1;
  };

  return [
    { t0: T.decode0, t1: T.decode1, draw: (ctx, t, era) => drawDecode(ctx, L('medal'), camMedalTop, t, PAL[era]) },
    holdShot(T.decode1, m['v0.1.0-beta'][0], L('medal'), camTrack([[10.6, camMedalTop], [17.6, camMedalFor, ease.inOutSine]]), { flow: flowOn(11.6, 21.4, 0.8) }),
    morphShot(...m['v0.1.0-beta'], pB01, camMedalFor, camB01For, { flowA: {}, flowB: {}, highlight: R.b01 }),
    holdShot(m['v0.1.0-beta'][1], m['v0.2.0-beta'][0], L('v0.1.0-beta'), camTrack([[26.6, camB01For], [27.6, camB01Join]]), { highlight: R.b01, mix: settle(m['v0.1.0-beta'][1], 1.2, 1.0), wash: washIn(m['v0.1.0-beta'][1], 1.0, 0.9), flow: flowOn(m['v0.1.0-beta'][1] - 0.01, 26.7, 0, 0.5) }),
    morphShot(...m['v0.2.0-beta'], pB02, camB01Join, camB01Join, { highlight: R.b02 }),
    holdShot(m['v0.2.0-beta'][1], m['v0.4.0-beta'][0], L('v0.3.0-beta'), camB01Join, { highlight: R.b02, mix: settle(m['v0.2.0-beta'][1]), wash: washIn(m['v0.2.0-beta'][1]) }),
    morphShot(...m['v0.4.0-beta'], pB04, camB01Join, camB01Join, { highlight: R.b04 }),
    holdShot(m['v0.4.0-beta'][1], m['v0.5.0-beta'][0], L('v0.4.0-beta'), camB01Join, { highlight: R.b04, mix: settle(m['v0.4.0-beta'][1], 0.9, 0.5), wash: washIn(m['v0.4.0-beta'][1], 0.9, 0.5) }),
    morphShot(...m['v0.5.0-beta'], pB05, camB01Join, camB01Join, { highlight: R.b05 }),
    holdShot(m['v0.5.0-beta'][1], m['v0.7.0'][0], L('v0.6.0-beta'), camTrack([[45.4, camB01Join], [46.4, camB06For]]), { highlight: R.b05, mix: settle(m['v0.5.0-beta'][1], 2.2), wash: washIn(m['v0.5.0-beta'][1], 2.2) }),
    morphShot(...m['v0.7.0'], p07, camB06For, cam07For, { highlight: R.r07 }),
    holdShot(m['v0.7.0'][1], m['v0.8'][0], L('v0.7.0'), cam07For, { highlight: R.r07, mix: settle(m['v0.7.0'][1], 1.0, 0.8), wash: washIn(m['v0.7.0'][1], 1.0, 0.8), flow: flowOn(50.2, 54.2, 0.7, 0.3) }),
    morphShot(...m['v0.8'], p08, cam07For, cam08Wide, { flowA: { loopA: 1 }, flowB: { gotoA: 0 }, inserted: 'base' }),
    holdShot(m['v0.8'][1], m['v2-v0.1'][0], L08, cam08Wide, { flow: flowOn(m['v0.8'][1] - 0.01, 66.6, 0, 0.5) }),
    morphShot(...m['v2-v0.1'], pV2, cam08Wide, camV2For, { flowA: { gotoA: 0 }, flowB: {}, highlight: R.v2 }),
    holdShot(m['v2-v0.1'][1], m['v2.1'][0], L('v2-v0.1'), camTrack([[73.6, camV2For], [74.4, camV2While]]), { highlight: R.v2, mix: settle(m['v2-v0.1'][1], 1.6), wash: washIn(m['v2-v0.1'][1], 1.6), flow: flowOn(m['v2-v0.1'][1] - 0.01, 73.8, 0, 0.5) }),
    morphShot(...m['v2.1'], pV21, camV2While, camV2While, { highlight: R.v21 }),
    holdShot(m['v2.1'][1], m['v2.5'][0], L('v2.1.1'), camV21While),
    morphShot(...m['v2.5'], pV25, camV21While, camV25Join, { highlight: R.v25 }),
    holdShot(m['v2.5'][1], T.homing0, L251, camTrack([[98.2, camV25Join], [99.6, macroA, ease.glide]]), { highlight: R.v25, mix: settle(m['v2.5'][1], 2.0), wash: washIn(m['v2.5'][1], 2.0) }),
    { t0: T.homing0, t1: T.homing1, draw: (ctx, t, era) => drawHoming(ctx, HOMING, (t - T.homing0) / (T.homing1 - T.homing0), macroA, macroB, PAL[era], ERA[era]) },
    holdShot(T.homing1, T.orig + 0.8, L26, camTrack([[109.8, macroB], [111.2, read26, ease.glide], [112.6, read26], [113.8, read26b]]), { highlight: HOMING.plan.inserted, mix: settle(T.homing1, 3.6, 1.8), wash: washIn(T.homing1 - 0.45, 3.6, 1.8), lineAlpha: exprFocus }),
  ];
}

function drawCodeTrack(ctx, t, era) {
  if (!SHOTS) return;
  let shot = null;
  for (const s of SHOTS) if (t >= s.t0 && t < s.t1) { shot = s; break; }
  if (!shot) return;
  const clip = clipFor(era);
  clipRect(ctx, clip, () => shot.draw(ctx, t, era));
  // soft edges where the code runs under the top and bottom of its area
  const e = ERA[era];
  const fadeH = 46;
  for (const [y0, y1] of [[clip.y, clip.y + fadeH], [clip.y + clip.h, clip.y + clip.h - fadeH]]) {
    const g = ctx.createLinearGradient(0, y0, 0, y1);
    g.addColorStop(0, rgba(e.surface, 1));
    g.addColorStop(1, rgba(e.surface, 0));
    ctx.fillStyle = g;
    ctx.fillRect(clip.x - 2, Math.min(y0, y1), clip.w + 4, fadeH);
  }
}

// ------------------------------------------------------------------- bytes become medal's code

function drawDecode(ctx, L, cam, t, pal) {
  const { f: cf, cw, lh, baseline } = CM;
  const t0 = T.decode0 + 0.2, dur = T.decode1 - t0 - 0.1;
  const [first, last] = visibleLines(L, cam, CLIP);
  const tick = Math.floor(t * 30);
  const HEXC = '0123456789abcdef';
  withCamera(ctx, cam, () => {
    setFont(ctx, cf);
    ctx.textBaseline = 'alphabetic';
    ctx.textAlign = 'left';
    const base = ctx.globalAlpha;
    for (const tk of L.tokens) {
      if (tk.lastLine < first || tk.line > last) continue;
      const color = pal[tk.k] || pal.id;
      for (const part of tk.parts) {
        if (part.line < first || part.line > last) continue;
        const rowT = t0 + ((part.line - first) / Math.max(1, last - first)) * dur * 0.62;
        const py = part.line * lh + baseline;
        for (let c = 0; c < part.text.length; c++) {
          const ch = part.text[c];
          if (ch === ' ') continue;
          const st = rowT + (part.col + c) * 0.006 + hash01(11, part.line, part.col + c) * 0.16;
          if (t < st - 0.22) continue;
          const px = (part.col + c) * cw;
          if (t < st) {
            ctx.globalAlpha = base * (0.35 + 0.4 * hash01(5, part.line, part.col + c, tick));
            ctx.fillStyle = pal.com;
            ctx.fillText(HEXC[Math.floor(hash01(3, part.line, part.col + c, tick) * 16)], px, py);
          } else {
            ctx.globalAlpha = base;
            ctx.fillStyle = color;
            ctx.fillText(ch, px, py);
          }
        }
      }
    }
  });
}

// ----------------------------------------------------------------------- V2.6: copies come home
// Luau -O2 pasted helpers into their callers. V2.6 finds each pasted copy and rebuilds the call.
// From the token diff of V2.5.1's output and V2.6's: every edit hunk whose new text calls a helper
// defined in the file is a copy of that helper. Its removed tokens fly home to the helper's
// definition; the call types in where the copy was.

function analyzeHoming(plan) {
  const A = plan.a, B = plan.b;
  const removedByLine = new Map();
  plan.removed.forEach((r, i) => { if (!removedByLine.has(r.line)) removedByLine.set(r.line, []); removedByLine.get(r.line).push(i); });
  const keptA = new Map(); // A line -> Set of B lines
  for (const g of plan.kept.values()) {
    for (let q = 0; q < g.nums.length; q += 5) {
      const la = g.nums[q + 1], lb = g.nums[q + 3];
      if (!keptA.has(la)) keptA.set(la, new Set());
      keptA.get(la).add(lb);
    }
  }
  const insertedLines = new Set(plan.runs.map((r) => r.line));
  // helpers defined at the top level of the new output
  const helpers = new Map();
  B.lines.forEach((s, i) => {
    const m = s.match(/^local function (\w+)\(/);
    if (!m) return;
    let end = i;
    for (let j = i + 1; j < B.lines.length; j++) if (B.lines[j] === 'end') { end = j; break; }
    const col = s.indexOf(m[1]);
    helpers.set(m[1], { name: m[1], line: i, end, col, len: m[1].length, copies: [], arrive: Infinity });
  });

  // walk A: unchanged lines anchor; runs of changed lines between anchors form hunks
  const hunks = [];
  let cur = null, prevAnchorB = -1;
  const closeHunk = (nextAnchorB) => {
    if (cur) { cur.b0 = prevAnchorB + 1; cur.b1 = nextAnchorB - 1; hunks.push(cur); cur = null; }
  };
  for (let la = 0; la < A.lineCount; la++) {
    const rem = removedByLine.get(la);
    const kb = keptA.get(la);
    if (!rem && !kb) continue; // blank line
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
    // a folded constant solved back: a long number became a helper call or an exact fraction
    for (const i of h.removed) {
      const r = plan.removed[i];
      if (r.k !== 'num' || r.text.length < 8) continue;
      const kb = keptA.get(r.line);
      if (!kb || kb.size !== 1) continue;
      const lb = [...kb][0];
      const lineText = B.lines[lb];
      const call = [...helpers.values()].map((hh) => lineText.match(new RegExp(`\\b${hh.name}\\([^()]*\\)`))).find(Boolean);
      const m = call || lineText.match(/\b\d+(?:\.\d+)? \/ \d+(?:\.\d+)?\b/);
      if (m) exprs.push({ from: r.text, to: m[0], line: lb });
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

  // timing inside p (0..1): copies leave in file order, one after another
  copies.sort((a, b) => a.l0 - b.l0);
  copies.forEach((c, i) => {
    c.depart = 0.15 + i * 0.075;
    c.travel = 0.2;
    c.arrive = c.depart + c.travel * 0.92;
    c.helper.arrive = Math.max(c.helper.arrive === Infinity ? 0 : c.helper.arrive, c.arrive);
  });
  const copyOf = new Map();
  copies.forEach((c) => c.removed.forEach((i) => copyOf.set(i, c)));
  // each removed token's order inside its copy, so a copy peels off in reading order
  const order = new Map();
  for (const c of copies) [...c.removed].sort((a, b) => a - b).forEach((i, k, all) => order.set(i, k / Math.max(1, all.length - 1)));
  // inserted runs on a helper's own lines type in as its copies arrive; the rest at the call sites
  const helperOfLine = (l) => { for (const h of helpers.values()) if (h.copies.length && l >= h.line && l <= h.end) return h; return null; };
  const runInfo = plan.runs.map((r) => ({ run: r, helper: helperOfLine(r.line) }));
  const helperList = [...helpers.values()].filter((h) => h.copies.length);
  return { plan, copies, copyOf, order, runInfo, helpers: helperList, exprs };
}

function drawHoming(ctx, H, p, camA, camB, pal, era) {
  const plan = H.plan;
  const { f: cf, cw, lh, baseline } = CM;
  const P = clamp(p);
  const mv = clamp((P - 0.42) / 0.38);
  const cam = lerpCam(camA, camB, ease.inOut(mv));
  ctx.save();
  ctx.translate(cam.x, cam.y);
  ctx.scale(cam.s, cam.s);
  setFont(ctx, cf);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  const base = ctx.globalAlpha;

  // 1. the copies are marked: a band behind each, a thin rule at its left, the helper's name beside it.
  //    When a copy leaves, the band and its tokens travel together as one card.
  const select = ease.out(clamp((P - 0.015) / 0.09));
  const drawCard = (c, lit) => {
    const bx = c.c0 * cw - cw * 0.8, by = c.l0 * lh + lh * 0.06;
    const bw = (c.c1 - c.c0) * cw + cw * 1.6, bh = (c.l1 - c.l0 + 1) * lh - lh * 0.12;
    const a = ctx.globalAlpha;
    ctx.globalAlpha = a * lit;
    ctx.fillStyle = rgba(pal.accent, 0.1);
    roundRect(ctx, bx, by, bw, bh, lh * 0.18);
    ctx.fillStyle = pal.accent;
    ctx.fillRect(bx, by, Math.max(2, 2.5 / cam.s), bh);
    ctx.globalAlpha = a;
    for (const i of c.removed) {
      const r = plan.removed[i];
      ctx.fillStyle = lit >= 1 ? pal.accent : mix(hexOf(pal[r.k] || pal.id), pal.accent, lit);
      ctx.fillText(r.text, r.col * cw, r.line * lh + baseline);
    }
  };
  for (const c of H.copies) {
    const q = clamp((P - c.depart) / c.travel);
    if (q >= 1) continue;
    const ax = ((c.c0 + c.c1) / 2) * cw, ay = ((c.l0 + c.l1 + 1) / 2) * lh;
    const tx = (c.helper.col + c.helper.len / 2) * cw, ty = (c.helper.line + 0.5) * lh;
    if (q <= 0) {
      ctx.globalAlpha = base;
      drawCard(c, select);
      ctx.fillStyle = pal.accent;
      ctx.globalAlpha = base * select;
      ctx.fillText(c.helper.name, (c.c1 + 2.5) * cw, c.l0 * lh + baseline);
      continue;
    }
    // the route: a quadratic arc that swings out to the left of the code
    const cx = Math.min(ax, tx) - 14 * cw, cy = (ay + ty) / 2;
    const at = (e) => [(1 - e) * (1 - e) * ax + 2 * (1 - e) * e * cx + e * e * tx, (1 - e) * (1 - e) * ay + 2 * (1 - e) * e * cy + e * e * ty];
    const head = ease.inOutCubic(q);
    // a fine trail behind the card
    ctx.save();
    ctx.strokeStyle = pal.accent;
    ctx.lineWidth = 1.6 / cam.s;
    ctx.lineCap = 'round';
    const tail = Math.max(0, head - 0.5), steps = 24;
    let prev = at(tail);
    for (let k = 1; k <= steps; k++) {
      const pt = at(tail + ((head - tail) * k) / steps);
      ctx.globalAlpha = base * 0.45 * (k / steps) * (1 - smoothstep(0.82, 1, q));
      ctx.beginPath();
      ctx.moveTo(prev[0], prev[1]);
      ctx.lineTo(pt[0], pt[1]);
      ctx.stroke();
      prev = pt;
    }
    ctx.restore();
    // the card: lifts a touch, then shrinks into the helper's name as it arrives
    const [px, py] = at(head);
    const lift = 1 + 0.05 * Math.sin(Math.min(1, q * 3) * Math.PI);
    const k = lerp(1, 0.16, ease.inQuad(q)) * lift;
    ctx.save();
    ctx.globalAlpha = base * (1 - smoothstep(0.7, 1, q));
    ctx.translate(px, py);
    ctx.scale(k, k);
    ctx.translate(-ax, -ay);
    drawCard(c, 1);
    ctx.restore();
  }
  // the helpers' definitions glow as each copy arrives
  for (const h of H.helpers) {
    let glow = 0.0;
    for (const c of h.copies) glow = Math.max(glow, envelope(P, c.arrive - 0.03, c.arrive + 0.16, 0.03, 0.13));
    const ready = select * (1 - clamp((P - 0.9) / 0.1));
    if (ready > 0) {
      ctx.globalAlpha = base * ready * (0.35 + 0.65 * glow);
      ctx.fillStyle = rgba(pal.accent, 0.08 + 0.16 * glow);
      roundRect(ctx, h.col * cw - cw * 0.35, h.line * lh + lh * 0.1, h.len * cw + cw * 0.7, lh * 0.8, lh * 0.16);
    }
  }

  // 2. removed tokens that are not copies dissolve once the copies are on their way
  const po = clamp((P - 0.42) / 0.2);
  for (let i = 0; i < plan.removed.length; i++) {
    if (H.copyOf.has(i)) continue;
    const r = plan.removed[i];
    const q = ease.inOut(po);
    if (q >= 1) continue;
    ctx.globalAlpha = base * (1 - q);
    ctx.fillStyle = pal[r.k] || pal.id;
    ctx.fillText(r.text, r.col * cw, r.line * lh + baseline - q * lh * 0.35);
  }

  // 4. the code closes the gaps the copies left (kept tokens glide from A to B)
  const spread = 0.3;
  ctx.globalAlpha = base;
  for (const [k, g] of plan.kept) {
    ctx.fillStyle = pal[k] || pal.id;
    const nums = g.nums, texts = g.text;
    for (let n = 0, q = 0; n < texts.length; n++, q += 5) {
      const d = nums[q + 4] * spread;
      const e = mv <= 0 ? 0 : mv >= 1 ? 1 : ease.inOut(clamp((mv - d) / (1 - spread)));
      ctx.fillText(texts[n], (nums[q] + (nums[q + 2] - nums[q]) * e) * cw, (nums[q + 1] + (nums[q + 3] - nums[q + 1]) * e) * lh + baseline);
    }
  }

  // 5. new text types in: on a helper's lines as its copies arrive, at the call sites after the move
  for (const { run, helper } of H.runInfo) {
    const start = helper ? helper.arrive : 0.66 + (run.line / Math.max(1, plan.b.lineCount)) * 0.18;
    const typed = (P - start) / 0.0045;
    if (typed <= 0) continue;
    for (const pc of run.pieces) {
      const visible = typed - pc.at;
      if (visible <= 0) break;
      ctx.fillStyle = pal.accent;
      const whole = Math.min(pc.text.length, Math.floor(visible));
      const px = pc.col * cw, py = run.line * lh + baseline;
      ctx.globalAlpha = base;
      if (whole > 0) ctx.fillText(pc.text.slice(0, whole), px, py);
      if (whole < pc.text.length) {
        ctx.globalAlpha = base * (visible - whole);
        ctx.fillText(pc.text[whole], px + whole * cw, py);
      }
    }
  }
  ctx.restore();
}

// ------------------------------------------------------------------------------- the cold open

function buildHex() {
  const s = D.sample;
  const hex = s.bytecode_hex;
  const bytes = hex.length / 2;
  const PER = 48;
  const rows = [];
  for (let r = 0; r * PER < bytes; r++) {
    const off = (r * PER).toString(16).padStart(6, '0');
    let body = '';
    for (let b = r * PER; b < Math.min(bytes, (r + 1) * PER); b += 2) body += (body ? ' ' : '') + hex.slice(b * 2, b * 2 + 4);
    rows.push({ off, body, text: off + '  ' + body, b0: r * PER, b1: Math.min(bytes, (r + 1) * PER) });
  }
  // where shield's instructions sit in the file (little-endian words)
  const le = (w) => w.match(/../g).reverse().join('');
  const seq = s.shield_bytecode.map((i) => le(i.word) + (i.aux ? le(i.aux) : '')).join('');
  const at = hex.indexOf(seq);
  const hl0 = at >= 0 ? at / 2 : -1, hl1 = at >= 0 ? (at + seq.length) / 2 : -1;
  const F = TY.dark.hex;
  const cw = measure('0', F);
  const lh = 25.5;
  const width = rows[0].text.length * cw;
  const x0 = Math.round((1920 - width) / 2);
  const y0 = Math.round(540 - (rows.length * lh) / 2 + lh * 0.75);
  const charOf = (k) => 8 + Math.floor(k / 2) * 5 + (k % 2) * 2; // byte k of a row -> its first hex char
  rows.forEach((row) => {
    const a = Math.max(hl0, row.b0), b = Math.min(hl1, row.b1);
    row.hl = a < b ? [charOf(a - row.b0), charOf(b - 1 - row.b0) + 2] : null;
  });
  const hlRows = rows.map((r, i) => (r.hl ? i : -1)).filter((i) => i >= 0);
  return { rows, F, cw, lh, x0, y0, width, bytes, hlRows, instructions: s.shield_bytecode.length };
}

function drawOpen(ctx, t) {
  if (t >= T.decode1 + 0.2) return;
  const H = HEX, e = ERA.dark;
  const pull = ease.inOut(clamp((t - 0.5) / 4.8));
  const s = lerp(2.3, 1, pull);
  const fx = lerp(H.x0 + 6 * H.cw, 960, pull), fy = lerp(H.y0 - H.lh * 0.3, 540, pull);
  const leave = ease.inOut(clamp((t - 6.0) / 1.0));
  const ROW = 0.1, TYPE = 0.42;
  const focus = ease.inOut(clamp((t - T.focus) / 0.8));
  ctx.save();
  ctx.translate(960 + leave * 220, 540);
  ctx.scale(s * (1 - 0.05 * leave), s * (1 - 0.05 * leave));
  ctx.translate(-fx, -fy);
  setFont(ctx, H.F);
  ctx.textBaseline = 'alphabetic';
  const base = ctx.globalAlpha;
  const hlLeave = ease.inOut(clamp((t - 6.35) / 0.85));
  let caret = null;
  H.rows.forEach((row, r) => {
    const tr = T.hex0 + r * ROW;
    if (t < tr) return;
    const n = Math.min(row.text.length, Math.floor(((t - tr) / TYPE) * row.text.length));
    const y = H.y0 + r * H.lh;
    const seg = (c0, c1, color, a) => {
      const b = Math.min(c1, n);
      if (b <= c0 || a <= 0) return;
      ctx.globalAlpha = base * a;
      ctx.fillStyle = color;
      ctx.fillText(row.text.slice(c0, b), H.x0 + c0 * H.cw, y);
    };
    const dimA = lerp(0.62, 0.15, focus) * (1 - leave);
    seg(0, 6, e.ink3, lerp(0.8, 0.3, focus) * (1 - leave));
    if (row.hl) {
      seg(8, row.hl[0], e.ink2, dimA);
      seg(row.hl[0], row.hl[1], focus > 0 ? mix(e.ink2, e.ink, focus) : e.ink2, lerp(0.62, 1, focus) * (1 - hlLeave));
      seg(row.hl[1], row.text.length, e.ink2, dimA);
    } else seg(8, row.text.length, e.ink2, dimA);
    if (n < row.text.length) {
      // the typing front is brighter, like a terminal under load
      seg(Math.max(8, n - 6), n, e.ink, 0.9 * (1 - leave));
      caret = { x: H.x0 + n * H.cw, y };
    } else if (r === H.rows.length - 1) caret = { x: H.x0 + n * H.cw + H.cw * 0.4, y, idle: true };
  });
  // the caret: blinking alone before the bytes, riding the front while they stream
  if (t < T.hex0) caret = { x: H.x0, y: H.y0, idle: true };
  if (caret && t < T.focus + 0.2) {
    const on = caret.idle ? Math.floor(t * 1.9) % 2 === 0 : true;
    if (on) {
      ctx.globalAlpha = base * 0.95;
      ctx.fillStyle = e.ink;
      ctx.fillRect(caret.x + 1, caret.y - H.F.size * 0.78, H.cw * 0.62, H.F.size * 0.98);
    }
  }
  // labels: the file, then shield's bytes
  const L1 = layoutText(`${D.sample.file}  ·  ${formatNumber(H.bytes)} bytes  ·  Luau bytecode v${D.sample.bytecode_version}`, TY.dark.label);
  ctx.globalAlpha = base * (1 - leave);
  typewriter(ctx, L1, H.x0, H.y0 - H.lh * 1.9, clamp((t - 1.6) * 34, 0, L1.glyphCount), { color: e.ink2 });
  if (H.hlRows.length) {
    const r0 = H.hlRows[0], r1 = H.hlRows[H.hlRows.length - 1];
    const bx = H.x0 + H.width + 26;
    const yTop = H.y0 + r0 * H.lh - H.lh * 0.78, yBot = H.y0 + r1 * H.lh + H.lh * 0.3;
    const a = ease.out(clamp((t - T.focus - 0.15) / 0.6)) * (1 - hlLeave);
    if (a > 0) {
      ctx.globalAlpha = base * a;
      ctx.fillStyle = e.ink2;
      const grow = (yBot - yTop) * ease.out(clamp((t - T.focus - 0.15) / 0.7));
      ctx.fillRect(bx, yTop, Math.max(1, pixel(ctx)), grow);
      const L2 = layoutText(`${D.sample.focus_function}()`, TY.dark.prompt);
      const L3 = layoutText(`${H.instructions} instructions`, TY.dark.label);
      reveal(ctx, L2, bx + 18, (yTop + yBot) / 2 - 2, t, { unit: 'line', start: T.focus + 0.3, dur: 0.7, color: e.ink });
      reveal(ctx, L3, bx + 18, (yTop + yBot) / 2 + 26, t, { unit: 'line', start: T.focus + 0.45, dur: 0.7, color: e.ink2 });
    }
  }
  ctx.restore();
}

// ----------------------------------------------------------------------------------- the ledger
// A small readout of the file as each release decompiles it. Values roll when they change; a value
// that got better glows in the era's accent for a moment.

const LEDGER = [
  { label: 'non-blank lines', get: (s) => s.nonblank_lines, better: -1 },
  { label: 'goto', get: (s) => s.features.goto, better: -1 },
  { label: 'dispatcher states', get: (s) => s.features.dispatcher_states, better: -1 },
  { label: 'generated names', get: (s) => s.features.generated_names, better: -1 },
  { label: 'rebuilt calls', get: (s) => s.features.rebuilt_calls, better: 1 },
];

function buildLedgerEvents() {
  const ev = [{ t: T.ledger, stage: ST.medal }];
  for (const s of ORDER.slice(1)) {
    const key = s === V26_STAGE ? 'last' : s.tag;
    const t = s === V26_STAGE ? T.homing0 + 4.4 : MORPH[s.tag] ? MORPH[s.tag][0] : HEAD[key];
    if (t != null) ev.push({ t, stage: s });
  }
  return ev.sort((a, b) => a.t - b.t);
}

function drawLedger(ctx, t, era) {
  if (t < T.ledger - 0.1 || t > T.orig + 0.8) return;
  const e = ERA[era], ty = TY[era === 'dark' ? 'dark' : era];
  const a = ease.out(clamp((t - T.ledger) / 0.9)) * (1 - ease.inOut(clamp((t - T.orig) / 0.7)));
  if (a <= 0) return;
  let i = 0;
  while (i + 1 < LEDGER_EVENTS.length && LEDGER_EVENTS[i + 1].t <= t) i++;
  const cur = LEDGER_EVENTS[i], prev = LEDGER_EVENTS[Math.max(0, i - 1)];
  const y0 = 828, gap = 26, xr = COL + 420;
  ctx.save();
  ctx.globalAlpha *= a;
  const head = layoutText(D.sample.file.toUpperCase(), TY.dark.label);
  drawText(ctx, head, COL, y0 - 8, { color: e.ink2, tracking: 0.1 });
  hairline(ctx, COL, y0 + 4, xr - COL, e.ink, 0.18);
  LEDGER.forEach((row, k) => {
    const y = y0 + 32 + k * gap;
    const v = row.get(cur.stage), pv = row.get(prev.stage);
    drawText(ctx, layoutText(row.label, ty.lab), COL, y, { color: e.ink2 });
    const since = t - cur.t;
    const improved = i > 0 && (v - pv) * row.better > 0;
    const glow = improved && era !== 'v2' ? envelope(since, 0, 2.6, 0.3, 1.2) : 0;
    const color = glow > 0 ? mix(e.ink, e.accent, glow) : e.ink;
    if (i > 0 && v !== pv && since < 1.3) drawOdometer(ctx, t, { from: pv, to: v, start: cur.t, dur: 1.1, x: xr, y, font: ty.val, align: 'right', color, turns: 0 });
    else drawCounter(ctx, v, xr, y, ty.val, { align: 'right', color });
  });
  ctx.restore();
}

// ------------------------------------------------------------------------------ the text column

// medal: "It began as medal." and the people who wrote it
function textMedal(ctx, t) {
  if (t < T.medal - 0.1 || t > HEAD['v0.1.0-beta'] + 0.2) return;
  const e = ERA.dark, ty = TY.dark, m = ST.medal;
  const words = m.headline.split(' ');
  const half = Math.ceil(words.length / 2);
  const out = { start: T.medalOut, stagger: 0.03, dur: 0.55 };
  reveal(ctx, layoutText(words.slice(0, half).join(' ') + '\n' + words.slice(half).join(' '), ty.hero, { lineHeight: 1.0 }), COL - 6, 280, t,
    { unit: 'word', start: T.medal, stagger: 0.1, dur: 1.1, color: e.ink, tracking: -0.02, out });
  const first = D.medal_history.first_commit;
  reveal(ctx, layoutText('FIRST COMMIT', ty.label), COL, 470, t, { unit: 'line', start: 9.6, dur: 0.8, color: e.ink2, tracking: 0.14, out });
  reveal(ctx, layoutText(longDate(first.date), ty.date), COL, 520, t, { unit: 'glyph', start: 9.7, stagger: 0.025, dur: 0.8, color: e.ink, out });
  const authors = D.medal_history.authors;
  reveal(ctx, layoutText('A Luau decompiler by', ty.credit), COL, 600, t, { unit: 'line', start: 10.3, dur: 0.9, color: e.ink2, out });
  reveal(ctx, layoutText(authors.join(' and '), ty.credit2, { maxWidth: COLW }), COL, 642, t, { unit: 'line', start: 10.45, dur: 0.9, color: e.ink, out });
  const url = (D.medal_history.fork_repo?.html_url || '').replace(/^https?:\/\//, '');
  if (url) reveal(ctx, layoutText(url, ty.url), COL, 700, t, { unit: 'line', start: 11.0, dur: 0.9, color: e.ink2, out });
}

// the betas and July, as a terminal: everything is monospace and every change rolls in place
function textTerminal(ctx, t) {
  const t0 = HEAD['v0.1.0-beta'];
  if (t < t0 - 0.1) return;
  const e = ERA.dark, ty = TY.dark;
  const rel = TERM.filter((r) => r.head <= t);
  if (!rel.length) return;
  const k = rel.length - 1, cur = rel[k], prev = rel[k - 1];

  // the prompt and the binary's own version line
  const P1 = layoutText('$ luau-lifter --version', ty.prompt);
  typewriter(ctx, P1, COL, 150, clamp((t - t0) * 38, 0, P1.glyphCount), { color: e.ink2 });
  const verPrev = prev ? prev.stage.cli?.version || '' : '';
  const ver = cur.stage.cli?.version || '';
  if (t > t0 + 0.55) {
    if (!prev) typewriter(ctx, layoutText(ver, ty.prompt), COL, 184, clamp((t - t0 - 0.55) * 40, 0, ver.length), { color: e.ink });
    else rollText(ctx, verPrev, ver, COL, 184, ty.prompt, t, cur.head, { color: e.ink });
    const idxOf = (r) => `${String(r.index).padStart(2, '0')} / ${D.totals.releases}`;
    const idx = idxOf(cur), ix = COL + COLW - idx.length * measure('0', ty.prompt);
    if (!prev) typewriter(ctx, layoutText(idx, ty.prompt), ix, 184, clamp((t - t0 - 0.9) * 30, 0, idx.length), { color: e.ink2 });
    else rollText(ctx, idxOf(prev), idx, ix, 184, ty.prompt, t, cur.head, { color: e.ink2 });
  }

  // the release name, then its date; both roll from the previous release
  const nameFrom = prev ? prev.stage.name : '';
  if (!prev) reveal(ctx, layoutText(cur.stage.name, ty.title), COL - 4, 318, t, { unit: 'glyph', start: t0 + 0.2, stagger: 0.035, dur: 0.7, color: e.ink });
  else rollText(ctx, nameFrom, cur.stage.name, COL - 4, 318, ty.title, t, cur.head, { color: e.ink, dur: 0.6, stagger: 0.05 });
  const medalDate = D.medal_history.first_commit.date.slice(0, 10);
  if (!prev) {
    if (t < t0 + 0.8) typewriter(ctx, layoutText(medalDate, ty.mdate), COL, 384, clamp((t - t0 - 0.35) * 40, 0, 10), { color: e.ink2 });
    else rollText(ctx, medalDate, cur.stage.date, COL, 384, ty.mdate, t, t0 + 1.0, { color: e.ink, dur: 0.7, stagger: 0.07 });
  } else rollText(ctx, prev.stage.date, cur.stage.date, COL, 384, ty.mdate, t, cur.head + 0.1, { color: e.ink, dur: 0.6, stagger: 0.06 });

  // the headline types in like a commit message; 0.8's is the line of the month
  const headT = cur.head + (prev ? 0.3 : 1.5);
  const nextHead = TERM[k + 1]?.head ?? Infinity;
  const fadeOut = 1 - clamp((t - (nextHead - 0.25)) / 0.2);
  if (cur.big) {
    const L = layoutText(stripDot(cur.stage.headline).replace(', ', ',\n') + '.', ty.big, { lineHeight: 1.05 });
    reveal(ctx, L, COL - 4, 520, t, { unit: 'glyph', start: cur.head + 0.4, stagger: 0.045, dur: 0.6, color: e.ink, out: { start: nextHead - 0.5, stagger: 0.01, dur: 0.4 } });
  } else if (fadeOut > 0) {
    const L = layoutText('# ' + cur.stage.headline, ty.head, { maxWidth: COLW, lineHeight: 1.32 });
    ctx.save();
    ctx.globalAlpha *= fadeOut;
    typewriter(ctx, L, COL, 452, clamp((t - headT) * 62, 0, L.glyphCount), { color: e.ink, caret: t < headT + L.glyphCount / 62 + 0.8, caretAlpha: Math.floor(t * 2.2) % 2 ? 0.8 : 0.25 });
    ctx.restore();
  }
  drawCalendar(ctx, t);
}

let TERM = null; // terminal-era releases: { stage, head, index, big }

function buildCalendar() {
  const betas = TERM.filter((r) => r.stage.date.slice(0, 7) === TERM[0].stage.date.slice(0, 7));
  const first = new Date(betas[0].stage.date + 'T00:00:00Z'), lastD = new Date(betas[betas.length - 1].stage.date + 'T00:00:00Z');
  const days = [];
  for (let d = new Date(first); d <= lastD; d.setUTCDate(d.getUTCDate() + 1)) {
    const iso = d.toISOString().slice(0, 10);
    days.push({ iso, day: d.getUTCDate(), wd: weekday(iso), rel: betas.filter((r) => r.stage.date === iso) });
  }
  const releaseDays = days.filter((d) => d.rel.length).length;
  return { betas, days, title: `${cap(word(releaseDays))} days, ${word(betas.length)} betas.`, releaseDays };
}

function drawCalendar(ctx, t) {
  if (t < T.cal - 0.1 || t > T.calOut + 0.8) return;
  const e = ERA.dark, ty = TY.dark;
  const a = ease.out(clamp((t - T.cal) / 0.8)) * (1 - ease.inOut(clamp((t - T.calOut) / 0.6)));
  if (a <= 0) return;
  const drop = ease.inOut(clamp((t - T.calOut) / 0.6)) * 24;
  ctx.save();
  ctx.globalAlpha *= a;
  ctx.translate(0, drop);
  reveal(ctx, layoutText(CAL.title, ty.cal), COL, 584, t, { unit: 'word', start: T.cal + 0.1, stagger: 0.08, dur: 0.8, color: e.ink });
  const cellW = 112, gap = (COLW - cellW * CAL.days.length) / (CAL.days.length - 1), top = 604, cellH = 182;
  const current = CAL.betas.filter((r) => r.head <= t).pop();
  CAL.days.forEach((d, i) => {
    const x = COL + i * (cellW + gap);
    const ci = clamp((t - T.cal - 0.2 - i * 0.06) / 0.6);
    if (ci <= 0) return;
    const isCur = current && current.stage.date === d.iso;
    const passed = current && d.iso <= current.stage.date;
    const px = pixel(ctx);
    ctx.save();
    ctx.globalAlpha *= ease.out(ci);
    // cell
    ctx.fillStyle = rgba(e.ink, isCur ? 0.075 : 0.025);
    roundRect(ctx, x, top, cellW, cellH, 8);
    ctx.strokeStyle = rgba(e.ink, isCur ? 0.4 : 0.12);
    ctx.lineWidth = Math.max(1, px);
    ctx.beginPath();
    if (ctx.roundRect) ctx.roundRect(x + px / 2, top + px / 2, cellW - px, cellH - px, 8); else ctx.rect(x, top, cellW, cellH);
    ctx.stroke();
    drawText(ctx, layoutText(d.wd, ty.wday), x + 14, top + 30, { color: e.ink2, tracking: 0.08 });
    drawText(ctx, layoutText(String(d.day), ty.day), x + 12, top + 80, { color: passed ? e.ink : e.ink3, alpha: passed ? 1 : 0.6 });
    // a chip for each release that day, landing as it ships
    d.rel.forEach((r, j) => {
      const q = clamp((t - r.head) / 0.45);
      if (q <= 0) return;
      const label = r.stage.name.replace(/^beta /, '');
      const L = layoutText(label, ty.chip);
      const cy = top + 96 + j * 27;
      ctx.save();
      ctx.globalAlpha *= ease.out(q);
      ctx.translate(0, (1 - ease.outBack(q)) * -10);
      const isNew = current === r;
      ctx.fillStyle = rgba(e.ink, isNew ? 0.92 : 0.12);
      roundRect(ctx, x + 12, cy, L.width + 16, 23, 11.5);
      drawText(ctx, L, x + 20, cy + 16.5, { color: isNew ? e.surface : e.ink });
      ctx.restore();
    });
    ctx.restore();
  });
  ctx.restore();
}

// one release block for the page-style eras (V2, V2.1, V2.5, V2.6): kicker, name, headline
function pageBlock(ctx, t, era, list, opts = {}) {
  const e = ERA[era], ty = TY[era];
  const shown = list.filter((r) => r.head <= t);
  if (!shown.length) return null;
  const k = shown.length - 1, cur = shown[k];
  const titleF = opts.titleFont || ty.title;
  const y = opts.y || { meta: 150, tovek: 214, title: 420, head: 506 };
  // the date line and the release count
  list.forEach((r, i) => {
    const next = list[i + 1];
    const out = next ? { start: next.head - 0.35, stagger: 0.01, dur: 0.35 } : undefined;
    const same = i > 0 && list[i - 1].meta === r.meta;
    if (!same) reveal(ctx, layoutText(r.meta, ty.meta), COL, y.meta, t, { unit: 'line', start: r.head, dur: 0.8, color: opts.metaColor || e.ink2, tracking: opts.metaTracking || 0, out: next && next.meta !== r.meta ? out : undefined });
    reveal(ctx, layoutText(r.name, titleF), COL - 8, y.title, t, { unit: 'glyph', start: r.head + 0.1, stagger: 0.05, dur: 0.85, color: e.ink, tracking: opts.titleTracking ?? -0.03, out });
    const H = layoutText(r.stage.headline, opts.headFont || ty.head, { maxWidth: opts.headWidth || COLW, lineHeight: 1.12 });
    const hOut = opts.headOut ? { start: opts.headOut, stagger: 0.04, dur: 0.45 } : out;
    reveal(ctx, H, COL, y.head, t, { unit: 'word', start: r.head + 0.45, stagger: 0.06, dur: 0.8, color: opts.headColor || e.ink2, out: hOut });
  });
  reveal(ctx, layoutText('Tovek', ty.tovek), COL - 2, y.tovek, t, { unit: 'line', start: list[0].head, dur: 0.8, color: e.ink, tracking: -0.01 });
  return cur;
}

function textV2(ctx, t) {
  const e = ERA.v2, ty = TY.v2, s = ST['v2-v0.1'];
  const list = [{ stage: s, head: HEAD['v2-v0.1'], name: s.name, meta: `${usDate(s.date)}  ·  Release ${RIDX.get(s)} of ${D.totals.releases}` }];
  pageBlock(ctx, t, 'v2', list, { titleTracking: -0.05, headFont: HEAD_FIT.v2 });
  // anonymous bindings across the corpus, from the release notes
  const n = N.v2;
  if (n) {
    const t0 = HEAD['v2-v0.1'] + 1.4;
    const a = ease.out(clamp((t - t0) / 0.6));
    if (a > 0) {
      ctx.save();
      ctx.globalAlpha *= a;
      drawOdometer(ctx, t, { from: n.from, to: n.to, start: t0 + 0.3, dur: 2.2, x: COL - 4, y: 690, font: ty.num, color: e.ink, turns: 1 });
      drawText(ctx, layoutText(`${n.label}, down from ${formatNumber(n.from)}`, ty.unit), COL, 734, { color: e.ink2 });
      ctx.restore();
    }
  }
}

function textV21(ctx, t) {
  const e = ERA.v21, ty = TY.v21;
  const s1 = ST['v2.1'], s2 = ST['v2.1.1'];
  const list = [
    { stage: s1, head: HEAD['v2.1'], name: s1.name, meta: `RELEASE NOTES  ·  ${usDate(s1.date).toUpperCase()}` },
    { stage: s2, head: HEAD['v2.1.1'], name: s2.name, meta: `RELEASE NOTES  ·  ${usDate(s2.date).toUpperCase()}` },
  ];
  pageBlock(ctx, t, 'v21', list, { titleFont: TITLE_FIT.v21, headFont: HEAD_FIT.v21, metaColor: e.accent, metaTracking: 0.12, titleTracking: -0.035 });
  // V2 against V2.1 on one game, one thread: two timers racing
  const n = N.v21;
  if (n) {
    const t0 = HEAD['v2.1'] + 1.4, race = 2.6;
    const a = ease.out(clamp((t - t0) / 0.6)) * (1 - ease.inOut(clamp((t - (HEAD['v2.1.1'] - 0.4)) / 0.4)));
    if (a > 0) {
      ctx.save();
      ctx.globalAlpha *= a;
      drawText(ctx, layoutText(`${formatNumber(n.scripts)}-script game, one thread`, ty.unit), COL, 600, { color: e.ink2 });
      const wMax = 440;
      const rows = [{ label: 'V2', secs: n.v2, color: e.ink3 }, { label: s1.name, secs: n.v21, color: e.fill }];
      rows.forEach((r, i) => {
        const y = 650 + i * 62;
        const elapsed = clamp((t - t0 - 0.4) / race) * n.v2;
        const run = Math.min(elapsed, r.secs);
        drawText(ctx, layoutText(r.label, ty.num), COL, y + 10, { color: e.ink });
        ctx.fillStyle = rgba(e.ink, 0.08);
        ctx.fillRect(COL + 110, y - 6, wMax, 22);
        ctx.fillStyle = r.color;
        ctx.fillRect(COL + 110, y - 6, (wMax * run) / n.v2, 22);
        drawCounter(ctx, run, COL + COLW, y + 10, ty.num, { align: 'right', decimals: 1, suffix: ' s', color: run >= r.secs && i === 1 ? e.accent : e.ink });
      });
      ctx.restore();
    }
  }
  // 1,500 damaged scripts: the ones V2 aborted on, then none
  const d = N.v211;
  if (d) {
    const t0 = HEAD['v2.1.1'] + 0.9;
    const a = ease.out(clamp((t - t0) / 0.6));
    if (a > 0) {
      ctx.save();
      ctx.globalAlpha *= a;
      const cols = 75, rows = Math.ceil(d.scripts / cols), step = 8, r0 = 2;
      const fixed = ease.inOut(clamp((t - t0 - 1.5) / 0.9));
      for (let i = 0; i < d.scripts; i++) {
        const cx = COL + 3 + (i % cols) * step, cy = 568 + Math.floor(i / cols) * step;
        const bad = ABORTED.has(i);
        ctx.fillStyle = bad ? mix(e.accent, e.ink3, fixed) : e.ink3;
        ctx.globalAlpha = a * (bad ? lerp(1, 0.35, fixed) : 0.35);
        const rr = bad ? lerp(r0 * 1.5, r0, fixed) : r0;
        ctx.fillRect(cx - rr, cy - rr, rr * 2, rr * 2);
      }
      ctx.globalAlpha = a;
      const yb = 568 + rows * step + 30;
      drawText(ctx, layoutText(`${formatNumber(d.scripts)} damaged scripts`, ty.unit), COL, yb, { color: e.ink2 });
      const msg = layoutText(fixed < 0.5 ? `V2 aborted ${formatNumber(d.aborted)} times` : `${s2.name}: never`, ty.num);
      drawText(ctx, msg, COL + COLW, yb, { color: fixed < 0.5 ? e.ink : e.accent, align: 'right' });
      ctx.restore();
    }
  }
}

let ABORTED = new Set();

function textV25(ctx, t) {
  const e = ERA.v25, ty = TY.v25;
  const s1 = ST['v2.5'], s2 = ST['v2.5.1'];
  const list = [
    { stage: s1, head: HEAD['v2.5'], name: s1.name, meta: usDate(s1.date) },
    { stage: s2, head: HEAD['v2.5.1'], name: s2.name, meta: usDate(s2.date) },
  ];
  pageBlock(ctx, t, 'v25', list, { titleFont: TITLE_FIT.v25, headFont: HEAD_FIT.v25, titleTracking: -0.025 });
  // semantic fuzzing: wrong or missing outputs, V2.1.1 to V2.5
  const n = N.v25;
  if (n) {
    const t0 = HEAD['v2.5'] + 1.0;
    const a = ease.out(clamp((t - t0) / 0.6)) * (1 - ease.inOut(clamp((t - (HEAD['v2.5.1'] - 0.4)) / 0.4)));
    if (a > 0) {
      ctx.save();
      ctx.globalAlpha *= a;
      const landed = clamp((t - t0 - 2.9) / 0.5);
      drawOdometer(ctx, t, { from: n.from, to: n.to, start: t0 + 0.4, dur: 2.6, x: COL - 6, y: 700, font: ty.num, color: landed > 0 ? mix(e.ink, e.accent, landed) : e.ink, turns: 1 });
      drawText(ctx, layoutText('wrong or missing outputs in semantic fuzzing', ty.unit), COL, 746, { color: e.ink2 });
      drawText(ctx, layoutText(`${n.fromLabel} → ${s1.name}`, ty.unit), COL, 778, { color: e.ink2, alpha: 0.85 });
      ctx.restore();
    }
  }
  const c = N.v251;
  if (c) {
    const t0 = HEAD['v2.5.1'] + 0.7;
    const a = ease.out(clamp((t - t0) / 0.6));
    if (a > 0) {
      ctx.save();
      ctx.globalAlpha *= a;
      drawOdometer(ctx, t, { from: c.from ?? c.to, to: c.to, start: t0 + 0.3, dur: 1.8, x: COL - 6, y: 700, font: ty.num, color: e.ink, turns: 1 });
      drawText(ctx, layoutText(c.label, ty.unit, { maxWidth: COLW }), COL, 746, { color: e.ink2 });
      ctx.restore();
    }
  }
}

function textV26(ctx, t) {
  if (t > T.orig + 0.9) return;
  const e = ERA.v26, ty = TY.v26, s = V26_STAGE;
  const leave = ease.inOut(clamp((t - T.orig) / 0.7));
  ctx.save();
  ctx.globalAlpha *= 1 - leave;
  const meta = `${(s.released ? usDate(s.date) : monthYear(s.date)).toUpperCase()}  ·  RELEASE ${RIDX.get(s)} OF ${D.totals.releases}`;
  const list = [{ stage: s, head: HEAD.last, name: s.name, meta }];
  pageBlock(ctx, t, 'v26', list, { metaTracking: 0.12, titleTracking: -0.03, headWidth: 470, headColor: e.ink, headOut: T.expr - 0.5, y: { meta: 140, tovek: 206, title: 404, head: 486 } });

  // the helpers Luau pasted, checked off as their copies come home
  const tl0 = T.homing0 + 0.2, tl1 = T.expr - 0.3;
  const la = ease.out(clamp((t - tl0) / 0.6)) * (1 - ease.inOut(clamp((t - tl1) / 0.4)));
  if (la > 0) {
    ctx.save();
    ctx.globalAlpha *= la;
    drawText(ctx, layoutText('PASTED BY LUAU -O2', ty.small), COL, 640, { color: e.ink2, tracking: 0.12 });
    HOMING.helpers.forEach((h, i) => {
      const y = 690 + i * 44;
      const n = h.copies.length;
      drawText(ctx, layoutText(h.name, ty.list), COL, y, { color: e.ink });
      drawText(ctx, layoutText(`${n} ${n === 1 ? 'copy' : 'copies'}`, ty.list), COL + 190, y, { color: e.ink2 });
      const arriveT = T.homing0 + h.arrive * (T.homing1 - T.homing0);
      const q = ease.out(clamp((t - arriveT) / 0.5));
      if (q > 0) {
        const L = layoutText(`→ ${n} ${n === 1 ? 'call' : 'calls'}`, ty.list);
        reveal(ctx, L, COL + 340, y, t, { unit: 'word', start: arriveT, stagger: 0.08, dur: 0.6, color: e.accent });
      }
    });
    ctx.restore();
  }

  // folded constants, solved back
  const xa = ease.out(clamp((t - T.expr) / 0.5)) * (1 - ease.inOut(clamp((t - (T.nums - 0.3)) / 0.4)));
  if (xa > 0) {
    ctx.save();
    ctx.globalAlpha *= xa;
    drawText(ctx, layoutText('FOLDED BY LUAU, SOLVED BACK', ty.small), COL, 500, { color: e.ink2, tracking: 0.12 });
    HOMING.exprs.forEach((x, i) => {
      const y = 566 + i * 132, t0 = x.t0;
      const A = layoutText(x.from, ty.expA);
      typewriter(ctx, A, COL, y, clamp((t - t0 + 0.6) * 48, 0, A.glyphCount), { color: e.ink2 });
      const strike = ease.inOut(clamp((t - t0 - 0.2) / 0.45));
      if (strike > 0) hairline(ctx, COL, y - 8, A.width * strike, e.ink2, 0.9);
      reveal(ctx, layoutText(x.to, ty.expB), COL - 2, y + 62, t, { unit: 'glyph', start: t0 + 0.45, stagger: 0.05, dur: 0.7, color: e.accent });
    });
    ctx.restore();
  }

  // the field: rebuilt calls across four real games, and fresh fuzzing
  const na = ease.out(clamp((t - T.nums) / 0.5));
  if (na > 0 && N.v26a) {
    const n = N.v26a, z = N.v26b;
    const swap = T.nums + 2.9;
    ctx.save();
    ctx.globalAlpha *= na;
    const a1 = 1 - ease.inOut(clamp((t - swap) / 0.4));
    if (a1 > 0) {
      ctx.save();
      ctx.globalAlpha *= a1;
      drawOdometer(ctx, t, { from: n.from ?? 0, to: n.to, start: T.nums + 0.2, dur: 2.0, x: COL - 6, y: 640, font: ty.num, color: e.ink, turns: 1 });
      const Ln = layoutText(n.label, ty.unit, { maxWidth: 480, lineHeight: 1.3 });
      drawText(ctx, Ln, COL, 690, { color: e.ink2 });
      if (n.paren) drawText(ctx, layoutText(n.paren, ty.unit), COL, 690 + Ln.height + 32, { color: e.ink2 });
      ctx.restore();
    }
    if (z && t > swap) {
      const q = ease.out(clamp((t - swap - 0.2) / 0.6));
      ctx.save();
      ctx.globalAlpha *= q;
      drawCounter(ctx, z.to, COL - 6, 640, ty.num, { color: e.accent });
      const Lz = layoutText(z.label, ty.unit, { maxWidth: 480, lineHeight: 1.3 });
      drawText(ctx, Lz, COL, 690, { color: e.ink2 });
      if (z.paren) drawText(ctx, layoutText(z.paren, ty.unit), COL, 690 + Lz.height + 32, { color: e.ink2 });
      ctx.restore();
    }
    ctx.restore();
  }
  ctx.restore();
}

let RIDX = null;        // stage -> release number (1..17)
let REPO = '';          // the Tovek repository, read from the release binaries' sources
let TITLE_FIT = null;   // per-era title fonts sized so the longest release name fits the column
let HEAD_FIT = null;    // per-era headline fonts sized so every headline of the era fits one line

// ------------------------------------------------------------------- beside the original source

function buildSide() {
  const v = V26_STAGE.excerpts.shield.text, o = D.sample.focus_source.text;
  const plan = codeMorph(v, o);
  const kinds = new Set([...plan.removed.map((r) => r.k), ...[...plan.inserted].map((i) => plan.b.tokens[i].k)]);
  const onlyNames = [...kinds].every((k) => ['id', 'fn', 'prop', 'glob', 'num', 'com'].includes(k));
  const size = 14, lineHeight = 1.36;
  const cm = codeMetrics(size, lineHeight);
  const removedRuns = [];
  for (const r of plan.removed) {
    const last = removedRuns[removedRuns.length - 1];
    const c1 = r.col + r.text.length;
    if (last && last.line === r.line && r.col - last.c1 <= 1) last.c1 = Math.max(last.c1, c1);
    else removedRuns.push({ line: r.line, c0: r.col, c1 });
  }
  return { plan, size, lineHeight, cm, onlyNames, kept: plan.stats.kept, total: plan.b.tokens.length, removedRuns, insertedRuns: washRunsWith(plan.b, plan.inserted) };
}

function washRunsWith(L, set) {
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

function drawSide(ctx, t) {
  if (t < T.orig + 0.2) return;
  const e = ERA.v26, ty = TY.v26, S2 = SIDE;
  const a = ease.out(clamp((t - T.orig - 0.3) / 0.8));
  const { cw, lh } = S2.cm;
  const top = 236;
  const panels = [
    { x: COL, L: S2.plan.b, label: `Original source  ·  ${D.sample.file}`, runs: S2.insertedRuns, first: D.sample.focus_source.first_line },
    { x: 1000, L: S2.plan.a, label: `Tovek ${V26_STAGE.name}`, runs: S2.removedRuns },
  ];
  ctx.save();
  ctx.globalAlpha *= a;
  // the count of tokens that came back exactly
  const t0 = T.orig + 0.8;
  const kept = Math.round(lerp(0, S2.kept, ease.out(clamp((t - t0) / 1.6))));
  const statL = layoutText(`of ${S2.total} tokens come back exactly.`, ty.stat);
  const cwv = drawCounter(ctx, kept, COL - 2, 130, ty.stat, { color: e.accent });
  drawText(ctx, statL, COL - 2 + Math.max(cwv, 92) + 16, 130, { color: e.ink });
  const washA = ease.out(clamp((t - T.orig - 2.4) / 0.6));
  panels.forEach((pn, i) => {
    const pa = ease.out(clamp((t - T.orig - 0.4 - i * 0.15) / 0.8));
    ctx.save();
    ctx.globalAlpha *= pa;
    ctx.translate((1 - pa) * (i === 0 ? -24 : 24), 0);
    drawText(ctx, layoutText(pn.label.toUpperCase(), ty.side), pn.x, top - 26, { color: e.ink2, tracking: 0.1 });
    hairline(ctx, pn.x, top - 14, 800, e.ink, 0.16);
    ctx.save();
    ctx.translate(pn.x, top);
    if (washA > 0) {
      ctx.save();
      ctx.globalAlpha *= washA;
      ctx.fillStyle = rgba(e.ink, 0.085);
      for (const r of pn.runs) roundRect(ctx, r.c0 * cw - cw * 0.25, r.line * lh + lh * 0.1, (r.c1 - r.c0) * cw * ease.out(washA) + cw * 0.5, lh * 0.8, lh * 0.16);
      ctx.restore();
    }
    drawCode(ctx, pn.L, { size: S2.size, lineHeight: S2.lineHeight, palette: PAL.v26, lineAlpha: (l) => ease.out(clamp((t - T.orig - 0.5 - l * 0.012 - i * 0.15) / 0.5)) });
    ctx.restore();
    ctx.restore();
  });
  ctx.restore();
}

// ------------------------------------------------------------------------------------ the end

function drawEnd(ctx, t) {
  const e = ERA.end, ty = TY.end;
  const t0 = T.end + 0.2;
  const tt = D.totals;
  const items = [
    { v: tt.releases, label: 'releases' },
    { v: tt.days_since_first_beta, label: `days since ${ST['v0.1.0-beta'].name}` },
    { v: tt.commits, label: 'commits' },
  ];
  const outT = T.end + 3.0;
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
  // the mark lands with its eight bits, then the name
  const mt = outT + 0.5;
  if (t > mt - 0.1) {
    const size = 120 * (0.94 + 0.06 * spring(t - mt, { freq: 1.1, damping: 0.6 }));
    drawMark(ctx, 960 - markWidth(size) / 2, 300 + (120 - size) / 2, size, { color: e.ink, bitColor: e.accent, body: progress(t, mt, 0.8, ease.out), bits: progress(t, mt + 0.25, 1.3) });
    reveal(ctx, layoutText(`Tovek ${V26_STAGE.name}`, ty.card), 960, 610, t, { unit: 'word', start: mt + 0.5, stagger: 0.14, dur: 1.1, align: 'center', color: e.ink, tracking: -0.02 });
    const url = layoutText(REPO, ty.url);
    const typed = clamp((t - mt - 1.3) * 28, 0, url.glyphCount);
    const caretOn = typed < url.glyphCount || Math.floor((t - mt) * 1.7) % 2 === 0;
    typewriter(ctx, url, 960 - url.width / 2, 700, typed, { color: e.ink2, caret: t > mt + 1.2, caretAlpha: caretOn ? 0.8 : 0 });
    const credit = layoutText(`Built on medal by ${D.medal_history.authors.join(' and ')}`.toUpperCase(), ty.credit);
    drawText(ctx, credit, 960, 880, { color: e.ink2, align: 'center', tracking: 0.12, alpha: ease.out(clamp((t - mt - 1.8) / 0.9)) });
  }
}

// ------------------------------------------------------------------------------ eras and turns

function drawEra(ctx, t, era) {
  const e = ERA[era];
  ctx.fillStyle = e.surface;
  ctx.fillRect(-4, -4, 1928, 1088);
  if (era === 'end') { drawEnd(ctx, t); return; }
  if (era === 'dark') drawOpen(ctx, t);
  if (t < T.orig + 0.9) {
    ctx.save();
    if (era === 'v26') ctx.globalAlpha *= 1 - ease.inOut(clamp((t - T.orig) / 0.7));
    drawCodeTrack(ctx, t, era);
    ctx.restore();
  }
  if (era === 'dark') { textMedal(ctx, t); textTerminal(ctx, t); }
  else if (era === 'v2') textV2(ctx, t);
  else if (era === 'v21') textV21(ctx, t);
  else if (era === 'v25') textV25(ctx, t);
  else if (era === 'v26') { textV26(ctx, t); drawSide(ctx, t); }
  drawLedger(ctx, t, era);
}

// ------------------------------------------------------------------------------- warming up
// The first time a scene's type is drawn at the stage's pixel size, the GPU rasterises its glyphs:
// 20-70 ms for that one frame. In idle time, ahead of the playhead, the film draws its own frames on
// a hidden canvas of the same size, so the glyphs are ready before playback reaches them. This
// only fills caches; it never changes what a frame looks like.

const WARM_STEP = 0.2;
let warm = null;

function warmUp(ctx, t) {
  const c = ctx.canvas;
  if (typeof document === 'undefined' || !c || c.width < 640 || !('requestAnimationFrame' in globalThis)) return;
  if (ctx.getContextAttributes && ctx.getContextAttributes().willReadFrequently) return; // the export's CPU canvas
  if (warm && warm.w === c.width && warm.h === c.height) { warm.lastT = t; return; }
  if (warm && warm.w * warm.h > c.width * c.height && !warm.finished) return; // keep the larger stage
  // an OffscreenCanvas: transferToImageBitmap() submits its GPU work without reading pixels back
  const canvas = typeof OffscreenCanvas !== 'undefined' ? new OffscreenCanvas(c.width, c.height) : null;
  if (!canvas || !canvas.transferToImageBitmap) return;
  const wctx = canvas.getContext('2d', { alpha: false });
  const n = Math.ceil(T.duration / WARM_STEP);
  warm = { w: c.width, h: c.height, canvas, wctx, done: new Uint8Array(n), left: n, lastT: t, finished: false };
  const idle = globalThis.requestIdleCallback || ((cb) => setTimeout(() => cb({ timeRemaining: () => 8 }), 40));
  const W = warm;
  const step = (deadline) => {
    if (warm !== W) return;
    while (W.left > 0 && deadline.timeRemaining() > 5) {
      let i = Math.max(0, Math.floor(W.lastT / WARM_STEP));
      while (i < n && W.done[i]) i++;
      if (i >= n) { i = 0; while (i < n && W.done[i]) i++; }
      W.done[i] = 1;
      W.left--;
      W.wctx.setTransform(W.w / 1920, 0, 0, W.h / 1080, 0, 0);
      try { render(W.wctx, i * WARM_STEP, 1920, 1080, true); W.canvas.transferToImageBitmap().close(); } catch { W.left = 0; }
    }
    if (W.left > 0) idle(step, { timeout: 500 });
    else { W.finished = true; W.canvas.width = W.canvas.height = 1; }
  };
  idle(step, { timeout: 500 });
}

function render(ctx, t, w, h, warming = false) {
  if (!warming) warmUp(ctx, t);
  const live = ERA_SPANS.filter((s) => t >= s.t0 && t < s.t1);
  if (live.length < 2) { drawEra(ctx, t, (live[0] || ERA_SPANS[ERA_SPANS.length - 1]).id); return; }
  const [from, to] = live;
  const p = ease.inOutCubic(clamp((t - to.t0) / (from.t1 - to.t0)));
  // Turn edges sit on whole device pixels: a clip with a fractional edge makes the GPU build a
  // coverage mask the size of the canvas the first time, a 50 ms frame in the middle of the turn.
  const px = pixel(ctx);
  const snap = (v) => Math.round(v / px) * px;
  if (to.id === 'end') {
    // the last turn rises from the bottom like a curtain
    const edge = snap(lerp(h + 2, -2, p));
    clipRect(ctx, { x: 0, y: 0, w, h: edge }, () => drawEra(ctx, t, from.id));
    clipRect(ctx, { x: 0, y: edge, w, h: h - edge }, () => drawEra(ctx, t, to.id));
    hairline(ctx, 0, edge, w, ERA[from.id].ink, 0.25);
    return;
  }
  // a page turn: the next era's page slides in from the right edge
  const edge = snap(lerp(w + 2, -2, p));
  clipRect(ctx, { x: 0, y: 0, w: edge, h }, () => drawEra(ctx, t, from.id));
  clipRect(ctx, { x: edge, y: 0, w: w - edge, h }, () => drawEra(ctx, t, to.id));
  ctx.save();
  ctx.fillStyle = rgba(ERA[to.id].ink, 0.22);
  ctx.fillRect(edge, 0, Math.max(1, pixel(ctx)), h);
  ctx.restore();
}

// --------------------------------------------------------------------------------- the film

const story = {
  id: 'story',
  title: 'One function, seventeen releases',
  description: 'The same function, decompiled by medal and by every Tovek release.',
  duration: T.duration,
  poster: 5.9,
  background: V26.night,
  fonts: Object.values(TY).flatMap((set) => Object.values(set)).filter((x) => !/Inter|Hanken|Google/.test(x.family)).map((x) => x.css),
  glyphs: 'Tovek 0123456789 →·—–…×#$-_/:()[]',
  chapters: [{ t: 0, title: 'One script, as bytes' }],
  captions: [],
  score: undefined,

  async prepare() {
    if (D) return;
    await loadEraFonts();
    D = await (await fetch(new URL('../data/story.json', import.meta.url))).json();
    ORDER = D.stages;
    ST = Object.fromEntries(ORDER.map((s) => [s.tag, s]));
    V26_STAGE = ORDER[ORDER.length - 1];
    ST.last = V26_STAGE;
    RIDX = new Map(ORDER.slice(1).map((s, i) => [s, i + 1]));
    REPO = ((ORDER.find((s) => /github\.com\/[\w.-]+\/[\w.-]+\/releases/.test(s.tool || ''))?.tool || '').match(/github\.com\/[\w.-]+\/[\w.-]+/) || [''])[0];
    CM = codeMetrics(CODE.size, CODE.lineHeight);
    PAL = {
      dark: codePalettes.night,
      v2: makePalette(ERA.v2, 'rgba(198,241,53,0.85)'),
      v21: makePalette(ERA.v21, 'rgba(217,119,87,0.2)'),
      v25: makePalette(ERA.v25, 'rgba(157,210,255,0.2)'),
      v26: codePalettes.paper,
      end: codePalettes.night,
    };
    PAL.v2.accent = ERA.v2.ink;

    TITLE_FIT = {
      v21: font(Math.min(TY.v21.title.size, ...['v2.1', 'v2.1.1'].map((g) => fitSize(ST[g].name, TY.v21.title, COLW))), { family: FAM.hanken, weight: 600 }),
      v25: font(Math.min(TY.v25.title.size, ...['v2.5', 'v2.5.1'].map((g) => fitSize(ST[g].name, TY.v25.title, COLW))), { family: FAM.gsans, weight: 500 }),
    };

    const headFit = (era, tags, family, weight) => font(Math.max(38, Math.min(TY[era].head.size, ...tags.map((g) => fitSize(ST[g].headline, TY[era].head, COLW)))), { family, weight });
    HEAD_FIT = {
      v2: headFit('v2', ['v2-v0.1'], FAM.inter, 500),
      v21: headFit('v21', ['v2.1', 'v2.1.1'], FAM.hanken, 500),
      v25: headFit('v25', ['v2.5', 'v2.5.1'], FAM.gsans, 400),
    };

    // the terminal era: every release from beta 0.1 to the last one before V2
    const termTags = ['v0.1.0-beta', 'v0.2.0-beta', 'v0.3.0-beta', 'v0.4.0-beta', 'v0.5.0-beta', 'v0.5.1-beta', 'v0.5.2-beta', 'v0.6.0-beta', 'v0.7.0', 'v0.8', 'v0.9.0-beta'];
    TERM = termTags.filter((g) => ST[g]).map((g) => ({ stage: ST[g], head: HEAD[g], index: RIDX.get(ST[g]), big: g === 'v0.8' }));
    CAL = buildCalendar();

    // numbers from the release notes (fall back to nothing if a note is worded differently)
    N = {};
    const numText = (g, i) => ST[g]?.numbers?.[i]?.text || '';
    let m = numText('v2-v0.1', 0).match(/^(.+?) ([\d,]+) to ([\d,]+)$/);
    if (m) N.v2 = { label: m[1], from: num(m[2]), to: num(m[3]) };
    m = numText('v2.1', 0).match(/([\d,]+)-script game in ([\d.]+) s on one thread \(V2: ([\d.]+) s\)/);
    if (m) N.v21 = { scripts: num(m[1]), v21: +m[2], v2: +m[3] };
    m = numText('v2.1.1', 0).match(/([\d,]+) damaged scripts: V2 aborted ([\d,]+) times/);
    if (m) N.v211 = { scripts: num(m[1]), aborted: num(m[2]) };
    m = numText('v2.5', 0).match(/(\d[\d,]*) wrong or missing \((.+?)\) to (\d[\d,]*)/);
    if (m) N.v25 = { from: num(m[1]), fromLabel: m[2], to: num(m[3]) };
    const callsA = numText('v2.5', 1).match(/^([\d,]+) /), callsB = parseNumber(numText('v2.5.1', 0));
    if (callsB) N.v251 = { ...callsB, from: callsA ? num(callsA[1]) : undefined };
    const nn = V26_STAGE.numbers || [];
    N.v26a = nn[0] ? parseNumber(nn[0].text) : null;
    N.v26b = nn[1] ? parseNumber(nn[1].text) : null;
    if (N.v211) {
      const picks = new Set();
      for (let i = 0; picks.size < N.v211.aborted && i < N.v211.scripts * 4; i++) picks.add(Math.floor(hash01(211, i) * N.v211.scripts));
      ABORTED = picks;
    }

    HEX = buildHex();
    HOMING = analyzeHoming(codeMorph(ST['v2.5.1'].output, V26_STAGE.output));
    // the close-ups of the solved constants, in file order, after the push-in
    HOMING.exprs.forEach((x, i) => { x.t0 = T.expr + 0.6 + i * 2.6; });
    SIDE = buildSide();
    SHOTS = buildShots();
    LEDGER_EVENTS = buildLedgerEvents();
    buildChaptersCaptionsScore();
  },

  render: (ctx, t, w, h) => render(ctx, t, w, h),
};

async function loadEraFonts() {
  if (typeof document === 'undefined' || !document.fonts) return;
  if (!document.querySelector('link[data-story-fonts]')) {
    const link = document.createElement('link');
    link.rel = 'stylesheet';
    link.href = ERA_FONTS_CSS;
    link.dataset.storyFonts = '';
    const loaded = new Promise((res) => { link.onload = res; link.onerror = res; });
    document.head.append(link);
    await Promise.race([loaded, new Promise((res) => setTimeout(res, 4000))]);
  }
  const specs = Object.values(TY).flatMap((set) => Object.values(set)).filter((x) => /Inter|Hanken|Google/.test(x.family)).map((x) => x.css);
  await Promise.all(specs.map((s) => document.fonts.load(s, story.glyphs).catch(() => null)));
  await document.fonts.ready;
}

// ------------------------------------------------------------ chapters, captions and the score

function buildChaptersCaptionsScore() {
  const s = (g) => ST[g];
  const v26 = V26_STAGE;
  const hl = (g) => stripDot(s(g).headline);
  story.chapters = [
    { t: 0, title: 'One script, as bytes', still: 5.9 },
    { t: 8.0, title: stripDot(s('medal').headline), still: 14.2 },
    { t: 19.0, title: cap(s('v0.1.0-beta').name), still: 25.4 },
    { t: 26.0, title: stripDot(CAL.title), still: 37.6 },
    { t: 45.0, title: `${s('v0.8').name}: ${hl('v0.8')}`, still: 58.6 },
    { t: 63.4, title: `${s('v2-v0.1').name}: ${hl('v2-v0.1')}`, still: 70.6 },
    { t: 72.4, title: `${s('v2.1').name}: ${hl('v2.1')}`, still: 77.2 },
    { t: 82.6, title: `${s('v2.5').name}: ${hl('v2.5')}`, still: 88.4 },
    { t: 96.6, title: `${v26.name}: ${hl('last')}`, still: 104.6 },
    { t: T.orig, title: 'Beside the original', still: 126.4 },
    { t: 128.6, title: `${cap(word(D.totals.releases))} releases`, still: 135.6 },
  ];

  // names a release brought, read from the morph: the new local names in its output
  const newNames = (a, b, limit = 3) => {
    const p = codeMorph(s(a).output, s(b).output);
    const seen = [];
    for (const i of p.inserted) {
      const tk = p.b.tokens[i];
      if (tk.k === 'id' && /^[a-z]\w*[A-Z]?\w*$/.test(tk.text) && tk.text.length > 2 && !seen.includes(tk.text)) seen.push(tk.text);
    }
    return seen.slice(0, limit);
  };
  const list = (xs) => (xs.length < 2 ? xs.join('') : xs.slice(0, -1).join(', ') + ' and ' + xs[xs.length - 1]);
  const shieldLines = (g) => s(g).excerpts.shield.lines;
  const unchanged = CAL.betas.filter((r, i) => i > 0 && !r.stage.changed_from_previous).map((r) => r.stage.name.replace(/^beta /, ''));
  const helpers = Object.entries(D.sample.inlined_by_luau.helpers);
  const times = (n) => (n === 1 ? 'once' : n === 2 ? 'twice' : `${word(n)} times`);
  const fold = HOMING.exprs;
  const n = N;
  const fmt = formatNumber;
  const side = SIDE;
  const c = [];
  const add = (start, end, text) => { if (text) c.push({ start, end, text }); };
  add(1.4, 4.9, `A small Roblox script, written for this film and compiled by Luau ${(D.sample.compiler.match(/Luau ([\d.]+)/) || [])[1] || ''} with -O2.`.replace('Luau  with', 'Luau with'));
  add(5.2, 8.1, `The function we follow is ${D.sample.focus_function}.`);
  add(8.6, 12.5, 'medal is a Luau decompiler written in Rust, published as open source in their honour and memory.');
  add(12.8, 18.6, "This is medal's output for the same bytes: correct in spirit, but raw. Generated names, and a goto into an else block.");
  add(19.6, 24.2, `${cap(s('v0.1.0-beta').name)} reads the same bytes. ${s('v0.1.0-beta').points[0]}`);
  add(24.6, 26.4, `For now the loop still jumps: ${s('v0.1.0-beta').features.goto} gotos.`);
  add(26.8, 29.8, 'Watch the second function, the one that runs for each new player.');
  const orName = newNames('v0.1.0-beta', 'v0.2.0-beta', 1)[0];
  if (orName) add(30.0, 32.6, `${cap(s('v0.2.0-beta').name)} names a value after what its or expression yields: ${orName}.`);
  add(33.2, 35.8, `${cap(s('v0.4.0-beta').name)}: ${lc(s('v0.4.0-beta').points[2])}`);
  add(36.2, 38.6, `${cap(s('v0.5.0-beta').name)} keeps not (a < b): with NaN it is not the same as a >= b.`);
  if (unchanged.length) add(39.0, 44.4, `${list(unchanged)} change nothing in this script.`);
  add(45.8, 49.4, `${s('v0.7.0').name} rebuilds the pipeline, and more locals get names: ${list(newNames('v0.6.0-beta', 'v0.7.0'))}.`);
  add(49.6, 53.2, 'The loop still jumps with goto. The arrows show where each jump lands.');
  add(53.4, 57.6, `${s('v0.8').name}, the same day, removes every goto. It writes a small state machine instead.`);
  add(57.8, 60.2, `No goto, but the function grows from ${shieldLines('v0.7.0')} to ${shieldLines('v0.8')} lines.`);
  add(60.4, 63.2, `${s('v0.9.0-beta').name}: ${lc(s('v0.9.0-beta').points[0])} ${s('v0.9.0-beta').points[1]}`);
  add(65.2, 70.0, `${s('v2-v0.1').name} writes the loop with continue and break: ${shieldLines('v0.9.0-beta')} lines become ${shieldLines('v2-v0.1')}.`);
  add(70.2, 72.4, s('v2-v0.1').points[1]);
  if (n.v21) add(74.0, 78.4, `${s('v2.1').name} decompiles a ${fmt(n.v21.scripts)}-script game in ${n.v21.v21} seconds on one thread. V2 took ${n.v21.v2}.`);
  if (n.v211) add(78.9, 82.4, `Fed ${fmt(n.v211.scripts)} damaged scripts, V2 aborted ${fmt(n.v211.aborted)} times. ${s('v2.1.1').name} never does.`);
  if (n.v25) add(84.2, 88.8, `${s('v2.5').name} is checked by semantic fuzzing: wrong or missing outputs go from ${n.v25.from} to ${n.v25.to}.`);
  const ctxName = newNames('v2.1.1', 'v2.5', 1)[0];
  if (ctxName) add(89.0, 92.4, `Names come from context: the connection becomes ${ctxName}.`);
  if (n.v251) add(92.8, 96.4, `${s('v2.5.1').name}: ${fmt(n.v251.to)} ${n.v251.label}.`);
  if (helpers.length) {
    const [[h0, k0], ...rest] = helpers;
    add(98.2, 101.6, `Luau's -O2 compiler pasted ${h0} into ${D.sample.focus_function} ${times(k0)}${rest.map(([h, k]) => `, and ${h} ${times(k)}`).join('')}.`);
  }
  add(101.8, 105.6, `${v26.name} finds each pasted copy and sends it home to its helper.`);
  add(105.8, 110.0, 'In its place, a call, marked as an inferred equivalent.');
  fold.forEach((x, i) => add(+(x.t0 - 0.4).toFixed(2), +(x.t0 + 2.2).toFixed(2), i === 0 ? `Constants Luau folded are solved back: ${x.from} is ${x.to}.` : `And ${x.from} becomes ${x.to}.`));
  if (n.v26a) add(T.nums, T.nums + 2.9, `Across four real Roblox games${n.v26a.paren ? `, ${n.v26a.paren}` : ''}, rebuilt calls go from ${fmt(n.v26a.from ?? 0)} to ${fmt(n.v26a.to)}.`);
  if (n.v26b) {
    const m = n.v26b.label.match(/^(wrong outputs) in (.+)$/);
    const was = n.v26b.paren.match(/^(.+?): ([\d,]+)$/);
    add(T.nums + 3.0, T.orig, m ? `In ${m[2]}: ${n.v26b.to} ${m[1]}.${was ? ` ${was[1]} had ${was[2]}.` : ''}` : `${n.v26b.to} ${n.v26b.label}.`);
  }
  add(T.orig + 0.6, T.orig + 4.4, 'Beside the source it was compiled from.');
  add(T.orig + 4.6, 128.8, side.onlyNames ? 'Every keyword, call and operator matches. Only names, constants and comments differ.' : 'Most of it matches, token for token.');
  add(130.0, 133.0, `${cap(word(D.totals.releases))} releases in ${D.totals.days_since_first_beta} days, built on medal.`);
  add(133.4, 136.4, `${D.credits.origin.replace(/ \(.*?\)/, '')}. ${D.credits.luau.replace(/ \(.*?\)/, '')}.`);
  story.captions = c;

  // ---- the score: a pulse for each release, ticks while code types, one swell for V2.6
  const E = [];
  const D0 = T.duration;
  E.push({ t: 0, voice: 'air', dur: D0, gain: 0.028, cutoff: 760 });
  E.push({ t: 0.2, voice: 'hum', dur: 21, freq: note('A1'), gain: 0.15, attack: 3, release: 4 });
  E.push(...ticks(T.hex0, T.hex0 + 3.8, { rate: 30, seed: 2, gain: 0.012 }));
  E.push({ t: T.focus, voice: 'pulse', freq: note('E3'), gain: 0.07, dur: 3 });
  E.push({ t: T.decode0 + 0.2, voice: 'sub', freq: 72, to: 36, gain: 0.12, dur: 1.6 });
  E.push(...ticks(T.decode0 + 0.3, T.decode1 - 0.1, { rate: 22, seed: 5, gain: 0.016 }));
  E.push({ t: T.medal, voice: 'pulse', freq: note('A2'), gain: 0.12, dur: 4 });
  E.push({ t: 19.0, voice: 'hum', dur: 46, freq: note('A1'), gain: 0.075, attack: 4, release: 3, cutoff: 360 });
  const notes = {
    'v0.1.0-beta': 'A3', 'v0.2.0-beta': 'C#4', 'v0.3.0-beta': 'E4', 'v0.4.0-beta': 'B3', 'v0.5.0-beta': 'C#4',
    'v0.5.1-beta': 'E4', 'v0.5.2-beta': 'F#4', 'v0.6.0-beta': 'E4', 'v0.7.0': 'A3', 'v0.8': 'E3', 'v0.9.0-beta': 'C#4',
    'v2-v0.1': 'A2', 'v2.1': 'C#4', 'v2.1.1': 'E4', 'v2.5': 'F#3', 'v2.5.1': 'A3', 'last': 'A3',
  };
  for (const [g, nt] of Object.entries(notes)) if (HEAD[g] != null) E.push({ t: HEAD[g], voice: 'pulse', freq: note(nt), gain: g === 'last' ? 0.1 : 0.085, dur: 2.6 });
  for (const [g, [m0, m1]] of Object.entries(MORPH)) E.push(...ticks(m0 + (m1 - m0) * 0.52, m1 - 0.05, { rate: 16, seed: 7 + m0, gain: 0.026 }));
  E.push({ t: MORPH['v0.8'][1] - 0.2, voice: 'chime', freq: note('E5'), gain: 0.045, dur: 3 });
  for (const tw of [63.4, 72.4, 82.6, 96.6]) E.push({ t: tw + 0.35, voice: 'sub', freq: 64, to: 34, gain: 0.11, dur: 1.4 });
  E.push({ t: 82.6, voice: 'hum', dur: 15, freq: note('E1'), gain: 0.08, attack: 3, release: 3 });
  if (N.v25) E.push({ t: HEAD['v2.5'] + 3.9, voice: 'chime', freq: note('B5'), gain: 0.045, dur: 3 });
  E.push({ t: 96.6, voice: 'swell', dur: 25, notes: ['A2', 'E3', 'A3', 'C#4', 'E4'].map(note), gain: 0.1, attack: 5, release: 5, open: 2200 });
  HOMING.copies.forEach((cp, i) => E.push({ t: T.homing0 + cp.arrive * (T.homing1 - T.homing0), voice: 'chime', freq: note(['A5', 'C#6', 'E6', 'A6'][i % 4]), gain: 0.032, dur: 2.4 }));
  E.push(...ticks(T.homing0 + 0.66 * (T.homing1 - T.homing0), T.homing1 - 0.4, { rate: 16, seed: 31, gain: 0.024 }));
  HOMING.exprs.forEach((x) => E.push(...ticks(x.t0 + 0.45, x.t0 + 1.0, { rate: 14, seed: 40 + x.line, gain: 0.022 })));
  E.push({ t: T.nums + 0.2, voice: 'pulse', freq: note('E4'), gain: 0.07, dur: 2.6 });
  E.push({ t: T.nums + 3.1, voice: 'chime', freq: note('A5'), gain: 0.04, dur: 3 });
  E.push({ t: T.orig + 0.4, voice: 'pulse', freq: note('C#4'), gain: 0.06, dur: 3 });
  E.push({ t: T.end - 0.6, voice: 'sub', freq: 70, to: 35, gain: 0.16, dur: 1.8 });
  E.push({ t: T.end + 0.2, voice: 'hum', dur: D0 - T.end - 0.2, freq: note('A1'), gain: 0.12, attack: 2, release: 2.5 });
  E.push({ t: T.end + 0.3, voice: 'pulse', freq: note('A2'), gain: 0.1, dur: 4 });
  E.push({ t: T.end + 3.5, voice: 'swell', dur: D0 - T.end - 3.5, notes: ['A2', 'E3', 'C#4', 'E4'].map(note), gain: 0.09, attack: 1.4, release: 2.2 });
  E.push({ t: T.end + 3.6, voice: 'chime', freq: note('A5'), gain: 0.05, dur: 3 });
  story.score = defineScore({ duration: D0, seed: 17, reverb: { seconds: 3.6, wet: 0.3 }, events: E });
}

export default story;
