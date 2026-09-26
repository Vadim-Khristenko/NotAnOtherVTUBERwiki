<script setup lang="ts">
/* VAI's line, on a wave that follows the pointer, and a wave that runs
   across it once when it comes into view. The letters are measured once
   (and again on resize), never while they move, and one loop springs them
   all toward the wave: no layout work under the pointer. */
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { i18n, t } from "../i18n";
import { useSeen, useFormation, reducedMotion, toast } from "../lib/motion";
import SplitText from "./SplitText.vue";

const root = ref<HTMLElement | null>(null);
const line = ref<HTMLElement | null>(null);
const seen = useSeen(root, 0.6);
useFormation(root, "wave");

let chars: HTMLElement[] = [];
// where each letter's centre sits relative to the line, with no wave applied
let home: { x: number; y: number }[] = [];
const y = new Float32Array(256), vy = new Float32Array(256), target = new Float32Array(256), tilt = new Float32Array(256);
let raf = 0, last = 0, source: null | { x: number; y: number; k: number } = null;

function measure() {
  const l = line.value;
  if (!l) return;
  // offsetLeft/Top ignore transforms, so a wave in flight does not skew this
  home = chars.map((c) => {
    const inLine = c.offsetParent === l;
    return { x: c.offsetLeft + c.offsetWidth / 2 - (inLine ? 0 : l.offsetLeft), y: c.offsetTop + c.offsetHeight / 2 - (inLine ? 0 : l.offsetTop) };
  });
}
function aim() {
  const l = line.value?.getBoundingClientRect();
  if (!l) return;
  for (let i = 0; i < chars.length; i++) {
    if (!source) { target[i] = 0; tilt[i] = 0; continue; }
    const dx = l.left + home[i].x - source.x;
    const d = Math.hypot(dx, (l.top + home[i].y - source.y) * 0.6);
    const f = Math.exp(-((d / 150) ** 2)) * source.k;
    target[i] = -28 * f;
    tilt[i] = dx * 0.05 * f;
  }
}
function tick(now: number) {
  const dt = Math.min(0.033, (now - last) / 1000 || 0.016);
  last = now;
  let busy = false;
  for (let i = 0; i < chars.length; i++) {
    // a soft spring: quick to rise, a small settle, no jitter
    vy[i] += ((target[i] - y[i]) * 220 - vy[i] * 22) * dt;
    y[i] += vy[i] * dt;
    if (Math.abs(target[i] - y[i]) > 0.05 || Math.abs(vy[i]) > 0.05) busy = true;
    const r = (tilt[i] * (y[i] / -28 || 0)).toFixed(2);
    chars[i].style.transform = Math.abs(y[i]) < 0.05 ? "" : `translate3d(0, ${y[i].toFixed(2)}px, 0) rotate(${r}deg)`;
  }
  raf = busy || source ? requestAnimationFrame(tick) : 0;
}
const run = () => { if (!raf) { last = performance.now(); raf = requestAnimationFrame(tick); } };
const move = (e: PointerEvent) => {
  if (reducedMotion() || e.pointerType === "touch") return;
  source = { x: e.clientX, y: e.clientY, k: 1 };
  aim();
  run();
};
const calm = () => { source = null; aim(); run(); };

let ro: ResizeObserver | null = null;
function collect() {
  chars = [...(line.value?.querySelectorAll<HTMLElement>(".c") ?? [])].slice(0, 256);
  y.fill(0); vy.fill(0); target.fill(0); tilt.fill(0);
  measure();
}
// a new language means new letters
watch(() => i18n.lang, () => nextTick(collect));
onMounted(() => {
  collect();
  document.fonts?.ready.then(measure);
  ro = new ResizeObserver(measure);
  if (line.value) ro.observe(line.value);
});
onBeforeUnmount(() => { cancelAnimationFrame(raf); ro?.disconnect(); });

watch(seen, (v) => {
  if (!v || reducedMotion() || !line.value) return;
  const t0 = performance.now();
  const sweep = (now: number) => {
    const k = (now - t0) / 1700;
    if (source && source.k === 1 && k > 0) return; // the pointer took over
    if (k > 1) return calm();
    const l = line.value!.getBoundingClientRect();
    source = { x: l.left + k * l.width, y: l.top + l.height / 2, k: Math.sin(k * Math.PI) * 0.999 };
    aim();
    run();
    requestAnimationFrame(sweep);
  };
  requestAnimationFrame(sweep);
});
</script>

<template>
  <section ref="root" class="quote-band" aria-label="Quote" @pointermove="move" @pointerleave="calm">
    <blockquote>
      <p ref="line" class="quote" @click="toast(t('quote.note'))"><SplitText :text="t('quote.text')" /></p>
      <cite><b>VAI_PROG</b>, {{ t("quote.cite") }}</cite>
    </blockquote>
  </section>
</template>

<style scoped>
.quote-band { padding: clamp(5rem, 12vw, 9rem) 1.25rem; text-align: center; background: var(--hot); color: #fff; overflow: clip; border-block: 4px solid #1d0f2e; }
blockquote { margin: 0 auto; max-width: 70rem; }
.quote { margin: 0 0 1.4rem; font: 900 clamp(2rem, 5.4vw, 4.4rem)/1.1 var(--display); letter-spacing: -.04em; cursor: default; }
.quote :deep(.c) { will-change: transform; }
cite { font-style: normal; }
</style>
