// NotAnotherWiki optional async worker. Never on the request path:
// reading works without this process, stale but correct.

// The engine relies on Bun features past 1.4.0, so refuse to boot older.
if (!Bun.semver.satisfies(Bun.version, ">=1.4.2")) {
  console.error(`naw-worker needs Bun >= 1.4.2, found ${Bun.version}`);
  process.exit(1);
}

const port = Number(process.env.WORKER_PORT ?? 8081);

Bun.serve({
  port,
  fetch(req) {
    const url = new URL(req.url);
    if (url.pathname === "/health") {
      return Response.json({ status: "ok" });
    }
    return new Response("not found", { status: 404 });
  },
});

console.log(`naw worker listening on ${port}`);
