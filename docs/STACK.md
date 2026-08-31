# Stack

English version. Russian translation: [STACK.md](ru/STACK.md)

Every technology here was chosen for a reason, and every version was verified against
crates.io, npm or the official release feed on 2026-08-30. Nothing in this file is
recalled from memory.

This document explains **why**. If you want to know **what is being built and when**, read
[ROADMAP.md](ROADMAP.md).

---

## The one decision that matters most

**We render on write and serve on read.**

Almost every wiki in existence renders the page when you ask for it. That is why they are
slow, and it is why they need so much hardware. We do the opposite.

```text
WRITE PATH (async)                READ PATH (synchronous, Rust only)
  save or publish                   GET /wiki/Page
      |                                  |
  render job on a queue             render_cache lookup by content hash
      |                                  |
  Markdown pipeline (Rust)          HTML, ETag, Brotli, done
      |
  component SSR (Bun worker)
      |
  sanitize, cache, done
```

The consequences are the whole point of the project:

- A cache hit does a lookup and a response. No Markdown parsing, no templating, no
  database round trip for content.
- A cache miss renders in the background. Readers keep getting the previous, perfectly
  valid version and never wait.
- The Bun worker, the heaviest part of the system, is only ever reached from a background
  job. If it is down, slow, or not installed, reading is unaffected.
- Publishing a template enqueues re-renders for exactly the pages that depend on it,
  never the whole cache.

Everything else in this file is in service of that.

---

## Serving tier: Rust

| Choice | Why |
|--------|-----|
| **Rust, edition 2024** | Memory safety without a garbage collector, zero cost abstractions, and the fastest possible tail latency. The reader path is the biggest part of our traffic and carries our hardest promises. |
| **Axum 0.8.9** | Built on Tower and Tokio, so the entire middleware ecosystem is available. Extractors make handlers clean. It is the most boring good choice in the Rust HTTP world, and boring is a feature. |
| **Tokio 1.53.1** | The industry standard async runtime with a work stealing scheduler. |
| **MiniJinja 2.24.0** | Jinja2 compatible, sandboxed, hot reloads in development. Skins are MiniJinja templates, so a skin author does not need to know Rust. |
| **pulldown-cmark 0.13.4** | CommonMark plus the extensions people actually use: tables, footnotes, task lists. |
| **ammonia 4.1.4** | Whitelist based HTML sanitizer. Every path that produces HTML goes through it. |
| **TailwindCSS 4** | JIT compiled, dark mode first, and the Typography plugin handles prose styling for rendered Markdown. |

Why not Node or Bun for the serving tier? Bun 1.4.0 is genuinely good and it is now
written in Rust, see the worker section. But it still executes JavaScript on
JavaScriptCore, which means a garbage collector on the hot path. Serving cached HTML is a
lookup and a response. Rust does that with no heap to collect and no allocator pauses to
hide. On a Raspberry Pi, that difference is the entire budget.

---

## Data tier: PostgreSQL 18

One database that does five jobs, instead of five databases that each do one job.

| Job | How |
|-----|-----|
| Relational core | Pages, revisions, users, memberships, categories, redirects |
| Flexible metadata | JSONB for per-wiki settings and plugin configuration |
| Full text search | `tsvector` plus `pg_trgm` for typo tolerance, no extra service |
| Cache and queues | paired with Valkey for the hot paths |
| Isolation | Row level security as a second line of defence behind application filters |

Why not MongoDB? Wikis are deeply relational: pages have revisions, revisions have
authors, categories have parents, templates have dependents. Document storage is the wrong
shape.

Why not SQLite? It is excellent and we may support it for single user installs one day, but
full text search, JSONB indexing and concurrent writers across replicas are all harder there.

**SQLx 0.9.0** with the compile checked macros means a typo in a query is a compile error,
not a 3am page. The tradeoff is that builds need the database schema available, which we
solve with a committed `.sqlx` directory and `SQLX_OFFLINE=true` in CI.

---

## Cache tier: Valkey 9.1.1

Valkey is the community fork of Redis, and it is what we use for sessions, the render
cache, rate limit counters and the render job queue.

Why Valkey rather than Redis? It is actively developed under a genuinely open governance
model, and it is protocol compatible, so every Redis client works unchanged. Version 9.1.1
matters specifically because it is a security release fixing a use after free in TLS
connection handling.

Valkey is also why the web tier can be stateless. Sessions and cache live outside the
process, so any replica can serve any request and replicas can be added freely.

