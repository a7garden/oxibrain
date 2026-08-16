//! Shared LLM provider construction from environment variables.
//!
//! Used by `extract` and `reextract`. Providers:
//!   - `OXIBRAIN_LLM_PROVIDER=anthropic` (+ `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL`)
//!   - `OXIBRAIN_LLM_PROVIDER=openai`     (+ `OPENAI_API_KEY`, `OPENAI_MODEL`)
//!   - `OXIBRAIN_LLM_PROVIDER=local`      (GGUF from `oxibrain model pull`, §8.4)
//!
//! Resolution order when `OXIBRAIN_LLM_PROVIDER` is unset: Anthropic if
//! `ANTHROPIC_API_KEY` is present, otherwise **local** — the standalone
//! guarantee (C2) means extraction must work with no API key.
//!
//! `OXIBRAIN_MODEL` is a fallback for the HTTP model id. The mechanism
//! (tool-call / json-schema / GBNF grammar) follows the provider — Anthropic
//! uses forced tool calls, OpenAI native json_schema structured output, and
//! the local path grammar-constrained decoding (DESIGN §7.4, §9.4).

use anyhow::Context as _;
use oxibrain_core::extraction::ExtractMechanism;
use oxibrain_ports::{LlmPort, TokenizerPort};
use std::sync::Arc;

/// Which provider `from_env` resolved. Testable without touching the network
/// or loading model weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Local,
}

/// A resolved LLM provider: the port plus everything `ExtractorConfig` and
/// `Brain` need to reflect it (model id, mechanism, weights digest, exact
/// tokenizer when the provider ships one).
pub struct ProviderLlm {
    pub port: Arc<dyn LlmPort>,
    pub model_id: String,
    pub mechanism: ExtractMechanism,
    /// blake3 hex digest of the weights, when the provider is a local artifact
    /// (§9.5 — weight changes must invalidate the extraction cache).
    pub model_digest: Option<String>,
    pub tokenizer: Option<Arc<dyn TokenizerPort>>,
}

/// Decide the provider from the environment, without constructing anything.
/// `key_present`/`openai_key_present` are injected so tests stay hermetic.
pub fn resolve_provider(
    explicit: Option<&str>,
    anthropic_key_present: bool,
    openai_key_present: bool,
) -> anyhow::Result<Provider> {
    match explicit {
        Some("anthropic") => Ok(Provider::Anthropic),
        Some("openai") => Ok(Provider::OpenAi),
        Some("local") => Ok(Provider::Local),
        Some(other) => anyhow::bail!(
            "unknown OXIBRAIN_LLM_PROVIDER={other} (expected: anthropic|openai|local)"
        ),
        // No explicit choice: prefer a configured HTTP provider, fall back to
        // the local model so the no-API-key promise holds.
        None if anthropic_key_present => Ok(Provider::Anthropic),
        None if openai_key_present => Ok(Provider::OpenAi),
        None => Ok(Provider::Local),
    }
}

/// Build an LLM port from the environment.
pub async fn from_env() -> anyhow::Result<ProviderLlm> {
    let explicit = std::env::var("OXIBRAIN_LLM_PROVIDER").ok();
    let provider = resolve_provider(
        explicit.as_deref(),
        std::env::var("ANTHROPIC_API_KEY").is_ok(),
        std::env::var("OPENAI_API_KEY").is_ok(),
    )?;
    match provider {
        Provider::Anthropic => {
            let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
                anyhow::anyhow!("ANTHROPIC_API_KEY not set (required for extraction)")
            })?;
            let model = std::env::var("ANTHROPIC_MODEL")
                .or_else(|_| std::env::var("OXIBRAIN_MODEL"))
                .unwrap_or_else(|_| "claude-sonnet-4-5".to_string());
            Ok(ProviderLlm {
                port: Arc::new(oxibrain_llm_http::AnthropicLlm::new(key, model.clone())),
                model_id: model,
                mechanism: ExtractMechanism::ToolCall,
                model_digest: None,
                tokenizer: None,
            })
        }
        Provider::OpenAi => {
            let key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY not set (required for extraction)"))?;
            let model = std::env::var("OPENAI_MODEL")
                .or_else(|_| std::env::var("OXIBRAIN_MODEL"))
                .unwrap_or_else(|_| "gpt-4o".to_string());
            Ok(ProviderLlm {
                port: Arc::new(oxibrain_llm_http::OpenAiLlm::new(key, model.clone())),
                model_id: model,
                mechanism: ExtractMechanism::JsonSchema,
                model_digest: None,
                tokenizer: None,
            })
        }
        Provider::Local => local_from_manifest().await,
    }
}

/// Pick the extract-role entry out of a manifest. Pure, for tests.
fn extract_entry(
    entries: &[oxibrain::models::ModelEntry],
) -> Option<&oxibrain::models::ModelEntry> {
    entries
        .iter()
        .find(|e| e.role == oxibrain::models::ModelRole::Extract)
}

