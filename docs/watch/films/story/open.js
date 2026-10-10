// Act I. One script, as bytes. The dump streams in; the camera dives into one byte; the byte opens
// into its opcode, the opcode into its instruction, the instruction into the line of source it came
// from. That line becomes medal's version of it, the camera pulls back over medal's output, and the
// film says where it began. Then medal's first commit rolls forward to Tovek's first beta, to the
// minute.

import {
  ease, clamp, lerp, envelope, smoothstep, spring,
  layoutText, drawText, reveal, typewriter, measure, metrics, setFont,
  layoutCode, codeMorph, drawMorph, rgba, mix, hash01, pixel, formatNumber,
} from '../../engine/index.js';
import {
  T, ERA, TY, TRACK, AROUND, CODE, S, surface, hairline, vline, faded, rollText, ticker, zoomCam, drift, driftOffset,
  flowOf, codeAt, longDate, stamp, label, span,
} from './base.js';

let HEX = null;     // the dump and the byte we dive into
let MED = null;     // medal's shot: the line morph and the code landscape

// ------------------------------------------------------------------------------------- prepare

export function prepareOpen() {
  const s = S.D.sample;
  const hex = s.bytecode_hex;
  const bytes = hex.length / 2;
  const PER = 48;
  const rows = [];
  for (let r = 0; r * PER < bytes; r++) {
    const off = (r * PER).toString(16).padStart(6, '0');
    let body = '';
    for (let b = r * PER; b < Math.min(bytes, (r + 1) * PER); b += 2) body += (body ? ' ' : '') + hex.slice(b * 2, b * 2 + 4);
    rows.push({ text: off + '  ' + body, b0: r * PER, b1: Math.min(bytes, (r + 1) * PER) });
  }
  // where shield's instructions sit in the file (little-endian words)
  const le = (w) => w.match(/../g).reverse().join('');
  const code = s.shield_bytecode;
  const seq = code.map((i) => le(i.word) + (i.aux ? le(i.aux) : '')).join('');
  const at = hex.indexOf(seq);
  const hl0 = at >= 0 ? at / 2 : -1, hl1 = at >= 0 ? (at + seq.length) / 2 : -1;
  // the instruction we follow: the first call by name, the line that finds the humanoid
  const k = Math.max(0, code.findIndex((i) => i.op === 'NAMECALL'));
  const ins = code[k];
  const insBytes = ins.aux ? 8 : 4;
  const target = hl0 >= 0 ? hl0 + ins.pc * 4 : 0; // the opcode is the low byte of the word, first in the file
  const F = TY.dark.hex;
  const cw = measure('0', F);
  const lh = 25.5;
  const width = rows[0].text.length * cw;
  const x0 = Math.round((1920 - width) / 2);
  const y0 = Math.round(540 - (rows.length * lh) / 2 + lh * 0.75);
  const charOf = (b) => 8 + Math.floor(b / 2) * 5 + (b % 2) * 2; // byte b of a row -> its first hex char
  const capH = metrics(F).capHeight || F.size * 0.7;
  const tr = Math.floor(target / PER);
  const bx = x0 + (charOf(target - tr * PER) + 1) * cw, by = y0 + tr * lh - capH / 2;
  const hlRows = rows.map((r, i) => (Math.max(hl0, r.b0) < Math.min(hl1, r.b1) ? i : -1)).filter((i) => i >= 0);
  // the instruction's own bytes in file order, and its fields
  const wordBytes = le(ins.word).match(/../g), auxBytes = ins.aux ? le(ins.aux).match(/../g) : [];
  const opcode = parseInt(ins.word.slice(6), 16);
  const srcLines = s.source.replace(/\r\n?/g, '\n').split('\n');
  const srcLine = srcLines[ins.line - 1] || '';
  const listing = code.slice(Math.max(0, k - 1), k + 4);
  HEX = {
    rows, F, cw, lh, x0, y0, width, bytes, hl0, hl1, hlRows, PER, charOf, capH,
    instructions: code.length, ins, k, target, insBytes, bx, by, wordBytes, auxBytes, opcode,
    srcLine, listing, scale: TY.dark.byte.size / F.size,
  };

  // medal: the source line becomes medal's line, in place, inside medal's own output
  const medal = S.ST.medal;
  const ex = medal.excerpts.shield;
  const Lm = layoutCode(ex.text);
  const mLine = Lm.lineOf(/FindFirstChildOfClass/);
  const indent = (Lm.lines[mLine].match(/^\t*/) || [''])[0];
  const plan = codeMorph(indent + srcLine.trim(), Lm.lines[mLine]);
  const flow = flowOf(Lm);
  const g = flow.gotos[0] || null;
  MED = { L: Lm, line: mLine, plan, goto: g, indentCols: indent.length * 4 };
}

