/* ---------------------------------------------------------------------------
   Snack Radio: "Snack Break", a lo-fi loop written as code.

   Nothing is downloaded. Every sound is synthesised with the Web Audio API:
   an electric piano from two sine waves (one bending the other, which gives
   the bell at the start of each note), a round bass, boom-bap drums made from
   a falling sine and filtered noise, vinyl crackle, and a whistled melody that
   is composed anew every sixteen bars from the notes of the chords under it.

   90 bpm, lazy swing. Sixteen bars:
     A  Fmaj9  Em7   Dm9   Cmaj9   (twice)
     B  Am7    Dm9   G13   Cmaj9   Fmaj9  Fm6  Em7  A7
   The first time round it starts with piano alone, the drums come in on bar
   5 and the melody on bar 9; every fourth time round the drums sit out a
   verse, so it breathes. Every fourth bar ends on a little drum fill, the B
   part picks up a shaker and ghost notes on the snare, and the bass walks
   into each next chord from a half step away.

   --------------------------------------------------------------------------- */

import type { Station } from "./common";

const BPM = 90;
const BEAT = 60 / BPM;
const BAR = BEAT * 4;
const SWING = 0.6; // where the off-beat eighth lands, as a share of the beat
const KEYS_LEVEL = 0.62;
const BASS_LEVEL = 0.26;

type Chord = { bass: number; voicing: number[]; tones: number[] };
const midi = (n: number) => 440 * Math.pow(2, (n - 69) / 12);

// voicings sit around middle C; `tones` are the notes a melody may lean on
const CH: Record<string, Chord> = {
  Fmaj9: { bass: 41, voicing: [57, 60, 64, 67], tones: [65, 69, 72, 76, 79, 67, 74] },
  Em7: { bass: 40, voicing: [55, 59, 62, 64], tones: [64, 67, 71, 74, 76, 79] },
  Dm9: { bass: 38, voicing: [53, 57, 60, 64], tones: [62, 65, 69, 72, 74, 76, 77] },
  Cmaj9: { bass: 36, voicing: [52, 55, 59, 62], tones: [64, 67, 71, 72, 74, 76, 79] },
  Am7: { bass: 45, voicing: [55, 57, 60, 64], tones: [64, 67, 69, 72, 76, 79, 81] },
  G13: { bass: 43, voicing: [53, 59, 64, 69], tones: [67, 71, 74, 76, 77, 79] },
  Fm6: { bass: 41, voicing: [56, 60, 62, 65], tones: [65, 68, 72, 74, 77] },
  A7: { bass: 45, voicing: [55, 61, 64, 69], tones: [67, 69, 73, 76, 79] },
};
const FORM = ["Fmaj9", "Em7", "Dm9", "Cmaj9", "Fmaj9", "Em7", "Dm9", "Cmaj9", "Am7", "Dm9", "G13", "Cmaj9", "Fmaj9", "Fm6", "Em7", "A7"];

function rng(seed: number) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let x = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    x = (x + Math.imul(x ^ (x >>> 7), 61 | x)) ^ x;
    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}

export class LoFi implements Station {
  readonly bar = BAR;
  readonly out: GainNode;
  private noise: AudioBuffer;
  private keys: GainNode;
  private bass: GainNode;
  private drums: GainNode;
  private mel: GainNode;
  private wobble: GainNode;
  private melody: Array<{ at: number; len: number; note: number }[]> = [];
  kicks: number[] = [];

