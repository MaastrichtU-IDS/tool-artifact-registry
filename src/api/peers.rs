//! Federation endpoints (spec §7.8, §9).
//!
//! Peer data is always a read-only stub in `<urn:tar:peer:{id}>` and is never merged into
//! `<urn:tar:local>`. A peer cannot create, modify or delete local records; an inbound
//! announcement only produces a *suggestion* for admin review.

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::ns;
use crate::ops::PeerRecord;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub async fn list(State(state): State<Arc<AppState>>, principal: Principal) -> AppResult<impl IntoResponse> {
    principal.require_admin()?;
    let peers = state.ops.list_peers(Some("active")).await.map_err(AppError::from)?;
    Ok(Json(json!({ "items": peers, "total": peers.len() })))
}

pub async fn suggested(State(state): State<Arc<AppState>>, principal: Principal) -> AppResult<impl IntoResponse> {
    principal.require_admin()?;
    let peers = state.ops.list_peers(Some("suggested")).await.map_err(AppError::from)?;
    Ok(Json(json!({ "items": peers, "total": peers.len() })))
}

#[derive(Debug, Deserialize)]
pub struct AddPeer {
    pub base_url: String,
    /// Preview only: fetch and validate the well-known document without storing anything.
    /// The add-peer flow shows what will be trusted before the admin confirms (handoff §5.9).
    #[serde(default)]
    pub preview: bool,
    /// Announce ourselves back for mutual discovery (spec §9.2).
    #[serde(default = "default_true")]
    pub announce: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, serde::Deserialize)]
struct WellKnown {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    operator: Option<String>,
    #[serde(default)]
    base_iri: Option<String>,
    #[serde(default)]
    software_version: Option<String>,
    #[serde(default)]
    peers: Vec<PeerStub>,
}

#[derive(Debug, serde::Deserialize)]
struct PeerStub {
    base_iri: String,
    #[serde(default)]
    title: Option<String>,
}

