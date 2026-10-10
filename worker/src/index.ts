// NotAnotherWiki optional async worker. Never on the request path:
// reading works without this process, stale but correct.
//
// The engine calls it from background jobs only, with a timeout, and
// sanitizes every answer. It holds no database credentials and no secrets.

import { DiagramError, mermaidReady, render, type Lang, type Theme } from "./diagram";
import { IMAGE_MAX, ImageError, shrink } from "./image";

// The engine relies on Bun features past 1.4.0, so refuse to boot older.
if (!Bun.semver.satisfies(Bun.version, ">=1.4.2")) {
  console.error(`naw-worker needs Bun >= 1.4.2, found ${Bun.version}`);
  process.exit(1);
}

const port = Number(process.env.WORKER_PORT ?? 8081);
// Loopback unless told otherwise: the worker has no authentication of its own.
const hostname = process.env.WORKER_HOST ?? "127.0.0.1";

const LANGS: readonly Lang[] = ["mermaid", "dot"];
const THEMES: readonly Theme[] = ["light", "dark"];

Bun.serve({
  port,
  hostname,
  // Pictures are the largest bodies; each route checks its own limit too.
  maxRequestBodySize: IMAGE_MAX + 1024 * 1024,
  async fetch(req) {
    const url = new URL(req.url);
    if (url.pathname === "/health") {
      return Response.json({ status: "ok", diagrams: { dot: true, mermaid: mermaidReady() } });
    }
    if (url.pathname === "/render/diagram" && req.method === "POST") {
      return renderDiagram(req);
    }
    if (url.pathname === "/render/image" && req.method === "POST") {
      return renderImage(req, Number(url.searchParams.get("width")));
    }
    return new Response("not found", { status: 404 });
  },
});

async function renderDiagram(req: Request): Promise<Response> {
  let body: unknown;
  try {
    body = await req.json();
  } catch {
    return Response.json({ error: "the body is not JSON" }, { status: 400 });
  }
  const { lang, theme, source } = (body ?? {}) as Record<string, unknown>;
  if (!LANGS.includes(lang as Lang) || !THEMES.includes(theme as Theme) || typeof source !== "string") {
    return Response.json({ error: "expected lang, theme and source" }, { status: 400 });
  }
  try {
    const drawing = await render(lang as Lang, theme as Theme, source);
    return Response.json(drawing);
  } catch (err) {
    if (err instanceof DiagramError) {
      // The author's mistake: the engine shows the source and stops retrying.
      return Response.json({ error: err.message }, { status: 422 });
    }
    console.error("diagram render failed", err);
    return Response.json({ error: "the worker could not draw it" }, { status: 500 });
  }
}

async function renderImage(req: Request, width: number): Promise<Response> {
  try {
    const bytes = new Uint8Array(await req.arrayBuffer());
    const out = await shrink(bytes, width);
    return new Response(out, { headers: { "content-type": "image/webp" } });
  } catch (err) {
    if (err instanceof ImageError) {
      return new Response(err.message, { status: 422 });
    }
    console.error("image shrink failed", err);
    return new Response("the worker could not shrink it", { status: 500 });
  }
}

console.log(`naw worker listening on ${hostname}:${port}`);
