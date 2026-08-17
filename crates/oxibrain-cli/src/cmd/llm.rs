//! Shared LLM provider construction from environment variables.
//!
//! Used by `extract` and `reextract`. Providers:
//!   - `OXIBRAIN_LLM_PROVIDER=anthropic` (+ `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL`)
//!   - `OXIBRAIN_LLM_PROVIDER=openai`     (+ `OPENAI_API_KEY`, `OPENAI_MODEL`)
//!   - `OXIBRAIN_LLM_PROVIDER=local`      (GGUF from `oxibrain model pull`, §8.4)
//!
//! Resolution order for [`from_env_for_role`], the role-aware entry point
//! (Oxi Foundation v1, Task 3 §3):
//!
//!   1. Explicit `OXIBRAIN_LLM_PROVIDER` (CLI / automation override).
//!   2. Foundation profile for the requested role whose declared
//!      capabilities satisfy the configured extraction mechanism, and whose
//!      Keychain secret resolves. A missing/unavailable secret reports why
//!      that profile cannot run and falls through to (3). It never silently
//!      sends extraction to a different remote provider.
//!   3. Existing `ANTHROPIC_*` / `OPENAI_*` compatibility environment.
//!   4. Local GGUF (C2 — no API key required, default).
//!
//! The legacy [`from_env`] / [`resolve_provider`] entry points remain in
//! place so the existing `extract` / `reextract` callers do not move; they
//! default to role `memory.extract`. `OXIBRAIN_LLM_ROLE` overrides the role
//! when present.
//!
//! `OXIBRAIN_MODEL` is a fallback for the HTTP model id. The mechanism
//! (tool-call / json-schema / GBNF grammar) follows the provider — Anthropic
//! uses forced tool calls, OpenAI native json_schema structured output, and
//! the local path grammar-constrained decoding (DESIGN §7.4, §9.4).

use anyhow::Context as _;
use oxibrain_core::extraction::ExtractMechanism;
use oxibrain_ports::{LlmPort, TokenizerPort};
use std::sync::Arc;

use crate::cmd::foundation::{
    self, FoundationError, ProfileRole, ProviderKind, ProviderProfile, ResolvedProfiles,
    SecretResolver, default_secret_resolver,
};

/// Which provider `from_env` resolved. Testable without touching the network
/// or loading model weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Local,
}

/// Where the resolved provider came from. Carries enough metadata for callers
/// (and tests) to prove the resolution ladder actually fired the step they
/// expect. Foundation-resolved profiles additionally surface the profile id
/// so Task 5 can plumb it into `ExtractorConfig` / `ExtractorId` provenance.
#[derive(Debug, Clone)]
// Variant fields and the `ExplicitOverride` variant are part of the public
// resolution-ladder API; some are constructed for future callers / pattern
// matches without binding their fields, which trips cargo's `dead_code` lint
// from inside the `oxibrain-cli` crate. The allow keeps the API surface
// unencumbered; the lint still fires for genuinely unused code below.
#[allow(dead_code)]
pub enum ResolutionSource {
    /// Explicit `OXIBRAIN_LLM_PROVIDER=…` override.
    ExplicitOverride(Provider),
    /// A Foundation profile for the requested role, with the secret resolved
    /// out-of-band. `profile_id` is the profile's `id` field; `model_id` is
    /// the profile's `model`; `provider` and `mechanism` are derived from the
    /// profile's `provider` field and the host's adapter catalogue.
    FoundationProfile {
        profile_id: String,
        provider: ProviderKind,
        model_id: String,
        mechanism: ExtractMechanism,
    },
    /// Existing compatibility environment variable. `kind` is `Anthropic` or
    /// `OpenAi`; the model id is whatever `*_MODEL` / `OXIBRAIN_MODEL`
    /// resolved to.
    CompatEnv {
        kind: ProviderKind,
        model_id: String,
    },
    /// Local GGUF — the standalone default (C2).
    Local,
}

