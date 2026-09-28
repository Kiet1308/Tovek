/* Tovek V2.1 release notes: hero collage, charts, tabbed figures. No dependencies. */
(() => {
  'use strict';
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const DATA = JSON.parse($('#v21-data').textContent);

  /* ------------------------------------------------------------ seeded noise */

  function rng(seed) {
    let s = seed >>> 0;
    return () => {
      s = (s + 0x6d2b79f5) >>> 0;
      let t = s;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  // One small grain tile, reused by every painted surface.
  const grainTile = (() => {
    const c = document.createElement('canvas');
    c.width = c.height = 160;
    const g = c.getContext('2d');
    const img = g.createImageData(160, 160);
    const r = rng(7);
    for (let i = 0; i < img.data.length; i += 4) {
      const v = r() * 255;
      img.data[i] = img.data[i + 1] = img.data[i + 2] = v;
      img.data[i + 3] = 255;
    }
    g.putImageData(img, 0, 0);
    return c;
  })();

  function grain(g, x, y, w, h, alpha, mode = 'overlay') {
    g.save();
    g.globalAlpha = alpha;
    g.globalCompositeOperation = mode;
    if (mode === 'overlay' && g.__atop) g.globalCompositeOperation = 'source-atop';
    g.fillStyle = g.createPattern(grainTile, 'repeat');
    g.fillRect(x, y, w, h);
    g.restore();
  }

  // A vertical edge that looks cut from paper rather than ruled.
  function tornPath(g, x, y0, y1, amp, seed, side) {
    const r = rng(seed);
    const pts = [];
    for (let y = y0; y <= y1; y += 6) pts.push([x + (r() - 0.5) * amp, y]);
    return side < 0 ? pts : pts.reverse();
  }

  /* ------------------------------------------------------------ hero collage */

  const HEX = '0123456789ABCDEF';
  function paintHero(canvas) {
    const dpr = Math.min(devicePixelRatio || 1, 2);
    const W = canvas.clientWidth, H = canvas.clientHeight;
    if (!W || !H) return;
    canvas.width = Math.round(W * dpr);
    canvas.height = Math.round(H * dpr);
    const g = canvas.getContext('2d');
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const r = rng(20260928);
    const left = Math.max(22, Math.min(86, W * 0.059));
    const right = Math.max(22, Math.min(118, W * 0.085));
    const cx0 = left, cx1 = W - right;
    const cw = cx1 - cx0;
    const horizon = H * 0.56;

    g.fillStyle = '#0e0d0c';
    g.fillRect(0, 0, W, H);

    /* left strip: ochre paper with an ink control-flow sketch, sand below */
    g.save();
    g.fillStyle = '#e8b12e';
    g.fillRect(0, 0, left + 4, H);
    const sandTop = H * 0.62;
    const sand = g.createLinearGradient(0, sandTop, 0, H);
    sand.addColorStop(0, '#cfc6b4'); sand.addColorStop(1, '#8d8373');
    g.fillStyle = sand;
    g.beginPath();
    g.moveTo(0, sandTop);
    for (let x = 0; x <= left + 4; x += 4) g.lineTo(x, sandTop + Math.sin(x * 0.3) * 4 + r() * 5);
    g.lineTo(left + 4, H); g.lineTo(0, H); g.closePath(); g.fill();
    for (let i = 0; i < 260; i++) {
      g.fillStyle = `rgba(40,32,24,${0.1 + r() * 0.25})`;
      g.fillRect(r() * left, sandTop + r() * (H - sandTop), 1 + r() * 2, 1 + r() * 1.5);
    }
    g.strokeStyle = 'rgba(52,34,18,.82)';
    g.lineCap = 'round';
    g.lineWidth = 2.2;
    const sx = left * 0.62;
    g.beginPath();
    g.moveTo(sx, H * 0.14);
    for (let y = H * 0.14; y < sandTop - 30; y += 12) g.lineTo(sx + Math.sin(y * 0.05) * 3 + (r() - 0.5) * 2, y);
    g.stroke();
    for (let k = 0; k < 4; k++) {
      const ty = H * (0.19 + k * 0.1) + r() * 12;
      g.lineWidth = 1.8;
      g.beginPath();
      g.moveTo(sx - left * 0.45, ty + (r() - 0.5) * 3);
      g.quadraticCurveTo(sx, ty + (r() - 0.5) * 6, sx + left * 0.35, ty + (r() - 0.5) * 4);
      g.stroke();
    }
    g.restore();
    grain(g, 0, 0, left + 4, H, 0.22);

    /* right strip: night above, terracotta rock below */
    g.save();
    g.fillStyle = '#151210';
    g.fillRect(cx1 - 4, 0, right + 4, H);
    const rockTop = H * 0.5;
    const rock = g.createLinearGradient(cx1, rockTop, W, H);
    rock.addColorStop(0, '#c7602d'); rock.addColorStop(.5, '#a9461f'); rock.addColorStop(1, '#6d2a14');
    g.fillStyle = rock;
    g.beginPath();
    g.moveTo(cx1 - 4, rockTop + 30);
    for (let x = cx1 - 4; x <= W; x += 5) g.lineTo(x, rockTop + 18 * Math.sin((x - cx1) * 0.08) + r() * 16);
    g.lineTo(W, H); g.lineTo(cx1 - 4, H); g.closePath(); g.fill();
    for (let i = 0; i < 26; i++) {
      const x = cx1 + r() * right;
      g.strokeStyle = `rgba(60,20,8,${0.35 + r() * 0.4})`;
      g.lineWidth = 1 + r() * 3;
      g.beginPath();
      g.moveTo(x, rockTop + 20 + r() * 40);
      for (let y = rockTop + 40; y < H; y += 18) g.lineTo(x + (r() - 0.5) * 10, y);
      g.stroke();
    }
    for (let i = 0; i < 400; i++) {
      g.fillStyle = r() < 0.5 ? `rgba(255,190,140,${r() * 0.25})` : `rgba(40,12,4,${r() * 0.35})`;
      g.fillRect(cx1 + r() * right, rockTop + r() * (H - rockTop), 1 + r() * 2.5, 1 + r() * 2.5);
    }
    g.restore();
    grain(g, cx1 - 4, 0, right + 4, H, 0.2);

    /* centre: sunrise over a planet of bytecode */
    g.save();
    const edgeL = tornPath(g, cx0, -6, H + 6, 3, 11, -1);
    const edgeR = tornPath(g, cx1, -6, H + 6, 3, 12, 1);
    g.beginPath();
    edgeL.forEach(([x, y], i) => (i ? g.lineTo(x, y) : g.moveTo(x, y)));
    edgeR.forEach(([x, y]) => g.lineTo(x, y));
    g.closePath();
    g.clip();

    const sky = g.createLinearGradient(0, 0, 0, horizon);
    sky.addColorStop(0, '#0a0f1c');
    sky.addColorStop(0.24, '#15294a');
    sky.addColorStop(0.5, '#2f5d93');
    sky.addColorStop(0.72, '#6f9acb');
    sky.addColorStop(0.86, '#b8b2b4');
    sky.addColorStop(0.93, '#eea26b');
    sky.addColorStop(1, '#ff6b1e');
    g.fillStyle = sky;
    g.fillRect(cx0, 0, cw, horizon + 2);

    for (let i = 0; i < 90; i++) {
      const y = r() * horizon * 0.45;
      g.fillStyle = `rgba(255,255,255,${(1 - y / (horizon * 0.45)) * (0.2 + r() * 0.6)})`;
      g.fillRect(cx0 + r() * cw, y, r() < 0.9 ? 1 : 1.6, r() < 0.9 ? 1 : 1.6);
    }

    const glow = g.createRadialGradient(W / 2, horizon, 0, W / 2, horizon, cw * 0.62);
    glow.addColorStop(0, 'rgba(255,120,40,.55)');
    glow.addColorStop(0.35, 'rgba(255,110,40,.18)');
    glow.addColorStop(1, 'rgba(255,110,40,0)');
    g.fillStyle = glow;
    g.fillRect(cx0, horizon - cw * 0.4, cw, cw * 0.6);

    // Planet limb: a very large circle whose top edge is the horizon.
    const R = cw * 3.2;
    const pcx = W / 2, pcy = horizon + R;
    const ground = g.createLinearGradient(0, horizon, 0, H);
    ground.addColorStop(0, '#2a1308');
    ground.addColorStop(0.08, '#160c07');
    ground.addColorStop(0.5, '#0f0a07');
    ground.addColorStop(1, '#0c0a08');
    g.fillStyle = ground;
    g.beginPath();
    g.arc(pcx, pcy, R, 0, Math.PI * 2);
    g.fill();

    g.save();
    g.shadowColor = 'rgba(255,120,40,.95)';
    g.shadowBlur = 22;
    g.strokeStyle = 'rgba(255,170,90,.95)';
    g.lineWidth = 1.6;
    g.beginPath();
    g.arc(pcx, pcy, R, Math.PI * 1.25, Math.PI * 1.75);
    g.stroke();
    g.restore();

    // The surface is bytecode: rows of hex that recede toward the horizon.
    g.save();
    g.beginPath();
    g.arc(pcx, pcy, R - 1, 0, Math.PI * 2);
    g.clip();
    g.textBaseline = 'alphabetic';
    let row = 0;
    for (let t = 0.02; t < 1; t += 0.035 + t * 0.05, row++) {
      const y = horizon + Math.pow(t, 1.6) * (H - horizon) * 1.05 + 3;
      const size = 3 + t * t * 22;
      const alpha = Math.min(0.3, 0.04 + (1 - t) * 0.3) * (t < 0.1 ? t / 0.1 : 1) * (t < 0.3 ? 0.55 : 1);
      g.font = `500 ${size}px ui-monospace, Menlo, Consolas, monospace`;
      const step = size * 2.9;
      const spread = 1 + t * 1.6;
      const x0 = W / 2 - (cw * spread) / 2 + ((row * 37) % step);
      for (let x = x0; x < W / 2 + (cw * spread) / 2; x += step) {
        const warm = 1 - t;
        g.fillStyle = `rgba(${255},${Math.round(120 + warm * 70)},${Math.round(60 + warm * 40)},${alpha * (0.5 + r() * 0.5)})`;
        g.fillText(HEX[(r() * 16) | 0] + HEX[(r() * 16) | 0], x, y);
      }
    }
    g.restore();

    const haze = g.createLinearGradient(0, horizon - 40, 0, horizon + 60);
    haze.addColorStop(0, 'rgba(255,140,70,0)');
    haze.addColorStop(0.45, 'rgba(255,140,70,.18)');
    haze.addColorStop(1, 'rgba(255,140,70,0)');
    g.fillStyle = haze;
    g.fillRect(cx0, horizon - 40, cw, 100);

    const fade = g.createLinearGradient(0, H * 0.78, 0, H);
    fade.addColorStop(0, 'rgba(14,13,12,0)');
    fade.addColorStop(1, 'rgba(14,13,12,1)');
    g.fillStyle = fade;
    g.fillRect(cx0, H * 0.78, cw, H * 0.22);
    grain(g, cx0, 0, cw, H, 0.16);
    g.restore();

    // Paper edges catch a little light.
    g.strokeStyle = 'rgba(255,255,255,.18)';
    g.lineWidth = 1;
    g.beginPath(); edgeL.forEach(([x, y], i) => (i ? g.lineTo(x, y) : g.moveTo(x, y))); g.stroke();
    g.beginPath(); edgeR.forEach(([x, y], i) => (i ? g.lineTo(x, y) : g.moveTo(x, y))); g.stroke();
  }

  /* ------------------------------------------------------------ closing weave */

  function paintWeave(canvas) {
    const dpr = Math.min(devicePixelRatio || 1, 2);
    const W = canvas.clientWidth, H = canvas.clientHeight;
    if (!W || !H) return;
    canvas.width = Math.round(W * dpr);
    canvas.height = Math.round(H * dpr);
    const g = canvas.getContext('2d');
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, W, H);
    const r = rng(1308);
    const cell = W < 700 ? 5 : 7;
    // A torn top edge that dips toward the middle, like cloth pulled at the corners.
    const edge = [];
    for (let x = 0; x <= W + cell; x += cell) {
      const u = x / W;
      edge.push(H * (0.16 - 0.1 * Math.sin(u * Math.PI)) + r() * 6 + (r() < 0.1 ? r() * 12 : 0));
    }
    // Motif: flowing leaf shapes from a warped wave field, woven on a madder-red ground.
    const field = (x, y) => {
      const a = Math.sin(x * 0.0105 + Math.sin(y * 0.021 + x * 0.002) * 1.7);
      const b = Math.cos(y * 0.019 + Math.sin(x * 0.0072) * 2.1);
      return a * b + 0.3 * Math.sin((x + y * 1.3) * 0.016);
    };
    // Backing thread colour shows through the gaps between stitches.
    g.fillStyle = '#2a0e09';
    g.beginPath();
    g.moveTo(0, H);
    edge.forEach((y, i) => g.lineTo(i * cell, Math.ceil(y / cell) * cell));
    g.lineTo(W, H);
    g.closePath();
    g.fill();
    const tone = (m) => (m > 0.62 ? [236, 142, 78] : m > 0.42 ? [214, 106, 52] : m > 0.33 ? [70, 26, 18] : [168, 48, 32]);
    for (let cxi = 0, x = 0; x < W; x += cell, cxi++) {
      const top = edge[cxi];
      for (let y = Math.ceil(top / cell) * cell; y < H; y += cell) {
        const m = field(x, y);
        const col = tone(m);
        const warp = ((cxi + (y / cell | 0)) & 1) === 0;
        const k = (warp ? 1.05 : 0.88) * (0.92 + r() * 0.14);
        g.fillStyle = `rgb(${col[0] * k | 0},${col[1] * k | 0},${col[2] * k | 0})`;
        g.fillRect(x, y, cell - 0.7, cell - 0.7);
        g.fillStyle = warp ? 'rgba(255,220,190,.10)' : 'rgba(0,0,0,.12)';
        g.fillRect(x, y, cell - 0.7, (cell - 0.7) / 2);
        if (m <= 0.33 && r() < 0.035) {
          g.fillStyle = 'rgba(58,72,120,.7)';
          g.fillRect(x, y, cell - 0.7, cell - 0.7);
        }
      }
      if (r() < 0.6) {
        g.strokeStyle = `rgba(${160 + r() * 60 | 0},${56 + r() * 30 | 0},38,.85)`;
        g.lineWidth = 1;
        g.beginPath();
        g.moveTo(x + r() * cell, top + 1);
        g.quadraticCurveTo(x + (r() - 0.5) * 5, top - 5, x + (r() - 0.5) * 8, top - 4 - r() * 10);
        g.stroke();
      }
    }
    g.__atop = true;
    grain(g, 0, 0, W, H, 0.22);
    g.__atop = false;
    g.save();
    g.globalCompositeOperation = 'source-atop';
    const fade = g.createLinearGradient(0, H * 0.3, 0, H);
    fade.addColorStop(0, 'rgba(20,20,19,0)');
    fade.addColorStop(1, 'rgba(20,20,19,1)');
    g.fillStyle = fade;
    g.fillRect(0, 0, W, H);
    g.restore();
  }

  /* ------------------------------------------------------------ charts */

  const NS = 'http://www.w3.org/2000/svg';
  function el(name, attrs = {}, parent) {
    const node = document.createElementNS(NS, name);
    for (const k in attrs) node.setAttribute(k, attrs[k]);
    if (parent) parent.appendChild(node);
    return node;
  }
  const scale = (kind, d0, d1, r0, r1) => {
    const f = kind === 'log' ? Math.log : (v) => v;
    const a = f(d0), b = f(d1);
    return (v) => r0 + (f(v) - a) / (b - a) * (r1 - r0);
  };
  const fmt = (v) => (Math.abs(v) >= 1000 ? v.toLocaleString('en-US') : String(v));

  function drawChart(svg, spec) {
    const W = Math.max(300, svg.parentNode.clientWidth);
    const narrow = W < 560;
    const H = Math.round(Math.min(560, Math.max(320, W * (narrow ? 0.9 : 0.6))));
    svg.setAttribute('viewBox', `0 0 ${W} ${H}`);
    svg.setAttribute('height', H);
    svg.textContent = '';
    const m = { t: 30, r: narrow ? 14 : 34, b: 64, l: narrow ? 64 : 88 };
    const x = scale(spec.x.scale, spec.x.min, spec.x.max, m.l, W - m.r);
    const y = scale(spec.y.scale, spec.y.min, spec.y.max, H - m.b, m.t);
    const gGrid = el('g', {}, svg);
    for (const t of spec.y.ticks) {
      el('line', { x1: m.l, x2: W - m.r, y1: y(t), y2: y(t), stroke: '#e5e4dd', 'stroke-width': 1 }, gGrid);
      const label = el('text', { x: m.l - 12, y: y(t) + 4.5, 'text-anchor': 'end', 'font-size': narrow ? 11.5 : 14 }, gGrid);
      label.textContent = spec.y.fmt ? spec.y.fmt.replace('{}', fmt(t)) : fmt(t);
    }
    for (const t of spec.x.ticks) {
      const label = el('text', { x: x(t), y: H - m.b + 24, 'text-anchor': 'middle', 'font-size': narrow ? 11.5 : 14 }, gGrid);
      label.textContent = spec.x.fmt ? spec.x.fmt.replace('{}', fmt(t)) : fmt(t);
    }
    el('line', { x1: m.l, x2: m.l, y1: m.t - 10, y2: H - m.b, stroke: '#141413', 'stroke-width': 1.2 }, gGrid);
    el('line', { x1: m.l, x2: W - m.r, y1: H - m.b, y2: H - m.b, stroke: '#141413', 'stroke-width': 1.2 }, gGrid);
    const xt = el('text', { x: (m.l + W - m.r) / 2, y: H - 12, 'text-anchor': 'middle', class: 'axis-title' }, gGrid);
    xt.textContent = spec.x.title;
    const yt = el('text', { transform: `translate(${narrow ? 11 : 26},${(m.t + H - m.b) / 2}) rotate(-90)`, 'text-anchor': 'middle', class: 'axis-title' }, gGrid);
    yt.textContent = spec.y.title;

    const gLines = el('g', {}, svg);
    const gPts = el('g', {}, svg);
    spec.series.forEach((s, si) => {
      const pts = s.points.map((p) => [x(p.x), y(p.y), p]);
      const line = el('polyline', {
        points: pts.map((p) => p.slice(0, 2).join(',')).join(' '),
        fill: 'none', stroke: s.color, 'stroke-width': 3, 'stroke-linejoin': 'round', 'stroke-linecap': 'round',
      }, gLines);
      if (!reduce) {
        const len = line.getTotalLength ? line.getTotalLength() : 0;
        line.style.strokeDasharray = len;
        line.style.strokeDashoffset = len;
        line.getBoundingClientRect();
        line.style.transition = `stroke-dashoffset 1.1s ${0.12 * si}s cubic-bezier(.3,.6,.2,1)`;
        requestAnimationFrame(() => (line.style.strokeDashoffset = 0));
      }
      pts.forEach(([px, py, p], pi) => {
        const c = el('circle', { cx: px, cy: py, r: narrow ? 5.5 : 7.5, fill: s.color, stroke: '#141413', 'stroke-width': 1.3 }, gPts);
        const title = el('title', {}, c);
        title.textContent = `${s.name}: ${p.tip || fmt(p.y)}`;
        if (!reduce) {
          c.style.opacity = 0;
          c.style.transition = `opacity .3s ${0.12 * si + 0.1 * pi}s`;
          requestAnimationFrame(() => (c.style.opacity = 1));
        }
        if (p.label && (!narrow || s.labelNarrow !== false)) {
          const t = el('text', { x: px + (p.dx || 0), y: py + (p.dy || -15), 'text-anchor': 'middle', class: 'pt-label' }, gPts);
          t.textContent = p.label;
        }
      });
    });
  }

  function mountCharts() {
    $$('[data-charts]').forEach((fig) => {
      const buttons = $$('.tabbar button', fig);
      const panels = $$('.chart-panel', fig);
      const render = (panel) => {
        const svg = $('svg', panel);
        if (svg && !panel.hidden) drawChart(svg, DATA.charts[panel.dataset.chart]);
      };
      const select = (i, focus) => {
        buttons.forEach((b, j) => {
          b.setAttribute('aria-selected', String(i === j));
          b.tabIndex = i === j ? 0 : -1;
        });
        panels.forEach((p, j) => (p.hidden = i !== j));
        render(panels[i]);
        if (focus) buttons[i].focus();
      };
      buttons.forEach((b, i) => {
        b.addEventListener('click', () => select(i));
        b.addEventListener('keydown', (e) => {
          const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
          if (d) select((i + d + buttons.length) % buttons.length, true);
        });
      });
      let seen = false;
      const io = new IntersectionObserver((es) => {
        if (es.some((e) => e.isIntersecting) && !seen) { seen = true; select(0); io.disconnect(); }
      }, { rootMargin: '0px 0px -15% 0px' });
      io.observe(fig);
      let w = 0;
      new ResizeObserver(() => {
        if (!seen || Math.abs(fig.clientWidth - w) < 8) return;
        w = fig.clientWidth;
        const panel = panels.find((p) => !p.hidden);
        if (panel) render(panel);
      }).observe(fig);
    });
  }

  /* ------------------------------------------------------------ result cards */

  function mountCards() {
    $$('[data-cards]').forEach((root) => {
      const buttons = $$('.tabbar button', root);
      const cards = $$('.card:not(.card-ghost)', root);
      const ghost = $('.card-ghost', root);
      let at = 0;
      const show = (i, focus) => {
        at = (i + cards.length) % cards.length;
        cards.forEach((c, j) => (c.hidden = j !== at));
        buttons.forEach((b, j) => {
          b.setAttribute('aria-selected', String(j === at));
          b.tabIndex = j === at ? 0 : -1;
        });
        if (ghost) ghost.innerHTML = cards[(at + 1) % cards.length].innerHTML;
        if (!reduce) {
          const c = cards[at];
          c.style.opacity = 0; c.style.transform = 'translateX(18px)';
          requestAnimationFrame(() => requestAnimationFrame(() => { c.style.opacity = 1; c.style.transform = 'none'; }));
        }
        if (focus) buttons[at].focus();
      };
      buttons.forEach((b, i) => {
        b.addEventListener('click', () => show(i));
        b.addEventListener('keydown', (e) => {
          const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
          if (d) show(i + d, true);
        });
      });
      $('[data-prev]', root)?.addEventListener('click', () => show(at - 1));
      $('[data-next]', root)?.addEventListener('click', () => show(at + 1));
      show(0);
    });
  }

  /* ------------------------------------------------------------ side-by-side */

  function mountCompare() {
    $$('[data-compare]').forEach((root) => {
      const buttons = $$('.tabbar button', root);
      const panels = $$('.compare-panel', root);
      let at = 0;
      const play = () => {
        const items = $$('.reveal', panels[at]);
        items.forEach((n) => n.classList.remove('on'));
        items.forEach((n, i) => setTimeout(() => n.classList.add('on'), reduce ? 0 : 90 + i * 140));
      };
      const show = (i, focus) => {
        at = i;
        buttons.forEach((b, j) => {
          b.setAttribute('aria-selected', String(j === i));
          b.tabIndex = j === i ? 0 : -1;
        });
        panels.forEach((p, j) => (p.hidden = j !== i));
        play();
        if (focus) buttons[i].focus();
      };
      buttons.forEach((b, i) => {
        b.addEventListener('click', () => show(i));
        b.addEventListener('keydown', (e) => {
          const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
          if (d) show((i + d + buttons.length) % buttons.length, true);
        });
      });
      $('.replay', root)?.addEventListener('click', play);
      let started = false;
      new IntersectionObserver((es, io) => {
        if (es.some((e) => e.isIntersecting) && !started) { started = true; show(0); io.disconnect(); }
      }, { rootMargin: '0px 0px -20% 0px' }).observe(root);
      panels.forEach((p, j) => (p.hidden = j !== 0));
    });
  }

  /* ------------------------------------------------------------ page chrome */

  function mountChrome() {
    const nav = $('.nav');
    const hero = $('.hero');
    const onScroll = () => nav.classList.toggle('is-solid', hero.getBoundingClientRect().bottom < 68);
    addEventListener('scroll', onScroll, { passive: true });
    onScroll();

    const io = new IntersectionObserver((es) => es.forEach((e) => {
      if (e.isIntersecting) { e.target.classList.add('is-in'); io.unobserve(e.target); }
    }), { rootMargin: '0px 0px -8% 0px' });
    $$('[data-in]').forEach((n) => io.observe(n));

    const live = $('#live');
    $$('[data-copy]').forEach((btn) => btn.addEventListener('click', async () => {
      const text = document.getElementById(btn.dataset.copy).innerText.trim();
      try { await navigator.clipboard.writeText(text); btn.textContent = 'Copied'; live.textContent = 'Copied to clipboard'; }
      catch { btn.textContent = 'Select and copy'; }
      setTimeout(() => (btn.textContent = 'Copy'), 1600);
    }));
  }

  function mountArt() {
    const hero = $('.hero-art');
    const weave = $('.weave');
    let lastW = 0;
    const paint = () => {
      if (Math.abs(innerWidth - lastW) < 2) return;
      lastW = innerWidth;
      paintHero(hero);
      paintWeave(weave);
    };
    paint();
    let t;
    addEventListener('resize', () => { clearTimeout(t); t = setTimeout(paint, 120); });
    if (document.fonts) document.fonts.ready.then(() => { lastW = 0; paint(); });
  }

  mountChrome();
  mountArt();
  mountCharts();
  mountCards();
  mountCompare();
})();
