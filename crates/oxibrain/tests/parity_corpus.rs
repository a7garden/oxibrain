//! Parity corpus validation (§7.8, 7.13).
//!
//! Loads every episode under `eval/parity/` and asserts:
//! - Entity surfaces appear VERBATIM at their byte spans (the fabricated-entity gate)
//! - Expected statement predicates exist in the core/v1 registry
//! - The manifest lists the episodes that exist on disk

use oxibrain_core::registry::core_v1;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct EpisodeFile {
    #[allow(dead_code)]
    lang: String,
    #[allow(dead_code)]
    properties: Vec<String>,
    episode: String,
    #[serde(default)]
    entities: Vec<EntityAnno>,
    #[serde(default)]
    expected_statements: Vec<ExpectedStatement>,
    #[serde(default)]
    #[allow(dead_code)]
    questions: Vec<Question>,
}

#[derive(Deserialize)]
struct EntityAnno {
    surface: String,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    ty: String,
    span: (u32, u32),
}

#[derive(Deserialize)]
struct ExpectedStatement {
    predicate: String,
    subject_surface: String,
    object_surface: String,
}

#[derive(Deserialize)]
struct Question {
    #[allow(dead_code)]
    question: String,
    #[allow(dead_code)]
    answer: String,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    languages: Vec<LanguageEntry>,
}

#[derive(Deserialize)]
struct LanguageEntry {
    lang: String,
    #[allow(dead_code)]
    properties: Vec<String>,
    episodes: Vec<String>,
}

fn parity_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval/parity")
}

fn read_episodes() -> Vec<(PathBuf, EpisodeFile)> {
    let dir = parity_dir();
    let mut out = Vec::new();
    for lang_dir in std::fs::read_dir(&dir).expect("parity dir") {
        let lang_dir = lang_dir.expect("entry").path();
        if !lang_dir.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&lang_dir).expect("lang dir") {
            let file = file.expect("file").path();
            if file.extension().is_some_and(|e| e == "toml") {
                let content = std::fs::read_to_string(&file).expect("read episode");
                let ep: EpisodeFile = toml::from_str(&content)
                    .unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));
                out.push((file, ep));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn corpus_exists_with_all_seven_languages() {
    for lang in ["en", "es", "ko", "ja", "zh", "ar", "th"] {
        let dir = parity_dir().join(lang);
        assert!(dir.is_dir(), "missing parity language dir: {lang}");
        let count = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter(|e| {
                e.as_ref()
                    .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "toml"))
            })
            .count();
        assert!(
            count >= 2,
            "language {lang} needs >= 2 episodes, has {count}"
        );
    }
}

#[test]
fn entity_surfaces_are_verbatim_at_byte_spans() {
    let episodes = read_episodes();
    assert!(
        !episodes.is_empty(),
        "no episodes found in {}",
        parity_dir().display()
    );
    for (path, ep) in &episodes {
        let text_bytes = ep.episode.as_bytes();
        for ent in &ep.entities {
            let (start, end) = ent.span;
            assert!(
                end as usize <= text_bytes.len(),
                "{}: entity {} span [{start},{end}) out of bounds (len {})",
                path.display(),
                ent.surface,
                text_bytes.len()
            );
            let got =
                std::str::from_utf8(&text_bytes[start as usize..end as usize]).expect("utf8 span");
            assert_eq!(
                got,
                ent.surface,
                "{}: surface mismatch at span [{start},{end}): got '{got}' expected '{}'",
                path.display(),
                ent.surface
            );
        }
    }
}

#[test]
fn expected_statements_use_registry_predicates() {
    let episodes = read_episodes();
    let preds: std::collections::HashSet<&str> =
        core_v1().iter().map(|p| p.name.as_str()).collect();

    for (path, ep) in &episodes {
        for stmt in &ep.expected_statements {
            assert!(
                preds.contains(stmt.predicate.as_str()),
                "{}: predicate '{}' not in core/v1 registry",
                path.display(),
                stmt.predicate
            );
            // Surfaces must appear in the episode text (verbatim).
            assert!(
                ep.episode.contains(&stmt.subject_surface),
                "{}: subject surface '{}' not in episode",
                path.display(),
                stmt.subject_surface
            );
            assert!(
                ep.episode.contains(&stmt.object_surface),
                "{}: object surface '{}' not in episode",
                path.display(),
                stmt.object_surface
            );
        }
    }
}

#[test]
fn manifest_matches_disk() {
    let manifest: Manifest = toml::from_str(
        &std::fs::read_to_string(parity_dir().join("manifest.toml")).expect("manifest"),
    )
    .expect("parse manifest");
    let mut on_disk: std::collections::HashSet<String> = std::collections::HashSet::new();
    for lang_dir in std::fs::read_dir(parity_dir()).expect("parity dir") {
        let lang_dir = lang_dir.expect("entry").path();
        if lang_dir.is_dir() {
            for file in std::fs::read_dir(&lang_dir).expect("lang dir") {
                let file = file.expect("file").path();
                if file.extension().is_some_and(|e| e == "toml") {
                    on_disk.insert(format!(
                        "{}/{}",
                        lang_dir.file_name().unwrap().to_string_lossy(),
                        file.file_name().unwrap().to_string_lossy()
                    ));
                }
            }
        }
    }
    let mut listed = std::collections::HashSet::new();
    for lang in &manifest.languages {
        for ep in &lang.episodes {
            listed.insert(format!("{}/{}", lang.lang, ep));
        }
    }
    assert_eq!(
        listed,
        on_disk,
        "manifest must list exactly the episodes on disk (missing: {:?}, extra: {:?})",
        listed.difference(&on_disk).collect::<Vec<_>>(),
        on_disk.difference(&listed).collect::<Vec<_>>()
    );
}
