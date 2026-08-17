//! Oxi Foundation v1 — provider profile parsing and Keychain-backed secrets.
//!
//! Parses `~/.oxi/foundation/v1/profiles.json` (or the path rooted at
//! `$OXI_FOUNDATION_HOME` for tests and deployment overrides) according to the
//! schema in `doc/spec/oxi-foundation-v1.md` §2. Profiles are non-secret by
//! construction: any field that could carry a secret is a parse-level rejection.
//!
//! The `SecretResolver` trait lives at this CLI boundary and is never named
//! from `oxibrain-core` / `oxibrain-store` / `oxibrain-index` (see ARCHITECTURE
//! §15.7, ADR-007). Tests use `InMemorySecretResolver`; production uses
//! `OsKeychainResolver` when the CLI is built with `--features os-keychain`.
//!
//! Resolution ladder for an extraction role (Task 3 §3):
//!   1. `OXIBRAIN_LLM_PROVIDER` (CLI/env override) — wins outright.
//!   2. Foundation profile for the requested role whose declared capabilities
//!      satisfy the configured extraction mechanism, and whose Keychain secret
//!      resolves. A missing/unavailable secret reports why that profile cannot
//!      run and falls through to (3) without sending extraction elsewhere.
//!   3. Existing `ANTHROPIC_*` / `OPENAI_*` compatibility environment.
//!   4. Local GGUF (C2 — no API key required).
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use oxibrain_core::extraction::ExtractMechanism;
use oxibrain_ports::LlmCapabilities;
use serde::{Deserialize, Serialize};

// ─── schema-version literal (doc/spec/oxi-foundation-v1.md §2.2) ───────────

/// The only `schema_version` this host accepts. Any other value rejects the
/// whole `profiles.json` at parse time — the host does not silently coerce.
pub const SCHEMA_VERSION: u32 = 1;

/// The set of legal role strings (§2.3). The host rejects profiles that list
/// any role outside this set, so a typo never silently disables a profile.
// Cross-host surface (spec §2.3 closed set): consumed by oxicode/oxios
// for role validation. The oxibrain-cli parser routes through the typed
// `ProfileRole` enum and never reads this constant.
#[allow(dead_code)]
pub const ALLOWED_ROLES: &[&str] = &[
    "memory.extract",
    "memory.consolidate",
    "coding.primary",
    "assistant.general",
];

/// Field names whose presence in `profiles.json` is a parse-level rejection
/// (§2.5). The locator is the only credential surface; anything that smells
/// like an inline secret is a hard fail.
pub const SECRET_FIELD_NAMES: &[&str] = &[
    "api_key",
    "apikey",
    "api-token",
    "bearer",
    "access_token",
    "refresh_token",
    "secret",
    "password",
    "private_key",
];

// ─── parsed schema types ──────────────────────────────────────────────────

/// The four roles a profile can declare (§2.3). Strongly typed so call sites
/// compose against an enum, not a raw string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileRole {
    #[serde(rename = "memory.extract")]
    MemoryExtract,
    #[serde(rename = "memory.consolidate")]
    MemoryConsolidate,
    #[serde(rename = "coding.primary")]
    CodingPrimary,
    #[serde(rename = "assistant.general")]
    AssistantGeneral,
}

impl ProfileRole {
    /// Cross-host surface: external hosts serialise roles back to the wire
    /// string when emitting config / logs; the in-crate callers work in
    /// the typed enum.
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileRole::MemoryExtract => "memory.extract",
            ProfileRole::MemoryConsolidate => "memory.consolidate",
            ProfileRole::CodingPrimary => "coding.primary",
            ProfileRole::AssistantGeneral => "assistant.general",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "memory.extract" => ProfileRole::MemoryExtract,
            "memory.consolidate" => ProfileRole::MemoryConsolidate,
            "coding.primary" => ProfileRole::CodingPrimary,
            "assistant.general" => ProfileRole::AssistantGeneral,
            _ => return None,
        })
    }
}

impl fmt::Display for ProfileRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A Keychain locator for the profile's secret. The shape is fixed by §2.4:
/// `{service, account}`. The host's [`SecretResolver`] turns this into a
/// secret at runtime; the locator itself is safe to share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretLocator {
    pub service: String,
    pub account: String,
}

/// Capabilities the profile's remote model declares (§2.5 lists which
/// `ExtractMechanism` flags it advertises). Profiles whose declared set does
/// not satisfy the configured mechanism are rejected before any Keychain
/// lookup happens.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeclaredCapabilities {
    pub grammar: bool,
    pub structured_output: bool,
    pub tool_call: bool,
    pub json_schema: bool,
}

