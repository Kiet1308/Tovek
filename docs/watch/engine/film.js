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
  if (document.fonts) {
    // the sample text pulls in every subset a film is likely to draw (digits, arrows, dashes, quotes)
    const sample = film.glyphs || 'Tovek 0123456789 →←↓·—–…“”‘’×−%#{}[]';
    await Promise.all((film.fonts || []).map((spec) => document.fonts.load(spec, sample).catch(() => null)));
    await document.fonts.ready;
  }
  if (typeof film.prepare === 'function') await film.prepare({ base });
  return film;
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
