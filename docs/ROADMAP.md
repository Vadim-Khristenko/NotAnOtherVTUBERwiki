# Roadmap

English version. Russian translation: [ROADMAP.md](ru/ROADMAP.md)

Where we are going, in what order, and what has to be true before we call a phase done.
If you want to know why the technology is what it is, read [STACK.md](STACK.md).

---

## Where we are

**Closed alpha.** There is no public release yet. What exists, as of 2026-09-28:

- A design that has survived one full review, a database schema, and the decisions written
  down with their reasons. This is still the most valuable part.
- A running engine, in a closed alpha at `alpha.filian.wiki`. Accounts are handed out by
  hand; `filian.wiki` explains how to ask for one. Pages render and serve from cache,
  history, diffs, restore and search work, and one wiki carries articles in several
  languages (`/ru/about`).
- Sign-in with a username and password, and through GitHub, Discord and Telegram OIDC.
  Registration can be closed, so providers only sign in people who already have an
  account. A browser without JavaScript (or in iPhone Lockdown Mode) can sign in through
  another device with a one-time code. Email verification is not built yet.
- An admin panel: accounts with per-person rights, sanctions and notes, pages, the audit
  log, wiki settings, languages, header and footer, error pages, 7TV emote sources.
- Images in articles, avatars, profiles, and curators who can protect pages and look
  after other people's profiles. Article text may be up to 5 MB and each image up to
  20 MB, counted apart, and search covers every word of even the longest article.
- 7TV emotes, written `:name:`, kept on the wiki's own storage so a reader never loads
  anything from 7TV, with a searchable list and a picker in the editor.
- Images can come from a link: the wiki downloads and keeps them, and outside images in
  an article are copied on save. Outside fetches never reach a private address and fall
  back to a proxy when a route is throttled.
- Templates with one code for every language: a translation of a template carries only
  its documentation and field labels, so no language can run a different template. Fields
  can be passed as a strict YAML block and declared in a `<params>` schema, from which the
  template's page builds its field list and ready examples. Pages and templates may write
  HTML from a fixed list of tags, with a safe subset of `style`.
- Reports: a reader can report a mistake, complain or suggest a change; moderators work
  through a queue, and the reporter sees their answer in settings.
- Drafts: the editor autosaves to the server while you write, and My drafts lists what
  you have not saved yet, on any device.
- The landing at `filian.wiki` and `snackers.wiki`, in Vue with a 3D scene and Snack
  Radio, prerendered per language.
- Cloudflare is DNS only on every domain, deliberately: its proxy ranges are blocked in
  Russia, and a large part of this community reads from there.

Editing a design document costs an afternoon. Editing a database schema after a thousand
wikis depend on it costs a year. That is why the order below looks slow at the start.

The first wiki built on this engine is **FilianWIKI**, for the Snackers, the community
around Filian. Some of its content currently lives on FANDOM and will be migrated later,
while the new wiki grows.

This is a fan project. It is not affiliated with Filian, not endorsed by her, and not part
of any ARG.

---

## What we are actually building

A wiki engine that is fast on a Raspberry Pi and fast on a cluster, that a fan community
can skin and extend without a deploy, and that nobody can turn into a closed product.

Four things have to be true for that to work, and they shape the whole roadmap:

1. **Reading must never wait.** Render on write, serve from cache. A reader request is a
   lookup and a response.
2. **The community must be able to change things.** Templates and components live in the
   database, not in files. Editing an infobox is editing a page.
3. **It must work with JavaScript off.** Reading and editing both. Every component is
   server rendered first and enhanced second.
4. **Migration must not cost everything.** Wikitext importer, MediaWiki API compatibility,
   and an open export format.

---

## Principles

- **Design before code, while design is still cheap.** The schema is the expensive thing to
  get wrong.
- **Verify, do not assume.** Every version we name is checked against a real source.
  Performance claims get measured before they get published.
- **Degrade, never fail.** A missing worker means stale HTML, not an error page. A missing
  JavaScript runtime means a plain editor, not a broken one.