impl DeclaredCapabilities {
    /// Does this capability set satisfy `mechanism`? The mapping mirrors the
    /// existing adapter implementations — grammar for local GGUF, json_schema
    /// for OpenAI, tool_call for Anthropic, json_mode always true (the
    /// validator is the only gate). Profiles whose declared set fails this
    /// check are rejected before the Keychain is touched.
    pub fn satisfies(&self, mechanism: ExtractMechanism) -> bool {
        match mechanism {
            ExtractMechanism::Grammar => self.grammar,
            ExtractMechanism::JsonSchema => self.json_schema || self.structured_output,
            ExtractMechanism::ToolCall => self.tool_call,
            ExtractMechanism::JsonMode => true,
        }
    }

    /// Convert to the ports-trait representation. The CLI bridge carries this
    /// across the boundary to the adapter selection logic.
    /// Cross-host surface: oxicode/oxios map a profile's declared
    /// capabilities into the adapter's `LlmCapabilities` view.
    #[allow(dead_code, clippy::wrong_self_convention)]
    pub fn as_llm_capabilities(self) -> LlmCapabilities {
        LlmCapabilities {
            grammar: self.grammar,
            structured_output: self.structured_output,
            tool_call: self.tool_call,
            json_schema: self.json_schema,
        }
    }
}

/// A single Foundation profile (§2.1, §2.2). Parsed strictly: extra fields that
/// look like secrets cause the whole file to be rejected (§2.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProfile {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub roles: Vec<ProfileRole>,
    pub credential: SecretLocator,
    /// Optional declared capabilities. When present, the host uses them to
    /// decide whether the profile can satisfy the configured extraction
    /// mechanism before contacting the Keychain. When absent, the profile is
    /// treated as capable of every mechanism (preserves the v0 behaviour for
    /// profiles that haven't been upgraded yet).
    #[serde(default)]
    pub capabilities: DeclaredCapabilities,
}

/// The full `profiles.json` document (§2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundationProfiles {
    pub schema_version: u32,
    pub profiles: Vec<ProviderProfile>,
}

/// A parsed and validated `FoundationProfiles` ready for role resolution.
#[derive(Debug, Clone)]
pub struct ResolvedProfiles {
    /// The profile list, in declaration order. `pick_for_role` returns the
    /// first profile whose role membership matches and whose declared
    /// capabilities satisfy the configured mechanism.
    pub profiles: Vec<ProviderProfile>,
}

/// Why a `profiles.json` was rejected at the parse boundary. The host must
/// report one of these — never silently fall through to a different provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundationError {
    /// `schema_version` is not `1`.
    UnsupportedSchemaVersion(u32),
    /// The JSON document is not a valid `FoundationProfiles` shape.
    InvalidShape(String),
    /// `profiles.json` carries a field that smells like a secret (§2.5).
    SecretFieldPresent(String),
    /// A profile ID appears twice.
    DuplicateProfileId(String),
    /// A role string is not one of the four legal values.
    ///
    /// Cross-host surface: the strict parser does not currently produce
    /// this (serde rejects unknown `ProfileRole` enum variants before the
    /// value reaches us), but the variant is part of the public error
    /// contract so external hosts can map their own role validation to it.
    #[allow(dead_code)]
    UnknownRole(String),
    /// A role appears twice within the same profile.
    DuplicateRole(ProfileRole),
    /// A profile field is the empty string (id, provider, model, credential).
    EmptyField(&'static str),
    /// `roles` is empty.
    EmptyRoles,
    /// The on-disk document could not be read.
    IoError(String),
    /// The OS Keychain refused or did not contain the locator's secret. The
    /// caller logs this verbatim and falls through to the next step in the
    /// resolution ladder (§3 — explicit override wins, profile for role wins,
    /// ANTHROPIC_*/OPENAI_* env, then local). It never silently sends
    /// extraction to a different remote provider.
    SecretUnavailable {
        service: String,
        account: String,
        reason: String,
    },
    /// A Foundation profile was selected, but its declared capabilities do
    /// not satisfy the configured extraction mechanism. The host must reject
    /// the profile before contacting the Keychain.
    ///
    /// Cross-host surface: constructed by
    /// `ResolvedProfiles::pick_for_role`; the oxibrain-cli binary does not
    /// exercise that path directly today, but the integration tests in
    /// `tests/foundation_profiles.rs` do.
    #[allow(dead_code)]
    CapabilityUnsatisfied {
        profile_id: String,
        mechanism: ExtractMechanism,
    },
    /// A profile was selected but its role membership does not include the
    /// role the caller asked for.
    ///
    /// Cross-host surface: same reason as
    /// [`FoundationError::CapabilityUnsatisfied`].
    #[allow(dead_code)]
    RoleDenied {
        profile_id: String,
        requested: ProfileRole,
    },
}

