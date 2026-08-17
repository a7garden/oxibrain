//! Oxi Foundation v1 — `packages.lock` reader (spec §3).
//!
//! Typed, read-only helpers for inspecting the resolved Foundation packages a
//! host has selected. This module **does not** execute, install, or otherwise
//! act on package payloads — `oxibrain` is not a package runtime. It only
//! parses the on-disk `packages.lock` and lets a host query which packages
//! apply to a given `target`.
//!
//! The reader enforces the schema-level rejection rules in spec §3:
//!
//! - `schema_version` must equal `1`. Any other value rejects the whole file.
//! - `digest` must be exactly `sha256-` followed by 64 lowercase hex chars.
//! - `trust` must be one of `verified`, `pinned`, `untrusted`.
//! - All required fields must be present and well-typed.
//!
//! Unknown abstract requirement strings are **preserved**, not silently
//! dropped, so a host can decide policy. The reader carries them as
//! [`AbstractRequirement::Unknown`]; the host's resolver can then reject,
//! ignore, or escalate to its own approval flow. This is the rule Task 4
//! requires: a parse must never silently drop a requirement a package
//! declared.
//!
//! Capability rule (spec §3.3, §3.4): a package declares *abstract*
//! requirements. The reader does not grant them. In particular, a package
//! that lists `brain.query` does **not** receive any scope — it only signals
//! that the host *might* want to map that to a read-only `BrainClient`
//! surface. `brain.ingest`, `brain.declare`, `brain.retract`, and
//! `brain.redact` are not package-grantable at all. The reader exposes them
//! only as the opaque string the package wrote; it never interprets any
//! declaration as authority.
//!
//! Authority and read-only-ness:
//!
//! - The reader never writes to the lockfile.
//! - The reader never opens the brain socket.
//! - The reader never modifies the daemon's `Scope`.
//! - `select_package_for_target` is a pure function over the parsed lock.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

// ─── schema-version literal (doc/spec/oxi-foundation-v1.md §3.2) ───────────

/// The only `schema_version` this reader accepts. Any other value rejects
/// the whole `packages.lock` at parse time — the reader does not silently
/// coerce.
pub const SCHEMA_VERSION: u32 = 1;

/// Environment variable that overrides the Foundation home directory for tests
/// and deployment overrides (mirror of `OXI_FOUNDATION_HOME` used elsewhere).
/// When unset, the reader defaults to `$HOME/.oxi/foundation/v1/`.
pub const OXI_FOUNDATION_HOME_ENV: &str = "OXI_FOUNDATION_HOME";

/// Closed set of legal abstract requirement strings per spec §3.3.
pub const ALLOWED_REQUIREMENTS: &[&str] = &[
    "workspace.read",
    "workspace.patch",
    "shell.execute",
    "browser.navigate",
    "brain.query",
    "schedule.manage",
];

/// Closed set of legal `trust` states per spec §3.4.
pub const ALLOWED_TRUST_STATES: &[&str] = &["verified", "pinned", "untrusted"];

/// Expected prefix for a package `digest` per spec §3.2 / §3.4.
pub const DIGEST_PREFIX: &str = "sha256-";
/// Number of lowercase hex characters after the `sha256-` prefix.
pub const DIGEST_HEX_LEN: usize = 64;

// ─── typed parsed schema ──────────────────────────────────────────────────

/// Trust state declared by a package (spec §3.4).
///
/// The reader carries the package's declared trust verbatim so the host's
/// policy code can decide how to honour it. The mapping rules live in
/// `doc/spec/oxi-foundation-v1.md` §3.4 and `doc/spec/oxi-foundation-v1.md`
/// §7 — the reader itself does not enforce them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    /// Host has confirmed the digest against a published signature.
    Verified,
    /// Host has pinned the digest from a trusted channel but has not
    /// re-verified it on this run.
    Pinned,
    /// Host has not verified the digest.
    Untrusted,
}

impl TrustState {
    /// Parse a `trust` string into a typed value. Unknown values are a parse
    /// rejection (spec §3.2).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "verified" => Some(Self::Verified),
            "pinned" => Some(Self::Pinned),
            "untrusted" => Some(Self::Untrusted),
            _ => None,
        }
    }

    /// Returns `true` when the trust state may be used for read-only flows
    /// (spec §3.4: `verified` and `pinned` are read-only-safe; `untrusted`
    /// is not).
    pub fn allows_reads(self) -> bool {
        matches!(self, Self::Verified | Self::Pinned)
    }

    /// Returns `true` when the trust state may be used for write-bearing
    /// flows (spec §3.4: only `verified`).
    pub fn allows_writes(self) -> bool {
        matches!(self, Self::Verified)
    }
}

