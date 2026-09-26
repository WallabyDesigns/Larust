# Security Policy

## Reporting a vulnerability

Please **do not** open a public GitHub issue for a security vulnerability.

Instead, use GitHub's private vulnerability reporting:
[github.com/wallabydesigns/Larust/security/advisories/new](https://github.com/wallabydesigns/Larust/security/advisories/new).
This opens a private conversation with the maintainers - nothing is
visible publicly until a fix is ready and you both agree to disclose it.

Please include:

- What the vulnerability is and its potential impact
- Steps to reproduce it (a minimal example app or test case is ideal)
- Which version/commit you found it in

## Supported versions

Larust doesn't yet follow a formal release/support-window schedule - until
it does, security fixes are only guaranteed against the current `main`
branch. If you're running an older commit, update first and confirm the
issue still reproduces before reporting.

## Scope

Larust is a framework: most of what an app built on it does - its own
routes, its own auth checks, its own data handling - is that app's own
responsibility, not something a framework-level report should cover.
Vulnerabilities in the framework itself are in scope, including (but not
limited to):

- Authentication/session handling (`larust-auth`, session cookies, CSRF)
- The `xr` CLI's own admin/restart channel, or its systemd integration
  (`xr service:install`)
- Anything that could let one request/session access another's data by
  default, with no app-level mistake required
- Template rendering that could allow injection (XSS, SSTI-equivalent)
  through normal, documented usage

Out of scope: vulnerabilities that require an app author to have already
made an unsafe choice the docs explicitly warn against (e.g. leaving
`APP_DEBUG=true` in production, as documented in
[`docs/the-basics/error-handling.md`](docs/the-basics/error-handling.md)).
