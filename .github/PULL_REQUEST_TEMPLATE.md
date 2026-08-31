# Pull request

## What this changes

<!-- One or two sentences. What does it do? -->

## Why

<!-- What problem does it solve? Link the issue. If there is no issue, say why not. -->

Closes #

## Type of change

- [ ] Bug fix (non breaking change that fixes an issue)
- [ ] New feature (non breaking change that adds functionality)
- [ ] Breaking change (existing behaviour changes, needs a migration note)
- [ ] Refactor (no behaviour change)
- [ ] Performance
- [ ] Documentation or translation
- [ ] Artwork or media
- [ ] Infrastructure, CI, or tooling

## How you tested it

<!-- Commands you ran, tests you added, or what you could not test. Be honest. -->

```text

```

## AI assistance

<!--
Optional, not required, and not held against you. See AI-POLICY.md.
If AI tools materially shaped this change, add the trailer to your commits:

    Assisted-by: LLM
    Assisted-by: LLM clippy

If AI helped you find a bug, confirm you reproduced it yourself.
-->

- [ ] No AI assistance, or AI assistance disclosed with an `Assisted-by:` trailer
- [ ] I can explain every line of this change if a reviewer asks
- [ ] I did not send anything I could not build or test, or I said so above

## Checklist

- [ ] `cargo fmt` is clean
- [ ] `cargo clippy --all-targets -- -D warnings` is clean
- [ ] `cargo test` passes
- [ ] New or changed SQL goes through `sqlx::query!` or `sqlx::query_as!`
- [ ] If any query changed, I ran `cargo sqlx prepare` and committed the `.sqlx` change
- [ ] Migrations are append only, I did not edit an applied migration
- [ ] Commits are signed off (`git commit -s`) and follow Conventional Commits
- [ ] Branch is named `{type}/vai/{short-name}`
- [ ] No secrets, tokens, `.env` files or personal data in the diff
- [ ] No em-dashes in code, comments, commit messages or documentation
- [ ] License headers and `NOTICE` are untouched
- [ ] I updated `docs/ROADMAP.md` or `docs/STACK.md` if this changes a plan or a choice

## Anything a reviewer should know

<!-- Trade-offs you made, places you are unsure, things you deliberately left out. -->
