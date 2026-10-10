// The stage: one canvas, sized in device pixels, onto which a film draws in a fixed 1920×1080
// design space. Films never see device pixels, so the same render is sharp at 390 px wide on a
// phone, at 2× on a laptop, and at 3840 px in an export.

export const DESIGN_W = 1920;
export const DESIGN_H = 1080;

/** Largest backing store we allow (4K UHD), so a huge fullscreen window cannot stall the GPU. */
const MAX_PIXELS = 3840 * 2160;

export function createStage(canvas, { opaque = true } = {}) {
  const ctx = canvas.getContext('2d', { alpha: !opaque });
  const stage = {
    canvas,
    ctx,
    /** Size the backing store for a CSS box at a device pixel ratio. Returns true if it changed. */
    resize(cssW, cssH, dpr = 1) {
      let pw = Math.max(1, Math.round(cssW * dpr));
      let ph = Math.max(1, Math.round(cssH * dpr));
      const over = (pw * ph) / MAX_PIXELS;
      if (over > 1) { const k = Math.sqrt(over); pw = Math.round(pw / k); ph = Math.round(ph / k); }
      return stage.setPixels(pw, ph);
    },
    /** Set the exact backing-store size (the export uses this). */
    setPixels(pw, ph) {
      if (canvas.width === pw && canvas.height === ph) return false;
      canvas.width = pw;
      canvas.height = ph;
      return true;
    },
    /** Draw `film` at time `t`. Every frame starts from a clean context state. */
    draw(film, t) {
      const c = ctx;
      c.setTransform(1, 0, 0, 1, 0, 0);
      c.globalAlpha = 1;
      c.globalCompositeOperation = 'source-over';
      if ('filter' in c) c.filter = 'none';
      c.imageSmoothingEnabled = true;
      c.imageSmoothingQuality = 'high';
      c.fillStyle = film.background || '#0d0d0c';
      c.fillRect(0, 0, canvas.width, canvas.height);
      c.setTransform(canvas.width / DESIGN_W, 0, 0, canvas.height / DESIGN_H, 0, 0);
      c.save();
      film.render(c, Math.max(0, Math.min(film.duration, t)), DESIGN_W, DESIGN_H);
      c.restore();
    },
  };
  return stage;
}

/** Device pixels per design unit for the context's current transform (for 1-device-pixel hairlines). */
export function pixel(ctx) {
  const m = ctx.getTransform();
  return 1 / (Math.hypot(m.a, m.b) || 1);
}
