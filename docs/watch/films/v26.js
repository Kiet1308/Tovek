// "Tovek V2.6": the launch film. What Tovek is, and what V2.6 brings, in under two minutes.
//
// Every byte, line of code and number on screen is real: the hero script's bytecode and Tovek's output
// of it, Roblox's own ClickToMoveController for the helpers, the fuzz campaign program by program, and
// four real games timed on this machine. It is all read from ../data/launch.json, written by
// v26/data/launch.py (make_all.py --only launch), so the film follows the data when the final V2.6
// binary regenerates it.
//
// One device carries the film: the read head, a hairline of the accent that turns bytecode (night)
// into source (paper). The cut (films/v26/*.js), on a 96 bpm grid:
//   open.js    I    bytes      close on the bytes; the camera pulls back until the file fills the frame
//              II   source     the bytes gather into the shape of the code; the head scans it into Luau
//   names.js   III  names      the names drain away, then come back from their evidence, close up
//              IV   loops      the head sweeps the loop back to its jumps, and forward into its structure
//   new.js     V    V2.6       New in V2.6; the head opens it along the middle
//              VI   helpers    one pasted copy and its line numbers; eleven copies fold into calls
//              VII  constants  0.016666666666666666, its 64 bits, Luau's fold of 1 / 60, frames(1)
//   proof.js   VIII checked    silence; 3,000 programs, lit as the head passes; 0 wrong
//              IX   scale      four games, every script, in real time
//   close.js   X    free       free, MIT, three systems, the command line and the server, the browser
//              XI   end        the mark, Tovek V2.6, the site, medal
// A frame is a pure function of t: each shot draws its own surface, later shots draw over earlier ones
// while they arrive, and nothing is carried between frames.

import { V26, defineScore, note, ticks, codeFont } from '../engine/index.js';
import { T, TY, S, BEAT, BAR, f, fmt, timeFromData, buildHero, PAGE } from './v26/base.js';
import { prepareOpen, drawBytes, drawSource } from './v26/open.js';
import { prepareNames, drawPage } from './v26/names.js';
import { prepareHelpers, drawSting, drawHelpers, drawConstants, foldTimes, FOLD_DUR } from './v26/new.js';
import { prepareProof, drawChecked, drawScale } from './v26/proof.js';
import { drawFree, drawEnd } from './v26/close.js';

// Each shot draws from t0 to t1. Where two overlap, the later one draws over the earlier.
let SHOTS = [];
function buildShots() {
  SHOTS = [
    { id: 'bytes', t0: 0, t1: T.source, draw: drawBytes },
    { id: 'source', t0: T.source, t1: T.scan0, draw: drawSource },
    { id: 'page', t0: T.scan0, t1: T.sting, draw: drawPage },
    { id: 'helpers', t0: T.split, t1: T.consts, draw: drawHelpers },
    { id: 'sting', t0: T.sting, t1: T.split + 1.05, draw: (ctx, t) => drawSting(ctx, t) },
    { id: 'constants', t0: T.consts, t1: T.checked, draw: drawConstants },
    { id: 'checked', t0: T.checked, t1: T.scale, draw: drawChecked },
    { id: 'scale', t0: T.scale, t1: T.free, draw: drawScale },
    { id: 'free', t0: T.free - 0.6, t1: T.end, draw: drawFree },
    { id: 'end', t0: T.end, t1: T.duration + 1, draw: drawEnd },
  ];
}

function render(ctx, t, w, h, warming = false) {
  if (!warming) warmUp(ctx, t);
  for (const s of SHOTS) {
    if (t < s.t0 || t >= s.t1) continue;
    ctx.save();
    s.draw(ctx, t);
    ctx.restore();
  }
}

// ------------------------------------------------------------------------------- warming up
// The first time a shot's type is drawn at the stage's pixel size, the GPU rasterises its glyphs. In
// idle time, ahead of the playhead, the film draws its own frames on a hidden canvas of the same size,
// so the glyphs are ready before playback reaches them. This only fills caches; it never changes what
// a frame looks like.

