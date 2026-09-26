<script setup lang="ts">
/* The wiki's six sections as a snack machine. Point at a slot and the screen
   says what is inside; press it (or its code on the keypad) and the coil
   turns, the snack drops into the tray, and then the wiki page opens.
   Without scripts every slot is a plain link. A finger has no hover, so on
   a touch screen the first tap opens the slot's label right where the
   finger is, and the second tap buys.

   The slots are the exact purple of the art, so the two drawings that stay
   pictures (Lore and Schedule, whose fine lines do not survive tracing) sit
   in them as if they were painted there. */
import { computed, ref } from "vue";
import { t } from "../i18n";
import { ICONS } from "../icons";
import { useFormation, useSeen, reducedMotion } from "../lib/motion";

type Slot = { code: string; key: string; icon: string; href: string; picture?: string };
const slots: Slot[] = [
  { code: "A1", key: "character", icon: "character", href: "https://alpha.filian.wiki/filian" },
  { code: "A2", key: "lore", icon: "lore", href: "https://alpha.filian.wiki/lore", picture: "/assets/art/tile-lore.webp" },
  { code: "A3", key: "glossary", icon: "glossary", href: "https://alpha.filian.wiki/glossary" },
  { code: "B1", key: "schedule", icon: "schedule", href: "https://alpha.filian.wiki/schedule", picture: "/assets/art/tile-schedule.webp" },
  { code: "B2", key: "clips", icon: "clips", href: "https://alpha.filian.wiki/clips" },
  { code: "B3", key: "fan_art", icon: "fan-art", href: "https://alpha.filian.wiki/fan-art" },
];
const root = ref<HTMLElement | null>(null);
const seen = useSeen(root, 0.15);
useFormation(root, "rain");
const active = ref<Slot | null>(null);
const vending = ref<string | null>(null);
const dropped = ref<Slot | null>(null);
const armed = ref<string | null>(null);
let pointer = "mouse";
const screen = computed(() => (active.value ? `${active.value.code} · ${t(`tile.${active.value.key}`)}: ${t(`tile.${active.value.key}_text`)}` : t("machine.led")));

function buy(slot: Slot, e?: MouseEvent) {
  if (e && (e.metaKey || e.ctrlKey || e.shiftKey || e.button === 1)) return; // a new tab: no show
  e?.preventDefault();
  if (vending.value) return;
  if (e && pointer === "touch" && armed.value !== slot.code) { armed.value = slot.code; active.value = slot; return; }
  armed.value = null;
  if (reducedMotion()) { location.href = slot.href; return; }
  vending.value = slot.code;
  active.value = slot;
  setTimeout(() => { dropped.value = slot; }, 650);
  setTimeout(() => { location.href = slot.href; }, 1500);
}
</script>

<template>
  <section id="wiki" ref="root" class="machine-section wrap" aria-labelledby="machine-title">
    <header class="section-head reveal" :class="{ 'is-in': seen }">
      <h2 id="machine-title" class="section-title">{{ t("machine.title") }}</h2>
      <p class="section-lede">{{ t("machine.lede") }}</p>
    </header>

    <div class="machine" :class="{ on: seen }">
      <div class="sign" aria-hidden="true"><span>{{ t("machine.brand") }}</span></div>
      <div class="glass">
        <ul class="slots" role="list">
          <li v-for="(s, i) in slots" :key="s.code" :style="{ '--i': i }">
            <a class="slot" :href="s.href" :class="{ vending: vending === s.code, gone: dropped?.code === s.code }"
               @pointerdown="pointer = $event.pointerType" @pointerenter="active = s" @focus="active = s" @pointerleave="active = null" @blur="active = null; armed = null" @click="buy(s, $event)">
              <span class="item">
                <img v-if="s.picture" :src="s.picture" alt="" width="190" height="125" loading="lazy" />
                <svg v-else :viewBox="ICONS[s.icon].viewBox" aria-hidden="true"><path class="tone" :d="ICONS[s.icon].tone" /><path class="ink" :d="ICONS[s.icon].ink" /></svg>
              </span>
              <svg class="coil" viewBox="0 0 160 34" aria-hidden="true"><path d="M4 17 C 14 -2, 24 -2, 24 17 S 34 36, 44 17 S 54 -2, 64 17 S 74 36, 84 17 S 94 -2, 104 17 S 114 36, 124 17 S 134 -2, 144 17 S 154 36, 158 20" /></svg>
              <span class="tag"><b>{{ s.code }}</b><span>{{ t(`tile.${s.key}`) }}</span><em>{{ t("machine.price") }}</em></span>
              <span class="visually-hidden">{{ t(`tile.${s.key}_text`) }}</span>
              <span v-if="armed === s.code && vending !== s.code" class="peek" aria-hidden="true">{{ t(`tile.${s.key}_text`) }}<b>{{ t("machine.again") }}</b></span>
            </a>
          </li>
        </ul>
        <span class="shine" aria-hidden="true"></span>
      </div>
      <aside class="panel">
        <p class="screen" aria-live="polite"><span :key="screen">{{ screen }}</span></p>
        <p class="keypad-label">{{ t("machine.keypad") }}</p>
        <div class="keypad">
          <button v-for="s in slots" :key="s.code" type="button" class="key" @click="buy(s)" @pointerenter="active = s" @pointerleave="active = null">{{ s.code }}</button>
        </div>
        <div class="coin" aria-hidden="true"><i></i><span>0 ADS</span></div>
      </aside>
      <div class="tray" aria-hidden="true">
        <span class="flap">{{ t("machine.tray") }}</span>
        <span v-if="dropped" :key="dropped.code" class="landed">
          <img v-if="dropped.picture" :src="dropped.picture" alt="" />
          <svg v-else :viewBox="ICONS[dropped.icon].viewBox"><path class="tone" :d="ICONS[dropped.icon].tone" /><path class="ink" :d="ICONS[dropped.icon].ink" /></svg>
        </span>
      </div>
    </div>
  </section>
