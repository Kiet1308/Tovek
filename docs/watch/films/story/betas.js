// Act II. The terminal years. Beta 0.1 reads the same bytes; four days of betas go by as a montage
// clocked by their real publish times; a breath on the function they changed; then July: 0.7, the
// goto jumps drawn as huge arcs across the frame, and 0.8, the same day, snapping them into a loop.

import {
  ease, clamp, lerp, envelope, smoothstep, spring,
  layoutText, drawText, reveal, typewriter, measure, metrics, setFont,
  layoutCode, codeMorph, drawMorph, withCamera, drawCode, roundRect, rgba, mix, pixel, formatNumber,
} from '../../engine/index.js';
import {
  T, ERA, TY, TRACK, CODE, COL, COLW, BOX, CLIP, S, surface, hairline, vline, faded, clipRect, rollText, ticker,
  camAt, camFit, lerpCam, zoomCam, camTrack, drift, driftOffset, codeAt, edgeFade, washRuns, drawWashes,
  recovered, settle, washIn, flowOf, drawFlow, arrowHead, drawLedger, label, span, stamp, hhmm, weekday, shortMonth,
  word, cap, stripDot,
} from './base.js';

let B = null; // everything this act prepares

// ------------------------------------------------------------------------------------- prepare

export function prepareBetas(ledgerEvents) {
  const ST = S.ST;
  const L = (tag) => layoutCode(ST[tag].output);
  const shieldTop = (tag) => ST[tag].excerpts.shield.first_line - 1;
  const forLine = (tag) => L(tag).lineOf(/^\s*for _, /, shieldTop(tag));
  const plan = (a, b) => codeMorph(ST[a].output, ST[b].output);
  const forCam = (tag, box = BOX) => camAt(forLine(tag) - 2, 1, box);

  const term = ['v0.1.0-beta', 'v0.2.0-beta', 'v0.3.0-beta', 'v0.4.0-beta', 'v0.5.0-beta', 'v0.5.1-beta', 'v0.5.2-beta', 'v0.6.0-beta']
    .filter((g) => ST[g]).map((g) => ST[g]);
  const when = (st) => st.published_at || st.date + 'T12:00:00Z';
  const ms = (iso) => Date.parse(iso);

  // the montage: one slot per beta after 0.1; a beta that changed this file holds longer
  const slots = [];
  let at = T.slams0;
  term.slice(1).forEach((st) => {
    const dur = st.changed_from_previous ? 1.5 : 0.95;
    slots.push({ st, t0: at, t1: at + dur, slam: at + 0.3, changed: st.changed_from_previous });
    at += dur;
  });
  const scale = (T.mont1 - T.slams0) / (at - T.slams0); // fit the slots to the montage exactly
  slots.forEach((s) => { s.t0 = T.slams0 + (s.t0 - T.slams0) * scale; s.t1 = T.slams0 + (s.t1 - T.slams0) * scale; s.slam = s.t0 + 0.3; });

  // the days on the strip, UTC
  const day0 = Date.parse(when(term[0]).slice(0, 10) + 'T00:00:00Z');
  const day1 = Date.parse(when(term[term.length - 1]).slice(0, 10) + 'T00:00:00Z') + 86400000;
  const days = [];
  for (let d = day0; d < day1; d += 86400000) days.push(new Date(d).toISOString().slice(0, 10));
  const releaseDays = new Set(term.map((st) => when(st).slice(0, 10))).size;
  const title = `${cap(word(releaseDays))} days, ${word(term.length)} betas.`;

  // what each changed beta did to the second function, as lines of the last beta's output
  const pB02 = plan('v0.1.0-beta', 'v0.2.0-beta');
  const pB04 = plan('v0.3.0-beta', 'v0.4.0-beta');
  const pB05 = plan('v0.4.0-beta', 'v0.5.0-beta');
  const Lend = L('v0.6.0-beta');
  const joinTop = ST['v0.6.0-beta'].excerpts.join.first_line - 1, joinEnd = ST['v0.6.0-beta'].excerpts.join.last_line - 1;
  const changeOf = (st, p, focus) => {
    const rec = recovered(p, focus);
    const lines = [...new Set([...rec].map((i) => p.b.tokens[i].line))].sort((a, b) => a - b);
    // the first changed line inside the second function
    const jt = ST[st.tag].excerpts.join.first_line - 1, je = ST[st.tag].excerpts.join.last_line - 1;
    const line = lines.find((l) => l >= jt && l <= je) ?? lines[0];
    const text = p.b.lines[line] ?? '';
    const words = new Set([...rec].filter((i) => p.b.tokens[i].line === line).map((i) => p.b.tokens[i].text));
    // the same line in the last beta's output, and the tokens to light there
    const endLine = Lend.lines.findIndex((s, i) => i >= joinTop && i <= joinEnd && s === text);
    const lit = new Set();
    if (endLine >= 0) for (const tk of Lend.tokens) if (tk.line === endLine && words.has(tk.text)) lit.add(tk.i);
    return { text: text.trim(), words, endLine, lit, st };
  };
  const changes = new Map([
    ['v0.2.0-beta', changeOf(ST['v0.2.0-beta'], pB02, 'names')],
    ['v0.4.0-beta', changeOf(ST['v0.4.0-beta'], pB04, 'names')],
    ['v0.5.0-beta', changeOf(ST['v0.5.0-beta'], pB05, 'all')],
  ]);

  // July
  const p07 = plan('v0.6.0-beta', 'v0.7.0');
  const p08 = plan('v0.7.0', 'v0.8');
  const L07 = L('v0.7.0'), L08 = L('v0.8');
  const for08 = forLine('v0.8');
  const inner08 = flowOf(L08).loops.find((lp) => lp.inner && lp.from > for08);
  const after08 = inner08 ? inner08.to + 2 : L08.lineOf(/^\s*local \w+ = os\.clock\(\)/, for08) + 1;
  const cam08 = camFit(for08 - 1, after08, { x: 0, y: 150, w: 1920, h: 790 });
  cam08.x = 960 - (L08.colsIn(for08 - 1, after08) * S.CM.cw * cam08.s) / 2;
  const g07 = flowOf(L07).gotos;
  const for07 = forLine('v0.7.0');
  const lastGoto = g07.length ? Math.max(...g07.map((g) => Math.max(g.from, g.to))) : for07 + 20;
  // the arcs shot frames 0.7's loop at the left of the frame, leaving the right for the jumps
  const camArcs = camFit(for07 - 1, lastGoto + 2, { x: 150, y: 150, w: 700, h: 780 }, 1.05);
  const loop07 = [for07 - 1, lastGoto + 2], loop08 = [for08 - 1, after08];

  B = {
    term, slots, days, day0, day1, title, releaseDays, ms, when, changes, Lend, joinTop, joinEnd,
    pB01: plan('medal', 'v0.1.0-beta'), camMedalFor: forCam('medal'), camB01For: forCam('v0.1.0-beta'),
    camB06For: forCam('v0.6.0-beta'), cam07For: forCam('v0.7.0'),
    p07, p08, L07, L08, cam08, camArcs, inner08, g07, loop07, loop08,
    R: { b01: recovered(plan('medal', 'v0.1.0-beta'), 'names'), r07: recovered(p07, 'names') },
    ledger: ledgerEvents,
  };
}

