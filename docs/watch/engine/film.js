// Loading and checking film modules. See README.md for the module contract.

/** Fill defaults and check a film module's shape. Throws with a readable message on a bad module. */
export function normalizeFilm(mod) {
  const film = mod && (mod.default || mod.film || mod);
  const problems = [];
  if (!film || typeof film !== 'object') throw new Error('film module must export default { id, title, duration, render }');
  if (typeof film.id !== 'string') problems.push('id (string)');
  if (typeof film.title !== 'string') problems.push('title (string)');
  if (!(film.duration > 0)) problems.push('duration (seconds > 0)');
  if (typeof film.render !== 'function') problems.push('render(ctx, t, w, h)');
  if (problems.length) throw new Error('film module is missing: ' + problems.join(', '));
  const chapters = (film.chapters && film.chapters.length ? film.chapters.slice() : [{ t: 0, title: film.title }])
    .sort((a, b) => a.t - b.t)
    .map((c, i, all) => ({ ...c, end: all[i + 1] ? all[i + 1].t : film.duration }));
  return {
    background: '#0d0d0c',
    poster: Math.min(film.duration, 1),
    fonts: [],
    ...film,
    chapters,
    captions: (film.captions || []).slice().sort((a, b) => a.start - b.start),
  };
}

/**
 * Get a film ready to draw: load its fonts, then run its optional `prepare()` (fetch data, warm
 * layout caches). `base` is the URL the film's relative data paths resolve against.
 */
export async function prepareFilm(film, { base = location.href } = {}) {
  // the sample text pulls in every subset a film is likely to draw (digits, arrows, dashes, quotes)
  await loadFonts(film.fonts || [], film.glyphs || 'Tovek 0123456789 →←↓·—–…“”‘’×−%#{}[]');
  if (typeof film.prepare === 'function') await film.prepare({ base });
  return film;
}

/**
 * Load the faces `specs` (CSS font strings) need for `text`. Safari can leave a `document.fonts.load()`
 * promise pending after its face has loaded, so this also polls `document.fonts.check()`, and it stops
 * waiting after `ms` (a film then draws with the fallback face rather than never starting).
 */
export function loadFonts(specs, text, ms = 8000) {
  if (typeof document === 'undefined' || !document.fonts) return Promise.resolve();
  const loads = Promise.all(specs.map((s) => document.fonts.load(s, text).catch(() => null))).then(() => document.fonts.ready);
  return new Promise((resolve) => {
    const t0 = performance.now();
    const poll = () => {
      let loaded = false;
      try { loaded = document.fonts.status === 'loaded' && specs.every((s) => document.fonts.check(s, text)); } catch { /* a spec it cannot parse */ }
      if (loaded || performance.now() - t0 > ms) resolve();
      else setTimeout(poll, 100);
    };
    loads.then(resolve, resolve);
    setTimeout(poll, 100);
  });
}

/**
 * `requestIdleCallback` for browsers without it (Safari): one slice of about 8 ms per timer tick, with a
 * deadline that really runs out, so a loop that works while `timeRemaining()` lasts gives the page back.
 */
export function idleFallback(cb) {
  return setTimeout(() => {
    const end = performance.now() + 8;
    cb({ didTimeout: false, timeRemaining: () => Math.max(0, end - performance.now()) });
  }, 40);
}

/** The chapter that contains `t`. */
export function chapterAt(film, t) {
  let c = film.chapters[0];
  for (const ch of film.chapters) if (t >= ch.t) c = ch;
  return c;
}

/** The caption showing at `t`, or null. */
export function captionAt(film, t) {
  for (const c of film.captions) {
    if (t >= c.start && t < c.end) return c;
    if (c.start > t) break;
  }
  return null;
}

/**
 * Module -> ready film: run prepare (fonts, data) first, so a film may build its captions or
 * chapters from data inside prepare(), then normalise.
 */
export async function openFilm(mod, opts) {
  const raw = mod && (mod.default || mod.film || mod);
  await prepareFilm(raw, opts);
  return normalizeFilm(raw);
}