- **Boring and pinned beats new and surprising**, except where new is the entire point.
- **The engine stays free.** Nobody can sell a closed fork, everybody can sell a skin.

---

## Phase 0 · Decisions and the benchmark spike

**Status:** mostly done, one task open.

- [x] Design review completed, plan rewritten
- [x] Licensing decided: AGPL-3.0-or-later plus a skin, theme and plugin exception
- [x] Crate naming decided: `naw-*`
- [x] Diagrams in Markdown decided
- [x] Per tier scaling decided
- [ ] **Benchmark spike**, one day, before the stack is frozen

The spike matters because everything we claim about speed is currently reasoning, not
measurement. We will run three configurations on the same machine and the same page:

- Axum plus MiniJinja, render on read
- Axum plus MiniJinja, render on write, serve from Valkey
- Bun plus Vue SSR, render on read

Measured with k6 at 200 virtual users, then a saturation ramp: p50 and p99 time to first
byte, requests per second where p99 crosses 100 ms, resident memory at idle and under load,
cold start, and behaviour with the cache disabled. Results go in
`docs/benchmark-2026-08.md` with raw numbers and the scripts, and they replace the
estimated table in the stack document.

**Done when:** the numbers are published and the reader path choice is backed by them.

---

## Phase 1 · Foundation

The skeleton everything else hangs on.

- [x] Cargo workspace: `naw-core`, `naw-web`, `naw-cli`, `naw-markdown`
- [x] Shared error type and shared application state
- [x] Configuration from file and environment, secrets never logged
- [x] Migrations, and the SQLx offline workflow wired into CI
- [x] Sign-in without JavaScript, through another device and a one-time code
- [x] Session tokens stored as digests, `__Host-` cookies, and cross-origin POSTs refused
- [ ] Authentication, mostly landed. Sessions, the identity store, sign-in with
      GitHub, Discord and Telegram OIDC, and password login with Argon2id all
      work, with throttling and admin-issued temporary passwords. Registration
      can be open or closed. Email verification does not exist yet.
      **Deviation from this plan, on purpose:** sessions live in the Postgres
      `sessions` table, not in Valkey. The cookie carries a random UUID and the
      server owns the lookup, so a session survives a cache flush and can be
      revoked from one place. Valkey holds the short lived OAuth state and PKCE
      verifiers instead, where a TTL is the whole point.
- [ ] Role based access control, namespace aware. Enforced since 2026-09-22:
      capabilities over a per-install and a per-wiki role, guests read only by
      default, switches per wiki, per-person allow and deny overrides, and
      sanctions (mute, wiki ban, install ban). Namespace awareness is not in
      yet, beyond profiles living in their own namespace.
- [x] The wiki resolver, so one install can serve many wikis
- [x] The Markdown pipeline with raw HTML disabled at the parser (since 2026-09: a whitelist of tags and a safe subset of `style`, for templates)
- [ ] The render job pipeline. The cache and the render-on-miss path work; the
      queue and the worker loop do not exist, so a miss renders inline.
- [x] Default skin, health check, structured logging. `NAW_LOG_TRACE=1` adds
      per-request detail with an `x-request-id` on every response, and
      credentials are redacted from the header dump.

**Done when:** a page can be saved as Markdown, rendered, and served from cache, and a
second request for it does not touch the renderer.

---

## Phase 2 · Content engine

The part that makes it a wiki rather than a blog.

- [x] Page create, read, update, and archive with full revision history
- [x] Diff viewer and one click undo
- [x] The `Template:` namespace and transclusion: `{{Infobox VTuber | name = Filian}}`,
      parameters as `{{{field|default}}}`, `{{#if:}}` and `{{#ifeq:}}`, `<noinclude>` and
      `<includeonly>`. Nesting stops at 16, loops are caught, and the number of calls and
      the size of the result are capped. Curators and up edit templates
- [x] An `:::infobox` card in the markup and a first `Infobox VTuber` template; starter
      templates on the new page form ("VTuber article" arrives with the card and sections)
- [x] A template edit shows on every page that uses it at once: the cache key is the hash
      of the expanded text, so nothing needs a full flush. A template's page lists where it
      is used