  constructor(private ctx: BaseAudioContext, destination: AudioNode) {
    const c = ctx;
    this.noise = c.createBuffer(1, c.sampleRate * 2, c.sampleRate);
    const data = this.noise.getChannelData(0);
    for (let i = 0; i < data.length; i++) data[i] = Math.random() * 2 - 1;

    // mix: every bus into a gentle "tape" filter and a compressor
    this.out = c.createGain();
    this.out.gain.value = 0.5;
    const tape = c.createBiquadFilter();
    tape.type = "lowpass";
    tape.frequency.value = 9000;
    tape.Q.value = 0.4;
    const comp = c.createDynamicsCompressor();
    comp.threshold.value = -20;
    comp.ratio.value = 3;
    comp.attack.value = 0.01;
    comp.release.value = 0.2;
    const trim = c.createGain();
    trim.gain.value = 0.9;
    this.out.connect(tape).connect(comp).connect(trim).connect(destination);

    // a small room, from decaying noise
    const verb = c.createConvolver();
    const len = Math.floor(c.sampleRate * 2.4);
    const ir = c.createBuffer(2, len, c.sampleRate);
    for (let ch = 0; ch < 2; ch++) {
      const d = ir.getChannelData(ch);
      for (let i = 0; i < len; i++) d[i] = (Math.random() * 2 - 1) * Math.pow(1 - i / len, 3.2);
    }
    verb.buffer = ir;
    const verbIn = c.createGain();
    verbIn.gain.value = 0.26;
    verbIn.connect(verb).connect(this.out);

    this.keys = c.createGain();
    this.keys.gain.value = KEYS_LEVEL;
    const keysTone = c.createBiquadFilter();
    keysTone.type = "lowpass";
    keysTone.frequency.value = 2600;
    const trem = c.createGain();
    const tremLfo = c.createOscillator();
    const tremDepth = c.createGain();
    tremLfo.frequency.value = 4.3;
    tremDepth.gain.value = 0.1;
    tremLfo.connect(tremDepth).connect(trem.gain);
    trem.gain.value = 0.9;
    tremLfo.start();
    this.keys.connect(keysTone).connect(trem);
    trem.connect(this.out);
    trem.connect(verbIn);

    // a slow pitch drift shared by the piano: the "old tape" feeling
    const wob = c.createOscillator();
    wob.frequency.value = 0.31;
    this.wobble = c.createGain();
    this.wobble.gain.value = 6; // cents
    wob.connect(this.wobble);
    wob.start();

    this.bass = c.createGain();
    this.bass.gain.value = BASS_LEVEL;
    const bassTone = c.createBiquadFilter();
    bassTone.type = "lowpass";
    bassTone.frequency.value = 420;
    this.bass.connect(bassTone).connect(this.out);

    this.drums = c.createGain();
    this.drums.gain.value = 0.42;
    this.drums.connect(this.out);

    this.mel = c.createGain();
    this.mel.gain.value = 0.17;
    const echo = c.createDelay(2);
    echo.delayTime.value = BEAT * 0.75;
    const fb = c.createGain();
    fb.gain.value = 0.32;
    const echoTone = c.createBiquadFilter();
    echoTone.type = "lowpass";
    echoTone.frequency.value = 2400;
    this.mel.connect(this.out);
    this.mel.connect(verbIn);
    this.mel.connect(echo).connect(echoTone).connect(fb).connect(echo);
    echoTone.connect(this.out);

    // vinyl: a bed of filtered hiss
    const hiss = c.createBufferSource();
    hiss.buffer = this.noise;
    hiss.loop = true;
    const hp = c.createBiquadFilter();
    hp.type = "bandpass";
    hp.frequency.value = 3000;
    hp.Q.value = 0.5;
    const hissGain = c.createGain();
    hissGain.gain.value = 0.012;
    hiss.connect(hp).connect(hissGain).connect(this.out);
    hiss.start();
  }

  /* ---- instruments */
  private piano(t: number, note: number, dur: number, vel: number, pan: number) {
    const c = this.ctx;
    const f = midi(note);
    const car = c.createOscillator();
    const mod = c.createOscillator();
    const modGain = c.createGain();
    const amp = c.createGain();
    const p = c.createStereoPanner();
    car.frequency.value = f;
    mod.frequency.value = f;
    modGain.gain.setValueAtTime(f * 2.2 * vel, t);
    modGain.gain.exponentialRampToValueAtTime(f * 0.15 + 1, t + 0.35);
    mod.connect(modGain).connect(car.frequency);
    this.wobble.connect(car.detune);
    amp.gain.setValueAtTime(0, t);
    amp.gain.linearRampToValueAtTime(0.28 * vel, t + 0.006);
    amp.gain.exponentialRampToValueAtTime(0.1 * vel, t + 0.9);
    amp.gain.setTargetAtTime(0.0001, t + dur, 0.28);
    p.pan.value = pan;
    car.connect(amp).connect(p).connect(this.keys);
    // a quiet octave partial for shine
    const hi = c.createOscillator();
    const hiAmp = c.createGain();
    hi.type = "triangle";
    hi.frequency.value = f * 2;
    hiAmp.gain.setValueAtTime(0.04 * vel, t);
    hiAmp.gain.exponentialRampToValueAtTime(0.0001, t + 0.6);
    hi.connect(hiAmp).connect(p);
    const end = t + dur + 1.6;
    for (const o of [car, mod, hi]) { o.start(t); o.stop(end); }
  }