export const bmeta = () => B;

// --------------------------------------------------------------------------------- the terminal

const idxOf = (st) => `${String(S.RIDX.get(st)).padStart(2, '0')} / ${S.D.totals.releases}`;
const verOf = (st) => st.cli?.version || '';
const whenOf = (st) => (st.published_at ? `${stamp(st.published_at)} UTC` : st.date);

/**
 * The terminal column: the binary's own version line, the release name, its publish time and its
 * headline. With `prev`, every line rolls from the previous release in place.
 */
function terminal(ctx, t, st, prev, t0, { headAt = t0 + 0.4, alpha = 1, dateFrom = null, big = false, roll = true } = {}) {
  if (alpha <= 0 || t < t0 - 0.1) return;
  const e = ERA.dark, ty = TY.dark;
  ctx.save();
  ctx.globalAlpha *= alpha;
  const P1 = layoutText('$ luau-lifter --version', ty.prompt);
  if (!roll) prev = null; // the terminal was not on screen just before: type it fresh
  if (prev) drawText(ctx, P1, COL, 150, { color: e.ink2 });
  else typewriter(ctx, P1, COL, 150, clamp((t - t0) * 38, 0, P1.glyphCount), { color: e.ink2 });
  const ix = (s) => COL + COLW - s.length * measure('0', ty.prompt);
  if (prev) {
    rollText(ctx, verOf(prev), verOf(st), COL, 184, ty.prompt, t, t0, { color: e.ink });
    rollText(ctx, idxOf(prev), idxOf(st), ix(idxOf(st)), 184, ty.prompt, t, t0, { color: e.ink2 });
  } else if (t > t0 + 0.55) {
    typewriter(ctx, layoutText(verOf(st), ty.prompt), COL, 184, clamp((t - t0 - 0.55) * 40, 0, verOf(st).length), { color: e.ink });
    typewriter(ctx, layoutText(idxOf(st), ty.prompt), ix(idxOf(st)), 184, clamp((t - t0 - 0.9) * 30, 0, 7), { color: e.ink2 });
  }
  if (prev) rollText(ctx, prev.name, st.name, COL - 4, 318, ty.title, t, t0, { color: e.ink, dur: 0.6, stagger: 0.05 });
  else reveal(ctx, layoutText(st.name, ty.title), COL - 4, 318, t, { unit: 'glyph', start: t0 + 0.2, stagger: 0.035, dur: 0.7, color: e.ink });
  // the publish time, rolled from the one before (or landed by the date shot)
  const d1 = whenOf(st);
  const d0 = dateFrom ?? (prev ? whenOf(prev) : d1);
  if (d0 === d1) drawText(ctx, layoutText(d1, ty.mdate), COL, 384, { color: e.ink });
  else ticker(ctx, d0.padEnd(d1.length), d1, COL, 384, ty.mdate, t, t0 + 0.05, 0.9, { color: e.ink, turns: 1 });
  if (big) {
    const Lb = layoutText(stripDot(st.headline).replace(', ', ',\n') + '.', ty.big, { lineHeight: 1.05 });
    reveal(ctx, Lb, COL - 4, 520, t, { unit: 'glyph', start: headAt, stagger: 0.045, dur: 0.6, color: e.ink });
  } else {
    const L = layoutText('# ' + st.headline, ty.head, { maxWidth: COLW, lineHeight: 1.32 });
    const n = clamp((t - headAt) * 62, 0, L.glyphCount);
    typewriter(ctx, L, COL, 452, n, { color: e.ink, caret: t < headAt + L.glyphCount / 62 + 0.8 && t > headAt, caretAlpha: Math.floor(t * 2.2) % 2 ? 0.8 : 0.25 });
  }
  ctx.restore();
}

