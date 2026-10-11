// III. Names, from evidence. The page loses its names (the bytecode keeps none), then the camera goes
//      close on four of them; each comes back from the token it was read from, along a hairline.
// IV. Loops, not jumps. The read head sweeps the loop back into the bytecode: its real instructions
//     and their four jumps. Then it sweeps forward again, and each jump is carried into the loop's
//     structure: the loop itself, its continue, its break.
// The page is one object from the scan to the end of the loop shot; one camera moves over it.

import {
  T, C, TY, S, PAGE, f, fmt, surface, kicker, say, leader, underline, numberCard, strokePath, cameraTrack, frameAt, applyCamera, toScreen, drawHero, tokenXY, headV, drift, TRACK,
} from './base.js';
import { ease, clamp, lerp, progress, envelope, layoutText, drawText, codeMetrics, setFont } from '../../engine/index.js';
import { wideCam, drawScan } from './open.js';

// ---------------------------------------------------------------------------------------- set-up

let N = null; // prepared: featured names, var timing

export function prepareNames() {
  const D = S.D, H = S.hero;
  const decls = D.hero.declared;
  const varOf = (line, col) => decls.findIndex((d) => d.line === line && d.col === col);
  const ats = [T.n1, T.n2, T.n3, T.n4];
  const featured = D.hero.featured_names.slice(0, 4).map((fn, i) => {
    const v = varOf(fn.decl.line, fn.decl.col);
    if (v < 0) throw new Error(`names: ${fn.name} is not a declared local`);
    const sameLine = fn.evidence[0].line === fn.decl.line;
    return { ...fn, v, at: ats[i], sameLine, resolve: ats[i] + (sameLine ? 0.8 : 1.1) };
  });
  const lastLine = H.named.lineCount - 1;
  // when each name comes back: the featured ones at their beats, the rest in a wave down the page
  const resolveAt = decls.map((d, v) => {
    const fe = featured.find((x) => x.v === v);
    if (fe) return fe.resolve;
    return T.rest0 + (d.line / lastLine) * (T.rest1 - T.rest0 - 0.3);
  });
  const drainAt = decls.map((d) => T.drain0 + (d.line / lastLine) * (T.drain1 - T.drain0 - 0.35));
  N = { featured, resolveAt, drainAt };
  prepareLoop();
}

/** 0..1: how much of variable v's name shows at t. */
function namedAt(t) {
  return (v) => {
    if (t < N.drainAt[v]) return 1;
    const out = ease.inOut(clamp((t - N.drainAt[v]) / 0.38));
    const back = ease.inOut(clamp((t - N.resolveAt[v]) / 0.5));
    return Math.max(1 - out, back);
  };
}

// ---------------------------------------------------------------------------------------- camera

/** Frame lines [l0, l1] so their text starts at screen x `left`, centred on screen y `cy`, at most `maxW` wide. */
function frameLines(l0, l1, { left = 170, cy = 540, maxW = 1600, maxS = 1.9 } = {}) {
  const H = S.hero, L = H.named;
  let indent = Infinity, cols = 0;
  for (let l = l0; l <= l1; l++) {
    const raw = L.lines[l];
    if (!raw.trim()) continue;
    const lead = raw.length - raw.replace(/^\t+/, '').length;
    indent = Math.min(indent, lead * 4);
    cols = Math.max(cols, L.lineCols[l]);
  }
  const s = Math.min(maxS, maxW / ((cols - indent) * H.cw));
  const midY = (H.lineY(l0) + H.lineY(l1) + H.lh) / 2;
  return frameAt(indent * H.cw, midY, left, cy, s);
}

