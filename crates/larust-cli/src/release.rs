//! `xr release [--major|--minor|--patch]` - a thin front-end over plain
//! `git`/file commands, the same shape `xr audit`/`xr update` already
//! use over `cargo audit`/`cargo update` - not a new deployment
//! mechanism, and not a git reimplementation. Prepares a release:
//! optionally bumps this app's own `Cargo.toml` version, builds release
//! notes from the commits since the last tag (grouped by this
//! workspace's own commit-type convention - `feat`/`fix`/`refactor`/
//! `perf`/`docs`/`test`/`ci`/`chore`), and creates a local, annotated
//! git tag.
//!
//! **Deliberately never pushes anything, and never deploys anything.**
//! Pushing a tag/branch is visible to everyone else with access to the
//! remote and can't be quietly undone the way a local commit can - this
//! command stops at "ready to push, whenever you decide to," and prints
//! the exact command to run next rather than running it. `xr deploy`
//! (a separate, already-existing command) is still the thing that
//! actually builds and restarts the app - running on whichever machine
//! actually serves it, pulling whatever `git ref` that machine's own
//! remote/checkout points at by then. This command only prepares the
//! *tag*; wiring a server to fetch and deploy automatically on push
//! (git's own `post-receive` hook, "push to deploy") is a separate,
//! already-well-understood pattern this command doesn't need to
//! reinvent - see `docs/deployment-and-desktop-apps.md`.
//!
//! **Why bumping commits before tagging, not just tagging bare**: a tag
//! with no corresponding version bump still works (pass no `--major`/
//! `--minor`/`--patch` flag, and the current `Cargo.toml` version is
//! tagged as-is), but when a bump *is* requested, the changed
//! `Cargo.toml` needs to land in a real commit for the tag to point at
//! something that actually matches the version string it's named after
//! - an uncommitted file change isn't part of any commit's tree at all.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bump {
    Major,
    Minor,
    Patch,
}

pub fn run(bump: Option<Bump>) -> Result<()> {
    ensure_clean_working_tree()?;

    let cargo_toml_path = Path::new("Cargo.toml");
    anyhow::ensure!(
        cargo_toml_path.is_file(),
        "no Cargo.toml found here (run this from an app's own root)"
    );
    let contents = std::fs::read_to_string(cargo_toml_path)
        .with_context(|| format!("reading {}", cargo_toml_path.display()))?;
    let current_version = read_package_version(&contents).with_context(|| {
        format!(
            "couldn't find a `version = \"...\"` line under [package] in {}",
            cargo_toml_path.display()
        )
    })?;

    // Computed from *current* `HEAD`, before any version-bump commit
    // below - so a bump's own "chore: release vX.Y.Z" commit doesn't
    // show up inside the very notes describing everything since the
    // last release.
    //
    // `git describe` fails outright when there's no tag at all yet (a
    // real, expected case for a project's first release, not an error
    // worth propagating) - falls back to every commit in the repo.
    let last_tag = run_git_output(&["describe", "--tags", "--abbrev=0"]).ok();
    let range = match &last_tag {
        Some(prev) if !prev.trim().is_empty() => format!("{}..HEAD", prev.trim()),
        _ => "HEAD".to_string(),
    };
    let log = run_git_output(&["log", &range, "--pretty=format:%s"])?;
    let notes = release_notes(&log);

    let version = match bump {
        Some(bump) => {
            let bumped = bump_version(&current_version, bump).with_context(|| {
                format!(
                    "Cargo.toml's version {current_version:?} isn't a plain major.minor.patch \
                     triple - bump it by hand instead"
                )
            })?;
            let updated = set_package_version(&contents, &current_version, &bumped)
                .context("found the [package] version line once, then lost it - this is a bug")?;
            std::fs::write(cargo_toml_path, updated)
                .with_context(|| format!("writing {}", cargo_toml_path.display()))?;
            run_git(&["commit", "-am", &format!("chore: release v{bumped}")])
                .context("failed to commit the version bump")?;
            println!("Bumped Cargo.toml: {current_version} -> {bumped}");
            bumped
        }
        None => current_version,
    };

    let tag = format!("v{version}");
    let existing = run_git_output(&["tag", "--list", &tag])?;
    anyhow::ensure!(
        existing.trim().is_empty(),
        "tag {tag} already exists - pass --major/--minor/--patch to release a new version, or \
         delete the existing tag yourself (`git tag -d {tag}`) if this one needs redoing"
    );

    let tag_message = if notes.is_empty() {
        tag.clone()
    } else {
        format!("{tag}\n\n{notes}")
    };
    run_git(&["tag", "-a", &tag, "-m", &tag_message]).context("failed to create the git tag")?;

    println!("\nCreated tag {tag} locally - nothing pushed yet.");
    if !notes.is_empty() {
        println!("\n{notes}");
    }
    println!(
        "\nWhen ready:\n    git push && git push origin {tag}\n\nThen deploy on the server that \
         actually runs this app:\n    xr deploy"
    );

    Ok(())
}

