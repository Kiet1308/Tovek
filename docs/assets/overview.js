/* Tovek — Overview interactions. No dependencies. */
(() => {
  'use strict';

  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const esc = (s) => s.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
  const clamp = (v, a, b) => Math.min(b, Math.max(a, v));

  /* ------------------------------------------------------------ highlighting */

  const KW = new Set('and break continue do else elseif end false for function if in local nil not or repeat return then true until while'.split(' '));
  const GLOBALS = new Set('game workspace script Enum Color3 UDim UDim2 TweenInfo Vector3 math table string os setmetatable require warn print typeof self'.split(' '));
  const TOK = /(--[^\n]*)|("(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*')|(`(?:[^`\\]|\\.)*`)|(\d+(?:\.\d+)?)|([A-Za-z_]\w*)|([ \t]+|\n)|([\s\S])/g;

  function hlLuau(src, names) {
    let out = '';
    let prev = '';
    let fnHead = false;
    for (const m of src.matchAll(TOK)) {
      const [t, com, str, tpl, num, id, ws, other] = m;
      if (ws !== undefined) { out += ws; continue; }
      if (com !== undefined) out += `<span class="c">${esc(com)}</span>`;
      else if (str !== undefined) out += `<span class="s">${esc(str)}</span>`;
      else if (tpl !== undefined) out += hlTemplate(tpl, names);
      else if (num !== undefined) out += `<span class="n">${num}</span>`;
      else if (id !== undefined) {
        const field = prev === '.' || prev === ':';
        if (!field && KW.has(id)) {
          out += `<span class="k">${id}</span>`;
          if (id === 'function') fnHead = true;
          prev = 'kw';
          continue;
        }
        const cls = fnHead ? 'fn' : field ? 'f' : GLOBALS.has(id) ? 'g' : 'v';
        if (!field && names && names.has(id)) out += `<span class="nm ${cls}" data-nm="${id}">${id}</span>`;
        else out += `<span class="${cls}">${id}</span>`;
      } else {
        if (other === '(') fnHead = false;
        out += `<span class="p">${esc(other)}</span>`;
      }
      prev = t[t.length - 1];
    }
    return out;
  }

  function hlTemplate(s, names) {
    let out = '';
    let last = 0;
    const re = /\{([^}]*)\}/g;
    let m;
    while ((m = re.exec(s))) {
      out += `<span class="s">${esc(s.slice(last, m.index))}</span><span class="ip">{</span>${hlLuau(m[1], names)}<span class="ip">}</span>`;
      last = m.index + m[0].length;
    }
    return out + `<span class="s">${esc(s.slice(last))}</span>`;
  }

  function hlAsm(line) {
    const m = line.match(/^(L\d+:)?\s*(\S+)(.*)$/);
    if (!m) return esc(line);
    const [, lab, op, rest] = m;
    const body = esc(rest).replace(/(\[[^\]]*\])|\b(R\d+)\b|\b(K\d+)\b|\b(L\d+)\b|(-?\b\d+\b)/g,
      (t, c, r, k, l, n) => c ? `<span class="s">${c}</span>`
        : r ? `<span class="r">${r}</span>`
        : k ? `<span class="kk">${k}</span>`
        : l ? `<span class="lb">${l}</span>`
        : `<span class="n">${n}</span>`);
    const label = lab ? `<span class="lb">${lab}</span> ` : '    ';
    return `${label}<span class="op">${op}</span>${body}`;
  }

  function intoLines(pre, html) {
    pre.innerHTML = html.split('\n')
      .map((l, i) => `<span class="ln" data-n="${i + 1}">${l || ' '}</span>`)
      .join('');
  }

  function source(pre) {
    return pre.textContent.replace(/\s+$/, '');
  }

  const nameSet = new Set();
  $$('#evi li').forEach((li) => li.dataset.names.split(' ').forEach((n) => nameSet.add(n)));

  $$('pre[data-lang]').forEach((pre) => {
    const text = source(pre);
    if (pre.dataset.lang === 'asm') {
      intoLines(pre, text.split('\n').map(hlAsm).join('\n'));
    } else {
      intoLines(pre, hlLuau(text, pre.id === 'nm-code' ? nameSet : null));
    }
  });

  // hero film: highlight but keep it light (no line spans needed)
  $$('.fc pre').forEach((pre) => { pre.innerHTML = hlLuau(source(pre)); });

  /* ------------------------------------------------------------ bands, marks, hover maps */

  function rangeList(spec) {
    const out = [];
    spec.split(',').forEach((part) => {
      const [a, b] = part.split('-').map(Number);
      for (let i = a; i <= (b || a); i++) out.push(i);
    });
    return out;
  }

  $$('pre[data-bands]').forEach((pre) => {
    const lines = $$('.ln', pre);
    pre.dataset.bands.split('|').forEach((entry) => {
      const [range, key, label] = entry.split(':');
      rangeList(range).forEach((n, i) => {
        const ln = lines[n - 1];
        if (!ln) return;
        ln.classList.add(`b-${key}`);
        ln.dataset.k = key;
        if (i === 0 && label) ln.dataset.label = label;
      });
    });
  });

  $$('pre[data-marks]').forEach((pre) => {
    const lines = $$('.ln', pre);
    pre.dataset.marks.split('|').forEach((entry) => {
      const [n, key] = entry.split(':');
      const ln = lines[Number(n) - 1];
      if (!ln) return;
      ln.classList.add(`b-${key}`, `m-${key}`);
      ln.dataset.k = key;
    });
  });

  function wireMap(src) {
    const dst = document.getElementById(src.dataset.target);
    if (!dst) return;
    const sl = $$('.ln', src);
    const dl = $$('.ln', dst);
    const fwd = new Map();
    const back = new Map();
    const push = (map, k, v) => { if (!map.has(k)) map.set(k, []); map.get(k).push(v); };
    src.dataset.map.split('|').forEach((entry) => {
      const [a, b] = entry.split(':');
      rangeList(a).forEach((i) => rangeList(b).forEach((j) => { push(fwd, i, j); push(back, j, i); }));
    });
    const holder = src.closest('.spec-cf, .spec-di');
    const clear = () => {
      const arcs = $('.arcs', src);
      holder.classList.remove('is-mapping');
      sl.concat(dl).forEach((l) => l.classList.remove('is-hot'));
      if (arcs) { arcs.classList.remove('has-hot'); $$('.is-hot', arcs).forEach((a) => a.classList.remove('is-hot')); }
    };
    const light = (srcIdx, dstIdx) => {
      clear();
      if (!srcIdx.length && !dstIdx.length) return;
      holder.classList.add('is-mapping');
      srcIdx.forEach((i) => sl[i - 1] && sl[i - 1].classList.add('is-hot'));
      dstIdx.forEach((j) => dl[j - 1] && dl[j - 1].classList.add('is-hot'));
      const arcs = $('.arcs', src);
      if (arcs) {
        const hot = $$(`[data-from]`, arcs).filter((a) => srcIdx.includes(Number(a.dataset.from)));
        if (hot.length) { arcs.classList.add('has-hot'); hot.forEach((a) => a.classList.add('is-hot')); }
      }
    };
    src.addEventListener('pointerover', (e) => {
      const ln = e.target.closest('.ln');
      if (!ln) return;
      const i = Number(ln.dataset.n);
      const targets = fwd.get(i) || [];
      const siblings = new Set();
      targets.forEach((j) => (back.get(j) || []).forEach((k) => siblings.add(k)));
      light(targets.length ? [...siblings].filter((k) => sameGroup(fwd, k, i)) : [i], targets);
    });
    dst.addEventListener('pointerover', (e) => {
      const ln = e.target.closest('.ln');
      if (!ln) return;
      const j = Number(ln.dataset.n);
      light(back.get(j) || [], back.has(j) ? [j] : []);
    });
    src.addEventListener('pointerleave', clear);
    dst.addEventListener('pointerleave', clear);
  }
  // an instruction lights the output lines it maps to, plus the other instructions of that same mapping entry
  function sameGroup(fwd, k, i) {
    const a = (fwd.get(k) || []).join();
    return a === (fwd.get(i) || []).join();
  }
  $$('pre[data-map]').forEach(wireMap);

  /* ------------------------------------------------------------ jump arcs */

  function drawArcs(pre) {
    let svg = $('.arcs', pre);
    if (!svg) {
      svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
      svg.setAttribute('class', 'arcs');
      svg.setAttribute('aria-hidden', 'true');
      pre.prepend(svg);
    }
    const lines = $$('.ln', pre);
    const labels = {};
    lines.forEach((ln, i) => {
      const m = ln.textContent.match(/^\s*(L\d+):/);
      if (m) labels[m[1]] = i;
    });
    const jumps = [];
    lines.forEach((ln, i) => {
      const body = ln.textContent.replace(/^\s*L\d+:\s*/, '');
      const m = body.match(/\b(L\d+)\b/);
      if (m && labels[m[1]] !== undefined) jumps.push({ from: i, to: labels[m[1]] });
    });
    // lanes: short spans hug the code, long spans sit further out
    jumps.sort((a, b) => Math.abs(a.to - a.from) - Math.abs(b.to - b.from));
    const lanes = [];
    jumps.forEach((j) => {
      const lo = Math.min(j.from, j.to);
      const hi = Math.max(j.from, j.to);
      let lane = 0;
      while ((lanes[lane] || []).some(([a, b]) => !(hi < a || lo > b))) lane++;
      (lanes[lane] = lanes[lane] || []).push([lo, hi]);
      j.lane = lane;
    });
    const right = 108;
    const step = 10;
    const mid = (ln) => ln.offsetTop + ln.offsetHeight / 2;
    let html = '';
    jumps.forEach((j) => {
      const y1 = mid(lines[j.from]);
      const y2 = mid(lines[j.to]);
      const x = right - 16 - j.lane * step;
      const r = Math.min(6, Math.abs(y2 - y1) / 2);
      const dir = y2 > y1 ? 1 : -1;
      const back = j.to <= j.from;
      const d = `M${right} ${y1}H${x + r}Q${x} ${y1} ${x} ${y1 + r * dir}V${y2 - r * dir}Q${x} ${y2} ${x + r} ${y2}H${right - 1}`;
      html += `<path class="arc${back ? ' arc--back' : ''}" data-from="${j.from + 1}" pathLength="1" d="${d}"/>`;
      html += `<path class="arc-head${back ? ' arc-head--back' : ''}" data-from="${j.from + 1}" d="M${right - 6} ${y2 - 3.5}L${right} ${y2}L${right - 6} ${y2 + 3.5}Z"/>`;
    });
    svg.setAttribute('height', pre.scrollHeight);
    svg.setAttribute('viewBox', `0 0 112 ${pre.scrollHeight}`);
    svg.style.height = `${pre.scrollHeight}px`;
    svg.innerHTML = html;
  }
  const arcPres = $$('pre[data-arcs]');

  /* ------------------------------------------------------------ de-inline connectors */

  const spec = $('#spec-di');
  const links = $('#di-links');
  function drawLinks() {
    if (!spec || !links) return;
    const asm = $('#di-asm');
    const pane = $('.pane:not([hidden]) .code', spec);
    const sr = spec.getBoundingClientRect();
    const ar = asm.getBoundingClientRect();
    const orr = pane.getBoundingClientRect();
    if (orr.left < ar.right) { links.replaceChildren(); return; }
    links.setAttribute('viewBox', `0 0 ${sr.width} ${sr.height}`);
    ['a', 'b'].forEach((k) => {
      const band = $$(`.ln.b-${k}`, asm);
      const mark = $(`.ln.m-${k}`, pane);
      let g = $(`.lk-${k}`, links);
      if (!band.length || !mark) { if (g) g.remove(); return; }
      const b1 = band[0].getBoundingClientRect();
      const b2 = band[band.length - 1].getBoundingClientRect();
      const m = mark.getBoundingClientRect();
      const x1 = ar.right - sr.left;
      const x2 = orr.left - sr.left;
      const y1a = b1.top - sr.top;
      const y1b = b2.bottom - sr.top;
      const y2a = m.top - sr.top;
      const y2b = m.bottom - sr.top;
      const c = (x2 - x1) * 0.55;
      const top = `M${x1} ${y1a}C${x1 + c} ${y1a} ${x2 - c} ${y2a} ${x2} ${y2a}`;
      const bot = `M${x1} ${y1b}C${x1 + c} ${y1b} ${x2 - c} ${y2b} ${x2} ${y2b}`;
      const fill = `${top}L${x2} ${y2b}C${x2 - c} ${y2b} ${x1 + c} ${y1b} ${x1} ${y1b}Z`;
      if (!g) {
        links.insertAdjacentHTML('beforeend', `<g class="lk lk-${k}"><path class="lk-fill"/><path class="lk-edge" pathLength="1"/><path class="lk-edge" pathLength="1"/></g>`);
        g = $(`.lk-${k}`, links);
      }
      const [pf, pt, pb] = g.children;
      pf.setAttribute('d', fill);
      pt.setAttribute('d', top);
      pb.setAttribute('d', bot);
    });
  }

  if (spec) {
    spec.addEventListener('pointerover', (e) => {
      const ln = e.target.closest('.ln[data-k]');
      spec.classList.remove('focus-a', 'focus-b');
      if (ln && (ln.dataset.k === 'a' || ln.dataset.k === 'b')) spec.classList.add(`focus-${ln.dataset.k}`);
    });
    spec.addEventListener('pointerleave', () => spec.classList.remove('focus-a', 'focus-b'));

    const tabs = $$('[role="tab"]', spec);
    const select = (tab) => {
      tabs.forEach((t) => {
        const on = t === tab;
        t.setAttribute('aria-selected', String(on));
        t.tabIndex = on ? 0 : -1;
        document.getElementById(t.getAttribute('aria-controls')).hidden = !on;
      });
      drawLinks();
    };
    tabs.forEach((t, i) => {
      t.addEventListener('click', () => select(t));
      t.addEventListener('keydown', (e) => {
        if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
        const next = tabs[(i + (e.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length];
        select(next);
        next.focus();
      });
    });
  }

  /* ------------------------------------------------------------ names: redact → declassify */

  const sheet = $('#sheet');
  const evi = $$('#evi li');
  const nameEls = sheet ? $$('.nm', sheet) : [];
  let revealTimers = [];

  evi.forEach((li, gi) => {
    const fb = li.classList.contains('fb');
    li.dataset.g = gi;
    li.dataset.names.split(' ').forEach((n) => {
      nameEls.filter((el) => el.dataset.nm === n).forEach((el) => {
        el.dataset.g = gi;
        if (fb) el.classList.add('fb');
      });
    });
    li.addEventListener('pointerenter', () => hotGroup(gi, true));
    li.addEventListener('pointerleave', () => hotGroup(gi, false));
  });
  if (sheet) {
    sheet.addEventListener('pointerover', (e) => {
      const el = e.target.closest('.nm');
      sheet.classList.remove('has-hot');
      evi.forEach((li) => li.classList.remove('is-hot'));
      nameEls.forEach((n) => n.classList.remove('is-hot'));
      if (el && el.classList.contains('is-shown')) hotGroup(Number(el.dataset.g), true);
    });
    sheet.addEventListener('pointerleave', () => { sheet.classList.remove('has-hot'); evi.forEach((li) => li.classList.remove('is-hot')); nameEls.forEach((n) => n.classList.remove('is-hot')); });
  }
  function hotGroup(gi, on) {
    evi[gi] && evi[gi].classList.toggle('is-hot', on);
    if (sheet) sheet.classList.toggle('has-hot', on);
    nameEls.forEach((el) => { if (Number(el.dataset.g) === gi) el.classList.toggle('is-hot', on && el.classList.contains('is-shown')); });
  }
  function redact() {
    revealTimers.forEach(clearTimeout);
    revealTimers = [];
    nameEls.forEach((el) => el.classList.remove('is-shown', 'is-hot'));
    evi.forEach((li) => li.classList.remove('is-done'));
  }
  function declassify(delay = 450) {
    evi.forEach((li, gi) => {
      revealTimers.push(setTimeout(() => {
        nameEls.forEach((el) => { if (Number(el.dataset.g) === gi) el.classList.add('is-shown'); });
        li.classList.add('is-done');
      }, reduce ? 0 : delay + gi * 420));
    });
  }
  const replay = $('#sheet-replay');
  if (replay) replay.addEventListener('click', () => { redact(); requestAnimationFrame(() => declassify(700)); });

  /* ------------------------------------------------------------ hero: bytes + lens */

  const hero = $('#hero');
  const hx = $('#hx');
  const hf = $('#hf');
  const ht = $('#ht');
  const lens = $('#lens');
  const roA = $('#ro-a');
  const roB = $('#ro-b');
  const roBk = $('#ro-b-k');
  const bytesEl = $('#hero-bytes');
  let bytes = new Uint8Array(0);
  try { bytes = Uint8Array.from(atob(bytesEl.textContent.trim()), (c) => c.charCodeAt(0)); } catch (e) { /* keep empty */ }

  const hex2 = (n) => n.toString(16).toUpperCase().padStart(2, '0');
  const hex4 = (n) => n.toString(16).toUpperCase().padStart(4, '0');
  const hexGeom = { rows: 1, colW: 1, left: 0, top: 0, lineH: 22 };

  function renderHex() {
    if (!bytes.length) return;
    const cs = getComputedStyle(hx);
    const lineH = parseFloat(cs.lineHeight) || 22;
    const top = parseFloat(cs.paddingTop) || 0;
    const rows = Math.ceil((hero.clientHeight - top) / lineH) + 1;
    const colsWanted = 4;
    let html = '';
    for (let c = 0; c < colsWanted; c++) {
      let col = '';
      for (let r = 0; r < rows; r++) {
        const off = ((c * rows + r) * 16) % bytes.length;
        let h = '';
        let a = '';
        for (let k = 0; k < 16; k++) {
          const b = bytes[(off + k) % bytes.length];
          h += hex2(b) + (k === 7 ? '  ' : ' ');
          a += b >= 33 && b <= 126 ? String.fromCharCode(b) : '·';
        }
        col += `<i>${hex4(off)}</i>  ${h} <b>${esc(a)}</b>\n`;
      }
      html += `<div class="hx-col">${col}</div>`;
    }
    hx.innerHTML = html;
    const first = hx.firstElementChild;
    const gap = parseFloat(cs.columnGap) || 44;
    hexGeom.rows = rows;
    hexGeom.colW = first ? first.offsetWidth + gap : 600;
    hexGeom.left = first ? first.offsetLeft : 0;
    hexGeom.top = top;
    hexGeom.lineH = lineH;
  }

  // the film carries an outlined twin of the headline
  if (ht && hf) {
    const twin = ht.cloneNode(true);
    twin.removeAttribute('id');
    $$('[id]', twin).forEach((el) => el.removeAttribute('id'));
    hf.appendChild(twin);
  }

  const filmCols = $$('.fc-col', hf || document);
  let W = 0;
  let H = 0;
  let baseR = 170;
  let maxR = 1600;
  let x = 0;
  let y = 0;
  let tx = 0;
  let ty = 0;
  let r = 0;
  let manualUntil = 0;
  let moved = false;
  let expanded = false;
  let heroVisible = true;
  let raf = 0;
  const t0 = performance.now();
  const toggle = $('#xray-toggle');

  function sizeHero() {
    W = hero.clientWidth;
    H = hero.clientHeight;
    baseR = W < 760 ? 108 : clamp(W * 0.115, 140, 196);
    maxR = Math.hypot(W, H);
    if (!x && !y) { x = tx = W * (W < 760 ? 0.5 : 0.36); y = ty = H * 0.5; }
    const hc = $('#hc');
    if (hc && getComputedStyle(hc).position !== 'absolute') {
      hero.style.setProperty('--ht-pb', `${hc.offsetHeight + 48}px`);
    } else {
      hero.style.removeProperty('--ht-pb');
    }
    renderHex();
    measureFilm();
  }

  // film geometry is cached on layout so the animation loop never reads layout
  let filmGeom = [];
  function measureFilm() {
    filmGeom = filmCols.map((c) => {
      const left = c.offsetLeft;
      let acc = c.offsetTop;
      const tabs = $$('.fc-tab', c);
      const parts = $$('pre', c).map((pre, i) => {
        acc += tabs[i] ? tabs[i].offsetHeight : 0;
        const cs = getComputedStyle(pre);
        const part = {
          name: tabs[i] ? tabs[i].querySelector('b').textContent.replace(/\..*$/, '') : 'Sprint',
          top: acc,
          h: pre.offsetHeight,
          lh: parseFloat(cs.lineHeight) || 28,
        };
        acc += pre.offsetHeight + (parseFloat(cs.marginBottom) || 0);
        return part;
      });
      return { left, w: c.offsetWidth, h: acc - c.offsetTop, parts };
    });
    filmStatic = { x: clamp(W * 0.04, 16, 56), y: W < 760 ? 116 : 150 };
  }

  let lastRo = '';
  let filmStatic = { x: 56, y: 150 };
  let fcx = 56;
  let fcy = 150;
  const fc = $('.fc', hf || document);
  function readouts() {
    const col = Math.max(0, Math.floor((x - hexGeom.left) / hexGeom.colW));
    const row = Math.max(0, Math.floor((y - hexGeom.top) / hexGeom.lineH));
    const off = bytes.length ? ((col * hexGeom.rows + row) * 16) % bytes.length : 0;
    let name = '';
    let line = 0;
    if (moved) {
      const lx = x - fcx;
      const ly = y - fcy;
      for (const c of filmGeom) {
        if (lx < c.left - 40) continue;
        for (let i = 0; i < c.parts.length; i++) {
          const part = c.parts[i];
          if (ly < part.top + part.h || i === c.parts.length - 1) {
            name = part.name;
            line = clamp(Math.floor((ly - part.top) / part.lh) + 1, 1, Math.round(part.h / part.lh));
            break;
          }
        }
      }
    }
    const key = `${off}|${name}|${line}`;
    if (key === lastRo) return;
    lastRo = key;
    roA.textContent = `0x${hex4(off)}`;
    if (moved) {
      roBk.textContent = name;
      roB.textContent = `ln ${String(line).padStart(2, '0')}`;
    }
  }

  function frame(now) {
    raf = 0;
    if (!heroVisible) return;
    const t = (now - t0) / 1000;
    const auto = now > manualUntil;
    if (auto) {
      const narrow = W < 760;
      tx = W * ((narrow ? 0.5 : 0.36) + (narrow ? 0.3 : 0.2) * Math.sin(t * 0.29));
      ty = H * ((narrow ? 0.4 : 0.5) + (narrow ? 0.2 : 0.22) * Math.sin(t * 0.43 + 1.1));
    }
    const k = auto ? 0.035 : 0.2;
    x += (tx - x) * k;
    y += (ty - y) * k;

    const rect = hero.getBoundingClientRect();
    const p = clamp(-rect.top / (H * 0.7), 0, 1);
    const ease = p * p * (3 - 2 * p);
    const goal = expanded ? maxR : baseR + (maxR - baseR) * ease;
    r += (goal - r) * (expanded ? 0.08 : 0.14);

    placeFilm();
    hero.style.setProperty('--lx', `${x.toFixed(1)}px`);
    hero.style.setProperty('--ly', `${y.toFixed(1)}px`);
    hero.style.setProperty('--lr', `${r.toFixed(1)}px`);
    hero.style.setProperty('--rot', `${(x * 0.12 + y * 0.05).toFixed(1)}deg`);
    lens.classList.toggle('is-wide', r > baseR * 1.6);
    readouts();
    raf = requestAnimationFrame(frame);
  }
  const kick = () => { if (!raf && heroVisible) raf = requestAnimationFrame(frame); };

  // Like a loupe over a page: the code slides so the lens always sits on text,
  // scanning the file as the lens travels. Fully open, the file settles in place.
  function placeFilm() {
    if (!fc || !filmGeom.length) return;
    const first = filmGeom[0];
    const open = clamp((r - baseR) / (Math.max(maxR * 0.5, baseR + 1) - baseR), 0, 1);
    const fx = clamp(x / W, 0, 1);
    const fy = clamp(y / H, 0, 1);
    const scanX = x - (24 + fx * first.w * 0.58);
    const scanY = y - (18 + fy * (first.h - 60));
    fcx = scanX + (filmStatic.x - scanX) * open;
    fcy = scanY + (filmStatic.y - scanY) * open;
    fc.style.transform = `translate3d(${fcx.toFixed(1)}px, ${fcy.toFixed(1)}px, 0)`;
  }

  if (hero && !reduce) {
    hero.classList.add('is-armed');
    hero.addEventListener('pointermove', (e) => {
      if (e.pointerType === 'touch' && !e.isPrimary) return;
      const b = hero.getBoundingClientRect();
      tx = e.clientX - b.left;
      ty = e.clientY - b.top;
      manualUntil = performance.now() + (e.pointerType === 'touch' ? 2400 : 3600);
      moved = true;
    }, { passive: true });
    hero.addEventListener('click', (e) => {
      if (e.target.closest('a, button, .hc')) return;
      setExpanded(!expanded);
    });
    new IntersectionObserver((ents) => {
      heroVisible = ents[0].isIntersecting;
      kick();
    }).observe(hero);
  } else if (hero) {
    // reduced motion: a still lens over the headline
    requestAnimationFrame(() => {
      sizeHero();
      x = W * 0.62; y = H * 0.44; r = baseR;
      hero.style.setProperty('--lx', `${x}px`);
      hero.style.setProperty('--ly', `${y}px`);
      hero.style.setProperty('--lr', `${r}px`);
      moved = true;
      placeFilm();
      readouts();
    });
  }
  function setExpanded(on) {
    expanded = on;
    if (toggle) toggle.setAttribute('aria-pressed', String(on));
    if (reduce) {
      r = on ? maxR : baseR;
      hero.style.setProperty('--lr', `${r}px`);
      placeFilm();
    }
    kick();
  }
  if (toggle) toggle.addEventListener('click', () => setExpanded(!expanded));

  /* ------------------------------------------------------------ header + tabs */

  const top = $('#top');
  let specVisible = false;
  let linkRaf = 0;
  if (spec) new IntersectionObserver((ents) => { specVisible = ents[0].isIntersecting; }).observe(spec);
  const onScroll = () => {
    top.classList.toggle('is-scrolled', window.scrollY > 24);
    kick();
    if (specVisible && !linkRaf) linkRaf = requestAnimationFrame(() => { linkRaf = 0; drawLinks(); });
  };
  addEventListener('scroll', onScroll, { passive: true });
  onScroll();

  const tabsNav = $('.tabs');
  const thumb = $('.tab-thumb');
  const tabEls = $$('.tab', tabsNav);
  const current = tabEls.find((t) => t.getAttribute('aria-current') === 'page') || tabEls[0];
  function placeThumb(el, instant) {
    if (!thumb || !el) return;
    if (instant) thumb.style.transition = 'none';
    thumb.style.width = `${el.offsetWidth}px`;
    thumb.style.transform = `translateX(${el.offsetLeft}px)`;
    tabEls.forEach((t) => t.classList.toggle('is-on', t === el));
    if (instant) { void thumb.offsetWidth; thumb.style.transition = ''; }
  }
  if (tabsNav) {
    tabsNav.classList.add('is-live');
    tabEls.forEach((t) => t.addEventListener('pointerenter', () => placeThumb(t)));
    tabsNav.addEventListener('pointerleave', () => placeThumb(current));
    tabEls.forEach((t) => t.addEventListener('focus', () => placeThumb(t)));
    tabEls.forEach((t) => t.addEventListener('blur', () => placeThumb(current)));
  }

  /* ------------------------------------------------------------ iris to changelog */

  const iris = $('#iris');
  document.addEventListener('click', (e) => {
    const a = e.target.closest('a[data-iris]');
    if (!a || e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    if (reduce || !iris) return;
    e.preventDefault();
    let cx = e.clientX;
    let cy = e.clientY;
    if (!cx && !cy) {
      const b = a.getBoundingClientRect();
      cx = b.left + b.width / 2;
      cy = b.top + b.height / 2;
    }
    if (a.closest('.tabs')) placeThumb(a);
    iris.style.setProperty('--ix', `${cx}px`);
    iris.style.setProperty('--iy', `${cy}px`);
    requestAnimationFrame(() => iris.classList.add('is-on'));
    setTimeout(() => { location.href = a.href; }, 640);
  });
  addEventListener('pageshow', (e) => {
    if (e.persisted && iris) {
      iris.style.transition = 'none';
      iris.classList.remove('is-on');
      void iris.offsetWidth;
      iris.style.transition = '';
      placeThumb(current, true);
    }
  });

  /* ------------------------------------------------------------ copy */

  const live = $('#live');
  $$('[data-copy]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const text = document.getElementById(btn.dataset.copy).innerText.trim();
      let ok = false;
      try { await navigator.clipboard.writeText(text); ok = true; } catch (err) {
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.setAttribute('readonly', '');
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        try { ok = document.execCommand('copy'); } catch (e2) { ok = false; }
        ta.remove();
      }
      btn.textContent = ok ? 'Copied' : 'Select + copy';
      btn.classList.toggle('is-done', ok);
      if (live) live.textContent = ok ? 'Copied to clipboard' : 'Copy failed';
      setTimeout(() => { btn.textContent = 'Copy'; btn.classList.remove('is-done'); }, 1800);
    });
  });

  /* ------------------------------------------------------------ count-up */

  function countUp(el) {
    const to = parseFloat(el.dataset.count);
    const dec = Number(el.dataset.dec || 0);
    const fmt = (v) => v.toLocaleString('en-US', { minimumFractionDigits: dec, maximumFractionDigits: dec });
    if (reduce) { el.textContent = fmt(to); return; }
    const start = performance.now();
    const dur = 1600;
    const step = (now) => {
      const p = clamp((now - start) / dur, 0, 1);
      const e = 1 - Math.pow(1 - p, 4);
      el.textContent = fmt(to * e);
      if (p < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }

  /* ------------------------------------------------------------ reveals */

  const io = new IntersectionObserver((ents) => {
    ents.forEach((ent) => {
      if (!ent.isIntersecting) return;
      const el = ent.target;
      el.classList.add('is-in');
      io.unobserve(el);
      if (el.contains(sheet)) declassify();
      $$('[data-count]', el).forEach(countUp);
    });
  }, { threshold: 0.06, rootMargin: '0px 0px -10% 0px' });
  $$('[data-reveal]').forEach((el) => io.observe(el));

  /* ------------------------------------------------------------ layout */

  let resizeRaf = 0;
  function layout() {
    resizeRaf = 0;
    if (hero) sizeHero();
    arcPres.forEach(drawArcs);
    drawLinks();
    kick();
  }
  const relayout = () => { if (!resizeRaf) resizeRaf = requestAnimationFrame(layout); };
  addEventListener('resize', relayout);
  if ('ResizeObserver' in window) {
    const ro = new ResizeObserver(relayout);
    [spec, ...arcPres].forEach((el) => el && ro.observe(el));
  }
  layout();
  placeThumb(current, true);
  if (document.fonts && document.fonts.ready) document.fonts.ready.then(() => { layout(); placeThumb(current, true); });
})();
