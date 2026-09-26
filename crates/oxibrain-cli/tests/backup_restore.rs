//! End-to-end backup/restore checks against disposable stores only.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

fn cli(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_oxibrain"))
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn oxibrain")
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 stdout")
}

#[test]
fn backup_restores_a_detached_store_and_preserves_source() {
    let root = tempdir().expect("tempdir");
    let source = root.path().join("source");
    let backup = root.path().join("backup");
    let restored = root.path().join("restored");
    success(cli(&source, &["admin", "init", "--space", "personal"]));
    fs::write(source.join("documents.toml"), "# registered roots\n").expect("config");
    fs::create_dir_all(source.join("models")).expect("models");
    fs::write(source.join("models").join("test.bin"), b"model bytes").expect("model");
    let source_db_before = fs::read(source.join("brain.db")).expect("source db");

    success(cli(
        &source,
        &["admin", "backup", "--out", backup.to_str().expect("path")],
    ));
    assert_eq!(
        fs::read(source.join("brain.db")).expect("source db"),
        source_db_before
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(backup.join("manifest.json")).expect("manifest"))
            .expect("manifest JSON");
    assert_eq!(manifest["format"], "oxibrain-backup/1");
    assert_eq!(manifest["include_cache"], true);
    assert_eq!(manifest["include_projection"], true);
    assert!(
        manifest["files"]
            .as_array()
            .expect("files")
            .iter()
            .any(|f| f["path"] == "brain.db")
    );
    success(cli(
        &restored,
        &["admin", "restore", backup.to_str().expect("path")],
    ));
    assert_eq!(
        fs::read(restored.join("documents.toml")).expect("restored config"),
        b"# registered roots\n"
    );
    assert_eq!(
        fs::read(restored.join("models").join("test.bin")).expect("restored model"),
        b"model bytes"
    );
    assert!(success(cli(&restored, &["admin", "spaces"])).contains("personal"));

    // Restore must never overwrite an existing live or test directory.
    let refusal = cli(
        &restored,
        &["admin", "restore", backup.to_str().expect("path")],
    );
    assert!(!refusal.status.success());
    assert!(success(cli(&restored, &["admin", "spaces"])).contains("personal"));
}

#[test]
fn omitted_cache_reprojects_in_isolation_and_damage_is_rejected() {
    let root = tempdir().expect("tempdir");
    let source = root.path().join("source");
    let backup = root.path().join("backup");
    let restored = root.path().join("restored");
    success(cli(&source, &["admin", "init", "--space", "personal"]));
    fs::create_dir_all(source.join("models")).expect("models");
    fs::write(source.join("models").join("test.bin"), b"model bytes").expect("model");
    success(cli(
        &source,
        &[
            "admin",
            "backup",
            "--no-cache",
            "--out",
            backup.to_str().expect("path"),
        ],
    ));
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(backup.join("manifest.json")).expect("manifest"))
            .expect("manifest JSON");
    assert_eq!(manifest["include_cache"], false);
    assert_eq!(manifest["include_projection"], false);
    assert!(!backup.join("models").exists());
    success(cli(
        &restored,
        &["admin", "restore", backup.to_str().expect("path")],
    ));
    assert!(success(cli(&restored, &["admin", "spaces"])).contains("personal"));

    // Even if SQLite happens to tolerate this byte change, the file digest
    // must reject it before any restore destination is published.
    let db = backup.join("brain.db");
    let mut bytes = fs::read(&db).expect("backup db");
    let last = bytes.last_mut().expect("nonempty db");
    *last ^= 0x55;
    fs::write(&db, bytes).expect("damage backup");
    let rejected = root.path().join("rejected");
    let output = cli(
        &rejected,
        &["admin", "restore", backup.to_str().expect("path")],
    );
    assert!(!output.status.success());
    assert!(!rejected.exists());
}
