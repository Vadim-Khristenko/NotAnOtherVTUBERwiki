<div align="center">

<img src=".github/assets/FILIAN_WIKI_BANNER.jpeg" alt="NotAnotherWiki" width="100%">

# NotAnotherWiki Engine

**A wiki engine for fan communities that refuses to suck.**

Markdown first, genuinely skinnable, fast on a Raspberry Pi, readable with JavaScript off,
and licensed so that nobody can turn it into a closed product.

*Codename: NotAnOtherVTUBERwiki. First deployment: FilianWIKI, the wiki of the Snackers,
the community around Filian.*

[English](README.md) | [Русский](docs/ru/README.md)

</div>

---

## Status: pre-alpha

Nothing is shipped yet. This repository currently holds the design, the schema and the
decisions.

- **[docs/ROADMAP.md](docs/ROADMAP.md)**: what we are building, in what order, and what
  has to be true before a phase is done.
- **[docs/STACK.md](docs/STACK.md)**: why every technology was chosen, with verified
  versions and the things we deliberately rejected.

Version numbers in this document were verified against crates.io, npm and the official
release feeds on 2026-08-30, not recalled from memory.

<details>
<summary><b>Show the ASCII art</b> (it is wide, so it hides by default)</summary>

```text
             --@@-:::::::::..      @= -++-:@@@   .        @@                @:@:-
        ... ..  -*@@%++====+@-    @-.@@ :@::@@@@@@@@@@@@. .@ .    @       @@@*@#@    ..          ..
            ... -  -#%*=:-:::---@%@ @@@@*.=@ @@@@....-+@@@ @   @@@@@@@  %@@@:#%#@@-  .    .....
    . ... ... -@@+.  ----:::::. @@@ @*:@@. @@@.  .......:-.. @@@@#=-*%@@@@-:+@+--@@*  .  .... ...
 . . .. . .  -@:.- ...  ------*@@@ @@@@.@@@@+ .@@@@@@@@@@@@@@@  . :......@=:#=.@@@ @. . .  .  .....
   . .. . . -#@%@@=---     . .@+@@#@@%:@*@@..@@@@@      +@@@@@@@@@@@@@@#. @@.@@@@@ @. @ ...  ......
  .  .. . . -@::::::------.. :@- @@@ @@%@@ @@@@@@@@@@@=@@@@@@@ .- =.  %@@@..@.@*%@@@@@. .... ......
  .  .. .. --:::::::::::-- . .@*-@ %@@%#@#@@@@@@#=    .@@@@@@@@@. -@@@.%@@@ @-@*@@ @@@  .. ..  ....
   ....... ---------------... @@  @@@@#@@@@@@@:  .@@@@@@@@@@@@@@@@@* .=@@@@@ @@@*@@@@@ ............
   . ..     ...   ..      ..  @@.@@%%%@@%@@@@=@@@@@@@@@@@@@@@@@@@@@@@@  .@@@@.@.@=@+@. ............
   .. .:@@@:  ............... :@@@@%@#@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@  @@%@@@%@ +@  ............
 ..... -%%@=  .    ..........  @@%%@%@@@@%@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@#@@ @.@. .. .....   ..
 ...  .     ...........       @@@@@%%@%@@%@%@@@@@@@@@@@@@@@@@@@@@@@@@@@@%@@@@+@@@=@@  .. ..........
      ...........  ..  @@@@@ @@%@@@%@%@@@%*@@@@#@@@@@@@@@@@@@@@@@@@@@@@@#@@@@+@@@.@   .............
   ..  . ......... .  @-:@@@:@%%@@@%@%@@@:*@@@:=@@@@@@@@@@@@@@@@@@@@@%@@@%@:@+@%@.@@  .............
     ..          ..  =---:::@@@@@@@%%@@@@=@@@:-=@@@@@@@@@@@@@@@@@@@@@@@-@@@ @+@%@@*@  .............
 ...  ..............  ----  @@@@@@%%%@@@-*@@-+%:@@@@@@@@@@@@@@@@@@@@#@@ %@@*@%@@%@:@-
       .           .       .@%@@@@#@@@@-@##:=@@-%@@@@#@@@@@@@@@@@@@#@@%-:@@@.@@%#@:@@@@@@. .     ..
 ..                ..  .  @:@%@@#%%@@@.@@#*@@@@#+@@@@@@@@@@@@@@@@@#@@@@--=@@.@@%%@=@@+@@:=  .......
            ....    ..   @@:@@@@*@@@=@%@%#*@@@@@@@@@@@#@@@@@@%@@@@%@@%@-+=-#@@@@%@-@@ ----= ..  ..
   ..  .         .....   @.@@@@@@@#=#@-@@@@@@@@%=-%@@@*@@@@@@%%@@@@@%@@@@@@*@ @@@#:@@ -:..  ..... .
     ..    .       .     @ @*+@@.-%@@@@@..@#..%@@@-.@@*#@%@@@@@#@@@#**@@=%-=@#.@@#*@@ :-   .. .....
    .      .    . ...    @.@*=#@@@@@@@@.       .@@@@@@@@%@@@@@@*#%@@@@@@@@@@@ @.@@@@@. .. ...    ..
 .      ..............  #@@@@@@@%%@@@@    ...:.   +@@@@@@@@@%%@@@@@@@@@*% @@=@ @ @@#@* ....   .....
 ........  ....... ..   @@.@%@@@%@%@@@.@@. ......==@@@@@@@@@@@@@          @@:@@@@*=@@#  .   ..  .
   ......... ..  ...   @@: @%%@@%@#@@@@:@@.... . @@@@@@@@@@@%@@@ ........   %@@@%@@#@.  ... ..  . .
 ............   ...   @@ #.@%@%%@@%@@ @-.%.==+#@@@@%@@@@@%@@@@@@=   . ..@@ .@@@@ =@@@.  ...... ....
              ...    @@@.#*@#@%%%@@@@@+@+---:::++*%%%@@@@#%%@%@@@@@@@+ @@ @@@@@@*#%@@   ..    .....
 .........  ...     @.%#=#**%%%%%@.@%@:--@-=***%%@@@@@@@%@@%%%@@%=:..==::@@@:@%+.@#@@    ..........
 .  ...   .. ...=  @.%%=.%%.#*%#@@.@#@@*%#*%@@@@@@@@%%%%%@@@@@@@@%*==-::@@-.@@@#@@%@@    .. .......
 ....  ..  ...   .@ @%#.@=@-@*##%@@+@%@:@@@@@@@@@@@@@@@@@@@@%@@%%@@@%%*=-: @@@@-@@@*%%    .........
   .....    .      @@@ @ +@%*%**%#@+@@@@+@@@@@%%%@%@%++*###%@@%@@@@@%@%@#+@@@ #%##*@.@    .. ......
 ...........   .:@@@:@ @@@ @%=@##%@@-@.%@@@@@@@@@@@@%%@@@@@%*%@@@%%@@@@:@@@@ @@%@%==.@@   ...... ..
 ............  .@@@@ @@@@  %@ @%+%@.%*@..@@+@@@@@%%%@@@%%%@@@@%%@@@@@ @@@ @.@%%@@ @#.@@@.  .... ...
            ..:+@@@=@@-:..%.@@ #-%%%.@@.@: #==@@@@@@@@@@%%%%@@@@@@%@@@..@.@@*+@@ @ .*@@ @  ........
 .............    ...@%.@++. @@.#--*@.@@ @% .:..-@@@@@@@@@@@@@@@@*: .. @%@@*%@@ @@ %..@        ....
 ................#@@@  @@*.@@ @. @@-:-+@@.@@*--::..:#@@@@@@@+.  -.:+@ @.**#@@..@@ @ @%%@.      ....
 ......    ......    .*@-..-. :@. +@@=::#.@@@===--::...  ....:@@--+...@@#%# :@@. @@ * @@@. ......
        :%      .. . :#. @*  . @@.:@@@@@:@#@  .=====------===@@ = .@*:.. @*.@ .#- @@@ . #@@.   ....
 @@@@@@@@@@@@:    .. . .:. ....@@  @@@@@@%@@      :========: @@ @@@@@@@@@@ @@.+:@  +@@@ .  ....
#@%*=-----=+@@@   ....   .... @ @  :@@@@@ :@                 @@.-..*@@@@@ +@  +@@@   .*@@.      :@*
:+===-----.@%@=@@ .....         @..- @@@@:. @               @@. .@@@@@@@ @@@%   .-  .       @@@@@@@=
:++=++=-=+:@#%-@*@    .%@@@@@@@@@@   @@@@                     ..@= @%@@.  #@@@@@@@@#=....=@@@=:%@*::
.---=++=+*@#%-@@-@@@@@@@@@@+@@       .@@@                    @.   .@*@@ . .    @@@@@@@@@@%=@+%*#@@#:
.------=*.  *@= @%%-==    @   @      .@@@                    @@@   @@@  @@@@@@#.       *==:+@@*.-@@=
.===#%%%@@@@* @@@@++@@@.   .@   @.   =# .                     @@.  @@*  :  -@@@@@@@@@@@#=+-*@@@@#:..
:#**# ..:%=:#@@%+*.@@=%@@.   .@  .@                            @:  :@  @.  @   .---.:::::-#-@-:-@@%.
.@@**@@:  -++-:.=. %*+*+%@@.   @@  :@   .                             @  .@   *@@#@%%@@@@@   +:+--=:
  .@@@%#%@@*+-+-=:=@##%**+@@@    @@  @-                             .@  #@   @%%*=**%@@@@@@@=+=++*+:
      @@-::=+*##**%@+=====*+@      @   @.                          :@  @@   @%==*-+*@@##**#@**@@@@@*
      .@@@@%#%%@@@*++=------#@      #@  .#     .:@@@@@@@@.        @-  @= . .@++==+#%%##**#%@@@@:
       @@@%++*=#%%*+--:.::-..@=       +#  +- ..:@@@.  .-@@%..##  @   @     ++-----+#%%#++#%@@
        @@@@*=::--=+======:.:-@         #.  +.               =..+  .#      @-:-::-=+**+*+#@@%      .
.       .#%%#=:+*++++=::.:..+.-@          #                   .   #.       *.--=+=**===*#@@@       .
.          ...              ....            .                   ..         .          .....        .
```

