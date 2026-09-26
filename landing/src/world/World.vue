<script setup lang="ts">
/* The fixed canvas behind the whole page. The 3D code loads after the page
   is up, and only where it should: not with reduced motion, not on Save-Data,
   not without WebGL. Everything on the page works the same without it. */
import { onMounted, ref } from "vue";
import { t } from "../i18n";
import { reducedMotion, toast, confetti } from "../lib/motion";

const host = ref<HTMLElement | null>(null);
function webgl() {
  try { const c = document.createElement("canvas"); return !!(c.getContext("webgl2") || c.getContext("webgl")); } catch { return false; }
}
onMounted(() => {
  const saveData = (navigator as unknown as { connection?: { saveData?: boolean } }).connection?.saveData;
  if (!host.value || reducedMotion() || saveData || !webgl()) return;
  const go = async () => {
    try {
      const { start } = await import("./world");
      await start(host.value!, {
        boop: () => toast(t("egg.boop")),
        dizzy: () => { toast(t("egg.dizzy")); confetti(["star", "cat", "ring"], 14); },
        // the logo sticker steps back only once the cat is actually drawn
        ready: () => document.documentElement.classList.add("has-3d"),
      });
    } catch (err) {
      console.warn("the 3D world could not start; the page stays as it is", err);
    }
  };
  const idle = (window as unknown as { requestIdleCallback?: (f: () => void, o?: object) => void }).requestIdleCallback;
  if (idle) idle(go, { timeout: 1200 }); else setTimeout(go, 300);
});
</script>

<template>
  <div ref="host" class="world" aria-hidden="true"></div>
</template>

<style scoped>
.world { position: fixed; inset: 0; z-index: 0; pointer-events: none; }
.world :deep(canvas) { display: block; width: 100%; height: 100%; }
</style>
