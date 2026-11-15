#!/usr/bin/env node
// Builds docs/index.html, the Tovek landing page.
//
//   node scripts/build_landing.mjs            write docs/index.html
//   node scripts/build_landing.mjs --check    build, report the line map, write nothing
//
// Everything the page shows comes from docs/data/landing.json (one real script, its bytecode, the notes, the
// measurements) and docs/data/landing-history.json (the same script as every release printed it). The page's idea is
// that colour is evidence: an instruction wears the colour of the source line its line-table entry names. This script
// works out which printed line each instruction belongs to, so the page can colour both sides the same.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = (p) => fs.readFileSync(path.join(ROOT, p), 'utf8');
const data = JSON.parse(read('docs/data/landing.json'));
const hist = JSON.parse(read('docs/data/landing-history.json'));
const CHECK = process.argv.includes('--check');
const notices = [];

const ASSETS = 'assets/landing';
const OLD = 'assets/v26/landing'; // posters, the icon and Beacon's bytecode still live here
const REPO = data.release.repository;
const V = data.release.name; // "V2.7"

// Release notes shown in the changelog list (newest first). The data file does not carry these.
const CHANGES = [
  { tag: 'v2.7', name: 'V2.7', text: 'More copies come back; every edge case exact.', date: '2026-11-02' },
  { tag: 'v2.6', name: 'V2.6', text: 'Helpers come back, with proof.', date: '2026-10-11' },
  { tag: 'v2.5', name: 'V2.5', text: 'Our next era of faithful decompilation.', date: '2026-10-02' },
  { tag: 'v2.1', name: 'V2.1', text: 'Faster, and reads newer bytecode.', date: '2026-09-28' },
];

// Source lines whose instructions belong to a printed line the text alignment cannot find (the printer merged
// statements). source line -> printed line. Checked below: a stale entry is reported.
const MERGED = { 52: 45, 53: 45, 65: 59 };

// medal's printing of the hero method, lines 42-59 of its output. medal's output never changes, so this table is
// written by hand: which source line each printed line stands for. `join` folds a line into the group above it.
const MEDAL_HERO = {
  first: 42, last: 59, rest: 3,
  expect: { 46: 'TweenInfo.new(0.75)', 50: 'leaderstats', 55: '+ 25', 58: 'FireAllClients' },
  lines: {
    42: { src: 60 }, 44: { src: 61 }, 45: { src: 62 },
    46: { src: 64, chip: { text: '0.75', helper: 'frames' } }, 47: { src: 64, join: 46 }, 48: { src: 65 },
    49: { src: 67 },
    50: { src: 24, pasted: 'award' }, 51: { src: 25, pasted: 'award' }, 52: { src: 25, pasted: 'award', join: 51 },
    53: { src: 25, pasted: 'award', join: 51 }, 54: { src: 26, pasted: 'award' }, 55: { src: 27, pasted: 'award' },
    58: { src: 70 }, 59: { src: 71 },
  },
};

