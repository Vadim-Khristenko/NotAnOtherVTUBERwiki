/* ---------------------------------------------------------------------------
   Languages, loaded on the fly.

   public/assets/i18n/languages.json lists them; <code>.json holds the strings.
   Adding a language is one file and one line there, no build: the switcher
   reads the list at runtime and the strings arrive with the first click.
   Every language is also prerendered into its own page with its strings
   inside (scripts/prerender.ts), and the server hands out the right one
   from the first byte, so a reload paints in the reader's language at once
   and nothing swaps after. English is built in, and any key a translation
   leaves out reads in English.
   --------------------------------------------------------------------------- */
import { reactive, readonly } from "vue";
import en from "../public/assets/i18n/en.json";

type Value = string | string[];
export type Dict = Record<string, Value>;
export type Lang = { code: string; name: string };

const state = reactive({
  lang: "en",
  dict: en as Dict,
  languages: [{ code: "en", name: "English" }, { code: "ru", name: "Русский" }] as Lang[],
});
export const i18n = readonly(state);

export function t(key: string, vars?: Record<string, string | number>): string {
  const v = state.dict[key] ?? (en as Dict)[key];
  let s = typeof v === "string" ? v : key;
  if (vars) for (const [k, val] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, String(val));
  return s;
}

export function tList(key: string): string[] {
  const v = state.dict[key] ?? (en as Dict)[key];
  return Array.isArray(v) ? v : [];
}

const base = "/assets/i18n/";
async function fetchJson<T>(name: string): Promise<T | null> {
  try {
    const r = await fetch(base + name, { cache: "no-cache" });
    return r.ok ? ((await r.json()) as T) : null;
  } catch {
    return null;
  }
}

/** Sets the strings without fetching: for the prerender, and for taking over a prerendered page. */
export function useDictionary(code: string, dict: Dict) {
  state.dict = dict;
  state.lang = code;
}

/** The language a prerendered page was written in, with its strings, if it carries them. */
export function adoptPrerendered() {
  const el = document.getElementById("naw-i18n");
  if (!el?.textContent) return;
  try {
    const { lang, dict } = JSON.parse(el.textContent) as { lang: string; dict: Dict };
    useDictionary(lang, dict);
  } catch {}
}

function remember(code: string) {
  try { localStorage.setItem("naw-lang", code); } catch {}
  // the server reads this to send the right page on the next visit
  document.cookie = `naw_lang=${code}; path=/; max-age=31536000; samesite=lax`;
}

export async function setLanguage(code: string) {
  const dict = code === "en" ? (en as Dict) : await fetchJson<Dict>(`${code}.json`);
  if (!dict) return;
  useDictionary(code, dict);
  document.documentElement.lang = code;
  const title = dict["meta.title"] ?? (en as Dict)["meta.title"];
  if (typeof title === "string") document.title = title;
  remember(code);
}

/** In the browser, once: the list of languages, and the reader's choice. */
export async function startLanguages() {
  const list = await fetchJson<Lang[]>("languages.json");
  if (list?.length) state.languages = list;
  const codes = state.languages.map((l) => l.code);
  const asked = new URLSearchParams(location.search).get("lang");
  let stored: string | null = null;
  try { stored = localStorage.getItem("naw-lang"); } catch {}
  const browser = (navigator.languages ?? [navigator.language]).map((l) => (l || "").toLowerCase().split("-")[0]);
  const pick = [asked, stored, ...browser].find((c): c is string => !!c && codes.includes(c)) ?? "en";
  // Normally the server already sent this language; swap only if it could not
  // (a language added after the last build, or no server-side choice at all).
  if (pick !== state.lang) await setLanguage(pick);
  else remember(pick);
}
