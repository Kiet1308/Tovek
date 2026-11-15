// Tovek landing.
// The page is built by scripts/build_landing.mjs; everything it shows is already in the HTML. This module makes it
// work: the hero's lens (one method as medal, Tovek and the source printed it, tied by ribbons to the same bytecode),
// the reader (point at a line, see its instructions), the 64-bit check, every release of one script, and the
// visitor's own file read by the in-browser build of Tovek.

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
const reduced = matchMedia('(prefers-reduced-motion: reduce)');
const phone = matchMedia('(max-width: 759px)');
const here = (p) => new URL(p, import.meta.url).href;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const clamp = (x, a, b) => Math.max(a, Math.min(b, x));
const easeInOut = (x) => (x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2);
const easeOut = (x) => 1 - Math.pow(1 - x, 4);
const pageData = JSON.parse($('#page-data').textContent);
window.__landing = true;

// ------------------------------------------------------------------ tokens (same rules as the build script)

const KEYWORDS = new Set(['and', 'break', 'continue', 'do', 'else', 'elseif', 'end', 'false', 'for', 'function', 'if',
  'in', 'local', 'nil', 'not', 'or', 'repeat', 'return', 'then', 'true', 'until', 'while', 'export', 'type']);
const TOKEN = /(--.*$)|("(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')|(`)|(0[xX][0-9a-fA-F_]+|0[bB][01_]+|(?:\d[\d_]*\.?[\d_]*|\.\d[\d_]*)(?:[eE][+-]?\d+)?)|([A-Za-z_][A-Za-z0-9_]*)|(\.\.\.|\.\.=?|[=~<>]=|\/\/=?|[-+*/%^#&|]=?|::|[()[\]{};:,.=<>])|(\s+)|(.)/y;
const RECOVERED = /equivalent calls? inferred|inferred (?:equivalent )?call/;
const GENERATED = /^(?:[vp]_u_\d+|[vp]\d*)$/;

function tokenize(line) {
  const out = [];
  let pos = 0;
  let fname = 0;
  while (pos < line.length) {
    if (line[pos] === '`') {
      let j = pos + 1;
      let buf = '`';
      while (j < line.length) {
        const ch = line[j];
        if (ch === '\\' && j + 1 < line.length) { buf += line.slice(j, j + 2); j += 2; continue; }
        if (ch === '{') {
          out.push(['s', buf + '{']);
          let k = j + 1;
          let depth = 1;
          while (k < line.length && depth) { if (line[k] === '{') depth++; else if (line[k] === '}') depth--; k++; }
          out.push(...tokenize(line.slice(j + 1, k - 1)));
          buf = '}';
          j = k;
          continue;
        }
        buf += ch;
        j++;
        if (ch === '`') break;
      }
      out.push(['s', buf]);
      pos = j;
      continue;
    }
    TOKEN.lastIndex = pos;
    const m = TOKEN.exec(line);
    let kind;
    const text = m[0];
    if (m[1] !== undefined) kind = RECOVERED.test(text) ? 'r' : 'c';
    else if (m[2] !== undefined) kind = 's';
    else if (m[4] !== undefined) kind = 'n';
    else if (m[5] !== undefined) kind = KEYWORDS.has(text) ? 'k' : (fname === 1 ? 'f' : (GENERATED.test(text) ? 'g' : 'i'));
    else if (m[6] !== undefined) kind = 'o';
    else if (m[7] !== undefined) kind = 'x';
    else kind = 'o';
    if (kind === 'x') { /* keep state */ }
    else if (kind === 'k') fname = text === 'function' ? 1 : 0;
    else if (kind === 'f') fname = 2;
    else if (fname === 2 && (text === '.' || text === ':')) fname = 1;
    else fname = 0;
    out.push([kind, text]);
    pos = TOKEN.lastIndex;
  }
  return out;
}

const escapeHtml = (s) => s.replace(/[&<>]/g, (c) => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : '&gt;'));
const BREAK_AFTER = new Set([':', '.', '(']);
function tokensHtml(tokens) {
  let h = '';
  for (const [kind, text] of tokens) {
    h += kind === 'i' || kind === 'x' ? escapeHtml(text) : `<span class="${kind}">${escapeHtml(text)}</span>`;
    if (kind === 'o' && BREAK_AFTER.has(text)) h += '<wbr>';
  }
  return h;
}
function lineHtml(text, n, highlight = true, marks = null) {
  const ind = text.length - text.replace(/^\t+/, '').length;
  const body = text.slice(ind);
  let t = '';
  if (body) t = !highlight ? escapeHtml(body) : marks && marks.length ? markedHtml(body, ind, marks) : tokensHtml(tokenize(body));
  // No band here: a band is the colour of the line a row's instructions carry, and these printings come without their
  // line table. Only the words are marked: a call written back, and the helper it came from.
  return `<div class="ln${body ? '' : ' blank'}" data-n="${n}" style="--i:${ind}"><span class="t">${t}</span></div>`;
}
// like tokensHtml, with [start, end) column ranges of the raw line wrapped in <mark>
function markedHtml(body, ind, marks) {
  const cuts = new Set();
  for (const m of marks) { cuts.add(m.start); cuts.add(m.end); }
  const tokens = [];
  let col = ind;
  for (const [kind, text] of tokenize(body)) {
    const b = col + text.length;
    let a = col;
    for (const c of [...[...cuts].filter((x) => x > col && x < b).sort((x, y) => x - y), b]) { tokens.push({ kind, text: text.slice(a - col, c - col), a, b: c }); a = c; }
    col = b;
  }
  let h = '';
  for (const tk of tokens) {
    for (const m of marks) if (m.start === tk.a) h += `<mark class="${m.cls}">`;
    h += tk.kind === 'i' || tk.kind === 'x' ? escapeHtml(tk.text) : `<span class="${tk.kind}">${escapeHtml(tk.text)}</span>`;
    if (tk.kind === 'o' && BREAK_AFTER.has(tk.text)) h += '<wbr>';
    for (const m of marks) if (m.end === tk.b) h += '</mark>';
  }
  return h;
}

