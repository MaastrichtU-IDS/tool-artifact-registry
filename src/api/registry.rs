//! Self-description, health and audit (spec §7.1, §9.1).

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::ns;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// `/.well-known/tar-registry` — what a peer reads before trusting us (spec §9.1).
pub async fn well_known(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    let peers = state.ops.list_peers(Some("active")).await.unwrap_or_default();
    let cfg = &state.config;
    let doc = json!({
        "@context": ns::jsonld_context(),
        "@id": cfg.base_iri,
        "@type": "dcat:Catalog",
        "title": cfg.title,
        "operator": cfg.operator,
        "base_iri": cfg.base_iri,
        "software": "tool-artifact-registry",
        "software_version": state.version,
        "public_read": cfg.public_read,
        "sparql_url": format!("{}/sparql", cfg.base_iri),
        "api_base": format!("{}/api/v1", cfg.base_iri),
        "capabilities": ["software", "instances", "artifacts", "runs", "capabilities", "federation", "openlineage", "sparql"],
        // How a client should authenticate. A peer or a tool reads this to learn whether it
        // needs a registry token or can present an OIDC token it already holds.
        "auth": {
            "anonymous_read": cfg.public_read,
            "api_tokens": true,
            "oidc": {
                "enabled": cfg.oidc.enabled(),
                "issuer": cfg.oidc.issuer,
                "client_id": cfg.oidc.client_id,
                "human_signin": cfg.oidc.human_signin_enabled(),
                "workload_issuers": cfg.oidc.accepted_issuers(),
                "audience": cfg.oidc.audience,
                "client_claim": cfg.oidc.client_claim,
                "scopes": crate::auth::ALL_SCOPES,
            }
        },
        "peers": peers.iter().map(|p| json!({"base_iri": p.base_iri, "title": p.title})).collect::<Vec<_>>(),
    });
    Ok(Json(doc))
}

pub async fn registry(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    let counts = entity_counts(&state)?;
    let peers = state.ops.list_peers(Some("active")).await.unwrap_or_default();
    Ok(Json(json!({
        "@context": ns::jsonld_context(),
        "@id": state.config.base_iri,
        "@type": "dcat:Catalog",
        "title": state.config.title,
        "operator": state.config.operator,
        "base_iri": state.config.base_iri,
        "software_version": state.version,
        "started_at": state.started_at.to_rfc3339(),
        "counts": counts,
        "peer_count": peers.len(),
        "triples": state.store.count().unwrap_or(0),
        "oidc_enabled": state.config.oidc.enabled(),
        "human_signin": state.config.oidc.human_signin_enabled(),
    })))
}

pub async fn context() -> impl IntoResponse {
    Json(json!({ "@context": ns::jsonld_context() }))
}

/// What a credential resolves to. The first thing to curl when a CI job gets a 403.
pub async fn whoami(principal: Principal) -> AppResult<impl IntoResponse> {
    Ok(Json(json!({
        "authenticated": !principal.is_anonymous(),
        "credential": principal.credential,
        "subject": principal.subject,
        "display_name": principal.display_name,
        "instance": principal.instance_iri,
        "issuer": principal.issuer,
        "scopes": principal.scopes,
        "roles": principal.roles,
        "is_curator": principal.is_curator(),
        "is_admin": principal.is_admin(),
    })))
}

fn entity_counts(state: &AppState) -> AppResult<serde_json::Value> {
    let mut out = serde_json::Map::new();
    // A Release is also a `schema:SoftwareApplication` (spec §4.2); the tar: marker types are
    // what tell them apart, here and in the SHACL targets.
    for (name, type_iri) in [
        ("software", crate::domain::software::TYPE_TAR_SOFTWARE),
        ("releases", crate::domain::software::TYPE_TAR_RELEASE),
        ("instances", crate::domain::instance::TYPE_TAR_INSTANCE),
        ("artifacts", crate::domain::artifact::TYPE_DATASET),
        ("runs", crate::domain::run::TYPE_ACTIVITY),
    ] {
        let q = format!(
            "{p}\nSELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{\n\
               GRAPH ?g {{ ?s a <{type_iri}> }}\n\
               FILTER NOT EXISTS {{ GRAPH ?tg {{ ?s tar:tombstoned true }} }} }}",
            p = ns::PREFIXES
        );
        let n = state
            .store
            .select(&q)
            .map_err(AppError::from)?
            .rows
            .first()
            .and_then(|r| r.i64("n"))
            .unwrap_or(0);
        out.insert(name.to_string(), json!(n));
    }
    Ok(serde_json::Value::Object(out))
}

pub async fn healthz() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

pub async fn readyz(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    state.store.count().map_err(|e| AppError::internal(format!("graph store not ready: {e}")))?;
    sqlx::query("SELECT 1").execute(state.ops.pool()).await.map_err(AppError::from)?;
    Ok(Json(json!({"status": "ready"})))
}

/// Prometheus exposition (spec §7.1). Deliberately small: the interesting operational signal
/// for a catalogue is how much it holds and how federation is doing.
pub async fn metrics(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    let counts = entity_counts(&state)?;
    let peers = state.ops.list_peers(None).await.unwrap_or_default();
    let failing = peers.iter().filter(|p| p.resolve_status == "error").count();
    let mut out = String::new();
    out.push_str("# HELP tar_triples Total triples in the graph store\n# TYPE tar_triples gauge\n");
    out.push_str(&format!("tar_triples {}\n", state.store.count().unwrap_or(0)));
    out.push_str("# HELP tar_entities Records by entity type\n# TYPE tar_entities gauge\n");
    for (k, v) in counts.as_object().into_iter().flatten() {
        out.push_str(&format!("tar_entities{{kind=\"{k}\"}} {v}\n"));
    }
    out.push_str("# HELP tar_peers Configured peer registries\n# TYPE tar_peers gauge\n");
    out.push_str(&format!("tar_peers {}\n", peers.iter().filter(|p| p.state == "active").count()));
    out.push_str("# HELP tar_peers_failing Peers whose last resolve attempt failed\n# TYPE tar_peers_failing gauge\n");
    out.push_str(&format!("tar_peers_failing {failing}\n"));
    out.push_str("# HELP tar_uptime_seconds Seconds since start\n# TYPE tar_uptime_seconds counter\n");
    out.push_str(&format!("tar_uptime_seconds {}\n", (chrono::Utc::now() - state.started_at).num_seconds()));
    Ok(([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], out))
}

#[derive(Deserialize)]
pub struct DumpQuery {
    pub graph: Option<String>,
}

/// N-Quads backup of every graph, or one (spec §10.6).
pub async fn dump(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<DumpQuery>,
) -> AppResult<impl IntoResponse> {
    principal.require_admin()?;
    let body = state.store.dump_nquads(q.graph.as_deref()).map_err(AppError::from)?;
    Ok(([(axum::http::header::CONTENT_TYPE, "application/n-quads")], body))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<i64>,
}

pub async fn audit(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<AuditQuery>,
) -> AppResult<impl IntoResponse> {
    principal.require_admin()?;
    let rows = state.ops.recent_audit(q.limit.unwrap_or(100).clamp(1, 1000)).await.map_err(AppError::from)?;
    Ok(Json(json!({"items": rows})))
}