const WARM_STEP = 0.2;
let warm = null;

function warmUp(ctx, t) {
  const c = ctx.canvas;
  if (typeof document === 'undefined' || !c || c.width < 640 || !('requestAnimationFrame' in globalThis)) return;
  if (ctx.getContextAttributes && ctx.getContextAttributes().willReadFrequently) return; // the export's CPU canvas
  if (warm && warm.w === c.width && warm.h === c.height) { warm.lastT = t; return; }
  if (warm && warm.w * warm.h > c.width * c.height && !warm.finished) return; // keep the larger stage
  const canvas = typeof OffscreenCanvas !== 'undefined' ? new OffscreenCanvas(c.width, c.height) : null;
  if (!canvas || !canvas.transferToImageBitmap) return;
  const wctx = canvas.getContext('2d', { alpha: false });
  const n = Math.ceil(T.duration / WARM_STEP);
  warm = { w: c.width, h: c.height, canvas, wctx, done: new Uint8Array(n), left: n, lastT: 0, finished: false, t0: performance.now() };
  const idle = globalThis.requestIdleCallback || ((cb) => setTimeout(() => cb({ timeRemaining: () => 8 }), 40));
  const W = warm;
  // first the frames where something is drawn for the first time (each shot's start, each new size of
  // display type), then whatever lies ahead of the playhead
  const firsts = [...new Set(firstTimes().map((t) => Math.min(n - 1, Math.max(0, Math.round(t / WARM_STEP)))))];
  const step = (deadline) => {
    if (warm !== W) return;
    while (W.left > 0 && deadline.timeRemaining() > 5) {
      let i = -1;
      while (firsts.length && i < 0) { const k = firsts.shift(); if (!W.done[k]) i = k; }
      if (i < 0) {
        i = Math.max(0, Math.floor(W.lastT / WARM_STEP));
        while (i < n && W.done[i]) i++;
        if (i >= n) { i = 0; while (i < n && W.done[i]) i++; }
      }
      W.done[i] = 1;
      W.left--;
      W.wctx.setTransform(W.w / 1920, 0, 0, W.h / 1080, 0, 0);
      try { render(W.wctx, i * WARM_STEP, 1920, 1080, true); W.canvas.transferToImageBitmap().close(); } catch { W.left = 0; }
    }
    if (W.left > 0) idle(step, { timeout: 500 });
    else { W.finished = true; W.ms = performance.now() - W.t0; W.canvas.width = W.canvas.height = 1; }
  };
  idle(step, { timeout: 500 });
}

/** Moments that draw something new: each shot's first frames and each first appearance of display type. */
function firstTimes() {
  const out = SHOTS.map((sh) => sh.t0 + 0.1);
  const k = [
    T.first + 0.5, T.line1 + 0.7, T.label + 0.5, T.source + 0.7, T.gather0 + 0.5, T.gather1 + 0.2, T.scan0 + 0.3, T.scan1 + 0.4,
    T.scan1 + 1.4, T.names + 0.6, T.n1 + 1.2, T.n2 + 1.2, T.n3 + 1.2, T.n4 + 1.2, T.namesNum + 1.2, T.loops + 1.0, T.back0 + 0.5,
    T.arcs0 + 1.0, T.fwd0 + 0.8, T.loopsNum + 1.0, T.sting + 1.5, T.split + 0.5, T.evidence0 + 1.5, T.hpull + 0.8, T.fold0 + 0.6,
    T.fold1 + 0.6, T.helpersNum + 1.2, T.consts + 0.5, T.bits0 + 1.0, T.match0 + 1.0, T.back + 1.0, T.frac0 + 1.2, T.say8 + 0.7,
    T.grid0 + 1.0, T.zero + 1.2, T.scale + 0.7, T.sweep0 + 1.0, T.total + 1.2, T.free + 1.3, T.os + 0.9, T.term0 + 1.6,
    T.browser + 0.9, T.mark0 + 1.0, T.word + 1.2, T.url + 1.0,
  ];
  return out.concat(k).filter((t) => t >= 0 && t < T.duration);
}

