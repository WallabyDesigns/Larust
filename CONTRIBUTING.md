# Contributing to Larust

Thanks for taking a look at Larust. This document covers how to get set
up, what's expected of a change, and how to submit one.

## Getting set up

```bash
git clone https://github.com/wallabydesigns/Larust.git
cd Larust
./install.sh       # macOS / Linux / Git Bash
# or, on Windows PowerShell:
.\install.ps1
```

Both scripts bootstrap Rust itself (via rustup) if it isn't already on
`PATH`, then install the `xr` CLI from this checkout. See
[`docs/getting-started/installation.md`](docs/getting-started/installation.md)
for the full walkthrough, including scaffolding a test app to develop
against.

### One-time: enable the pre-push hook

```bash
git config core.hooksPath .githooks
```

Runs `cargo check --workspace --all-targets --locked` before every push -
the exact first check CI itself runs - so a change that doesn't compile
never leaves your machine. Bypass a single push with `git push --no-verify`
if you genuinely need to (e.g. a WIP branch you know is broken).

## Before opening a PR

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

CI runs all three (plus `cargo check --workspace --all-targets --locked`)
on both `ubuntu-latest` and `windows-latest` - both platforms matter here,
not just the usual Linux-only default: zero-downtime restart/deploy has
real, independently-implemented Unix and Windows code paths, and this
project's own history has genuine Windows-only bugs a Linux-only CI
matrix would never have caught.

If you're touching `demo/` or `examples/blog/`, make sure both still build
and their own test suites pass - they're reference apps other people
actually read to understand how a real Larust app is put together, not
throwaway fixtures.

## Commit messages

This repo follows [Conventional Commits](https://www.conventionalcommits.org/):
`type(scope): description`, e.g. `fix(upgrade): ensure "up to date" check
accounts for both checkout and installed binary`. Common types: `feat`,
`fix`, `docs`, `refactor`, `test`, `chore`. The scope is usually the crate
or area touched (`deploy`, `upgrade`, `logging`, `service`, ...).

## Code style and philosophy

A few things that are more deliberate here than in a typical Rust project,
worth knowing before your first PR:

- **Doc comments explain *why*, not *what*.** A comment that just restates
  the code next to it gets deleted in review. A comment that explains a
  real constraint, a bug that motivated a specific shape, or a trade-off
  that isn't obvious from reading the code alone is exactly what belongs
  here. Skim any file in `crates/larust-core/src/` for the expected depth -
  most non-trivial functions carry a paragraph of "why this shape and not
  the obvious one."
- **Verify claims, don't assume them.** If a fix addresses a specific bug,
  the PR description (or a comment) should say how it was confirmed - a
  failing test before the fix and passing after, a reproduction script, a
  concrete before/after. "This should fix it" without verification is not
  the bar here.
- **`docs/GOTCHAS.md`** is where non-obvious, hard-won constraints get
  written down permanently - a real bug, its root cause, and its fix, in
  enough detail that nobody re-discovers it the hard way. If you hit one,
  add an entry.
- **`MILESTONES.md`** is the project's own running changelog, most recent
  entry first - substantial changes get one, written in enough detail
  that "what changed and why" is still clear months later without needing
  to re-read the diff.

## Cutting a release

Maintainers only - see [`RELEASING.md`](RELEASING.md) for the full
process (versioning, tagging, GitHub Releases).

## Reporting a security issue

Please don't open a public issue for a security vulnerability - see
[`SECURITY.md`](SECURITY.md) instead.

## Questions

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) covers the crate graph and
the single-dependency-surface pattern generated apps depend on;
[`docs/GOTCHAS.md`](docs/GOTCHAS.md) covers the non-obvious constraints
already discovered. If neither answers it, open an issue.
