// Engine reel: an 18-second film that runs every engine helper once. It is not linked from the
// player UI (open watch/?film=demo). Its numbers and code come from demo.data.json, written by
// v26/export/gen_demo_data.py from the V2.5.1 and V2.6 binaries.

import {
  ease, progress, clamp, lerp, timeline, fade,
  font, layoutText, reveal, decode, drawText,
  drawOdometer, drawCounter, formatNumber,
  drawCode, codeMorph, drawMorph, fitCamera, withCamera,
  defineScore, note, ticks, drawMark, markWidth, V26, rgba, pixel,
} from '../engine/index.js';

const F = {
  title: font(300, { family: 'display', weight: 650, stretch: 87.5 }),
  big: font(250, { family: 'display', weight: 600, stretch: 87.5 }),
  side: font(210, { family: 'display', weight: 600, stretch: 87.5 }),
  card: font(124, { family: 'display', weight: 650, stretch: 87.5 }),
  unit: font(40, { family: 'text', weight: 500 }),
  kicker: font(21, { family: 'mono', weight: 500 }),
  mono: font(24, { family: 'mono', weight: 450 }),
  monoSmall: font(20, { family: 'mono', weight: 450 }),
  versions: font(34, { family: 'mono', weight: 500 }),
};

const CODE = { size: 26, lineHeight: 1.55 };
const BOX = { x: 150, y: 140, w: 1010, h: 800 };
const SIDE = 1290; // left edge of the data column in the morph scene

// timings (seconds)
const T = {
  titleOut: 3.5,
  countIn: 3.9,
  morphIn: 7.4,
  morphStart: 9.0,
  morphEnd: 13.8,
  endIn: 15.6,
  duration: 18.6,
};

let D = null; // data, set in prepare()
let plan = null;
let views = null;

const kicker = (ctx, text, x, y, alpha = 1, align = 'left') =>
  drawText(ctx, layoutText(text.toUpperCase(), F.kicker), x, y, { color: V26.onNight2, tracking: 0.12, alpha, align });

function hairline(ctx, x0, x1, y, p, alpha = 0.22) {
  const px = pixel(ctx);
  ctx.fillStyle = rgba(V26.onNight, alpha);
  const mid = (x0 + x1) / 2, half = ((x1 - x0) / 2) * p;
  ctx.fillRect(mid - half, y, half * 2, Math.max(1, px));
}

function drawTitle(ctx, s) {
  const t = s.t;
  const L = layoutText('Tovek', F.title);
  reveal(ctx, L, 960, 600, t, {
    unit: 'glyph', start: 0.25, stagger: 0.055, dur: 1.15, ease: ease.out,
    tracking: -0.028, trackingFrom: 0.05, align: 'center', color: V26.onNight,
    out: { start: T.titleOut, stagger: 0.035, dur: 0.7, ease: ease.inOut },
  });
  const away = 1 - progress(t, T.titleOut + 0.1, 0.5);
  fade(ctx, away, () => {
    hairline(ctx, 560, 1360, 668, progress(t, 0.9, 1.3, ease.inOut));
    const v = layoutText(`${D.binaries.a.label}  →  ${D.binaries.b.label}`, F.versions);
    decode(ctx, v, 960, 740, t, { start: 1.25, dur: 0.9, stagger: 0.03, seed: 26, align: 'center', color: V26.onNight2, settledColor: V26.onNight2 });
  });
}

