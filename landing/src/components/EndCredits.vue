<script setup lang="ts">
/* The end credits. The screen holds still while you scroll and the credits
   roll past it, the way they do after a film; then the duo, and a signature
   that writes itself. With motion off it is a plain list. */
import { computed, ref } from "vue";
import { t } from "../i18n";
import { useScrollProgress, useFormation, reducedMotion, clamp } from "../lib/motion";

const root = ref<HTMLElement | null>(null);
const progress = useScrollProgress(root);
useFormation(root, "fireworks");
const rows = [
  ["credits.role_dev", "VAI_PROG"],
  ["credits.role_curation", "Akane"],
  ["credits.role_art", "just.call.me.l"],
  ["credits.role_music", "Akane"],
  ["credits.role_3d", "Three.js"],
  ["credits.role_type", "Unbounded · Golos Text"],
  ["credits.role_snacks", "credits.snacks"],
  ["credits.role_trackers", "credits.trackers"],
];
const name = (v: string) => (v.startsWith("credits.") ? t(v) : v);
/* the roll: from below the screen to above it over the first two thirds of the scroll */
const roll = computed(() => (reducedMotion() ? 0 : 1 - clamp((progress.value - 0.18) / 0.45, 0, 1)));
const outro = computed(() => reducedMotion() || progress.value > 0.58);
</script>

<template>
  <section id="credits" ref="root" class="credits" aria-labelledby="credits-title">
    <div class="screen">
      <p id="credits-title" class="card">{{ t("credits.kicker") }}</p>
      <div class="roll" :style="{ transform: `translateY(${roll * 100}%)`, opacity: outro ? 0 : 1 }">
        <dl>
          <div v-for="[role, who] in rows" :key="role"><dt>{{ t(role) }}</dt><dd>{{ name(who) }}</dd></div>
        </dl>
      </div>
      <div class="outro" :class="{ on: outro }">
        <svg class="signature" viewBox="0 0 900 140" aria-hidden="true"><text x="450" y="98" text-anchor="middle">VAI_PROG × Akane</text></svg>
        <p class="duo" v-html="t('credits.duo')"></p>
        <div class="bios">
          <p v-html="t('credits.vai')"></p>
          <p v-html="t('credits.akane')"></p>
        </div>
        <p class="links"><a href="https://vai-rice.space">vai-rice.space</a> <a href="https://github.com/Vadim-Khristenko">GitHub</a> <a href="https://git.vai-rice.space/VAI_PROG">git.vai-rice.space</a></p>
      </div>
    </div>
  </section>
</template>

<style scoped>
.credits { position: relative; height: 240vh; background: color-mix(in srgb, #07040d 74%, transparent); color: #f3ecff; }
.screen { position: sticky; top: 0; height: 100vh; overflow: hidden; display: grid; place-items: center; padding: 5rem 1.25rem 3rem; }
.card { position: absolute; top: 5.5rem; left: 50%; translate: -50% 0; margin: 0; font: 800 1.05rem/1 var(--display); letter-spacing: -.01em; color: var(--hot); white-space: nowrap; }
.roll { position: absolute; left: 0; right: 0; top: 0; height: 100%; display: grid; align-content: center; transition: opacity .6s; }
dl { margin: 0 auto; display: grid; gap: 1.4rem; width: min(52rem, 100%); padding-inline: 1rem; }
dl div { display: grid; grid-template-columns: 1fr 1fr; gap: 2rem; align-items: baseline; }
dt { text-align: right; font: 600 .95rem/1.3 var(--body); text-transform: uppercase; letter-spacing: .14em; color: #a898bd; }
dd { margin: 0; font: 900 clamp(1.2rem, 2.6vw, 1.9rem)/1.1 var(--display); letter-spacing: -.02em; }
.outro { position: relative; z-index: 2; max-width: 60rem; text-align: center; opacity: 0; transform: translateY(30px) scale(.97); transition: opacity .8s var(--ease), transform .9s var(--spring); pointer-events: none; }
.outro.on { opacity: 1; transform: none; pointer-events: auto; }
.signature { width: min(900px, 100%); height: auto; overflow: visible; }
.signature text { font: 900 92px/1 var(--display); letter-spacing: -3px; fill: transparent; stroke: var(--hot); stroke-width: 2; stroke-dasharray: 1700; stroke-dashoffset: 1700; }
.outro.on .signature text { animation: sign 2.8s var(--ease) forwards; }
@keyframes sign { 70% { stroke-dashoffset: 0; fill: transparent; } 100% { stroke-dashoffset: 0; fill: var(--hot); } }
html:not(.js) .signature text { fill: var(--hot); stroke-dashoffset: 0; }
html:not(.js) .outro { opacity: 1; transform: none; }
html:not(.js) .credits { height: auto; }
html:not(.js) .screen { position: static; height: auto; display: grid; gap: 3rem; }
html:not(.js) .roll { position: static; transform: none !important; opacity: 1 !important; }
.duo { margin: 0 auto 2rem; max-width: 46rem; font: 700 clamp(1.1rem, 2.2vw, 1.5rem)/1.4 var(--display); letter-spacing: -.02em; text-wrap: balance; }
.duo :deep(.aside) { color: #a898bd; font-weight: 500; }
.duo :deep(b) { color: var(--hot); }
.bios { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem 2rem; text-align: left; color: #cfc4e0; font-size: .98rem; }
.bios p { margin: 0; }
.bios :deep(b) { color: #fff; }
.links { display: flex; gap: 1.2rem; justify-content: center; margin-top: 1.6rem; }
.links a { color: #cfc4e0; }
@media (max-width: 700px) { .bios { grid-template-columns: 1fr; } dl div { gap: 1rem; } dt { font-size: .75rem; } }
@media (prefers-reduced-motion: reduce) { .credits { height: auto; } .screen { position: static; height: auto; display: grid; gap: 3rem; } .roll { position: static; } }
</style>
