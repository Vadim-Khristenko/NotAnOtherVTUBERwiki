# Contributing to NotAnotherWiki

Thanks for being here. This project exists because fan communities deserve better tools,
and it will not get built by one person alone.

Read [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) before anything else. It is short and it
applies to every interaction in this project.

English version. Russian translation: [CONTRIBUTING.md](../docs/ru/CONTRIBUTING.md).

---

## Where the project is right now

Pre-alpha. There is no running code yet, only design. That changes what contributing
looks like:

- **Design and review are the highest value work today.** If you can poke holes in
  [docs/ROADMAP.md](../docs/ROADMAP.md) or the stack in [docs/STACK.md](../docs/STACK.md),
  that is more useful right now than a pull request.
- Code contributions are welcome, but the scaffold does not exist yet. Ask before writing
  a large amount of it, or you may be building against a plan that is about to change.
- Translations, art, and issue triage are all useful immediately.

---

## Ways to contribute

| What | How to start | Skill needed |
|------|--------------|--------------|
| Design review | Comment on [docs/ROADMAP.md](../docs/ROADMAP.md) or open a design issue | Wiki or backend experience |
| Code | Pick an issue labelled `good first issue`, or ask for one | Rust, TypeScript, Vue |
| Documentation | Open a PR against any `.md` file | Clear writing |
| Translation | Create the same-named file in `docs/ru/` directory and translate, see below | Russian or English |
| Art | See [AI-ASSETS.md](../AI-ASSETS.md) | Drawing, pixel art, SVG |
| Triage | Reproduce bugs, label issues, answer questions | Patience |
| Wiki content | That happens on SnackersWIKI once it is deployed, not here | Fandom knowledge |

---

## Ground rules

1. **Discuss before building anything large.** Open an issue first. This avoids wasted
   work and wasted arguments. For anything that touches the schema, the render pipeline,
   the permission model or the license, a design discussion is mandatory.
2. **One logical change per branch.** Do not sweep unrelated fixes into a branch because
   it is convenient.
3. **You keep your copyright.** You grant the project the right to distribute your work
   under the project license. See the DCO section below.
4. **Never commit secrets.** No tokens, no passwords, no `.env`, no real credentials. If
   you accidentally do, say so immediately so history can be rewritten before it spreads.
5. **Copyright and license headers must stay.** Do not remove `NOTICE`, do not strip
   license headers, do not rebrand the engine.

---

## Getting set up

```bash
git clone https://github.com/Vadim-Khristenko/NotAnOtherVTUBERwiki.git
cd NotAnOtherVTUBERwiki

docker compose up -d            # PostgreSQL 18 and Valkey 9

cargo sqlx database setup       # create the database and apply migrations
cargo sqlx prepare              # regenerate .sqlx after changing any query
cargo test
cargo run --bin naw -- serve
```

You need:

- Rust 1.98.0 or newer, edition 2024
- `sqlx-cli` (`cargo install sqlx-cli --no-default-features --features postgres,rustls`)
- Docker, or a local PostgreSQL 18 and Valkey 9
- Bun 1.4.0 and Node, only if you are touching `worker/` or `ui/`

If the database is unreachable, build with `SQLX_OFFLINE=true` and rely on the committed
`.sqlx` directory. If you changed any SQL, you must regenerate it and commit the change.

---

## Branches and commits

Branch naming:

```text
{feat|bug-fix|chore|docs|refactor|test}/{your-name}/{short-name}

feat/vai/namespace-transclusion
bug-fix/vai/render-cache-stampede
docs/vai/licensing-exception
```

Commit messages follow Conventional Commits:

```text
feat(markdown): add mermaid fence rendering
fix(web): stop render jobs from racing on the same content hash
docs(plan): record the Bun 1.4.0 verification
chore(deps): bump axum to 0.8.9
```

Types in use: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`,
`chore`, `revert`. Breaking changes get a `!` and a `BREAKING CHANGE:` footer.

---

## Sign off your work (DCO)

Every commit must be signed off. This certifies that you wrote the contribution or
otherwise have the right to submit it under the project license.

```bash
git commit -s -m "feat(markdown): add mermaid fence rendering"
```

That adds:

```text
Signed-off-by: Your Name <you@example.com>
```

Use your real name. If you would rather not publish your legal name publicly, talk to the
maintainer before contributing, there are options.

---

## Code style

### Rust

- `cargo fmt` before every push. Config is the default, do not fight it.
- `cargo clippy -- -D warnings` must pass.
- Prefer `impl Trait` over `dyn Trait` unless dynamic dispatch is needed.
- `#[instrument]` from `tracing` on async handlers.
- Compose middleware with `tower::ServiceBuilder`.
- All SQL goes through `sqlx::query!` or `sqlx::query_as!`. No string-built SQL, ever.
- Every query touching a wiki-scoped table takes `wiki_id` from `WikiContext`, never from
  user input.
- Never log secrets, never return them from config dumps.

### TypeScript, Vue, and the worker

- Prettier with the repo config, ESLint clean.
- No new npm dependencies in `worker/` without a discussion. Bun 1.4.0 has markdown
  parsing, image processing, cron, terminal and headless browsing built in, so the answer
  to "add a package for that" is usually no.
