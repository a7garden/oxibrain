//! Plan what (if anything) to pull so the local extract model is ready.
//! Pure — caller executes the plan. P9 (store fetches, core decides).

use std::path::Path;

use crate::models::{ModelEntry, ModelRole};

/// Whether the local extract model is ready to use, or what to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractPullPlan {
    /// The extract entry is present and verified — nothing to do.
    NoOp,
    /// The extract entry exists in the manifest but the file is missing or
    /// failed digest verification — fetch it.
    NeedsPullFromManifest(ModelEntry),
    /// The manifest has no extract entry — fetch the default extract model
    /// (one-time bootstrap).
    NeedsBootstrap(ModelEntry),
}

/// Decide what to pull to make the local extract model ready. Pure: no
/// network, no fs writes. Inspects the manifest snapshot and the directory
/// layout, then classifies the situation.
///
/// `defaults` is the shipped default manifest (passed in for testability).
pub fn plan_extract_pull(
    manifest: &[ModelEntry],
    dir: &Path,
    defaults: &[ModelEntry],
) -> ExtractPullPlan {
    if let Some(entry) = manifest.iter().find(|e| e.role == ModelRole::Extract) {
        let path = dir.join(&entry.file);
        if !path.exists() || crate::models::verify_entry(entry, dir).is_err() {
            return ExtractPullPlan::NeedsPullFromManifest(entry.clone());
        }
        return ExtractPullPlan::NoOp;
    }
    let extract = defaults
        .iter()
        .find(|e| e.role == ModelRole::Extract)
        .expect("default manifest must contain an extract entry")
        .clone();
    ExtractPullPlan::NeedsBootstrap(extract)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ModelRole;

    fn entry(role: ModelRole, name: &str, file: &str) -> ModelEntry {
        ModelEntry {
            role,
            name: name.into(),
            file: file.into(),
            url: String::new(),
            digest: String::new(),
            size_mb: 1,
            license: String::new(),
        }
    }

    #[test]
    fn empty_manifest_returns_bootstrap_from_defaults() {
        let defaults = vec![entry(ModelRole::Extract, "qwen", "qwen.gguf")];
        let plan = plan_extract_pull(&[], Path::new("/nonexistent"), &defaults);
        assert_eq!(plan, ExtractPullPlan::NeedsBootstrap(defaults[0].clone()));
    }

    #[test]
    fn manifest_with_extract_but_missing_file_returns_pull() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = vec![entry(ModelRole::Extract, "qwen", "qwen.gguf")];
        let plan = plan_extract_pull(&manifest, temp.path(), &[]);
        assert!(matches!(plan, ExtractPullPlan::NeedsPullFromManifest(_)));
    }

    #[test]
    fn manifest_with_extract_and_present_file_returns_noop() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("qwen.gguf");
        // Empty file means digest verification will fail, so we need a
        // matching digest. Build a real digest of an empty file: blake3 of
        // empty = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae317f2a6..."
        // … which is inconvenient. Instead, place a non-empty file and use
        // the actual digest.
        let content = b"placeholder weights";
        std::fs::write(&file, content).unwrap();
        let actual = crate::models::digest_file(&file).unwrap();
        let manifest = vec![ModelEntry {
            role: ModelRole::Extract,
            name: "qwen".into(),
            file: "qwen.gguf".into(),
            url: String::new(),
            digest: actual,
            size_mb: 1,
            license: String::new(),
        }];
        let plan = plan_extract_pull(&manifest, temp.path(), &[]);
        assert_eq!(plan, ExtractPullPlan::NoOp);
    }

    #[test]
    fn manifest_with_extract_but_corrupt_file_returns_pull() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("qwen.gguf");
        std::fs::write(&file, b"placeholder").unwrap();
        let manifest = vec![ModelEntry {
            role: ModelRole::Extract,
            name: "qwen".into(),
            file: "qwen.gguf".into(),
            url: String::new(),
            // Digest of a different file → verify_entry fails.
            digest: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            size_mb: 1,
            license: String::new(),
        }];
        let plan = plan_extract_pull(&manifest, temp.path(), &[]);
        assert!(matches!(plan, ExtractPullPlan::NeedsPullFromManifest(_)));
    }
}
