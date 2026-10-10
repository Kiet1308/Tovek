// "One function, seventeen releases": the history film.
//
// One small script, compiled once to Luau bytecode, decompiled by medal and by every Tovek release.
// The code on screen is each release's real output; dates, publish times, headlines, notes and
// numbers come from the release notes. Everything is read from ../data/story.json (written by
// v26/data/story.py), so the film follows the data when the final V2.6 binary regenerates it.
//
// The cut, in five acts (films/story/*.js):
//   I    open.js    the dump, a dive into one byte, its opcode, its instruction, its line; medal
//   II   betas.js   beta 0.1 in the terminal; four days of betas clocked by their publish times;
//                   0.7's goto jumps as huge arcs, snapped into a loop by 0.8
//   III  pages.js   the release pages: V2 fills a white frame, V2.1 races, V2.5 counts 198 down
//   IV   climax.js  V2.6: paper floods out of one point; the pasted copies fly home; constants
//   V    end.js     beside the original, the numbers, the mark
// A frame is a pure function of t: each shot draws its own surface, later shots draw over earlier
// ones while they arrive (a page turn, a flood, tiles, a wipe), and nothing is carried between frames.

import { ease, defineScore, note, ticks, V26, formatNumber, codeMetrics, codePalettes, codeMorph } from '../engine/index.js';
import { T, ERA, TY, ERA_FONTS_CSS, CODE, S, makePalette, parseNumber, num, cap, lc, word, stripDot, longDate, hhmm } from './story/base.js';
import { prepareOpen, drawOpen, drawMedal, drawDate } from './story/open.js';
import { prepareBetas, bmeta, drawBeta01, drawMontage, drawBreath, drawJuly } from './story/betas.js';
import { preparePages, drawV2, drawV21, drawV25 } from './story/pages.js';
import { prepareClimax, hmeta, drawV26 } from './story/climax.js';
import { prepareEnd, smeta, drawSide, drawEnd } from './story/end.js';

