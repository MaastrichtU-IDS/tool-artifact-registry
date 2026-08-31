//! JSON-RPC 2.0 framing and MCP protocol-version handling.
//!
//! Written out rather than taken from an SDK. The framing is a hundred lines of serde and the
//! interesting requirements — the `2026-07-28` header/body mirror validation, the
//! `UnsupportedProtocolVersionError`, and serving both the modern (stateless, per-request
//! `_meta`) and legacy (`initialize` handshake) eras from one endpoint — are exactly the parts
//! an SDK's own transport abstraction would want to own. Reusing `rmcp` would have meant
//! running its service model beside axum's, and re-deriving the `Principal` extractor inside
//! it, to gain code we would still have to audit against this revision.

use serde::Deserialize;
use serde_json::{json, Value};

// ------------------------------------------------------------- protocol versions

pub const V_2026_07_28: &str = "2026-07-28";
pub const V_2025_11_25: &str = "2025-11-25";
pub const V_2025_06_18: &str = "2025-06-18";
pub const V_2025_03_26: &str = "2025-03-26";

/// Newest first — `supportedVersions` and `UnsupportedProtocolVersionError.data.supported`
/// are both served from this, so a client always sees our preference order.
pub const SUPPORTED: [&str; 4] = [V_2026_07_28, V_2025_11_25, V_2025_06_18, V_2025_03_26];

/// The revision this server is written against.
pub const PINNED: &str = V_2026_07_28;

/// Which era a request belongs to (`basic/versioning`: "modern" vs "legacy").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// `2026-07-28`: stateless, per-request `_meta`, mirrored HTTP headers.
    Modern,
    /// `2025-11-25` and earlier: an `initialize` handshake opened the session.
    Legacy,
}

pub fn era_of(version: &str) -> Option<Era> {
    match version {
        V_2026_07_28 => Some(Era::Modern),
        V_2025_11_25 | V_2025_06_18 | V_2025_03_26 => Some(Era::Legacy),
        _ => None,
    }
}

// ------------------------------------------------------------------ error codes

/// JSON-RPC 2.0 reserved codes.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

/// MCP protocol-defined codes (`basic/index#error-codes`).
pub const HEADER_MISMATCH: i32 = -32020;
pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;

/// Not a JSON-RPC code: our own marker for "authentication required", carried alongside the
/// HTTP 401 so a client that only reads the body still gets a sentence.
pub const UNAUTHORIZED: i32 = -32001;

// --------------------------------------------------------------------- messages

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    #[serde(default)]
    pub jsonrpc: Option<String>,
    /// Absent (or null) means this is a *notification*: no response may be sent.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl RpcRequest {
    pub fn is_notification(&self) -> bool {
        matches!(self.id, None | Some(Value::Null))
    }

    /// `params.name`, or `params.uri` for `resources/read` — the source field the transport
    /// mirrors into `Mcp-Name`.
    pub fn mcp_name(&self) -> Option<&str> {
        self.params.get("name").or_else(|| self.params.get("uri")).and_then(Value::as_str)
    }

    /// `params._meta["io.modelcontextprotocol/protocolVersion"]`, the body's source of truth
    /// for the version a modern request declares.
    pub fn meta_version(&self) -> Option<&str> {
        self.params
            .get("_meta")
            .and_then(|m| m.get("io.modelcontextprotocol/protocolVersion"))
            .and_then(Value::as_str)
    }
}

pub fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: &Value, code: i32, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

pub fn error_with_data(id: &Value, code: i32, message: impl Into<String>, data: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id,
        "error": { "code": code, "message": message.into(), "data": data }
    })
}

/// `UnsupportedProtocolVersionError` — HTTP 400 plus the versions we do speak, which is what
/// lets a dual-era client retry instead of falling back to `initialize`.
pub fn unsupported_version(id: &Value, requested: &str) -> Value {
    error_with_data(
        id,
        UNSUPPORTED_PROTOCOL_VERSION,
        "Unsupported protocol version",
        json!({ "supported": SUPPORTED, "requested": requested }),
    )
}

// ------------------------------------------------------------- header mirroring

/// Decode the `=?base64?…?=` sentinel a client must use when a value is not header-safe.
///
/// `Mcp-Name` carries a tool name or a resource URI, and the spec only *recommends* that those
/// stay in the ASCII-safe set, so the encoded form has to be understood before comparing.
pub fn decode_header_value(raw: &str) -> Option<String> {
    let Some(inner) = raw.strip_prefix("=?base64?").and_then(|s| s.strip_suffix("?=")) else {
        return Some(raw.to_string());
    };
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(inner).ok()?;
    String::from_utf8(bytes).ok()
}

/// Methods whose `params` carry a name or URI that must be mirrored into `Mcp-Name`.
pub fn requires_mcp_name(method: &str) -> bool {
    matches!(method, "tools/call" | "resources/read" | "prompts/get")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eras_are_classified() {
        assert_eq!(era_of(V_2026_07_28), Some(Era::Modern));
        assert_eq!(era_of(V_2025_11_25), Some(Era::Legacy));
        assert_eq!(era_of(V_2025_06_18), Some(Era::Legacy));
        assert_eq!(era_of("1900-01-01"), None);
    }

    #[test]
    fn plain_header_values_pass_through() {
        assert_eq!(decode_header_value("tools/call").as_deref(), Some("tools/call"));
    }

    #[test]
    fn base64_sentinel_is_decoded() {
        // The spec's own example: "Hello, 世界".
        assert_eq!(decode_header_value("=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(), Some("Hello, 世界"));
    }

    #[test]
    fn malformed_sentinel_is_rejected_rather_than_taken_literally() {
        assert_eq!(decode_header_value("=?base64?not-valid-base64!!?="), None);
    }

    #[test]
    fn notifications_have_no_id() {
        let r: RpcRequest = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert!(r.is_notification());
        let r: RpcRequest = serde_json::from_str(r#"{"jsonrpc":"2.0","id":0,"method":"tools/list"}"#).unwrap();
        assert!(!r.is_notification());
    }

    #[test]
    fn mcp_name_reads_name_then_uri() {
        let r: RpcRequest =
            serde_json::from_str(r#"{"id":1,"method":"tools/call","params":{"name":"vocab_search"}}"#).unwrap();
        assert_eq!(r.mcp_name(), Some("vocab_search"));
        let r: RpcRequest =
            serde_json::from_str(r#"{"id":1,"method":"resources/read","params":{"uri":"file:///x"}}"#).unwrap();
        assert_eq!(r.mcp_name(), Some("file:///x"));
    }

    #[test]
    fn meta_version_is_read_from_the_namespaced_key() {
        let r: RpcRequest = serde_json::from_str(
            r#"{"id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        )
        .unwrap();
        assert_eq!(r.meta_version(), Some(V_2026_07_28));
    }
}
