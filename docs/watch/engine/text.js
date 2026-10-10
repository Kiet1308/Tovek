// Type on the canvas: fonts, cached layout, and kinetic typography.
//
// Layout is measured once per (font, text, width) and cached, so animating a title costs only the
// draw calls. Glyph positions come from measuring prefixes, so kerning survives per-glyph motion.
// Tracking is applied at draw time (x + i · tracking · size), so it can be animated for free.
//
//   const title = font(140, { family: 'display', weight: 700, stretch: 87.5 });
//   const L = layoutText('Tovek V2.6', title);
//   reveal(ctx, L, 960, 560, t, { unit: 'glyph', start: 0.4, stagger: 0.035, align: 'center' });

import { clamp, lerp } from './tween.js';
import { ease as E } from './ease.js';
import { hash01 } from './random.js';

export const FAMILIES = {
  display: '"Bricolage Grotesque", "Geist", system-ui, sans-serif',
  text: '"Geist", system-ui, "Segoe UI", sans-serif',
  mono: '"Geist Mono", ui-monospace, Consolas, monospace',
};

const STRETCH = [
  [50, 'ultra-condensed'], [62.5, 'extra-condensed'], [75, 'condensed'], [87.5, 'semi-condensed'],
  [100, 'normal'], [112.5, 'semi-expanded'], [125, 'expanded'],
];
function stretchKeyword(s) {
  if (typeof s === 'string') return s;
  let best = STRETCH[4];
  for (const k of STRETCH) if (Math.abs(k[0] - s) < Math.abs(best[0] - s)) best = k;
  return best[1];
}

/**
 * A font spec. `family` is 'display' | 'text' | 'mono' or any CSS family list.
 * `stretch` is a percentage (75, 87.5, 100) or a CSS keyword; canvas only knows the keyword steps.
 * Returns `{ css, size, stretch, family, weight }`; pass it to the layout and draw helpers.
 */
export function font(size, { family = 'text', weight = 400, stretch = 100, style = 'normal' } = {}) {
  const fam = FAMILIES[family] || family;
  const kw = stretchKeyword(stretch);
  return { css: `${style} ${weight} ${kw} ${size}px ${fam}`, size, stretch: kw, family: fam, weight, style };
}

/** Apply a font spec to a context. */
export function setFont(ctx, f) {
  ctx.font = f.css;
  if ('fontStretch' in ctx) ctx.fontStretch = f.stretch;
}

/** The CSS string to preload this font with `document.fonts.load` (a film's `fonts` list). */
export const fontLoadSpec = (f) => f.css;

let measureCtx = null;
function mctx() {
  if (!measureCtx) {
    const c = typeof OffscreenCanvas !== 'undefined' ? new OffscreenCanvas(8, 8) : document.createElement('canvas');
    measureCtx = c.getContext('2d');
  }
  return measureCtx;
}

const widthCache = new Map();
/** Width of `text` in font `f` (no tracking). Cached. */
export function measure(text, f) {
  const key = f.css + '\u0000' + text;
  let w = widthCache.get(key);
  if (w === undefined) {
    const c = mctx();
    setFont(c, f);
    w = c.measureText(text).width;
    if (widthCache.size > 20000) widthCache.clear();
    widthCache.set(key, w);
  }
  return w;
}

const metricCache = new Map();
/** `{ ascent, descent }` of the font box, in px. */
export function metrics(f) {
  let m = metricCache.get(f.css);
  if (!m) {
    const c = mctx();
    setFont(c, f);
    const tm = c.measureText('Hg');
    m = {
      ascent: tm.fontBoundingBoxAscent ?? f.size * 0.92,
      descent: tm.fontBoundingBoxDescent ?? f.size * 0.24,
      capHeight: tm.actualBoundingBoxAscent ?? f.size * 0.7,
    };
    metricCache.set(f.css, m);
  }
  return m;
}

