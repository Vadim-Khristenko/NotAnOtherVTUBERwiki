// Downloads the landing's two font families (SIL OFL 1.1) from Google Fonts
// once, so the page serves them itself: no third party on every visit, and
// nothing that breaks when Google is slow or blocked. Run with `bun run fonts`.
const families = "family=Unbounded:wght@500;700;900&family=Golos+Text:wght@400;500;700";
const keep = new Set(["latin", "cyrillic"]);
const ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36";
const css = await (await fetch(`https://fonts.googleapis.com/css2?${families}&display=swap`, { headers: { "user-agent": ua } })).text();

const out: string[] = ["/* Unbounded and Golos Text, SIL Open Font License 1.1. See fonts/OFL.txt. */"];
const saved = new Map<string, string>();
for (const block of css.matchAll(/\/\*\s*([a-z-]+)\s*\*\/\s*@font-face\s*\{([^}]*)\}/g)) {
  const [, subset, body] = block;
  if (!keep.has(subset)) continue;
  const url = body.match(/url\((https:[^)]+)\)/)![1];
  const family = body.match(/font-family:\s*'([^']+)'/)![1];
  let file = saved.get(url);
  if (!file) {
    file = `${family.toLowerCase().replace(/\s+/g, "-")}-${subset}-${saved.size}.woff2`;
    await Bun.write(`assets/fonts/${file}`, await (await fetch(url)).arrayBuffer());
    saved.set(url, file);
  }
  out.push(`@font-face {${body.replace(/url\([^)]+\)/, `url(/assets/fonts/${file})`)}}`);
}
await Bun.write("assets/fonts/fonts.css", out.join("\n") + "\n");
console.log(`${saved.size} files, ${out.length - 1} faces`);