function drawCount(ctx, s) {
  const t = s.t, t0 = T.countIn;
  const out = progress(t, T.morphIn - 0.55, 0.65, ease.inOut);
  fade(ctx, 1 - out, () => {
    fade(ctx, progress(t, t0 + 0.1, 0.6), () => kicker(ctx, `${D.set.scripts} open-source modules · decompiled twice`, 150, 300));
    const odo = drawOdometer(ctx, t, {
      from: D.set.linesA, to: D.set.linesB, start: t0 + 0.6, dur: 2.3,
      x: 150, y: 560, font: F.big, color: V26.onNight, turns: 2,
    });
    fade(ctx, progress(t, t0 + 0.9, 0.6), () => {
      drawText(ctx, layoutText('lines', F.unit), 150 + odo.width + 26, 560, { color: V26.onNight2 });
    });

    // V2.5.1's length as a quiet bar, V2.6's shrinking onto it in step with the count
    const x0 = 150, barW = 1620, px = pixel(ctx);
    const grow = progress(t, t0 + 0.6, 2.3, ease.inOut);
    const rows = [
      { label: D.binaries.a.label, value: D.set.linesA, w: barW, a: 0.3, p: progress(t, t0 + 0.3, 1.0, ease.out) },
      { label: D.binaries.b.label, value: D.set.linesB, w: lerp(barW, (barW * D.set.linesB) / D.set.linesA, grow), a: 0.95, p: progress(t, t0 + 0.45, 1.0, ease.out) },
    ];
    rows.forEach((r, i) => {
      const y = 690 + i * 86;
      fade(ctx, r.p, () => {
        drawText(ctx, layoutText(r.label, F.monoSmall), x0, y - 18, { color: V26.onNight2 });
        drawCounter(ctx, r.value, x0 + barW, y - 18, F.monoSmall, { align: 'right', color: V26.onNight2 });
        ctx.fillStyle = rgba(V26.onNight, 0.1);
        ctx.fillRect(x0, y, barW, Math.max(1, px));
        ctx.fillStyle = rgba(V26.onNight, r.a);
        ctx.fillRect(x0, y - 2, r.w * r.p, 5);
      });
    });
  });
}

function drawMorphScene(ctx, s) {
  const t = s.t;
  const p = progress(t, T.morphStart, T.morphEnd - T.morphStart);
  const { size, lineHeight } = CODE;
  const leave = 1 - progress(t, T.endIn - 0.35, 0.7, ease.inOut);

  // the code: an establishing shot of the whole excerpt, then the camera pushes in while it morphs
  const enter = (l) => clamp((t - T.morphIn - 0.15) * 2.4 - l * 0.016);
  fade(ctx, leave, () => {
    if (t < T.morphStart) {
      const cam = fitCamera(plan.a, BOX, { size, lineHeight, lines: views.a, align: 'top' });
      clip(ctx, () => withCamera(ctx, cam, () => {
        drawCode(ctx, plan.a, { size, lineHeight, palette: 'night', lineAlpha: (l) => ease.out(enter(l)) });
      }));
    } else if (t < T.morphEnd) {
      drawMorph(ctx, plan, p, { box: BOX, clip: true, size, lineHeight, palette: 'night', linesA: views.a, linesB: views.b, align: 'top' });
    } else {
      const cam = fitCamera(plan.b, BOX, { size, lineHeight, lines: views.b, align: 'top' });
      clip(ctx, () => withCamera(ctx, cam, () => {
        drawCode(ctx, plan.b, { size, lineHeight, palette: 'night', highlight: plan.inserted });
      }));
    }
  });

  // the data column
  const head = progress(t, T.morphIn + 0.2, 0.9, ease.out);
  fade(ctx, head * leave, () => {
    kicker(ctx, `${D.sample.project} · ${D.sample.file}`, SIDE, 178);

    // the release line
    const y = 252, xa = SIDE, xb = 1770, px = pixel(ctx);
    const la = layoutText(D.binaries.a.label, F.versions), lb = layoutText(D.binaries.b.label, F.versions);
    drawText(ctx, la, xa, y, { color: p < 0.5 ? V26.onNight : V26.onNight2 });
    drawText(ctx, lb, xb, y, { color: p >= 0.5 ? V26.onNight : V26.onNight2, align: 'right' });
    const x0 = xa + la.width + 22, x1 = xb - lb.width - 22;
    ctx.fillStyle = rgba(V26.onNight, 0.16);
    ctx.fillRect(x0, y - 11, x1 - x0, Math.max(1, px * 1.5));
    ctx.fillStyle = V26.onNight;
    ctx.fillRect(x0, y - 11, (x1 - x0) * ease.inOut(p), Math.max(1, px * 1.5));

    // lines in the excerpt
    drawOdometer(ctx, t, { from: D.excerpt.linesA, to: D.excerpt.linesB, start: T.morphStart + 0.5, dur: (T.morphEnd - T.morphStart) * 0.8, x: SIDE - 8, y: 520, font: F.side, color: V26.onNight, turns: 1 });
    drawText(ctx, layoutText('lines', F.mono), SIDE, 598, { color: V26.onNight2 });

    // the token diff behind the morph, counted as it happens
    const st = plan.stats;
    const rows = [
      { label: 'tokens kept', value: st.kept, p: progress(t, T.morphStart + 0.6, 2.6, ease.out) },
      { label: 'tokens gone', value: st.removed, p: progress(t, T.morphStart, 2.0, ease.out) },
      { label: 'tokens new', value: st.inserted, p: progress(t, T.morphStart + 2.4, 2.4, ease.out), accent: true },
    ];
    rows.forEach((r, i) => {
      const ry = 720 + i * 62;
      hairline(ctx, SIDE, 1770, ry - 40, head, 0.14);
      if (r.accent) {
        ctx.fillStyle = V26.signal;
        ctx.fillRect(SIDE, ry - 15, 10, 10);
      }
      drawText(ctx, layoutText(r.label, F.mono), SIDE + (r.accent ? 24 : 0), ry, { color: V26.onNight2 });
      drawCounter(ctx, Math.round(r.value * r.p), 1770, ry, F.mono, { align: 'right', color: V26.onNight });
    });
    hairline(ctx, SIDE, 1770, 720 + 3 * 62 - 40, head, 0.14);
  });
}