/// A resolved LLM provider: the port plus everything `ExtractorConfig` and
/// `Brain` need to reflect it (model id, mechanism, weights digest, exact
/// tokenizer when the provider ships one, plus the resolution source for
/// provenance).
pub struct ProviderLlm {
    pub port: Arc<dyn LlmPort>,
    pub model_id: String,
    pub mechanism: ExtractMechanism,
    /// blake3 hex digest of the weights, when the provider is a local artifact
    /// (§9.5 — weight changes must invalidate the extraction cache).
    pub model_digest: Option<String>,
    pub tokenizer: Option<Arc<dyn TokenizerPort>>,
    /// Where the resolution ladder picked this provider. Task 5 threads
    /// profile identity / model digest into `ExtractorId` provenance from
    /// this field; the CLI does not edit `ExtractorConfig` directly here.
    pub source: ResolutionSource,
}

impl ProviderLlm {
    /// Foundation profile id when the provider came from a profile
    /// resolution; `None` for the legacy compat env / explicit override /
    /// local GGUF paths. Consumed by Task 5 to fold the binding into
    /// `ExtractorConfig::provider_profile_id` and invalidate cached
    /// summaries when the role changes (§13).
    pub fn profile_id(&self) -> Option<String> {
        match &self.source {
            ResolutionSource::FoundationProfile { profile_id, .. } => Some(profile_id.clone()),
            ResolutionSource::ExplicitOverride(_)
            | ResolutionSource::CompatEnv { .. }
            | ResolutionSource::Local => None,
        }
    }
}

/// Decide the provider from the explicit override alone, without consulting
/// Foundation profiles. `key_present` / `openai_key_present` are injected so
/// tests stay hermetic. Kept for the legacy callers; new code should prefer
/// [`from_env_for_role`].
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

/// Role chosen by `OXIBRAIN_LLM_ROLE` (or the default). The env var is the
/// only way to override the role today; future revisions can extend
/// `OXIBRAIN_LLM_ROLE` to comma-separated lists for fan-out consolidation.
pub fn resolve_role() -> ProfileRole {
    if let Ok(raw) = std::env::var("OXIBRAIN_LLM_ROLE") {
        if let Some(role) = ProfileRole::parse(&raw) {
            return role;
        }
        // An unparseable role is loud — extraction must not silently fall to
        // a different role. The caller treats this as a Foundation parse
        // rejection when it eventually surfaces.
        tracing::warn!(
            role = %raw,
            "OXIBRAIN_LLM_ROLE is not a known role; falling back to memory.extract"
        );
    }
    ProfileRole::MemoryExtract
}

/// Build an LLM port from the environment using the legacy (role-less)
/// ladder. Preserved for existing `extract` / `reextract` callers that have
/// not yet opted into the Foundation-aware entry point.
pub async fn from_env() -> anyhow::Result<ProviderLlm> {
    from_env_for_role(resolve_role()).await
}

/// Build an LLM port from the environment for a specific role.
///
/// Walks the resolution ladder documented at the top of this module. A
/// missing/unavailable Foundation secret is reported to stderr and falls
/// through to the next step — never silently to a different remote provider.
pub async fn from_env_for_role(role: ProfileRole) -> anyhow::Result<ProviderLlm> {
    let explicit = std::env::var("OXIBRAIN_LLM_PROVIDER").ok();
    let anthropic_key_present = std::env::var("ANTHROPIC_API_KEY").is_ok();
    let openai_key_present = std::env::var("OPENAI_API_KEY").is_ok();

    // Step 1 — explicit override always wins (automation / dev override).
    if let Some(name) = explicit.as_deref() {
        match resolve_provider(Some(name), anthropic_key_present, openai_key_present)? {
            Provider::Anthropic => return anthropic_from_env(),
            Provider::OpenAi => return openai_from_env(),
            Provider::Local => return local_from_manifest().await,
        }
    }

    // Step 2 — Foundation profile for the requested role. `secret_resolver`
    // is the production default unless the caller passes its own.
    let resolved_profiles =
        foundation::load_profiles(&foundation::foundation_home()).map_err(anyhow::Error::msg)?;
    if let Some(profiles) = resolved_profiles {
        if let Some(provider) =
            try_foundation_profile(&profiles, role, default_secret_resolver().as_ref()).await?
        {
            return Ok(provider);
        }
    }

    // Step 3 — ANTHROPIC_* / OPENAI_* compat env.
    if anthropic_key_present {
        return anthropic_from_env();
    }
    if openai_key_present {
        return openai_from_env();
    }

    // Step 4 — local (C2).
    local_from_manifest().await
}

