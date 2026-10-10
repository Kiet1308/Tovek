// Export hook: renders any frame of a film at an exact size, and the score to a WAV.
import { filmById } from './films/index.js';
import { createStage, openFilm, renderScoreWav } from './engine/index.js';

const q = new URLSearchParams(location.search);
const id = q.get('film') || 'story';
const W = Math.max(16, Math.round(+q.get('w') || 1920));
const H = Math.max(16, Math.round(+q.get('h') || 1080));
const canvas = document.getElementById('frame');
const status = document.getElementById('status');
const stage = createStage(canvas);
let film = null;

function toBase64(bytes) {
  let s = '';
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) s += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK));
  return btoa(s);
}

window.__ready = (async () => {
  const entry = filmById(id);
  if (!entry || !entry.available) throw new Error(`no film "${id}"`);
  const mod = await import(new URL(entry.src, new URL('./films/', import.meta.url)).href);
  film = await openFilm(mod, { base: location.href });
  stage.setPixels(W, H);
  canvas.style.width = `${W / devicePixelRatio}px`;
  canvas.style.height = `${H / devicePixelRatio}px`;
  stage.draw(film, q.has('t') ? +q.get('t') : film.poster);
  status.textContent = `${film.id} · ${W}×${H} · ${film.duration}s`;
  return { id: film.id, title: film.title, duration: film.duration, chapters: film.chapters, hasScore: !!film.score };
})();

/** Draw the frame at `t` seconds; resolve to a PNG data URL (or nothing with { encode: false }). */
window.__renderFrame = async (t, { encode = 'png' } = {}) => {
  await window.__ready;
  stage.draw(film, t);
  if (!encode) return null;
  return canvas.toDataURL(encode === 'jpeg' ? 'image/jpeg' : 'image/png');
};

/** FNV-1a hash of the frame's pixels at `t`: a cheap way to prove two renders are identical. */
window.__frameHash = async (t) => {
  await window.__ready;
  stage.draw(film, t);
  const data = stage.ctx.getImageData(0, 0, canvas.width, canvas.height).data;
  let h = 0x811c9dc5;
  for (let i = 0; i < data.length; i += 1) h = Math.imul(h ^ data[i], 0x01000193);
  return (h >>> 0).toString(16);
};

/** The whole score as base64 WAV bytes (48 kHz, 16-bit stereo), or null for a silent film. */
window.__renderScore = async ({ from = 0, to } = {}) => {
  await window.__ready;
  if (!film.score) return null;
  const bytes = await renderScoreWav(film.score, { from, to: to ?? film.score.duration });
  return toBase64(bytes);
};