// ---------------------------------------------------------------------------------- beta 0.1

export function drawBeta01(ctx, t) {
  const e = ERA.dark;
  surface(ctx, e.surface);
  const st = S.ST['v0.1.0-beta'];
  const pal = S.PAL.dark;
  // the code: medal's output morphs into beta 0.1's
  const a = ease.out(clamp((t - T.b01) / 0.7));
  faded(ctx, a, () => clipRect(ctx, CLIP, () => {
    const p = clamp((t - T.b01Morph0) / (T.b01Morph1 - T.b01Morph0));
    if (p <= 0) codeAt(ctx, B.pB01.a, B.camMedalFor, pal);
    else if (p < 1) drawMorph(ctx, B.pB01, p, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, cameraA: B.camMedalFor, cameraB: B.camB01For, blur: 5, highlight: B.R.b01 });
    else codeAt(ctx, B.pB01.b, B.camB01For, pal, { highlight: B.R.b01, mix: settle(T.b01Morph1, 1.4, 1.0)(t), washAlpha: washIn(T.b01Morph1, 1.2, 0.9)(t), washColor: rgba(pal.accent, 0.2) });
    const fa = p <= 0 ? envelope(t, T.b01, T.b01Morph0 + 0.4, 0.6, 0.4) : envelope(t, T.b01Morph1 - 0.01, T.b011 + 1, 0.6, 0.4);
    if (fa > 0) drawFlow(ctx, p <= 0 ? B.pB01.a : B.pB01.b, p <= 0 ? B.camMedalFor : B.camB01For, t, e.ink2, { gotoA: fa, loopA: fa });
    edgeFade(ctx, CLIP, e.surface);
  }));
  terminal(ctx, t, st, null, T.b01 - 0.05, { headAt: T.b01 + 1.6, dateFrom: null });
  drawLedger(ctx, t, B.ledger, 'dark', COL, 828, ease.out(clamp((t - T.b01 - 0.8) / 0.8)) * (1 - ease.inOut(clamp((t - T.b011 + 0.3) / 0.3))));
}

// --------------------------------------------------------------------------- four days, eight betas