*Generated art, kept because it makes the repo less boring. See [AI-ASSETS.md](AI-ASSETS.md).*

</details>

---

## Why this exists

Fan wikis are the most volunteer-run, least well-served software category on the web. The
incumbents extract value from volunteers and give very little back.

| What hurts today                        | What we do instead                                                  |
|-----------------------------------------|---------------------------------------------------------------------|
| Ads, trackers, bloat                    | Zero ads. Zero trackers. Funded by donations.                        |
| Slow pages, 3 to 8 second TTFB          | Render on write, serve cached HTML. Sub-20 ms p50 on a cache hit.    |
| Locked vendor ecosystem                 | Self-hosted. Your data, exportable in an open format, forever.       |
| Rigid MediaWiki markup                  | Markdown first, with real transclusion and namespaces.               |
| Templates need a deploy                 | `Template:` pages live in the database, editable in the browser.     |
| Adding a widget means writing PHP       | `Module:` components in TypeScript or Vue SFC, bundled server-side.  |
| Breaks without JavaScript               | Reading and editing both work with JavaScript disabled.              |
| Weak API                                | REST, GraphQL, webhooks, SSE, plus MediaWiki API compatibility.      |
| Migration is a dead end                 | Wikitext importer and open export from day one.                      |

---

## Architecture in one picture