fn ensure_clean_working_tree() -> Result<()> {
    let status = run_git_output(&["status", "--porcelain"])?;
    anyhow::ensure!(
        status.trim().is_empty(),
        "the working tree isn't clean - commit or stash your changes before running `xr release`"
    );
    Ok(())
}

/// Finds the `version = "..."` line inside `Cargo.toml`'s `[package]`
/// table specifically - not a workspace member's `[workspace.package]`
/// default, and not some dependency's own inline `version = "..."`
/// (`serde = { version = "1" }`), both of which share the exact same
/// bare key name a naive whole-file search would match. A small,
/// deliberately narrow line scanner rather than pulling in a `toml`-
/// parsing dependency for reading and rewriting one field - the same
/// "duplicate a few lines rather than add a dependency for them"
/// reasoning this codebase already applies elsewhere (e.g. `model.rs`'s
/// own `now_unix_secs`, ported rather than depending on `larust-queue`
/// for an identical one).
fn package_version_line_index(cargo_toml: &str) -> Option<usize> {
    let mut in_package_section = false;
    for (i, line) in cargo_toml.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package_section = trimmed == "[package]";
            continue;
        }
        if in_package_section && trimmed.starts_with("version") {
            let rest = trimmed["version".len()..].trim_start();
            if rest.starts_with('=') {
                return Some(i);
            }
        }
    }
    None
}

fn extract_quoted_value(line: &str) -> Option<String> {
    let (_, rhs) = line.split_once('=')?;
    let rhs = rhs.trim();
    let rhs = rhs.strip_prefix('"')?;
    let rhs = rhs.strip_suffix('"')?;
    Some(rhs.to_string())
}

fn read_package_version(cargo_toml: &str) -> Option<String> {
    let index = package_version_line_index(cargo_toml)?;
    extract_quoted_value(cargo_toml.lines().nth(index)?)
}

fn set_package_version(cargo_toml: &str, old_version: &str, new_version: &str) -> Option<String> {
    let index = package_version_line_index(cargo_toml)?;
    let old_quoted = format!("\"{old_version}\"");
    let new_quoted = format!("\"{new_version}\"");
    let rewritten: Vec<String> = cargo_toml
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i == index {
                line.replacen(&old_quoted, &new_quoted, 1)
            } else {
                line.to_string()
            }
        })
        .collect();
    let mut joined = rewritten.join("\n");
    if cargo_toml.ends_with('\n') {
        joined.push('\n');
    }
    Some(joined)
}

/// A plain `major.minor.patch` bump - no pre-release/build-metadata
/// handling (`1.2.3-beta.1`, `1.2.3+build5`). `xr new`'s own scaffolded
/// `Cargo.toml` never writes either form, and nothing in this codebase
/// needs them yet to justify the extra parsing surface.
fn bump_version(current: &str, bump: Bump) -> Option<String> {
    let mut parts = current.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(match bump {
        Bump::Major => format!("{}.0.0", major + 1),
        Bump::Minor => format!("{major}.{}.0", minor + 1),
        Bump::Patch => format!("{major}.{minor}.{}", patch + 1),
    })
}

