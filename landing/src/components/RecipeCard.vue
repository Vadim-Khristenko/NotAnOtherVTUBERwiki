<script setup lang="ts">
/* The roadmap as a recipe card from a kitchen drawer: the steps we have done
   are crossed out with a marker, the one we are on is in the oven (you can
   see the wiki rising through the glass), and the rest is still to come.
   Anyone reads a recipe; nobody has to learn a board or a chart for it. */
import { ref } from "vue";
import { t } from "../i18n";
import { useFormation, useSeen } from "../lib/motion";

type Step = { key: string; state: "done" | "now" | "next" | "then" };
const steps: Step[] = [
  { key: "one", state: "done" },
  { key: "two", state: "done" },
  { key: "three", state: "done" },
  { key: "four", state: "now" },
  { key: "five", state: "next" },
  { key: "six", state: "then" },
];
const root = ref<HTMLElement | null>(null);
const card = ref<HTMLElement | null>(null);
const seen = useSeen(root, 0.1);
const cardSeen = useSeen(card, 0.35);
useFormation(root, "trail");
// every marker stroke wobbles its own way
const strokes = ["M1 6 C 18 3.5, 36 7.5, 55 5 S 88 3.8, 99 6.2", "M1 5 C 22 7, 40 3, 62 5.5 S 90 6.5, 99 4.4", "M1 5.8 C 15 4, 44 6.8, 60 4.6 S 85 5.5, 99 5"];
</script>

<template>
  <section id="roadmap" ref="root" class="roadmap wrap" aria-labelledby="roadmap-title">
    <header class="section-head reveal" :class="{ 'is-in': seen }">
      <h2 id="roadmap-title" class="section-title">{{ t("roadmap.title") }}</h2>
      <p class="section-lede">{{ t("roadmap.lede") }}</p>
    </header>

    <article ref="card" class="recipe" :class="{ 'is-in': cardSeen }">
      <span class="tape" aria-hidden="true"></span>
      <header class="top">
        <h3 class="dish">{{ t("recipe.title") }}</h3>
        <dl class="meta">
          <div><dt>{{ t("recipe.serves_k") }}</dt><dd>{{ t("recipe.serves") }}</dd></div>
          <div><dt>{{ t("recipe.ready_k") }}</dt><dd>{{ t("recipe.ready") }}</dd></div>
        </dl>
      </header>

      <ol class="steps" role="list">
        <li v-for="(s, i) in steps" :key="s.key" class="step" :class="s.state" :style="{ '--i': i }">
          <span class="n" aria-hidden="true">{{ i + 1 }}</span>
          <div class="body">
            <h4 class="verb">
              <span>{{ t(`recipe.step_${s.key}`) }}</span>
              <svg v-if="s.state === 'done'" class="strike" viewBox="0 0 100 10" preserveAspectRatio="none" aria-hidden="true">
                <path :d="strokes[i % strokes.length]" pathLength="1" />
              </svg>
              <span v-if="s.state === 'done'" class="visually-hidden">({{ t("phase.done") }})</span>
            </h4>
            <p class="what"><b>{{ t(`phase.${s.key}`) }}.</b> {{ t(`phase.${s.key}_text`) }}</p>
            <p v-if="s.state === 'now'" class="when now-tag">{{ t("recipe.baking") }}</p>
            <p v-else-if="s.state !== 'done'" class="when">{{ t(`phase.${s.state}`) }}</p>
          </div>
          <div v-if="s.state === 'now'" class="oven" aria-hidden="true">
            <svg viewBox="0 0 132 112">
              <defs>
                <radialGradient id="oven-glow" cx="50%" cy="70%" r="75%">
                  <stop offset="0" stop-color="#ffd27a" />
                  <stop offset=".55" stop-color="#ff8a3d" />
                  <stop offset="1" stop-color="#8a2a1a" />
                </radialGradient>
                <clipPath id="oven-glass"><rect x="22" y="38" width="88" height="50" rx="7" /></clipPath>
              </defs>
              <rect x="4" y="4" width="124" height="104" rx="12" class="shell" />
              <rect x="4" y="4" width="124" height="22" rx="12" class="panel" />
              <circle cx="22" cy="15" r="5" class="knob" /><circle cx="38" cy="15" r="5" class="knob" />
              <rect x="84" y="10" width="32" height="10" rx="3" class="clock" />
              <text x="100" y="18.2" text-anchor="middle" class="clock-text">180°</text>
              <rect x="14" y="32" width="104" height="66" rx="9" class="door" />
              <rect x="30" y="31" width="72" height="4" rx="2" class="handle" />
              <g clip-path="url(#oven-glass)">
                <rect x="22" y="38" width="88" height="50" class="glow" fill="url(#oven-glow)" />
                <g class="bake"><image href="/assets/art/mark.svg" x="46" y="44" width="40" height="40" /></g>
                <rect x="22" y="80" width="88" height="3" class="rack" />
                <path d="M30 40 L 52 40 L 38 86 L 22 86 Z" class="shine" />
              </g>
              <rect x="22" y="38" width="88" height="50" rx="7" class="glass-edge" />
            </svg>
          </div>
        </li>
      </ol>

      <p class="note">
        {{ t("recipe.note") }}
        <a href="https://github.com/Vadim-Khristenko/NotAnOtherVTUBERwiki/blob/dev/docs/ROADMAP.md">{{ t("roadmap.full") }} ↗</a>
      </p>
    </article>
  </section>
