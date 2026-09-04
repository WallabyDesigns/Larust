//! Copies the source Laravel app's static asset files - `public/` and the
//! pre-build `resources/css`/`resources/js` source - into the converted
//! project verbatim. Every converted Blade template already references
//! these exact paths (`/images/...`, `/css/app.css`, `/favicon.ico`, ...)
//! unchanged (an asset path is a plain string literal, never PHP/Blade
//! syntax `blade::expr` would translate), and Larust serves `public/`
//! directly at the URL root (`ServeDir` in `larust-core`'s `Application`)
//! the same way Laravel's own webserver docroot does - so without this
//! phase, every converted page renders unstyled and imageless even though
//! its own HTML is otherwise correct.

use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AssetSummary {
    pub public_files: usize,
    pub resource_files: usize,
}

impl AssetSummary {
    pub fn total(&self) -> usize {
        self.public_files + self.resource_files
    }
}

/// `public/index.php` - Laravel's own PHP front controller - is the one
/// top-level entry deliberately skipped; everything else under `public/`
/// (images, already-built CSS/JS, favicons, `robots.txt`, vendored
/// third-party assets, ...) is copied through unchanged. `resources/css`
/// and `resources/js` (Laravel's own pre-build asset *source*, as opposed
/// to `public/`'s already-built output) land at those *exact same*
/// paths in the converted project - never rehomed under Larust's own
/// `resources/assets/` scaffold convention, unlike a fresh `xr new` app
/// with no existing frontend build config to preserve. This is load-
/// bearing, not cosmetic: `vite.config.js` (copied verbatim by
/// `xr::convert::copy_node_tooling` - see that function's own doc
/// comment) still names `resources/css/app.min.css`/`resources/js/
/// app.min.js` in its own `input` array, unchanged, and Vite's build
/// manifest is keyed by those exact strings - the same strings
/// `blade::scan`'s `@vite(...)` → `@code larust_support::vitex::tags(&
/// [...])` translation hardcodes verbatim from the original template.
/// Moving the source files anywhere else would silently break every one
/// of those lookups. Either source directory being absent is not an
/// error - plenty of real Laravel apps have no `resources/css`/
/// `resources/js` at all (Blade-only, no frontend build step).
pub fn convert(laravel_root: &Path, out_root: &Path) -> Result<AssetSummary> {
    let mut summary = AssetSummary::default();

    let source_public = laravel_root.join("public");
    if source_public.is_dir() {
        summary.public_files = copy_dir(&source_public, &out_root.join("public"), &["index.php"])?;
    }

    for name in ["css", "js"] {
        let source = laravel_root.join("resources").join(name);
        if source.is_dir() {
            summary.resource_files +=
                copy_dir(&source, &out_root.join("resources").join(name), &[])?;
        }
    }

    Ok(summary)
}

/// Copies `package.json`, `vite.config.js`/`.ts`, and `postcss.config.js`
/// verbatim, and `tailwind.config.js`/`.ts` with one deliberate text
/// substitution - so `npm install && npm run dev`/`npm run build` work
/// in the converted project exactly as they did in the original Laravel
/// app, giving `@vitex`'s own dev/production detection (`public/hot`,
/// `public/build/manifest.json` - see `larust_support::vitex`'s own doc
/// comment) something real to find.
///
/// `vite.config.js` is copied **completely unchanged**, on purpose - its
/// own `input` array is what Vite's build manifest keys come from, and
/// those exact strings are also what `blade::scan`'s `@vite(...)` →
/// `@vitex` translation hardcoded verbatim from the original template
/// (see `convert`'s own doc comment above). Editing either side of that
/// pairing without the other breaks every asset lookup silently.
///
/// `tailwind.config.js` is different: its `content` glob only tells
/// Tailwind's own JIT engine which files to scan for class names, read
/// by nothing else in this pipeline - but the source app's own glob
/// (real source: `"./resources/**/*.blade.php"`) would now match zero
/// files, since the converted templates live under `.blade.xr` and no
/// `.blade.php` file exists in the new project at all. Every literal
/// `.blade.php` gets rewritten to `.blade.xr` so Tailwind actually finds
/// the real, converted templates.
pub fn copy_node_tooling(laravel_root: &Path, out_root: &Path) -> Result<Vec<String>> {
    let mut copied = Vec::new();
    for name in [
        "package.json",
        "vite.config.js",
        "vite.config.ts",
        "postcss.config.js",
    ] {
        let source = laravel_root.join(name);
        if source.is_file() {
            std::fs::copy(&source, out_root.join(name))
                .with_context(|| format!("copying {name}"))?;
            copied.push(name.to_string());
        }
    }
    for name in ["tailwind.config.js", "tailwind.config.ts"] {
        let source = laravel_root.join(name);
        if source.is_file() {
            let content = std::fs::read_to_string(&source)
                .with_context(|| format!("reading {}", source.display()))?;
            std::fs::write(
                out_root.join(name),
                content.replace(".blade.php", ".blade.xr"),
            )
            .with_context(|| format!("writing {name}"))?;
            copied.push(name.to_string());
        }
    }
    Ok(copied)
}

