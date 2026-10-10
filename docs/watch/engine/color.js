// Colour helpers and the V2.6 palette, mirrored from assets/v26/tokens.css so films and pages agree.

export const V26 = Object.freeze({
  paper: '#f2f0eb',
  paper2: '#e9e6df',
  sheet: '#faf9f6',
  night: '#0d0d0c',
  night2: '#1a1a18',
  ink: '#121210',
  ink2: '#4a4943',
  ink3: '#8b8980',
  onNight: '#f2f0eb',
  onNight2: '#a9a69c',
  /** The one accent: what Tovek recovers or proves. Use it for meaning, never for decoration. */
  signal: '#ff4d1a',
  /** The accent for text and hairlines on paper (AA contrast). */
  signalDeep: '#d63a0c',
});

const cache = new Map();

/** '#rgb' | '#rrggbb' -> [r, g, b] in 0..255. Cached. */
export function parseHex(hex) {
  let c = cache.get(hex);
  if (c) return c;
  let h = hex.replace('#', '');
  if (h.length === 3) h = h[0] + h[0] + h[1] + h[1] + h[2] + h[2];
  const n = parseInt(h, 16);
  c = [(n >> 16) & 255, (n >> 8) & 255, n & 255];
  cache.set(hex, c);
  return c;
}

/** CSS colour string for `hex` at `alpha`. */
export function rgba(hex, alpha = 1) {
  const [r, g, b] = parseHex(hex);
  return alpha >= 1 ? `rgb(${r},${g},${b})` : `rgba(${r},${g},${b},${+alpha.toFixed(4)})`;
}

const toLinear = (c) => {
  c /= 255;
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
};
const toSrgb = (c) => {
  const v = c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
  return Math.round(Math.max(0, Math.min(1, v)) * 255);
};

function toOklab([r, g, b]) {
  const lr = toLinear(r), lg = toLinear(g), lb = toLinear(b);
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

function fromOklab([L, A, B]) {
  const l = (L + 0.3963377774 * A + 0.2158037573 * B) ** 3;
  const m = (L - 0.1055613458 * A - 0.0638541728 * B) ** 3;
  const s = (L - 0.0894841775 * A - 1.291485548 * B) ** 3;
  return [
    toSrgb(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    toSrgb(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    toSrgb(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
  ];
}

/** Perceptual mix of two hex colours (OKLab), returned as 'rgb(...)' / 'rgba(...)'. */
export function mix(a, b, p, alpha = 1) {
  if (p <= 0) return rgba(a, alpha);
  if (p >= 1) return rgba(b, alpha);
  const A = toOklab(parseHex(a)), B = toOklab(parseHex(b));
  const [r, g, bl] = fromOklab([A[0] + (B[0] - A[0]) * p, A[1] + (B[1] - A[1]) * p, A[2] + (B[2] - A[2]) * p]);
  return alpha >= 1 ? `rgb(${r},${g},${bl})` : `rgba(${r},${g},${bl},${+alpha.toFixed(4)})`;
}