// ---------------------------------------------------------------------------------- the dump

/** Byte classes for colour: 0 offset, 1 other bytes, 2 shield, 3 the instruction, 4 the byte we follow. */
function byteClass(b) {
  const H = HEX;
  if (b === H.target) return 4;
  if (b >= H.target && b < H.target + H.insBytes) return 3;
  if (b >= H.hl0 && b < H.hl1) return 2;
  return 1;
}

function dumpCamera(t) {
  // streaming: a slow pull back from the first rows to the whole dump
  const H = HEX;
  const pull = ease.inOut(clamp((t - 0.5) / 4.1));
  const s = lerp(2.3, 1, pull);
  const fx = lerp(H.x0 + 6 * H.cw, 960, pull), fy = lerp(H.y0 - H.lh * 0.3, 540, pull);
  const base = { s, x: 960 - fx * s, y: 540 - fy * s };
  if (t < T.dive0) return base;
  // the dive: constant zoom into one byte, which arrives at the centre of the frame
  const p = ease.inOutCubic(clamp((t - T.dive0) / (T.dive1 - T.dive0)));
  const end = { s: H.scale, x: 960 - H.bx * H.scale, y: 540 - H.by * H.scale };
  return zoomCam({ s: 1, x: 0, y: 0 }, end, p, { x: H.bx, y: H.by }, ease.inOut(clamp(p * 1.25)));
}

