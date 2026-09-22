---
title: Deployment & Desktop Apps
nav_order: 10
---

# Deployment & Desktop Apps
{: .no_toc }

1. TOC
{:toc}

## Opting into zero-downtime restarts

By default, a generated app behaves exactly like any plain `axum::serve`
program - Ctrl+C exits immediately. Zero-downtime restart is a real,
opt-in change to your process-lifecycle behavior, in two independent
steps:

```rust
Application::at_root(env!("CARGO_MANIFEST_DIR"), config::app::config)?
    .router(route.into_axum_router())
    .with_graceful_shutdown(GracefulShutdown {
        drain_timeout: Duration::from_secs(30),
        restart_channel: true,
    })
    .serve().await
```

- No `.with_graceful_shutdown(...)` call at all → unchanged, original
  behavior.
- `restart_channel: false` → graceful shutdown on Ctrl+C/SIGTERM only (a
  smaller, legitimate feature on its own): stop accepting new connections,
  drain in-flight ones, with a hard `drain_timeout` backstop so a stuck
  connection can't block a shutdown forever.
- `restart_channel: true` → the full mechanism below.

`xr new` doesn't enable either by default - this is a deliberate step
your app takes once you understand the drain-timeout trade-off, not
silently baked in.

## How the handoff actually works

No external supervisor, reverse proxy, or process manager required (and
nothing here conflicts with one either, if you already use one). The
currently-running process spawns its own replacement, **hands it the
exact same listening socket** it was already using (real inter-process
socket-passing - `fcntl`-based fd inheritance on Unix,
`WSADuplicateSocketW` on Windows - not a second process racing for the
same port), waits for the replacement to confirm it's genuinely serving,
and only then drains its own in-flight requests and exits. Verified with
real subprocess integration tests driving continuous HTTP traffic through
an actual restart, with zero failed requests across the handoff.

```bash
xr restart
```

Sends the restart command over a local admin channel (a Unix socket /
Windows named pipe). `xr dev` and `xr deploy` both build on this exact
same mechanism internally - every rebuild during `xr dev` is a
zero-downtime handoff, not a stop-then-start.

## `xr deploy`

```bash
xr deploy [--run] [--service]
```

```
# .env
DEPLOY_TYPE=web   # default
```

`cargo build --release`, publish to a fresh, monotonically-increasing
slot under `storage/releases/` (release builds and `xr dev`'s own dev
builds use separate counting namespaces, so one can't prune the other's
files away), then the same restart-handoff `xr restart` uses against
whatever's currently running. Builds frontend assets first
(`npm run build`) when `node_modules/` exists, stopping the deploy
outright if that build fails - a broken asset build must never ship.

The very first deploy of an app that's never been started has nothing
running to hand off to; `--run` cold-starts the freshly published binary
in the background for you instead of just publishing it and leaving you
to start it manually.

{: .warning }
`xr deploy --run` starts the app for as long as the machine and its
process table live - nothing about it survives a reboot, or restarts the
app if it actually crashes. `xr deploy --service` (below) is the
one-line fix; plain `--run` alone is really only worth reaching for on a
throwaway/test box you don't care about surviving a restart.

## Surviving a crash or a reboot

```bash
xr deploy --service     # first deploy: build, publish, and register with systemd, in one line
xr service:install      # equivalent, if you'd rather do it as its own separate step
xr service:uninstall    # undo it
```

`--service` is `--run`'s systemd-backed sibling, for the exact same
"nothing is running to hand off to yet" moment: instead of a bare
detached process, it calls the same `xr service:install` logic below
directly, so the very first deploy is already crash-and-reboot-safe with
no second command needed. Wins over `--run` if both are given - starting
the app both ways at once would just race each other for the same port.
Same scope as `--run` otherwise: no effect once something is already
running (every later `xr deploy` just hot-swaps it, exactly as before),
none on `DEPLOY_TYPE=app` builds.

Linux (`systemd`) only today. Registers the app as a real `systemd`
service: `WorkingDirectory` and `ExecStart` point at whatever `storage/
releases/current` currently resolves to, `Restart=on-failure` brings it
back after an actual crash, and `WantedBy=multi-user.target` (via
`systemctl enable`) starts it automatically on boot - closing exactly the
gap a plain `xr deploy --run` leaves open.

Deliberately `Restart=on-failure`, not `Restart=always`: this framework's
own zero-downtime restart handoff works by having the *old* process spawn
its own replacement, hand off the listening socket, drain in-flight
requests, and only then exit cleanly - a normal, successful exit on every
single ordinary `xr deploy`/`xr restart`, not a failure. `Restart=always`
can't tell that apart from a real crash: `systemd` would see the old
process exit and spawn *another* fresh instance racing the handoff's own
already-running replacement for the same port - the zero-downtime
mechanism broken by the very thing meant to keep the app running.
`on-failure` only ever fires on an actual crash (a non-zero exit or a
killing signal), so ordinary deploys keep working exactly as before -
`xr service:install` is a one-time setup step, not something later
deploys need to know about.

If `/etc/systemd/system/` isn't writable (not running as root), it prints
the unit file and the exact `sudo` commands to run instead of failing
silently.

## Tauri desktop apps

```bash
xr new myapp --tauri     # scaffold Tauri support from the start
xr add tauri              # or retrofit it onto an existing app
```

```
# .env
DEPLOY_TYPE=app
```

```bash
xr deploy       # cargo tauri build, from src-tauri/
```

Uses an "embedded local server" model: `src-tauri/src/main.rs` spawns
your app's own `serve()` on a background tokio runtime, then points a
native OS webview at `http://127.0.0.1:{port}` - one real router, reached
over loopback instead of a browser tab, not a rewritten or parallel
implementation. This is exactly why a generated app's boot sequence lives
in `src/lib.rs` (`connect_database`/`port`/`application`/`router`/`serve`)
rather than inline in `src/main.rs`'s `main()` - an embedded host reuses
those functions directly instead of spawning your compiled binary as a
subprocess.

`src-tauri/Cargo.toml` is a plain sibling crate depending on your app via
`path = ".."`. Icons aren't generated at scaffold time - `cargo tauri
build` fails with an install-style hint (`cargo tauri icon <path>`) if
`src-tauri/icons/` is still empty, the same "clear hint, no silent
half-working state" posture `xr audit` takes for a missing `cargo-audit`
install.

## Next

You've now covered the whole framework surface. [Converting a Laravel
App](../converting-a-laravel-app) if you're bringing an existing project
over, or back to [Home](../) for the full map.