async fn fetch_well_known(state: &AppState, base_url: &str) -> AppResult<WellKnown> {
    let url = format!("{}/.well-known/tar-registry", base_url.trim_end_matches('/'));
    let resp =
        state.http.get(&url).send().await.map_err(|e| AppError::bad_request(format!("cannot reach {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::bad_request(format!("{url} returned {}", resp.status())));
    }
    resp.json::<WellKnown>()
        .await
        .map_err(|e| AppError::bad_request(format!("{url} is not a registry self-description: {e}")))
}

pub async fn add(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(input): Json<AddPeer>,
) -> AppResult<impl IntoResponse> {
    principal.require_admin()?;
    let base_url = input.base_url.trim_end_matches('/').to_string();
    if base_url == state.config.base_iri {
        return Err(AppError::bad_request("that is this registry"));
    }
    let wk = fetch_well_known(&state, &base_url).await?;
    // The advertised base IRI must match what we were given, or the peer's own IRIs would not
    // dereference where we think they do (spec §9.2).
    if let Some(advertised) = wk.base_iri.as_deref() {
        if advertised.trim_end_matches('/') != base_url {
            return Err(AppError::conflict(format!(
                "peer advertises base IRI {advertised}, which is not the URL given ({base_url}); \
                 cross-links would not resolve"
            )));
        }
    }
    if input.preview {
        return Ok((
            StatusCode::OK,
            Json(json!({
                "preview": true,
                "base_iri": base_url,
                "title": wk.title,
                "operator": wk.operator,
                "software_version": wk.software_version,
                "peers_of_peer": wk.peers.iter().map(|p| &p.base_iri).collect::<Vec<_>>(),
            })),
        ));
    }

    let id = uuid::Uuid::now_v7().to_string();
    let existing = state.ops.get_peer(&base_url).await.map_err(AppError::from)?;
    let record = PeerRecord {
        id: existing.as_ref().map(|p| p.id.clone()).unwrap_or(id),
        base_iri: base_url.clone(),
        title: wk.title.clone(),
        operator: wk.operator.clone(),
        added_at: existing.as_ref().map(|p| p.added_at.clone()).unwrap_or_else(|| Utc::now().to_rfc3339()),
        last_seen_at: Some(Utc::now().to_rfc3339()),
        resolve_status: "ok".into(),
        last_error: None,
        record_count: existing.as_ref().map(|p| p.record_count).unwrap_or(0),
        state: "active".into(),
        suggested_by: None,
        stale: false,
    };
    state.ops.upsert_peer(&record).await.map_err(AppError::from)?;

    // Peers of peers become suggestions, never auto-added (spec §9.5, D9).
    for p in &wk.peers {
        let b = p.base_iri.trim_end_matches('/').to_string();
        if b == state.config.base_iri || state.ops.get_peer(&b).await.map_err(AppError::from)?.is_some() {
            continue;
        }
        let s = PeerRecord {
            id: uuid::Uuid::now_v7().to_string(),
            base_iri: b,
            title: p.title.clone(),
            operator: None,
            added_at: Utc::now().to_rfc3339(),
            last_seen_at: None,
            resolve_status: "unknown".into(),
            last_error: None,
            record_count: 0,
            state: "suggested".into(),
            suggested_by: Some(base_url.clone()),
            stale: false,
        };
        let _ = state.ops.upsert_peer(&s).await;
    }

    if input.announce {
        let url = format!("{}/api/v1/peers/announce", base_url);
        let _ = state
            .http
            .post(&url)
            .json(&json!({"base_url": state.config.base_iri, "title": state.config.title}))
            .send()
            .await;
    }

    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "peer.add", Some(&base_url), wk.title.as_deref(), None)
        .await;
    Ok((StatusCode::CREATED, Json(json!(record))))
}

#[derive(Debug, Deserialize)]
pub struct Announce {
    pub base_url: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// Inbound mutual discovery. Produces a suggestion for admin review — never a peer
/// (spec §8.4). Deliberately unauthenticated: it grants nothing.
pub async fn announce(State(state): State<Arc<AppState>>, Json(input): Json<Announce>) -> AppResult<impl IntoResponse> {
    let base = input.base_url.trim_end_matches('/').to_string();
    if base == state.config.base_iri {
        return Ok(Json(json!({"accepted": false, "reason": "that is this registry"})));
    }
    if let Some(existing) = state.ops.get_peer(&base).await.map_err(AppError::from)? {
        return Ok(Json(json!({"accepted": true, "state": existing.state, "note": "already known"})));
    }
    let s = PeerRecord {
        id: uuid::Uuid::now_v7().to_string(),
        base_iri: base.clone(),
        title: input.title.clone(),
        operator: None,
        added_at: Utc::now().to_rfc3339(),
        last_seen_at: None,
        resolve_status: "unknown".into(),
        last_error: None,
        record_count: 0,
        state: "suggested".into(),
        suggested_by: Some("announce".into()),
        stale: false,
    };
    state.ops.upsert_peer(&s).await.map_err(AppError::from)?;
    let _ = state.ops.audit(None, "anonymous", "peer.announce", Some(&base), input.title.as_deref(), None).await;
    Ok(Json(json!({"accepted": true, "state": "suggested", "note": "queued for admin review"})))
}

/// Removing a peer drops its cached graph (spec §7.8) — destructive, and the UI names the
/// record count before confirming.
pub async fn remove(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    principal.require_admin()?;
    let Some(peer) = state.ops.get_peer(&id).await.map_err(AppError::from)? else {
        return Err(AppError::not_found(format!("no peer {id}")));
    };
    super::blocking({
        let (state, peer_id) = (state.clone(), peer.id.clone());
        move || state.store.drop_graph(&ns::peer_graph(&peer_id)).map_err(AppError::from)
    })
    .await?;
    state.ops.delete_peer(&peer.id).await.map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "peer.remove", Some(&peer.base_iri), None, None)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct ResolveQuery {
    pub iri: String,
    #[serde(default)]
    pub refresh: bool,
}

/// Resolve a foreign IRI, cache a stub in the peer graph, return it (spec §7.8, §9.4).
pub async fn resolve(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ResolveQuery>,
) -> AppResult<impl IntoResponse> {
    let iri = q.iri.clone();
    if crate::ids::is_local(state.base(), &iri) {
        return Err(AppError::bad_request("that IRI is local — dereference it directly"));
    }
    if !q.refresh {
        let cached = super::blocking({
            let (state, iri) = (state.clone(), iri.clone());
            move || -> AppResult<Option<serde_json::Value>> {
                if !state.store.exists(&iri).map_err(AppError::from)? {
                    return Ok(None);
                }
                let graph = state.store.graph_of(&iri).map_err(AppError::from)?;
                let quads = state.store.describe(&iri).map_err(AppError::from)?;
                let turtle = crate::negotiate::serialize(&quads, crate::negotiate::Repr::Turtle, state.base())?;
                Ok(Some(json!({ "iri": iri, "cached": true, "graph": graph, "turtle": turtle })))
            }
        })
        .await?;
        if let Some(body) = cached {
            return Ok(Json(body));
        }
    }
    let stub = fetch_stub(&state, &iri).await?;
    Ok(Json(stub))
}

/// Dereference a foreign IRI with `Accept: text/turtle`, write a minimal stub into the peer
/// graph, and record success or backoff — on the IRI, and on the peer it belongs to.
pub async fn fetch_stub(state: &Arc<AppState>, iri: &str) -> AppResult<serde_json::Value> {
    let peer = owning_peer(state, iri).await;
    let failed = |msg: String| {
        let (state, peer_id) = (state.clone(), peer.as_ref().map(|p| p.id.clone()));
        async move {
            let _ = state.ops.mark_resolve_failed(iri, &msg).await;
            if let Some(id) = peer_id {
                let _ = state.ops.note_peer_contact(&id, Some(&msg)).await;
            }
            AppError::bad_request(msg)
        }
    };
    let body = match state.http.get(iri).header(axum::http::header::ACCEPT, "text/turtle").send().await {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(body) => body,
            // Not an empty document: parsing "" would replace the cached stub with nothing.
            Err(e) => return Err(failed(format!("reading {iri}: {e}")).await),
        },
        Ok(r) => return Err(failed(format!("{iri} returned {}", r.status())).await),
        Err(e) => return Err(failed(format!("cannot dereference {iri}: {e}")).await),
    };
    // It answered, so it is alive, whatever the body turns out to be.
    if let Some(p) = &peer {
        let _ = state.ops.note_peer_contact(&p.id, None).await;
    }
    let peer_id = peer.as_ref().map(|p| p.id.clone()).unwrap_or_else(|| "unknown".into());
    let graph = ns::peer_graph(&peer_id);
    let stub = match stub_quads(iri, &body, &graph) {
        Ok(s) => s,
        Err(e) => return Err(failed(format!("{iri} did not return parseable Turtle: {e}")).await),
    };
    // Other records the document describes. Before stubs were trimmed, the whole document was
    // loaded, so this graph may hold them from an earlier fetch; they go now, unless one of them
    // is a stub in its own right, which its own refresh keeps.
    let mut leftovers = Vec::new();
    for s in stub.others {
        if !state.ops.is_tracked(&s).await.unwrap_or(true) {
            leftovers.push(s);
        }
    }
    // Replace whatever we cached before with the trimmed stub, as one `GraphTx`.
    let n = stub.quads.len();
    let was_cached = super::blocking({
        let (state, iri, graph) = (state.clone(), iri.to_string(), graph.clone());
        move || {
            let q = format!(
                "ASK {{ GRAPH {} {{ {} ?p ?o }} }}",
                crate::store::queries::iri(&graph)?,
                crate::store::queries::iri(&iri)?
            );
            let was_cached = state.store.ask(&q).map_err(AppError::from)?;
            let mut tx = crate::store::GraphTx::new();
            tx.replace_subject(&iri, &graph);
            for s in &leftovers {
                tx.replace_subject(s, &graph);
            }
            tx.extend(stub.quads);
            state.store.apply(tx).map_err(AppError::from)?;
            Ok(was_cached)
        }
    })
    .await?;
    let ttl = chrono::Duration::from_std(state.config.peer_resolve_ttl).unwrap_or(chrono::Duration::hours(24));
    // Tracked from now on, however it was first resolved, so its refresh keeps it and another
    // document's refresh does not mistake it for a leftover.
    let _ = state.ops.queue_resolve(iri, peer.as_ref().map(|p| p.id.as_str())).await;
    let _ = state.ops.mark_resolved(iri, ttl).await;
    // A refresh is not a new record; counting it again would inflate "cached" once a day.
    if let (Some(p), false) = (&peer, was_cached) {
        let _ = state.ops.set_peer_record_count(&p.id, p.record_count + 1).await;
    }
    Ok(json!({ "iri": iri, "cached": false, "graph": graph, "triples": n, "peer": peer.map(|p| p.base_iri) }))
}

/// Namespaces the registry's record model reads. A statement in any other vocabulary is
/// something no screen or query here would ever show, so a stub does not keep it.
const STUB_NAMESPACES: [&str; 11] =
    [ns::RDF, ns::RDFS, ns::DCT, ns::DCAT, ns::PROV, ns::SCHEMA, ns::SKOS, ns::SPDX, ns::CODEMETA, ns::FOAF, ns::TAR];
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";

/// Trim a peer's Turtle to a stub (design note: peer stubs §4). Only the record itself and what
/// it owns — its blank nodes and the sub-resources `describe` would return with it — and only
/// in the vocabularies the model reads. Other records in the same document are dropped; they
/// are resolved on their own if something here cites them.
struct Stub {
    quads: Vec<oxigraph::model::Quad>,
    /// Named subjects in the document that are not part of the stub.
    others: Vec<String>,
}

/// Ownership is followed no deeper than the external backend's delete does
/// (`queries::DEFAULT_DEPTH`), so a refresh there removes everything the last fetch wrote.
fn stub_quads(iri: &str, turtle: &str, graph: &str) -> anyhow::Result<Stub> {
    use oxigraph::io::{RdfFormat, RdfParser};
    use oxigraph::model::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term};
    let parsed: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
        .with_base_iri(iri)?
        .for_slice(turtle.as_bytes())
        .collect::<Result<_, _>>()?;
    let graph = GraphName::NamedNode(NamedNode::new(graph)?);
    let wanted = |p: &str| p == OWL_SAME_AS || STUB_NAMESPACES.iter().any(|ns| p.starts_with(ns));
    let mut frontier = vec![(NamedOrBlankNode::NamedNode(NamedNode::new(iri)?), 0)];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    while let Some((node, depth)) = frontier.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        let deeper = depth < crate::store::queries::DEFAULT_DEPTH;
        for q in parsed.iter().filter(|q| q.subject == node && wanted(q.predicate.as_str())) {
            match &q.object {
                Term::BlankNode(b) if deeper => frontier.push((NamedOrBlankNode::BlankNode(b.clone()), depth + 1)),
                Term::NamedNode(n) if deeper && crate::store::is_owned_subresource(q.predicate.as_str()) => {
                    frontier.push((NamedOrBlankNode::NamedNode(n.clone()), depth + 1))
                }
                _ => {}
            }
            out.push(Quad::new(q.subject.clone(), q.predicate.clone(), q.object.clone(), graph.clone()));
        }
    }
    let mut others: Vec<String> = parsed
        .iter()
        .filter_map(|q| match &q.subject {
            NamedOrBlankNode::NamedNode(n) if !seen.contains(&q.subject) => Some(n.as_str().to_string()),
            _ => None,
        })
        .collect();
    others.sort();
    others.dedup();
    Ok(Stub { quads: out, others })
}