/// Recursively copies `source`'s contents into `dest` (creating `dest` and
/// any intermediate directories as needed), skipping any *top-level* entry
/// (a direct child of `source`) whose file name matches one in
/// `skip_top_level` - used for `public/index.php` only; nothing nested
/// deeper is ever skipped (recursive calls always pass `&[]`). Returns the
/// number of files actually copied. Symlinks are silently skipped - not
/// observed in any real Laravel `public/` directory this has run against,
/// and copying them safely (resolve vs. preserve) needs a real decision
/// this doesn't attempt to guess.
fn copy_dir(source: &Path, dest: &Path, skip_top_level: &[&str]) -> Result<usize> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    let mut count = 0;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("reading {}", source.display()))?
    {
        let entry = entry?;
        let file_name = entry.file_name();
        if skip_top_level
            .iter()
            .any(|skip| file_name == std::ffi::OsStr::new(skip))
        {
            continue;
        }
        let source_path = entry.path();
        let dest_path = dest.join(&file_name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            count += copy_dir(&source_path, &dest_path, &[])?;
        } else if file_type.is_file() {
            std::fs::copy(&source_path, &dest_path).with_context(|| {
                format!(
                    "copying {} to {}",
                    source_path.display(),
                    dest_path.display()
                )
            })?;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn copies_public_recursively_skipping_only_the_top_level_index_php() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        write(&laravel_root.join("public/index.php"), "<?php");
        write(&laravel_root.join("public/favicon.ico"), "ico-bytes");
        write(&laravel_root.join("public/images/logo.svg"), "<svg></svg>");

        let summary = convert(&laravel_root, &out_root).unwrap();

        assert_eq!(summary.public_files, 2);
        assert!(!out_root.join("public/index.php").exists());
        assert_eq!(
            std::fs::read_to_string(out_root.join("public/favicon.ico")).unwrap(),
            "ico-bytes"
        );
        assert_eq!(
            std::fs::read_to_string(out_root.join("public/images/logo.svg")).unwrap(),
            "<svg></svg>"
        );
    }

    #[test]
    fn copies_resources_css_and_js_to_the_same_paths_vite_config_expects() {
        // Real source: `vite.config.js`'s own `input` array names
        // `resources/css/app.min.css`/`resources/js/app.min.js` verbatim
        // - copied here unchanged, not rehomed under `resources/assets/`,
        // so `vite.config.js` (copied as-is) still finds its own source
        // files and the build manifest keys still match what `@vitex`
        // requests.
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        write(&laravel_root.join("resources/css/app.css"), "body{}");
        write(&laravel_root.join("resources/js/app.js"), "console.log(1)");

        let summary = convert(&laravel_root, &out_root).unwrap();

        assert_eq!(summary.resource_files, 2);
        assert_eq!(
            std::fs::read_to_string(out_root.join("resources/css/app.css")).unwrap(),
            "body{}"
        );
        assert_eq!(
            std::fs::read_to_string(out_root.join("resources/js/app.js")).unwrap(),
            "console.log(1)"
        );
    }

    #[test]
    fn a_missing_public_or_resources_css_js_directory_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        std::fs::create_dir_all(&laravel_root).unwrap();
        let out_root = dir.path().join("out");

        let summary = convert(&laravel_root, &out_root).unwrap();

        assert_eq!(summary, AssetSummary::default());
    }

    #[test]
    fn copies_package_json_and_vite_config_completely_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&out_root).unwrap();
        write(
            &laravel_root.join("package.json"),
            r#"{"scripts": {"dev": "vite"}}"#,
        );
        write(
            &laravel_root.join("vite.config.js"),
            "export default { plugins: [] };",
        );
        write(
            &laravel_root.join("postcss.config.js"),
            "export default { plugins: {} };",
        );

        let copied = copy_node_tooling(&laravel_root, &out_root).unwrap();

        assert_eq!(
            copied,
            vec!["package.json", "vite.config.js", "postcss.config.js"]
        );
        assert_eq!(
            std::fs::read_to_string(out_root.join("vite.config.js")).unwrap(),
            "export default { plugins: [] };"
        );
    }

    #[test]
    fn tailwind_config_gets_its_blade_php_glob_rewritten_to_blade_xr() {
        // Real source: `tailwind.config.js`'s own `content: ["./resources/
        // **/*.blade.php", ...]` - the converted templates live under
        // `.blade.xr`, so the original glob would match nothing.
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&out_root).unwrap();
        write(
            &laravel_root.join("tailwind.config.js"),
            "export default { content: [\"./resources/**/*.blade.php\", \"./resources/**/*.js\"] };",
        );

        let copied = copy_node_tooling(&laravel_root, &out_root).unwrap();

        assert_eq!(copied, vec!["tailwind.config.js"]);
        let content = std::fs::read_to_string(out_root.join("tailwind.config.js")).unwrap();
        assert!(content.contains("./resources/**/*.blade.xr"));
        assert!(!content.contains(".blade.php"));
        assert!(content.contains("./resources/**/*.js"));
    }

    #[test]
    fn missing_node_tooling_files_are_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        std::fs::create_dir_all(&laravel_root).unwrap();
        let out_root = dir.path().join("out");
        std::fs::create_dir_all(&out_root).unwrap();

        let copied = copy_node_tooling(&laravel_root, &out_root).unwrap();

        assert!(copied.is_empty());
    }

    #[test]
    fn a_nested_index_php_inside_public_is_not_skipped() {
        // The skip list only applies to `public/index.php` at the true
        // top level - `skip_top_level` is never propagated into recursive
        // calls, so a same-named file nested deeper (e.g. a vendored
        // third-party demo page) is treated as ordinary content.
        let dir = tempfile::tempdir().unwrap();
        let laravel_root = dir.path().join("laravel");
        let out_root = dir.path().join("out");
        write(
            &laravel_root.join("public/vendor/widget/index.php"),
            "<?php // demo",
        );

        let summary = convert(&laravel_root, &out_root).unwrap();

        assert_eq!(summary.public_files, 1);
        assert!(out_root.join("public/vendor/widget/index.php").exists());
    }
}
