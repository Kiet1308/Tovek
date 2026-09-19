'use strict';

document.documentElement.classList.add('js');

(() => {
  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
  let paused = reduceMotion.matches;
  const motionListeners = [];
  const motionButton = document.querySelector('[data-motion]');

  function updateMotion() {
    document.body.classList.toggle('paused', paused);
    if (motionButton) {
      motionButton.setAttribute('aria-pressed', String(paused));
      motionButton.textContent = paused ? 'Enable motion' : 'Pause motion';
    }
    motionListeners.forEach(listener => listener(paused));
  }
  motionButton?.addEventListener('click', () => { paused = !paused; updateMotion(); });
  reduceMotion.addEventListener('change', event => { paused = event.matches; updateMotion(); });
  updateMotion();

  // Content remains visible if script loading fails or JavaScript is disabled.
  if ('IntersectionObserver' in window && !reduceMotion.matches) {
    const reveal = new IntersectionObserver(entries => {
      entries.forEach(entry => {
        if (entry.isIntersecting) {
          entry.target.classList.remove('pending');
          reveal.unobserve(entry.target);
        }
      });
    }, { threshold: 0.08 });
    document.querySelectorAll('.reveal').forEach(element => {
      if (element.getBoundingClientRect().top > innerHeight) element.classList.add('pending');
      reveal.observe(element);
    });
  }

  const hero = document.querySelector('.hero');
  const art = document.querySelector('.hero-art');
  const progress = document.querySelector('.reading-progress');
  let scrollFrame = 0;
  function onScroll() {
    if (scrollFrame) return;
    scrollFrame = requestAnimationFrame(() => {
      scrollFrame = 0;
      if (art) art.style.setProperty('--art-drift', paused ? '0px' : `${Math.min(scrollY * .11, 85)}px`);
      if (progress) {
        const range = document.documentElement.scrollHeight - innerHeight;
        progress.style.width = `${range > 0 ? Math.min(100, scrollY / range * 100) : 0}%`;
      }
    });
  }
  window.addEventListener('scroll', onScroll, { passive: true });
  window.addEventListener('resize', onScroll, { passive: true });
  motionListeners.push(onScroll);
  hero?.addEventListener('pointermove', event => {
    if (paused || event.pointerType === 'touch') return;
    const bounds = hero.getBoundingClientRect();
    art.style.setProperty('--art-angle', `${(event.clientX / bounds.width - .5) * 6}deg`);
  }, { passive: true });
  hero?.addEventListener('pointerleave', () => art.style.setProperty('--art-angle', '0deg'));

  function bindTabs(selector, onSelect) {
    const tabs = [...document.querySelectorAll(selector)];
    function select(tab, focus = false) {
      tabs.forEach(item => {
        const selected = item === tab;
        item.setAttribute('aria-selected', String(selected));
        item.tabIndex = selected ? 0 : -1;
      });
      const panel = document.getElementById(tab.getAttribute('aria-controls'));
      panel?.setAttribute('aria-labelledby', tab.id);
      onSelect(tab);
      if (focus) tab.focus();
    }
    tabs.forEach((tab, index) => {
      tab.addEventListener('click', () => select(tab));
      tab.addEventListener('keydown', event => {
        let next;
        if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
        if (event.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length;
        if (event.key === 'Home') next = 0;
        if (event.key === 'End') next = tabs.length - 1;
        if (next !== undefined) { event.preventDefault(); select(tabs[next], true); }
      });
    });
  }

  const examples = {
    imports: {
      before: 'local components = {}\nlocal label = require(package.Label)\ncomponents.Label = label\nlocal button = require(package.Button)\ncomponents.Button = button\n\nreturn components',
      after: 'local components = {\n    Label = require(package.Label),\n    Button = require(package.Button),\n}\n\nreturn components',
      explanation: 'Illustrative, shortened example. Single-use import relays can join an unobserved table’s constructor. Capture and evaluation-order checks decide when this is safe.'
    },
    names: {
      before: '-- measure returns .Width, .Height\nlocal v, v2 = measure(widget)\n\nreturn v, v2',
      after: '-- Roles follow the returned fields\nlocal width, height = measure(widget)\n\nreturn width, height',
      explanation: 'Illustrative, shortened example. Tuple roles can carry field evidence back to the receiving locals. Inferred names describe usage; they are not a claim to recover stripped source names.'
    },
    returns: {
      before: 'local v\nif condition then\n    v = true\nelse\n    v = false\nend\nreturn v',
      after: 'return not not condition',
      explanation: 'Illustrative, shortened example. A private, uncaptured terminal result can become an exact scalar return. Here the double negation preserves a boolean result for every input value.'
    }
  };

  function highlight(text, target) {
    target.replaceChildren();
    const code = document.createElement('code');
    const pattern = /(--[^\n]*|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\b(?:local|return|function|end|if|then|else|not|true|false|require)\b)/g;
    let cursor = 0;
    for (const match of text.matchAll(pattern)) {
      code.append(document.createTextNode(text.slice(cursor, match.index)));
      const span = document.createElement('span');
      span.className = match[0].startsWith('--') ? 'syntax-comment' : /^['"]/.test(match[0]) ? 'syntax-string' : 'syntax-key';
      span.textContent = match[0];
      code.append(span);
      cursor = match.index + match[0].length;
    }
    code.append(document.createTextNode(text.slice(cursor)));
    target.append(code);
  }
  bindTabs('[data-example]', tab => {
    const example = examples[tab.dataset.example];
    highlight(example.before, document.querySelector('#example-before'));
    highlight(example.after, document.querySelector('#example-after'));
    document.querySelector('#example-explanation').textContent = example.explanation;
  });

  const metrics = {
    names: { title: 'Anonymous bindings · lower is better', beta: '54,058', v2: '36,826', widths: [90.0967,61.3767], mid: '30,000', max: '60,000', note: 'Bindings with generated p/v names fell 31.9% across the same 3,975 parseable private files. This measures fewer anonymous names, not recovery of the author’s original identifiers.' },
    structure: { title: 'Mean raw structural ratio · higher is better', beta: '0.8265', v2: '0.8660', widths: [82.6522,86.5955], mid: '0.5', max: '1.0', note: '405 common public profiles measured: 217 improve, 59 regress, 129 are unchanged. Another 108 profiles exceed alignment budget. Structural similarity is not a proof of equivalent behavior.' },
    runtime: { title: 'Passing shared runtime profiles · higher is better', beta: '138 / 198', v2: '198 / 198', widths: [69.69697,100], mid: '99', max: '198', note: 'The same 198 targeted runtime profiles run against both versions. Beta diverges in 60; V2 passes all 198. The expanded V2 suite passes 246 profiles. These are finite tests, not a whole-game proof.' }
  };
  bindTabs('[data-metric]', tab => {
    const metric = metrics[tab.dataset.metric];
    document.querySelector('#chart-title').textContent = metric.title;
    document.querySelector('#chart-beta-value').textContent = metric.beta;
    document.querySelector('#chart-v2-value').textContent = metric.v2;
    document.querySelector('#chart-beta').style.setProperty('--bar', `${metric.widths[0]}%`);
    document.querySelector('#chart-v2').style.setProperty('--bar', `${metric.widths[1]}%`);
    document.querySelector('#chart-axis-mid').textContent = metric.mid;
    document.querySelector('#chart-axis-max').textContent = metric.max;
    document.querySelector('#chart-note').textContent = metric.note;
  });

  document.querySelectorAll('[data-copy]').forEach(button => {
    const label = button.textContent;
    let reset;
    button.addEventListener('click', async () => {
      const text = document.getElementById(button.dataset.copy).textContent;
      let success = false;
      try { await navigator.clipboard.writeText(text); success = true; } catch {
        const field = document.createElement('textarea');
        field.value = text;
        field.style.cssText = 'position:fixed;left:-9999px;top:0;';
        document.body.append(field);
        field.select();
        try { success = document.execCommand('copy'); } catch { success = false; }
        field.remove();
        button.focus();
      }
      clearTimeout(reset);
      button.textContent = success ? 'Copied ✓' : 'Select to copy';
      document.querySelector('#copy-status').textContent = success ? 'Command copied to clipboard.' : 'Clipboard is unavailable. Select the command text to copy it.';
      reset = setTimeout(() => { button.textContent = label; }, 2200);
    });
  });

  // An original, deterministic 3D point field. No external renderer or artwork.
  const canvas = document.querySelector('#cosmos');
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  canvas.parentElement.classList.add('canvas-ready');
  const pauseButton = document.querySelector('#cosmos-pause');
  const resetButton = document.querySelector('#cosmos-reset');
  let seed = 20260919;
  function random() { seed = (seed * 1664525 + 1013904223) >>> 0; return seed / 4294967296; }
  const mask = document.createElement('canvas');
  mask.width = 400; mask.height = 500;
  const maskContext = mask.getContext('2d', { willReadFrequently: true });
  maskContext.font = '700 490px Arial';
  maskContext.textAlign = 'center';
  maskContext.fillText('2', 200, 427);
  const pixels = maskContext.getImageData(0,0,400,500).data;
  const particles = [];
  for (let y = 40; y < 460; y += 5) {
    for (let x = 30; x < 375; x += 5) {
      if (pixels[(y * 400 + x) * 4 + 3] > 128 && random() > .13) {
        particles.push({ x: (x - 200) / 200 + (random() - .5) * .025, y: (y - 260) / 200 + (random() - .5) * .025, z: (random() - .5) * .8, size: .55 + random() ** 3 * 3.0, phase: random() * Math.PI * 2, color: random() > .89 ? 1 : 0 });
      }
    }
  }
  const background = Array.from({ length: 470 }, () => ({ x: random(), y: random(), size: .25 + random() ** 3 * 1.6, alpha: .15 + random() * .6, phase: random() * 6.28 }));
  const sprites = ['#c9eeff', '#ffdab8'].map(color => {
    const sprite = document.createElement('canvas'); sprite.width = sprite.height = 64;
    const c = sprite.getContext('2d');
    const glow = c.createRadialGradient(32,32,0,32,32,32);
    glow.addColorStop(0,'#ffffff'); glow.addColorStop(.1,color); glow.addColorStop(.25,color + 'aa'); glow.addColorStop(.55,color + '28'); glow.addColorStop(1,color + '00');
    c.fillStyle = glow; c.fillRect(0,0,64,64);
    return sprite;
  });
  let width = 0, height = 0, angle = -.16, targetAngle = -.16, tilt = -.08;
  let dragStart = null, startAngle = 0, visible = true, frame = 0, previous = 0, time = 0;
  function resize() {
    const rect = canvas.getBoundingClientRect();
    width = rect.width; height = rect.height;
    const dpr = Math.min(window.devicePixelRatio || 1, 1.5);
    canvas.width = Math.round(width * dpr); canvas.height = Math.round(height * dpr);
    ctx.setTransform(dpr,0,0,dpr,0,0);
    render();
  }
  function render() {
    if (!width || !height) return;
    ctx.clearRect(0,0,width,height);
    const glow = ctx.createRadialGradient(width*.5,height*.5,0,width*.5,height*.5,width*.53);
    glow.addColorStop(0,'#010406'); glow.addColorStop(.6,'#02070b'); glow.addColorStop(1,'#07131d');
    ctx.fillStyle = glow; ctx.fillRect(0,0,width,height);
    background.forEach(star => {
      ctx.globalAlpha = star.alpha * (.86 + Math.sin(time*.3 + star.phase)*.14);
      ctx.fillStyle = '#b4d8ec'; ctx.beginPath();
      ctx.arc(star.x*width,star.y*height,star.size,0,Math.PI*2); ctx.fill();
    });
    const scale = Math.min(height * .46, width * (width < 740 ? .47 : .37));
    const turn = angle + (paused ? 0 : Math.sin(time * .12) * .055);
    const cos = Math.cos(turn), sin = Math.sin(turn), ct = Math.cos(tilt), st = Math.sin(tilt);
    const projected = particles.map(point => {
      const x = point.x*cos + point.z*sin;
      const z = -point.x*sin + point.z*cos;
      const y = point.y*ct-z*st;
      const depth = point.y*st+z*ct;
      const perspective = 4.8/(4.8-depth);
      return { ...point, px: width*.5 + x*scale*perspective, py: height*.5 + y*scale*perspective, depth, perspective };
    }).sort((a,b) => a.depth-b.depth);
    ctx.globalCompositeOperation = 'screen';
    projected.forEach(point => {
      const flicker = .75 + .25*Math.sin(time*.75+point.phase);
      const radius = point.size * point.perspective * Math.max(.7,Math.min(1.3,width/1300));
      ctx.globalAlpha = (.55 + (point.depth+.65)*.25) * flicker;
      if (point.size > 1.35) ctx.drawImage(sprites[point.color],point.px-radius*5,point.py-radius*5,radius*10,radius*10);
      ctx.fillStyle = point.color ? '#ffdac3' : '#d7f3ff';
      ctx.beginPath();ctx.arc(point.px,point.py,radius*.56,0,Math.PI*2);ctx.fill();
    });
    ctx.globalCompositeOperation = 'source-over';ctx.globalAlpha = 1;
  }
  function animate(now) {
    frame = 0;
    if (!visible || document.hidden || paused) return;
    if (now-previous > 32) {
      time += Math.min((now-previous)/1000,.05);
      angle += (targetAngle-angle)*.08;
      previous = now;render();
    }
    frame = requestAnimationFrame(animate);
  }
  function start() {
    if (!frame && !paused && visible && !document.hidden) { previous=performance.now();frame=requestAnimationFrame(animate); }
  }
  function motionState() {
    pauseButton.setAttribute('aria-pressed',String(paused));
    pauseButton.setAttribute('aria-label',paused?'Play star field':'Pause star field');
    pauseButton.textContent=paused?'▷':'Ⅱ';
    if (paused) { cancelAnimationFrame(frame);frame=0; } else start();
    render();
  }
  pauseButton.addEventListener('click',() => { paused=!paused;updateMotion(); });
  motionListeners.push(motionState);
  resetButton.addEventListener('click',() => { angle=targetAngle=-.16;tilt=-.08;render(); });
  canvas.addEventListener('pointerdown',event => { dragStart=event.clientX;startAngle=targetAngle;canvas.setPointerCapture(event.pointerId); });
  canvas.addEventListener('pointermove',event => {
    if (dragStart===null) return;
    targetAngle=startAngle+(event.clientX-dragStart)/Math.max(width,1)*3.5;
    if (paused) { angle=targetAngle;render(); }
  });
  function endDrag() { dragStart=null; }
  canvas.addEventListener('pointerup',endDrag);canvas.addEventListener('pointercancel',endDrag);
  canvas.addEventListener('keydown',event => {
    if (!['ArrowLeft','ArrowRight','Home'].includes(event.key)) return;
    event.preventDefault();
    if (event.key==='Home') angle=targetAngle=-.16;
    else targetAngle += event.key==='ArrowLeft'?-.16:.16;
    if (paused) angle=targetAngle;
    render();
  });
  const canvasObserver = new IntersectionObserver(entries => {
    visible=entries[0].isIntersecting;
    if (!visible) { cancelAnimationFrame(frame);frame=0; } else start();
  });
  canvasObserver.observe(canvas);
  document.addEventListener('visibilitychange',() => {
    if (document.hidden) { cancelAnimationFrame(frame);frame=0; } else start();
  });
  new ResizeObserver(resize).observe(canvas);
  resize();motionState();start();
})();