// The helpers a printing wrote calls back from: each takes a highlighter, in the order they are defined (so Beacon's
// three get the same colours here as in the listing the page ships with). A helper's definition line says so itself.
const HL = ['hl-pink', 'hl-green', 'hl-yellow', 'hl-pink alt', 'hl-green alt', 'hl-yellow alt'];
function helpersOf(source) {
  const map = new Map();
  for (const m of source.matchAll(/^\s*(?:local\s+)?function\s+([\w.:]+)\s*\(.*--\s*\d+ equivalent calls? inferred from this helper/gm)) if (!map.has(m[1])) map.set(m[1], HL[map.size % HL.length]);
  return [...map].map(([name, cls]) => ({ name, cls, re: new RegExp(`(?<![\\w.:])${name.replace(/[.$]/g, '\\$&')}\\(`, 'g') }));
}
// on a line that says a call was inferred: where the helper's name (its definition) or its calls are
function callMarks(text, helpers) {
  if (!helpers.length || !RECOVERED.test(text)) return null;
  const codeText = text.replace(/--.*$/, (c) => ' '.repeat(c.length));
  const marks = [];
  const def = /^\s*(?:local\s+)?function\s+([\w.:]+)\s*\(/.exec(codeText);
  if (def) {
    const h = helpers.find((x) => x.name === def[1]);
    const at = codeText.indexOf(def[1]);
    if (h) marks.push({ start: at, end: at + def[1].length, cls: `call ${h.cls}` });
    return marks;
  }
  for (const h of helpers) {
    h.re.lastIndex = 0;
    let m;
    while ((m = h.re.exec(codeText))) {
      let depth = 0;
      let end = -1;
      for (let k = m.index + h.name.length; k < codeText.length; k++) {
        if (codeText[k] === '(') depth++;
        else if (codeText[k] === ')') { depth--; if (!depth) { end = k + 1; break; } }
      }
      if (end > 0 && !marks.some((x) => m.index < x.end && x.start < end)) marks.push({ start: m.index, end, cls: `call ${h.cls}` });
    }
  }
  return marks;
}

// ------------------------------------------------------------------ the morph: shared words glide, the rest fade

const tokenKey = (text) => { const t = text.trim().replace(/^\[|\]$/g, '').replace(/^['"`]|['"`]$/g, '').replace(/^\}|\{$/g, ''); return t || null; };
const styleCache = new Map();
function styleOf(el) {
  const key = `${el.className}|${el.parentElement.className}|${el.closest('.ln')?.className || ''}`;
  let st = styleCache.get(key);
  if (!st) {
    const cs = getComputedStyle(el);
    st = { color: cs.color, font: `${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`, ls: cs.letterSpacing === 'normal' ? '0px' : cs.letterSpacing };
    styleCache.set(key, st);
  }
  return st;
}

// the words of the given lines that are inside `clip` (a viewport rect), read straight from the layout
function tokensIn(lines, clip) {
  const out = [];
  const range = document.createRange();
  for (const line of lines) {
    const lr = line.getBoundingClientRect();
    if (lr.bottom < clip.top || lr.top > clip.bottom) continue;
    const t = line.querySelector('.t');
    if (!t) continue;
    const walker = document.createTreeWalker(t, NodeFilter.SHOW_TEXT);
    while (walker.nextNode()) {
      const node = walker.currentNode;
      const parent = node.parentElement;
      const re = /\S+/g;
      let m;
      while ((m = re.exec(node.textContent))) {
        range.setStart(node, m.index);
        range.setEnd(node, m.index + m[0].length);
        const rects = range.getClientRects();
        if (rects.length !== 1) continue;
        const r = rects[0];
        if (r.bottom < clip.top || r.top > clip.bottom) continue;
        const st = styleOf(parent);
        out.push({ text: m[0], key: tokenKey(m[0]), x: r.left, y: r.top, h: r.height, color: st.color, font: st.font, ls: st.ls });
      }
    }
  }
  return out;
}

function lcsPairs(a, b) {
  const n = a.length;
  const m = b.length;
  if (!n || !m) return [];
  const w = m + 1;
  const dp = new Uint16Array((n + 1) * w);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i * w + j] = a[i].key && a[i].key === b[j].key ? dp[(i + 1) * w + j + 1] + 1 : Math.max(dp[(i + 1) * w + j], dp[i * w + j + 1]);
    }
  }
  const pairs = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i].key && a[i].key === b[j].key) { pairs.push([i, j]); i++; j++; }
    else if (dp[(i + 1) * w + j] >= dp[i * w + j + 1]) i++;
    else j++;
  }
  return pairs;
}

const clipOf = (host) => {
  const r = host.getBoundingClientRect();
  return { left: r.left + host.clientLeft, top: r.top + host.clientTop, right: r.left + host.clientLeft + host.clientWidth, bottom: r.top + host.clientTop + host.clientHeight };
};

// One canvas over the visible part of `host` draws the change; the new text underneath shows when it leaves.
function playMorph(host, before, now, hide, dur = 760) {
  const clip = clipOf(host);
  const W = clip.right - clip.left;
  const H = clip.bottom - clip.top;
  if (W < 20 || H < 20) return Promise.resolve();
  const pairs = lcsPairs(before, now);
  const dpr = Math.min(devicePixelRatio || 1, 2);
  const cv = document.createElement('canvas');
  cv.className = 'morph-canvas';
  cv.width = Math.round(W * dpr);
  cv.height = Math.round(H * dpr);
  cv.style.cssText = `left:${host.scrollLeft}px;top:${host.scrollTop}px;width:${W}px;height:${H}px`;
  const ctx = cv.getContext('2d');
  ctx.scale(dpr, dpr);
  ctx.textBaseline = 'alphabetic';
  const descents = new Map();
  const setFont = (tok) => { ctx.font = tok.font; if ('letterSpacing' in ctx) ctx.letterSpacing = tok.ls; };
  const descentOf = (tok) => {
    let d = descents.get(tok.font);
    if (d === undefined) { setFont(tok); d = ctx.measureText('Hg').fontBoundingBoxDescent || 3; descents.set(tok.font, d); }
    return d;
  };
  const ox = clip.left;
  const oy = clip.top;
  const matchedOld = new Set(pairs.map(([i]) => i));
  const matchedNew = new Set(pairs.map(([, j]) => j));
  const moves = pairs.map(([i, j]) => ({ a: before[i], b: now[j], delay: Math.min(0.14, Math.abs(now[j].y - before[i].y) / 3600) }));
  const leaving = before.filter((_, i) => !matchedOld.has(i));
  const arriving = now.filter((_, j) => !matchedNew.has(j)).map((t) => ({ t, delay: 0.24 + Math.min(0.24, Math.max(0, t.y - oy) / 2600) }));
  for (const el of hide) el.style.visibility = 'hidden';
  host.append(cv);
  const glyph = (tok, x, y, alpha) => {
    ctx.globalAlpha = alpha;
    setFont(tok);
    ctx.fillStyle = tok.color;
    ctx.fillText(tok.text, x - ox, y + tok.h - descentOf(tok) - oy);
  };
  return new Promise((resolve) => {
    const t0 = performance.now();
    const frame = (clock) => {
      const t = Math.min(1, (clock - t0) / dur);
      ctx.clearRect(0, 0, W, H);
      const fade = Math.max(0, 1 - t / 0.3);
      if (fade > 0) for (const k of leaving) glyph(k, k.x, k.y, fade);
      for (const m of moves) {
        const e = easeOut(clamp((t - m.delay) / (1 - m.delay), 0, 1));
        const x = m.a.x + (m.b.x - m.a.x) * e;
        const y = m.a.y + (m.b.y - m.a.y) * e;
        if (m.a.text === m.b.text && m.a.font === m.b.font) glyph(e < 0.5 ? m.a : m.b, x, y, 1);
        else { glyph(m.a, x, y, 1 - e); glyph(m.b, x, y, e); }
      }
      for (const n of arriving) {
        const e = easeOut(clamp((t - n.delay) / (1 - n.delay), 0, 1));
        if (e > 0) glyph(n.t, n.t.x, n.t.y + (1 - e) * 4, e);
      }
      ctx.globalAlpha = 1;
      if (t < 1) requestAnimationFrame(frame);
      else {
        for (const el of hide) el.style.visibility = '';
        cv.remove();
        resolve();
      }
    };
    requestAnimationFrame(frame);
  });
}
const textEls = (lines) => lines.map((l) => l.querySelector('.t') || l);