// --------------------------------------------------------------------------------- the film

const MONO = [15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 30, 46, 56, 72].map((s) => codeFont(s).css);
const film = {
  id: 'v26',
  title: 'Tovek V2.6',
  description: 'What Tovek is, and what V2.6 brings: real bytecode, real output, measured numbers.',
  duration: T.duration,
  poster: 1.3,
  background: V26.night,
  fonts: [...new Set([
    ...Object.values(TY).map((x) => x.css), ...MONO,
    f(25, 'mono', 500).css, f(48, 'text', 500).css, f(36, 'text', 400).css, f(26, 'text', 400).css, f(21, 'text', 400).css,
    f(76, 'display', 640).css, f(380, 'display', 640).css,
  ])],
  glyphs: 'Tovek 0123456789abcdef →·—–…×#$-_/:()[]{}"\'.,=<>+%',
  chapters: [{ t: 0, title: 'Bytecode' }],
  captions: [],
  score: undefined,

  async prepare() {
    if (S.D) return;
    const D = await (await fetch(new URL('../data/launch.json', import.meta.url))).json();
    S.D = D;
    timeFromData(D);
    S.hero = buildHero(D);
    prepareOpen();
    prepareNames();
    prepareHelpers();
    prepareProof();
    buildShots();
    buildChaptersCaptions();
    film.score = buildScore();
  },

  render: (ctx, t, w, h) => render(ctx, t, w, h),
  warmState: () => (warm ? { left: warm.left, total: warm.done.length, finished: warm.finished, ms: warm.ms ?? performance.now() - warm.t0 } : null),
};

// ---------------------------------------------------------------------------- chapters and captions

