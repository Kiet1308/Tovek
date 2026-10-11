// I. "Every Roblox game ships as bytecode." We start a hand's width from the bytes: a few of them,
//    huge, with a shallow focus; the ASCII of a string glints under its hex. The camera pulls back
//    until all 1,252 bytes of the hero script fill the frame.
// II. "Tovek turns it back into source." Each byte flies to the line of the output its instruction
//    became (or to the name its string spells); the metadata sinks. The bytes stand in the shape of the
//    code, a narrow column of hex. Then the read head scans down the frame: above it is paper and
//    Luau, below it the night and the hex.

import { T, C, TY, S, PAGE, f, fmt, surface, kicker, say, headH, frameAt, toScreen, drawHero, mixQ } from './base.js';
import { ease, clamp, lerp, progress, envelope, layoutText, drawText, hash01, codeMetrics, rgba } from '../../engine/index.js';

// ------------------------------------------------------------------------------------------ bytes

const COLS = 48;
const PITCH = 40;                      // the wall's cells are square and fill the frame exactly
let ROWS = 27;
const WALL_TEXT = 21;                  // px of a byte on the wall

let B = null; // prepared bytes: { n, val, kind, tx, ty, line, ascii, focus }

/**
 * Glyph sheets of the 256 byte values in Geist Mono at 18, 36 and 72 px (a frame picks the smallest
 * sheet at least as large as the byte it draws, so a sheet is never shrunk by more than half), and a
 * soft one for bytes out of focus. Larger bytes are drawn as text, so the macro stays sharp at 4K.
 */
let ATLAS = null;
function buildAtlas() {
  if (typeof OffscreenCanvas === 'undefined') return;
  const make = (size, blur) => {
    const F = f(size, 'mono', 400);
    const probe = new OffscreenCanvas(8, 8).getContext('2d');
    probe.font = F.css;
    const gw = Math.ceil(probe.measureText('00').width);
    const cw = gw + Math.ceil(size * 0.5) + blur * 4, ch = Math.ceil(size * 1.25) + blur * 4;
    const cv = new OffscreenCanvas(cw * 16, ch * 16);
    const c = cv.getContext('2d', { willReadFrequently: true });
    c.font = F.css;
    c.textBaseline = 'middle';
    c.textAlign = 'center';
    c.fillStyle = C.onNight;
    if (blur) c.filter = `blur(${blur}px)`;
    for (let v = 0; v < 256; v++) c.fillText(v.toString(16).padStart(2, '0'), (v % 16) * cw + cw / 2, Math.floor(v / 16) * ch + ch / 2 + size * 0.04);
    return { cv, cw, ch, gw, size };
  };
  ATLAS = { sharp: [make(18, 0), make(36, 0), make(72, 0)], soft: make(48, 5) };
}

/** Device pixels per design unit for the current frame (set before a shot draws bytes). */
let DS = 1;
function byteMode(ctx) {
  ctx.imageSmoothingQuality = 'low';
  const m = ctx.getTransform();
  DS = Math.hypot(m.a, m.b) || 1;
}

const MONO = new Map();
const monoFont = (px) => {
  const q = Math.max(1, Math.round(px));
  let F = MONO.get(q);
  if (!F) { F = f(q, 'mono', 400); MONO.set(q, F); }
  return F;
};

