//! Search, capability matchmaking and subgraph extraction (spec §7.3, §7.7).
//!
//! Federated search *propagates*: a query reaches this registry's peers, their peers, and so
//! on to a configured hop budget. The loop prevention that makes that safe in a cyclic mesh
//! lives in `crate::ops::federation`; the protocol is written up in
//! `docs/specs/2026-08-31-federated-search-propagation.md`.

use crate::domain::Ctx;
use crate::error::{AppError, AppResult};
use crate::ids;
use crate::model::*;
use crate::ns;
use crate::ops::federation::{self, Claim, FedPeerStatus, FedSearchHit, FedSearchResults, FedSettings, FederationTrace};
use crate::ops::PeerRecord;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Below this there is no point paying for a round trip; ask for local results instead.
const MIN_USEFUL_HOP_BUDGET: Duration = Duration::from_millis(250);

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    /// Restrict to one entity type: software | instance | artifact | run.
    #[serde(rename = "type")]
    pub entity_type: Option<String>,
    #[serde(default)]
    pub federated: bool,
    pub limit: Option<usize>,

    // ---- propagation envelope (all optional; absent means "a user started this here") ----
    /// The query's identity, minted by the origin and carried unchanged across every hop.
    /// Seeing one twice is how a cycle is detected.
    pub fed_id: Option<String>,
    /// Hops we are still allowed to spend. Clamped to our own maximum on arrival, so a peer
    /// cannot grant us a bigger budget than it was given.
    pub fed_hops: Option<u32>,
    /// Base IRI of the registry where the query started. Advisory: for tracing only.
    pub fed_origin: Option<String>,
    /// Comma-separated base IRIs already on this query's path.
    pub fed_path: Option<String>,
    /// Milliseconds the caller will wait for us. We spend strictly less on our own fan-out.
    pub fed_budget_ms: Option<u64>,
}

