// A deterministic score. The music is a list of timed events played by a few small synth voices.
// The same list plays in real time in sync with the picture (ScorePlayer) and renders offline to a
// WAV for the file export (renderScoreWav). Noise and the reverb tail come from a seeded generator,
// so the score is a pure schedule. Two offline renders agree to within one least significant bit:
// WebAudio does not fix the order in which it sums parallel voices, so float rounding can differ.
//
//   export const score = defineScore({
//     duration: 18, seed: 26,
//     events: [
//       { t: 0,   voice: 'hum',   dur: 16, freq: note('A1'), gain: 0.2 },
//       { t: 2.0, voice: 'pulse', freq: note('E4') },
//       ...ticks(6.0, 8.5, { rate: 16, seed: 3 }),
//       { t: 14,  voice: 'swell', dur: 4, notes: ['A2', 'E3', 'C#4'].map(note) },
//     ],
//   });
//
// Voices: hum, air, pulse, tick, swell, chime, sub (see VOICES below for their parameters).

import { rng, hash01 } from './random.js';

const NOTE_INDEX = { C: 0, 'C#': 1, Db: 1, D: 2, 'D#': 3, Eb: 3, E: 4, F: 5, 'F#': 6, Gb: 6, G: 7, 'G#': 8, Ab: 8, A: 9, 'A#': 10, Bb: 10, B: 11 };