- A preview of affected pages before a template is published
- [x] A page for every uploaded file, as on Wikimedia Commons: `/image:ferris.png`,
      `/audio:…`, `/video:…`, `/file:…`, with the file or a player, what it is, who uploaded
      it, where it is used, and a description with its own history. Articles use a file by
      name, `![Ferris](image:ferris.png)`; audio and video play in place, PDFs download
- [x] A file keeps its history: a new version under the same name (a better quality or
      another format of the same kind) shows everywhere the file is used, and an older one
      can be brought back. Readers report a file like a page; moderators hide it, admins
      delete it with its stored copies. Still to come: thumbnails in several sizes
- The other namespaces (`Talk:`, `Category:`) and `Module:` for components
- [x] Template fields as a strict YAML block inside the call, checked against the
      template's own `<params>` schema; the template's page lists the fields and ready
      examples. Still to come: an editor form built from the schema, and the values as
      queryable data
- [x] One template code for every language: the main version holds it, a translation
      brings only its documentation and `<labels>`, read by `{{#label:key|Default}}`.
      A translation with code, or a main version that calls itself, is refused on save
- [x] HTML from a fixed list of tags in pages and templates, with `style` limited to looks
      (colors, borders, shadows, fonts within reason, spacing); scripts, frames, forms and
      positioning are dropped
- [x] Paths the engine uses itself (`admin`, `account`, `search`, `drafts` and so on)
      and language codes refused as page addresses, with the author's text kept
- [x] Reports: mistakes, complaints and suggested changes, a moderators' queue, and the
      answer visible to the reporter
- [x] Categories: `[[Category:VTubers]]` in a page or in a template it uses, a box of them
      under every page, and a page for each category with its description, its pages
      by letter, its files and its subcategories. Names in any script; levels with a slash
      (`Streams/ARG` at `/category:streams:arg`), with every spelling of an address landing
      on one and a list of everything inside a category. See [categories.md](categories.md)
- Redirects, and slug normalisation for article addresses in Japanese and Cyrillic
- [x] Media upload behind a swappable storage backend. Images are recognised by their
      bytes (never SVG), stored by content hash, served with a CSP that allows nothing.
      Only the wiki's own uploads render as images; an outside image becomes a link.
      Uploaded one file at a time from the editor or `/media`, up to 20 MB each.
- [x] Profiles at `/user/{name}`: a page of one's own, in the same editor and history,
      with an avatar. Only the person, their curator, or a moderator and up may edit it.
- [x] Page protection by role: curators and up protect a page at their own level or
      below, and never loosen a protection set above them
- [x] Special pages at `/system:name`: recent changes (by day, with the size of each edit and
      a link to its diff), all pages, files, statistics, a random page, and `/system` listing them
- [x] More special pages, in groups: all pages by kind (articles, categories, templates, page
      templates) with an A to Z bar and paging; categories, new pages, templates by use and
      active editors; and for upkeep, pages in no category, categories with no description,
      pages missing in this language, short, long and stale pages, and unused files
- [x] Pending changes: a wiki may hold back new pages, edits, or both, from people without
      the pass until a curator accepts them; readers keep the accepted text. A review queue
      with the changes and a preview, refusing an accept that would drop a later edit, a list
      of edits nobody checked, and a one-click rollback of the last author
- [x] Maintenance notices as templates (Update, Stub, Sources needed, Citation needed,
      Cleanup, Disputed, In progress, Spoiler, Speculation) that file pages under
      Maintenance/..., and infoboxes for streams, songs and events with page templates
- [x] Every content and people limit in one table, set by the config file, the environment
      or the command line, and per wiki where it is cheap. See [configuration.md](configuration.md)
- [x] Watchlists: a star on every page, pages you write watched on their own, a watch that
      covers every language and follows a rename, and `/watchlist` by latest change
- [ ] Audit log with a retention policy. The log is written and browsable; retention is not in
- [x] Search behind a swappable backend, PostgreSQL full text first. A long article is
      indexed in 200 000 character pieces, so all of a 5 MB article is searchable and no
      vector nears PostgreSQL's 1 MB limit