function drawDump(ctx, t) {
  const H = HEX, e = ERA.dark;
  const cam = dumpCamera(t);
  const ROW = 0.1, TYPE = 0.42;
  const focus = ease.inOut(clamp((t - T.focus) / 0.8));
  const ls = Math.log(cam.s);
  const others = 1 - smoothstep(Math.log(2.4), Math.log(9), ls);
  const word = 1 - smoothstep(Math.log(9), Math.log(22), ls);
  if (cam.s > H.scale * 0.999 && t > T.dive1) return;
  ctx.save();
  ctx.translate(cam.x, cam.y);
  ctx.scale(cam.s, cam.s);
  setFont(ctx, H.F);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  const base = ctx.globalAlpha;
  // only the rows and characters that are on screen (the dive magnifies 35x)
  const yTop = -cam.y / cam.s - H.lh * 2, yBot = (1080 - cam.y) / cam.s + H.lh;
  const c0 = Math.floor((-cam.x / cam.s - H.x0) / H.cw) - 2, c1 = Math.ceil(((1920 - cam.x) / cam.s - H.x0) / H.cw) + 2;
  const tone = [e.ink3, e.ink2, e.ink2, e.ink, e.ink];
  let caret = null;
  H.rows.forEach((row, r) => {
    const y = H.y0 + r * H.lh;
    if (y < yTop || y > yBot) return;
    const tr = T.hex0 + r * ROW;
    if (t < tr) return;
    const n = Math.min(row.text.length, Math.floor(((t - tr) / TYPE) * row.text.length));
    const seg = (a, b, cls) => {
      const lo = Math.max(a, c0), hi = Math.min(b, n, c1);
      if (hi <= lo) return;
      let alpha;
      if (cls === 0) alpha = lerp(0.8, 0.3, focus) * others;
      else if (cls === 1) alpha = lerp(0.62, 0.15, focus) * others;
      else if (cls === 2) alpha = lerp(0.62, 0.9, focus) * others;
      else if (cls === 3) alpha = lerp(0.62, 1, focus) * Math.max(word, others);
      else alpha = lerp(0.62, 1, focus);
      if (alpha <= 0.002) return;
      ctx.globalAlpha = base * alpha;
      ctx.fillStyle = cls >= 2 ? mix(e.ink2, e.ink, focus * (cls === 2 ? 0.5 : 1)) : tone[cls];
      ctx.fillText(row.text.slice(lo, hi), H.x0 + lo * H.cw, y);
    };
    seg(0, 6, 0);
    // runs of bytes of one class are one slice of the row's text
    let runStart = row.b0, runCls = byteClass(row.b0);
    for (let b = row.b0 + 1; b <= row.b1; b++) {
      const cls = b < row.b1 ? byteClass(b) : -1;
      if (cls !== runCls) {
        seg(H.charOf(runStart - row.b0), H.charOf(b - 1 - row.b0) + 2, runCls);
        runStart = b;
        runCls = cls;
      }
    }
    if (n < row.text.length) {
      seg(Math.max(8, n - 6), n, 3);
      caret = { x: H.x0 + n * H.cw, y };
    } else if (r === H.rows.length - 1) caret = { x: H.x0 + n * H.cw + H.cw * 0.4, y, idle: true };
  });
  if (t < T.hex0) caret = { x: H.x0, y: H.y0, idle: true };
  if (caret && t < T.focus + 0.2) {
    const on = caret.idle ? Math.floor(t * 1.9) % 2 === 0 : true;
    if (on) {
      ctx.globalAlpha = base * 0.95;
      ctx.fillStyle = e.ink;
      ctx.fillRect(caret.x + 1, caret.y - H.F.size * 0.78, H.cw * 0.62, H.F.size * 0.98);
    }
  }
  // labels: the file, then shield's bytes (they ride the camera and leave as it dives)
  const leave = 1 - smoothstep(Math.log(1.05), Math.log(2), ls);
  if (t < T.dive0 + 1 && leave > 0) {
    ctx.globalAlpha = base * (t < T.dive0 ? 1 : leave);
    const L1 = layoutText(`${S.D.sample.file}  ·  ${formatNumber(H.bytes)} bytes  ·  Luau bytecode v${S.D.sample.bytecode_version}`, TY.dark.label);
    typewriter(ctx, L1, H.x0, H.y0 - H.lh * 1.9, clamp((t - 1.6) * 34, 0, L1.glyphCount), { color: e.ink2 });
    if (H.hlRows.length) {
      const r0 = H.hlRows[0], r1 = H.hlRows[H.hlRows.length - 1];
      const bx = H.x0 + H.width + 26;
      const yA = H.y0 + r0 * H.lh - H.lh * 0.78, yB = H.y0 + r1 * H.lh + H.lh * 0.3;
      const a = ease.out(clamp((t - T.focus - 0.15) / 0.6));
      if (a > 0) {
        ctx.globalAlpha *= a;
        ctx.fillStyle = e.ink2;
        ctx.fillRect(bx, yA, Math.max(1, pixel(ctx)), (yB - yA) * ease.out(clamp((t - T.focus - 0.15) / 0.7)));
        reveal(ctx, layoutText(`${S.D.sample.focus_function}()`, TY.dark.prompt), bx + 18, (yA + yB) / 2 - 2, t, { unit: 'line', start: T.focus + 0.3, dur: 0.7, color: e.ink });
        reveal(ctx, layoutText(`${H.instructions} instructions`, TY.dark.label), bx + 18, (yA + yB) / 2 + 26, t, { unit: 'line', start: T.focus + 0.45, dur: 0.7, color: e.ink2 });
      }
    }
  }
  ctx.restore();
}

// ------------------------------------------------------------------ the byte opens up

/** A centred monospace string at (x, y) scaled by k about its centre. */
function monoAt(ctx, text, F, cx, y, k, color, alpha = 1) {
  if (alpha <= 0) return;
  ctx.save();
  ctx.globalAlpha *= alpha;
  ctx.translate(cx, y);
  ctx.scale(k, k);
  setFont(ctx, F);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'center';
  ctx.fillStyle = color;
  ctx.fillText(text, 0, 0);
  ctx.restore();
}

