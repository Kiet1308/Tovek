// The cinema page: loads a film module, draws it live on the stage, and drives the controls.
// The film is a pure function of time, so the player only owns a clock: play, pause and seek
// change `t`, and every frame is drawn from `t` alone.

import { FILMS, DEFAULT_FILM, filmById } from './films/index.js';
import { createStage, openFilm, chapterAt, captionAt, ScorePlayer, prepareScore, timecode, clamp } from './engine/index.js';

const $ = (id) => document.getElementById(id);
const el = {
  screen: $('screen'), canvas: $('stage'), poster: $('poster'), posterKicker: $('poster-kicker'), posterTitle: $('poster-title'),
  pill: $('play-pill'), pillLabel: $('play-pill-label'), pillTime: $('play-pill-time'),
  notice: $('notice'), noticeKicker: $('notice-kicker'), noticeTitle: $('notice-title'), noticeNote: $('notice-note'), noticeLink: $('notice-link'),
  captions: $('captions'), captionLine: $('caption-line'), osd: $('osd'), controls: $('controls'),
  scrub: $('scrub'), track: $('scrub-track'), head: $('scrub-head'),
  preview: $('preview'), previewCanvas: $('preview-frame'), previewTime: $('preview-time'), previewChapter: $('preview-chapter'),
  play: $('btn-play'), cc: $('btn-cc'), mute: $('btn-mute'), vol: $('vol'), volRange: $('vol-range'), fs: $('btn-fs'),
  clockNow: $('clock-now'), clockDur: $('clock-dur'), nowChapter: $('now-chapter'),
  reels: $('reels'), kicker: $('film-kicker'), title: $('film-title'), desc: $('film-desc'), facts: $('film-facts'),
  chapters: $('chapters'), chaptersCount: $('chapters-count'), stills: $('stills'), stillsGrid: $('stills-grid'),
};

const params = new URLSearchParams(location.search);
const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');
const store = {
  get(k, d) { try { const v = localStorage.getItem('tovek.watch.' + k); return v === null ? d : v; } catch { return d; } },
  set(k, v) { try { localStorage.setItem('tovek.watch.' + k, String(v)); } catch { /* private mode */ } },
};

let entry = filmById(params.get('film')) || filmById(DEFAULT_FILM);
let film = null;
let stage = null;
let previewStage = null;
let score = null;
let raf = 0;
let started = false;
let ended = false;
let idleTimer = 0;
let osdTimer = 0;

// ---------------------------------------------------------------- clock

const clock = {
  playing: false, t: 0, t0: 0, perf0: 0,
  now() { return this.playing ? this.t0 + (performance.now() - this.perf0) / 1000 : this.t; },
  play() { this.t0 = this.t; this.perf0 = performance.now(); this.playing = true; },
  pause() { this.t = this.now(); this.playing = false; },
  seek(t) { this.t = t; this.t0 = t; this.perf0 = performance.now(); },
};

// optional frame statistics for testing: watch/?film=demo&stats
const stats = params.has('stats') ? { frames: [], render: [], t: [] } : null;
if (stats) window.__watchStats = stats;

// ---------------------------------------------------------------- page setup

function markReels() {
  for (const a of el.reels.querySelectorAll('.reel')) {
    const f = filmById(a.dataset.film);
    if (a.dataset.film === entry.id) a.setAttribute('aria-current', 'page');
    if (f && !f.available) {
      const soon = document.createElement('span');
      soon.className = 'reel-soon';
      soon.textContent = 'soon';
      a.append(' ', soon);
    }
  }
  if (entry.listed === false) {
    const a = document.createElement('a');
    a.className = 'reel';
    a.href = `?film=${entry.id}`;
    a.textContent = entry.label;
    a.setAttribute('aria-current', 'page');
    el.reels.append(a);
  }
}

function showNotice({ kicker, title, note, link }) {
  el.screen.classList.remove('is-loading');
  el.notice.hidden = false;
  el.poster.hidden = true;
  el.controls.hidden = true;
  el.noticeKicker.textContent = kicker || '';
  el.noticeTitle.textContent = title || '';
  el.noticeNote.textContent = note || '';
  el.noticeLink.hidden = !link;
  if (link) { el.noticeLink.href = link.href; el.noticeLink.firstChild.textContent = link.text + ' '; }
}