/** One byte centred at (x, y), `size` px tall, with `soft` (0..1) of it out of focus. */
function drawByte(ctx, v, x, y, size, alpha, soft = 0) {
  if (alpha <= 0.003) return;
  const px = size * DS;
  if (soft < 1) {
    ctx.globalAlpha = alpha * (1 - soft);
    if (px > 76 || !ATLAS) {
      const F = monoFont(size);
      ctx.font = F.css;
      ctx.fillText(HEX2[v], x, y + size * 0.04);
    } else {
      const S3 = ATLAS.sharp;
      const a = px <= S3[0].size ? S3[0] : px <= S3[1].size ? S3[1] : S3[2];
      const k = size / a.size;
      ctx.drawImage(a.cv, (v % 16) * a.cw, Math.floor(v / 16) * a.ch, a.cw, a.ch, x - (a.cw * k) / 2, y - (a.ch * k) / 2, a.cw * k, a.ch * k);
    }
  }
  if (soft > 0 && ATLAS) {
    const a = ATLAS.soft, k = size / a.size;
    ctx.globalAlpha = alpha * soft;
    ctx.drawImage(a.cv, (v % 16) * a.cw, Math.floor(v / 16) * a.ch, a.cw, a.ch, x - (a.cw * k) / 2, y - (a.ch * k) / 2, a.cw * k, a.ch * k);
  }
}
const HEX2 = Array.from({ length: 256 }, (_, v) => v.toString(16).padStart(2, '0'));

// ------------------------------------------------------------------------------------- prepare

export function prepareOpen() {
  const D = S.D;
  const hex = D.hero.hex;
  const n = hex.length / 2;
  ROWS = Math.ceil(n / COLS);
  const val = new Uint8Array(n);
  for (let i = 0; i < n; i++) val[i] = parseInt(hex.substr(i * 2, 2), 16);
  const kind = Uint8Array.from(D.hero.byte_kind, (c) => +c);
  // the macro opens on a string the film comes back to, the first one clear of the wall's edges (the
  // camera's first frame must not see past the wall)
  const find = (word) => {
    const want = [...word].map((ch) => ch.charCodeAt(0).toString(16).padStart(2, '0')).join('');
    for (let i = hex.indexOf(want); i >= 0; i = hex.indexOf(want, i + 1)) if (i % 2 === 0) return i / 2;
    return -1;
  };
  let at = -1, len = 0;
  for (const word of FOCUS_WORDS) {
    const i = find(word);
    if (i < 0) continue;
    const mid = i + Math.floor(word.length / 2), r = Math.floor(mid / COLS);
    if (r >= 2 && r <= ROWS - 3) { at = i; len = word.length; break; }
  }
  if (at < 0) { at = Math.floor(n / 2); len = 2; }
  const ascii = new Uint8Array(n);
  for (let i = 0; i < n; i++) ascii[i] = kind[i] === 2 && val[i] >= 32 && val[i] < 127 ? 1 : 0;
  // the focus: a little before the string's middle (the camera drifts along it)
  const fi = at + Math.max(1, Math.floor(len / 2) - 3);
  const fx = clamp(((fi % COLS) + 0.5) * PITCH, 960 / MACRO, 1920 - 960 / MACRO);
  const fy = clamp((Math.floor(fi / COLS) + 0.5) * PITCH, 540 / MACRO, ROWS * PITCH - 540 / MACRO);
  B = { n, val, kind, ascii, focus: { x: fx, y: fy } };
  buildAtlas();
  prepareHex(D);   // where each byte lands: its two cells of the hex silhouette
}

// ------------------------------------------------------------------------------------- the camera
// The wall is the frame itself (1920 x 1080 at scale 1). The camera starts close on the string and
// pulls back along a log curve, the focus gliding to the wall's centre.

const MACRO = 8.5;
const FOCUS_WORDS = ['RespawnLocation', 'SpawnLocation', 'Humanoid', 'leaderstats', 'Checkpoints'];
function wallCam(t) {
  const p = progress(t, T.pull0, T.pull1 - T.pull0, ease.inOutCubic);
  const s = Math.exp(lerp(Math.log(MACRO), 0, p));
  // a slow drift along the string while close
  const drift = (t - T.first) * 9;
  const k = clamp((1 / s - 1 / MACRO) / (1 - 1 / MACRO));
  return { x: lerp(B.focus.x + drift, 960, k), y: lerp(B.focus.y, 540, k), s };
}

// --------------------------------------------------------------------------------------- I. bytes

