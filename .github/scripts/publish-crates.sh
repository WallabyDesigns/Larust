#!/usr/bin/env bash
# Publishes every Larust crate to crates.io, in dependency order - run
# from the workspace root, by `.github/workflows/publish.yml` on every
# `vX.Y.Z` tag push (the tag `xr release` itself creates, never pushed
# automatically - see that command's own doc comment). Safe to re-run:
# a crate already published at the current version is skipped, not
# treated as a failure, so a job that died partway through (a transient
# network error, crates.io's own rate limit) can just be re-triggered.
#
# `cargo publish` already blocks until each crate is actually resolvable
# on the registry before returning (confirmed directly during the very
# first publish round - see MILESTONES.md), so nothing extra is needed
# between entries for index-propagation delay; this list is purely the
# topological order itself; a crate only ever needs the ones before it.
set -euo pipefail

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
  echo "::group::Publishing $crate"
  set +e
  output=$(cargo publish -p "$crate" 2>&1)
  status=$?
  set -e
  echo "$output"
  echo "::endgroup::"
  if [ "$status" -eq 0 ]; then
    return 0
  fi
  if echo "$output" | grep -q "is already uploaded"; then
    echo "$crate: already published at this version - skipping"
    return 0
  fi
  echo "Failed to publish $crate - stopping (later crates may depend on it)" >&2
  return 1
}

for crate in "${CRATES[@]}"; do
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
    sed -i '/^larust-support = /d' crates/larust-macros/Cargo.toml
    publish_one "$crate"
    git checkout -- crates/larust-macros/Cargo.toml
  else
    publish_one "$crate"
  fi
done
