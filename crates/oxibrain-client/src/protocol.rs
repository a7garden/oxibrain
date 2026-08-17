//! Versioned handshake protocol for Oxi Foundation daemon discovery.
//!
//! This is the additive client/server surface described in
//! `doc/spec/oxi-foundation-v1.md` §8. It is **not** an MCP tool — it rides on
//! the existing JSON-RPC transport so the MCP tool count stays at fifteen.
//!
//! Wire shape (newline-delimited JSON-RPC 2.0):
//!
//! ```text
//! client → server: {"jsonrpc":"2.0","id":N,"method":"handshake",
//!                   "params": ClientHello}
//! server → client: {"jsonrpc":"2.0","id":N,"result": ServerInfo}
//! ```
//!
//! The handshake runs after optional `auth` and before any MCP tool routing.
//! Discovery metadata never replaces a token and never broadens scope
//! (Foundation v1 §8).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Wire method name for the handshake. Distinct from MCP `initialize` because
/// it is a transport-level capability negotiation, not part of the MCP tool
/// surface.
pub const HANDSHAKE_METHOD: &str = "handshake";

/// The Oxi Foundation protocol range this crate speaks.
///
/// The daemon advertises its `min_compatible` and `max_compatible` in
/// `ServerInfo`; clients reject themselves if their `BrainProtocolVersion` is
/// not within that range.
pub const PROTOCOL_VERSION_MIN: u32 = 1;
pub const PROTOCOL_VERSION_MAX: u32 = 1;

/// Minimum store-format revision this crate understands.
///
/// The daemon advertises `store_format_version` in `ServerInfo`; a client
/// whose `min_store_format_version` is higher than what the server reports
/// refuses to talk. This lets us rev the on-disk format safely.
pub const MIN_STORE_FORMAT_VERSION: u32 = 1;
/// Store format revision the server is shipping.
pub const CURRENT_STORE_FORMAT_VERSION: u32 = 1;

/// Operations the client supports over the JSON-RPC transport, independent of
/// any MCP tool. The server returns the intersection of what it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientOperation {
    /// Send `tools/call` JSON-RPC requests.
    McpToolCall,
    /// Subscribe to `notifications/*` server-initiated pushes (read-only by
    /// default).
    Notifications,
    /// Use `sampling/createMessage` server-initiated requests (§12.3).
    Sampling,
}

impl ClientOperation {
    pub const ALL: &'static [ClientOperation] = &[
        ClientOperation::McpToolCall,
        ClientOperation::Notifications,
        ClientOperation::Sampling,
    ];
}

/// Numeric Oxi Foundation protocol version (server- and client-side).
///
/// `BrainProtocolVersion` is a strict integer; the daemon advertises a range
/// `[min_compatible, max_compatible]` and the client picks any value in that
/// range that it supports. The client must set this to a value the daemon will
/// accept — typically [`PROTOCOL_VERSION_MAX`] — and the server validates the
/// range on receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BrainProtocolVersion(pub u32);

impl BrainProtocolVersion {
    pub const fn new(v: u32) -> Self {
        Self(v)
    }
}

impl fmt::Display for BrainProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What the client sends to the server at the start of every connection.
///
/// `client_version` is the human-readable version string of the consuming
/// application (e.g. `oxicode 0.4.1`). `protocol_version` is the Foundation
/// wire revision it speaks. `supported_operations` lists what the client
/// intends to use; the server advertises the intersection back. No API key,
/// token, or other credential is included — auth is a separate, earlier
/// message on token-protected sockets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientHello {
    /// The Foundation wire protocol version the client wants to use.
    pub protocol_version: BrainProtocolVersion,
    /// The lowest Foundation protocol version the client will accept.
    /// The server must satisfy `min_compatible ≤ requested ≤ max_compatible`.
    #[serde(default)]
    pub min_compatible: Option<BrainProtocolVersion>,
    /// The highest Foundation protocol version the client understands.
    /// The server must satisfy `requested ≤ max_compatible`.
    #[serde(default)]
    pub max_compatible: Option<BrainProtocolVersion>,
    /// The lowest store-format revision the client can read. Lets us reject
    /// before reading any bytes from the SQLite store.
    pub min_store_format_version: u32,
    /// Human-readable identity of the calling program (`"oxicode"`, `"oxios"`,
    /// …). For diagnostics; never trusted for security decisions.
    pub client_version: String,
    /// Transport-level operations the client intends to use. The server
    /// advertises the subset it actually supports.
    #[serde(default)]
    pub supported_operations: Vec<ClientOperation>,
}

