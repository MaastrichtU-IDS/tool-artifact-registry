//! The Streamable HTTP binding: `POST {base}/mcp`.
//!
//! Implements revision **`2026-07-28`** — one endpoint, one HTTP POST per JSON-RPC message, no
//! protocol-level session, no `Mcp-Session-Id`, no standalone GET stream, and the request
//! metadata mirrored into `Mcp-Method` / `Mcp-Name` / `MCP-Protocol-Version` and validated
//! against the body.
//!
//! It is also **dual-era**. `2026-07-28/basic/versioning` describes a server that serves both
//! the modern per-request-metadata protocol and the older `initialize` handshake, and this one
//! does: a request carrying modern metadata is served statelessly per this revision, an
//! `initialize` request selects legacy semantics for `2025-11-25`, `2025-06-18` or
//! `2025-03-26`. That costs about fifty lines and is what makes the endpoint usable by the
//! clients that exist rather than only the clients the spec describes. Statelessness makes it
//! nearly free: there is no session to keep, so the legacy handshake is a handshake in name
//! only, and every request is authenticated and authorised on its own.
//!
//! Every response is a single JSON object. No tool here streams or reports progress, so the
//! spec's alternative — an SSE response stream scoped to the request — would add a content type
//! for no benefit.

use super::oauth;
use super::rpc::{self, Era};
use super::{tools, McpConfig, SERVER_NAME};
use crate::auth::Principal;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::sync::Arc;

/// How long a client may cache `tools/list`. Short, because the set varies with the caller's
/// authority and an operator can grant a scope at any moment.
const TOOLS_TTL_MS: u64 = 60_000;
/// `server/discover` is the same for everyone and changes only on deployment.
const DISCOVER_TTL_MS: u64 = 3_600_000;

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

/// 2026-07-28 removed the GET stream and the DELETE session teardown; the spec asks a
/// modern-only endpoint to answer both with 405 so an older client fails fast.
pub async fn method_not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "the MCP endpoint accepts POST only: protocol revision 2026-07-28 removed the GET stream and \
         the DELETE session teardown",
    )
        .into_response()
}

/// Origin allow-list, for the DNS-rebinding protection the transport spec requires.
///
/// A browser page on another origin cannot read a cross-origin response without CORS, but it
/// *can* send the request, and a registry reachable on a private network is exactly the target
/// the requirement is about. Non-browser clients send no `Origin` at all and are unaffected.
fn origin_allowed(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let base = state.base();
    let base_origin = base
        .split_once("://")
        .map(|(scheme, rest)| format!("{scheme}://{}", rest.split('/').next().unwrap_or_default()))
        .unwrap_or_else(|| base.to_string());
    if origin.eq_ignore_ascii_case(&base_origin) {
        return true;
    }
    std::env::var("TAR_MCP_ALLOWED_ORIGINS")
        .ok()
        .map(|v| v.split(',').any(|o| o.trim().eq_ignore_ascii_case(origin)))
        .unwrap_or(false)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The 401 that starts the OAuth flow: RFC 9728's `WWW-Authenticate` challenge naming the
/// protected-resource metadata document, from which a client discovers Keycloak on its own.
fn unauthorized(state: &AppState, id: &Value, detail: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::WWW_AUTHENTICATE, oauth::challenge(state)),
            (header::CONTENT_TYPE, "application/json".to_string()),
        ],
        rpc::error(id, rpc::UNAUTHORIZED, detail).to_string(),
    )
        .into_response()
}

