// "Tovek V2.7": the release film. Mist over ink-wash ridges, like the V2.7 page, and in it what the
// release does: copies Luau pasted into a script turn back into calls, five new places it reaches,
// four real games, every edge case exact, names, and where to get it.
//
// Every line of code and every number on screen is real. It is all read from ../data/v27.json,
// written by v27/film/make_v27_data.py from the release binaries' own outputs and measurements.
//
// The cut:
//   I    mist      the ridges, the title condensing out of the cloud
//   II   copies    V2.6.1's output of one test script; it folds into V2.7's, the copies back to calls
//   III  places    five places earlier releases could not reach, one rebuilt line each
//   IV   games     four real games: rebuilt calls, release by release
//   V    exact     every edge-case run, lit by V2.6.1, then by V2.7; review cases and fresh programs
//   VI   names     names from evidence, and how many generated names are left
//   VII  free      free, MIT, the systems, the browser
//   VIII end       the ridges again, the mark, Tovek V2.7
// A frame is a pure function of t: each scene draws its own surface and nothing is carried between
// frames.

import {
  ease, clamp, lerp, progress, envelope, noise1, hash01, font, layoutText, drawText, reveal, drawOdometer,
  formatNumber, codeMorph, drawMorph, fitCamera, withCamera, drawCode, codePalettes, codeFont, drawMark, markWidth,
  defineScore, note, ticks, roundRect,
} from '../engine/index.js';

const CINZEL = '"Cinzel", "Times New Roman", serif';
const LORA = '"Lora", Georgia, serif';
const INTER = '"Inter", system-ui, sans-serif';
const FONTS_CSS = 'https://fonts.googleapis.com/css2?family=Cinzel:wght@400;600&family=Lora:wght@400;600&family=Inter:wght@400;500;600&display=swap';

const C = {
  skyTop: '#cfd6de', skyLow: '#eef0f2', page: '#e8e8e8', card: '#f6f6f6',
  ink: '#111111', ink2: 'rgba(17,17,17,0.62)', ink3: 'rgba(17,17,17,0.4)', hair: 'rgba(0,0,0,0.08)',
  ridges: ['#a7b0bb', '#79838f', '#46505c'], fog: '238,240,243', accent: '#1677ff', track: '#d3d6da', lit: '#9aa3ae',
};
const PAPER = { ...codePalettes.paper, accent: C.accent, wash: 'rgba(22,119,255,0.12)', base: C.card };

const F = {
  mark: font(400, { family: CINZEL, weight: 400 }),
  name: font(58, { family: CINZEL, weight: 400 }),
  title: font(76, { family: LORA, weight: 600 }),
  say: font(46, { family: LORA, weight: 400 }),
  sayS: font(34, { family: LORA, weight: 400 }),
  label: font(26, { family: INTER, weight: 500 }),
  small: font(21, { family: INTER, weight: 400 }),
  num: font(168, { family: INTER, weight: 600 }),
  numM: font(96, { family: INTER, weight: 600 }),
  numS: font(54, { family: INTER, weight: 600 }),
};

// scene times, in seconds
const T = {
  title0: 1.0, name0: 4.4, copies: 9, morph0: 16.2, morph1: 21.2, places: 26.5, games: 39, exact: 48.5, lit26: 50.2,
  lit27: 53.4, review: 56.2, names: 61, free: 67, end: 72.5, duration: 79,
};

const S = { D: null };

// ------------------------------------------------------------------------------------------ surfaces