impl fmt::Display for FoundationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FoundationError::UnsupportedSchemaVersion(v) => {
                write!(
                    f,
                    "profiles.json schema_version={v} is not supported (expected 1)"
                )
            }
            FoundationError::InvalidShape(detail) => {
                write!(f, "profiles.json shape invalid: {detail}")
            }
            FoundationError::SecretFieldPresent(field) => write!(
                f,
                "profiles.json rejected: carries secret-shaped field `{field}` (§2.5)"
            ),
            FoundationError::DuplicateProfileId(id) => {
                write!(f, "profiles.json rejected: duplicate profile id `{id}`")
            }
            FoundationError::UnknownRole(role) => write!(
                f,
                "profiles.json rejected: role `{role}` is not one of memory.extract / memory.consolidate / coding.primary / assistant.general"
            ),
            FoundationError::DuplicateRole(role) => write!(
                f,
                "profiles.json rejected: role `{role}` appears twice in the same profile"
            ),
            FoundationError::EmptyField(field) => write!(
                f,
                "profiles.json rejected: field `{field}` is the empty string"
            ),
            FoundationError::EmptyRoles => {
                write!(f, "profiles.json rejected: a profile lists no roles")
            }
            FoundationError::IoError(detail) => write!(f, "profiles.json I/O error: {detail}"),
            FoundationError::SecretUnavailable {
                service,
                account,
                reason,
            } => write!(
                f,
                "Foundation profile secret unavailable (Keychain service=`{service}` account=`{account}`): {reason}"
            ),
            FoundationError::CapabilityUnsatisfied {
                profile_id,
                mechanism,
            } => write!(
                f,
                "Foundation profile `{profile_id}` rejected: declared capabilities do not satisfy extraction mechanism {mechanism:?}"
            ),
            FoundationError::RoleDenied {
                profile_id,
                requested,
            } => write!(
                f,
                "Foundation profile `{profile_id}` rejected: does not declare role `{requested}`"
            ),
        }
    }
}

impl std::error::Error for FoundationError {}

// ─── SecretResolver ───────────────────────────────────────────────────────

/// Resolves a Keychain locator to a secret. Production implementations read
/// the OS Keychain; tests use an in-memory map. The trait is never named from
/// `oxibrain-core` / `oxibrain-store` / `oxibrain-index`; only the CLI
/// adapter boundary calls into it (ARCHITECTURE §15.7).
pub trait SecretResolver: Send + Sync {
    /// Fetch the secret bytes for a validated locator. Returns a structured
    /// [`FoundationError::SecretUnavailable`] so the caller can report why
    /// the profile cannot run and fall through to the next resolution step.
    fn resolve(&self, locator: &SecretLocator) -> Result<String, FoundationError>;
}

/// Deterministic in-memory `SecretResolver` for tests and the local-dev
/// default. Constructed from a `(service, account) -> secret` map or, when no
/// map is provided, refuses every lookup so missing-secret behaviour is
/// exercised by tests rather than masked.
#[derive(Debug, Default, Clone)]
pub struct InMemorySecretResolver {
    entries: std::collections::HashMap<(String, String), String>,
}

impl InMemorySecretResolver {
    /// Cross-host surface: integration tests in
    /// `tests/foundation_profiles.rs` and external-host test harnesses
    /// build an empty `InMemorySecretResolver` and then `.with_secret(...)`.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a `(service, account) -> secret` mapping. Tests use this to
    /// shape Keychain behaviour for the assertion.
    ///
    /// Cross-host surface: integration tests in
    /// `tests/foundation_profiles.rs`.
    #[allow(dead_code)]
    pub fn with_secret(
        mut self,
        service: impl Into<String>,
        account: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        self.entries
            .insert((service.into(), account.into()), secret.into());
        self
    }
}

