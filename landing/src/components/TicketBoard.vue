<script setup lang="ts">
/* Where the community already is, as tickets pinned to a wall. Each ticket
   says what the place really is; the stub on the right is the way in, a big
   and obvious one, and it tears a little when you reach for it.
   Hover is caught by boxes that never move (the list item, the link); only
   the paper inside them moves, so the pointer cannot fall off the thing it
   is hovering and set it flickering. */
import { computed, ref } from "vue";
import { i18n, t } from "../i18n";
import { useFormation, useSeen } from "../lib/motion";

type Ticket = { key: string; href: string; host: string; color: string; html?: boolean };
const all: Ticket[] = [
  { key: "discord", href: "https://discord.gg/filian", host: "discord.gg/filian", color: "pink" },
  { key: "reddit", href: "https://www.reddit.com/r/filian/", host: "reddit.com/r/filian", color: "orange" },
  { key: "ru", href: "https://discord.gg/Hw9SG2wrV3", host: "discord.gg/Hw9SG2wrV3", color: "violet" },
  { key: "filianru", href: "https://filian.ru", host: "filian.ru", color: "sky" },
  { key: "arg", href: "https://discord.gg/bFuzBKwhqA", host: "discord.gg/bFuzBKwhqA", color: "cream", html: true },
  { key: "coffee", href: "https://vai-rice.space", host: "vai-rice.space", color: "mint" },
  { key: "mail", href: "mailto:vadim@filian.wiki", host: "vadim@filian.wiki", color: "ink" },
];
const kinds: Record<string, string> = { discord: "places.discord_kind", reddit: "places.reddit_kind", ru: "places.ru_kind", filianru: "places.filianru_kind", arg: "places.arg_kind", coffee: "", mail: "" };
// Russian readers get our own filian.ru first: it is the place we most want them to see
const tickets = computed(() => (i18n.lang === "ru" ? [all[3], ...all.filter((_, i) => i !== 3)] : all));
const tilts = [-2, 1.5, -1, 2.2, -1.6, 1.2, -1.4];
const root = ref<HTMLElement | null>(null);
const seen = useSeen(root, 0.12);
useFormation(root, "drift");
const bars = (seed: number) => Array.from({ length: 26 }, (_, i) => 1 + ((seed * 31 + i * 17) % 4));
</script>

<template>
  <section id="places" ref="root" class="places wrap" :class="{ in: seen }" aria-labelledby="places-title">
    <header class="section-head reveal" :class="{ 'is-in': seen }">
      <h2 id="places-title" class="section-title">{{ t("places.title") }}</h2>
      <p class="section-lede">{{ t("places.lede") }}</p>
    </header>
    <ul class="wall" role="list">
      <li v-for="(k, i) in tickets" :key="k.key" class="ticket" :class="k.color" :style="{ '--i': i, '--r': `${tilts[i % tilts.length]}deg` }">
        <div class="card">
        <div class="main">
          <p class="admit">{{ t("places.admit") }} · N° {{ String(i + 1).padStart(3, "0") }}</p>
          <h3>{{ t(`places.${k.key}`) }}</h3>
          <p v-if="kinds[k.key]" class="kind">{{ t(kinds[k.key]) }}</p>
          <p v-if="k.html" class="text" v-html="t(`places.${k.key}_text`)"></p>
          <p v-else class="text">{{ t(`places.${k.key}_text`) }}</p>
          <span class="barcode" aria-hidden="true"><i v-for="(w, n) in bars(i + 3)" :key="n" :style="{ width: `${w}px` }"></i></span>
        </div>
        <a class="stub" :href="k.href" :rel="k.href.startsWith('http') ? 'noopener' : undefined">
          <span class="tear">
            <span class="enter">{{ t("places.enter") }} <span aria-hidden="true">↗</span></span>
            <span class="host">{{ k.host }}</span>
          </span>
        </a>
        </div>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.places { padding-block: clamp(4rem, 10vw, 8rem); }
