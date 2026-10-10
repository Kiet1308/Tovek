// The Tovek mark (assets/brand/tovek-mark.svg), drawable on the canvas at any size. The shape is
// fixed; only its colour and the arrival of its eight bits may be animated.

import { clamp } from './tween.js';
import { ease as E } from './ease.js';

const VIEW = { x: 64, y: 72, w: 384, h: 360 };
const BODY = 'M100 88H412a20 20 0 0 1 20 20V160a20 20 0 0 1-20 20H320V320H192V180H100a20 20 0 0 1-20-20V108a20 20 0 0 1 20-20Z';
// the eight bits, in reading order
const BITS = [[196, 328], [228, 328], [260, 328], [292, 328], [196, 360], [260, 360], [292, 360], [228, 392]];

let bodyPath = null;

/**
 * Draw the mark with its top-left at (x, y), `size` tall. `bits` (0..1) staggers the eight bits
 * in; `body` (0..1) fades the T. `bitColor` may tint the bits (e.g. the accent while they land).
 */
export function drawMark(ctx, x, y, size, { color = '#f2f0eb', bitColor, body = 1, bits = 1 } = {}) {
  if (!bodyPath && typeof Path2D !== 'undefined') bodyPath = new Path2D(BODY);
  const k = size / VIEW.h;
  ctx.save();
  ctx.translate(x - VIEW.x * k, y - VIEW.y * k);
  ctx.scale(k, k);
  const base = ctx.globalAlpha;
  if (body > 0) {
    ctx.globalAlpha = base * body;
    ctx.fillStyle = color;
    ctx.fill(bodyPath);
  }
  for (let i = 0; i < BITS.length; i++) {
    const p = E.out(clamp(bits * 1.6 - i * 0.075));
    if (p <= 0) continue;
    const [bx, by] = BITS[i];
    ctx.globalAlpha = base * p;
    ctx.fillStyle = bitColor || color;
    const s = 24 * (0.6 + 0.4 * p);
    const o = (24 - s) / 2;
    ctx.beginPath();
    if (ctx.roundRect) ctx.roundRect(bx + o, by + o + (1 - p) * 10, s, s, 5);
    else ctx.rect(bx + o, by + o + (1 - p) * 10, s, s);
    ctx.fill();
  }
  ctx.restore();
}

/** Width of the mark when drawn `size` tall. */
export const markWidth = (size) => (size * VIEW.w) / VIEW.h;
