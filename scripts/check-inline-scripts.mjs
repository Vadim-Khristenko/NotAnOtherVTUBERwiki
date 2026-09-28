// Parses every inline <script> in the skins and fails on the first one that
// does not parse. A broken inline script breaks silently: the page still
// renders, and every feature the script carries (the editor toolbar, live
// preview, image paste, drafts) is simply gone. It happened once, for days.
//
// Template tags are replaced before parsing: {{ expr }} by 0, {% ... %} and
// {# ... #} by nothing. A script that only parses with its template values in
// place is rare enough to fix by hand if this ever complains about one.
//
// usage: node scripts/check-inline-scripts.mjs [skins-dir]
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const root = process.argv[2] ?? "skins";
const files = [];
const walk = (dir) => {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) walk(path);
    else if (name.endsWith(".html")) files.push(path);
  }
};
walk(root);

let scripts = 0;
let broken = 0;
for (const file of files) {
  const html = readFileSync(file, "utf8");
  const pattern = /<script(\s[^>]*)?>([\s\S]*?)<\/script>/g;
  let match;
  while ((match = pattern.exec(html))) {
    const attrs = match[1] ?? "";
    // JSON data blocks and external scripts are not code to parse here
    if (/type=["']application\/(ld\+)?json["']/.test(attrs) || /\ssrc=/.test(attrs)) continue;
    scripts++;
    const code = match[2]
      .replace(/\{\{[\s\S]*?\}\}/g, "0")
      .replace(/\{%[\s\S]*?%\}/g, "")
      .replace(/\{#[\s\S]*?#\}/g, "");
    try {
      new Function(code);
    } catch (err) {
      broken++;
      const line = html.slice(0, match.index).split("\n").length;
      console.log(`::error file=${file},line=${line}::inline script does not parse: ${err.message}`);
    }
  }
}
console.log(`${scripts} inline scripts in ${files.length} templates, ${broken} broken`);
process.exit(broken ? 1 : 0);
