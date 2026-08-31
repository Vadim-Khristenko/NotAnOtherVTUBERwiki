# Security policy

English version. Russian translation: [SECURITY.md](../docs/ru/SECURITY.md)

## Supported versions

Nothing is supported yet, because nothing is released. This policy applies to the
repository as it stands today.

| Version | Supported |
|---------|-----------|
| pre-alpha (`dev` branch) | Security issues are accepted and fixed, but there is no release to patch |
| Any future release | Supported for 12 months after the next minor release |

Once there are releases, the table above will list real version numbers.

## Reporting a vulnerability

**Do not open a public issue for a security problem.**

Email **vadim@vai-rice.space** with the subject line starting with `[SECURITY]`.

If you would rather not use email, say so in a private GitHub security advisory on this
repository. GitHub private reporting is enabled for that reason.

Please include:

- What the vulnerability is, in your own words.
- Where it is: file path, crate, endpoint, or component name.
- Steps to reproduce, or a proof of concept if you have one. A failing test is ideal.
- What an attacker could achieve, and under what preconditions.
- Whether you believe it is being exploited anywhere right now.
- Whether you want to be credited, and under what name.

Do not include real user data, real credentials, or anything from a system you do not own.

## What to expect

| Stage | Target |
|-------|--------|
| Acknowledgement | Within 72 hours, always within 7 days |
| Initial assessment | Within 7 days |
| Fix or mitigation plan | Within 30 days for anything rated high or critical |
| Public disclosure | After a fix is available, coordinated with you |

If we disagree about severity, we will explain our reasoning and we will still fix real
problems. We will not quietly ignore a report.

## Disclosure

We prefer coordinated disclosure. The usual timeline is 90 days from acknowledgement,
extended if the fix genuinely needs more work and we are making visible progress. We will
not threaten you to stay quiet, and we will not sit on a report without responding.

We do not publish exploit details ahead of a fix.

## Scope

In scope:

- Authentication, sessions, and password handling
- Authorization and the permission model, including namespace and component rights
- The render pipeline: Markdown, sanitizing, transclusion, component sandbox escapes
- SQL injection, path traversal, SSRF, template injection
- The API, tokens, webhooks, and their signatures
- The MediaWiki compatibility layer
- Anything that lets one wiki affect another on a shared install

Out of scope, report them as normal bugs:

- Vulnerabilities that require an administrator to attack their own install
- Missing security headers on a deployment you control
- Denial of service that is really just "send more traffic than the box can handle"
- Issues in databases, containers or reverse proxies that you chose and configured
- Findings from automated scanners with no demonstrated impact
- Anything that requires disabling a documented security control first

## Safe harbor

If you are acting in good faith, following this policy, not accessing or destroying data
that is not yours, and not disrupting service, we will not pursue legal action against you
and we will not report you. If something goes wrong in the course of good faith research,
tell us and we will work it out.

This project is built by a small team including a minor. Please be decent.

## Recognition

Researchers who report valid issues are credited in the release notes and in the `NOTICE`
credits section, under whatever name they prefer, or anonymously if they prefer that. We
have no bug bounty budget, because the project runs on donations, but we will say thank
you properly and publicly.

## A note on the project's own security posture

The design makes specific commitments that are worth knowing before you report:

- Raw HTML is disabled in Markdown at the parser level, not just sanitized after the fact.
- Component SSR runs in a separate process with no database credentials and no filesystem
  access to wiki data.
- Sessions, cache and queues live in Valkey, so the web tier holds no per-request state.
- Every query touching wiki scoped data takes `wiki_id` from the resolved `WikiContext`.

If you find a place where the code violates one of those, that is a bug even if you cannot
turn it into an exploit.