// ------------------------------------------------------------------ text helpers

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const fmt = (n) => Number(n).toLocaleString('en-US');
const pad2 = (n) => String(n).padStart(2, '0');
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const shortDate = (iso) => { const [y, m, d] = iso.split('-').map(Number); return `${d} ${MONTHS[m - 1]} ${y}`; };
const dayMonth = (iso) => { const [, m, d] = iso.split('-').map(Number); return `${d} ${MONTHS[m - 1]}`; };
const clock = (s) => `${Math.floor(s / 60)}:${pad2(s % 60)}`;
// `code` in the data's prose becomes <code>
const prose = (s) => esc(s).replace(/`([^`]+)`/g, '<code>$1</code>');

// ------------------------------------------------------------------ Luau tokens (the page's script uses the same rules)

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

const BREAK_AFTER = new Set([':', '.', '(']);

// One code line. `marks` are [start, end) column ranges of the raw line (tabs included) to wrap in <mark>.
function lineHtml(text, n, { cls = '', attrs = '', marks = [], id = '', tail = '' } = {}) {
  const ind = text.length - text.replace(/^\t+/, '').length;
  const body = text.slice(ind);
  let tokens = [];
  let col = ind;
  for (const [kind, t] of tokenize(body)) { tokens.push({ kind, text: t, a: col, b: col + t.length }); col += t.length; }
  // split tokens at mark boundaries so a mark can start inside a string
  const cuts = new Set();
  for (const m of marks) { cuts.add(m.start); cuts.add(m.end); }
  if (cuts.size) {
    const next = [];
    for (const tk of tokens) {
      const inner = [...cuts].filter((c) => c > tk.a && c < tk.b).sort((x, y) => x - y);
      let a = tk.a;
      for (const c of [...inner, tk.b]) { next.push({ kind: tk.kind, text: tk.text.slice(a - tk.a, c - tk.a), a, b: c }); a = c; }
    }
    tokens = next;
  }
  let h = '';
  for (const tk of tokens) {
    for (const m of marks) if (m.start === tk.a) h += `<mark class="${m.cls}"${m.attrs || ''}>`;
    h += tk.kind === 'i' || tk.kind === 'x' ? esc(tk.text) : `<span class="${tk.kind}">${esc(tk.text)}</span>`;
    if (tk.kind === 'o' && BREAK_AFTER.has(tk.text)) h += '<wbr>';
    for (const m of marks) if (m.end === tk.b) h += '</mark>';
  }
  const c = ['ln', body ? '' : 'blank', cls].filter(Boolean).join(' ');
  return `<div${id ? ` id="${id}"` : ''} class="${c}" data-n="${n}"${attrs} style="--i:${ind}"><span class="t">${h}</span>${tail}</div>`;
}

// ------------------------------------------------------------------ colour: a function of the source line

const helpers = data.script.helpers; // [{name, lines:[a,b], inlined_copies, rebuilt_calls}]
const HL = ['pink', 'green', 'yellow', 'pink', 'green', 'yellow'];
const helperAt = (line) => helpers.find((h) => line >= h.lines[0] && line <= h.lines[1]) || null;
const hlOf = (name) => HL[helpers.findIndex((h) => h.name === name)] || 'pink';
function colourOf(line) {
  const h = helperAt(line);
  if (h) return `hl-${hlOf(h.name)}${(line - h.lines[0]) % 2 ? ' alt' : ''}`;
  return `c${line % 6}`;
}

// ------------------------------------------------------------------ which printed line stands for which source line

const src = data.script.source.split('\n');
const out = data.script.output.split('\n');

function feats(line) {
  const s = line.replace(/--.*$/, '').trim();
  const f = [];
  const re = /([.:])([A-Za-z_]\w*)|"([^"]*)"|`([^`]*)`|\b([A-Za-z_]\w*)\b|(\d+(?:\.\d+)?)|([-+*\/#<>=~]=?|[{}()])/g;
  let m;
  while ((m = re.exec(s))) {
    if (m[2]) f.push('m:' + m[2]);
    else if (m[3] !== undefined) f.push('s:' + m[3]);
    else if (m[4] !== undefined) f.push('s:' + m[4]);
    else if (m[5]) f.push(KEYWORDS.has(m[5]) ? 'k:' + m[5] : 'i');
    else if (m[6]) f.push('n');
    else if (m[7]) f.push('o:' + m[7]);
  }
  return f;
}
const weight = (x) => (x === 'i' || x === 'n' ? 0.3 : 1);
function sim(a, b) {
  if (!a.length || !b.length) return 0;
  const cb = new Map();
  for (const x of b) cb.set(x, (cb.get(x) || 0) + 1);
  let hit = 0;
  for (const x of a) { const c = cb.get(x) || 0; if (c) { hit += weight(x); cb.set(x, c - 1); } }
  return (2 * hit) / (a.reduce((s, x) => s + weight(x), 0) + b.reduce((s, x) => s + weight(x), 0));
}

const srcOfOut = new Map(); // printed line -> source line
const outOfSrc = new Map(); // source line -> printed line
for (const p of data.passages) {
  const [s0, s1] = p.src;
  const [o0, o1] = p.out;
  const sa = [];
  const sb = [];
  for (let k = s0; k <= s1; k++) sa.push(feats(src[k - 1]));
  for (let k = o0; k <= o1; k++) sb.push(feats(out[k - 1]));
  const n = sa.length;
  const m = sb.length;
  const dp = Array.from({ length: n + 1 }, () => new Float64Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) {
    const s = sim(sa[i], sb[j]);
    dp[i][j] = Math.max(dp[i + 1][j], dp[i][j + 1], s > 0.34 ? dp[i + 1][j + 1] + s : -1);
  }
  let i = 0;
  let j = 0;
  const usedS = new Set();
  while (i < n && j < m) {
    const s = sim(sa[i], sb[j]);
    if (s > 0.34 && dp[i][j] === dp[i + 1][j + 1] + s) { srcOfOut.set(o0 + j, s0 + i); outOfSrc.set(s0 + i, o0 + j); usedS.add(i); i++; j++; }
    else if (dp[i][j] === dp[i + 1][j]) i++;
    else j++;
  }
  // statements the printer moved: match what is left, in any order
  for (let jj = 0; jj < m; jj++) {
    if (srcOfOut.has(o0 + jj) || !sb[jj].length) continue;
    let best = -1;
    let bestS = 0.5;
    for (let ii = 0; ii < n; ii++) { if (usedS.has(ii)) continue; const s = sim(sa[ii], sb[jj]); if (s > bestS) { bestS = s; best = ii; } }
    if (best >= 0) { srcOfOut.set(o0 + jj, s0 + best); outOfSrc.set(s0 + best, o0 + jj); usedS.add(best); }
  }
}
for (const [s, o] of Object.entries(MERGED)) {
  if (outOfSrc.has(+s)) notices.push(`MERGED entry ${s} -> ${o} is no longer needed: source line ${s} aligns to printed line ${outOfSrc.get(+s)}`);
  else outOfSrc.set(+s, o);
}

// ------------------------------------------------------------------ rows: every instruction, with the printed line it belongs to

const passageOfOut = new Map();
for (const p of data.passages) for (let k = p.out[0]; k <= p.out[1]; k++) passageOfOut.set(k, p.id);

const callLine = new Map(); // passage id -> helper name -> printed lines holding its rebuilt calls, in order
for (const p of data.passages) {
  const m = new Map();
  for (const h of helpers) {
    const re = new RegExp(`(?<![.:\\w])${h.name}\\(`);
    const lines = [];
    for (let k = p.out[0]; k <= p.out[1]; k++) if (RECOVERED.test(out[k - 1]) && re.test(out[k - 1]) && !/^\s*local function/.test(out[k - 1])) lines.push(k);
    m.set(h.name, lines);
  }
  callLine.set(p.id, m);
}

const rowsOf = new Map(); // passage id -> [{head} | row]
const linesWithRows = new Map(); // printed line -> [{cls, pasted}]
let totalRows = 0;
let pastedRuns = 0;
for (const p of data.passages) {
  const list = [];
  const seen = new Map(); // helper -> runs seen in this passage
  for (const b of p.bytecode) {
    list.push({ head: b.main ? 'main chunk' : `function ${b.name || '(anonymous)'}`, sub: b.main ? (p.kind === 'fn' ? 'the function is made and stored' : '') : `defined on line ${b.rows.length ? Math.min(...b.rows.filter((r) => !r.from || b.name === r.from).map((r) => r.line)) - 1 : ''}` });
    let prevFrom = null;
    let run = null;
    for (const r of b.rows) {
      const pasted = r.from && !b.main && b.name !== r.from ? r.from : null;
      let target = null;
      if (pasted) {
        if (r.remark === 'inlined' || prevFrom !== pasted) { const k = seen.get(pasted) || 0; seen.set(pasted, k + 1); run = (callLine.get(p.id).get(pasted) || [])[k] ?? null; pastedRuns++; if (run === null) notices.push(`${p.id}: no rebuilt call found for pasted run ${k + 1} of ${pasted}`); }
        target = run;
      } else {
        target = outOfSrc.get(r.line) ?? null;
        if (target !== null && passageOfOut.get(target) !== p.id) target = null;
        if (target === null && !(r.op === 'PREPVARARGS')) notices.push(`${p.id}: no printed line for ${r.op} on source line ${r.line}`);
      }
      prevFrom = pasted;
      const row = { pc: r.pc, line: r.line, label: r.label, op: r.op, args: r.args, pasted, start: pasted && (r.remark === 'inlined'), cls: colourOf(r.line), target, proto: b.proto };
      list.push(row);
      totalRows++;
      if (target !== null) { if (!linesWithRows.has(target)) linesWithRows.set(target, []); linesWithRows.get(target).push(row); }
    }
  }
  rowsOf.set(p.id, list);
}

// a printed line takes the colour most of its own instructions carry; a line made only of pasted rows takes the helper's
function bandOf(rows) {
  if (!rows || !rows.length) return '';
  const own = rows.filter((r) => !r.pasted);
  const pool = own.length ? own : rows;
  const count = new Map();
  for (const r of pool) { const base = own.length ? r.cls : r.cls.replace(' alt', ''); count.set(base, (count.get(base) || 0) + 1); }
  return [...count].sort((a, b) => b[1] - a[1])[0][0];
}

// the rebuilt call on a printed line: its [start, end) columns
function callSpan(text, name, from = 0) {
  const re = new RegExp(`(?<![.:\\w])${name}\\(`, 'g');
  re.lastIndex = from;
  const m = re.exec(text);
  if (!m) return null;
  let depth = 0;
  for (let k = m.index + name.length; k < text.length; k++) {
    if (text[k] === '(') depth++;
    else if (text[k] === ')') { depth--; if (!depth) return [m.index, k + 1]; }
  }
  return null;
}

const callMarks = new Map(); // printed line -> [{start,end,cls}]
for (const p of data.passages) {
  for (const [name, lines] of callLine.get(p.id)) for (const k of lines) {
    const span = callSpan(out[k - 1], name);
    if (!span) continue;
    if (!callMarks.has(k)) callMarks.set(k, []);
    callMarks.get(k).push({ start: span[0], end: span[1], cls: `call hl-${hlOf(name)}` });
  }
}

// ------------------------------------------------------------------ notes

const tourOf = new Map(data.tour.filter((t) => t.note).map((t) => [t.note, t]));
// A note outside the tour has no title in the data, only a kind. These are written from each note's own text; an entry
// applies only while the note still has the kind it was written for, otherwise the kind stands in.
const NOTE_TITLES = {
  n3: { kind: 'Module name', title: '`Beacon`, named after the script' },
  n5: { kind: 'No evidence', title: 'No evidence, so no invented name' },
  n8: { kind: 'String', title: 'A `format` call, written back as a backtick string' },
  n9: { kind: 'Not in the bytecode', title: 'A name that never reached the bytecode' },
  n11: { kind: 'Name', title: '`heartbeatConnection`, named after what it holds' },
  n12: { kind: 'Name', title: '`character`, named by the event that passes it' },
};
const titleOf = (n) => tourOf.get(n.id)?.title || (NOTE_TITLES[n.id]?.kind === n.kind ? NOTE_TITLES[n.id].title : n.kind);
const notes = data.notes.map((n, i) => ({ ...n, i, title: titleOf(n) }));
const noteMarks = new Map(); // printed line -> [{start,end,cls,attrs}]
for (const n of notes) for (const [line, a, b] of n.spans) {
  if (!noteMarks.has(line)) noteMarks.set(line, []);
  noteMarks.get(line).push({ start: a, end: b, cls: 'ns', attrs: ` data-note="${n.id}"` });
}
// a note's span and a rebuilt call often cover the same words: one <mark> then carries both
function marksFor(line, withNotes) {
  const calls = (callMarks.get(line) || []).map((m) => ({ ...m }));
  if (!withNotes) return calls;
  const res = calls;
  for (const nm of noteMarks.get(line) || []) {
    const same = res.find((m) => m.start === nm.start && m.end === nm.end);
    if (same) { same.cls += ' ns'; same.attrs = (same.attrs || '') + nm.attrs; }
    else if (!res.some((m) => nm.start < m.end && m.start < nm.end)) res.push({ ...nm });
    else {
      // the note covers part of a call (or the reverse): keep the note, drop the overlap
      for (let k = res.length - 1; k >= 0; k--) if (nm.start < res[k].end && res[k].start < nm.end) res.splice(k, 1);
      res.push({ ...nm });
    }
  }
  return res;
}

// ------------------------------------------------------------------ bytecode rows as HTML

function argsHtml(args) {
  const m = /^(.*?)(\s*)(\[.*\])$/.exec(args);
  return m ? `${esc(m[1])}${m[2]}<span class="s">${esc(m[3])}</span>` : esc(args);
}
// `key` builds the data attributes that tie a row to its printed line(s)
function rowsHtml(list, key) {
  let h = '';
  let prev = null;
  for (const r of list) {
    if (r.head) { h += `<p class="bh"><b>${esc(r.head)}</b>${r.sub ? `<span>${esc(r.sub)}</span>` : ''}</p>`; prev = null; continue; }
    const cont = prev && prev.line === r.line && !!prev.pasted === !!r.pasted;
    const tag = r.pasted && r.start ? `<span class="fr">pasted from ${esc(r.pasted)}</span>` : '';
    h += `<div class="bc ${r.cls}${r.pasted ? ' is-pasted' : ''}${cont ? ' cont' : ''}"${key(r)}><span class="g">${r.line}</span><span class="lb">${r.label ? esc(r.label) : ''}</span><span class="op">${esc(r.op)}</span><span class="ar">${argsHtml(r.args)}</span>${tag}</div>`;
    prev = r;
  }
  return h;
}

// ------------------------------------------------------------------ the hero: one method, three printings, one bytecode

const heroP = data.passages.find((p) => p.id === data.hero.passage);
const heroRows = rowsOf.get(heroP.id);
const heroViews = {};

{ // Tovek
  const lines = [];
  for (let k = heroP.out[0]; k <= heroP.out[1]; k++) {
    const rows = linesWithRows.get(k);
    lines.push(lineHtml(out[k - 1], k, { cls: bandOf(rows), attrs: rows ? ` data-g="t${k}"` : '', marks: marksFor(k, false) }));
  }
  for (let k = heroP.out[1] + 1; k <= Math.min(out.length, heroP.out[1] + 5); k++) lines.push(lineHtml(out[k - 1], k, { cls: 'rest' }));
  heroViews.tovek = { html: lines.join(''), note: `Lines ${heroP.out[0]}&ndash;${heroP.out[1]} of ${out.length}, as Tovek ${V} printed them, unedited.` };
  for (const r of heroRows) if (!r.head) r.gt = r.target !== null ? `t${r.target}` : '';
}
{ // Source
  const pastedTarget = new Map(); // printed call line -> source line of the call
  const lines = [];
  const srcRows = new Map();
  for (const r of heroRows) {
    if (r.head) continue;
    const s = r.pasted ? (srcOfOut.get(r.target) ?? null) : r.line;
    r.gs = s !== null && s >= heroP.src[0] && s <= heroP.src[1] ? `s${s}` : '';
    if (r.gs) { if (!srcRows.has(s)) srcRows.set(s, []); srcRows.get(s).push(r); }
    if (r.pasted && s !== null) pastedTarget.set(s, r.pasted);
  }
  for (let k = heroP.src[0]; k <= heroP.src[1]; k++) {
    const rows = srcRows.get(k);
    const marks = [];
    const name = pastedTarget.get(k);
    if (name) { const span = callSpan(src[k - 1], name); if (span) marks.push({ start: span[0], end: span[1], cls: `call hl-${hlOf(name)}` }); }
    lines.push(lineHtml(src[k - 1], k, { cls: bandOf(rows), attrs: rows ? ` data-g="s${k}"` : '', marks }));
  }
  for (let k = heroP.src[1] + 1; k <= Math.min(src.length, heroP.src[1] + 5); k++) lines.push(lineHtml(src[k - 1], k, { cls: 'rest' }));
  heroViews.source = { html: lines.join(''), note: `Lines ${heroP.src[0]}&ndash;${heroP.src[1]} of <code>${esc(data.script.file)}</code>, as we wrote them.` };
}
{ // medal
  const medal = hist.stages.find((s) => s.tag === 'medal');
  const text = medal ? medal.text.split('\n') : [];
  const ok = medal && Object.entries(MEDAL_HERO.expect).every(([n, needle]) => (text[n - 1] || '').includes(needle));
  if (!ok) notices.push('medal: the hero table no longer matches medal\'s output; the medal view is left out');
  else {
    const groupOf = (n) => { const e = MEDAL_HERO.lines[n]; return e ? `m${e.join || n}` : ''; };
    const lines = [];
    for (let k = MEDAL_HERO.first; k <= MEDAL_HERO.last; k++) {
      const e = MEDAL_HERO.lines[k];
      const marks = [];
      if (e && e.chip) { const at = text[k - 1].indexOf(e.chip.text); if (at >= 0) marks.push({ start: at, end: at + e.chip.text.length, cls: `call hl-${hlOf(e.chip.helper)}` }); }
      lines.push(lineHtml(text[k - 1], k, { cls: e ? colourOf(e.src) : '', attrs: e ? ` data-g="${groupOf(k)}"` : '', marks }));
    }
    for (let k = MEDAL_HERO.last + 1; k <= MEDAL_HERO.last + MEDAL_HERO.rest + 2; k++) lines.push(lineHtml(text[k - 1], k, { cls: 'rest' }));
    heroViews.medal = { html: lines.join(''), note: `As medal, the decompiler Tovek grew from, printed it. ${esc(data.medal_bytecode_note)}` };
    const entries = Object.entries(MEDAL_HERO.lines).map(([n, e]) => ({ n: +n, ...e })).filter((e) => !e.join);
    for (const r of heroRows) {
      if (r.head) continue;
      let e;
      if (r.pasted) e = entries.find((x) => x.pasted === r.pasted && x.src === r.line) || entries.find((x) => x.chip && x.chip.helper === r.pasted);
      else e = entries.find((x) => !x.pasted && x.src === r.line);
      r.gm = e ? `m${e.n}` : '';
    }
  }
}
const heroKey = (r) => ` data-gt="${r.gt || ''}" data-gs="${r.gs || ''}"${heroViews.medal ? ` data-gm="${r.gm || ''}"` : ''}`;
const heroBc = rowsHtml(heroRows, heroKey);
const heroHelpers = [...new Set(heroRows.filter((r) => r.pasted).map((r) => r.pasted))];

// ------------------------------------------------------------------ the reader: all printed lines, all rows

const codeHtml = out.map((text, i) => {
  const k = i + 1;
  const rows = linesWithRows.get(k);
  const pid = passageOfOut.get(k) || '';
  const tail = notes.filter((n) => n.line === k).map((n) => `<button type="button" class="nb" data-note="${n.id}" aria-label="Note ${n.i + 1}: ${esc(n.title.replace(/`/g, ''))}">${n.i + 1}</button>`).join('');
  return lineHtml(text, k, { id: `L${k}`, cls: bandOf(rows), attrs: `${pid ? ` data-p="${pid}"` : ''}${rows ? ` data-rows="${rows.length}"` : ''}`, marks: marksFor(k, true), tail });
}).join('\n');

const readerBc = data.passages.map((p) => {
  const list = rowsOf.get(p.id);
  return `<div class="bcp" data-p="${p.id}" data-title="${esc(p.title)}" data-n="${p.instructions}" hidden>${rowsHtml(list, (r) => (r.target !== null ? ` data-t="${r.target}"` : ''))}</div>`;
}).join('\n');

// the strip: every instruction as one stripe, grouped by passage
const stripHtml = data.passages.map((p) => {
  const list = rowsOf.get(p.id).filter((r) => !r.head);
  const stripes = list.map((r) => `<i class="${r.cls}${r.pasted ? ' is-pasted' : ''}"></i>`).join('');
  return `<a class="seg${list.length < 14 ? ' is-small' : ''}" href="#L${p.out[0]}" data-p="${p.id}" style="--n:${list.length}" title="${esc(p.title)}: ${list.length} instruction${list.length === 1 ? '' : 's'}"><span class="seg-bar">${stripes}</span><span class="seg-n">${esc(p.title)}</span></a>`;
}).join('');

function evidenceHtml(e) {
  const lua = /medal/.test(e.label);
  const body = lua
    ? e.code.split('\n').map((l) => { const ind = l.length - l.replace(/^\t+/, '').length; return '  '.repeat(ind) + tokenize(l.slice(ind)).map(([k, t]) => (k === 'i' || k === 'x' ? esc(t) : `<span class="${k}">${esc(t)}</span>`)).join(''); }).join('\n')
    : esc(e.code).replace(/^([A-Z][A-Z0-9]+)(\s)/gm, '<span class="op">$1</span>$2');
  return `<figure class="ev"><figcaption>${esc(e.label)}</figcaption><pre>${body}</pre></figure>`;
}
const notesHtml = notes.map((n) => `<article class="note" id="note-${n.id}" data-note="${n.id}" data-line="${n.line}" hidden><header class="note-h"><h3>${prose(n.title)}</h3><span class="note-l">line ${n.line}</span></header><p class="note-t">${prose(n.text)}</p>${n.evidence.map(evidenceHtml).join('')}</article>`).join('\n');
const noteDots = notes.map((n) => `<button type="button" class="dot" data-note="${n.id}" aria-label="Note ${n.i + 1}: ${esc(n.title.replace(/`/g, ''))}, line ${n.line}"><span>${n.i + 1}</span></button>`).join('');