/** The sky and its ridges: three ink-wash layers, far to near, with fog drifting between them. */
function landscape(ctx, t, { ridges = 1, fog = 1, rise = 0 } = {}) {
  const g = ctx.createLinearGradient(0, 0, 0, 1080);
  g.addColorStop(0, C.skyTop);
  g.addColorStop(0.55, '#dde2e7');
  g.addColorStop(1, C.skyLow);
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, 1920, 1080);
  if (ridges <= 0) return;
  const layers = [
    { base: 700, amp: 150, freq: 0.0015, seed: 11, drift: 4 },
    { base: 790, amp: 180, freq: 0.0021, seed: 23, drift: 9 },
    { base: 890, amp: 200, freq: 0.0027, seed: 37, drift: 16 },
  ];
  layers.forEach((L, i) => {
    const off = t * L.drift;
    const yOf = (x) => {
      let h = 0, a = 1, f = L.freq, n = 0;
      for (let o = 0; o < 3; o++) {
        const v = noise1((x + off) * f, L.seed + o * 7);
        h += a * (1 - Math.abs(v * 2 - 1)) ** 2;
        n += a;
        a *= 0.42;
        f *= 2.3;
      }
      return L.base + rise * (60 + i * 40) - L.amp * (h / n) * 1.25;
    };
    const top = L.base - L.amp * 1.25 + rise * (60 + i * 40);
    const wash = ctx.createLinearGradient(0, top, 0, L.base + 220);
    wash.addColorStop(0, C.ridges[i]);
    wash.addColorStop(0.55, C.ridges[i]);
    wash.addColorStop(1, `rgba(${C.fog},0)`);
    ctx.save();
    ctx.globalAlpha *= ridges;
    ctx.beginPath();
    ctx.moveTo(-20, 1100);
    for (let x = -20; x <= 1940; x += 8) ctx.lineTo(x, yOf(x));
    ctx.lineTo(1940, 1100);
    ctx.closePath();
    ctx.fillStyle = wash;
    ctx.fill();
    ctx.restore();
    fogBand(ctx, t, L.base + 40 + rise * (60 + i * 40), 150 + i * 30, fog * (0.55 + i * 0.12), L.seed);
  });
  fogBand(ctx, t, 1000, 260, fog * 0.95, 91);
}

/** A soft band of fog: a horizontal wash and a few drifting blobs. */
function fogBand(ctx, t, y, h, amount, seed) {
  if (amount <= 0) return;
  const g = ctx.createLinearGradient(0, y - h, 0, y + h);
  g.addColorStop(0, `rgba(${C.fog},0)`);
  g.addColorStop(0.5, `rgba(${C.fog},${0.7 * amount})`);
  g.addColorStop(1, `rgba(${C.fog},0)`);
  ctx.fillStyle = g;
  ctx.fillRect(0, y - h, 1920, h * 2);
  for (let j = 0; j < 5; j++) {
    const speed = 14 + hash01(seed, j, 1) * 22;
    const x = ((hash01(seed, j, 2) * 2600 + t * speed) % 2600) - 340;
    const by = y + (hash01(seed, j, 3) - 0.5) * h;
    const rx = 380 + hash01(seed, j, 4) * 260, ry = h * 0.55;
    ctx.save();
    ctx.translate(x, by);
    ctx.scale(1, ry / rx);
    const r = ctx.createRadialGradient(0, 0, 0, 0, 0, rx);
    r.addColorStop(0, `rgba(${C.fog},${0.65 * amount})`);
    r.addColorStop(1, `rgba(${C.fog},0)`);
    ctx.fillStyle = r;
    ctx.fillRect(-rx, -rx, rx * 2, rx * 2);
    ctx.restore();
  }
}

function page(ctx) {
  ctx.fillStyle = C.page;
  ctx.fillRect(0, 0, 1920, 1080);
}

/** Text that rises in word by word and leaves the same way. */
function say(ctx, text, x, y, t, at, { f = F.say, color = C.ink, out = Infinity, align = 'left', maxWidth = 1500, unit = 'word' } = {}) {
  const L = layoutText(text, f, { maxWidth, lineHeight: 1.3 });
  reveal(ctx, L, x, y, t, { unit, start: at, stagger: 0.05, dur: 0.8, color, align, out: { start: out, stagger: 0.02, dur: 0.5 } });
}

function label(ctx, text, x, y, { f = F.label, color = C.ink2, align = 'left', alpha = 1 } = {}) {
  if (alpha <= 0) return;
  drawText(ctx, layoutText(text, f), x, y, { color, align, alpha });
}

