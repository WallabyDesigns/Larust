//! Larust's own answer to Laravel's `@vite(...)` directive — real
//! integration with the app's existing Vite/Tailwind toolchain (the
//! `laravel-vite-plugin`-based `vite.config.js`/`tailwind.config.js`/
//! `package.json` a converted app's own `resources/assets/` already
//! carries over), not a from-scratch asset bundler. `xr convert`
//! translates every real `@vite([...])` call it finds into a
//! `@code`/`{!! !!}` pair calling [`tags`] with the exact same entry-point
//! strings — see `larust_convert::blade::scan`'s own `"vite"` directive
//! arm.
//!
//! Mirrors Vite's own dual dev/production behavior:
//! - **Dev**: `public/hot` exists (written by `vite`'s dev server on
//!   startup, removed on clean shutdown — the same file Laravel's own
//!   `@vite` directive checks) → emit `<script type="module">` tags
//!   pointing at the live dev server, enabling real HMR (including
//!   Tailwind's own JIT recompilation on every save) exactly as it would
//!   for the original Laravel app.
//! - **Production**: no `public/hot` → read `public/build/manifest.json`
//!   (Vite's own build manifest, keyed by the same source paths
//!   `vite.config.js`'s `input` list — and `@vite(...)`'s own call
//!   sites — already use) and emit `<link>`/`<script>` tags pointing at
//!   the real hashed, already-built output. An ES module entry's own
//!   `import`s (e.g. a vendor chunk) don't need a separate tag — the
//!   browser's module loader resolves those from the entry file's own
//!   `import` statements; only a `.css` array (CSS pulled in *from* a JS
//!   entry, extracted into its own file at build time) needs an explicit
//!   `<link>`, since CSS has no equivalent runtime auto-loading.

use serde_json::Value;

const HOT_FILE: &str = "public/hot";
const MANIFEST_FILE: &str = "public/build/manifest.json";

/// Renders the `<link>`/`<script>` tags for `entries` (dotted-slash
/// source paths, e.g. `"resources/css/app.min.css"` — exactly what the
/// original `@vite([...])` call listed). Degrades to an empty string
/// (never a broken page) when neither `public/hot` nor a build manifest
/// exists yet — a fresh checkout that hasn't run `npm run dev`/`npm run
/// build` at all — and silently skips any entry the manifest doesn't
/// know about, rather than guessing a URL that might not exist.
pub fn tags(entries: &[&str]) -> String {
    if let Ok(hot) = std::fs::read_to_string(HOT_FILE) {
        return dev_tags(hot.trim(), entries);
    }
    let Ok(manifest_text) = std::fs::read_to_string(MANIFEST_FILE) else {
        return String::new();
    };
    let Ok(manifest) = serde_json::from_str::<Value>(&manifest_text) else {
        return String::new();
    };
    production_tags(&manifest, entries)
}

fn dev_tags(hot_url: &str, entries: &[&str]) -> String {
    let mut out = format!("<script type=\"module\" src=\"{hot_url}/@vite/client\"></script>");
    for entry in entries {
        out.push_str(&format!(
            "<script type=\"module\" src=\"{hot_url}/{entry}\"></script>"
        ));
    }
    out
}

fn production_tags(manifest: &Value, entries: &[&str]) -> String {
    let mut out = String::new();
    for entry in entries {
        let Some(item) = manifest.get(entry) else {
            continue;
        };
        if let Some(file) = item.get("file").and_then(Value::as_str) {
            out.push_str(&asset_tag(file));
        }
        if let Some(css_list) = item.get("css").and_then(Value::as_array) {
            for css in css_list.iter().filter_map(Value::as_str) {
                out.push_str(&asset_tag(css));
            }
        }
    }
    out
}

