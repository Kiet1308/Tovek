// Luau on the canvas: a tokenizer, a cached monospace layout, and a renderer with near-greyscale
// tones. The accent is reserved for highlighted tokens (what Tovek recovered), never for syntax.
//
//   const L = layoutCode(src);                      // cached per source text
//   drawCode(ctx, L, { x: 160, y: 140, size: 24, palette: 'night', highlight: L.find('success') });

import { measure, metrics, font as makeFont, setFont } from './text.js';
import { clamp } from './tween.js';
import { mix } from './color.js';

const KEYWORDS = new Set([
  'and', 'break', 'do', 'else', 'elseif', 'end', 'for', 'function', 'if', 'in', 'local', 'not', 'or',
  'repeat', 'return', 'then', 'until', 'while', 'continue', 'export', 'type', 'typeof',
]);
const CONSTANTS = new Set(['true', 'false', 'nil']);
const GLOBALS = new Set([
  'game', 'workspace', 'script', 'require', 'self', 'Enum', 'math', 'string', 'table', 'task', 'coroutine',
  'debug', 'buffer', 'bit32', 'utf8', 'os', 'setmetatable', 'getmetatable', 'pairs', 'ipairs', 'next',
  'print', 'warn', 'error', 'assert', 'pcall', 'xpcall', 'select', 'tostring', 'tonumber', 'rawget',
  'rawset', 'rawequal', 'unpack', 'Instance', 'Vector3', 'Vector2', 'CFrame', 'Color3', 'UDim2', 'UDim',
  'TweenInfo', 'NumberSequence', 'ColorSequence', 'Ray', 'RaycastParams', 'tick', 'time', 'wait', 'spawn', 'delay',
]);
const OPERATORS = ['...', '//=', '..=', '==', '~=', '<=', '>=', '+=', '-=', '*=', '/=', '%=', '^=', '//', '..', '->', '::'];

/** Token kinds and what they are for. Palettes map each kind to a tone. */
export const KINDS = ['kw', 'const', 'num', 'str', 'com', 'id', 'fn', 'prop', 'glob', 'op', 'punct'];

function longBracket(src, i) {
  // at '[': returns the level of a long bracket ([[, [=[, ...) or -1
  let j = i + 1, level = 0;
  while (src[j] === '=') { level++; j++; }
  return src[j] === '[' ? level : -1;
}

/**
 * Tokenize Luau. Returns `[{ k, text, offset }]` without whitespace. Multi-line tokens (long
 * strings, block comments) stay one token. Interpolated strings are one string token.
 */