function fillProgramme(f, pendingNote) {
  el.kicker.textContent = entry.kicker || entry.label;
  el.title.textContent = f ? f.title : entry.title;
  el.desc.textContent = pendingNote ? `${entry.description || ''} ${pendingNote}`.trim() : (f && f.description) || entry.description || '';
  document.querySelector('.programme').classList.toggle('is-pending', !f);
  document.title = `${entry.label} · Tovek Films`;
  el.facts.replaceChildren();
  if (!f) return;
  const fact = (k, v) => {
    const d = document.createElement('div');
    d.innerHTML = '<dt></dt><dd></dd>';
    d.firstChild.textContent = k;
    d.lastChild.textContent = v;
    el.facts.append(d);
  };
  fact('Length', timecode(f.duration));
  fact('Chapters', String(f.chapters.length));
  fact('Sound', f.score ? 'Original score' : 'None');
  fact('Captions', f.captions.length ? 'English' : 'None');
}

function buildChapters() {
  el.chapters.replaceChildren();
  el.chaptersCount.textContent = `${film.chapters.length} · ${timecode(film.duration)}`;
  film.chapters.forEach((ch, i) => {
    const li = document.createElement('li');
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'chapter';
    b.innerHTML = '<canvas class="chapter-still" aria-hidden="true"></canvas><span class="chapter-n"></span><span class="chapter-title"></span><span class="chapter-time"></span><i class="chapter-progress" aria-hidden="true"></i>';
    b.children[1].textContent = String(i + 1).padStart(2, '0');
    b.children[2].textContent = ch.title;
    b.children[3].textContent = timecode(ch.t);
    ch.still = ch.still ?? Math.min(ch.end - 0.01, ch.t + Math.min(2.5, (ch.end - ch.t) / 2));
    ch.stillCanvas = b.children[0];
    b.setAttribute('aria-label', `Chapter ${i + 1}: ${ch.title}, at ${timecode(ch.t)}`);
    b.addEventListener('click', () => {
      seek(ch.t);
      if (!clock.playing && !ended) play();
    });
    ch.button = b;
    ch.bar = b.lastChild;
    li.append(b);
    el.chapters.append(li);
  });

  // one still per chapter, drawn from the film itself, one per frame so boot stays light
  const stills = film.chapters.slice();
  const nextStill = () => {
    const ch = stills.shift();
    if (!ch) return;
    const r = ch.stillCanvas.getBoundingClientRect();
    const s = createStage(ch.stillCanvas);
    s.resize(r.width || 104, r.height || 58.5, Math.min(2, devicePixelRatio || 1));
    try { s.draw(film, ch.still); } catch { /* the main stage reports errors */ }
    requestAnimationFrame(nextStill);
  };
  requestAnimationFrame(nextStill);

  el.track.replaceChildren();
  film.chapters.forEach((ch) => {
    const seg = document.createElement('span');
    seg.className = 'seg';
    seg.style.setProperty('--len', String(Math.max(0.0001, ch.end - ch.t)));
    seg.innerHTML = '<i class="seg-hover"></i><i class="seg-fill"></i>';
    ch.seg = { hover: seg.firstChild, fill: seg.lastChild, lastFill: -1, lastHover: -1 };
    el.track.append(seg);
  });
}

// ---------------------------------------------------------------- scrubber geometry

const geo = { width: 0, gap: 3, segs: [] };

function layoutScrub() {
  geo.width = el.track.clientWidth;
  const n = film.chapters.length;
  const avail = Math.max(1, geo.width - geo.gap * (n - 1));
  let x = 0;
  geo.segs = film.chapters.map((ch) => {
    const w = (avail * (ch.end - ch.t)) / film.duration;
    const s = { x, w, t: ch.t, end: ch.end };
    x += w + geo.gap;
    return s;
  });
}

function xOf(t) {
  for (const s of geo.segs) if (t <= s.end) return s.x + clamp((t - s.t) / (s.end - s.t || 1)) * s.w;
  return geo.width;
}

