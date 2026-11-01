// V2.6.1 and V2.7. The mist of the V2.7 page rolls in over V2.6's paper; V2.6.1 prints this script as
// V2.6 did; V2.7 changes two things in it, shown as a morph of its real output; then its numbers.

import {
  ease, progress, hash01, layoutText, drawText, reveal, codeMorph, drawMorph, fitCamera, withCamera, drawCode,
  drawOdometer, formatNumber, roundRect,
} from '../../engine/index.js';
import { T, ERA, TY, S, surface, label, parseNumber, longDate } from './base.js';

let V = null;

/** The top-level function starting at the line that contains `head`, through its column-0 `end`. */
function block(text, head) {
  const lines = text.split('\n');
  const at = lines.findIndex((l) => l.includes(head));
  if (at < 0) return '';
  const end = lines.findIndex((l, i) => i > at && /^end\b/.test(l));
  return lines.slice(at, end + 1).join('\n');
}

export function prepareV27() {
  const a = S.V26, b = S.LAST;
  const top = codeMorph(block(a.output, 'local function isAlive'), block(b.output, 'local function isAlive'));
  const join = codeMorph(a.excerpts.join.text, b.excerpts.join.text);
  const nums = (b.numbers || []).map((n) => parseNumber(n.text)).filter(Boolean);
  const named = [...join.inserted].map((i) => join.b.tokens[i]).find((tk) => tk.k === 'id');
  V = { top, join, nums, name: named ? named.text : null, same261: S.ST['v2.6.1'] && !S.ST['v2.6.1'].changed_from_previous };
  return V;
}

export const vmeta = () => V;

/** Fog drifting across the frame, thickest low. */
function fog(ctx, t, amount) {
  const e = ERA.v27;
  for (let j = 0; j < 7; j++) {
    const speed = 12 + hash01(27, j, 1) * 20;
    const x = ((hash01(27, j, 2) * 2600 + (t - T.v27) * speed) % 2600) - 340;
    const y = 700 + hash01(27, j, 3) * 420;
    const rx = 420 + hash01(27, j, 4) * 300, ry = 90 + hash01(27, j, 5) * 60;
    ctx.save();
    ctx.translate(x, y);
    ctx.scale(1, ry / rx);
    const r = ctx.createRadialGradient(0, 0, 0, 0, 0, rx);
    r.addColorStop(0, `rgba(${e.fog},${0.7 * amount})`);
    r.addColorStop(1, `rgba(${e.fog},0)`);
    ctx.fillStyle = r;
    ctx.fillRect(-rx, -rx, rx * 2, rx * 2);
    ctx.restore();
  }
}

const CARD = { x: 820, y: 150, w: 940, h: 780 };
const SIZE = 19, LH = 1.5;

function morphBox(ctx, plan, p, box) {
  if (p < 1) {
    drawMorph(ctx, plan, p, { box, size: SIZE, lineHeight: LH, palette: S.PAL.v27, clip: true, align: 'top', minScale: 0.5 });
  } else {
    const cam = fitCamera(plan.b, box, { size: SIZE, lineHeight: LH, align: 'top', minScale: 0.5 });
    withCamera(ctx, cam, () => drawCode(ctx, plan.b, { size: SIZE, lineHeight: LH, palette: S.PAL.v27, highlight: plan.inserted }));
  }
}

export function drawV27(ctx, t) {
  const e = ERA.v27, ty = TY.v27;
  const a = progress(t, T.v27, 0.9, ease.inOut);
  ctx.save();
  ctx.globalAlpha *= a;
  surface(ctx, e.surface);
  fog(ctx, t, 1 - 0.5 * progress(t, T.v27 + 1, 2));
  // V2.6.1 first, briefly; then the name of V2.7 condenses out of the mist
  const s261 = S.ST['v2.6.1'];
  if (s261) {
    const q = progress(t, T.v27 + 0.3, 0.5) * (1 - progress(t, T.v27title - 0.1, 0.4));
    label(ctx, `TOVEK ${s261.name} · ${longDate(s261.published_at || s261.date).toUpperCase()}`, 120, 300, e.ink2, { alpha: q, font: ty.meta });
    label(ctx, V.same261 ? 'THIS SCRIPT PRINTS AS IN V2.6' : 'THIS SCRIPT CHANGES', 120, 340, e.ink3, { alpha: q, font: ty.meta });
  }
  const pt = progress(t, T.v27title, 1.6, ease.out);
  if (pt > 0) {
    const L = layoutText(S.LAST.name, ty.title);
    const spread = (1 - pt) * 14;
    for (let k = 3; k >= 0; k--) {
      const al = k === 0 ? pt : pt * (1 - pt) * 0.4;
      drawText(ctx, L, 116 + (k ? Math.cos(k * 2.1) * spread : 0), 330 + (k ? Math.sin(k * 2.1) * spread : 0), {
        color: e.ink, alpha: al, tracking: 0.12 * (1 - pt) + 0.02,
      });
    }
  }
  reveal(ctx, layoutText(S.LAST.headline, ty.head, { maxWidth: 600, lineHeight: 1.35 }), 120, 430, t, {
    unit: 'word', start: T.v27title + 0.7, stagger: 0.05, dur: 0.8, color: e.ink2, mask: false,
  });
  // the card: isAlive, then onPlayerAdded, each morphing from V2.6's output to V2.7's
  const c = progress(t, T.v27 + 1.2, 0.8, ease.out);
  if (c > 0) {
    ctx.save();
    ctx.globalAlpha *= c;
    ctx.fillStyle = e.card;
    roundRect(ctx, CARD.x - 36, CARD.y - 36, CARD.w + 72, CARD.h + 72, 22);
    ctx.fill();
    const p1 = progress(t, T.v27Morph0, 1.4, ease.inOut);
    const p2 = progress(t, T.v27Morph0 + 0.9, 1.5, ease.inOut);
    morphBox(ctx, V.top, p1, { x: CARD.x, y: CARD.y, w: CARD.w, h: 4 * SIZE * LH });
    morphBox(ctx, V.join, p2, { x: CARD.x, y: CARD.y + 6 * SIZE * LH, w: CARD.w, h: CARD.h - 6 * SIZE * LH });
    label(ctx, p2 < 0.5 ? `TOVEK ${S.V26.name}` : `TOVEK ${S.LAST.name}`, CARD.x, CARD.y + CARD.h + 4, p2 < 0.5 ? e.ink3 : e.accent, { font: ty.meta, alpha: c });
    ctx.restore();
  }
  // the numbers, under the headline
  V.nums.slice(0, 2).forEach((n, i) => {
    const at = T.v27nums + i * 1.4;
    const q = progress(t, at, 0.6, ease.out);
    if (q <= 0) return;
    const y = 690 + i * 175;
    ctx.save();
    ctx.globalAlpha *= q;
    drawOdometer(ctx, t, { from: n.from ?? 0, to: n.to, start: at, dur: 1.4, x: 120, y, font: ty.num, color: e.ink, turns: 1 });
    drawText(ctx, layoutText(n.label, ty.unit, { maxWidth: 600 }), 124, y + 40, { color: e.ink2 });
    drawText(ctx, layoutText(`from ${formatNumber(n.from ?? 0)} in ${S.ST['v2.6.1'] ? S.ST['v2.6.1'].name : S.V26.name}`, ty.unit), 124, y + 72, { color: e.ink3 });
    ctx.restore();
  });
  ctx.restore();
}