/** The title written in Cinzel, condensing out of a blur of white as it lands. */
function condensed(ctx, text, x, y, f, p, { color = '255,255,255', glow = 1 } = {}) {
  if (p <= 0) return;
  const L = layoutText(text, f);
  const tracking = lerp(0.18, 0.02, ease.out(p));
  ctx.save();
  ctx.shadowColor = `rgba(255,255,255,${0.9 * glow})`;
  ctx.shadowBlur = lerp(90, 22, p) * (f.size / 400);
  const spread = (1 - p) * f.size * 0.06;
  for (let k = 0; k < 4; k++) {
    const a = k === 0 ? p : p * (1 - p) * 0.5;
    if (a <= 0.003) continue;
    const ang = k * 2.1;
    drawText(ctx, L, x + Math.cos(ang) * spread * (k ? 1 : 0), y + Math.sin(ang) * spread * (k ? 1 : 0), {
      color: `rgba(${color},1)`, align: 'center', tracking, alpha: a,
    });
  }
  ctx.restore();
}

// ------------------------------------------------------------------------------------------- I. mist

function drawMist(ctx, t) {
  landscape(ctx, t, { rise: 0 });
  const p = progress(t, T.title0, 3.2, ease.out);
  condensed(ctx, '2.7', 960, 560, F.mark, p);
  fogBand(ctx, t, 600, 120, 0.35 * (1 - p * 0.6), 5);
  const n = progress(t, T.name0, 1.6, ease.out);
  condensed(ctx, 'Tovek V2.7', 960, 740, F.name, n, { glow: 0.6 });
}

// ------------------------------------------------------------------------------------------ II. copies

const BOX = { x: 760, y: 120, w: 1000, h: 840 };

function morphPlan() {
  const M = S.D.morph;
  return codeMorph(M.before, M.after);
}

function drawCopies(ctx, t) {
  const M = S.D.morph;
  page(ctx);
  const plan = morphPlan();
  const p = progress(t, T.morph0, T.morph1 - T.morph0, ease.inOut);
  // the card
  ctx.save();
  ctx.fillStyle = C.card;
  roundRect(ctx, BOX.x - 40, BOX.y - 36, BOX.w + 80, BOX.h + 72, 24);
  ctx.fill();
  ctx.restore();
  const opts = {
    box: BOX, size: 24, lineHeight: 1.55, palette: PAPER, clip: true, minScale: 0.04, maxScale: 1,
    linesB: M.frame_after, fit: 'contain', align: 'center',
  };
  const enter = progress(t, T.copies, 1.2, ease.out);
  // V2.6.1's whole file scrolls by at a readable size, then the camera pulls back until all of it
  // fits; the morph leaves from there
  const WINDOW = 26;
  const top = (plan.a.lineCount - WINDOW) * progress(t, T.copies + 1.2, T.morph0 - T.copies - 3.0, ease.inOut);
  const read = fitCamera(plan.a, BOX, { size: 24, lineHeight: 1.55, lines: [0, WINDOW - 1], minScale: 0.04 });
  read.y -= top * 24 * 1.55 * read.s;
  const whole = fitCamera(plan.a, BOX, { size: 24, lineHeight: 1.55, minScale: 0.04 });
  const pull = progress(t, T.morph0 - 1.6, 1.5, ease.inOut);
  // linear in scale and offset, so the last line stays on the bottom edge all the way
  const camA = { s: lerp(read.s, whole.s, pull), x: lerp(read.x, whole.x, pull), y: lerp(read.y, whole.y, pull) };
  ctx.save();
  ctx.globalAlpha *= enter;
  if (p < 1) {
    drawMorph(ctx, plan, p, { ...opts, cameraA: camA });
  } else {
    const cam = fitCamera(plan.b, BOX, { size: 24, lineHeight: 1.55, lines: M.frame_after, minScale: 0.04 });
    ctx.save();
    ctx.beginPath();
    ctx.rect(BOX.x, BOX.y, BOX.w, BOX.h);
    ctx.clip();
    withCamera(ctx, cam, () => drawCode(ctx, plan.b, { size: 24, lineHeight: 1.55, palette: PAPER, highlight: plan.inserted }));
    ctx.restore();
  }
  ctx.restore();

  // the left column: which release, how long, how many calls
  const a = enter;
  const which = p < 0.5 ? 'Tovek V2.6.1' : 'Tovek V2.7';
  const swap = 1 - Math.abs(clamp((p - 0.5) / 0.12, -1, 1));
  label(ctx, 'One test script, compiled with Luau -O2', 150, 220, { alpha: a, color: C.ink3, f: F.small });
  ctx.save();
  ctx.globalAlpha *= a * (1 - swap);
  drawText(ctx, layoutText(which, F.title), 150, 330, { color: p < 0.5 ? C.ink : C.accent });
  ctx.restore();
  const calls = p >= 0.75 ? M.after_calls : M.before_calls;
  ctx.save();
  ctx.globalAlpha *= a;
  drawOdometer(ctx, t, { from: M.before_lines, to: M.after_lines, start: T.morph0 + 0.8, dur: 3.2, font: F.numM, x: 150, y: 520, color: C.ink, exact: false });
  label(ctx, 'lines of output', 152, 566, { color: C.ink2 });
  drawOdometer(ctx, t, { from: M.before_calls, to: M.after_calls, start: T.morph0 + 3.4, dur: 1.2, font: F.numM, x: 150, y: 730, color: calls ? C.accent : C.ink });
  label(ctx, 'calls rebuilt, each one checked', 152, 776, { color: C.ink2 });
  ctx.restore();
}

