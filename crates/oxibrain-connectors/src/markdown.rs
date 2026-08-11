use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tracing::warn;
use walkdir::WalkDir;

/// A single markdown file discovered during a directory scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownFile {
    /// Path relative to the scan root, using forward-slash separators.
    pub path: PathBuf,
    /// File contents decoded as UTF-8.
    pub content: String,
    /// Last modification time of the file on disk.
    pub modified: SystemTime,
}

/// Recursively scan `dir` for `.md` files and return their contents.
///
/// Files whose extension is not `.md` are ignored. Unreadable files are skipped
/// and a warning is logged but do not abort the scan. Results are sorted by path
/// for deterministic downstream consumption.
pub fn scan_directory(dir: &Path) -> Vec<MarkdownFile> {
    let mut out = Vec::new();

    for entry in WalkDir::new(dir).follow_links(false) {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "skipping unreadable directory entry");
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("md"))
        {
            continue;
        }

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, path = %path.display(), "skipping unreadable markdown file");
                continue;
            }
        };

        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let relative = path
            .strip_prefix(dir)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf());

        out.push(MarkdownFile {
            path: relative,
            content,
            modified,
        });
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::time::Duration;

    use tempfile::tempdir;

    fn write(dir: &Path, relative: &str, body: &str) {
        let full = dir.join(relative);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, body).unwrap();
    }

    #[test]
    fn scan_collects_md_files_sorted() {
        let dir = tempdir().unwrap();
        // Write out of order on purpose; scan_directory must sort.
        write(dir.path(), "c.md", "third\n");
        write(dir.path(), "a.md", "first\n");
        write(dir.path(), "b.md", "second\n");

        let files = scan_directory(dir.path());
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, PathBuf::from("a.md"));
        assert_eq!(files[1].path, PathBuf::from("b.md"));
        assert_eq!(files[2].path, PathBuf::from("c.md"));
        assert_eq!(files[0].content, "first\n");
        assert_eq!(files[2].content, "third\n");
    }

    #[test]
    fn scan_ignores_non_md_files() {
        let dir = tempdir().unwrap();
        write(dir.path(), "note.md", "# md\n");
        write(dir.path(), "note.txt", "plain text\n");
        write(dir.path(), "README", "no extension\n");
        write(dir.path(), "guide.markdown", "wrong extension\n");
        write(dir.path(), "nested/skip.rst", "rst content\n");

        let files = scan_directory(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("note.md"));
        assert_eq!(files[0].content, "# md\n");
    }

    #[test]
    fn scan_empty_dir_returns_empty_vec() {
        let dir = tempdir().unwrap();
        let files = scan_directory(dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn scan_traverses_nested_directories() {
        let dir = tempdir().unwrap();
        write(dir.path(), "top.md", "top\n");
        write(dir.path(), "sub/inner.md", "inner\n");
        write(dir.path(), "sub/deep/leaf.md", "leaf\n");
        write(dir.path(), "unrelated.txt", "ignored\n");

        let files = scan_directory(dir.path());

        let paths: Vec<&Path> = files.iter().map(|f| f.path.as_path()).collect();
        assert_eq!(
            paths,
            vec![
                Path::new("sub/deep/leaf.md"),
                Path::new("sub/inner.md"),
                Path::new("top.md"),
            ]
        );
        assert_eq!(files[0].content, "leaf\n");
        assert_eq!(files[2].content, "top\n");
    }

    #[test]
    fn scan_records_modified_time() {
        let dir = tempdir().unwrap();
        write(dir.path(), "note.md", "x\n");

        let files = scan_directory(dir.path());
        assert_eq!(files.len(), 1);

        let now = SystemTime::now();
        let mtime = files[0].modified;
        let drift = now.duration_since(mtime).unwrap_or(Duration::ZERO);
        assert!(
            drift < Duration::from_secs(60),
            "modified time should be within the last minute, got drift {drift:?}"
        );
    }
}
