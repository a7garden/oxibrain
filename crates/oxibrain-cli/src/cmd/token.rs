use oxibrain::{Brain, BrainConfig, Capability, Scope};
use std::path::Path;

pub async fn run_issue(
    dir: &Path,
    space: &str,
    caps: &str,
    label: Option<&str>,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let caps_set = Capability::parse_set(caps);
    let scope = Scope {
        spaces: vec![space_id],
        caps: caps_set,
        predicate_filter: None,
        entity_type_filter: None,
        expires_at: None,
        label: String::new(),
    };
    let (info, secret) = brain.issue_token(&scope, "cli", label).await?;
    println!("issued token id={}", info.id);
    println!("label: {:?}", info.label);
    println!("issued_by: {}", info.issued_by);
    println!("issued_at: {}", info.issued_at.millis());
    println!("revoked_at: {:?}", info.revoked_at);
    println!("secret (shown once): {secret}");
    Ok(())
}

pub async fn run_list(dir: &Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let tokens = brain.list_tokens().await?;
    println!("tokens: {}", tokens.len());
    for t in &tokens {
        let status = if t.revoked_at.is_some() {
            "revoked"
        } else {
            "active"
        };
        let caps: Vec<&str> = t.scope.caps.iter().map(|c| c.as_str()).collect();
        println!(
            "  [{}] id={} label={:?} issued_by={} spaces={:?} caps={:?}",
            status, t.id, t.label, t.issued_by, t.scope.spaces, caps,
        );
    }
    Ok(())
}

pub async fn run_revoke(dir: &Path, id: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    brain.revoke_token(id).await?;
    println!("revoked token {id}");
    Ok(())
}