/** Segment into user-perceived characters (keeps emoji and combining marks together). */
const segmenter = typeof Intl !== 'undefined' && Intl.Segmenter ? new Intl.Segmenter('en', { granularity: 'grapheme' }) : null;
const graphemes = (s) => (segmenter ? Array.from(segmenter.segment(s), (x) => x.segment) : Array.from(s));

function layoutLine(text, f) {
  const chars = graphemes(text);
  const xs = new Float64Array(chars.length + 1);
  let prefix = '';
  for (let i = 0; i < chars.length; i++) {
    xs[i] = i === 0 ? 0 : measure(prefix, f);
    prefix += chars[i];
  }
  xs[chars.length] = chars.length ? measure(prefix, f) : 0;
  const words = [];
  let i = 0;
  while (i < chars.length) {
    while (i < chars.length && chars[i] === ' ') i++;
    if (i >= chars.length) break;
    const s = i;
    while (i < chars.length && chars[i] !== ' ') i++;
    words.push({ start: s, end: i });
  }
  return { text, chars, xs, width: xs[chars.length], words };
}

const layoutCache = new Map();

/**
 * Lay out text. `\n` breaks lines; with `maxWidth` words wrap greedily.
 * Returns `{ font, lines: [{ text, chars, xs, width, words, y }], width, height, lineGap, glyphCount, wordCount }`
 * where `y` is each line's baseline offset from the first baseline and `xs[i]` the pen x of glyph i.
 */
export function layoutText(text, f, { maxWidth = Infinity, lineHeight = 1.12 } = {}) {
  const key = `${f.css}\u0000${maxWidth}\u0000${lineHeight}\u0000${text}`;
  const hit = layoutCache.get(key);
  if (hit) return hit;
  const rows = [];
  for (const para of String(text).split('\n')) {
    if (maxWidth === Infinity) { rows.push(para); continue; }
    const words = para.split(' ');
    let line = '';
    for (const w of words) {
      const next = line ? line + ' ' + w : w;
      if (line && measure(next, f) > maxWidth) { rows.push(line); line = w; } else line = next;
    }
    rows.push(line);
  }
  const lineGap = f.size * lineHeight;
  let glyphCount = 0, wordCount = 0;
  const lines = rows.map((r, i) => {
    const L = layoutLine(r, f);
    L.y = i * lineGap;
    L.glyphBase = glyphCount;
    L.wordBase = wordCount;
    glyphCount += L.chars.length;
    wordCount += L.words.length;
    return L;
  });
  const out = {
    font: f, lines, lineGap, glyphCount, wordCount,
    width: Math.max(0, ...lines.map((l) => l.width)),
    height: (lines.length - 1) * lineGap,
    metrics: metrics(f),
  };
  if (layoutCache.size > 4000) layoutCache.clear();
  layoutCache.set(key, out);
  return out;
}

const alignShift = (align, w) => (align === 'center' ? -w / 2 : align === 'right' ? -w : 0);
const trackedWidth = (line, tr, size) => line.width + Math.max(0, line.chars.length - 1) * tr * size;

/**
 * Static draw. `y` is the first baseline. Options: `color`, `align` ('left' | 'center' | 'right'),
 * `tracking` (em, may be animated), `alpha`.
 */
export function drawText(ctx, L, x, y, { color = '#f2f0eb', align = 'left', tracking = 0, alpha = 1 } = {}) {
  if (alpha <= 0) return;
  ctx.save();
  setFont(ctx, L.font);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  ctx.fillStyle = color;
  ctx.globalAlpha *= alpha;
  const size = L.font.size;
  for (const line of L.lines) {
    const ox = x + alignShift(align, trackedWidth(line, tracking, size));
    if (tracking === 0) ctx.fillText(line.text, ox, y + line.y);
    else for (let i = 0; i < line.chars.length; i++) ctx.fillText(line.chars[i], ox + line.xs[i] + i * tracking * size, y + line.y);
  }
  ctx.restore();
}