// ------------------------------------------------------------------ the hero: lens and ribbons

const hero = $('.hero');
const weave = $('#weave');
const readBody = $('#read-body');
const bcBody = $('#bc-body');
const lens = $('#lens');
const lensNote = $('#lens-note');
const loom = $('#loom');
const readPane = $('.pane-read', hero);
const bcPane = $('.pane-bc', hero);
const rows = $$('.bc', bcBody);
const NS = 'http://www.w3.org/2000/svg';
const paths = rows.map((row) => {
  const p = document.createElementNS(NS, 'path');
  p.setAttribute('class', row.className.replace(/\b(bc|cont|is-pasted)\b/g, '').trim());
  loom.append(p);
  return p;
});
let view = 'tovek';
let textView = 'tovek';
let lensBusy = false;
let opened = false;
const viewEl = (v) => $(`.view[data-view="${v}"]`, readBody);
const keyOf = (v) => `g${v[0]}`; // data-gt, data-gm, data-gs

// where each row's ribbon starts (its share of the printed line it belongs to) and ends (the row itself)
function geometry(v) {
  const el = viewEl(v);
  if (!el || el.hidden) return rows.map(() => null);
  const host = hero.getBoundingClientRect();
  const groups = new Map();
  for (const ln of $$('.ln[data-g]', el)) {
    const r = ln.getBoundingClientRect();
    let g = groups.get(ln.dataset.g);
    if (!g) { g = { top: r.top, bottom: r.bottom, rows: [] }; groups.set(ln.dataset.g, g); }
    g.top = Math.min(g.top, r.top);
    g.bottom = Math.max(g.bottom, r.bottom);
  }
  const k = keyOf(v);
  rows.forEach((row, i) => { const g = groups.get(row.dataset[k]); if (g) g.rows.push(i); });
  const xL = readPane.getBoundingClientRect().right - host.left;
  const xR = bcPane.getBoundingClientRect().left - host.left;
  const res = rows.map(() => null);
  if (xR - xL < 8) return res;
  for (const g of groups.values()) {
    const h = (g.bottom - g.top) / g.rows.length;
    g.rows.forEach((i, n) => {
      const r = rows[i].getBoundingClientRect();
      res[i] = { xL, xR, la: g.top + n * h - host.top, lb: g.top + (n + 1) * h - host.top, ra: r.top - host.top, rb: r.bottom - host.top };
    });
  }
  return res;
}
function pathD(g) {
  const k = (g.xR - g.xL) * 0.5;
  const e = 0.35; // neighbours overlap a hair, so no seam shows between them
  return `M${g.xL},${g.la - e}C${g.xL + k},${g.la - e} ${g.xR - k},${g.ra - e} ${g.xR},${g.ra - e}L${g.xR},${g.rb + e}C${g.xR - k},${g.rb + e} ${g.xL + k},${g.lb + e} ${g.xL},${g.lb + e}Z`;
}
function drawRibbons(geo) {
  paths.forEach((p, i) => {
    const g = geo[i];
    if (!g) { p.style.display = 'none'; return; }
    p.style.display = '';
    p.style.opacity = '';
    p.setAttribute('d', pathD(g));
  });
}
function tweenRibbons(from, to, dur) {
  return new Promise((resolve) => {
    const t0 = performance.now();
    const step = (now) => {
      const t = Math.min(1, (now - t0) / dur);
      const e = easeOut(t);
      paths.forEach((p, i) => {
        const a = from[i];
        const b = to[i];
        if (!a && !b) { p.style.display = 'none'; return; }
        p.style.display = '';
        const s = a || { ...b, la: (b.la + b.lb) / 2, lb: (b.la + b.lb) / 2 };
        const d = b || { ...a, la: (a.la + a.lb) / 2, lb: (a.la + a.lb) / 2 };
        p.setAttribute('d', pathD({ xL: d.xL, xR: d.xR, ra: d.ra, rb: d.rb, la: s.la + (d.la - s.la) * e, lb: s.lb + (d.lb - s.lb) * e }));
        p.style.opacity = !a ? e : !b ? 1 - e : '';
      });
      if (t < 1) requestAnimationFrame(step);
      else { drawRibbons(to); resolve(); }
    };
    requestAnimationFrame(step);
  });
}
// the readable pane is as tall as its longest printing, so switching never moves the page
function fitReadBody() {
  readBody.style.removeProperty('--read-h');
  let need = 0;
  for (const el of $$('.view', readBody)) {
    const was = el.hidden;
    el.hidden = false;
    const lines = $$('.ln:not(.rest)', el);
    const last = lines[lines.length - 1];
    if (last) need = Math.max(need, last.offsetTop + last.offsetHeight); // readBody is the lines' offset parent
    el.hidden = was;
  }
  const row = parseFloat(getComputedStyle(readBody).lineHeight) || 24;
  if (need) readBody.style.setProperty('--read-h', `${(Math.ceil(need / row) + 1) * row}px`); // whole rows, one to spare
}
const redraw = () => drawRibbons(geometry(textView));
// ribbons that travel furthest lie underneath; the pasted ones lie on top
function orderRibbons() {
  const geo = geometry(textView);
  const reach = (i) => (geo[i] ? Math.abs((geo[i].la + geo[i].lb) - (geo[i].ra + geo[i].rb)) : 0);
  const pasted = (i) => Number(rows[i].classList.contains('is-pasted'));
  for (const i of paths.map((_, k) => k).sort((a, b) => pasted(a) - pasted(b) || reach(b) - reach(a))) loom.append(paths[i]);
}

function markLens(v) {
  for (const b of $$('button', lens)) {
    const on = b.dataset.view === v;
    b.setAttribute('aria-checked', String(on));
    b.tabIndex = on ? 0 : -1;
  }
}

async function setView(v, dur = 760) {
  if (lensBusy || v === view || (v !== 'bytecode' && !viewEl(v))) return;
  light(null);
  if (v === 'bytecode') { view = v; weave.dataset.view = v; markLens(v); return; }
  lensBusy = true;
  const wasText = view !== 'bytecode';
  const oldEl = viewEl(textView);
  const newEl = viewEl(v);
  const animate = wasText && dur > 0 && !reduced.matches && oldEl !== newEl;
  const from = animate ? geometry(textView) : null;
  const before = animate ? tokensIn($$('.ln', oldEl), clipOf(readBody)) : [];
  oldEl.hidden = true;
  newEl.hidden = false;
  view = v;
  textView = v;
  weave.dataset.view = v;
  markLens(v);
  lensNote.innerHTML = lensNote.dataset[v] || '';
  const to = geometry(v);
  if (animate) {
    const lines = $$('.ln', newEl);
    const now = tokensIn(lines, clipOf(readBody));
    await Promise.all([playMorph(readBody, before, now, textEls(lines), dur), tweenRibbons(from, to, dur)]);
  } else drawRibbons(to);
  lensBusy = false;
}

