import { createSSRApp } from "vue";
import { renderToString } from "vue/server-renderer";
import App from "./App.vue";
import { useDictionary, type Dict } from "./i18n";

/** The page as it reads in one language, before any script runs. */
export async function render(lang: string, dict: Dict) {
  useDictionary(lang, dict);
  return renderToString(createSSRApp(App));
}