</template>

<style scoped>
.machine-section { padding-block: clamp(4rem, 10vw, 8rem); }
.machine {
  position: relative; display: grid; grid-template-columns: minmax(0, 1fr) 15rem; grid-template-areas: "sign sign" "glass panel" "tray panel";
  gap: 1.2rem; padding: 1.2rem; background: var(--hot); border: 4px solid #1d0f2e; box-shadow: var(--lift), inset 0 0 0 6px color-mix(in srgb, #fff 18%, transparent);
  transform: perspective(1400px) rotateX(6deg) translateY(40px); opacity: 0; transition: transform 1.2s var(--ease), opacity .6s;
}
.machine.on { transform: none; opacity: 1; }
.sign { grid-area: sign; overflow: hidden; background: #1d0f2e; border: 3px solid #1d0f2e; padding: .7rem 1rem; }
.sign span { display: block; text-align: center; font: 900 clamp(1.3rem, 3vw, 2.2rem)/1 var(--display); letter-spacing: .12em; color: var(--cream); text-shadow: 0 0 12px var(--cream), 0 0 2px var(--cream); animation: neon 4s steps(1) infinite; }
@keyframes neon { 0%, 100% { opacity: 1; } 92% { opacity: .55; } 93% { opacity: 1; } 96% { opacity: .7; } }
.glass { grid-area: glass; position: relative; background: #140b22; border: 3px solid #1d0f2e; padding: 1rem; overflow: hidden; }
.shine { position: absolute; inset: 0; pointer-events: none; background: linear-gradient(115deg, transparent 30%, rgb(255 255 255 / .1) 38%, transparent 44%, transparent 60%, rgb(255 255 255 / .06) 64%, transparent 68%); }
.slots { list-style: none; margin: 0; padding: 0; display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 1rem .8rem; }
.slot { position: relative; display: grid; grid-template-columns: minmax(0, 1fr); grid-template-rows: auto auto auto; text-decoration: none; background: var(--art); border-bottom: 5px solid #0d0616; padding: .9rem .7rem .5rem; overflow: hidden; transition: box-shadow .2s; }
.slot:hover, .slot:focus-visible { box-shadow: inset 0 0 0 3px var(--cream); }
.item { display: grid; place-items: center; height: clamp(5rem, 9vw, 7rem); transition: transform .5s var(--spring); }
.item img, .item svg { max-height: 100%; width: auto; max-width: 100%; }
.item svg { height: 100%; }
.slot:hover .item { transform: translateY(-6px) rotate(-3deg) scale(1.05); }
.slot.vending .item { animation: fall 1s cubic-bezier(.55, 0, .85, .4) .45s forwards; }
@keyframes fall { 20% { transform: translateY(-4px) rotate(4deg); } 100% { transform: translateY(260%) rotate(40deg); opacity: 0; } }
.tone { fill: #bdb3cc; fill-rule: evenodd; }
.ink { fill: #fff; fill-rule: evenodd; }
.coil { width: 100%; height: 1.3rem; margin-top: .2rem; }
.coil path { fill: none; stroke: #d7d0e3; stroke-width: 3; stroke-linecap: round; stroke-dasharray: 10 6; }
.slot:hover .coil path, .slot.vending .coil path { animation: coil .6s linear infinite; }
@keyframes coil { to { stroke-dashoffset: -32; } }
.tag { display: flex; align-items: baseline; gap: .45rem; margin-top: .45rem; padding: .35rem .5rem; background: #0d0616; color: #fff; font: 800 .78rem/1.2 var(--display); }
.tag b { color: var(--cream); }
.tag span { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tag em { font-style: normal; color: var(--mint); font-size: .7rem; text-transform: uppercase; }
.js .slots li { opacity: 0; transform: translateY(24px) scale(.94); transition: transform .7s var(--ease), opacity .5s; transition-delay: calc(.3s + var(--i) * 70ms); }
.machine.on .slots li { opacity: 1; transform: none; }
.panel { grid-area: panel; display: grid; align-content: start; gap: .9rem; background: #1d0f2e; border: 3px solid #1d0f2e; padding: 1rem; color: #fff; }
.screen { margin: 0; min-height: 7.5rem; padding: .8rem; background: #0a1a12; color: #7cf2c9; border: 2px solid #0d0616; font: 600 .82rem/1.45 var(--mono); text-shadow: 0 0 6px #7cf2c9; overflow: hidden; background-image: repeating-linear-gradient(transparent 0 3px, rgb(0 0 0 / .25) 3px 4px); }
.screen span { display: block; animation: type .5s steps(20); }
@keyframes type { from { clip-path: inset(0 100% 0 0); } to { clip-path: inset(0 0 0 0); } }
.keypad-label { margin: 0; font: 800 .82rem/1 var(--display); letter-spacing: -.005em; color: var(--muted); }
.keypad { display: grid; grid-template-columns: repeat(3, 1fr); gap: .45rem; }
.key { padding: .7rem 0; background: #e8e1f0; color: #1d0f2e; border: 0; border-bottom: 4px solid #8a7fa0; font: 900 .9rem/1 var(--display); cursor: pointer; transition: transform .1s, border-width .1s; }
.key:hover { background: var(--cream); }
.key:active { transform: translateY(3px); border-bottom-width: 1px; }
.coin { display: flex; align-items: center; gap: .6rem; font: 800 .7rem/1 var(--display); color: var(--muted); }
.coin i { width: .6rem; height: 2.2rem; background: #0d0616; border: 2px solid #5a4a73; }
.tray { grid-area: tray; position: relative; height: 5rem; background: #0d0616; border: 3px solid #1d0f2e; overflow: hidden; display: grid; place-items: center; }
.flap { font: 800 .72rem/1 var(--display); letter-spacing: .14em; text-transform: uppercase; color: #5a4a73; }
.landed { position: absolute; inset: .4rem 30%; display: grid; place-items: center; background: var(--art); animation: land .6s var(--spring); }
.landed img, .landed svg { max-height: 100%; width: auto; }
/* what a touch shows before it buys: the label, over the snack, and how to take it */
.peek { position: absolute; inset: 0; display: grid; align-content: center; gap: .6rem; padding: .8rem .7rem; background: color-mix(in srgb, #0d0616 88%, transparent); color: #7cf2c9; font: 600 .78rem/1.4 var(--mono); text-align: left; animation: peek .25s var(--ease); }
.peek b { font: 800 .74rem/1.2 var(--display); color: var(--cream); }
@keyframes peek { from { opacity: 0; transform: translateY(8px); } }
@keyframes land { from { transform: translateY(-120%) rotate(-30deg); } }
@media (max-width: 860px) {
  /* one column: the screen above the glass and the tray right under it, so both stay near the slot you touched */
  .machine { grid-template-columns: minmax(0, 1fr); grid-template-areas: "sign" "panel" "glass" "tray"; box-shadow: var(--lift); }
  .slots { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .keypad { grid-template-columns: repeat(6, 1fr); }
}
@media (max-width: 560px) {
  .tag em { display: none; }
  .tag { flex-wrap: wrap; gap: .1rem .4rem; font-size: clamp(.62rem, 2.9vw, .72rem); letter-spacing: -.02em; }
  .tag span { flex-basis: 100%; white-space: normal; overflow: visible; hyphens: auto; }
  .slot { padding-inline: .5rem; }
  .screen { min-height: 4.2rem; }
  .keypad-label, .keypad, .coin { display: none; }
  .panel { padding: .7rem; }
}
</style>