```text
                     readers (almost all traffic)
                                |
                   [ Caddy / CDN edge: Brotli, HTTP/3 ]
                                |
                 +--------------+--------------+
                 |                             |
      [ naw-web: N replicas ]        [ worker: M replicas ]
        Rust, Axum, stateless          Bun 1.4.0, async
        cached HTML + full API         bundling, SSR, diagrams
                 |                             |
                 +------+---------+------------+
                        |         |
               [ PostgreSQL 18 ] [ Valkey 9 ]
                                       |
                        [ object storage: disk or S3 ]
```

The single most important decision in the project: **render on write, serve on read**.

A reader request never parses Markdown, never renders a template and never touches the
worker. It looks up `render_cache` by content hash and returns. A cache miss renders
asynchronously in the background while the reader keeps getting the previous, perfectly
valid version.

Every tier scales on its own. No Bun worker installed? Reading is unaffected, you simply
lose publishing new components and rendering new diagrams.

---

## Stack

| Layer             | Choice                                  |
|-------------------|-----------------------------------------|
| Language          | Rust, edition 2024, rustc 1.98.0        |
| HTTP              | Axum 0.8.9                              |
| Database          | PostgreSQL 18                           |
| Driver            | SQLx 0.9.0, compile-checked             |
| Cache and queues  | Valkey 9.1.1                            |
| Templates         | MiniJinja 2.24.0                        |
| Markdown          | pulldown-cmark 0.13.4, ammonia 4.1.4    |
| Authoring UI      | Vue 3.5.42 plus Vite 8.2.2              |
| Worker            | Bun 1.4.0 (Rust based), optional        |
| Bundler           | rolldown 1.2.6                          |
| Search            | Postgres FTS first, Meilisearch opt-in  |
| Storage           | local disk first, S3 via object_store   |

