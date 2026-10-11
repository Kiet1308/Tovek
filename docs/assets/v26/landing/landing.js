// Tovek landing: "The annotated script".
// The script, its notes and every number are already in the HTML (filled from data/landing.json by the data step).
// This module lays the notes out in the margin and draws their leaders, keeps the minimap and the running head in
// step with reading, turns any passage into its bytecode or its original source (the lens), and reads the
// visitor's own file with the in-browser build of Tovek.

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
const reduced = matchMedia('(prefers-reduced-motion: reduce)');
const narrow = matchMedia('(max-width: 899px)');
const EASE = 'cubic-bezier(0.16, 1, 0.3, 1)';
const here = (p) => new URL(p, import.meta.url).href;

const ms = $('#ms');
const code = $('#code');
const leaders = $('#leaders');
const runhead = $('#runhead');
const lens = $('#lens');
const crumbPsg = $('#crumb-psg');
const crumbFile = $('#crumb-file');
const passages = $$('.psg', code);
const notes = $$('.note', code);
const noteById = new Map(notes.map((n) => [n.id, n]));

let data = null;            // landing.json, fetched when first needed
let history = null;         // landing-history.json
let laidOut = false;
let reading = null;         // { title, ... } while the column shows another file
let current = null;         // the passage at the reading line
let onNote = null;          // note lit by hover or focus
let nearNote = null;        // note nearest the reading line
const views = new Map(passages.map((p) => [p.id, 'tovek']));

function loadData() {
  if (!data) data = fetch(here('../../../data/landing.json')).then((r) => r.json()).then((d) => (data = d));
  return Promise.resolve(data);
}

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

function tokensHtml(tokens) {
  let h = '';
  for (const [kind, text] of tokens) {
    if (kind === 'i' || kind === 'x') h += escapeHtml(text);
    else h += `<span class="${kind}">${escapeHtml(text)}</span>`;
  }
  return h;
}

function lineHtml(text, n, extraClass = '', highlight = true) {
  const ind = text.length - text.replace(/^\t+/, '').length;
  const body = text.slice(ind);
  const t = body ? (highlight ? tokensHtml(tokenize(body)) : escapeHtml(body)) : '';
  return `<div class="ln${body ? '' : ' blank'}${extraClass ? ' ' + extraClass : ''}" data-n="${n}" style="--i:${ind}"><span class="t">${t}</span></div>`;
}

// ------------------------------------------------------------------ header and running head

const top = $('#top');
let stuck = false;
let solid = false;
function onScrollChrome() {
  const so = scrollY > 8;
  if (so !== solid) { solid = so; top.classList.toggle('is-solid', so); }
  const st = scrollY + geo.topH >= geo.runheadTop - 0.5 && scrollY + geo.topH < geo.msEnd;
  if (st !== stuck) { stuck = st; runhead.classList.toggle('is-stuck', st); }
}

// ------------------------------------------------------------------ geometry cache (refreshed on layout)

let geo = { lines: [], notes: [], passages: [], codeTop: 0, codeHeight: 1, lineCount: 0, topH: 64, rhH: 52,
  runheadTop: 1e9, msEnd: 1e9, railH: 1, stripW: 1 };

function docTop(el) { return el.getBoundingClientRect().top + scrollY; }

function measure() {
  const lines = $$('.psg-body:not([hidden]) > .ln, .psg-alt:not([hidden]) > .ln, .reading .ln', code);
  geo.codeTop = docTop(code);
  geo.codeHeight = code.offsetHeight;
  geo.topH = top.offsetHeight;
  geo.rhH = runhead.offsetHeight;
  const wasStuck = runhead.classList.contains('is-stuck');
  geo.runheadTop = docTop(runhead.parentElement) ;
  geo.msEnd = docTop(runhead.parentElement) + runhead.parentElement.offsetHeight;
  geo.railH = railMap.clientHeight || 1;
  geo.stripW = stripCanvas.clientWidth || 1;
  void wasStuck;
  geo.passages = passages.filter((p) => !p.hidden).map((p) => ({ el: p, top: docTop(p), bottom: docTop(p) + p.offsetHeight }));
  geo.notes = notes.filter((n) => n.offsetParent && !n.classList.contains('is-hidden')).map((n) => {
    const line = $(`#L${n.dataset.line}`);
    return { el: n, id: n.id, line: +n.dataset.line, top: line ? docTop(line) : 0 };
  });
  geo.lineCount = lines.length;
}

// ------------------------------------------------------------------ margin notes and leaders (wide screens)

const NS = 'http://www.w3.org/2000/svg';

function lineEnd(lineEl, msRect) {
  const t = $('.t', lineEl);
  const range = document.createRange();
  range.selectNodeContents(t);
  const rects = Array.from(range.getClientRects()).filter((r) => r.width > 0);
  const last = rects[rects.length - 1] || t.getBoundingClientRect();
  const lr = lineEl.getBoundingClientRect();
  return { x: last.right - msRect.left, y: (rects.length ? last.top + last.height / 2 : lr.top + lr.height / 2) - msRect.top };
}