/// Attempt to resolve a Foundation profile for the role. Returns:
///   - `Ok(Some(_))` when a profile was selected and its secret resolved.
///   - `Ok(None)` when the resolver reported `SecretUnavailable` for the
///     only candidate profile; the caller falls through to the next ladder
///     step after logging the reason. This is the explicit "do not silently
///     send to a different remote provider" guarantee.
///   - `Err(_)` for hard parse / capability rejections that should surface to
///     the operator.
#[doc(hidden)]
pub async fn try_foundation_profile(
    profiles: &ResolvedProfiles,
    role: ProfileRole,
    secret_resolver: &dyn SecretResolver,
) -> anyhow::Result<Option<ProviderLlm>> {
    // Pick the configured mechanism per provider so a truthful OpenAI profile
    // that declares only `json_schema: true` is accepted, not bailed with
    // CapabilityUnsatisfied against ToolCall. We iterate the profiles and
    // try each one with its native mechanism so a single profile list can
    // carry heterogeneous declarations.
    //
    // Algorithm:
    //   1. For each profile that lists `role`, determine its native mechanism
    //      from its `provider` field.
    //   2. Validate against that mechanism. Reject loudly if declared
    //      capabilities don't satisfy it.
    //   3. Return the first profile that survives capability validation.
    //
    // When no profile declares the role we fall through silently (compat env
    // / local may still satisfy the request).
    let mut selected_profile: Option<&ProviderProfile> = None;
    for profile in profiles.iter() {
        if !profile.roles.contains(&role) {
            continue;
        }
        let mechanism = match ProviderKind::parse(&profile.provider) {
            Some(ProviderKind::OpenAi) => ExtractMechanism::JsonSchema,
            Some(ProviderKind::Anthropic) | None => ExtractMechanism::ToolCall,
        };
        if !profile.capabilities.clone().satisfies(mechanism) {
            anyhow::bail!(
                "Foundation profile `{}` rejected: declared capabilities do not satisfy extraction mechanism {:?}",
                profile.id,
                mechanism
            );
        }
        selected_profile = Some(profile);
        break;
    }
    let profile = match selected_profile {
        Some(p) => p,
        None => return Ok(None),
    };
    let mechanism = match ProviderKind::parse(&profile.provider) {
        Some(ProviderKind::OpenAi) => ExtractMechanism::JsonSchema,
        _ => ExtractMechanism::ToolCall,
    };

    // Resolve the secret out-of-band. A missing secret here falls through to
    // compat env / local; we never send extraction to a different remote
    // provider.
    let secret = match secret_resolver.resolve(&profile.credential) {
        Ok(s) => s,
        Err(e @ FoundationError::SecretUnavailable { .. }) => {
            tracing::warn!("{e}");
            return Ok(None);
        }
        Err(other) => return Err(anyhow::Error::msg(other.to_string())),
    };

    let provider_kind = ProviderKind::parse(&profile.provider).ok_or_else(|| {
        anyhow::anyhow!(
            "Foundation profile `{}` has unknown provider `{}`",
            profile.id,
            profile.provider
        )
    })?;

    let port: Arc<dyn LlmPort> = match provider_kind {
        ProviderKind::Anthropic => Arc::new(oxibrain_llm_http::AnthropicLlm::new(
            secret,
            profile.model.clone(),
        )),
        ProviderKind::OpenAi => Arc::new(oxibrain_llm_http::OpenAiLlm::new(
            secret,
            profile.model.clone(),
        )),
    };

    Ok(Some(ProviderLlm {
        port,
        model_id: profile.model.clone(),
        mechanism,
        model_digest: None,
        tokenizer: None,
        source: ResolutionSource::FoundationProfile {
            profile_id: profile.id.clone(),
            provider: provider_kind,
            model_id: profile.model.clone(),
            mechanism,
        },
    }))
}

