//! `composer.json` → third-party package report. No PHP parsing needed
//! here — `composer.json` is plain JSON, read with `serde_json` (already a
//! workspace dependency).
//!
//! **Packages are never auto-ported.** A small, hand-curated mapping table
//! (below) is populated deliberately over time, the same one-at-a-time way
//! `larust-mail`/`larust-queue`/`larust-scheduler`/`larust-notifications`
//! were each built as individual crates — never auto-generated PHP-to-Rust
//! translation of a package's internals. Anything not in the table is
//! named, with its version constraint, in the report — never silently
//! dropped, never guessed at.

use crate::report::PackageNote;
use anyhow::{Context, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
}

/// A single hand-curated entry: a composer package name mapped to a short
/// note pointing at its Larust equivalent. Starts empty/near-empty — its
/// value is the mechanism and the detection, not a pre-built library of
/// ports (see this module's own doc comment). Add an entry here only once
/// a real Larust equivalent actually exists and has been verified to cover
/// the package's common usage — never as a hopeful placeholder.
///
/// Not every entry here points at a hand-built Larust crate — three
/// shapes of note actually land in this table:
/// - a real, hand-built crate in this workspace (`spatie/laravel-permission`);
/// - a package whose role is already fully covered by Larust's own
///   architecture, nothing to port (`laravel/octane` — its whole reason
///   to exist, avoiding PHP's per-request bootstrap cost, doesn't apply
///   to an already-compiled, already-long-running native binary);
/// - a package that trivially maps to an existing crates.io crate, no
///   Larust-specific wrapper needed at all (`stripe/stripe-php` →
///   `async-stripe`).
const TIER_1: &[(&str, &str)] = &[
    (
        "spatie/laravel-permission",
        "maps to larust-permissions (this workspace) — compile-checked permission/role names, \
         DB-backed assignment; see its own doc comment for the hybrid design and what's still \
         manual (Blade @can/@role directives, role:/permission: middleware strings)",
    ),
    (
        "laravel/octane",
        "not needed — Larust's own compiled, long-running native binary already avoids the \
         per-request PHP bootstrap cost Octane exists to eliminate",
    ),
    (
        "stripe/stripe-php",
        "maps to the async-stripe crate (crates.io) — Stripe's own actively-maintained Rust \
         SDK; add it directly, no Larust-specific wrapper needed",
    ),
];

/// Parses `composer.json`'s `require` object into `(package, version)`
/// pairs, in the order they appear in the file. Skips `php` itself (a
/// runtime version constraint, not a package). Malformed JSON or a missing
/// `require` object is a real error — this is called only after the
/// caller has already confirmed the file exists.
pub fn parse_require(source: &str) -> Result<Vec<Package>> {
    let value: serde_json::Value =
        serde_json::from_str(source).context("parsing composer.json as JSON")?;
    let require = value
        .get("require")
        .and_then(|r| r.as_object())
        .context("composer.json has no `require` object")?;

    let mut packages = Vec::new();
    for (name, version) in require {
        if name == "php" {
            continue;
        }
        let version = version.as_str().unwrap_or("*").to_string();
        packages.push(Package {
            name: name.clone(),
            version,
        });
    }
    // `serde_json::Value::Object` preserves insertion order (the `preserve_order`
    // feature isn't enabled here, so it's a `BTreeMap` internally — sort
    // explicitly so output is deterministic regardless of that).
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

/// `true` if `packages` looks like a real Laravel application — used to
/// fail `xr convert` fast on an unrelated directory rather than three
/// minutes into a partial conversion.
pub fn looks_like_laravel(packages: &[Package]) -> bool {
    packages.iter().any(|p| p.name == "laravel/framework")
}

/// Splits `packages` into the two report tiers. `laravel/framework` itself
/// is excluded — it isn't a third-party dependency to port, Larust *is*
/// its wholesale replacement, so listing it as "no Larust equivalent;
/// port manually" would be misleading noise rather than a real gap.
pub fn classify(packages: &[Package]) -> (Vec<PackageNote>, Vec<PackageNote>) {
    let tier_1: BTreeMap<&str, &str> = TIER_1.iter().copied().collect();
    let mut mapped = Vec::new();
    let mut unmapped = Vec::new();

    for package in packages {
        if package.name == "laravel/framework" {
            continue;
        }
        if let Some(note) = tier_1.get(package.name.as_str()) {
            mapped.push(PackageNote {
                name: package.name.clone(),
                version: package.version.clone(),
                note: note.to_string(),
            });
        } else {
            unmapped.push(PackageNote {
                name: package.name.clone(),
                version: package.version.clone(),
                note: "no Larust equivalent; port manually".to_string(),
            });
        }
    }

    (mapped, unmapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "require": {
            "php": "^8.2",
            "laravel/framework": "^11.0",
            "spatie/laravel-permission": "^6.0",
            "spatie/laravel-activitylog": "^4.0"
        }
    }"#;

    #[test]
    fn parse_require_skips_php_and_extracts_the_rest() {
        let packages = parse_require(SAMPLE).unwrap();
        assert_eq!(packages.len(), 3);
        assert!(packages
            .iter()
            .any(|p| p.name == "laravel/framework" && p.version == "^11.0"));
        assert!(packages
            .iter()
            .any(|p| p.name == "spatie/laravel-permission" && p.version == "^6.0"));
    }

    #[test]
    fn looks_like_laravel_checks_for_the_framework_package() {
        let packages = parse_require(SAMPLE).unwrap();
        assert!(looks_like_laravel(&packages));
        assert!(!looks_like_laravel(&[]));
    }

    #[test]
    fn classify_splits_mapped_and_unmapped_packages() {
        // `spatie/laravel-permission` is a real TIER_1 entry (points at
        // `larust-permissions`); `spatie/laravel-activitylog` has no
        // mapping yet and stays unmapped — this is the mapped/unmapped
        // split actually exercised, not just "everything's unmapped
        // because the table happens to be empty."
        let packages = parse_require(SAMPLE).unwrap();
        let (mapped, unmapped) = classify(&packages);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].name, "spatie/laravel-permission");
        assert!(mapped[0].note.contains("larust-permissions"));
        assert_eq!(unmapped.len(), 1);
        assert_eq!(unmapped[0].name, "spatie/laravel-activitylog");
    }

    #[test]
    fn classify_excludes_laravel_framework_itself() {
        let packages = parse_require(SAMPLE).unwrap();
        let (_, unmapped) = classify(&packages);
        assert!(!unmapped.iter().any(|p| p.name == "laravel/framework"));
    }

    #[test]
    fn parse_require_rejects_missing_require_object() {
        assert!(parse_require("{}").is_err());
    }
}