Full reasoning, including why Bun is not on the request path, is in
[docs/STACK.md](docs/STACK.md).

---

## Quickstart

Prerequisites: Rust 1.98 (edition 2024), Docker with compose, Bun 1.4.2 or newer, sqlx-cli.

```bash
git clone https://github.com/Vadim-Khristenko/NotAnOtherVTUBERwiki.git
cd NotAnOtherVTUBERwiki

# 1. Start PostgreSQL 18 and Valkey 9 on ports 5433 and 6380
docker compose up -d postgres valkey

# 2. Configure the environment
cp .env.example .env

# 3. Apply the schema
cargo install sqlx-cli --version 0.9.0 --no-default-features --features postgres,rustls
cargo sqlx migrate run

# 4. Run the engine
cargo run -p naw-cli -- serve
# health: http://127.0.0.1:4242/health, readiness: /ready

# 5. Optionally build the authoring UI
cd ui && bun install && bun run build
```

The web tier serves cached HTML on the reader path; rendering happens on write.
The worker in `worker/` is optional and never on the request path.

---

## Repository layout

| Path                 | What it is                                            |
|----------------------|-------------------------------------------------------|
| `docs/ROADMAP.md`    | What we are building, in what order. Start here.       |
| `docs/STACK.md`      | Why every technology was chosen.                       |
| `docs/`              | Roadmap, stack notes, benchmark results.               |
| `.github/AI-POLICY.md` | Using AI is allowed, but you must be able to explain your own changes |
| `AI-ASSETS.md`       | Artwork, AI generated images, and submission terms     |
| `.github/assets/`    | Project art, see [AI-ASSETS.md](AI-ASSETS.md).        |
| `naw-core`           | Domain, services, database. (not scaffolded yet)      |
| `naw-web`            | Axum, routes, middleware, API. (not scaffolded yet)   |
| `naw-markdown`       | The render pipeline. (not scaffolded yet)             |
| `naw-cli`            | Admin, migrate, seed, import. (not scaffolded yet)    |
| `ui/`                | Vue 3 authoring app. (not scaffolded yet)             |
| `worker/`            | Bun service. (not scaffolded yet)                     |

---

## Licensing, in plain words