export function tokenizeLuau(src) {
  const out = [];
  const n = src.length;
  let i = 0;
  const push = (k, s, e) => out.push({ k, text: src.slice(s, e), offset: s });
  while (i < n) {
    const c = src[i];
    if (c === ' ' || c === '\t' || c === '\r' || c === '\n') { i++; continue; }
    const s = i;
    if (c === '-' && src[i + 1] === '-') {
      if (src[i + 2] === '[') {
        const lvl = longBracket(src, i + 2);
        if (lvl >= 0) {
          const close = ']' + '='.repeat(lvl) + ']';
          const e = src.indexOf(close, i + 4 + lvl);
          i = e < 0 ? n : e + close.length;
          push('com', s, i);
          continue;
        }
      }
      while (i < n && src[i] !== '\n') i++;
      push('com', s, i);
      continue;
    }
    if (c === '"' || c === "'" || c === '`') {
      i++;
      while (i < n && src[i] !== c && src[i] !== '\n') i += src[i] === '\\' ? 2 : 1;
      i = Math.min(n, i + 1);
      push('str', s, i);
      continue;
    }
    if (c === '[') {
      const lvl = longBracket(src, i);
      if (lvl >= 0) {
        const close = ']' + '='.repeat(lvl) + ']';
        const e = src.indexOf(close, i + 2 + lvl);
        i = e < 0 ? n : e + close.length;
        push('str', s, i);
        continue;
      }
    }
    if ((c >= '0' && c <= '9') || (c === '.' && src[i + 1] >= '0' && src[i + 1] <= '9')) {
      if (c === '0' && (src[i + 1] === 'x' || src[i + 1] === 'X' || src[i + 1] === 'b' || src[i + 1] === 'B')) {
        i += 2;
        while (i < n && /[0-9a-fA-F_]/.test(src[i])) i++;
      } else {
        while (i < n && /[0-9_.]/.test(src[i])) i++;
        if (src[i] === 'e' || src[i] === 'E') {
          i++;
          if (src[i] === '+' || src[i] === '-') i++;
          while (i < n && /[0-9_]/.test(src[i])) i++;
        }
      }
      push('num', s, i);
      continue;
    }
    if (/[A-Za-z_]/.test(c)) {
      while (i < n && /[A-Za-z0-9_]/.test(src[i])) i++;
      const w = src.slice(s, i);
      push(KEYWORDS.has(w) ? 'kw' : CONSTANTS.has(w) ? 'const' : 'id', s, i);
      continue;
    }
    let op = null;
    for (const o of OPERATORS) if (src.startsWith(o, i)) { op = o; break; }
    if (op) { i += op.length; push('op', s, i); continue; }
    i++;
    push('(){}[],;.:'.includes(c) ? 'punct' : 'op', s, i);
  }
  // refine identifiers: field after . or :, call name before ( { or a string, function names
  for (let k = 0; k < out.length; k++) {
    const tk = out[k];
    if (tk.k !== 'id') continue;
    const prev = out[k - 1], next = out[k + 1];
    if (prev && prev.k === 'punct' && (prev.text === '.' || prev.text === ':')) tk.k = 'prop';
    else if (GLOBALS.has(tk.text)) tk.k = 'glob';
    if (next && ((next.k === 'punct' && (next.text === '(' || next.text === '{')) || next.k === 'str')) {
      if (tk.k !== 'glob') tk.k = 'fn';
    }
    if (prev && prev.k === 'kw' && prev.text === 'function') tk.k = 'fn';
  }
  return out;
}

const layoutCache = new Map();

/**
 * Lay out Luau source on a character grid. Cached per (source, tabSize).
 * Returns `{ src, tokens, lineCount, cols, lines }`, where each token has
 * `parts: [{ line, col, text }]` (one per source line it spans) plus `line`/`col` of its start.
 * Layout is in grid units; renderers multiply by the cell width and line height.
 */
export function layoutCode(src, { tabSize = 4 } = {}) {
  const key = tabSize + '\u0000' + src;
  const hit = layoutCache.get(key);
  if (hit) return hit;
  const text = src.replace(/\r\n?/g, '\n');
  const tokens = tokenizeLuau(text);
  // column of each offset, with tabs expanded
  const lineStarts = [0];
  for (let i = 0; i < text.length; i++) if (text[i] === '\n') lineStarts.push(i + 1);
  const lines = lineStarts.map((s, li) => text.slice(s, li + 1 < lineStarts.length ? lineStarts[li + 1] - 1 : text.length));
  let li = 0;
  const colOf = (line, off) => {
    let col = 0;
    for (let i = lineStarts[line]; i < off; i++) col = text[i] === '\t' ? col + tabSize - (col % tabSize) : col + 1;
    return col;
  };
  const lineCols = new Int32Array(lines.length);
  let cols = 0;
  for (let l = 0; l < lines.length; l++) {
    lineCols[l] = colOf(l, lineStarts[l] + lines[l].length);
    cols = Math.max(cols, lineCols[l]);
  }
  for (let k = 0; k < tokens.length; k++) {
    const tk = tokens[k];
    tk.i = k;
    while (li + 1 < lineStarts.length && lineStarts[li + 1] <= tk.offset) li++;
    const pieces = tk.text.split('\n');
    tk.parts = [];
    let off = tk.offset;
    for (let p = 0; p < pieces.length; p++) {
      const line = li + p;
      if (pieces[p].length) tk.parts.push({ line, offset: off, col: colOf(line, off), text: pieces[p].replace(/\t/g, ' '.repeat(tabSize)) });
      off += pieces[p].length + 1;
    }
    tk.line = li;
    tk.col = tk.parts.length ? tk.parts[0].col : 0;
    tk.lastLine = li + pieces.length - 1;
  }
  const byKind = new Map();
  for (const tk of tokens) {
    if (!byKind.has(tk.k)) byKind.set(tk.k, []);
    byKind.get(tk.k).push(tk.i);
  }
  const L = {
    src: text, tokens, lines, lineCols, lineCount: lines.length, cols, tabSize, byKind,
    /** Widest line (in cells) among lines [from, to]. */
    colsIn(from, to) {
      let c = 0;
      for (let l = Math.max(0, from); l <= Math.min(lines.length - 1, to); l++) c = Math.max(c, lineCols[l]);
      return c;
    },
    /** First line (0-based) whose text matches `re`, or -1. */
    lineOf(re, from = 0) {
      for (let l = from; l < lines.length; l++) if (re.test(lines[l])) return l;
      return -1;
    },
    /** Indices of tokens whose text equals `text` (optionally only on `line`). */
    find(textOrRe, line) {
      const out = new Set();
      for (const tk of tokens) {
        if (line != null && tk.line !== line) continue;
        if (typeof textOrRe === 'string' ? tk.text === textOrRe : textOrRe.test(tk.text)) out.add(tk.i);
      }
      return out;
    },
    /** Indices of every token on lines [from, to] (0-based, inclusive). */
    lineTokens(from, to = from) {
      const out = new Set();
      for (const tk of tokens) if (tk.line >= from && tk.line <= to) out.add(tk.i);
      return out;
    },
  };
  if (layoutCache.size > 400) layoutCache.clear();
  layoutCache.set(key, L);
  return L;
}