// ------------------------------------------------------------------ one call, proven: the 64-bit check

const bitNote = notes.find((n) => n.evidence.some((e) => /^0x[0-9A-Fa-f]{16}/.test(e.code)) && n.evidence.some((e) => /LOADK/.test(e.code)));
let callSection = '';
if (bitNote) {
  const loadk = bitNote.evidence.find((e) => /LOADK/.test(e.code)).code.replace(/\s+/g, ' ');
  const constant = /\[([^\]]+)\]/.exec(loadk)[1];
  const loadkArgs = /LOADK\s+(.*?)\s*\[/.exec(loadk)[1];
  const [hex, bitsLine] = bitNote.evidence.find((e) => /^0x/.test(e.code)).code.split('\n');
  const bits = bitsLine.replace(/\s+/g, '');
  const helper = helperAt(Number(/line (\d+)/.exec(bitNote.evidence.find((e) => /LOADK/.test(e.code)).label)[1]));
  const hl = hlOf(helper.name);
  const defOut = outOfSrc.get(helper.lines[0]);
  const bodyOut = outOfSrc.get(helper.lines[0] + 1);
  const bodyText = out[bodyOut - 1].trim(); // return p / 60
  const param = /\(([^)]*)\)/.exec(out[defOut - 1])[1];
  const divisor = /\/\s*(\d+)/.exec(bodyText)[1];
  const callText = out[bitNote.line - 1].trim();
  const arg = /\((\d+)\)/.exec(out[bitNote.line - 1].slice(bitNote.spans[0][1], bitNote.spans[0][2]))[1];
  const helperRows = rowsOf.get(data.passages.find((p) => p.name === helper.name).id).filter((r) => !r.head && r.proto !== undefined && r.line === helper.lines[0] + 1);
  const cells = (cls) => [...bits].map((b, i) => `<i class="${b === '1' ? 'on' : ''}${i === 0 ? ' sg' : i < 12 ? ' ex' : ''}"${cls ? ` style="--k:${i}"` : ''}></i>`).join('');
  callSection = `
  <section class="sec call hl-${hl}" id="call" aria-labelledby="call-h">
    <header class="sec-head">
      <h2 id="call-h">One call, proven to the bit.</h2>
      <p class="sec-lead">In a callback inside <code>Beacon.start</code>, the bytecode holds a bare number where the source called a helper. Until V2.6, every release printed that number. This is each step Tovek ${V} takes before it writes the call back.</p>
    </header>
    <ol class="steps">
      <li class="step">
        <p class="step-s">The instruction loads a constant. Its line-table entry says line <b>${helper.lines[0] + 1}</b>.</p>
        <div class="spec"><span class="g">${helper.lines[0] + 1}</span><span class="op">LOADK</span><span class="ar">${esc(loadkArgs)} <b>[${esc(constant)}]</b></span></div>
      </li>
      <li class="step">
        <p class="step-s">Line ${helper.lines[0] + 1} is not in this function. It is the body of <code>${esc(helper.name)}</code>, which divides its argument by ${divisor}.</p>
        <div class="spec spec-rows">${helperRows.map((r) => `<span class="g">${r.line}</span><span class="op">${esc(r.op)}</span><span class="ar">${argsHtml(r.args)}</span>`).join('')}</div>
      </li>
      <li class="step">
        <p class="step-s">Tovek solves that body for the argument.</p>
        <div class="spec spec-eq"><span>${esc(param)} / ${divisor} = ${esc(constant)}</span><span class="then">${esc(param)} = <b>${arg}</b></span></div>
      </li>
      <li class="step step-bits" id="bits">
        <p class="step-s">It divides ${arg} by ${divisor} the way Luau folds constants, and compares the result with the constant in the file, bit by bit.</p>
        <div class="bits" role="img" aria-label="Both values are ${esc(hex)}: all 64 bits agree.">
          <p class="bits-row"><span class="bits-l">in the bytecode</span><span class="bits-c">${cells(false)}</span><span class="bits-x">${esc(hex)}</span></p>
          <p class="bits-row bits-b"><span class="bits-l">${arg} / ${divisor}, folded</span><span class="bits-c">${cells(true)}</span><span class="bits-x">${esc(hex)}</span></p>
          <p class="bits-row bits-k"><span class="bits-l"></span><span class="bits-c"><span class="lab-e">sign and exponent, 2<sup>&minus;6</sup></span><span class="lab-m">mantissa, 52 bits</span></span><b id="bits-n"><span>64</span> of 64 bits agree</b></p>
        </div>
      </li>
      <li class="step step-out">
        <p class="step-s">Only now is the call written, and marked as inferred.</p>
        <div class="spec spec-out">${lineHtml(out[bitNote.line - 1].trim(), bitNote.line, { marks: [(() => { const s = callSpan(callText, helper.name); return { start: s[0], end: s[1], cls: 'call' }; })()] })}</div>
      </li>
    </ol>
    <p class="call-fine">The mark means a call that behaves the same. It makes no claim about where the source called the helper. Where a file has no line info, or any check fails, the number stays a number.</p>
  </section>`;
} else notices.push('no note with a LOADK row and a 64-bit pattern: the "one call" section is left out');