- Components live in the `Module:` namespace and are authored as Vue SFC or plain TS.

### SQL and migrations

- Migrations are append-only. Never edit an applied migration.
- Every new table that is scoped to a wiki carries `wiki_id` and an index on it.
- After changing a query, run `cargo sqlx prepare` and commit the `.sqlx` change.

### Documentation and house style

- **No em-dashes.** Use commas, colons, parentheses, or restructure the sentence. This is
  a hard rule in code, comments, commit messages and documentation.
- Write like a human. No corporate filler, no "simply", no "leverage".
- When you name a version, a port or an API shape, check it against a real source and say
  where you checked. Guessed versions have burned this project already.

### Translations

Russian translations live in `docs/ru/`, using the same filename as the English source:

```text
README.md        ->  docs/ru/README.md
CONTRIBUTING.md  ->  docs/ru/CONTRIBUTING.md
```

Rules: translate everything including headings, keep all relative links pointing at the
the English file, unless a Russian translation exists, keep code blocks and commands untranslated,
and add a language switcher line at the top mirroring the English file. If you cannot
keep a translation current, say so in the issue, a stale translation is worse than none.

---

## Tests

- Every bug fix gets a regression test.
- Every new service gets unit tests at the service layer, not through HTTP.
- Anything touching permissions gets a test proving the denial path, not just the
  allowance path.
- Anything touching the render pipeline gets a test proving cache invalidation works.
- SQL changes need a migration test that applies and rolls back.

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

---

## Pull request process

1. Open an issue first, or reference an existing one.
2. Branch from `dev`, using the naming above.
3. Keep the PR focused. If you find unrelated broken things, open separate issues.
4. Fill in the [pull request template](PULL_REQUEST_TEMPLATE.md) honestly. If a
   checkbox is not true, do not tick it.
5. CI must be green: fmt, clippy, tests, SQLx offline check.
6. One approving review is required. For schema, security, or license changes, the
   maintainer approves personally.
7. Squash and merge, unless the history genuinely tells a story.

Expect real review comments. They are about the code, not about you. Reviewers owe you
specifics and a reason; you owe them a response to each one.

### What will get a PR rejected

- Removing or weakening license headers, `NOTICE`, or attribution.
- Hand-rolled SQL outside the sqlx macros.
- New runtime dependencies added without discussion.
- Tests disabled or deleted to make CI pass.
- Generated files committed without their source, or the reverse.
- Anything that silently changes what the license requires.

---

## Labels and automation

The label set lives in `.github/labels.yml` and is the single source of truth. It is synced
to GitHub automatically when that file changes, and to Forgejo with
`scripts/sync-labels-forgejo.ts`. Do not create labels by hand, they will drift.

What the automation does:

- Labels new issues by area, based on keywords in the title and body.
- Marks a bug report `needs-info` when it is very short or has no reproduction steps, and
  leaves a comment listing what is missing.
- Closes `needs-info` issues after 14 days with no reply, and reopens them on any comment.
- Marks issues and pull requests `stale` after 60 days of silence, closes after 21 more.
  Anything labelled `P0`, `P1`, `epic`, `blocked`, `decided` or `security` is exempt.

What the automation deliberately does **not** do: judge whether your issue is worth
reading. Nothing closes a report for being low quality, off topic, or unclear in a way the
bot thinks it can detect. That kind of heuristic produces false positives, and a false
positive means telling someone who volunteered their time that their report was garbage.
Housekeeping is automated. Judgement is not.

---

## Security issues

Do not open a public issue for a vulnerability. Read [SECURITY.md](SECURITY.md) and
report privately to vadim@vai-rice.space.

---

## Using AI tools

**Allowed, welcome, and never required.** What matters is that you understand your own change and can explain it.

Read [AI-POLICY.md](AI-POLICY.md). The short version:

- You may use AI assistance to find and fix bugs, write tests, or draft documentation.
- A human must sign off. AI agents must not add `Signed-off-by`, only you can.
- Disclosure is optional, but please add an `Assisted-by: LLM` trailer to the commit.
- If AI helped you find a bug, reproduce it before reporting it. Unverified reports cost maintainer time and are worse than no report.
- If you cannot explain a line when a reviewer asks about it, do not send it yet.

We will not reject a contribution for using AI. We will reject one whose author cannot explain it or has not tested it.

---

## Art and AI generated assets

Some current artwork is AI generated, and that is disclosed openly in
[AI-ASSETS.md](../AI-ASSETS.md). All the code is written by humans. Fan art is very welcome
to replace the generated images, and the terms you agree to by submitting are set out in
that file. Read it before sending art.

---

## Getting help

- Open a question issue with the `question` label.
- Ask in the pull request or issue you are working on, context helps.
- There is no chat server yet. When there is one, it will be listed here and the Code of
  Conduct will apply there too.

---

## Recognition

Everyone who contributes gets listed in the `NOTICE` credits section and in the release
notes. This includes design review, triage, translation and art, not just code. If you
contributed and are not listed, that is a bug, open an issue.

Thank you. Now go read the [roadmap](../docs/ROADMAP.md) and tell us where it is wrong.