  private bassNote(t: number, note: number, dur: number, vel = 1) {
    const c = this.ctx;
    const o = c.createOscillator();
    const sub = c.createOscillator();
    const amp = c.createGain();
    o.type = "triangle";
    o.frequency.value = midi(note);
    sub.frequency.value = midi(note);
    amp.gain.setValueAtTime(0, t);
    amp.gain.linearRampToValueAtTime(0.9 * vel, t + 0.012);
    amp.gain.setTargetAtTime(0.5 * vel, t + 0.05, 0.2);
    amp.gain.setTargetAtTime(0.0001, t + dur, 0.06);
    o.connect(amp);
    sub.connect(amp);
    amp.connect(this.bass);
    for (const x of [o, sub]) { x.start(t); x.stop(t + dur + 0.5); }
  }

  private kick(t: number, vel = 1) {
    const c = this.ctx;
    const o = c.createOscillator();
    const amp = c.createGain();
    o.frequency.setValueAtTime(140, t);
    o.frequency.exponentialRampToValueAtTime(46, t + 0.12);
    amp.gain.setValueAtTime(1.1 * vel, t);
    amp.gain.exponentialRampToValueAtTime(0.001, t + 0.42);
    o.connect(amp).connect(this.drums);
    o.start(t);
    o.stop(t + 0.45);
    // the piano and the bass lean back a little on every kick
    for (const [bus, base] of [[this.keys, KEYS_LEVEL], [this.bass, BASS_LEVEL]] as const) {
      bus.gain.setTargetAtTime(base * 0.72, t, 0.01);
      bus.gain.setTargetAtTime(base, t + 0.06, 0.12);
    }
    this.kicks.push(t);
  }

  private noiseHit(t: number, dur: number, type: BiquadFilterType, freq: number, q: number, vel: number, dest: AudioNode = this.drums) {
    const c = this.ctx;
    const src = c.createBufferSource();
    src.buffer = this.noise;
    const f = c.createBiquadFilter();
    f.type = type;
    f.frequency.value = freq;
    f.Q.value = q;
    const amp = c.createGain();
    amp.gain.setValueAtTime(vel, t);
    amp.gain.exponentialRampToValueAtTime(0.001, t + dur);
    src.connect(f).connect(amp).connect(dest);
    src.start(t, Math.random() * 1.5);
    src.stop(t + dur + 0.02);
  }

  private snare(t: number, vel = 1) {
    this.noiseHit(t, 0.2, "bandpass", 1900, 0.7, 0.55 * vel);
    const c = this.ctx;
    const o = c.createOscillator();
    const amp = c.createGain();
    o.type = "triangle";
    o.frequency.setValueAtTime(200, t);
    o.frequency.exponentialRampToValueAtTime(150, t + 0.08);
    amp.gain.setValueAtTime(0.35 * vel, t);
    amp.gain.exponentialRampToValueAtTime(0.001, t + 0.12);
    o.connect(amp).connect(this.drums);
    o.start(t);
    o.stop(t + 0.14);
  }

  private shaker(t: number, vel: number) {
    this.noiseHit(t, 0.06, "bandpass", 6200, 1.4, 0.1 * vel);
  }

  private hat(t: number, vel: number, open = false) {
    this.noiseHit(t, open ? 0.28 : 0.05, "highpass", 7600, 0.6, 0.22 * vel);
  }

