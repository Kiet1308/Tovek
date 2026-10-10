// Scenes and timelines. A film is usually a list of scenes, each alive for a window of time, that
// overlap to crossfade. The composer finds the live scenes for `t`, works out each one's local
// time and envelope, and calls its `draw`. No state is kept between frames.
//
//   const film = timeline(sequence([
//     { id: 'hook',  dur: 6,  in: 0,   out: 1.2, draw: drawHook },
//     { id: 'bytes', dur: 9,  in: 1.2, out: 1.0, draw: drawBytes },   // starts 1.2 s before hook ends
//   ]));
//   export default { ..., duration: film.duration, render: film.render };

import { clamp } from './tween.js';
import { ease as E } from './ease.js';

/**
 * Lay scenes end to end. Each scene's `at` is the previous scene's end minus the overlap. The
 * overlap is the scene's own `overlap`, else its `in` (so the incoming fade is a crossfade), else
 * the `overlap` option. Scenes that already have `at` keep it and reset the cursor.
 */
export function sequence(list, { start = 0, overlap = 0 } = {}) {
  let cursor = start;
  return list.map((sc, i) => {
    let at;
    if (sc.at != null) at = sc.at;
    else {
      const ov = i === 0 ? 0 : sc.overlap ?? sc.in ?? overlap;
      at = cursor - ov;
    }
    cursor = at + sc.dur;
    return { ...sc, at };
  });
}

/**
 * Build a renderer from scenes `{ id, at, dur, in?, out?, z?, fade?, easeIn?, easeOut?, draw }`.
 *
 * `draw(ctx, s)` receives:
 *   s.t      film time (s)           s.local  seconds since the scene started
 *   s.dur    scene length            s.p      local / dur, 0..1
 *   s.enter  0..1 across the `in`    s.exit   0..1 across the `out` (0 until it starts)
 *   s.alpha  enter·(1 − exit)        s.w, s.h design size
 * Unless `fade: false`, ctx.globalAlpha is already multiplied by s.alpha.
 */
export function timeline(scenes) {
  const list = scenes
    .map((sc, order) => ({
      in: 0, out: 0, z: 0, fade: true, easeIn: E.inOut, easeOut: E.inOut, ...sc, order,
    }))
    .sort((a, b) => a.z - b.z || a.at - b.at || a.order - b.order);
  const duration = list.reduce((m, sc) => Math.max(m, sc.at + sc.dur), 0);

  function info(sc, t, w, h) {
    const local = t - sc.at;
    const enter = sc.in > 0 ? sc.easeIn(clamp(local / sc.in)) : 1;
    const exit = sc.out > 0 ? sc.easeOut(clamp((local - (sc.dur - sc.out)) / sc.out)) : 0;
    return { id: sc.id, t, local, dur: sc.dur, p: clamp(local / sc.dur), enter, exit, alpha: enter * (1 - exit), w, h };
  }

  function active(t) {
    return list.filter((sc) => t >= sc.at && t < sc.at + sc.dur);
  }

  function render(ctx, t, w, h) {
    // Hold the last frame at (and after) the end instead of going blank.
    const tt = t >= duration ? duration - 1e-6 : t;
    for (const sc of list) {
      if (tt < sc.at || tt >= sc.at + sc.dur) continue;
      const s = info(sc, tt, w, h);
      if (sc.fade && s.alpha <= 0) continue;
      ctx.save();
      if (sc.fade) ctx.globalAlpha *= s.alpha;
      sc.draw(ctx, s);
      ctx.restore();
    }
  }

  return { scenes: list, duration, render, active, info: (id, t, w = 1920, h = 1080) => {
    const sc = list.find((x) => x.id === id);
    return sc ? info(sc, t, w, h) : null;
  } };
}

/** Draw `fn` with the current alpha multiplied by `a` (skips the call when `a` is 0). */
export function fade(ctx, a, fn) {
  if (a <= 0) return;
  ctx.save();
  ctx.globalAlpha *= Math.min(1, a);
  fn();
  ctx.restore();
}

/** Draw `fn` clipped to a rectangle that grows with `p` from one edge: 'left' | 'right' | 'up' | 'down'. */
export function wipe(ctx, p, rect, dir, fn) {
  if (p <= 0) return;
  const { x, y, w, h } = rect;
  ctx.save();
  ctx.beginPath();
  if (dir === 'left') ctx.rect(x, y, w * p, h);
  else if (dir === 'right') ctx.rect(x + w * (1 - p), y, w * p, h);
  else if (dir === 'down') ctx.rect(x, y, w, h * p);
  else ctx.rect(x, y + h * (1 - p), w, h * p);
  ctx.clip();
  fn();
  ctx.restore();
}