impl fmt::Display for TrustState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Verified => "verified",
            Self::Pinned => "pinned",
            Self::Untrusted => "untrusted",
        };
        f.write_str(s)
    }
}

/// Abstract requirement declared by a package (spec §3.3).
///
/// The closed set is the six values in [`ALLOWED_REQUIREMENTS`]. Anything
/// else is preserved as [`Self::Unknown`] so the host can decide policy
/// rather than having the reader silently drop a package's declaration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum AbstractRequirement {
    /// Read access to a host-defined workspace root.
    WorkspaceRead,
    /// Apply patches the host has approved through its own approval flow.
    WorkspacePatch,
    /// Sandboxed shell, only after host policy has approved it.
    ShellExecute,
    /// Host-mediated browser tab, not raw network.
    BrowserNavigate,
    /// `BrainClient` connection to the local daemon (default socket).
    /// Maps **only** to existing scoped Brain *read* operations — it does
    /// not grant `brain.ingest`, `brain.declare`, `brain.retract`, or
    /// `brain.redact`. Those are distinct privileged capabilities no
    /// package can grant by declaration.
    BrainQuery,
    /// Host's own scheduler; never a Foundation-side one.
    ScheduleManage,
    /// A requirement string the reader does not recognise. Preserved so
    /// the host's policy code can decide how to handle it; the reader
    /// itself never silently drops it.
    Unknown(String),
}

impl AbstractRequirement {
    /// Parse a requirement string into a typed value. Unknown values are
    /// returned as [`Self::Unknown`].
    pub fn parse(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        match raw.as_str() {
            "workspace.read" => Self::WorkspaceRead,
            "workspace.patch" => Self::WorkspacePatch,
            "shell.execute" => Self::ShellExecute,
            "browser.navigate" => Self::BrowserNavigate,
            "brain.query" => Self::BrainQuery,
            "schedule.manage" => Self::ScheduleManage,
            _ => Self::Unknown(raw),
        }
    }

    /// The raw string the package declared. For the closed-set variants
    /// this is the canonical spec spelling; for [`Self::Unknown`] this is
    /// the package's own string preserved verbatim.
    pub fn as_str(&self) -> &str {
        match self {
            Self::WorkspaceRead => "workspace.read",
            Self::WorkspacePatch => "workspace.patch",
            Self::ShellExecute => "shell.execute",
            Self::BrowserNavigate => "browser.navigate",
            Self::BrainQuery => "brain.query",
            Self::ScheduleManage => "schedule.manage",
            Self::Unknown(s) => s.as_str(),
        }
    }

    /// `true` when the requirement is one of the closed set in spec §3.3.
    /// `false` for [`Self::Unknown`].
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

impl fmt::Display for AbstractRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<AbstractRequirement> for String {
    fn from(r: AbstractRequirement) -> Self {
        let owned = match &r {
            AbstractRequirement::Unknown(s) => s.clone(),
            _ => r.as_str().to_owned(),
        };
        owned
    }
}

impl TryFrom<String> for AbstractRequirement {
    type Error = std::convert::Infallible;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Ok(Self::parse(s))
    }
}

/// One resolved Foundation package (spec §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundationPackage {
    /// Package name (spec §3.2).
    pub name: String,
    /// SemVer string.
    pub version: String,
    /// Content digest — `sha256-` + 64 lowercase hex characters.
    pub digest: String,
    /// Origin identifier (e.g. `foundation`, a registry URL).
    pub source: String,
    /// Trust state (see spec §3.4).
    pub trust: TrustState,
    /// Hosts the package applies to. `None` means "all targets".
    pub targets: Option<Vec<String>>,
    /// Abstract requirements the package declares.
    pub requirements: Vec<AbstractRequirement>,
}

impl FoundationPackage {
    /// Returns `true` when this package applies to `target`.
    ///
    /// Absent `targets` means "all". Otherwise membership of `target` in
    /// `targets` is required.
    pub fn applies_to(&self, target: &str) -> bool {
        match &self.targets {
            None => true,
            Some(list) => list.iter().any(|t| t == target),
        }
    }
}

/// The full `packages.lock` document (spec §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagesLock {
    /// Schema version — must equal [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Resolved packages.
    pub packages: Vec<FoundationPackage>,
}

// ─── manifest reader ─────────────────────────────────────────────────────
//
// A package manifest is a separate, host-local file describing the contents
// of a single Foundation package — its name/version/digest, optional
// targets, optional persona metadata, prompt payload locations, and
// abstract `requires`. It is distinct from `packages.lock`, which only
// records the resolution result. The contract line that fixes the
// manifest shape is in the global plan ("A package manifest has `name`,
// `version`, `digest`, optional `targets`, optional `persona`, prompt
// payload locations, and abstract `requires`").
//
// The manifest reader applies the same rejection rules as the lock reader
// (spec §3.2/§3.3/§3.4) because both files describe the same package.