// ------------------------------------------------------------------ history

const stages = data.history.map((s) => ({ tag: s.tag, name: s.name, date: shortDate(s.date), lines: s.lines, plain: s.plain_names, calls: s.calls_written_back, decimals: s.long_decimals, anchor: s.anchor, changed: s.changed, sil: s.silhouette.map(([ind, len]) => [ind, len]) }));
const last = stages[stages.length - 1];
const histStart = Math.max(1, last.anchor - 4);
const histHtml = out.slice(histStart - 1, histStart + 27).map((t, i) => lineHtml(t, histStart + i)).join('');
const eras = [];
for (let i = 0; i < data.history.length; i++) {
  const h = data.history[i];
  const era = h.tag === 'medal' ? 'medal' : /beta|^v0\./.test(h.tag) ? 'betas' : h.name.replace(/^(V\d+(?:\.\d+)?).*$/, '$1');
  const lastEra = eras[eras.length - 1];
  if (lastEra && lastEra.name === era) lastEra.b = i; else eras.push({ name: era, a: i, b: i });
}
const pageData = { helpers: data.script.helpers.reduce((s, h) => s + h.inlined_copies, 0), stages, release: V };

// ------------------------------------------------------------------ the measurements

const P = data.proof;
const measuredHtml = `
      <div class="m-row m-fuzz">
        <p class="m-s"><b class="m-n">${fmt(P.fuzz.wrong)}</b> wrong outputs in ${fmt(P.fuzz.programs)} freshly generated programs.</p>
        <p class="m-t">${fmt(P.fuzz.passed)} came back behaving the same as their bytecode, ${fmt(P.fuzz.refused)} were refused rather than approximated, and ${fmt(P.fuzz.invalid)} did not compile and are left out. V2.5.1 got ${fmt(P.fuzz.wrong_v251)} wrong.</p>
        <div class="m-g"><canvas id="waffle" data-passed="${P.fuzz.passed}" data-refused="${P.fuzz.refused}" data-invalid="${P.fuzz.invalid}" data-wrong="${P.fuzz.wrong}" role="img" aria-label="${fmt(P.fuzz.programs)} programs: ${fmt(P.fuzz.passed)} behave the same, ${fmt(P.fuzz.refused)} refused, ${fmt(P.fuzz.invalid)} did not compile, ${fmt(P.fuzz.wrong)} wrong."></canvas><p class="m-key"><span class="k-pass">behaves the same</span><span class="k-ref">refused</span><span class="k-inv">did not compile</span><span class="k-wrong">wrong: none</span></p></div>
      </div>
      <div class="m-row">
        <p class="m-s"><b class="m-n">${fmt(P.edge.current.passed)}</b> of ${fmt(P.edge.current.runs)} edge-case runs pass.</p>
        <p class="m-t">${fmt(P.edge.current.cases)} reported or fuzzed bugs, each kept as a test: compiled at -O0, -O1 and -O2, decompiled, compiled again and run beside the original on Luau&rsquo;s own VM. V2.6.1 passed ${fmt(P.edge.previous.passed)}.</p>
      </div>
      <div class="m-row">
        <p class="m-s"><b class="m-n">${fmt(P.games.ok)}</b> of ${fmt(P.games.scripts)} scripts decompile.</p>
        <p class="m-t">Every script in ${P.games.games} real Roblox games, which stay unnamed.</p>
      </div>
      <div class="m-row">
        <p class="m-s"><b class="m-n">${fmt(P.games.calls)}</b> helper calls written back.</p>
        <p class="m-t">In those same games. ${esc(P.games.previous)} wrote ${fmt(P.games.calls_previous)}.</p>
      </div>`;

