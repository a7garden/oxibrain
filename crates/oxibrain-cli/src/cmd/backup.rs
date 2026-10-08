//! Detached, verifiable brain snapshots. Restore always targets a new directory.

use anyhow::{Context, bail, ensure};
use oxibrain_store::{Store, backup};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const FORMAT: &str = "oxibrain-backup/1";

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    format: String,
    generation: String,
    created_at_utc_ms: i64,
    ledger_schema_version: i64,
    projection_version: i64,
    include_projection: bool,
    include_cache: bool,
    files: Vec<FileRecord>,
    exclusions: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FileRecord {
    path: String,
    bytes: u64,
    blake3: String,
}

pub async fn run_backup(
    dir: &Path,
    no_projection: bool,
    no_cache: bool,
    out: Option<PathBuf>,
) -> anyhow::Result<()> {
    let created_at_utc_ms =
        i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    let generation = format!("{created_at_utc_ms}-{}", std::process::id());
    let output = out.unwrap_or_else(|| {
        let parent = dir.parent().unwrap_or_else(|| Path::new("."));
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("brain");
        parent.join(format!("{name}-backup-{generation}"))
    });
    ensure_absent(&output)?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create backup parent {}", parent.display()))?;
    let stage = tempfile::Builder::new()
        .prefix(".oxibrain-backup-")
        .tempdir_in(parent)?;
    private_dir(stage.path())?;

    // A read-only connection coexists with the operation-scoped writer.
    // Store::open would take the writer lock and might create a missing DB.
    let source = Store::open_read_only(dir).context("open source brain read-only")?;
    let db = stage.path().join("brain.db");
    backup::online_backup(&source, &db).context("online SQLite backup")?;
    drop(source);
    backup::inspect_snapshot(&db).context("verify source snapshot before pruning")?;
    backup::prune_snapshot(&db, no_projection, no_cache)
        .context("omit selected rebuildable snapshot rows")?;
    let (ledger_schema_version, projection_version) =
        backup::inspect_snapshot(&db).context("verify detached snapshot")?;
    private_file(&db)?;

    let mut files = vec![file_record(stage.path(), Path::new("brain.db"))?];
    let config = dir.join("documents.toml");
    let mut exclusions = vec![
        "documents.db (rebuildable index)".to_string(),
        "external document roots and shared vaults (owned by source apps)".to_string(),
        "model weights outside the brain directory (including OXIBRAIN_MODELS_DIR)".to_string(),
    ];
    if config.exists() {
        files.push(copy_record(
            &config,
            stage.path(),
            Path::new("documents.toml"),
        )?);
    } else {
        exclusions.push("documents.toml (not present)".to_string());
    }
    if no_cache {
        exclusions.push("extraction cache and model weights (--no-cache)".to_string());
    } else {
        let models = dir.join("models");
        if models.exists() {
            copy_tree(&models, stage.path(), Path::new("models"), &mut files)?;
        } else {
            exclusions.push("models/ (not present)".to_string());
        }
    }
    if no_projection || no_cache {
        exclusions
            .push("rebuildable memory projection (--no-projection or --no-cache)".to_string());
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = Manifest {
        format: FORMAT.to_string(),
        generation,
        created_at_utc_ms,
        ledger_schema_version,
        projection_version,
        include_projection: !no_projection && !no_cache,
        include_cache: !no_cache,
        files,
        exclusions,
    };
    let manifest_path = stage.path().join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    private_file(&manifest_path)?;
    verify_artifact(stage.path(), &manifest)?;
    ensure_absent(&output)?;
    let stage_path = stage.keep();
    if let Err(error) = fs::rename(&stage_path, &output) {
        let _ = fs::remove_dir_all(&stage_path);
        return Err(error).with_context(|| format!("publish backup {}", output.display()));
    }
    println!(
        "backup generation {} at {}",
        manifest.generation,
        output.display()
    );
    Ok(())
}

pub async fn run_restore(dir: &Path, artifact: PathBuf) -> anyhow::Result<()> {
    // Refuse an existing path, including a symlink. Cutover is an explicit
    // operator step after clients have stopped.
    ensure_absent(dir)?;
    let manifest_bytes = fs::read(artifact.join("manifest.json"))
        .with_context(|| format!("read backup manifest at {}", artifact.display()))?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse backup manifest")?;
    verify_artifact(&artifact, &manifest)?;
    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".oxibrain-restore-")
        .tempdir_in(parent)?;
    private_dir(stage.path())?;
    for file in &manifest.files {
        let relative = Path::new(&file.path);
        let copied = copy_record(&artifact.join(relative), stage.path(), relative)?;
        ensure!(
            &copied == file,
            "backup changed during restore: {}",
            file.path
        );
    }
    let db = stage.path().join("brain.db");
    let (schema, projection) =
        backup::inspect_snapshot(&db).context("verify restored SQLite database")?;
    ensure!(
        schema == manifest.ledger_schema_version && projection == manifest.projection_version,
        "restored schema does not match backup manifest"
    );
    if !manifest.include_projection {
        // Rebuild only inside this isolated directory. No model or document
        // root is read and no daemon is started.
        let store = Store::open(stage.path()).context("open isolated restore")?;
        let _report =
            oxibrain_store::reproject(store.connection()).context("reproject isolated restore")?;
        drop(store);
        backup::inspect_snapshot(&db).context("verify rebuilt restore")?;
    }
    ensure_absent(dir)?;
    let stage_path = stage.keep();
    if let Err(error) = fs::rename(&stage_path, dir) {
        let _ = fs::remove_dir_all(&stage_path);
        return Err(error).with_context(|| format!("publish restore {}", dir.display()));
    }
    println!(
        "restored generation {} into {}",
        manifest.generation,
        dir.display()
    );
    if !manifest.include_cache {
        eprintln!(
            "extraction cache omitted; run oxibrain admin extract --pending after configuring a model"
        );
    }
    Ok(())
}

