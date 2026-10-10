// Seeded randomness. A film's frame must depend only on `t`, so never call Math.random() in
// render. Use `hash01(seed, i, ...)` for per-item values (order-independent: item 7 gets the same
// value whether or not items 0..6 were drawn), and `rng(seed)` only in prepare-time code.

/** 32-bit integer hash of any number of integers (or floats, which are folded to ints). */
export function hash32(...xs) {
  let h = 0x811c9dc5 ^ xs.length;
  for (let k = 0; k < xs.length; k++) {
    let x = xs[k];
    if (!Number.isInteger(x)) x = Math.floor(x * 1e6);
    x |= 0;
    h = Math.imul(h ^ x, 0x01000193);
    h ^= h >>> 15;
    h = Math.imul(h, 0x2c1b3c6d);
    h ^= h >>> 12;
    h = Math.imul(h, 0x297a2d39);
    h ^= h >>> 15;
  }
  return h >>> 0;
}

/** Uniform value in [0, 1) for these integer coordinates. Pure. */
export const hash01 = (...xs) => hash32(...xs) / 4294967296;

/** Uniform value in [lo, hi) for these coordinates. */
export const hashRange = (lo, hi, ...xs) => lo + (hi - lo) * hash01(...xs);

/** Sequential generator (mulberry32). Deterministic for a seed, but stateful: prepare-time only. */
export function rng(seed = 1) {
  let a = seed >>> 0;
  return function next() {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Smooth 1D value noise in [0, 1): `noise1(t * 0.8, 3)` wanders gently and is pure. */
export function noise1(x, seed = 0) {
  const i = Math.floor(x);
  const f = x - i;
  const u = f * f * (3 - 2 * f);
  return hash01(seed, i) * (1 - u) + hash01(seed, i + 1) * u;
}

/** Pick an element of `list` for coordinates `xs`. */
export const pick = (list, ...xs) => list[hash32(...xs) % list.length];
