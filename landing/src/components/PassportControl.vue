<script setup lang="ts">
/* Our addresses, as stamps in a passport, and a desk where any address can
   be shown and gets its own stamp: ours, friendly, lookalike, or nothing to
   do with us. The stamps are plain links in the markup, so the list is there
   without scripts too: it is how a careful reader tells us from a copy. */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { t } from "../i18n";
import { OURS, FRIENDS, verdict, hostOf, foreignLetters, type Verdict } from "../lib/domains";
import { useFormation, useSeen, reducedMotion } from "../lib/motion";

const root = ref<HTMLElement | null>(null);
const seen = useSeen(root, 0.2);
useFormation(root, "orbit");
const tilt = [-8, 5, -3, 9];
const input = ref("");
const result = ref<Verdict | null>(null);
const shown = ref("");
const stampKey = ref(0);
const touched = ref(false);

function check(value = input.value) {
  shown.value = value;
  result.value = verdict(value);
  stampKey.value++;
}
const stampText = computed(() => (result.value && result.value !== "empty" ? t(`domains.v_${result.value}`) : ""));
const odd = computed(() => new Set(foreignLetters(shown.value)));
const letters = computed(() => [...hostOf(shown.value)]);
const stampNote = computed(() => {
  if (!result.value) return "";
  if (result.value === "empty") return t("domains.v_empty");
  const base = t(`domains.v_${result.value}_text`);
  return odd.value.size ? `${base} ${t("domains.v_foreign")}` : base;
});

/* Until somebody types, the desk checks a few addresses on its own. */
// Only fakes that look fake on screen: a demo must never stamp something that reads as ours.
const demo = ["filian.wiki", "fi1ian.wiki", "filian.rip", "snackers-wiki.com", "filian.ru", "filian.mom", "filianwiki.xyz", "example.com"];
let demoTimer = 0;
let typeTimer = 0;
function typeDemo(i: number) {
  if (touched.value) return;
  const word = demo[i % demo.length];
  let n = 0;
  input.value = "";
  const next = () => {
    if (touched.value) return;
    input.value = word.slice(0, ++n);
    if (n < word.length) typeTimer = window.setTimeout(next, 70);
    else { check(word); demoTimer = window.setTimeout(() => typeDemo(i + 1), 2600); }
  };
  next();
}
watch(seen, (v) => { if (v && !reducedMotion()) demoTimer = window.setTimeout(() => typeDemo(0), 1400); });
function take() { touched.value = true; clearTimeout(demoTimer); clearTimeout(typeTimer); }
onBeforeUnmount(() => { clearTimeout(demoTimer); clearTimeout(typeTimer); });
onMounted(() => { if (reducedMotion()) check("fi1ian.wiki"); });
</script>

<template>
  <section id="domains" ref="root" class="passport-section" :class="{ in: seen }" aria-labelledby="domains-title">
    <svg width="0" height="0" aria-hidden="true" style="position: absolute">
      <filter id="ink"><feTurbulence type="fractalNoise" baseFrequency="1.1" numOctaves="1" seed="4" result="n" /><feDisplacementMap in="SourceGraphic" in2="n" scale="1.1" /></filter>
    </svg>
    <div class="wrap">
      <header class="section-head reveal" :class="{ 'is-in': seen }">
        <h2 id="domains-title" class="section-title">{{ t("domains.title") }}</h2>
        <p class="section-lede">{{ t("domains.lede") }}</p>
      </header>

      <div class="counter">
        <div class="passport">
          <div class="page ours">
            <p class="page-title">{{ t("domains.ours") }}</p>
            <ul class="stamps" role="list">
              <li v-for="(d, i) in OURS" :key="d.host" :style="{ '--r': `${tilt[i]}deg`, '--d': `${i * 0.18}s` }">
                <a class="stamp round" :href="`https://${d.host}`">
                  <svg viewBox="0 0 200 200" aria-hidden="true">
                    <defs><path :id="`arc-${i}`" d="M 100 100 m -72 0 a 72 72 0 1 1 144 0 a 72 72 0 1 1 -144 0" /></defs>
                    <circle cx="100" cy="100" r="92" /><circle cx="100" cy="100" r="58" />
                    <text class="arc"><textPath :href="`#arc-${i}`" startOffset="50%" text-anchor="middle">★ {{ d.host.toUpperCase() }} ★</textPath></text>
                    <text class="mid" x="100" y="96" text-anchor="middle">{{ t("domains.v_ours") }}</text>
                    <text class="mid small" x="100" y="120" text-anchor="middle">✓</text>
                  </svg>
                  <span class="host">{{ d.host }}</span>
                  <small>{{ t(d.note) }}</small>
                </a>
              </li>
            </ul>
          </div>
          <div class="page visas">
            <p class="page-title">{{ t("domains.friends") }}</p>
            <ul class="stamps" role="list">
              <li v-for="(d, i) in FRIENDS" :key="d.host" :style="{ '--r': `${[-5, 6, -3][i % 3]}deg`, '--d': `${0.8 + i * 0.2}s` }">
                <a class="stamp visa" :href="`https://${d.host}`" rel="noopener">
                  <span class="visa-top">VISA · {{ t("domains.v_friend") }}</span>
                  <span class="host">{{ d.host }}</span>
                  <small>{{ t(d.note) }}</small>
                </a>
              </li>
            </ul>
          </div>
        </div>

        <form class="desk js-only" @submit.prevent="take(); check()">
          <label class="desk-label" for="addr">{{ t("domains.check_label") }}</label>
          <div class="desk-row">
            <input id="addr" v-model="input" type="text" inputmode="url" autocomplete="off" spellcheck="false" placeholder="filian.wiki" maxlength="120" @focus="take" @input="take" />
            <button class="slab" type="submit">{{ t("domains.check") }}</button>
          </div>
          <div class="desk-paper">
            <p class="desk-shown"><template v-if="shown"><span v-for="(c, i) in letters" :key="i" :class="{ odd: odd.has(i) }">{{ c }}</span></template><template v-else>…</template></p>
            <span v-if="stampText" :key="stampKey" class="verdict" :class="result">{{ stampText }}</span>
            <p class="desk-note" aria-live="polite">{{ stampNote }}</p>
          </div>
        </form>
      </div>
      <p class="warn">{{ t("domains.warn") }}</p>
    </div>
  </section>
