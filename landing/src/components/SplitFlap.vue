<script setup lang="ts">
/* An airport split-flap board. The grid is fixed (rows × cols), so a new word
   never changes the size of anything: letters flip through a few random
   characters and land, one cell after another. It only turns while it is on
   screen, and its letter size is set once per resize rather than by a
   container query, which made every flip re-measure the whole page. */
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { reducedMotion } from "../lib/motion";

const props = withDefaults(defineProps<{ words: string[]; cols?: number; rows?: number; every?: number }>(), { cols: 20, rows: 2, every: 2600 });
const cells = ref<string[]>(layout(props.words[0] ?? ""));
const flipping = ref<boolean[]>(Array(props.cols * props.rows).fill(false));
const current = ref(0);
const board = ref<HTMLElement | null>(null);
const fontPx = ref<number | null>(null);
const NOISE = "ABCDEFGHIJKLMNOPQRSTUVWXYZАБВГДЕЖЗИКЛМНОПРСТУФХЦЧШЩЭЮЯ0123456789";

/** Words wrapped into the rows, each row centred. */
function layout(text: string): string[] {
  const lines: string[] = [];
  let line = "";
  for (const word of text.toUpperCase().split(/\s+/)) {
    const next = line ? `${line} ${word}` : word;
    if (next.length <= props.cols) line = next;
    else { if (line) lines.push(line); line = word.slice(0, props.cols); }
  }
  if (line) lines.push(line);
  const used = lines.slice(0, props.rows);
  const top = Math.floor((props.rows - used.length) / 2);
  const out: string[] = Array(props.cols * props.rows).fill(" ");
  used.forEach((l, r) => {
    const left = Math.floor((props.cols - l.length) / 2);
    [...l].forEach((ch, c) => (out[(r + top) * props.cols + left + c] = ch));
  });
  return out;
}

const timers: number[] = [];
function show(text: string, animate = true) {
  const target = layout(text);
  timers.splice(0).forEach(clearTimeout);
  target.forEach((ch, i) => {
    if (!animate || reducedMotion()) { cells.value[i] = ch; return; }
    if (cells.value[i] === ch && ch === " ") return;
    const steps = 3 + Math.floor(Math.random() * 5);
    const delay = (i % props.cols) * 28 + Math.floor(i / props.cols) * 90;
    for (let s = 0; s <= steps; s++) {
      timers.push(window.setTimeout(() => {
        cells.value[i] = s === steps ? ch : NOISE[Math.floor(Math.random() * NOISE.length)];
        flipping.value[i] = false;
        requestAnimationFrame(() => (flipping.value[i] = true));
      }, delay + s * 60));
    }
  });
}

let loop = 0;
let visible = false;
function run() {
  clearInterval(loop);
  if (!visible || reducedMotion() || props.words.length < 2) return;
  loop = window.setInterval(() => {
    current.value = (current.value + 1) % props.words.length;
    show(props.words[current.value]);
  }, props.every);
}
function start() {
  current.value = 0;
  if (props.words.length) show(props.words[0], false);
  run();
}
let ro: ResizeObserver | null = null;
let io: IntersectionObserver | null = null;
onMounted(() => {
  const el = board.value;
  if (el) {
    ro = new ResizeObserver(([e]) => { fontPx.value = Math.round((e.contentRect.width / props.cols) * 0.62 * 10) / 10; });
    ro.observe(el);
    io = new IntersectionObserver(([e]) => { visible = e.isIntersecting; run(); });
    io.observe(el);
  }
  start();
});
watch(() => props.words.join("|"), start);
onBeforeUnmount(() => { clearInterval(loop); timers.forEach(clearTimeout); ro?.disconnect(); io?.disconnect(); });
</script>

<template>
  <div ref="board" class="board" :style="{ '--cols': cols, '--fs': fontPx ? `${fontPx}px` : undefined }" role="img" :aria-label="words.join(', ')">
    <span v-for="(ch, i) in cells" :key="i" class="cell" :class="{ flip: flipping[i], blank: ch === ' ' }"><span>{{ ch }}</span></span>
  </div>
</template>

<style scoped>
.board { display: grid; grid-template-columns: repeat(var(--cols), minmax(0, 1fr)); gap: 3px; padding: 8px; background: #0d0616; border: 3px solid #1d0f2e; box-shadow: var(--lift); }
.cell { position: relative; display: grid; place-items: center; aspect-ratio: 3 / 4.2; background: linear-gradient(#2a1d3d 49.5%, #0d0616 49.5% 51%, #231637 51%); color: var(--cream); font: 800 var(--fs, clamp(.55rem, 1.6vw, 1.05rem))/1 var(--mono); overflow: hidden; perspective: 200px; contain: layout paint style; }
.cell.blank { color: transparent; }
.cell span { display: block; }
.cell.flip span { animation: flap .12s ease-out; }
@keyframes flap { from { transform: rotateX(-90deg); opacity: .4; } }
</style>