export function drawBytes(ctx, t, { dim = 1 } = {}) {
  surface(ctx, C.night);
  if (t < T.first) {
    // the black opening: touch the glyph sheets once so the GPU has them before the bytes arrive
    if (ATLAS) {
      ctx.save();
      ctx.globalAlpha = 1 / 1024;
      for (const a of ATLAS.sharp) ctx.drawImage(a.cv, 0, 0, 64, 64);
      ctx.drawImage(ATLAS.soft.cv, 0, 0, 64, 64);
      ctx.restore();
    }
    return;
  }
  const cam = wallCam(t);
  const appear = progress(t, T.first, 1.6, ease.out);
  const macro = clamp((cam.s - 1) / (MACRO - 1));
  ctx.save();
  byteMode(ctx);
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillStyle = C.onNight;
  const base = ctx.globalAlpha * appear * dim;
  const size = WALL_TEXT * cam.s;
  const half = (PITCH * cam.s) / 2;
  // visible cells only
  const c0 = Math.max(0, Math.floor((cam.x - 960 / cam.s) / PITCH) - 1), c1 = Math.min(COLS - 1, Math.ceil((cam.x + 960 / cam.s) / PITCH) + 1);
  const r0 = Math.max(0, Math.floor((cam.y - 540 / cam.s) / PITCH) - 1), r1 = Math.min(ROWS - 1, Math.ceil((cam.y + 540 / cam.s) / PITCH) + 1);
  for (let r = r0; r <= r1; r++) {
    for (let c = c0; c <= c1; c++) {
      const i = r * COLS + c;
      if (i >= B.n) continue;
      const x = 960 + ((c + 0.5) * PITCH - cam.x) * cam.s;
      const y = 540 + ((r + 0.5) * PITCH - cam.y) * cam.s;
      if (x < -half || x > 1920 + half || y < -half || y > 1080 + half) continue;
      // a shallow focus while close: one band of rows is sharp, the rest go soft with distance
      const soft = clamp((Math.abs(y - 540) / 540 - 0.08) * 2.4) * macro;
      const tone = B.kind[i] === 2 ? 0.95 : B.kind[i] === 1 ? 0.72 : 0.44;
      const a = base * tone * (0.86 + 0.14 * hash01(7, i));
      drawByte(ctx, B.val[i], x, y, size, a, soft);
      // close up, the letters of a string glint under its hex
      if (B.ascii[i] && macro > 0.25) {
        const ga = a * clamp((macro - 0.25) / 0.4) * 0.55 * (1 - soft);
        if (ga > 0.01) {
          ctx.globalAlpha = ga;
          ctx.font = monoFont(size * 0.42).css;
          ctx.fillText(String.fromCharCode(B.val[i]), x, y + size * 0.86);
        }
      }
    }
  }
  ctx.restore();

  // the line, over a scrim that lifts it off the bytes
  const ta = envelope(t, T.line1 - 0.2, T.textOut + 0.5, 0.5, 0.6);
  if (ta > 0) scrim(ctx, ta);
  if (t < T.textOut + 1.2) {
    say(ctx, 'Every Roblox game', 140, 492, t, T.line1, { out: T.textOut });
    say(ctx, 'ships as bytecode.', 140, 492 + 160, t, T.line2, { out: T.textOut + 0.08 });
  }
  // the wall's label
  const la = envelope(t, T.label, T.source + 0.15, 0.5, 0.4);
  if (la > 0) label(ctx, `${S.D.hero.file.replace('.luau', '')} · ${fmt(S.D.hero.bytes)} bytes · ${S.D.hero.compiler.replace('luau-compile --binary ', 'luau ')}`, la);
}

/** A dark wash from the left, under display type set over the bytes. */
function scrim(ctx, a) {
  ctx.save();
  ctx.globalAlpha *= a;
  const g = ctx.createLinearGradient(0, 0, 1500, 0);
  g.addColorStop(0, rgba(C.night, 0.94));
  g.addColorStop(0.6, rgba(C.night, 0.8));
  g.addColorStop(1, rgba(C.night, 0));
  ctx.fillStyle = g;
  ctx.fillRect(-4, -4, 1504, 1088);
  ctx.restore();
}