function drawByteStory(ctx, t) {
  const H = HEX, e = ERA.dark;
  if (t < T.dive1 - 0.02 || t > T.open1 + 0.4) return;
  const big = TY.dark.byte;
  const capBig = metrics(big).capHeight || big.size * 0.7;
  const hexByte = H.wordBytes[0];
  // where the byte sits: centre of frame at the end of the dive
  const yByte0 = 540 + capBig / 2;
  // B. the byte shrinks up, its opcode decodes beneath it
  const pb = ease.inOut(clamp((t - T.opcode) / 0.85));
  // C. the byte becomes the first cell of the instruction's bytes; the opcode its first word
  const pc = ease.inOut(clamp((t - T.instr) / 0.9));
  // D. the instruction joins its neighbours in the listing; E. the line of source
  const pd = ease.inOut(clamp((t - T.listing) / 0.9));

  const BF = TY.dark.bytes, cwB = measure('0', BF);
  const cells = [...H.wordBytes, ...H.auxBytes];
  const gapB = cwB * 1.2, groupGap = cwB * 2.2;
  const cellX = [];
  let x = 0;
  cells.forEach((_, i) => { cellX.push(x); x += cwB * 2 + (i === 3 ? groupGap : gapB); });
  const rowW = x - gapB;
  const rowX0 = 960 - rowW / 2, rowY = 452;

  // the byte itself: giant, then a small tag above the opcode, then the first cell of the row
  const k1 = lerp(1, 0.22, pb);
  const yB = lerp(yByte0, 352, pb);
  const cellCx = rowX0 + cellX[0] + cwB;
  const kC = BF.size / big.size;
  const bx = lerp(960, cellCx, pc), by = lerp(yB, rowY, pc), bk = lerp(k1, kC, pc);
  const byteAlpha = 1 - pd;
  monoAt(ctx, hexByte, big, bx, by, bk, e.ink, byteAlpha);
  // a quiet 0x in front of the byte while it is read as a number
  const cwBig = measure('0', big) * bk;
  monoAt(ctx, '0x', big, bx - cwBig * 2, by, bk, e.ink3, byteAlpha * ease.out(clamp((pb - 0.35) / 0.4)) * (1 - pc));

  // the offset of the byte in the file
  const offA = envelope(t, T.dive1 - 0.5, T.instr + 0.2, 0.4, 0.4);
  label(ctx, `BYTE ${formatNumber(H.target)} OF ${formatNumber(H.bytes)}`, 960, 150, e.ink2, { alpha: offA, align: 'center' });

  // the opcode decodes: cells flicker through hex and settle from the centre out
  const OF = TY.dark.opcode, IF = TY.dark.instr;
  const name = H.ins.op;
  const cwO = measure('0', OF), cwI = measure('0', IF);
  const insText = H.ins.text;
  const insW = insText.length * cwI;
  const insX0 = 960 - insW / 2, insY = 640;
  const oScale = lerp(1, IF.size / OF.size, pc);
  const oX0 = lerp(960 - (name.length * cwO) / 2, insX0, pc), oY = lerp(640, insY, pc);
  if (t > T.opcode + 0.15) {
    ctx.save();
    ctx.globalAlpha *= 1 - pd;
    ctx.translate(oX0, oY);
    ctx.scale(oScale, oScale);
    setFont(ctx, OF);
    ctx.textBaseline = 'alphabetic';
    ctx.textAlign = 'left';
    const tick = Math.floor(t * 26);
    const mid = (name.length - 1) / 2;
    for (let i = 0; i < name.length; i++) {
      const st = T.opcode + 0.25 + Math.abs(i - mid) * 0.07;
      if (t < st) continue;
      const settled = t >= st + 0.38;
      ctx.fillStyle = settled ? e.ink : e.ink3;
      ctx.globalAlpha = (1 - pd) * (settled ? 1 : 0.5 + 0.5 * hash01(9, i, tick));
      ctx.fillText(settled ? name[i] : '0123456789abcdef'[Math.floor(hash01(4, i, tick) * 16)], i * cwO, 0);
    }
    ctx.restore();
    // a hairline from the byte down to its opcode, and the opcode's number
    const la = envelope(t, T.opcode + 0.5, T.instr + 0.3, 0.4, 0.3);
    if (la > 0) {
      vline(ctx, 960, 384, 120 * ease.out(clamp((t - T.opcode - 0.5) / 0.5)), e.ink2, la * 0.8);
      label(ctx, `OPCODE ${H.opcode}`, 960, 712, e.ink3, { alpha: la, align: 'center' });
    }
  }

  // C. the rest of the instruction's bytes and its text
  if (pc > 0) {
    const a = (1 - pd);
    ctx.save();
    ctx.globalAlpha *= a;
    setFont(ctx, BF);
    ctx.textBaseline = 'alphabetic';
    ctx.textAlign = 'left';
    // which byte lights with which operand while the instruction types in
    const typedAt = (needle) => { const i = insText.indexOf(needle); return i < 0 ? Infinity : T.instr + 0.35 + (i / insText.length) * 0.9; };
    const ops = insText.split(' ');
    const pairs = [[1, ops[1]], [2, ops[2]], [4, ops[3]]];
    cells.forEach((c, i) => {
      if (i === 0) return;
      const st = T.instr + 0.15 + i * 0.05;
      const q = ease.out(clamp((t - st) / 0.4));
      if (q <= 0) return;
      const pair = pairs.find((p) => p[0] === i);
      const lit = pair ? envelope(t, typedAt(pair[1]), typedAt(pair[1]) + 1.1, 0.12, 0.6) : 0;
      ctx.globalAlpha = a * q * lerp(0.55, 1, lit);
      ctx.fillStyle = lit > 0 ? mix(e.ink2, e.ink, lit) : e.ink2;
      ctx.fillText(c, rowX0 + cellX[i], rowY + (1 - q) * 14);
    });
    // field labels under the bytes
    const fa = ease.out(clamp((t - T.instr - 0.5) / 0.5)) * a;
    if (fa > 0) {
      const lab = (txt, i0, i1) => {
        const xa = rowX0 + cellX[i0], xb = rowX0 + cellX[i1] + cwB * 2;
        hairline(ctx, xa, rowY + 22, xb - xa, e.ink2, fa * 0.5);
        label(ctx, txt, (xa + xb) / 2, rowY + 50, e.ink3, { alpha: fa, align: 'center', tracking: 0.1 });
      };
      lab('OPCODE', 0, 0); lab('A', 1, 1); lab('B', 2, 2); lab('C', 3, 3);
      if (H.auxBytes.length) lab('AUX', 4, 7);
      label(ctx, `PC ${H.ins.pc}`, rowX0 - 60, rowY - 8, e.ink3, { alpha: fa, align: 'right', tracking: 0.1 });
    }
    // the operands type in after the opcode
    const rest = insText.slice(name.length);
    const typed = clamp((t - T.instr - 0.35) / 0.9) * rest.length;
    const Lr = layoutText(rest, IF);
    ctx.globalAlpha = a;
    typewriter(ctx, Lr, insX0 + name.length * cwI, insY, typed, { color: e.ink2 });
    ctx.restore();
  }

  // D. the listing: the instruction among its neighbours, each with the source line it came from
  if (pd > 0) {
    const LF = TY.dark.prompt, cwL = measure('0', LF), lhL = 44;
    const rows = H.listing;
    const at = rows.indexOf(H.ins);
    const textCol = 6;
    const w = Math.max(...rows.map((r) => (textCol + r.text.length) * cwL));
    const lx = 960 - w / 2;
    const ly = 470 - at * lhL;
    const out = ease.inOut(clamp((t - T.line - 0.55) / 0.7));
    ctx.save();
    setFont(ctx, LF);
    ctx.textBaseline = 'alphabetic';
    ctx.textAlign = 'left';
    rows.forEach((r, i) => {
      const y = ly + i * lhL;
      const me = r === H.ins;
      const q = me ? 1 : ease.out(clamp((t - T.listing - 0.25 - Math.abs(i - at) * 0.08) / 0.5));
      const a = q * (1 - out);
      if (a <= 0) return;
      ctx.globalAlpha = a;
      // the -g1 line of each instruction, in a gutter
      const tagLit = me ? ease.out(clamp((t - T.line) / 0.4)) : 0;
      ctx.fillStyle = tagLit > 0 ? mix(e.ink3, e.ink, tagLit) : e.ink3;
      ctx.fillText(String(r.line).padStart(3, ' '), lx, y);
      ctx.fillStyle = me ? e.ink : e.ink2;
      if (me) {
        // the instruction shrinks into its row
        const k = lerp(IF.size / LF.size, 1, pd);
        ctx.save();
        ctx.translate(lerp(insX0, lx + textCol * cwL, pd), lerp(insY, y, pd));
        ctx.scale(k, k);
        ctx.fillText(r.text, 0, 0);
        ctx.restore();
      } else ctx.fillText(r.text, lx + textCol * cwL, y);
    });
    ctx.globalAlpha = 1 - out;
    label(ctx, 'LINE', lx, ly - lhL * 0.95, e.ink3, { alpha: ease.out(clamp((t - T.listing - 0.3) / 0.5)) * (1 - out), tracking: 0.1 });
    label(ctx, `${S.D.sample.focus_function}()`, lx + textCol * cwL, ly - lhL * 0.95, e.ink3, { alpha: ease.out(clamp((t - T.listing - 0.3) / 0.5)) * (1 - out), tracking: 0.04 });
    ctx.restore();

    // E. the line of source the instruction came from
    if (t > T.line) {
      const SF = TY.dark.line, cwS = measure('0', SF);
      const line = H.srcLine.trim();
      const tagX = lx + 1.5 * cwL, tagY = ly + at * lhL;
      const fall = ease.out(clamp((t - T.line - 0.1) / 0.45));
      const sy0 = 790, sy = lerp(sy0, 540 + metrics(SF).capHeight / 2, out);
      vline(ctx, tagX, tagY + 12, (sy0 - 60 - tagY) * fall, e.ink2, 0.7 * (1 - out));
      const typed = clamp((t - T.line - 0.35) / 0.55) * line.length;
      const sx = 960 - (line.length * cwS) / 2;
      typewriter(ctx, layoutText(line, SF), sx, sy, typed, { color: e.ink });
      label(ctx, `${S.D.sample.file.toUpperCase()}  ·  LINE ${H.ins.line}`, 960, sy - 64, e.ink2, { alpha: ease.out(clamp((t - T.line - 0.3) / 0.4)) * (1 - out), align: 'center' });
    }
  }
}