// Each shot draws from t0 to t1. Where two overlap, the later one is arriving over the earlier.
const SHOTS = [
  { id: 'open', t0: 0, t1: T.open1, draw: drawOpen },
  { id: 'medal', t0: T.medal0, t1: T.medal1, draw: drawMedal },
  { id: 'date', t0: T.date0, t1: T.date1, draw: drawDate },
  { id: 'beta01', t0: T.b01, t1: T.b011, draw: drawBeta01 },
  { id: 'montage', t0: T.mont0, t1: T.mont1, draw: drawMontage },
  { id: 'breath', t0: T.breath0, t1: T.breath1, draw: drawBreath },
  { id: 'july', t0: T.r07, t1: T.july1, draw: drawJuly },
  { id: 'v2', t0: T.v2, t1: T.v21 + 1.2, draw: drawV2 },
  { id: 'v21', t0: T.v21, t1: T.v25 + 1.15, draw: drawV21 },
  { id: 'v25', t0: T.v25, t1: T.flood + 1.35, draw: drawV25 },
  { id: 'v26', t0: T.flood, t1: T.orig, draw: drawV26 },
  { id: 'side', t0: T.orig, t1: T.end + 1.5, draw: drawSide },
  { id: 'end', t0: T.end - 0.2, t1: T.duration + 1, draw: drawEnd },
];

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
// The first time a scene's type is drawn at the stage's pixel size, the GPU rasterises its glyphs:
// 20-70 ms for that one frame. In idle time, ahead of the playhead, the film draws its own frames on
// a hidden canvas of the same size, so the glyphs are ready before playback reaches them. This
// only fills caches; it never changes what a frame looks like.

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
  // playback starts at the top, whatever the poster shows: warm from there
  warm = { w: c.width, h: c.height, canvas, wctx, done: new Uint8Array(n), left: n, lastT: 0, finished: false, t0: performance.now() };
  const idle = globalThis.requestIdleCallback || ((cb) => setTimeout(() => cb({ timeRemaining: () => 8 }), 40));
  const W = warm;
  const step = (deadline) => {
    if (warm !== W) return;
    while (W.left > 0 && deadline.timeRemaining() > 5) {
      let i = Math.max(0, Math.floor(W.lastT / WARM_STEP));
      while (i < n && W.done[i]) i++;
      if (i >= n) { i = 0; while (i < n && W.done[i]) i++; }
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

// --------------------------------------------------------------------------------- the film

const story = {
  id: 'story',
  title: 'One function, seventeen releases',
  description: 'The same function, decompiled by medal and by every Tovek release.',
  duration: T.duration,
  poster: 5.3,
  background: V26.night,
  fonts: Object.values(TY).flatMap((set) => Object.values(set)).filter((x) => !/Inter|Hanken|Google/.test(x.family)).map((x) => x.css),
  glyphs: 'Tovek 0123456789 →·—–…×#$-_/:()[]',
  chapters: [{ t: 0, title: 'One script, as bytes' }],
  captions: [],
  score: undefined,

  async prepare() {
    if (S.D) return;
    await loadEraFonts();
    const D = await (await fetch(new URL('../data/story.json', import.meta.url))).json();
    S.D = D;
    S.ORDER = D.stages;
    S.ST = Object.fromEntries(S.ORDER.map((s) => [s.tag, s]));
    S.V26 = S.ORDER[S.ORDER.length - 1];
    S.ST.last = S.V26;
    S.RIDX = new Map(S.ORDER.slice(1).map((s, i) => [s, i + 1]));
    S.REPO = ((S.ORDER.find((s) => /github\.com\/[\w.-]+\/[\w.-]+\/releases/.test(s.tool || ''))?.tool || '').match(/github\.com\/[\w.-]+\/[\w.-]+/) || [''])[0];
    S.CM = codeMetrics(CODE.size, CODE.lineHeight);
    S.PAL = {
      dark: codePalettes.night,
      v2: makePalette(ERA.v2, 'rgba(198,241,53,0.85)'),
      v21: makePalette(ERA.v21, 'rgba(217,119,87,0.2)'),
      v25: makePalette(ERA.v25, 'rgba(157,210,255,0.2)'),
      v26: codePalettes.paper,
      end: codePalettes.night,
    };
    S.PAL.v2.accent = ERA.v2.ink;
    S.N = parseNumbers();

    const ledger = ledgerEvents();
    prepareOpen();
    prepareBetas(ledger);
    preparePages(ledger);
    prepareClimax();
    prepareEnd();
    buildChaptersCaptionsScore();
  },

  render: (ctx, t, w, h) => render(ctx, t, w, h),
  /** For tests: how far the idle warm-up has got. */
  warmState: () => (warm ? { left: warm.left, total: warm.done.length, finished: warm.finished, ms: warm.ms ?? performance.now() - warm.t0 } : null),
};

/** Numbers from the release notes (a widget is skipped if a note is worded differently). */
function parseNumbers() {
  const ST = S.ST, N = {};
  const numText = (g, i) => ST[g]?.numbers?.[i]?.text || '';
  let m = numText('v2-v0.1', 0).match(/^(.+?) ([\d,]+) to ([\d,]+)$/);
  if (m) N.v2 = { label: m[1], from: num(m[2]), to: num(m[3]) };
  m = numText('v2.1', 0).match(/([\d,]+)-script game in ([\d.]+) s on one thread \(V2: ([\d.]+) s\)/);
  if (m) N.v21 = { scripts: num(m[1]), v21: +m[2], v2: +m[3] };
  m = numText('v2.1.1', 0).match(/([\d,]+) damaged scripts: V2 aborted ([\d,]+) times/);
  if (m) N.v211 = { scripts: num(m[1]), aborted: num(m[2]) };
  m = numText('v2.5', 0).match(/(\d[\d,]*) wrong or missing \((.+?)\) to (\d[\d,]*)/);
  if (m) N.v25 = { from: num(m[1]), fromLabel: m[2], to: num(m[3]) };
  const callsA = numText('v2.5', 1).match(/^([\d,]+) /), callsB = parseNumber(numText('v2.5.1', 0));
  if (callsB) N.v251 = { ...callsB, from: callsA ? num(callsA[1]) : undefined };
  const nn = S.V26.numbers || [];
  N.v26a = nn[0] ? parseNumber(nn[0].text) : null;
  N.v26b = nn[1] ? parseNumber(nn[1].text) : null;
  return N;
}

/** When the ledger switches from one release's output to the next. */
function ledgerEvents() {
  const ST = S.ST;
  const ev = [{ t: 0, stage: ST.medal }, { t: T.b01Morph0, stage: ST['v0.1.0-beta'] }];
  const betas = ['v0.2.0-beta', 'v0.3.0-beta', 'v0.4.0-beta', 'v0.5.0-beta', 'v0.5.1-beta', 'v0.5.2-beta', 'v0.6.0-beta'];
  betas.forEach((g, i) => { if (ST[g]) ev.push({ t: T.slams0 + i * 0.5, stage: ST[g] }); });
  ev.push({ t: T.r07Morph0, stage: ST['v0.7.0'] }, { t: T.snap0, stage: ST['v0.8'] }, { t: T.r09, stage: ST['v0.9.0-beta'] });
  ev.push({ t: T.v2Morph0, stage: ST['v2-v0.1'] }, { t: T.v21, stage: ST['v2.1'] }, { t: T.v211, stage: ST['v2.1.1'] });
  ev.push({ t: T.v25, stage: ST['v2.5'] }, { t: T.v251, stage: ST['v2.5.1'] }, { t: T.homing0 + 6, stage: S.V26 });
  return ev.filter((x) => x.stage).sort((a, b) => a.t - b.t);
}

async function loadEraFonts() {
  if (typeof document === 'undefined' || !document.fonts) return;
  if (!document.querySelector('link[data-story-fonts]')) {
    const link = document.createElement('link');
    link.rel = 'stylesheet';
    link.href = ERA_FONTS_CSS;
    link.dataset.storyFonts = '';
    const loaded = new Promise((res) => { link.onload = res; link.onerror = res; });
    document.head.append(link);
    await Promise.race([loaded, new Promise((res) => setTimeout(res, 4000))]);
  }
  const specs = Object.values(TY).flatMap((set) => Object.values(set)).filter((x) => /Inter|Hanken|Google/.test(x.family)).map((x) => x.css);
  await Promise.all(specs.map((s) => document.fonts.load(s, story.glyphs).catch(() => null)));
  await document.fonts.ready;
}

// ------------------------------------------------------------ chapters, captions and the score

function buildChaptersCaptionsScore() {
  const ST = S.ST, D = S.D, N = S.N;
  const s = (g) => ST[g];
  const v26 = S.V26;
  const hl = (g) => stripDot(s(g).headline);
  const Bm = bmeta(), Hm = hmeta(), Sd = smeta();
  story.chapters = [
    { t: 0, title: 'One script, as bytes', still: 8.9 },
    { t: T.medal0, title: stripDot(s('medal').headline), still: 17.6 },
    { t: T.date0, title: cap(s('v0.1.0-beta').name), still: 33.4 },
    { t: T.mont0, title: stripDot(Bm.title), still: 38.4 },
    { t: T.r07, title: `${s('v0.8').name}: ${hl('v0.8')}`, still: 62.6 },
    { t: T.v2, title: `${s('v2-v0.1').name}: ${hl('v2-v0.1')}`, still: 83.0 },
    { t: T.v21, title: `${s('v2.1').name}: ${hl('v2.1')}`, still: 90.8 },
    { t: T.v25, title: `${s('v2.5').name}: ${hl('v2.5')}`, still: 101.6 },
    { t: T.flood, title: `${v26.name}: ${hl('last')}`, still: 120.9 },
    { t: T.orig, title: 'Beside the original', still: 149.0 },
    { t: T.end, title: `${cap(word(D.totals.releases))} releases`, still: 159.0 },
  ];

  // names a release brought, read from the morph: the new local names in its output
  const newNames = (a, b, limit = 3) => {
    const p = codeMorph(s(a).output, s(b).output); // cached by the engine
    const seen = [];
    for (const i of p.inserted) {
      const tk = p.b.tokens[i];
      if (tk.k === 'id' && /^[a-z]\w*[A-Z]?\w*$/.test(tk.text) && tk.text.length > 2 && !seen.includes(tk.text)) seen.push(tk.text);
    }
    return seen.slice(0, limit);
  };
  const list = (xs) => (xs.length < 2 ? xs.join('') : xs.slice(0, -1).join(', ') + ' and ' + xs[xs.length - 1]);
  const shieldLines = (g) => s(g).excerpts.shield.lines;
  const unchanged = Bm.term.filter((st, i) => i > 0 && !st.changed_from_previous).map((st) => st.name.replace(/^beta /, ''));
  const changed = Bm.term.filter((st, i) => i > 0 && st.changed_from_previous).map((st) => st.name.replace(/^beta /, ''));
  const helpers = Object.entries(D.sample.inlined_by_luau.helpers);
  const times = (n) => (n === 1 ? 'once' : n === 2 ? 'twice' : `${word(n)} times`);
  const fmt = formatNumber;
  const ins = D.sample.shield_bytecode.find((i) => i.op === 'NAMECALL');
  const c = [];
  const add = (start, end, text) => { if (text) c.push({ start: +start.toFixed(2), end: +end.toFixed(2), text }); };
  const gap = (a, b) => {
    const m = Math.round((Date.parse(b) - Date.parse(a)) / 60000);
    const h = Math.floor(m / 60), mm = m % 60;
    return `${h} ${h === 1 ? 'hour' : 'hours'}${mm ? ` ${mm} ${mm === 1 ? 'minute' : 'minutes'}` : ''}`;
  };

  // I. bytes
  add(1.4, 4.2, `A small Roblox script, written for this film and compiled by Luau ${(D.sample.compiler.match(/Luau ([\d.]+)/) || [])[1] || ''} with -O2.`.replace('Luau  with', 'Luau with'));
  add(4.4, 7.4, `The function we follow is ${D.sample.focus_function}.`);
  if (ins) {
    add(7.7, 9.6, `One byte of it is an opcode: ${ins.op}.`);
    add(9.8, 11.2, 'The bytes after it are its operands.');
    add(11.4, 13.4, `Line info in the bytecode says it came from line ${ins.line} of the source.`);
  }
  add(13.6, 16.2, "A decompiler starts from the bytes alone. This is medal's version of that line.");
  add(16.4, 19.4, `medal is a Luau decompiler written in Rust, ${lc(s('medal').points[1]).replace(/\.$/, '')}.`);
  add(19.6, 22.4, `A Luau decompiler by ${D.medal_history.authors.join(' and ')}, first committed on ${longDate(D.medal_history.first_commit.date)}.`);
  add(22.6, 25.3, `${lc(s('medal').points[2]).replace(/^its/, 'Its').replace(/^i/, 'I')}`.replace('raw: v1, p3, goto.', 'raw: generated names, and a goto into an else block.'));
  const b01 = s('v0.1.0-beta');
  if (b01.published_at) add(25.6, 28.8, `${cap(b01.name)} was published on ${longDate(b01.published_at)} at ${hhmm(b01.published_at)} UTC.`);
  // II. the terminal
  add(29.2, 32.6, `${cap(b01.name)} reads the same bytes. ${b01.points[0]}`);
  add(32.8, 35.4, `For now the loop still jumps: ${b01.features.goto} gotos.`);
  add(35.7, 36.9, `${Bm.title}`);
  const b02 = s('v0.2.0-beta');
  if (b02.published_at && b01.published_at) add(37.1, 39.9, `${cap(b02.name)} shipped ${gap(b01.published_at, b02.published_at)} after ${b01.name.replace(/^beta /, '')}.`);
  if (unchanged.length) add(40.1, 45.2, `${list(unchanged)} change nothing in this script; ${list(changed)} do.`);
  add(45.6, 47.7, 'They changed the second function, the one that runs for each new player.');
  const orName = newNames('v0.1.0-beta', 'v0.2.0-beta', 1)[0];
  const beat = (i) => T.breath0 + 2.4 + i * 2.2;
  if (orName) add(beat(0), beat(1) - 0.05, `${cap(b02.name)} names a value after what its or expression yields: ${orName}.`);
  add(beat(1), beat(2) - 0.05, `${cap(s('v0.4.0-beta').name)}: ${lc(s('v0.4.0-beta').points[2])}`);
  add(beat(2), T.breath1 - 0.1, `${cap(s('v0.5.0-beta').name)} keeps not (a < b): with NaN it is not the same as a >= b.`);
  add(T.r07 + 0.2, T.arcs0 - 0.8, `${s('v0.7.0').name} rebuilds the pipeline, and more locals get names: ${list(newNames('v0.6.0-beta', 'v0.7.0'))}.`);
  add(T.arcs0 - 0.6, T.nogoto - 0.1, 'The loop still jumps with goto. The arrows show where each jump lands.');
  add(T.nogoto, T.snap0 - 0.05, `${s('v0.8').name} shipped the same day.`);
  add(T.snap0, T.r08wide + 0.4, `${s('v0.8').name} removes every goto. It writes a small state machine instead.`);
  add(T.r08wide + 0.6, T.r09 - 0.1, `No goto, but the function grows from ${shieldLines('v0.7.0')} to ${shieldLines('v0.8')} lines.`);
  add(T.r09 + 0.1, T.july1 - 0.05, `${s('v0.9.0-beta').name}: ${lc(s('v0.9.0-beta').points[0])} ${s('v0.9.0-beta').points[1]}`);
  // III. pages
  add(T.v2 + 0.3, T.v2Morph1 - 0.4, `${s('v2-v0.1').name} writes the loop with continue and break: ${shieldLines('v0.9.0-beta')} lines become ${shieldLines('v2-v0.1')}.`);
  add(T.v2Morph1 - 0.2, T.v21 - 0.1, s('v2-v0.1').points[1]);
  if (N.v21) add(T.v21 + 0.4, T.v211 - 0.1, `${s('v2.1').name} decompiles a ${fmt(N.v21.scripts)}-script game in ${N.v21.v21} seconds on one thread. V2 took ${N.v21.v2}.`);
  if (N.v211) add(T.v211 + 0.2, T.v25 - 0.1, `Fed ${fmt(N.v211.scripts)} damaged scripts, V2 aborted ${fmt(N.v211.aborted)} times. ${s('v2.1.1').name} never does.`);
  if (N.v25) add(T.v25 + 0.3, T.ctx25 - 0.1, `${s('v2.5').name} is checked by semantic fuzzing: wrong or missing outputs go from ${N.v25.from} to ${N.v25.to}.`);
  const ctxName = newNames('v2.1.1', 'v2.5', 1)[0];
  if (ctxName) add(T.ctx25 + 0.1, T.v251 - 0.1, `Names come from context: the connection becomes ${ctxName}.`);
  if (N.v251) add(T.v251 + 0.2, T.flood - 0.05, `${s('v2.5.1').name}: ${fmt(N.v251.to)} ${N.v251.label}.`);
  // IV. V2.6
  if (helpers.length) {
    const [[h0, k0], ...rest] = helpers;
    add(T.flood + 0.4, T.homing0 + 1.9, `Luau's -O2 compiler pasted ${h0} into ${D.sample.focus_function} ${times(k0)}${rest.map(([h, k]) => `, and ${h} ${times(k)}`).join('')}.`);
  }
  add(T.homing0 + 2.0, Hm.glide0 - 0.05, `${v26.name} finds each pasted copy and sends it home to its helper.`);
  add(Hm.glide0, Hm.type1 + 0.2, 'In its place, a call, marked as an inferred equivalent.');
  add(Hm.type1 + 0.4, T.consts0 - 0.1, `The same file: ${s('v2.5.1').lines} lines become ${v26.lines}, with ${v26.features.rebuilt_calls} calls rebuilt.`);
  Hm.exprs.forEach((x, i) => add(x.t0 + 0.1, x.t0 + 3.0, i === 0 ? `Constants Luau folded are solved back: ${x.from} is ${x.to}.` : `And ${x.from} becomes ${x.to}.`));
  if (N.v26a) add(T.nums + 0.2, T.nums + 3.0, `Across four real Roblox games${N.v26a.paren ? `, ${N.v26a.paren}` : ''}, rebuilt calls go from ${fmt(N.v26a.from ?? 0)} to ${fmt(N.v26a.to)}.`);
  if (N.v26b) {
    const m = N.v26b.label.match(/^(wrong outputs) in (.+)$/);
    const was = N.v26b.paren.match(/^(.+?): ([\d,]+)$/);
    add(T.nums + 3.2, T.orig - 0.1, m ? `In ${m[2]}: ${N.v26b.to} ${m[1]}.${was ? ` ${was[1]} had ${was[2]}.` : ''}` : `${N.v26b.to} ${N.v26b.label}.`);
  }
  // V. the end
  add(T.orig + 0.4, T.orig + 3.8, 'Beside the source it was compiled from.');
  add(T.orig + 4.0, T.end - 0.1, Sd.onlyNames ? 'Every keyword, call and operator matches. Only names, constants and comments differ.' : 'Most of it matches, token for token.');
  add(T.end + 0.6, T.end + 4.2, `${cap(word(D.totals.releases))} releases in ${D.totals.days_since_first_beta} days.`);
  add(T.end + 4.6, T.duration - 0.4, `Built on ${D.credits.origin.replace(/ \(.*?\)/, '')}. ${D.credits.luau.replace(/ \(.*?\)/, '')}.`);
  story.captions = c;
  story.score = buildScore(Bm, Hm);
}


// ---------------------------------------------------------------------------------- the score
// A pulse for each release, keystrokes while code types, a racing clock under the betas, a hit on
// the white cut, one tick per fuzz failure going out, and one long swell for V2.6.

function buildScore(Bm, Hm) {
  const E = [];
  const D0 = T.duration;
  const pulse = (t, n, gain = 0.085, dur = 2.6) => E.push({ t, voice: 'pulse', freq: note(n), gain, dur });
  const sub = (t, gain = 0.12, f0 = 70, f1 = 35, dur = 1.5) => E.push({ t, voice: 'sub', freq: f0, to: f1, gain, dur });
  E.push({ t: 0, voice: 'air', dur: D0, gain: 0.026, cutoff: 760 });
  // I. the dump hums; the dive falls; each scale step is a soft tone
  E.push({ t: 0.2, voice: 'hum', dur: 25, freq: note('A1'), gain: 0.15, attack: 3, release: 4 });
  E.push(...ticks(T.hex0, T.hex0 + 3.6, { rate: 30, seed: 2, gain: 0.012 }));
  pulse(T.focus, 'E3', 0.06, 3);
  E.push({ t: T.dive0, voice: 'swell', dur: 3.2, notes: ['A2', 'E3'].map(note), gain: 0.05, attack: 2.4, release: 0.8, open: 1400 });
  sub(T.dive1 - 0.05, 0.13, 80, 38, 1.6);
  pulse(T.opcode + 0.6, 'A3', 0.06);
  E.push(...ticks(T.opcode + 0.25, T.opcode + 0.85, { rate: 26, seed: 4, gain: 0.014 }));
  pulse(T.instr + 0.1, 'C#4', 0.05);
  E.push(...ticks(T.instr + 0.35, T.instr + 1.25, { rate: 22, seed: 6, gain: 0.016 }));
  pulse(T.listing + 0.1, 'E4', 0.045);
  E.push(...ticks(T.line + 0.35, T.line + 0.9, { rate: 22, seed: 8, gain: 0.016 }));
  // medal
  pulse(T.medalTitle, 'A2', 0.12, 4);
  E.push(...ticks(T.lineMorph0 + 0.6, T.lineMorph1, { rate: 16, seed: 9, gain: 0.02 }));
  // the date rolls four years forward
  sub(T.date0, 0.1, 64, 34, 1.2);
  E.push(...ticks(T.date0 + 0.55, T.date0 + 2.2, { rate: 34, seed: 10, gain: 0.014, jitter: 0.1 }));
  pulse(T.date0 + 2.25, 'A3', 0.085);
  // II. beta 0.1, the montage, the breath
  E.push({ t: T.b01, voice: 'hum', dur: T.july1 - T.b01, freq: note('A1'), gain: 0.07, attack: 3, release: 3, cutoff: 360 });
  E.push(...ticks(T.b01Morph0 + 1.6, T.b01Morph1, { rate: 16, seed: 11, gain: 0.024 }));
  E.push({ t: T.mont0 + 0.08, voice: 'sub', freq: 90, to: 40, gain: 0.12, dur: 0.9 });
  E.push({ t: T.mont0 + 0.4, voice: 'sub', freq: 90, to: 40, gain: 0.12, dur: 0.9 });
  const scale = ['C#4', 'E4', 'B3', 'C#4', 'E4', 'F#4', 'E4'];
  Bm.slots.forEach((sl, i) => {
    // the clock races, then the version lands
    E.push(...ticks(sl.t0 + 0.02, sl.slam - 0.02, { rate: 60, seed: 20 + i, gain: 0.01, jitter: 0.05 }));
    sub(sl.slam, 0.11, 84, 40, 0.8);
    pulse(sl.slam, scale[i % scale.length], 0.07, 1.8);
    if (sl.changed) E.push(...ticks(sl.slam + 0.12, sl.slam + 0.62, { rate: 30, seed: 40 + i, gain: 0.016 }));
  });
  pulse(T.breath0 + 0.4, 'A3', 0.06, 4);
  [0, 1, 2].forEach((i) => E.push({ t: T.breath0 + 2.4 + i * 2.2, voice: 'chime', freq: note(['E5', 'A5', 'C#6'][i]), gain: 0.022, dur: 2.4 }));
  // July
  pulse(T.r07 + 0.2, 'A3');
  E.push(...ticks(T.r07Morph0 + 1.2, T.r07Morph1, { rate: 16, seed: 50, gain: 0.024 }));
  E.push({ t: T.arcs0 + 0.6, voice: 'hum', dur: T.snap0 - T.arcs0, freq: note('E2'), gain: 0.07, attack: 1.4, release: 0.6, cutoff: 600 });
  sub(T.nogoto, 0.15, 76, 34, 1.6);
  pulse(T.nogoto, 'E3', 0.1, 3);
  E.push({ t: T.snap0 + 0.6, voice: 'chime', freq: note('E5'), gain: 0.045, dur: 3 });
  pulse(T.r09 + 0.1, 'C#4', 0.06);
  // III. the white cut, the pages
  sub(T.v2, 0.18, 96, 36, 1.6);
  pulse(T.v2, 'A2', 0.1, 3.4);
  E.push(...ticks(T.v2Morph0 + 2.0, T.v2Morph1, { rate: 16, seed: 60, gain: 0.022 }));
  sub(T.v21 + 0.35, 0.1, 64, 34, 1.3);
  pulse(T.v21 + 0.5, 'C#4');
  E.push(...ticks(T.v21 + 2.0, T.v21 + 4.4, { rate: 40, seed: 61, gain: 0.008, jitter: 0.1 }));
  pulse(T.v211 + 0.2, 'E4');
  E.push({ t: T.v25, voice: 'hum', dur: T.flood - T.v25 + 1, freq: note('E1'), gain: 0.08, attack: 2, release: 2 });
  sub(T.v25 + 0.3, 0.11, 64, 34, 1.3);
  pulse(T.v25 + 0.7, 'F#3');
  // one tick for each fuzz failure as it goes out (the count eases, so do the ticks)
  if (S.N.v25) {
    const n = S.N.v25.from, start = T.fuzz0 + 0.3, dur = 3.2;
    for (let k = 1; k <= n; k += 2) {
      // the time the count passes n - k (inverse of the eased count), found by bisection
      let lo = 0, hi = 1;
      for (let it = 0; it < 18; it++) { const mid = (lo + hi) / 2; if (ease.inOut(mid) * n < k) lo = mid; else hi = mid; }
      E.push({ t: start + hi * dur, voice: 'tick', gain: 0.012, tone: 0.2 + 0.6 * (1 - k / n) });
    }
    E.push({ t: start + dur + 0.05, voice: 'chime', freq: note('B5'), gain: 0.045, dur: 3 });
  }
  E.push(...ticks(T.ctx25 + 0.9, T.ctx25 + 1.8, { rate: 16, seed: 70, gain: 0.022 }));
  pulse(T.v251 + 0.2, 'A3');
  // IV. V2.6: the swell carries the copies home
  E.push({ t: T.flood, voice: 'sub', freq: 60, to: 32, gain: 0.12, dur: 1.6 });
  E.push({ t: T.flood + 0.1, voice: 'swell', dur: Hm.type1 - T.flood + 3.2, notes: ['A2', 'E3', 'A3', 'C#4', 'E4'].map(note), gain: 0.32, attack: 6.5, release: 4.5, open: 2800 });
  pulse(T.title26 + 0.2, 'A3', 0.1);
  Hm.copies.forEach((cp, i) => {
    E.push({ t: cp.depart + 0.05, voice: 'pulse', freq: note(['E4', 'A4', 'C#5'][i % 3]), gain: 0.03, dur: 1.4 });
    E.push({ t: cp.arrive, voice: 'chime', freq: note(['A5', 'C#6', 'E6', 'A6'][i % 4]), gain: 0.05, dur: 2.6 });
  });
  E.push(...ticks(Hm.type0, Hm.type1, { rate: 16, seed: 31, gain: 0.024 }));
  Hm.exprs.forEach((x, i) => {
    E.push(...ticks(x.t0, x.t0 + 0.45, { rate: 22, seed: 80 + i, gain: 0.018 }));
    sub(x.t0 + 1.55, 0.08, 70, 40, 0.9);
    E.push({ t: x.t0 + 1.95, voice: 'chime', freq: note(i ? 'E6' : 'C#6'), gain: 0.035, dur: 2.4 });
  });
  pulse(T.nums + 0.2, 'E4', 0.07);
  E.push({ t: T.nums + 3.4, voice: 'chime', freq: note('A5'), gain: 0.04, dur: 3 });
  // V. the end
  pulse(T.orig + 0.4, 'C#4', 0.06, 3);
  sub(T.end - 0.1, 0.15, 70, 35, 1.8);
  E.push({ t: T.end + 0.4, voice: 'hum', dur: D0 - T.end - 0.4, freq: note('A1'), gain: 0.12, attack: 2, release: 3 });
  pulse(T.end + 0.6, 'A2', 0.1, 4);
  E.push({ t: T.end + 4.2, voice: 'swell', dur: D0 - T.end - 4.2, notes: ['A2', 'E3', 'C#4', 'E4'].map(note), gain: 0.09, attack: 1.6, release: 3 });
  E.push({ t: T.end + 4.3, voice: 'chime', freq: note('A5'), gain: 0.05, dur: 3.4 });
  return defineScore({ duration: D0, seed: 17, reverb: { seconds: 3.6, wet: 0.3 }, events: E });
}

export default story;