/// `GET /api/v1/search`.
///
/// Three modes, distinguished by what the request carries:
///
/// * no `federated`, no `fed_id` — a plain local search. Nothing is claimed, nothing is
///   fanned out, and the response carries no `federation` block.
/// * `federated=true`, no `fed_id` — a user started a federated search here. We mint a query
///   id, claim it (so the query coming back round a cycle is refused *by us* too), and fan out.
/// * `fed_id=…` — a leg of someone else's query. We claim the id first; if we have already
///   handled it, we refuse explicitly and do no work at all.
pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(sq): Query<SearchQuery>,
) -> AppResult<Json<FedSearchResults>> {
    let ctx = Ctx::new(&state).await?;
    let q = sq.q.clone().unwrap_or_default();
    let fed = FedSettings::from_env();
    let me = state.base().trim_end_matches('/').to_string();

    // The id is attacker-controlled and is stored, echoed and logged. Refuse a bad one
    // outright: rewriting it would break the sender's own deduplication.
    let inbound_id = match sq.fed_id.as_deref() {
        Some(raw) if federation::valid_query_id(raw) => Some(raw.to_string()),
        Some(_) => {
            return Err(AppError::bad_request(
                "fed_id must be 1–100 characters of [A-Za-z0-9._:-] — it is a query identifier, not free text",
            ))
        }
        None => None,
    };
    // A request is a leg of a federated query if it is marked federated *or* carries an id.
    // A peer that sets only `fed_id` is asking for a local answer under that id, and it must
    // still be deduplicated: it is the same query.
    let is_relayed_leg = inbound_id.is_some();
    let participates = sq.federated || is_relayed_leg;

    if q.trim().is_empty() {
        return Ok(Json(FedSearchResults::empty(&q)));
    }

    let incoming_path = federation::parse_path(sq.fed_path.as_deref(), fed.max_hops as usize + 2);
    let received_from = incoming_path.last().cloned();
    let origin_iri = sq
        .fed_origin
        .as_deref()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty() && s.len() <= 200);
    let query_id = inbound_id.unwrap_or_else(federation::new_query_id);

    // ---------------------------------------------------------------- loop guard
    if participates {
        let claim = federation::claim_query(
            &state.ops,
            &query_id,
            origin_iri.as_deref(),
            received_from.as_deref(),
            fed.id_ttl,
            fed.max_seen_rows,
        )
        .await
        .map_err(|e| AppError::internal(format!("federated query bookkeeping failed: {e}")))?;

        if let Claim::AlreadyHandled { first_seen_at, repeat_count } = claim {
            // The explicit refusal. Not an empty result set, not a 5xx, not silence: the
            // caller is told which id was refused, when we first served it, and why zero
            // hits is the *correct* answer here — the results it is missing were returned
            // on the path that reached us first.
            let reason = format!(
                "{me} already handled federated query {query_id} at {first_seen_at}; \
                 its results were returned on the path that arrived first. This is the {} repeat, \
                 which means the peer graph contains a cycle.",
                ordinal(repeat_count)
            );
            tracing::debug!(query_id = %query_id, repeats = repeat_count, "refused a repeated federated query");
            return Ok(Json(FedSearchResults {
                query: q,
                hits: vec![],
                total: 0,
                partial: false,
                peers: vec![],
                already_handled: true,
                federation: Some(FederationTrace {
                    query_id,
                    origin: origin_iri,
                    registry: me,
                    max_hops: fed.max_hops,
                    hops_granted: 0,
                    hops_forwarded: 0,
                    budget_exhausted: false,
                    path: incoming_path,
                    first_seen_at: Some(first_seen_at),
                    reason: Some(reason),
                }),
            }));
        }
    }

    // ------------------------------------------------------------- local answer
    let limit = sq.limit.unwrap_or(30).clamp(1, 100);
    let mut hits: Vec<FedSearchHit> =
        local_hits(&ctx, &q, sq.entity_type.as_deref(), limit)?.into_iter().map(FedSearchHit::local).collect();
    let mut peers_status: Vec<FedPeerStatus> = Vec::new();
    let mut partial = false;

    // Hops we may still spend, clamped to *our* ceiling. This is the defence against a peer
    // that hands us a bigger budget than it was given: we never trust the number, we
    // intersect it with our own policy.
    let hops_granted = sq.fed_hops.unwrap_or(fed.max_hops).min(fed.max_hops);

    // Likewise for time. The caller told us how long it will wait; we answer inside that,
    // and give our own peers strictly less, so a chain of hops still fits in the origin's
    // per-peer timeout instead of multiplying by depth.
    let own_timeout = state.config.federated_search_timeout.min(fed.total_timeout);
    let time_budget = match sq.fed_budget_ms {
        Some(ms) => own_timeout.min(Duration::from_millis(ms.min(600_000))),
        None => own_timeout,
    };
    let forward_budget = time_budget.saturating_sub(fed.hop_margin);
    let hops_forwarded = if hops_granted > 0 && forward_budget >= MIN_USEFUL_HOP_BUDGET { hops_granted - 1 } else { 0 };

    let mut outgoing_path = incoming_path.clone();
    if !outgoing_path.iter().any(|e| e == &me) {
        outgoing_path.push(me.clone());
    }

    let mut trace = FederationTrace {
        query_id: query_id.clone(),
        origin: origin_iri.clone().or_else(|| Some(me.clone())),
        registry: me.clone(),
        max_hops: fed.max_hops,
        hops_granted,
        hops_forwarded,
        budget_exhausted: false,
        path: outgoing_path.clone(),
        first_seen_at: None,
        reason: None,
    };

    if sq.federated {
        let all_peers = state.ops.list_peers(Some("active")).await.unwrap_or_default();

        // Never ask a registry that is already on this query's path, or the origin, or
        // ourselves. The id check would refuse those anyway; skipping them saves the round
        // trip, and reporting them keeps the topology honest instead of hiding an edge.
        let (mut askable, on_path): (Vec<PeerRecord>, Vec<PeerRecord>) =
            all_peers.into_iter().partition(|p| {
                let b = p.base_iri.trim_end_matches('/');
                b != me && !outgoing_path.iter().any(|e| e == b) && Some(b) != origin_iri.as_deref()
            });
        for p in on_path.into_iter().take(fed.max_peers) {
            peers_status.push(FedPeerStatus {
                status: "skipped".into(),
                note: Some("already on this query's path — asking it would close a loop".into()),
                ..FedPeerStatus::direct(&p, "skipped")
            });
        }

        // A registry with hundreds of peers must not open hundreds of sockets per query.
        if askable.len() > fed.max_peers {
            for p in askable.split_off(fed.max_peers).into_iter().take(fed.max_peers) {
                peers_status.push(FedPeerStatus {
                    status: "skipped".into(),
                    note: Some(format!("fan-out capped at {} peers per query", fed.max_peers)),
                    ..FedPeerStatus::direct(&p, "skipped")
                });
            }
            partial = true;
        }

        if hops_granted == 0 && !askable.is_empty() {
            trace.budget_exhausted = true;
            for p in askable.iter() {
                peers_status.push(FedPeerStatus {
                    status: "skipped".into(),
                    note: Some("hop budget exhausted — the walk stops here".into()),
                    ..FedPeerStatus::direct(p, "skipped")
                });
            }
        } else if !askable.is_empty() {
            // Hops left but no time to spend them: the peers are asked for a local answer
            // only. That is still a bounded answer, and it says so.
            if hops_granted > 1 && hops_forwarded == 0 {
                trace.budget_exhausted = true;
            }
            let path_param = outgoing_path.join(",");
            let origin_param = origin_iri.clone().unwrap_or_else(|| me.clone());
            let futures: Vec<_> = askable
                .iter()
                .map(|p| {
                    let client = state.http.clone();
                    let mut url = format!(
                        "{}/api/v1/search?q={}&federated=true&fed_id={}&fed_hops={}&fed_origin={}&fed_path={}&fed_budget_ms={}",
                        p.base_iri.trim_end_matches('/'),
                        urlencode(&q),
                        urlencode(&query_id),
                        hops_forwarded,
                        urlencode(&origin_param),
                        urlencode(&path_param),
                        forward_budget.as_millis().max(1),
                    );
                    if let Some(t) = sq.entity_type.as_deref() {
                        url.push_str(&format!("&type={}", urlencode(t)));
                    }
                    url.push_str(&format!("&limit={limit}"));
                    let peer = p.clone();
                    let cap = fed.max_peer_bytes;
                    async move {
                        // One timeout over send *and* body read: a peer that answers headers
                        // instantly and then dribbles a gigabyte must not pin this task.
                        let r = tokio::time::timeout(time_budget, fetch_peer(&client, &url, cap)).await;
                        (peer, r)
                    }
                })
                .collect();

            for (peer, result) in futures::future::join_all(futures).await {
                match result {
                    Err(_) => {
                        partial = true;
                        peers_status.push(FedPeerStatus {
                            error: Some(format!("no response within {}ms", time_budget.as_millis())),
                            ..FedPeerStatus::direct(&peer, "timeout")
                        });
                    }
                    Ok(Err(e)) => {
                        partial = true;
                        peers_status.push(FedPeerStatus { error: Some(e), ..FedPeerStatus::direct(&peer, "error") });
                    }
                    Ok(Ok(r)) => {
                        if r.already_handled {
                            // The peer refused a repeat. That is a healthy answer, not a
                            // failure: some other path already covered it, so `partial`
                            // stays false and the caller sees exactly which edge was cut.
                            peers_status.push(FedPeerStatus {
                                note: r
                                    .federation
                                    .as_ref()
                                    .and_then(|f| f.reason.clone())
                                    .or_else(|| Some("peer had already handled this query id".into())),
                                ..FedPeerStatus::direct(&peer, "already_handled")
                            });
                            continue;
                        }
                        partial |= r.partial;
                        let taken: Vec<FedSearchHit> =
                            r.hits.into_iter().take(fed.max_peer_hits).map(|h| h.relayed(&peer)).collect();
                        let n = taken.len() as i64;
                        hits.extend(taken);
                        // The peer's own view of *its* peers, re-expressed from ours, so the
                        // caller can see the whole subtree that answered.
                        for s in r.peers.into_iter().take(fed.max_peer_statuses) {
                            if s.base_iri.trim_end_matches('/') == me {
                                continue;
                            }
                            peers_status.push(s.relayed(&peer));
                        }
                        peers_status.push(FedPeerStatus { hits: n, ..FedPeerStatus::direct(&peer, "ok") });
                    }
                }
            }
        }
    }

    // Two paths can deliver the same record (B and C both peer with D). Keep the most direct
    // copy — fewest hops is the strongest evidence — and count it once.
    let mut best: HashMap<(String, String), FedSearchHit> = HashMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    for h in hits {
        let key = (h.hit.iri.clone(), h.hit.entity_type.clone());
        match best.get(&key) {
            Some(existing) if existing.hops <= h.hops => {}
            Some(_) => {
                best.insert(key, h);
            }
            None => {
                order.push(key.clone());
                best.insert(key, h);
            }
        }
    }
    let mut hits: Vec<FedSearchHit> = order.into_iter().filter_map(|k| best.remove(&k)).collect();
    hits.sort_by(|a, b| b.hit.score.partial_cmp(&a.hit.score).unwrap_or(std::cmp::Ordering::Equal));
    if hits.len() > fed.max_total_hits {
        hits.truncate(fed.max_total_hits);
        partial = true;
    }
    let total = hits.len() as i64;
    Ok(Json(FedSearchResults {
        query: q,
        hits,
        total,
        partial,
        peers: peers_status,
        already_handled: false,
        federation: participates.then_some(trace),
    }))
}