function buildChaptersCaptions() {
  const D = S.D, sc = D.scale, h = D.hero, fz = D.fuzz, hp = D.helper, k = D.constant;
  const browser = D.browser && D.browser.available;
  film.chapters = [
    { t: 0, title: 'Every game ships as bytecode', still: T.label + 0.8 },
    { t: T.source, title: 'Back to source', still: T.scan1 + 1.4 },
    { t: T.names, title: 'Names, from evidence', still: T.n1 + 1.7 },
    { t: T.loops, title: 'Loops, not jumps', still: T.fwd1 + 0.9 },
    { t: T.sting, title: 'New in V2.6', still: T.sting + 2.4 },
    { t: T.split, title: 'Helpers, rebuilt', still: T.evidence0 + 2.9 },
    { t: T.consts, title: 'Constants, solved', still: T.match1 + 0.3 },
    { t: T.checked, title: 'Checked against the bytecode', still: T.zero + 2.0 },
    { t: T.scale, title: 'Four real games', still: T.total + 2.6 },
    { t: T.free, title: 'Free and open source', still: T.term0 + 3.9 },
    { t: T.end, title: 'Tovek V2.6', still: T.duration - 1.0 },
  ];
  const c = [];
  const add = (start, end, text) => { if (text && end > start) c.push({ start: +start.toFixed(2), end: +end.toFixed(2), text }); };
  const featured = h.featured_names.slice(0, 4);
  const at = [T.n1, T.n2, T.n3, T.n4];
  const level = (h.compiler.match(/-O\d/) || ['-O2'])[0];
  // I-II
  add(0.9, 5.4, 'Every Roblox game ships as bytecode.');
  add(5.6, 9.9, `This script is ${fmt(h.bytes)} bytes: written for this film, compiled by Luau at ${level}.`);
  add(T.source + 0.1, T.sayOut + 0.15, 'Tovek turns it back into source.');
  add(T.gather0 + 0.2, T.scan0 - 0.05, 'Each byte goes to the line its instruction came from.');
  add(T.scan0 + 0.1, T.scan1 + 0.3, 'And comes out as plain Luau.');
  add(T.scan1 + 0.45, T.names - 0.1, 'This is the real output of Tovek V2.6.');
  // III
  add(T.names + 0.1, at[0] - 0.05, 'The bytecode keeps no names.');
  featured.forEach((fe, i) => {
    const end = i + 1 < featured.length ? at[i + 1] - 0.05 : T.rest0 - 0.05;
    add(at[i], end, `${fe.name}: ${fe.why}.`);
  });
  add(T.rest0, T.namesNum + 0.05, 'All the rest, the same way.');
  add(T.namesNum + 0.1, T.loops - 0.1, `${fmt(sc.locals_named)} locals named in four real games.`);
  // IV
  add(T.loops + 0.1, T.back0 - 0.05, 'Loops, not jumps.');
  add(T.back0, T.fwd0 - 0.05, 'In bytecode, a loop is only jumps.');
  add(T.fwd0, T.loopsNum - 0.1, 'Tovek turns each jump back into a loop, a continue, a break.');
  add(T.loopsNum + 0.2, T.sting - 0.1, `${fmt(sc.goto_statements)} goto statements in ${fmt(sc.scripts)} scripts.`);
  // V-VI
  add(T.sting + 0.3, T.split + 0.6, 'New in Tovek V2.6.');
  add(T.split + 0.6, T.evidence0 - 0.05, `This is Roblox's own ${hp.script}.`);
  add(T.evidence0, T.hpull - 0.05, 'Luau pasted a helper in here. Its line numbers still point at the helper.');
  add(T.hpull, T.fold1 - 0.3, `Tovek V2.6 finds all ${words(foldCount())} copies and turns them back into calls.`);
  add(T.fold1 - 0.25, T.helpersNum + 0.3, 'Each call is marked as an inferred equivalent.');
  add(T.helpersNum + 0.4, T.consts - 0.1, `${fmt(D.field.rebuilt_calls)} helper calls rebuilt in four real games.`);
  // VII
  add(T.consts + 0.1, T.bits0 - 0.05, 'Luau folded frames(1) into a number.');
  add(T.bits0, T.back - 0.05, 'Tovek folds 1 / 60 the way Luau does. All 64 bits match.');
  add(T.back, T.frac0 - 0.05, 'So the call comes back: frames(1).');
  add(T.frac0 + 0.2, T.checked - 0.15, `Exact fractions too: ${k.fraction.constant} is ${k.fraction.v26.split('= ')[1]}.`);
  // VIII
  add(T.checked + 0.35, T.grid0 - 0.05, 'Each output is compiled again and run beside its bytecode.');
  add(T.grid0, T.zero - 0.05, `${fmt(fz.programs)} fresh programs at ${fz.levels}. Patched bytecode is refused, not guessed.`);
  add(T.zero + 0.1, T.scale - 0.1, `${fmt(fz.counts.failed || 0)} wrong outputs.`);
  // IX
  add(T.scale + 0.05, T.sweep0 - 0.05, `Four real games: ${fmt(sc.scripts)} scripts.`);
  add(T.sweep0, T.total + 1.3, T.sweepRate > 1.001 ? `Shown ${T.sweepRate.toFixed(1)} times faster than it ran.` : `This is real time, on one ${sc.machine && sc.machine.kind ? sc.machine.kind : 'computer'}.`);
  add(T.total + 1.4, T.free - 0.1, `${fmt(sc.scripts)} scripts in ${sc.wall_seconds.toFixed(1)} seconds. ${sc.failed === 0 ? 'None failed.' : ''}`.trim());
  // X
  add(T.free + 0.1, T.os - 0.05, `Tovek is free and open source, under the ${D.license || 'MIT'} license.`);
  add(T.os, T.term0 - 0.05, `For ${list(osList())}.`);
  add(T.term0, browser ? T.browser - 0.05 : T.end - 0.2, 'A command-line tool, and a local server.');
  if (browser) add(T.browser, T.end - 0.2, 'And it runs in your browser. Nothing is uploaded.');
  // XI
  add(T.word, T.credit - 0.05, `Tovek V2.6. ${D.site}`);
  add(T.credit, T.duration - 0.4, 'Built on medal, by Jujhar Singh and Mathias Pedersen.');
  film.captions = c;
}