/// Where a package's prompt payload lives. Manifest-only — the lockfile
/// does not name payload locations.
///
/// Either an `inline` string (the payload is the string value) or a
/// relative `path` inside the package's resolved directory. The host is
/// responsible for opening the path; the reader only validates the shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PayloadLocation {
    /// Inline string payload — the value is the prompt text itself.
    Inline { value: String },
    /// Relative path inside the package's resolved directory.
    Path { value: String },
}

impl PayloadLocation {
    /// Returns `true` when the location is a `path` reference with a
    /// non-empty relative path.
    pub fn is_path(&self) -> bool {
        matches!(self, Self::Path { value } if !value.is_empty())
    }
}

/// Persona metadata for a package (manifest-only). Persona describes the
/// voice/role the package wants the model to adopt and is metadata only —
/// it grants no authority. Hosts may use it to label prompts; they must
/// not treat it as a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagePersona {
    /// Short persona name (e.g. `"senior-reviewer"`).
    pub name: String,
    /// Optional free-form description of the persona.
    #[serde(default)]
    pub description: Option<String>,
}

/// One package manifest. Same shape as a `FoundationPackage` plus optional
/// `persona` and `payloads`.
///
/// `requires` is the manifest's spelling of what the lockfile calls
/// `requirements`. Both spellings appear in the contract — the manifest
/// is the source of truth, and the lockfile is the resolved view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageManifest {
    /// Package name.
    pub name: String,
    /// SemVer string.
    pub version: String,
    /// Content digest — `sha256-` + 64 lowercase hex characters.
    pub digest: String,
    /// Hosts the package applies to. `None` means "all targets".
    #[serde(default)]
    pub targets: Option<Vec<String>>,
    /// Optional persona (manifest-only; not present on lock entries).
    #[serde(default)]
    pub persona: Option<PackagePersona>,
    /// Prompt payload locations (manifest-only; not present on lock
    /// entries).
    #[serde(default)]
    pub payloads: Vec<PayloadLocation>,
    /// Abstract capabilities the package declares. Preserved verbatim
    /// (same rule as lockfile `requirements`).
    #[serde(default)]
    pub requires: Vec<AbstractRequirement>,
}

impl PackageManifest {
    /// Returns `true` when this manifest applies to `target`. Same rule
    /// as `FoundationPackage::applies_to`.
    pub fn applies_to(&self, target: &str) -> bool {
        match &self.targets {
            None => true,
            Some(list) => list.iter().any(|t| t == target),
        }
    }

    /// Confirm this manifest and a lock entry describe the same package.
    /// Spec §3.4: "The host verifies the digest against the package's
    /// source before loading it. A mismatch is a hard error."
    ///
    /// Only `name` and `digest` are compared — the lockfile is the
    /// resolved view (it adds `source`/`trust`), so other fields are
    /// allowed to differ by construction.
    pub fn matches_lock_entry(&self, entry: &FoundationPackage) -> Result<(), PackageError> {
        if self.name != entry.name {
            return Err(PackageError::IdentityMismatch {
                manifest_name: self.name.clone(),
                lock_name: entry.name.clone(),
            });
        }
        if self.digest != entry.digest {
            return Err(PackageError::DigestMismatch {
                name: self.name.clone(),
                manifest_digest: self.digest.clone(),
                lock_digest: entry.digest.clone(),
            });
        }
        Ok(())
    }
}

// ─── typed errors ─────────────────────────────────────────────────────────