/** A label on a dark plate, top left. */
function label(ctx, text, a) {
  ctx.save();
  ctx.globalAlpha *= a;
  const L = layoutText(text.toUpperCase(), TY.kicker);
  const w = L.width + L.lines[0].chars.length * 0.12 * TY.kicker.size + 40;
  ctx.fillStyle = rgba(C.night, 0.88);
  ctx.fillRect(120, 64, w, 46);
  drawText(ctx, L, 140, 94, { color: C.onNight, tracking: 0.12 });
  ctx.restore();
}

// ------------------------------------------------------------------------------ II. back to source

/** The page camera while the bytes gather and the head scans: the whole column in the middle. */
export function wideCam() {
  const H = S.hero;
  const s = 900 / H.height;
  return frameAt(H.width / 2, H.height / 2, 960, 548, s);
}

/** The read head's height on the frame at `t` (it passes from above the page to below it). */
export const headY = (t) => lerp(-40, 1120, progress(t, T.scan0, T.scan1 - T.scan0, ease.inOutSine));

export function drawSource(ctx, t) {
  if (t < T.gather0 + 0.02) {
    drawBytes(ctx, t, { dim: 1 - 0.6 * envelope(t, T.source - 0.05, T.gather0 + 0.3, 0.6, 0.3) });
  } else {
    surface(ctx, C.night);
    drawGather(ctx, t);
  }
  if (t < T.gather0 + 0.8) {
    say(ctx, 'Tovek turns it back', 140, 492, t, T.source + 0.05, { out: T.sayOut });
    say(ctx, 'into source.', 140, 492 + 160, t, T.source + 0.35, { out: T.sayOut + 0.08 });
  }
}

/** When byte i leaves the wall (gather): row by row from the top, each row a little from the left. */
const FLY = 0.62;
const departAt = (i) => {
  const r = Math.floor(i / COLS), c = i % COLS;
  return T.gather0 + r * ((T.gather1 - T.gather0 - FLY - 0.16) / ROWS) + (c / COLS) * 0.12 + hash01(11, i) * 0.04;
};

