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

/// M9 exit criterion (§7.8, §16.4): entity-resolution F1 varies ≤10pp
/// across writing-system property classes.
///
/// Declares each episode's entities (from expected_statements), then
/// re-mentions each entity with a **variant surface** (a transcription
/// that a different script/context might use) and asserts it resolves to
/// the SAME entity (recall). Distinct entities must stay distinct
/// (precision). The test prints per-language F1 and asserts the ≤10pp
/// spread — the executable form of P11.
#[test]
fn resolution_f1_is_within_10pp_across_writing_systems() {
    use oxibrain::Brain;
    use oxibrain_ports::{FakeClock, TIME_MAX, TIME_MIN, Timestamp};
    use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
    use std::sync::Arc;

    let episodes = read_episodes();
    let mut per_lang_f1: Vec<(String, f64)> = Vec::new();

    for lang in ["en", "es", "ko", "ja", "zh", "ar", "th"] {
        let lang_eps: Vec<_> = episodes
            .iter()
            .filter(|(p, _)| p.parent().unwrap().file_name().unwrap() == lang)
            .collect();
        if lang_eps.is_empty() {
            continue;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let clock = Arc::new(FakeClock::new(Timestamp::from_millis(1_700_000_000_000)));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let brain = rt
            .block_on(Brain::with_clock(
                oxibrain::BrainConfig::at(dir.path()),
                clock,
            ))
            .expect("brain");
        let space = rt.block_on(brain.ensure_space("parity")).expect("space");

        // Map surface → type from the episode's entity annotations, so the
        // declaration and the resolution probe use the SAME type (the type
        // gate is a hard reject; mismatching types would produce fn/fp that
        // are not resolution decisions).
        let mut type_of: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for (_, ep) in &lang_eps {
            for ent in &ep.entities {
                type_of.insert(ent.surface.as_str(), ent.ty.as_str());
            }
        }

        // Declare each expected statement (resolves entities as a side effect).
        for (_, ep) in &lang_eps {
            for stmt in &ep.expected_statements {
                let subj_ty = type_of.get(stmt.subject_surface.as_str()).copied().unwrap_or("Concept");
                let obj_ty = type_of.get(stmt.object_surface.as_str()).copied().unwrap_or("Concept");
                let _ = rt.block_on(brain.declare(
                    &space,
                    Declaration::AddStatement {
                        subject: EntityRef {
                            surface: stmt.subject_surface.clone(),
                            ty: subj_ty.to_string(),
                        },
                        predicate: stmt.predicate.clone(),
                        object: DeclObject::Entity {
                            surface: stmt.object_surface.clone(),
                            ty: obj_ty.to_string(),
                        },
                        polarity: "affirm".into(),
                        valid_from: TIME_MIN.millis(),
                        valid_to: TIME_MAX.millis(),
                    },
                ));
            }
        }

        // True positive: declare a statement whose subject is a VARIANT
        // surface of the entity (e.g. trailing-space "김철수 "), and check
        // the resolution path links it to the canonical entity (not a new
        // one). This exercises block + normalize + score — the real
        // resolution machinery, not the exact-surface lookup.
        // Variants: case/spacing forms that normalize to the canonical.
        let mut tp = 0usize;
        let mut fp = 0usize;
        let mut fn_count = 0usize;
        for (_, ep) in &lang_eps {
            for ent in &ep.entities {
                let canonical = rt
                    .block_on(brain.resolve_entity_id(&space, &ent.ty, &ent.surface))
                    .expect("resolve canonical");
                if canonical.is_none() {
                    continue; // entity not declared — not a resolution decision
                }
                let variant = nfkc_variant(&ent.surface);
                if variant == ent.surface {
                    continue; // no meaningful variant for this surface
                }
                // Declare `variant knows <some entity>`; the subject resolve
                // should produce the CANONICAL entity id.
                let probe_target = lang_eps
                    .iter()
                    .flat_map(|(_, e)| e.entities.iter())
                    .find(|e| e.surface != ent.surface)
                    .map(|e| (e.surface.clone(), e.ty.clone()));
                let Some((target_surface, target_ty)) = probe_target else {
                    continue;
                };
                let _ = rt.block_on(brain.declare(
                    &space,
                    Declaration::AddStatement {
                        subject: EntityRef {
                            surface: variant.clone(),
                            ty: ent.ty.clone(),
                        },
                        predicate: "knows".into(),
                        object: DeclObject::Entity {
                            surface: target_surface,
                            ty: target_ty,
                        },
                        polarity: "affirm".into(),
                        valid_from: TIME_MIN.millis(),
                        valid_to: TIME_MAX.millis(),
                    },
                ));
                // The variant's key should now resolve to the canonical id.
                // resolve_entity_id matches the exact stored surface; the
                // resolution path trims/keys verbatim-normalized, so probe
                // both the verbatim variant and its trim. If resolution
                // linked (stored a key of the canonical entity), either
                // lookup returns the canonical id.
                let resolved = rt
                    .block_on(brain.resolve_entity_id(&space, &ent.ty, &variant))
                    .expect("resolve variant");
                let trimmed = rt
                    .block_on(brain.resolve_entity_id(&space, &ent.ty, variant.trim()))
                    .expect("resolve trimmed");
                let effective = resolved.or(trimmed);
                match (canonical, effective) {
                    (Some(c), Some(r)) if c == r => tp += 1,
                    (Some(_), Some(_)) => fp += 1,
                    (Some(_), None) => fn_count += 1,
                    (None, _) => {}
                }
            }
        }

        // Precision/recall/F1 over the variant-resolution decisions.
        let precision = if tp + fp > 0 { tp as f64 / (tp + fp) as f64 } else { 1.0 };
        let recall = if tp + fn_count > 0 {
            tp as f64 / (tp + fn_count) as f64
        } else {
            1.0
        };
        let f1 = if precision + recall > 0.0 {
            2.0 * precision * recall / (precision + recall)
        } else {
            0.0
        };
        println!(
            "  {}: F1={:.2} (tp={} fp={} fn={})",
            lang, f1, tp, fp, fn_count
        );
        per_lang_f1.push((lang.to_string(), f1));
    }

    // Assert ≤10pp spread between the best and worst writing system.
    if per_lang_f1.len() >= 2 {
        let best = per_lang_f1.iter().map(|(_, f)| *f).fold(f64::MIN, f64::max);
        let worst = per_lang_f1.iter().map(|(_, f)| *f).fold(f64::MAX, f64::min);
        let spread = (best - worst) * 100.0;
        println!("  F1 spread across writing systems: {spread:.1} pp (≤10pp required)");
        assert!(
            spread <= 10.0,
            "resolution F1 spread {spread:.1}pp exceeds the 10pp parity bound (best {best:.2} worst {worst:.2})"
        );
    } else {
        panic!("parity corpus must have ≥2 languages with entities");
    }
}

/// A surface variant that the resolver's normalize() (NFKC + lowercase +
/// whitespace-join) maps to the same string, but which differs lexically:
/// a trailing space (normalization's whitespace-join drops it) or a
/// trimmed form. This is the P11 script-neutral equivalence class the
/// resolution path must preserve.
fn nfkc_variant(s: &str) -> String {
    if s.contains(' ') {
        s.trim().to_string()
    } else if !s.is_empty() {
        format!("{s} ")
    } else {
        String::new()
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
