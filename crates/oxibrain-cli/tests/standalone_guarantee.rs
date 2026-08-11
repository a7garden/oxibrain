//! Asserts no oxi-ecosystem crate leaks into the default build (AGENTS.md standalone rule).
//! Complements the CI `cargo tree | grep` check; runs as a normal test too.
use std::process::Command;

#[test]
fn no_oxi_ecosystem_deps() {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "oxibrain",
            "--no-default-features",
            "--features",
            "http-llm",
        ])
        .output()
        .expect("cargo tree");
    assert!(out.status.success(), "cargo tree failed");
    let tree = String::from_utf8(out.stdout).expect("utf8");
    for line in tree.lines() {
        let lower = line.to_ascii_lowercase();
        assert!(!lower.contains("oxios-"), "oxios dep leaked: {line}");
        assert!(!lower.contains("oxicode-"), "oxicode dep leaked: {line}");
    }
}