const versionOf = (st) => st.name.replace(/^beta /, '');

function stripGeom() {
  return { x0: 150, x1: 1770, y: 152 };
}
const xOfTime = (ms) => { const g = stripGeom(); return lerp(g.x0, g.x1, (ms - B.day0) / (B.day1 - B.day0)); };

/** The four-and-a-bit days as a ruler, with a tick for each beta that has shipped by `t`. */
function drawStrip(ctx, t, { alpha = 1, head = null, landed = () => true, labels = true } = {}) {
  if (alpha <= 0) return;
  const e = ERA.dark, g = stripGeom();
  ctx.save();
  ctx.globalAlpha *= alpha;
  hairline(ctx, g.x0, g.y, g.x1 - g.x0, e.ink, 0.28);
  B.days.forEach((d, i) => {
    const x = xOfTime(Date.parse(d + 'T00:00:00Z'));
    vline(ctx, x, g.y - 10, 10, e.ink, 0.35);
    const xn = xOfTime(Date.parse(d + 'T00:00:00Z') + 43200000);
    label(ctx, `${weekday(d)} ${Number(d.slice(8, 10))}`, xn, g.y - 22, e.ink3, { align: 'center', tracking: 0.12 });
  });
  vline(ctx, g.x1, g.y - 10, 10, e.ink, 0.35);
  // release ticks
  B.term.forEach((st, i) => {
    const q = landed(st, i);
    if (q <= 0) return;
    const x = xOfTime(B.ms(B.when(st)));
    ctx.save();
    ctx.globalAlpha *= q;
    ctx.fillStyle = e.ink;
    ctx.fillRect(x - 1, g.y - 6 + (1 - q) * -10, 2, 18);
    if (labels) {
      // stagger labels that would collide (same-day releases close together)
      const row = i % 2;
      label(ctx, versionOf(st), x, g.y + 38 + row * 20, e.ink2, { align: 'center', tracking: 0.04 });
    }
    ctx.restore();
  });
  if (head != null) {
    const x = xOfTime(head);
    ctx.fillStyle = e.ink;
    ctx.fillRect(x - 1, g.y - 16, 2, 32);
  }
  ctx.restore();
}

/** Film time -> the real time the montage's clock shows. */
function clockAt(t) {
  const sl = B.slots;
  let prevMs = B.ms(B.when(B.term[0]));
  for (const s of sl) {
    const ms = B.ms(B.when(s.st));
    if (t < s.t0) return { ms: prevMs, slot: null };
    if (t < s.slam) return { ms: lerp(prevMs, ms, ease.inOut(clamp((t - s.t0) / (s.slam - s.t0)))), slot: s, racing: true };
    if (t < s.t1) return { ms, slot: s };
    prevMs = ms;
  }
  return { ms: prevMs, slot: sl[sl.length - 1] };
}

function clockText(ms) {
  const iso = new Date(Math.floor(ms / 60000) * 60000).toISOString();
  return `${weekday(iso)} ${iso.slice(8, 10)} ${shortMonth(iso)}  ${hhmm(iso)}`;
}