/**
 * Kinetic reveal, the workhorse of the titles. Each unit (glyph, word or line) rises into place
 * from under its line's mask, with its own delay. Exits run the same way with `out`.
 *
 * Options:
 *   unit     'glyph' | 'word' | 'line'          (default 'word')
 *   start    seconds when the first unit starts
 *   stagger  seconds between units              (default 0.06)
 *   dur      seconds each unit takes            (default 0.9)
 *   ease     curve for each unit                (default ease.out)
 *   rise     em the unit travels up             (default 0.9 with a mask, 0.35 without)
 *   mask     clip each line to its own box      (default true)
 *   fadeIn   also fade alpha 0→1                (default true without mask, false with)
 *   align, color, tracking, trackingFrom (em: tracking eases from this value as units land)
 *   out      { start, stagger, dur, ease, rise } to send units away again (they rise out the top)
 *   colorOf  (unitIndex) => colour, to tint particular words (e.g. the accent on one word)
 */
export function reveal(ctx, L, x, y, t, o = {}) {
  const unit = o.unit || 'word';
  const start = o.start ?? 0, stagger = o.stagger ?? 0.06, dur = o.dur ?? 0.9, fn = o.ease || E.out;
  const mask = o.mask ?? true;
  const rise = (o.rise ?? (mask ? 0.9 : 0.35)) * L.font.size;
  const fadeIn = o.fadeIn ?? !mask;
  const align = o.align || 'left';
  const color = o.color || '#f2f0eb';
  const size = L.font.size;
  const m = L.metrics;
  const out = o.out;
  const total = unit === 'glyph' ? L.glyphCount : unit === 'word' ? L.wordCount : L.lines.length;

  // tracking settles from trackingFrom to tracking as the whole group lands
  const lastLand = start + Math.max(0, total - 1) * stagger + dur;
  const trP = o.trackingFrom == null ? 1 : fn(clamp((t - start) / Math.max(0.001, lastLand - start)));
  const tracking = o.trackingFrom == null ? (o.tracking || 0) : lerp(o.trackingFrom, o.tracking || 0, trP);

  if (t < start) return;
  ctx.save();
  setFont(ctx, L.font);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  const baseAlpha = ctx.globalAlpha;

  for (let li = 0; li < L.lines.length; li++) {
    const line = L.lines[li];
    const ox = x + alignShift(align, trackedWidth(line, tracking, size));
    const by = y + line.y;
    if (mask) {
      ctx.save();
      ctx.beginPath();
      ctx.rect(ox - size, by - m.ascent * 1.08, trackedWidth(line, tracking, size) + size * 2, (m.ascent + m.descent) * 1.16);
      ctx.clip();
    }
    const drawUnit = (idx, text, px) => {
      const p = fn(clamp((t - (start + idx * stagger)) / dur));
      if (p <= 0) return;
      let dy = (1 - p) * rise, a = fadeIn ? p : 1;
      if (out) {
        const q = (out.ease || E.inOut)(clamp((t - ((out.start ?? Infinity) + idx * (out.stagger ?? stagger))) / (out.dur ?? 0.6)));
        if (q >= 1) return;
        dy -= q * (out.rise ?? (mask ? 0.9 : 0.35)) * size;
        if (!mask || out.fade) a *= 1 - q;
      }
      if (a <= 0) return;
      ctx.globalAlpha = baseAlpha * a;
      ctx.fillStyle = o.colorOf ? o.colorOf(idx) || color : color;
      ctx.fillText(text, px, by + dy);
    };
    if (unit === 'line') {
      if (tracking === 0) drawUnit(li, line.text, ox);
      else for (let i = 0; i < line.chars.length; i++) drawUnit(li, line.chars[i], ox + line.xs[i] + i * tracking * size);
    } else if (unit === 'word') {
      for (let w = 0; w < line.words.length; w++) {
        const wd = line.words[w];
        const idx = line.wordBase + w;
        if (tracking === 0) drawUnit(idx, line.chars.slice(wd.start, wd.end).join(''), ox + line.xs[wd.start]);
        else for (let i = wd.start; i < wd.end; i++) drawUnit(idx, line.chars[i], ox + line.xs[i] + i * tracking * size);
      }
    } else {
      for (let i = 0; i < line.chars.length; i++) {
        if (line.chars[i] === ' ') continue;
        drawUnit(line.glyphBase + i, line.chars[i], ox + line.xs[i] + i * tracking * size);
      }
    }
    if (mask) ctx.restore();
  }
  ctx.restore();
}

