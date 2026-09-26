import { createSSRApp } from "vue";
import App from "./App.vue";
import { adoptPrerendered } from "./i18n";
import "./styles/base.css";

// The page arrives already rendered (see scripts/prerender.ts); this takes it over.
// It starts in the language the page was written in, so hydration matches the markup.
adoptPrerendered();
createSSRApp(App).mount("#app");