</template>

<style scoped>
.roadmap { position: relative; padding-block: clamp(4rem, 10vw, 7rem); }
.recipe {
  --paper: #fffaf0; --rule: #cfe0f5; --margin: #ff9bb8; --pen: #1d0f2e;
  position: relative; max-width: 54rem; margin-inline: auto; padding: 3.2rem clamp(1.2rem, 4vw, 3rem) 2.2rem clamp(3rem, 7vw, 5rem);
  color: var(--pen); background:
    linear-gradient(90deg, transparent calc(clamp(2.2rem, 5.4vw, 3.8rem) - 1px), var(--margin) 0 calc(clamp(2.2rem, 5.4vw, 3.8rem) + 1px), transparent 0),
    repeating-linear-gradient(transparent 0 calc(2rem - 1px), var(--rule) 0 2rem) 0 1.4rem / 100% 100% no-repeat,
    var(--paper);
  border-radius: 4px; box-shadow: var(--lift); transform: rotate(-.8deg);
}
.js .recipe { opacity: 0; transform: rotate(-3deg) translateY(40px); transition: transform 1s var(--ease), opacity .6s; }
.js .recipe.is-in { opacity: 1; transform: rotate(-.8deg); }
.tape { position: absolute; top: -.9rem; left: 50%; width: 8.5rem; height: 1.9rem; translate: -50% 0; rotate: 2deg; background: color-mix(in srgb, var(--hot) 55%, #fff); opacity: .85; clip-path: polygon(3% 0, 97% 4%, 100% 50%, 96% 100%, 2% 96%, 0 45%); }

.top { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: end; gap: .6rem 2rem; margin-bottom: 1.6rem; }
.dish { margin: 0; font: 900 clamp(1.5rem, 3.4vw, 2.3rem)/1.05 var(--display); letter-spacing: -.04em; }
.meta { display: flex; gap: 1.4rem; margin: 0; font-size: .92rem; }
.meta div { display: grid; }
.meta dt { color: #7a3dff; font-weight: 700; }
.meta dd { margin: 0; }

.steps { list-style: none; margin: 0; padding: 0; display: grid; gap: 1.1rem; }
.step { position: relative; display: grid; grid-template-columns: 2.2rem 1fr auto; gap: .2rem 1rem; align-items: start; }
.n { font: 900 1.35rem/1.4 var(--display); color: #c2005c; }
.verb { position: relative; display: inline-block; margin: 0 0 .15rem; font: 800 clamp(1.05rem, 2vw, 1.3rem)/1.4 var(--display); letter-spacing: -.02em; }
.what { margin: 0; max-width: 36rem; font-size: .98rem; line-height: 1.6; color: #3d2b55; }
.what b { color: var(--pen); }
.when { margin: .3rem 0 0; font-size: .85rem; font-weight: 700; color: #6e5c86; }

/* done: struck through with a pink marker, drawn when the card arrives */
.strike { position: absolute; left: -3%; top: 34%; width: 106%; height: .55em; overflow: visible; pointer-events: none; }
.strike path { fill: none; stroke: color-mix(in srgb, #ff2d7a 78%, transparent); stroke-width: 3.2; stroke-linecap: round; stroke-dasharray: 1; }
.js .strike path { stroke-dashoffset: 1; transition: stroke-dashoffset .55s cubic-bezier(.6, 0, .3, 1); transition-delay: calc(.5s + var(--i) * .35s); }
.js .recipe.is-in .strike path { stroke-dashoffset: 0; }
.done .what { color: #6e5c86; }

/* now: in the oven */
.now { padding: 1rem 0; margin-block: .2rem; }
.now::before { content: ""; position: absolute; inset: 0 -1rem 0 -3.2rem; z-index: -1; background: color-mix(in srgb, #ffcf5c 26%, transparent); border-radius: 3px; clip-path: polygon(0 8%, 100% 0, 99% 92%, 1% 100%); }
.now .verb { color: #c2005c; }
.now-tag { color: #b3470f; }
.now-tag::before { content: ""; display: inline-block; width: .55rem; height: .55rem; margin-right: .45rem; border-radius: 2px; background: #ff8a3d; vertical-align: .05em; }
.oven { width: clamp(7rem, 15vw, 9rem); margin-top: -.4rem; }
.oven svg { display: block; width: 100%; height: auto; overflow: visible; }
.shell { fill: #3a1f5c; }
.panel { fill: #2a1545; }
.knob { fill: #fffaf0; stroke: #1d0f2e; stroke-width: 2; }
.clock { fill: #120a20; }
.clock-text { font: 700 7.5px var(--mono); fill: #ff8a3d; }
.door { fill: #4b2a74; }
.handle { fill: #cfc2e3; }
.rack { fill: #1d0f2e; opacity: .45; }
.shine { fill: #fff; opacity: .12; }
.glass-edge { fill: none; stroke: #1d0f2e; stroke-width: 3; }
.bake { transform-box: fill-box; transform-origin: 50% 100%; }
.js .recipe.is-in .glow { animation: flicker 2.8s ease-in-out infinite alternate; }
.js .recipe.is-in .bake { animation: rise 4.5s ease-in-out infinite alternate; }
@keyframes flicker { 0% { opacity: .82; } 45% { opacity: 1; } 60% { opacity: .9; } 100% { opacity: .98; } }
@keyframes rise { from { transform: scale(.86, .8); } to { transform: scale(1, 1.02); } }

.next .verb, .then .verb { color: #3d2b55; }

.note { margin: 1.8rem 0 0; padding-top: 1rem; border-top: 2px dashed #d9c9ec; font-size: .95rem; color: #3d2b55; }
.note a { font-weight: 700; color: #c2005c; text-underline-offset: .25em; white-space: nowrap; }

@media (max-width: 640px) {
  .recipe { padding: 2.8rem 1.1rem 1.8rem 2.9rem; background:
    linear-gradient(90deg, transparent 1.9rem, var(--margin) 0 calc(1.9rem + 2px), transparent 0),
    repeating-linear-gradient(transparent 0 calc(2rem - 1px), var(--rule) 0 2rem) 0 1.4rem / 100% 100% no-repeat, var(--paper); }
  .step { grid-template-columns: 1.6rem 1fr; }
  .n { margin-left: -.4rem; }
  .oven { grid-column: 2; width: 7.5rem; margin: .6rem 0 0; }
  .now::before { inset: 0 -.6rem 0 -2.4rem; }
}
</style>