/**
 * Tones. Identifiers are the brightest (names are what a reader came for); keywords and
 * punctuation step back. `accent` is only for highlighted tokens.
 */
export const codePalettes = {
  night: {
    id: '#ebe8e0', fn: '#f2f0eb', glob: '#dedbd2', prop: '#cfccc3', kw: '#9b988e', const: '#b8b5ab',
    num: '#c7c3b8', str: '#a9a598', com: '#6d6b63', op: '#8b8980', punct: '#76746c',
    accent: '#ff4d1a', wash: 'rgba(255,77,26,0.16)', gutter: '#4b4a45', base: '#0d0d0c',
  },
  paper: {
    id: '#121210', fn: '#121210', glob: '#24231f', prop: '#2e2d29', kw: '#6c6a62', const: '#55534c',
    num: '#3a3934', str: '#5c584f', com: '#98958c', op: '#86847b', punct: '#9a978e',
    accent: '#d63a0c', wash: 'rgba(255,77,26,0.14)', gutter: '#b3b0a7', base: '#f2f0eb',
  },
};
const paletteOf = (p) => (typeof p === 'string' ? codePalettes[p] : p) || codePalettes.night;

/** Font spec for code at `size` px. */
export const codeFont = (size, weight = 400) => makeFont(size, { family: 'mono', weight });

/** Grid metrics for code at `size`: `{ cw, lh, baseline }` (cell width, line height, baseline offset in a line). */
export function codeMetrics(size, lineHeight = 1.55, weight = 400) {
  const f = codeFont(size, weight);
  const cw = measure('0', f);
  const lh = size * lineHeight;
  const m = metrics(f);
  return { f, cw, lh, baseline: lh / 2 + (m.ascent - m.descent) / 2 };
}

/** Width and height in px of a layout at `size`. */
export function codeSize(L, size = 24, lineHeight = 1.55) {
  const { cw, lh } = codeMetrics(size, lineHeight);
  return { w: L.cols * cw, h: L.lineCount * lh };
}

/**
 * Draw a code layout. `x, y` is the top-left of the first line box.
 *
 * Options:
 *   size (24), lineHeight (1.55), weight (400), palette ('night' | 'paper' | object)
 *   highlight     Set of token indices (or a function (tok) => bool) drawn in the accent
 *   highlightMix  0..1, how far highlighted tokens are pulled to the accent (animate to fade in/out)
 *   wash          also draw a soft accent wash behind highlighted tokens
 *   chars         type-on: show only source characters [0, chars) (fractional fades the next one)
 *   lines         [first, last] window of lines to draw (others are skipped, not just clipped)
 *   lineAlpha     (line) => alpha, e.g. to dim everything except a focus line
 *   gutter        draw line numbers in a gutter this many cells wide (numbers start at `firstLine`)
 */
