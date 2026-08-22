use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tracing::warn;
use walkdir::WalkDir;

use crate::html::html_note_to_text;

/// A single note file discovered during a directory scan.
///
/// Covers both `.md` and `.html` notes — the latter have their frontmatter
/// comment stripped and the body run through HTML→text extraction before
/// `content` is populated, so callers can treat the field uniformly as the
/// indexable body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownFile {
    /// Path relative to the scan root, using forward-slash separators.
    pub path: PathBuf,
    /// File contents decoded as UTF-8 (for `.html`, the indexable text body).
    pub content: String,
    /// Last modification time of the file on disk.
    pub modified: SystemTime,
}

/// Recursively scan `dir` for `.md` and `.html` note files and return their
/// contents.
///
/// Files whose extension is neither `.md` nor `.html` (case-insensitive) are
/// ignored, as are per-folder templates (`TEMPLATE.md`, `TEMPLATE.html`),
/// root-reserved oxios inbox files — excluded from ingestion only, and vault
/// config files (`oximemo.toml`, legacy `config.toml`). The `_assets/` directory
/// and any directory whose name starts with a dot (e.g. `.trash/`) are also
/// ignored. Unreadable files are skipped and a warning is logged but do not abort
/// the scan. Results are sorted by path for deterministic downstream consumption.
///
/// For `.html` notes, the frontmatter comment (oximemo spec §3.1) is stripped
/// and the body is converted to plain text via [`crate::html::html_to_text`]
/// before being stored in `content`. `.md` files are stored verbatim.
pub fn scan_directory(dir: &Path) -> Vec<MarkdownFile> {
    let mut out = Vec::new();

    let walker = WalkDir::new(dir)
        .follow_links(false)
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
            continue;
        }

        let path = entry.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if path
            .strip_prefix(dir)
            .map(|relative| {
                relative
                    .parent()
                    .is_none_or(|parent| parent.as_os_str().is_empty())
            })
            .unwrap_or(false)
            && matches!(file_name, "Chat.md" | "Later.md")
        {
            continue;
        }

        // Per-folder templates and vault config are not notes. The legacy
        // config filename matches `paths::LEGACY_CONFIG_NAME` in oximemo.
        if matches!(
            file_name,
            "TEMPLATE.md" | "TEMPLATE.html" | "oximemo.toml" | "config.toml"
        ) {
            continue;
        }

        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };

        let is_md = ext.eq_ignore_ascii_case("md");
        let is_html = ext.eq_ignore_ascii_case("html");
        if !is_md && !is_html {
            continue;
        }

        let raw = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, path = %path.display(), "skipping unreadable note file");
                continue;
            }
        };
        let content = if is_html {
            html_note_to_text(&raw)
        } else {
            raw
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

/// Reject `walkdir` entries that are directories we never want to descend
/// into: hidden directories (`.*`) and oximemo's `_assets/` asset folder.
/// Files inside those dirs are never visited because the parent is pruned.
///
/// The walk root is always allowed through — callers pass arbitrary roots
/// (tempdirs on macOS produce `.tmpXXXX` paths) and the predicate must not
/// prune the entry point.
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
    fn scan_ignores_unsupported_extensions() {
        let dir = tempdir().unwrap();
        write(dir.path(), "note.md", "# md\n");
        write(dir.path(), "note.txt", "plain text\n");
        write(dir.path(), "README", "no extension\n");
        write(dir.path(), "guide.markdown", "wrong extension\n");
        write(dir.path(), "legacy.htm", "wrong extension\n");
        write(dir.path(), "nested/skip.rst", "rst content\n");

        let files = scan_directory(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("note.md"));
        assert_eq!(files[0].content, "# md\n");
    }

    #[test]
    fn scan_collects_html_files_with_text_extracted() {
        let dir = tempdir().unwrap();
        let html_body = "<!--\n+++\nid = \"abc\"\n+++\n-->\n<h1>Rust 소유권</h1>\n<p>소유권은 <em>정적</em> 디스패치다.</p>";
        write(dir.path(), "note.html", html_body);

        let files = scan_directory(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("note.html"));
        // Frontmatter gone, tags stripped, entities decoded.
        assert!(files[0].content.contains("Rust 소유권"));
        assert!(files[0].content.contains("소유권은 정적 디스패치다"));
        assert!(!files[0].content.contains("id ="));
        assert!(!files[0].content.contains("<h1>"));
    }

    #[test]
    fn scan_html_without_frontmatter_treats_whole_file_as_body() {
        let dir = tempdir().unwrap();
        write(dir.path(), "page.html", "<p>plain html body</p>");

        let files = scan_directory(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, "plain html body");
    }

    #[test]
    fn scan_accepts_uppercase_html_extension() {
        let dir = tempdir().unwrap();
        write(dir.path(), "Note.HTML", "<p>uppercase</p>");

        let files = scan_directory(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, "uppercase");
    }

    #[test]
    fn scan_skips_template_and_config_files() {
        let dir = tempdir().unwrap();
        write(dir.path(), "real.html", "<p>note</p>");
        write(dir.path(), "TEMPLATE.md", "md template\n");
        write(dir.path(), "TEMPLATE.html", "<p>html template</p>");
        write(dir.path(), "oximemo.toml", "[vault]\n");
        write(dir.path(), "config.toml", "legacy config\n");

        let files = scan_directory(dir.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("real.html"));
        assert_eq!(files[0].content, "note");
    }
    #[test]
    fn scan_skips_root_chat_and_later_but_not_folder_chat() {
        let dir = tempdir().unwrap();
        write(dir.path(), "Chat.md", "inbox");
        write(dir.path(), "Later.md", "later");
        write(
            dir.path(),
            "notes/Chat.md",
            "a user memo that must be ingested",
        );

        let names: Vec<String> = scan_directory(dir.path())
            .into_iter()
            .map(|f| f.path.display().to_string())
            .collect();
        assert!(!names.iter().any(|p| p == "Chat.md"));
        assert!(!names.iter().any(|p| p == "Later.md"));
        assert!(names.iter().any(|p| p == "notes/Chat.md"));
    }

    #[test]
    fn scan_skips_hidden_dirs_and_assets() {
        let dir = tempdir().unwrap();
        write(dir.path(), "keep.html", "<p>kept</p>");
        write(dir.path(), ".trash/deleted.html", "<p>trashed</p>");
        write(dir.path(), ".cache/c.html", "<p>cache</p>");
        write(dir.path(), "_assets/asset.html", "<p>asset</p>");
        write(dir.path(), "visible/inner.html", "<p>visible inner</p>");

        let files = scan_directory(dir.path());
        let paths: Vec<&Path> = files.iter().map(|f| f.path.as_path()).collect();
        assert_eq!(
            paths,
            vec![Path::new("keep.html"), Path::new("visible/inner.html"),]
        );
        assert_eq!(files[0].content, "kept");
        assert_eq!(files[1].content, "visible inner");
    }

    #[test]
    fn scan_mixes_md_and_html_sorted() {
        let dir = tempdir().unwrap();
        write(dir.path(), "c.md", "third\n");
        write(dir.path(), "b.html", "<p>second</p>");
        write(dir.path(), "a.md", "first\n");

        let files = scan_directory(dir.path());
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, PathBuf::from("a.md"));
        assert_eq!(files[0].content, "first\n");
        assert_eq!(files[1].path, PathBuf::from("b.html"));
        assert_eq!(files[1].content, "second");
        assert_eq!(files[2].path, PathBuf::from("c.md"));
        assert_eq!(files[2].content, "third\n");
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