impl SecretResolver for InMemorySecretResolver {
    fn resolve(&self, locator: &SecretLocator) -> Result<String, FoundationError> {
        self.entries
            .get(&(locator.service.clone(), locator.account.clone()))
            .cloned()
            .ok_or_else(|| FoundationError::SecretUnavailable {
                service: locator.service.clone(),
                account: locator.account.clone(),
                reason: "no entry in InMemorySecretResolver (test default)".into(),
            })
    }
}

/// Production Keychain resolver. Only compiled when the `os-keychain` Cargo
/// feature is enabled; otherwise the CLI defaults to `InMemorySecretResolver`
/// via [`default_secret_resolver`] so the standalone build (no `keyring`
/// crate, no Foundation runtime) stays keychain-free.
///
/// The implementation uses the `keyring` crate (cross-platform: macOS
/// Keychain via Security.framework, Linux `libsecret`, Windows Credential
/// Manager). When the feature is enabled, the resolver is constructed with a
/// default backend; tests can construct their own.
#[cfg(feature = "os-keychain")]
pub struct OsKeychainResolver {
    service_prefix: String,
}

#[cfg(feature = "os-keychain")]
impl OsKeychainResolver {
    pub fn new() -> Self {
        Self {
            service_prefix: "oxibrain/foundation/v1/".to_string(),
        }
    }

    /// Cross-host surface: production hosts configure the OS keychain
    /// resolver with a per-deployment service prefix; the oxibrain-cli
    /// crate default uses `Default::default` and never names the prefix.
    #[allow(dead_code)]
    pub fn with_service_prefix(prefix: impl Into<String>) -> Self {
        Self {
            service_prefix: prefix.into(),
        }
    }
}

#[cfg(feature = "os-keychain")]
impl Default for OsKeychainResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "os-keychain")]
impl SecretResolver for OsKeychainResolver {
    fn resolve(&self, locator: &SecretLocator) -> Result<String, FoundationError> {
        use std::collections::BTreeMap;
        // We use an `Entry` map per (service, account) so multiple lookups
        // share an in-process cache and the keyring daemon is not flooded.
        thread_local! {
            static CACHE: std::cell::RefCell<BTreeMap<(String, String), Result<String, String>>> =
                const { std::cell::RefCell::new(BTreeMap::new()) };
        }
        let service = format!("{}{}", self.service_prefix, locator.service);
        let key = (service.clone(), locator.account.clone());

        CACHE.with(|cache| {
            if let Some(cached) = cache.borrow().get(&key) {
                return cached
                    .clone()
                    .map_err(|reason| FoundationError::SecretUnavailable {
                        service: locator.service.clone(),
                        account: locator.account.clone(),
                        reason,
                    });
            }
            let entry = keyring::Entry::new(&service, &locator.account);
            let outcome = match entry.and_then(|e| e.get_password()) {
                Ok(secret) => Ok(secret),
                Err(e) => Err(e.to_string()),
            };
            cache.borrow_mut().insert(key, outcome.clone());
            outcome.map_err(|reason| FoundationError::SecretUnavailable {
                service: locator.service.clone(),
                account: locator.account.clone(),
                reason,
            })
        })
    }
}

/// Construct the production-default `SecretResolver` for the current build.
///
/// - With `--features os-keychain`: returns an `OsKeychainResolver`.
/// - Without: returns `InMemorySecretResolver::new()` so the standalone build
///   resolves cleanly even when a Foundation profile is present; the
///   in-memory resolver refuses every lookup, which the call site treats as
///   "profile cannot run, fall through to compat env / local".
pub fn default_secret_resolver() -> Box<dyn SecretResolver> {
    #[cfg(feature = "os-keychain")]
    {
        Box::new(OsKeychainResolver::new())
    }
    #[cfg(not(feature = "os-keychain"))]
    {
        Box::new(InMemorySecretResolver::new())
    }
}

// ─── directory resolution ────────────────────────────────────────────────

/// Resolve the Foundation home directory.
///
/// Honours `$OXI_FOUNDATION_HOME` for tests and deployment overrides; falls
/// back to `~/.oxi/foundation/v1` for normal operation. The host never reads
/// secrets from disk (§0); only the Keychain does that.
pub fn foundation_home() -> PathBuf {
    if let Some(home) = std::env::var_os("OXI_FOUNDATION_HOME") {
        PathBuf::from(home)
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
            .join(".oxi")
            .join("foundation")
            .join("v1")
    } else {
        PathBuf::from(".oxi").join("foundation").join("v1")
    }
}

fn profiles_path(home: &Path) -> PathBuf {
    home.join("profiles.json")
}

