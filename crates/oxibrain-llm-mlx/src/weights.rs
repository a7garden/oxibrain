//! Weight loading for the MLX engine: resolve a model directory (a direct
//! path, or an HF-cache repo id like `mlx-community/Qwen3-…-4bit`), read the
//! safetensors shard map, and merge every shard into one name → `Array` map.

use mlx_rs::Array;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolve `spec` to a model directory containing `config.json` and
/// safetensors weights.
///
/// Resolution order:
///   1. `spec` itself, when it is an existing directory (absolute or
///      relative to cwd).
///   2. `OXIBRAIN_MLX_HOME/<spec>` — a curated local models directory.
///   3. The HuggingFace hub cache (`HF_HUB_HOME` or
///      `~/.cache/huggingface/hub`): `models--<spec with '/'→'--'>/snapshots/<latest>/`.
pub fn resolve_model_dir(spec: &str) -> Result<PathBuf, String> {
    let direct = Path::new(spec);
    if direct.is_dir() {
        return Ok(direct.to_path_buf());
    }
    if let Ok(home) = std::env::var("OXIBRAIN_MLX_HOME")
        && let dir = Path::new(&home).join(spec)
        && dir.is_dir()
    {
        return Ok(dir);
    }
    let hub_root = std::env::var("HF_HUB_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_hub_root());
    let repo_dir = hub_root.join(format!("models--{}", spec.replace('/', "--")));
    let snapshots = repo_dir.join("snapshots");
    let entries = std::fs::read_dir(&snapshots)
        .map_err(|e| {
            format!(
                "MLX model `{spec}` not found: not a directory, not under \
                 OXIBRAIN_MLX_HOME, and no HF cache snapshot at {} ({e})",
                snapshots.display()
            )
        })?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir());
    let mut latest: Option<PathBuf> = None;
    for snap in entries {
        if !snap.join("config.json").is_file() {
            continue;
        }
        if latest.as_ref().is_none_or(|prev| snap > *prev) {
            latest = Some(snap);
        }
    }
    latest.ok_or_else(|| format!("no complete snapshot under {}", snapshots.display()))
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn home_hub_root() -> PathBuf {
    let mut p = home_dir();
    p.push(".cache");
    p.push("huggingface");
    p.push("hub");
    p
}

/// List the safetensors shard files for a model directory, in map order.
fn shard_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let index = dir.join("model.safetensors.index.json");
    if index.is_file() {
        let text = std::fs::read_to_string(&index)
            .map_err(|e| format!("read {}: {e}", index.display()))?;
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("parse index: {e}"))?;
        let mut files: Vec<String> = parsed["weight_map"]
            .as_object()
            .ok_or("index missing weight_map")?
            .values()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        files.sort();
        files.dedup();
        return Ok(files.into_iter().map(|f| dir.join(f)).collect());
    }
    let single = dir.join("model.safetensors");
    if single.is_file() {
        return Ok(vec![single]);
    }
    Err(format!("no safetensors weights in {}", dir.display()))
}

/// Load and merge every shard of a model directory into one weight map.
pub fn load_weights(dir: &Path) -> Result<HashMap<String, Array>, String> {
    let mut merged: HashMap<String, Array> = HashMap::new();
    for shard in shard_files(dir)? {
        let weights = Array::load_safetensors(&shard)
            .map_err(|e| format!("load {}: {e}", shard.display()))?;
        for (k, v) in weights {
            merged.insert(k, v);
        }
    }
    Ok(merged)
}

/// Cheap content fingerprint for cache identity (§9.5): blake3 over
/// `config.json` bytes plus every shard's name and byte size, sorted. A
/// full 17 GB re-read per store open would cost more than the extraction
/// itself, so the fingerprint pins the artifact set (config + exact shard
/// sizes) instead of every weight byte; in-place weight edits that keep
/// identical sizes would evade it — accepted for v1, see ADR-017.
pub fn model_fingerprint(dir: &Path) -> Result<String, String> {
    let mut hasher = blake3::Hasher::new();
    let config = std::fs::read(dir.join("config.json"))
        .map_err(|e| format!("read config.json: {e}"))?;
    hasher.update(&config);
    for shard in shard_files(dir)? {
        let name = shard
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("non-utf8 shard name")?;
        let size = std::fs::metadata(&shard)
            .map_err(|e| format!("stat {}: {e}", shard.display()))?
            .len();
        hasher.update(name.as_bytes());
        hasher.update(&size.to_le_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}
