// Serves dist/ the way nginx does in production, for looking at it locally:
// "/" is answered with the page in the reader's language, picked from ?lang,
// then the naw_lang cookie, then Accept-Language, English otherwise.
import { existsSync, readFileSync } from "node:fs";

const root = new URL("../dist/", import.meta.url).pathname.replace(/^\/([A-Z]:)/, "$1");
const codes = (JSON.parse(readFileSync(root + "assets/i18n/languages.json", "utf8")) as { code: string }[]).map((l) => l.code);

function pick(req: Request, url: URL) {
  const asked = url.searchParams.get("lang");
  const cookie = /(?:^|;\s*)naw_lang=([a-z-]+)/.exec(req.headers.get("cookie") ?? "")?.[1];
  const accept = (req.headers.get("accept-language") ?? "").split(",").map((p) => p.trim().split(/[-;]/)[0].toLowerCase());
  return [asked, cookie, ...accept].find((c): c is string => !!c && codes.includes(c)) ?? "en";
}

Bun.serve({
  port: Number(process.env.PORT ?? 4300),
  hostname: "127.0.0.1",
  async fetch(req) {
    const url = new URL(req.url);
    let path = decodeURIComponent(url.pathname);
    if (path.includes("..")) return new Response("no", { status: 400 });
    if (path === "/") {
      const lang = pick(req, url);
      const file = lang === "en" || !existsSync(`${root}${lang}/index.html`) ? "index.html" : `${lang}/index.html`;
      return new Response(Bun.file(root + file), { headers: { "content-type": "text/html; charset=utf-8", vary: "Cookie, Accept-Language", "cache-control": "no-cache" } });
    }
    if (path.endsWith("/")) path += "index.html";
    const file = Bun.file(root + path.slice(1));
    return (await file.exists()) ? new Response(file) : new Response("not found", { status: 404 });
  },
});
console.log(`http://127.0.0.1:${process.env.PORT ?? 4300}`);
