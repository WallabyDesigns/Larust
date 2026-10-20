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
    output.lines().filter_map(|line| line.trim().parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
