<script setup lang="ts">
/* Anyone can join. The split-flap board says who we are short of, Filian
   leans out of her frame toward the button, and the application is a
   postcard that writes itself. It is an email in a fixed shape: this page
   collects nothing. */
import { computed, ref, watch } from "vue";
import { t, tList } from "../i18n";
import { useSeen, useScrollProgress, useFormation, reducedMotion, toast, confetti, vMagnetic } from "../lib/motion";
import SplitFlap from "./SplitFlap.vue";
import SplitText from "./SplitText.vue";

const root = ref<HTMLElement | null>(null);
const card = ref<HTMLElement | null>(null);
const seen = useSeen(root, 0.15);
const cardSeen = useSeen(card, 0.35);
const progress = useScrollProgress(root);
useFormation(root, "heart");

const TEMPLATE = "Username that you wanna claim: \nYour age group: \nWho are you?: \nWhy Vadim should approve your registration: ";
const MAIL = "mailto:vadim+calpha@filian.wiki?subject=Closed%20alpha%20application&body=Username%20that%20you%20wanna%20claim%3A%20%0AYour%20age%20group%3A%20%0AWho%20are%20you%3F%3A%20%0AWhy%20Vadim%20should%20approve%20your%20registration%3A%20%0A";
const typed = ref(TEMPLATE);
const typing = ref(false);
watch(cardSeen, (v) => {
  if (!v || reducedMotion()) return;
  typed.value = "";
  typing.value = true;
  let i = 0;
  const step = () => {
    typed.value = TEMPLATE.slice(0, ++i);
    if (i < TEMPLATE.length) setTimeout(step, TEMPLATE[i - 1] === "\n" ? 240 : 16 + Math.random() * 30);
    else typing.value = false;
  };
  setTimeout(step, 900);
});
async function copy() {
  try { await navigator.clipboard.writeText(TEMPLATE + "\n"); toast(t("join.copied")); confetti(["letter", "heart"], 12); }
  catch { toast(t("join.copy_failed")); }
}
const lean = computed(() => (reducedMotion() ? 0 : (progress.value - 0.5) * 2));
</script>

<template>
  <section id="join" ref="root" class="join" :class="{ in: seen }" aria-labelledby="join-title">
    <div class="wrap top">
      <div class="art" aria-hidden="true">
        <span class="frame" :style="{ transform: `rotate(${-6 + lean * 3}deg)` }"></span>
        <img class="filian" src="/assets/art/cut/character.webp" alt="" width="900" height="768" loading="lazy"
             :style="{ transform: `translate(${lean * -18}px, ${lean * -40}px) rotate(${lean * 4}deg)` }" />
      </div>
      <div class="copy">
        <h2 id="join-title" class="title"><SplitText :text="t('join.title')" /></h2>
        <p class="lede">{{ t("join.lede") }}:</p>
        <p class="board-label">{{ t("join.board") }}</p>
        <SplitFlap :words="tList('join.words')" :cols="20" :rows="2" />
        <p class="how" v-html="t('join.how')"></p>
        <a v-magnetic class="slab" :href="MAIL">{{ t("join.write") }}</a>
      </div>
    </div>

    <div class="wrap bottom">
      <div ref="card" class="postcard" :class="{ in: cardSeen }">
        <div class="msg">
          <p class="label">{{ t("join.postcard") }}</p>
          <pre class="text" :class="{ typing }">{{ typed }}</pre>
          <div class="row">
            <a class="slab" :href="MAIL">{{ t("join.write") }}</a>
            <button class="slab is-quiet js-only" type="button" @click="copy">{{ t("join.copy") }}</button>
          </div>
        </div>
        <div class="addr">
          <div class="stamp" aria-hidden="true"><img src="/assets/art/mark.svg" alt="" /><span>{{ t("join.stamp") }}</span></div>
          <svg class="postmark" viewBox="0 0 220 90" aria-hidden="true">
            <circle cx="46" cy="45" r="36" /><circle cx="46" cy="45" r="28" />
            <text x="46" y="42" text-anchor="middle">FILIAN</text><text x="46" y="56" text-anchor="middle">WIKI</text>
            <path d="M92 25 q 16 -10 32 0 t 32 0 t 32 0 t 32 0 M92 45 q 16 -10 32 0 t 32 0 t 32 0 t 32 0 M92 65 q 16 -10 32 0 t 32 0 t 32 0 t 32 0" />
          </svg>
          <p class="to">To: Vadim<br /><span>vadim+calpha@filian.wiki</span></p>
        </div>
      </div>
      <dl class="notes">
        <div><dt>{{ t("join.age") }}</dt><dd>{{ t("join.age_text") }}</dd></div>
        <div><dt>{{ t("join.who") }}</dt><dd>{{ t("join.who_text") }}</dd></div>
        <div><dt>{{ t("join.lang") }}</dt><dd>{{ t("join.lang_text") }}</dd></div>
      </dl>
    </div>
  </section>
</template>

