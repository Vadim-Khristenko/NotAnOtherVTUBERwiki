<script setup lang="ts">
/* Three odometer drums, real 3D: ten faces round a cylinder. They spin
   through every digit when they come into view and settle on 0, because
   that is honestly how many trackers, ads and analytics scripts there are. */
import { ref, watch } from "vue";
import { t } from "../i18n";
import { useSeen, useFormation, reducedMotion, toast } from "../lib/motion";

const root = ref<HTMLElement | null>(null);
const seen = useSeen(root, 0.35);
useFormation(root, "zeros");
const turns = ref([0, 0, 0]);
const labels = ["counts.trackers", "counts.ads", "counts.analytics"];
const spin = (i: number, laps: number) => { if (!reducedMotion()) turns.value[i] += laps; };
watch(seen, (v) => { if (v) [3, 4, 5].forEach((laps, i) => setTimeout(() => spin(i, laps), i * 220)); });
function poke(i: number) { spin(i, 2); toast(t("egg.zero")); }
</script>

<template>
  <section id="counts" ref="root" class="zeros" aria-label="0 trackers, 0 ads, 0 analytics">
    <div class="drums">
      <button v-for="(key, i) in labels" :key="key" class="drum-box" type="button" :class="`d${i}`" @click="poke(i)">
        <span class="window" aria-hidden="true">
          <span class="drum" :style="{ transform: `rotateX(${turns[i] * 360}deg)`, transitionDuration: `${1.6 + i * 0.35}s` }">
            <b v-for="d in 10" :key="d" :style="{ transform: `rotateX(${-(d - 1) * 36}deg) translateZ(var(--r))` }">{{ d - 1 }}</b>
          </span>
        </span>
        <span class="visually-hidden">0</span>
        <span class="label">{{ t(key) }}</span>
      </button>
    </div>
    <p class="note">{{ t("counts.note") }} <span class="hint js-only">{{ t("counts.hint") }}</span></p>
  </section>
</template>

<style scoped>
.zeros { padding: clamp(4rem, 9vw, 7rem) 0 clamp(3rem, 6vw, 5rem); }
.drums { display: grid; grid-template-columns: repeat(3, 1fr); gap: clamp(.6rem, 3vw, 2.5rem); width: min(var(--wrap), 100% - 2.5rem); margin: 0 auto; }
.drum-box { display: grid; justify-items: center; gap: 1rem; padding: 0; background: none; border: 0; cursor: pointer; }
.window {
  --h: clamp(5rem, 15vw, 10.5rem); --r: calc(var(--h) * 1.539);
  position: relative; display: block; width: calc(var(--h) * .82); height: var(--h); perspective: 700px;
  border: 3px solid var(--ink); background: var(--surface); box-shadow: var(--lift);
  -webkit-mask-image: linear-gradient(transparent, #000 22%, #000 78%, transparent); mask-image: linear-gradient(transparent, #000 22%, #000 78%, transparent);
  transform: rotate(-2deg);
}
.d1 .window { transform: rotate(1.5deg); } .d2 .window { transform: rotate(-.5deg); }
.drum { position: absolute; inset: 0; transform-style: preserve-3d; transition-property: transform; transition-timing-function: cubic-bezier(.16, .9, .2, 1.02); }
.drum b { position: absolute; inset: 0; display: grid; place-items: center; backface-visibility: hidden; font: 900 calc(var(--h) * .86)/1 var(--display); color: var(--hot); }
.d1 .drum b { color: var(--violet); } .d2 .drum b { color: var(--cream); }
.drum-box:hover .window { box-shadow: var(--lift-up); }
.label { font: 800 clamp(.78rem, 1.5vw, 1rem)/1 var(--display); letter-spacing: -.005em; color: var(--ink-soft); text-shadow: var(--halo); }
.note { width: min(46rem, 100% - 2.5rem); margin: 3rem auto 0; text-align: center; color: var(--muted); text-shadow: var(--halo); }
.hint { color: var(--hot); white-space: nowrap; }
</style>
