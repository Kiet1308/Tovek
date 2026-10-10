# Tovek film engine

Films on `docs/watch/` are drawn live from source, one frame at a time, on a canvas. A film is a
plain ES module. The player owns only a clock; the film turns a time `t` into a picture.

```
docs/watch/
  index.html, watch.css, watch.js   the cinema page (player UI)
  export.html, export.js            frame-exact export hook (PNG frames + WAV score)
  engine/                           this folder: helpers a film imports from engine/index.js
  films/index.js                    the film registry (which films exist, their reel labels)
  films/<id>.js                     one module per film
  data/                             film data written by scripts (never hand-typed numbers)
```

## The rule: a frame is a pure function of `t`

- `render(ctx, t, w, h)` may read only `t`, its prepared data and constants. No state carried from
  the previous frame, no `Date.now()`, no `Math.random()`. Use `hash01(seed, i, ...)` for
  per-item randomness and `noise1(x, seed)` for smooth wandering.
- Caches are fine (layouts, diffs, measured text): they are keyed by their inputs and never change
  what a frame looks like.
- Then scrubbing is exact, frames can be drawn out of order, at any size, and exported.

## The film module

```js
// films/story.js
import { timeline, sequence, font, layoutText, reveal, ease, V26 } from '../engine/index.js';

const H1 = font(140, { family: 'display', weight: 650, stretch: 87.5 });
let data = null;

const scenes = timeline(sequence([
  { id: 'open',  dur: 8,  in: 0,   out: 1.2, draw: (ctx, s) => {
      reveal(ctx, layoutText('It began as medal.', H1), 960, 560, s.local, { unit: 'word', start: 0.6, align: 'center' });
  } },
  { id: 'beta',  dur: 10, in: 1.2, out: 1.0, draw: drawBeta },   // starts 1.2 s before 'open' ends: a crossfade
]));

export default {
  id: 'story',                          // must match films/index.js
  title: 'One function, seventeen releases',
  description: 'The same function, decompiled by every Tovek release since medal.',
  duration: scenes.duration,            // seconds
  poster: 21.5,                         // frame shown behind the play button (default 1)
  posterTitle: true,                    // false hides the page's title block over the poster
  background: V26.night,                // cleared before every frame
  fonts: [H1.css],                      // every font spec the film draws with (preloaded first)
  glyphs: 'Tovek 0123456789 →·—',       // optional: characters to preload in those fonts
  chapters: [{ t: 0, title: 'It began as medal', still: 4 }, { t: 18, title: 'Four days, seven betas' }],
  captions: [{ start: 0.6, end: 4.2, text: 'It began as medal.' }],
  score: undefined,                     // optional, see "Score"

  async prepare({ base }) {             // optional, runs once before the first frame
    data = await (await fetch(new URL('../data/story.json', import.meta.url))).json();
  },
  render(ctx, t, w, h) { scenes.render(ctx, t, w, h); },
};
```

`prepare()` runs before the player reads `chapters` and `captions`, so a film may build them from
data there (the demo does this for its captions).

Register the film in `films/index.js` (`available: true`). The page then offers it as a reel, and
`watch/?film=<id>` opens it. Unlisted films (`listed: false`) open only by URL.

## Design space

- The engine scales the context so a film always draws in **1920 × 1080 design units**; `w` and
  `h` are those numbers. The canvas itself is sized in device pixels (CSS size × devicePixelRatio,
  capped at 4K), so text and lines stay sharp at any size. Never read `ctx.canvas.width` to lay
  things out.
- For a hairline exactly one device pixel thick: `ctx.fillRect(x, y, w, pixel(ctx))`.
- **Title-safe area**: keep text inside x 96–1824, y 54–972. When the viewer moves the mouse, the
  controls cover the bottom ~12% of the frame; captions sit just above them.
- Captions are DOM text over the stage (crisp, translatable, toggled with C). Do not burn the same
  words into the frame.

## Helpers (all exported from `engine/index.js`)

### Time and easing (`ease.js`, `tween.js`)