| Thing                          | Terms                                                       |
|--------------------------------|-------------------------------------------------------------|
| The engine                     | AGPL-3.0-or-later. Modify it, serve it, and you publish the source. |
| Skins, themes, plugins         | Yours to license, including commercially, under the exception. |
| Documentation                  | CC BY-SA 4.0.                                                |
| The name and the logo          | Not licensed. See [TRADEMARK.md](TRADEMARK.md).              |

Nobody can turn this into a closed commercial product, because anyone who interacts with a
modified copy over a network is entitled to the complete source. Everybody can still sell
skins, themes and plugins.

- [LICENSE](LICENSE) - AGPL-3.0
- [LICENSE-EXCEPTION](docs/LICENSE-EXCEPTION) - the skin, template and plugin exception
- [LICENSE-DOCS](docs/LICENSE-DOCS) - CC BY-SA 4.0
- [NOTICE](NOTICE) - authorship, must be preserved
- [TRADEMARK.md](TRADEMARK.md) - name and logo policy

---

## Contributing

Read [CONTRIBUTING.md](.github/CONTRIBUTING.md) first, and [CODE_OF_CONDUCT.md](.github/CODE_OF_CONDUCT.md)
before you talk to anyone.

In short:

- Start with an issue. Big changes need a design discussion first, this project is still
  mostly design.
- Conventional Commits, branch naming `feat/vai/thing`, one logical change per branch.
- `cargo fmt`, `cargo clippy`, `cargo test` before you push.
- SQL goes through `sqlx::query!`. CI builds with `SQLX_OFFLINE=true`.
- No em-dashes in code, comments, commits or docs. House style.
- Read the [Code of Conduct](.github/CODE_OF_CONDUCT.md). It is short, and it applies everywhere.

Art is welcome too. See [AI-ASSETS.md](AI-ASSETS.md) for the terms, the short version is
that you grant the project perpetual rights to use your work and you must own it.

**Using AI tools is allowed and welcome**, in code as well as art. See
[AI-POLICY.md](.github/AI-POLICY.md). We do not require it and we do not ban it. The only real
test is that you can explain the change you are sending.

### Translations

Every English document has a Russian translation in `docs/ru/` with the same filename
rather than put in a subdirectory. Both languages are first class: if you change a document,
please say in the pull request whether the other language needs the same change, or open a
follow up issue for it. A stale translation is worse than none, so tell us if you cannot
keep one current.

| English                | Russian                        |
|------------------------|--------------------------------|
| `README.md`            | `docs/ru/README.md`                 |
| `.github/CONTRIBUTING.md` | `docs/ru/CONTRIBUTING.md`       |
| `.github/CODE_OF_CONDUCT.md` | `docs/ru/CODE_OF_CONDUCT.md`    |
| `.github/SECURITY.md`        | `docs/ru/SECURITY.md`           |
| `.github/AI-POLICY.md`        | `docs/ru/AI-POLICY.md`          |
| `AI-ASSETS.md`         | `docs/ru/AI-ASSETS.md`              |
| `docs/STACK.md`        | `docs/ru/STACK.md`             |
| `docs/ROADMAP.md`      | `docs/ru/ROADMAP.md`           |

### Language

This is a Russian speaking project at its core. The author and a large part of the
community write Russian, which is why every document here exists in both languages and why
Russian is a first class language for us.

For issues and pull requests, please prefer English. Russian is always welcome and we will
never close anything for being written in it, but English keeps the tracker and the review
threads readable for everyone who might be able to help, including people who do not speak
Russian.

If writing English is hard for you, write Russian. A clear report in Russian is far more
useful than an unclear one in English, and we will translate it into the thread if that
helps.

---

## About the Snackers of it all

This project started inside the Snackers, the community around Filian. The first wiki built
on it is FilianWIKI. There are references to the community scattered through the codebase
because they are harmless and because we like them. They are not load bearing, and the
engine is in no way specific to one VTuber.

Filian and the Snackers are not affiliated with this project and have not endorsed it. If
that ever needs to change, it will.

---

<div align="center">

Built by fans, for fans. Licensed so it stays that way.

[Русская версия](docs/ru/README.md)

</div>