/// Why a `packages.lock` was rejected at the parse boundary. Spec §3.2 / §3.4
/// list the rejection rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    /// `schema_version` is not `1`.
    UnsupportedSchemaVersion { found: u32 },
    /// JSON does not parse.
    InvalidJson { reason: String },
    /// A required field is missing or has the wrong JSON type.
    InvalidShape { reason: String },
    /// A duplicate package name appeared in `packages`. Spec does not
    /// require uniqueness but a lockfile with duplicate names is almost
    /// certainly a host-side bug; we reject to surface it.
    DuplicatePackageName { name: String },
    /// `digest` is not `sha256-` + 64 lowercase hex.
    InvalidDigest { name: String, digest: String },
    /// `trust` is not one of `verified`, `pinned`, `untrusted`.
    InvalidTrustState { name: String, trust: String },
    /// I/O error reading the lockfile.
    Io { path: PathBuf, reason: String },
    /// File does not exist (returned separately so callers can map it to
    /// their own "no Foundation packages" path).
    NotFound { path: PathBuf },
    /// Spec §3.4: a package's manifest digest does not match its lock
    /// entry's digest. The host must hard-error before loading.
    DigestMismatch {
        name: String,
        manifest_digest: String,
        lock_digest: String,
    },
    /// The manifest names one package and the lock entry names another.
    IdentityMismatch {
        manifest_name: String,
        lock_name: String,
    },
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { found } => {
                write!(
                    f,
                    "packages.lock schema_version {found} is not supported (expected {})",
                    SCHEMA_VERSION
                )
            }
            Self::InvalidJson { reason } => {
                write!(f, "packages.lock is not valid JSON: {reason}")
            }
            Self::InvalidShape { reason } => {
                write!(f, "packages.lock has invalid shape: {reason}")
            }
            Self::DuplicatePackageName { name } => {
                write!(f, "packages.lock contains duplicate package name {name:?}")
            }
            Self::InvalidDigest { name, digest } => write!(
                f,
                "packages.lock package {name:?} has invalid digest {digest:?} \
                 (expected \"sha256-\" + 64 lowercase hex)"
            ),
            Self::InvalidTrustState { name, trust } => write!(
                f,
                "packages.lock package {name:?} has unknown trust state {trust:?} \
                 (expected one of verified, pinned, untrusted)"
            ),
            Self::Io { path, reason } => {
                write!(f, "I/O error reading {}: {reason}", path.display())
            }
            Self::NotFound { path } => {
                write!(f, "packages.lock not found at {}", path.display())
            }
            Self::DigestMismatch {
                name,
                manifest_digest,
                lock_digest,
            } => write!(
                f,
                "digest mismatch for package {name:?}: \
                 manifest says {manifest_digest:?}, lockfile says {lock_digest:?} \
                 (spec §3.4 hard error)"
            ),
            Self::IdentityMismatch {
                manifest_name,
                lock_name,
            } => write!(
                f,
                "manifest package name {manifest_name:?} does not match \
                 lockfile entry name {lock_name:?}"
            ),
        }
    }
}

impl std::error::Error for PackageError {}

// ─── helpers ──────────────────────────────────────────────────────────────

/// Validate that `digest` matches the spec §3.4 format.
fn validate_digest(name: &str, digest: &str) -> Result<(), PackageError> {
    if digest.len() != DIGEST_PREFIX.len() + DIGEST_HEX_LEN {
        return Err(PackageError::InvalidDigest {
            name: name.to_owned(),
            digest: digest.to_owned(),
        });
    }
    let (prefix, hex) = digest.split_at(DIGEST_PREFIX.len());
    if prefix != DIGEST_PREFIX {
        return Err(PackageError::InvalidDigest {
            name: name.to_owned(),
            digest: digest.to_owned(),
        });
    }
    if !hex
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(PackageError::InvalidDigest {
            name: name.to_owned(),
            digest: digest.to_owned(),
        });
    }
    Ok(())
}