function layoutNotes() {
  if (narrow.matches || reading) {
    for (const n of notes) n.style.top = '';
    leaders.replaceChildren();
    return;
  }
  const msRect = ms.getBoundingClientRect();
  const codeRect = code.getBoundingClientRect();
  const visible = notes.filter((n) => n.offsetParent && !n.classList.contains('is-hidden'));
  const GAP = 26;
  const items = visible.map((n) => {
    const line = $(`#L${n.dataset.line}`);
    const lr = line.getBoundingClientRect();
    return { n, line, ideal: lr.top - msRect.top - 3, h: n.offsetHeight };
  });
  // each note as close to its line as order and spacing allow: least squares under "in order, never overlapping",
  // solved exactly by pooling adjacent violators on the offsets
  let acc = 0;
  for (const it of items) { it.S = acc; it.t = it.ideal - acc; acc += it.h + GAP; }
  const pools = [];
  for (const it of items) {
    pools.push({ sum: it.t, n: 1 });
    while (pools.length > 1) {
      const b = pools[pools.length - 1];
      const a = pools[pools.length - 2];
      if (a.sum / a.n <= b.sum / b.n) break;
      a.sum += b.sum;
      a.n += b.n;
      pools.pop();
    }
  }
  let k = 0;
  for (const pool of pools) for (let c = 0; c < pool.n; c++, k++) items[k].y = pool.sum / pool.n + items[k].S;
  let y = codeRect.top - msRect.top + 18;
  for (const it of items) { it.y = Math.max(it.y, y); y = it.y + it.h + GAP; }
  const limit = codeRect.bottom - msRect.top + 40;
  for (let i = items.length - 1; i >= 0; i--) {
    const cap = i === items.length - 1 ? limit - items[i].h : items[i + 1].y - GAP - items[i].h;
    if (items[i].y > cap) items[i].y = Math.max(items[i].ideal - 600, cap);
  }
  for (const it of items) {
    const psgTop = it.n.offsetParent.getBoundingClientRect().top - msRect.top;
    it.n.style.top = `${Math.round(it.y - psgTop)}px`;
  }
  // leaders: from the end of the anchor line, out past the code column, then over to the note's kicker
  const frag = document.createDocumentFragment();
  const bendX = codeRect.right - msRect.left + 12;
  for (const it of items) {
    const a = lineEnd(it.line, msRect);
    const ax = Math.min(a.x + 10, codeRect.right - msRect.left - 6);
    const nx = it.n.getBoundingClientRect().left - msRect.left - 10;
    const ny = it.y + 9;
    const g = document.createElementNS(NS, 'g');
    g.dataset.note = it.n.id;
    const p = document.createElementNS(NS, 'path');
    const mid = (bendX + nx) / 2;
    const dy = ny - a.y;
    const d = Math.abs(dy) < 1
      ? `M${ax} ${a.y}H${nx}`
      : `M${ax} ${a.y}H${bendX}C${mid} ${a.y} ${mid} ${ny} ${nx} ${ny}`;
    p.setAttribute('d', d);
    p.setAttribute('pathLength', '1');
    if (it.n.classList.contains('seen')) g.classList.add('seen');
    const c = document.createElementNS(NS, 'circle');
    c.setAttribute('cx', ax);
    c.setAttribute('cy', a.y);
    c.setAttribute('r', 2.5);
    g.append(p, c);
    frag.append(g);
  }
  leaders.replaceChildren(frag);
  leaders.setAttribute('width', msRect.width);
  leaders.setAttribute('height', msRect.height);
  paintNoteStates();
}

let layoutQueued = false;
function relayout() {
  if (layoutQueued) return;
  layoutQueued = true;
  requestAnimationFrame(() => {
    layoutQueued = false;
    layoutNotes();
    measure();
    drawMaps();
    lastView = '';
    onScroll();
    if (!laidOut) { laidOut = true; ms.classList.add('is-laid'); }
  });
}

// ------------------------------------------------------------------ note states: lit (hover/focus) and near (reading)

function paintNoteStates() {
  for (const n of notes) {
    n.classList.toggle('is-on', n.id === onNote);
    n.classList.toggle('is-near', n.id === nearNote && n.id !== onNote);
  }
  for (const m of $$('mark.ns', code)) {
    m.classList.toggle('is-on', m.dataset.note === onNote);
    m.classList.toggle('is-near', m.dataset.note === nearNote && m.dataset.note !== onNote);
  }
  for (const g of leaders.children) {
    g.classList.toggle('is-on', g.dataset.note === onNote);
    g.classList.toggle('is-near', g.dataset.note === nearNote && g.dataset.note !== onNote);
  }
  for (const m of $$('.rail-mark')) m.classList.toggle('is-on', m.dataset.note === (onNote || nearNote));
}

function setOn(id) {
  if (onNote === id) return;
  onNote = id;
  paintNoteStates();
  // bring the lit note's lines to the front of the rail
  for (const l of $$('.ln.is-hot', code)) l.classList.remove('is-hot');
  if (id) {
    const n = noteById.get(id);
    const line = n && $(`#L${n.dataset.line}`);
    if (line) line.classList.add('is-hot');
  }
}

code.addEventListener('pointerover', (e) => {
  if (e.pointerType === 'touch') return;
  const hit = e.target.closest('mark.ns, .note');
  if (hit) setOn(hit.dataset.note || hit.id);
});
code.addEventListener('pointerout', (e) => {
  if (e.pointerType === 'touch') return;
  const from = e.target.closest('mark.ns, .note');
  const to = e.relatedTarget && e.relatedTarget.closest && e.relatedTarget.closest('mark.ns, .note');
  if (from && (!to || (to.dataset.note || to.id) !== (from.dataset.note || from.id))) setOn(null);
});
code.addEventListener('focusin', (e) => {
  const n = e.target.closest('.note');
  if (n) setOn(n.id);
});
code.addEventListener('focusout', (e) => {
  if (e.target.closest('.note')) setOn(null);
});

// phones: a marker opens its note under the line
code.addEventListener('click', (e) => {
  const m = e.target.closest('.nm');
  if (!m) return;
  const n = noteById.get(m.dataset.note);
  const open = !n.classList.contains('is-open');
  n.classList.toggle('is-open', open);
  m.setAttribute('aria-expanded', String(open));
  setOn(open ? n.id : null);
  if (open) setTimeout(() => measure(), 420);
});

// ------------------------------------------------------------------ reading position: current passage, near note, maps

function readingY() { return scrollY + Math.min(innerHeight * 0.4, 360); }

function onScroll() {
  onScrollChrome();
  const y = readingY();
  if (!reading) {
    let cur = null;
    for (const p of geo.passages) if (p.top <= y) cur = p.el;
    if (!cur && geo.passages.length) cur = geo.passages[0].el;
    if (cur !== current) setCurrent(cur);
    let near = null;
    for (const n of geo.notes) if (n.top <= y + 40) near = n.id;
    if (scrollY + innerHeight < geo.codeTop + 80) near = null;
    if (near !== nearNote) { nearNote = near; paintNoteStates(); }
  }
  moveMapViews();
}

function setCurrent(p) {
  if (current) current.classList.remove('is-current');
  current = p;
  if (!p) return;
  p.classList.add('is-current');
  crumbPsg.textContent = p.dataset.title;
  const v = views.get(p.id);
  for (const b of $$('button', lens)) b.setAttribute('aria-checked', String(b.dataset.view === v));
}

