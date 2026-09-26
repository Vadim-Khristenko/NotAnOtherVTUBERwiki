<script setup lang="ts">
/* Snack Radio, as a cassette in the corner. The reels turn while it plays,
   the window between them is an oscilloscope, and the three stations are
   the mechanical preset buttons of an old car radio: the pressed one stays
   down. Nothing plays until somebody presses play. */
import { computed, onMounted, ref } from "vue";
import { t } from "../i18n";
import { radio } from "../audio/store";
import type { StationId } from "../audio/engine";

const scope = ref<HTMLCanvasElement | null>(null);
const open = ref(false);
onMounted(() => radio.setScope(scope.value));
const stations: StationId[] = ["lofi", "electro", "past"];
const track = computed(() => t(`radio.track_${radio.station}`));
const speed = computed(() => ({ lofi: "3.2s", electro: "1.9s", past: "1.4s" })[radio.station]);
</script>

<template>
  <aside class="deck js-only" :class="{ playing: radio.playing, open: open || radio.playing }" :aria-label="t('radio.title')">
    <button class="play" type="button" :aria-pressed="radio.playing" :aria-label="radio.playing ? t('radio.pause') : t('radio.play')" @click="radio.toggle()">
      <svg viewBox="0 0 24 24" aria-hidden="true"><path v-if="!radio.playing" d="M8 5v14l11-7z" /><path v-else d="M7 5h4v14H7zM13 5h4v14h-4z" /></svg>
    </button>
    <button class="tape" type="button" :style="{ '--spin': speed }" @click="open = !open" :aria-expanded="open || radio.playing">
      <span class="label"><b>{{ t("radio.title") }}</b><i>{{ track }}</i><span class="visually-hidden">, {{ t("radio.stations") }}</span></span>
      <span class="reel l" aria-hidden="true"></span>
      <canvas ref="scope" class="scope" aria-hidden="true"></canvas>
      <span class="reel r" aria-hidden="true"></span>
    </button>
    <div class="controls">
      <div class="presets" role="radiogroup" :aria-label="t('radio.stations')">
        <button v-for="s in stations" :key="s" type="button" role="radio" :aria-checked="radio.station === s" class="preset" :class="{ down: radio.station === s }" @click="radio.tune(s)">{{ t(`radio.${s}`) }}</button>
      </div>
      <label class="vol"><span class="visually-hidden">{{ t("radio.volume") }}</span>
        <input type="range" min="0" max="1" step="0.01" :value="radio.volume" @input="radio.setVolume(Number(($event.target as HTMLInputElement).value))" />
      </label>
      <p class="note">{{ t("radio.note") }}</p>
    </div>
  </aside>
</template>

<style scoped>
.deck { position: fixed; right: clamp(.7rem, 2vw, 1.4rem); bottom: clamp(.7rem, 2vw, 1.4rem); z-index: 60; display: grid; grid-template-columns: auto auto; gap: .5rem .6rem; align-items: center; padding: .55rem; background: #1d0f2e; border: 3px solid #0d0616; box-shadow: var(--lift); color: #fff; max-width: calc(100vw - 1.4rem); }
.play { width: 3rem; height: 3rem; display: grid; place-items: center; background: var(--hot); color: #fff; border: 2px solid #0d0616; cursor: pointer; transition: transform .2s var(--spring); }
.play:hover { transform: rotate(-6deg) scale(1.06); }
.play svg { width: 1.3rem; fill: currentColor; }
.playing .play { animation: throb .7s var(--ease) infinite alternate; }
@keyframes throb { to { box-shadow: 0 0 0 .4rem color-mix(in srgb, var(--hot) 30%, transparent); } }
/* the cassette */
.tape { position: relative; width: 13.5rem; height: 5.4rem; padding: 0; border: 2px solid #0d0616; cursor: pointer; background: linear-gradient(#2b1d40, #221533); display: grid; grid-template-columns: 1fr 5.2rem 1fr; grid-template-rows: 1.9rem 1fr; align-items: center; justify-items: center; }
.label { grid-column: 1 / -1; align-self: stretch; justify-self: stretch; margin: .25rem .35rem 0; padding: .15rem .45rem; background: var(--cream); color: #1d0f2e; text-align: left; display: grid; overflow: hidden; }
.label b { font: 900 .62rem/1 var(--display); letter-spacing: .08em; text-transform: uppercase; }
.label i { font: italic 600 .72rem/1.2 var(--body); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.reel { width: 2.4rem; height: 2.4rem; border-radius: 50%; background: radial-gradient(circle, #1d0f2e 0 22%, transparent 23%), repeating-conic-gradient(#e8e1f0 0 12deg, #0d0616 12deg 60deg); border: 3px solid #e8e1f0; }
.playing .reel { animation: spin var(--spin) linear infinite; }
.playing .reel.r { animation-duration: calc(var(--spin) * 1.3); }
@keyframes spin { to { transform: rotate(360deg); } }
.scope { width: 5rem; height: 2.1rem; background: #0a1a12; border: 2px solid #0d0616; }
.controls { grid-column: 1 / -1; display: none; gap: .5rem; }
.open .controls { display: grid; }
.presets { display: grid; grid-template-columns: repeat(3, 1fr); gap: .35rem; }
.preset { padding: .55rem .2rem; background: #e8e1f0; color: #1d0f2e; border: 0; border-bottom: 5px solid #8a7fa0; font: 900 .62rem/1 var(--display); text-transform: uppercase; letter-spacing: .04em; cursor: pointer; transition: transform .1s, border-width .1s, background .2s; }
.preset:hover { background: #fff; }
.preset.down { transform: translateY(4px); border-bottom-width: 1px; background: var(--mint); }
.vol input { width: 100%; accent-color: var(--hot); }
.note { margin: 0; font-size: .68rem; color: #a898bd; max-width: 17rem; }
@media (max-width: 560px) {
  .deck { padding: .4rem; gap: .4rem; }
  .deck:not(.open) .tape { width: 5.6rem; height: 3rem; grid-template-columns: 1fr 1fr; grid-template-rows: 1fr; }
  .deck:not(.open) .label, .deck:not(.open) .scope { display: none; }
  .deck:not(.open) .reel { width: 1.6rem; height: 1.6rem; }
  .play { width: 2.6rem; height: 2.6rem; }
  .tape { width: 11rem; grid-template-columns: 1fr 4rem 1fr; }
  .scope { width: 3.8rem; }
  .reel { width: 1.9rem; height: 1.9rem; }
}
</style>
