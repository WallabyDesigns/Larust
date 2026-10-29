//! Finds every *other* currently-running `xr` process on this machine, by
//! executable image name - not `dev_registry`'s own best-effort session
//! tracking, which only ever learns about a process that successfully
//! reached its own registration point inside `xr dev`'s `run()`. This is
//! the fallback for exactly the gap that tracking can't close: something
//! is holding a lock on the installed `xr` binary itself (blocking `cargo
//! install`'s own "replace the old binary" step) that `dev_registry` never
//! learned existed at all - reported directly: `xr upgrade` failed on
//! Windows with "Access is denied" moving the newly-built binary over the
//! installed one, while `xr list` reported no sessions running at all.
//! Nothing a *session* registry knows about can explain a lock from, say,
//! a different terminal's still-open `xr dev`, a hung `xr deploy --run`,
//! or any other `xr` invocation that was never a tracked dev session to
//! begin with.

use std::collections::HashSet;

/// Matches the one specific Windows failure shape that's worth a bounded
/// retry rather than reporting immediately: `ERROR_ACCESS_DENIED` (`os
/// error 5`) on a file operation cargo's own linker/installer just ran
/// against a `.exe` it, or something upstream of it, just finished
/// writing. Reported directly, twice, against two different commands -
/// `xr dev`'s rebuild ("failed to remove file ...debug\<name>.exe") and
/// `xr upgrade`'s reinstall ("failed to move ...cargo-install*\xr.exe to
/// ...\xr.exe") - with [`other_running_xr_pids`] finding nothing both
/// times, ruling out another tracked `xr` process as the culprit. The
/// remaining, much more likely explanation neither command can detect or
/// wait on proactively: Windows Defender (or another antivirus/EDR
/// product) briefly holding a freshly-linked `.exe` open to scan it,
/// clearing on its own well within a couple of seconds. Both call sites
/// share this one predicate rather than duplicating the signature match,
/// even though *what* they retry (a whole `cargo build`, a whole `cargo
/// install`) differs enough that the retry loop itself stays separate in
/// each.
///
/// Deliberately narrow - matches only a file-operation verb ("remove
/// file"/"move") *together with* the Windows error text, not either
/// alone: "Access is denied" by itself could just as easily be a genuine,
/// persistent permissions problem (a read-only `target/`, a non-admin
/// install location) that retrying would never fix, and retrying those
/// would only delay a real error's own message for no benefit.
pub fn looks_like_a_transient_windows_file_lock(message: &str) -> bool {
    (message.contains("failed to remove file") || message.contains("failed to move"))
        && (message.contains("os error 5") || message.contains("Access is denied"))
}

/// Every other process currently running this same executable - always
/// excludes this process's own PID, so calling this from inside `xr`
/// itself (as every caller here does) never reports itself as a
/// candidate to stop.
pub fn other_running_xr_pids() -> Vec<u32> {
    exclude_self(all_pids_for_this_executable(), std::process::id())
}

/// The actual self-exclusion logic behind [`other_running_xr_pids`], split
/// out so it's unit-testable without really shelling out to `tasklist`/
/// `pgrep` - same "split out the pure logic" precedent as `dev.rs`'s own
/// `resolve_app_port`/`upgrade.rs`'s own `plan`. Sorted for a deterministic,
/// readable order in whatever prints these pids - a plain `HashSet`'s own
/// iteration order is unspecified.
fn exclude_self(mut pids: HashSet<u32>, self_pid: u32) -> Vec<u32> {
    pids.remove(&self_pid);
    let mut pids: Vec<u32> = pids.into_iter().collect();
    pids.sort_unstable();
    pids
}

