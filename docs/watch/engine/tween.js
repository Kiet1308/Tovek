// Time and value utilities. Everything here is a pure function of its arguments, so a film that
// builds its frame only from these (and `t`) can be scrubbed, rendered out of order and exported.

import { linear } from './ease.js';

export const clamp = (x, lo = 0, hi = 1) => (x < lo ? lo : x > hi ? hi : x);
export const lerp = (a, b, p) => a + (b - a) * p;
/** Where `x` sits between `a` and `b`, unclamped. */
export const invLerp = (a, b, x) => (a === b ? 0 : (x - a) / (b - a));
/** Map `x` from [a0, a1] to [b0, b1], clamped, through an optional ease. */
export const remap = (x, a0, a1, b0, b1, fn = linear) => lerp(b0, b1, fn(clamp(invLerp(a0, a1, x))));
export const smoothstep = (a, b, x) => {
  const p = clamp(invLerp(a, b, x));
  return p * p * (3 - 2 * p);
};
export const fract = (x) => x - Math.floor(x);

/**
 * Progress of a beat that starts at `start` and lasts `dur` seconds, clamped to 0..1 and eased.
 *   const p = progress(t, 2.0, 0.8, ease.out);
 */
export function progress(t, start, dur, fn = linear) {
  if (dur <= 0) return t >= start ? 1 : 0;
  return fn(clamp((t - start) / dur));
}

/** Tween a number from `from` to `to` over [start, start + dur]. */
export const tween = (t, start, dur, from, to, fn = linear) => lerp(from, to, progress(t, start, dur, fn));

/**
 * A trapezoid envelope: 0 before `start`, rises over `fadeIn`, holds, falls over `fadeOut` so it
 * reaches 0 at `end`. Optional eases shape each ramp.
 *   alpha = envelope(t, 3, 9, 0.6, 0.8);
 */
export function envelope(t, start, end, fadeIn = 0, fadeOut = 0, fnIn = linear, fnOut = linear) {
  if (t <= start || t >= end) return fadeIn === 0 && t === start ? 1 : 0;
  const a = fadeIn > 0 ? fnIn(clamp((t - start) / fadeIn)) : 1;
  const b = fadeOut > 0 ? fnOut(clamp((end - t) / fadeOut)) : 1;
  return Math.min(a, b);
}

/**
 * Delay for item `i` of `count` in a staggered group, spread over `spread` seconds.
 * `from`: 'start' (default), 'end', 'center', or an index to radiate from.
 * Optional `fn` eases the spacing, so items bunch at one end.
 */
export function stagger(i, count, spread, { from = 'start', fn = linear } = {}) {
  if (count <= 1) return 0;
  let d;
  if (from === 'start') d = i / (count - 1);
  else if (from === 'end') d = 1 - i / (count - 1);
  else {
    const origin = from === 'center' ? (count - 1) / 2 : from;
    const far = Math.max(origin, count - 1 - origin) || 1;
    d = Math.abs(i - origin) / far;
  }
  return fn(d) * spread;
}

/**
 * Keyframe track. Frames are `[time, value]` or `[time, value, ease]` (the ease shapes the
 * segment that ENDS at that frame). Values may be numbers or equal-length arrays of numbers.
 * Before the first frame the first value holds; after the last, the last value holds.
 *
 *   const camX = keyframes([[0, 0], [2, 320, ease.inOut], [5, 320], [6, 0, ease.out]]);
 *   camX(t)
 */
export function keyframes(frames, { ease: defaultEase = linear } = {}) {
  const ts = frames.map((f) => f[0]);
  const vs = frames.map((f) => f[1]);
  const fs = frames.map((f) => f[2] || defaultEase);
  const isArray = Array.isArray(vs[0]);
  const n = frames.length;
  return function sample(t) {
    if (t <= ts[0]) return vs[0];
    if (t >= ts[n - 1]) return vs[n - 1];
    let i = 1;
    while (ts[i] < t) i++;
    const p = fs[i]((t - ts[i - 1]) / (ts[i] - ts[i - 1]));
    const a = vs[i - 1], b = vs[i];
    if (!isArray) return a + (b - a) * p;
    const out = new Array(a.length);
    for (let k = 0; k < a.length; k++) out[k] = a[k] + (b[k] - a[k]) * p;
    return out;
  };
}

/**
 * Closed-form damped spring from 0 to 1 at `t` seconds after release. No integration and no
 * state, so it is exact at any `t`. `freq` is in Hz; `damping` 0..1 (1 = critically damped).
 */
export function spring(t, { freq = 1.4, damping = 0.62 } = {}) {
  if (t <= 0) return 0;
  const w = 2 * Math.PI * freq;
  const z = Math.min(damping, 0.999);
  const wd = w * Math.sqrt(1 - z * z);
  return 1 - Math.exp(-z * w * t) * (Math.cos(wd * t) + ((z * w) / wd) * Math.sin(wd * t));
}

/** Seconds to `m:ss` (or `h:mm:ss`). Used by the player; handy for on-screen timecodes too. */
export function timecode(seconds, { tenths = false } = {}) {
  const s = Math.max(0, seconds);
  const whole = Math.floor(s);
  const h = Math.floor(whole / 3600), m = Math.floor((whole % 3600) / 60), sec = whole % 60;
  const ss = String(sec).padStart(2, '0');
  const tail = tenths ? '.' + Math.floor((s - whole) * 10) : '';
  return h ? `${h}:${String(m).padStart(2, '0')}:${ss}${tail}` : `${m}:${ss}${tail}`;
}
