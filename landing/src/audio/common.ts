/* Shared pieces for the Snack Radio stations: every sound is synthesised,
   nothing is downloaded. A station owns its mix and schedules one bar at a
   time; the player (engine.ts) decides when. */

export interface Station {
  /** Seconds in one bar. */
  readonly bar: number;
  /** Times of scheduled kicks, for the 3D scene to jump on. */
  kicks: number[];
  scheduleBar(n: number, t: number): void;
}

export const midi = (n: number) => 440 * Math.pow(2, (n - 69) / 12);

export function rng(seed: number) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let x = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    x = (x + Math.imul(x ^ (x >>> 7), 61 | x)) ^ x;
    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}

export function noiseBuffer(ctx: BaseAudioContext, seconds = 2) {
  const b = ctx.createBuffer(1, Math.floor(ctx.sampleRate * seconds), ctx.sampleRate);
  const d = b.getChannelData(0);
  for (let i = 0; i < d.length; i++) d[i] = Math.random() * 2 - 1;
  return b;
}

export function room(ctx: BaseAudioContext, seconds: number, decay: number) {
  const conv = ctx.createConvolver();
  const len = Math.floor(ctx.sampleRate * seconds);
  const ir = ctx.createBuffer(2, len, ctx.sampleRate);
  for (let ch = 0; ch < 2; ch++) {
    const d = ir.getChannelData(ch);
    for (let i = 0; i < len; i++) d[i] = (Math.random() * 2 - 1) * Math.pow(1 - i / len, decay);
  }
  conv.buffer = ir;
  return conv;
}

/** The end of every station's chain: a gentle compressor and a trim. */
export function master(ctx: BaseAudioContext, destination: AudioNode, level = 0.5) {
  const out = ctx.createGain();
  out.gain.value = level;
  const comp = ctx.createDynamicsCompressor();
  comp.threshold.value = -16;
  comp.ratio.value = 4;
  comp.attack.value = 0.005;
  comp.release.value = 0.18;
  out.connect(comp).connect(destination);
  return out;
}

/** Drums every station can share. */
export class Kit {
  constructor(private ctx: BaseAudioContext, private noise: AudioBuffer, private out: AudioNode, private onKick?: (t: number) => void) {}

  kick(t: number, vel = 1, from = 150, to = 48, len = 0.42) {
    const c = this.ctx;
    const o = c.createOscillator();
    const a = c.createGain();
    o.frequency.setValueAtTime(from, t);
    o.frequency.exponentialRampToValueAtTime(to, t + 0.09);
    a.gain.setValueAtTime(1.15 * vel, t);
    a.gain.exponentialRampToValueAtTime(0.001, t + len);
    o.connect(a).connect(this.out);
    o.start(t);
    o.stop(t + len + 0.02);
    // the click at the front of a club kick
    this.noiseHit(t, 0.012, "highpass", 3000, 0.7, 0.35 * vel);
    this.onKick?.(t);
  }

  noiseHit(t: number, dur: number, type: BiquadFilterType, freq: number, q: number, vel: number, dest: AudioNode = this.out) {
    const c = this.ctx;
    const src = c.createBufferSource();
    src.buffer = this.noise;
    const f = c.createBiquadFilter();
    f.type = type;
    f.frequency.value = freq;
    f.Q.value = q;
    const a = c.createGain();
    a.gain.setValueAtTime(vel, t);
    a.gain.exponentialRampToValueAtTime(0.001, t + dur);
    src.connect(f).connect(a).connect(dest);
    src.start(t, Math.random() * 1.5);
    src.stop(t + dur + 0.02);
  }

  clap(t: number, vel = 1, dest: AudioNode = this.out) {
    for (let k = 0; k < 3; k++) this.noiseHit(t + k * 0.011, 0.02, "bandpass", 1300, 1.2, 0.5 * vel, dest);
    this.noiseHit(t + 0.03, 0.22, "bandpass", 1200, 0.9, 0.42 * vel, dest);
  }

  snare(t: number, vel = 1) {
    this.noiseHit(t, 0.18, "bandpass", 2200, 0.7, 0.5 * vel);
    const c = this.ctx;
    const o = c.createOscillator();
    const a = c.createGain();
    o.type = "triangle";
    o.frequency.setValueAtTime(220, t);
    o.frequency.exponentialRampToValueAtTime(160, t + 0.07);
    a.gain.setValueAtTime(0.32 * vel, t);
    a.gain.exponentialRampToValueAtTime(0.001, t + 0.1);
    o.connect(a).connect(this.out);
    o.start(t);
    o.stop(t + 0.12);
  }

  hat(t: number, vel: number, open = false) {
    this.noiseHit(t, open ? 0.24 : 0.04, "highpass", 8000, 0.6, 0.2 * vel);
  }

  crash(t: number, vel = 1) {
    this.noiseHit(t, 1.8, "highpass", 5000, 0.4, 0.22 * vel);
  }
}

/** A saw stack, detuned for width: the supersaw of every early-2000s track. */
export function supersaw(ctx: BaseAudioContext, dest: AudioNode, t: number, note: number, dur: number, vel: number, voices = 7, spread = 22, attack = 0.01, release = 0.25) {
  const a = ctx.createGain();
  a.gain.setValueAtTime(0, t);
  a.gain.linearRampToValueAtTime(vel / voices, t + attack);
  a.gain.setTargetAtTime(0.0001, t + dur, release / 3);
  a.connect(dest);
  for (let v = 0; v < voices; v++) {
    const o = ctx.createOscillator();
    o.type = "sawtooth";
    o.frequency.value = midi(note);
    o.detune.value = ((v / (voices - 1)) * 2 - 1) * spread;
    const p = ctx.createStereoPanner();
    p.pan.value = ((v / (voices - 1)) * 2 - 1) * 0.7;
    o.connect(p).connect(a);
    o.start(t);
    o.stop(t + dur + release + 0.1);
  }
}

/** A short plucked note: a bright wave through a closing filter. */
export function pluck(ctx: BaseAudioContext, dest: AudioNode, t: number, note: number, vel: number, type: OscillatorType = "square", cutoff = 5000, len = 0.22) {
  const o = ctx.createOscillator();
  o.type = type;
  o.frequency.value = midi(note);
  const f = ctx.createBiquadFilter();
  f.type = "lowpass";
  f.Q.value = 6;
  f.frequency.setValueAtTime(cutoff, t);
  f.frequency.exponentialRampToValueAtTime(300, t + len);
  const a = ctx.createGain();
  a.gain.setValueAtTime(vel, t);
  a.gain.exponentialRampToValueAtTime(0.001, t + len + 0.05);
  o.connect(f).connect(a).connect(dest);
  o.start(t);
  o.stop(t + len + 0.1);
}

/** Pumping: the bus drops on each kick and swells back, the dance-music breath. */
export function duck(bus: GainNode, base: number, t: number, depth = 0.6, back = 0.22) {
  bus.gain.setTargetAtTime(base * (1 - depth), t, 0.005);
  bus.gain.setTargetAtTime(base, t + 0.03, back / 3);
}