// ------------------------------------------------------------------ the minimap rail (wide) and strip (phones)

const rail = $('#rail');
const railMap = $('.rail-map', rail);
const railView = $('.rail-view', rail);
const railMarks = $('#rail-marks');
const strip = $('#strip');
const stripCanvas = $('canvas', strip);
const stripView = $('.strip-view', strip);
let mapLines = [];   // [{ top, indent, len, kind }] in code-column coordinates

function collectMapLines() {
  const codeTop = code.getBoundingClientRect().top;
  const rows = $$('.ln', code).filter((l) => l.offsetParent);
  return rows.map((l) => {
    const t = $('.t', l);
    const text = t ? t.textContent : '';
    const r = l.getBoundingClientRect();
    return { top: r.top - codeTop, h: r.height, indent: +(l.style.getPropertyValue('--i') || 0), len: text.length,
      rc: !!$('.r', l), bc: l.classList.contains('bc'), from: l.classList.contains('from') };
  });
}

function sizeCanvas(c) {
  const dpr = Math.min(devicePixelRatio || 1, 3);
  const w = c.clientWidth;
  const h = c.clientHeight;
  if (!w || !h) return null;
  if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) { c.width = Math.round(w * dpr); c.height = Math.round(h * dpr); }
  const ctx = c.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  return { ctx, w, h };
}

function drawMaps() {
  mapLines = collectMapLines();
  const total = Math.max(1, code.offsetHeight);
  // rail: lines run down
  if (!narrow.matches) {
    const s = sizeCanvas(railMap);
    if (s) {
      const { ctx, w, h } = s;
      const k = h / total;
      for (const l of mapLines) {
        if (!l.len) continue;
        const y = l.top * k;
        const bh = Math.max(1, Math.min(l.h * k * 0.62, 3));
        const x = Math.min(l.indent * 2.2, w * 0.4);
        const bw = Math.max(1.5, Math.min(l.len * 0.24, w - x));
        ctx.fillStyle = l.rc || l.from ? 'rgba(214, 58, 12, 0.85)' : 'rgba(18, 18, 16, 0.26)';
        ctx.fillRect(x, y, bw, bh);
      }
    }
    railMarks.replaceChildren();
    if (!reading) {
      const railH = railMap.clientHeight;
      for (const n of notes) {
        if (!n.offsetParent || n.classList.contains('is-hidden')) continue;
        const line = $(`#L${n.dataset.line}`);
        if (!line) continue;
        const top = line.getBoundingClientRect().top - code.getBoundingClientRect().top;
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'rail-mark';
        b.dataset.note = n.id;
        b.tabIndex = -1;
        b.style.top = `${(top / total) * railH}px`;
        b.setAttribute('aria-label', `Note ${$('.nn', n).textContent}: ${$('.nkind', n).textContent}, line ${n.dataset.line}`);
        railMarks.append(b);
      }
    }
  }
  // strip: lines run across
  if (narrow.matches) {
    const s = sizeCanvas(stripCanvas);
    if (s) {
      const { ctx, w, h } = s;
      const k = w / total;
      for (const l of mapLines) {
        if (!l.len) continue;
        const x = l.top * k;
        const bw = Math.max(0.8, l.h * k * 0.7);
        const bh = Math.min(l.len * 0.14, h - 10);
        ctx.fillStyle = l.rc || l.from ? 'rgba(214, 58, 12, 0.9)' : 'rgba(18, 18, 16, 0.28)';
        ctx.fillRect(x, h - 5 - bh, bw, bh);
      }
      if (!reading) {
        for (const n of notes) {
          const line = $(`#L${n.dataset.line}`);
          if (!line || !line.offsetParent) continue;
          const top = line.getBoundingClientRect().top - code.getBoundingClientRect().top;
          ctx.fillStyle = '#d63a0c';
          ctx.beginPath();
          ctx.arc((top / total) * w, 4, 2.2, 0, Math.PI * 2);
          ctx.fill();
        }
      }
    }
  }
  moveMapViews();
}

let lastView = '';
function moveMapViews() {
  const total = Math.max(1, geo.codeHeight);
  const visTop = scrollY + geo.topH + geo.rhH - geo.codeTop;
  const visH = innerHeight - geo.topH - geo.rhH;
  const a = Math.max(0, Math.min(total, visTop));
  const b = Math.max(0, Math.min(total, visTop + visH));
  const key = `${narrow.matches}|${Math.round(a)}|${Math.round(b)}|${geo.railH}|${geo.stripW}`;
  if (key === lastView) return;
  lastView = key;
  if (!narrow.matches) {
    railView.style.transform = `translateY(${(a / total) * geo.railH}px)`;
    railView.style.height = `${Math.max(10, ((b - a) / total) * geo.railH)}px`;
  } else {
    stripView.style.transform = `translateX(${(a / total) * geo.stripW}px)`;
    stripView.style.width = `${Math.max(8, ((b - a) / total) * geo.stripW)}px`;
  }
}

function scrollToCodeY(y, smooth = true) {
  const target = geo.codeTop + y - (top.offsetHeight + runhead.offsetHeight) - 24;
  scrollTo({ top: Math.max(0, target), behavior: smooth && !reduced.matches ? 'smooth' : 'auto' });
}

function mapPointer(e, el, horizontal) {
  const r = el.getBoundingClientRect();
  const f = horizontal ? (e.clientX - r.left) / r.width : (e.clientY - r.top) / r.height;
  const total = code.offsetHeight;
  const visH = innerHeight - top.offsetHeight - runhead.offsetHeight;
  return Math.max(0, f * total - visH / 2 + 24);
}