/// Groups commit subjects (`git log --pretty=format:%s`, one per line)
/// by this workspace's own commit-type convention - `feat:`/`fix:`/
/// `refactor:`/`perf:`/`docs:`/`test:`/`ci:`/`chore:`. A subject with no
/// such prefix (or a type this convention doesn't name) lands under
/// "Other" rather than being silently dropped - a non-conforming commit
/// message should still be visible in the notes, not invisible because
/// it didn't match a pattern.
fn release_notes(log: &str) -> String {
    const SECTIONS: &[(&str, &str)] = &[
        ("feat", "Features"),
        ("fix", "Fixes"),
        ("refactor", "Refactoring"),
        ("perf", "Performance"),
        ("docs", "Docs"),
        ("test", "Tests"),
        ("ci", "CI"),
        ("chore", "Chores"),
    ];
    let mut grouped: Vec<Vec<&str>> = vec![Vec::new(); SECTIONS.len()];
    let mut other: Vec<&str> = Vec::new();

    for subject in log.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let matched = SECTIONS.iter().enumerate().find_map(|(i, (prefix, _))| {
            subject
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix(':'))
                .map(|rest| (i, rest.trim()))
        });
        match matched {
            Some((i, rest)) => grouped[i].push(rest),
            None => other.push(subject),
        }
    }

    let mut out = String::new();
    for (i, (_, label)) in SECTIONS.iter().enumerate() {
        if grouped[i].is_empty() {
            continue;
        }
        out.push_str(label);
        out.push_str(":\n");
        for item in &grouped[i] {
            out.push_str("- ");
            out.push_str(item);
            out.push('\n');
        }
        out.push('\n');
    }
    if !other.is_empty() {
        out.push_str("Other:\n");
        for item in &other {
            out.push_str("- ");
            out.push_str(item);
            out.push('\n');
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn run_git(args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .status()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    anyhow::ensure!(status.success(), "git {} failed", args.join(" "));
    Ok(())
}

fn run_git_output(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    anyhow::ensure!(output.status.success(), "git {} failed", args.join(" "));
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_package_version_not_a_dependencys_own() {
        let cargo_toml = "[package]\nname = \"demo\"\nversion = \"1.2.3\"\n\n[dependencies]\nserde = { version = \"1.0.0\" }\n";
        assert_eq!(read_package_version(cargo_toml), Some("1.2.3".to_string()));
    }

    #[test]
    fn returns_none_with_no_package_section_at_all() {
        let cargo_toml = "[workspace]\nmembers = [\".\"]\n";
        assert_eq!(read_package_version(cargo_toml), None);
    }

    #[test]
    fn set_package_version_only_touches_the_package_lines_version() {
        let cargo_toml = "[package]\nname = \"demo\"\nversion = \"1.2.3\"\n\n[dependencies]\nserde = { version = \"1.2.3\" }\n";
        let updated = set_package_version(cargo_toml, "1.2.3", "1.3.0").unwrap();
        assert_eq!(
            updated,
            "[package]\nname = \"demo\"\nversion = \"1.3.0\"\n\n[dependencies]\nserde = { version = \"1.2.3\" }\n"
        );
    }

    #[test]
    fn bump_version_covers_all_three_kinds() {
        assert_eq!(bump_version("1.2.3", Bump::Major).unwrap(), "2.0.0");
        assert_eq!(bump_version("1.2.3", Bump::Minor).unwrap(), "1.3.0");
        assert_eq!(bump_version("1.2.3", Bump::Patch).unwrap(), "1.2.4");
    }

    #[test]
    fn bump_version_rejects_a_non_triple() {
        assert!(bump_version("1.2.3-beta.1", Bump::Patch).is_none());
        assert!(bump_version("1.2", Bump::Patch).is_none());
        assert!(bump_version("not-a-version", Bump::Patch).is_none());
    }

    #[test]
    fn release_notes_groups_by_commit_type_and_keeps_unrecognized_ones() {
        let log = "feat: add widgets\nfix: broken thing\nfeat: add gadgets\nsomething else entirely\nchore: bump deps";
        let notes = release_notes(log);
        assert_eq!(
            notes,
            "Features:\n- add widgets\n- add gadgets\n\nFixes:\n- broken thing\n\nChores:\n- bump deps\n\nOther:\n- something else entirely"
        );
    }

    #[test]
    fn release_notes_is_empty_for_an_empty_log() {
        assert_eq!(release_notes(""), "");
        assert_eq!(release_notes("\n\n"), "");
    }

    #[test]
    fn release_notes_does_not_confuse_a_type_prefixed_word_with_the_real_type() {
        // "fixture: ..." starts with "fix" but isn't the `fix:` type -
        // the required `:` immediately after the bare type name is what
        // this guards against matching.
        let notes = release_notes("fixture: add test data");
        assert_eq!(notes, "Other:\n- fixture: add test data");
    }
}
