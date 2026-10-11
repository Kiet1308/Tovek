// X. Free and open source. MIT; Windows, macOS, Linux; a command line and a local server, with their
//    real output; in the browser too, when the in-browser build is published (launch.json browser flag).
// XI. The end card: the mark and Tovek V2.6, the site underlined by the read head, and medal, where it
//     began. The opening's bytes are there, faint, behind it.

import {
  T, C, TY, S, BEAT, f, fmt, surface, kicker, say, note, TRACK, AROUND, headH, drift, PAIRS,
} from './base.js';
import { ease, clamp, lerp, progress, envelope, layoutText, drawText, reveal, typewriter, drawMark, markWidth, mix } from '../../engine/index.js';

// ----------------------------------------------------------------------------------------- X. free

const OS_ORDER = ['Windows', 'macOS', 'Linux'];
const ARCH = { x86_64: 'x86_64', aarch64: 'arm64' };

function osRows() {
  const pk = S.D.platforms.packages || [];
  const rows = OS_ORDER.filter((o) => pk.some((p) => p.os === o)).map((o) => {
    const archs = pk.filter((p) => p.os === o).map((p) => (o === 'macOS' && p.arch === 'aarch64' ? 'Apple silicon' : ARCH[p.arch] || p.arch));
    return { os: o, note: archs.join(' · ') };
  });
  return rows.length ? rows : OS_ORDER.map((o) => ({ os: o, note: '' }));
}

const WORD_Y = [372, 560, 748];

export function drawFree(ctx, t) {
  // the paper rises over the last of the night, the head on its edge
  const wipe = progress(t, T.free - 0.6, 0.6, ease.inOutCubic);
  const edge = lerp(1090, -6, wipe);
  ctx.save();
  if (wipe < 1) {
    ctx.beginPath();
    ctx.rect(-10, edge, 1940, 1100);
    ctx.clip();
  }
  surface(ctx, C.paper);
  ctx.save();
  drift(ctx, t, T.free, T.term0, 0.02, 600, 560);
  const words = ['Free.', 'Open source.', `${S.D.license || 'MIT'} license.`];
  const leave = T.term0 - 0.15;
  words.forEach((w, i) => {
    say(ctx, w, 140, WORD_Y[i], t, T.free + 0.05 + i * BEAT, { font: TY.word, color: C.ink, out: leave + i * 0.05, stagger: 0.05, tracking: TRACK.word });
  });
  // the systems, on the same baselines as the words
  osRows().forEach((r, i) => {
    const at = T.os + i * BEAT * 0.8;
    const y = WORD_Y[i];
    say(ctx, r.os, 1250, y, t, at, { font: TY.os, color: C.ink, out: leave + 0.15 + i * 0.05, tracking: TRACK.word });
    const a = progress(t, at + 0.3, 0.5) * (1 - progress(t, leave + 0.15, 0.4));
    if (r.note) kicker(ctx, r.note, 1254, y + 44, { color: C.ink3, alpha: a, size: 17 });
  });
  ctx.restore();
  drawTerminal(ctx, t);
  ctx.restore();
  if (wipe > 0 && wipe < 1) headH(ctx, edge, 1, { side: 1, glow: 0.14 });
}

function drawTerminal(ctx, t) {
  const D = S.D, cli = D.cli;
  const browser = D.browser && D.browser.available;
  const gone = browser ? T.browser - 0.15 : T.end - 0.45;
  // in the browser, when there is a build for it
  if (browser && t >= T.browser) {
    say(ctx, 'And in your browser.', 140, 520, t, T.browser, { font: TY.title, color: C.ink, out: T.end - 0.45, tracking: TRACK.title });
    note(ctx, 'Nothing is uploaded.', 144, 620, t, T.browser + 0.5, T.end - 0.5, { color: C.ink2, font: f(40, 'text', 400) });
  }
  const a = progress(t, T.term0, 0.4, ease.out) * (1 - progress(t, gone, 0.45));
  if (a <= 0) return;
  const ex = cli.example || {};
  const FC = f(46, 'mono', 400), FO = f(30, 'mono', 400);
  const x = 150;
  ctx.save();
  ctx.globalAlpha *= a;
  ctx.translate(0, -progress(t, gone, 0.45, ease.inCubic) * 30);
  const lines = [
    { y: 392, text: '$ ' + cli.folder, kind: 'cmd', at: T.term0 + 0.3, dur: 0.95 },
    { y: 458, text: ex.done || '', kind: 'out', at: T.term0 + 1.45, color: C.ink },
    { y: 504, text: ex.time || '', kind: 'out', at: T.term0 + 1.62, color: C.ink3 },
    { y: 676, text: '$ ' + cli.server, kind: 'cmd', at: T.term0 + 2.15, dur: 0.4 },
    { y: 742, text: cli.server_line.line, kind: 'out', at: T.term0 + 2.75, color: C.ink },
  ];
  kicker(ctx, 'Command line', x, 312, { color: C.ink2, alpha: progress(t, T.term0 + 0.1, 0.4) });
  kicker(ctx, 'Local server', x, 596, { color: C.ink2, alpha: progress(t, T.term0 + 1.95, 0.4) });
  for (const l of lines) {
    if (t < l.at || !l.text) continue;
    if (l.kind === 'cmd') {
      const L = layoutText(l.text, FC);
      const n = clamp((t - l.at) / l.dur) * L.glyphCount;
      const typing = t < l.at + l.dur + 0.7;
      typewriter(ctx, L, x, l.y, n, { color: C.ink, caret: typing, caretAlpha: Math.floor(t * 2.4) % 2 === 0 ? 1 : 0.25, caretColor: C.signalDeep });
    } else {
      drawText(ctx, layoutText(l.text, FO), x, l.y, { color: l.color, alpha: progress(t, l.at, 0.25) });
    }
  }
  ctx.restore();
}