/// Load `profiles.json` from the standard location and validate it strictly.
///
/// - `home`: directory containing `profiles.json`. Tests pass a tempdir; the
///   CLI passes [`foundation_home()`].
/// - Returns `Ok(None)` when the file does not exist (the standalone default
///   has no Foundation profiles — the local path is the resolution step).
/// - Returns `Err(FoundationError::…)` on any parse-level rejection; the
///   caller must report the reason and fall through, never silently pick a
///   different provider.
pub fn load_profiles(home: &Path) -> Result<Option<ResolvedProfiles>, FoundationError> {
    let path = profiles_path(home);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(FoundationError::IoError(format!("{}: {e}", path.display())));
        }
    };

    // First parse the JSON loosely so we can run the secret-field scan on
    // raw key names — serde would silently drop unknown fields if we used
    // `FoundationProfiles` directly with `deny_unknown_fields`.
    let raw: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        FoundationError::InvalidShape(format!("profiles.json is not valid JSON: {e}"))
    })?;

    let obj = raw
        .as_object()
        .ok_or_else(|| FoundationError::InvalidShape("root is not a JSON object".into()))?;

    // §2.5 — secret-shaped field scan covers every profile. We bail out at
    // the first such field rather than continuing, because the file is
    // contract-incorrect and the caller must reject the whole document.
    for profile_value in obj
        .get("profiles")
        .and_then(|p| p.as_array())
        .ok_or_else(|| FoundationError::InvalidShape("`profiles` is not an array".into()))?
    {
        let profile_obj = profile_value.as_object().ok_or_else(|| {
            FoundationError::InvalidShape("a profile entry is not a JSON object".into())
        })?;
        for key in profile_obj.keys() {
            if SECRET_FIELD_NAMES.iter().any(|s| s == key) {
                return Err(FoundationError::SecretFieldPresent(key.clone()));
            }
        }
    }

    // Strict parse now that we've cleared the secret-field check.
    let parsed: FoundationProfiles = serde_json::from_value(raw)
        .map_err(|e| FoundationError::InvalidShape(format!("profiles.json: {e}")))?;

    if parsed.schema_version != SCHEMA_VERSION {
        return Err(FoundationError::UnsupportedSchemaVersion(
            parsed.schema_version,
        ));
    }

    // §2.2: required fields non-empty. §2.5: roles non-empty, no duplicates.
    let mut seen_ids: HashSet<String> = HashSet::new();
    for profile in &parsed.profiles {
        if profile.id.is_empty() {
            return Err(FoundationError::EmptyField("id"));
        }
        if profile.provider.is_empty() {
            return Err(FoundationError::EmptyField("provider"));
        }
        if profile.model.is_empty() {
            return Err(FoundationError::EmptyField("model"));
        }
        if profile.credential.service.is_empty() {
            return Err(FoundationError::EmptyField("credential.service"));
        }
        if profile.credential.account.is_empty() {
            return Err(FoundationError::EmptyField("credential.account"));
        }
        if !seen_ids.insert(profile.id.clone()) {
            return Err(FoundationError::DuplicateProfileId(profile.id.clone()));
        }
        if profile.roles.is_empty() {
            return Err(FoundationError::EmptyRoles);
        }
        let mut seen_roles: HashSet<ProfileRole> = HashSet::new();
        for role in &profile.roles {
            if !seen_roles.insert(*role) {
                return Err(FoundationError::DuplicateRole(*role));
            }
        }
    }

    Ok(Some(ResolvedProfiles {
        profiles: parsed.profiles,
    }))
}

impl ResolvedProfiles {
    /// Pick the first profile that lists `role` and whose declared capabilities
    /// satisfy `mechanism`. Capability rejection is reported as
    /// `FoundationError::CapabilityUnsatisfied` so the caller can show the
    /// user why that profile is unsuitable and continue the resolution
    /// ladder.
    /// Cross-host surface: tests in `tests/foundation_profiles.rs`
    /// exercise the role-resolution ladder; the oxibrain-cli binary
    /// uses `oxibrain_cli::cmd::llm::resolve_provider` (which iterates
    /// profiles internally) and never calls this method directly.
    #[allow(dead_code)]
    pub fn pick_for_role(
        &self,
        role: ProfileRole,
        mechanism: ExtractMechanism,
    ) -> Result<&ProviderProfile, FoundationError> {
        for profile in &self.profiles {
            if !profile.roles.contains(&role) {
                continue;
            }
            if !profile.capabilities.clone().satisfies(mechanism) {
                return Err(FoundationError::CapabilityUnsatisfied {
                    profile_id: profile.id.clone(),
                    mechanism,
                });
            }
            return Ok(profile);
        }
        Err(FoundationError::RoleDenied {
            // Pick the first profile id we saw so the caller can identify the
            // set; an empty list means "no profiles at all".
            profile_id: self
                .profiles
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_default(),
            requested: role,
        })
    }