let railDrag = false;
railMap.addEventListener('pointerdown', (e) => {
  railDrag = true;
  railMap.setPointerCapture(e.pointerId);
  scrollToCodeY(mapPointer(e, railMap, false), false);
});
railMap.addEventListener('pointermove', (e) => { if (railDrag) scrollToCodeY(mapPointer(e, railMap, false), false); });
railMap.addEventListener('pointerup', () => { railDrag = false; });
railMarks.addEventListener('click', (e) => {
  const b = e.target.closest('.rail-mark');
  if (!b) return;
  goToNote(b.dataset.note);
});
// rail as one keyboard stop: arrows walk the notes
rail.tabIndex = 0;
rail.setAttribute('aria-label', 'Minimap of the script. Use the arrow keys to move between notes.');
rail.addEventListener('keydown', (e) => {
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(e.key)) return;
  e.preventDefault();
  const list = geo.notes;
  if (!list.length) return;
  const idx = list.findIndex((n) => n.id === (onNote || nearNote));
  let next = idx;
  if (e.key === 'ArrowDown') next = Math.min(list.length - 1, idx + 1);
  if (e.key === 'ArrowUp') next = Math.max(0, idx - 1);
  if (e.key === 'Home') next = 0;
  if (e.key === 'End') next = list.length - 1;
  goToNote(list[next].id);
});
const railTip = document.createElement('span');
railTip.className = 'rail-tip';
rail.querySelector('.rail-in').append(railTip);
railMarks.addEventListener('pointerover', (e) => {
  const b = e.target.closest('.rail-mark');
  if (!b) return;
  railTip.textContent = b.getAttribute('aria-label');
  railTip.style.top = b.style.top;
  railTip.classList.add('is-on');
});
railMarks.addEventListener('pointerout', () => railTip.classList.remove('is-on'));

let stripDrag = false;
strip.addEventListener('pointerdown', (e) => {
  stripDrag = true;
  strip.setPointerCapture(e.pointerId);
  scrollToCodeY(mapPointer(e, stripCanvas, true), false);
});
strip.addEventListener('pointermove', (e) => { if (stripDrag) scrollToCodeY(mapPointer(e, stripCanvas, true), false); });
strip.addEventListener('pointerup', () => { stripDrag = false; });

function goToNote(id) {
  const n = noteById.get(id);
  if (!n) return;
  const line = $(`#L${n.dataset.line}`);
  if (!line) return;
  scrollToCodeY(line.getBoundingClientRect().top - code.getBoundingClientRect().top - innerHeight * 0.18);
  setOn(id);
  if (narrow.matches && !n.classList.contains('is-open')) {
    const m = $(`.nm[data-note="${id}"]`);
    if (m) m.click();
  }
}

// ------------------------------------------------------------------ the lens: a passage as bytecode, as Tovek, as written

function wrapWords(container) {
  // identifier text becomes its own span so every token can move on its own
  for (const t of $$('.t', container)) {
    const walker = document.createTreeWalker(t, NodeFilter.SHOW_TEXT);
    const texts = [];
    while (walker.nextNode()) texts.push(walker.currentNode);
    for (const node of texts) {
      if (node.parentElement.classList.contains('t') || node.parentElement.tagName === 'MARK') {
        const parts = node.textContent.split(/(\s+)/);
        if (parts.length === 1 && !parts[0]) continue;
        const frag = document.createDocumentFragment();
        for (const part of parts) {
          if (!part) continue;
          if (/^\s+$/.test(part)) frag.append(part);
          else { const s = document.createElement('span'); s.className = 'w'; s.textContent = part; frag.append(s); }
        }
        node.replaceWith(frag);
      }
    }
  }
  container.classList.add('tk');
}

