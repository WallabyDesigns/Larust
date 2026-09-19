//! `xr list` - shows every `xr dev` watch session currently running on this
//! machine, from `dev_registry`. Deliberately doesn't attempt to also
//! enumerate arbitrary served-app processes (an `xr deploy --run` cold
//! start, or a bare `cargo run`) - those have no registry entry of their
//! own to list (see `dev_registry`'s own doc comment for why only `xr
//! dev`'s supervisor needed one), and reaching one to stop it was already
//! fully solved by `xr kill`'s direct admin-channel `STOP` without needing
//! to know it exists ahead of time.
//!
//! The `ID` column is each session's own PID, not a separate counter -
//! `xr kill --id <pid>` reads it directly. A synthetic ordinal would need
//! to stay stable between one `xr list` call and a later `xr kill --id`
//! call for the same session, which nothing here can guarantee (a session
//! from an intervening `xr list` could have exited by then, reflowing the
//! numbering); a PID has no such problem; it already *is* the session's own
//! stable identity for as long as it's still running.

use crate::dev_registry;

pub fn run() {
    let sessions = dev_registry::list_live();
    if sessions.is_empty() {
        println!("No `xr dev` sessions currently running.");
        return;
    }

    println!(
        "{:<10} {:<20} {:<7} {:<10} DIRECTORY",
        "ID", "APP", "PORT", "UPTIME"
    );
    let now = dev_registry::now_unix();
    for session in &sessions {
        let uptime = now.saturating_sub(session.started_at_unix);
        println!(
            "{:<10} {:<20} {:<7} {:<10} {}",
            session.pid,
            session.app_name,
            session.port,
            format_uptime(uptime),
            session.project_dir.display()
        );
    }
}

/// `3661` -> `"1h1m"`, `45` -> `"45s"`, `125` -> `"2m5s"` - just enough
/// precision to tell "just started" from "been running all day" at a
/// glance; nothing here needs second-accuracy once hours are involved.
fn format_uptime(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m{secs}s")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::format_uptime;

    #[test]
    fn formats_seconds_only_under_a_minute() {
        assert_eq!(format_uptime(45), "45s");
    }

    #[test]
    fn formats_minutes_and_seconds_under_an_hour() {
        assert_eq!(format_uptime(125), "2m5s");
    }

    #[test]
    fn formats_hours_and_minutes_dropping_seconds() {
        assert_eq!(format_uptime(3661), "1h1m");
    }

    #[test]
    fn formats_exactly_zero_as_zero_seconds() {
        assert_eq!(format_uptime(0), "0s");
    }
}