/** The scale of things: one constant-size label that steps down as the dive does. */
function drawScaleLabel(ctx, t) {
  const e = ERA.dark;
  const steps = [
    [T.dive0 + 0.2, `${formatNumber(HEX.bytes)} BYTES`],
    [T.dive1 - 0.1, '1 BYTE'],
    [T.opcode + 0.5, '1 OPCODE'],
    [T.instr + 0.3, '1 INSTRUCTION'],
    [T.line, '1 LINE'],
  ];
  const a = envelope(t, T.dive0, T.open1 + 0.2, 0.5, 0.5);
  if (a <= 0) return;
  let k = 0;
  while (k + 1 < steps.length && steps[k + 1][0] <= t) k++;
  const prev = k > 0 ? steps[k - 1][1] : steps[0][1];
  const F = TY.dark.small;
  ctx.save();
  ctx.globalAlpha *= a;
  hairline(ctx, 96, 92, 36, e.ink2, 0.8);
  rollText(ctx, prev.padEnd(16), steps[k][1].padEnd(16), 96, 124, F, t, steps[k][0], { color: e.ink2, dur: 0.4, stagger: 0.02 });
  ctx.restore();
}

export function drawOpen(ctx, t) {
  surface(ctx, ERA.dark.surface);
  drawDump(ctx, t);
  drawByteStory(ctx, t);
  drawScaleLabel(ctx, t);
}

