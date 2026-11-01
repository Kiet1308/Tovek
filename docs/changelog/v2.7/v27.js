/* Tovek V2.7 release notes: hero scene and page behaviour. */
(function () {
  'use strict';
  var reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;

  /* ── header, rail, floating action ───────────────────────── */
  var head = document.querySelector('.head');
  var hero = document.querySelector('.hero');
  var rail = document.querySelector('.rail');
  var railMark = rail && rail.querySelector('i');
  var float = document.querySelector('.float');
  var article = document.querySelector('main');
  // on narrow screens the scatter scrolls; start at the newest release
  var scatter = document.querySelector('.scatter .plot');
  if (scatter) scatter.scrollLeft = scatter.scrollWidth;
  function onScroll() {
    var y = scrollY, h = hero ? hero.offsetHeight : 0;
    head.classList.toggle('is-solid', y > h - 80);
    if (float) float.classList.toggle('on', y > h * 0.7 && y < document.body.scrollHeight - innerHeight - 320);
    if (rail) {
      var top = article.offsetTop + h, len = article.offsetHeight - h - innerHeight;
      var p = Math.min(1, Math.max(0, (y - top + innerHeight * 0.4) / Math.max(1, len)));
      rail.classList.toggle('on', y > h * 0.6);
      railMark.style.transform = 'translateY(' + (p * (rail.offsetHeight - 42)).toFixed(1) + 'px)';
    }
  }
  addEventListener('scroll', onScroll, { passive: true });
  addEventListener('resize', onScroll);
  onScroll();

  /* ── download menu ───────────────────────────────────────── */
  var dl = document.querySelector('.dl');
  if (dl) {
    var btn = dl.querySelector('button');
    btn.addEventListener('click', function (e) {
      e.stopPropagation();
      var open = !dl.classList.contains('open');
      dl.classList.toggle('open', open);
      btn.setAttribute('aria-expanded', open);
    });
    document.addEventListener('click', function () { dl.classList.remove('open'); btn.setAttribute('aria-expanded', 'false'); });
    document.addEventListener('keydown', function (e) { if (e.key === 'Escape') { dl.classList.remove('open'); btn.setAttribute('aria-expanded', 'false'); } });
  }

  /* ── reveals: headings, charts ───────────────────────────── */
  var io = new IntersectionObserver(function (entries) {
    entries.forEach(function (e) {
      if (!e.isIntersecting) return;
      var n = e.target;
      n.classList.add('is-in');
      if (n.classList.contains('chart')) {
        n.querySelectorAll('.bar i').forEach(function (bar) { bar.style.width = bar.dataset.w + '%'; });
      }
      io.unobserve(n);
    });
  }, { rootMargin: '0px 0px -8% 0px' });
  document.querySelectorAll('.blur-in, .chart').forEach(function (n) { io.observe(n); });

  /* ── examples: thumbnails switch the panel ───────────────── */
  document.querySelectorAll('[data-examples]').forEach(function (box) {
    var tabs = box.querySelectorAll('.thumbs button');
    var panes = box.querySelectorAll('.pane');
    var caps = box.querySelectorAll('[data-caption]');
    function show(i) {
      tabs.forEach(function (t, j) { t.setAttribute('aria-selected', j === i); t.tabIndex = j === i ? 0 : -1; });
      panes.forEach(function (p, j) { p.classList.toggle('on', j === i); });
      caps.forEach(function (c, j) { c.hidden = j !== i; });
    }
    tabs.forEach(function (t, i) {
      t.addEventListener('click', function () { show(i); });
      t.addEventListener('keydown', function (e) {
        var d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
        if (!d) return;
        var k = (i + d + tabs.length) % tabs.length;
        show(k); tabs[k].focus(); e.preventDefault();
      });
    });
    show(0);
  });

  /* ── hero ────────────────────────────────────────────────── */
  var scroll = document.querySelector('.scroll');
  if (scroll) scroll.addEventListener('click', function () { document.getElementById('intro').scrollIntoView({ behavior: reduce ? 'auto' : 'smooth' }); });
  var canvas = hero && hero.querySelector('canvas');
  if (!canvas) return;
  var gl = canvas.getContext('webgl', { antialias: false, alpha: false, premultipliedAlpha: false, preserveDrawingBuffer: false });
  if (!gl) { hero.classList.add('is-written'); return; }

  var VERT = 'attribute vec2 p; varying vec2 vUv; void main(){ vUv = p * 0.5 + 0.5; gl_Position = vec4(p, 0.0, 1.0); }';
  var FRAG = [
    'precision highp float;',
    'varying vec2 vUv;',
    'uniform vec2 uRes; uniform float uTime; uniform vec2 uMouse; uniform float uMouseOn;',
    'uniform sampler2D uMark; uniform float uMarkT; uniform float uCloud; uniform float uDrift; uniform float uSeed;',
    'float hash(vec2 p){ p = fract(p * vec2(234.34, 435.345)); p += dot(p, p + 34.23); return fract(p.x * p.y); }',
    'float noise(vec2 p){ vec2 i = floor(p), f = fract(p); vec2 u = f * f * (3.0 - 2.0 * f);',
    '  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x), mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y); }',
    'const mat2 R = mat2(1.6, 1.2, -1.2, 1.6);',
    'float fbm(vec2 p){ float v = 0.0, a = 0.5; for (int i = 0; i < 5; i++){ v += a * noise(p); p = R * p; a *= 0.5; } return v; }',
    'float fbm3(vec2 p){ float v = 0.0, a = 0.5; for (int i = 0; i < 3; i++){ v += a * noise(p); p = R * p; a *= 0.5; } return v; }',
    // a ridge line: sharp crests from ridged noise, broad peaks first
    'float ridge(float x, float k){ float s = uSeed + k * 11.7; float h = 0.0, a = 0.62, f = 0.62 + k * 0.2;',
    '  for (int i = 0; i < 5; i++){ float n = 1.0 - abs(noise(vec2(x * f + s, s * 1.3 + float(i) * 7.1)) * 2.0 - 1.0); h += a * n * n; f *= 2.13; a *= 0.44; }',
    '  return h; }',
    'float ridgeLow(float x, float k){ float s = uSeed + k * 11.7; float h = 0.0, a = 0.62, f = 0.62 + k * 0.2;',
    '  for (int i = 0; i < 2; i++){ float d = noise(vec2(x * f + s, s * 1.3 + float(i) * 7.1)) * 2.0 - 1.0; float n = 1.0 - sqrt(d * d + 0.03); h += a * n * n; f *= 2.13; a *= 0.44; }',
    '  return h + 0.12; }',
    'vec3 sky(vec2 p){ vec3 top = vec3(0.93, 0.915, 0.885), mid = vec3(0.835, 0.865, 0.89), low = vec3(0.90, 0.915, 0.925);',
    '  vec3 c = mix(low, mid, smoothstep(0.25, 0.7, p.y)); c = mix(c, top, smoothstep(0.74, 1.0, p.y));',
    '  float streak = fbm(vec2(p.x * 1.1 + uTime * 0.004 * uDrift, p.y * 7.0)) * smoothstep(0.66, 0.98, p.y);',
    '  return mix(c, vec3(0.975, 0.97, 0.955), streak * 0.5); }',
    // ink-wash ridge: darkest at the crest, washing into the mist below, a lit edge on the side facing the light
    'vec3 mountain(vec2 p, float k, float base, float amp, out float inside){',
    '  float h = pow(ridge(p.x, k), 1.35) * 1.2; float y = base + amp * h; float aa = 1.5 / uRes.y;',
    '  inside = smoothstep(y + aa, y - aa, p.y);',
    '  float e = 0.003; float slope = (pow(ridge(p.x + e, k), 1.35) - pow(ridge(p.x - e, k), 1.35)) * 1.2 * amp / (2.0 * e);',
    '  float below = max(base + amp * pow(ridgeLow(p.x, k), 1.35) * 1.2 - p.y, 0.0);',
    '  float len = 0.07 + amp * 0.5 * (0.75 + 0.5 * fbm3(vec2(p.x * 1.4 + k * 2.0, p.y * 1.4)));',
    '  float wash = smoothstep(0.0, len, below);',
    '  float rock = fbm(p * 13.0 + k * 4.0);',
    '  vec3 ink = vec3(0.17, 0.21, 0.26) * (0.86 + 0.28 * rock);',
    '  float edge = smoothstep(0.022, 0.0, max(y - p.y, 0.0)) * clamp(-slope * 0.5, 0.0, 1.0);',
    '  vec3 c = mix(ink, vec3(0.85, 0.88, 0.91), edge * 0.6);',
    '  return mix(c, vec3(0.80, 0.835, 0.865), wash); }',
    'float cloudBand(vec2 p, float y0, float thick, float scale, float speed){',
    '  vec2 q = vec2(p.x * scale + uTime * speed * uDrift, (p.y - y0) * scale * 2.6);',
    '  float n = fbm(q + vec2(fbm3(q * 0.7 + uTime * 0.01 * uDrift), 0.0) * 1.2);',
    '  float body = smoothstep(y0 + thick, y0 - thick * 0.6, p.y);',
    '  return clamp((n - 0.42) * 2.6, 0.0, 1.0) * body; }',
    'void main(){',
    '  float aspect = uRes.x / uRes.y; vec2 p = vec2((vUv.x - 0.5) * aspect + 0.5, vUv.y);',
    '  vec3 col = sky(p);',
    '  float part = 1.0;',
    '  if (uMouseOn > 0.5){ vec2 m = vec2((uMouse.x - 0.5) * aspect + 0.5, uMouse.y); part = mix(0.35, 1.0, smoothstep(0.02, 0.26, length(p - m))); }',
    '  float inside;',
    // far to near: four ridges, each followed by the mist that sits below it
    '  for (int i = 0; i < 4; i++){',
    '    float k = float(i);',
    '    float base = 0.54 - k * 0.105; float amp = 0.2 + k * 0.07;',
    '    vec3 m = mountain(p, k, base, amp, inside);',
    '    m = mix(m, vec3(0.82, 0.855, 0.885), 0.55 - k * 0.15);',
    '    col = mix(col, m, inside);',
    '    float c = cloudBand(p, base + 0.02, 0.09 + k * 0.03, 2.0 + k * 0.5, 0.006 + k * 0.004) * uCloud * part;',
    '    vec3 cc = mix(vec3(0.80, 0.835, 0.865), vec3(0.975, 0.98, 0.985), smoothstep(base - 0.1, base + 0.08, p.y));',
    '    col = mix(col, cc, c * (0.6 + k * 0.08));',
    '  }',
    // the sea of clouds the ridges rise from
    '  vec2 q = vec2(p.x * 1.4 + uTime * 0.012 * uDrift, p.y * 3.6);',
    '  float d = fbm(q + fbm3(q * 1.3 - uTime * 0.008 * uDrift) * 1.1);',
    '  float sea = smoothstep(0.34, 0.02, p.y + (d - 0.5) * 0.22) * uCloud;',
    '  float g = fbm(q + vec2(0.0, 0.06)) - d;',
    '  vec3 seaCol = mix(vec3(0.83, 0.86, 0.89), vec3(0.98, 0.985, 0.99), clamp(0.6 + g * 3.0, 0.0, 1.0));',
    '  col = mix(col, seaCol, sea * mix(0.55, 1.0, part));',
    // the mark, condensing from the mist
    '  vec2 mu = vec2(vUv.x, 1.0 - vUv.y);',
    '  float t = texture2D(uMark, mu).r;',
    '  float glow = texture2D(uMark, mu + vec2(0.004, 0.006)).g;',
    '  float er = fbm(p * 9.0 + vec2(uTime * 0.03, 0.0));',
    '  float a = clamp((t * (0.7 + er * 0.6) - (1.0 - uMarkT) * 1.1) * 3.0, 0.0, 1.0);',
    '  col = mix(col, col * 0.86, glow * 0.35 * uMarkT);',
    '  col = mix(col, vec3(0.985, 0.99, 1.0), a);',
    // a light grain against banding
    '  col += (hash(gl_FragCoord.xy + fract(uTime)) - 0.5) / 255.0 * 1.5;',
    '  gl_FragColor = vec4(col, 1.0);',
    '}'
  ].join('\n');

  function shader(type, src) {
    var s = gl.createShader(type);
    gl.shaderSource(s, src); gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) { console.warn(gl.getShaderInfoLog(s)); return null; }
    return s;
  }
  var vs = shader(gl.VERTEX_SHADER, VERT), fs = shader(gl.FRAGMENT_SHADER, FRAG);
  if (!vs || !fs) { hero.classList.add('is-written'); return; }
  var prog = gl.createProgram();
  gl.attachShader(prog, vs); gl.attachShader(prog, fs); gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) { hero.classList.add('is-written'); return; }
  gl.useProgram(prog);
  var buf = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]), gl.STATIC_DRAW);
  var loc = gl.getAttribLocation(prog, 'p');
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
  var U = {};
  ['uRes', 'uTime', 'uMouse', 'uMouseOn', 'uMark', 'uMarkT', 'uCloud', 'uDrift', 'uSeed'].forEach(function (n) { U[n] = gl.getUniformLocation(prog, n); });

  // the mark: "2.7", drawn once into a texture the shader reads as mist density
  var markCanvas = document.createElement('canvas');
  var tex = gl.createTexture();
  function drawMark(w, h) {
    markCanvas.width = w; markCanvas.height = h;
    var c = markCanvas.getContext('2d');
    c.fillStyle = '#000'; c.fillRect(0, 0, w, h);
    var size = Math.min(h * 0.46, w * 0.3);
    c.textAlign = 'center'; c.textBaseline = 'alphabetic';
    c.font = '400 ' + Math.round(size) + 'px Cinzel, "Lora", Georgia, serif';
    var y = h * 0.62;
    // green: a soft shadow under the letters; red: the letters
    c.filter = 'blur(' + Math.round(size * 0.05) + 'px)';
    c.fillStyle = '#00ff00'; c.fillText('2.7', w / 2, y);
    c.filter = 'blur(' + Math.max(1, Math.round(size * 0.008)) + 'px)';
    c.globalCompositeOperation = 'lighter';
    c.fillStyle = '#ff0000'; c.fillText('2.7', w / 2, y);
    c.globalCompositeOperation = 'source-over'; c.filter = 'none';
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, markCanvas);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  var scale = 0.55, W = 0, H = 0;
  function resize() {
    var dpr = Math.min(devicePixelRatio || 1, 2);
    W = Math.max(2, Math.round(canvas.clientWidth * dpr * scale));
    H = Math.max(2, Math.round(canvas.clientHeight * dpr * scale));
    canvas.width = W; canvas.height = H;
    gl.viewport(0, 0, W, H);
    drawMark(Math.min(1024, W), Math.min(1024, Math.round(H * Math.min(1024, W) / W)));
  }

  var state = { cloud: 1, drift: 1, markStart: 0, mouse: [0.5, 0.5], mouseOn: 0, seed: 3.0 };
  var cloudNow = 1, driftNow = 1, mouseNow = [0.5, 0.5];
  hero.addEventListener('pointermove', function (e) {
    var r = hero.getBoundingClientRect();
    state.mouse = [(e.clientX - r.left) / r.width, 1 - (e.clientY - r.top) / r.height];
    state.mouseOn = 1;
  });
  hero.addEventListener('pointerleave', function () { state.mouseOn = 0; });

  var buttons = hero.querySelectorAll('.scene button');
  buttons.forEach(function (b) {
    b.addEventListener('click', function () {
      var on = b.getAttribute('aria-pressed') !== 'true';
      switch (b.dataset.scene) {
        case 'clouds': state.cloud = on ? 1 : 0; b.setAttribute('aria-pressed', on); break;
        case 'wind': state.drift = on ? 1 : 0; b.setAttribute('aria-pressed', on); break;
        case 'mark': hero.classList.remove('is-written'); state.markStart = now(); break;
        case 'range': state.seed = (state.seed + 1.618) % 50; break;
      }
    });
  });

  var t0 = performance.now();
  function now() { return (performance.now() - t0) / 1000; }
  var visible = true, raf = 0, last = 0;
  new IntersectionObserver(function (es) {
    visible = es[0].isIntersecting;
    if (visible && !raf) raf = requestAnimationFrame(frame);
  }).observe(hero);

  function markProgress(t) {
    var s = t - state.markStart - 1.1;
    var x = Math.min(1, Math.max(0, s / 3.2));
    return x * x * (3 - 2 * x);
  }
  function frame() {
    raf = 0;
    if (!visible) return;
    var t = reduce ? 6 : now();
    if (!reduce && t - last < 1 / 40) { raf = requestAnimationFrame(frame); return; }
    last = t;
    cloudNow += (state.cloud - cloudNow) * 0.06;
    driftNow += (state.drift - driftNow) * 0.05;
    mouseNow[0] += (state.mouse[0] - mouseNow[0]) * 0.08;
    mouseNow[1] += (state.mouse[1] - mouseNow[1]) * 0.08;
    var mt = reduce ? 1 : markProgress(t);
    if (mt > 0.55) hero.classList.add('is-written');
    gl.uniform2f(U.uRes, W, H);
    gl.uniform1f(U.uTime, t * driftNow + 40);
    gl.uniform2f(U.uMouse, mouseNow[0], mouseNow[1]);
    gl.uniform1f(U.uMouseOn, state.mouseOn);
    gl.uniform1f(U.uMarkT, mt);
    gl.uniform1f(U.uCloud, cloudNow);
    gl.uniform1f(U.uDrift, 1);
    gl.uniform1f(U.uSeed, state.seed);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.uniform1i(U.uMark, 0);
    gl.drawArrays(gl.TRIANGLES, 0, 6);
    if (!reduce) raf = requestAnimationFrame(frame);
  }

  function start() {
    resize();
    hero.classList.add('has-gl');
    state.markStart = now();
    raf = requestAnimationFrame(frame);
  }
  addEventListener('resize', function () { resize(); if (reduce) frame(); });
  // the mark needs its font; draw once it is there
  (document.fonts && document.fonts.load ? document.fonts.load('400 120px Cinzel') : Promise.resolve()).then(start, start);
})();