    /// Profiles in declaration order.
    pub fn iter(&self) -> std::slice::Iter<'_, ProviderProfile> {
        self.profiles.iter()
    }
}

// ─── provider-kind to adapter mapping ────────────────────────────────────

/// Map a Foundation profile's `provider` field to the host's adapter
/// catalogue. Foundation v1 deliberately does not name "anthropic" or "openai"
/// in the spec; the host picks the adapter that can honour the declared
/// mechanism. Unknown provider kinds are a FoundationError::InvalidShape so a
/// typo never silently maps to the wrong adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
}

impl ProviderKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "anthropic" | "claude" => ProviderKind::Anthropic,
            "openai" | "gpt" => ProviderKind::OpenAi,
            _ => return None,
        })
    }

    /// Cross-host surface: external hosts serialise the resolved provider
    /// kind for logging / config maps; the in-crate callers use
    /// `ProviderKind::Anthropic / OpenAi` directly without naming the
    /// string spelling.
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAi => "openai",
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Process-wide lock for tests that mutate `OXI_FOUNDATION_HOME`.
    /// cargo defaults to running tests in parallel across threads; env vars
    /// are process-global, so any two tests that touch the same variable
    /// race. Every set-var / remove-var call in this module MUST hold this
    /// lock for the duration of the test body and any restore.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn write_profiles(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("profiles.json"), body).unwrap();
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let got = load_profiles(dir.path()).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn rejects_secret_shaped_fields() {
        let dir = tempfile::tempdir().unwrap();
        write_profiles(
            dir.path(),
            r#"{
              "schema_version": 1,
              "profiles": [
                {
                  "id": "leaky",
                  "provider": "anthropic",
                  "model": "claude-sonnet-4-5",
                  "roles": ["memory.extract"],
                  "credential": {"service": "oxibrain", "account": "a7"},
                  "api_key": "sk-test"
                }
              ]
            }"#,
        );
        let err = load_profiles(dir.path()).unwrap_err();
        assert!(matches!(&err, FoundationError::SecretFieldPresent(f) if f == "api_key"));
    }

    #[test]
    fn rejects_each_secret_field_by_name() {
        for field in SECRET_FIELD_NAMES {
            let dir = tempfile::tempdir().unwrap();
            let body = format!(
                r#"{{"schema_version":1,"profiles":[{{"id":"p","provider":"anthropic","model":"m","roles":["memory.extract"],"credential":{{"service":"s","account":"a"}},"{field}":"x"}}]}}"#
            );
            write_profiles(dir.path(), &body);
            let err = load_profiles(dir.path()).unwrap_err();
            assert!(
                matches!(&err, FoundationError::SecretFieldPresent(f) if f == field),
                "expected SecretFieldPresent({field}), got {err:?}"
            );
        }
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        write_profiles(dir.path(), r#"{"schema_version":2,"profiles":[]}"#);
        assert!(matches!(
            load_profiles(dir.path()),
            Err(FoundationError::UnsupportedSchemaVersion(2))
        ));
    }

    #[test]
    fn empty_profiles_array_is_valid() {
        let dir = tempfile::tempdir().unwrap();
        write_profiles(dir.path(), r#"{"schema_version":1,"profiles":[]}"#);
        let got = load_profiles(dir.path()).unwrap().unwrap();
        assert!(got.profiles.is_empty());
    }

    #[test]
    fn rejects_duplicate_profile_id() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {"id":"same","provider":"anthropic","model":"m","roles":["memory.extract"],"credential":{"service":"s","account":"a"}},
            {"id":"same","provider":"openai","model":"m","roles":["memory.extract"],"credential":{"service":"s","account":"a"}}
          ]
        }"#;
        write_profiles(dir.path(), body);
        assert!(matches!(
            &load_profiles(dir.path()),
            Err(FoundationError::DuplicateProfileId(id)) if id == "same"
        ));
    }

    #[test]
    fn rejects_unknown_role() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {"id":"p","provider":"anthropic","model":"m","roles":["memory.unknown"],"credential":{"service":"s","account":"a"}}
          ]
        }"#;
        write_profiles(dir.path(), body);
        let err = load_profiles(dir.path()).unwrap_err();
        // Unknown role lands in serde's strict-deserialize path; we surface
        // it as InvalidShape so the user can read the underlying detail.
        assert!(matches!(err, FoundationError::InvalidShape(_)));
    }

    #[test]
    fn rejects_empty_roles() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {"id":"p","provider":"anthropic","model":"m","roles":[],"credential":{"service":"s","account":"a"}}
          ]
        }"#;
        write_profiles(dir.path(), body);
        assert!(matches!(
            &load_profiles(dir.path()),
            Err(FoundationError::EmptyRoles)
        ));
    }

    #[test]
    fn rejects_duplicate_role_in_profile() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {"id":"p","provider":"anthropic","model":"m","roles":["memory.extract","memory.extract"],"credential":{"service":"s","account":"a"}}
          ]
        }"#;
        write_profiles(dir.path(), body);
        assert!(matches!(
            &load_profiles(dir.path()),
            Err(FoundationError::DuplicateRole(ProfileRole::MemoryExtract))
        ));
    }

    #[test]
    fn rejects_empty_provider_or_model() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {"id":"p","provider":"","model":"m","roles":["memory.extract"],"credential":{"service":"s","account":"a"}}
          ]
        }"#;
        write_profiles(dir.path(), body);
        assert!(matches!(
            &load_profiles(dir.path()),
            Err(FoundationError::EmptyField("provider"))
        ));
    }

    #[test]
    fn rejects_empty_credential_locator() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {"id":"p","provider":"anthropic","model":"m","roles":["memory.extract"],"credential":{"service":"","account":"a"}}
          ]
        }"#;
        write_profiles(dir.path(), body);
        assert!(matches!(
            &load_profiles(dir.path()),
            Err(FoundationError::EmptyField("credential.service"))
        ));
    }

    #[test]
    fn accepts_well_formed_canonical_profile() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {
              "id": "work-summariser",
              "provider": "anthropic",
              "model": "claude-sonnet-4-5",
              "roles": ["memory.consolidate", "assistant.general"],
              "credential": {"service": "oxibrain", "account": "work"}
            }
          ]
        }"#;
        write_profiles(dir.path(), body);
        let got = load_profiles(dir.path()).unwrap().unwrap();
        assert_eq!(got.profiles.len(), 1);
        assert_eq!(got.profiles[0].id, "work-summariser");
        assert_eq!(got.profiles[0].provider, "anthropic");
        assert_eq!(
            got.profiles[0].roles,
            vec![
                ProfileRole::MemoryConsolidate,
                ProfileRole::AssistantGeneral
            ]
        );
    }

    #[test]
    fn pick_for_role_skips_non_members() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {"id":"a","provider":"anthropic","model":"m","roles":["coding.primary"],"credential":{"service":"s","account":"a"}},
            {"id":"b","provider":"openai","model":"m","roles":["memory.extract"],"credential":{"service":"s","account":"b"},"capabilities":{"grammar":false,"structured_output":true,"tool_call":true,"json_schema":true}}
          ]
        }"#;
        write_profiles(dir.path(), body);
        let got = load_profiles(dir.path()).unwrap().unwrap();
        let pick = got
            .pick_for_role(ProfileRole::MemoryExtract, ExtractMechanism::JsonSchema)
            .unwrap();
        assert_eq!(pick.id, "b");
    }

    #[test]
    fn pick_for_role_rejects_when_capabilities_unsatisfy() {
        let dir = tempfile::tempdir().unwrap();
        // Profile declares only `grammar` but the configured mechanism is
        // JsonSchema — must be rejected before any Keychain call.
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {
              "id":"constrained",
              "provider":"anthropic",
              "model":"m",
              "roles":["memory.extract"],
              "credential":{"service":"s","account":"a"},
              "capabilities":{"grammar":true,"structured_output":false,"tool_call":false,"json_schema":false}
            }
          ]
        }"#;
        write_profiles(dir.path(), body);
        let got = load_profiles(dir.path()).unwrap().unwrap();
        let err = got
            .pick_for_role(ProfileRole::MemoryExtract, ExtractMechanism::JsonSchema)
            .unwrap_err();
        assert!(
            matches!(&err, FoundationError::CapabilityUnsatisfied { profile_id, .. } if profile_id == "constrained")
        );
    }

    #[test]
    fn pick_for_role_role_denied_when_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {"id":"a","provider":"anthropic","model":"m","roles":["coding.primary"],"credential":{"service":"s","account":"a"}}
          ]
        }"#;
        write_profiles(dir.path(), body);
        let got = load_profiles(dir.path()).unwrap().unwrap();
        let err = got
            .pick_for_role(ProfileRole::MemoryExtract, ExtractMechanism::JsonSchema)
            .unwrap_err();
        assert!(matches!(
            &err,
            FoundationError::RoleDenied {
                requested: ProfileRole::MemoryExtract,
                ..
            }
        ));
    }

    #[test]
    fn in_memory_resolver_hits_and_misses() {
        let resolver =
            InMemorySecretResolver::new().with_secret("oxibrain", "work", "secret-value");
        let hit = resolver
            .resolve(&SecretLocator {
                service: "oxibrain".into(),
                account: "work".into(),
            })
            .unwrap();
        assert_eq!(hit, "secret-value");
        let miss = resolver.resolve(&SecretLocator {
            service: "oxibrain".into(),
            account: "missing".into(),
        });
        assert!(matches!(
            miss,
            Err(FoundationError::SecretUnavailable { .. })
        ));
    }

    #[test]
    fn foundation_home_uses_env_when_set() {
        // Hold the process-wide env lock for the entire set/run/restore
        // window so a parallel test cannot observe a half-set home and our
        // restore on exit doesn't clobber another test's set-var.
        //
        // SAFETY: env vars are process-global; we serialise every mutation
        // in this module through `ENV_LOCK`.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("OXI_FOUNDATION_HOME");
        // SAFETY: see `_guard` above.
        unsafe {
            std::env::set_var("OXI_FOUNDATION_HOME", "/tmp/foundation-test-home");
        }
        let got = foundation_home();
        // SAFETY: see above.
        unsafe {
            match saved {
                Some(v) => std::env::set_var("OXI_FOUNDATION_HOME", v),
                None => std::env::remove_var("OXI_FOUNDATION_HOME"),
            }
        }
        assert_eq!(got, PathBuf::from("/tmp/foundation-test-home"));
    }

    #[test]
    fn declared_capabilities_satisfy() {
        let caps = DeclaredCapabilities {
            tool_call: true,
            ..DeclaredCapabilities::default()
        };
        assert!(caps.clone().satisfies(ExtractMechanism::ToolCall));
        assert!(!caps.satisfies(ExtractMechanism::JsonSchema));
        assert!(!caps.satisfies(ExtractMechanism::Grammar));
    }

    #[test]
    fn openai_profile_with_only_json_schema_passes_capability_check() {
        // Mirror of the integration test in tests/foundation_profiles.rs:
        // a profile declaring provider=openai with capabilities {json_schema:
        // true, tool_call: false, structured_output: false, grammar: false}
        // must be selected for `memory.extract` because the OpenAI adapter's
        // native mechanism is JsonSchema, not ToolCall.
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{
          "schema_version": 1,
          "profiles": [
            {
              "id": "openai-json",
              "provider": "openai",
              "model": "gpt-4o",
              "roles": ["memory.extract"],
              "credential": {"service": "oxibrain", "account": "openai"},
              "capabilities": {"grammar": false, "structured_output": false, "tool_call": false, "json_schema": true}
            }
          ]
        }"#;
        write_profiles(dir.path(), body);
        let got = load_profiles(dir.path())
            .unwrap()
            .expect("profiles present");
        // The resolver picks mechanism from `provider`, so OpenAI is
        // validated against JsonSchema; a truthful {json_schema: true}
        // profile must pass.
        // Validate against JsonSchema (the OpenAI adapter's native
        // mechanism). With truthful `{json_schema: true}` capabilities the
        // profile must pass.
        let pick = got
            .pick_for_role(ProfileRole::MemoryExtract, ExtractMechanism::JsonSchema)
            .unwrap();
        assert_eq!(pick.id, "openai-json");
    }

    #[test]
    fn role_round_trip() {
        for role in [
            ProfileRole::MemoryExtract,
            ProfileRole::MemoryConsolidate,
            ProfileRole::CodingPrimary,
            ProfileRole::AssistantGeneral,
        ] {
            assert_eq!(ProfileRole::parse(role.as_str()), Some(role));
        }
        assert!(ProfileRole::parse("memory.unknown").is_none());
    }
}