function clip(ctx, fn) {
  ctx.save();
  ctx.beginPath();
  ctx.rect(BOX.x, BOX.y, BOX.w, BOX.h);
  ctx.clip();
  fn();
  ctx.restore();
}

function drawEnd(ctx, s) {
  const t = s.t, t0 = T.endIn;
  const size = 170;
  drawMark(ctx, 960 - markWidth(size) / 2, 268, size, { color: V26.onNight, body: progress(t, t0 + 0.1, 0.8, ease.out), bits: progress(t, t0 + 0.35, 1.4) });
  const L = layoutText(`Tovek ${D.binaries.b.label}`, F.card);
  reveal(ctx, L, 960, 640, t, { unit: 'word', start: t0 + 0.6, stagger: 0.12, dur: 1.0, align: 'center', color: V26.onNight });
  fade(ctx, progress(t, t0 + 1.3, 0.8), () => {
    drawText(ctx, layoutText('github.com/Kiet1308/Tovek', F.mono), 960, 724, { color: V26.onNight2, align: 'center' });
  });
  fade(ctx, progress(t, t0 + 1.7, 0.8), () => {
    kicker(ctx, 'Built on medal by Jujhar Singh and Mathias Pedersen', 960, 900, 1, 'center');
  });
}

const film = timeline([
  { id: 'title', at: 0, dur: T.countIn + 0.4, in: 0, out: 0.4, draw: drawTitle },
  { id: 'count', at: T.countIn - 0.2, dur: T.morphIn - T.countIn + 0.8, in: 0.3, out: 0.3, draw: drawCount },
  { id: 'morph', at: T.morphIn, dur: T.endIn - T.morphIn + 0.6, in: 0.2, out: 0.3, draw: drawMorphScene },
  { id: 'end', at: T.endIn, dur: T.duration - T.endIn, in: 0.3, out: 0, draw: drawEnd },
]);

