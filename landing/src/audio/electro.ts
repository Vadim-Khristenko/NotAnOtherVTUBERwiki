/* "Night Shift at the Snack Factory": 126 bpm, F minor, the machines hum.

   Kick on every beat, a clap on two and four, sixteenth hats with a little
   shuffle. The pad breathes with the kick, a sub bass rolls on the off
   beats, and after the first verse a plucked arpeggio bounces across the
   stereo field through a ping-pong echo. Every sixteen bars the hats drop
   out, a noise riser climbs, and the next bar lands with a crash. Every
   other time round the B part breaks down: the kick leaves, the arpeggio
   and the pad play alone, a snare roll builds, and it all drops back in.
   Every fourth bar ends on a fill; a rim ticks off-beat patterns that change
   each time round, and the bass rolls in sixteenths in the B part.

     A  Fm   Db   Ab   Eb     (twice)
     B  Bbm  Fm   Db   C      (twice)
*/
import { Kit, duck, master, midi, noiseBuffer, pluck, rng, room, type Station } from "./common";

const BPM = 126;
const BEAT = 60 / BPM;
const CH = {
  Fm: { bass: 29, pad: [53, 56, 60, 63], arp: [65, 68, 72, 75, 77] },
  Db: { bass: 25, pad: [53, 56, 61, 65], arp: [61, 65, 68, 72, 73] },
  Ab: { bass: 32, pad: [51, 56, 60, 63], arp: [63, 68, 72, 75, 80] },
  Eb: { bass: 27, pad: [51, 55, 58, 62], arp: [63, 67, 70, 74, 75] },
  Bbm: { bass: 34, pad: [53, 58, 61, 65], arp: [65, 70, 73, 77, 82] },
  C: { bass: 24, pad: [52, 55, 60, 64], arp: [64, 67, 72, 76, 79] },
} as const;
type Name = keyof typeof CH;
const FORM: Name[] = ["Fm", "Db", "Ab", "Eb", "Fm", "Db", "Ab", "Eb", "Bbm", "Fm", "Db", "C", "Bbm", "Fm", "Db", "C"];
const PAD = 0.3, BASS = 0.36, ARP = 0.2;

export class Electro implements Station {
  readonly bar = BEAT * 4;
  kicks: number[] = [];
  private kit: Kit;
  private pad: GainNode;
  private bass: GainNode;
  private arp: GainNode;
  private padFilter: BiquadFilterNode;
  private noise: AudioBuffer;

  constructor(private ctx: BaseAudioContext, destination: AudioNode) {
    const c = ctx;
    this.noise = noiseBuffer(c);
    const out = master(c, destination, 0.5);
    const verb = room(c, 2.2, 3);
    const send = c.createGain();
    send.gain.value = 0.3;
    send.connect(verb).connect(out);

    const drums = c.createGain();
    drums.gain.value = 0.62;
    drums.connect(out);
    this.kit = new Kit(c, this.noise, drums, (t) => {
      this.kicks.push(t);
      duck(this.pad, PAD, t, 0.75, 0.3);
      duck(this.bass, BASS, t, 0.55, 0.18);
    });

    this.pad = c.createGain();
    this.pad.gain.value = PAD;
    this.padFilter = c.createBiquadFilter();
    this.padFilter.type = "lowpass";
    this.padFilter.frequency.value = 900;
    this.padFilter.Q.value = 2;
    // the filter opens and closes slowly: the factory breathing
    const lfo = c.createOscillator();
    const depth = c.createGain();
    lfo.frequency.value = 0.07;
    depth.gain.value = 700;
    lfo.connect(depth).connect(this.padFilter.frequency);
    lfo.start();
    this.pad.connect(this.padFilter).connect(out);
    this.padFilter.connect(send);

    this.bass = c.createGain();
    this.bass.gain.value = BASS;
    const bassTone = c.createBiquadFilter();
    bassTone.type = "lowpass";
    bassTone.frequency.value = 260;
    this.bass.connect(bassTone).connect(out);

    // ping-pong: left echo feeds the right one and back
    this.arp = c.createGain();
    this.arp.gain.value = ARP;
    const dl = c.createDelay(1), dr = c.createDelay(1);
    dl.delayTime.value = dr.delayTime.value = BEAT * 0.75;
    const fb = c.createGain();
    fb.gain.value = 0.38;
    const pl = c.createStereoPanner(), pr = c.createStereoPanner();
    pl.pan.value = -0.8;
    pr.pan.value = 0.8;
    const tone = c.createBiquadFilter();
    tone.type = "lowpass";
    tone.frequency.value = 3200;
    this.arp.connect(out);
    this.arp.connect(dl);
    dl.connect(pl).connect(out);
    dl.connect(tone).connect(dr);
    dr.connect(pr).connect(out);
    dr.connect(fb).connect(dl);
    this.arp.connect(send);
  }

  private padChord(t: number, notes: readonly number[], dur: number) {
    const c = this.ctx;
    for (const n of notes) {
      for (const detune of [-9, 0, 9]) {
        const o = c.createOscillator();
        o.type = "sawtooth";
        o.frequency.value = midi(n);
        o.detune.value = detune;
        const a = c.createGain();
        a.gain.setValueAtTime(0, t);
        a.gain.linearRampToValueAtTime(0.09, t + 0.35);
        a.gain.setTargetAtTime(0.0001, t + dur, 0.25);
        o.connect(a).connect(this.pad);
        o.start(t);
        o.stop(t + dur + 1.2);
      }
    }
  }