  private crackle(t: number) {
    this.noiseHit(t, 0.004 + Math.random() * 0.01, "highpass", 2500, 0.5, 0.05 + Math.random() * 0.08, this.out);
  }

  private whistle(t: number, note: number, dur: number) {
    const c = this.ctx;
    const f = midi(note);
    const o = c.createOscillator();
    const o2 = c.createOscillator();
    const amp = c.createGain();
    const vib = c.createOscillator();
    const vibDepth = c.createGain();
    o.type = "sine";
    o2.type = "triangle";
    o.frequency.value = f;
    o2.frequency.value = f * 2;
    vib.frequency.value = 5.4;
    vibDepth.gain.setValueAtTime(0, t);
    vibDepth.gain.linearRampToValueAtTime(f * 0.006, t + Math.min(0.25, dur));
    vib.connect(vibDepth);
    vibDepth.connect(o.frequency);
    vibDepth.connect(o2.frequency);
    const o2Amp = c.createGain();
    o2Amp.gain.value = 0.12;
    amp.gain.setValueAtTime(0, t);
    amp.gain.linearRampToValueAtTime(1, t + 0.035);
    amp.gain.setTargetAtTime(0.7, t + 0.06, 0.2);
    amp.gain.setTargetAtTime(0.0001, t + dur, 0.07);
    o.connect(amp);
    o2.connect(o2Amp).connect(amp);
    amp.connect(this.mel);
    for (const x of [o, o2, vib]) { x.start(t); x.stop(t + dur + 0.5); }
  }

  /* ---- composition */
  private compose(cycle: number) {
    const r = rng(0x5eed + cycle * 977);
    const bars: { at: number; len: number; note: number }[][] = [];
    let motif: { at: number; len: number; step: number }[] = [];
    let prev = 72;
    for (let b = 0; b < 16; b++) {
      const chord = CH[FORM[b]];
      const notes: { at: number; len: number; note: number }[] = [];
      // Every other bar answers the one before it with the same rhythm.
      if (b % 2 === 0 || !motif.length) {
        motif = [];
        const slots = [0, 0.5, 1, 1.5, 2, 2.5, 3, 3.5];
        let busy = 0;
        for (const s of slots) {
          const density = b % 4 === 3 ? 0.25 : 0.5;
          if (r() < density && busy <= s) {
            const len = [0.5, 0.5, 1, 1, 1.5][Math.floor(r() * 5)];
            motif.push({ at: s, len, step: Math.floor(r() * 5) - 2 });
            busy = s + len;
          }
        }
        if (!motif.length) motif.push({ at: 0.5, len: 1.5, step: 0 });
      }
      for (const m of motif) {
        // walk from the last note toward a chord tone, a step or two away
        const pool = chord.tones.filter((n) => Math.abs(n - prev) <= 7);
        const choices = pool.length ? pool : chord.tones;
        const sorted = [...choices].sort((x, y) => Math.abs(x - (prev + m.step * 2)) - Math.abs(y - (prev + m.step * 2)));
        const note = sorted[Math.min(sorted.length - 1, Math.floor(r() * 2))];
        prev = note;
        notes.push({ at: m.at, len: Math.min(m.len, 4 - m.at), note });
      }
      bars.push(notes);
    }
    return bars;
  }

