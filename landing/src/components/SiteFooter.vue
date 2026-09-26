<script setup lang="ts">
import { t } from "../i18n";
</script>

<template>
  <footer class="foot">
    <div class="wrap inner">
      <div>
        <p class="h">{{ t("footer.engine") }}</p>
        <p>{{ t("footer.engine_text") }}</p>
      </div>
      <div>
        <p class="h">{{ t("footer.contact") }}</p>
        <p><a href="mailto:vadim@filian.wiki">vadim@filian.wiki</a><br /><a href="mailto:vadim@snackers.wiki">vadim@snackers.wiki</a><br /><a href="https://vai-rice.space">vai-rice.space</a></p>
      </div>
      <p class="legal">{{ t("footer.legal") }}</p>
    </div>
  </footer>
</template>

<style scoped>
.foot { position: relative; z-index: 1; padding: 3rem 0 8rem; border-top: 3px solid var(--ink); background: var(--bg-2); }
.inner { display: grid; grid-template-columns: 2fr 1fr; gap: 1.5rem 3rem; }
.h { margin: 0 0 .4rem; font: 800 .86rem/1 var(--display); letter-spacing: -.005em; color: var(--muted); }
p { margin: 0; color: var(--ink-soft); }
.legal { grid-column: 1 / -1; padding-top: 1.5rem; border-top: 1px solid var(--line); font-size: .9rem; }
@media (max-width: 700px) { .inner { grid-template-columns: 1fr; } }
</style>