export function drawMontage(ctx, t) {
  const e = ERA.dark;
  surface(ctx, e.surface);
  // 1. the title, word by word, slammed
  if (t < T.slams0) {
    const L1 = B.title.split(' ');
    const lines = [L1.slice(0, 2).join(' '), L1.slice(2).join(' ')];
    const F = TY.dark.nogoto;
    lines.forEach((ln, li) => {
      const st = T.mont0 + 0.08 + li * 0.32;
      const q = clamp((t - st) / 0.22);
      if (q <= 0) return;
      const k = lerp(1.22, 1, ease.outExpo(q));
      const Lt = layoutText(ln, F);
      ctx.save();
      ctx.translate(960, 470 + li * 190);
      ctx.scale(k, k);
      drawText(ctx, Lt, 0, 0, { color: e.ink, align: 'center', alpha: Math.min(1, q * 3) * (1 - ease.inOut(clamp((t - (T.slams0 - 0.14)) / 0.14))), tracking: -0.01 });
      ctx.restore();
    });
    return;
  }
  const ck = clockAt(t);
  // 2. the strip and its racing playhead
  drawStrip(ctx, t, {
    alpha: ease.out(clamp((t - T.slams0) / 0.3)),
    head: ck.ms,
    landed: (st, i) => (i === 0 ? 1 : ease.out(clamp((t - (B.slots[i - 1]?.slam ?? Infinity)) / 0.25))),
  });
  // 3. the clock, racing between publish times
  const cf = TY.dark.clock;
  const ctext = clockText(ck.ms);
  drawText(ctx, layoutText('UTC', TY.dark.small), 1770, 262, { color: e.ink3, align: 'right', tracking: 0.12 });
  drawText(ctx, layoutText(ctext, cf), 1770 - 64, 262, { color: ck.racing ? e.ink2 : e.ink, align: 'right' });
  // 4. the version, slammed full frame; it holds until the next one lands on top of it
  let s = ck.slot;
  if (!s) return;
  if (t < s.slam) {
    const i = B.slots.indexOf(s);
    if (i <= 0) return;
    s = B.slots[i - 1];
  }
  const since = t - s.slam;
  const vf = TY.dark.slam;
  const v = versionOf(s.st);
  if (since >= 0) {
    const q = clamp(since / 0.16);
    const k = lerp(1.28, 1, ease.outExpo(q));
    const last = s === B.slots[B.slots.length - 1];
    const leave = last && T.mont1 - t < 0.08 ? (T.mont1 - t) / 0.08 : 1;
    ctx.save();
    ctx.translate(960, 712);
    ctx.scale(k, k);
    const Lv = layoutText(v, vf);
    drawText(ctx, Lv, 0, 0, { color: e.ink, align: 'center', alpha: Math.min(1, q * 2.5) * leave, tracking: -0.01 });
    ctx.restore();
    drawText(ctx, layoutText('beta', TY.dark.slamBeta), 960 - layoutText(v, vf).width / 2 + 8, 330, { color: e.ink2, alpha: Math.min(1, q * 2.5) * leave });
    // what this beta changed in the file, typed under the number
    const ch = B.changes.get(s.st.tag);
    if (ch && s.changed) {
      const CF = TY.dark.head;
      const Lc = layoutText(ch.text, CF);
      const x = 960 - Lc.width / 2;
      const typed = clamp((since - 0.12) / 0.5) * Lc.glyphCount;
      // the recovered words in the accent, the rest in base tones
      const cw = measure('0', CF);
      ctx.save();
      setFont(ctx, CF);
      ctx.textBaseline = 'alphabetic';
      ctx.textAlign = 'left';
      ctx.globalAlpha *= leave;
      const toks = layoutCode(ch.text).tokens;
      for (const tk of toks) {
        const vis = typed - tk.col;
        if (vis <= 0) continue;
        const txt = tk.text.slice(0, Math.ceil(vis));
        ctx.fillStyle = ch.words.has(tk.text) ? e.accent : S.PAL.dark[tk.k] || e.ink2;
        ctx.fillText(txt, x + tk.col * cw, 812);
      }
      ctx.restore();
    } else if (!s.changed) {
      label(ctx, 'NO CHANGE IN THIS SCRIPT', 960, 812, e.ink3, { align: 'center', alpha: ease.out(clamp((since - 0.1) / 0.25)) * leave, tracking: 0.14 });
    }
  }
}

// ------------------------------------------------------------------------------------- the breath

function breathCam() {
  const top = B.joinTop, end = B.joinEnd;
  const { cw, lh } = S.CM;
  const s = Math.min(1.0, 700 / ((end - top + 1) * lh));
  const cols = B.Lend.colsIn(top, end);
  return { s, x: 960 - (cols * cw * s) / 2, y: 640 - ((top + end + 1) / 2) * lh * s };
}