/// Parse a `PackagesLock` from a raw JSON string.
pub fn parse_packages_lock(json: &str) -> Result<PackagesLock, PackageError> {
    let raw: serde_json::Value =
        serde_json::from_str(json).map_err(|e| PackageError::InvalidJson {
            reason: e.to_string(),
        })?;

    let obj = raw.as_object().ok_or_else(|| PackageError::InvalidShape {
        reason: "top-level value must be an object".to_owned(),
    })?;

    let schema_version = obj
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| PackageError::InvalidShape {
            reason: "schema_version is required and must be an integer".to_owned(),
        })?;
    if schema_version != u64::from(SCHEMA_VERSION) {
        return Err(PackageError::UnsupportedSchemaVersion {
            found: schema_version as u32,
        });
    }

    let packages_value = obj
        .get("packages")
        .ok_or_else(|| PackageError::InvalidShape {
            reason: "packages is required".to_owned(),
        })?;
    let packages_array = packages_value
        .as_array()
        .ok_or_else(|| PackageError::InvalidShape {
            reason: "packages must be an array".to_owned(),
        })?;

    let mut packages = Vec::with_capacity(packages_array.len());
    let mut seen_names: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(packages_array.len());

    for (idx, pkg_value) in packages_array.iter().enumerate() {
        let pkg = pkg_value
            .as_object()
            .ok_or_else(|| PackageError::InvalidShape {
                reason: format!("packages[{idx}] must be an object"),
            })?;

        let name = pkg
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PackageError::InvalidShape {
                reason: format!("packages[{idx}].name is required and must be a string"),
            })?
            .to_owned();
        if name.is_empty() {
            return Err(PackageError::InvalidShape {
                reason: format!("packages[{idx}].name must not be empty"),
            });
        }
        if !seen_names.insert(name.clone()) {
            return Err(PackageError::DuplicatePackageName { name });
        }

        let version = pkg
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PackageError::InvalidShape {
                reason: format!("packages[{idx}].version is required and must be a string"),
            })?
            .to_owned();
        if version.is_empty() {
            return Err(PackageError::InvalidShape {
                reason: format!("packages[{idx}].version must not be empty"),
            });
        }

        let digest = pkg
            .get("digest")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PackageError::InvalidShape {
                reason: format!("packages[{idx}].digest is required and must be a string"),
            })?
            .to_owned();
        validate_digest(&name, &digest)?;

        let source = pkg
            .get("source")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PackageError::InvalidShape {
                reason: format!("packages[{idx}].source is required and must be a string"),
            })?
            .to_owned();

        let trust_raw = pkg
            .get("trust")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PackageError::InvalidShape {
                reason: format!("packages[{idx}].trust is required and must be a string"),
            })?
            .to_owned();
        let trust =
            TrustState::parse(&trust_raw).ok_or_else(|| PackageError::InvalidTrustState {
                name: name.clone(),
                trust: trust_raw,
            })?;

        let targets = match pkg.get("targets") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => {
                let arr = v.as_array().ok_or_else(|| PackageError::InvalidShape {
                    reason: format!("packages[{idx}].targets must be an array of strings"),
                })?;
                let mut list = Vec::with_capacity(arr.len());
                for t in arr {
                    let s = t.as_str().ok_or_else(|| PackageError::InvalidShape {
                        reason: format!("packages[{idx}].targets entries must be strings"),
                    })?;
                    list.push(s.to_owned());
                }
                Some(list)
            }
        };

        let reqs_value = pkg
            .get("requirements")
            .ok_or_else(|| PackageError::InvalidShape {
                reason: format!("packages[{idx}].requirements is required"),
            })?;
        let reqs_array = reqs_value
            .as_array()
            .ok_or_else(|| PackageError::InvalidShape {
                reason: format!("packages[{idx}].requirements must be an array of strings"),
            })?;
        let mut requirements = Vec::with_capacity(reqs_array.len());
        for (r_idx, r) in reqs_array.iter().enumerate() {
            let s = r.as_str().ok_or_else(|| PackageError::InvalidShape {
                reason: format!("packages[{idx}].requirements[{r_idx}] must be a string"),
            })?;
            // Preserve unknown requirement strings rather than silently
            // dropping them. The host's policy decides how to honour them.
            requirements.push(AbstractRequirement::parse(s));
        }

        packages.push(FoundationPackage {
            name,
            version,
            digest,
            source,
            trust,
            targets,
            requirements,
        });
    }

    Ok(PackagesLock {
        schema_version: SCHEMA_VERSION,
        packages,
    })
}

/// Resolve the Foundation home directory for this reader.
///
/// Honours `$OXI_FOUNDATION_HOME` for tests and deployment overrides; falls
/// back to `$HOME/.oxi/foundation/v1`. The reader never reads secrets from
/// disk (spec §0); only the lockfile is consulted here.
pub fn foundation_home() -> PathBuf {
    if let Some(p) = std::env::var_os(OXI_FOUNDATION_HOME_ENV) {
        let buf = PathBuf::from(p);
        if !buf.as_os_str().is_empty() {
            return buf;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut buf = PathBuf::from(home);
        buf.push(".oxi");
        buf.push("foundation");
        buf.push("v1");
        return buf;
    }
    PathBuf::from(".oxi/foundation/v1")
}

/// Path of `packages.lock` inside `home`.
fn packages_path(home: &Path) -> PathBuf {
    home.join("packages.lock")
}

/// Load `packages.lock` from `home` and validate it strictly.
///
/// - `home`: directory containing `packages.lock`. Tests pass a tempdir;
///   callers normally pass [`foundation_home()`].
/// - Returns `Ok(None)` when the file does not exist (no Foundation
///   packages resolved — the standalone default continues to work).
/// - Returns `Err(PackageError::…)` on any parse-level rejection; the
///   caller must surface the reason and not silently pick a different
///   resolution path.
pub fn load_packages_lock(home: &Path) -> Result<Option<PackagesLock>, PackageError> {
    let path = packages_path(home);
    let raw = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(e) => {
            return Err(PackageError::Io {
                path: path.clone(),
                reason: e.to_string(),
            });
        }
    };
    let json = std::str::from_utf8(&raw).map_err(|e| PackageError::Io {
        path: path.clone(),
        reason: format!("packages.lock is not valid UTF-8: {e}"),
    })?;
    parse_packages_lock(json).map(Some)
}

/// Look up the first package in `lock` whose `targets` includes `target`.
///
/// Absent `targets` means "all targets", so a package with no `targets`
/// field matches every call. If no package matches, `None` is returned —
/// the caller decides what that means for its policy.
///
/// This is a pure, read-only helper. It does not write to the lockfile,
/// does not open the brain socket, and does not grant any capability. The
/// host uses it *after* it has independently decided which `target` it is
/// running as.
pub fn select_package_for_target<'a>(
    lock: &'a PackagesLock,
    target: &str,
) -> Option<&'a FoundationPackage> {
    lock.packages.iter().find(|p| p.applies_to(target))
}