// ------------------------------------------------------------------------------------------ XI. end

export function drawEnd(ctx, t) {
  surface(ctx, C.paper);
  drawWallFaint(ctx, t);
  ctx.save();
  drift(ctx, t, T.end, T.duration, 0.03, 960, 520);
  // the lockup: the mark, then the name beside it, centred as one
  const size = 196;
  const L = layoutText('Tovek V2.6', TY.mark, { around: AROUND, pairs: PAIRS });
  const gap = 64;
  const mw = markWidth(size);
  const total = mw + gap + L.width + (L.glyphCount - 1) * TRACK.title * TY.mark.size;
  const x0 = 960 - total / 2;
  const base = 560;
  const capTop = base - TY.mark.size * 0.7;
  const my = capTop + (TY.mark.size * 0.7 - size) / 2 + 2;
  if (t >= T.mark0) {
    const settle = progress(t, T.mark0 + 1.6, 1.4);
    drawMark(ctx, x0, my + (1 - ease.out(progress(t, T.mark0, 0.9))) * 26, size, {
      color: C.ink, bitColor: mix(C.signalDeep, C.ink, settle),
      body: progress(t, T.mark0, 0.8, ease.out), bits: progress(t, T.mark0 + 0.4, 1.4),
    });
  }
  reveal(ctx, L, x0 + mw + gap, base, t, { unit: 'glyph', start: T.word, stagger: 0.045, dur: 0.95, color: C.ink, mask: true, rise: 1.0, tracking: TRACK.title });
  // the address, and the head that underlines it
  const site = S.D.site;
  const UL = layoutText(site, TY.url);
  const ua = progress(t, T.url, 0.7);
  if (ua > 0) drawText(ctx, UL, 960, 716, { color: C.ink, align: 'center', alpha: ua });
  const up = progress(t, T.url + 0.3, 0.9, ease.inOutCubic);
  if (up > 0) {
    ctx.fillStyle = C.signalDeep;
    ctx.fillRect(960 - UL.width / 2, 736, UL.width * up, 2.5);
  }
  ctx.restore();
  const ca = progress(t, T.credit, 0.9);
  if (ca > 0) {
    const origin = S.D.credits.origin.match(/originally by (.+)$/);
    drawText(ctx, layoutText(`Built on medal, by ${origin ? origin[1] : 'Jujhar Singh and Mathias Pedersen'}.`, TY.credit), 960, 852, { color: C.ink2, align: 'center', alpha: ca });
    drawText(ctx, layoutText(`Free and open source · ${S.D.license || 'MIT'} license · Windows, macOS, Linux`, TY.credit), 960, 888, { color: C.ink3, align: 'center', alpha: ca * 0.95 });
  }
}

/** The opening's bytes once more, faint on paper behind the name: where the film began. */
function drawWallFaint(ctx, t) {
  const a = progress(t, T.end + 0.3, 2.6, ease.inOut) * 0.075;
  if (a <= 0) return;
  const hex = S.D.hero.hex, n = hex.length / 2, cols = 48;
  const px = 40, py = 40;
  const drift = (t - T.end) * 4;
  ctx.save();
  ctx.fillStyle = C.ink;
  ctx.font = f(21, 'mono', 400).css;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  const base = ctx.globalAlpha * a;
  for (let i = 0; i < n; i++) {
    const c = i % cols, r = Math.floor(i / cols);
    const x = (c + 0.5) * px, y = (r + 0.5) * py - drift;
    // the bytes keep to the edges of the frame; the middle stays clear for the name
    const dx = (x - 960) / 960, dy = (y - 560) / 540;
    const v = clamp((Math.sqrt(dx * dx * 0.8 + dy * dy * 1.6) - 0.55) / 0.5);
    if (v <= 0.01) continue;
    ctx.globalAlpha = base * v;
    ctx.fillText(hex.substr(i * 2, 2), x, y);
  }
  ctx.restore();
}