export function drawCode(ctx, L, o = {}) {
  const size = o.size ?? 24, lineHeight = o.lineHeight ?? 1.55;
  const pal = paletteOf(o.palette);
  const { f, cw, lh, baseline } = codeMetrics(size, lineHeight, o.weight ?? 400);
  const x0 = (o.x ?? 0) + (o.gutter ? o.gutter * cw : 0), y0 = o.y ?? 0;
  const hl = o.highlight;
  const isHl = !hl ? () => false : typeof hl === 'function' ? hl : (tk) => hl.has(tk.i);
  const mixAmt = o.highlightMix ?? 1;
  const chars = o.chars ?? Infinity;
  const [first, last] = o.lines || [0, L.lineCount - 1];
  const lineAlpha = o.lineAlpha;

  ctx.save();
  setFont(ctx, f);
  ctx.textBaseline = 'alphabetic';
  ctx.textAlign = 'left';
  const base = ctx.globalAlpha;

  if (o.gutter) {
    ctx.fillStyle = pal.gutter;
    ctx.textAlign = 'right';
    for (let l = first; l <= last; l++) {
      const a = lineAlpha ? lineAlpha(l) : 1;
      if (a <= 0) continue;
      ctx.globalAlpha = base * a;
      ctx.fillText(String((o.firstLine ?? 1) + l), x0 - cw * 1.5, y0 + (l - first) * lh + baseline);
    }
    ctx.textAlign = 'left';
  }

  const drawTok = (tk, color) => {
    for (const part of tk.parts) {
      if (part.line < first || part.line > last) continue;
      const a = lineAlpha ? lineAlpha(part.line) : 1;
      if (a <= 0) continue;
      let text = part.text;
      let fadeLast = 0;
      if (chars !== Infinity) {
        const visible = chars - part.offset;
        if (visible <= 0) continue;
        if (visible < text.length) {
          fadeLast = visible - Math.floor(visible);
          text = text.slice(0, Math.floor(visible));
        }
      }
      const px = x0 + part.col * cw, py = y0 + (part.line - first) * lh + baseline;
      ctx.globalAlpha = base * a;
      ctx.fillStyle = color;
      if (text) ctx.fillText(text, px, py);
      if (fadeLast > 0) {
        ctx.globalAlpha = base * a * fadeLast;
        ctx.fillText(part.text[text.length], px + text.length * cw, py);
      }
    }
  };

  // washes first, so text sits on top
  if (hl && o.wash && mixAmt > 0) {
    ctx.fillStyle = pal.wash;
    for (const tk of L.tokens) {
      if (!isHl(tk)) continue;
      for (const part of tk.parts) {
        if (part.line < first || part.line > last) continue;
        ctx.globalAlpha = base * mixAmt;
        roundRect(ctx, x0 + part.col * cw - cw * 0.3, y0 + (part.line - first) * lh + lh * 0.12, part.text.length * cw + cw * 0.6, lh * 0.76, lh * 0.14);
      }
    }
  }
  // one pass per tone keeps fillStyle changes to a dozen per frame
  for (const [k, idxs] of L.byKind) {
    const color = pal[k] || pal.id;
    const hlColor = mixAmt >= 1 ? pal.accent : mix(color.startsWith('#') ? color : '#ffffff', pal.accent, mixAmt);
    for (const i of idxs) {
      const tk = L.tokens[i];
      if (tk.lastLine < first || tk.line > last) continue;
      drawTok(tk, isHl(tk) && mixAmt > 0 ? hlColor : color);
    }
  }
  ctx.restore();
}

export function roundRect(ctx, x, y, w, h, r) {
  ctx.beginPath();
  if (ctx.roundRect) ctx.roundRect(x, y, w, h, r);
  else ctx.rect(x, y, w, h);
  ctx.fill();
}

/** Number of source characters in a layout (the `chars` value at which typing completes). */
export const codeLength = (L) => L.src.length;

/** `chars` for a type-on that takes `dur` seconds from `start`, at a steady rate. */
export const typeChars = (L, t, start, dur) => clamp((t - start) / dur) * L.src.length;