// ------------------------------------------------------------------ samples, films, downloads, usage

const sampleIndex = JSON.parse(read('docs/assets/v26/tovek/samples/index.json'));
const SAMPLE_ORDER = ['SpawnShield', 'Store', 'Signal', 'Trove', 'ClickToMoveController', 'Promise'];
const SAMPLE_TITLE = { Store: 'Rodux Store' };
const sampleRows = SAMPLE_ORDER.map((id) => sampleIndex.samples.find((s) => s.id === id)).filter(Boolean);
const samplesHtml = [
  `<li><button type="button" class="chip" data-sample="Beacon" data-url="${esc(data.script.bytecode_url)}" data-name="Beacon.lua" data-sha="${esc(data.script.output_sha256)}" aria-pressed="false" title="The script on this page, decompiled again here and compared byte for byte"><span>Beacon</span><small>this page</small></button></li>`,
  ...sampleRows.map((s) => `<li><button type="button" class="chip" data-sample="${esc(s.id)}" aria-pressed="false" title="${esc(s.about)}"><span>${esc(SAMPLE_TITLE[s.id] || s.title)}</span><small>${fmt(s.native.lines)} lines</small></button></li>`),
].join('\n');

const playIcon = '<svg class="i i-play" viewBox="0 0 16 16" aria-hidden="true"><path d="M5 3.2v9.6L12.6 8z"/></svg>';
const downIcon = '<svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2.5v8m-3.5-3.5L8 10.5 11.5 7M3 13.5h10"/></svg>';
const arrowIcon = '<svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M3 8h10M9 4l4 4-4 4"/></svg>';
const filmsHtml = data.films.map((f) => `<a class="film" href="watch/?film=${esc(f.id)}" data-film="${esc(f.id)}"${f.id === 'story' ? ' data-live' : ''}><span class="film-frame"><img src="${esc(f.poster)}" alt="" width="960" height="540" loading="lazy" decoding="async"><span class="film-play" aria-hidden="true">${playIcon}</span><span class="film-time">${clock(f.duration)}</span></span><span class="film-title">${esc(f.title)}</span><span class="film-text">${esc(f.description)}</span></a>`).join('\n');
const changesHtml = CHANGES.map((c) => `<li><a href="changelog/${c.tag}/"><b>${c.name}</b><span>${esc(c.text)}</span><time datetime="${c.date}">${dayMonth(c.date)}</time></a></li>`).join('\n');
const dlBase = `${REPO}/releases/download/${data.release.tag}/`;
const downloadsHtml = data.release.assets.map((a) => `<li><a class="dl" href="${dlBase}${esc(a.file)}" data-platform="${esc(a.platform)}"><b>${esc(a.platform)}</b><span class="dl-n">${esc(a.note)}</span><code>${esc(a.file)}</code>${downIcon}</a></li>`).join('\n');
const usageBlock = (title, rows) => `<div class="use"><h3>${title}</h3><div class="use-rows">${rows.map((r) => `<p class="use-row"><code>${/^(POST|GET) /.test(r.cmd) ? '' : '<i aria-hidden="true">$ </i>'}${esc(r.cmd)}</code><span>${esc(r.what)}</span></p>`).join('')}</div></div>`;
const story = data.films.find((f) => f.id === 'story');
const relFilm = data.films.find((f) => f.id === data.release.film);

