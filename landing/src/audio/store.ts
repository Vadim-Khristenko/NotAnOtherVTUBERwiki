/* What the page knows about Snack Radio. The player itself (engine.ts and
   the three stations) loads on the first press of play, not before. */
import { reactive } from "vue";
import type { StationId } from "./engine";

type Player = ReturnType<typeof import("./engine").createPlayer>;
let player: Player | null = null;
let scope: HTMLCanvasElement | null = null;

async function get() {
  if (!player) {
    const { createPlayer } = await import("./engine");
    player = createPlayer();
    player.setScope(scope);
    player.setVolume(radio.volume);
    await player.switchTo(radio.station);
  }
  return player;
}

export const radio = reactive({
  playing: false,
  station: "lofi" as StationId,
  volume: 0.6,
  async toggle() {
    const p = await get();
    if (p.playing) p.pause(); else await p.play();
    radio.playing = p.playing;
  },
  async tune(id: StationId) {
    radio.station = id;
    const p = await get();
    await p.switchTo(id);
    if (!p.playing) { await p.play(); radio.playing = true; }
  },
  setVolume(v: number) {
    radio.volume = v;
    player?.setVolume(v);
  },
  setScope(el: HTMLCanvasElement | null) {
    scope = el;
    player?.setScope(el);
  },
});
