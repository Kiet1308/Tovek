// Easing functions. Every one maps 0..1 to 0..1 (some overshoot on purpose) and is pure.
//
//   import { ease } from './engine/index.js';
//   const x = lerp(0, 400, ease.out(p));
//
// `ease.out` and `ease.inOut` are the same curves as `--ease-out` and `--ease-in-out` in
// assets/v26/tokens.css, so motion on the canvas matches motion on the page.

const { pow, sin, cos, sqrt, PI } = Math;

/**
 * A CSS-style cubic Bézier timing function. Precomputes a sample table, then refines with
 * Newton-Raphson (falling back to bisection), so evaluation is fast and exact to ~1e-7.
 */
export function cubicBezier(x1, y1, x2, y2) {
  if (x1 === y1 && x2 === y2) return linear;
  const cx = 3 * x1, bx = 3 * (x2 - x1) - cx, ax = 1 - cx - bx;
  const cy = 3 * y1, by = 3 * (y2 - y1) - cy, ay = 1 - cy - by;
  const sx = (u) => ((ax * u + bx) * u + cx) * u;
  const sy = (u) => ((ay * u + by) * u + cy) * u;
  const dx = (u) => (3 * ax * u + 2 * bx) * u + cx;
  const N = 11, step = 1 / (N - 1);
  const table = new Float64Array(N);
  for (let i = 0; i < N; i++) table[i] = sx(i * step);

  function solve(x) {
    let i = 1;
    while (i < N - 1 && table[i] <= x) i++;
    i--;
    let u = (i + (x - table[i]) / (table[i + 1] - table[i])) * step;
    let d = dx(u);
    if (d >= 0.001) {
      for (let k = 0; k < 6; k++) {
        d = dx(u);
        if (d === 0) break;
        u -= (sx(u) - x) / d;
      }
      return u;
    }
    if (d === 0) return u;
    let lo = i * step, hi = lo + step;
    for (let k = 0; k < 24; k++) {
      u = (lo + hi) / 2;
      const e = sx(u) - x;
      if (Math.abs(e) < 1e-7) break;
      if (e > 0) hi = u; else lo = u;
    }
    return u;
  }
  return (p) => (p <= 0 ? 0 : p >= 1 ? 1 : sy(solve(p)));
}

export const linear = (p) => p;

const inPow = (n) => (p) => pow(p, n);
const outPow = (n) => (p) => 1 - pow(1 - p, n);
const inOutPow = (n) => (p) => (p < 0.5 ? pow(2 * p, n) / 2 : 1 - pow(2 - 2 * p, n) / 2);

/** Overshoot a little, then settle. `s` controls the overshoot (1.70158 is the classic 10%). */
export const outBack = (s = 1.70158) => (p) => {
  const q = p - 1;
  return 1 + q * q * ((s + 1) * q + s);
};

export const ease = {
  linear,
  // the shared V2.6 curves
  out: cubicBezier(0.16, 1, 0.3, 1),
  inOut: cubicBezier(0.65, 0, 0.35, 1),
  // a softer arrival for large moves (camera, panels)
  glide: cubicBezier(0.33, 0, 0.12, 1),
  // anticipation-free snap used by UI-like beats
  snap: cubicBezier(0.2, 0.9, 0.1, 1),

  inQuad: inPow(2), outQuad: outPow(2), inOutQuad: inOutPow(2),
  inCubic: inPow(3), outCubic: outPow(3), inOutCubic: inOutPow(3),
  inQuart: inPow(4), outQuart: outPow(4), inOutQuart: inOutPow(4),
  inQuint: inPow(5), outQuint: outPow(5), inOutQuint: inOutPow(5),

  inSine: (p) => 1 - cos((p * PI) / 2),
  outSine: (p) => sin((p * PI) / 2),
  inOutSine: (p) => -(cos(PI * p) - 1) / 2,

  inExpo: (p) => (p <= 0 ? 0 : pow(2, 10 * p - 10)),
  outExpo: (p) => (p >= 1 ? 1 : 1 - pow(2, -10 * p)),
  inOutExpo: (p) => (p <= 0 ? 0 : p >= 1 ? 1 : p < 0.5 ? pow(2, 20 * p - 10) / 2 : (2 - pow(2, -20 * p + 10)) / 2),

  inCirc: (p) => 1 - sqrt(1 - p * p),
  outCirc: (p) => sqrt(1 - (p - 1) * (p - 1)),

  outBack: outBack(),
  outBackSoft: outBack(0.9),

  /** Hard steps: `ease.steps(4)` quantises progress into four even jumps. */
  steps: (n) => (p) => (p >= 1 ? 1 : Math.floor(p * n) / n),
};

/** Mirror an ease: `reverse(ease.out)` arrives the way `ease.out` departs. */
export const reverse = (fn) => (p) => 1 - fn(1 - p);

/** Run `fn` forwards then backwards across 0..1 (0 → 1 → 0). */
export const yoyo = (fn) => (p) => (p < 0.5 ? fn(p * 2) : fn(2 - p * 2));