// -------------------------------------------------------------------------------------- medal

function medalCam(t) {
  const { cw, lh } = S.CM;
  const M = MED;
  // close: the line at the size the source line had, centred where it was
  const SF = TY.dark.line;
  const sClose = SF.size / CODE.size;
  const line = HEX.srcLine.trim();
  const lineW = line.length * measure('0', SF);
  const close = { s: sClose, x: 960 - lineW / 2 - M.indentCols * cw * sClose, y: 540 - (M.line + 0.5) * lh * sClose };
  // wide: medal's whole function as a landscape, a little left of centre
  const sWide = 0.84;
  const wide = { s: sWide, x: 1000 - (M.L.cols * cw * sWide) / 2, y: 540 - ((M.L.lineCount) * lh * sWide) / 2 };
  // the goto: push in on the jump into the else block
  const g = M.goto;
  const sGo = 1.5;
  const go = g ? { s: sGo, x: 960 - ((g.c0 + g.endCol) / 2) * cw * sGo, y: 540 - ((g.from + g.to + 1) / 2) * lh * sGo } : wide;
  let cam;
  if (t < T.pull0) cam = close;
  else if (t < T.gotoPush) cam = zoomCam(close, wide, ease.inOut(clamp((t - T.pull0) / (T.pull1 - T.pull0))));
  else cam = zoomCam(wide, go, ease.inOut(clamp((t - T.gotoPush) / 2.0)));
  return { close, wide, cam };
}