lens.addEventListener('click', (e) => {
  const b = e.target.closest('button[data-view]');
  if (b) { opened = true; setView(b.dataset.view); }
});
lens.addEventListener('keydown', (e) => {
  if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
  const bs = $$('button', lens).filter((b) => b.offsetParent);
  const i = bs.indexOf(document.activeElement);
  const j = e.key === 'Home' ? 0 : e.key === 'End' ? bs.length - 1 : (i + (e.key === 'ArrowRight' ? 1 : bs.length - 1)) % bs.length;
  bs[j].focus();
  bs[j].click();
  e.preventDefault();
});

// pointing at a printed line or an instruction lights the whole group they belong to
function light(key) {
  hero.classList.toggle('is-focus', !!key);
  const k = keyOf(textView);
  for (const ln of $$('.ln[data-g]', viewEl(textView))) ln.classList.toggle('is-lit', !!key && ln.dataset.g === key);
  rows.forEach((row, i) => {
    const on = !!key && row.dataset[k] === key;
    row.classList.toggle('is-lit', on);
    paths[i].classList.toggle('is-lit', on);
  });
}
readBody.addEventListener('pointerover', (e) => { const ln = e.target.closest('.ln[data-g]'); if (!lensBusy) light(ln ? ln.dataset.g : null); });
bcBody.addEventListener('pointerover', (e) => { const row = e.target.closest('.bc'); if (!lensBusy && view !== 'bytecode') light(row ? row.dataset[keyOf(textView)] || null : null); });
readBody.addEventListener('pointerleave', () => light(null));
bcBody.addEventListener('pointerleave', () => light(null));
// on a phone the instructions are a strip of their colours: a tap on it opens them as rows
bcBody.addEventListener('click', () => { if (phone.matches && view !== 'bytecode') { opened = true; setView('bytecode'); } });

// first sight: the method shows as medal printed it, then becomes Tovek's
function prepareOpening() {
  const html = document.documentElement;
  const wants = html.classList.contains('opening') && !!viewEl('medal') && !phone.matches;
  html.classList.remove('opening');
  if (!wants) return false;
  viewEl('tovek').hidden = true;
  viewEl('medal').hidden = false;
  view = 'medal';
  textView = 'medal';
  weave.dataset.view = 'medal';
  markLens('medal');
  lensNote.innerHTML = lensNote.dataset.medal;
  return true;
}
async function playOpening() {
  await sleep(1500);
  if (opened) return;
  const r = readBody.getBoundingClientRect();
  const seen = r.bottom > 0 && r.top < innerHeight && !document.hidden;
  setView('tovek', seen ? 1250 : 0);
}

// ------------------------------------------------------------------ one call: the second row checks itself, bit by bit