pub async fn endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let cfg = McpConfig::from_env();
    if !cfg.enabled {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({ "type": "https://w3id.org/tar/problem/not-found", "title": "MCP endpoint disabled", "status": 404 }),
        );
    }

    if !origin_allowed(&state, &headers) {
        return json_response(
            StatusCode::FORBIDDEN,
            rpc::error(&Value::Null, rpc::INVALID_REQUEST, "Origin not allowed"),
        );
    }

    let Ok(req) = serde_json::from_slice::<rpc::RpcRequest>(&body) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            rpc::error(&Value::Null, rpc::PARSE_ERROR, "request body is not a JSON-RPC message"),
        );
    };
    let id = req.id.clone().unwrap_or(Value::Null);

    // ------------------------------------------------------------ era and version

    let hdr_version = header_str(&headers, "mcp-protocol-version");
    let era = if req.method == "initialize" {
        // A legacy client opens with `initialize` and no version header. The era is chosen by
        // how the client opens, exactly as the versioning page prescribes.
        Era::Legacy
    } else {
        let declared = hdr_version.or_else(|| req.meta_version());
        match declared {
            Some(v) => match rpc::era_of(v) {
                Some(e) => e,
                None => {
                    return json_response(StatusCode::BAD_REQUEST, rpc::unsupported_version(&id, v));
                }
            },
            // "A server that supports clients implementing protocol versions earlier than
            // 2025-06-18 MAY treat a request that omits the header as protocol version
            // 2025-03-26." We do, because we support that era anyway.
            None => Era::Legacy,
        }
    };

    if era == Era::Modern {
        if let Err(msg) = validate_mirrored_headers(&headers, &req) {
            return json_response(StatusCode::BAD_REQUEST, rpc::error(&id, rpc::HEADER_MISMATCH, msg));
        }
    }

    // A notification gets no response body, whatever it says.
    if req.is_notification() {
        return (StatusCode::ACCEPTED, ()).into_response();
    }

    // -------------------------------------------------------------- authentication

    let raw_auth = header_str(&headers, "authorization").map(str::to_string);
    let principal = match raw_auth.as_deref() {
        None => Principal::anonymous(),
        Some(h) => {
            let token = h.split_once(' ').filter(|(s, _)| s.eq_ignore_ascii_case("bearer")).map(|(_, t)| t.trim());
            match token {
                None => Principal::anonymous(),
                Some(t) => match crate::auth::authenticate(&state, t).await {
                    Ok(p) => p,
                    Err(e) => {
                        return unauthorized(
                            &state,
                            &id,
                            &e.detail.unwrap_or_else(|| "the bearer token was rejected".into()),
                        )
                    }
                },
            }
        }
    };

    // ------------------------------------------------------------------- dispatch

    match req.method.as_str() {
        // Open: the handshake reveals the server's name, version and capabilities and no
        // registry content — the same thing /healthz already reveals — and answering it
        // unauthenticated is what lets a client reach the 401 challenge on `tools/list` and
        // start the OAuth flow, rather than failing at connection time.
        "server/discover" if era == Era::Modern => json_response(StatusCode::OK, rpc::result(&id, discover())),
        "initialize" if era == Era::Legacy => {
            json_response(StatusCode::OK, rpc::result(&id, initialize(&req.params)))
        }
        "ping" => json_response(StatusCode::OK, rpc::result(&id, json!({}))),

        // Everything past here needs a credential. An unauthenticated caller learns nothing
        // about this registry — not even which tools exist.
        "tools/list" => {
            if principal.is_anonymous() {
                return unauthorized(&state, &id, "listing tools needs a credential");
            }
            let list: Vec<Value> =
                tools::visible(&principal, cfg.read_only).iter().map(tools::Tool::to_json).collect();
            let mut result = json!({ "tools": list });
            if era == Era::Modern {
                let obj = result.as_object_mut().expect("object");
                obj.insert("resultType".into(), json!("complete"));
                obj.insert("ttlMs".into(), json!(TOOLS_TTL_MS));
                // Private, not public: the set is filtered by the caller's authority, so a
                // shared cache must not serve one credential's list to another.
                obj.insert("cacheScope".into(), json!("private"));
            }
            json_response(StatusCode::OK, rpc::result(&id, result))
        }
        "tools/call" => {
            if principal.is_anonymous() {
                return unauthorized(&state, &id, "calling a tool needs a credential");
            }
            let name = req.params.get("name").and_then(Value::as_str).unwrap_or_default();
            if name.is_empty() {
                return json_response(
                    StatusCode::OK,
                    rpc::error(&id, rpc::INVALID_PARAMS, "tools/call needs params.name"),
                );
            }
            let args = req.params.get("arguments").cloned().unwrap_or(json!({}));
            let outcome =
                super::call::call(&state, &principal, raw_auth.as_deref(), name, &args, cfg.read_only).await;

            let mut result = json!({
                "content": [{ "type": "text", "text": outcome.text }],
                "isError": outcome.is_error,
            });
            let obj = result.as_object_mut().expect("object");
            if let Some(structured) = outcome.structured {
                // The spec asks a tool returning structured content to also serialise it into a
                // text block, for clients that only read `content`.
                if let Ok(s) = serde_json::to_string(&structured) {
                    obj.get_mut("content")
                        .and_then(Value::as_array_mut)
                        .expect("array")
                        .push(json!({ "type": "text", "text": s }));
                }
                obj.insert("structuredContent".into(), structured);
            }
            if era == Era::Modern {
                obj.insert("resultType".into(), json!("complete"));
            }
            json_response(StatusCode::OK, rpc::result(&id, result))
        }

        // An `initialize` reaching a modern-era request, or a modern method on a legacy
        // request, is a real mismatch — name the versions we speak, since a legacy client has
        // no fall-forward mechanism and this message may be all its user ever sees.
        "initialize" | "server/discover" => json_response(
            StatusCode::BAD_REQUEST,
            rpc::error_with_data(
                &id,
                rpc::INVALID_REQUEST,
                format!(
                    "`{}` does not belong to the protocol era this request declared",
                    req.method
                ),
                json!({ "supported": rpc::SUPPORTED }),
            ),
        ),
        other => json_response(
            StatusCode::NOT_FOUND,
            rpc::error_with_data(
                &id,
                rpc::METHOD_NOT_FOUND,
                format!("this server implements tools only; `{other}` is not available"),
                json!({ "supported": rpc::SUPPORTED }),
            ),
        ),
    }
}