---

## Content model: namespaces and transclusion

This is the part most wiki engines get wrong, and the part we care most about.

Most Markdown wikis give you a flat list of pages. Real fan wikis run on templates: one
infobox, edited once, included on hundreds of pages. If your templates live in files, then
editing one is a deploy. We put them in the database.

| Namespace | Purpose |
|-----------|---------|
| `Main` | Articles |
| `Talk` | Discussion attached to a page |
| `Template` | Reusable markup, transcluded with `{{Name\|arg=value}}` |
| `Module` | Interactive components in TypeScript or Vue SFC |
| `File` | Media description pages |
| `User` | User home pages |
| `Project` | Policies and help |
| `Category` | Category pages |

Guards that make it safe: inclusion depth is capped, cycles are detected and broken with a
visible marker, editing `Template:` and `Module:` requires elevated rights because one bad
edit can break every page that includes it, and a dependency graph means publishing a
template invalidates exactly the pages that use it.

---

## Components: admins can build real widgets

In MediaWiki, adding an interactive widget means writing PHP and finding an operator to
deploy it. Here an admin writes a component in the browser.

```vue
<script setup lang="ts">
interface Props { name: string; avatar?: string; debut?: string }
const props = defineProps<Props>()
const expanded = ref(false)
</script>

<template>
  <aside class="infobox">
    <img v-if="avatar" :src="avatar" :alt="name">
    <h3>{{ name }}</h3>
    <button class="more" @click="expanded = !expanded">Details</button>
  </aside>
</template>
```

The build pipeline:

1. Saved in the `Module:` namespace as a new immutable version.
2. Compiled and bundled by **rolldown 1.2.6**, producing an SSR bundle and a client bundle.
3. Server rendered in the worker, sanitized by Rust, and cached.
4. Shipped with an SRI hash, loaded only on pages that use it, and only if JavaScript is
   available.

So the component is fully visible to search engines and to readers with JavaScript
disabled, and becomes interactive when JavaScript is there. That is the same
progressive enhancement contract as the rest of the engine.

There is a second, plainer flavour called `plain-ts`, a single `render(props): string`
function executed in an embedded Rust JavaScript engine. It exists so that an install with
no worker at all still gets components, at the cost of syntax and interactivity.

---

## Worker tier: Bun 1.4.0

Bun is an async worker. It is never in the request path.

**Bun 1.4.0 is written in Rust.** That is not a rumour, it is the headline of the official
release post from 2026-08-20. It removes the main objection to adding a second runtime to a
Rust project: there is no second toolchain to care about, because both are Rust.

What the worker does:

| Job | Built in API |
|-----|--------------|
| Component bundling | rolldown, plus the Vue SFC compiler |
| Component server rendering | Vue SSR |
| Diagram rendering | `Bun.WebView`, headless, replaces Puppeteer |
| Image thumbnails and WebP or AVIF | `Bun.Image`, replaces sharp |
| Scheduled jobs: digests, cache warm, reindex | `Bun.cron()` |
| Import and export tooling | plus `Bun.markdown` where consistency does not matter |

Verified numbers from the official post, quoted rather than guessed: Linux start in 5.1 ms
versus 10.9 ms for Bun 1.3 and 27.2 ms for Node.js 26, idle CPU 5x lower than Bun 1.3, and
`Bun.serve` peaking at 36 MB under load.

Two things we want to be precise about, because they are easy to get wrong:

- Bun still executes JavaScript on **JavaScriptCore**, so a garbage collector still exists.
  mimalloc improves memory reclamation, it does not remove collection. That is the real
  reason the worker stays off the request path.
- `bun build --compile --asset` can pack the worker and its assets into a single
  executable, so a host needs neither Node nor Bun installed.

`Bun.markdown` is deliberately not used for articles. `naw-markdown` with pulldown-cmark is
the single source of truth, so the web tier, the CLI, the worker and the editor preview can
never disagree about how a page renders.

---

## Authoring tier: Vue 3 and Vite

| Choice | Version | Why |
|--------|---------|-----|
| Vue 3 | 3.5.42 | The editor, the admin panel, the component editor and the skin editor |
| Vite | 8.2.2 | Builds to plain static assets that Rust serves |
| TipTap | 3.30.5 | Block editor on ProseMirror: tables, embeds, undo, collaboration later |
| rolldown-vite | 7.3.1 | Rust bundler behind Vite, same core as the component pipeline |

