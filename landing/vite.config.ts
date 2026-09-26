import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// The landing builds twice: the browser bundle into dist/, and a server
// bundle that scripts/prerender.ts uses to write the English page into
// dist/index.html, so search engines and readers without scripts get every
// word. The browser then takes the page over where it stands.
export default defineConfig({
  plugins: [vue()],
  base: "/",
  build: {
    target: "es2022",
    assetsDir: "assets/app",
    cssCodeSplit: true,
    chunkSizeWarningLimit: 800,
  },
  server: { port: 4300, host: "127.0.0.1" },
});
