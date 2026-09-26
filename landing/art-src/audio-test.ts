// Renders each Snack Radio station offline and draws what it sounds like:
// level, peaks, and an oscillogram and a spectrogram to look at.
import { LoFi } from "../src/audio/lofi";
import { Electro } from "../src/audio/electro";
import { Past } from "../src/audio/past";

const make = { lofi: (c: BaseAudioContext, o: AudioNode) => new LoFi(c, o), electro: (c: BaseAudioContext, o: AudioNode) => new Electro(c, o), past: (c: BaseAudioContext, o: AudioNode) => new Past(c, o) };

async function render(id: keyof typeof make, seconds: number, startBar = 0) {
  const rate = 44100;
  const ctx = new OfflineAudioContext(2, rate * seconds, rate);
  const st = make[id](ctx, ctx.destination);
  for (let n = 0, t = 0.05; t < seconds; n++, t += st.bar) st.scheduleBar(startBar + n, t);
  return ctx.startRendering();
}

function stats(buf: AudioBuffer) {
  const d = buf.getChannelData(0), e = buf.getChannelData(1);
  let peak = 0, sum = 0, clip = 0;
  for (let i = 0; i < d.length; i++) { const v = Math.max(Math.abs(d[i]), Math.abs(e[i])); peak = Math.max(peak, v); sum += d[i] * d[i]; if (v >= 0.99) clip++; }
  const rms = Math.sqrt(sum / d.length);
  return { peakDb: +(20 * Math.log10(peak)).toFixed(1), rmsDb: +(20 * Math.log10(rms)).toFixed(1), clipped: clip };
}

function draw(buf: AudioBuffer, canvas: HTMLCanvasElement) {
  const g = canvas.getContext("2d")!;
  const w = canvas.width, h = canvas.height;
  const d = buf.getChannelData(0);
  g.fillStyle = "#0b0713"; g.fillRect(0, 0, w, h);
  // top half: the oscillogram, min and max per pixel column
  g.strokeStyle = "#ff2d7a";
  const step = Math.floor(d.length / w);
  for (let x = 0; x < w; x++) {
    let mn = 1, mx = -1;
    for (let i = x * step; i < (x + 1) * step; i++) { mn = Math.min(mn, d[i]); mx = Math.max(mx, d[i]); }
    g.beginPath(); g.moveTo(x, h * 0.25 - mx * h * 0.24); g.lineTo(x, h * 0.25 - mn * h * 0.24); g.stroke();
  }
  // bottom half: a spectrogram from a small DFT per column (log frequency)
  const N = 256, bins = 36, top = h * 0.52, band = h * 0.46;
  for (let x = 0; x < w; x += 10) {
    const off = Math.floor((x / w) * (d.length - N));
    for (let b = 0; b < bins; b++) {
      const f = 40 * Math.pow(16000 / 40, b / bins);
      const k = (f * N) / buf.sampleRate;
      let re = 0, im = 0;
      for (let n = 0; n < N; n += 2) { const win = 0.5 - 0.5 * Math.cos((2 * Math.PI * n) / N); const ph = (2 * Math.PI * k * n) / N; re += d[off + n] * win * Math.cos(ph); im -= d[off + n] * win * Math.sin(ph); }
      const mag = Math.min(1, Math.sqrt(re * re + im * im) / 5);
      g.fillStyle = `hsl(${300 - mag * 260}, 90%, ${mag * 60}%)`;
      g.fillRect(x, top + band - ((b + 1) / bins) * band, 10, band / bins + 1);
    }
  }
}

(async () => {
  const out: Record<string, unknown> = {};
  for (const id of ["lofi", "electro", "past"] as const) {
    const buf = await render(id, 30, id === "lofi" ? 12 : 22);
    out[id] = stats(buf);
    const c = document.createElement("canvas");
    c.width = 1100; c.height = 300; c.style.display = "block"; c.style.marginBottom = "8px";
    const label = document.createElement("p"); label.textContent = `${id} ${JSON.stringify(out[id])}`; label.style.cssText = "color:#fff;font:14px monospace;margin:4px 0";
    document.body.append(label, c);
    draw(buf, c);
  }
  (window as unknown as { result: unknown }).result = out;
})();