/// `2026-07-28/basic/transports/streamable-http#server-validation`.
///
/// The headers mirror body fields so that gateways can route without parsing JSON; the danger
/// is a gateway acting on the header while the server acts on a different body. Rejecting the
/// divergence is the whole point, so a mismatch is refused even where a missing header is
/// tolerated.
fn validate_mirrored_headers(headers: &HeaderMap, req: &rpc::RpcRequest) -> Result<(), String> {
    match header_str(headers, "mcp-protocol-version") {
        None => return Err("MCP-Protocol-Version is required on a 2026-07-28 request".into()),
        Some(v) => {
            if let Some(body_v) = req.meta_version() {
                if body_v != v {
                    return Err(format!(
                        "MCP-Protocol-Version header {v:?} does not match \
                         _meta[\"io.modelcontextprotocol/protocolVersion\"] {body_v:?}"
                    ));
                }
            }
        }
    }

    match header_str(headers, "mcp-method") {
        None => return Err("Mcp-Method is required on a 2026-07-28 request".into()),
        Some(m) if m != req.method => {
            return Err(format!("Mcp-Method header {m:?} does not match the body method {:?}", req.method))
        }
        Some(_) => {}
    }

    if rpc::requires_mcp_name(&req.method) {
        let body_name = req.mcp_name();
        match header_str(headers, "mcp-name") {
            Some(raw) => {
                let Some(decoded) = rpc::decode_header_value(raw) else {
                    return Err(format!("Mcp-Name {raw:?} is not a decodable header value"));
                };
                if Some(decoded.as_str()) != body_name {
                    return Err(format!(
                        "Mcp-Name header {decoded:?} does not match the body value {:?}",
                        body_name.unwrap_or("(absent)")
                    ));
                }
            }
            // Absent is a spec violation on the client's side, but refusing the call would
            // break an otherwise-correct client over a header that exists for intermediaries
            // this deployment does not have. The divergence case above — the one with security
            // consequences — is still refused.
            None => {
                tracing::debug!(method = %req.method, "MCP client omitted the Mcp-Name header");
            }
        }
    }
    Ok(())
}

fn server_info() -> Value {
    json!({
        "name": SERVER_NAME,
        "title": "Tool Artifact Registry",
        "version": env!("CARGO_PKG_VERSION"),
        "websiteUrl": "https://w3id.org/tar/ns#",
    })
}

fn discover() -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": rpc::SUPPORTED,
        "capabilities": { "tools": {} },
        "instructions": tools::instructions(),
        "_meta": { "io.modelcontextprotocol/serverInfo": server_info() },
        "ttlMs": DISCOVER_TTL_MS,
        "cacheScope": "public",
    })
}