export function drawBreath(ctx, t) {
  const e = ERA.dark;
  surface(ctx, e.surface);
  // the last slam settles into its tick; the strip stays as a ruler of the four days
  drawStrip(ctx, t, { alpha: 1 - ease.inOut(clamp((t - (T.breath1 - 0.6)) / 0.6)), landed: () => 1 });
  const lastV = versionOf(B.term[B.term.length - 1]);
  const shrink = ease.inOut(clamp((t - T.breath0) / 0.6));
  if (shrink < 1) {
    const x = lerp(960, xOfTime(B.ms(B.when(B.term[B.term.length - 1]))), shrink);
    const y = lerp(712, 152 + 58, shrink);
    const k = lerp(1, 0.04, shrink);
    ctx.save();
    ctx.translate(x, y);
    ctx.scale(k, k);
    drawText(ctx, layoutText(lastV, TY.dark.slam), 0, 0, { color: e.ink, align: 'center', alpha: 1 - shrink * 0.6, tracking: -0.01 });
    ctx.restore();
  }
  // the second function, as a landscape, and the three changes lit in turn
  const cam = drift(breathCam(), t, T.breath0, T.breath1 + 1, { zoom: 0.05, dx: -26, dy: -10 });
  const pal = S.PAL.dark;
  const ca = ease.out(clamp((t - T.breath0 - 0.4) / 0.9)) * (1 - ease.inOut(clamp((t - (T.breath1 - 0.5)) / 0.5)));
  const order = ['v0.2.0-beta', 'v0.4.0-beta', 'v0.5.0-beta'];
  const beat = (i) => T.breath0 + 2.4 + i * 2.2;
  let lit = null, litK = 0, litTag = null;
  order.forEach((g, i) => {
    const k = envelope(t, beat(i), beat(i) + 2.2, 0.3, 0.35);
    if (k > litK) { litK = k; lit = B.changes.get(g); litTag = g; }
  });
  faded(ctx, ca, () => {
    const focusLine = lit && litK > 0 ? lit.endLine : -1;
    codeAt(ctx, B.Lend, cam, pal, {
      clip: { x: 0, y: 230, w: 1920, h: 850 },
      highlight: lit && lit.lit.size ? lit.lit : null, mix: litK, washAlpha: litK, washColor: rgba(pal.accent, 0.2),
      lineAlpha: (l) => (l < B.joinTop || l > B.joinEnd ? 0 : focusLine < 0 ? 0.9 : l === focusLine ? 1 : lerp(0.9, 0.4, litK)),
    });
    // the release that made the change, at the end of its line
    if (lit && litK > 0 && lit.endLine >= 0) {
      const { cw, lh } = S.CM;
      const xEnd = cam.x + (B.Lend.lineCols[lit.endLine] + 2) * cw * cam.s;
      const y = cam.y + (lit.endLine + 0.5) * lh * cam.s;
      ctx.save();
      ctx.globalAlpha *= litK;
      hairline(ctx, xEnd, y, 46, e.ink2, 0.7);
      label(ctx, S.ST[litTag].name.toUpperCase(), xEnd + 58, y + 6, e.ink, { tracking: 0.1 });
      ctx.restore();
    }
  });
}

// ----------------------------------------------------------------------------------------- July