| helper | what it does |
|---|---|
| `ease.out`, `ease.inOut` | the same curves as `--ease-out` / `--ease-in-out` in `assets/v26/tokens.css` |
| `ease.glide`, `ease.snap`, `ease.outBack`, `ease.inOutCubic`, … | the rest of the set; `cubicBezier(x1, y1, x2, y2)` for your own |
| `progress(t, start, dur, fn)` | 0..1 for a beat, clamped and eased. The workhorse |
| `tween(t, start, dur, from, to, fn)` | a number over a beat |
| `envelope(t, start, end, fadeIn, fadeOut)` | 0 → 1 → 0 trapezoid, for things that come and go |
| `keyframes([[t, v, ease?], …])(t)` | a track of numbers or arrays |
| `stagger(i, count, spread, { from })` | delay for item `i` (`from`: 'start', 'end', 'center' or an index) |
| `spring(t, { freq, damping })` | closed-form damped spring 0 → 1, exact at any `t` |
| `clamp`, `lerp`, `invLerp`, `remap`, `smoothstep`, `timecode` | the usual |

### Scenes (`timeline.js`)

- `sequence(list, { start, overlap })` lays scenes end to end; each scene overlaps the previous by
  its own `in` (so the fade-in is a crossfade) unless it sets `overlap` or an absolute `at`.
- `timeline(scenes)` returns `{ render, duration, scenes, active(t), info(id, t) }`.
- A scene: `{ id, at, dur, in, out, z, fade, draw(ctx, s) }`. `s` carries `t`, `local`, `p`,
  `enter`, `exit`, `alpha`, `w`, `h`. Unless `fade: false`, `globalAlpha` is already multiplied by
  `s.alpha`, so a scene fades in and out for free.
- `fade(ctx, a, fn)` multiplies alpha for a block; `wipe(ctx, p, rect, dir, fn)` clips a reveal.

### Type (`text.js`)

- `font(size, { family: 'display' | 'text' | 'mono', weight, stretch })`. Families are Bricolage
  Grotesque, Geist and Geist Mono. `stretch` takes 75 / 87.5 / 100 (canvas only knows those steps).
- `layoutText(text, font, { maxWidth, lineHeight })` measures once and caches: lines, words, glyph
  pen positions (kerning kept). `y` arguments are the **first baseline**.
- `drawText(ctx, L, x, y, { color, align, tracking, alpha })`; `tracking` is in em and may be
  animated (it costs nothing extra).
- `reveal(ctx, L, x, y, t, opts)` kinetic type: `unit` 'glyph' | 'word' | 'line', `start`,
  `stagger`, `dur`, `ease`, `rise`, `mask` (each line clipped so units rise from under it),
  `trackingFrom` (tracking settles as units land), `out: { start, stagger, dur }` to send them
  away, `colorOf(i)` to tint one unit (the accent, for meaning only). `revealEnd()` tells when it lands.
- `typewriter(ctx, L, x, y, count, { caret, caretAlpha })`, `decode(ctx, L, x, y, t, { charset, seed })`
  (glyphs flicker through hex until they settle), `fitSize(text, font, maxWidth)`.

### Numbers (`counter.js`)

- `countTo(t, { start, dur, from, to, ease })` → number; `formatNumber(n, { decimals })`.
- `drawCounter(ctx, value, x, y, font, { align, decimals, prefix, suffix })` draws digits in
  fixed-width cells so a counting number never jitters.
- `drawOdometer(ctx, t, { from, to, start, dur, x, y, font, align, turns })` rolls each digit
  column (backwards when counting down), settling left to right. Returns `{ width }`.

### Code (`code.js`)

- `layoutCode(src, { tabSize })` tokenizes Luau and lays it on a character grid (cached per text).
  Tokens carry `line`, `col` and `parts` (multi-line strings and comments). Helpers on the layout:
  `find(text | regex, line?)` → Set of token indices, `lineTokens(from, to)`, `lineOf(regex)`,
  `colsIn(from, to)`.
- `drawCode(ctx, L, { x, y, size, lineHeight, palette, highlight, highlightMix, wash, chars, lines, lineAlpha, gutter })`.
  Tones are near-greyscale (`codePalettes.night` / `.paper`; names brightest, punctuation dimmest).
  `highlight` (a Set from `find`, or a predicate) is the **only** use of the accent: what Tovek
  recovered. `chars` types the code on; `lineAlpha(l)` dims or reveals line by line.
- `codeMetrics(size, lineHeight)` → `{ cw, lh, baseline }`; `codeSize(L, size)` → `{ w, h }`.

### The token morph (`morph.js`), the signature move