function tokenKey(el) {
  let t = el.textContent.trim();
  if (!t) return null;
  if (el.classList.contains('op') || el.classList.contains('lbl') || el.classList.contains('bc-from')) return null;
  if (/^R\d+$|^K\d+$|^L\d+:?$|^\+\d+$/.test(t)) return null;
  t = t.replace(/^\[|\]$/g, '').replace(/^['"`]|['"`]$/g, '').replace(/^\}|\{$/g, '');
  return t || null;
}

function tokensOf(container) {
  return $$('.t span:not(:has(span))', container).filter((s) => s.offsetParent).map((el) => ({ el, key: tokenKey(el) }));
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

const SPACER = '<div class="ln blank" data-n="" style="--i:0"><span class="t"></span></div>';

function bytecodeHtml(p) {
  const rows = [];
  const multi = p.bytecode.filter((g) => !g.main).length > 1 || (p.kind === 'fn' && p.bytecode.some((g) => g.main));
  let count = 0;
  for (const g of p.bytecode) {
    if (multi) {
      const label = g.main ? 'main chunk: the function is made and stored'
        : `function ${g.name || '(anonymous)'}, defined on line ${g.line_defined}`;
      rows.push(`<div class="ln fnhead" data-n="" style="--i:0"><span class="t">${escapeHtml(label)}</span></div>`);
    }
    let prevLine = null;
    let prevFrom = null;
    for (const r of g.rows) {
      count++;
      const cont = r.line === prevLine;
      const from = r.from;
      const label = `<span class="lbl">${r.label ? r.label + ':' : ''}</span>`;
      const args = (r.args.match(/\[[^\]]*\]|\S+/g) || [])
        .map((a) => `<span class="${a.startsWith('[') ? 's' : 'a'}">${escapeHtml(a)}</span>`).join(' ');
      const tag = from && from !== prevFrom ? `<span class="bc-from">pasted from ${escapeHtml(from)}</span>` : '';
      rows.push(`<div class="ln bc${cont ? ' cont' : ''}${from ? ' from' : ''}" data-n="${r.line ?? ''}" style="--i:0"><span class="t">${label}<span class="op">${r.op}</span> ${args}${tag}</span></div>`);
      prevLine = r.line;
      prevFrom = from;
    }
  }
  const tag = `<p class="psg-tag"><b>Bytecode</b><span>${count} instructions</span><span>numbers are the source lines each one carries</span></p>`;
  return tag + rows.join('') + SPACER;
}

function sourceHtml(p, d) {
  const lines = d.script.source.split('\n');
  const [a, b] = p.src;
  const rows = [];
  for (let i = a; i <= b; i++) rows.push(lineHtml(lines[i - 1], i, 'src'));
  const tag = `<p class="psg-tag"><b>Source</b><span>lines ${a}–${b} of ${escapeHtml(d.script.file)}, as written</span></p>`;
  return tag + rows.join('') + SPACER;
}

let morphing = false;

async function setView(p, view) {
  if (!p || morphing || reading) return;
  const prev = views.get(p.id);
  if (prev === view) return;
  const d = await loadData();
  morphing = true;
  const body = $('.psg-body', p);
  let alt = $(`.psg-alt[data-view="${view}"]`, p);
  if (view !== 'tovek' && !alt) {
    alt = document.createElement('div');
    alt.className = 'psg-alt';
    alt.dataset.view = view;
    alt.hidden = true;
    const info = d.passages.find((q) => q.id === p.id);
    alt.innerHTML = view === 'bytecode' ? bytecodeHtml(info) : sourceHtml(info, d);
    wrapWords(alt);
    p.append(alt);
  }
  const oldEl = prev === 'tovek' ? body : $(`.psg-alt[data-view="${prev}"]`, p);
  const newEl = view === 'tovek' ? body : alt;
  if (!body.classList.contains('tk')) wrapWords(body);

  const animate = !reduced.matches;
  const codeRect0 = code.getBoundingClientRect();
  const before = animate ? tokensOf(oldEl).map((t) => ({ ...t, rect: t.el.getBoundingClientRect() })) : [];
  const after = passages.slice(passages.indexOf(p) + 1);
  const afterTop0 = after.length ? after[0].getBoundingClientRect().top : 0;
  const pTop0 = p.getBoundingClientRect().top;

  leaders.classList.add('is-quiet');
  oldEl.hidden = true;
  newEl.hidden = false;
  views.set(p.id, view);
  p.dataset.view = view;
  for (const n of notes) if (n.dataset.passage === p.id) n.classList.toggle('is-hidden', view !== 'tovek');
  for (const b of $$('button', lens)) b.setAttribute('aria-checked', String(b.dataset.view === view));
  // keep the passage where the reader is looking
  const shift = p.getBoundingClientRect().top - pTop0;
  if (shift) scrollBy(0, shift);
  layoutNotes();

  if (animate) {
    const codeRect = code.getBoundingClientRect();
    const now = tokensOf(newEl).map((t) => ({ ...t, rect: t.el.getBoundingClientRect() }));
    const pairs = lcsPairs(before, now);
    const matchedNew = new Set();
    const matchedOld = new Set();
    const vh = innerHeight;
    const anims = [];
    for (const [i, j] of pairs) {
      const o = before[i].rect;
      const n = now[j].rect;
      if ((o.bottom < -200 && n.bottom < -200) || (o.top > vh + 200 && n.top > vh + 200)) continue;
      matchedNew.add(j);
      matchedOld.add(i);
      const dx = o.left - n.left;
      const dy = (o.top - codeRect0.top) - (n.top - codeRect.top);
      if (Math.abs(dx) < 0.5 && Math.abs(dy) < 0.5) continue;
      anims.push(now[j].el.animate([{ transform: `translate(${dx}px, ${dy}px)` }, { transform: 'none' }],
        { duration: 680, easing: EASE, delay: Math.min(140, Math.abs(dy) * 0.08) }));
    }
    now.forEach((t, j) => {
      if (matchedNew.has(j)) return;
      if (t.rect.bottom < -50 || t.rect.top > vh + 50) return;
      anims.push(t.el.animate([{ opacity: 0, transform: 'translateY(5px)' }, { opacity: 1, transform: 'none' }],
        { duration: 420, easing: EASE, delay: 180 + Math.min(240, (t.rect.top - codeRect.top) * 0.05), fill: 'backwards' }));
    });
    // the old view's unmatched tokens leave from where they were
    const ghosts = document.createElement('div');
    ghosts.className = 'morph-ghosts';
    const cs = getComputedStyle(code);
    before.forEach((t, i) => {
      if (matchedOld.has(i)) return;
      if (t.rect.bottom < -50 || t.rect.top > vh + 50) return;
      const s = document.createElement('span');
      s.className = t.el.className;
      s.textContent = t.el.textContent;
      s.style.left = `${t.rect.left - codeRect0.left}px`;
      s.style.top = `${t.rect.top - codeRect0.top}px`;
      s.style.font = cs.font;
      if (t.el.closest('.r')) s.classList.add('r');
      ghosts.append(s);
    });
    code.append(ghosts);
    const fade = ghosts.animate([{ opacity: 1 }, { opacity: 0, filter: 'blur(2px)' }], { duration: 260, easing: 'ease-out', fill: 'forwards' });
    fade.finished.then(() => ghosts.remove());
    // what follows the passage slides to its new place
    if (after.length) {
      const dy = afterTop0 - after[0].getBoundingClientRect().top + shift;
      if (Math.abs(dy) > 0.5) {
        for (const q of after.slice(0, 4)) anims.push(q.animate([{ transform: `translateY(${dy}px)` }, { transform: 'none' }], { duration: 620, easing: EASE }));
      }
    }
    await Promise.all(anims.map((a) => a.finished.catch(() => {})));
  }
  morphing = false;
  leaders.classList.remove('is-quiet');
  relayout();
}

lens.addEventListener('click', (e) => {
  const b = e.target.closest('button[data-view]');
  if (b && current) setView(current, b.dataset.view);
});
lens.addEventListener('keydown', (e) => {
  if (!['ArrowLeft', 'ArrowRight'].includes(e.key)) return;
  const bs = $$('button', lens);
  const i = bs.indexOf(document.activeElement);
  const j = (i + (e.key === 'ArrowRight' ? 1 : bs.length - 1)) % bs.length;
  bs[j].focus();
  bs[j].click();
  e.preventDefault();
});
for (const b of $$('button', lens)) b.addEventListener('pointerenter', () => loadData(), { once: true });

// copying from the column keeps the indentation (it is drawn as padding, not tabs)
document.addEventListener('copy', (e) => {
  const sel = getSelection();
  if (!sel.rangeCount || !code.contains(sel.anchorNode)) return;
  const range = sel.getRangeAt(0);
  const lines = $$('.ln', code).filter((l) => l.offsetParent && range.intersectsNode(l));
  if (lines.length < 2) return;
  const text = lines.map((l) => {
    const t = $('.t', l);
    const r = document.createRange();
    r.selectNodeContents(t);
    if (range.compareBoundaryPoints(Range.START_TO_START, r) > 0 && t.contains(range.startContainer)) r.setStart(range.startContainer, range.startOffset);
    if (range.compareBoundaryPoints(Range.END_TO_END, r) < 0 && t.contains(range.endContainer)) r.setEnd(range.endContainer, range.endOffset);
    const ind = +(l.style.getPropertyValue('--i') || 0);
    return '\t'.repeat(ind) + r.toString();
  }).join('\n');
  e.clipboardData.setData('text/plain', text);
  e.preventDefault();
});

// ------------------------------------------------------------------ Tovek in the browser

let tovek = null;
function loadTovek() {
  if (!tovek) {
    tovek = import(here('../tovek/tovek.js')).then(async (m) => {
      const info = await m.load();
      return { ...m, info };
    });
    tovek.catch(() => { tovek = null; });
  }
  return tovek;
}

async function sha256Hex(text) {
  const bytes = new TextEncoder().encode(text);
  const h = await crypto.subtle.digest('SHA-256', bytes);
  return Array.from(new Uint8Array(h), (b) => b.toString(16).padStart(2, '0')).join('');
}

const reader = $('#reader');
const ownOpen = $('#own-open');
const status = $('#reader-status');
const samplesList = $('#samples');
const readcard = $('#readcard');
let samplesReady = false;

function setStatus(text, busy = false) {
  status.textContent = text;
  status.classList.toggle('is-busy', busy);
}

async function openReader(focus = true) {
  const open = reader.hidden;
  if (!open) { reader.hidden = true; ownOpen.setAttribute('aria-expanded', 'false'); relayout(); return; }
  reader.hidden = false;
  ownOpen.setAttribute('aria-expanded', 'true');
  relayout();
  const r = runhead.getBoundingClientRect();
  if (r.top > innerHeight * 0.5 || r.bottom < 0) scrollTo({ top: docTop(runhead) - top.offsetHeight, behavior: reduced.matches ? 'auto' : 'smooth' });
  if (focus) $('#file').focus({ preventScroll: true });
  if (!samplesReady) {
    samplesReady = true;
    setStatus('Loading Tovek into this page…', true);
    try {
      const t = await loadTovek();
      const rows = await t.samples();
      for (const row of rows) {
        if (row.id === 'SpawnShield.client') continue;
        const li = document.createElement('li');
        const b = document.createElement('button');
        b.type = 'button';
        b.textContent = row.title;
        b.title = row.about;
        b.setAttribute('aria-pressed', 'false');
        b.addEventListener('click', () => readSample(row, b));
        li.append(b);
        samplesList.append(li);
      }
      setStatus(`Ready: Tovek ${t.info.version} runs in this page. Nothing leaves your browser.`);
    } catch (err) {
      setStatus('This browser cannot run Tovek here. Download it for your machine instead.');
    }
  }
}

ownOpen.addEventListener('click', (e) => openReader(e.detail === 0));
$('#own-open-2').addEventListener('click', (e) => { if (reader.hidden) openReader(e.detail === 0); else $('#file').focus(); });
for (const el of [ownOpen, $('#own-open-2'), $('#check-page')]) {
  el.addEventListener('pointerenter', () => loadTovek(), { once: true });
  el.addEventListener('focus', () => loadTovek(), { once: true });
}

async function readSample(row, button) {
  for (const b of $$('button', samplesList)) b.setAttribute('aria-pressed', String(b === button));
  setStatus(`Decompiling ${row.title}…`, true);
  const t = await loadTovek();
  const bytes = await t.sampleBytes(row);
  const res = await t.decompile(bytes, { name: row.name });
  let same = null;
  if (!res.error) same = (await sha256Hex(res.source)) === row.native.sha256;
  setStatus('');
  showResult(res, { title: row.title, kicker: 'A sample, decompiled in your browser', bytes: bytes.length, origin: row.origin, same, version: t.info });
}

async function readFile(file) {
  if (reader.hidden) await openReader(false);
  for (const b of $$('button', samplesList)) b.setAttribute('aria-pressed', 'false');
  setStatus(`Decompiling ${file.name}…`, true);
  try {
    const t = await loadTovek();
    const res = await t.decompile(file, { name: file.name, timeout: 60000 });
    setStatus('');
    showResult(res, { title: file.name, kicker: 'Your file, decompiled in your browser', bytes: file.size, version: t.info });
  } catch (err) {
    setStatus('This browser cannot run Tovek here. Download it for your machine instead.');
  }
}

$('#file').addEventListener('change', (e) => {
  const f = e.target.files && e.target.files[0];
  if (f) readFile(f);
  e.target.value = '';
});

// drop anywhere on the page
const veil = document.createElement('div');
veil.className = 'dropveil';
veil.innerHTML = '<p>Drop to decompile in your browser</p>';
document.body.append(veil);
let dragDepth = 0;
addEventListener('dragenter', (e) => {
  if (!e.dataTransfer || !Array.from(e.dataTransfer.types).includes('Files')) return;
  dragDepth++;
  veil.classList.add('is-on');
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
  readFile(f);
});

const fmtInt = (n) => n.toLocaleString('en-US');

function showResult(res, meta) {
  if (res.error) {
    readcard.hidden = false;
    readcard.innerHTML = `<p class="rc-kicker">${escapeHtml(meta.kicker)}</p><h3 class="rc-title">${escapeHtml(meta.title)}</h3>
      <p class="rc-error">${escapeHtml(res.error)}</p>
      <p class="rc-note">${res.stage === 'input' ? 'This does not look like Luau bytecode or a saved script.' : res.stage === 'decompile' ? 'Tovek could not decompile this file.' : res.stage === 'timeout' ? 'This took too long and was stopped.' : 'Something went wrong while decompiling.'}</p>
      <p class="rc-actions"><button class="btn" type="button" data-act="back">Back to the annotated script</button></p>`;
    enterReading(meta.title, null);
    return;
  }
  const lines = res.source.split('\n');
  const facts = [
    ['Input', `${fmtInt(meta.bytes)} bytes${res.form === 'text' ? ', saved text' : ''}`],
    ['Bytecode', res.key === 203 ? 'Roblox client' : 'plain Luau'],
    ['Output', `${fmtInt(lines.length)} lines`],
    ['Time', `${res.ms < 10 ? res.ms.toFixed(1) : Math.round(res.ms)} ms, in this page`],
    ['Decompiler', `Tovek ${meta.version.version}`],
  ];
  readcard.hidden = false;
  readcard.innerHTML = `<p class="rc-kicker">${escapeHtml(meta.kicker)}</p><h3 class="rc-title">${escapeHtml(meta.title)}</h3>
    <dl class="rc-facts">${facts.map(([a, b]) => `<div><dt>${a}</dt><dd>${escapeHtml(b)}</dd></div>`).join('')}</dl>
    ${meta.same === true ? '<p class="rc-same"><b>=</b><span>Byte for byte what the Tovek command line prints for this file.</span></p>' : ''}
    <p class="rc-note">Nothing was uploaded: the file never left this page.</p>
    <p class="rc-actions"><button class="btn btn-ink" type="button" data-act="copy">Copy</button><button class="btn" type="button" data-act="save">Save .luau</button><button class="btn" type="button" data-act="back">Back to the annotated script</button></p>`;
  readcard.dataset.name = meta.title.replace(/\.[^.]+$/, '') + '.luau';
  readcard._source = res.source;
  enterReading(meta.title, lines);
}

function enterReading(title, lines) {
  reading = { title };
  ms.classList.add('is-reading');
  for (const p of passages) p.hidden = true;
  let box = $('.reading', code);
  if (!box) { box = document.createElement('div'); box.className = 'reading'; code.append(box); }
  box.replaceChildren();
  if (lines) {
    const highlight = lines.length <= 4000;
    const CHUNK = 200;
    const frag = document.createDocumentFragment();
    for (let i = 0; i < lines.length; i += CHUNK) {
      const chunk = document.createElement('div');
      chunk.className = 'chunk';
      chunk.innerHTML = lines.slice(i, i + CHUNK).map((l, k) => lineHtml(l, i + k + 1, '', highlight)).join('');
      if (i + CHUNK < lines.length) chunk.style.containIntrinsicSize = `auto ${Math.min(CHUNK, lines.length - i) * parseFloat(getComputedStyle(code).lineHeight || 24)}px`;
      frag.append(chunk);
    }
    box.append(frag);
  }
  crumbFile.textContent = title;
  crumbPsg.textContent = lines ? `${fmtInt(lines.length)} lines` : 'not decompiled';
  for (const b of $$('button', lens)) { b.disabled = true; b.setAttribute('aria-checked', String(b.dataset.view === 'tovek')); }
  relayout();
  requestAnimationFrame(() => scrollTo({ top: docTop(ms) - top.offsetHeight - runhead.offsetHeight, behavior: 'auto' }));
}

function leaveReading() {
  reading = null;
  ms.classList.remove('is-reading');
  const box = $('.reading', code);
  if (box) box.remove();
  for (const p of passages) p.hidden = false;
  readcard.hidden = true;
  readcard.innerHTML = '';
  crumbFile.textContent = 'Beacon.luau';
  for (const b of $$('button', lens)) b.disabled = false;
  for (const b of $$('button', samplesList)) b.setAttribute('aria-pressed', 'false');
  current = null;
  relayout();
}

readcard.addEventListener('click', async (e) => {
  const b = e.target.closest('[data-act]');
  if (!b) return;
  if (b.dataset.act === 'back') { leaveReading(); scrollTo({ top: docTop(ms) - top.offsetHeight - runhead.offsetHeight - 12 }); }
  if (b.dataset.act === 'copy') {
    try { await navigator.clipboard.writeText(readcard._source); b.textContent = 'Copied'; } catch { b.textContent = 'Copy failed'; }
    setTimeout(() => { b.textContent = 'Copy'; }, 1600);
  }
  if (b.dataset.act === 'save') {
    const url = URL.createObjectURL(new Blob([readcard._source + '\n'], { type: 'text/plain' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = readcard.dataset.name || 'decompiled.luau';
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 2000);
  }
  if (b.dataset.act === 'stage') readStage(b.dataset.stage);
});

// the page checks itself: decompile Beacon's bytecode here and compare with the column, byte for byte
$('#check-page').addEventListener('click', async (e) => {
  const btn = e.currentTarget;
  const out = $('#check-status');
  btn.disabled = true;
  out.textContent = 'Loading Tovek into this page…';
  try {
    const [t, d] = await Promise.all([loadTovek(), loadData()]);
    out.textContent = 'Decompiling Beacon…';
    const bytes = new Uint8Array(await (await fetch(here('../../../' + d.script.bytecode_url))).arrayBuffer());
    const res = await t.decompile(bytes, { name: d.script.name });
    if (res.error) { out.textContent = `Tovek stopped: ${res.error}`; return; }
    const same = (await sha256Hex(res.source)) === d.script.output_sha256;
    const n = res.source.split('\n').length;
    out.innerHTML = same
      ? `<b>Same ${fmtInt(n)} lines, byte for byte.</b> Decompiled in this page in ${Math.round(res.ms)} ms by Tovek ${escapeHtml(t.info.version)}. Nothing was uploaded.`
      : `Tovek ${escapeHtml(t.info.version)} in this page printed ${fmtInt(n)} lines, and they differ from the column, which came from the V2.6 build.`;
  } catch (err) {
    out.textContent = 'This browser cannot run Tovek here. Download it for your machine instead.';
  } finally {
    btn.disabled = false;
  }
});

// ------------------------------------------------------------------ printing history: read the script as any release printed it

async function readStage(tag) {
  if (!history) history = await (await fetch(here('../../../data/landing-history.json'))).json();
  const d = await loadData();
  const stages = history.stages;
  const i = stages.findIndex((s) => s.tag === tag);
  const s = stages[i];
  const h = d.history.find((x) => x.tag === tag);
  const total = d.script.helpers.reduce((a, x) => a + x.inlined_copies, 0);
  const lines = s.text.replace(/\n+$/, '').split('\n');
  const prev = stages[i - 1];
  const next = stages[i + 1];
  readcard.hidden = false;
  readcard.innerHTML = `<p class="rc-kicker">Printing history</p><h3 class="rc-title">Beacon, as ${escapeHtml(s.name)} printed it</h3>
    <dl class="rc-facts">
      <div><dt>Released</dt><dd>${new Date(s.date + 'T12:00:00Z').toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' })}</dd></div>
      <div><dt>Lines</dt><dd>${h.lines}</dd></div>
      <div><dt>Plain names</dt><dd>${h.plain_names}</dd></div>
      <div><dt>Helper calls</dt><dd>${h.calls_written_back} of ${total}</dd></div>
      <div><dt>Bytecode read</dt><dd>${escapeHtml(h.bytecode)}</dd></div>
    </dl>
    <p class="rc-actions">${prev ? `<button class="btn" type="button" data-act="stage" data-stage="${prev.tag}">← ${escapeHtml(prev.name)}</button>` : ''}${next ? `<button class="btn" type="button" data-act="stage" data-stage="${next.tag}">${escapeHtml(next.name)} →</button>` : ''}<button class="btn" type="button" data-act="back">Back to the annotated script</button></p>`;
  readcard._source = s.text;
  enterReading('Beacon.luau', lines);
  crumbPsg.textContent = `as ${s.name} printed it, ${fmtInt(lines.length)} lines`;
}

$('#history').addEventListener('click', (e) => {
  const b = e.target.closest('button.read');
  if (b) readStage(b.dataset.stage);
});

// ------------------------------------------------------------------ films: the registry decides, frames are drawn live

async function films() {
  let registry;
  try { registry = await import(here('../../../watch/films/index.js')); } catch { return; }
  const launch = registry.FILMS.find((f) => f.id === 'v26' && f.available && f.listed !== false);
  if (launch) {
    const btn = $('.btn-launch');
    if (btn) btn.hidden = false;
    const list = $('#films-list');
    if (!$('[data-film="v26"]', list)) {
      const a = document.createElement('a');
      a.className = 'film';
      a.href = 'watch/?film=v26';
      a.dataset.film = 'v26';
      a.innerHTML = `<span class="film-frame"><canvas aria-hidden="true"></canvas><span class="film-play" aria-hidden="true"><svg viewBox="0 0 16 16"><path d="M5 3.2v9.6L12.6 8z"/></svg></span></span>
        <span class="film-kicker">${escapeHtml(launch.kicker || 'Launch film')}</span><span class="film-title">${escapeHtml(launch.title)}</span><span class="film-text">${escapeHtml(launch.description || '')}</span>`;
      list.append(a);
    }
  }
  const io = new IntersectionObserver((entries) => {
    for (const en of entries) {
      if (!en.isIntersecting) continue;
      io.unobserve(en.target);
      poster(en.target, registry).catch(() => {});
    }
  }, { rootMargin: '300px 0px' });
  for (const f of $$('.film')) io.observe(f);
}

async function poster(card, registry) {
  const entry = registry.filmById(card.dataset.film);
  if (!entry || !entry.available) return;
  const [engine, mod] = await Promise.all([
    import(here('../../../watch/engine/index.js')),
    import(here('../../../watch/films/' + entry.src.replace(/^\.\//, ''))),
  ]);
  const film = await engine.openFilm(mod, { base: here('../../../watch/') });
  const canvas = $('canvas', card);
  const stage = engine.createStage(canvas);
  const draw = () => {
    stage.resize(canvas.clientWidth, canvas.clientHeight, Math.min(devicePixelRatio || 1, 2));
    stage.draw(film, film.poster ?? 1);
  };
  draw();
  new ResizeObserver(() => requestAnimationFrame(draw)).observe(canvas);
}

// ------------------------------------------------------------------ the title is annotated too: a leader from the promise to the script's facts

const hero = $('.hero');
const heroLeader = document.createElementNS(NS, 'svg');
heroLeader.classList.add('hero-leader');
heroLeader.setAttribute('aria-hidden', 'true');
hero.append(heroLeader);

function layoutHero() {
  if (narrow.matches) { heroLeader.replaceChildren(); return; }
  const hr = hero.getBoundingClientRect();
  const lines = $$('.h1-line', hero);
  const last = lines[lines.length - 1];
  const range = document.createRange();
  range.selectNodeContents(last);
  const rects = Array.from(range.getClientRects()).filter((r) => r.width > 0);
  const end = rects[rects.length - 1];
  const head = $('.facts-head', hero).getBoundingClientRect();
  if (!end) return;
  const ax = end.right - hr.left + 14;
  const ay = end.top + end.height * 0.56 - hr.top;
  const nx = head.left - hr.left - 12;
  const ny = head.top + head.height * 0.5 - hr.top - 6;
  if (nx - ax < 40) { heroLeader.replaceChildren(); return; }
  const mid = (ax + nx) / 2;
  heroLeader.setAttribute('width', hr.width);
  heroLeader.setAttribute('height', hr.height);
  heroLeader.innerHTML = `<path pathLength="1" d="M${ax} ${ay}C${mid} ${ay} ${mid} ${ny} ${nx} ${ny}"/><circle cx="${ax}" cy="${ay}" r="3"/>`;
}

// leaders draw themselves from the code to the note the first time a note comes into view
const seenIO = 'IntersectionObserver' in window && !reduced.matches ? new IntersectionObserver((entries) => {
  for (const en of entries) {
    if (!en.isIntersecting) continue;
    en.target.classList.add('seen');
    const g = $(`g[data-note="${en.target.id}"]`, leaders);
    if (g) g.classList.add('seen');
    seenIO.unobserve(en.target);
  }
}, { rootMargin: '0px 0px -12% 0px' }) : null;

// ------------------------------------------------------------------ start

function start() {
  for (const n of notes) n.tabIndex = narrow.matches ? -1 : 0;
  if (seenIO) for (const n of notes) seenIO.observe(n);
  else for (const n of notes) n.classList.add('seen');
  relayout();
  layoutHero();
  let ticking = false;
  addEventListener('scroll', () => {
    if (ticking) return;
    ticking = true;
    requestAnimationFrame(() => { ticking = false; onScroll(); });
  }, { passive: true });
  new ResizeObserver(() => relayout()).observe(code);
  addEventListener('resize', () => { relayout(); layoutHero(); });
  narrow.addEventListener('change', () => {
    for (const n of notes) n.tabIndex = narrow.matches ? -1 : 0;
    relayout();
    layoutHero();
  });
  films();
  if (location.hash === '#read') openReader(false);
}

onScrollChrome();
document.fonts.ready.then(start);