/** Seconds until the last unit of a `reveal` lands (handy for chaining beats). */
export function revealEnd(L, { unit = 'word', start = 0, stagger = 0.06, dur = 0.9 } = {}) {
  const total = unit === 'glyph' ? L.glyphCount : unit === 'word' ? L.wordCount : L.lines.length;
  return start + Math.max(0, total - 1) * stagger + dur;
}

/**
 * Typewriter: draws the first `count` glyphs (fractional counts fade the next glyph in).
 * `caret: true` draws a thin caret after the last glyph; pass `caretAlpha` to blink it.
 */
export function typewriter(ctx, L, x, y, count, { color = '#f2f0eb', align = 'left', caret = false, caretAlpha = 1, caretColor } = {}) {
  ctx.save();
  setFont(ctx, L.font);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  ctx.fillStyle = color;
  const base = ctx.globalAlpha;
  let left = count;
  let cx = x, cy = y;
  for (const line of L.lines) {
    const ox = x + alignShift(align, line.width);
    const n = line.chars.length;
    const whole = Math.max(0, Math.min(n, Math.floor(left)));
    if (whole > 0) ctx.fillText(line.chars.slice(0, whole).join(''), ox, y + line.y);
    const frac = left - whole;
    if (whole < n && frac > 0) {
      ctx.globalAlpha = base * frac;
      ctx.fillText(line.chars[whole], ox + line.xs[whole], y + line.y);
      ctx.globalAlpha = base;
    }
    cx = ox + line.xs[Math.min(n, whole)];
    cy = y + line.y;
    left -= n;
    if (left <= 0) break;
  }
  if (caret && caretAlpha > 0) {
    const m = L.metrics;
    ctx.globalAlpha = base * caretAlpha;
    ctx.fillStyle = caretColor || color;
    const w = Math.max(1.5, L.font.size * 0.06);
    ctx.fillRect(cx + L.font.size * 0.04, cy - m.ascent * 0.82, w, (m.ascent + m.descent) * 0.86);
  }
  ctx.restore();
}

/**
 * Decode: every glyph flickers through `charset` until it settles on its real character at its own
 * (staggered) time. Deterministic for a `seed`. Best with a monospace font.
 */
export function decode(ctx, L, x, y, t, { start = 0, dur = 1.2, stagger = 0.02, seed = 7, rate = 24, charset = '0123456789abcdef', color = '#f2f0eb', settledColor, align = 'left' } = {}) {
  if (t < start) return;
  ctx.save();
  setFont(ctx, L.font);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  const tick = Math.floor(t * rate);
  for (const line of L.lines) {
    const ox = x + alignShift(align, line.width);
    for (let i = 0; i < line.chars.length; i++) {
      const ch = line.chars[i];
      if (ch === ' ') continue;
      const gi = line.glyphBase + i;
      const t0 = start + gi * stagger;
      if (t < t0) continue;
      const settled = t >= t0 + dur;
      const shown = settled ? ch : charset[Math.floor(hash01(seed, gi, tick) * charset.length)];
      ctx.fillStyle = settled ? settledColor || color : color;
      ctx.globalAlpha = settled ? 1 : 0.55 + 0.45 * hash01(seed + 1, gi, tick);
      ctx.fillText(shown, ox + line.xs[i], y + line.y);
    }
  }
  ctx.restore();
}

/** Largest font size (≤ spec size) at which `text` fits in `maxWidth`. */
export function fitSize(text, f, maxWidth) {
  const w = measure(text, f);
  return w <= maxWidth ? f.size : Math.floor((f.size * maxWidth) / w);
}