const mark = '<svg class="brand-mark" viewBox="64 72 384 360" aria-hidden="true"><path d="M100 88H412a20 20 0 0 1 20 20V160a20 20 0 0 1-20 20H320V320H192V180H100a20 20 0 0 1-20-20V108a20 20 0 0 1 20-20Z"/><rect x="196" y="328" width="24" height="24" rx="5"/><rect x="228" y="328" width="24" height="24" rx="5"/><rect x="260" y="328" width="24" height="24" rx="5"/><rect x="292" y="328" width="24" height="24" rx="5"/><rect x="196" y="360" width="24" height="24" rx="5"/><rect x="260" y="360" width="24" height="24" rx="5"/><rect x="292" y="360" width="24" height="24" rx="5"/><rect x="228" y="392" width="24" height="24" rx="5"/></svg>';

const BC_COLS = '<p class="bc-cols" aria-hidden="true"><span>line</span><span></span><span>instruction</span></p>';
const lensBtn = (view, label, on) => `<button type="button" role="radio" aria-checked="${on}" tabindex="${on ? 0 : -1}" data-view="${view}">${label}</button>`;
const lensHtml = [heroViews.medal ? lensBtn('medal', 'medal', false) : '', lensBtn('tovek', `Tovek ${V}`, true), lensBtn('source', 'Source', false), `<button type="button" role="radio" aria-checked="false" tabindex="-1" data-view="bytecode" class="only-phone">Bytecode</button>`].join('');
const viewHtml = (view, on) => (heroViews[view] ? `<div class="view" data-view="${view}"${on ? '' : ' hidden'}>${heroViews[view].html}</div>` : '');
const helperKey = heroHelpers.map((h) => `<span class="key hl-${hlOf(h)}">pasted from ${esc(h)}</span>`).join('');
const pastedCount = helpers.reduce((s, h) => s + h.inlined_copies, 0);
const rebuiltCount = helpers.reduce((s, h) => s + h.rebuilt_calls, 0);
const allBack = stages.find((s) => s.calls >= pastedCount); // the first release that wrote every pasted copy back
const numberWord = (n) => ['zero', 'one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight', 'nine', 'ten', 'eleven', 'twelve', 'thirteen', 'fourteen', 'fifteen', 'sixteen', 'seventeen', 'eighteen', 'nineteen', 'twenty'][n] || String(n);
const cap = (s) => s[0].toUpperCase() + s.slice(1);

// ------------------------------------------------------------------ the page

const description = `Tovek is a free, open-source Luau decompiler for Roblox bytecode. It writes back the helper calls Luau -O2 pasted into their callers and the constants it folded, and only where the bytecode's own line table proves them. Read a real script against its bytecode, or decompile your own file in the browser.`;

const html = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
  <title>Tovek: the Luau decompiler that writes the calls back</title>
  <meta name="description" content="${esc(description)}">
  <meta name="theme-color" content="#ffffff">
  <meta property="og:title" content="Tovek: the Luau decompiler that writes the calls back">
  <meta property="og:description" content="Every instruction remembers its line. Tovek uses that to undo what Luau -O2 pasted and folded, with proof.">
  <meta property="og:type" content="website">
  <meta property="og:url" content="https://kiet1308.github.io/Tovek/">
  <meta property="og:image" content="https://kiet1308.github.io/Tovek/${ASSETS}/og.png">
  <meta property="og:image:width" content="1200">
  <meta property="og:image:height" content="630">
  <meta property="og:image:alt" content="A decompiled Luau method beside its bytecode, each instruction in the colour of the source line it carries.">
  <meta name="twitter:card" content="summary_large_image">
  <link rel="canonical" href="https://kiet1308.github.io/Tovek/">
  <link rel="icon" href="${OLD}/icon.svg" type="image/svg+xml">
  <link rel="preload" href="${ASSETS}/recursive-sans-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="preload" href="${ASSETS}/recursive-mono-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="stylesheet" href="${ASSETS}/landing.css">
  <script>
    (function () {
      var d = document.documentElement;
      d.classList.add('js');
      // the hero first shows the method as medal printed it, then turns it into Tovek's (landing.js); a calm page skips that
      if (window.matchMedia && !matchMedia('(prefers-reduced-motion: reduce)').matches && !matchMedia('(max-width: 759px)').matches) d.classList.add('opening');
      setTimeout(function () { if (!window.__landing) d.classList.remove('opening'); }, 4000);
    })();
  </script>
  <script type="module" src="${ASSETS}/landing.js"></script>
