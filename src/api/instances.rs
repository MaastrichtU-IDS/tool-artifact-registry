//! Instance endpoints (spec §7.4). An Instance is the deployment that acts, and — under the
//! workload-identity model — the thing an OIDC client id binds to.

use super::{count, page_iris, resource_response, Paging};
use crate::auth::Principal;
use crate::domain::{instance as dom, run as rundom, Ctx};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::model::*;
use crate::negotiate::{Repr, Signposting};
use crate::ns;
use crate::rdf::Props;
use crate::shacl;
use crate::state::AppState;
use crate::store::GraphTx;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize, Default)]
pub struct InstanceFilter {
    pub q: Option<String>,
    pub software: Option<String>,
    pub operator: Option<String>,
    pub status: Option<String>,
    pub release: Option<String>,
    pub registry: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

fn where_body(base: &str, f: &InstanceFilter) -> String {
    let mut w = format!(
        "GRAPH ?g {{ ?s a <{t}> ; rdfs:label ?label }}",
        t = dom::TYPE_SOFTWARE_AGENT
    );
    if let Some(q) = &f.q {
        w.push('\n');
        w.push_str(&super::text_filter(q, &["?label"]));
    }
    if let Some(sw) = f.software.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Software, sw);
        w.push_str(&format!("\nGRAPH ?g {{ ?s tar:instanceOf <{iri}> }}"));
    }
    if let Some(r) = f.release.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Release, r);
        w.push_str(&format!("\nGRAPH ?g {{ ?s tar:runsRelease <{iri}> }}"));
    }
    if let Some(o) = f.operator.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!("\nGRAPH ?g {{ ?s dct:publisher <{o}> }}"));
    }
    if let Some(s) = f.status.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!("\nGRAPH ?g {{ ?s tar:health \"{}\" }}", super::escape_literal(s)));
    }
    match f.registry.as_deref() {
        Some("local") => w.push_str(&format!("\nFILTER(?g = <{}>)", ns::G_LOCAL)),
        Some(peer) if !peer.is_empty() => w.push_str(&format!("\nFILTER(?g = <{}>)", ns::peer_graph(peer))),
        _ => {}
    }
    w.push_str(&format!("\n{}", f.paging.cursor_filter("?s")));
    w
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(f): Query<InstanceFilter>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let body = where_body(state.base(), &f);
    let (iris, next) = page_iris(&state, &body, &f.paging)?;
    let total = count(&state, &body)?;
    let signals = dom::instance_signals(&ctx, None)?;
    let mut items = Vec::new();
    for iri in iris {
        let quads = state.store.describe(&iri).map_err(AppError::from)?;
        let p = Props::from_quads(&iri, &quads);
        let mut i = dom::instance_from_props(&ctx, &iri, &p);
        if let Some(s) = signals.get(&iri) {
            i.last_run_at = s.last_run_at.clone();
            i.runs_30d = s.runs_30d;
            i.failures_30d = s.failures_30d;
            i.artifact_count = s.artifacts;
        }
        items.push(i);
    }
    dom::decorate(&ctx, &mut items)?;
    Ok(Json(Page::new(items, total, next)))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    principal: Principal,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    let mut inst = dom::load_instance(&ctx, &iri)?;
    // Token count is operational state; only someone who could manage them needs it.
    if principal.is_curator() || principal.instance_iri.as_deref() == Some(iri.as_str()) {
        inst.token_count = state.ops.list_tokens(&iri).await.map(|t| t.iter().filter(|x| x.revoked_at.is_none()).count() as i64).unwrap_or(0);
    }
    let mut sp = Signposting::new(&iri).collection(&format!("{}/api/v1/instances", state.base()));
    if let Some(e) = &inst.endpoint_url {
        sp = sp.item(e, None);
    }
    if let Some(o) = &inst.operator {
        sp = sp.author(&o.iri);
    }
    Ok(resource_response(&state, &headers, &iri, &inst, sp, Repr::Json)?)
}