const demo = {
  id: 'demo',
  title: 'Engine reel',
  description: 'Every helper of the film engine, once: kinetic type, counters, a token morph between two real Tovek outputs, and the score.',
  duration: T.duration,
  poster: 2.7,
  background: V26.night,
  fonts: Object.values(F).map((f) => f.css),
  chapters: [
    { t: 0, title: 'Title', still: 2.6 },
    { t: T.countIn, title: 'Count', still: 6.9 },
    { t: T.morphIn, title: 'Morph', still: 11.6 },
    { t: T.endIn, title: 'End card', still: 17.8 },
  ],
  captions: [],

  async prepare() {
    if (D) return;
    D = await (await fetch(new URL('./demo.data.json', import.meta.url))).json();
    plan = codeMorph(D.excerpt.a, D.excerpt.b);
    // A: the whole excerpt. B: from the table that opens the action log to the end.
    const from = Math.max(0, plan.b.lineOf(/^\s*local \w+ = \{ type = /) - 1);
    views = { a: [0, plan.a.lineCount - 1], b: [from, plan.b.lineCount - 1] };
    const n = (x) => formatNumber(x);
    demo.captions = [
      { start: 0.6, end: 3.4, text: `Tovek, from ${D.binaries.a.label} to ${D.binaries.b.label}.` },
      { start: 4.2, end: 7.2, text: `${D.set.scripts} open-source modules: ${n(D.set.linesA)} lines become ${n(D.set.linesB)}.` },
      { start: 7.6, end: 9.0, text: `${D.sample.project}'s Store, as ${D.binaries.a.label} wrote it.` },
      { start: 9.2, end: 13.6, text: `The same bytecode, decompiled again by ${D.binaries.b.label}.` },
      { start: 13.9, end: 15.5, text: `In the accent: what ${D.binaries.b.label} recovered.` },
      { start: 16.2, end: 18.4, text: `Tovek ${D.binaries.b.label}.` },
    ];
    demo.score = defineScore({
      duration: T.duration,
      seed: 26,
      reverb: { seconds: 3.6, wet: 0.3 },
      events: [
        { t: 0, voice: 'air', dur: T.duration, gain: 0.035, cutoff: 700 },
        { t: 0, voice: 'hum', dur: T.endIn + 1.5, freq: note('A1'), gain: 0.16, attack: 3 },
        { t: 0.3, voice: 'pulse', freq: note('E3'), gain: 0.16, dur: 2.6 },
        { t: 1.3, voice: 'pulse', freq: note('B3'), gain: 0.08, dur: 2 },
        ...ticks(1.3, 2.1, { rate: 26, seed: 4, gain: 0.03 }),
        { t: T.countIn + 0.6, voice: 'pulse', freq: note('A3'), gain: 0.14, dur: 2.4 },
        { t: T.countIn + 2.9, voice: 'chime', freq: note('E5'), gain: 0.05, dur: 2.6 },
        { t: T.morphIn, voice: 'sub', freq: 80, to: 40, gain: 0.18, dur: 1.2 },
        { t: T.morphStart, voice: 'pulse', freq: note('C#4'), gain: 0.1, dur: 2.4 },
        ...ticks(T.morphStart + 2.4, T.morphEnd - 0.1, { rate: 18, seed: 9, gain: 0.035 }),
        { t: T.morphEnd, voice: 'chime', freq: note('A5'), gain: 0.07, dur: 3.2 },
        { t: T.endIn - 0.6, voice: 'swell', dur: T.duration - T.endIn + 0.6, notes: ['A2', 'E3', 'C#4', 'E4'].map(note), gain: 0.12, attack: 1.6, release: 1.8 },
        { t: T.endIn, voice: 'sub', freq: 70, to: 35, gain: 0.2, dur: 1.6 },
        { t: T.endIn + 0.35, voice: 'pulse', freq: note('A4'), gain: 0.06, dur: 3 },
      ],
    });
  },

  render(ctx, t, w, h) {
    film.render(ctx, t, w, h);
  },
};

export default demo;
