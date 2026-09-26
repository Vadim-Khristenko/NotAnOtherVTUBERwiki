// Writes one finished page per language from the server bundle: English into
// dist/index.html, every other language into dist/<code>/index.html with its
// strings embedded. The server picks the page by the reader's choice (cookie,
// ?lang, browser language), so the first paint is already in their language
// and search engines see every word in every language.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";

const { render } = await import(pathToFileURL(resolve("dist-ssr/entry-server.js")).href);
const shell = readFileSync("dist/index.html", "utf8");
if (!shell.includes("<!--app-html-->")) throw new Error("dist/index.html has no <!--app-html--> slot");
const languages: { code: string }[] = JSON.parse(readFileSync("public/assets/i18n/languages.json", "utf8"));
const en = JSON.parse(readFileSync("public/assets/i18n/en.json", "utf8"));
const attr = (s: string) => s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");
// The fonts come inline, and each page preloads the files its own letters need,
// so the text does not reflow when a script's subset arrives late.
const fontCss = readFileSync("public/assets/fonts/fonts.css", "utf8");
const fontFiles: Record<string, string[]> = {
  latin: ["unbounded-latin-3.woff2", "golos-text-latin-1.woff2"],
  cyrillic: ["unbounded-cyrillic-2.woff2", "golos-text-cyrillic-0.woff2"],
};
const scripts: Record<string, string[]> = { ru: ["latin", "cyrillic"], uk: ["latin", "cyrillic"], be: ["latin", "cyrillic"] };
const preloads = (code: string) =>
  (scripts[code] ?? ["latin"]).flatMap((k) => fontFiles[k]).map((f) => `<link rel="preload" href="/assets/fonts/${f}" as="font" type="font/woff2" crossorigin />`).join("\n");
const url = (code: string) => `https://filian.wiki/${code === "en" ? "" : code + "/"}`;
const alternates = [
  ...languages.map((l) => `<link rel="alternate" hreflang="${l.code}" href="${url(l.code)}" />`),
  `<link rel="alternate" hreflang="x-default" href="${url("en")}" />`,
].join("\n");

for (const { code } of languages) {
  const dict = code === "en" ? en : { ...en, ...JSON.parse(readFileSync(`public/assets/i18n/${code}.json`, "utf8")) };
  const html = await render(code, dict);
  const title = attr(dict["meta.title"]);
  const description = attr(dict["meta.description"]);
  const share = attr(dict["meta.share"]);
  let page = shell
    .replace(/<html lang="[^"]*"/, `<html lang="${code}"`)
    .replace(/<title>[^<]*<\/title>/, `<title>${title}</title>`)
    .replace(/<meta name="description" content="[^"]*"/, `<meta name="description" content="${description}"`)
    .replace(/<meta property="og:title" content="[^"]*"/, `<meta property="og:title" content="${title}"`)
    .replace(/<meta property="og:description" content="[^"]*"/, `<meta property="og:description" content="${share}"`)
    .replace(/<meta property="og:url" content="[^"]*"/, `<meta property="og:url" content="${url(code)}"`)
    .replace(/<link rel="canonical" href="[^"]*" \/>/, `<link rel="canonical" href="${url(code)}" />\n${alternates}`)
    .replace(/<link rel="preload" href="\/assets\/fonts\/[^"]+" as="font" type="font\/woff2" crossorigin \/>/, preloads(code))
    .replace(/<link rel="stylesheet" href="\/assets\/fonts\/fonts\.css" \/>/, `<style>${fontCss}</style>`)
    .replace("<!--app-html-->", html);
  // English is built into the script; other languages carry their strings, so nothing is fetched to start
  if (code !== "en") {
    const data = JSON.stringify({ lang: code, dict }).replace(/</g, "\\u003c");
    page = page.replace("</body>", `<script id="naw-i18n" type="application/json">${data}</script>\n</body>`);
  }
  const out = code === "en" ? "dist/index.html" : `dist/${code}/index.html`;
  if (code !== "en") mkdirSync(`dist/${code}`, { recursive: true });
  writeFileSync(out, page);
  console.log(`${code}: ${Math.round(page.length / 1024)} KB -> ${out}`);
}
