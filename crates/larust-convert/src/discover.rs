//! Recursive directory discovery - needed for the first time by Blade
//! (`resources/views/**` nests arbitrarily); Phase 1/2a's migrations/
//! config/requests directories are all flat (one `read_dir` call each),
//! so nothing before this needed it.

use std::path::{Path, PathBuf};

/// Every file under `dir` (recursing into subdirectories) whose name ends
/// with `suffix` (e.g. `".blade.php"` - a compound extension, not just
/// `.php`), sorted for deterministic output. A missing or unreadable
/// directory yields an empty list rather than an error - the caller
/// already checks `dir.is_dir()` before deciding whether to convert
/// anything at all.
pub fn find_files_recursive(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut results = Vec::new();
    walk(dir, suffix, &mut results);
    results.sort();
    results
}

fn walk(dir: &Path, suffix: &str, results: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, suffix, results);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.ends_with(suffix))
        {
            results.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_files_matching_suffix_at_any_depth() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("posts")).unwrap();
        std::fs::write(tmp.path().join("welcome.blade.php"), "").unwrap();
        std::fs::write(tmp.path().join("posts/index.blade.php"), "").unwrap();
        std::fs::write(tmp.path().join("posts/notes.txt"), "").unwrap();

        let found = find_files_recursive(tmp.path(), ".blade.php");
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|p| p
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with(".blade.php")));
    }

    #[test]
    fn returns_empty_for_a_missing_directory() {
        let found = find_files_recursive(Path::new("/definitely/does/not/exist"), ".blade.php");
        assert!(found.is_empty());
    }
}