.wall { list-style: none; margin: 0; padding: 0; display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 30rem), 1fr)); gap: 1.6rem 2rem; }
.ticket { --paper: var(--surface-2); --ink2: var(--ink); position: relative; color: var(--ink2); }
.card { display: grid; grid-template-columns: minmax(0, 1fr) 8.5rem; transform: rotate(var(--r)); filter: drop-shadow(0 14px 18px rgb(8 3 18 / .42)); transition: transform .45s var(--ease); }
.js .places .card { opacity: 0; transform: rotate(calc(var(--r) * 6)) translateY(60px); transition: transform .8s var(--ease), opacity .5s; transition-delay: calc(var(--i) * 90ms); }
.js .places.in .card { opacity: 1; transform: rotate(var(--r)); transition: transform .45s var(--ease), opacity .5s; transition-delay: 0s; }
.js .places.in .ticket:hover .card { transform: rotate(0deg) translateY(-4px); }
.pink { --paper: var(--hot); --ink2: #fff; } .violet { --paper: var(--violet); --ink2: #fff; }
.cream { --paper: var(--cream); --ink2: #1d0f2e; } .mint { --paper: var(--mint); --ink2: #0b2a20; }
.ink { --paper: var(--surface-2); --ink2: var(--ink); }
.orange { --paper: #ffb35c; --ink2: #2a1400; } .sky { --paper: #8ecbff; --ink2: #0b1d33; }
/* the paper, with the notches where a ticket is torn */
.main, .tear { background: var(--paper); -webkit-mask: radial-gradient(circle .7rem at var(--mx, 100%) 0, transparent 98%, #000) top / 100% 51% no-repeat, radial-gradient(circle .7rem at var(--mx, 100%) 100%, transparent 98%, #000) bottom / 100% 51% no-repeat; mask: radial-gradient(circle .7rem at var(--mx, 100%) 0, transparent 98%, #000) top / 100% 51% no-repeat, radial-gradient(circle .7rem at var(--mx, 100%) 100%, transparent 98%, #000) bottom / 100% 51% no-repeat; }
.main { padding: 1.2rem 1.4rem; border-right: 3px dashed color-mix(in srgb, var(--ink2) 45%, transparent); }
/* the link is the still hit area; the stub paper inside it tears away */
.stub { display: grid; text-decoration: none; color: var(--ink2); cursor: pointer; }
.stub:focus-visible { outline-offset: 4px; }
.tear { --mx: 0%; display: grid; place-content: center; gap: .5rem; padding: 1rem .8rem; text-align: center; transform-origin: 0 100%; transition: transform .35s var(--spring), background .2s; }
.stub:hover .tear, .stub:focus-visible .tear { transform: rotate(4deg) translate(4px, -2px); background: color-mix(in srgb, var(--paper) 82%, #fff); }
.stub:active .tear { transform: rotate(9deg) translate(10px, 4px); }
.enter { font: 900 1.15rem/1 var(--display); text-transform: uppercase; letter-spacing: .02em; }
.host { font: 600 .66rem/1.3 var(--mono); overflow-wrap: anywhere; opacity: .85; }
.admit { margin: 0 0 .5rem; font: 700 .64rem/1 var(--mono); letter-spacing: .12em; opacity: .75; }
h3 { margin: 0; font: 900 1.35rem/1.1 var(--display); letter-spacing: -.02em; hyphens: auto; overflow-wrap: anywhere; }
.kind { margin: .3rem 0 0; font: 800 .68rem/1 var(--display); text-transform: uppercase; letter-spacing: .14em; opacity: .8; }
.text { margin: .7rem 0 .9rem; font-size: .95rem; }
.text :deep(a) { color: inherit; font-weight: 700; }
.barcode { display: flex; gap: 2px; height: 1.6rem; align-items: stretch; opacity: .7; }
.barcode i { background: currentColor; }
@media (max-width: 520px) { .card { grid-template-columns: minmax(0, 1fr) 6rem; } .enter { font-size: .95rem; } .main { padding: 1rem 1rem; } h3 { font-size: 1.12rem; } }
</style>
