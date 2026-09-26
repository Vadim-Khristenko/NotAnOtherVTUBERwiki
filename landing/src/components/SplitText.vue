<script setup lang="ts">
/* Text split into words and letters, so each letter can move on its own.
   Understands <em> in a translation; anything else is shown as plain text. */
import { computed } from "vue";

const props = defineProps<{ text: string; tag?: string }>();
type Piece = { word: string; em: boolean; start: number };
const pieces = computed(() => {
  const out: Piece[] = [];
  let i = 0;
  for (const part of props.text.split(/(<em>[\s\S]*?<\/em>)/)) {
    if (!part) continue;
    const em = part.startsWith("<em>");
    const plain = part.replace(/<\/?em>/g, "");
    for (const word of plain.split(/\s+/).filter(Boolean)) {
      out.push({ word, em, start: i });
      i += word.length;
    }
  }
  return out;
});
const label = computed(() => props.text.replace(/<[^>]+>/g, ""));
</script>

<template>
  <component :is="tag ?? 'span'" class="split">
    <span class="visually-hidden">{{ label }}</span>
    <span aria-hidden="true">
      <template v-for="(p, n) in pieces" :key="n">
        <span class="w" :class="{ em: p.em }"><span v-for="(ch, k) in [...p.word]" :key="k" class="c" :style="{ '--i': p.start + k }">{{ ch }}</span></span>{{ " " }}
      </template>
    </span>
  </component>
</template>

<style scoped>
.w { display: inline-block; white-space: nowrap; }
.c { display: inline-block; transform-origin: 50% 100%; }
.em { color: var(--hot); }
</style>