let CAM = null;
export function pageCamera(t) {
  if (!CAM) {
    const H = S.hero;
    const wide = wideCam();
    const right = frameAt(H.width / 2, H.height / 2, 1395, 548, wide.s);
    const fe = N.featured;
    const macro = fe.map((x) => {
      const lines = [x.decl.line, ...x.evidence.map((e) => e.line)];
      const l0 = Math.min(...lines), l1 = Math.max(...lines);
      return frameLines(l0, l1, { left: 180, cy: l0 === l1 ? 500 : 520, maxS: l0 === l1 ? 1.9 : 1.22 });
    });
    const L = S.D.hero.loop.luau_lines;
    LOOPCAM = frameLines(L[0], L[1], { left: 270, cy: 586, maxW: 1480, maxS: 1.3 });
    // after the scan: close enough to read, and a slow pan down the first function
    const read0 = frameLines(0, 12, { left: 300, cy: 560, maxS: 1.05 });
    const read1 = { ...read0, y: read0.y + 6.5 * H.lh, s: read0.s * 1.02 };
    const keys = [
      [T.scan1, wide],
      [T.scan1 + 0.9, read0, ease.inOutCubic],
      [T.names - 0.25, read1, ease.inOutSine],
      [T.names + 0.95, right, ease.inOutCubic],
      [T.n1 - 0.45, right],
    ];
    fe.forEach((x, i) => {
      keys.push([x.at + 0.35, macro[i], ease.inOutCubic]);
      const next = i + 1 < fe.length ? fe[i + 1].at - 0.45 : T.rest0 - 0.1;
      // a slow push while the name is read
      keys.push([next, { ...macro[i], s: macro[i].s * 1.035 }, ease.inOutSine]);
    });
    keys.push([T.rest0 + 0.85, right, ease.inOutCubic]);
    keys.push([T.loops, right]);
    keys.push([T.loops + 1.05, LOOPCAM, ease.inOutCubic]);
    keys.push([T.loopsNum - 0.2, { ...LOOPCAM, s: LOOPCAM.s * 1.02 }, ease.inOutSine]);
    keys.push([T.loopsNum + 0.7, { ...LOOPCAM, x: LOOPCAM.x - 330 / LOOPCAM.s, s: LOOPCAM.s * 0.92 }, ease.inOutCubic]);
    CAM = cameraTrack(keys);
  }
  return CAM(t);
}
let LOOPCAM = null;

// ------------------------------------------------------------------------------------- the scene

export function drawPage(ctx, t) {
  const H = S.hero;
  const cam = pageCamera(t);
  if (t < T.scan1) {
    // the scan itself
    drawScan(ctx, t, cam);
    return;
  }
  surface(ctx, C.paper);
  const named = namedAt(t);
  const L = S.D.hero.loop.luau_lines;
  const loopFocus = progress(t, T.loops + 0.1, 0.8, ease.inOut);
  const focus = t >= N.featured[0].at - 0.3 && t < T.rest0 + 0.6 ? focusOf(t) : null;
  const numDim = envelope(t, T.namesNum + 0.2, T.loops + 0.4, 0.6, 0.5);
  const lineAlpha = (l) => {
    let a = 1;
    if (loopFocus > 0 && (l < L[0] || l > L[1])) a *= 1 - loopFocus;
    if (focus) {
      const fe = focus.fe;
      const own = l === fe.decl.line || fe.evidence.some((e) => e.line === l);
      if (!own) a *= 1 - 0.86 * focus.a;
    }
    return a * (1 - 0.72 * numDim);
  };
  const accent = (tk) => {
    let a = 0;
    if (tk.v >= 0) {
      const r = N.resolveAt[tk.v];
      const isF = N.featured.some((x) => x.v === tk.v);
      a = envelope(t, r - 0.05, r + (isF ? 2.0 : 0.9), 0.2, isF ? 1.1 : 0.7) * (isF ? 1 : 0.8);
    }
    return Math.max(a, loopAccent(t, tk, cam));
  };
  ctx.save();
  applyCamera(ctx, cam);
  drawHero(ctx, H, { named, accent, lineAlpha });
  ctx.restore();

  // the label while the page holds wide
  const la = envelope(t, T.scan1 + 0.15, T.scan1 + 1.75, 0.4, 0.45);
  if (la > 0) kicker(ctx, `${S.D.hero.file} · the output of Tovek V2.6`, 140, 96, { color: C.ink2, alpha: la });

  if (t >= T.names - 0.1 && t < T.loops + 0.5) drawNames(ctx, t, cam, named);
  if (t >= T.loops - 0.1) drawLoop(ctx, t, cam);
}

// --------------------------------------------------------------------------------------- III. names

/** The featured name being shown at t (with its own envelope), or null. */
function focusOf(t) {
  let best = null, a = 0;
  for (const fe of N.featured) {
    const e = envelope(t, fe.at - 0.25, nextAt(fe) - 0.05, 0.45, 0.4);
    if (e > a) { a = e; best = fe; }
  }
  return best ? { fe: best, a } : null;
}

