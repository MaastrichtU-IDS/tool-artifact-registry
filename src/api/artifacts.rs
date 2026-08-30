//! Artifact endpoints (spec §7.5).

use super::{count, page_iris, resource_response, Paging};
use crate::auth::Principal;
use crate::domain::{artifact as dom, Ctx};
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
pub struct ArtifactFilter {
    pub q: Option<String>,
    pub conforms_to: Option<String>,
    pub license: Option<String>,
    pub availability: Option<String>,
    pub instance: Option<String>,
    pub software: Option<String>,
    pub run: Option<String>,
    pub registry: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

fn where_body(base: &str, f: &ArtifactFilter) -> String {
    let mut w = format!(
        "GRAPH ?g {{ ?s a <{t}> . OPTIONAL {{ ?s dct:title ?title }} OPTIONAL {{ ?s dct:description ?desc }} }}",
        t = dom::TYPE_DATASET
    );
    if let Some(q) = &f.q {
        w.push('\n');
        w.push_str(&super::text_filter(q, &["?title", "?desc"]));
    }
    if let Some(t) = f.conforms_to.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!("\nGRAPH ?g {{ ?s dct:conformsTo <{t}> }}"));
    }
    if let Some(l) = f.license.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!("\nGRAPH ?g {{ ?s dct:license <{l}> }}"));
    }
    if let Some(a) = f.availability.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!(
            "\nGRAPH ?g {{ ?s dcat:distribution/tar:availability \"{}\" }}",
            super::escape_literal(a)
        ));
    }
    if let Some(i) = f.instance.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Instance, i);
        w.push_str(&format!("\nGRAPH ?g {{ ?s prov:wasGeneratedBy ?r . ?r tar:atInstance <{iri}> }}"));
    }
    if let Some(sw) = f.software.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Software, sw);
        w.push_str(&format!(
            "\nGRAPH ?g {{ ?s prov:wasGeneratedBy ?r . ?r tar:atInstance ?i . ?i tar:instanceOf <{iri}> }}"
        ));
    }
    if let Some(r) = f.run.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Run, r);
        w.push_str(&format!("\nGRAPH ?g {{ ?s prov:wasGeneratedBy <{iri}> }}"));
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
    Query(f): Query<ArtifactFilter>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let body = where_body(state.base(), &f);
    let (iris, next) = page_iris(&state, &body, &f.paging)?;
    let total = count(&state, &body)?;
    let items: Vec<Artifact> = iris.iter().filter_map(|i| dom::load_artifact(&ctx, i).ok()).collect();
    Ok(Json(Page::new(items, total, next)))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Artifact, &id);
    let artifact = dom::load_artifact(&ctx, &iri)?;
    let mut sp = Signposting::new(&iri).collection(&format!("{}/api/v1/artifacts", state.base()));
    if let Some(t) = &artifact.conforms_to {
        sp = sp.type_(&t.iri);
    }
    if let Some(l) = &artifact.license {
        sp = sp.license(l);
    }
    if let Some(p) = &artifact.publisher {
        sp = sp.author(&p.iri);
    }
    // rel="item" only where bytes exist (spec §6.3).
    for d in &artifact.distributions {
        if d.availability != "metadata-only" {
            if let Some(url) = d.download_url.as_deref().or(d.access_url.as_deref()) {
                sp = sp.item(url, d.media_type.as_deref());
            }
        }
    }
    Ok(resource_response(&state, &headers, &iri, &artifact, sp, Repr::Json)?)
}

/// Direct artifact registration (spec §7.5). Advertising through `/advertise/*` is the usual
/// path; this exists for a curator recording an artifact that predates the registry.
pub async fn create(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(input): Json<ArtifactIn>,
) -> AppResult<impl IntoResponse> {
    if !principal.is_curator() {
        principal.require_scope(crate::auth::SCOPE_ADVERTISE_PRODUCE)?;
    }
    let iri = ids::mint(state.base(), Kind::Artifact);
    let quads = dom::artifact_quads(state.base(), &iri, &input, &principal.subject, None);
    shacl::enforce(state.shapes.validate_quads(&quads), state.config.shacl_validate_writes)?;
    let mut tx = GraphTx::new();
    tx.extend(quads);
    state.store.apply(tx).map_err(AppError::from)?;
    if let Some(k) = &input.external_key {
        let _ = state.ops.remember_artifact(k, &iri).await;
    }
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "artifact.create", Some(&iri), input.title.as_deref(), None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok((StatusCode::CREATED, Json(dom::load_artifact(&ctx, &iri)?)))
}

#[derive(Debug, Deserialize)]
pub struct LineageQuery {
    pub depth: Option<i32>,
    pub direction: Option<String>,
}

pub async fn lineage(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<LineageQuery>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Artifact, &id);
    if !state.store.exists(&iri).map_err(AppError::from)? {
        return Err(AppError::not_found(format!("no artifact at {iri}")));
    }
    let depth = q.depth.unwrap_or(1).clamp(1, 6);
    let direction = q.direction.unwrap_or_else(|| "both".into());
    if !["up", "down", "both"].contains(&direction.as_str()) {
        return Err(AppError::bad_request("direction must be up, down or both"));
    }
    Ok(Json(dom::lineage(&ctx, &iri, depth, &direction)?))
}

/// Other artifacts in the same `dct:isVersionOf` series (handoff §5.5 "Versions").
pub fn version_series(ctx: &Ctx, series_iri: &str) -> Vec<ArtifactRef> {
    let q = format!(
        "{p}\nSELECT ?s WHERE {{ GRAPH ?g {{ ?s dct:isVersionOf <{series_iri}> }} }} ORDER BY DESC(STR(?s))",
        p = ns::PREFIXES
    );
    ctx.state
        .store
        .select(&q)
        .map(|b| b.rows.iter().filter_map(|r| r.iri("s")).map(|i| dom::artifact_ref(ctx, &i)).collect())
        .unwrap_or_default()
}

/// Read-time helper for the detail page: props of an artifact plus its series siblings.
pub fn artifact_props(state: &AppState, iri: &str) -> AppResult<Props> {
    let quads = state.store.describe(iri).map_err(AppError::from)?;
    Ok(Props::from_quads(iri, &quads))
}
