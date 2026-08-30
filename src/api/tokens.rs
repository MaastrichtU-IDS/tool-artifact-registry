//! Per-Instance API tokens (spec §8.2, handoff §5.8).
//!
//! These remain the zero-dependency credential: a registry with no identity provider still
//! works. Where Keycloak (or any OIDC issuer) is available, bind the Instance to a client id
//! instead — see `PATCH /api/v1/instances/{id}` with `oidc_client_id`, and the addendum in
//! `docs/specs/2026-08-30-workload-identity-addendum.md`.

use crate::auth::{Principal, ALL_SCOPES};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct MintToken {
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// e.g. `90d`, `24h`. Omit for a non-expiring token.
    #[serde(default)]
    pub expires_in: Option<String>,
}

fn may_manage(principal: &Principal, instance_iri: &str) -> AppResult<()> {
    if principal.is_admin() || principal.is_curator() {
        return Ok(());
    }
    if principal.instance_iri.as_deref() == Some(instance_iri) {
        return Ok(());
    }
    Err(AppError::forbidden("only the instance owner, a curator or an admin may manage these tokens"))
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    may_manage(&principal, &iri)?;
    let tokens = state.ops.list_tokens(&iri).await.map_err(AppError::from)?;
    Ok(Json(json!({"items": tokens, "total": tokens.len()})))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(input): Json<MintToken>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    may_manage(&principal, &iri)?;
    if !state.store.exists(&iri).map_err(AppError::from)? {
        return Err(AppError::not_found(format!("no instance at {iri}")));
    }
    let scopes: Vec<String> = if input.scopes.is_empty() {
        vec!["advertise:produce".into(), "advertise:consume".into()]
    } else {
        input.scopes.clone()
    };
    for s in &scopes {
        if !ALL_SCOPES.contains(&s.as_str()) {
            return Err(AppError::bad_request(format!("unknown scope {s:?}; known: {}", ALL_SCOPES.join(", "))));
        }
        // Only an admin may hand out admin.
        if s == crate::auth::SCOPE_ADMIN && !principal.is_admin() {
            return Err(AppError::forbidden("only an admin can mint an admin:* token"));
        }
    }
    let ttl = match input.expires_in.as_deref() {
        Some(v) => Some(
            chrono::Duration::from_std(crate::config::parse_duration(v).map_err(|e| AppError::bad_request(e.to_string()))?)
                .map_err(|e| AppError::bad_request(e.to_string()))?,
        ),
        None => None,
    };
    let (rec, plaintext) = state
        .ops
        .mint_token(Some(&iri), "instance", &scopes, input.label.as_deref(), Some(&principal.subject), ttl)
        .await
        .map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "token.mint", Some(&iri), input.label.as_deref(), None)
        .await;
    // Shown exactly once (handoff §5.8).
    Ok((StatusCode::CREATED, Json(json!({ "token": plaintext, "record": rec, "shown_once": true }))))
}

pub async fn revoke(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((id, token_id)): Path<(String, String)>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    may_manage(&principal, &iri)?;
    let Some(rec) = state.ops.get_token(&token_id).await.map_err(AppError::from)? else {
        return Err(AppError::not_found("no such token"));
    };
    if rec.instance_iri.as_deref() != Some(iri.as_str()) {
        return Err(AppError::not_found("no such token for this instance"));
    }
    state.ops.revoke_token(&token_id).await.map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "token.revoke", Some(&iri), Some(&rec.prefix), None)
        .await;
    Ok(StatusCode::NO_CONTENT)
}