async fn owning_peer(state: &Arc<AppState>, iri: &str) -> Option<PeerRecord> {
    let peers = state.ops.list_peers(None).await.ok()?;
    peers.into_iter().find(|p| iri.starts_with(&p.base_iri))
}

/// The background resolver (spec §9.4). Never on the advertisement path.
pub async fn resolver_loop(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        ticker.tick().await;
        let due = match state.ops.due_resolves(10).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "resolve queue read failed");
                continue;
            }
        };
        for iri in due {
            match fetch_stub(&state, &iri).await {
                Ok(_) => tracing::info!(iri = %iri, "resolved foreign IRI"),
                Err(e) => tracing::debug!(iri = %iri, error = ?e.detail, "resolve failed; backing off"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Limitations #7: the record and what it owns, in vocabularies the registry reads.
    #[test]
    fn a_stub_keeps_the_record_and_what_it_owns_and_nothing_else() {
        let doc = r#"
            @prefix dct: <http://purl.org/dc/terms/> .
            @prefix dcat: <http://www.w3.org/ns/dcat#> .
            @prefix spdx: <http://spdx.org/rdf/terms#> .
            @prefix ex: <https://unrelated.example/ns#> .
            <https://peer.example/artifact/1> a dcat:Dataset ;
                dct:title "report" ;
                ex:internalScore 42 ;
                dcat:distribution <https://peer.example/distribution/1> .
            <https://peer.example/distribution/1> dcat:downloadURL <https://files.example/r.ttl> ;
                spdx:checksum [ spdx:checksumValue "abc" ] .
            <https://peer.example/artifact/2> dct:title "a neighbour" .
            <https://peer.example/> a dcat:Catalog ; dct:title "the peer" .
        "#;
        let stub = stub_quads("https://peer.example/artifact/1", doc, "urn:tar:peer:p").unwrap();
        assert_eq!(stub.others, ["https://peer.example/", "https://peer.example/artifact/2"], "what a refresh clears");
        let quads = stub.quads;
        let text: Vec<String> = quads.iter().map(|q| q.to_string()).collect();
        let has = |needle: &str| text.iter().any(|t| t.contains(needle));
        assert!(has("\"report\""), "{text:#?}");
        assert!(has("downloadURL"), "the owned distribution is kept: {text:#?}");
        assert!(has("\"abc\""), "and its checksum, through a blank node: {text:#?}");
        assert!(!has("internalScore"), "a vocabulary nothing here reads is dropped: {text:#?}");
        assert!(!has("a neighbour"), "another record in the document is dropped: {text:#?}");
        assert!(!has("the peer"), "and so is the peer's catalog: {text:#?}");
        assert!(quads.iter().all(|q| q.graph_name.to_string() == "<urn:tar:peer:p>"));
        assert_eq!(quads.len(), 6, "{text:#?}");
    }
}