#[cfg(windows)]
fn all_pids_for_this_executable() -> HashSet<u32> {
    // `/FO CSV` gives a stable, quoted format to parse
    // ("Image Name","PID","Session Name","Session#","Mem Usage") -
    // simpler and less locale-dependent than the default table layout
    // `is_alive`/`list_live` parse elsewhere in this crate.
    let Ok(output) = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq xr.exe", "/NH", "/FO", "CSV"])
        .output()
    else {
        return HashSet::new();
    };
    parse_tasklist_csv_pids(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(windows)]
fn parse_tasklist_csv_pids(output: &str) -> HashSet<u32> {
    output
        .lines()
        .filter_map(|line| line.split(',').nth(1)?.trim_matches('"').parse().ok())
        .collect()
}

#[cfg(unix)]
fn all_pids_for_this_executable() -> HashSet<u32> {
    let Ok(output) = std::process::Command::new("pgrep")
        .args(["-x", "xr"])
        .output()
    else {
        return HashSet::new();
    };
    parse_pgrep_pids(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(unix)]
fn parse_pgrep_pids(output: &str) -> HashSet<u32> {
    output
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_a_transient_windows_file_lock_matches_xr_devs_own_report() {
        let message = "error: failed to remove file `E:\\app\\target\\debug\\larust-app.exe`\n\n\
                        Caused by:\n  Access is denied. (os error 5)";
        assert!(looks_like_a_transient_windows_file_lock(message));
    }

    #[test]
    fn looks_like_a_transient_windows_file_lock_matches_xr_upgrades_own_report() {
        let message = "error: failed to move \
                        `C:\\Users\\x\\.cargo\\bin\\cargo-installis30EJ\\xr.exe` to \
                        `C:\\Users\\x\\.cargo\\bin\\xr.exe`\n\nCaused by:\n  Access is denied. \
                        (os error 5)";
        assert!(looks_like_a_transient_windows_file_lock(message));
    }

    #[test]
    fn looks_like_a_transient_windows_file_lock_rejects_an_ordinary_compile_error() {
        let message = "error: expected one of `!` or `::`, found `is`";
        assert!(!looks_like_a_transient_windows_file_lock(message));
    }

    #[test]
    fn looks_like_a_transient_windows_file_lock_rejects_an_unrelated_access_denied() {
        // A genuinely persistent permissions problem would fail
        // identically on every retry - only the exact verb+error
        // combination should match, never "Access is denied" alone.
        let message = "error: Access is denied. (os error 5) opening `.env`";
        assert!(!looks_like_a_transient_windows_file_lock(message));
    }

    #[test]
    fn exclude_self_removes_only_the_matching_pid() {
        let pids = HashSet::from([100, 200, 300]);
        assert_eq!(exclude_self(pids, 200), vec![100, 300]);
    }

    #[test]
    fn exclude_self_returns_results_in_sorted_order() {
        let pids = HashSet::from([300, 100, 200]);
        assert_eq!(exclude_self(pids, 999), vec![100, 200, 300]);
    }

    #[test]
    fn exclude_self_is_a_no_op_when_self_is_not_in_the_set() {
        let pids = HashSet::from([100, 200]);
        assert_eq!(exclude_self(pids, 999), vec![100, 200]);
    }

    #[test]
    fn exclude_self_on_an_empty_set_stays_empty() {
        assert_eq!(exclude_self(HashSet::new(), 1), Vec::<u32>::new());
    }

    #[cfg(windows)]
    #[test]
    fn parse_tasklist_csv_pids_extracts_the_pid_column() {
        let output = "\"xr.exe\",\"12345\",\"Console\",\"1\",\"4,100 K\"\n\
                       \"xr.exe\",\"6789\",\"Console\",\"1\",\"3,900 K\"\n";
        let pids = parse_tasklist_csv_pids(output);
        assert_eq!(pids, HashSet::from([12345, 6789]));
    }

    #[cfg(windows)]
    #[test]
    fn parse_tasklist_csv_pids_ignores_a_no_matching_processes_line() {
        // `tasklist`'s own output when the filter matches nothing - not a
        // CSV row at all, must not be mistaken for one.
        let output = "INFO: No tasks are running which match the specified criteria.\r\n";
        assert_eq!(parse_tasklist_csv_pids(output), HashSet::new());
    }

    #[cfg(unix)]
    #[test]
    fn parse_pgrep_pids_extracts_each_line_as_a_pid() {
        assert_eq!(parse_pgrep_pids("123\n456\n"), HashSet::from([123, 456]));
    }

    #[cfg(unix)]
    #[test]
    fn parse_pgrep_pids_on_no_matches_stays_empty() {
        assert_eq!(parse_pgrep_pids(""), HashSet::new());
    }
}