function tOf(x) {
  for (const s of geo.segs) if (x < s.x + s.w + geo.gap / 2) return s.t + clamp((x - s.x) / (s.w || 1)) * (s.end - s.t);
  return film.duration;
}

// ---------------------------------------------------------------- drawing

let lastSecond = -1;
let lastCaption = undefined;
let lastChapter = null;
let lastAria = 0;

function draw(t) {
  if (stats) {
    const a = performance.now();
    stage.draw(film, t);
    stats.render.push(performance.now() - a);
    stats.t.push(t);
  } else stage.draw(film, t);
  updateUI(t);
}

function updateUI(t) {
  const sec = Math.floor(t);
  if (sec !== lastSecond) {
    lastSecond = sec;
    el.clockNow.textContent = timecode(t);
  }
  for (const ch of film.chapters) {
    const p = clamp((t - ch.t) / (ch.end - ch.t || 1));
    if (Math.abs(p - ch.seg.lastFill) > 0.0005) {
      ch.seg.fill.style.transform = `scaleX(${p.toFixed(4)})`;
      ch.seg.lastFill = p;
    }
  }
  el.head.style.setProperty('--x', `${xOf(t).toFixed(1)}px`);
  const ch = chapterAt(film, t);
  if (ch !== lastChapter) {
    if (lastChapter) lastChapter.button.classList.remove('is-current');
    ch.button.classList.add('is-current');
    el.nowChapter.textContent = ch.title;
    lastChapter = ch;
  }
  ch.bar.style.transform = `scaleX(${clamp((t - ch.t) / (ch.end - ch.t || 1)).toFixed(4)})`;
  const cap = captionAt(film, t);
  if (cap !== lastCaption) {
    el.captionLine.textContent = cap ? cap.text : '';
    lastCaption = cap;
  }
  const now = performance.now();
  if (!clock.playing || now - lastAria > 1000) {
    lastAria = now;
    el.scrub.setAttribute('aria-valuenow', t.toFixed(1));
    el.scrub.setAttribute('aria-valuetext', `${timecode(t)} of ${timecode(film.duration)}, ${ch.title}`);
  }
}

function frame(ts) {
  raf = 0;
  let t = clock.now();
  if (clock.playing && t >= film.duration) {
    t = film.duration;
    clock.seek(t);
    finish();
  }
  if (stats && clock.playing) stats.frames.push(ts);
  try {
    draw(t);
  } catch (err) {
    console.error(err);
    pause();
    showNotice({ kicker: entry.label, title: 'Something went wrong', note: `The film stopped at ${timecode(t)}. Reload the page to try again.` });
    return;
  }
  if (clock.playing) {
    if (score) score.update(t);
    raf = requestAnimationFrame(frame);
  }
}

function requestDraw() {
  if (!raf && film) raf = requestAnimationFrame(frame);
}

// ---------------------------------------------------------------- transport

function setStarted(v) {
  started = v;
  el.screen.classList.toggle('is-started', v);
}

function play() {
  if (!film) return;
  // the poster shows a frame from inside the film; the film itself starts at the top
  if (!started) clock.seek(0);
  if (ended || clock.t >= film.duration - 1e-3) {
    ended = false;
    el.screen.classList.remove('is-ended');
    clock.seek(0);
  }
  setStarted(true);
  clock.play();
  startScore();
  el.screen.classList.add('is-playing');
  el.play.setAttribute('aria-label', 'Pause');
  document.body.classList.add('lights-down');
  wake();
  requestDraw();
}

function pause() {
  if (!film) return;
  clock.pause();
  if (score) score.pause();
  el.screen.classList.remove('is-playing');
  el.play.setAttribute('aria-label', 'Play');
  document.body.classList.remove('lights-down');
  wake();
  requestDraw();
}

function finish() {
  clock.pause();
  if (score) score.pause();
  ended = true;
  el.screen.classList.remove('is-playing');
  el.screen.classList.add('is-ended');
  el.play.setAttribute('aria-label', 'Play again');
  el.pillLabel.textContent = 'Watch again';
  document.body.classList.remove('lights-down');
  wake();
}

