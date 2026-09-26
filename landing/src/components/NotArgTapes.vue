<script setup lang="ts">
/* What this is not: two crossing tapes that run faster and lean harder the
   faster you scroll, then the plain words. Right under the hero on purpose:
   a fan wiki about a real person has to say it, and a 3D page with music
   would otherwise get read as ARG content in this community. The tapes
   slow to a stop under the pointer, so they can be read. */
import { onBeforeUnmount, onMounted, ref } from "vue";
import { t } from "../i18n";
import { useFormation, reducedMotion, toast } from "../lib/motion";

const root = ref<HTMLElement | null>(null);
const tracks: HTMLElement[] = [];
const keep = (el: unknown) => { if (el instanceof HTMLElement && !tracks.includes(el)) tracks.push(el); };
useFormation(root, "ribbon");
const skew = ref(0);
const hold = ref(false);
let last = 0, lastY = 0, raf = 0, speed = 0, rate = 1;
let io: IntersectionObserver | null = null;
const tick = (now: number) => {
  raf = requestAnimationFrame(tick);
  const dt = Math.min(0.05, (now - last) / 1000 || 0.016);
  last = now;
  const v = (scrollY - lastY) / Math.max(dt, 0.001);
  lastY = scrollY;
  speed += (Math.min(4000, Math.abs(v)) - speed) * 0.08;
  skew.value += (Math.sign(v) * Math.min(10, Math.abs(v) / 200) - skew.value) * 0.1;
  rate += ((hold.value ? 0 : 1 + speed / 350) - rate) * 0.12;
  for (const el of tracks) for (const a of el.getAnimations()) a.playbackRate = rate;
};
const run = (on: boolean) => {
  cancelAnimationFrame(raf);
  if (on) { last = performance.now(); lastY = scrollY; raf = requestAnimationFrame(tick); }
};
onMounted(() => {
  if (reducedMotion() || !root.value) return;
  // only while the tapes are on screen
  io = new IntersectionObserver(([e]) => run(e.isIntersecting));
  io.observe(root.value);
});
onBeforeUnmount(() => { cancelAnimationFrame(raf); io?.disconnect(); });
</script>

<template>
  <section id="disclaimer" ref="root" class="notice" aria-labelledby="notice-tag" @dblclick="toast(t('egg.not_arg'))">
    <div class="tapes" aria-hidden="true" @pointerenter="hold = true" @pointerleave="hold = false">
      <div class="tape a" :style="{ transform: `rotate(-4deg) skewX(${skew}deg)` }">
        <div :ref="keep" class="track"><span v-for="n in 4" :key="n">{{ t("tape.one") }}</span></div>
      </div>
      <div class="tape b" :style="{ transform: `rotate(3deg) skewX(${-skew}deg)` }">
        <div :ref="keep" class="track rev"><span v-for="n in 4" :key="n">{{ t("tape.two") }}</span></div>
      </div>
    </div>
    <div class="card wrap">
      <p id="notice-tag" class="tag">{{ t("disclaimer.tag") }}</p>
      <p>{{ t("disclaimer.one") }}</p>
      <p>{{ t("disclaimer.two") }}</p>
    </div>
  </section>
</template>

<style scoped>
.notice { position: relative; padding-bottom: clamp(3rem, 8vw, 5rem); overflow: clip; }
.tapes { position: relative; height: clamp(9rem, 17vw, 12rem); }
.tape { position: absolute; left: -10%; width: 120%; overflow: hidden; padding: 1rem 0; font: 900 clamp(.95rem, 1.8vw, 1.3rem)/1 var(--display); letter-spacing: .04em; white-space: nowrap; border-block: 3px solid #1d0f2e; }
.tape.a { top: 16%; background: var(--hot); color: #fff; z-index: 2; }
.tape.b { top: 50%; background: var(--cream); color: #1d0f2e; }
.track { display: inline-flex; gap: 2rem; padding-right: 2rem; animation: run 30s linear infinite; }
.track.rev { animation-direction: reverse; animation-duration: 36s; }
.track span { flex: none; }
@keyframes run { to { transform: translateX(-50%); } }
.card { display: grid; grid-template-columns: auto 1fr 1fr; gap: 1rem 2.5rem; padding: 1.8rem 2rem; background: var(--surface); border: 3px solid var(--ink); box-shadow: var(--lift); }
.card p { margin: 0; color: var(--ink-soft); }
.tag { font: 900 1rem/1.2 var(--display); letter-spacing: -.01em; color: var(--hot) !important; }
@media (max-width: 860px) { .card { grid-template-columns: 1fr; } }
</style>