function drawGather(ctx, t) {
  const cam = wideCam();
  const base = ctx.globalAlpha;
  // the scan will draw the code's letters at this scale for the first time: touch them once, invisibly
  // (ink at 1/1024 on the night does not change a pixel), so the GPU has them before the head arrives
  if (t >= T.gather1 - 0.8) {
    ctx.save();
    ctx.globalAlpha = 1 / 1024;
    ctx.translate(960 - cam.x * cam.s, 540 - cam.y * cam.s);
    ctx.scale(cam.s, cam.s);
    drawHero(ctx, S.hero);
    ctx.restore();
  }
  ctx.save();
  if (t >= T.gather1 + 0.05) {
    // every byte has landed: the silhouette as whole lines
    drawHexLines(ctx, cam, HEX.text, () => 1);
    ctx.restore();
    return;
  }
  // 1. the cells no byte of their own lands on fill in once their line is complete
  drawHexLines(ctx, cam, HEX.filler, (l) => progress(t, HEX.done[l] - 0.15, 0.3));
  // 2. the bytes: waiting on the wall, in flight, and set into their two cells of the column
  byteMode(ctx);
  const sizeOnPage = PAGE.size * cam.s;
  for (let i = 0; i < B.n; i++) {
    const r = Math.floor(i / COLS), c = i % COLS;
    const x0 = (c + 0.5) * PITCH, y0 = (r + 0.5) * PITCH;
    const q = clamp((t - departAt(i)) / FLY);
    if (B.kind[i] === 0 || Number.isNaN(B.tx[i])) {
      // metadata (header, constants, line tables): sinks and goes out as its row leaves
      const m = clamp((t - departAt(i) + 0.15) / 0.55);
      const a = (1 - ease.inQuad(m)) * 0.44;
      if (a <= 0.003) continue;
      drawByte(ctx, B.val[i], x0, y0 + 36 * ease.inQuad(m), WALL_TEXT, base * a);
      continue;
    }
    const tone = (B.kind[i] === 2 ? 0.95 : 0.72) * (0.86 + 0.14 * hash01(7, i));
    if (q <= 0) { drawByte(ctx, B.val[i], x0, y0, WALL_TEXT, base * tone * lerp(0.4, 0.24, progress(t, T.gather0, 0.6))); continue; }
    const x1 = 960 + (B.tx[i] - cam.x) * cam.s, y1 = 540 + (B.ty[i] - cam.y) * cam.s;
    if (q >= 1) {
      // set: it is the silhouette now (a byte that shares its cells with an earlier one fades out)
      const a = B.first[i] ? HEX_ALPHA : HEX_ALPHA * (1 - clamp((t - departAt(i) - FLY) / 0.25));
      if (a > 0.003) drawByte(ctx, B.val[i], x1, y1, sizeOnPage, base * a);
      continue;
    }
    // one shared arc: across first, then down into the line, lifting a little on the way
    const ex = ease.inOutCubic(q), ey = ease.inOutSine(q);
    const x = lerp(x0, x1, ex);
    const y = lerp(y0, y1, ey) - Math.sin(Math.PI * q) * 36;
    const size = lerp(WALL_TEXT, sizeOnPage, ex);
    const a = lerp(tone * 0.4, HEX_ALPHA, ease.inOutSine(q)) * (1 + 0.6 * Math.sin(Math.PI * q));
    drawByte(ctx, B.val[i], x, y, size, base * Math.min(1, a));
  }
  ctx.restore();
}

/** How strong the hex silhouette is (in the paper tone on the night). */
const HEX_ALPHA = 0.56;

/** Lines of hex text under the page camera, each drawn whole (`texts[l]`, `alphaOf(l)`). */
function drawHexLines(ctx, cam, texts, alphaOf, { from = -Infinity } = {}) {
  const H = S.hero;
  const F = codeMetrics(PAGE.size, PAGE.lineHeight).f;
  ctx.save();
  ctx.font = F.css;
  const T0 = ctx.getTransform();
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  ctx.fillStyle = C.onNight;
  const base = ctx.globalAlpha;
  for (let l = 0; l < texts.length; l++) {
    const text = texts[l];
    if (!text) continue;
    const y = 540 + (H.lineY(l) + H.baseline - cam.y) * cam.s;
    if (y < from) continue;
    const a = alphaOf(l);
    if (a <= 0) continue;
    ctx.globalAlpha = base * a * HEX_ALPHA;
    ctx.setTransform(T0.a * cam.s, 0, 0, T0.d * cam.s, T0.e + T0.a * (960 + (0 - cam.x) * cam.s), T0.f + T0.d * y);
    ctx.fillText(text, 0, 0);
  }
  ctx.restore();
}