/// Parse a single `PackageManifest` from JSON.
///
/// The manifest has no top-level `schema_version` (it is per-package, not
/// per-collection), so the strict-rejection rule for unknown schema is a
/// no-op here. The package body's `digest` is still validated against the
/// §3.4 format, and abstract requirement strings are still preserved
/// verbatim (closed-set variants for known values,
/// `AbstractRequirement::Unknown` for everything else).
pub fn parse_package_manifest(json: &str) -> Result<PackageManifest, PackageError> {
    let raw: serde_json::Value =
        serde_json::from_str(json).map_err(|e| PackageError::InvalidJson {
            reason: e.to_string(),
        })?;
    let obj = raw.as_object().ok_or_else(|| PackageError::InvalidShape {
        reason: "manifest must be a JSON object".to_owned(),
    })?;

    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| PackageError::InvalidShape {
            reason: "manifest.name is required and must be a string".to_owned(),
        })?
        .to_owned();
    if name.is_empty() {
        return Err(PackageError::InvalidShape {
            reason: "manifest.name must not be empty".to_owned(),
        });
    }

    let version = obj
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| PackageError::InvalidShape {
            reason: "manifest.version is required and must be a string".to_owned(),
        })?
        .to_owned();
    if version.is_empty() {
        return Err(PackageError::InvalidShape {
            reason: "manifest.version must not be empty".to_owned(),
        });
    }

    let digest = obj
        .get("digest")
        .and_then(|v| v.as_str())
        .ok_or_else(|| PackageError::InvalidShape {
            reason: "manifest.digest is required and must be a string".to_owned(),
        })?
        .to_owned();
    validate_digest(&name, &digest)?;

    let targets = match obj.get("targets") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => {
            let arr = v.as_array().ok_or_else(|| PackageError::InvalidShape {
                reason: "manifest.targets must be an array of strings".to_owned(),
            })?;
            let mut list = Vec::with_capacity(arr.len());
            for t in arr {
                let s = t.as_str().ok_or_else(|| PackageError::InvalidShape {
                    reason: "manifest.targets entries must be strings".to_owned(),
                })?;
                list.push(s.to_owned());
            }
            Some(list)
        }
    };

    let persona = match obj.get("persona") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => {
            let p = v.as_object().ok_or_else(|| PackageError::InvalidShape {
                reason: "manifest.persona must be an object".to_owned(),
            })?;
            let pname = p
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| PackageError::InvalidShape {
                    reason: "manifest.persona.name is required and must be a string".to_owned(),
                })?
                .to_owned();
            if pname.is_empty() {
                return Err(PackageError::InvalidShape {
                    reason: "manifest.persona.name must not be empty".to_owned(),
                });
            }
            let description = p
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned());
            Some(PackagePersona {
                name: pname,
                description,
            })
        }
    };

    let payloads = match obj.get("payloads") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(v) => {
            let arr = v.as_array().ok_or_else(|| PackageError::InvalidShape {
                reason: "manifest.payloads must be an array".to_owned(),
            })?;
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let item_obj = item.as_object().ok_or_else(|| PackageError::InvalidShape {
                    reason: format!("manifest.payloads[{i}] must be an object"),
                })?;
                let kind = item_obj
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| PackageError::InvalidShape {
                        reason: format!(
                            "manifest.payloads[{i}].kind is required and must be \"inline\" or \"path\""
                        ),
                    })?;
                let value = item_obj
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| PackageError::InvalidShape {
                        reason: format!(
                            "manifest.payloads[{i}].value is required and must be a string"
                        ),
                    })?
                    .to_owned();
                match kind {
                    "inline" => out.push(PayloadLocation::Inline { value }),
                    "path" => {
                        if value.is_empty() {
                            return Err(PackageError::InvalidShape {
                                reason: format!("manifest.payloads[{i}] path must not be empty"),
                            });
                        }
                        out.push(PayloadLocation::Path { value });
                    }
                    other => {
                        return Err(PackageError::InvalidShape {
                            reason: format!(
                                "manifest.payloads[{i}].kind {other:?} is not \"inline\" or \"path\""
                            ),
                        });
                    }
                }
            }
            out
        }
    };

    let requires = match obj.get("requires") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(v) => {
            let arr = v.as_array().ok_or_else(|| PackageError::InvalidShape {
                reason: "manifest.requires must be an array of strings".to_owned(),
            })?;
            let mut out = Vec::with_capacity(arr.len());
            for (i, r) in arr.iter().enumerate() {
                let s = r.as_str().ok_or_else(|| PackageError::InvalidShape {
                    reason: format!("manifest.requires[{i}] must be a string"),
                })?;
                // Preserve unknown requirement strings verbatim (spec §3.7).
                out.push(AbstractRequirement::parse(s));
            }
            out
        }
    };

    Ok(PackageManifest {
        name,
        version,
        digest,
        targets,
        persona,
        payloads,
        requires,
    })
}