  /** Schedules bar number `bar` (counted from the start) at time `t`. */
  scheduleBar(bar: number, t: number) {
    const cycle = Math.floor(bar / 16);
    const b = bar % 16;
    if (b === 0 || !this.melody.length) this.melody = this.compose(cycle);
    const r = rng(bar * 131 + 7);
    const human = () => (r() - 0.5) * 0.018;
    const swing = (beatPos: number) => {
      const whole = Math.floor(beatPos);
      const frac = beatPos - whole;
      return (whole + (frac === 0.5 ? SWING : frac)) * BEAT;
    };
    const chord = CH[FORM[b]];
    const next = CH[FORM[(b + 1) % 16]];
    const firstTime = cycle === 0;
    const fill = b % 4 === 3;
    const partB = b >= 8;
    const drumsOn = !(firstTime && b < 4) && !(cycle % 4 === 3 && b >= 8 && b < 12);
    const melodyOn = !(firstTime && b < 8);

    // piano: a long chord on one, a short push on the "and" of two, sometimes a ghost on four
    const spread = (i: number) => (i - 1.5) * 0.18;
    chord.voicing.forEach((n, i) => this.piano(t + i * 0.012 + human(), n, BEAT * 1.4, 0.8 + r() * 0.15, spread(i)));
    chord.voicing.forEach((n, i) => this.piano(t + swing(1.5) + i * 0.01 + human(), n, BEAT * 0.55, 0.55 + r() * 0.1, spread(i)));
    if (r() < 0.45) chord.voicing.slice(1).forEach((n, i) => this.piano(t + swing(3.5) + human(), n, BEAT * 0.4, 0.4, spread(i + 1)));

    // bass
    if (drumsOn || !firstTime) {
      this.bassNote(t + human(), chord.bass, BEAT * 1.2);
      this.bassNote(t + swing(1.5) + human(), chord.bass, BEAT * 0.4, 0.7);
      this.bassNote(t + swing(2) + human(), chord.bass + (r() < 0.5 ? 7 : 12), BEAT * 0.9, 0.8);
      if (r() < 0.6) this.bassNote(t + swing(3.5) + human(), chord.bass + 12, BEAT * 0.35, 0.6);
      // a half step below or above the next chord's root, walking into it
      if (next.bass !== chord.bass && r() < 0.7) this.bassNote(t + BEAT * 3.75 + human(), next.bass + (r() < 0.5 ? -1 : 1), BEAT * 0.22, 0.65);
    }

    // drums: boom, bap, the kick that drags, and swung hats
    if (drumsOn) {
      this.kick(t + human());
      if (r() < 0.5) this.kick(t + swing(1.5) + human(), 0.7);
      this.kick(t + swing(2.5) + human(), 0.9);
      this.snare(t + BEAT + 0.012 + human());
      if (!fill) this.snare(t + BEAT * 3 + 0.014 + human());
      // ghost notes: the snare barely touched, between the real hits
      if (partB || r() < 0.35) {
        if (r() < 0.7) this.snare(t + BEAT * 1.75 + human(), 0.18);
        if (r() < 0.5) this.snare(t + BEAT * 2.25 + human(), 0.14);
      }
      for (let e = 0; e < (fill ? 6 : 8); e++) {
        const pos = e / 2;
        const open = e === 7 && r() < 0.3;
        this.hat(t + swing(pos) + human(), e % 2 ? 0.6 + r() * 0.2 : 0.9, open);
      }
      if (partB) for (let q = 0; q < 16; q++) this.shaker(t + q * (BEAT / 4) + (q % 2 ? BEAT * 0.06 : 0) + human(), q % 4 === 2 ? 1 : 0.5);
      // the fill: the last beat of every fourth bar, a different one each time
      if (fill) {
        const kind = b === 15 ? 2 : Math.floor(r() * 3);
        const at = (x: number) => t + BEAT * 3 + x * (BEAT / 4) + human();
        if (kind === 0) [0, 1, 2, 3].forEach((x) => this.snare(at(x), 0.35 + x * 0.18));
        else if (kind === 1) { this.snare(at(0), 0.8); this.kick(at(1.5), 0.7); this.snare(at(2), 0.45); this.snare(at(3), 0.9); }
        else { [0, 0.5, 1, 1.5, 2, 2.5, 3].forEach((x, k) => this.snare(at(x), 0.2 + k * 0.11)); this.hat(at(3.5), 1, true); }
      }
      // the top of each time round: a soft cymbal
      if (b === 0 && !firstTime) this.noiseHit(t, 1.4, "highpass", 6500, 0.4, 0.09);
    }

    // melody
    if (melodyOn) for (const n of this.melody[b]) this.whistle(t + swing(n.at) + human(), n.note, n.len * BEAT * 0.92);

    // vinyl crackle, a few pops a second
    const pops = 4 + Math.floor(r() * 6);
    for (let i = 0; i < pops; i++) this.crackle(t + r() * BAR);

    // forget kicks that have long gone by
    this.kicks = this.kicks.filter((k) => k > t - 2);
  }
}
