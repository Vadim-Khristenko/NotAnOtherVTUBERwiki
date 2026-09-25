// Keeps this wiki's emote files in the browser, so a page with hundreds of
// emotes does not ask the server for each of them on every visit.
//
// Only GET requests for /media/emotes/ are touched; everything else goes to
// the network as if this worker were not here. The cache is named after the
// version the page registered it with (/emote-cache.js?v=N). The server bumps
// N after an emote sync or when an admin asks, the browser installs the new
// worker, and the new worker deletes every older emote cache. A picture that
// fails to decode is refetched once from the network: the page asks for it
// again with ?fresh=1, which skips the cache and replaces the stored copy.

const VERSION = new URL(self.location.href).searchParams.get("v") || "0";
const PREFIX = "naw-emotes-";
const CACHE = PREFIX + VERSION;

self.addEventListener("install", () => self.skipWaiting());

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys
            .filter((key) => key.startsWith(PREFIX) && key !== CACHE)
            .map((key) => caches.delete(key)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin || !url.pathname.startsWith("/media/emotes/")) return;
  // One cache entry per file, whatever the query.
  const key = url.origin + url.pathname;
  const fresh = url.searchParams.has("fresh");
  event.respondWith(
    caches.open(CACHE).then(async (cache) => {
      if (!fresh) {
        const hit = await cache.match(key);
        if (hit) return hit;
      }
      const response = await fetch(key, { cache: fresh ? "reload" : "default" });
      const type = response.headers.get("content-type") || "";
      if (response.ok && type.startsWith("image/")) {
        await cache.put(key, response.clone());
      } else if (fresh) {
        await cache.delete(key);
      }
      return response;
    }),
  );
});