**Done when:** an editor can create a template, use it on fifty pages, edit it once, and
see all fifty update without a deploy and without a full cache flush.

---

## Phase 3 · Components and the worker

The feature that puts us ahead of every self hosted wiki.

- `Module:` namespace, with Vue SFC and plain TypeScript flavours, immutable versions
- The worker: SFC compilation, rolldown bundling, dual SSR and client bundles, SRI hashes
- Worker protocol: JSON over a local socket, timeouts, crash isolation, no credentials
- Plain TypeScript fallback in an embedded engine, for installs with no worker
- Rust sanitises every worker output before it is cached
- A 3 KB client loader that imports only the bundles a page actually uses
- Draft, publish and rollback, with audit entries
- **Diagrams in Markdown**: mermaid, graphviz and PlantUML rendered server side to inline
  SVG, so they work with JavaScript disabled
- Image thumbnails and WebP or AVIF conversion
- Scheduled jobs for digests, cache warm and reindex

**Done when:** an admin can write an interactive infobox in the browser, publish it, and
have it appear on a hundred pages, fully readable with JavaScript off and interactive with
it on.

---

## Phase 4 · Migration

Communities will not move if moving costs everything they built.

- Wikitext to Markdown converter with a coverage report
- FANDOM and MediaWiki XML dump importer in the CLI, with a dry run mode
- Template mapping assistant: convert a wikitext infobox into a `Template:` plus a `Module:`
- Media and user attribution import, preserving authorship, timestamps and history
- Open export format: Markdown plus JSON, and Wikipedia style XML
- Automated backup and restore, with a restore drill in CI

**Done when:** an existing FANDOM wiki can be imported with authorship preserved, and
exported again in a format another engine can read.

---

## Phase 5 · API platform

The API is a product surface, not a side effect.

- REST v1 with cursor pagination, ETags, `problem+json`, idempotency keys
- OpenAPI 3.1 generated from code, published with every release, SDK generation in CI
- Personal access tokens with scopes, and OAuth 2.0 with PKCE for third party apps
- Webhooks with HMAC signatures, retries, a delivery log and replay
- Server sent events stream of recent changes
- GraphQL facade with depth and complexity limits
- **MediaWiki compatibility layer** at `/api.php`, with a published coverage matrix, so
  existing bots mostly need a new base URL

**Done when:** a bot written for FANDOM can talk to us, and a Discord relay can subscribe
to live changes without polling.

---

## Phase 6 · Authoring UI

Make editing pleasant, without ever putting it on the reader path.

- Vue 3 application, built with Vite, served by Rust as hashed static assets
- Loaded only on an explicit action, never on a page view
- Block editor on TipTap, with a Markdown source toggle
- Live preview through the real server pipeline, debounced
- Media drag and drop upload
- [x] Autosave locally and on the server, with My drafts; still to come: a three way merge
      on conflict instead of the conflict page
- Component picker that inserts a template or component with a prop form
- Component editor: source, live preview, build log, version history
- Admin panel: roles, sponsor privileges, skin variables, webhooks, audit log.
  The server rendered panel exists (accounts, rights, sanctions, pages, audit log,
  settings, languages, header and footer); the Vue part does not.
- Plain Markdown fallback that is fully functional without JavaScript

**Done when:** an editor can do everything from a phone in Lockdown Mode, including saving.

---

## Phase 7 · Skinning and FilianWIKI

- [x] Skin loader with template inheritance: a skin ships only what it changes,
      usually just a `_theme.html`, and reloads on the fly. Per wiki asset isolation
      is not in yet.
- [x] The `snackers` skin, pinks and purples, dark mode by default
- Admin UI for skin variables as CSS custom properties
- Per page layout overrides
- Error page overrides: `errors/409_conflict`, `errors/404_not_found` and so on, per skin
  and per wiki, over the generic error page