  private sub(t: number, note: number, dur: number) {
    const c = this.ctx;
    const o = c.createOscillator();
    o.frequency.value = midi(note);
    const a = c.createGain();
    a.gain.setValueAtTime(0, t);
    a.gain.linearRampToValueAtTime(0.9, t + 0.01);
    a.gain.setTargetAtTime(0.0001, t + dur, 0.03);
    o.connect(a).connect(this.bass);
    o.start(t);
    o.stop(t + dur + 0.2);
  }

  private riser(t: number, len: number) {
    const c = this.ctx;
    const src = c.createBufferSource();
    src.buffer = this.noise;
    src.loop = true;
    const f = c.createBiquadFilter();
    f.type = "bandpass";
    f.Q.value = 4;
    f.frequency.setValueAtTime(400, t);
    f.frequency.exponentialRampToValueAtTime(9000, t + len);
    const a = c.createGain();
    a.gain.setValueAtTime(0.001, t);
    a.gain.exponentialRampToValueAtTime(0.25, t + len);
    a.gain.setValueAtTime(0.0001, t + len + 0.01);
    src.connect(f).connect(a).connect(this.arp);
    src.start(t);
    src.stop(t + len + 0.05);
  }

  scheduleBar(n: number, t: number) {
    const cycle = Math.floor(n / 16);
    const b = n % 16;
    const ch = CH[FORM[b]];
    const r = rng(n * 7919 + 3);
    const first = cycle === 0;
    const breakdown = cycle % 2 === 1 && b >= 8 && b < 12;
    const build = b === 15 || (breakdown && b === 11);
    const fill = b % 4 === 3 && !build;
    const partB = b >= 8;
    // the rim pattern, new each time round
    const rim = rng(cycle * 31 + 5);
    const rims = Array.from({ length: 16 }, (_, i) => i % 4 !== 0 && rim() < 0.28);
    const s16 = (i: number) => t + i * (BEAT / 4) + (i % 2 ? BEAT * 0.02 : 0);

    // drums
    if (breakdown) {
      // no kick: the claps thin out, then a roll climbs into the drop
      if (b === 11) for (let i = 0; i < 16; i++) this.kit.snare(s16(i), 0.15 + i * 0.05);
      else { this.kit.clap(t + BEAT * 3, 0.6); for (let i = 2; i < 16; i += 4) this.kit.hat(s16(i), 0.6, true); }
    } else if (!(first && b < 2)) {
      for (let q = 0; q < 4; q++) this.kit.kick(t + q * BEAT, q === 0 ? 1 : 0.92);
      this.kit.clap(t + BEAT, 0.9);
      if (!fill) this.kit.clap(t + BEAT * 3, 0.9);
      if (!build) for (let i = 0; i < (fill ? 12 : 16); i++) this.kit.hat(s16(i), i % 4 === 2 ? 0.85 : 0.35 + r() * 0.25, i % 4 === 2 && r() < 0.5);
      else for (let i = 8; i < 16; i++) this.kit.clap(s16(i), 0.25 + (i - 8) * 0.06);
      if (!first || b >= 4) for (let i = 0; i < 16; i++) if (rims[i] && !(fill && i >= 12)) this.kit.noiseHit(s16(i), 0.03, "bandpass", 3400, 3, 0.32);
      if (fill) {
        const kind = Math.floor(r() * 3);
        if (kind === 0) for (let i = 12; i < 16; i++) this.kit.snare(s16(i), 0.4 + (i - 12) * 0.18);
        else if (kind === 1) { this.kit.clap(s16(12), 0.9); this.kit.clap(s16(14), 0.7); this.kit.clap(s16(15), 0.95); }
        else for (let i = 12; i < 16; i++) this.kit.noiseHit(s16(i), 0.09, "bandpass", 900 + (i - 12) * 500, 2, 0.5);
      }
      if ((b === 0 && !first) || (b === 12 && cycle % 2 === 1)) this.kit.crash(t);
    }
    // pad, one chord a bar
    this.padChord(t, ch.pad, this.bar * 0.95);
    // sub bass on the off beats, with a push into the next bar
    if (!(first && b < 4) && !breakdown) {
      if (partB && !first) {
        // the rolling bass: three sixteenths after every kick
        for (let q = 0; q < 4; q++) for (const x of [1, 2, 3]) this.sub(t + q * BEAT + x * (BEAT / 4), ch.bass + (x === 3 && r() < 0.3 ? 24 : 12), BEAT * 0.2);
      } else {
        for (let q = 0; q < 4; q++) this.sub(t + q * BEAT + BEAT / 2, ch.bass + 12, BEAT * 0.4);
        if (r() < 0.5) this.sub(t + BEAT * 3.75, ch.bass + 24, BEAT * 0.2);
      }
    }
    // the arpeggio: up and down the chord in sixteenths, a pattern per cycle
    if (!(first && b < 8)) {
      const order = [0, 2, 1, 3, 2, 4, 3, 1];
      for (let i = 0; i < 16; i++) {
        if (r() < 0.12) continue;
        const note = ch.arp[order[(i + cycle) % order.length] % ch.arp.length];
        pluck(this.ctx, this.arp, s16(i), note, i % 4 === 0 ? 0.9 : 0.6, "sawtooth", 2200 + (b % 8) * 450, 0.18);
      }
    }
    if (build) this.riser(t, this.bar);
    this.kicks = this.kicks.filter((k) => k > t - 2);
  }
}
