//! CI enforcement of §18 rule 6 (P11): no crate outside `oxibrain-index`
//! contains a natural-language word list, stemmer, or script check.
//!
//! This is the executable form of C3 — language independence by construction.
//! It fails when someone adds an English-shaped optimization outside the index
//! crate, and keeps passing for languages we never tested.

use std::fs;
use std::path::{Path, PathBuf};

/// Patterns that indicate language-specific code. Matched as substrings
/// against non-comment lines. Each is specific enough that false positives
/// from English prose are unlikely.
const FORBIDDEN_PATTERNS: &[&str] = &[
    "STOP_WORDS",
    "stopword",
    "stop_word",
    // Stemmers
    "porter",
    "stemmer",
    "snowball",
    // Script detectors / language identifiers
    "is_cjk",
    "is_chinese",
    "is_japanese",
    "is_korean",
    "is_latin_script",
    "detect_language",
    "detect_script",
    "script_type",
];

/// Crates allowed to contain language primitives (§18 rule 6).
const ALLOWED_CRATE: &str = "oxibrain-index";

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
}

/// Check if a line is a comment or doc-comment (heuristic: trimmed line
/// starts with `//` or `//!` or `///`). String literals are harder to
/// exclude, but the forbidden patterns are specific enough.
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with("/*")
}

#[test]
fn no_language_specific_code_outside_index() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("crates");

    let mut files = Vec::new();
    collect_rust_files(&workspace, &mut files);

    let mut violations: Vec<String> = Vec::new();

    for file in &files {
        // Skip the allowed crate.
        if file.to_string_lossy().contains(ALLOWED_CRATE) {
            continue;
        }

        let source = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for (lineno, line) in source.lines().enumerate() {
            if is_comment_line(line) {
                continue;
            }
            for pattern in FORBIDDEN_PATTERNS {
                if line.to_lowercase().contains(&pattern.to_lowercase()) {
                    let rel = file.strip_prefix(&workspace).unwrap_or(file).display();
                    violations.push(format!(
                        "{rel}:{lineno}: forbidden pattern '{pattern}' (§18 rule 6)"
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "§18 rule 6 violation — language-specific code outside {}:\n{}\n\
         Character n-grams (§7.3) are the only permitted lexical primitive.",
        ALLOWED_CRATE,
        violations.join("\n")
    );
}