// The hex silhouette: each output line spelled in hex. Every byte that lands on a line takes two
// cells of it (a pair of non-space characters, strings inside the token they spell), so the landed
// bytes are the silhouette; cells left over are filled from a hash when the line completes.
let HEX = null;
function prepareHex(D) {
  const H = S.hero;
  const tgt = D.hero.byte_target;
  const lines = H.named.lines.map((raw) => raw.replace(/\t/g, '    '));
  const cells = lines.map((s, l) => [...s].map((ch, j) => (ch === ' ' ? ' ' : '0123456789abcdef'[Math.floor(hash01(13, l, j) * 16)])));
  const owned = lines.map((s) => new Uint8Array(s.length));
  const perLine = lines.map(() => ({ str: new Map(), code: [] }));
  for (let i = 0; i < B.n; i++) {
    if (B.kind[i] === 0 || tgt[i * 3] < 0) continue;
    const l = tgt[i * 3];
    if (B.kind[i] === 2) {
      const key = tgt[i * 3 + 1] + ':' + tgt[i * 3 + 2];
      if (!perLine[l].str.has(key)) perLine[l].str.set(key, []);
      perLine[l].str.get(key).push(i);
    } else perLine[l].code.push(i);
  }
  const tx = new Float32Array(B.n).fill(NaN), ty = new Float32Array(B.n).fill(NaN), first = new Uint8Array(B.n);
  const done = new Float32Array(lines.length).fill(-1);
  const place = (l, list, a, b) => {
    const s = lines[l];
    // free pairs of non-space cells in [a, b), left to right
    const slots = [];
    for (let c = a; c + 1 < Math.min(b, s.length); c++) {
      if (s[c] !== ' ' && s[c + 1] !== ' ' && !owned[l][c] && !owned[l][c + 1]) { slots.push(c); c++; }
    }
    list.forEach((i, j) => {
      let c;
      if (j < slots.length) {
        c = slots[j];
        owned[l][c] = owned[l][c + 1] = 1;
        cells[l][c] = HEX2[B.val[i]][0];
        cells[l][c + 1] = HEX2[B.val[i]][1];
        first[i] = 1;
      } else c = slots.length ? slots[j % slots.length] : Math.max(0, s.search(/\S/));
      tx[i] = (c + 1) * H.cw;
      ty[i] = H.lineY(l) + H.lh / 2;
      done[l] = Math.max(done[l], departAt(i) + FLY);
    });
  };
  perLine.forEach((p, l) => {
    for (const [key, list] of p.str) {
      const [c0, c1] = key.split(':').map(Number);
      place(l, list, c0, c1);
    }
    place(l, p.code, 0, lines[l].length);
  });
  for (let l = 0; l < lines.length; l++) if (done[l] < 0) done[l] = T.gather1 - 0.4;   // lines no byte lands on
  const text = cells.map((cs) => cs.join('').replace(/\s+$/, ''));
  const filler = cells.map((cs, l) => cs.map((ch, j) => (owned[l][j] ? ' ' : ch)).join('').replace(/\s+$/, ''));
  Object.assign(B, { tx, ty, first });
  HEX = { text: text.map((s) => (s.trim() ? s : '')), filler: filler.map((s) => (s.trim() ? s : '')), done };
}

/**
 * The scan: above the head, paper and the decoded page; below it, night and the hex column. Just-read
 * lines glow in the accent and settle to ink. Drawn by the names shot underneath its own work while
 * the head is still on screen.
 */
export function drawScan(ctx, t, cam) {
  const hy = headY(t);
  const H = S.hero;
  // below the head: the night and the hex
  ctx.save();
  ctx.beginPath();
  ctx.rect(-4, hy, 1928, 1100);
  ctx.clip();
  surface(ctx, C.night);
  drawHexLines(ctx, cam, HEX.text, () => 1, { from: hy - H.lh * cam.s });
  ctx.restore();
  // above it: the paper and the code
  ctx.save();
  ctx.beginPath();
  ctx.rect(-4, -4, 1928, Math.max(0, hy + 4));
  ctx.clip();
  surface(ctx, C.paper);
  ctx.save();
  ctx.translate(960 - cam.x * cam.s, 540 - cam.y * cam.s);
  ctx.scale(cam.s, cam.s);
  const accent = (tk) => {
    const ly = 540 + (H.lineY(tk.line) + H.lh / 2 - cam.y) * cam.s;
    return ly < hy ? clamp(1 - (hy - ly) / 170) * 0.95 : 0;
  };
  drawHero(ctx, H, { accent });
  ctx.restore();
  ctx.restore();
  headH(ctx, hy, envelope(t, T.scan0, T.scan1, 0.12, 0.2), { side: -1 });
}

export { COLS, PITCH, drawHexLines };
