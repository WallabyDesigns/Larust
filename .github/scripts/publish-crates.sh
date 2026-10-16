#!/usr/bin/env bash
# Publishes every Larust crate to crates.io, in dependency order - run
# from the workspace root, by `.github/workflows/publish.yml` on every
# `vX.Y.Z` tag push (the tag `xr release` itself creates, never pushed
# automatically - see that command's own doc comment). Safe to re-run:
# every crate is checked against the real crates.io API *before* ever
# attempting to publish it, and skipped outright if that exact version
# already exists - so a job that died partway through (a transient
# network error, crates.io's own rate limit) can just be re-triggered
# without redoing - or failing on - whatever already succeeded.
#
# This pre-check, not a post-hoc parse of `cargo publish`'s own error
# text, is deliberate: confirmed directly (a real, intentional duplicate
# publish attempt against an already-published version) that the actual
# message is "error: crate <name>@<version> already exists on crates.io
# index" - not "is already uploaded", the wrong string an earlier version
# of this script guessed and matched against, which is exactly why the
# very first re-run attempt still failed outright on the first already-
# published crate it reached instead of skipping it. A pre-check sidesteps
# ever needing to get that exact text right again, and also skips the
# wasted compile time `cargo publish`'s own verify step would spend on a
# crate that doesn't need publishing at all.
#
# `cargo publish` already blocks until each crate is actually resolvable
# on the registry before returning (confirmed directly during the very
# first publish round - see MILESTONES.md), so nothing extra is needed
# between entries for index-propagation delay; this list is purely the
# topological order itself; a crate only ever needs the ones before it.
set -euo pipefail

# Every crate shares one version via `version.workspace = true` - read it
# once here rather than per-crate, the same single-source-of-truth shape
# the workspace's own versioning already has.
read_workspace_version() {
  awk '
    /^\[workspace\.package\]/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && /^version[[:space:]]*=/ {
      match($0, /"[^"]*"/)
      print substr($0, RSTART + 1, RLENGTH - 2)
      exit
    }
  ' Cargo.toml
}

VERSION=$(read_workspace_version)
if [ -z "$VERSION" ]; then
  echo "could not read [workspace.package] version from Cargo.toml" >&2
  exit 1
fi
echo "Target version: $VERSION"

# crates.io's own per-version endpoint - a 200 means that exact version
# already exists for this crate; anything else (404 included) means it
# genuinely still needs publishing.
is_already_published() {
  local crate="$1"
  local status
  status=$(curl -s -o /dev/null -w "%{http_code}" -A "larust-publish-workflow" \
    "https://crates.io/api/v1/crates/$crate/$VERSION")
  [ "$status" = "200" ]
}

CRATES=(
  larust-core
  larust-events
  larust-view
  larust-orm
  larust-lang
  larust-storage
  larust-console
  larust-repository
  larust-convert
  larust-cache
  larust-queue
  larust-scheduler
  larust-validation
  larust-http
  larust-mssql
  larust-auth
  larust-mail
  larust-sitemap
  larust-spa
  larust-live
  larust-reverb
  larust-socialite
  larust-db
  larust-permissions
  larust-sanctum
  larust-notifications
  larust-shield
  larust-macros
  larust-support
  larust-testing
  larust-cli
)

publish_one() {
  local crate="$1"
  local extra_arg="${2:-}"
  echo "::group::Publishing $crate"
  set +e
  output=$(cargo publish -p "$crate" ${extra_arg:+"$extra_arg"} 2>&1)
  status=$?
  set -e
  echo "$output"
  echo "::endgroup::"
  if [ "$status" -eq 0 ]; then
    return 0
  fi
  if echo "$output" | grep -q "already exists on crates.io index"; then
    # Shouldn't normally fire at all - `is_already_published` below
    # already skips before this is ever called - but kept as a defensive
    # backstop against a race (another run publishing the same crate
    # between this job's pre-check and its own publish attempt), not the
    # primary mechanism anymore.
    echo "$crate: already published at this version - skipping"
    return 0
  fi
  echo "Failed to publish $crate - stopping (later crates may depend on it)" >&2
  return 1
}

for crate in "${CRATES[@]}"; do
  if is_already_published "$crate"; then
    echo "$crate: $VERSION already published - skipping"
    continue
  fi
  if [ "$crate" = "larust-macros" ]; then
    # `larust-macros`'s test suite deliberately dev-depends on
    # `larust-support` (to test generated macro code exactly the way real
    # apps consume it) - but `larust-support` has a REAL dependency on
    # `larust-macros` for its own re-exports, and Cargo insists on
    # resolving dev-dependencies at publish time too, even though they
    # never ship to consumers (confirmed directly - see MILESTONES.md's
    # crates.io-publishing entries for the full incident). Breaking that
    # one edge here is safe: this is a disposable CI checkout, thrown
    # away after the job finishes, never the real working tree.
    #
    # `--allow-dirty` is required here, and *only* here: the `sed` below
    # deliberately leaves this one file uncommitted, and `cargo publish`
    # refuses to proceed with any uncommitted change otherwise (confirmed
    # directly - the first real CI run of this script failed on exactly
    # that check, not a dependency problem). Every other crate publishes
    # from an untouched, fully-committed checkout, so it keeps that
    # safety check intact.
    sed -i '/^larust-support = /d' crates/larust-macros/Cargo.toml
    publish_one "$crate" "--allow-dirty"
    git checkout -- crates/larust-macros/Cargo.toml
  else
    publish_one "$crate"
  fi
done
