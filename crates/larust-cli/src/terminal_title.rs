//! Gives `xr dev`'s own long-lived process a title that names the app it's
//! watching. Found wanting directly: `xr dev`'s own process always shows up
//! as an indistinguishable `xr.exe` (Windows) or `xr` (Unix) command line,
//! so running it against two or more apps at once leaves no way to tell
//! which one is which without cross-referencing PIDs against working
//! directories by hand - exactly the wrong time for that, since the usual
//! reason to look is "one of these needs to be killed right now."
//!
//! Neither half below renames the `xr` executable itself - Windows has no
//! API for a process to rewrite what `tasklist`/Task Manager report as its
//! own image name, and there'd be nothing analogous to gain on Unix either
//! (the binary on disk is still just `xr`). Both instead relabel whatever
//! *is* mutable at runtime:
//!
//! - [`proctitle::set_title`] (the `proctitle` crate) - on Windows,
//!   `SetConsoleTitleW` (the text Task Manager's grouped "Apps" view and
//!   `tasklist /V`'s "Window Title" column show for this console) plus a
//!   named event handle carrying the title, discoverable in Process
//!   Explorer/Process Hacker even with no console at all - the same
//!   technique PostgreSQL uses on Windows for the identical reason. On
//!   Linux, `prctl(PR_SET_NAME)` (what `ps`/`top`/`pgrep -f` call a
//!   process's "comm", truncated to 15 bytes). On the BSDs, a real
//!   `setproctitle()` call (no truncation - it rewrites `argv` itself, the
//!   same technique nginx/postgres use there too).
//! - A raw OSC 0 escape sequence on non-Windows - `proctitle` has no
//!   terminal-title behavior of its own outside Windows, so without this,
//!   a Linux/macOS user running `xr dev` in several terminal tabs would
//!   still see every tab labeled identically even though each underlying
//!   process now has a distinguishable name.
//!
//! A user can then find and end the right one - `Get-Process xr |
//! Where-Object MainWindowTitle -like "*myapp*" | Stop-Process` on Windows,
//! `pkill -f "xr dev (myapp)"` on Linux - without matching PIDs to
//! directories by hand.
//!
//! Windows' console title is a property of the console *window*, not of
//! any one process attached to it - so calling this once, early in `xr
//! dev`'s `run()`, labels the whole session for as long as that window
//! stays open, even once a rebuilt app binary (which never touches the
//! title itself) takes over as the console's foreground process.

/// `title` is used as-is - no validation, no truncation beyond whatever the
/// underlying OS call itself imposes (e.g. Linux's 15-byte `comm` limit). A
/// title that's merely unusual (empty, absurdly long, containing control
/// characters) degrades to a merely-unusual title, never a reason to fail
/// `xr dev`'s own startup over something this cosmetic.
pub fn set(title: &str) {
    proctitle::set_title(title);
    set_terminal_title(title);
}

#[cfg(not(windows))]
fn set_terminal_title(title: &str) {
    use std::io::Write;
    print!("\x1b]0;{title}\x07");
    let _ = std::io::stdout().flush();
}

// `proctitle::set_title` already calls `SetConsoleTitleW` on Windows - a
// second OSC-0 write would be redundant, and unlike a real terminal
// emulator, `cmd.exe`/legacy `conhost.exe` sessions render the raw escape
// bytes as literal text instead of interpreting them.
#[cfg(windows)]
fn set_terminal_title(_title: &str) {}