fn asset_tag(file: &str) -> String {
    if file.ends_with(".css") {
        format!("<link rel=\"stylesheet\" href=\"/build/{file}\">")
    } else {
        format!("<script type=\"module\" src=\"/build/{file}\"></script>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `tags()` reads real relative paths (`public/hot`, `public/build/
    // manifest.json`) off the process's own CWD — matching every other
    // runtime path in this framework (`AppPaths::default()` is CWD-based
    // too). Tests that need a specific CWD state serialize through this
    // lock and always restore the original CWD, since `std::env::
    // set_current_dir` is process-global and `cargo test` runs cases in
    // parallel threads by default.
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    fn in_temp_cwd<F: FnOnce()>(f: F) {
        let _guard = CWD_LOCK.lock().unwrap();
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        f();
        std::env::set_current_dir(original).unwrap();
    }

    #[test]
    fn dev_mode_emits_the_client_script_and_one_module_script_per_entry() {
        in_temp_cwd(|| {
            std::fs::create_dir_all("public").unwrap();
            std::fs::write("public/hot", "http://localhost:5173").unwrap();

            let out = tags(&["resources/css/app.min.css", "resources/js/app.min.js"]);

            assert!(out.contains(
                "<script type=\"module\" src=\"http://localhost:5173/@vite/client\"></script>"
            ));
            assert!(out.contains(
                "<script type=\"module\" src=\"http://localhost:5173/resources/css/app.min.css\"></script>"
            ));
            assert!(out.contains(
                "<script type=\"module\" src=\"http://localhost:5173/resources/js/app.min.js\"></script>"
            ));
        });
    }

    #[test]
    fn production_mode_reads_the_manifest_and_emits_hashed_urls() {
        // Real shape: `WallabyLivewire`'s own `public/build/manifest.json`.
        in_temp_cwd(|| {
            std::fs::create_dir_all("public/build").unwrap();
            std::fs::write(
                "public/build/manifest.json",
                r#"{
                    "resources/css/app.min.css": {
                        "file": "css/app-CIgGbGpH.css",
                        "isEntry": true
                    },
                    "resources/js/app.min.js": {
                        "file": "js/app.min-DfglQ1R1.js",
                        "isEntry": true,
                        "imports": ["_vendor-BlbTNsYY.js"]
                    }
                }"#,
            )
            .unwrap();

            let out = tags(&["resources/css/app.min.css", "resources/js/app.min.js"]);

            assert!(out.contains("<link rel=\"stylesheet\" href=\"/build/css/app-CIgGbGpH.css\">"));
            assert!(out.contains(
                "<script type=\"module\" src=\"/build/js/app.min-DfglQ1R1.js\"></script>"
            ));
            // The vendor chunk is a real ES module `import` inside the
            // entry file itself — the browser resolves it automatically,
            // no separate tag needed.
            assert!(!out.contains("vendor"));
        });
    }

    #[test]
    fn a_js_entry_with_its_own_imported_css_gets_a_link_tag_too() {
        in_temp_cwd(|| {
            std::fs::create_dir_all("public/build").unwrap();
            std::fs::write(
                "public/build/manifest.json",
                r#"{
                    "resources/js/app.js": {
                        "file": "js/app-abc123.js",
                        "isEntry": true,
                        "css": ["css/app-def456.css"]
                    }
                }"#,
            )
            .unwrap();

            let out = tags(&["resources/js/app.js"]);

            assert!(
                out.contains("<script type=\"module\" src=\"/build/js/app-abc123.js\"></script>")
            );
            assert!(out.contains("<link rel=\"stylesheet\" href=\"/build/css/app-def456.css\">"));
        });
    }

    #[test]
    fn an_entry_missing_from_the_manifest_is_silently_skipped() {
        in_temp_cwd(|| {
            std::fs::create_dir_all("public/build").unwrap();
            std::fs::write("public/build/manifest.json", "{}").unwrap();

            assert_eq!(tags(&["resources/js/app.js"]), "");
        });
    }

    #[test]
    fn neither_hot_file_nor_manifest_degrades_to_an_empty_string() {
        in_temp_cwd(|| {
            assert_eq!(tags(&["resources/js/app.js"]), "");
        });
    }

    #[test]
    fn dev_mode_wins_over_a_stale_production_manifest() {
        in_temp_cwd(|| {
            std::fs::create_dir_all("public/build").unwrap();
            std::fs::write("public/hot", "http://localhost:5173").unwrap();
            std::fs::write(
                "public/build/manifest.json",
                r#"{"resources/js/app.js": {"file": "js/app-stale.js"}}"#,
            )
            .unwrap();

            let out = tags(&["resources/js/app.js"]);

            assert!(out.contains("localhost:5173"));
            assert!(!out.contains("stale"));
        });
    }
}