</head>
<body>
<a class="skip" href="#main">Skip to content</a>

<header class="bar" id="top">
  <a class="brand" href="./" aria-label="Tovek, home">${mark}<span class="brand-name">tovek</span></a>
  <nav class="bar-nav" aria-label="Site">
    <a href="#listing">The listing</a>
    <a href="#yours">Your file</a>
    <a href="#measured">Measured</a>
    <a href="watch/">Films</a>
    <a href="changelog.html">Changelog</a>
  </nav>
  <div class="bar-end">
    <a class="bar-link" href="${REPO}">GitHub</a>
    <a class="btn btn-ink btn-s" href="#get">Download<span class="hide-s"> ${V}</span></a>
  </div>
</header>

<main id="main">

  <section class="hero" aria-labelledby="hero-h">
    <div class="plate">
      <h1 id="hero-h">Every instruction remembers its line.</h1>
      <p class="lead">Luau <code>-O2</code> pastes small helpers into their callers and folds calls into bare numbers. Each pasted instruction still carries the helper&rsquo;s line number. Tovek, a free decompiler for Roblox bytecode, uses it to write the call back.</p>
      <div class="actions">
        <a class="btn btn-ink" id="dl-hero" href="#get"><span>Download ${V}</span>${downIcon}</a>
        <label class="btn btn-line"><input type="file" id="file-hero" class="sr-only"><span>Decompile a file here</span></label>
      </div>
      <p class="fine">Open source, MIT. A file you pick is decompiled in your browser, never uploaded.</p>
    </div>

    <div class="weave" id="weave" data-view="tovek">
      <div class="pane pane-read">
        <div class="pane-head">
          <p class="pane-title"><b>${esc(data.hero.title)}</b><span>as printed by</span></p>
          <div class="lens" id="lens" role="radiogroup" aria-label="Show this method as printed by">${lensHtml}</div>
        </div>
        <div class="pane-body" id="read-body">
${viewHtml('tovek', true)}
${viewHtml('medal', false)}
${viewHtml('source', false)}
        </div>
        <p class="pane-note" id="lens-note" data-tovek="${esc(heroViews.tovek.note)}"${heroViews.medal ? ` data-medal="${esc(heroViews.medal.note)}"` : ''} data-source="${esc(heroViews.source.note)}">${heroViews.tovek.note}</p>
      </div>
      <svg class="loom" id="loom" aria-hidden="true"></svg>
      <div class="pane pane-bc">
        <div class="pane-head">
          <p class="pane-title"><b>Bytecode</b><span>${data.hero.instructions} instructions</span></p>
          <p class="keys">${helperKey}</p>
        </div>
        ${BC_COLS}
        <div class="pane-body" id="bc-body">${heroBc}</div>
      </div>
    </div>
  </section>
${callSection}

  <section class="sec reader" id="listing" aria-labelledby="listing-h">
    <header class="sec-head">
      <h2 id="listing-h">Read all ${out.length} lines against the bytecode.</h2>
      <p class="sec-lead"><code>${esc(data.script.file)}</code> is a module we wrote. It was compiled the way Roblox ships scripts, <code>-O2</code> with line info, and printed by Tovek ${V}, unedited. Point at a line to see the instructions that carry it. ${cap(numberWord(notes.length))} notes mark what came back and what could not.</p>
    </header>
    <div class="strip" id="strip">${stripHtml}</div>
    <p class="strip-legend">${helpers.map((h) => `<span class="key hl-${hlOf(h.name)}">${esc(h.name)}</span>`).join('')}<span class="key-tick">pasted into another function</span></p>
    <p class="strip-key">All ${totalRows} instructions in file order, each in the colour of the line it carries. ${cap(numberWord(pastedCount))} highlighted runs sit inside a function that does not own their colour: those are the copies Luau pasted. Tovek writes ${rebuiltCount === pastedCount ? `all ${numberWord(rebuiltCount)}` : fmt(rebuiltCount)} back as calls.</p>

    <div class="frame" id="frame">
      <div class="fr-list">
        <div class="pane-head">
          <p class="pane-title"><b>${esc(data.script.file)}</b><span>Tovek ${V}, ${out.length} lines</span></p>
          <div class="fr-nav">
            <button class="ic" type="button" id="note-prev" aria-label="Previous note">${arrowIcon}</button>
            <p class="fr-count" aria-live="polite"><span>Note</span> <b id="note-at">1</b> <span>of ${notes.length}</span></p>
            <button class="ic" type="button" id="note-next" aria-label="Next note">${arrowIcon}</button>
          </div>
        </div>
        <div class="fr-port" id="port" tabindex="0" role="region" aria-label="${esc(data.script.file)} as Tovek ${V} printed it. Use the arrow keys to scroll.">
          <div class="code" id="code">
${codeHtml}
          </div>
        </div>
      </div>
      <div class="fr-side">
        <div class="fr-bc">
          <div class="pane-head">
            <p class="pane-title"><b>Bytecode</b><span id="bc-title"></span></p>
          </div>
          ${BC_COLS}
          <div class="fr-bcport" id="bcport" tabindex="0" role="region" aria-label="Bytecode of the function being read">
${readerBc}
          </div>
        </div>
        <div class="fr-note" id="note-box">
          <div class="dots" id="dots" role="group" aria-label="Notes">${noteDots}</div>
${notesHtml}
        </div>
      </div>
    </div>
  </section>

  <section class="sec hist" id="history" aria-labelledby="history-h">
    <header class="sec-head">
      <h2 id="history-h">${cap(numberWord(stages.length))} printings of one script.</h2>
      <p class="sec-lead"><code>${esc(data.script.file)}</code> as medal and all ${stages.length - 1} Tovek releases printed it. Drag through them: generated names thin out, and ${allBack ? `by ${esc(allBack.name)} all ${numberWord(pastedCount)} pasted helpers are calls again` : 'the pasted helpers come back as calls'}.${story ? ` <a href="watch/?film=${esc(story.id)}">Watch it as a film, ${clock(story.duration)}</a>` : ''}</p>
    </header>
    <div class="hist-grid">
      <div class="hist-ctl">
        <div class="sils" id="sils" role="group" aria-label="Every printing, as the shape of its text">
