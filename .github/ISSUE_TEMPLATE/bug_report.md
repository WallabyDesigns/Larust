---
name: Bug report
about: Something in Larust itself isn't working as documented
title: ""
labels: bug
assignees: ""
---

**What happened**

A clear description of the bug - what you expected vs. what actually
happened.

**Steps to reproduce**

A minimal repro is ideal - a small generated app (`xr new`) and the exact
commands that trigger it, or a failing test case if you've already gotten
that far.

1. ...
2. ...

**Environment**

- Larust commit/tag (`xr --version`):
- OS (Linux/Windows/macOS):
- Rust version (`rustc --version`):

**Relevant output**

```
paste any error output, panic message, or log lines here
```

---

Before filing: check [`docs/GOTCHAS.md`](../../docs/GOTCHAS.md) - it's
where already-diagnosed, non-obvious issues get written down, and yours
might already be covered there. If this is a security vulnerability,
please use [`SECURITY.md`](../../SECURITY.md) instead of a public issue.
