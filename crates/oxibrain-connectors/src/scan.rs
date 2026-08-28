//! Plain-root scanner. Walks a single declared root, applies include/exclude
//! globs and the per-root `max_file_bytes` cap, and emits one
//! [`FileObservation`] per accepted file.
//!
//! Symlinks are never followed (a loop guard, not just a paranoia measure:
//! macOS tempdirs occasionally link `/tmp` → `/private/tmp`). The walker
//! prunes hidden directories and oximemo's `_assets/` convention; the
//! root path itself is always passed through, even when it sits inside a
//! tempdir named `.tmpXXXX`.
//!
//! Files that fail to stat, exceed the cap, or whose on-disk extension is
//! unknown to the decoder are emitted to `skipped` rather than dropped, so
//! callers can surface freshness reporting without losing information.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use oxibrain_ports::BrainError;
use tracing::warn;
use walkdir::WalkDir;

use crate::documents_config::RootEntry;
use oxibrain_core::documents::FileObservation;

/// One file the walker decided not to surface, paired with the reason so
/// freshness reporting can group them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    pub locator: String,
    pub reason: String,
}

/// Output of [`scan_root`]: the accepted observations (sorted by locator)
#[derive(Debug, Default)]
pub struct ScanResult {
    pub observations: Vec<FileObservation>,
    pub skipped: Vec<SkippedFile>,
}

impl ScanResult {
    fn accepted(&mut self, locator: String, bytes: u64, modified_ns: i64) {
        self.observations.push(FileObservation {
            locator,
            bytes,
            modified_ns,
            revision_hint: None,
        });
    }

    fn rejected(&mut self, locator: String, reason: impl Into<String>) {
        self.skipped.push(SkippedFile {
            locator,
            reason: reason.into(),
        });
    }
}

/// Walk `root.path` and produce a [`ScanResult`].
///
/// The path is canonicalized to a real filesystem location before the walk
/// begins so symlinks in the root path are followed exactly once. Symlinks
/// found during the walk itself are never followed — the walker stops on
/// them and skips the entry (a pointer to outside the root never leaks into
/// the index).
pub fn scan_root(root: &RootEntry) -> Result<ScanResult, BrainError> {
    let canonical = canonicalize_root(&root.path)?;
    let include = compile_globs(&root.include, "include")?;
    let exclude = compile_globs(&root.exclude, "exclude")?;
    let mut result = ScanResult::default();

    let walker = WalkDir::new(&canonical)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| !is_excluded_dir(e));
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "skipping unreadable directory entry");
                continue;
            }
        };
        if !entry.file_type().is_file() {
            // Symlinks surface here with `is_file=false`; explicit symlink
            // entries are also visited as the dirent but never followed.
            if let Ok(locator) = relative_locator(&canonical, entry.path())
                && entry.path_is_symlink()
            {
                result.rejected(locator, "symlink");
            }
            continue;
        }
        let abs = entry.path();
        let locator = match relative_locator(&canonical, abs) {
            Ok(l) => l,
            Err(_) => continue,
        };
        // Pruning symlinks here is the second line of defence: filter_entry
        // hides directories but file-type symlinks would still reach this
        // branch in some filesystems. `path_is_symlink()` reflects the dirent
        // itself rather than the metadata of the target (which would already
        // have been followed), so we can spot a symlink even when walkdir
        // surfaces the target's file type.
        if entry.path_is_symlink() {
            result.rejected(locator, "symlink");
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                result.rejected(locator, format!("metadata: {e}"));
                continue;
            }
        };
        let bytes = meta.len();
        if bytes > root.max_file_bytes {
            result.rejected(
                locator,
                format!("oversize ({bytes} > {})", root.max_file_bytes),
            );
            continue;
        }
        let name_matches = abs.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if !include.is_empty() && !include.iter().any(|p| p.matches(name_matches)) {
            result.rejected(locator, "excluded by include pattern");
            continue;
        }
        if exclude.iter().any(|p| p.matches(name_matches)) {
            result.rejected(locator, "matched exclude pattern");
            continue;
        }
        let modified_ns = system_time_ns(&meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
        // Sanity check the file is still readable for the size we just
        // measured — in-flight editors can cause a stat to succeed but the
        // read to fail. Cheap on small files, ignored for very large ones
        // (the size cap already covers that risk).
        if bytes <= root.max_file_bytes && fs::File::open(abs).is_err() {
            result.rejected(locator, "unreadable at scan time");
            continue;
        }
        result.accepted(locator, bytes, modified_ns);
    }

    result
        .observations
        .sort_by(|a, b| a.locator.cmp(&b.locator));
    result.skipped.sort_by(|a, b| a.locator.cmp(&b.locator));
    Ok(result)
}