fn ordinal(n: i64) -> String {
    match n {
        1 => "first".into(),
        2 => "second".into(),
        _ => format!("{n}th"),
    }
}

/// Fetch one peer's search response, refusing to buffer an unbounded body.
///
/// A peer is not trusted to be small. `Content-Length` is checked first when offered, and
/// the stream is cut off at `cap` regardless — a peer cannot make us allocate its way to an
/// out-of-memory kill by lying about, or omitting, the length.
async fn fetch_peer(client: &reqwest::Client, url: &str, cap: usize) -> Result<FedSearchResults, String> {
    let mut resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("peer returned {}", resp.status()));
    }
    if let Some(len) = resp.content_length() {
        if len > cap as u64 {
            return Err(format!("peer announced {len} bytes, over the {cap}-byte cap"));
        }
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > cap {
            return Err(format!("peer response exceeds the {cap}-byte cap"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<FedSearchResults>(&body).map_err(|e| format!("unreadable response: {e}"))
}

fn urlencode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn local_hits(ctx: &Ctx, q: &str, only: Option<&str>, limit: usize) -> AppResult<Vec<SearchHit>> {
    let filter = super::text_filter(q, &["?title", "?extra"]);
    let mut hits = Vec::new();
    let kinds: Vec<(&str, &str, &str, &str)> = vec![
        ("software", crate::domain::software::TYPE_SOFTWARE, "schema:name", "dct:abstract|tar:tagline"),
        ("instance", crate::domain::instance::TYPE_SOFTWARE_AGENT, "rdfs:label", "dct:description"),
        ("artifact", crate::domain::artifact::TYPE_DATASET, "dct:title", "dct:description"),
        ("run", crate::domain::run::TYPE_ACTIVITY, "rdfs:label", "dct:identifier|tar:externalKey"),
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