const togglePlay = () => (clock.playing ? pause() : play());

// Sound follows the picture. Building the audio graph costs 30-90 ms (the audio device, the reverb's
// FFT setup), so it happens on the press that comes before play (warmAudio). If the very first
// input is the play key itself, the first frame is drawn first and the sound joins a frame later.
function startScore() {
  if (!score) return;
  if (score.ready) { score.start(clock.now()); return; }
  requestAnimationFrame(() => setTimeout(() => { if (clock.playing) score.start(clock.now()); }, 0));
}

const PLAY_KEYS = new Set([' ', 'k', 'K', 'Enter']);
function warmAudio(e) {
  if (!score || score.ready) return;
  if (e.type === 'keydown' && (PLAY_KEYS.has(e.key) || /^[0-9]$/.test(e.key))) return;
  score.warm();
}
document.addEventListener('pointerdown', warmAudio, { capture: true, passive: true });
document.addEventListener('keydown', warmAudio, { capture: true });

function seek(t) {
  if (!film) return;
  t = clamp(t, 0, film.duration);
  clock.seek(t);
  if (ended && t < film.duration) {
    ended = false;
    el.screen.classList.remove('is-ended');
    el.pillLabel.textContent = 'Play';
  }
  if (!started) setStarted(true);
  if (clock.playing) startScore();
  requestDraw();
}

const seekBy = (dt) => seek(clock.now() + dt);

// ---------------------------------------------------------------- idle, house lights, osd

function wake() {
  el.screen.classList.remove('is-idle');
  clearTimeout(idleTimer);
  if (clock.playing) idleTimer = setTimeout(goIdle, 2600);
}

function goIdle() {
  if (!clock.playing) return;
  if (el.controls.matches(':hover') || el.controls.contains(document.activeElement)) {
    idleTimer = setTimeout(goIdle, 1500);
    return;
  }
  el.screen.classList.add('is-idle');
}

function osd(text) {
  el.osd.textContent = text;
  el.osd.classList.add('is-on');
  clearTimeout(osdTimer);
  osdTimer = setTimeout(() => el.osd.classList.remove('is-on'), 900);
}

// ---------------------------------------------------------------- sound, captions, full screen

function applySound() {
  const muted = store.get('muted', '0') === '1';
  const vol = +store.get('volume', '0.9');
  el.volRange.value = String(vol);
  el.volRange.style.setProperty('--v', `${vol * 100}%`);
  el.screen.classList.toggle('is-muted', muted || vol === 0);
  el.mute.setAttribute('aria-label', muted ? 'Unmute' : 'Mute');
  if (score) { score.setVolume(vol); score.setMuted(muted); }
}

function toggleMute() {
  if (!score) return;
  const muted = store.get('muted', '0') !== '1';
  store.set('muted', muted ? '1' : '0');
  if (!muted && +store.get('volume', '0.9') === 0) store.set('volume', '0.6');
  applySound();
  osd(muted ? 'Sound off' : 'Sound on');
}

function applyCaptions() {
  const on = store.get('captions', '1') === '1' && film && film.captions.length > 0;
  el.screen.classList.toggle('captions-off', !on);
  el.cc.setAttribute('aria-pressed', String(on));
  el.cc.setAttribute('aria-label', on ? 'Turn captions off' : 'Turn captions on');
  el.captions.setAttribute('aria-live', on ? 'polite' : 'off');
}

function toggleCaptions() {
  if (!film || !film.captions.length) return;
  const on = store.get('captions', '1') !== '1';
  store.set('captions', on ? '1' : '0');
  applyCaptions();
  osd(on ? 'Captions on' : 'Captions off');
}

const fsElement = () => document.fullscreenElement || document.webkitFullscreenElement;
function toggleFullscreen() {
  const s = el.screen;
  if (fsElement()) (document.exitFullscreen || document.webkitExitFullscreen).call(document);
  else if (s.requestFullscreen) s.requestFullscreen().catch(() => {});
  else if (s.webkitRequestFullscreen) s.webkitRequestFullscreen();
}
document.addEventListener('fullscreenchange', () => {
  el.fs.setAttribute('aria-label', fsElement() ? 'Exit full screen' : 'Full screen');
  wake();
});