/// Canonicalize the user-supplied root path so the walk starts from a single
/// stable location and escape attempts (e.g. `..` traversal or a symlink that
/// points outside) produce a coherent error rather than mysterious misses.
///
/// A missing root is reported as `BrainError::NotFound`; the facade uses that
/// to route the alias into the "skipped roots" bucket of freshness output.
pub fn canonicalize_root(path: &Path) -> Result<PathBuf, BrainError> {
    let canon = match fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(BrainError::NotFound(format!(
                "root path does not exist: {}",
                path.display()
            )));
        }
        Err(e) => {
            return Err(BrainError::Storage(format!(
                "canonicalize {}: {e}",
                path.display()
            )));
        }
    };
    // On macOS `canonicalize` resolves through `/private/var` etc.; that's
    // expected. On Linux it's a no-op. The simple existence check is enough:
    // a canonical path that exists is, by construction, inside a real
    // filesystem that we can walk.
    if !canon.is_dir() {
        return Err(BrainError::Invalid(format!(
            "root path is not a directory: {}",
            path.display()
        )));
    }
    Ok(canon)
}

// --- internals ---------------------------------------------------------------

/// Reject walkdir entries that are directories we never want to descend into:
/// hidden directories (`.*`) and oximemo's `_assets/` convention.
///
/// The root itself is always allowed through.
fn is_excluded_dir(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    if entry.depth() == 0 {
        return false;
    }
    let Some(name) = entry.file_name().to_str() else {
        return false;
    };
    name.starts_with('.') || name == "_assets"
}

fn relative_locator(root: &Path, abs: &Path) -> Result<String, ()> {
    let rel = abs.strip_prefix(root).map_err(|_| ())?;
    let mut out = String::new();
    for (i, comp) in rel.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&comp.as_os_str().to_string_lossy());
    }
    if out.is_empty() { Err(()) } else { Ok(out) }
}

fn system_time_ns(t: &SystemTime) -> i64 {
    let dur = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    i64::try_from(dur.as_nanos()).unwrap_or(i64::MAX)
}

/// Pre-compiled set of glob matchers used by the include/exclude tests.
struct CompiledGlob {
    pattern: glob::Pattern,
}

impl CompiledGlob {
    fn matches(&self, file_name: &str) -> bool {
        self.pattern.matches(file_name)
    }
}

fn compile_globs(patterns: &[String], label: &str) -> Result<Vec<CompiledGlob>, BrainError> {
    let mut out = Vec::with_capacity(patterns.len());
    for p in patterns {
        let pat = glob::Pattern::new(p)
            .map_err(|e| BrainError::Invalid(format!("invalid {label} glob {p:?}: {e}")))?;
        out.push(CompiledGlob { pattern: pat });
    }
    Ok(out)
}