export function drawJuly(ctx, t) {
  const e = ERA.dark;
  surface(ctx, e.surface);
  const pal = S.PAL.dark;
  const s06 = S.ST['v0.6.0-beta'], s07 = S.ST['v0.7.0'], s08 = S.ST['v0.8'], s09 = S.ST['v0.9.0-beta'];
  const ty = TY.dark;

  // ---- 0.7 in the terminal: the code pans from the second function to the loop, then morphs
  if (t < T.arcs0 + 1.2) {
    const out = ease.inOut(clamp((t - T.arcs0) / 0.9));
    terminal(ctx, t, s07, s06, T.r07 + 0.1, { headAt: T.r07 + 1.4, alpha: 1 - out, dateFrom: whenOf(s06), roll: false });
    drawLedger(ctx, t, B.ledger, 'dark', COL, 828, ease.out(clamp((t - T.r07 - 0.4) / 0.6)) * (1 - out));
  }
  // the camera: the 0.6 loop, the morph to 0.7, then the arcs framing at the left of the frame
  const camIn = lerpCam(breathCodeCam(), B.camB06For, ease.inOut(clamp((t - T.r07) / 0.9)));
  const pm = clamp((t - T.r07Morph0) / (T.r07Morph1 - T.r07Morph0));
  const toArcs = ease.inOut(clamp((t - T.arcs0) / 1.1));
  const camHold = zoomCam(B.cam07For, B.camArcs, toArcs);

  // ---- 0.8: the code morphs while the arcs snap into its loop
  const ps = clamp((t - T.snap0) / (T.snap1 - T.snap0));
  const cam08 = drift(B.cam08, t, T.snap1, T.july1, { zoom: 0.045, dx: -22, dy: -8 });
  const nogotoDim = envelope(t, T.nogoto - 0.1, T.snap0 + 0.6, 0.3, 0.6);
  const codeDim = 1 - 0.6 * nogotoDim - 0.62 * envelope(t, T.r09 - 0.2, T.july1 + 1, 0.5, 0.1);
  const clip = { x: 0, y: 40, w: 1920, h: 1000 };
  faded(ctx, codeDim, () => {
    if (t < T.r07Morph0) codeAt(ctx, B.p07.a, camIn, pal, { clip, lineAlpha: (l) => (l >= B.joinTop && l <= B.joinEnd ? 1 : ease.out(clamp((t - T.r07) / 0.8))) });
    else if (pm < 1) drawMorph(ctx, B.p07, pm, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, cameraA: B.camB06For, cameraB: B.cam07For, blur: 5, highlight: B.R.r07 });
    else if (t < T.snap0) codeAt(ctx, B.L07, camHold, pal, { clip, highlight: B.R.r07, mix: settle(T.r07Morph1, 1.0, 0.8)(t), washAlpha: washIn(T.r07Morph1, 1.0, 0.8)(t), washColor: rgba(pal.accent, 0.2), lineAlpha: (l) => (l >= B.loop07[0] && l <= B.loop07[1] ? 1 : 1 - 0.85 * toArcs) });
    else if (ps < 1) drawMorph(ctx, B.p08, ps, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, cameraA: B.camArcs, cameraB: B.cam08, blur: 5, inserted: 'base' });
    else codeAt(ctx, B.L08, cam08, pal, { clip, lineAlpha: (l) => (l >= B.loop08[0] && l <= B.loop08[1] ? 1 : 0.25) });
    // the loop arrows of 0.8 settle in after the snap
    if (ps >= 1) drawFlow(ctx, B.L08, cam08, t, e.ink2, { gotoA: 0, loopA: ease.out(clamp((t - T.snap1) / 0.4)) * 0.85, innerA: 0 });
  });
  // the goto arcs: huge across the frame, then snapping into 0.8's loop
  drawArcs(ctx, t, camHold, cam08);

  // ---- "No goto, ever." full frame
  if (t > T.nogoto - 0.1 && t < T.snap0 + 1.2) {
    const head = stripDot(s08.headline).replace(', ', ',\n') + '.';
    const L = layoutText(head, ty.nogoto, { lineHeight: 1.02 });
    reveal(ctx, L, 960, 470, t, { unit: 'glyph', start: T.nogoto, stagger: 0.035, dur: 0.5, color: e.ink, align: 'center', tracking: -0.01, out: { start: T.snap0 - 0.1, stagger: 0.012, dur: 0.45 } });
    const tag = `${s08.name}  ·  ${whenOf(s08)}  ·  ${s07.name} WAS ${hhmm(s07.published_at || '')} THE SAME DAY`;
    label(ctx, tag.toUpperCase(), 960, 760, e.ink2, { align: 'center', alpha: envelope(t, T.nogoto + 0.5, T.snap0 + 0.2, 0.4, 0.3), tracking: 0.12 });
  }
  // ---- 0.8's corner label and the ledger, over the wide shot
  const wa = envelope(t, T.snap1 - 0.2, T.r09 + 0.2, 0.6, 0.4);
  if (wa > 0) {
    ctx.save();
    ctx.globalAlpha *= wa;
    drawText(ctx, layoutText(`${s08.name}  ·  ${s08.headline}`, ty.head), 96, 92, { color: e.ink });
    drawText(ctx, layoutText(whenOf(s08), ty.small), 96, 124, { color: e.ink2, tracking: 0.04 });
    ctx.restore();
  }
  drawLedger(ctx, t, B.ledger, 'dark', 96, 828, envelope(t, T.snap1 - 0.3, T.r09 + 0.3, 0.6, 0.4), { width: 360 });

  // ---- 0.9 beta, quietly, over the dimmed code
  if (t > T.r09 - 0.1) {
    const q = T.r09;
    reveal(ctx, layoutText(s09.name, TY.dark.nogoto), 960, 500, t, { unit: 'glyph', start: q, stagger: 0.03, dur: 0.6, color: e.ink, align: 'center', tracking: -0.01 });
    reveal(ctx, layoutText(s09.headline, TY.dark.line), 960, 590, t, { unit: 'word', start: q + 0.5, stagger: 0.08, dur: 0.7, color: e.ink2, align: 'center' });
    label(ctx, whenOf(s09).toUpperCase(), 960, 330, e.ink3, { align: 'center', alpha: ease.out(clamp((t - q - 0.3) / 0.5)), tracking: 0.12 });
  }
}

