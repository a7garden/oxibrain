//! Model artifact management (§8.4).
//!
//! Manifest at `~/.oxi/models/manifest.toml` declares the model set:
//! role, name, url, blake3 digest, size, license. `oxibrain model` commands
//! list / pull / verify / use it. The digest feeds `ExtractorId` (§9.5):
//! changing weights must change the extractor id, or a silent quality
//! change would poison the extraction cache.

use oxibrain_ports::BrainError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio_stream::StreamExt as _;

/// What a model is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// Text generation / extraction (LlmPort).
    Extract,
    /// Embeddings (EmbeddingPort).
    Embed,
}

impl ModelRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Extract => "extract",
            Self::Embed => "embed",
        }
    }
}

/// One model in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub role: ModelRole,
    /// Stable name, e.g. `qwen2.5-1.5b-instruct`.
    pub name: String,
    /// Local file name under the models dir.
    pub file: String,
    /// Download URL (HuggingFace resolve or any https URL).
    pub url: String,
    /// blake3 hex digest of the weights. Verified on pull and by `verify`.
    pub digest: String,
    /// File size in MiB.
    pub size_mb: u64,
    pub license: String,
}

/// The shipped default model set (§8.4): a multilingual instruct model for
/// extraction and a multilingual embedding model.
pub fn default_manifest() -> Vec<ModelEntry> {
    vec![
        ModelEntry {
            role: ModelRole::Extract,
            name: "qwen2.5-1.5b-instruct".into(),
            file: "qwen2.5-1.5b-instruct-q4_k_m.gguf".into(),
            url: "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf".into(),
            digest: "2619da49b802f7bc7e92264edd12e4bf093dd97f42584535ae77dc587ab55362".into(),
            size_mb: 1065,
            license: "apache-2.0".into(),
        },
        ModelEntry {
            role: ModelRole::Embed,
            name: "bge-m3".into(),
            file: "bge-m3-Q4_K_M.gguf".into(),
            url: "https://huggingface.co/lm-kit/bge-m3-gguf/resolve/main/bge-m3-Q4_K_M.gguf".into(),
            digest: "b7f56ba6ceb9fce993f0b1ea2810257ecceb23b5fa2e9787ed679c8174f96ec4".into(),
            size_mb: 417,
            license: "mit".into(),
        },
    ]
}

/// The models directory: `$OXIBRAIN_MODELS_DIR` if set, else `~/.oxi/models/`.
///
/// The env override is the air-gapped escape hatch (§8.4): point it at a
/// pre-pulled directory and the lazy pull becomes a verify-only no-op.
pub fn model_dir() -> PathBuf {
    model_dir_with(std::env::var_os("OXIBRAIN_MODELS_DIR"))
}

fn model_dir_with(override_dir: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(dir) = override_dir {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".oxi").join("models")
}

/// The manifest path: `~/.oxi/models/manifest.toml`.
pub fn manifest_path() -> PathBuf {
    model_dir().join("manifest.toml")
}

/// Load the manifest from a specific dir. Returns an empty list if absent.
pub fn load_manifest_at(dir: &Path) -> Result<Vec<ModelEntry>, BrainError> {
    let path = dir.join("manifest.toml");
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let manifest: Manifest = toml::from_str(&s).map_err(|e| {
                BrainError::Config(format!("manifest parse {}: {e}", path.display()))
            })?;
            Ok(manifest.models)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(BrainError::Config(format!(
            "manifest read {}: {e}",
            path.display()
        ))),
    }
}

/// Save the manifest to a specific dir, creating it if needed.
pub fn save_manifest_at(dir: &Path, entries: &[ModelEntry]) -> Result<(), BrainError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| BrainError::Config(format!("models dir create: {e}")))?;
    let manifest = Manifest {
        models: entries.to_vec(),
    };
    let s = toml::to_string(&manifest)
        .map_err(|e| BrainError::Config(format!("manifest serialize: {e}")))?;
    std::fs::write(dir.join("manifest.toml"), s)
        .map_err(|e| BrainError::Config(format!("manifest write: {e}")))
}

/// Load the manifest from the default models dir.
pub fn load_manifest() -> Result<Vec<ModelEntry>, BrainError> {
    load_manifest_at(&model_dir())
}

/// Save the manifest to the default models dir.
pub fn save_manifest(entries: &[ModelEntry]) -> Result<(), BrainError> {
    save_manifest_at(&model_dir(), entries)
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    models: Vec<ModelEntry>,
}

