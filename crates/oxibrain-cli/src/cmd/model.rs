//! `oxibrain model {list,pull,verify,use}` — model artifact management (§8.4).

use anyhow::Context as _;
use oxibrain::models::{
    cli_progress, default_manifest, digest_file, load_manifest, model_dir, pull_entry,
    save_manifest, verify_entry,
};

pub async fn run(args: &crate::cli::ModelCmd) -> anyhow::Result<()> {
    let dir = model_dir();
    match args {
        crate::cli::ModelCmd::List => list(&dir).await,
        crate::cli::ModelCmd::Pull { name } => pull(&dir, name.as_deref()).await,
        crate::cli::ModelCmd::Verify { name } => verify(&dir, name.as_deref()).await,
        crate::cli::ModelCmd::Use { name } => r#use(&dir, name).await,
    }
}

async fn list(dir: &std::path::Path) -> anyhow::Result<()> {
    let manifest = load_manifest().context("load manifest")?;
    println!("models dir: {}", dir.display());
    if manifest.is_empty() {
        println!("(no models installed — run `oxibrain model pull`)");
        return Ok(());
    }
    println!(
        "{:<16} {:<24} {:>8} {:>6}  file",
        "role", "name", "size", "status"
    );
    println!("{}", "-".repeat(70));
    for entry in &manifest {
        let path = dir.join(&entry.file);
        let status = if !path.exists() {
            "missing"
        } else if verify_entry(entry, dir).is_ok() {
            "ok"
        } else {
            "corrupt"
        };
        println!(
            "{:<16} {:<24} {:>6} MiB {:>6}  {}",
            entry.role.as_str(),
            entry.name,
            entry.size_mb,
            status,
            entry.file
        );
    }
    Ok(())
}

async fn pull(dir: &std::path::Path, name: Option<&str>) -> anyhow::Result<()> {
    let defaults = default_manifest();
    let mut manifest = load_manifest().context("load manifest")?;

    // Merge defaults into the manifest (add missing entries; keep existing digests).
    for d in &defaults {
        if !manifest.iter().any(|m| m.name == d.name) {
            manifest.push(d.clone());
        }
    }

    let targets: Vec<_> = match name {
        Some(n) => manifest
            .iter()
            .filter(|m| m.name == n || m.file == n)
            .cloned()
            .collect(),
        None => manifest.clone(),
    };
    if targets.is_empty() {
        anyhow::bail!("no model named `{}` in the manifest", name.unwrap_or(""));
    }

    for entry in &targets {
        let path = dir.join(&entry.file);
        if path.exists() && verify_entry(entry, dir).is_ok() {
            println!("{} already present and verified", entry.name);
            continue;
        }
        println!("pulling {} ({} MiB)...", entry.name, entry.size_mb);
        let entry_for_pull = entry.clone();
        pull_entry(&entry_for_pull, dir, cli_progress)
            .await
            .with_context(|| format!("pull {}", entry.name))?;
        println!("  {} verified", entry.name);
    }
    save_manifest(&manifest).context("save manifest")?;
    Ok(())
}

async fn verify(dir: &std::path::Path, name: Option<&str>) -> anyhow::Result<()> {
    let manifest = load_manifest().context("load manifest")?;
    let targets: Vec<_> = match name {
        Some(n) => manifest.iter().filter(|m| m.name == n).collect(),
        None => manifest.iter().collect(),
    };
    if targets.is_empty() {
        println!("(no models in manifest)");
        return Ok(());
    }
    for &entry in &targets {
        let path = dir.join(&entry.file);
        if !path.exists() {
            println!("{}: MISSING ({})", entry.name, path.display());
            continue;
        }
        let actual = digest_file(&path)?;
        let ok = actual == entry.digest;
        println!(
            "{}: {} (expected {}..., got {}...)",
            entry.name,
            if ok { "ok" } else { "CORRUPT" },
            &entry.digest[..entry.digest.len().min(12)],
            &actual[..actual.len().min(12)],
        );
    }
    Ok(())
}

async fn r#use(dir: &std::path::Path, name: &str) -> anyhow::Result<()> {
    let manifest = load_manifest().context("load manifest")?;
    let entry = manifest
        .iter()
        .find(|m| m.name == name)
        .with_context(|| format!("model `{name}` not in manifest"))?;
    let path = dir.join(&entry.file);
    if !path.exists() {
        anyhow::bail!("model file not downloaded: run `oxibrain model pull {name}`");
    }
    verify_entry(entry, dir).context("verify")?;
    // The digest is what matters for ExtractorId; the resolved file path
    // is what the local adapter loads.
    println!("using {} ({})", entry.name, entry.file);
    println!("model path: {}", path.display());
    println!("model digest: {}", entry.digest);
    Ok(())
}