${stages.map((s, i) => `<button type="button" class="sil${i === stages.length - 1 ? ' is-on' : ''}${s.changed ? '' : ' is-same'}" data-i="${i}" aria-label="${esc(s.name)}, ${esc(s.date)}"><canvas width="1" height="1"></canvas></button>`).join('')}
        </div>
        <div class="h-eras" aria-hidden="true">${eras.map((e) => `<span${e.b - e.a >= 3 ? ' class="wide"' : ''} style="--a:${e.a};--b:${e.b};--n:${stages.length}">${esc(e.name)}</span>`).join('')}</div>
        <input type="range" id="h-range" min="0" max="${stages.length - 1}" step="1" value="${stages.length - 1}" aria-label="Printed by" aria-valuetext="${esc(last.name)}">
        <p class="h-now"><b id="h-name">${esc(last.name)}</b><span id="h-date">${esc(last.date)}</span><button class="btn btn-line btn-s" type="button" id="h-play" aria-pressed="false">${playIcon}<span>Play every release</span></button></p>
        <dl class="h-facts">
          <div><dt>Lines</dt><dd id="h-lines">${last.lines}</dd></div>
          <div><dt>Generated names</dt><dd id="h-plain">${last.plain}</dd></div>
          <div><dt>Helper calls written back</dt><dd><span id="h-calls">${last.calls}</span><small> of ${pastedCount}</small></dd></div>
          <div><dt>Long decimals</dt><dd id="h-dec">${last.decimals}</dd></div>
        </dl>
        <p class="h-fine">${esc(data.medal_bytecode_note)} Every Tovek release read the same version ${data.script.bytecode_version} bytes.</p>
      </div>
      <div class="hist-code">
        <div class="pane-head"><p class="pane-title"><b>${esc(data.script.file)}</b><span id="h-as">as ${esc(last.name)} printed it</span></p></div>
        <div class="h-port" id="h-port" tabindex="0" role="region" aria-label="The script as the selected release printed it"><div class="code" id="hcode">${histHtml}</div></div>
      </div>
    </div>
  </section>

  <section class="sec yours" id="yours" aria-labelledby="yours-h">
    <header class="sec-head">
      <h2 id="yours-h">Check it on your own file.</h2>
      <p class="sec-lead">Tovek also runs in this page, compiled to WebAssembly. Drop a compiled Luau file or pick a sample, and read the output here.</p>
    </header>
    <div class="own-grid">
      <div class="own-in">
        <div class="drop" id="drop">
          <p class="drop-title"><span class="hide-p">Drop a compiled Luau file here</span><span class="show-p">A compiled Luau file</span></p>
          <p class="drop-text">Raw bytecode from <code>luau-compile --binary</code>, Roblox client bytecode, or a saved script in base64.</p>
          <label class="btn btn-ink"><input type="file" id="file" class="sr-only"><span>Choose a file</span></label>
        </div>
        <p class="samples-h" id="samples-h">Or read a sample</p>
        <ul class="samples" id="samples" aria-labelledby="samples-h">
${samplesHtml}
        </ul>
        <p class="safe"><b>Nothing is uploaded.</b> The file stays in your browser; Tovek decompiles it here.</p>
      </div>
      <div class="out" id="out">
        <div class="pane-head">
          <p class="pane-title"><b id="out-name">Output</b><span id="out-facts"></span></p>
          <div class="out-act">
            <button class="btn btn-line btn-s" type="button" data-act="copy" disabled>Copy</button>
            <button class="btn btn-line btn-s" type="button" data-act="save" disabled>Save .luau</button>
          </div>
        </div>
        <div class="out-body" id="out-body" tabindex="-1" role="region" aria-label="Decompiled output">
          <div class="out-empty" id="out-empty">
            <p>The output appears here, decompiled in your browser.</p>
            <button class="btn btn-line" type="button" id="out-try">Try it on Beacon</button>
          </div>
        </div>
        <p class="out-status" id="out-status" role="status" aria-live="polite"></p>
      </div>
    </div>
  </section>

  <section class="sec measured" id="measured" aria-labelledby="measured-h">
    <header class="sec-head">
      <h2 id="measured-h">Measured, not claimed.</h2>
      <p class="sec-lead">Bytecode does not pin down one source, so Tovek writes code that behaves the same. These are the checks ${V} passed. <a href="changelog/${data.release.tag}/#footnotes">How each was measured</a></p>
    </header>
    <div class="m-rows">${measuredHtml}
    </div>
  </section>

  <section class="sec rel" id="releases" aria-labelledby="rel-h">
    <header class="sec-head">
      <h2 id="rel-h">Two films and every change.</h2>
      <p class="sec-lead">Both films play in your browser, drawn live from their source. <a href="watch/">Open the cinema</a></p>
    </header>
    <div class="rel-grid">
${filmsHtml}
      <div class="clog">
        <h3>Changelog</h3>
        <ol class="clog-list">
${changesHtml}
        </ol>
        <p class="clog-all"><a href="changelog.html">Every release</a><a href="${REPO}/releases">Earlier betas on GitHub</a></p>
      </div>
    </div>
  </section>

  <section class="sec get" id="get" aria-labelledby="get-h">
    <header class="sec-head">
      <h2 id="get-h">Get Tovek ${V}.</h2>
      <p class="sec-lead">Each download holds two programs, <code>luau-lifter</code> and <code>web-server</code>, and the licence. It is free, and it runs on your machine. <a href="${REPO}/releases/tag/${data.release.tag}">All files and checksums</a></p>
    </header>
    <ul class="dls">
${downloadsHtml}
    </ul>
    <div class="uses">
      ${usageBlock('Command line', data.usage.cli)}
      ${usageBlock('Local server', data.usage.server)}
    </div>
    <p class="use-more">Every flag, the executor client scripts and the batch format are in <a href="${REPO}#usage">the README</a>.</p>
  </section>
</main>

<footer class="foot">
  <div class="foot-in">
    <a class="brand" href="./" aria-label="Tovek, home">${mark}<span class="brand-name">tovek</span></a>
    <p class="credit">Tovek began as a fork of <a href="${esc(data.release.medal)}">medal</a>, the Luau decompiler originally written by Jujhar Singh and Mathias Pedersen.</p>
    <nav class="foot-nav" aria-label="Footer">
      <a href="${REPO}">GitHub</a>
      <a href="${esc(data.release.discord)}">Discord</a>
      <a href="${REPO}/blob/main/LICENSE.txt">MIT licence</a>
      <a href="changelog.html">Changelog</a>
      <a href="watch/">Films</a>
    </nav>
  </div>
</footer>

<script type="application/json" id="page-data">${JSON.stringify(pageData)}</script>
<noscript><style>.lens,.fr-nav,.dots,.own-in .drop,.samples,.out-act,.sils,#h-range,#h-play{display:none!important}.note[hidden]{display:block!important}.bcp[hidden]{display:block!important}.fr-note{max-height:none!important}</style></noscript>
</body>
</html>
`;

// ------------------------------------------------------------------ report and write

const linked = [...linesWithRows.keys()].length;
console.log(`landing: ${out.length} printed lines, ${totalRows} instructions, ${linked} lines carry instructions, ${pastedRuns} pasted runs, ${notes.length} notes`);
for (const n of notices) console.log('  notice: ' + n);
if (!CHECK) {
  fs.writeFileSync(path.join(ROOT, 'docs/index.html'), html);
  console.log(`wrote docs/index.html (${(html.length / 1024).toFixed(0)} KB)`);
}
