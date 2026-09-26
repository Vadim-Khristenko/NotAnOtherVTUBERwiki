<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref } from "vue";
import { i18n, t, setLanguage } from "../i18n";
import { toast, confetti } from "../lib/motion";

const stuck = ref(false);
const theme = ref<"dark" | "light">("dark");
let clicks = 0;
let clickTimer = 0;

const onScroll = () => {
  stuck.value = scrollY > 24;
};
onMounted(() => {
  theme.value = document.documentElement.dataset.theme === "light" ? "light" : "dark";
  addEventListener("scroll", onScroll, { passive: true });
  onScroll();
});
onBeforeUnmount(() => removeEventListener("scroll", onScroll));

function flipTheme() {
  theme.value = theme.value === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = theme.value;
  try { localStorage.setItem("naw-theme", theme.value); } catch {}
}
function brandClick(e: MouseEvent) {
  clicks++;
  clearTimeout(clickTimer);
  clickTimer = window.setTimeout(() => (clicks = 0), 500);
  if (clicks >= 3) { clicks = 0; e.preventDefault(); toast(t("egg.meow")); confetti(["paw", "cat"]); }
}
</script>

<template>
  <header class="head" :class="{ 'is-stuck': stuck }">
    <a class="brand" href="/" aria-label="FilianWIKI" @click="brandClick">
      <img class="brand-mark" src="/assets/art/mark.svg" alt="" width="34" height="34" />
      <span class="brand-word">Filian<b>WIKI</b></span>
    </a>
    <nav class="nav" :aria-label="t('nav.sections')">
      <a href="#wiki">{{ t("nav.wiki") }}</a>
      <a href="#join">{{ t("nav.join") }}</a>
      <a href="#roadmap">{{ t("nav.roadmap") }}</a>
      <a href="#domains">{{ t("nav.domains") }}</a>
    </nav>
    <div class="tools js-only">
      <label class="lang">
        <span class="visually-hidden">{{ t("nav.language") }}</span>
        <select :value="i18n.lang" @change="setLanguage(($event.target as HTMLSelectElement).value)">
          <option v-for="l in i18n.languages" :key="l.code" :value="l.code" :title="l.name">{{ l.code.toUpperCase() }}</option>
        </select>
      </label>
      <button class="theme" type="button" @click="flipTheme">
        <span class="bulb" :class="theme" aria-hidden="true"></span><span class="theme-word">{{ theme === "dark" ? t("nav.theme_light") : t("nav.theme_dark") }}</span>
      </button>
    </div>
  </header>
</template>

<style scoped>
.head { position: fixed; inset: 0 0 auto; z-index: 50; display: flex; align-items: center; gap: 1.25rem; padding: .9rem clamp(1rem, 3vw, 2.25rem); transition: background .4s var(--ease), padding .4s var(--ease); }
.head.is-stuck { background: color-mix(in srgb, var(--bg) 92%, transparent); box-shadow: 0 1px 0 var(--line); padding-block: .55rem; }
.brand { display: flex; align-items: center; gap: .6rem; text-decoration: none; font: 800 1.15rem/1 var(--display); letter-spacing: -.02em; text-shadow: var(--halo); }
.brand b { color: var(--hot); font-weight: 900; }
.brand-mark { width: 34px; height: 34px; transition: transform .6s var(--spring); }
.brand:hover .brand-mark { transform: rotate(-16deg) scale(1.15); }
.nav { display: flex; gap: .2rem; margin-left: auto; }
.nav a { position: relative; padding: .45rem .75rem; text-decoration: none; color: var(--ink-soft); font-weight: 600; font-size: .95rem; text-shadow: var(--halo); }
.nav a::after { content: ""; position: absolute; left: .75rem; right: .75rem; bottom: .2rem; height: 2px; background: var(--hot); transform: scaleX(0); transform-origin: right; transition: transform .35s var(--ease); }
.nav a:hover::after { transform: scaleX(1); transform-origin: left; }
.tools { display: flex; gap: .5rem; align-items: center; }
.lang select, .theme { appearance: none; font: 800 .78rem/1 var(--display); letter-spacing: .05em; color: var(--ink); background: var(--surface); border: 2px solid var(--line); padding: .55rem .8rem; cursor: pointer; }
.lang select:hover, .theme:hover { border-color: var(--hot); }
.theme { display: inline-flex; align-items: center; gap: .45rem; }
.bulb { width: .8rem; height: .8rem; border-radius: 50%; background: var(--cream); box-shadow: 0 0 10px var(--cream); transition: all .4s var(--spring); }
.bulb.light { background: #1d0f2e; box-shadow: inset -3px -2px 0 var(--cream); }
@media (max-width: 860px) { .nav { display: none; } .tools { margin-left: auto; } }
/* on a phone the theme button is just the bulb, so the brand keeps its room */
@media (max-width: 520px) {
  .head { gap: .6rem; }
  .theme { padding-inline: .7rem; }
  .theme-word { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
}
</style>
