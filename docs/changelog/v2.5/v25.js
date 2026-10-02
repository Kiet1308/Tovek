/* Tovek V2.5 release notes: hero art, bar charts, carousels, accordions. No dependencies. */
(() => {
  'use strict';
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const DATA = JSON.parse($('#v25-data').textContent);
  const NS = 'http://www.w3.org/2000/svg';
  const el = (name, attrs = {}, parent) => {
    const node = document.createElementNS(NS, name);
    for (const k in attrs) node.setAttribute(k, attrs[k]);
    if (parent) parent.appendChild(node);
    return node;
  };
  const fmt = (v, spec) => {
    if (spec && spec.fmt === 'pct') return `${(+v).toFixed(spec.digits ?? 1)}%`;
    if (spec && spec.fmt === 's') return `${(+v).toFixed(spec.digits ?? 2)} s`;
    return (+v).toLocaleString('en-US', { maximumFractionDigits: spec?.digits ?? 0 });
  };

  /* ------------------------------------------------------------ hero art */

  function rng(seed) {
    let s = seed >>> 0;
    return () => {
      s = (s + 0x6d2b79f5) >>> 0;
      let t = Math.imul(s ^ (s >>> 15), s | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  function paintHero(canvas, shift = 0) {
    const dpr = Math.min(devicePixelRatio || 1, 2);
    const W = canvas.clientWidth, H = canvas.clientHeight;
    if (!W || !H) return;
    if (canvas.width !== Math.round(W * dpr)) {
      canvas.width = Math.round(W * dpr);
      canvas.height = Math.round(H * dpr);
    }
    const g = canvas.getContext('2d');
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const r = rng(25);

    const base = g.createLinearGradient(0, H, W, 0);
    base.addColorStop(0, '#020f3a');
    base.addColorStop(0.35, '#0b3aa8');
    base.addColorStop(0.62, '#1f6fe5');
    base.addColorStop(1, '#8fd0ff');
    g.fillStyle = base;
    g.fillRect(0, 0, W, H);
    const glow = (x, y, rad, color) => {
      const rg = g.createRadialGradient(x, y, 0, x, y, rad);
      rg.addColorStop(0, color);
      rg.addColorStop(1, 'rgba(0,0,0,0)');
      g.fillStyle = rg;
      g.fillRect(0, 0, W, H);
    };
    glow(W * 0.78, H * 0.05, W * 0.5, 'rgba(190,230,255,.55)');
    glow(W * 0.28, H * 0.42, W * 0.42, 'rgba(60,140,255,.35)');
    glow(W * 0.1, H * 1.05, W * 0.55, 'rgba(0,6,30,.75)');

    // Fine dot texture drifting across the upper right, as on frosted glass.
    for (let i = 0; i < 2600; i++) {
      const x = W * (0.45 + r() * 0.55), y = H * r() * 0.8;
      const fall = Math.max(0, 1 - Math.hypot((x / W - 0.86) * 1.6, (y / H - 0.25) * 1.2));
      if (r() > fall) continue;
      g.fillStyle = `rgba(255,255,255,${0.12 + r() * 0.35 * fall})`;
      g.fillRect(x, y, 1.2, 1.2);
    }

    // The release number: a soft halo, then frosted glyphs drawn on their own layer
    // so the cooler lower edge tints only the glyphs.
    const narrow = W < 640;
    const size = narrow ? Math.min(H * 0.5, W * 0.46) : Math.min(H * 0.74, W * 0.42);
    const font = `500 ${size}px "Google Sans", "Segoe UI", sans-serif`;
    g.font = font;
    const text = '2.5';
    const w = g.measureText(text).width;
    const x = W * 0.95 - w + shift * 14, y = (narrow ? H * 0.36 : H * 0.5) + size * 0.36 + shift * 6;
    g.save();
    g.filter = 'blur(30px)';
    g.fillStyle = 'rgba(210,235,255,.5)';
    g.fillText(text, x, y);
    g.restore();
    const layer = document.createElement('canvas');
    layer.width = canvas.width;
    layer.height = canvas.height;
    const l = layer.getContext('2d');
    l.setTransform(dpr, 0, 0, dpr, 0, 0);
    l.font = font;
    const face = l.createLinearGradient(x, y - size * 0.8, x + w, y);
    face.addColorStop(0, 'rgba(255,255,255,.98)');
    face.addColorStop(0.55, 'rgba(236,246,255,.92)');
    face.addColorStop(1, 'rgba(196,226,255,.6)');
    l.fillStyle = face;
    l.fillText(text, x, y);
    l.globalCompositeOperation = 'source-atop';
    const edge = l.createLinearGradient(0, y - size * 0.75, 0, y);
    edge.addColorStop(0, 'rgba(255,255,255,0)');
    edge.addColorStop(1, 'rgba(40,110,230,.38)');
    l.fillStyle = edge;
    l.fillRect(0, 0, W, H);
    g.save();
    g.setTransform(1, 0, 0, 1, 0, 0);
    g.filter = 'blur(2px)';
    g.drawImage(layer, 0, 0);
    g.restore();
  }

  function mountHero() {
    const canvas = $('.hero-card canvas');
    if (!canvas) return;
    let shift = 0, target = 0, raf = 0;
    const paint = () => paintHero(canvas, shift);
    const loop = () => {
      shift += (target - shift) * 0.08;
      paint();
      raf = Math.abs(target - shift) > 0.002 ? requestAnimationFrame(loop) : 0;
    };
    paint();
    (document.fonts ? document.fonts.ready : Promise.resolve()).then(paint);
    let t;
    addEventListener('resize', () => { clearTimeout(t); t = setTimeout(() => { canvas.width = 0; paint(); }, 120); });
    if (!reduce) {
      canvas.parentElement.addEventListener('pointermove', (e) => {
        const b = canvas.getBoundingClientRect();
        target = ((e.clientX - b.left) / b.width - 0.5) * 2;
        if (!raf) raf = requestAnimationFrame(loop);
      });
      canvas.parentElement.addEventListener('pointerleave', () => { target = 0; if (!raf) raf = requestAnimationFrame(loop); });
    }
  }

  /* ------------------------------------------------------------ charts */

  function frame(svg, height) {
    const W = Math.max(300, svg.parentNode.clientWidth);
    svg.setAttribute('viewBox', `0 0 ${W} ${height}`);
    svg.setAttribute('height', height);
    svg.textContent = '';
    return W;
  }

  function axes(svg, m, W, H, max, ticks, spec) {
    const y = (v) => H - m.b - (v / max) * (H - m.t - m.b);
    for (const t of ticks) {
      el('line', { x1: m.l, x2: W - m.r, y1: y(t), y2: y(t), stroke: '#e8eaed' }, svg);
      const label = el('text', { x: m.l - 8, y: y(t) + 4, 'text-anchor': 'end', 'font-size': 11, fill: '#5f6368' }, svg);
      label.textContent = spec.tickFmt === 'pct' ? `${t}%` : t.toLocaleString('en-US');
    }
    el('line', { x1: m.l, x2: m.l, y1: m.t - 6, y2: H - m.b, stroke: '#444746' }, svg);
    el('line', { x1: m.l, x2: W - m.r, y1: H - m.b, y2: H - m.b, stroke: '#444746' }, svg);
    if (spec.yLabel) {
      const yl = el('text', { transform: `translate(14,${(m.t + H - m.b) / 2}) rotate(-90)`, 'text-anchor': 'middle', 'font-size': 12, fill: '#1f1f1f', 'font-weight': 500 }, svg);
      yl.textContent = spec.yLabel;
    }
    return y;
  }

  function wrapLabel(svg, x, y, text, size = 12, weight = 500) {
    const t = el('text', { x, y, 'text-anchor': 'middle', 'font-size': size, fill: '#1f1f1f', 'font-weight': weight }, svg);
    String(text).split('\n').forEach((line, i) => {
      const span = el('tspan', { x, dy: i ? size * 1.2 : 0 }, t);
      span.textContent = line;
    });
  }

  function barChart(svg, spec) {
    const H = spec.height || 340;
    const W = frame(svg, H);
    const m = { t: 34, r: 8, b: spec.bottom || 44, l: 54 };
    const y = axes(svg, m, W, H, spec.max, spec.ticks, spec);
    const n = spec.bars.length;
    const slot = (W - m.l - m.r) / n;
    const bw = Math.min(slot * 0.72, 120);
    spec.bars.forEach((bar, i) => {
      const x = m.l + slot * i + (slot - bw) / 2;
      const top = y(bar.value);
      const h = H - m.b - top;
      const rect = el('rect', { x, y: H - m.b, width: bw, height: 0, rx: 3, fill: bar.color, stroke: bar.stroke || '#3c4043', 'stroke-width': 0.8 }, svg);
      if (bar.star) {
        const s = el('text', { x: x + bw / 2, y: top - 26, 'text-anchor': 'middle', 'font-size': 18, fill: '#1f1f1f' }, svg);
        s.textContent = '✦';
      }
      const value = el('text', { x: x + bw / 2, y: top - 8, 'text-anchor': 'middle', 'font-size': 12, fill: '#3c4043' }, svg);
      value.textContent = bar.display ?? fmt(bar.value, spec);
      wrapLabel(svg, x + bw / 2, H - m.b + 18, bar.label, 12);
      const title = el('title', {}, rect);
      title.textContent = `${bar.label.replace('\n', ' ')}: ${value.textContent}`;
      if (reduce) { rect.setAttribute('y', top); rect.setAttribute('height', h); return; }
      rect.style.transition = `y .7s ${i * 0.06}s cubic-bezier(.2,0,0,1), height .7s ${i * 0.06}s cubic-bezier(.2,0,0,1)`;
      requestAnimationFrame(() => requestAnimationFrame(() => { rect.setAttribute('y', top); rect.setAttribute('height', h); }));
    });
  }

  function stackedChart(svg, spec) {
    const H = spec.height || 340;
    const W = frame(svg, H);
    const m = { t: 34, r: 8, b: spec.bottom || 44, l: 54 };
    const y = axes(svg, m, W, H, spec.max, spec.ticks, spec);
    const n = spec.columns.length;
    const slot = (W - m.l - m.r) / n;
    const bw = Math.min(slot * 0.62, 90);
    spec.columns.forEach((col, i) => {
      const x = m.l + slot * i + (slot - bw) / 2;
      let acc = 0;
      spec.keys.forEach((key) => {
        const v = col.values[key.key] || 0;
        if (!v) return;
        const y0 = y(acc), y1 = y(acc + v);
        const rect = el('rect', { x, y: y1, width: bw, height: Math.max(0, y0 - y1), fill: key.color, stroke: '#3c4043', 'stroke-width': 0.6 }, svg);
        const t = el('title', {}, rect);
        t.textContent = `${col.label.replace('\n', ' ')} · ${key.label}: ${v.toLocaleString('en-US')}`;
        acc += v;
      });
      const total = el('text', { x: x + bw / 2, y: y(acc) - 8, 'text-anchor': 'middle', 'font-size': 12, fill: '#3c4043' }, svg);
      total.textContent = col.display ?? fmt(acc, spec);
      if (col.star) {
        const s = el('text', { x: x + bw / 2, y: y(acc) - 26, 'text-anchor': 'middle', 'font-size': 18 }, svg);
        s.textContent = '✦';
      }
      wrapLabel(svg, x + bw / 2, H - m.b + 18, col.label, 11.5);
    });
  }

  function groupedChart(svg, spec) {
    const H = spec.height || 360;
    const W = frame(svg, H);
    const m = { t: 34, r: 8, b: spec.bottom || 44, l: 58 };
    const y = axes(svg, m, W, H, spec.max, spec.ticks, spec);
    const n = spec.groups.length, k = spec.series.length;
    const slot = (W - m.l - m.r) / n;
    const gw = slot * 0.8, bw = gw / k;
    spec.groups.forEach((group, gi) => {
      const gx = m.l + slot * gi + (slot - gw) / 2;
      spec.series.forEach((series, si) => {
        const v = group.values[si];
        const x = gx + bw * si + 1.5;
        const top = y(v);
        const rect = el('rect', { x, y: top, width: bw - 3, height: H - m.b - top, rx: 2, fill: series.color, stroke: '#3c4043', 'stroke-width': 0.6 }, svg);
        const t = el('title', {}, rect);
        t.textContent = `${group.label.replace('\n', ' ')} · ${series.name}: ${fmt(v, spec)}`;
        if (spec.labels !== false) {
          const lab = el('text', { x: x + (bw - 3) / 2, y: top - 6, 'text-anchor': 'middle', 'font-size': 10.5, fill: '#3c4043' }, svg);
          lab.textContent = fmt(v, spec);
        }
      });
      wrapLabel(svg, gx + gw / 2, H - m.b + 18, group.label, 12);
    });
  }

  const RENDER = { bar: barChart, stacked: stackedChart, grouped: groupedChart };
  function render(svg) {
    const spec = DATA.charts[svg.dataset.chart];
    RENDER[spec.type](svg, spec);
  }

  function mountCharts() {
    const drawn = new WeakSet();
    const io = new IntersectionObserver((es) => es.forEach((e) => {
      if (e.isIntersecting && !drawn.has(e.target)) { drawn.add(e.target); render(e.target); }
    }), { rootMargin: '0px 0px -10% 0px' });
    $$('svg[data-chart]').forEach((svg) => io.observe(svg));
    let w = innerWidth, t;
    addEventListener('resize', () => {
      clearTimeout(t);
      t = setTimeout(() => {
        if (Math.abs(innerWidth - w) < 8) return;
        w = innerWidth;
        $$('svg[data-chart]').forEach((svg) => drawn.has(svg) && render(svg));
      }, 150);
    });
  }

  /* ------------------------------------------------------------ carousels */

  function mountCarousels() {
    $$('[data-carousel]').forEach((root) => {
      const track = $('.track', root);
      const slides = $$('.slide', root);
      const prev = $('[data-prev]', root), next = $('[data-next]', root);
      const bar = $('.progress span', root);
      let at = 0;
      const show = (i) => {
        at = Math.max(0, Math.min(slides.length - 1, i));
        track.style.transform = `translateX(${-100 * at}%)`;
        prev.disabled = at === 0;
        next.disabled = at === slides.length - 1;
        bar.style.width = `${100 / slides.length}%`;
        bar.style.left = `${(100 / slides.length) * at}%`;
        slides.forEach((s, j) => s.setAttribute('aria-hidden', String(j !== at)));
        const svg = $('svg[data-chart]', slides[at]);
        if (svg) render(svg);
      };
      prev.addEventListener('click', () => show(at - 1));
      next.addEventListener('click', () => show(at + 1));
      root.addEventListener('keydown', (e) => {
        if (e.key === 'ArrowLeft') show(at - 1);
        if (e.key === 'ArrowRight') show(at + 1);
      });
      let x0 = null;
      root.addEventListener('pointerdown', (e) => { x0 = e.clientX; });
      root.addEventListener('pointerup', (e) => {
        if (x0 !== null && Math.abs(e.clientX - x0) > 50) show(at + (e.clientX < x0 ? 1 : -1));
        x0 = null;
      });
      show(0);
    });
  }

  /* ------------------------------------------------------------ page chrome */

  function accordion(root) {
    const button = $(':scope > button', root);
    button.addEventListener('click', () => {
      const open = root.dataset.open !== 'true';
      root.dataset.open = String(open);
      button.setAttribute('aria-expanded', String(open));
    });
  }

  function mountChrome() {
    const top = $('.top');
    const onScroll = () => top.classList.toggle('is-scrolled', scrollY > 4);
    addEventListener('scroll', onScroll, { passive: true });
    onScroll();
    $$('[data-accordion]').forEach(accordion);

    const toc = $('.toc');
    if (toc) {
      $('.toc-toggle', toc).addEventListener('click', () => {
        toc.dataset.open = String(toc.dataset.open !== 'true');
      });
      const links = $$('a', toc);
      const byId = new Map(links.map((a) => [a.hash.slice(1), a]));
      const spy = new IntersectionObserver((es) => es.forEach((e) => {
        if (!e.isIntersecting) return;
        links.forEach((a) => a.classList.remove('is-on'));
        byId.get(e.target.id)?.classList.add('is-on');
      }), { rootMargin: '-20% 0px -70% 0px' });
      byId.forEach((_, id) => { const h = document.getElementById(id); if (h) spy.observe(h); });
    }

    const live = $('#live');
    $('.share')?.addEventListener('click', async () => {
      const data = { title: document.title, url: location.href.split('#')[0] };
      try {
        if (navigator.share) await navigator.share(data);
        else { await navigator.clipboard.writeText(data.url); live.textContent = 'Link copied'; }
      } catch { /* dismissed */ }
    });
    $$('[data-copy]').forEach((btn) => btn.addEventListener('click', async () => {
      const text = document.getElementById(btn.dataset.copy).innerText.trim();
      try { await navigator.clipboard.writeText(text); btn.textContent = 'Copied'; live.textContent = 'Copied'; }
      catch { btn.textContent = 'Select and copy'; }
      setTimeout(() => (btn.textContent = 'Copy'), 1600);
    }));

    $$('[data-tabs]').forEach((root) => {
      const buttons = $$('.compare-tabs button', root);
      const panels = $$('.compare-panel', root);
      const show = (i) => {
        buttons.forEach((b, j) => { b.setAttribute('aria-selected', String(i === j)); b.tabIndex = i === j ? 0 : -1; });
        panels.forEach((p, j) => (p.hidden = i !== j));
      };
      buttons.forEach((b, i) => {
        b.addEventListener('click', () => show(i));
        b.addEventListener('keydown', (e) => {
          const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
          if (d) { const j = (i + d + buttons.length) % buttons.length; show(j); buttons[j].focus(); }
        });
      });
      show(0);
    });

    const io = new IntersectionObserver((es) => es.forEach((e) => {
      if (e.isIntersecting) { e.target.classList.add('is-in'); io.unobserve(e.target); }
    }), { rootMargin: '0px 0px -8% 0px' });
    $$('[data-in]').forEach((n) => io.observe(n));
  }

  mountChrome();
  mountHero();
  mountCharts();
  mountCarousels();
})();