fn initialize(params: &Value) -> Value {
    // Echo the client's version when we speak it, else name our best legacy one: a legacy
    // client cannot fall forward, so answering with something it understands beats an error.
    let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(rpc::V_2025_11_25);
    let negotiated = match rpc::era_of(requested) {
        Some(Era::Legacy) => requested,
        _ => rpc::V_2025_11_25,
    };
    json!({
        "protocolVersion": negotiated,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": server_info(),
        "instructions": tools::instructions(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn req(method: &str, params: Value) -> rpc::RpcRequest {
        serde_json::from_value(json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })).unwrap()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn a_well_formed_modern_call_passes_validation() {
        let r = req("tools/call", json!({ "name": "vocab_search", "arguments": { "q": "alignment" } }));
        let h = headers(&[
            ("mcp-protocol-version", rpc::V_2026_07_28),
            ("mcp-method", "tools/call"),
            ("mcp-name", "vocab_search"),
        ]);
        assert!(validate_mirrored_headers(&h, &r).is_ok());
    }

    #[test]
    fn a_diverging_mcp_name_is_refused() {
        let r = req("tools/call", json!({ "name": "vocab_search" }));
        let h = headers(&[
            ("mcp-protocol-version", rpc::V_2026_07_28),
            ("mcp-method", "tools/call"),
            ("mcp-name", "register_software"),
        ]);
        let err = validate_mirrored_headers(&h, &r).unwrap_err();
        assert!(err.contains("Mcp-Name"), "{err}");
    }

    #[test]
    fn a_diverging_method_is_refused() {
        let r = req("tools/list", json!({}));
        let h = headers(&[("mcp-protocol-version", rpc::V_2026_07_28), ("mcp-method", "tools/call")]);
        assert!(validate_mirrored_headers(&h, &r).is_err());
    }

    #[test]
    fn a_diverging_meta_version_is_refused() {
        let r = req(
            "tools/list",
            json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": rpc::V_2025_11_25 } }),
        );
        let h = headers(&[("mcp-protocol-version", rpc::V_2026_07_28), ("mcp-method", "tools/list")]);
        assert!(validate_mirrored_headers(&h, &r).is_err());
    }

    #[test]
    fn a_base64_encoded_name_is_compared_after_decoding() {
        let r = req("tools/call", json!({ "name": "vocab_search" }));
        let h = headers(&[
            ("mcp-protocol-version", rpc::V_2026_07_28),
            ("mcp-method", "tools/call"),
            ("mcp-name", "=?base64?dm9jYWJfc2VhcmNo?="),
        ]);
        assert!(validate_mirrored_headers(&h, &r).is_ok());
    }

    #[test]
    fn discover_advertises_the_pinned_revision_first() {
        let d = discover();
        assert_eq!(d["supportedVersions"][0], rpc::PINNED);
        assert_eq!(d["resultType"], "complete");
        assert_eq!(d["cacheScope"], "public");
        assert!(d["instructions"].as_str().unwrap().contains("DO NOT INVENT"));
    }

    #[test]
    fn initialize_echoes_a_legacy_version_and_falls_back_otherwise() {
        assert_eq!(initialize(&json!({ "protocolVersion": "2025-06-18" }))["protocolVersion"], "2025-06-18");
        assert_eq!(initialize(&json!({ "protocolVersion": "1999-01-01" }))["protocolVersion"], rpc::V_2025_11_25);
        assert_eq!(initialize(&json!({}))["protocolVersion"], rpc::V_2025_11_25);
    }

    async fn state_with_base(base: &str) -> Arc<AppState> {
        let config = Config::for_test(base);
        let store = Arc::new(crate::store::OxigraphStore::memory().unwrap());
        let ops = crate::ops::Ops::open(":memory:").await.unwrap();
        Arc::new(AppState::from_parts(config, store, ops))
    }

    #[tokio::test]
    async fn origin_checking_allows_absent_and_same_origin_and_refuses_others() {
        let s = state_with_base("https://reg.test.example").await;
        assert!(origin_allowed(&s, &HeaderMap::new()));
        assert!(origin_allowed(&s, &headers(&[("origin", "https://reg.test.example")])));
        assert!(!origin_allowed(&s, &headers(&[("origin", "https://evil.example")])));
    }
}
