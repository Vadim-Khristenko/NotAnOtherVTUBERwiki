/* "Dial-Up Hearts (2003)": the eurotrance that came out of a friend's CD
   burner. 140 bpm, A minor, the four chords everybody knows.

   Kick on every beat, the bass on every off beat, a supersaw gated in
   sixteenths, an arpeggio of square waves, and after the break a lead with
   a hook you could have set as a ringtone. It starts filtered, as if played
   through the wall of the next room, and opens up; every sixteen bars a
   snare roll builds to a crash. Every other time round there is a
   breakdown: the kick leaves, the chords hold and swell, a roll climbs,
   and the drop comes back with a reversed cymbal sucked in front of it.
   Every fourth bar ends on a short fill.

     Am  F  C  G   (four times, the lead joins on the third)
*/
import { Kit, duck, master, midi, noiseBuffer, pluck, rng, room, supersaw, type Station } from "./common";

const BPM = 140;
const BEAT = 60 / BPM;
const CH = [
  { bass: 33, saw: [57, 60, 64], arp: [69, 72, 76, 81] },
  { bass: 29, saw: [57, 60, 65], arp: [65, 69, 72, 77] },
  { bass: 36, saw: [55, 60, 64], arp: [67, 72, 76, 79] },
  { bass: 31, saw: [55, 59, 62], arp: [67, 71, 74, 79] },
];
// the hook, one bar per chord: [sixteenth, length in sixteenths, note]
const HOOK: [number, number, number][][] = [
  [[0, 3, 76], [3, 3, 74], [6, 2, 72], [8, 4, 76], [12, 4, 79]],
  [[0, 3, 77], [3, 3, 76], [6, 2, 74], [8, 6, 72]],
  [[0, 3, 76], [3, 3, 74], [6, 2, 72], [8, 4, 71], [12, 4, 72]],
  [[0, 6, 74], [6, 2, 72], [8, 8, 71]],
];
const SAW = 0.55, BASS = 0.38;

export class Past implements Station {
  readonly bar = BEAT * 4;
  kicks: number[] = [];
  private kit: Kit;
  private saws: GainNode;
  private bassBus: GainNode;
  private arp: GainNode;
  private lead: GainNode;
  private tone: BiquadFilterNode;

  constructor(private ctx: BaseAudioContext, destination: AudioNode) {
    const c = ctx;
    const noise = noiseBuffer(c);
    const out = master(c, destination, 0.52);
    // the whole mix goes through one filter, for the "next room" intro
    this.tone = c.createBiquadFilter();
    this.tone.type = "lowpass";
    this.tone.frequency.value = 20000;
    this.tone.connect(out);
    const verb = room(c, 2.2, 2.6);
    const send = c.createGain();
    send.gain.value = 0.34;
    send.connect(verb).connect(this.tone);

    const drums = c.createGain();
    drums.gain.value = 0.6;
    drums.connect(this.tone);
    this.kit = new Kit(c, noise, drums, (t) => {
      this.kicks.push(t);
      duck(this.saws, SAW, t, 0.5, 0.2);
      duck(this.bassBus, BASS, t, 0.8, 0.12);
    });

    this.saws = c.createGain();
    this.saws.gain.value = SAW;
    const sawHp = c.createBiquadFilter();
    sawHp.type = "highpass";
    sawHp.frequency.value = 220;
    this.saws.connect(sawHp).connect(this.tone);
    sawHp.connect(send);

    this.bassBus = c.createGain();
    this.bassBus.gain.value = BASS;
    const bassLp = c.createBiquadFilter();
    bassLp.type = "lowpass";
    bassLp.frequency.value = 700;
    bassLp.Q.value = 4;
    this.bassBus.connect(bassLp).connect(this.tone);

    this.arp = c.createGain();
    this.arp.gain.value = 0.22;
    const echo = c.createDelay(1);
    echo.delayTime.value = BEAT * 0.75;
    const fb = c.createGain();
    fb.gain.value = 0.35;
    this.arp.connect(this.tone);
    this.arp.connect(echo).connect(fb).connect(echo);
    fb.connect(this.tone);

    this.lead = c.createGain();
    this.lead.gain.value = 0.3;
    this.lead.connect(this.tone);
    this.lead.connect(send);
  }

