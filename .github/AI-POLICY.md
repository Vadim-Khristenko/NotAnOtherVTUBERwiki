# Policy on AI assisted contributions

English version. Russian translation: [AI-POLICY.md](../docs/ru/AI-POLICY.md)

## Position, in one paragraph

**Using AI tools on this project is allowed, welcome, and never required.** We do not ban
AI assistance in code, and we do not demand that anyone use it. What we demand is the same
thing we would demand of any contribution: that the human sending it understands it, stands
behind it, and can explain it. If you can walk us through every line of your change and why
it is correct, it does not matter to us whether a model, a rubber duck, or three cups of
coffee helped you get there.

This is not a compromise position. It is the position the Linux kernel arrived at after a
long public argument, and we are deliberately aligning with it. See
[AI Coding Assistants](https://docs.kernel.org/process/coding-assistants.html) in the
kernel documentation.

---

## Why we are not banning it

Two reasons, one practical and one principled.

**Practical.** Automated analysis is genuinely good at the boring half of software
maintenance: sweeping for a bug class across a codebase, spotting the uninitialised read,
flagging the missing bounds check, finding the place where the same mistake was made four
times. Telling contributors they may not use the best available tool for that would make
the project worse for no gain.

**Principled.** We judge work by what it is, not by how it was made. A patch that fixes a
real bug and is well explained is a good patch. Provenance matters for licensing and for
honesty, which is why we ask for disclosure below, but it does not change whether the code
is correct.

We are also not going to pretend this is costless. Automated tooling produces a lot of
noise, and unverified machine generated reports waste an enormous amount of maintainer
time. That is exactly why the rules below are about **verification and accountability**
rather than about prohibition.

---

## The rules

### 1. A human is responsible for every contribution

Paraphrasing the kernel policy, and we mean it the same way:

- **AI agents must not add a `Signed-off-by` line.** Only a human can certify the
  Developer Certificate of Origin. Our DCO requirement is in
  [CONTRIBUTING.md](CONTRIBUTING.md).
- The human submitter is responsible for reviewing all AI generated code, for ensuring
  licensing compliance, and for adding their own `Signed-off-by`.
- You take full responsibility for the contribution. "The model wrote it" is never an
  answer to a review question.

### 2. You must be able to explain it

This is the whole test. Before you open a pull request, you should be able to answer, for
any line a reviewer points at:

- What does this line do?
- Why does it need to be there?
- What breaks if it is removed?

If you cannot answer those, do not send it yet. Go understand it first. This is not a
gatekeeping hoop, it is the difference between contributing and forwarding.

We are not going to set a numeric acceptance threshold, because the honest number is this:
a change whose author can explain it and that is correct will be merged. A change whose
author cannot explain it will not, regardless of whether a model produced it.

### 3. Disclosure is optional but appreciated

We are not going to police how you work. But if AI assistance materially shaped a change,
tagging it helps everyone, and it costs you one line. We use the kernel's tag:

```text
Assisted-by: LLM
Assisted-by: LLM clippy
Assisted-by: LLM cargo-deny
```

Put it in the commit message trailer, where `Signed-off-by` goes. List specialised
analysis tools if you used any. Do not list ordinary tooling like git, cargo, your editor,
or the compiler.

### 4. If you used AI to find a bug, verify it first

This one matters more than it sounds. The kernel policy is blunt about the reason:
maintainers currently waste too much time analysing unverified reports. Reports that look
plausible and are wrong are worse than no report at all, because they cost attention.

Before reporting a bug you found with assistance:

1. Reproduce it. If you cannot, say so explicitly in the report.
2. Check it is in our code and not in a dependency, a container, or your own config.
3. Check it is not already reported or already fixed on `dev`.
4. If you can fix it, fix it. A report plus a patch is far more useful than a report.
5. Say plainly which parts you could not build, test, or verify.

Security reports follow [SECURITY.md](SECURITY.md) and must be sent privately. An
unverified machine generated vulnerability report sent publicly is still an unverified
vulnerability report.

### 5. Do not send what you have not run

Generated code that has never been compiled, never been tested, and never been read by its
author is not a contribution. If you could not build or test something, say so in the pull
request. We would much rather review an honest "I could not test this on Windows" than
discover it ourselves.

### 6. Art, images, and media are covered separately

This file is about code and technical contributions. Images, artwork and other media are
governed by [AI-ASSETS.md](../AI-ASSETS.md). The short version: tell us whether a work is
human made, AI assisted, or AI generated, because we label generated assets honestly and we
will not relabel a generated image as human made.

### 7. Licensing still applies in full

AI assistance does not change the licence of your contribution. Code is still
AGPL-3.0-or-later, still covered by the DCO, and still needs a human sign off. If a tool
you used has terms that would contaminate the contribution, do not use it for this project.

---

## What will get a contribution rejected

Not "you used AI". These will:

- You cannot explain what the change does when asked.
- The change is unverified and you presented it as tested.
- The commit carries a `Signed-off-by` that was not added by a human.
- Generated content is presented as human authored, in code or in art.
- A large generated dump that clearly was not read by its author.
- Licensing is unclear or contaminated.

---

## A note on that Linux kernel CVE story

You may have seen the headline that the kernel fixed hundreds of CVEs at once with AI help.
The underlying event is real and worth understanding correctly, so here is what the primary
source says.

On 2026-07-21, Jan Schaumann posted to the oss-sec mailing list noting that the Linux
kernel published **432 CVEs between 2026-07-19T09:09 and 2026-07-20T16:27**, on top of the
more than 40 already published that month. The post is a discussion of how anyone is
supposed to triage that volume, and it mentions pointing an LLM at the intake as one
possible aid, sceptically. It does not claim AI found or fixed those issues.

We are citing it for a narrower and more useful point: **finding potential problems is
becoming cheap, and verifying them is the scarce skill.** That is precisely why this policy
is written around accountability and reproduction rather than around whether a tool was
involved.

Sources, so you can check us: the
[oss-sec post](https://seclists.org/oss-sec/2026/q3/198) and the
[linux-cve-announce archive](https://lore.kernel.org/linux-cve-announce/).

---

## Summary

| Question | Answer |
|----------|--------|
| May I use AI tools? | Yes |
| Must I use them? | No |
| Must I disclose? | No, but please use the `Assisted-by:` trailer |
| Must a human sign off? | Yes, always |
| Must I understand the change? | Yes, this is the actual test |
| Must I verify bugs before reporting? | Yes |
| Will a PR be rejected for using AI? | No |
| Will it be rejected if you cannot explain it? | Yes |
