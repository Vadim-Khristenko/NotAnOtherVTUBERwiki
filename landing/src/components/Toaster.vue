<script setup lang="ts">
import { toasts } from "../lib/motion";
</script>

<template>
  <div role="status" aria-live="polite">
    <Transition name="toast"><p v-if="toasts.text" :key="toasts.id" class="toast">{{ toasts.text }}</p></Transition>
  </div>
</template>
