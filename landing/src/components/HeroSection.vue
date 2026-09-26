<script setup lang="ts">
import { onMounted, ref } from "vue";
import { t } from "../i18n";
import { useFormation, vMagnetic, reducedMotion } from "../lib/motion";
import { radio } from "../audio/store";
import SplitText from "./SplitText.vue";

const root = ref<HTMLElement | null>(null);
const title = ref<HTMLElement | null>(null);
const ready = ref(false);
useFormation(root, "vortex");
onMounted(() => requestAnimationFrame(() => (ready.value = !reducedMotion())));

/* a letter jumps when the pointer passes over it */
function hop(e: PointerEvent) {
  const c = (e.target as HTMLElement).closest(".c") as HTMLElement | null;
  if (!c || reducedMotion()) return;
  c.animate(
    [{ transform: "none" }, { transform: "translateY(-.22em) rotate(-8deg) scale(1.25)", color: "var(--hot)" }, { transform: "none" }],
    { duration: 520, easing: "cubic-bezier(.34,1.56,.64,1)" },
  );
}
</script>

<template>
  <section id="hero" ref="root" class="hero" aria-labelledby="hero-title">
    <!-- the logo sticker, until (or instead of) the 3D cat -->
    <img class="fallback" src="/assets/art/cut/logo.webp" alt="" width="560" height="343" />
    <div class="copy">
      <h1 id="hero-title" ref="title" class="title" :class="{ go: ready }" @pointerover="hop">
        <SplitText :text="t('hero.title')" />
      </h1>
      <p class="lede">{{ t("hero.lede") }}</p>
      <div class="row">
        <a v-magnetic class="slab" href="#join">{{ t("hero.cta_join") }}</a>
        <button v-magnetic class="slab is-cream js-only" type="button" :aria-pressed="radio.playing" @click="radio.toggle()">
          <span class="eq" :class="{ on: radio.playing }" aria-hidden="true"><i></i><i></i><i></i><i></i></span>
          {{ t("hero.cta_radio") }}
        </button>
        <a class="plain" href="https://github.com/Vadim-Khristenko/NotAnOtherVTUBERwiki">{{ t("hero.cta_code") }} ↗</a>
      </div>
      <p class="hint js-only">{{ t("hero.hint") }}</p>
    </div>
  </section>
</template>

<style scoped>
.hero { position: relative; min-height: 100svh; display: grid; align-items: center; padding: 7rem clamp(1.25rem, 5vw, 5rem) 5rem; }
.copy { position: relative; max-width: 40rem; }
.fallback { position: absolute; right: 5vw; top: 50%; width: min(40vw, 540px); transform: translateY(-50%) rotate(-5deg); filter: drop-shadow(0 22px 26px rgb(8 3 18 / .5)); transition: opacity .8s, transform .8s var(--spring); pointer-events: none; }
:global(.has-3d .hero .fallback) { opacity: 0; transform: translateY(-50%) rotate(-5deg) scale(.8); }
/* a veil under the words: the snacks swim behind it and never under the letters (no blur: over a live canvas it would redraw every frame) */
.copy::before { content: ""; position: absolute; inset: -2.2rem -2.4rem; z-index: -1; background: color-mix(in srgb, var(--bg) 82%, transparent); -webkit-mask-image: linear-gradient(90deg, #000 78%, transparent); mask-image: linear-gradient(90deg, #000 78%, transparent); }
.title { margin: 0 0 1.4rem; font: 900 clamp(2.2rem, 4.5vw, 4.3rem)/1.03 var(--display); letter-spacing: -.045em; text-wrap: balance; text-shadow: var(--halo); cursor: default; }
.title.go :deep(.c) { animation: rise 1.1s var(--ease) both; animation-delay: calc(var(--i) * 22ms + 200ms); }
@keyframes rise { from { transform: translateY(.8em) rotate(6deg); opacity: 0; } }
.lede { max-width: 34rem; margin: 0 0 2rem; color: var(--ink-soft); font-size: clamp(1.05rem, 1.4vw, 1.2rem); }
.plain { font: 700 .92rem/1 var(--display); color: var(--ink-soft); text-decoration-color: var(--hot); text-underline-offset: .3em; }
.hint { margin: 1.8rem 0 0; font-size: .85rem; color: var(--muted); }
.hint::before { content: "↗ "; color: var(--hot); }
.eq { display: inline-flex; gap: 2px; align-items: flex-end; height: 14px; }
.eq i { width: 3px; height: 4px; background: currentColor; }
.eq.on i { animation: eq .8s var(--ease) infinite alternate; }
.eq.on i:nth-child(2) { animation-delay: -.2s; } .eq.on i:nth-child(3) { animation-delay: -.5s; } .eq.on i:nth-child(4) { animation-delay: -.35s; }
@keyframes eq { to { height: 14px; } }
@media (max-width: 760px) {
  .hero { align-items: end; padding-top: 48svh; padding-bottom: 5rem; }
  .copy::before { inset: -1.4rem -1rem; }
  .fallback { top: 22%; right: 50%; width: 78vw; transform: translate(50%, -50%) rotate(-5deg); }
  :global(.has-3d .hero .fallback) { transform: translate(50%, -50%) rotate(-5deg) scale(.8); }

}
</style>