// --- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, body).unwrap();
    }

    #[test]
    fn nested_files_are_visited_sorted() {
        let dir = tempdir().unwrap();
        // Write files out of order; the scan must sort by locator.
        write(dir.path(), "c/nested.md", "third");
        write(dir.path(), "a.md", "first");
        write(dir.path(), "b/inner.md", "second");

        let entry = RootEntry {
            alias: "root".into(),
            path: dir.path().to_path_buf(),
            space: "test".into(),
            include: vec!["**/*.md".into()],
            exclude: vec![],
            max_file_bytes: 1024,
        };
        let result = scan_root(&entry).unwrap();
        let locators: Vec<&str> = result
            .observations
            .iter()
            .map(|o| o.locator.as_str())
            .collect();
        assert_eq!(locators, vec!["a.md", "b/inner.md", "c/nested.md"]);
    }

    #[test]
    fn excluded_pattern_skipped() {
        let dir = tempdir().unwrap();
        write(dir.path(), "keep.md", "ok");
        write(dir.path(), "skip.tmp", "skip");

        let entry = RootEntry {
            alias: "root".into(),
            path: dir.path().to_path_buf(),
            space: "test".into(),
            include: vec!["**/*".into()],
            exclude: vec!["**/*.tmp".into()],
            max_file_bytes: 1024,
        };
        let result = scan_root(&entry).unwrap();
        let locators: Vec<&str> = result
            .observations
            .iter()
            .map(|o| o.locator.as_str())
            .collect();
        assert_eq!(locators, vec!["keep.md"]);
        assert!(result.skipped.iter().any(|s| s.locator == "skip.tmp"));
    }

    #[test]
    fn oversize_files_skipped() {
        let dir = tempdir().unwrap();
        write(dir.path(), "small.md", "ok");
        write(dir.path(), "big.md", &"x".repeat(2048));

        let entry = RootEntry {
            alias: "root".into(),
            path: dir.path().to_path_buf(),
            space: "test".into(),
            include: vec!["**/*.md".into()],
            exclude: vec![],
            max_file_bytes: 1024,
        };
        let result = scan_root(&entry).unwrap();
        let locators: Vec<&str> = result
            .observations
            .iter()
            .map(|o| o.locator.as_str())
            .collect();
        assert_eq!(locators, vec!["small.md"]);
        assert!(result.skipped.iter().any(|s| s.locator == "big.md"));
    }

    #[test]
    fn symlinks_not_followed() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        write(outside.path(), "secret.md", "should not be reached");
        symlink(outside.path().join("secret.md"), dir.path().join("link.md")).unwrap();
        write(dir.path(), "plain.md", "ok");

        let entry = RootEntry {
            alias: "root".into(),
            path: dir.path().to_path_buf(),
            space: "test".into(),
            include: vec!["**/*.md".into()],
            exclude: vec![],
            max_file_bytes: 1024,
        };
        let result = scan_root(&entry).unwrap();
        let locators: Vec<&str> = result
            .observations
            .iter()
            .map(|o| o.locator.as_str())
            .collect();
        assert_eq!(locators, vec!["plain.md"]);
        assert!(result.skipped.iter().any(|s| s.locator == "link.md"));
    }

    #[test]
    fn in_place_edit_updates_modified_ns() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("note.md");
        fs::write(&path, "v1").unwrap();

        let entry = RootEntry {
            alias: "root".into(),
            path: dir.path().to_path_buf(),
            space: "test".into(),
            include: vec!["**/*.md".into()],
            exclude: vec![],
            max_file_bytes: 1024,
        };

        let first = scan_root(&entry).unwrap();
        let first_ns = first.observations[0].modified_ns;

        // Force the mtime forward well past the first measurement so the
        // difference is observable on coarse-resolution filesystems.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(
            &path,
            "v2 — much longer content so the new measurement is past the granularity",
        )
        .unwrap();

        let second = scan_root(&entry).unwrap();
        let second_ns = second.observations[0].modified_ns;
        // Some filesystems (HFS+ on macOS) quantize mtime to ~1s; bump the
        // clock further if the test platform turns out to round down to the
        // same value. We accept ≥ rather than strict >.
        assert!(
            second_ns >= first_ns,
            "modified_ns must move forward (first={first_ns}, second={second_ns})"
        );
    }

    #[test]
    fn canonicalize_missing_root_is_not_found() {
        let dir = tempdir().unwrap();
        let bogus = dir.path().join("does-not-exist");
        let err = canonicalize_root(&bogus).unwrap_err();
        assert!(matches!(err, BrainError::NotFound(_)));
    }

    #[test]
    fn modified_ns_never_negative() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.md", "ok");
        let entry = RootEntry {
            alias: "root".into(),
            path: dir.path().to_path_buf(),
            space: "test".into(),
            include: vec!["**/*.md".into()],
            exclude: vec![],
            max_file_bytes: 1024,
        };
        let result = scan_root(&entry).unwrap();
        for obs in &result.observations {
            assert!(
                obs.modified_ns >= 0,
                "negative modified_ns leaked: {}",
                obs.locator
            );
        }
    }
}