/// What the server returns after a successful handshake.
///
/// `min_compatible` / `max_compatible` form the closed range of wire protocol
/// versions the daemon is willing to speak *right now* — the client must
/// request a value inside it, otherwise the server rejects the connection
/// with a typed error. `store_format_version` is the on-disk revision the
/// daemon is shipping; clients whose `min_store_format_version` exceeds it
/// refuse to proceed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Wire protocol versions the daemon will accept. Closed interval.
    pub min_compatible: BrainProtocolVersion,
    /// Wire protocol versions the daemon will accept. Closed interval.
    pub max_compatible: BrainProtocolVersion,
    /// Store format revision the daemon is shipping.
    pub store_format_version: u32,
    /// Operations the daemon actually supports, intersected with what the
    /// client asked for. Always a subset of `ClientHello::supported_operations`.
    pub supported_operations: Vec<ClientOperation>,
    /// Identity of the daemon (`"oxibrain"`).
    pub server_name: String,
    /// Version string of the daemon (e.g. `"0.3.0"`). Diagnostic only.
    pub server_version: String,
}

impl ServerInfo {
    /// Returns true if `requested` is inside `[min_compatible, max_compatible]`.
    pub fn accepts(&self, requested: BrainProtocolVersion) -> bool {
        requested.0 >= self.min_compatible.0 && requested.0 <= self.max_compatible.0
    }
}

/// Capabilities and store compatibility summary — the client's view of what
/// this daemon offers. `BrainCapabilities` is the *typed* value the client
/// keeps after a handshake; `ServerInfo` is the wire form.
#[derive(Debug, Clone)]
pub struct BrainCapabilities {
    /// Agreed-upon wire protocol version (the client's request, validated).
    pub protocol_version: BrainProtocolVersion,
    /// Store format revision the daemon ships.
    pub store_format_version: u32,
    /// Operations both client and server agreed to.
    pub supported_operations: Vec<ClientOperation>,
    /// Daemon identity.
    pub server_name: String,
    /// Daemon version string.
    pub server_version: String,
}

impl From<ServerInfo> for BrainCapabilities {
    fn from(info: ServerInfo) -> Self {
        Self {
            protocol_version: BrainProtocolVersion::new(info.max_compatible.0),
            store_format_version: info.store_format_version,
            supported_operations: info.supported_operations,
            server_name: info.server_name,
            server_version: info.server_version,
        }
    }
}

/// Typed rejection from a handshake. The server returns one of these via the
/// JSON-RPC `error.data` field; the client surfaces it to the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandshakeError {
    /// `requested` is outside the daemon's supported range. The daemon
    /// includes the range it *does* support so the caller can adapt.
    IncompatibleProtocol {
        requested: u32,
        min_compatible: u32,
        max_compatible: u32,
    },
    /// The store format revision on disk is too old for this client.
    StoreTooOld { server_format: u32, client_min: u32 },
    /// The hello payload was malformed.
    MalformedHello { reason: String },
    /// The client asked for an operation the server does not support.
    UnsupportedOperations { unsupported: Vec<String> },
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandshakeError::IncompatibleProtocol {
                requested,
                min_compatible,
                max_compatible,
            } => write!(
                f,
                "incompatible protocol: requested {requested}, \
                 supported range [{min_compatible}, {max_compatible}]"
            ),
            HandshakeError::StoreTooOld {
                server_format,
                client_min,
            } => write!(
                f,
                "server store format {server_format} is older than client requires {client_min}"
            ),
            HandshakeError::MalformedHello { reason } => {
                write!(f, "malformed handshake hello: {reason}")
            }
            HandshakeError::UnsupportedOperations { unsupported } => write!(
                f,
                "server does not support client operations: {}",
                unsupported.join(", ")
            ),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// Convert a JSON-RPC `error` value into a typed `HandshakeError` when the
