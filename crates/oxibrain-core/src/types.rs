//! Ledger value types. Knowledge types (entities, statements, ...) land in M1.

use blake3::Hash;
use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

/// BLAKE3 digest over normalized content.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn from_hash(h: Hash) -> Self {
        Self(h.into())
    }
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ContentHash({})", self.hex())
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    Trusted,
    SemiTrusted,
    Untrusted,
}

impl TrustTier {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::SemiTrusted => "semi_trusted",
            Self::Untrusted => "untrusted",
        }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "trusted" => Some(Self::Trusted),
            "semi_trusted" => Some(Self::SemiTrusted),
            "untrusted" => Some(Self::Untrusted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeKind {
    Primary,
    Declaration,
    Derived,
}

impl EpisodeKind {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Declaration => "declaration",
            Self::Derived => "derived",
        }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s {
            "primary" => Some(Self::Primary),
            "declaration" => Some(Self::Declaration),
            "derived" => Some(Self::Derived),
            _ => None,
        }
    }
}

/// Where an episode came from. M0 supports Note and Declaration; others land later.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ref", rename_all = "snake_case")]
pub enum SourceRef {
    Note { path: String },
    Document { uri: String },
    Conversation,
    Message,
    AgentTrace,
    Declaration,
    Derived { of: String },
}

impl SourceRef {
    /// (source_kind column, source_ref column) for persistence.
    pub fn db_columns(&self) -> (&'static str, Option<String>) {
        match self {
            Self::Note { path } => ("note", Some(path.clone())),
            Self::Document { uri } => ("document", Some(uri.clone())),
            Self::Conversation => ("conversation", None),
            Self::Message => ("message", None),
            Self::AgentTrace => ("agent_trace", None),
            Self::Declaration => ("declaration", None),
            Self::Derived { of } => ("derived", Some(of.clone())),
        }
    }
    pub fn kind_db(&self) -> &'static str {
        self.db_columns().0
    }
}

/// A namespace and isolation boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    pub id: String,
    pub name: String,
    pub created_at: Timestamp,
}

/// The atom of record. Immutable once written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    pub space: String,
    pub seq: u64,
    pub content_hash: ContentHash,
    pub content: String,
    pub source: SourceRef,
    pub trust: TrustTier,
    pub kind: EpisodeKind,
    pub occurred_at: Timestamp,
    pub ingested_at: Timestamp,
    pub redacted_at: Option<Timestamp>,
}
