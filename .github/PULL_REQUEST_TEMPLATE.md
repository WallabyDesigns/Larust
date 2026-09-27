## What this changes and why

<!-- The "why" matters more than the "what" here - see CONTRIBUTING.md's
     "Code style and philosophy" section. If this fixes a bug, say how you
     confirmed it: a failing test before, passing after, or a concrete
     before/after repro. -->

## Checklist

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --check` is clean
- [ ] New/changed behavior has test coverage
- [ ] If this fixes a non-obvious bug, it's written up in
      [`docs/GOTCHAS.md`](../docs/GOTCHAS.md)
- [ ] If this is a substantial change, it has a
      [`MILESTONES.md`](../MILESTONES.md) entry
- [ ] If touching restart/deploy handoff code, verified on **both** Linux
      and Windows (see `CONTRIBUTING.md` for why both matter here)

## Related issue(s)

<!-- Closes #... , or "n/a" -->
