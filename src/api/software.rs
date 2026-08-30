//! Software, Release and Capability endpoints (spec §7.2, §7.3).

use super::{count, page_iris, resource_response, Paging};
use crate::auth::Principal;
use crate::domain::{software as dom, Ctx};
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
pub struct SoftwareFilter {
    pub q: Option<String>,
    pub license: Option<String>,
    pub publisher: Option<String>,
    pub edam_topic: Option<String>,
    pub keyword: Option<String>,
    pub kind: Option<String>,
    /// Matchmaking passthrough: software whose capability produces/consumes a type.
    pub produces: Option<String>,
    pub consumes: Option<String>,
    pub registry: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

fn where_body(f: &SoftwareFilter) -> String {
    let mut w = format!(
        "GRAPH ?g {{ ?s a <{t}> ; schema:name ?name . \
         OPTIONAL {{ ?s tar:tagline ?tagline }} OPTIONAL {{ ?s schema:description ?desc }} }}",
        t = dom::TYPE_SOFTWARE
    );
    if let Some(q) = &f.q {
        w.push('\n');
        w.push_str(&super::text_filter(q, &["?name", "?tagline", "?desc"]));
    }
    for (value, pattern) in [
        (&f.license, "GRAPH ?g {{ ?s dct:license <{v}> }}"),
        (&f.publisher, "GRAPH ?g {{ ?s dct:publisher <{v}> }}"),
        (&f.edam_topic, "GRAPH ?g {{ ?s dct:subject <{v}> }}"),
        (&f.produces, "GRAPH ?g {{ ?s tar:hasCapability/tar:produces <{v}> }}"),
        (&f.consumes, "GRAPH ?g {{ ?s tar:hasCapability/tar:consumes <{v}> }}"),
    ] {
        if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
            w.push('\n');
            w.push_str(&pattern.replace("{{", "{").replace("}}", "}").replace("{v}", v));
        }
    }
    if let Some(k) = f.keyword.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!("\nGRAPH ?g {{ ?s schema:keywords \"{}\" }}", super::escape_literal(k)));
    }
    if let Some(k) = f.kind.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!("\nGRAPH ?g {{ ?s tar:kind \"{}\" }}", super::escape_literal(k)));
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
    Query(f): Query<SoftwareFilter>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let body = where_body(&f);
    let (iris, next) = page_iris(&state, &body, &f.paging)?;
    let total = count(&state, &body)?;
    let counts = dom::software_counts(&ctx, None)?;
    let mut items = Vec::new();
    for iri in iris {
        let quads = state.store.describe(&iri).map_err(AppError::from)?;
        let p = Props::from_quads(&iri, &quads);
        let mut sw = dom::software_from_props(&ctx, &iri, &p);
        if let Some(c) = counts.get(&iri) {
            sw.instance_count = c.instances;
            sw.runs_30d = c.runs_30d;
        }
        items.push(sw);
    }
    let mut page = Page::new(items, total, next);
    page.facets = facets(&state)?;
    Ok(Json(page))
}