fn resolve_software_for(state: &AppState, input: &InstanceIn) -> AppResult<Option<String>> {
    if let Some(sw) = input.software.as_deref().filter(|v| !v.is_empty()) {
        return Ok(Some(ids::iri_for(state.base(), Kind::Software, sw)));
    }
    // Derive from the Release when only that was given.
    if let Some(r) = input.release.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(state.base(), Kind::Release, r);
        let quads = state.store.describe(&iri).map_err(AppError::from)?;
        return Ok(Props::from_quads(&iri, &quads).iri(ns::DCT, "isVersionOf"));
    }
    Ok(None)
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(mut input): Json<InstanceIn>,
) -> AppResult<impl IntoResponse> {
    if !principal.is_curator() {
        principal.require_scope(crate::auth::SCOPE_REGISTER_INSTANCE)?;
    }
    // Accept a bare id or a full IRI in `software` / `release`.
    if let Some(sw) = input.software.clone() {
        input.software = Some(ids::iri_for(state.base(), Kind::Software, &sw));
    }
    if let Some(r) = input.release.clone() {
        input.release = Some(ids::iri_for(state.base(), Kind::Release, &r));
    }
    let iri = ids::mint(state.base(), Kind::Instance);
    let software = resolve_software_for(&state, &input)?;
    let quads = dom::instance_quads(state.base(), &iri, &input, &principal.subject, software.as_deref());
    shacl::enforce(state.shapes.validate_quads(&quads), state.config.shacl_validate_writes)?;
    let mut tx = GraphTx::new();
    tx.extend(quads);
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "instance.create", Some(&iri), Some(&input.label), None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok((StatusCode::CREATED, Json(dom::load_instance(&ctx, &iri)?)))
}

pub async fn patch(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(mut input): Json<InstanceIn>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    // A deployment may maintain its own record; anyone else needs curator.
    if principal.instance_iri.as_deref() != Some(iri.as_str()) {
        principal.require_curator()?;
    }
    if !ids::is_local(state.base(), &iri) {
        return Err(AppError::forbidden("this registry is not authoritative for that IRI (spec §9.7)"));
    }
    if !state.store.exists(&iri).map_err(AppError::from)? {
        return Err(AppError::not_found(format!("no instance at {iri}")));
    }
    if let Some(sw) = input.software.clone() {
        input.software = Some(ids::iri_for(state.base(), Kind::Software, &sw));
    }
    if let Some(r) = input.release.clone() {
        input.release = Some(ids::iri_for(state.base(), Kind::Release, &r));
    }
    let software = resolve_software_for(&state, &input)?;
    let tx = dom::replace_instance(state.base(), &iri, &input, &principal.subject, software.as_deref());
    shacl::enforce(state.shapes.validate_quads(&tx.insert), state.config.shacl_validate_writes)?;
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "instance.update", Some(&iri), None, None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok(Json(dom::load_instance(&ctx, &iri)?))
}

pub async fn soft_delete(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    super::software::tombstone(&state, &iri, &principal).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Narrow the capability inherited from the Software (spec §7.3).
pub async fn put_capability(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(input): Json<CapabilityIn>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    if principal.instance_iri.as_deref() != Some(iri.as_str()) {
        principal.require_curator()?;
    }
    super::software::put_capability_on(&state, &principal, &iri, &input, "instance").await
}

pub async fn runs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(paging): Query<Paging>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    let body = format!(
        "GRAPH ?g {{ ?s a <{t}> ; tar:atInstance <{iri}> }}\n{}",
        paging.cursor_filter("?s"),
        t = rundom::TYPE_ACTIVITY
    );
    let (iris, next) = page_iris(&state, &body, &paging)?;
    let total = count(&state, &body)?;
    let mut items = Vec::new();
    for r in iris {
        if let Ok(s) = rundom::load_run_summary(&ctx, &r) {
            items.push(s);
        }
    }
    Ok(Json(Page::new(items, total, next)))
}

pub async fn artifacts(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(paging): Query<Paging>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    let body = format!(
        "GRAPH ?g {{ ?run tar:atInstance <{iri}> . ?s prov:wasGeneratedBy ?run }}\n{}",
        paging.cursor_filter("?s")
    );
    let (iris, next) = page_iris(&state, &body, &paging)?;
    let total = count(&state, &body)?;
    let items: Vec<Artifact> = iris
        .iter()
        .filter_map(|a| crate::domain::artifact::load_artifact(&ctx, a).ok())
        .collect();
    Ok(Json(Page::new(items, total, next)))
}
