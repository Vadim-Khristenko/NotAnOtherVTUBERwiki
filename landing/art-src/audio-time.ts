import { LoFi } from "../src/audio/lofi";
import { Electro } from "../src/audio/electro";
import { Past } from "../src/audio/past";
const make = { lofi: (c: BaseAudioContext, o: AudioNode) => new LoFi(c, o), electro: (c: BaseAudioContext, o: AudioNode) => new Electro(c, o), past: (c: BaseAudioContext, o: AudioNode) => new Past(c, o) };
(async () => {
  const out: Record<string, unknown> = {};
  for (const id of ["lofi", "electro", "past"] as const) {
    const t0 = performance.now();
    const ctx = new OfflineAudioContext(2, 44100 * 12, 44100);
    const st = make[id](ctx, ctx.destination);
    let nodes = 0;
    const orig = ctx.createOscillator.bind(ctx);
    (ctx as any).createOscillator = () => { nodes++; return orig(); };
    for (let n = 0, t = 0.05; t < 12; n++, t += st.bar) st.scheduleBar(n, t);
    const t1 = performance.now();
    await ctx.startRendering();
    out[id] = { scheduleMs: Math.round(t1 - t0), renderMs: Math.round(performance.now() - t1), oscillators: nodes };
  }
  (window as any).result = out;
})();
