//! The resource-server half of the MCP authorization spec
//! (`2026-07-28/basic/authorization`), so that a standards-compliant client can connect
//! knowing only the URL.
//!
//! The registry is not an authorization server and does not become one. It is an OAuth 2.1
//! *protected resource*, and its whole job here is to say, in the two places a client is
//! required to look, "my authorization server is over there":
//!
//! 1. **RFC 9728 Protected Resource Metadata**, served at both well-known locations the spec
//!    tells clients to probe — `/.well-known/oauth-protected-resource/mcp` (path-inserted, for
//!    the `/mcp` endpoint) and `/.well-known/oauth-protected-resource` (root).
//! 2. **A `WWW-Authenticate: Bearer resource_metadata="…"` challenge** on every 401, which is
//!    the mechanism clients must prefer over probing.
//!
//! From the `authorization_servers` entry the client runs RFC 8414 / OpenID Connect discovery
//! against Keycloak itself, registers or reuses a client, and comes back with a bearer token —
//! which `crate::auth` then verifies exactly as it verifies one presented to the REST API.
//!
//! ## A note on `resource` and `aud`
//!
//! RFC 9728 requires each metadata document's `resource` to be the identifier of the resource
//! the document was fetched for, so the path-inserted document names `{base}/mcp` and the root
//! document names `{base}`. The challenge deliberately points at the **root** document, because
//! `crate::auth::jwt` validates `aud` against a single configured audience that defaults to the
//! base IRI: a client following the challenge therefore sends `resource={base}`, the
//! authorization server mints `aud={base}`, and the token verifies. An operator whose
//! authorization server honours RFC 8707 per-path resources should set `TAR_OIDC_AUDIENCE`
//! accordingly.

use super::{resource_uri, SERVER_NAME};
use crate::state::AppState;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};
use std::sync::Arc;

/// Where the root Protected Resource Metadata document lives.
pub fn root_metadata_url(state: &AppState) -> String {
    format!("{}/.well-known/oauth-protected-resource", state.base())
}

/// Where the `/mcp`-specific document lives (RFC 9728 path insertion).
pub fn endpoint_metadata_url(state: &AppState) -> String {
    format!("{}/.well-known/oauth-protected-resource/mcp", state.base())
}

/// Optional operator override: the scope set to advertise as `scopes_supported`.
///
/// Left unset by default on purpose. The MCP scope-selection strategy says a client with no
/// `scope` in the challenge falls back to `scopes_supported`, and failing that omits the
/// parameter entirely — which is the behaviour that actually works against a stock Keycloak
/// realm, where the registry's roles arrive in the token without any custom scope having to be
/// requested. Advertising scope names the authorization server has never heard of would turn a
/// working sign-in into an `invalid_scope` error. Operators who *have* modelled
/// `register:software` and friends as OAuth scopes set `TAR_MCP_SCOPES` and get least privilege.
fn advertised_scopes() -> Option<Vec<String>> {
    let raw = std::env::var("TAR_MCP_SCOPES").ok()?;
    let v: Vec<String> = raw.split([',', ' ']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    (!v.is_empty()).then_some(v)
}

/// Build one RFC 9728 document for a given resource identifier.
fn metadata_for(state: &AppState, resource: &str) -> Value {
    let mut doc = json!({
        "resource": resource,
        "bearer_methods_supported": ["header"],
        "resource_name": format!("{} — {}", state.config.title, SERVER_NAME),
        // No `resource_documentation`: it is optional, and the registry serves its SPA on any
        // unrouted path, so any URL named here would resolve to the app shell rather than to
        // documentation. Naming nothing beats naming something misleading.
    });
    let obj = doc.as_object_mut().expect("object");

    // MCP requires at least one authorization server. When the registry runs without OIDC it
    // has none to name — it authenticates with opaque `tar_…` tokens minted in the UI — and
    // saying so honestly beats naming an issuer that does not exist.
    let issuers = state.config.oidc.accepted_issuers();
    if !issuers.is_empty() {
        obj.insert("authorization_servers".into(), json!(issuers));
    }
    if let Some(scopes) = advertised_scopes() {
        obj.insert("scopes_supported".into(), json!(scopes));
    }
    doc
}

/// `GET /.well-known/oauth-protected-resource[/mcp]`.
///
/// Unauthenticated by design: discovery metadata is what a client reads *before* it has a
/// credential, and it contains nothing but public endpoint locations.
pub async fn protected_resource_metadata(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
) -> impl IntoResponse {
    let resource =
        if uri.path().ends_with("/mcp") { resource_uri(&state) } else { state.base().to_string() };
    (
        [(axum::http::header::CACHE_CONTROL, "public, max-age=3600")],
        Json(metadata_for(&state, &resource)),
    )
}

/// The `WWW-Authenticate` value for a 401 on the MCP endpoint.
///
/// No `scope` parameter: read tools need authentication and nothing more, and per the spec a
/// challenge scope is "the minimum needed for the current operation". Per-operation authority
/// is reported inside the tool result instead, where a model can act on it — see
/// [`super::call`].
pub fn challenge(state: &AppState) -> String {
    format!(
        "Bearer realm=\"{}\", resource_metadata=\"{}\", error=\"invalid_token\", \
         error_description=\"an MCP request needs a bearer token: an OIDC token from a trusted issuer, or a tar_ registry token\"",
        SERVER_NAME,
        root_metadata_url(state)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    async fn state() -> Arc<AppState> {
        let mut config = Config::for_test("https://reg.test.example");
        config.oidc.issuer = Some("https://kc.test.example/realms/tar".into());
        let store = Arc::new(crate::store::OxigraphStore::memory().unwrap());
        let ops = crate::ops::Ops::open(":memory:").await.unwrap();
        Arc::new(AppState::from_parts(config, store, ops))
    }

    #[tokio::test]
    async fn root_document_names_the_base_iri_and_the_issuer() {
        let s = state().await;
        let doc = metadata_for(&s, s.base());
        assert_eq!(doc["resource"], "https://reg.test.example");
        assert_eq!(doc["authorization_servers"][0], "https://kc.test.example/realms/tar");
        assert_eq!(doc["bearer_methods_supported"][0], "header");
    }

    #[tokio::test]
    async fn path_inserted_document_names_the_endpoint() {
        let s = state().await;
        let doc = metadata_for(&s, &resource_uri(&s));
        assert_eq!(doc["resource"], "https://reg.test.example/mcp");
    }

    #[tokio::test]
    async fn challenge_points_at_the_root_metadata_document() {
        let s = state().await;
        let c = challenge(&s);
        assert!(c.starts_with("Bearer "), "{c}");
        assert!(
            c.contains("resource_metadata=\"https://reg.test.example/.well-known/oauth-protected-resource\""),
            "{c}"
        );
    }

    #[tokio::test]
    async fn scopes_are_absent_unless_an_operator_configures_them() {
        let s = state().await;
        let doc = metadata_for(&s, s.base());
        assert!(doc.get("scopes_supported").is_none());
    }
}