// ---------------------------------------------------------------- scrubber input

let dragging = false;
let resumeAfterDrag = false;
let trackRect = null;
let previewW = 0;
let previewT = -1;
let previewWanted = -1;
let previewRaf = 0;

function pointerT(e) {
  if (!trackRect) trackRect = el.track.getBoundingClientRect();
  return tOf(clamp(e.clientX - trackRect.left, 0, trackRect.width));
}

function showHover(e) {
  if (!film) return;
  if (!trackRect) trackRect = el.track.getBoundingClientRect();
  if (!previewW) previewW = el.preview.offsetWidth;
  const x = clamp(e.clientX - trackRect.left, 0, trackRect.width);
  const t = tOf(x);
  el.scrub.classList.add('is-hovering');
  el.preview.style.setProperty('--px', `${clamp(x - previewW / 2, -6, trackRect.width - previewW + 6).toFixed(1)}px`);
  el.previewTime.textContent = timecode(t);
  el.previewChapter.textContent = chapterAt(film, t).title;
  for (const ch of film.chapters) {
    const p = clamp((t - ch.t) / (ch.end - ch.t || 1));
    if (p !== ch.seg.lastHover) { ch.seg.hover.style.transform = `scaleX(${p.toFixed(4)})`; ch.seg.lastHover = p; }
  }
  previewWanted = t;
  if (!previewRaf) previewRaf = requestAnimationFrame(drawPreview);
}

function drawPreview() {
  previewRaf = 0;
  if (previewWanted < 0 || Math.abs(previewWanted - previewT) < 1 / 30) return;
  if (!previewStage) previewStage = createStage(el.previewCanvas);
  const r = el.previewCanvas.getBoundingClientRect();
  previewStage.resize(r.width, r.height, Math.min(2, devicePixelRatio || 1));
  previewT = previewWanted;
  try { previewStage.draw(film, previewT); } catch { /* the main stage reports errors */ }
}

function hideHover() {
  el.scrub.classList.remove('is-hovering');
  for (const ch of film ? film.chapters : []) { ch.seg.hover.style.transform = 'scaleX(0)'; ch.seg.lastHover = 0; }
  previewWanted = -1;
}

el.scrub.addEventListener('pointerenter', () => { trackRect = el.track.getBoundingClientRect(); previewW = el.preview.offsetWidth; });
el.scrub.addEventListener('pointermove', (e) => {
  if (e.pointerType === 'mouse' || dragging) showHover(e);
  if (dragging) seek(pointerT(e));
});
el.scrub.addEventListener('pointerleave', () => { if (!dragging) hideHover(); });
el.scrub.addEventListener('pointerdown', (e) => {
  if (!film || e.button > 0) return;
  e.preventDefault();
  el.scrub.setPointerCapture(e.pointerId);
  dragging = true;
  trackRect = el.track.getBoundingClientRect();
  el.scrub.classList.add('is-dragging');
  resumeAfterDrag = clock.playing;
  if (clock.playing) { clock.pause(); if (score) score.pause(); }
  seek(pointerT(e));
  showHover(e);
});
const endDrag = (e) => {
  if (!dragging) return;
  dragging = false;
  el.scrub.classList.remove('is-dragging');
  if (e.pointerType !== 'mouse') hideHover();
  if (resumeAfterDrag && !ended) { clock.play(); startScore(); requestDraw(); }
};
el.scrub.addEventListener('pointerup', endDrag);
el.scrub.addEventListener('pointercancel', endDrag);

// ---------------------------------------------------------------- buttons and keys

// the whole poster is the play button's hit area (the pill inside it is the focusable control)
el.poster.addEventListener('click', () => {
  if (!film) return;
  play();
  el.screen.focus({ preventScroll: true });
});
el.play.addEventListener('click', togglePlay);
el.cc.addEventListener('click', toggleCaptions);
el.mute.addEventListener('click', toggleMute);
el.fs.addEventListener('click', toggleFullscreen);
el.volRange.addEventListener('input', () => {
  const v = +el.volRange.value;
  store.set('volume', v);
  store.set('muted', v === 0 ? '1' : '0');
  applySound();
});

