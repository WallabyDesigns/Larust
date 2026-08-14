//! `CONVERSION_REPORT.md` — the trust mechanism this whole tool is built
//! around. Every item a converter touches lands in exactly one bucket
//! below; nothing is silently dropped, nothing is silently guessed at.
//! Expands on `rust-laravel.md`'s own two-bucket sketch ("Converted
//! automatically" / "Requires manual review") with a third bucket for
//! third-party (composer) packages, split into the two-tier design agreed
//! before this crate was built: packages with a hand-curated Larust
//! equivalent, and packages with none (named, never guessed at).

/// One "requires manual review" category — a heading plus the specific
/// items that triggered it. Per-item file-path detail, not just a count:
/// a bare "8 dynamic Eloquent scopes" is useless for a design whose whole
/// point is "never silently drop, always name it."
#[derive(Debug, Clone)]
pub struct ManualReviewSection {
    pub heading: String,
    pub items: Vec<String>,
}

/// One composer package entry — `note` is either the Larust equivalent
/// pointer (Tier 1) or a short "no mapping" explanation (Tier 2).
#[derive(Debug, Clone)]
pub struct PackageNote {
    pub name: String,
    pub version: String,
    pub note: String,
}

#[derive(Debug, Clone, Default)]
pub struct ConversionReport {
    /// One line per fully-mechanical category, e.g. "42 routes
    /// (routes/web.php, routes/api.php)" — a bare count is fine here,
    /// since nothing in this bucket needs a human to go look at it.
    pub converted_automatically: Vec<String>,
    pub manual_review: Vec<ManualReviewSection>,
    pub packages_mapped: Vec<PackageNote>,
    pub packages_unmapped: Vec<PackageNote>,
    /// Structure not attempted at all in this phase — named so nothing
    /// looks like it was silently ignored by accident.
    pub not_attempted: Vec<String>,
}

impl ConversionReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_manual_review(&mut self, heading: impl Into<String>, items: Vec<String>) {
        if items.is_empty() {
            return;
        }
        self.manual_review.push(ManualReviewSection {
            heading: heading.into(),
            items,
        });
    }

    pub fn render(&self) -> String {
        let mut out = String::from("# Conversion Report\n\n");

        out.push_str("## Converted automatically\n");
        if self.converted_automatically.is_empty() {
            out.push_str("(nothing converted automatically)\n");
        } else {
            for line in &self.converted_automatically {
                out.push_str(&format!("- {line}\n"));
            }
        }
        out.push('\n');

        out.push_str("## Requires manual review\n");
        if self.manual_review.is_empty() {
            out.push_str("(nothing flagged)\n");
        } else {
            for section in &self.manual_review {
                out.push_str(&format!(
                    "### {} ({})\n",
                    section.heading,
                    section.items.len()
                ));
                for item in &section.items {
                    out.push_str(&format!("- {item}\n"));
                }
            }
        }
        out.push('\n');

        out.push_str("## Third-party packages\n");
        out.push_str("### Tier 1 — mapped to a Larust equivalent\n");
        if self.packages_mapped.is_empty() {
            out.push_str("(none yet — the mapping table is populated deliberately, one package at a time; see `composer.rs`)\n");
        } else {
            for pkg in &self.packages_mapped {
                out.push_str(&format!("- {} {} — {}\n", pkg.name, pkg.version, pkg.note));
            }
        }
        out.push_str("### Tier 2 — no mapping, flagged\n");
        if self.packages_unmapped.is_empty() {
            out.push_str("(none)\n");
        } else {
            for pkg in &self.packages_unmapped {
                out.push_str(&format!("- {} {} — {}\n", pkg.name, pkg.version, pkg.note));
            }
        }
        out.push('\n');

        out.push_str("## Not attempted in this phase\n");
        if self.not_attempted.is_empty() {
            out.push_str("(nothing)\n");
        } else {
            for line in &self.not_attempted {
                out.push_str(&format!("- {line}\n"));
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_produces_all_five_sections() {
        let report = ConversionReport::new();
        let rendered = report.render();
        assert!(rendered.contains("## Converted automatically"));
        assert!(rendered.contains("## Requires manual review"));
        assert!(rendered.contains("## Third-party packages"));
        assert!(rendered.contains("### Tier 1"));
        assert!(rendered.contains("### Tier 2"));
        assert!(rendered.contains("## Not attempted in this phase"));
    }

    #[test]
    fn empty_manual_review_section_is_dropped_not_rendered_as_zero() {
        let mut report = ConversionReport::new();
        report.add_manual_review("Nothing here", vec![]);
        assert!(report.manual_review.is_empty());
    }

    #[test]
    fn manual_review_section_shows_count_and_every_item() {
        let mut report = ConversionReport::new();
        report.add_manual_review(
            "Migrations using timestamps()",
            vec!["database/migrations/0001_create_posts_table.php — created_at/updated_at columns emitted; no automatic population".to_string()],
        );
        let rendered = report.render();
        assert!(rendered.contains("### Migrations using timestamps() (1)"));
        assert!(rendered.contains("0001_create_posts_table.php"));
    }

    #[test]
    fn tier_1_and_tier_2_packages_render_separately() {
        let mut report = ConversionReport::new();
        report.packages_unmapped.push(PackageNote {
            name: "spatie/laravel-permission".to_string(),
            version: "^6.0".to_string(),
            note: "no Larust equivalent; port manually".to_string(),
        });
        let rendered = report.render();
        assert!(rendered.contains("spatie/laravel-permission ^6.0 — no Larust equivalent"));
    }
}