fn anthropic_from_env() -> anyhow::Result<ProviderLlm> {
    let key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set (required for extraction)"))?;
    let model = std::env::var("ANTHROPIC_MODEL")
        .or_else(|_| std::env::var("OXIBRAIN_MODEL"))
        .unwrap_or_else(|_| "claude-sonnet-4-5".to_string());
    Ok(ProviderLlm {
        port: Arc::new(oxibrain_llm_http::AnthropicLlm::new(key, model.clone())),
        model_id: model.clone(),
        mechanism: ExtractMechanism::ToolCall,
        model_digest: None,
        tokenizer: None,
        source: ResolutionSource::CompatEnv {
            kind: ProviderKind::Anthropic,
            model_id: model,
        },
    })
}

fn openai_from_env() -> anyhow::Result<ProviderLlm> {
    let key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY not set (required for extraction)"))?;
    let model = std::env::var("OPENAI_MODEL")
        .or_else(|_| std::env::var("OXIBRAIN_MODEL"))
        .unwrap_or_else(|_| "gpt-4o".to_string());
    Ok(ProviderLlm {
        port: Arc::new(oxibrain_llm_http::OpenAiLlm::new(key, model.clone())),
        model_id: model.clone(),
        mechanism: ExtractMechanism::JsonSchema,
        model_digest: None,
        tokenizer: None,
        source: ResolutionSource::CompatEnv {
            kind: ProviderKind::OpenAi,
            model_id: model,
        },
    })
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
        source: ResolutionSource::Local,
    })
}

/// Build a default extractor config from the env-resolved model + mechanism.
pub fn config(
    model_id: String,
    mechanism: ExtractMechanism,
    model_digest: Option<String>,
    provider_profile_id: Option<String>,
) -> oxibrain_core::extraction::ExtractorConfig {
    use oxibrain_core::registry::CORE_V1_MAJOR;
    oxibrain_core::extraction::ExtractorConfig {
        model_id,
        prompt_version: 2, // v2: quote-based mentions (ADR-006)
        registry_major: CORE_V1_MAJOR,
        mechanism,
        max_tokens: 8192,
        model_digest,
        provider_profile_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Process-wide lock for tests that mutate `OXIBRAIN_LLM_ROLE`.
    /// cargo defaults to running tests in parallel across threads; env
    /// vars are process-global, so any two tests that touch the same
    /// variable race. Every set-var / remove-var call in this module
    /// MUST hold this lock for the duration of the test body.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    fn resolve_role_defaults_to_memory_extract() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("OXIBRAIN_LLM_ROLE");
        // SAFETY: env vars are serialised via ENV_LOCK in this module.
        unsafe {
            std::env::remove_var("OXIBRAIN_LLM_ROLE");
        }
        let got = resolve_role();
        // SAFETY: see above.
        unsafe {
            if let Some(v) = saved {
                std::env::set_var("OXIBRAIN_LLM_ROLE", v);
            }
        }
        assert_eq!(got, ProfileRole::MemoryExtract);
    }

    #[test]
    fn resolve_role_honours_env_when_recognised() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("OXIBRAIN_LLM_ROLE");
        // SAFETY: env vars are serialised via ENV_LOCK in this module.
        unsafe {
            std::env::set_var("OXIBRAIN_LLM_ROLE", "coding.primary");
        }
        let got = resolve_role();
        // SAFETY: see above.
        unsafe {
            match saved {
                Some(v) => std::env::set_var("OXIBRAIN_LLM_ROLE", v),
                None => std::env::remove_var("OXIBRAIN_LLM_ROLE"),
            }
        }
        assert_eq!(got, ProfileRole::CodingPrimary);
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