// clicking the picture toggles playback; a double click toggles full screen. On touch screens the
// first tap on a playing film only brings the controls back.
let tapWakes = false;
el.screen.addEventListener('pointerdown', (e) => {
  tapWakes = e.pointerType !== 'mouse' && el.screen.classList.contains('is-idle');
  wake();
});
el.canvas.addEventListener('click', () => {
  if (tapWakes) { tapWakes = false; return; }
  if (started && !ended) togglePlay();
});
el.canvas.addEventListener('dblclick', toggleFullscreen);
el.screen.addEventListener('pointermove', (e) => { if (e.pointerType === 'mouse') wake(); });

document.addEventListener('keydown', (e) => {
  if (!film || e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return;
  const target = e.target;
  if (target === el.volRange && /^(Arrow|Home|End|Page)/.test(e.key)) return;
  const onControl = target instanceof HTMLButtonElement || target instanceof HTMLAnchorElement;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  const step = 1 / 60;
  switch (key) {
    case ' ':
      if (onControl) return;
      e.preventDefault();
      togglePlay();
      break;
    case 'k': togglePlay(); break;
    case 'ArrowLeft': e.preventDefault(); seekBy(-5); osd('−5 s'); break;
    case 'ArrowRight': e.preventDefault(); seekBy(5); osd('+5 s'); break;
    case ',': if (!clock.playing) { seekBy(-step); osd(timecode(clock.t, { tenths: true })); } break;
    case '.': if (!clock.playing) { seekBy(step); osd(timecode(clock.t, { tenths: true })); } break;
    case 'f': toggleFullscreen(); break;
    case 'm': toggleMute(); break;
    case 'c': toggleCaptions(); break;
    case 'Home': if (target === el.scrub) { e.preventDefault(); seek(0); } break;
    case 'End': if (target === el.scrub) { e.preventDefault(); seek(film.duration); } break;
    case 'PageUp': case 'PageDown': {
      if (target !== el.scrub) return;
      e.preventDefault();
      const i = film.chapters.indexOf(chapterAt(film, clock.now()));
      const next = film.chapters[clamp(i + (key === 'PageDown' ? 1 : -1), 0, film.chapters.length - 1)];
      seek(next.t);
      break;
    }
    default:
      if (/^[0-9]$/.test(key)) {
        seek((film.duration * +key) / 10);
        osd(timecode(clock.t));
      } else return;
  }
  wake();
});

document.addEventListener('visibilitychange', () => {
  if (document.hidden && clock.playing) pause();
});

// ---------------------------------------------------------------- reduced motion: stills

function buildStills() {
  el.stillsGrid.replaceChildren();
  const show = reducedMotion.matches && !!film;
  el.stills.hidden = !show;
  el.pillLabel.textContent = ended ? 'Watch again' : show ? 'Play the film' : 'Play';
  if (!show) return;
  film.chapters.forEach((ch, i) => {
    const li = document.createElement('li');
    li.className = 'still';
    const b = document.createElement('button');
    b.type = 'button';
    b.setAttribute('aria-label', `Show chapter ${i + 1}, ${ch.title}`);
    const c = document.createElement('canvas');
    b.append(c);
    const h = document.createElement('h3');
    h.innerHTML = '<span></span>';
    h.firstChild.textContent = String(i + 1).padStart(2, '0');
    h.append(ch.title);
    li.append(b, h);
    const lines = film.captions.filter((cap) => cap.start >= ch.t && cap.start < ch.end).map((cap) => cap.text);
    if (lines.length) {
      const p = document.createElement('p');
      p.textContent = lines.join(' ');
      li.append(p);
    }
    const at = ch.still;
    b.addEventListener('click', () => {
      pause();
      seek(at);
      el.screen.scrollIntoView({ block: 'center' });
    });
    el.stillsGrid.append(li);
    requestAnimationFrame(() => {
      const s = createStage(c);
      const r = c.getBoundingClientRect();
      s.resize(r.width || 320, r.height || 180, Math.min(2, devicePixelRatio || 1));
      s.draw(film, at);
    });
  });
}
reducedMotion.addEventListener('change', buildStills);

// ---------------------------------------------------------------- boot

function observeSize() {
  let last = null;
  // Size the backing store to the device pixels under the canvas. The device-pixel box is exact
  // (it snaps to the physical grid), but trust it only when it agrees with CSS size × DPR: some
  // emulated and zoomed setups report a CSS-sized box.
  const fit = () => {
    if (!last) return;
    const dpr = devicePixelRatio || 1;
    const ew = last.w * dpr, eh = last.h * dpr;
    const ok = last.dw && Math.abs(last.dw - ew) <= 2 && Math.abs(last.dh - eh) <= 2;
    if (stage.resize(ok ? last.dw : ew, ok ? last.dh : eh, 1)) requestDraw();
  };
  const ro = new ResizeObserver((entries) => {
    const e = entries[entries.length - 1];
    const box = e.devicePixelContentBoxSize && e.devicePixelContentBoxSize[0];
    last = { w: e.contentRect.width, h: e.contentRect.height, dw: box ? box.inlineSize : 0, dh: box ? box.blockSize : 0 };
    fit();
    if (film) { layoutScrub(); trackRect = null; previewW = 0; }
  });
  try { ro.observe(el.canvas, { box: 'device-pixel-content-box' }); } catch { ro.observe(el.canvas); }
  // moving the window to a screen with another pixel density
  const watchDpr = () => {
    const mq = matchMedia(`(resolution: ${devicePixelRatio}dppx)`);
    mq.addEventListener('change', () => { fit(); watchDpr(); }, { once: true });
  };
  watchDpr();
  new ResizeObserver(() => { if (film) { layoutScrub(); trackRect = null; requestDraw(); } }).observe(el.track);
}

async function boot() {
  markReels();
  fillProgramme(null);
  const pending = (note) => {
    fillProgramme(null, note);
    showNotice({ kicker: entry.kicker || entry.label, title: entry.title, note, link: entry.id !== DEFAULT_FILM ? { href: `?film=${DEFAULT_FILM}`, text: 'Watch the story' } : null });
  };
  if (!entry.available) return pending(entry.note);
  let mod;
  try {
    mod = await import(new URL(entry.src, new URL('./films/', import.meta.url)).href);
  } catch {
    return pending(entry.note || 'This film could not be loaded.');
  }
  try {
    film = await openFilm(mod, { base: location.href });
  } catch (err) {
    console.error(err);
    return pending('This film could not be loaded.');
  }

  stage = createStage(el.canvas);
  if (film.score && typeof AudioContext !== 'undefined') score = new ScorePlayer(film.score);
  else { el.mute.disabled = true; el.volRange.disabled = true; }

  fillProgramme(film);
  buildChapters();
  layoutScrub();
  observeSize();
  applySound();
  applyCaptions();

  el.clockDur.textContent = timecode(film.duration);
  el.scrub.setAttribute('aria-valuemax', film.duration.toFixed(1));
  el.posterKicker.textContent = `${entry.kicker || entry.label} · ${timecode(film.duration)}`;
  el.posterTitle.textContent = film.title;
  if (film.posterTitle === false) el.poster.querySelector('.poster-copy').hidden = true;
  el.pillTime.textContent = timecode(film.duration);
  el.pill.disabled = false;
  el.pill.setAttribute('aria-label', `Play ${film.title}, ${timecode(film.duration)}`);
  el.screen.classList.remove('is-loading');

  const startAt = params.has('t') ? clamp(+params.get('t') || 0, 0, film.duration) : film.poster;
  clock.seek(startAt);
  if (params.has('t')) setStarted(true);
  buildStills();
  requestDraw();
  if (film.score) (window.requestIdleCallback || setTimeout)(() => prepareScore(film.score), { timeout: 2000 });
}

boot();

// a small handle for tests and the export tooling
window.__player = {
  get film() { return film; },
  get t() { return clock.now(); },
  get playing() { return clock.playing; },
  play, pause, seek,
  films: FILMS,
};