<style scoped>
.join { position: relative; padding-block: clamp(4rem, 10vw, 8rem); overflow: clip; }
.top { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.1fr); gap: clamp(1.5rem, 4vw, 3rem); align-items: center; }
.art { position: relative; aspect-ratio: 1 / 1; }
.frame { position: absolute; inset: 12% 14% 10% 4%; background: var(--art); border: 4px solid #1d0f2e; box-shadow: var(--lift); transition: transform .3s linear; }
.filian { position: absolute; left: -2%; bottom: 4%; width: 112%; max-width: none; filter: drop-shadow(0 24px 30px rgb(0 0 0 / .45)); transition: transform .3s linear; }
.js .join .art { opacity: 0; transform: translateX(-10%) scale(.9) rotate(-6deg); transition: transform 1.2s var(--ease), opacity .7s; }
.js .join.in .art { opacity: 1; transform: none; }
.title { margin: 0 0 1.2rem; font: 900 clamp(2.4rem, 5.6vw, 4.6rem)/1 var(--display); letter-spacing: -.045em; text-shadow: var(--halo); }
.js .join .title :deep(.c) { opacity: 0; transform: translateY(.5em) rotateX(-70deg); transition: transform .8s var(--ease), opacity .4s; transition-delay: calc(var(--i) * 25ms); }
.js .join.in .title :deep(.c) { opacity: 1; transform: none; }
.lede { font-size: 1.15rem; color: var(--ink-soft); margin: 0 0 1rem; text-shadow: var(--halo); }
.board-label { margin: 0 0 .5rem; font: 800 .86rem/1 var(--display); letter-spacing: -.005em; color: var(--cream); }
.how { color: var(--ink-soft); margin: 1.4rem 0; text-shadow: var(--halo); }
.how :deep(a) { color: var(--ink); text-decoration-color: var(--hot); text-underline-offset: .2em; }

.bottom { display: grid; grid-template-columns: minmax(0, 1.5fr) minmax(0, 1fr); gap: 2rem 3rem; margin-top: clamp(3rem, 7vw, 5rem); align-items: start; }
.postcard { position: relative; display: grid; grid-template-columns: 1.4fr 1fr; background: #fffaf0; color: #1d0f2e; border: 3px solid #1d0f2e; box-shadow: var(--lift); transform: rotate(-1.5deg); }
.js .postcard { transform: rotate(-12deg) translateY(60px) scale(.9); opacity: 0; transition: transform 1s var(--ease), opacity .5s; }
.js .postcard.in { transform: rotate(-1.5deg); opacity: 1; }
.msg { padding: 1.6rem; border-right: 2px dashed #d8c3e8; }
.label { margin: 0 0 .8rem; font: 800 .86rem/1 var(--display); letter-spacing: -.005em; color: #b0006a; }
.text { margin: 0 0 1.4rem; min-height: 7.6em; font: 500 .98rem/1.9 var(--mono); white-space: pre-wrap; background: repeating-linear-gradient(transparent 0 1.85em, #f1dccb 1.85em 1.9em); }
.text.typing::after { content: "▍"; color: #e0005c; animation: blink .8s steps(1) infinite; }
@keyframes blink { 50% { opacity: 0; } }
.msg .slab.is-quiet { --c: #fff; --t: #1d0f2e; }
.addr { position: relative; padding: 1.4rem; display: grid; align-content: space-between; min-height: 16rem; }
.stamp { justify-self: end; width: 6.5rem; padding: .5rem; background: var(--art); display: grid; gap: .3rem; justify-items: center; transform: rotate(6deg);
  -webkit-mask: radial-gradient(circle 5px at 5px 5px, transparent 98%, #000) -5px -5px / 13px 13px; mask: radial-gradient(circle 5px at 5px 5px, transparent 98%, #000) -5px -5px / 13px 13px; }
.stamp img { width: 4rem; filter: brightness(0) invert(1); }
.stamp span { font: 800 .5rem/1.1 var(--display); color: #fff; text-align: center; text-transform: uppercase; letter-spacing: .06em; }
.js .postcard .stamp { transform: rotate(40deg) scale(2.2); opacity: 0; transition: transform .45s cubic-bezier(.5, 0, .9, .4) 1.1s, opacity .2s 1.1s; }
.js .postcard.in .stamp { transform: rotate(6deg) scale(1); opacity: 1; }
.postmark { position: absolute; top: 3.2rem; right: 3.5rem; width: 12rem; opacity: .55; pointer-events: none; }
.postmark circle, .postmark path { fill: none; stroke: #b0006a; stroke-width: 2.5; }
.postmark text { fill: #b0006a; font: 800 11px/1 var(--display); }
.to { margin: 0; font: 700 1rem/1.5 var(--mono); border-bottom: 2px solid #1d0f2e; }
.to span { font-size: .8rem; }
.notes { margin: 0; display: grid; gap: 1rem; }
.notes div { padding: 1rem 1.2rem; border-left: 4px solid var(--hot); background: color-mix(in srgb, var(--surface) 80%, transparent); }
.notes dt { font: 800 .85rem/1.2 var(--display); margin-bottom: .35rem; color: var(--cream); }
.notes dd { margin: 0; color: var(--ink-soft); font-size: .95rem; }
@media (max-width: 900px) {
  .top, .bottom { grid-template-columns: minmax(0, 1fr); }
  .art { max-width: 420px; margin-inline: auto; width: 100%; }
  .postcard { grid-template-columns: minmax(0, 1fr); }
  .msg { border-right: 0; border-bottom: 2px dashed #d8c3e8; }
  .addr { min-height: 12rem; }
}
</style>