const nextAt = (fe) => {
  const i = N.featured.indexOf(fe);
  return N.featured[i + 1] ? N.featured[i + 1].at : T.rest0;
};

function drawNames(ctx, t, cam, named) {
  const H = S.hero;
  // the title, left of the page
  if (t < N.featured[0].at + 0.4) {
    const out = N.featured[0].at - 0.5;
    say(ctx, 'Names,', 140, 372, t, T.names + 0.15, { font: TY.title, color: C.ink, out, tracking: TRACK.title });
    say(ctx, 'from evidence.', 140, 372 + 116, t, T.names + 0.3, { font: TY.title, color: C.ink, out: out + 0.05, tracking: TRACK.title });
  }

  // each featured name: underline the evidence, draw the leader to the name, then the name arrives
  for (const fe of N.featured) {
    const t0 = fe.at;
    const life = envelope(t, t0 - 0.05, nextAt(fe) - 0.1, 0.15, 0.35);
    if (life <= 0) continue;
    const s = cam.s;
    const ev = fe.evidence.map((e) => {
      const tk = H.tokAt(e.line, e.col);
      const p = tk ? tokenXY(H, tk, named) : { x: e.col * H.cw, y: H.lineY(e.line) };
      return { a: toScreen(cam, p.x, p.y + H.lh * 0.86), w: e.len * H.cw * s };
    });
    const dtk = H.tokAt(fe.decl.line, fe.decl.col);
    const dp = tokenXY(H, dtk, named);
    const nameW = lerp(dtk.plain.length, dtk.text.length, named(fe.v)) * H.cw;
    const d0 = toScreen(cam, dp.x, dp.y + H.lh * 0.86);
    const u = progress(t, t0 + 0.1, 0.45, ease.out);
    const lw = Math.max(2.5, 1.7 * s);
    ctx.save();
    ctx.globalAlpha *= life;
    for (const e of ev) underline(ctx, e.a.x, e.a.y, e.w, u, { width: lw });
    // the leader: from the evidence to the name's slot. On one line it dips into the gap below the
    // line; across lines it swings out to the left, through the indent
    const near = !fe.sameLine && Math.abs(fe.evidence[0].line - fe.decl.line) <= 1;
    const left = ev.reduce((m, e) => (e.a.x < m.a.x ? e : m), ev[0]);
    const from = near ? { x: left.a.x + 4, y: left.a.y + lw * 1.5 } : { x: ev[0].a.x + ev[0].w / 2, y: ev[0].a.y + lw * 1.5 };
    const to = { x: d0.x + (nameW * s) / 2, y: d0.y + lw * 1.5 };
    const lp = progress(t, t0 + 0.4, fe.sameLine || near ? 0.5 : 0.8, ease.inOut);
    // across lines the curve's middle runs down the indent, left of the code (quadratic: mid = (a + 2c + b) / 4)
    const gutterX = toScreen(cam, 1.6 * H.cw, 0).x;
    const ctrl = fe.sameLine
      ? { x: (from.x + to.x) / 2, y: from.y + H.lh * s * 1.05 }
      : near ? null : { x: (4 * gutterX - from.x - to.x) / 2, y: (from.y + to.y) / 2 };
    leader(ctx, from, to, lp, { width: 2.5, ctrl, bend: 16, head: true });
    // the name's slot is marked while it waits
    const slot = envelope(t, t0 + 0.3, fe.resolve + 0.4, 0.2, 0.3);
    if (slot > 0) underline(ctx, d0.x, d0.y, nameW * s, 1, { width: lw, alpha: slot });
    ctx.restore();
  }

  // the number
  if (t >= T.namesNum + 0.4) {
    const sc = S.D.scale;
    const none = sc.local_names_in_bytecode === 0;
    numberCard(ctx, t, T.namesNum + 0.45, {
      x: 140, y: 560, value: fmt(sc.locals_named),
      text: `locals named, in ${fmt(sc.scripts)} scripts\nfrom four real games.`,
      note: none ? 'Their bytecode stored no local names' : `${fmt(sc.local_names_in_bytecode)} local names were in the bytecode`,
      out: T.loops - 0.1,
    });
  }
}

// ---------------------------------------------------------------------------------------- IV. loops

let LOOP = null;
const LIST = { x: 700, labelX: 604, size: 25, lh: 37.5, top: 0, cy: 618 };

