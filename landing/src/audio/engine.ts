/* The Snack Radio player: one audio context, three stations, scheduled a
   bar ahead so timing never depends on the page being busy. It draws the
   oscilloscope in the cassette's window and leaves the levels in
   window.snackAudio, where the 3D world reads them every frame. */
import type { Station } from "./common";
import { noiseBuffer } from "./common";
import { LoFi } from "./lofi";
import { Electro } from "./electro";
import { Past, dialUp } from "./past";

export type StationId = "lofi" | "electro" | "past";
const make: Record<StationId, (ctx: BaseAudioContext, out: AudioNode) => Station> = {
  lofi: (c, o) => new LoFi(c, o),
  electro: (c, o) => new Electro(c, o),
  past: (c, o) => new Past(c, o),
};

export type Levels = { bass: number; mid: number; high: number; kick: number };

export function createPlayer() {
  const Ctx = window.AudioContext || (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
  const ctx = new Ctx();
  const volume = ctx.createGain();
  volume.gain.value = 0;
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 2048;
  analyser.smoothingTimeConstant = 0.78;
  volume.connect(analyser).connect(ctx.destination);

  let level = 0.6;
  let on = false;
  let id: StationId = "lofi";
  let bus: GainNode | null = null;
  let station: Station | null = null;
  let bar = 0;
  let nextBar = 0;
  let timer = 0;
  const shared: Levels = { bass: 0, mid: 0, high: 0, kick: 0 };
  (window as unknown as { snackAudio: Levels }).snackAudio = shared;

  /* A station gets its own bus, so the old one can fade out while the new one starts. */
  function tune(next: StationId, when = ctx.currentTime + 0.05) {
    const old = bus;
    if (old) {
      old.gain.setTargetAtTime(0, when, 0.12);
      setTimeout(() => old.disconnect(), 1500);
    }
    id = next;
    bus = ctx.createGain();
    bus.connect(volume);
    station = make[next](ctx, bus);
    bar = 0;
    nextBar = when;
  }

  /* Between stations: static, or for Past the sound of a modem dialling in. */
  function static_(t: number, len = 0.7) {
    const src = ctx.createBufferSource();
    src.buffer = noiseBuffer(ctx, len + 0.1);
    const bp = ctx.createBiquadFilter();
    bp.type = "bandpass";
    bp.Q.value = 3;
    bp.frequency.setValueAtTime(300, t);
    bp.frequency.exponentialRampToValueAtTime(6000, t + len);
    const a = ctx.createGain();
    a.gain.setValueAtTime(0.12, t);
    a.gain.exponentialRampToValueAtTime(0.001, t + len);
    src.connect(bp).connect(a).connect(volume);
    src.start(t);
    src.stop(t + len + 0.05);
  }

  const pump = () => {
    if (!station) return;
    while (nextBar < ctx.currentTime + 0.4) {
      station.scheduleBar(bar++, nextBar);
      nextBar += station.bar;
    }
  };

  let scope: HTMLCanvasElement | null = null;
  const wave = new Float32Array(analyser.fftSize);
  const freq = new Uint8Array(analyser.frequencyBinCount);
  const draw = () => {
    requestAnimationFrame(draw);
    analyser.getFloatTimeDomainData(wave);
    analyser.getByteFrequencyData(freq);
    const avg = (a: number, b: number) => { let s = 0; for (let i = a; i < b; i++) s += freq[i]; return s / (b - a) / 255; };
    const gate = on ? 1 : 0;
    shared.bass += (avg(1, 8) * gate - shared.bass) * 0.3;
    shared.mid += (avg(12, 70) * gate - shared.mid) * 0.2;
    shared.high += (avg(120, 360) * gate - shared.high) * 0.2;
    const now = ctx.currentTime;
    const recent = station?.kicks.filter((k) => k <= now).pop();
    shared.kick = on && recent !== undefined ? Math.exp(-(now - recent) * 9) : shared.kick * 0.9;
    if (!scope) return;
    const g = scope.getContext("2d");
    if (!g) return;
    const dpr = Math.min(devicePixelRatio || 1, 2);
    const w = scope.clientWidth, h = scope.clientHeight;
    if (scope.width !== Math.round(w * dpr)) { scope.width = Math.round(w * dpr); scope.height = Math.round(h * dpr); }
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);
    // find a rising zero crossing so the trace stands still instead of sliding
    let start = 0;
    for (let i = 1; i < wave.length / 2; i++) if (wave[i - 1] < 0 && wave[i] >= 0) { start = i; break; }
    g.lineWidth = 1.6;
    g.strokeStyle = "#7cf2c9";
    g.shadowColor = "#7cf2c9";
    g.shadowBlur = 5;
    g.beginPath();
    const span = 800;
    for (let i = 0; i < span; i++) {
      const x = (i / span) * w;
      const y = h / 2 - (on ? wave[start + i] ?? 0 : 0) * h * 1.7;
      if (i) g.lineTo(x, y); else g.moveTo(x, y);
    }
    g.stroke();
  };
  requestAnimationFrame(draw);

  return {
    get station() { return id; },
    get playing() { return on; },
    setScope(el: HTMLCanvasElement | null) { scope = el; },
    setVolume(v: number) { level = v; if (on) volume.gain.setTargetAtTime(v, ctx.currentTime, 0.08); },
    async play() {
      await ctx.resume();
      if (!station) tune(id);
      if (nextBar < ctx.currentTime) nextBar = ctx.currentTime + 0.05;
      pump();
      clearInterval(timer);
      timer = window.setInterval(pump, 60);
      volume.gain.cancelScheduledValues(ctx.currentTime);
      volume.gain.setTargetAtTime(level, ctx.currentTime, 0.5);
      on = true;
    },
    pause() {
      on = false;
      clearInterval(timer);
      volume.gain.cancelScheduledValues(ctx.currentTime);
      volume.gain.setTargetAtTime(0, ctx.currentTime, 0.12);
      setTimeout(() => { if (!on) ctx.suspend(); }, 700);
    },
    async switchTo(next: StationId) {
      if (next === id && station) return;
      if (!on) { id = next; station = null; return; }
      const t = ctx.currentTime + 0.03;
      if (next === "past") dialUp(ctx, volume, t); else static_(t);
      tune(next, t + (next === "past" ? 1.6 : 0.65));
      pump();
    },
  };
}