/** Where the breath left the code (the second function), so 0.7 can pan from it. */
function breathCodeCam() {
  const c = drift(breathCam(), T.breath1, T.breath0, T.breath1 + 1, { zoom: 0.05, dx: -26, dy: -10 });
  return c;
}

/** Cubic Bezier points for one goto, drawn as a jump that swings far out across the frame. */
function arcPose(g, cam, i, n) {
  const { cw, lh } = S.CM;
  const y = (l) => cam.y + (l + 0.5) * lh * cam.s;
  const x = (c) => cam.x + c * cw * cam.s;
  const p0 = [x(g.endCol) + 14, y(g.from)];
  const p3 = [x(g.labelEnd) + 14, y(g.to)];
  // control points far off the frame: each jump swings up to the top edge, across, and back
  const dir = g.to >= g.from ? 1 : -1;
  return [p0, [p0[0] + 700 + 330 * i, p0[1] - (720 + 90 * i) * dir], [2150 + 240 * i, p3[1] + (380 + 110 * i) * dir], p3];
}

/** The loop bracket of 0.8's dispatcher, in the same form, for the arcs to snap into. */
function loopPose(cam, i) {
  const lp = B.inner08;
  const { cw, lh } = S.CM;
  if (!lp) return null;
  const y = (l) => cam.y + (l + 0.5) * lh * cam.s;
  const x0 = cam.x + lp.c * cw * cam.s - 12 - i * 9;
  const bend = 26 + i * 8;
  return [[x0, y(lp.to)], [x0 - bend, y(lp.to)], [x0 - bend, y(lp.from)], [x0, y(lp.from)]];
}

function drawArcs(ctx, t, camHold, cam08) {
  const gs = B.g07;
  if (!gs.length || t < T.arcs0 + 0.4 || t > T.snap1 + 0.6) return;
  const e = ERA.dark;
  const n = gs.length;
  ctx.save();
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  ctx.strokeStyle = e.ink;
  gs.forEach((g, i) => {
    const grow = ease.inOut(clamp((t - T.arcs0 - 0.6 - i * 0.45) / 1.2));
    if (grow <= 0) return;
    const A = arcPose(g, camHold, i, n);
    const Bp = loopPose(cam08, i) || A;
    const sp = t < T.snap0 ? 0 : spring(t - T.snap0 - i * 0.12, { freq: 1.25, damping: 0.62 });
    const P = A.map((pt, k) => [lerp(pt[0], Bp[k][0], sp), lerp(pt[1], Bp[k][1], sp)]);
    const dim = 1 - 0.55 * envelope(t, T.nogoto - 0.1, T.snap0 + 0.1, 0.3, 0.2);
    const fadeOut = 1 - ease.inOut(clamp((t - T.snap1 - 0.1) / 0.5));
    const lw = lerp(3, 1.8, sp);
    ctx.globalAlpha = dim * fadeOut * lerp(0.95, 0.85, sp);
    ctx.lineWidth = lw;
    // drawn on along its length while it grows, then flowing toward the label; solid once it is a loop
    const len = 6000;
    const dash = lerp(lw * 3, 0, clamp(sp * 1.4));
    if (grow < 1) { ctx.setLineDash([len * grow, len]); ctx.lineDashOffset = 0; }
    else if (dash > 0.2) { ctx.setLineDash([dash, dash]); ctx.lineDashOffset = -t * 36; }
    else ctx.setLineDash([]);
    ctx.beginPath();
    ctx.moveTo(P[0][0], P[0][1]);
    ctx.bezierCurveTo(P[1][0], P[1][1], P[2][0], P[2][1], P[3][0], P[3][1]);
    ctx.stroke();
    ctx.setLineDash([]);
    if (grow >= 1) {
      // the arrowhead points along the curve's last tangent
      const dx = P[3][0] - P[2][0], dy = P[3][1] - P[2][1];
      const ang = Math.atan2(dy, dx), s = 10 + lw;
      ctx.beginPath();
      ctx.moveTo(P[3][0] - s * Math.cos(ang - 0.5), P[3][1] - s * Math.sin(ang - 0.5));
      ctx.lineTo(P[3][0], P[3][1]);
      ctx.lineTo(P[3][0] - s * Math.cos(ang + 0.5), P[3][1] - s * Math.sin(ang + 0.5));
      ctx.stroke();
    }
  });
  ctx.restore();
  // the goto keywords themselves glow while their jumps are drawn
}