function prepareLoop() {
  const lp = S.D.hero.loop;
  const rows = lp.listing.length;
  LIST.top = LIST.cy - (rows * LIST.lh) / 2;
  // lanes: the shortest jump innermost
  const order = lp.jumps.slice().sort((a, b) => Math.abs(a.to - a.from) - Math.abs(b.to - b.from));
  const lanes = new Map(order.map((j, i) => [j, LIST.labelX - 58 - i * 30]));
  LOOP = {
    rows,
    jumps: lp.jumps.map((j, i) => ({ ...j, lane: lanes.get(j), at: T.arcs0 + i * 0.36 })),
  };
}

const rowY = (r) => LIST.top + r * LIST.lh + LIST.lh / 2;
const headBack = (t) => lerp(1960, -40, progress(t, T.back0, T.back1 - T.back0, ease.inOutSine));
const headFwd = (t) => lerp(-40, 1960, progress(t, T.fwd0, T.fwd1 - T.fwd0, ease.inOutSine));

/** Where the night is during the loop shot: the region right of the back sweep, left of the forward one. */
function nightRegion(t) {
  if (t < T.back0) return null;
  if (t < T.fwd0) return { x0: headBack(t), x1: 1924 };
  if (t < T.fwd1) return { x0: headFwd(t), x1: 1924 };
  return null;
}

/** The four corners of a jump arrow in the listing. */
function listingPath(j) {
  const y0 = rowY(j.from), y1 = rowY(j.to), x = j.lane, x0 = LIST.labelX - 16;
  return [[x0, y0], [x, y0], [x, y1], [x0, y1]];
}

/**
 * The same jump carried into the Luau loop (screen space under the page camera): the back edge
 * becomes the loop's bracket; continue and break run down lanes on the right to where they land (the
 * loop's end, and the line after it).
 */
function structurePath(j, cam) {
  const H = S.hero, lp = S.D.hero.loop, K = lp.keywords;
  const at = (line, col) => toScreen(cam, col * H.cw, H.lineY(line) + H.lh / 2);
  const top = at(K.loop.line, 0), bottom = at(K.loop.end_line, 0);
  const gx = top.x - 30 * cam.s;
  if (j.role === 'loop') return [[bottom.x - 10, bottom.y], [gx, bottom.y], [gx, top.y], [top.x - 10, top.y]];
  if (j.role === 'enter') return [[top.x - 10, top.y], [gx, top.y], [gx, top.y], [top.x - 10, top.y]];
  let inner = 0;
  for (let l = K.loop.line + 1; l < K.loop.end_line; l++) inner = Math.max(inner, H.named.lineCols[l]);
  const k = K[j.role];
  const s = at(k.line, k.col + k.len);
  const lane = at(k.line, inner + (j.role === 'continue' ? 3 : 6)).x;
  const endLine = j.role === 'continue' ? K.loop.end_line : K.loop.end_line + 1;
  const e = at(endLine, j.role === 'continue' ? 3.6 : 0.4);
  return [[s.x + 12, s.y], [lane, s.y], [lane, e.y], [e.x, e.y]];
}

/** Accent on the loop's keywords: each lights as the forward head passes it, and settles after the number. */
function loopAccent(t, tk, cam) {
  if (!LOOP || t < T.fwd0) return 0;
  const K = S.D.hero.loop.keywords;
  const isKw = (tk.line === K.loop.line && (tk.text === 'for' || tk.text === 'do'))
    || (tk.line === K.loop.end_line && tk.text === 'end' && tk.colB === 0)
    || (tk.line === K.continue.line && tk.text === 'continue')
    || (tk.line === K.break.line && tk.text === 'break');
  if (!isKw) return 0;
  const x = toScreen(cam, tk.colB * S.hero.cw, 0).x;
  return clamp((headFwd(t) - x) / 90) * (1 - progress(t, T.loopsNum + 1.6, 0.8));
}