/// Path of a per-package manifest inside `home`. The convention is
/// `<home>/manifests/<name>.json`. Hosts may choose differently in their
/// own layout; this reader only follows the on-disk convention the
/// client ships with.
pub fn manifest_path(home: &Path, name: &str) -> PathBuf {
    // The package name may contain a `/` (e.g. `@oxi/code-review`); treat
    // it as a single flat filename so hosts can write it with a single
    // `fs::write`. The reader does not interpret the slash.
    let safe = name.replace('/', "__");
    home.join("manifests").join(format!("{safe}.json"))
}

/// Load a single `PackageManifest` from `home/manifests/<name>.json`.
///
/// - `Ok(None)` when the file does not exist (no manifest for that name).
/// - `Err(PackageError::…)` on parse failure or I/O error.
pub fn load_package_manifest(
    home: &Path,
    name: &str,
) -> Result<Option<PackageManifest>, PackageError> {
    let path = manifest_path(home, name);
    let raw = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(PackageError::Io {
                path: path.clone(),
                reason: e.to_string(),
            });
        }
    };
    let json = std::str::from_utf8(&raw).map_err(|e| PackageError::Io {
        path: path.clone(),
        reason: format!("manifest is not valid UTF-8: {e}"),
    })?;
    parse_package_manifest(json).map(Some)
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_DIGEST: &str =
        "sha256-0000000000000000000000000000000000000000000000000000000000000000";

    fn minimal_lock() -> &'static str {
        r#"{
          "schema_version": 1,
          "packages": [
            {
              "name": "@oxi/code-review",
              "version": "1.4.0",
              "digest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
              "source": "foundation",
              "trust": "verified",
              "targets": ["oxicode"],
              "requirements": ["workspace.read", "workspace.patch", "brain.query"]
            }
          ]
        }"#
    }

    #[test]
    fn parses_minimal_lock() {
        let lock = parse_packages_lock(minimal_lock()).unwrap();
        assert_eq!(lock.schema_version, 1);
        assert_eq!(lock.packages.len(), 1);
        let pkg = &lock.packages[0];
        assert_eq!(pkg.name, "@oxi/code-review");
        assert_eq!(pkg.version, "1.4.0");
        assert_eq!(pkg.digest, VALID_DIGEST);
        assert_eq!(pkg.source, "foundation");
        assert_eq!(pkg.trust, TrustState::Verified);
        assert_eq!(
            pkg.targets.as_deref(),
            Some(["oxicode".to_owned()].as_slice())
        );
        assert_eq!(pkg.requirements.len(), 3);
        assert!(
            pkg.requirements
                .iter()
                .any(|r| r == &AbstractRequirement::BrainQuery)
        );
    }

    #[test]
    fn rejects_unsupported_schema_version_before_any_parse() {
        let json = r#"{ "schema_version": 2, "packages": [] }"#;
        match parse_packages_lock(json) {
            Err(PackageError::UnsupportedSchemaVersion { found }) => {
                assert_eq!(found, 2);
            }
            other => panic!("expected UnsupportedSchemaVersion, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_digest_prefix() {
        let json = r#"{
          "schema_version": 1,
          "packages": [
            {
              "name": "@oxi/broken",
              "version": "1.0.0",
              "digest": "sha1-deadbeef",
              "source": "foundation",
              "trust": "verified",
              "requirements": []
            }
          ]
        }"#;
        match parse_packages_lock(json) {
            Err(PackageError::InvalidDigest { name, digest }) => {
                assert_eq!(name, "@oxi/broken");
                assert_eq!(digest, "sha1-deadbeef");
            }
            other => panic!("expected InvalidDigest, got {other:?}"),
        }
    }

    #[test]
    fn rejects_uppercase_hex_in_digest() {
        let json = r#"{
          "schema_version": 1,
          "packages": [
            {
              "name": "@oxi/upper",
              "version": "1.0.0",
              "digest": "sha256-AAAA000000000000000000000000000000000000000000000000000000000000",
              "source": "foundation",
              "trust": "verified",
              "requirements": []
            }
          ]
        }"#;
        match parse_packages_lock(json) {
            Err(PackageError::InvalidDigest { .. }) => {}
            other => panic!("expected InvalidDigest, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_trust_state() {
        let json = r#"{
          "schema_version": 1,
          "packages": [
            {
              "name": "@oxi/unknown-trust",
              "version": "1.0.0",
              "digest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
              "source": "foundation",
              "trust": "suspect",
              "requirements": []
            }
          ]
        }"#;
        match parse_packages_lock(json) {
            Err(PackageError::InvalidTrustState { name, trust }) => {
                assert_eq!(name, "@oxi/unknown-trust");
                assert_eq!(trust, "suspect");
            }
            other => panic!("expected InvalidTrustState, got {other:?}"),
        }
    }

    #[test]
    fn preserves_unknown_requirement() {
        let json = r#"{
          "schema_version": 1,
          "packages": [
            {
              "name": "@oxi/future",
              "version": "0.1.0",
              "digest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
              "source": "foundation",
              "trust": "verified",
              "requirements": ["workspace.read", "telemetry.exfiltrate"]
            }
          ]
        }"#;
        let lock = parse_packages_lock(json).unwrap();
        let pkg = &lock.packages[0];
        let reqs: Vec<&AbstractRequirement> = pkg.requirements.iter().collect();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0], &AbstractRequirement::WorkspaceRead);
        match &reqs[1] {
            AbstractRequirement::Unknown(s) => assert_eq!(s, "telemetry.exfiltrate"),
            other => panic!("expected Unknown, got {other:?}"),
        }
        assert!(reqs[0].is_known());
        assert!(!reqs[1].is_known());
    }

    #[test]
    fn target_exclusion_works() {
        let json = r#"{
          "schema_version": 1,
          "packages": [
            {
              "name": "@oxi/code-review",
              "version": "1.4.0",
              "digest": "sha256-0000000000000000000000000000000000000000000000000000000000000000",
              "source": "foundation",
              "trust": "verified",
              "targets": ["oxicode"],
              "requirements": []
            },
            {
              "name": "@oxi/universal",
              "version": "1.0.0",
              "digest": "sha256-1111111111111111111111111111111111111111111111111111111111111111",
              "source": "foundation",
              "trust": "pinned",
              "requirements": ["brain.query"]
            }
          ]
        }"#;
        let lock = parse_packages_lock(json).unwrap();
        // oxicode caller sees the oxicode-targeted package first.
        let pkg = select_package_for_target(&lock, "oxicode").unwrap();
        assert_eq!(pkg.name, "@oxi/code-review");
        // oxios caller only sees the universal (no-targets) package; the
        // oxicode-targeted package is excluded by its `targets` list.
        let pkg = select_package_for_target(&lock, "oxios").unwrap();
        assert_eq!(pkg.name, "@oxi/universal");
        // Sanity: a never-mentioned target still matches universal but not
        // the explicit-targets one. Use direct `applies_to` rather than
        // `select_package_for_target` so the assertion is unambiguous.
        assert!(lock.packages[0].applies_to("oxicode"));
        assert!(!lock.packages[0].applies_to("oxios"));
        assert!(lock.packages[1].applies_to("oxios"));
        assert!(lock.packages[1].applies_to("anything"));
    }

    #[test]
    fn trust_state_predicates() {
        assert!(TrustState::Verified.allows_reads());
        assert!(TrustState::Verified.allows_writes());
        assert!(TrustState::Pinned.allows_reads());
        assert!(!TrustState::Pinned.allows_writes());
        assert!(!TrustState::Untrusted.allows_reads());
        assert!(!TrustState::Untrusted.allows_writes());
    }

    #[test]
    fn load_does_not_mutate_lockfile_on_disk() {
        // The reader must not write to the lockfile. We assert by reading
        // bytes before and after load_packages_lock and requiring equality.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let lock_path = home.join("packages.lock");
        let original = br#"{
          "schema_version": 1,
          "packages": []
        }"#;
        std::fs::write(&lock_path, original).unwrap();

        let loaded = load_packages_lock(home).unwrap().unwrap();
        assert_eq!(loaded.packages.len(), 0);

        let after = std::fs::read(&lock_path).unwrap();
        assert_eq!(after, original, "lockfile on disk must be unchanged");
    }

    #[test]
    fn load_returns_none_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_packages_lock(dir.path()).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn helper_does_not_open_socket() {
        // The reader must not even know about the brain socket. Confirm
        // by inspection: this module has no reference to a socket path.
        // The compile-time invariant is that the only path it talks about
        // is `packages.lock` under `$HOME/.oxi/foundation/v1`.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join("packages.lock"), minimal_lock().as_bytes()).unwrap();
        let lock = load_packages_lock(home).unwrap().unwrap();
        // A pure read-only helper — no I/O side effects.
        let pkg = select_package_for_target(&lock, "oxicode").unwrap();
        assert_eq!(pkg.name, "@oxi/code-review");
        // Reload the lockfile; if the helper had cached or mutated
        // anything, behaviour would diverge.
        let lock2 = load_packages_lock(home).unwrap().unwrap();
        assert_eq!(lock, lock2);
    }
}
