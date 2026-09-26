<script setup lang="ts">
/* ---------------------------------------------------------------------------
   FilianWIKI landing.

   Still reading the source? Nothing hidden here is a secret: no keys, no
   admin routes, just easter eggs and more animation than a wiki needs.
   One real warning, because it matters more than the jokes: if anybody
   tells you to paste something into the console for emotes, a giveaway or
   a mod role, they are stealing your account. We will never ask.
   --------------------------------------------------------------------------- */
import { onMounted } from "vue";
import { startLanguages, t } from "./i18n";
import { toast, confetti } from "./lib/motion";
import World from "./world/World.vue";
import SiteHeader from "./components/SiteHeader.vue";
import HeroSection from "./components/HeroSection.vue";
import ZeroDrums from "./components/ZeroDrums.vue";
import NotArgTapes from "./components/NotArgTapes.vue";
import SnackMachine from "./components/SnackMachine.vue";
import JoinSection from "./components/JoinSection.vue";
import RecipeCard from "./components/RecipeCard.vue";
import PassportControl from "./components/PassportControl.vue";
import TicketBoard from "./components/TicketBoard.vue";
import QuoteWave from "./components/QuoteWave.vue";
import EndCredits from "./components/EndCredits.vue";
import SiteFooter from "./components/SiteFooter.vue";
import CassettePlayer from "./components/CassettePlayer.vue";
import Toaster from "./components/Toaster.vue";

onMounted(() => {
  startLanguages();
  const konami = ["ArrowUp", "ArrowUp", "ArrowDown", "ArrowDown", "ArrowLeft", "ArrowRight", "ArrowLeft", "ArrowRight", "b", "a"];
  let step = 0, typed = "";
  addEventListener("keydown", (e) => {
    step = e.key === konami[step] ? step + 1 : e.key === konami[0] ? 1 : 0;
    if (step === konami.length) { step = 0; toast(t("egg.snack")); confetti(["snack", "paw", "heart", "cat"]); }
    if (e.key?.length === 1) {
      typed = (typed + e.key.toLowerCase()).slice(-7);
      if (typed.endsWith("filian")) { toast(t("egg.name")); confetti(["heart", "cat"]); }
      if (typed.endsWith("snack")) toast(t("egg.snack"));
    }
  });
  addEventListener("blur", () => (document.title = t("meta.away")));
  addEventListener("focus", () => (document.title = t("meta.title")));
  (window as unknown as { snack: () => string }).snack = () => { confetti(["snack", "star"]); toast(t("egg.snack")); return t("egg.console"); };
  if (new URLSearchParams(location.search).has("snack")) { toast(t("egg.snack")); confetti(["snack", "heart"]); }
  console.log("%cFilianWIKI", "color:#ff2d7a;font-weight:900;font-size:20px");
  console.log("%cFan project. Not affiliated with Filian, not part of any ARG.", "color:#a366ff");
  console.log("Code: https://github.com/Vadim-Khristenko/NotAnOtherVTUBERwiki  ·  try snack()");
  console.warn("Security, not a joke: if anyone tells you to paste something here for emotes, a giveaway or a mod role, they are stealing your account. We will never ask you to do that.");
});
</script>

<template>
  <a class="skip" href="#main">{{ t("nav.skip") }}</a>
  <div class="grain" aria-hidden="true"></div>
  <World />
  <SiteHeader />
  <main id="main">
    <HeroSection />
    <ZeroDrums />
    <NotArgTapes />
    <SnackMachine />
    <JoinSection />
    <RecipeCard />
    <PassportControl />
    <TicketBoard />
    <QuoteWave />
    <EndCredits />
  </main>
  <SiteFooter />
  <CassettePlayer />
  <Toaster />
</template>