  private swell(t: number, len: number) {
    const c = this.ctx;
    const src = c.createBufferSource();
    src.buffer = noiseBuffer(c, 1);
    src.loop = true;
    const f = c.createBiquadFilter();
    f.type = "highpass";
    f.frequency.value = 4500;
    const a = c.createGain();
    a.gain.setValueAtTime(0.001, t);
    a.gain.exponentialRampToValueAtTime(0.2, t + len);
    a.gain.setValueAtTime(0, t + len + 0.005);
    src.connect(f).connect(a).connect(this.tone);
    src.start(t);
    src.stop(t + len + 0.02);
  }

  private bassNote(t: number, note: number) {
    const c = this.ctx;
    const o = c.createOscillator();
    o.type = "sawtooth";
    o.frequency.value = midi(note);
    const a = c.createGain();
    a.gain.setValueAtTime(0.9, t);
    a.gain.exponentialRampToValueAtTime(0.001, t + BEAT * 0.45);
    o.connect(a).connect(this.bassBus);
    o.start(t);
    o.stop(t + BEAT * 0.5);
  }

  private leadNote(t: number, note: number, dur: number, from?: number) {
    const c = this.ctx;
    const a = c.createGain();
    a.gain.setValueAtTime(0, t);
    a.gain.linearRampToValueAtTime(0.5, t + 0.01);
    a.gain.setTargetAtTime(0.3, t + 0.05, 0.1);
    a.gain.setTargetAtTime(0.0001, t + dur, 0.05);
    const vib = c.createOscillator();
    const vd = c.createGain();
    vib.frequency.value = 5.6;
    vd.gain.setValueAtTime(0, t);
    vd.gain.linearRampToValueAtTime(12, t + Math.min(dur, 0.3));
    vib.connect(vd);
    for (const det of [-8, 8]) {
      const o = c.createOscillator();
      o.type = "sawtooth";
      o.detune.value = det;
      // a little glide from the previous note: portamento, very 2003
      o.frequency.setValueAtTime(midi(from ?? note), t);
      o.frequency.exponentialRampToValueAtTime(midi(note), t + 0.05);
      vd.connect(o.detune);
      o.connect(a);
      o.start(t);
      o.stop(t + dur + 0.3);
    }
    vib.start(t);
    vib.stop(t + dur + 0.3);
    a.connect(this.lead);
  }