fn ensure_absent(path: &Path) -> anyhow::Result<()> {
    ensure!(
        fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "destination already exists or cannot be checked: {}",
        path.display()
    );
    Ok(())
}

fn verify_artifact(root: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    ensure!(manifest.format == FORMAT, "unsupported backup format");
    ensure!(
        !manifest.generation.is_empty(),
        "backup generation is empty"
    );
    let root = fs::canonicalize(root).context("resolve backup directory")?;
    let mut seen = HashSet::new();
    for file in &manifest.files {
        let relative = Path::new(&file.path);
        ensure!(
            relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
            "unsafe backup path: {}",
            file.path
        );
        ensure!(
            file.path == "brain.db"
                || file.path == "documents.toml"
                || relative.starts_with("models"),
            "unexpected backup path: {}",
            file.path
        );
        ensure!(
            seen.insert(&file.path),
            "duplicate backup path: {}",
            file.path
        );
        let path = root.join(relative);
        ensure!(
            fs::canonicalize(&path)?.starts_with(&root),
            "backup file escapes artifact: {}",
            file.path
        );
        ensure!(
            fs::symlink_metadata(&path)?.file_type().is_file(),
            "backup entry is not a file: {}",
            file.path
        );
        let actual = file_record(&root, relative)?;
        ensure!(
            actual.bytes == file.bytes && actual.blake3 == file.blake3,
            "backup digest mismatch: {}",
            file.path
        );
    }
    ensure!(
        seen.contains(&"brain.db".to_string()),
        "backup has no brain.db"
    );
    let (schema, projection) = backup::inspect_snapshot(&root.join("brain.db"))?;
    ensure!(
        schema == manifest.ledger_schema_version && projection == manifest.projection_version,
        "backup schema does not match manifest"
    );
    Ok(())
}

fn copy_tree(
    source: &Path,
    root: &Path,
    relative: &Path,
    files: &mut Vec<FileRecord>,
) -> anyhow::Result<()> {
    ensure!(
        fs::symlink_metadata(source)?.file_type().is_dir(),
        "model cache contains a symlink or special directory: {}",
        source.display()
    );
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        let kind = fs::symlink_metadata(entry.path())?.file_type();
        if kind.is_dir() {
            copy_tree(&entry.path(), root, &child, files)?;
        } else if kind.is_file() {
            files.push(copy_record(&entry.path(), root, &child)?);
        } else {
            bail!(
                "model cache contains a symlink or special file: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn copy_record(source: &Path, root: &Path, relative: &Path) -> anyhow::Result<FileRecord> {
    ensure!(
        fs::symlink_metadata(source)?.file_type().is_file(),
        "source is not a regular file: {}",
        source.display()
    );
    let destination = root.join(relative);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut input = fs::File::open(source)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()?;
    output.sync_all()?;
    private_file(&destination)?;
    let original = digest(source)?;
    let copied = file_record(root, relative)?;
    ensure!(
        original == copied.blake3,
        "source changed during backup copy: {}",
        source.display()
    );
    Ok(copied)
}

fn file_record(root: &Path, relative: &Path) -> anyhow::Result<FileRecord> {
    let path = root.join(relative);
    let bytes = fs::metadata(&path)?.len();
    let path = relative
        .to_str()
        .context("backup path is not UTF-8")?
        .replace('\\', "/");
    Ok(FileRecord {
        path,
        bytes,
        blake3: digest(&root.join(relative))?,
    })
}

fn digest(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(unix)]
fn private_dir(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn private_dir(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn private_file(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn private_file(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}
