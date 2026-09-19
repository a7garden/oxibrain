//! Shared LLM provider construction from environment variables.
//!
//! Used by `extract` and `reextract`. Providers:
//!   - `OXIBRAIN_LLM_PROVIDER=anthropic` (+ `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL`)
//!   - `OXIBRAIN_LLM_PROVIDER=openai`     (+ `OPENAI_API_KEY`, `OPENAI_MODEL`)
//!   - `OXIBRAIN_LLM_PROVIDER=loopback`   (+ `OXIBRAIN_LLM_MODEL`, optional
//!     `OXIBRAIN_LLM_BASE_URL` / `OXIBRAIN_LLM_API_KEY`) — a loopback
//!     OpenAI-compatible server: the MLX path (LM Studio's MLX engine,
//!     `mlx_lm.server`) or a llama.cpp `server`. Aliases: `lmstudio`, `mlx`.
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
//! uses forced tool calls, OpenAI and loopback servers native json_schema
//! structured output, and the local path grammar-constrained decoding
//! (DESIGN §7.4, §9.4).

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
    /// Loopback OpenAI-compatible server (LM Studio's MLX engine,
    /// `mlx_lm.server`, llama.cpp `server`). Local, no account, no key.
    Loopback,
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
        // "lmstudio" / "mlx" are mnemonic aliases for the same loopback
        // OpenAI-compatible surface; the base URL / model decide which
        // server actually answers.
        Some("loopback") | Some("lmstudio") | Some("mlx") => Ok(Provider::Loopback),
        Some("local") => Ok(Provider::Local),
        Some(other) => anyhow::bail!(
            "unknown OXIBRAIN_LLM_PROVIDER={other} (expected: anthropic|openai|loopback|local)"
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
            Provider::Loopback => return loopback_from_env(),
            Provider::Local => return local_from_manifest().await,
        }
    }

    // Step 2 — Foundation profile for the requested role. `secret_resolver`
    // is the production default unless the caller passes its own.
    let resolved_profiles =
        foundation::load_profiles(&foundation::foundation_home()).map_err(anyhow::Error::msg)?;
    if let Some(profiles) = resolved_profiles
        && let Some(provider) =
            try_foundation_profile(&profiles, role, default_secret_resolver().as_ref()).await?
    {
        return Ok(provider);
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

/// Loopback OpenAI-compatible server — the MLX path (LM Studio's MLX
/// engine by default; also `mlx_lm.server`, llama.cpp `server`). Local
/// process on this machine, no account, no API key. Structured output
/// rides the server's `response_format: json_schema` support (mechanism
/// JsonSchema; schema-and-repair in the pipeline) — GBNF grammar
/// constraints remain a llama.cpp-only capability (D28), and the
/// post-extraction validator gates correctness either way.
///
/// Env:
///   - `OXIBRAIN_LLM_BASE_URL` (default `http://127.0.0.1:1234/v1`, the
///     LM Studio server default)
///   - `OXIBRAIN_LLM_MODEL` (required — the server's model id; never
///     guessed)
///   - `OXIBRAIN_LLM_API_KEY` (optional bearer for servers behind auth)
fn loopback_from_env() -> anyhow::Result<ProviderLlm> {
    let base_url = std::env::var("OXIBRAIN_LLM_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:1234/v1".to_string());
    let model = std::env::var("OXIBRAIN_LLM_MODEL").map_err(|_| {
        anyhow::anyhow!(
            "loopback provider needs OXIBRAIN_LLM_MODEL (the server-side model id); \
             refusing to guess which model the server should load"
        )
    })?;
    let api_key = std::env::var("OXIBRAIN_LLM_API_KEY").ok();
    Ok(ProviderLlm {
        port: Arc::new(oxibrain_llm_http::OpenAiLlm::with_base_url(
            base_url,
            api_key,
            model.clone(),
        )),
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

/// Engine-aware entry selection: `OXIBRAIN_ENGINE=mlx` picks the first
/// `format = "mlx"` extract entry (falling back to the first extract entry
/// with a loud error path when none exists). Default: the first extract
/// entry, whatever its format. Pure, for tests.
fn select_extract_entry(
    entries: &[oxibrain::models::ModelEntry],
) -> Option<&oxibrain::models::ModelEntry> {
    let wants_mlx = std::env::var("OXIBRAIN_ENGINE").ok().as_deref() == Some("mlx");
    if wants_mlx {
        return entries
            .iter()
            .find(|e| e.role == oxibrain::models::ModelRole::Extract
                && e.format == oxibrain::models::ModelFormat::Mlx)
            .or_else(|| extract_entry(entries));
    }
    extract_entry(entries)
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
    // MLX entries resolve outside the models dir (HF cache / local path)
    // and are never pulled by this process — presence and fingerprint are
    // checked when the engine loads (ADR-017). Only the SELECTED entry
    // matters (OXIBRAIN_ENGINE=mlx), so a manifest that also carries a
    // GGUF entry still lazy-pulls it for GGUF runs.
    if select_extract_entry(&manifest)
        .is_some_and(|m| m.format == oxibrain::models::ModelFormat::Mlx)
    {
        return Ok(());
    }
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
    #[cfg_attr(feature = "mlx", allow(unused_imports))]
    use oxibrain::models::{ModelFormat, load_manifest, model_dir, verify_entry};

    ensure_local_model_present().await?;

    let manifest = load_manifest().context("load model manifest")?;
    let entry = select_extract_entry(&manifest)
        .ok_or_else(|| anyhow::anyhow!("local extract model could not be resolved after pull"))?;
    #[cfg_attr(feature = "mlx", allow(unused_variables))]
    let dir = model_dir();

    // The engine is chosen per manifest entry (ADR-017): GGUF loads through
    // llama.cpp (GBNF, D28); MLX safetensors load in-process on Apple
    // Silicon (schema-and-repair; the validator stays the gate).
    match entry.format {
        ModelFormat::Mlx => {
            #[cfg(feature = "mlx")]
            {
                let dir = oxibrain_llm_mlx::weights::resolve_model_dir(&entry.file)
                    .map_err(|e| anyhow::anyhow!("resolve MLX model `{}`: {e}", entry.file))?;
                let fingerprint = oxibrain_llm_mlx::weights::model_fingerprint(&dir)
                    .map_err(|e| anyhow::anyhow!("fingerprint MLX model: {e}"))?;
                if fingerprint != entry.digest {
                    anyhow::bail!(
                        "MLX model digest mismatch for {}: manifest {} != on disk {} \
                         (update the manifest digest after re-pulling the model)",
                        entry.name,
                        entry.digest,
                        fingerprint
                    );
                }
                let llm = Arc::new(oxibrain_llm_mlx::LocalMxlLlm::load(&entry.file, 16_384)?);
                Ok(ProviderLlm {
                    model_id: entry.name.clone(),
                    mechanism: ExtractMechanism::JsonMode,
                    model_digest: Some(entry.digest.clone()),
                    port: llm.clone(),
                    tokenizer: Some(llm),
                    source: ResolutionSource::Local,
                })
            }
            #[cfg(not(feature = "mlx"))]
            anyhow::bail!(
                "manifest entry `{}` has format = \"mlx\" but this oxibrain \
                 binary was built without the `mlx` feature; rebuild with \
                 `--features mlx` or switch the extract entry to a GGUF model",
                entry.name
            );
        }
        ModelFormat::Gguf => {
            // mlx builds statically link llama.cpp AND mlx-c; llama.cpp's
            // GGUF parsing segfaults in that binary (ADR-017). Refuse the
            // load loudly instead of crashing.
            #[cfg(feature = "mlx")]
            anyhow::bail!(
                "manifest entry `{}` has format = \"gguf\" but this oxibrain \
                 build links mlx-c alongside llama.cpp and cannot load GGUF \
                 weights (ADR-017); use a non-mlx build or an mlx manifest \
                 entry",
                entry.name
            );
            #[cfg(not(feature = "mlx"))]
            {
                verify_entry(entry, &dir)
                    .map_err(|e| anyhow::anyhow!("model digest mismatch for {}: {e}", entry.name))?;
                let path = dir.join(&entry.file);
                let llm = Arc::new(
                    oxibrain_llm_local::LocalLlm::open(
                        &path,
                        oxibrain_llm_local::LocalLlmOptions::default(),
                    )
                    .map_err(|e| anyhow::anyhow!("open local model {}: {e}", path.display()))?,
                );
                Ok(ProviderLlm {
                    model_id: entry.name.clone(),
                    mechanism: ExtractMechanism::Grammar,
                    model_digest: Some(entry.digest.clone()),
                    // LocalLlm implements both ports — same weights, exact
                    // token counts (§7.5: counted, never estimated).
                    port: llm.clone(),
                    tokenizer: Some(llm),
                    source: ResolutionSource::Local,
                })
            }
        }
    }
}

/// Bind the manifest/env-derived extractor identity WITHOUT loading weights
/// or contacting any network. Used by surfaces that never extract (serve,
/// op dispatch) so `pending_extraction_stats` reports the backlog the next
/// `admin extract --pending` would actually drain — after a model swap the
/// cache identity changes, and a default-identity count would read 0.
///
/// Resolution mirrors the no-network part of the ladder: explicit provider
/// env → anthropic/openai keys → the manifest's local extract entry.
pub fn bind_extract_identity(brain: oxibrain::Brain) -> oxibrain::Brain {
    let cfg = identity_from_env_or_manifest();
    match cfg {
        Some(c) => brain.with_extractor_config(c),
        None => brain,
    }
}

fn identity_from_env_or_manifest() -> Option<oxibrain_core::extraction::ExtractorConfig> {
    use oxibrain::models::{ModelFormat, ModelRole};
    let explicit = std::env::var("OXIBRAIN_LLM_PROVIDER").ok();
    let model_for = |prefix: &str| std::env::var(format!("{prefix}_MODEL")).ok();
    if let Some(name) = explicit.as_deref() {
        return match name {
            "anthropic" => model_for("ANTHROPIC").map(|m| {
                config(m, ExtractMechanism::ToolCall, None, None)
            }),
            "openai" => model_for("OPENAI").map(|m| {
                config(m, ExtractMechanism::JsonSchema, None, None)
            }),
            "loopback" | "lmstudio" | "mlx" => {
                std::env::var("OXIBRAIN_LLM_MODEL").ok().map(|m| {
                    config(m, ExtractMechanism::JsonSchema, None, None)
                })
            }
            "local" => None, // fall through to the manifest below
            _ => None,
        };
    }
    if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
        return model_for("ANTHROPIC")
            .map(|m| config(m, ExtractMechanism::ToolCall, None, None));
    }
    if std::env::var_os("OPENAI_API_KEY").is_some() {
        return model_for("OPENAI")
            .map(|m| config(m, ExtractMechanism::JsonSchema, None, None));
    }
    // Local: derive from the manifest extract entry (no weights loaded).
    let dir = oxibrain::models::model_dir();
    let manifest = oxibrain::models::load_manifest().ok()?;
    let defaults = oxibrain::models::default_manifest();
    let entry = select_extract_entry(&manifest)
        .or_else(|| defaults.iter().find(|e| e.role == ModelRole::Extract))?;
    let digest = if entry.format == ModelFormat::Mlx {
        #[cfg(feature = "mlx")]
        {
            oxibrain_llm_mlx::weights::resolve_model_dir(&entry.file)
                .ok()
                .and_then(|d| oxibrain_llm_mlx::weights::model_fingerprint(&d).ok())
        }
        #[cfg(not(feature = "mlx"))]
        {
            None
        }
    } else {
        Some(entry.digest.clone())
    };
    let mechanism = if entry.format == ModelFormat::Mlx {
        ExtractMechanism::JsonMode
    } else {
        ExtractMechanism::Grammar
    };
    let _ = dir;
    Some(config(
        entry.name.clone(),
        mechanism,
        digest,
        None,
    ))
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
    fn loopback_aliases_resolve_to_loopback() {
        for name in ["loopback", "lmstudio", "mlx"] {
            assert_eq!(
                resolve_provider(Some(name), false, false).unwrap(),
                Provider::Loopback,
                "alias {name} must resolve to the loopback provider"
            );
        }
    }

    #[test]
    fn engine_env_selects_mlx_entry() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let entries = vec![
            ModelEntryShim::gguf("daily-gguf"),
            ModelEntryShim::mlx("big-mlx"),
        ];
        let as_entries = |v: Vec<ModelEntryShim>| {
            v.into_iter()
                .map(|s| oxibrain::models::ModelEntry {
                    role: oxibrain::models::ModelRole::Extract,
                    name: s.name,
                    file: s.file,
                    url: String::new(),
                    digest: String::new(),
                    size_mb: 1,
                    license: String::new(),
                    format: s.format,
                })
                .collect::<Vec<_>>()
        };
        let saved = std::env::var_os("OXIBRAIN_ENGINE");
        // SAFETY: env vars are serialised via ENV_LOCK in this module.
        unsafe { std::env::remove_var("OXIBRAIN_ENGINE") };
        assert_eq!(select_extract_entry(&as_entries(entries.clone())).unwrap().name, "daily-gguf");
        unsafe { std::env::set_var("OXIBRAIN_ENGINE", "mlx") };
        assert_eq!(select_extract_entry(&as_entries(entries.clone())).unwrap().name, "big-mlx");
        // SAFETY: see above.
        unsafe {
            match saved {
                Some(v) => std::env::set_var("OXIBRAIN_ENGINE", v),
                None => std::env::remove_var("OXIBRAIN_ENGINE"),
            }
        }
    }

    #[derive(Clone)]
    struct ModelEntryShim {
        name: String,
        file: String,
        format: oxibrain::models::ModelFormat,
    }
    impl ModelEntryShim {
        fn gguf(name: &str) -> Self {
            Self { name: name.into(), file: format!("{name}.gguf"), format: oxibrain::models::ModelFormat::Gguf }
        }
        fn mlx(name: &str) -> Self {
            Self { name: name.into(), file: name.into(), format: oxibrain::models::ModelFormat::Mlx }
        }
    }

    #[test]
    fn loopback_from_env_requires_a_model_id() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_base = std::env::var_os("OXIBRAIN_LLM_BASE_URL");
        let saved_model = std::env::var_os("OXIBRAIN_LLM_MODEL");
        // SAFETY: env vars are serialised via ENV_LOCK in this module.
        unsafe {
            std::env::remove_var("OXIBRAIN_LLM_BASE_URL");
            std::env::remove_var("OXIBRAIN_LLM_MODEL");
        }

        // No model id: loud refusal — the server must be told which model
        // to serve, never guessed.
        assert!(loopback_from_env().is_err());

        // SAFETY: see above.
        unsafe {
            std::env::set_var("OXIBRAIN_LLM_MODEL", "qwen/qwen3-30b-a3b-2507");
        }
        let provider = loopback_from_env().unwrap();
        assert_eq!(provider.model_id, "qwen/qwen3-30b-a3b-2507");
        assert_eq!(provider.mechanism, ExtractMechanism::JsonSchema);
        assert!(provider.model_digest.is_none());
        // Default base URL is the LM Studio server port.
        match &provider.source {
            ResolutionSource::CompatEnv { kind, model_id } => {
                assert_eq!(*kind, ProviderKind::OpenAi);
                assert_eq!(model_id, "qwen/qwen3-30b-a3b-2507");
            }
            other => panic!("expected CompatEnv source, got {other:?}"),
        }

        // SAFETY: see above.
        unsafe {
            match saved_base {
                Some(v) => std::env::set_var("OXIBRAIN_LLM_BASE_URL", v),
                None => std::env::remove_var("OXIBRAIN_LLM_BASE_URL"),
            }
            match saved_model {
                Some(v) => std::env::set_var("OXIBRAIN_LLM_MODEL", v),
                None => std::env::remove_var("OXIBRAIN_LLM_MODEL"),
            }
        }
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
            format: oxibrain::models::ModelFormat::Gguf,
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