const osList = () => {
  const os = (S.D.platforms && S.D.platforms.os) || [];
  const order = ['Windows', 'macOS', 'Linux'];
  return os.length ? order.filter((o) => os.includes(o)) : order;
};
const foldCount = () => S.D.helper.function.rebuilt.split('\n').length - 2;
const WORDS = ['zero', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight', 'nine', 'ten', 'eleven', 'twelve'];
const words = (n) => WORDS[n] || String(n);
const list = (xs) => (xs.length < 2 ? xs.join('') : xs.slice(0, -1).join(', ') + ' and ' + xs[xs.length - 1]);

// ---------------------------------------------------------------------------------- the score
// Restrained: a low hum and room tone, ticks while bytes and code move, a soft struck tone where a
// shot lands, a quiet pulse on the bar through the features. One big moment: the swell that opens at
// New in V2.6 and peaks as the eleventh copy folds into its call. A full stop of silence before the
// proof, and a breath before the end card.

function buildScore() {
  const E = [];
  const D0 = T.duration;
  const pulse = (t, n, gain = 0.08, dur = 2.4) => E.push({ t, voice: 'pulse', freq: note(n), gain, dur });
  const sub = (t, gain = 0.1, f0 = 72, f1 = 36, dur = 1.4) => E.push({ t, voice: 'sub', freq: f0, to: f1, gain, dur });
  const chime = (t, n, gain = 0.035, dur = 2.6) => E.push({ t, voice: 'chime', freq: note(n), gain, dur });

  // room tone, broken by the silence before the proof
  E.push({ t: 0.3, voice: 'air', dur: T.checked - 0.3 + 0.05, gain: 0.024, cutoff: 820, release: 0.4 });
  E.push({ t: T.say8 - 0.1, voice: 'air', dur: T.end - T.say8 + 0.3, gain: 0.022, cutoff: 900, attack: 1.2, release: 0.6 });
  E.push({ t: T.mark0 - 0.2, voice: 'air', dur: D0 - T.mark0, gain: 0.02, cutoff: 760, attack: 1.4, release: 3 });

  // I. a hum out of the dark; the first bytes as faint ticks; a pulse under each line; the pull back
  E.push({ t: 0.4, voice: 'hum', dur: T.gather0 - 0.4, freq: note('A1'), gain: 0.1, attack: 3, release: 2 });
  E.push(...ticks(T.first + 0.1, T.first + 1.6, { rate: 12, seed: 2, gain: 0.01 }));
  pulse(T.line1, 'E3', 0.06, 3.2);
  pulse(T.line2, 'A3', 0.05, 3);
  E.push({ t: T.pull0 + 1.2, voice: 'swell', dur: T.pull1 - T.pull0, notes: ['A2', 'E3'].map(note), gain: 0.05, attack: 3.2, release: 1.6, open: 1300 });
  sub(T.pull1 - 0.1, 0.11, 78, 36, 1.6);
  // II. the line; the gather; the scan
  pulse(T.source + 0.05, 'C#4', 0.06, 2.6);
  E.push({ t: T.gather0, voice: 'swell', dur: T.scan1 - T.gather0 + 1.6, notes: ['A2', 'E3', 'A3'].map(note), gain: 0.085, attack: 2.4, release: 2.0, open: 2000 });
  E.push(...ticks(T.gather0 + 0.1, T.gather1, { rate: 34, seed: 4, gain: 0.009, jitter: 0.2 }));
  sub(T.scan0, 0.12, 70, 34, 1.8);
  E.push(...ticks(T.scan0 + 0.1, T.scan1 - 0.1, { rate: 26, seed: 5, gain: 0.015 }));
  chime(T.scan1 - 0.15, 'E5', 0.04, 3.2);
  E.push({ t: T.scan1, voice: 'hum', dur: T.sting - T.scan1, freq: note('A1'), gain: 0.055, attack: 2, release: 0.4, cutoff: 360 });

  // a quiet pulse on each bar through the features (not under the silences)
  for (let t = T.names; t < T.sting - 0.1; t += BAR) sub(t, 0.032, 60, 40, 0.7);
  for (let t = T.consts; t < T.checked - 0.1; t += BAR) sub(t, 0.028, 60, 40, 0.7);
  for (let t = T.free; t < T.end - 0.1; t += BAR) sub(t, 0.028, 60, 40, 0.7);

  // III. names drain with a falling run; each featured name lands with a bell
  pulse(T.names + 0.15, 'A3', 0.06);
  E.push(...ticks(T.drain0, T.drain1, { rate: 18, seed: 7, gain: 0.012 }));
  const featured = S.D.hero.featured_names.slice(0, 4);
  const resolveOf = (i) => [T.n1, T.n2, T.n3, T.n4][i] + (featured[i] && featured[i].evidence[0].line === featured[i].decl.line ? 0.8 : 1.1);
  ['E5', 'A5', 'C#6', 'E6'].forEach((n, i) => {
    const at = [T.n1, T.n2, T.n3, T.n4][i];
    E.push(...ticks(at + 0.45, at + 0.85, { rate: 20, seed: 20 + i, gain: 0.011 }));
    chime(resolveOf(i) + 0.05, n, 0.03, 2.2);
  });
  E.push(...ticks(T.rest0, T.rest1, { rate: 30, seed: 9, gain: 0.011 }));
  pulse(T.namesNum + 0.5, 'E3', 0.07, 2.6);
  // IV. loops: a low sweep back, each jump a soft tone as it is drawn, a bell as it lands in the loop
  pulse(T.loops + 0.2, 'A3', 0.055);
  sub(T.back0 + 0.1, 0.07, 64, 40, 1.0);
  ['C#4', 'E4', 'F#4', 'A4'].forEach((n, i) => pulse(T.arcs0 + i * 0.36 + 0.05, n, 0.032, 1.4));
  sub(T.fwd0 + 0.1, 0.07, 64, 40, 1.0);
  [0.42, 0.62, 0.82, 1.02].forEach((d, i) => chime(T.fwd0 + d + 0.7, ['A5', 'C#6', 'E6', 'A6'][i], 0.024, 2));
  pulse(T.loopsNum + 0.25, 'E3', 0.07, 2.6);

  // V-VI. THE moment: silence, a hit as V2.6 rises, then one swell that peaks on the last fold
  sub(T.sting + 0.35, 0.16, 90, 34, 1.8);
  pulse(T.sting + 0.5, 'A2', 0.1, 3.6);
  E.push({ t: T.sting + 0.6, voice: 'swell', dur: T.fold1 - T.sting + 3.6, notes: ['A2', 'E3', 'A3', 'C#4', 'E4'].map(note), gain: 0.4, attack: T.fold1 - T.sting - 0.75, release: 3.2, open: 3000 });
  sub(T.split, 0.08, 64, 40, 1.0);
  E.push(...ticks(T.evidence0 + 0.05, T.evidence0 + 0.9, { rate: 10, seed: 13, gain: 0.014 }));
  chime(T.evidence0 + 1.6, 'E5', 0.026, 2.4);
  // each fold: a tick, the last lands on the peak
  const n = foldCount();
  foldTimes(n).forEach((at, i) => E.push({ t: at + FOLD_DUR * 0.7, voice: 'tick', gain: 0.03, tone: 0.3 + 0.5 * (i / n) }));
  sub(T.fold1, 0.16, 80, 34, 2.0);
  chime(T.fold1 + 0.02, 'A5', 0.06, 3.4);
  chime(T.fold1 + 0.1, 'E6', 0.04, 3.0);
  pulse(T.helpersNum + 0.4, 'E3', 0.07, 2.6);
  // VII. a low bed; the bits land as ticks; a bell when all 64 match; another for 2 / 3
  E.push({ t: T.consts, voice: 'hum', dur: T.checked - T.consts - 0.15, freq: note('A1'), gain: 0.05, attack: 1.2, release: 0.9, cutoff: 380 });
  pulse(T.consts + 0.15, 'A3', 0.06);
  for (let i = 0; i < 64; i += 2) {
    const at = T.match0 + 0.2 + (i / 63) * (T.match1 - T.match0 - 0.45);
    E.push({ t: at, voice: 'tick', gain: 0.011, tone: 0.75 });
  }
  chime(T.match1 + 0.05, 'C#6', 0.045, 3);
  E.push(...ticks(T.back + 0.4, T.back + 1.4, { rate: 16, seed: 15, gain: 0.015 }));
  chime(T.frac0 + 1.4, 'E6', 0.034, 2.4);

  // VIII. silence; then the programs run as a stream of ticks under the head; a low hit on 0
  pulse(T.say8, 'A2', 0.07, 3.6);
  E.push({ t: T.say8, voice: 'hum', dur: T.scale - T.say8, freq: note('E1'), gain: 0.07, attack: 1.6, release: 1 });
  E.push(...ticks(T.grid0 + 0.15, T.grid1 - 0.3, { rate: 40, seed: 17, gain: 0.007, jitter: 0.1 }));
  sub(T.zero + 0.1, 0.15, 74, 34, 2.0);
  chime(T.zero + 0.2, 'B5', 0.045, 3.4);
  // IX. four games: the clock ticks every tenth of a second it shows; a tone as each game finishes
  pulse(T.scale + 0.1, 'A3', 0.06);
  E.push({ t: T.scale, voice: 'hum', dur: T.free - T.scale, freq: note('A1'), gain: 0.06, attack: 1.2, release: 0.5, cutoff: 480 });
  for (let s = 0; s < T.sweepDur; s += 0.1) E.push({ t: T.sweep0 + s, voice: 'tick', gain: 0.007, tone: 0.9 });
  let acc = 0;
  S.D.scale.games.forEach((g, i) => {
    acc += g.wall_seconds_median;
    pulse(T.sweep0 + acc / T.sweepRate, ['C#4', 'E4', 'F#4', 'A4'][i % 4], 0.045, 1.6);
  });
  chime(T.total + 0.6, 'A5', 0.04, 3);
  // X. a low bed; a struck tone on each word; keystrokes while the commands type
  E.push({ t: T.free - 0.4, voice: 'hum', dur: T.end - T.free + 0.2, freq: note('E2'), gain: 0.045, attack: 1.0, release: 0.8, cutoff: 420 });
  [0, 1, 2].forEach((i) => pulse(T.free + 0.05 + i * BEAT, ['A3', 'C#4', 'E4'][i], 0.06, 1.6));
  [0, 1, 2].forEach((i) => pulse(T.os + i * BEAT * 0.8, ['E4', 'F#4', 'A4'][i], 0.042, 1.4));
  E.push(...ticks(T.term0 + 0.35, T.term0 + 1.25, { rate: 16, seed: 19, gain: 0.02 }));
  E.push(...ticks(T.term0 + 2.2, T.term0 + 2.55, { rate: 16, seed: 21, gain: 0.02 }));
  if (S.D.browser && S.D.browser.available) chime(T.browser + 0.3, 'E5', 0.03, 2.4);
  // XI. a breath, the eight bits, then one chord under the name
  for (let i = 0; i < 8; i++) E.push({ t: T.mark0 + 0.45 + i * 0.1, voice: 'tick', gain: 0.02, tone: 0.4 + i * 0.05 });
  sub(T.mark0, 0.1, 70, 34, 1.8);
  E.push({ t: T.word - 0.2, voice: 'swell', dur: D0 - T.word + 0.2, notes: ['A2', 'E3', 'C#4', 'E4', 'A4'].map(note), gain: 0.12, attack: 1.6, release: 3.2, open: 2200 });
  chime(T.word + 0.05, 'A5', 0.05, 3.6);
  E.push({ t: T.word, voice: 'hum', dur: D0 - T.word, freq: note('A1'), gain: 0.08, attack: 2, release: 3 });
  return defineScore({ duration: D0, seed: 26, reverb: { seconds: 3.4, wet: 0.28 }, events: E });
}

export default film;