fn facets(state: &AppState) -> AppResult<Vec<Facet>> {
    let mut out = Vec::new();
    for (name, predicate) in [("license", "dct:license"), ("kind", "tar:kind"), ("edam_topic", "dct:subject")] {
        let q = format!(
            "{p}\nSELECT ?v (COUNT(DISTINCT ?s) AS ?n) WHERE {{ GRAPH ?g {{ ?s a <{t}> ; {predicate} ?v }} }} GROUP BY ?v ORDER BY DESC(?n) LIMIT 25",
            p = ns::PREFIXES,
            t = dom::TYPE_SOFTWARE
        );
        let rows = state.store.select(&q).map_err(AppError::from)?;
        let values: Vec<FacetValue> = rows
            .rows
            .iter()
            .filter_map(|r| {
                let v = r.str("v")?;
                Some(FacetValue { label: Some(ids::iri_tail(&v).to_string()), value: v, count: r.i64("n").unwrap_or(0) })
            })
            .collect();
        if !values.is_empty() {
            out.push(Facet { name: name.to_string(), values });
        }
    }
    Ok(out)
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    let sw = dom::load_software(&ctx, &iri)?;
    let mut sp = Signposting::new(&iri).collection(&format!("{}/api/v1/software", state.base()));
    if let Some(l) = &sw.license {
        sp = sp.license(l);
    }
    if let Some(p) = &sw.publisher {
        sp = sp.author(&p.iri);
    }
    if let Some(r) = &sw.code_repository {
        sp = sp.item(r, None);
    }
    Ok(resource_response(&state, &headers, &iri, &sw, sp, Repr::Json)?)
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(input): Json<SoftwareIn>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::mint(state.base(), Kind::Software);
    let quads = dom::software_quads(state.base(), &iri, &input, &principal.subject, None);
    shacl::enforce(state.shapes.validate_quads(&quads), state.config.shacl_validate_writes)?;
    let mut tx = GraphTx::new();
    tx.extend(quads);
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "software.create", Some(&iri), Some(&input.name), None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok((StatusCode::CREATED, Json(dom::load_software(&ctx, &iri)?)))
}

pub async fn patch(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(input): Json<SoftwareIn>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    if !ids::is_local(state.base(), &iri) {
        return Err(AppError::forbidden("this registry is not authoritative for that IRI (spec §9.7)"));
    }
    let existing = state.store.describe(&iri).map_err(AppError::from)?;
    if existing.is_empty() {
        return Err(AppError::not_found(format!("no software at {iri}")));
    }
    let created = Props::from_quads(&iri, &existing).str(ns::DCT, "created");
    let tx = dom::replace_software(state.base(), &iri, &input, &principal.subject, created);
    shacl::enforce(state.shapes.validate_quads(&tx.insert), state.config.shacl_validate_writes)?;
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "software.update", Some(&iri), None, None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok(Json(dom::load_software(&ctx, &iri)?))
}

/// Soft delete (spec §7.2): the IRI keeps resolving and the UI renders a tombstone banner.
pub async fn soft_delete(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    tombstone(&state, &iri, &principal).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn tombstone(state: &AppState, iri: &str, principal: &Principal) -> AppResult<()> {
    let existing = state.store.describe(iri).map_err(AppError::from)?;
    if existing.is_empty() {
        return Err(AppError::not_found(format!("nothing at {iri}")));
    }
    let mut n = crate::rdf::Node::local(iri);
    n.boolean(ns::TAR, "tombstoned", true);
    n.datetime(ns::TAR, "tombstonedAt", &chrono::Utc::now().to_rfc3339());
    n.link(ns::PROV, "wasAttributedTo", &principal.subject);
    let mut tx = GraphTx::new();
    tx.extend(n.finish());
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "tombstone", Some(iri), None, None)
        .await;
    Ok(())
}

pub async fn list_releases(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    let releases = dom::list_releases(&ctx, &iri)?;
    let total = releases.len() as i64;
    Ok(Json(Page::new(releases, total, None)))
}

pub async fn create_release(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(input): Json<ReleaseIn>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let software_iri = ids::iri_for(state.base(), Kind::Software, &id);
    if !state.store.exists(&software_iri).map_err(AppError::from)? {
        return Err(AppError::not_found(format!("no software at {software_iri}")));
    }
    let iri = ids::mint(state.base(), Kind::Release);
    let quads = dom::release_quads(state.base(), &iri, &software_iri, &input, &principal.subject);
    shacl::enforce(state.shapes.validate_quads(&quads), state.config.shacl_validate_writes)?;
    let mut tx = GraphTx::new();
    tx.extend(quads);
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "release.create", Some(&iri), Some(&input.version), None)
        .await;
    let ctx = Ctx::new(&state).await?;
    let quads = state.store.describe(&iri).map_err(AppError::from)?;
    let p = Props::from_quads(&iri, &quads);
    Ok((StatusCode::CREATED, Json(dom::release_from_props(&ctx, &iri, &p))))
}