</template>

<style scoped>
.passport-section { padding-block: clamp(4rem, 10vw, 8rem); }
.counter { display: grid; grid-template-columns: minmax(0, 1.5fr) minmax(0, 1fr); gap: 2rem; align-items: start; }
.passport { display: grid; grid-template-columns: 1fr 1fr; background: #f6efe0; color: #1d0f2e; border: 4px solid #3a1f5c; box-shadow: var(--lift); position: relative; }
.passport::after { content: ""; position: absolute; top: 0; bottom: 0; left: 50%; width: 2px; background: linear-gradient(#0002, #0004, #0002); }
.page { padding: 1.2rem 1.2rem 1.6rem; background-image: repeating-linear-gradient(135deg, transparent 0 14px, rgb(122 61 255 / .06) 14px 15px); }
.page-title { margin: 0 0 .8rem; font: 800 .86rem/1.2 var(--display); letter-spacing: -.005em; color: #7a3dff; }
.stamps { list-style: none; margin: 0; padding: 0; display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: .8rem; }
.visas .stamps { grid-template-columns: 1fr; gap: 1.3rem; padding: .4rem .6rem 0; }
.stamp { display: grid; justify-items: center; text-align: center; text-decoration: none; color: #c2005c; transform: rotate(var(--r)); filter: url(#ink); transition: transform .3s var(--spring); }
.stamp:hover { transform: rotate(0deg) scale(1.06); }
.js .passport-section .stamp { opacity: 0; transform: rotate(var(--r)) scale(2.4); }
.js .passport-section.in .stamp { animation: slam .5s cubic-bezier(.6, 0, .9, .5) var(--d) forwards; }
@keyframes slam { 80% { opacity: 1; transform: rotate(var(--r)) scale(.94); } 100% { opacity: 1; transform: rotate(var(--r)) scale(1); } }
.round svg { width: min(9rem, 100%); }
.round circle { fill: none; stroke: currentColor; stroke-width: 5; }
.round circle + circle { stroke-width: 2.5; }
.arc { font: 800 15px/1 var(--display); fill: currentColor; letter-spacing: 2px; }
.mid { font: 900 26px/1 var(--display); fill: currentColor; }
.mid.small { font-size: 18px; }
.host { font: 900 .92rem/1.2 var(--display); overflow-wrap: anywhere; }
.stamp small { font-size: .74rem; opacity: .85; }
.visa { gap: .15rem; padding: 1rem 1.3rem 1.1rem; border: 3px dashed #5b2bd6; color: #5b2bd6; }
.visa-top { font: 800 .62rem/1 var(--display); letter-spacing: .16em; margin-bottom: .4rem; }
.desk { position: sticky; top: 6rem; padding: 1.2rem; background: var(--surface); border: 3px solid var(--ink); box-shadow: var(--lift); }
.desk-label { display: block; font: 800 .92rem/1 var(--display); letter-spacing: -.005em; margin-bottom: .6rem; color: var(--cream); }
.desk-row { display: flex; gap: .6rem; }
.desk-row input { flex: 1; min-width: 0; padding: .8rem; font: 600 1rem/1 var(--mono); color: var(--ink); background: var(--bg); border: 2px solid var(--line); }
.desk-row input:focus { border-color: var(--hot); outline: none; }
.desk-paper { position: relative; margin-top: 1rem; min-height: 11rem; padding: 1rem; background: #fffaf0; color: #1d0f2e; overflow: hidden; }
.desk-shown { margin: 0; font: 700 1.05rem/1.3 var(--mono); overflow-wrap: anywhere; }
.desk-shown .odd { background: #d2003f; color: #fff; padding: 0 .1em; }
.desk-note { position: absolute; left: 1rem; right: 1rem; bottom: .9rem; margin: 0; font-size: .92rem; }
.verdict { position: absolute; left: 50%; top: 46%; padding: .35rem .8rem; border: 5px double currentColor; font: 900 clamp(1.1rem, 4.6vw, 2rem)/1.05 var(--display); letter-spacing: .04em; max-width: 92%; text-align: center; filter: url(#ink); transform: translate(-50%, -50%) rotate(-10deg); animation: thud .42s cubic-bezier(.6, 0, .9, .5); }
@keyframes thud { from { transform: translate(-50%, -50%) rotate(-24deg) scale(2.6); opacity: 0; } 80% { transform: translate(-50%, -50%) rotate(-9deg) scale(.95); opacity: 1; } }
.verdict.ours { color: #0a9c6a; } .verdict.friend { color: #5b2bd6; } .verdict.fake { color: #d2003f; } .verdict.other { color: #6b6b7b; }
.warn { margin: 2rem 0 0; color: var(--muted); }
@media (max-width: 960px) { .counter { grid-template-columns: minmax(0, 1fr); } .desk { position: static; } }
@media (max-width: 560px) { .passport { grid-template-columns: minmax(0, 1fr); } .passport::after { display: none; } .page { padding-inline: .9rem; } }
</style>
