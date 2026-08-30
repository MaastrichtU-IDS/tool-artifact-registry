//! Vocabulary lookup for the pickers (handoff §5.7: "EDAM autocomplete plus a free-IRI escape
//! hatch").
//!
//! Nobody should have to paste `http://edamontology.org/topic_3071` by hand to say "data
//! management". EDAM's topic and data branches ship with the registry (`shapes/edam.ttl`), so
//! this searches locally: the picker keeps working on a laptop with no network, which is the
//! same promise the rest of the deployment makes.
//!
//! It searches whatever is in the vocabulary and local graphs, so registry-minted ArtifactTypes
//! (D11) appear alongside EDAM without any special casing.

use super::Paging;
use crate::domain::Ctx;
use crate::error::{AppError, AppResult};
use crate::ns;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct VocabQuery {
    pub q: Option<String>,
    /// `topic` restricts to EDAM topics (what a Software is about); `data` to things an
    /// artifact can be. Omit for everything, including local types.
    pub branch: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct VocabHit {
    pub iri: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    /// `edam` | `local` | `external`
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// How well it matched, so the caller can keep the ordering the server chose.
    pub score: f32,
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(q): Query<VocabQuery>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let needle = q.q.clone().unwrap_or_default();
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    if needle.trim().len() < 2 {
        // One character matches most of EDAM; make the caller be a little specific.
        return Ok(Json(VocabResults { items: Vec::new(), total: 0 }));
    }

    let branch_filter = match q.branch.as_deref() {
        Some(b) if !b.is_empty() => format!("?c tar:edamBranch \"{}\" .", super::escape_literal(b)),
        _ => String::new(),
    };
    // Match the label or any synonym; EDAM's altLabels are how people actually name things.
    let filter = super::text_filter(&needle, &["?label", "?alt"]);
    let sparql = format!(
        r#"{p}
SELECT DISTINCT ?c ?label ?def ?branch WHERE {{
  GRAPH ?g {{
    ?c a skos:Concept .
    {{ ?c skos:prefLabel ?label }} UNION {{ ?c rdfs:label ?label }}
    OPTIONAL {{ ?c skos:altLabel ?alt }}
    OPTIONAL {{ ?c skos:definition ?def }}
    OPTIONAL {{ ?c tar:edamBranch ?branch }}
    {branch_filter}
  }}
  {filter}
}} LIMIT 400"#,
        p = ns::PREFIXES
    );

    let rows = state.store.select(&sparql).map_err(AppError::from)?;
    let lower = needle.to_lowercase();
    let mut items: Vec<VocabHit> = Vec::new();
    for row in rows.rows {
        let (Some(iri), Some(label)) = (row.iri("c"), row.str("label")) else { continue };
        if items.iter().any(|h| h.iri == iri) {
            continue;
        }
        let l = label.to_lowercase();
        // An exact name beats a name that starts with the query, which beats one that merely
        // contains it; a synonym-only match ranks below all three.
        let score = if l == lower {
            1.0
        } else if l.starts_with(&lower) {
            0.8
        } else if l.contains(&lower) {
            0.6
        } else {
            0.3
        };
        items.push(VocabHit {
            source: crate::domain::type_source(ctx.base(), &iri),
            branch: row.str("branch"),
            definition: row.str("def"),
            label,
            iri,
            score,
        });
    }
    items.sort_by(|a, b| {
        b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.label.len().cmp(&b.label.len()))
    });
    let total = items.len();
    items.truncate(limit);
    Ok(Json(VocabResults { items, total: total as i64 }))
}

#[derive(Debug, Serialize)]
pub struct VocabResults {
    pub items: Vec<VocabHit>,
    pub total: i64,
}

/// Resolve a set of IRIs to labels in one call, so a form can render chips for values it was
/// given without searching for each one.
#[derive(Debug, Deserialize)]
pub struct ResolveQuery {
    /// Comma-separated IRIs.
    pub iris: String,
}

pub async fn resolve(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ResolveQuery>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iris: Vec<String> = q.iris.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).take(100).collect();
    Ok(Json(ctx.type_refs(&iris)))
}

/// Unused today, but keeps the paging import honest if the endpoint grows a cursor.
pub fn _paging(_p: &Paging) {}