export function drawMedal(ctx, t) {
  const e = ERA.dark, M = MED;
  surface(ctx, e.surface);
  const { close, cam: c0 } = medalCam(t);
  const cam = t > T.pull1 && t < T.gotoPush ? drift(c0, t, T.pull1, T.gotoPush, { zoom: 0.04, dx: -30, dy: -12 }) : c0;
  const pal = S.PAL.dark;
  const lh = S.CM.lh;
  // the title dims the code behind it; the code comes back for the goto
  const dim = 1 - 0.8 * envelope(t, T.medalTitle - 0.2, T.gotoPush + 0.2, 0.6, 0.8);
  // 1. the one line morphs from the source's names to medal's
  const pm = clamp((t - T.lineMorph0) / (T.lineMorph1 - T.lineMorph0));
  if (t < T.pull0 + 0.05) {
    drawMorph(ctx, M.plan, pm, { size: CODE.size, lineHeight: CODE.lineHeight, palette: pal, cameraA: { ...close, y: close.y + M.line * lh * close.s }, cameraB: { ...close, y: close.y + M.line * lh * close.s }, blur: 5, inserted: 'base' });
    return;
  }
  // 2. medal's whole function grows around it as the camera pulls back
  const reveal0 = T.pull0 + 0.1;
  const lineAlpha = (l) => (l === M.line ? 1 : ease.out(clamp((t - reveal0 - Math.abs(l - M.line) * 0.035) / 0.6))) * dim;
  codeAt(ctx, M.L, cam, pal, { clip: { x: 0, y: 0, w: 1920, h: 1080 }, lineAlpha });
  // the goto into an else block, drawn as it jumps
  if (M.goto && t > T.gotoPush - 0.4) {
    const a = ease.out(clamp((t - T.gotoPush) / 0.8));
    drawGotoArc(ctx, M.L, M.goto, cam, t, e.ink, a);
  }
  // 3. the title, full frame, then the people
  const [ox, oy] = driftOffset(t, T.pull1, T.gotoPush, { depth: 0.3 });
  ctx.save();
  ctx.translate(ox, oy);
  const head = S.ST.medal.headline;
  const words = head.split(' ');
  const half = Math.ceil(words.length / 2);
  const L = layoutText(words.slice(0, half).join(' ') + '\n' + words.slice(half).join(' '), TY.dark.hero, { lineHeight: 0.98, around: AROUND });
  reveal(ctx, L, 960, 470, t, { unit: 'word', start: T.medalTitle, stagger: 0.12, dur: 1.1, color: e.ink, tracking: TRACK.hero, align: 'center', out: { start: T.credit - 0.5, stagger: 0.05, dur: 0.6 } });
  const first = S.D.medal_history.first_commit;
  const out = { start: T.gotoPush - 0.5, stagger: 0.03, dur: 0.5 };
  reveal(ctx, layoutText(`FIRST COMMIT  ·  ${longDate(first.date).toUpperCase()}`, TY.dark.label), 960, 400, t, { unit: 'line', start: T.credit, dur: 0.8, color: e.ink2, tracking: 0.14, align: 'center', out });
  reveal(ctx, layoutText('A Luau decompiler by', TY.dark.sub), 960, 480, t, { unit: 'line', start: T.credit + 0.25, dur: 0.9, color: e.ink2, align: 'center', out });
  reveal(ctx, layoutText(S.D.medal_history.authors.join(' and '), TY.dark.authors), 960, 534, t, { unit: 'line', start: T.credit + 0.4, dur: 0.9, color: e.ink, align: 'center', out });
  const url = (S.D.medal_history.fork_repo?.html_url || '').replace(/^https?:\/\//, '');
  if (url) reveal(ctx, layoutText(url, TY.dark.url), 960, 600, t, { unit: 'line', start: T.credit + 0.8, dur: 0.9, color: e.ink2, align: 'center', out });
  ctx.restore();
}

/** One goto as a dashed arc in the indentation, flowing toward its label. */
function drawGotoArc(ctx, L, g, cam, t, color, a) {
  if (a <= 0) return;
  const { cw, lh } = S.CM;
  const y = (l) => cam.y + (l + 0.5) * lh * cam.s;
  const x = (c) => cam.x + c * cw * cam.s;
  const lw = 2.2;
  const xa = x(g.c0) - 12, xb = x(g.c1) - 12;
  const xm = Math.min(xa, xb) - 90 * cam.s;
  ctx.save();
  ctx.globalAlpha *= a;
  ctx.strokeStyle = color;
  ctx.lineWidth = lw;
  ctx.lineCap = 'round';
  ctx.setLineDash([lw * 3, lw * 3]);
  ctx.lineDashOffset = -t * 30;
  ctx.beginPath();
  ctx.moveTo(xa, y(g.from));
  ctx.bezierCurveTo(xm, y(g.from), xm, y(g.to), xb, y(g.to));
  ctx.stroke();
  ctx.setLineDash([]);
  const s = 7;
  ctx.beginPath();
  ctx.moveTo(xb - s, y(g.to) - s * 0.75);
  ctx.lineTo(xb, y(g.to));
  ctx.lineTo(xb - s, y(g.to) + s * 0.75);
  ctx.stroke();
  ctx.restore();
}

// --------------------------------------------------------------------------------------- the date

/** medal's first commit rolls forward to Tovek's first beta, to the minute (UTC). */
export function dateFrom() { return stamp(S.D.medal_history.first_commit.date); }
export function dateTo() { return stamp(S.ST['v0.1.0-beta'].published_at || S.ST['v0.1.0-beta'].date + 'T00:00:00Z'); }

export function drawDate(ctx, t, hand = null) {
  const e = ERA.dark;
  surface(ctx, e.surface);
  const F = TY.dark.stamp;
  const a = ease.out(clamp((t - T.date0) / 0.35));
  const from = dateFrom(), to = dateTo();
  const roll0 = T.date0 + 0.55, roll1 = 1.7;
  const cw = measure('0', F);
  const w = to.length * cw;
  // the stamp flies to the terminal's date line as the terminal opens
  const fly = ease.inOut(clamp((t - (T.date1 - 0.75)) / 0.75));
  const target = hand || { x: 120, y: 384, size: TY.dark.mdate.size };
  const k = lerp(1, target.size / F.size, fly);
  const x = lerp(960 - w / 2, target.x, fly), y = lerp(600, target.y, fly);
  ctx.save();
  ctx.globalAlpha *= a;
  ctx.translate(x, y);
  ctx.scale(k, k);
  ticker(ctx, from, to, 0, 0, F, t, roll0, roll1, { color: e.ink, turns: 2 });
  ctx.restore();
  const la = a * (1 - fly);
  const before = 'MEDAL  ·  FIRST COMMIT', after = `TOVEK ${S.ST['v0.1.0-beta'].name.toUpperCase()}  ·  PUBLISHED  ·  UTC`;
  if (la > 0) {
    ctx.save();
    ctx.globalAlpha *= la;
    const lf = TY.dark.stampLab;
    const n = Math.max(before.length, after.length);
    const lx = 960 - (n * measure('0', lf)) / 2;
    rollText(ctx, before.padEnd(n), after.padEnd(n), lx, 420, lf, t, roll0 + roll1 * 0.7, { color: e.ink2, dur: 0.45, stagger: 0.015 });
    ctx.restore();
  }
}