function armBits() {
  const bits = $('.bits');
  if (!bits || reduced.matches || !('IntersectionObserver' in window)) return;
  const count = $('#bits-n span');
  bits.classList.add('is-armed');
  count.textContent = '0';
  new IntersectionObserver((entries, io) => {
    if (!entries.some((en) => en.isIntersecting)) return;
    io.disconnect();
    bits.classList.add('is-run');
    const t0 = performance.now();
    const tick = (now) => {
      const n = Math.min(64, Math.floor((now - t0) / 22) + 1);
      count.textContent = String(n);
      if (n < 64) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  }, { threshold: 0.6 }).observe(bits);
}

// ------------------------------------------------------------------ the reader

const frame = $('#frame');
const port = $('#port');
const code = $('#code');
const bcport = $('#bcport');
const bcps = $$('.bcp', bcport);
const bcTitle = $('#bc-title');
const noteEls = $$('.note', $('#note-box'));
const dots = $$('.dot', $('#dots'));
const noteAt = $('#note-at');
const segs = $$('.seg', $('#strip'));
let curP = null;
let at = -1;
let pinned = null; // the printed line the active note is about
let scrollAnim = 0;
let hovering = false;

function showPassage(pid, keepScroll = false) {
  if (!pid || pid === curP) return;
  curP = pid;
  for (const b of bcps) b.hidden = b.dataset.p !== pid;
  const b = bcps.find((x) => x.dataset.p === pid);
  bcTitle.textContent = `${b.dataset.title}, ${b.dataset.n} instruction${b.dataset.n === '1' ? '' : 's'}`;
  if (!keepScroll) bcport.scrollTop = 0;
  for (const s of segs) s.classList.toggle('is-on', s.dataset.p === pid);
}

function lightLine(n) {
  for (const el of $$('.is-lit', frame)) el.classList.remove('is-lit');
  frame.classList.remove('is-focus');
  if (n == null) return;
  const ln = $(`#L${n}`);
  if (!ln) return;
  showPassage(ln.dataset.p, true);
  const b = bcps.find((x) => x.dataset.p === curP);
  const rs = b ? $$(`.bc[data-t="${n}"]`, b) : [];
  if (!rs.length) return;
  ln.classList.add('is-lit');
  for (const r of rs) r.classList.add('is-lit');
  frame.classList.add('is-focus');
  const top = rs[0].offsetTop - bcport.offsetTop;
  const bottom = rs[rs.length - 1].offsetTop + rs[rs.length - 1].offsetHeight - bcport.offsetTop;
  const row = rowOf(bcport);
  if (top < bcport.scrollTop || bottom > bcport.scrollTop + bcport.clientHeight) bcport.scrollTop = Math.max(0, top - Math.round((bcport.clientHeight * 0.3) / row) * row);
}

// Listings step by whole rows. A row's height is the port's --row-bc (instructions) or its text's line height.
function rowOf(el) {
  const own = parseFloat(getComputedStyle(el).getPropertyValue('--row-bc'));
  if (el === bcport && own) return own;
  const inner = el.querySelector('.code, .out-code');
  return (inner && parseFloat(getComputedStyle(inner).lineHeight)) || 20;
}
// every scrolling listing is as tall as a whole number of its rows, so none is cut at the bottom edge
function fitPorts() {
  const ports = [port, bcport, hPort, outBody];
  for (const el of ports) { el.style.height = ''; el.style.flex = ''; }
  const sizes = ports.map((el) => { const row = rowOf(el); return Math.floor(el.clientHeight / row) * row; });
  ports.forEach((el, i) => { if (sizes[i] > 0) { el.style.flex = '0 0 auto'; el.style.height = `${sizes[i]}px`; } });
}

function scrollPortTo(y, instant = false) {
  cancelAnimationFrame(scrollAnim);
  const rowH = parseFloat(getComputedStyle(code).lineHeight) || 22;
  const target = clamp(Math.round(y / rowH) * rowH, 0, port.scrollHeight - port.clientHeight); // listings step by whole rows
  const from = port.scrollTop;
  const dist = target - from;
  if (instant || reduced.matches || Math.abs(dist) < 2) { port.scrollTop = target; return Promise.resolve(); }
  const dur = clamp(380 + Math.abs(dist) * 0.3, 420, 900);
  return new Promise((resolve) => {
    const t0 = performance.now();
    const tick = (now) => {
      const t = Math.min(1, (now - t0) / dur);
      port.scrollTop = from + dist * easeInOut(t);
      if (t < 1) scrollAnim = requestAnimationFrame(tick);
      else resolve();
    };
    scrollAnim = requestAnimationFrame(tick);
  });
}

function goNote(i, { scroll = true, instant = false } = {}) {
  i = clamp(i, 0, noteEls.length - 1);
  at = i;
  const note = noteEls[i];
  const id = note.dataset.note;
  noteEls.forEach((n, k) => { n.hidden = k !== i; });
  dots.forEach((d, k) => d.setAttribute('aria-current', String(k === i)));
  noteAt.textContent = String(i + 1);
  for (const m of $$('mark.ns', code)) m.classList.toggle('is-on', m.dataset.note === id);
  for (const b of $$('.nb', code)) b.classList.toggle('is-on', b.dataset.note === id);
  $('#note-box').scrollTop = 0;
  pinned = Number(note.dataset.line);
  const ln = $(`#L${pinned}`);
  if (scroll && ln) scrollPortTo(ln.offsetTop - port.clientHeight * 0.3, instant);
  lightLine(pinned);
}

dots.forEach((d, k) => d.addEventListener('click', () => goNote(k)));
$('#note-prev').addEventListener('click', () => goNote((at - 1 + noteEls.length) % noteEls.length));
$('#note-next').addEventListener('click', () => goNote((at + 1) % noteEls.length));
code.addEventListener('click', (e) => {
  const m = e.target.closest('mark.ns, .nb');
  if (!m) return;
  const i = noteEls.findIndex((n) => n.dataset.note === m.dataset.note);
  if (i >= 0) goNote(i, { scroll: false });
});
code.addEventListener('pointerover', (e) => {
  const ln = e.target.closest('.ln');
  if (!ln || e.pointerType === 'touch') return;
  hovering = true;
  lightLine(Number(ln.dataset.n));
});
code.addEventListener('pointerleave', () => { hovering = false; lightLine(pinned); });
code.addEventListener('pointerdown', (e) => { // touch: a tap on a line shows its instructions
  if (e.pointerType !== 'touch') return;
  const ln = e.target.closest('.ln');
  if (ln && !e.target.closest('mark.ns, .nb')) lightLine(Number(ln.dataset.n));
});
bcport.addEventListener('pointerover', (e) => {
  const row = e.target.closest('.bc[data-t]');
  if (!row || e.pointerType === 'touch') return;
  lightLine(Number(row.dataset.t));
});
bcport.addEventListener('pointerleave', () => lightLine(pinned));

// the bytecode pane follows the function being read
let portTick = false;
port.addEventListener('scroll', () => {
  if (portTick) return;
  portTick = true;
  requestAnimationFrame(() => {
    portTick = false;
    if (hovering) return;
    const y = port.scrollTop + port.clientHeight * 0.32;
    const lines = code.children;
    let lo = 0;
    let hi = lines.length - 1;
    while (lo < hi) { const mid = (lo + hi + 1) >> 1; if (lines[mid].offsetTop <= y) lo = mid; else hi = mid - 1; }
    let k = lo;
    while (k > 0 && !lines[k].dataset.p) k--;
    if (lines[k].dataset.p && lines[k].dataset.p !== curP) { showPassage(lines[k].dataset.p); if (pinned && $(`#L${pinned}`)?.dataset.p === curP) lightLine(pinned); else lightLine(null); }
  });
}, { passive: true });

for (const s of segs) s.addEventListener('click', (e) => {
  e.preventDefault();
  const ln = $(s.getAttribute('href'));
  if (!ln) return;
  pinned = null;
  lightLine(null);
  showPassage(s.dataset.p);
  scrollPortTo(ln.offsetTop);
  const r = frame.getBoundingClientRect();
  if (r.top > innerHeight * 0.7 || r.bottom < innerHeight * 0.3) frame.scrollIntoView({ block: 'center', behavior: reduced.matches ? 'auto' : 'smooth' });
});

// ------------------------------------------------------------------ twenty printings

const stages = pageData.stages;
const hRange = $('#h-range');
const hPort = $('#h-port');
const hcode = $('#hcode');
const hPlay = $('#h-play');
const sils = $$('.sil', $('#sils'));
let texts = null;
let hAt = stages.length - 1;
let hReady = false;
let hBusy = false;
let hWant = null;
let hPlaying = false;
let hToken = 0;
$('#sils').style.setProperty('--count', stages.length);

function loadTexts() {
  if (!texts) {
    texts = fetch(here('../../data/landing-history.json')).then((r) => r.json()).then((d) => new Map(d.stages.map((s) => [s.tag, s.text.replace(/\n+$/, '').split('\n')])));
    texts.catch(() => { texts = null; });
  }
  return texts;
}

function drawSils() {
  const dpr = Math.min(devicePixelRatio || 1, 2);
  const longest = Math.max(...stages.map((s) => s.sil.length));
  const widest = Math.max(...stages.flatMap((s) => s.sil.map(([ind, len]) => ind * 3 + len)));
  const ink = getComputedStyle(document.documentElement).getPropertyValue('--ink').trim() || '#15151d';
  sils.forEach((btn, i) => {
    const cv = $('canvas', btn);
    const w = cv.clientWidth;
    const h = cv.clientHeight;
    if (!w || !h) return;
    cv.width = Math.round(w * dpr);
    cv.height = Math.round(h * dpr);
    const ctx = cv.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = ink;
    const pad = 3;
    const row = (h - pad * 2) / longest;
    stages[i].sil.forEach(([ind, len], k) => {
      if (!len) return;
      ctx.fillRect(pad + (ind * 3 / widest) * (w - pad * 2), pad + k * row, Math.max(1, (len / widest) * (w - pad * 2)), Math.max(0.6, row * 0.62));
    });
  });
}

function paintStage(i) {
  const s = stages[i];
  $('#h-name').textContent = s.name;
  $('#h-date').textContent = s.date;
  $('#h-lines').textContent = s.lines;
  $('#h-plain').textContent = s.plain;
  $('#h-calls').textContent = s.calls;
  $('#h-dec').textContent = s.decimals;
  $('#h-as').textContent = `as ${s.name} printed it`;
  hRange.value = String(i);
  hRange.setAttribute('aria-valuetext', `${s.name}, ${s.date}`);
  sils.forEach((b, k) => b.classList.toggle('is-on', k === i));
}

async function showStage(i, morph) {
  const map = await loadTexts();
  const s = stages[i];
  const lines = map.get(s.tag);
  const animate = morph && hReady && !reduced.matches;
  const clip = clipOf(hPort);
  const before = animate ? tokensIn($$('.ln', hcode), clip) : null;
  const oldAnchor = hReady ? $(`.ln[data-n="${stages[hAt].anchor}"]`, hcode) : null;
  const row = rowOf(hPort);
  const offset = Math.round((oldAnchor ? oldAnchor.offsetTop - hPort.scrollTop : hPort.clientHeight * 0.16) / row) * row;
  const helpers = helpersOf(lines.join('\n'));
  hcode.innerHTML = lines.map((l, k) => lineHtml(l, k + 1, true, callMarks(l, helpers))).join('');
  hAt = i;
  hReady = true;
  const anchor = $(`.ln[data-n="${s.anchor}"]`, hcode);
  hPort.scrollTop = clamp(anchor.offsetTop - offset, 0, hPort.scrollHeight);
  paintStage(i);
  if (animate) {
    const visible = $$('.ln', hcode).filter((l) => { const r = l.getBoundingClientRect(); return r.bottom > clip.top && r.top < clip.bottom; });
    await playMorph(hPort, before, tokensIn(visible, clip), textEls(visible), 640);
  }
}

// one change at a time; a drag that outruns the morph jumps to where the thumb is now
async function requestStage(i, morph = true) {
  hWant = i;
  if (hBusy) return;
  hBusy = true;
  try {
    while (hWant !== null && (hWant !== hAt || !hReady)) {
      const target = hWant;
      hWant = null;
      await showStage(target, morph);
    }
  } catch { /* the excerpt already in the page stays */ }
  hWant = null;
  hBusy = false;
}

function stopHistoryPlay() {
  hPlaying = false;
  hToken++;
  hPlay.setAttribute('aria-pressed', 'false');
  $('span', hPlay).textContent = 'Play every release';
}
// rewind to medal, then step forward through every release; the text changes only where the release changed it
async function playHistory() {
  if (reduced.matches) { await requestStage(stages.length - 1, false); return; }
  const token = ++hToken;
  hPlaying = true;
  hPlay.setAttribute('aria-pressed', 'true');
  $('span', hPlay).textContent = 'Pause';
  await requestStage(0);
  await sleep(1100);
  for (let i = 1; i < stages.length; i++) {
    if (token !== hToken) return;
    const changed = stages[i].changed;
    await requestStage(i, changed);
    await sleep(changed ? 820 : 170);
  }
  if (token === hToken) stopHistoryPlay();
}
hRange.addEventListener('input', () => { stopHistoryPlay(); requestStage(+hRange.value); });
hPlay.addEventListener('click', () => { if (hPlaying) stopHistoryPlay(); else playHistory(); });
sils.forEach((b, i) => b.addEventListener('click', () => { stopHistoryPlay(); requestStage(i); }));

// ------------------------------------------------------------------ Tovek in the browser

let tovek = null;
function loadTovek() {
  if (!tovek) {
    tovek = import(here('../v26/tovek/tovek.js')).then(async (m) => {
      const info = await m.load();
      const rows = await m.samples();
      return { ...m, info, rows };
    });
    tovek.catch(() => { tovek = null; });
  }
  return tovek;
}
async function sha256Hex(text) {
  const h = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return Array.from(new Uint8Array(h), (b) => b.toString(16).padStart(2, '0')).join('');
}

const own = $('#yours');
const outBody = $('#out-body');
const outName = $('#out-name');
const outFacts = $('#out-facts');
const outStatus = $('#out-status');
const chips = $$('.chip', own);
const fmtInt = (n) => n.toLocaleString('en-US');
const NO_WASM = 'This browser cannot run Tovek here. Download it for your machine instead.';
let outSource = null;
let outFile = 'decompiled.luau';
let renderJob = 0;

function setStatus(html, busy = false) {
  outStatus.innerHTML = html;
  outStatus.classList.toggle('is-busy', busy);
}
for (const el of [$('.own-in', own), outBody, $('.plate .btn-line')]) {
  el.addEventListener('pointerenter', () => loadTovek(), { once: true });
  el.addEventListener('focusin', () => loadTovek(), { once: true });
  el.addEventListener('touchstart', () => loadTovek(), { once: true, passive: true });
}
const pressChip = (chip) => { for (const c of chips) c.setAttribute('aria-pressed', String(c === chip)); };

async function readChip(chip) {
  pressChip(chip);
  const title = $('span', chip).textContent;
  setStatus('Loading Tovek into this page', true);
  let t;
  try { t = await loadTovek(); } catch { setStatus(NO_WASM); return; }
  setStatus(`Decompiling ${escapeHtml(title)}`, true);
  let bytes;
  let name;
  let sha;
  let same;
  try {
    if (chip.dataset.sample === 'Beacon') {
      bytes = new Uint8Array(await (await fetch(here('../../' + chip.dataset.url))).arrayBuffer());
      name = chip.dataset.name;
      sha = chip.dataset.sha;
    } else {
      const row = t.rows.find((r) => r.id === chip.dataset.sample);
      bytes = await t.sampleBytes(row);
      name = row.name;
      sha = row.native.sha256;
    }
  } catch { setStatus('That sample could not be loaded. Check your connection and try again.'); return; }
  const res = await t.decompile(bytes, { name });
  if (!res.error) same = (await sha256Hex(res.source)) === sha;
  show(res, { title: name.replace(/\.lua$/, '') + '.luau', bytes: bytes.length, version: t.info.version, same, beacon: chip.dataset.sample === 'Beacon' });
}
for (const c of chips) c.addEventListener('click', () => readChip(c));
$('#out-try').addEventListener('click', () => readChip(chips[0]));

async function readFile(file) {
  pressChip(null);
  setStatus('Loading Tovek into this page', true);
  let t;
  try { t = await loadTovek(); } catch { setStatus(NO_WASM); return; }
  setStatus(`Decompiling ${escapeHtml(file.name)}`, true);
  const res = await t.decompile(file, { name: file.name, timeout: 60000 });
  show(res, { title: file.name.replace(/\.[^.]+$/, '') + '.luau', bytes: file.size, version: t.info.version, same: null });
}
const toReader = () => {
  const r = own.getBoundingClientRect();
  if (r.top > innerHeight * 0.6 || r.bottom < 0) scrollTo({ top: scrollY + r.top - $('#top').offsetHeight, behavior: reduced.matches ? 'auto' : 'smooth' });
};
for (const input of [$('#file'), $('#file-hero')]) input.addEventListener('change', (e) => {
  const f = e.target.files && e.target.files[0];
  e.target.value = '';
  if (!f) return;
  toReader();
  readFile(f);
});

const veil = document.createElement('div');
veil.className = 'dropveil';
veil.innerHTML = '<p>Drop to decompile it in your browser</p>';
document.body.append(veil);
let dragDepth = 0;
addEventListener('dragenter', (e) => {
  if (!e.dataTransfer || !Array.from(e.dataTransfer.types).includes('Files')) return;
  dragDepth++;
  veil.classList.add('is-on');
  loadTovek();
  e.preventDefault();
});
addEventListener('dragover', (e) => { if (veil.classList.contains('is-on')) e.preventDefault(); });
addEventListener('dragleave', () => { dragDepth = Math.max(0, dragDepth - 1); if (!dragDepth) veil.classList.remove('is-on'); });
addEventListener('drop', (e) => {
  dragDepth = 0;
  veil.classList.remove('is-on');
  const f = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
  if (!f) return;
  e.preventDefault();
  toReader();
  readFile(f);
});

function show(res, meta) {
  const job = ++renderJob;
  outBody.scrollTop = 0;
  outBody.tabIndex = 0;
  outName.textContent = meta.title;
  for (const b of $$('.out-act button', own)) b.disabled = !!res.error;
  if (res.error) {
    outSource = null;
    outFacts.textContent = '';
    const why = res.stage === 'input' ? 'This does not look like Luau bytecode or a saved script. Try a file from luau-compile --binary, or a saved Roblox script.'
      : res.stage === 'decompile' ? 'Tovek could not decompile this file. The message above is what it reported.'
      : res.stage === 'timeout' ? 'This took longer than a minute and was stopped. The command-line build has no such limit.' : 'Something went wrong while decompiling. Reload the page and try again.';
    outBody.innerHTML = `<p class="out-error">${escapeHtml(res.error)}</p>`;
    setStatus(escapeHtml(why));
    return;
  }
  outSource = res.source;
  outFile = meta.title;
  const lines = res.source.split('\n');
  const ms = res.ms < 10 ? res.ms.toFixed(1) : Math.round(res.ms);
  // a helper's definition line says how many calls were written back from it
  let calls = 0;
  for (const m of res.source.matchAll(/-- (\d+) equivalent calls? inferred from this helper/g)) calls += Number(m[1]);
  outFacts.textContent = `${fmtInt(lines.length)} lines, ${ms} ms${calls ? `, ${fmtInt(calls)} call${calls === 1 ? '' : 's'} written back` : ''}, Tovek ${meta.version}`;
  const box = document.createElement('div');
  box.className = 'out-code';
  const highlight = lines.length <= 4000;
  const CHUNK = 200;
  const chunks = [];
  for (let i = 0; i < lines.length; i += CHUNK) {
    const c = document.createElement('div');
    c.className = 'chunk';
    c.style.containIntrinsicSize = `auto ${Math.min(CHUNK, lines.length - i) * 20}px`;
    c.dataset.from = i;
    chunks.push(c);
  }
  const helpers = highlight ? helpersOf(res.source) : [];
  const fill = (c) => { const i = +c.dataset.from; c.innerHTML = lines.slice(i, i + CHUNK).map((l, k) => lineHtml(l, i + k + 1, highlight, callMarks(l, helpers))).join(''); };
  chunks.slice(0, 1).forEach(fill);
  box.append(...chunks);
  outBody.replaceChildren(box);
  let next = 1;
  const step = () => {
    if (job !== renderJob) return;
    const until = performance.now() + 7;
    while (next < chunks.length && performance.now() < until) fill(chunks[next++]);
    if (next < chunks.length) requestAnimationFrame(step);
  };
  if (next < chunks.length) requestAnimationFrame(step);
  const nothing = 'Nothing was uploaded.';
  if (meta.beacon && meta.same) setStatus(`<b>Same ${fmtInt(lines.length)} lines as the listing above, byte for byte.</b> ${nothing}`);
  else if (meta.same) setStatus(`<b>Byte for byte</b> what the Tovek command line prints for this file. ${nothing}`);
  else if (meta.beacon) setStatus(`Tovek ${escapeHtml(meta.version)} in this page printed ${fmtInt(lines.length)} lines; the listing above came from the ${escapeHtml(pageData.release)} build. ${nothing}`);
  else setStatus(`Decompiled in your browser from ${fmtInt(meta.bytes)} bytes. ${nothing}`);
}

own.addEventListener('click', async (e) => {
  const b = e.target.closest('[data-act]');
  if (!b || !outSource) return;
  if (b.dataset.act === 'copy') {
    try { await navigator.clipboard.writeText(outSource); b.textContent = 'Copied'; } catch { b.textContent = 'Copy failed'; }
    setTimeout(() => { b.textContent = 'Copy'; }, 1600);
  }
  if (b.dataset.act === 'save') {
    const url = URL.createObjectURL(new Blob([outSource + '\n'], { type: 'text/plain' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = outFile;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 2000);
  }
});

// copying from any code panel keeps the indentation (it is drawn as padding, not tabs)
document.addEventListener('copy', (e) => {
  const sel = getSelection();
  if (!sel.rangeCount) return;
  const host = sel.anchorNode && (sel.anchorNode.nodeType === 1 ? sel.anchorNode : sel.anchorNode.parentElement)?.closest('.code, .view, .out-code');
  if (!host) return;
  const range = sel.getRangeAt(0);
  const lines = $$('.ln', host).filter((l) => l.offsetParent && range.intersectsNode(l));
  if (lines.length < 2) return;
  const text = lines.map((l) => {
    const t = l.querySelector('.t');
    const r = document.createRange();
    r.selectNodeContents(t);
    if (range.compareBoundaryPoints(Range.START_TO_START, r) > 0 && t.contains(range.startContainer)) r.setStart(range.startContainer, range.startOffset);
    if (range.compareBoundaryPoints(Range.END_TO_END, r) < 0 && t.contains(range.endContainer)) r.setEnd(range.endContainer, range.endOffset);
    return '\t'.repeat(+(l.style.getPropertyValue('--i') || 0)) + r.toString();
  }).join('\n');
  e.clipboardData.setData('text/plain', text);
  e.preventDefault();
});

// ------------------------------------------------------------------ the measurements: 9,000 programs, one cell each

function drawWaffle() {
  const cv = $('#waffle');
  if (!cv) return;
  const passed = +cv.dataset.passed;
  const refused = +cv.dataset.refused;
  const invalid = +cv.dataset.invalid;
  const wrong = +cv.dataset.wrong;
  const total = passed + refused + invalid + wrong;
  const w = cv.parentElement.clientWidth;
  if (!w) return;
  const rowsWanted = w < 600 ? 90 : 36;
  const cols = Math.ceil(total / rowsWanted);
  const pitch = w / cols;
  const cell = Math.max(1, pitch * 0.74);
  const nRows = Math.ceil(total / cols);
  const h = Math.ceil(nRows * pitch);
  const dpr = Math.min(devicePixelRatio || 1, 2);
  cv.width = Math.round(w * dpr);
  cv.height = Math.round(h * dpr);
  cv.style.aspectRatio = `${w} / ${h}`;
  const ctx = cv.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const css = getComputedStyle(document.documentElement);
  const sky = css.getPropertyValue('--c0').trim() || '#c8f0fd';
  const pink = css.getPropertyValue('--pink').trim() || '#ff8dc3';
  ctx.lineWidth = Math.max(0.8, cell * 0.2);
  for (let i = 0; i < total; i++) {
    const x = (i % cols) * pitch;
    const y = Math.floor(i / cols) * pitch;
    if (i < wrong) { ctx.fillStyle = pink; ctx.fillRect(x, y, cell, cell); }
    else if (i < wrong + passed) { ctx.fillStyle = sky; ctx.fillRect(x, y, cell, cell); }
    else if (i < wrong + passed + refused) { ctx.strokeStyle = sky; const o = ctx.lineWidth / 2; ctx.strokeRect(x + o, y + o, cell - ctx.lineWidth, cell - ctx.lineWidth); }
    else { ctx.fillStyle = '#3a3a48'; ctx.fillRect(x, y, cell, cell); }
  }
}

// ------------------------------------------------------------------ downloads: the visitor's own platform first

function yourPlatform() {
  const ua = navigator.userAgent;
  if (/Windows NT/.test(ua)) return 'Windows';
  if (/Linux/.test(ua) && /x86_64|amd64/i.test(ua) && !/Android/.test(ua)) return 'Linux';
  return null;
}
function markDownload() {
  const plat = yourPlatform();
  const dl = plat && $(`.dl[data-platform="${plat}"]`);
  if (!dl) return;
  dl.classList.add('is-yours');
  dl.closest('ul').prepend(dl.parentElement);
  const btn = $('#dl-hero');
  btn.href = dl.href;
  $('span', btn).textContent = `Download ${pageData.release} for ${plat}`;
}

// ------------------------------------------------------------------ films: a card plays its film while the pointer rests on it

// The films are pure functions of time (watch/engine): the card draws its own film from the poster frame on, muted,
// in a canvas over the poster, and lets go of it when the pointer leaves.
const liveFilms = new Map();
const canHover = matchMedia('(hover: hover) and (pointer: fine)');
const watch = new URL('../../watch/', import.meta.url);
function liveFilm(card) {
  let entry = liveFilms.get(card);
  if (!entry) {
    entry = (async () => {
      const [registry, engine] = await Promise.all([import(new URL('films/index.js', watch).href), import(new URL('engine/index.js', watch).href)]);
      const f = registry.filmById(card.dataset.film);
      if (!f || !f.available) throw new Error('no film');
      const mod = await import(new URL(f.src, new URL('films/', watch)).href);
      const film = await engine.openFilm(mod, { base: watch.href });
      const canvas = document.createElement('canvas');
      canvas.className = 'film-live';
      canvas.setAttribute('aria-hidden', 'true');
      $('.film-frame img', card).after(canvas);
      const stage = engine.createStage(canvas);
      return { film, stage, canvas, t: film.poster || 0, raf: 0, last: 0, on: false };
    })();
    entry.catch(() => liveFilms.delete(card));
    liveFilms.set(card, entry);
  }
  return entry;
}
async function playCard(card) {
  if (reduced.matches || !canHover.matches) return;
  let live;
  try { live = await liveFilm(card); } catch { return; }
  if (!card.matches(':hover, :focus-visible')) return;
  const frameEl = $('.film-frame', card);
  let bar = $('.film-bar', frameEl);
  if (!bar) { bar = document.createElement('span'); bar.className = 'film-bar'; bar.setAttribute('aria-hidden', 'true'); frameEl.append(bar); }
  const r = frameEl.getBoundingClientRect();
  live.stage.resize(r.width, r.height, Math.min(devicePixelRatio || 1, 2));
  live.on = true;
  live.last = performance.now();
  card.classList.add('is-live');
  cancelAnimationFrame(live.raf);
  const tick = (now) => {
    if (!live.on) return;
    live.t += Math.min(0.1, (now - live.last) / 1000);
    live.last = now;
    if (live.t >= live.film.duration) live.t = 0;
    live.stage.draw(live.film, live.t);
    bar.style.transform = `scaleX(${(live.t / live.film.duration).toFixed(4)})`;
    live.raf = requestAnimationFrame(tick);
  };
  live.stage.draw(live.film, live.t);
  live.raf = requestAnimationFrame(tick);
}
async function stopCard(card) {
  const entry = liveFilms.get(card);
  card.classList.remove('is-live');
  if (!entry) return;
  let live;
  try { live = await entry; } catch { return; }
  live.on = false;
  cancelAnimationFrame(live.raf);
}
for (const card of $$('.film[data-live]')) {
  card.addEventListener('pointerenter', (e) => { if (e.pointerType === 'mouse') playCard(card); });
  card.addEventListener('pointerleave', () => stopCard(card));
  card.addEventListener('focus', () => { if (card.matches(':focus-visible')) playCard(card); });
  card.addEventListener('blur', () => stopCard(card));
}
// the cards were built with the registry's films; a film that is gone hides its card
async function checkFilms() {
  let registry;
  try { registry = await import(new URL('films/index.js', watch).href); } catch { return; }
  for (const card of $$('.film')) {
    const f = registry.filmById(card.dataset.film);
    if (!f || !f.available) card.hidden = true;
  }
}

// ------------------------------------------------------------------ start

// a function's name shows under its segment of the strip only where it fits whole, with room to spare
function fitStrip() {
  const range = document.createRange();
  for (const s of segs) {
    range.selectNodeContents($('.seg-n', s));
    s.classList.toggle('no-label', range.getBoundingClientRect().width > s.clientWidth - 12);
  }
}

function relayout() {
  styleCache.clear();
  fitReadBody();
  fitPorts();
  fitStrip();
  if (at >= 0 && pinned) { const ln = $(`#L${pinned}`); if (ln) scrollPortTo(ln.offsetTop - port.clientHeight * 0.3, true); }
  redraw();
  drawSils();
  drawWaffle();
}

function start() {
  fitReadBody();
  fitPorts();
  fitStrip();
  const opening = prepareOpening();
  orderRibbons();
  redraw();
  showPassage(bcps[0].dataset.p);
  goNote(0, { instant: true });
  drawSils();
  drawWaffle();
  armBits();
  markDownload();
  checkFilms();
  if (opening) playOpening();
  let rz = 0;
  addEventListener('resize', () => { cancelAnimationFrame(rz); rz = requestAnimationFrame(relayout); });
  if ('ResizeObserver' in window) new ResizeObserver(() => { if (!lensBusy) redraw(); }).observe(readBody);
  // every release's text is fetched when the reader gets near that section, or touches its controls
  const hist = $('#history');
  const warm = () => { if (!hReady) requestStage(stages.length - 1, false); };
  if ('IntersectionObserver' in window) new IntersectionObserver((entries, io) => { if (entries.some((en) => en.isIntersecting)) { io.disconnect(); warm(); } }, { rootMargin: '900px 0px' }).observe(hist);
  hist.addEventListener('pointerenter', warm, { once: true });
  hist.addEventListener('focusin', warm, { once: true });
}

(document.fonts && document.fonts.ready ? document.fonts.ready : Promise.resolve()).then(() => requestAnimationFrame(start));
