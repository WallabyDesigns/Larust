# Cutting a release

This is the process for tagging a Larust release: a version bump, a git tag,
and a GitHub Release with notes.

It's additive, not a replacement for `xr upgrade`'s own update mechanism:
`xr --version`/`xr upgrade` track the current commit hash directly rather
than a version number (see `crates/larust-cli/src/upgrade.rs`'s own doc
comment for why), and tagging a release doesn't change that - `xr upgrade`
still compares commit hashes, continuously, against `main`, exactly as it
does today. A tag just makes `xr --version` more meaningful (a real number
alongside the hash) and gives people something stable to reference.

## Versioning

[Semantic versioning](https://semver.org/), with the usual pre-1.0
caveat: before `1.0.0`, a `MINOR` bump (`0.1.0` -> `0.2.0`) is the
practical equivalent of a breaking change, and `PATCH` (`0.1.0` -> `0.1.1`)
is everything else - bug fixes, docs, additive features. There's no
promise of API stability before `1.0.0`, same as the rest of the Rust
ecosystem's own pre-1.0 convention.

## Steps

1. **Confirm `main` is actually green.** Check the
   [Actions tab](https://github.com/wallabydesigns/Larust/actions) (or
   `gh run list --branch main --limit 1`) - never tag a commit CI hasn't
   passed on.

2. **Pick the version number** (see Versioning above) and bump it in one
   place - `Cargo.toml`'s `[workspace.package] version` field. Every crate
   inherits it via `version.workspace = true`, so this one line is the
   only edit needed.

   ```bash
   cargo build --workspace   # regenerates Cargo.lock with the new version
   ```

3. **Write the release notes.** `MILESTONES.md` is already the running
   changelog - most releases are just "everything since the last tag,"
   lightly adapted from the relevant milestone entries into release-note
   form (less internal narration, more "what changed" from a user's
   perspective). Don't maintain a second, separate changelog file.

4. **Commit the version bump:**

   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "chore(release): v0.2.0"
   git push
   ```

5. **Tag and push it:**

   ```bash
   git tag -a v0.2.0 -m "v0.2.0"
   git push origin v0.2.0
   ```

6. **Publish the GitHub Release**, referencing the tag:

   ```bash
   gh release create v0.2.0 --title "v0.2.0" --notes-file /path/to/notes.md
   ```

   (Or use the GitHub web UI's "Draft a new release" - functionally
   identical, whichever's more convenient.)

7. **Confirm it worked**: `xr --version` on a fresh `xr upgrade --force`
   should show the new version number alongside the tagged commit's hash.

## What this doesn't do

Cutting a tag doesn't publish anything to crates.io - Larust still isn't
published there (every generated app resolves framework crates as local
path dependencies against a checkout - see `docs/ARCHITECTURE.md`), so a
release today is a GitHub-only checkpoint: a tag, a `Cargo.toml` version
bump, and a notes page. Publishing to crates.io, if that's ever wanted, is
a separate, larger decision - each crate would need its own real,
independent version discipline instead of the shared workspace version
this document describes, since crates.io has no concept of a workspace
version tying dependent crates together.