```js
const plan = codeMorph(v251Source, v26Source);      // LCS over tokens, cached per pair
drawMorph(ctx, plan, progress(t, 40, 5), {
  box: { x: 150, y: 140, w: 1010, h: 800 },          // the code is fitted into this box
  size: 26, lineHeight: 1.55, palette: 'night',
  linesA: [0, plan.a.lineCount - 1],                 // what the camera frames on each side
  linesB: [30, 52],                                  // (here: push in on the part that changed)
  clip: true,
});
// afterwards hold B with the same tokens lit:
withCamera(ctx, fitCamera(plan.b, box, { size, lineHeight, lines: [30, 52] }), () =>
  drawCode(ctx, plan.b, { size, lineHeight, highlight: plan.inserted }));
```

At `p = 0` the morph draws exactly A, at `p = 1` exactly B (inserted tokens in the accent, so a
following `drawCode(..., { highlight: plan.inserted })` continues seamlessly). In between:

- tokens both versions share glide from their old cell to their new one, rippling down the lines
  (`wave`);
- tokens only A has fade, drift up and soften (`blur`; drawn as offset copies, never `ctx.filter`, which
  would change how the canvas rasterises later frames and break exact scrubbing);
- tokens only B type in, run by run, in the accent (`inserted: 'base'` or `insertedMix` to settle it;
  `highlight: Set | (i) => bool` gives the accent only to the inserted B tokens that show what was
  recovered, and the rest type in in their base tone);
- a camera eases from the fit of A to the fit of B (`fit`, `align`, `maxScale`, `linesA`/`linesB`).

`timing: { out: [0, .42], move: [.12, .84], in: [.5, 1] }` sets the three phases inside 0..1.
`plan.stats` gives `{ kept, removed, inserted, linesA, linesB }`. An 89 → 76 line morph costs about
1–3 ms a frame on a laptop.

### Score (`score.js`)

```js
score: defineScore({
  duration: 135, seed: 17, reverb: { seconds: 3.6, wet: 0.3 },
  events: [
    { t: 0,  voice: 'air',   dur: 135, gain: 0.03 },
    { t: 0,  voice: 'hum',   dur: 30,  freq: note('A1'), gain: 0.16 },
    { t: 18, voice: 'pulse', freq: note('E3'), gain: 0.14 },      // one per release
    ...ticks(24, 30, { rate: 16, seed: 3 }),                      // keystrokes while code types
    { t: 95, voice: 'swell', dur: 8, notes: ['A2', 'E3', 'C#4'].map(note), gain: 0.12 },
  ],
}),
```

Voices: `hum` (drone), `air` (room tone), `pulse` (soft struck tone), `tick` (keystroke), `swell`
(opening pad), `chime` (bell), `sub` (low thump). The player plays the schedule in sync with the
clock (look-ahead scheduling, re-anchored on seek or drift); audio starts only when the viewer
presses play and has its own mute and volume. The export renders the same schedule with
`OfflineAudioContext` (`renderScoreWav`). Offline renders agree to within one least significant bit:
WebAudio does not fix the order in which it sums parallel voices.

### Also

- `drawMark(ctx, x, y, size, { color, body, bits })`: the Tovek mark; `bits` 0..1 lands its eight bits.
- `V26` holds the identity colours (`night`, `onNight`, `onNight2`, `paper`, `ink`, `signal`, …),
  `rgba(hex, a)` and `mix(a, b, p)` (OKLab).
- `createStage(canvas, { software })`, `openFilm(module)`, `chapterAt`, `captionAt`: what the
  player and export use.

## Export

```
python D:/Medal/v22-work/v26/export/export_film.py --film story --out story.mp4            # 1080p60
python D:/Medal/v22-work/v26/export/export_film.py --film story --w 3840 --h 2160 --workers 6
python D:/Medal/v22-work/v26/export/export_film.py --film demo --start 8.6 --end 11.6 --keep-frames
```

It serves `docs/`, opens `export.html?film=…&w=…&h=…` in several headless Edge pages, renders the
frames out of order (PNG), renders the score to WAV and muxes H.264 (CRF 16, yuv420p, BT.709) +
AAC with ffmpeg. `export.html?film=…&t=12.5` shows one frame; it exposes `window.__ready`,
`window.__renderFrame(t)` (PNG data URL), `window.__frameHash(t)` and `window.__renderScore()`.
The export canvas is CPU-backed, so the same `t` gives the same pixels on every run.

## Player keys

Space or K play/pause · ← → 5 s · , . one frame (paused) · 0–9 jump to 0–90% · F full screen ·
M mute · C captions · Home/End and PageUp/PageDown on the scrubber.
`watch/?film=<id>&t=42` opens paused at 42 s. `&stats` records frame times in `window.__watchStats`.
