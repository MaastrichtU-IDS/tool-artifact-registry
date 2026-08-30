//! Search, capability matchmaking and subgraph extraction (spec §7.3, §7.7).

use crate::domain::Ctx;
use crate::error::{AppError, AppResult};
use crate::ids;
use crate::model::*;
use crate::ns;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    /// Restrict to one entity type: software | instance | artifact | run.
    #[serde(rename = "type")]
    pub entity_type: Option<String>,
    #[serde(default)]
    pub federated: bool,
    pub limit: Option<usize>,
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(sq): Query<SearchQuery>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let q = sq.q.clone().unwrap_or_default();
    if q.trim().is_empty() {
        return Ok(Json(SearchResults { query: q, hits: vec![], total: 0, partial: false, peers: vec![] }));
    }
    let limit = sq.limit.unwrap_or(30).clamp(1, 100);
    let mut hits = local_hits(&ctx, &q, sq.entity_type.as_deref(), limit)?;
    let mut peers_status = Vec::new();
    let mut partial = false;

    if sq.federated {
        let peers = state.ops.list_peers(Some("active")).await.unwrap_or_default();
        let timeout = state.config.federated_search_timeout;
        let futures: Vec<_> = peers
            .iter()
            .map(|p| {
                let client = state.http.clone();
                let url = format!("{}/api/v1/search?q={}", p.base_iri.trim_end_matches('/'), urlencode(&q));
                let peer = p.clone();
                async move {
                    let r = tokio::time::timeout(timeout, client.get(&url).send()).await;
                    (peer, r)
                }
            })
            .collect();
        for (peer, result) in futures::future::join_all(futures).await {
            match result {
                Err(_) => {
                    partial = true;
                    peers_status.push(PeerSearchStatus {
                        peer_id: peer.id.clone(),
                        base_iri: peer.base_iri.clone(),
                        title: peer.title.clone(),
                        status: "timeout".into(),
                        hits: 0,
                        error: Some(format!("no response within {}s", timeout.as_secs())),
                    });
                }
                Ok(Err(e)) => {
                    partial = true;
                    peers_status.push(PeerSearchStatus {
                        peer_id: peer.id.clone(),
                        base_iri: peer.base_iri.clone(),
                        title: peer.title.clone(),
                        status: "error".into(),
                        hits: 0,
                        error: Some(e.to_string()),
                    });
                }
                Ok(Ok(resp)) => {
                    let parsed = resp.json::<SearchResults>().await;
                    match parsed {
                        Ok(mut r) => {
                            let n = r.hits.len() as i64;
                            for h in r.hits.iter_mut() {
                                // Federated results are never silently interleaved as local
                                // (handoff §5.10): the origin chip carries the peer.
                                h.origin = Origin {
                                    kind: "peer".into(),
                                    peer_id: Some(peer.id.clone()),
                                    peer_title: peer.title.clone(),
                                    peer_base_iri: Some(peer.base_iri.clone()),
                                    cached_at: None,
                                    resolve_status: Some("live".into()),
                                };
                            }
                            hits.extend(r.hits);
                            peers_status.push(PeerSearchStatus {
                                peer_id: peer.id.clone(),
                                base_iri: peer.base_iri.clone(),
                                title: peer.title.clone(),
                                status: "ok".into(),
                                hits: n,
                                error: None,
                            });
                        }
                        Err(e) => {
                            partial = true;
                            peers_status.push(PeerSearchStatus {
                                peer_id: peer.id.clone(),
                                base_iri: peer.base_iri.clone(),
                                title: peer.title.clone(),
                                status: "error".into(),
                                hits: 0,
                                error: Some(format!("unreadable response: {e}")),
                            });
                        }
                    }
                }
            }
        }
    }

    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let total = hits.len() as i64;
    Ok(Json(SearchResults { query: q, hits, total, partial, peers: peers_status }))
}