- Core component library: infobox, navbox, hatnote, tabs
- [x] `Template:Infobox VTuber` as the first real template
- `Module:StreamArchive` as the first real component
- [x] 7TV emotes: admins add 7TV users or emote sets, which are the allowlist; each
      file is downloaded once into local storage under a 1 GB budget, and `:name:` in
      an article shows it. `/emotes` lists them all.
- Snacker of the Month widget, admin configurable
- Timeline and relationship charts via mermaid

**Done when:** FilianWIKI is live at `filian.wiki` and looks like it belongs to that
community rather than to us. The landing is live there; the wiki itself is in closed alpha.

---

## Phase 8 · Growth

The things that decide whether anyone finds the wiki.

- [x] OpenGraph and Twitter cards: a description from the opening paragraph and the
      article's first picture. Generated share images are still to do
- [x] `sitemap.xml` with every language of every article, `robots.txt`, canonical URLs on
      the wiki's own domain, `hreflang`, and `noindex` on every page that is a tool
- Antispam: registration CAPTCHA, external link limits, new user heuristics
- [x] Notifications on the site: edits to watched pages, the review of your own edits and
      answers to your reports, behind a bell with an unread count
- Digest email
- [x] Internationalisation: engine UI translated (English and Russian packs, on
      the fly), per page locale routing by path or subdomain, translations with
      staleness notices
- Offline PWA reader mode
- Sponsor integrations: Ko-fi and Patreon webhooks

**Done when:** a link shared in Discord unfurls properly, and a wiki shows up in search.

---

## Phase 9 · Hardening and deploy

- Rate limiting, per route and per token
- CSRF protection on every state changing form
- Content Security Policy with no `unsafe-inline`, SRI on every first party bundle
- The invalidation graph from Phase 2, wired to the cache
- Compose stack, frp tunnel, Caddy on the VPS
- Backup automation with retention
- `/metrics` endpoint, dashboards in an optional profile
- k6 suite and the benchmark document with methodology
- [ ] Deploy FilianWIKI. The closed alpha runs behind the frp tunnel; the public launch
      waits for open registration.

**Done when:** the thing survives being on the public internet.

---

## Phase 10 · Plugins

No date. We will revisit this only if something genuinely cannot be expressed as a
component, which so far has not happened.

---

## Targets

Honest about which of these are committed and which are to be measured.

| Metric | Target | Status |
|--------|--------|--------|
| Time to first byte, cache hit | under 20 ms p50, under 50 ms p99 | to be measured |
| Time to first byte after a miss | old version instantly, new within 2 s | to be measured |
| Cache hit ratio | over 99% of reads, alerted on | to be measured |
| Concurrent requests on a Raspberry Pi | 200 | to be measured |
| Lighthouse | over 95 on performance, accessibility and best practices | to be measured |
| Idle memory, Rust core | under 128 MB | to be measured |
| Reading with JavaScript disabled | everything, including diagrams and components | committed by design |
| Editing with JavaScript disabled | fully functional | committed by design |
| Binary under 30 MB | unlikely with this stack, treated as a stretch goal | honest stretch |

The 200,000 concurrent connections figure that appeared in early drafts is **not** a
commitment. It moves to the benchmark document with a methodology, or it disappears.

---

## Explicit non goals

Things we are deliberately not doing, so nobody waits for them:

- A WASM plugin system, see Phase 10
- Native mobile apps
- A hosted commercial service. Self hosting and community hosting only
- Real time collaborative editing. Autosave, conflict detection and three way merge instead
- Blockchain, NFTs, or anything with a token
- AI generated article content. The engine will not write your wiki for you

---

## How to follow along and help

- Read [STACK.md](STACK.md) and tell us where we are wrong. Design review is the most
  valuable contribution right now.
- Read [../CONTRIBUTING.md](../.github/CONTRIBUTING.md) before opening a pull request.
- Issues are labelled by area and by effort, `good first issue` marks the entry points.
- Art is very welcome, and we would like to replace every generated image with human made
  art. See [../AI-ASSETS.md](../AI-ASSETS.md).

This is a project built by fans, largely in the open, mostly outside working hours. It will
take as long as it takes, and the order above is the order that unblocks the most people
fastest.
