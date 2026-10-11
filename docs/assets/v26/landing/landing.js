// Tovek landing.
// Everything the page shows is already in the HTML (filled from data/landing.json by the data step). This module
// makes it move: the hero's method reads itself back from bytecode and turns between bytecode, Tovek and source
// (the lens); the guided reader scrolls its code to each note and draws the note's leader; its last step scrubs
// through every release; and the visitor's own file is read with the in-browser build of Tovek.

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
const reduced = matchMedia('(prefers-reduced-motion: reduce)');
const phone = matchMedia('(max-width: 759px)');
const here = (p) => new URL(p, import.meta.url).href;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const clamp = (x, a, b) => Math.max(a, Math.min(b, x));
const easeInOut = (x) => (x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2);
const easeOut = (x) => 1 - Math.pow(1 - x, 4);
window.__landing = true;

// ------------------------------------------------------------------ tokens (same rules as the data step)

const KEYWORDS = new Set(['and', 'break', 'continue', 'do', 'else', 'elseif', 'end', 'false', 'for', 'function', 'if',
  'in', 'local', 'nil', 'not', 'or', 'repeat', 'return', 'then', 'true', 'until', 'while', 'export', 'type']);
const TOKEN = /(--.*$)|("(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')|(`)|(0[xX][0-9a-fA-F_]+|0[bB][01_]+|(?:\d[\d_]*\.?[\d_]*|\.\d[\d_]*)(?:[eE][+-]?\d+)?)|([A-Za-z_][A-Za-z0-9_]*)|(\.\.\.|\.\.=?|[=~<>]=|\/\/=?|[-+*/%^#&|]=?|::|[()[\]{};:,.=<>])|(\s+)|(.)/y;
const RECOVERED = /equivalent calls? inferred|inferred (?:equivalent )?call/;

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
    else if (m[5] !== undefined) kind = KEYWORDS.has(text) ? 'k' : (fname === 1 ? 'f' : 'i');
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

function lineHtml(text, n, highlight = true) {
  const ind = text.length - text.replace(/^\t+/, '').length;
  const body = text.slice(ind);
  const t = body ? (highlight ? tokensHtml(tokenize(body)) : escapeHtml(body)) : '';
  return `<div class="ln${body ? '' : ' blank'}" data-n="${n}" style="--i:${ind}"><span class="t">${t}</span></div>`;
}

// ------------------------------------------------------------------ the morph: shared tokens glide, the rest fade

function tokenKey(text, cls) {
  let t = text.trim();
  if (!t) return null;
  if (cls === 'op' || cls === 'lbl' || cls === 'bc-from') return null;
  if (/^R\d+$|^K\d+$|^L\d+:?$|^\+\d+$/.test(t)) return null;
  t = t.replace(/^\[|\]$/g, '').replace(/^['"`]|['"`]$/g, '').replace(/^\}|\{$/g, '');
  return t || null;
}

const styleCache = new Map();
function styleOf(el, cls, lineCls) {
  const key = `${cls}|${lineCls}`;
  let st = styleCache.get(key);
  if (!st) {
    const cs = getComputedStyle(el);
    st = { color: cs.color, font: `${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`,
      upper: cs.textTransform === 'uppercase', ls: cs.letterSpacing === 'normal' ? '0px' : cs.letterSpacing };
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
    const t = line.classList.contains('pane-tag') ? line : line.querySelector('.t');
    if (!t) continue;
    const walker = document.createTreeWalker(t, NodeFilter.SHOW_TEXT);
    while (walker.nextNode()) {
      const node = walker.currentNode;
      const parent = node.parentElement;
      const cls = parent === t || parent.tagName === 'MARK' ? '' : parent.className;
      const text = node.textContent;
      const re = /\S+/g;
      let m;
      while ((m = re.exec(text))) {
        range.setStart(node, m.index);
        range.setEnd(node, m.index + m[0].length);
        const rects = range.getClientRects();
        if (rects.length !== 1) continue;
        const r = rects[0];
        if (r.bottom < clip.top || r.top > clip.bottom) continue;
        const st = styleOf(parent, cls, line.className);
        out.push({ text: st.upper ? m[0].toUpperCase() : m[0], key: tokenKey(m[0], cls), x: r.left, y: r.top, h: r.height,
          color: st.color, font: st.font, ls: st.ls });
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
  const descentOf = (font) => {
    let d = descents.get(font);
    if (d === undefined) { ctx.font = font; d = ctx.measureText('Hg').fontBoundingBoxDescent || 3; descents.set(font, d); }
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
  const spacing = 'letterSpacing' in ctx;
  const glyph = (tok, x, y, alpha) => {
    ctx.globalAlpha = alpha;
    ctx.font = tok.font;
    if (spacing) ctx.letterSpacing = tok.ls;
    ctx.fillStyle = tok.color;
    ctx.fillText(tok.text, x - ox, y + tok.h - descentOf(tok.font) - oy);
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
        if (m.a.text === m.b.text && m.a.font === m.b.font && m.a.ls === m.b.ls) glyph(e < 0.5 ? m.a : m.b, x, y, 1);
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

const textEls = (lines) => lines.map((l) => (l.classList.contains('pane-tag') ? l : l.querySelector('.t') || l));

// ------------------------------------------------------------------ header

const top = $('#top');
let solid = false;
function onScrollChrome() {
  const s = scrollY > 8;
  if (s !== solid) { solid = s; top.classList.toggle('is-solid', s); }
}

// ------------------------------------------------------------------ the hero's method and its lens

const glBody = $('#gl-body');
const lens = $('#lens');
let glView = 'tovek';
let glBusy = false;
const pane = (view) => $(`.pane[data-view="${view}"]`, glBody);

function markLens(view) {
  for (const b of $$('button', lens)) {
    const on = b.dataset.view === view;
    b.setAttribute('aria-checked', String(on));
    b.tabIndex = on ? 0 : -1;
  }
}

async function setGlimpse(view, dur = 760) {
  if (glBusy || view === glView) return;
  glBusy = true;
  const oldEl = pane(glView);
  const newEl = pane(view);
  const animate = !reduced.matches;
  const before = animate ? tokensIn($$('.ln, .pane-tag', oldEl), clipOf(glBody)) : [];
  oldEl.hidden = true;
  newEl.hidden = false;
  glBody.scrollTop = 0;
  glView = view;
  markLens(view);
  oldEl.classList.remove('is-settled');
  if (animate) {
    const lines = $$('.ln, .pane-tag', newEl);
    const now = tokensIn(lines, clipOf(glBody));
    await playMorph(glBody, before, now, textEls(lines), dur);
  }
  newEl.classList.add('is-settled');
  glBusy = false;
}

lens.addEventListener('click', (e) => {
  const b = e.target.closest('button[data-view]');
  if (b) { glideCancelled = true; setGlimpse(b.dataset.view); }
});
lens.addEventListener('keydown', (e) => {
  if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
  const bs = $$('button', lens);
  const i = bs.indexOf(document.activeElement);
  let j = e.key === 'Home' ? 0 : e.key === 'End' ? bs.length - 1 : (i + (e.key === 'ArrowRight' ? 1 : bs.length - 1)) % bs.length;
  bs[j].focus();
  bs[j].click();
  e.preventDefault();
});

// first sight: the method shows as its bytecode, then reads itself back into source
let glideCancelled = false;
function prepareGlide() {
  const html = document.documentElement;
  if (!html.classList.contains('glide')) { pane('tovek').classList.add('is-settled'); return false; }
  pane('tovek').hidden = true;
  pane('bytecode').hidden = false;
  html.classList.remove('glide');
  glView = 'bytecode';
  markLens('bytecode');
  return true;
}
async function playGlide() {
  await sleep(1100);
  if (glideCancelled || document.hidden) {
    if (glView === 'bytecode' && !glideCancelled) setGlimpse('tovek', 0);
    return;
  }
  const r = glBody.getBoundingClientRect();
  const dur = r.bottom > 0 && r.top < innerHeight ? 1250 : 0;
  setGlimpse('tovek', dur);
}

// ------------------------------------------------------------------ the guided reader

const frame = $('#frame');
const frBody = $('#fr-body');
const port = $('#fr-port');
const code = $('#code');
const hcode = $('#hcode');
const notesBox = $('#fr-notes');
const leader = $('#fr-leader');
const steps = $$('.step', notesBox);
const tabs = $$('[role="tab"]', $('#fr-tabs'));
const frPsg = $('#fr-psg');
const frAt = $('#fr-at');
const frOf = $('#fr-of');
const playBtn = $('#fr-play');
const rail = $('#fr-rail');
const railCanvas = $('canvas', rail);
const railWin = $('.fr-win', rail);
const railMarks = $('.fr-marks', rail);
const historyIndex = steps.findIndex((s) => s.classList.contains('step-history'));

let at = -1;
let inHistory = false;
let scrollAnim = 0;
frOf.textContent = `/ ${String(steps.length).padStart(2, '0')}`;

const content = () => (inHistory ? hcode : code);
const contentHeight = () => {
  const c = content();
  const last = c.lastElementChild;
  return last ? last.offsetTop + last.offsetHeight + 18 : c.offsetHeight;
};
const maxScroll = () => Math.max(0, contentHeight() - port.clientHeight);

function anchorLine(step) {
  const id = step.dataset.note;
  const m = id && $(`mark.ns[data-note="${id}"]`, code);
  return m ? m.closest('.ln') : null;
}

// where the port scrolls for a note: its line a little above the middle, as far as the file allows
function portTarget(line) {
  return clamp(Math.round(line.offsetTop - port.clientHeight * (phone.matches ? 0.2 : 0.3)), 0, maxScroll());
}

// the note sits level with its line, so its leader runs straight across (unless the note is too tall to fit)
function alignNote(step, line, scrollY) {
  if (phone.matches) { step.style.removeProperty('--pt'); return; }
  const inner = step.querySelector('.st-in');
  const k = step.querySelector('.st-k');
  const row = parseFloat(getComputedStyle(line).lineHeight) || 22;
  const anchorY = line.offsetTop + line.offsetHeight - row / 2 - scrollY;
  const kc = k.offsetTop - inner.offsetTop + k.offsetHeight / 2;
  const room = notesBox.clientHeight - inner.offsetHeight - 30;
  step.style.setProperty('--pt', `${Math.round(clamp(anchorY - kc, 26, Math.max(26, room)))}px`);
}

function scrollPortTo(y, instant = false) {
  cancelAnimationFrame(scrollAnim);
  const target = clamp(Math.round(y), 0, maxScroll());
  const from = port.scrollTop;
  const dist = target - from;
  if (instant || reduced.matches || Math.abs(dist) < 2) {
    port.scrollTop = target;
    onPortScroll();
    return Promise.resolve();
  }
  const dur = clamp(380 + Math.abs(dist) * 0.32, 420, 980);
  return new Promise((resolve) => {
    const t0 = performance.now();
    const tick = (now) => {
      const t = Math.min(1, (now - t0) / dur);
      port.scrollTop = from + dist * easeInOut(t);
      onPortScroll();
      if (t < 1) scrollAnim = requestAnimationFrame(tick);
      else resolve();
    };
    scrollAnim = requestAnimationFrame(tick);
  });
}

// ---- the leader: from the end of the lit line, out of the code, over to the note's number

const NS = 'http://www.w3.org/2000/svg';
let leaderG = null;
function buildLeader(draw) {
  const step = steps[at];
  const line = !inHistory && !phone.matches && step ? anchorLine(step) : null;
  if (!line) { leader.replaceChildren(); leaderG = null; return; }
  const geo = leaderGeometry(step, line);
  if (!geo) { leader.replaceChildren(); leaderG = null; return; }
  const g = document.createElementNS(NS, 'g');
  const p = document.createElementNS(NS, 'path');
  p.setAttribute('pathLength', '1');
  const c = document.createElementNS(NS, 'circle');
  c.setAttribute('r', '2.6');
  g.append(p, c);
  leader.replaceChildren(g);
  leaderG = g;
  placeLeader(geo);
  if (draw && !reduced.matches) requestAnimationFrame(() => requestAnimationFrame(() => g.classList.add('is-drawn')));
  else g.classList.add('is-drawn');
}

function leaderGeometry(step, line) {
  const body = frBody.getBoundingClientRect();
  const pr = port.getBoundingClientRect();
  const t = line.querySelector('.t');
  const range = document.createRange();
  range.selectNodeContents(t);
  const rects = Array.from(range.getClientRects()).filter((r) => r.width > 0);
  const last = rects[rects.length - 1];
  if (!last) return null;
  const ay = last.top + last.height / 2 - body.top;
  const visible = ay > pr.top - body.top + 6 && ay < pr.bottom - body.top - 6;
  const ax = Math.min(last.right + 9, pr.right - 18) - body.left;
  const k = step.querySelector('.st-k');
  const nb = notesBox.getBoundingClientRect();
  const nx = nb.left - body.left + k.offsetLeft - 12;
  const ny = nb.top - body.top + k.offsetTop + k.offsetHeight / 2 - step.scrollTop;
  return { ax, ay, nx, ny, edge: pr.right - body.left, visible };
}

function placeLeader(geo) {
  if (!leaderG) return;
  const { ax, ay, nx, ny, edge, visible } = geo;
  const p = leaderG.firstChild;
  const c = leaderG.lastChild;
  const bend = edge + 6;
  const mid = (bend + nx) / 2;
  p.setAttribute('d', Math.abs(ny - ay) < 1 ? `M${ax} ${ay}H${nx}` : `M${ax} ${ay}H${bend}C${mid} ${ay} ${mid} ${ny} ${nx} ${ny}`);
  c.setAttribute('cx', ax);
  c.setAttribute('cy', ay);
  leaderG.classList.toggle('is-gone', !visible);
}

function refreshLeader() {
  if (!leaderG) return;
  const step = steps[at];
  const line = anchorLine(step);
  const geo = line && leaderGeometry(step, line);
  if (geo) placeLeader(geo);
}

// ---- the rail: the whole file in miniature, its notes, and the window being read

let railLines = [];
function drawRail() {
  if (phone.matches || !rail.offsetParent) return;
  const dpr = Math.min(devicePixelRatio || 1, 2);
  const w = railCanvas.clientWidth;
  const h = railCanvas.clientHeight;
  if (!w || !h) return;
  railCanvas.width = Math.round(w * dpr);
  railCanvas.height = Math.round(h * dpr);
  const ctx = railCanvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  const total = Math.max(1, contentHeight());
  const k = h / total;
  railLines = $$('.ln', content()).map((l) => ({ top: l.offsetTop, h: l.offsetHeight, ind: +(l.style.getPropertyValue('--i') || 0), len: (l.textContent || '').length, rc: !!l.querySelector('.r') }));
  for (const l of railLines) {
    if (!l.len) continue;
    const y = l.top * k;
    const bh = Math.max(1, Math.min(l.h * k * 0.6, 2.4));
    const x = Math.min(l.ind * 2, w * 0.45);
    const bw = Math.max(1.5, Math.min(l.len * 0.2, w - x));
    ctx.fillStyle = l.rc ? 'rgba(214, 58, 12, 0.55)' : 'rgba(18, 18, 16, 0.26)';
    ctx.fillRect(x, y, bw, bh);
  }
  const marks = [];
  if (!inHistory) {
    steps.forEach((s, i) => {
      const line = anchorLine(s);
      if (!line) return;
      const m = document.createElement('span');
      m.className = 'fr-mark' + (i === at ? ' is-on' : '');
      m.dataset.i = i;
      m.style.top = `${(line.offsetTop + line.offsetHeight / 2) * k}px`;
      marks.push(m);
    });
  }
  railMarks.replaceChildren(...marks);
  moveRailWindow();
}

function moveRailWindow() {
  if (phone.matches) return;
  const h = railCanvas.clientHeight;
  const total = Math.max(1, contentHeight());
  const k = h / total;
  railWin.style.transform = `translateY(${port.scrollTop * k}px)`;
  railWin.style.height = `${Math.max(14, Math.min(h, port.clientHeight * k))}px`;
}

function onPortScroll() {
  moveRailWindow();
  refreshLeader();
}

function railPointer(e) {
  const r = railCanvas.getBoundingClientRect();
  const f = clamp((e.clientY - r.top) / r.height, 0, 1);
  return f * contentHeight() - port.clientHeight / 2;
}
let railDrag = false;
rail.addEventListener('pointerdown', (e) => {
  const mark = !inHistory && $$('.fr-mark', railMarks).find((m) => Math.abs(m.getBoundingClientRect().top + 3.5 - e.clientY) < 7);
  if (mark) { goTo(+mark.dataset.i, { user: true }); return; }
  railDrag = true;
  rail.setPointerCapture(e.pointerId);
  stopPlay();
  scrollPortTo(railPointer(e), true);
});
rail.addEventListener('pointermove', (e) => { if (railDrag) scrollPortTo(railPointer(e), true); });
rail.addEventListener('pointerup', () => { railDrag = false; });
rail.addEventListener('pointercancel', () => { railDrag = false; });

port.addEventListener('keydown', (e) => {
  const lh = 22;
  const map = { ArrowDown: lh * 3, ArrowUp: -lh * 3, PageDown: port.clientHeight * 0.8, PageUp: -port.clientHeight * 0.8 };
  let y = null;
  if (e.key in map) y = port.scrollTop + map[e.key];
  if (e.key === 'Home') y = 0;
  if (e.key === 'End') y = maxScroll();
  if (y === null) return;
  e.preventDefault();
  stopPlay();
  scrollPortTo(y, e.repeat);
});

// a click on another note's words opens that note
code.addEventListener('click', (e) => {
  const m = e.target.closest('mark.ns, .mk');
  if (!m) return;
  const id = m.dataset.note || (steps.find((s) => s.id === m.dataset.step) || {}).dataset?.note;
  const i = steps.findIndex((s) => s.dataset.note === id);
  if (i >= 0 && i !== at) goTo(i, { user: true });
});

// ---- stepping

function paintStep(i) {
  tabs.forEach((t, k) => {
    const on = k === i;
    t.setAttribute('aria-selected', String(on));
    t.tabIndex = on ? 0 : -1;
    t.style.removeProperty('--p');
  });
  steps.forEach((s, k) => {
    s.classList.toggle('is-on', k === i);
    s.tabIndex = k === i ? 0 : -1;
    if (k === i) markMore(s);
  });
  frAt.textContent = String(i + 1).padStart(2, '0');
  const id = steps[i].dataset.note;
  for (const m of $$('mark.ns', code)) m.classList.toggle('is-on', m.dataset.note === id);
  for (const l of $$('.ln.is-anchor', code)) l.classList.remove('is-anchor');
  const line = anchorLine(steps[i]);
  if (line) line.classList.add('is-anchor');
  for (const mk of $$('.mk', code)) mk.classList.toggle('is-on', mk.dataset.step === steps[i].id);
  for (const m of $$('.fr-mark', railMarks)) m.classList.toggle('is-on', +m.dataset.i === i);
}

let stepToken = 0;
async function goTo(i, { user = false, instant = false } = {}) {
  i = clamp(i, 0, steps.length - 1);
  if (user) stopPlay();
  if (i === at && !instant) return;
  const token = ++stepToken;
  const prev = at;
  at = i;
  elapsed = 0;
  paintStep(i);
  leader.replaceChildren();
  leaderG = null;
  const step = steps[i];
  if (i === historyIndex) {
    frPsg.textContent = 'every release';
    await enterHistory(prev, token);
    return;
  }
  if (inHistory) leaveHistory();
  frPsg.textContent = step.dataset.psg;
  const line = anchorLine(step);
  if (line) {
    const y = portTarget(line);
    alignNote(step, line, y);
    await scrollPortTo(y, instant || prev < 0);
  }
  if (token !== stepToken) return;
  buildLeader(!instant);
}

tabs.forEach((t, k) => t.addEventListener('click', () => goTo(k, { user: true })));
$('#fr-tabs').addEventListener('keydown', (e) => {
  const i = tabs.indexOf(document.activeElement);
  if (i < 0) return;
  let j = null;
  if (e.key === 'ArrowRight') j = (i + 1) % tabs.length;
  if (e.key === 'ArrowLeft') j = (i - 1 + tabs.length) % tabs.length;
  if (e.key === 'Home') j = 0;
  if (e.key === 'End') j = tabs.length - 1;
  if (j === null) return;
  e.preventDefault();
  tabs[j].focus();
  goTo(j, { user: true });
});
$('#fr-prev').addEventListener('click', () => goTo(at - 1, { user: true }));
$('#fr-next').addEventListener('click', () => goTo(at + 1, { user: true }));
// a note taller than its panel scrolls inside it; its foot fades until the end is reached
const markMore = (s) => s.classList.toggle('is-more', s.scrollHeight - s.clientHeight - s.scrollTop > 6);
for (const s of steps) s.addEventListener('scroll', () => { refreshLeader(); markMore(s); }, { passive: true });

// swipe across the frame on a phone: next or previous note
let swipe = null;
for (const el of [$('.fr-codecol', frame), notesBox]) {
  el.addEventListener('pointerdown', (e) => { if (e.pointerType !== 'mouse') swipe = { x: e.clientX, y: e.clientY, t: performance.now() }; });
  el.addEventListener('pointerup', (e) => {
    if (!swipe || e.target.closest('input')) { swipe = null; return; }
    const dx = e.clientX - swipe.x;
    const dy = e.clientY - swipe.y;
    swipe = null;
    if (Math.abs(dx) > 48 && Math.abs(dx) > Math.abs(dy) * 1.4) goTo(at + (dx < 0 ? 1 : -1), { user: true });
  });
  el.addEventListener('pointercancel', () => { swipe = null; });
}

// ---- autoplay: the reader turns its own pages while it is in view, until the visitor takes over

let playing = false;
let userStopped = false;
let inView = false;
let hovered = false;
let elapsed = 0;
let lastTick = 0;
let playLoop = 0;

function stepDuration(i) {
  const s = steps[i];
  if (i === historyIndex) return Infinity;
  const words = (($('.st-text', s) || s).textContent.match(/\S+/g) || []).length;
  return clamp(4200 + words * 240, 6500, 14000);
}

function setPlaying(on) {
  playing = on;
  frame.classList.toggle('is-playing', on);
  playBtn.setAttribute('aria-pressed', String(on));
  playBtn.setAttribute('aria-label', on ? 'Pause the notes' : 'Play the notes');
  cancelAnimationFrame(playLoop);
  if (on) { lastTick = performance.now(); playLoop = requestAnimationFrame(tickPlay); }
}
function stopPlay() {
  if (playing) userStopped = true;
  setPlaying(false);
  stopHistoryPlay();
}
function tickPlay(now) {
  if (!playing) return;
  const dt = Math.min(100, now - lastTick);
  lastTick = now;
  if (inView && !hovered && !document.hidden && at !== historyIndex) {
    elapsed += dt;
    const d = stepDuration(at);
    const tab = tabs[at];
    tab.style.setProperty('--p', Math.min(1, elapsed / d).toFixed(4));
    if (elapsed >= d) goTo(at + 1);
  }
  playLoop = requestAnimationFrame(tickPlay);
}
playBtn.addEventListener('click', () => {
  if (playing) { stopPlay(); return; }
  userStopped = false;
  autoStarted = false;
  hovered = false;
  if (at === steps.length - 1) goTo(0);
  setPlaying(true);
  if (at === historyIndex) playHistory();
});
// a reader resting on the frame holds the page it is on (unless they pressed play themselves)
let autoStarted = false;
frame.addEventListener('pointerenter', (e) => { if (e.pointerType === 'mouse' && autoStarted) hovered = true; });
frame.addEventListener('pointerleave', () => { hovered = false; });
// a touch on the frame means the visitor is reading it: the pages stop turning by themselves
frame.addEventListener('pointerdown', (e) => { if (e.pointerType !== 'mouse' && playing && !e.target.closest('#fr-play')) stopPlay(); });
frame.addEventListener('focusin', (e) => { if (autoStarted && e.target.matches(':focus-visible') && e.target !== playBtn) hovered = true; });
frame.addEventListener('focusout', (e) => { if (!frame.contains(e.relatedTarget)) hovered = false; });

// ---- the last step: every release, one passage

const historyData = JSON.parse($('#history-data').textContent);
const stages = historyData.stages;
const hRange = $('#h-range');
const hName = $('#h-name');
const hDate = $('#h-date');
const hPlay = $('#h-play');
const histBox = $('.hist');
let texts = null;
let hAt = stages.length - 1;
let hBusy = false;
let hWant = null;
let hPlaying = false;
let hToken = 0;

function loadTexts() {
  if (!texts) {
    texts = fetch(here('../../../data/landing-history.json')).then((r) => r.json()).then((d) => {
      const map = new Map(d.stages.map((s) => [s.tag, s.text.replace(/\n+$/, '').split('\n')]));
      texts = map;
      return map;
    });
    texts.catch(() => { texts = null; });
  }
  return Promise.resolve(texts);
}

function paintStageFacts(i) {
  const s = stages[i];
  hName.textContent = s.name;
  hDate.textContent = s.date;
  $('#h-lines').textContent = s.lines;
  $('#h-plain').textContent = s.plain;
  $('#h-calls').textContent = s.calls;
  $('#h-dec').textContent = s.decimals;
  hRange.value = String(i);
  hRange.setAttribute('aria-valuetext', `${s.name}, ${s.date}`);
  const last = i === stages.length - 1;
  hRange.classList.toggle('is-last', last);
  histBox.classList.toggle('is-last', last);
  if (inHistory) frPsg.textContent = `as ${s.name} printed it`;
}

async function showStage(i, morph) {
  const map = await loadTexts();
  const s = stages[i];
  const lines = map.get(s.tag);
  const animate = morph && !reduced.matches && inHistory;
  const clip = clipOf(port);
  const before = animate ? tokensIn($$('.ln', hcode), clip) : null;
  const oldAnchor = $(`.ln[data-n="${stages[hAt].anchor}"]`, hcode);
  const offset = oldAnchor ? oldAnchor.offsetTop - port.scrollTop : port.clientHeight * (phone.matches ? 0.12 : 0.2);
  hcode.innerHTML = lines.map((l, k) => lineHtml(l, k + 1)).join('');
  hAt = i;
  const anchor = $(`.ln[data-n="${s.anchor}"]`, hcode);
  port.scrollTop = clamp(anchor.offsetTop - offset, 0, maxScroll());
  paintStageFacts(i);
  drawRail();
  if (animate) {
    const now = tokensIn($$('.ln', hcode), clip);
    await playMorph(port, before, now, textEls($$('.ln', hcode).filter((l) => { const r = l.getBoundingClientRect(); return r.bottom > clip.top && r.top < clip.bottom; })), 640);
  }
}

// one change at a time; a drag that outruns the morph jumps to where the thumb is now
async function requestStage(i, morph = true) {
  hWant = i;
  if (hBusy) return;
  hBusy = true;
  while (hWant !== null && hWant !== hAt) {
    const target = hWant;
    hWant = null;
    await showStage(target, morph);
  }
  hWant = null;
  hBusy = false;
}

async function enterHistory(prev, token) {
  frPsg.textContent = 'every release';
  let map;
  try { map = await loadTexts(); } catch { return; }
  if (token !== stepToken) return;
  // the V2.6 stage prints exactly the script above: swap to it where the reader already is, then settle on the passage
  const y = port.scrollTop;
  inHistory = true;
  hAt = stages.length - 1;
  hcode.innerHTML = map.get(stages[hAt].tag).map((l, k) => lineHtml(l, k + 1)).join('');
  code.hidden = true;
  hcode.hidden = false;
  port.scrollTop = y;
  paintStageFacts(hAt);
  const anchor = $(`.ln[data-n="${stages[hAt].anchor}"]`, hcode);
  await scrollPortTo(anchor.offsetTop - port.clientHeight * (phone.matches ? 0.12 : 0.2));
  drawRail();
  if (token !== stepToken) return;
  if (playing) playHistory();
}

function leaveHistory() {
  stopHistoryPlay();
  inHistory = false;
  hcode.hidden = true;
  code.hidden = false;
  drawRail();
}

function stopHistoryPlay() {
  hPlaying = false;
  hToken++;
  hPlay.setAttribute('aria-pressed', 'false');
  $('span', hPlay).textContent = 'Play every release';
  hPlay.setAttribute('aria-label', 'Play every release');
}

// rewind to medal, then step forward through every release; the passage changes only where the release changed it
async function playHistory() {
  if (reduced.matches) { await requestStage(stages.length - 1, false); return; }
  const token = ++hToken;
  hPlaying = true;
  hPlay.setAttribute('aria-pressed', 'true');
  $('span', hPlay).textContent = 'Pause';
  hPlay.setAttribute('aria-label', 'Pause');
  await sleep(500);
  if (token !== hToken) return;
  await requestStage(0);
  await sleep(1100);
  for (let i = 1; i < stages.length; i++) {
    if (token !== hToken) return;
    const changed = stages[i].changed;
    await requestStage(i, changed);
    await sleep(changed ? 820 : 170);
  }
  if (token !== hToken) return;
  stopHistoryPlay();
  if (playing) setPlaying(false);
}

hRange.addEventListener('input', () => {
  stopHistoryPlay();
  if (playing) stopPlay();
  requestStage(+hRange.value);
});
hPlay.addEventListener('click', () => {
  if (hPlaying) { stopHistoryPlay(); return; }
  playHistory();
});

// ------------------------------------------------------------------ Tovek in the browser

let tovek = null;
function loadTovek() {
  if (!tovek) {
    tovek = import(here('../tovek/tovek.js')).then(async (m) => {
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

const own = $('#read');
const outBody = $('#out-body');
const outName = $('#out-name');
const outFacts = $('#out-facts');
const outStatus = $('#out-status');
const chips = $$('.chip', own);
const fmtInt = (n) => n.toLocaleString('en-US');
let outSource = null;
let outFile = 'decompiled.luau';
let renderJob = 0;

function setStatus(html, busy = false) {
  outStatus.innerHTML = html;
  outStatus.classList.toggle('is-busy', busy);
}

for (const el of [own.querySelector('.own-in'), outBody]) {
  el.addEventListener('pointerenter', () => loadTovek(), { once: true });
  el.addEventListener('focusin', () => loadTovek(), { once: true });
  el.addEventListener('touchstart', () => loadTovek(), { once: true, passive: true });
}

function pressChip(chip) { for (const c of chips) c.setAttribute('aria-pressed', String(c === chip)); }

async function readChip(chip) {
  pressChip(chip);
  const title = chip.querySelector('span').textContent;
  setStatus(`Loading Tovek into this page…`, true);
  let t;
  try { t = await loadTovek(); } catch { setStatus('This browser cannot run Tovek here. Download it for your machine instead.'); return; }
  setStatus(`Decompiling ${escapeHtml(title)}…`, true);
  let bytes;
  let name;
  let sha;
  let same;
  if (chip.dataset.sample === 'Beacon') {
    bytes = new Uint8Array(await (await fetch(here('../../../' + chip.dataset.url))).arrayBuffer());
    name = chip.dataset.name;
    sha = chip.dataset.sha;
  } else {
    const row = t.rows.find((r) => r.id === chip.dataset.sample);
    bytes = await t.sampleBytes(row);
    name = row.name;
    sha = row.native.sha256;
  }
  const res = await t.decompile(bytes, { name });
  if (!res.error) same = (await sha256Hex(res.source)) === sha;
  show(res, { title: name.replace(/\.lua$/, '') + '.luau', bytes: bytes.length, version: t.info.version, same, beacon: chip.dataset.sample === 'Beacon' });
}
for (const c of chips) c.addEventListener('click', () => readChip(c));
const tryBtn = $('#out-try');
if (tryBtn) {
  tryBtn.addEventListener('pointerenter', () => loadTovek(), { once: true });
  tryBtn.addEventListener('click', () => readChip(chips[0]));
}

async function readFile(file) {
  pressChip(null);
  setStatus(`Loading Tovek into this page…`, true);
  let t;
  try { t = await loadTovek(); } catch { setStatus('This browser cannot run Tovek here. Download it for your machine instead.'); return; }
  setStatus(`Decompiling ${escapeHtml(file.name)}…`, true);
  const res = await t.decompile(file, { name: file.name, timeout: 60000 });
  show(res, { title: file.name.replace(/\.[^.]+$/, '') + '.luau', bytes: file.size, version: t.info.version, same: null });
}

$('#file').addEventListener('change', (e) => {
  const f = e.target.files && e.target.files[0];
  if (f) readFile(f);
  e.target.value = '';
});

const drop = $('#drop');
const veil = document.createElement('div');
veil.className = 'dropveil';
veil.innerHTML = '<p>Drop to decompile in your browser</p>';
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
  const r = own.getBoundingClientRect();
  if (r.top > innerHeight * 0.6 || r.bottom < 0) scrollTo({ top: own.offsetTop - top.offsetHeight, behavior: reduced.matches ? 'auto' : 'smooth' });
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
    const why = res.stage === 'input' ? 'This does not look like Luau bytecode or a saved script.'
      : res.stage === 'decompile' ? 'Tovek could not decompile this file.'
      : res.stage === 'timeout' ? 'This took too long and was stopped.' : 'Something went wrong while decompiling.';
    outBody.innerHTML = `<p class="out-error">${escapeHtml(res.error)}</p>`;
    setStatus(escapeHtml(why));
    return;
  }
  outSource = res.source;
  outFile = meta.title;
  const lines = res.source.split('\n');
  const ms = res.ms < 10 ? res.ms.toFixed(1) : Math.round(res.ms);
  outFacts.textContent = `${fmtInt(lines.length)} lines · ${ms} ms · Tovek ${meta.version}`;
  const box = document.createElement('div');
  box.className = 'out-code';
  const highlight = lines.length <= 4000;
  const CHUNK = 200;
  const chunks = [];
  for (let i = 0; i < lines.length; i += CHUNK) {
    const c = document.createElement('div');
    c.className = 'chunk';
    c.style.containIntrinsicSize = `auto ${Math.min(CHUNK, lines.length - i) * 19}px`;
    c.dataset.from = i;
    chunks.push(c);
  }
  const fill = (c) => { const i = +c.dataset.from; c.innerHTML = lines.slice(i, i + CHUNK).map((l, k) => lineHtml(l, i + k + 1, highlight)).join(''); };
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
  if (meta.beacon && meta.same) setStatus(`<b>Same ${fmtInt(lines.length)} lines as the script above, byte for byte.</b> ${nothing}`);
  else if (meta.same) setStatus(`<b>=</b> Byte for byte what the Tovek command line prints for this file. ${nothing}`);
  else if (meta.beacon) setStatus(`Tovek ${escapeHtml(meta.version)} in this page printed ${fmtInt(lines.length)} lines; the script above came from the V2.6 build. ${nothing}`);
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
  const host = sel.anchorNode && (sel.anchorNode.nodeType === 1 ? sel.anchorNode : sel.anchorNode.parentElement)?.closest('.code, .pane, .out-code');
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
  const hero = $('.hero .btn-ink');
  hero.href = dl.href;
  hero.title = `Tovek V2.6 for ${plat} (${dl.querySelector('span').textContent})`;
}

// ------------------------------------------------------------------ films: a card plays its film while the pointer rests on it

// The films are pure functions of time (watch/engine): the card draws its own film from the poster frame on, muted,
// in a canvas over the poster, and lets go of it when the pointer leaves.
const liveFilms = new Map();
const canHover = matchMedia('(hover: hover) and (pointer: fine)');
function liveFilm(card) {
  let entry = liveFilms.get(card);
  if (!entry) {
    entry = (async () => {
      const watch = new URL('../../../watch/', import.meta.url);
      const [registry, engine] = await Promise.all([import(new URL('films/index.js', watch).href), import(new URL('engine/index.js', watch).href)]);
      const f = registry.filmById(card.dataset.film);
      if (!f || !f.available) throw new Error('no film');
      const mod = await import(new URL(f.src, new URL('films/', watch)).href);
      const film = await engine.openFilm(mod, { base: watch.href });
      const canvas = document.createElement('canvas');
      canvas.className = 'film-live';
      canvas.setAttribute('aria-hidden', 'true');
      card.querySelector('.film-frame img').after(canvas);
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
  const frameEl = card.querySelector('.film-frame');
  let bar = frameEl.querySelector('.film-bar');
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
// warm a live card ahead of the pointer: once the row is near and the page has stopped scrolling, load the film and
// draw its poster frame once at the card's size (that fills the glyph caches), in idle time
function warmCards() {
  if (reduced.matches || !canHover.matches || !('IntersectionObserver' in window)) return;
  const cards = $$('.film[data-live]');
  if (!cards.length) return;
  let near = false;
  let still = 0;
  const idle = window.requestIdleCallback || ((fn) => setTimeout(fn, 200));
  const tryWarm = () => {
    if (!near) return;
    clearTimeout(still);
    still = setTimeout(() => idle(async () => {
      for (const card of cards) {
        try {
          const live = await liveFilm(card);
          if (live.warmed) continue;
          const r = card.querySelector('.film-frame').getBoundingClientRect();
          live.stage.resize(r.width, r.height, Math.min(devicePixelRatio || 1, 2));
          await new Promise((res) => idle(res, { timeout: 1500 }));
          live.stage.draw(live.film, live.t);
          live.warmed = true;
        } catch { /* the poster stays */ }
      }
    }, { timeout: 2000 }), 260);
  };
  new IntersectionObserver((entries) => { near = entries.some((e) => e.isIntersecting); tryWarm(); }, { rootMargin: '0px 0px 600px 0px' }).observe($('#films'));
  addEventListener('scroll', tryWarm, { passive: true });
}

for (const card of $$('.film[data-live]')) {
  card.addEventListener('pointerenter', (e) => { if (e.pointerType === 'mouse') playCard(card); });
  card.addEventListener('pointerleave', () => stopCard(card));
  card.addEventListener('focus', () => { if (card.matches(':focus-visible')) playCard(card); });
  card.addEventListener('blur', () => stopCard(card));
}

// ------------------------------------------------------------------ films: built with the registry's films; re-check here

async function films() {
  let registry;
  try { registry = await import(here('../../../watch/films/index.js')); } catch { return; }
  for (const card of $$('.film')) {
    const f = registry.filmById(card.dataset.film);
    if (!f || !f.available) card.hidden = true;
  }
  const launch = registry.filmById('v26');
  if (!launch || !launch.available) {
    const b = $('.btn-watch');
    if (b && b.getAttribute('href').includes('v26')) { b.setAttribute('href', 'watch/?film=story'); b.querySelector('span').textContent = 'Watch the film'; }
  }
}

// ------------------------------------------------------------------ start

function relayout() {
  drawRail();
  for (const s of steps) markMore(s);
  if (at >= 0 && !inHistory) {
    const line = anchorLine(steps[at]);
    if (line) {
      const y = portTarget(line);
      alignNote(steps[at], line, y);
      port.scrollTop = y;
    }
    buildLeader(false);
  }
}

function start() {
  const glide = prepareGlide();
  goTo(0, { instant: true });
  drawRail();
  if (glide) playGlide();
  let ticking = false;
  addEventListener('scroll', () => {
    if (ticking) return;
    ticking = true;
    requestAnimationFrame(() => { ticking = false; onScrollChrome(); });
  }, { passive: true });
  onScrollChrome();
  let rz = 0;
  addEventListener('resize', () => { cancelAnimationFrame(rz); rz = requestAnimationFrame(relayout); });
  phone.addEventListener('change', relayout);
  // the frame comes into view: its leader draws itself, and it starts turning its pages
  if ('IntersectionObserver' in window) {
    let first = true;
    new IntersectionObserver((entries) => {
      for (const en of entries) {
        inView = en.isIntersecting && en.intersectionRatio >= 0.5;
        if (inView && first) {
          first = false;
          buildLeader(true);
          if (!reduced.matches && !userStopped) { autoStarted = true; setPlaying(true); }
        }
      }
    }, { threshold: [0, 0.5, 0.75] }).observe(frame);
  }
  // prefetch what the last step needs once the reader is close
  const idle = window.requestIdleCallback || ((fn) => setTimeout(fn, 400));
  idle(() => { if (frame.getBoundingClientRect().top < innerHeight * 2) loadTexts(); }, { timeout: 3000 });
  markDownload();
  films();
  warmCards();
}

document.fonts.ready.then(() => requestAnimationFrame(start));