/// Make sure the local extract model is on disk before we open it. Pure
/// decision in `oxibrain::pull_plan`; the pull (network, fs writes) lives
/// here where it can show progress to a real terminal.
async fn ensure_local_model_present() -> anyhow::Result<()> {
    use oxibrain::models::{default_manifest, load_manifest, model_dir, pull_entry, save_manifest};
    use oxibrain::pull_plan::{ExtractPullPlan, plan_extract_pull};

    let dir = model_dir();
    // Touch the dir so plan_extract_pull can find files there.
    std::fs::create_dir_all(&dir)?;
    // A malformed manifest is a loud error, not a silent reset: bootstrap
    // must never overwrite entries the user cannot see were dropped.
    let manifest = load_manifest().map_err(|e| anyhow::anyhow!("load model manifest: {e}"))?;
    let defaults = default_manifest();
    let plan = plan_extract_pull(&manifest, &dir, &defaults);

    let entry = match plan {
        ExtractPullPlan::NoOp => return Ok(()),
        ExtractPullPlan::NeedsPullFromManifest(e) => e,
        ExtractPullPlan::NeedsBootstrap(e) => {
            // First-time setup: persist the default manifest so subsequent
            // loads are stable.
            let mut next = manifest.clone();
            if !next.iter().any(|m| m.name == e.name) {
                next.push(e.clone());
                save_manifest(&next)?;
            }
            e
        }
    };

    println!(
        "pulling local extract model {} ({} MiB) — first use only...",
        entry.name, entry.size_mb
    );
    pull_entry(&entry, &dir, oxibrain::models::cli_progress)
        .await
        .map_err(|e| anyhow::anyhow!("pull {}: {e}", entry.name))?;
    println!("  verified");
    Ok(())
}

/// Load the local extraction model from the artifact manifest (§8.4): verify
/// the digest (weight changes must change the ExtractorId, §9.5), open the
/// GGUF, and expose its tokenizer (§7.5). Lazy-pulls the model on first use
/// so `oxibrain init` does not have to download anything.
async fn local_from_manifest() -> anyhow::Result<ProviderLlm> {
    use oxibrain::models::{load_manifest, model_dir, verify_entry};

    ensure_local_model_present().await?;

    let manifest = load_manifest().context("load model manifest")?;
    let entry = extract_entry(&manifest)
        .ok_or_else(|| anyhow::anyhow!("local extract model could not be resolved after pull"))?;
    let dir = model_dir();
    verify_entry(entry, &dir)
        .map_err(|e| anyhow::anyhow!("model digest mismatch for {}: {e}", entry.name))?;
    let path = dir.join(&entry.file);
    let llm = Arc::new(
        oxibrain_llm_local::LocalLlm::open(&path, oxibrain_llm_local::LocalLlmOptions::default())
            .map_err(|e| anyhow::anyhow!("open local model {}: {e}", path.display()))?,
    );
    Ok(ProviderLlm {
        model_id: entry.name.clone(),
        mechanism: ExtractMechanism::Grammar,
        model_digest: Some(entry.digest.clone()),
        // LocalLlm implements both ports — same weights, exact token counts
        // (§7.5: counted, never estimated).
        port: llm.clone(),
        tokenizer: Some(llm),
    })
}

/// Build a default extractor config from the env-resolved model + mechanism.
pub fn config(
    model_id: String,
    mechanism: ExtractMechanism,
    model_digest: Option<String>,
) -> oxibrain_core::extraction::ExtractorConfig {
    use oxibrain_core::registry::CORE_V1_MAJOR;
    oxibrain_core::extraction::ExtractorConfig {
        model_id,
        prompt_version: 2, // v2: quote-based mentions (ADR-006)
        registry_major: CORE_V1_MAJOR,
        mechanism,
        max_tokens: 8192,
        model_digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_provider_wins() {
        assert_eq!(
            resolve_provider(Some("local"), true, true).unwrap(),
            Provider::Local
        );
        assert_eq!(
            resolve_provider(Some("openai"), true, false).unwrap(),
            Provider::OpenAi
        );
        assert_eq!(
            resolve_provider(Some("anthropic"), false, false).unwrap(),
            Provider::Anthropic
        );
    }

    #[test]
    fn unknown_provider_is_rejected() {
        assert!(resolve_provider(Some("gemini"), false, false).is_err());
    }

    #[test]
    fn no_explicit_and_no_key_falls_back_to_local() {
        // C2: extraction must work with no API key.
        assert_eq!(
            resolve_provider(None, false, false).unwrap(),
            Provider::Local
        );
    }

    #[test]
    fn anthropic_key_preferred_over_local() {
        assert_eq!(
            resolve_provider(None, true, false).unwrap(),
            Provider::Anthropic
        );
        assert_eq!(
            resolve_provider(None, false, true).unwrap(),
            Provider::OpenAi
        );
    }

    #[test]
    fn extract_role_entry_is_selected() {
        use oxibrain::models::{ModelEntry, ModelRole};
        let mk = |role: ModelRole, name: &str| ModelEntry {
            role,
            name: name.into(),
            url: String::new(),
            digest: format!("d-{name}"),
            size_mb: 1,
            license: String::new(),
            file: format!("{name}.gguf"),
        };
        let entries = vec![
            mk(ModelRole::Embed, "bge-m3"),
            mk(ModelRole::Extract, "qwen2.5-1.5b-instruct"),
        ];
        let got = extract_entry(&entries).expect("extract entry");
        assert_eq!(got.name, "qwen2.5-1.5b-instruct");
        assert_eq!(got.digest, "d-qwen2.5-1.5b-instruct");
    }
}