  scheduleBar(n: number, t: number) {
    const cycle = Math.floor(n / 16);
    const b = n % 16;
    const ch = CH[b % 4];
    const r = rng(n * 104729 + 11);
    const s16 = (i: number) => t + i * (BEAT / 4);
    const first = cycle === 0;

    // the intro opens the filter over its first eight bars
    if (first && b < 8) {
      this.tone.frequency.setValueAtTime(400 * Math.pow(40, b / 8), t);
      this.tone.frequency.exponentialRampToValueAtTime(400 * Math.pow(40, (b + 1) / 8), t + this.bar);
    } else if (first && b === 8) this.tone.frequency.setValueAtTime(20000, t);

    const breakdown = cycle % 2 === 1 && b >= 8 && b < 12;
    const roll = b === 15 || (breakdown && b === 11);
    const fill = b % 4 === 3 && !roll;
    for (let q = 0; q < 4; q++) {
      if (!breakdown && !(roll && q >= 2)) this.kit.kick(t + q * BEAT, 1, 170, 45, 0.3);
      if (!breakdown) this.bassNote(t + q * BEAT + BEAT / 2, ch.bass + 12);
      if (!breakdown || b === 10) this.kit.hat(t + q * BEAT + BEAT / 2, 0.9, true);
      // closed hats on the sixteenths around it once the track is going
      if (!breakdown && !first) { this.kit.hat(s16(q * 4 + 1), 0.35); this.kit.hat(s16(q * 4 + 3), 0.45); }
    }
    if (roll) for (let i = 0; i < 16; i++) this.kit.snare(s16(i), 0.25 + i * 0.045);
    else if (!breakdown) {
      this.kit.clap(t + BEAT, 0.8);
      if (!fill) this.kit.clap(t + BEAT * 3, 0.8);
      else if (r() < 0.5) [12, 13, 14, 15].forEach((i, k) => this.kit.snare(s16(i), 0.45 + k * 0.15));
      else { this.kit.snare(s16(12), 0.8); this.kit.snare(s16(14), 0.6); this.kit.snare(s16(15), 0.9); }
    }
    if ((b === 0 && !first) || (b === 12 && cycle % 2 === 1)) this.kit.crash(t);
    // a reversed cymbal, sucked in before the drop
    if (roll) this.swell(t + this.bar - BEAT * 2, BEAT * 2);

    // supersaw chords, gated in sixteenths (the trance gate)
    // One held chord per bar, chopped by automating its level: cheap enough for a phone.
    const gate = [1, 0, 1, 1, 0, 1, 1, 0, 1, 0, 1, 1, 0, 1, 1, 1];
    const g = this.ctx.createGain();
    g.gain.setValueAtTime(0, t);
    if (breakdown) {
      // in the breakdown the chord is held and swells instead of chopping
      g.gain.linearRampToValueAtTime(0.35 + (b - 8) * 0.12, t + this.bar * 0.9);
    } else for (let i = 0; i < 16; i++) {
      g.gain.setValueAtTime(gate[i] ? 1 : 0, s16(i));
      if (gate[i]) g.gain.setTargetAtTime(0.25, s16(i) + 0.01, BEAT / 12);
    }
    g.gain.setValueAtTime(0, t + this.bar);
    g.connect(this.saws);
    for (const nn of ch.saw) supersaw(this.ctx, g, t, nn + 12, this.bar, 0.55, 5, 20, 0.003, 0.02);

    // the square-wave arpeggio
    if (!(first && b < 4)) for (let i = 0; i < 16; i++) pluck(this.ctx, this.arp, s16(i), ch.arp[(i * 3 + (i >> 2)) % 4], i % 2 ? 0.5 : 0.8, "square", 3800, 0.12);

    // the hook, from the third time round the four chords, with the odd note varied
    if (b >= 8 || !first) {
      let prev: number | undefined;
      for (const [at, len, note] of HOOK[b % 4]) {
        const nudged = r() < 0.12 ? note + (r() < 0.5 ? 2 : -1) : note;
        this.leadNote(s16(at), nudged, len * (BEAT / 4) * 0.95, prev);
        prev = nudged;
      }
    }
    this.kicks = this.kicks.filter((k) => k > t - 2);
  }
}

/** The dial-up handshake, a second and a half of it, for switching to this station. */
export function dialUp(ctx: BaseAudioContext, dest: AudioNode, t: number) {
  const tone = (f: number, from: number, len: number, vol = 0.12, type: OscillatorType = "sine") => {
    const o = ctx.createOscillator();
    o.type = type;
    o.frequency.value = f;
    const a = ctx.createGain();
    a.gain.setValueAtTime(vol, t + from);
    a.gain.setValueAtTime(0, t + from + len);
    o.connect(a).connect(dest);
    o.start(t + from);
    o.stop(t + from + len + 0.02);
  };
  tone(350, 0, 0.35); tone(440, 0, 0.35); // dial tone
  [941, 1336, 697, 1209, 852, 1477].forEach((f, i) => tone(f, 0.38 + Math.floor(i / 2) * 0.09, 0.07, 0.1)); // the number
  tone(2100, 0.7, 0.35, 0.08); // answer
  tone(1200, 1.05, 0.2, 0.08, "square"); tone(2400, 1.05, 0.2, 0.05, "square");
  const noise = noiseBuffer(ctx, 1);
  const src = ctx.createBufferSource();
  src.buffer = noise;
  const bp = ctx.createBiquadFilter();
  bp.type = "bandpass";
  bp.frequency.value = 1800;
  const a = ctx.createGain();
  a.gain.setValueAtTime(0.15, t + 1.25);
  a.gain.exponentialRampToValueAtTime(0.001, t + 1.6);
  src.connect(bp).connect(a).connect(dest);
  src.start(t + 1.25);
  src.stop(t + 1.62);
}