// ------------------------------------------------------------------------------------------ III. places

function drawPlaces(ctx, t) {
  page(ctx);
  const P = S.D.places;
  const a = progress(t, T.places, 0.8);
  ctx.save();
  ctx.globalAlpha *= a;
  say(ctx, 'Five places earlier releases could not reach', 150, 200, t, T.places + 0.2, { f: F.title, out: T.games - 0.7, maxWidth: 1750 });
  P.forEach((row, i) => {
    const at = T.places + 1.6 + i * 1.7;
    const y = 360 + i * 136;
    const q = progress(t, at, 0.9, ease.out) * (1 - progress(t, T.games - 0.8, 0.6));
    if (q <= 0) return;
    ctx.save();
    ctx.globalAlpha *= q;
    ctx.translate(0, (1 - q) * 18);
    label(ctx, row.label, 150, y, { f: F.sayS, color: C.ink });
    const code = layoutCodeLine(row.code);
    ctx.fillStyle = C.ink;
    drawCode(ctx, code.L, { x: 152, y: y + 26, size: 26, lineHeight: 1.5, palette: PAPER, highlight: code.hl });
    ctx.restore();
  });
  ctx.restore();
}

const codeLines = new Map();
function layoutCodeLine(text) {
  if (!codeLines.has(text)) {
    const plan = codeMorph('', text);
    const callee = (text.match(/([A-Za-z_][\w.]*)\s*\(/) || [])[1];
    const hl = new Set();
    plan.b.tokens.forEach((tk, i) => { if (tk.text === callee || tk.k === 'com') hl.add(i); });
    codeLines.set(text, { L: plan.b, hl });
  }
  return codeLines.get(text);
}

// ------------------------------------------------------------------------------------------- IV. games

function drawGames(ctx, t) {
  page(ctx);
  const G = S.D.games;
  const a = progress(t, T.games, 0.8) * (1 - progress(t, T.exact - 0.6, 0.6));
  if (a <= 0) return;
  ctx.save();
  ctx.globalAlpha *= a;
  say(ctx, `Four real games, ${formatNumber(G.scripts)} scripts`, 150, 220, t, T.games + 0.1, { f: F.title });
  drawOdometer(ctx, t, { from: G.calls['v2.6.1'], to: G.calls['v2.7'], start: T.games + 1.4, dur: 2.6, font: F.num, x: 150, y: 520, color: C.ink });
  label(ctx, 'helper calls rebuilt', 154, 580, { color: C.ink2, f: F.label });
  const rows = [['V2.5.1', G.calls['v2.5.1']], ['V2.6.1', G.calls['v2.6.1']], ['V2.7', G.calls['v2.7']]];
  const max = Math.max(...rows.map((r) => r[1]));
  rows.forEach(([name, v], i) => {
    const y = 720 + i * 72;
    const q = progress(t, T.games + 1.0 + i * 0.35, 1.4, ease.out);
    label(ctx, `Tovek ${name}`, 150, y + 14, { color: i === 2 ? C.ink : C.ink2, f: F.label });
    ctx.fillStyle = i === 2 ? C.accent : C.track;
    ctx.fillRect(400, y - 8, 1180 * (v / max) * q, 30);
    label(ctx, formatNumber(Math.round(v * q)), 400 + 1180 * (v / max) * q + 18, y + 14, { color: C.ink, f: F.small, alpha: q });
  });
  ctx.restore();
}

// -------------------------------------------------------------------------------------------- V. exact

function grid() {
  const E = S.D.edge;
  const cols = 60, cell = 21, gap = 4;
  const rows = Math.ceil(E.runs / cols);
  const w = cols * (cell + gap) - gap;
  return { cols, rows, cell, gap, x: (1920 - w) / 2, y: 470, w };
}

function drawExact(ctx, t) {
  page(ctx);
  const E = S.D.edge, R = S.D.review;
  const a = progress(t, T.exact, 0.8) * (1 - progress(t, T.names - 0.6, 0.6));
  if (a <= 0) return;
  ctx.save();
  ctx.globalAlpha *= a;
  const gone = progress(t, T.review - 0.2, 0.6, ease.inOut);
  ctx.save();
  ctx.globalAlpha *= 1 - gone;
  say(ctx, `${formatNumber(E.fixtures)} edge cases, each at -O0, -O1 and -O2`, 150, 220, t, T.exact + 0.1, { f: F.title });
  const g = grid();
  const lit26 = progress(t, T.lit26, 2.2, ease.inOut);
  const lit27 = progress(t, T.lit27, 1.6, ease.inOut);
  for (let i = 0; i < E.runs; i++) {
    const cx = g.x + (i % g.cols) * (g.cell + g.gap), cy = g.y + Math.floor(i / g.cols) * (g.cell + g.gap);
    const order = i / E.runs;
    const was = E.passed['v2.6.1'][i], now = E.passed['v2.7'][i];
    let fill = C.track;
    if (was && lit26 > order) fill = C.lit;
    if (!was && now && lit27 > hash01(7, i)) fill = C.accent;
    ctx.fillStyle = fill;
    ctx.fillRect(cx, cy, g.cell, g.cell);
  }
  const n26 = E.correct['v2.6.1'], n27 = E.correct['v2.7'];
  const shown = lit27 > 0 ? Math.round(lerp(n26, n27, lit27)) : Math.round(n26 * lit26);
  const by = g.y + g.rows * (g.cell + g.gap) + 90;
  drawText(ctx, layoutText(`${formatNumber(shown)} of ${formatNumber(E.runs)}`, F.numS), g.x, by, { color: lit27 > 0 ? C.accent : C.ink });
  label(ctx, lit27 > 0 ? 'decompiled exactly by Tovek V2.7' : 'decompiled exactly by Tovek V2.6.1', g.x, by + 44, { color: C.ink2 });
  ctx.restore();
  // review cases and fresh programs
  if (t >= T.review) {
    say(ctx, 'And on cases from our reviews', 150, 260, t, T.review + 0.2, { f: F.title });
    drawOdometer(ctx, t, { from: R.wrong['v2.6.1'], to: R.wrong['v2.7'], start: T.review + 1.0, dur: 1.8, font: F.num, x: 150, y: 560, color: C.ink });
    label(ctx, `of ${formatNumber(R.cases)} review cases decompiled wrong (V2.6.1: ${formatNumber(R.wrong['v2.6.1'])})`, 154, 620, { color: C.ink2, alpha: progress(t, T.review + 1.0, 0.6) });
    const fz = S.D.fuzz;
    const q = progress(t, T.review + 2.4, 0.8, ease.out);
    drawText(ctx, layoutText('0', F.num), 150, 880, { color: C.ink, alpha: q });
    label(ctx, `wrong outputs on ${formatNumber(fz.programs)} fresh programs; patched bytecode is refused, not guessed`, 154, 940, { color: C.ink2, alpha: q });
  }
  ctx.restore();
}

// -------------------------------------------------------------------------------------------- VI. names

function drawNames(ctx, t) {
  page(ctx);
  const N = S.D.names;
  const a = progress(t, T.names, 0.8) * (1 - progress(t, T.free - 0.6, 0.6));
  if (a <= 0) return;
  ctx.save();
  ctx.globalAlpha *= a;
  say(ctx, 'Names, from evidence', 150, 220, t, T.names + 0.1, { f: F.title });
  N.examples.forEach((ex, i) => {
    const q = progress(t, T.names + 0.9 + i * 0.5, 0.8, ease.out);
    if (q <= 0) return;
    const plan = codeMorph('', ex.code);
    const hl = new Set();
    plan.b.tokens.forEach((tk, k) => { if (tk.text === ex.name) hl.add(k); });
    ctx.save();
    ctx.globalAlpha *= q;
    drawCode(ctx, plan.b, { x: 152, y: 400 + i * 92 + (1 - q) * 14, size: 32, lineHeight: 1.4, palette: PAPER, highlight: hl });
    ctx.restore();
  });
  const G = S.D.games;
  drawOdometer(ctx, t, { from: G.generated['v2.6.1'], to: G.generated['v2.7'], start: T.names + 2.6, dur: 2.2, font: F.numM, x: 1100, y: 520, color: C.ink });
  label(ctx, 'generated names (v, p) left in four games', 1104, 570, { color: C.ink2, alpha: progress(t, T.names + 2.6, 0.6) });
  label(ctx, `from ${formatNumber(G.generated['v2.6.1'])} in V2.6.1`, 1104, 606, { color: C.ink3, alpha: progress(t, T.names + 3.4, 0.6) });
  ctx.restore();
}

// ---------------------------------------------------------------------------------------------- VII. free

function drawFree(ctx, t) {
  page(ctx);
  const a = progress(t, T.free, 0.6) * (1 - progress(t, T.end - 0.6, 0.6));
  if (a <= 0) return;
  ctx.save();
  ctx.globalAlpha *= a;
  const out = T.end - 0.7;
  say(ctx, 'Free.', 150, 360, t, T.free + 0.1, { f: F.title, out });
  say(ctx, 'Open source.', 150, 470, t, T.free + 0.4, { f: F.title, out });
  say(ctx, `${S.D.license} license.`, 150, 580, t, T.free + 0.7, { f: F.title, out });
  S.D.platforms.forEach((p, i) => say(ctx, p, 1120, 360 + i * 110, t, T.free + 1.3 + i * 0.3, { f: F.title, color: C.ink2, out }));
  label(ctx, 'luau-lifter and web-server, in one download', 152, 760, { alpha: progress(t, T.free + 2.6, 0.6) * (1 - progress(t, out, 0.4)) });
  label(ctx, 'or decompile in the browser: nothing is uploaded', 1122, 760, { alpha: progress(t, T.free + 3.0, 0.6) * (1 - progress(t, out, 0.4)) });
  ctx.restore();
}

// ---------------------------------------------------------------------------------------------- VIII. end

function drawEnd(ctx, t) {
  const p = progress(t, T.end, 1.4, ease.inOut);
  ctx.save();
  ctx.globalAlpha *= p;
  landscape(ctx, t, { rise: 1 - p });
  const m = progress(t, T.end + 0.8, 1.2, ease.out);
  const size = 96;
  ctx.save();
  ctx.globalAlpha *= m;
  drawMark(ctx, 960 - markWidth(size) / 2, 200, size, { color: C.ink, bits: progress(t, T.end + 1.0, 1.4) });
  ctx.restore();
  condensed(ctx, 'Tovek V2.7', 960, 410, F.name, progress(t, T.end + 1.4, 1.6, ease.out), { color: '17,17,17', glow: 0.5 });
  const u = progress(t, T.end + 2.6, 0.8);
  if (u > 0) {
    label(ctx, S.D.site, 960, 476, { color: C.ink2, align: 'center', alpha: u, f: F.label });
  }
  ctx.restore();
}

// ------------------------------------------------------------------------------------------------ the film

const SCENES = [
  { t0: 0, t1: T.copies + 1.3, draw: drawMist },
  { t0: T.copies, t1: T.places + 1, draw: drawCopies, fadeIn: 1.2 },
  { t0: T.places, t1: T.games + 1, draw: drawPlaces, fadeIn: 0.9 },
  { t0: T.games, t1: T.exact + 1, draw: drawGames },
  { t0: T.exact, t1: T.names + 1, draw: drawExact },
  { t0: T.names, t1: T.free + 1, draw: drawNames },
  { t0: T.free, t1: T.end + 1.5, draw: drawFree },
  { t0: T.end, t1: T.duration + 1, draw: drawEnd },
];

function render(ctx, t) {
  for (const s of SCENES) {
    if (t < s.t0 || t >= s.t1) continue;
    ctx.save();
    if (s.fadeIn) ctx.globalAlpha *= progress(t, s.t0, s.fadeIn, ease.inOut);
    s.draw(ctx, t);
    ctx.restore();
  }
}

async function loadFonts() {
  if (typeof document === 'undefined' || !document.fonts) return;
  if (!document.querySelector('link[data-v27-fonts]')) {
    const link = document.createElement('link');
    link.rel = 'stylesheet';
    link.href = FONTS_CSS;
    link.dataset.v27Fonts = '';
    const loaded = new Promise((res) => { link.onload = res; link.onerror = res; });
    document.head.append(link);
    await Promise.race([loaded, new Promise((res) => setTimeout(res, 4000))]);
  }
  const specs = Object.values(F).map((f) => f.css);
  await Promise.all(specs.map((s) => document.fonts.load(s, 'Tovek V2.7 0123456789,').catch(() => null)));
}

const film = {
  id: 'v27',
  title: 'Tovek V2.7',
  description: 'What V2.7 gives back: real output, measured numbers.',
  duration: T.duration,
  poster: 6.4,
  posterTitle: false,
  background: C.page,
  fonts: [codeFont(24).css, codeFont(26).css, codeFont(32).css],
  glyphs: 'Tovek 0123456789 ,.-()[]{}=:"',
  chapters: [],
  captions: [],
  score: undefined,

  async prepare() {
    if (S.D) return;
    await loadFonts();
    S.D = await (await fetch(new URL('../data/v27.json', import.meta.url))).json();
    buildChaptersCaptions();
    film.score = buildScore();
  },
  render: (ctx, t) => render(ctx, t),
};

function buildChaptersCaptions() {
  const D = S.D, M = D.morph, G = D.games, E = D.edge, R = D.review, fmt = formatNumber;
  film.chapters = [
    { t: 0, title: 'Tovek V2.7', still: 6.4 },
    { t: T.copies, title: 'Copies back to calls', still: T.morph1 + 1.5 },
    { t: T.places, title: 'Five new places', still: T.games - 1.2 },
    { t: T.games, title: 'Four real games', still: T.exact - 2 },
    { t: T.exact, title: 'Every edge case, exact', still: T.review - 0.6 },
    { t: T.names, title: 'Names', still: T.free - 1.2 },
    { t: T.free, title: 'Free and open source', still: T.end - 1 },
    { t: T.end, title: 'Tovek V2.7', still: T.duration - 1 },
  ];
  const c = [];
  const add = (start, end, text) => c.push({ start: +start.toFixed(2), end: +end.toFixed(2), text });
  add(1.0, 4.2, 'Tovek V2.7.');
  add(4.4, 8.6, 'It gives back more of what the Luau compiler took away.');
  add(T.copies + 0.4, T.copies + 3.8, 'Luau copies small helper functions into the code that calls them.');
  add(T.copies + 4.0, T.morph0 - 0.1, `This test script came out of Tovek V2.6.1 as ${fmt(M.before_lines)} lines, with no calls rebuilt.`);
  add(T.morph0, T.morph1 - 0.1, 'V2.7 proves each copy is one, and writes the call back.');
  add(T.morph1, T.places - 0.2, `${fmt(M.after_lines)} lines. Every call is marked, and checked against the bytecode.`);
  add(T.places + 0.3, T.games - 0.2, 'Five kinds of copy that earlier releases left in place.');
  add(T.games + 0.2, T.exact - 0.2, `Across four real games, rebuilt calls go from ${fmt(G.calls['v2.6.1'])} to ${fmt(G.calls['v2.7'])}.`);
  add(T.exact + 0.2, T.lit27 - 0.1, `Each edge case is decompiled, compiled again and run. V2.6.1 got ${fmt(E.correct['v2.6.1'])} of ${fmt(E.runs)} right.`);
  add(T.lit27, T.review - 0.1, `V2.7 gets all ${fmt(E.runs)}.`);
  add(T.review + 0.2, T.review + 2.3, `Review cases decompiled wrong: ${fmt(R.wrong['v2.6.1'])} in V2.6.1, ${fmt(R.wrong['v2.7'])} now.`);
  add(T.review + 2.4, T.names - 0.2, `${fmt(D.fuzz.programs)} fresh programs, no wrong output.`);
  add(T.names + 0.2, T.free - 0.2, `Names come from evidence. Generated names in the four games: ${fmt(G.generated['v2.6.1'])} down to ${fmt(G.generated['v2.7'])}.`);
  add(T.free + 0.2, T.end - 0.3, `Free and open source, under the ${D.license} license.`);
  add(T.end + 1.4, T.end + 4.6, `Tovek V2.7. ${D.site}`);
  add(T.end + 4.8, T.duration - 0.4, 'Built on medal, by Jujhar Singh and Mathias Pedersen.');
  film.captions = c;
}

function buildScore() {
  const E = [];
  const D0 = T.duration;
  const pulse = (t, n, gain = 0.07, dur = 2.6) => E.push({ t, voice: 'pulse', freq: note(n), gain, dur });
  const chime = (t, n, gain = 0.04, dur = 2.6) => E.push({ t, voice: 'chime', freq: note(n), gain, dur });
  E.push({ t: 0, voice: 'air', dur: D0, gain: 0.03, cutoff: 900 });
  E.push({ t: 0.3, voice: 'hum', dur: T.copies + 1, freq: note('D2'), gain: 0.1, attack: 3, release: 3 });
  E.push({ t: T.title0, voice: 'swell', dur: 7, notes: ['D3', 'A3', 'F#4'].map(note), gain: 0.08, attack: 3, release: 3, open: 1600 });
  chime(T.name0 + 0.4, 'A5', 0.035);
  pulse(T.copies + 0.2, 'D3');
  E.push({ t: T.morph0, voice: 'swell', dur: 6.4, notes: ['D3', 'A3', 'D4', 'F#4'].map(note), gain: 0.14, attack: 2.6, release: 3, open: 2400 });
  E.push(...ticks(T.morph0 + 2.5, T.morph1, { rate: 16, seed: 3, gain: 0.02 }));
  chime(T.morph1 - 0.3, 'D6', 0.04);
  pulse(T.places + 0.2, 'F#3');
  [0, 1, 2, 3, 4].forEach((i) => chime(T.places + 1.7 + i * 1.7, ['A4', 'D5', 'F#5', 'A5', 'D6'][i], 0.026, 1.8));
  pulse(T.games + 0.2, 'A3');
  E.push(...ticks(T.games + 1.4, T.games + 4, { rate: 30, seed: 5, gain: 0.01, jitter: 0.1 }));
  chime(T.games + 4.1, 'E5', 0.035);
  pulse(T.exact + 0.2, 'D3');
  E.push(...ticks(T.lit26, T.lit26 + 2.2, { rate: 40, seed: 7, gain: 0.008, jitter: 0.1 }));
  E.push(...ticks(T.lit27, T.lit27 + 1.6, { rate: 22, seed: 9, gain: 0.014, jitter: 0.1 }));
  chime(T.lit27 + 1.7, 'F#5', 0.045, 3);
  pulse(T.review + 0.2, 'A3');
  chime(T.review + 2.9, 'A5', 0.035);
  pulse(T.names + 0.2, 'B3');
  E.push(...ticks(T.names + 2.6, T.names + 4.8, { rate: 30, seed: 11, gain: 0.01, jitter: 0.1 }));
  pulse(T.free + 0.2, 'D4', 0.06);
  E.push({ t: T.end, voice: 'hum', dur: D0 - T.end, freq: note('D2'), gain: 0.11, attack: 2, release: 3 });
  E.push({ t: T.end + 0.8, voice: 'swell', dur: D0 - T.end - 0.8, notes: ['D3', 'A3', 'D4', 'F#4', 'A4'].map(note), gain: 0.1, attack: 2, release: 3 });
  chime(T.end + 1.5, 'D6', 0.05, 3.4);
  return defineScore({ duration: D0, seed: 27, reverb: { seconds: 3.8, wet: 0.32 }, events: E });
}

export default film;