/// Declare produces[]/consumes[] at the Software layer (spec §7.3).
pub async fn put_capability(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(input): Json<CapabilityIn>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    put_capability_on(&state, &principal, &iri, &input, "software").await
}

pub async fn put_capability_on(
    state: &Arc<AppState>,
    principal: &Principal,
    subject: &str,
    input: &CapabilityIn,
    layer: &str,
) -> AppResult<axum::response::Response> {
    let existing = state.store.describe(subject).map_err(AppError::from)?;
    if existing.is_empty() {
        return Err(AppError::not_found(format!("nothing at {subject}")));
    }
    let props = Props::from_quads(subject, &existing);
    let cap_iri = props.iri(ns::TAR, "hasCapability").unwrap_or_else(|| ids::mint(state.base(), Kind::Capability));
    let cap_quads = dom::capability_quads(&cap_iri, input);
    shacl::enforce(state.shapes.validate_quads(&cap_quads), state.config.shacl_validate_writes)?;
    let mut tx = GraphTx::new();
    tx.replace_subject(&cap_iri, ns::G_LOCAL);
    tx.extend(cap_quads);
    let mut n = crate::rdf::Node::local(subject);
    n.link(ns::TAR, "hasCapability", &cap_iri);
    tx.extend(n.finish());
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "capability.declare", Some(subject), Some(layer), None)
        .await;
    let ctx = Ctx::new(state).await?;
    let cap = dom::capability_from(&ctx, &cap_iri, layer);
    Ok(Json(cap).into_response())
}

/// biotoolsSchema export (spec §2.5, §7.2) — our descriptions can populate bio.tools or an
/// RSD instance with no runtime dependency in either direction.
pub async fn export_biotools(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    let sw = dom::load_software(&ctx, &iri)?;

    let function = sw.capability.as_ref().map(|c| {
        serde_json::json!([{
            "operation": [],
            "input": c.consumes.iter().map(|t| serde_json::json!({"data": {"uri": t.iri, "term": t.label}})).collect::<Vec<_>>(),
            "output": c.produces.iter().map(|t| serde_json::json!({"data": {"uri": t.iri, "term": t.label}})).collect::<Vec<_>>(),
        }])
    });
    let doc = serde_json::json!({
        "name": sw.name,
        "biotoolsID": sw.id,
        "description": sw.description.clone().or_else(|| sw.tagline.clone()),
        "homepage": sw.homepage,
        "toolType": sw.kind.as_deref().map(|k| vec![match k {
            "service" => "Web service",
            "cli" => "Command-line tool",
            "library" => "Library",
            "workflow" => "Workflow",
            _ => "Command-line tool",
        }]).unwrap_or_default(),
        "topic": sw.edam_topics.iter().map(|t| serde_json::json!({"uri": t.iri, "term": t.label})).collect::<Vec<_>>(),
        "function": function,
        "license": sw.license.as_deref().map(ids::iri_tail),
        "version": sw.latest_release.as_ref().map(|r| vec![r.version.clone()]).unwrap_or_default(),
        "documentation": sw.documentation.map(|d| vec![serde_json::json!({"url": d, "type": ["General"]})]).unwrap_or_default(),
        "download": sw.latest_release.as_ref().and_then(|r| r.container_image.clone())
            .map(|img| vec![serde_json::json!({"url": img, "type": "Container file"})]).unwrap_or_default(),
        "link": sw.code_repository.map(|r| vec![serde_json::json!({"url": r, "type": ["Repository"]})]).unwrap_or_default(),
        "credit": sw.publisher.map(|p| vec![serde_json::json!({"name": p.name, "typeEntity": "Institute", "url": p.homepage, "orcidid": p.identifier})]).unwrap_or_default(),
        "publication": sw.publications.iter().map(|p| serde_json::json!({"doi": p})).collect::<Vec<_>>(),
    });
    Ok(Json(doc))
}