function drawLoop(ctx, t, cam) {
  const lp = S.D.hero.loop;
  const night = nightRegion(t);
  // the night side: the loop as the bytecode has it
  if (night) {
    ctx.save();
    ctx.beginPath();
    ctx.rect(night.x0, -4, night.x1 - night.x0, 1088);
    ctx.clip();
    surface(ctx, C.night);
    drawListing(ctx, t);
    ctx.restore();
  }
  // the title: ink on the paper, paper on the night
  const title = (color) => say(ctx, 'Loops, not jumps.', 140, 196, t, T.loops + 0.45, { font: TY.title, color, out: T.loopsNum - 0.4, tracking: TRACK.title });
  splitDraw(ctx, night, () => title(C.ink), () => title(C.onNight));
  // the jumps. On the night side they are the bytecode's jumps; on the paper side, the loop's structure.
  // As the forward head passes, each jump turns into what it became (the enter jump folds into `for`)
  const out = 1 - progress(t, T.loopsNum + 1.6, 0.8);
  const paperEdge = night ? night.x0 : 1924;
  for (const j of LOOP.jumps) {
    if (night && t >= j.at) {
      ctx.save();
      ctx.beginPath();
      ctx.rect(night.x0, -4, night.x1 - night.x0, 1088);
      ctx.clip();
      strokePath(ctx, listingPath(j), progress(t, j.at, 0.5, ease.inOut), { color: C.onNight2, width: 2.4, head: true, dot: true });
      ctx.restore();
    }
    if (t >= T.fwd0 && j.role !== 'enter' && out > 0) {
      ctx.save();
      ctx.globalAlpha *= out;
      ctx.beginPath();
      ctx.rect(-4, -4, paperEdge + 4, 1088);
      ctx.clip();
      strokePath(ctx, structurePath(j, cam), 1, { color: C.signalDeep, width: 3, head: true, dot: true });
      ctx.restore();
    }
  }
  // the heads
  if (t >= T.back0 && t < T.back1 + 0.1) headV(ctx, headBack(t), envelope(t, T.back0, T.back1, 0.08, 0.15), { side: 1 });
  if (t >= T.fwd0 && t < T.fwd1 + 0.1) headV(ctx, headFwd(t), envelope(t, T.fwd0, T.fwd1, 0.08, 0.15), { side: -1 });
  // the number
  if (t >= T.loopsNum) {
    const sc = S.D.scale;
    numberCard(ctx, t, T.loopsNum + 0.25, {
      x: 140, y: 620, value: fmt(sc.goto_statements), color: sc.goto_statements === 0 ? C.signalDeep : C.ink,
      text: `goto statements, in ${fmt(sc.scripts)} scripts\nfrom four real games.`,
      note: 'Luau has no goto', out: T.sting - 0.25, maxWidth: 900,
    });
  }
}

/** Draw `paper` on the paper side and `nightFn` on the night side of the frame. */
function splitDraw(ctx, night, paper, nightFn) {
  if (!night) { paper(); return; }
  ctx.save();
  ctx.beginPath();
  ctx.rect(-4, -4, night.x0 + 4, 1088);
  ctx.clip();
  paper();
  ctx.restore();
  ctx.save();
  ctx.beginPath();
  ctx.rect(night.x0, -4, night.x1 - night.x0, 1088);
  ctx.clip();
  nightFn();
  ctx.restore();
}

function drawListing(ctx, t) {
  const lp = S.D.hero.loop;
  const F = f(LIST.size, 'mono', 400), Fb = f(LIST.size, 'mono', 500), FL = f(LIST.size - 3, 'mono', 400);
  ctx.save();
  ctx.textBaseline = 'middle';
  kicker(ctx, 'The same loop, in the bytecode', LIST.labelX, LIST.top - 30, { color: C.onNight2, size: 18 });
  for (let r = 0; r < lp.listing.length; r++) {
    const row = lp.listing[r];
    const y = rowY(r);
    if (row.label) {
      setFont(ctx, FL);
      ctx.textAlign = 'left';
      ctx.fillStyle = C.onNight2;
      ctx.fillText(row.label, LIST.labelX, y);
    }
    ctx.textAlign = 'left';
    const sp = row.text.indexOf(' ');
    const op = sp < 0 ? row.text : row.text.slice(0, sp);
    const rest = sp < 0 ? '' : row.text.slice(sp);
    const isJump = /^(JUMP|FORG)/.test(op);
    setFont(ctx, isJump ? Fb : F);
    ctx.fillStyle = isJump ? C.onNight : '#b8b5ab';
    ctx.fillText(op, LIST.x, y);
    const w = ctx.measureText(op).width;
    setFont(ctx, F);
    ctx.fillStyle = '#7f7d75';
    ctx.fillText(rest, LIST.x + w, y);
  }
  ctx.restore();
}
