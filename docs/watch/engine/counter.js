// Numbers that count. Digits sit in fixed-width cells (canvas has no tabular-figures switch), so a
// counting number never jitters sideways.
//
//   const v = countTo(t, { start: 2, dur: 1.6, from: 0, to: 9874 });
//   drawCounter(ctx, v, 960, 600, font(180, { family: 'display', weight: 600 }), { align: 'center' });
//   drawOdometer(ctx, t, { from: 5656, to: 4876, start: 3, dur: 2.2, x: 960, y: 600, font: f, align: 'center' });

import { clamp, lerp, smoothstep } from './tween.js';
import { ease as E } from './ease.js';
import { measure, metrics, setFont } from './text.js';

/** The value of a count from `from` to `to` over [start, start + dur], eased. */
export function countTo(t, { start = 0, dur = 1.5, from = 0, to = 100, ease = E.out } = {}) {
  return lerp(from, to, ease(clamp((t - start) / dur)));
}

/** Locale-independent formatting: `formatNumber(12345.6, { decimals: 1 })` -> '12,345.6'. */
export function formatNumber(n, { decimals = 0, sep = ',', point = '.' } = {}) {
  const neg = n < 0;
  const fixed = Math.abs(n).toFixed(decimals);
  const [int, frac] = fixed.split('.');
  let grouped = '';
  for (let i = 0; i < int.length; i++) {
    if (i > 0 && (int.length - i) % 3 === 0) grouped += sep;
    grouped += int[i];
  }
  return (neg ? '−' : '') + grouped + (frac ? point + frac : '');
}

function digitCell(f) {
  let w = 0;
  for (let d = 0; d <= 9; d++) w = Math.max(w, measure(String(d), f));
  return w;
}

function cellsFor(str, f) {
  const cw = digitCell(f);
  const cells = [];
  let x = 0;
  for (const ch of str) {
    const isDigit = ch >= '0' && ch <= '9';
    const w = isDigit ? cw : measure(ch, f);
    cells.push({ ch, x, w, isDigit });
    x += w;
  }
  return { cells, width: x, cw };
}

const shift = (align, w) => (align === 'center' ? -w / 2 : align === 'right' ? -w : 0);

/** Draw a number (already counted) in tabular cells. `y` is the baseline. */
export function drawCounter(ctx, value, x, y, f, { decimals = 0, sep = ',', align = 'left', color = '#f2f0eb', prefix = '', suffix = '' } = {}) {
  const str = prefix + formatNumber(value, { decimals, sep }) + suffix;
  const { cells, width } = cellsFor(str, f);
  ctx.save();
  setFont(ctx, f);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'center';
  ctx.fillStyle = color;
  const ox = x + shift(align, width);
  for (const c of cells) ctx.fillText(c.ch, ox + c.x + c.w / 2, y);
  ctx.restore();
  return width;
}

/** Width of a counter string in tabular cells (for layout around a counter). */
export function counterWidth(value, f, { decimals = 0, sep = ',', prefix = '', suffix = '' } = {}) {
  return cellsFor(prefix + formatNumber(value, { decimals, sep }) + suffix, f).width;
}

/**
 * Odometer: each digit column rolls from its old digit to its new one, the right-hand columns
 * spinning a few extra turns, settling left to right. The layout is the final number's.
 *
 * Options: from, to, start, dur, x, y (baseline), font, align, color, sep, stagger (fraction of
 * dur by which each column to the left settles earlier), turns (extra turns of the ones column), ease.
 */
export function drawOdometer(ctx, t, o) {
  const { from, to, font: f } = o;
  const start = o.start ?? 0, dur = o.dur ?? 2, align = o.align || 'left', color = o.color || '#f2f0eb';
  const sep = o.sep ?? ',', fn = o.ease || E.inOut, maxTurns = o.turns ?? 2, stagger = o.stagger ?? 0.14;
  const target = formatNumber(Math.round(to), { sep });
  const digitsTo = String(Math.round(Math.abs(to)));
  const digitsFrom = String(Math.round(Math.abs(from))).padStart(digitsTo.length, '0').slice(-digitsTo.length);
  const { cells, width, cw } = cellsFor(target, f);
  const m = metrics(f);
  // digits roll through a slot just taller than the cap height
  const cap = m.capHeight || f.size * 0.72;
  const pad = cap * 0.16;
  const lineH = cap + pad * 2;
  const ox = (o.x ?? 0) + shift(align, width);
  const y = o.y ?? 0;
  const nDigits = digitsTo.length;
  const dir = to >= from ? 1 : -1; // counting down rolls the other way
  const current = lerp(from, to, fn(clamp((t - start) / dur)));

  ctx.save();
  setFont(ctx, f);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'center';
  ctx.fillStyle = color;
  const base = ctx.globalAlpha;
  let di = 0;
  for (const c of cells) {
    if (!c.isDigit) {
      ctx.globalAlpha = base;
      ctx.fillText(c.ch, ox + c.x + c.w / 2, y);
      continue;
    }
    const a = +digitsFrom[di], b = +digitsTo[di];
    const place = nDigits - 1 - di; // 0 = ones
    // the ones column spins `turns` extra times, each column to the left one fewer
    const extra = Math.max(0, maxTurns - place);
    const steps = ((dir > 0 ? b - a : a - b) + 10) % 10 + 10 * extra;
    // every column starts together; the leftmost settles first
    const colDur = dur * Math.max(0.3, 1 - stagger * place);
    const p = fn(clamp((t - start) / colDur));
    const pos = a + dir * steps * p;
    const speed = Math.abs(steps * (fn(clamp((t + 1 / 120 - start) / colDur)) - p)) * 120;
    // a leading column the old number did not have fades in as the count reaches it
    const pow = Math.pow(10, place);
    const alpha = place === 0 || Math.abs(from) >= pow ? 1 : smoothstep(pow * 0.85, pow, Math.abs(current));
    const cx = ox + c.x + c.w / 2;
    ctx.save();
    ctx.beginPath();
    ctx.rect(ox + c.x - 2, y - cap - pad, c.w + 4, lineH);
    ctx.clip();
    const whole = Math.floor(pos), frac = pos - whole;
    const fast = Math.min(1, speed / 30);
    for (let k = 0; k <= 1; k++) {
      const dy = (k - frac) * lineH;
      // a digit fades as it leaves the baseline, so mid-roll reads as an exchange, not a cut
      const near = Math.pow(1 - Math.min(1, Math.abs(dy) / lineH), 1.6);
      if (near <= 0.002) continue;
      ctx.globalAlpha = base * alpha * near * (1 - 0.3 * fast);
      ctx.fillText(String((((whole + k) % 10) + 10) % 10), cx, y + dy);
    }
    ctx.restore();
    di++;
  }
  ctx.restore();
  return { width, cell: cw };
}