/// error came from a `handshake` call.
pub fn parse_handshake_error(err: &serde_json::Value) -> Option<HandshakeError> {
    let data = err.get("data")?;
    serde_json::from_value::<HandshakeError>(data.clone()).ok()
}

/// Build the `ClientHello` payload this crate sends by default.
pub fn default_client_hello(client_version: impl Into<String>) -> ClientHello {
    ClientHello {
        protocol_version: BrainProtocolVersion::new(PROTOCOL_VERSION_MAX),
        min_compatible: Some(BrainProtocolVersion::new(PROTOCOL_VERSION_MIN)),
        max_compatible: Some(BrainProtocolVersion::new(PROTOCOL_VERSION_MAX)),
        min_store_format_version: MIN_STORE_FORMAT_VERSION,
        client_version: client_version.into(),
        supported_operations: ClientOperation::ALL.to_vec(),
    }
}

/// Construct the `ServerInfo` this daemon (oxibrain) advertises.
pub fn server_info(
    server_name: impl Into<String>,
    server_version: impl Into<String>,
) -> ServerInfo {
    ServerInfo {
        min_compatible: BrainProtocolVersion::new(PROTOCOL_VERSION_MIN),
        max_compatible: BrainProtocolVersion::new(PROTOCOL_VERSION_MAX),
        store_format_version: CURRENT_STORE_FORMAT_VERSION,
        supported_operations: ClientOperation::ALL.to_vec(),
        server_name: server_name.into(),
        server_version: server_version.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_client_hello_uses_current_protocol() {
        let hello = default_client_hello("test-client/1.0");
        assert_eq!(hello.protocol_version.0, PROTOCOL_VERSION_MAX);
        assert_eq!(hello.min_compatible.unwrap().0, PROTOCOL_VERSION_MIN);
        assert_eq!(hello.max_compatible.unwrap().0, PROTOCOL_VERSION_MAX);
        assert_eq!(hello.min_store_format_version, MIN_STORE_FORMAT_VERSION);
        assert!(
            hello
                .supported_operations
                .contains(&ClientOperation::McpToolCall)
        );
    }

    #[test]
    fn server_info_accepts_versions_in_range() {
        let info = server_info("oxibrain", "0.3.0");
        assert!(info.accepts(BrainProtocolVersion::new(1)));
        assert!(!info.accepts(BrainProtocolVersion::new(0)));
        assert!(!info.accepts(BrainProtocolVersion::new(2)));
    }

    #[test]
    fn handshake_error_renders_supported_range() {
        let err = HandshakeError::IncompatibleProtocol {
            requested: 99,
            min_compatible: 1,
            max_compatible: 1,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("incompatible protocol"));
        assert!(rendered.contains("requested 99"));
        assert!(rendered.contains("[1, 1]"));
    }

    #[test]
    fn parse_handshake_error_recovers_typed_data() {
        let err = serde_json::json!({
            "code": -32000,
            "message": "incompatible protocol",
            "data": {
                "kind": "incompatible_protocol",
                "requested": 99,
                "min_compatible": 1,
                "max_compatible": 1
            }
        });
        let typed = parse_handshake_error(&err).expect("typed parse");
        match typed {
            HandshakeError::IncompatibleProtocol {
                requested,
                min_compatible,
                max_compatible,
            } => {
                assert_eq!(requested, 99);
                assert_eq!(min_compatible, 1);
                assert_eq!(max_compatible, 1);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn client_hello_round_trips_through_json() {
        let hello = default_client_hello("oxicode/0.4.1");
        let line = serde_json::to_string(&hello).unwrap();
        let back: ClientHello = serde_json::from_str(&line).unwrap();
        assert_eq!(back.protocol_version.0, hello.protocol_version.0);
        assert_eq!(back.client_version, hello.client_version);
    }
}