/** 'A4' -> 440, 'C#3' -> 138.59. Also accepts MIDI numbers. */
export function note(n) {
  if (typeof n === 'number') return 440 * Math.pow(2, (n - 69) / 12);
  const m = /^([A-G][#b]?)(-?\d)$/.exec(n);
  if (!m) throw new Error(`bad note ${n}`);
  const midi = (+m[2] + 1) * 12 + NOTE_INDEX[m[1]];
  return 440 * Math.pow(2, (midi - 69) / 12);
}

/** Keystroke ticks from `start` to `end`: `rate` per second with seeded human jitter. */
export function ticks(start, end, { rate = 14, seed = 1, gain = 0.05, jitter = 0.35 } = {}) {
  const out = [];
  const n = Math.floor((end - start) * rate);
  for (let i = 0; i < n; i++) {
    const t = start + (i + (hash01(seed, i) - 0.5) * jitter) / rate;
    if (hash01(seed, i, 9) < 0.12) continue; // the odd pause
    out.push({ t: Math.max(start, t), voice: 'tick', gain: gain * (0.7 + 0.6 * hash01(seed, i, 2)), tone: hash01(seed, i, 3) });
  }
  return out;
}

const DEFAULT_LENGTH = { tick: 0.08, pulse: 2.2, chime: 3.2, sub: 1.4 };

/** Normalise a score: sort events, fill lengths, keep it frozen. */
export function defineScore({ duration, seed = 1, gain = 0.8, reverb = {}, events = [] }) {
  const list = events
    .map((e, i) => ({ dur: DEFAULT_LENGTH[e.voice] ?? 2, gain: 0.2, pan: 0, ...e, id: i }))
    .sort((a, b) => a.t - b.t || a.id - b.id);
  return Object.freeze({ duration, seed, gain, reverb: { seconds: 3.4, wet: 0.26, ...reverb }, events: list });
}

// ---------------------------------------------------------------------------------------- envelopes

/**
 * Schedule an envelope on `param`. `points` are [time, value] relative to the event start
 * (value 0 is clamped to a floor for exponential segments). `offset` seconds of the event have
 * already passed: the param starts at the envelope's value there, at `when`.
 */
function envelope(param, points, when, offset, curve = 'lin') {
  const floor = 0.0001;
  const valueAt = (x) => {
    if (x <= points[0][0]) return points[0][1];
    for (let i = 1; i < points.length; i++) {
      const [t1, v1] = points[i], [t0, v0] = points[i - 1];
      if (x <= t1) {
        const f = (x - t0) / (t1 - t0 || 1);
        if (curve === 'exp' && v0 > 0 && v1 > 0) return v0 * Math.pow(v1 / v0, f);
        return v0 + (v1 - v0) * f;
      }
    }
    return points[points.length - 1][1];
  };
  param.cancelScheduledValues(when);
  param.setValueAtTime(Math.max(curve === 'exp' ? floor : 0, valueAt(offset)), when);
  let lastAt = when;
  for (const [t, v] of points) {
    if (t <= offset) continue;
    lastAt = when + (t - offset);
    if (curve === 'exp') param.exponentialRampToValueAtTime(Math.max(floor, v), lastAt);
    else param.linearRampToValueAtTime(v, lastAt);
  }
  // An exponential fade cannot reach 0, so land it on exact silence. Otherwise a filter's tail
  // leaks through at 1e-4 until the browser stops processing it, at a render quantum that varies
  // from run to run, and two offline renders would differ in the last bit.
  const last = points[points.length - 1][1];
  if (curve === 'exp' && last <= floor) param.setValueAtTime(0, Math.max(when, lastAt));
}

// ---------------------------------------------------------------------------------------- the bus

const irCache = new Map();
const noiseCache = new Map();

function noiseBuffer(ac, seed) {
  const key = ac.sampleRate + ':' + seed;
  let data = noiseCache.get(key);
  if (!data) {
    const r = rng(seed * 7919 + 13);
    data = new Float32Array(ac.sampleRate * 2);
    for (let i = 0; i < data.length; i++) data[i] = r() * 2 - 1;
    noiseCache.set(key, data);
  }
  const buf = ac.createBuffer(1, data.length, ac.sampleRate);
  buf.copyToChannel(data, 0);
  return buf;
}

function impulse(ac, seed, seconds) {
  const key = `${ac.sampleRate}:${seed}:${seconds}`;
  let chans = irCache.get(key);
  if (!chans) {
    const len = Math.floor(ac.sampleRate * seconds);
    chans = [0, 1].map((c) => {
      const r = rng(seed * 104729 + c * 31 + 1);
      const d = new Float32Array(len);
      let lp = 0;
      for (let i = 0; i < len; i++) {
        const x = i / len;
        // dark, smooth tail: one-pole low-passed noise under an exponential decay
        lp += 0.35 * ((r() * 2 - 1) - lp);
        d[i] = lp * Math.pow(1 - x, 2.2) * (i < ac.sampleRate * 0.012 ? i / (ac.sampleRate * 0.012) : 1);
      }
      return d;
    });
    irCache.set(key, chans);
  }
  const buf = ac.createBuffer(2, chans[0].length, ac.sampleRate);
  buf.copyToChannel(chans[0], 0);
  buf.copyToChannel(chans[1], 1);
  return buf;
}

/** input -> (dry + seeded convolution reverb) -> gentle compressor -> master gain -> destination. */
function buildBus(ac, score) {
  const input = ac.createGain();
  const wet = ac.createGain();
  wet.gain.value = score.reverb.wet;
  const conv = ac.createConvolver();
  conv.normalize = true;
  conv.buffer = impulse(ac, score.seed, score.reverb.seconds);
  const comp = ac.createDynamicsCompressor();
  comp.threshold.value = -18;
  comp.knee.value = 12;
  comp.ratio.value = 3;
  comp.attack.value = 0.01;
  comp.release.value = 0.25;
  const master = ac.createGain();
  master.gain.value = score.gain;
  input.connect(comp);
  input.connect(conv);
  conv.connect(wet);
  wet.connect(comp);
  comp.connect(master);
  master.connect(ac.destination);
  return { input, master, noise: noiseBuffer(ac, score.seed) };
}

// ---------------------------------------------------------------------------------------- voices

function out(ac, bus, ev) {
  const g = ac.createGain();
  g.gain.value = 0;
  let node = g;
  if (ev.pan && ac.createStereoPanner) {
    const p = ac.createStereoPanner();
    p.pan.value = ev.pan;
    g.connect(p);
    node = p;
  }
  node.connect(bus.input);
  return g;
}

function osc(ac, type, freq, detune = 0) {
  const o = ac.createOscillator();
  o.type = type;
  o.frequency.value = freq;
  o.detune.value = detune;
  return o;
}

/**
 * Each voice: (ac, bus, ev, when, offset) -> { out, sources, end } where `end` is the context time
 * at which it falls silent. `offset` > 0 when playback starts in the middle of the event.
 */
export const VOICES = {
  /** A low drone. freq (Hz), dur, attack (2.5), release (2.5), gain. */
  hum(ac, bus, ev, when, offset) {
    const g = out(ac, bus, ev);
    const lp = ac.createBiquadFilter();
    lp.type = 'lowpass';
    lp.frequency.value = ev.cutoff ?? 420;
    lp.Q.value = 0.4;
    lp.connect(g);
    const f = ev.freq ?? 55;
    const srcs = [osc(ac, 'sine', f), osc(ac, 'sine', f * 1.0035), osc(ac, 'triangle', f * 2, 4)];
    const mixes = [0.55, 0.45, 0.12];
    srcs.forEach((o, i) => {
      const m = ac.createGain();
      m.gain.value = mixes[i];
      o.connect(m);
      m.connect(lp);
    });
    const a = ev.attack ?? 2.5, r = ev.release ?? 2.5, d = ev.dur;
    envelope(g.gain, [[0, 0], [a, ev.gain], [Math.max(a, d - r), ev.gain], [d, 0]], when, offset);
    return { out: g, sources: srcs, end: when + (d - offset) };
  },

  /** Filtered noise bed (room tone). dur, gain, cutoff (900), attack/release (3). */
  air(ac, bus, ev, when, offset) {
    const g = out(ac, bus, ev);
    const src = ac.createBufferSource();
    src.buffer = bus.noise;
    src.loop = true;
    const bp = ac.createBiquadFilter();
    bp.type = 'lowpass';
    bp.frequency.value = ev.cutoff ?? 900;
    src.connect(bp);
    bp.connect(g);
    const a = ev.attack ?? 3, r = ev.release ?? 3, d = ev.dur;
    envelope(g.gain, [[0, 0], [a, ev.gain], [Math.max(a, d - r), ev.gain], [d, 0]], when, offset);
    src.loopStart = 0;
    return { out: g, sources: [src], end: when + (d - offset), bufferOffset: offset % 2 };
  },

  /** A soft struck tone, one per release or beat. freq, decay (dur), gain. */
  pulse(ac, bus, ev, when, offset) {
    const g = out(ac, bus, ev);
    const f = ev.freq ?? 220;
    const srcs = [osc(ac, 'sine', f), osc(ac, 'triangle', f * 2.001)];
    const m2 = ac.createGain();
    m2.gain.value = 0.18;
    srcs[0].connect(g);
    srcs[1].connect(m2);
    m2.connect(g);
    const d = ev.dur;
    envelope(g.gain, [[0, 0.0001], [0.012, ev.gain], [d, 0.0001]], when, offset, 'exp');
    return { out: g, sources: srcs, end: when + (d - offset) };
  },

  /** A keystroke: a few ms of band-passed noise. tone (0..1 varies pitch), gain. */
  tick(ac, bus, ev, when, offset) {
    const g = out(ac, bus, ev);
    const src = ac.createBufferSource();
    src.buffer = bus.noise;
    const bp = ac.createBiquadFilter();
    bp.type = 'bandpass';
    bp.frequency.value = 2600 + 2400 * (ev.tone ?? 0.5);
    bp.Q.value = 1.4;
    src.connect(bp);
    bp.connect(g);
    envelope(g.gain, [[0, 0.0001], [0.003, ev.gain], [0.06, 0.0001]], when, offset, 'exp');
    return { out: g, sources: [src], end: when + (0.08 - offset), bufferOffset: (ev.id * 0.0371) % 1.8 };
  },

  /** A slow pad that opens up. notes (Hz[]), dur, attack (3), release (2.5), gain. */
  swell(ac, bus, ev, when, offset) {
    const g = out(ac, bus, ev);
    const lp = ac.createBiquadFilter();
    lp.type = 'lowpass';
    lp.Q.value = 0.7;
    lp.connect(g);
    const a = ev.attack ?? 3, r = ev.release ?? 2.5, d = ev.dur;
    envelope(lp.frequency, [[0, 280], [a, ev.open ?? 2600], [d, 900]], when, offset, 'exp');
    const srcs = [];
    for (const f of ev.notes || [110, 165, 220]) {
      for (const dt of [-7, 0, 6]) {
        const o = osc(ac, 'sawtooth', f, dt);
        const m = ac.createGain();
        m.gain.value = 0.09;
        o.connect(m);
        m.connect(lp);
        srcs.push(o);
      }
    }
    envelope(g.gain, [[0, 0], [a, ev.gain], [Math.max(a, d - r), ev.gain * 0.9], [d, 0]], when, offset);
    return { out: g, sources: srcs, end: when + (d - offset) };
  },

  /** A bell: inharmonic sine partials with their own decays. freq, dur, gain. */
  chime(ac, bus, ev, when, offset) {
    const g = out(ac, bus, ev);
    const f = ev.freq ?? 660;
    const ratios = [1, 2.76, 5.4, 8.93], amps = [1, 0.42, 0.2, 0.08], decays = [1, 0.55, 0.3, 0.18];
    const srcs = ratios.map((k, i) => {
      const o = osc(ac, 'sine', f * k);
      const m = ac.createGain();
      o.connect(m);
      m.connect(g);
      envelope(m.gain, [[0, 0.0001], [0.004, amps[i]], [ev.dur * decays[i], 0.0001]], when, offset, 'exp');
      return o;
    });
    envelope(g.gain, [[0, ev.gain], [ev.dur, ev.gain]], when, offset);
    return { out: g, sources: srcs, end: when + (ev.dur - offset) };
  },

  /** A low thump that falls in pitch. freq (90 -> 38), dur, gain. */
  sub(ac, bus, ev, when, offset) {
    const g = out(ac, bus, ev);
    const o = osc(ac, 'sine', ev.freq ?? 90);
    o.connect(g);
    envelope(o.frequency, [[0, ev.freq ?? 90], [ev.dur * 0.6, ev.to ?? 38]], when, offset, 'exp');
    envelope(g.gain, [[0, 0.0001], [0.02, ev.gain], [ev.dur, 0.0001]], when, offset, 'exp');
    return { out: g, sources: [o], end: when + (ev.dur - offset) };
  },
};

function startVoice(ac, bus, ev, when, offset) {
  const make = VOICES[ev.voice];
  if (!make) return null;
  const v = make(ac, bus, ev, when, offset);
  for (const s of v.sources) {
    if (s instanceof AudioBufferSourceNode) s.start(when, (v.bufferOffset || 0) % (s.buffer.duration - 0.1));
    else s.start(when);
    s.stop(v.end + 0.05);
  }
  return v;
}

// ---------------------------------------------------------------------------------------- realtime

/**
 * Plays a score in step with a film clock. The player calls `update(t)` every frame with the film
 * time; the score keeps a short look-ahead of events scheduled and re-anchors itself if the audio
 * clock drifts from the picture by more than 50 ms.
 */
export class ScorePlayer {
  constructor(score) {
    this.score = score;
    this.ac = null;
    this.bus = null;
    this.live = [];
    this.next = 0;
    this.anchor = null;
    this.volume = 0.9;
    this.muted = false;
    this.lookahead = 0.35;
  }

  get supported() { return typeof AudioContext !== 'undefined'; }

  /** Call from a user gesture (play button / key). */
  async start(t) {
    if (!this.supported) return;
    if (!this.ac) {
      this.ac = new AudioContext({ latencyHint: 'playback' });
      this.bus = buildBus(this.ac, this.score);
      this.applyVolume(true);
    }
    if (this.ac.state === 'suspended') await this.ac.resume();
    this.anchorAt(t);
  }

  anchorAt(t) {
    this.silence();
    const ac = this.ac;
    const when = ac.currentTime + 0.05;
    this.anchor = { film: t, audio: when };
    const evs = this.score.events;
    let i = 0;
    for (; i < evs.length && evs[i].t < t; i++) {
      const ev = evs[i];
      const offset = t - ev.t;
      if (offset < ev.dur - 0.02) this.track(startVoice(ac, this.bus, ev, when, offset));
    }
    this.next = i;
    this.schedule(t);
  }

  schedule(t) {
    const evs = this.score.events;
    while (this.next < evs.length && evs[this.next].t < t + this.lookahead) {
      const ev = evs[this.next++];
      const when = this.anchor.audio + (ev.t - this.anchor.film);
      if (when < this.ac.currentTime) continue;
      this.track(startVoice(this.ac, this.bus, ev, when, 0));
    }
    const now = this.ac.currentTime;
    if (this.live.length > 48) this.live = this.live.filter((v) => v.end > now);
  }

  track(v) { if (v) this.live.push(v); }

  /** Every frame while playing. */
  update(t) {
    if (!this.ac || !this.anchor || this.ac.state !== 'running') return;
    const drift = (this.ac.currentTime - this.anchor.audio) - (t - this.anchor.film);
    if (Math.abs(drift) > 0.05) this.anchorAt(t);
    else this.schedule(t);
  }

  /** Stop everything now, with a 40 ms fade so nothing clicks. */
  silence() {
    if (!this.ac) return;
    const now = this.ac.currentTime;
    for (const v of this.live) {
      try {
        v.out.gain.cancelScheduledValues(now);
        v.out.gain.setValueAtTime(v.out.gain.value, now);
        v.out.gain.linearRampToValueAtTime(0, now + 0.04);
        for (const s of v.sources) s.stop(now + 0.06);
      } catch { /* already stopped */ }
    }
    this.live = [];
    this.anchor = null;
  }

  pause() { this.silence(); }

  setVolume(v) { this.volume = v; this.applyVolume(); }
  setMuted(m) { this.muted = m; this.applyVolume(); }
  applyVolume(immediate = false) {
    if (!this.bus) return;
    const target = this.muted ? 0 : this.score.gain * this.volume * this.volume;
    const g = this.bus.master.gain, now = this.ac.currentTime;
    g.cancelScheduledValues(now);
    if (immediate) g.setValueAtTime(target, now);
    else { g.setValueAtTime(g.value, now); g.linearRampToValueAtTime(target, now + 0.08); }
  }

  dispose() {
    this.silence();
    if (this.ac) this.ac.close();
    this.ac = null;
  }
}

// ---------------------------------------------------------------------------------------- offline

/** Render the whole score to an AudioBuffer with OfflineAudioContext. Deterministic. */
export async function renderScore(score, { sampleRate = 48000, from = 0, to = score.duration } = {}) {
  const len = Math.ceil((to - from) * sampleRate);
  const ac = new OfflineAudioContext(2, len, sampleRate);
  const bus = buildBus(ac, score);
  for (const ev of score.events) {
    if (ev.t + ev.dur <= from || ev.t >= to) continue;
    const offset = Math.max(0, from - ev.t);
    startVoice(ac, bus, ev, Math.max(0, ev.t - from), offset);
  }
  return ac.startRendering();
}

/** 16-bit PCM WAV bytes for an AudioBuffer. */
export function encodeWav(buffer) {
  const ch = buffer.numberOfChannels, sr = buffer.sampleRate, n = buffer.length;
  const bytes = new ArrayBuffer(44 + n * ch * 2);
  const v = new DataView(bytes);
  const str = (o, s) => { for (let i = 0; i < s.length; i++) v.setUint8(o + i, s.charCodeAt(i)); };
  str(0, 'RIFF'); v.setUint32(4, 36 + n * ch * 2, true); str(8, 'WAVE');
  str(12, 'fmt '); v.setUint32(16, 16, true); v.setUint16(20, 1, true); v.setUint16(22, ch, true);
  v.setUint32(24, sr, true); v.setUint32(28, sr * ch * 2, true); v.setUint16(32, ch * 2, true); v.setUint16(34, 16, true);
  str(36, 'data'); v.setUint32(40, n * ch * 2, true);
  const data = [];
  for (let c = 0; c < ch; c++) data.push(buffer.getChannelData(c));
  let o = 44;
  for (let i = 0; i < n; i++) {
    for (let c = 0; c < ch; c++) {
      const s = Math.max(-1, Math.min(1, data[c][i]));
      v.setInt16(o, s < 0 ? Math.round(s * 32768) : Math.round(s * 32767), true);
      o += 2;
    }
  }
  return new Uint8Array(bytes);
}

/** Score -> WAV bytes, the export path. */
export async function renderScoreWav(score, opts) {
  return encodeWav(await renderScore(score, opts));
}