We dropped our original plan to build the editor in Rust compiled to WebAssembly. Its
strengths are server rendering and hydration, and we use neither, because the reader path
is Rust and zero JavaScript. What we actually need is a client application with a rich text
editor, a form library, drag and drop, and accessible components. Building all of that from
scratch in Rust would take months and produce something worse than what Vue already has.

The authoring app is loaded only when someone clicks Edit or opens the admin panel. It is
never on the reader path.

---

## Interfaces that stay swappable

Three capabilities sit behind Rust traits, so an install only pays for what it uses.

| Trait | Default | Alternatives |
|-------|---------|--------------|
| `SearchBackend` | PostgreSQL full text search | Meilisearch 1.53.1, Tantivy 0.26.1 |
| `StorageBackend` | Local disk | Any S3 compatible store via object_store 0.14.1 |
| `DiagramBackend` | Mermaid in `Bun.WebView` | graphviz, PlantUML |

A single node wiki runs PostgreSQL and nothing else. A large one adds Meilisearch and S3.
The application code does not change.

---

## Operations

| Component | Choice | Why |
|-----------|--------|-----|
| Containers | Docker plus compose | One command for the whole dev stack |
| Reverse proxy | Caddy 2 | Automatic TLS, HTTP/3, a config you can read |
| Tunnel | frp | Publishes a home server without port forwarding |
| CI | GitHub Actions | fmt, clippy, tests, SQLx offline check |
| Metrics | `/metrics` text endpoint | Prometheus can scrape it, dashboards are optional |
| Logging | tracing plus tracing-subscriber | Structured JSON, span based, OpenTelemetry ready |
| Backups | pg_dump plus restic | Automated, encrypted, with retention |

---

## What we deliberately did not choose

| Not chosen | Why not |
|------------|---------|
| Leptos or any Rust to WASM frontend | Its SSR and hydration are unused here, and the ecosystem for a block editor and admin panel does not exist in Rust |
| Meilisearch as the only search engine | It is a second database to run, and it does not fit the Raspberry Pi target |
| MinIO as a required dependency | A single node wiki should not need six containers to store a PNG |
| Redis instead of Valkey | Valkey is the actively developed fork under open governance, and it is protocol compatible |
| A WASM plugin system | Months of work on sandboxing and capabilities, for integrations that components already cover |
| Node.js for the worker | Bun starts 5.3x faster on Linux and ships the built ins we would otherwise install |
| Microservices everywhere | Three tiers with clear boundaries, not fifteen services |

---

## How it scales

Every tier has its own resource envelope, its own replica count and its own failure domain.

```text
                     readers (almost all traffic)
                                |
                   [ Caddy or CDN edge: Brotli, HTTP/3 ]
                                |
                 +--------------+--------------+
                 |                             |
      [ naw-web: N replicas ]        [ worker: M replicas ]
        Rust, Axum, stateless          Bun 1.4.0, async
        cached HTML plus full API      bundling, SSR, diagrams
                 |                             |
                 +------+---------+------------+
                        |         |
               [ PostgreSQL 18 ] [ Valkey 9 ]
                                       |
                        [ object storage: disk or S3 ]
```

- `naw-web` is stateless. Sessions, cache and queues live in Valkey, so any replica serves
  any request.
- The worker is only reached through a queue. Adding workers is a replica count change.
- Rendered output is content addressed, so any worker can produce it and any web replica
  can serve it.
- No worker at all means stale but correct HTML, never an error page.

---

## Verified versions

| Component | Version | Component | Version |
|-----------|---------|-----------|---------|
| Rust | 1.98.0, edition 2024 | Axum | 0.8.9 |
| PostgreSQL | 18 | Tokio | 1.53.1 |
| SQLx | 0.9.0 | Valkey | 9.1.1 |
| redis crate | 1.6.0 | MiniJinja | 2.24.0 |
| pulldown-cmark | 0.13.4 | ammonia | 4.1.4 |
| Vue | 3.5.42 | Vite | 8.2.2 |
| TipTap | 3.30.5 | Bun | 1.4.0 |
| rolldown | 1.2.6 | rolldown-vite | 7.3.1 |
| object_store | 0.14.1 | tower-governor | 0.8.0 |
| Meilisearch | 1.53.1 | Tantivy | 0.26.1 |
| boa_engine | 0.22.0 | utoipa | 5.5.0 |

Pinned and boring beats new and surprising.