/// Compute the blake3 hex digest of a file. Reads in chunks (no full-file load).
pub fn digest_file(path: &Path) -> Result<String, BrainError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)
        .map_err(|e| BrainError::Model(format!("digest open {}: {e}", path.display())))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 1 << 16];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| BrainError::Model(format!("digest read: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize().as_bytes()))
}

/// Verify a downloaded model file matches its manifest digest.
pub fn verify_entry(entry: &ModelEntry, dir: &Path) -> Result<(), BrainError> {
    let path = dir.join(&entry.file);
    if !path.exists() {
        return Err(BrainError::NotFound(format!(
            "model file {} not present",
            path.display()
        )));
    }
    let actual = digest_file(&path)?;
    if actual != entry.digest {
        return Err(BrainError::Model(format!(
            "digest mismatch for {}: expected {} got {}",
            entry.name, entry.digest, actual
        )));
    }
    Ok(())
}

/// Download a model with progress reporting and resume support.
///
/// Downloads to `<file>.part`, resuming from the existing partial size via
/// HTTP Range. On completion, verifies the digest and renames to `<file>`.
pub async fn pull_entry(
    entry: &ModelEntry,
    dir: &Path,
    progress: impl Fn(u64, u64),
) -> Result<(), BrainError> {
    use std::io::Write as _;

    std::fs::create_dir_all(dir)
        .map_err(|e| BrainError::Config(format!("models dir create: {e}")))?;
    let part_path = dir.join(format!("{}.part", entry.file));
    let final_path = dir.join(&entry.file);

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| BrainError::Provider {
            retryable: true,
            message: format!("http client: {e}"),
        })?;

    let existing = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
    let mut req = client.get(&entry.url);
    if existing > 0 {
        req = req.header("Range", format!("bytes={existing}-"));
    }
    let resp = req.send().await.map_err(|e| BrainError::Provider {
        retryable: true,
        message: format!("download: {e}"),
    })?;
    let status = resp.status();
    if !status.is_success() && status.as_u16() != 206 {
        return Err(BrainError::Provider {
            retryable: status.as_u16() >= 500,
            message: format!("download {}: HTTP {status}", entry.url),
        });
    }

    let total = resp.content_length().unwrap_or(0) + existing;
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part_path)
        .map_err(|e| BrainError::Config(format!("open part file: {e}")))?;

    let mut stream = resp.bytes_stream();
    let mut downloaded = existing;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| BrainError::Provider {
            retryable: true,
            message: format!("stream: {e}"),
        })?;
        out.write_all(&chunk)
            .map_err(|e| BrainError::Config(format!("write part: {e}")))?;
        downloaded += chunk.len() as u64;
        progress(downloaded, total);
    }
    out.flush()
        .map_err(|e| BrainError::Config(format!("flush: {e}")))?;
    drop(out);

    // Verify the digest, then move into place.
    let actual = digest_file(&part_path)?;
    if actual != entry.digest {
        return Err(BrainError::Model(format!(
            "digest mismatch after download for {}: expected {} got {} — corrupt or wrong URL",
            entry.name, entry.digest, actual
        )));
    }
    std::fs::rename(&part_path, &final_path)
        .map_err(|e| BrainError::Config(format!("finalize download: {e}")))?;
    Ok(())
}

/// Progress reporter for the CLI: prints `downloaded/total MiB` every 1 MiB.
pub fn cli_progress(downloaded: u64, total: u64) {
    const CHUNK: u64 = 1 << 20; // 1 MiB
    if downloaded % CHUNK == 0 || downloaded == total {
        let dl = downloaded as f64 / (1 << 20) as f64;
        let tot = total as f64 / (1 << 20) as f64;
        eprintln!("  {dl:6.0} / {tot:6.0} MiB");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        let entries = default_manifest();
        let dir = tempfile::tempdir().expect("tempdir");
        save_manifest_at(dir.path(), &entries).expect("save");
        let loaded = load_manifest_at(dir.path()).expect("load");
        assert_eq!(loaded.len(), entries.len());
        assert_eq!(loaded[0].name, "qwen2.5-1.5b-instruct");
        assert_eq!(loaded[0].role, ModelRole::Extract);
        assert_eq!(loaded[1].role, ModelRole::Embed);
    }

    #[test]
    fn model_dir_env_override_wins() {
        assert_eq!(
            model_dir_with(Some("/opt/oxibrain-models".into())),
            PathBuf::from("/opt/oxibrain-models")
        );
    }

    #[test]
    fn model_dir_default_is_home_dot_oxi_models() {
        let dir = model_dir_with(None);
        assert!(dir.ends_with(".oxi/models"), "got {dir:?}");
    }

    #[test]
    fn digest_changes_with_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f1 = dir.path().join("a.bin");
        let f2 = dir.path().join("b.bin");
        std::fs::write(&f1, b"same length!!").expect("write");
        std::fs::write(&f2, b"same length??").expect("write");
        let d1 = digest_file(&f1).expect("digest1");
        let d2 = digest_file(&f2).expect("digest2");
        assert_ne!(d1, d2, "different content must hash differently");
        assert_eq!(d1.len(), 64, "blake3 hex is 64 chars");
    }

    #[test]
    fn verify_detects_corruption() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model.gguf");
        std::fs::write(&path, b"fake weights").expect("write");
        let digest = digest_file(&path).expect("digest");
        let mut entry = default_manifest()[0].clone();
        entry.file = "model.gguf".into();
        entry.digest = digest.clone();
        assert!(
            verify_entry(&entry, dir.path()).is_ok(),
            "matching digest verifies"
        );

        // Corrupt the file.
        std::fs::write(&path, b"fake weightz").expect("write");
        assert!(
            verify_entry(&entry, dir.path()).is_err(),
            "corruption detected"
        );
    }
}
