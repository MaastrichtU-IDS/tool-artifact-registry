//! Local `ArtifactType` registration (spec D11, §4.4).
//!
//! `ArtifactType` is any IRI and EDAM is the recommended default, but life-science typing must
//! not be a hard dependency: a SHACL shapes graph, an RML mapping and a hash-chained patch log
//! have no EDAM term. Those get a registry-minted `skos:Concept` here, which is what makes the
//! chips in the UI render a label instead of an opaque IRI tail.

use super::Paging;
use crate::auth::Principal;
use crate::domain::Ctx;
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::ns;
use crate::rdf::{Node, Props};
use crate::state::AppState;
use crate::store::GraphTx;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const TYPE_CONCEPT: &str = "http://www.w3.org/2004/02/skos/core#Concept";

#[derive(Debug, Deserialize)]
pub struct ArtifactTypeIn {
    pub label: String,
    #[serde(default)]
    pub definition: Option<String>,
    #[serde(default)]
    pub default_media_type: Option<String>,
    /// A readable last path segment instead of a UUIDv7. Spec §4.4 mints UUIDs; a slug is
    /// allowed here because a type IRI is quoted in documentation and mapping files by hand,
    /// where `…/type/shacl-shapes-graph` survives review better than `…/type/01a05…`.
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ArtifactTypeOut {
    pub iri: String,
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_media_type: Option<String>,
    pub artifact_count: i64,
}

fn slugify(s: &str) -> String {
    let out: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    out.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(input): Json<ArtifactTypeIn>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    if input.label.trim().is_empty() {
        return Err(AppError::bad_request("an artifact type needs a label"));
    }
    let iri = match input.slug.as_deref().map(slugify).filter(|s| !s.is_empty()) {
        Some(slug) => format!("{}/type/{slug}", state.base()),
        None => ids::mint(state.base(), Kind::Type),
    };
    // Re-registering the same slug updates the label rather than minting a duplicate concept:
    // a type IRI is a name, and names are meant to be stable.
    let mut tx = GraphTx::new();
    tx.replace_subject(&iri, ns::G_LOCAL);
    let mut n = Node::local(&iri);
    n.a(TYPE_CONCEPT);
    n.text(ns::SKOS, "prefLabel", &input.label);
    n.opt_text(ns::SKOS, "definition", &input.definition);
    n.opt_text(ns::TAR, "defaultMediaType", &input.default_media_type);
    n.link(ns::PROV, "wasAttributedTo", &principal.subject);
    tx.extend(n.finish());
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "type.create", Some(&iri), Some(&input.label), None)
        .await;

    let ctx = Ctx::new(&state).await?;
    Ok((StatusCode::CREATED, Json(load(&ctx, &iri)?)))
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(paging): Query<Paging>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    // Every type actually in use, whether registry-minted or external (EDAM and friends), not
    // just the ones this registry happens to have minted a concept for.
    let q = format!(
        r#"{p}
SELECT DISTINCT ?s WHERE {{
  GRAPH ?g {{ {{ ?s a <{TYPE_CONCEPT}> }} UNION {{ ?a dct:conformsTo ?s }}
              UNION {{ ?c tar:produces ?s }} UNION {{ ?c tar:consumes ?s }} }}
}} ORDER BY STR(?s)"#,
        p = ns::PREFIXES
    );
    let rows = state.store.select(&q).map_err(AppError::from)?;
    let iris: Vec<String> = rows.rows.iter().filter_map(|r| r.iri("s")).collect();
    let total = iris.len() as i64;
    let items: Vec<ArtifactTypeOut> =
        iris.iter().take(paging.limit()).filter_map(|i| load(&ctx, i).ok()).collect();
    Ok(Json(crate::model::Page::new(items, total, None)))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Type, &id);
    Ok(Json(load(&ctx, &iri)?))
}

fn load(ctx: &Ctx, iri: &str) -> AppResult<ArtifactTypeOut> {
    let quads = ctx.state.store.describe(iri).map_err(AppError::from)?;
    let p = Props::from_quads(iri, &quads);
    let count = ctx
        .state
        .store
        .select(&format!(
            "{p}\nSELECT (COUNT(DISTINCT ?a) AS ?n) WHERE {{ GRAPH ?g {{ ?a dct:conformsTo <{iri}> }} }}",
            p = ns::PREFIXES
        ))
        .ok()
        .and_then(|b| b.rows.first().and_then(|r| r.i64("n")))
        .unwrap_or(0);
    let t = ctx.type_ref(iri);
    Ok(ArtifactTypeOut {
        id: ids::local_id(ctx.base(), iri).map(|(_, i)| i).unwrap_or_else(|| ids::iri_tail(iri).to_string()),
        label: t.label.unwrap_or_else(|| ids::iri_tail(iri).to_string()),
        definition: p.str(ns::SKOS, "definition").or(t.definition),
        default_media_type: p.str(ns::TAR, "defaultMediaType"),
        artifact_count: count,
        iri: iri.to_string(),
    })
}