fn urlencode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn local_hits(ctx: &Ctx, q: &str, only: Option<&str>, limit: usize) -> AppResult<Vec<SearchHit>> {
    let filter = super::text_filter(q, &["?title", "?extra"]);
    let mut hits = Vec::new();
    let kinds: Vec<(&str, &str, &str, &str)> = vec![
        ("software", crate::domain::software::TYPE_SOFTWARE, "schema:name", "tar:tagline"),
        ("instance", crate::domain::instance::TYPE_SOFTWARE_AGENT, "rdfs:label", "dct:description"),
        ("artifact", crate::domain::artifact::TYPE_DATASET, "dct:title", "dct:description"),
        ("run", crate::domain::run::TYPE_ACTIVITY, "rdfs:label", "tar:externalKey"),
    ];
    for (name, type_iri, title_pred, extra_pred) in kinds {
        if let Some(o) = only {
            if o != name {
                continue;
            }
        }
        let sparql = format!(
            r#"{p}
SELECT ?s ?title ?extra ?g WHERE {{
  GRAPH ?g {{ ?s a <{type_iri}> . OPTIONAL {{ ?s {title_pred} ?title }} OPTIONAL {{ ?s {extra_pred} ?extra }} }}
  {filter}
}} LIMIT {limit}"#,
            p = ns::PREFIXES
        );
        for row in ctx.state.store.select(&sparql).map_err(AppError::from)?.rows {
            let Some(iri) = row.iri("s") else { continue };
            let title = row.str("title").unwrap_or_else(|| ids::iri_tail(&iri).to_string());
            // Cheap relevance: exact match beats prefix beats contains.
            let lower = title.to_lowercase();
            let ql = q.to_lowercase();
            let score = if lower == ql {
                1.0
            } else if lower.starts_with(&ql) {
                0.8
            } else if lower.contains(&ql) {
                0.6
            } else {
                0.3
            };
            hits.push(SearchHit {
                iri,
                entity_type: name.to_string(),
                title,
                snippet: row.str("extra"),
                origin: ctx.origin(row.iri("g").as_deref()),
                score,
            });
        }
    }
    Ok(hits)
}

#[derive(Debug, Deserialize)]
pub struct CapabilityQuery {
    pub produces: Option<String>,
    pub consumes: Option<String>,
}

/// The matchmaking endpoint (spec §7.3): *what can consume what shacl-manager emits?*
/// It answers before any run exists, which is why D6 keeps capability separate from lineage.
pub async fn capabilities(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CapabilityQuery>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let mut clauses = String::new();
    if let Some(t) = q.produces.as_deref().filter(|v| !v.is_empty()) {
        clauses.push_str(&format!("?cap tar:produces <{t}> . "));
    }
    if let Some(t) = q.consumes.as_deref().filter(|v| !v.is_empty()) {
        clauses.push_str(&format!("?cap tar:consumes <{t}> . "));
    }
    if clauses.is_empty() {
        return Err(AppError::bad_request("give at least one of produces= or consumes= (an ArtifactType IRI)"));
    }
    let sparql = format!(
        r#"{p}
SELECT DISTINCT ?s ?cap WHERE {{ GRAPH ?g {{ ?s tar:hasCapability ?cap . {clauses} }} }} LIMIT 200"#,
        p = ns::PREFIXES
    );
    let rows = state.store.select(&sparql).map_err(AppError::from)?;
    let mut items = Vec::new();
    for row in rows.rows {
        let (Some(iri), Some(cap)) = (row.iri("s"), row.iri("cap")) else { continue };
        let quads = state.store.describe(&iri).map_err(AppError::from)?;
        let props = crate::rdf::Props::from_quads(&iri, &quads);
        let entity = if props.has_type(crate::domain::software::TYPE_SOFTWARE) {
            "software"
        } else if props.has_type(crate::domain::instance::TYPE_SOFTWARE_AGENT) {
            "instance"
        } else {
            "release"
        };
        items.push(serde_json::json!({
            "iri": iri,
            "entity_type": entity,
            "name": props.str(ns::SCHEMA, "name").or_else(|| props.str(ns::RDFS, "label")),
            "capability": crate::domain::software::capability_from(&ctx, &cap, entity),
            "origin": ctx.origin(props.graph.as_deref()),
        }));
    }
    let total = items.len();
    Ok(Json(serde_json::json!({ "items": items, "total": total })))
}

#[derive(Debug, Deserialize)]
pub struct GraphQuery {
    pub iri: String,
    pub depth: Option<i32>,
}

/// A subgraph around an IRI for UI rendering (spec §7.7).
pub async fn graph(
    State(state): State<Arc<AppState>>,
    Query(q): Query<GraphQuery>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let depth = q.depth.unwrap_or(1).clamp(1, 4);
    Ok(Json(crate::domain::artifact::lineage(&ctx, &q.iri, depth, "both")?))
}

/// Used by the peer resolver's timeout budget.
pub fn budget(d: Duration) -> Duration {
    d
}
