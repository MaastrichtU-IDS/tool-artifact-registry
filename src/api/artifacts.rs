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
    /// A keyword from the registry's list (by label, slug or IRI) or any free-text keyword.
    pub keyword: Option<String>,
    pub instance: Option<String>,
    pub software: Option<String>,
    pub run: Option<String>,
    /// Bytes, named. Takes the content identifier, or the bare digest a producer has just
    /// computed. Spans local and peer graphs, which is the whole point of it.
    pub content: Option<String>,
    pub registry: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

fn where_body(base: &str, f: &ArtifactFilter) -> String {
    let mut w = format!(
        "GRAPH ?g {{ ?s a <{t}> . OPTIONAL {{ ?s dct:title ?title }} OPTIONAL {{ ?s dct:description ?desc }} }}\n\
         FILTER NOT EXISTS {{ GRAPH ?tg {{ ?s tar:tombstoned true }} }}",
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
        w.push_str(&format!("\nGRAPH ?g {{ ?s dcat:distribution/tar:availability \"{}\" }}", super::escape_literal(a)));
    }
    if let Some(k) = f.keyword.as_deref().filter(|v| !v.is_empty()) {
        // Accept whatever the caller has to hand: the concept IRI, the slug, the label, or any
        // alias. A filter that only understood one of those would send people back to guessing
        // spellings, which is the problem the list exists to remove.
        match crate::domain::keywords::lookup(k.rsplit('/').next().unwrap_or(k))
            .or_else(|| crate::domain::keywords::lookup(k))
        {
            Some(entry) => {
                let iri = crate::domain::keywords::iri(base, entry.slug);
                w.push_str(&format!("\nGRAPH ?g {{ ?s dcat:theme <{iri}> }}"));
            }
            // Not on the list, so it is free text and only the literal can match.
            None => w.push_str(&format!("\nGRAPH ?g {{ ?s dcat:keyword \"{}\" }}", super::escape_literal(k))),
        }
    }
    if let Some(i) = f.instance.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Instance, i);
        w.push_str(&format!(
            "\nGRAPH ?g {{ ?s prov:wasGeneratedBy ?r . ?r prov:wasAssociatedWith|tar:atInstance <{iri}> }}"
        ));
    }
    if let Some(sw) = f.software.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Software, sw);
        w.push_str(&format!(
            "\nGRAPH ?g {{ ?s prov:wasGeneratedBy ?r . ?r prov:wasAssociatedWith|tar:atInstance ?i . ?i tar:instanceOf <{iri}> }}"
        ));
    }
    if let Some(r) = f.run.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Run, r);
        w.push_str(&format!("\nGRAPH ?g {{ ?s prov:wasGeneratedBy <{iri}> }}"));
    }
    if let Some(c) = f.content.as_deref().filter(|v| !v.is_empty()) {
        // One hop through the distribution, because that is where the identifier honestly sits.
        // `?g` stays unbound so a peer's cached record matches on the same footing as a local
        // one — recognising the same bytes across registries is the reason this filter exists,
        // and binding the graph to <urn:tar:local> would quietly remove it. Nothing is merged:
        // two records for one file come back as two rows, each with its own origin.
        match crate::domain::content::parse_query(c) {
            Some(iri) => w.push_str(&format!("\nGRAPH ?g {{ ?s dcat:distribution/prov:specializationOf <{iri}> }}")),
            // A value that names no bytes cannot match anything. Answering it with an unfiltered
            // list would look like "every artifact has this content", which is worse than empty.
            None => w.push_str("\nFILTER(false)"),
        }
    }
    match f.registry.as_deref() {
        Some("local") => w.push_str(&format!("\nFILTER(?g = <{}>)", ns::G_LOCAL)),
        Some(peer) if !peer.is_empty() => w.push_str(&format!("\nFILTER(?g = <{}>)", ns::peer_graph(peer))),
        _ => {}
    }
    w.push_str(&format!("\n{}", f.paging.cursor_filter("?s")));
    w
}

pub async fn list(State(state): State<Arc<AppState>>, Query(f): Query<ArtifactFilter>) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let page = super::blocking(move || {
        let body = where_body(ctx.base(), &f);
        let (iris, next) = page_iris(&ctx.state, &body, &f.paging)?;
        let total = count(&ctx.state, &body)?;
        let items: Vec<Artifact> = iris.iter().filter_map(|i| dom::load_artifact(&ctx, i).ok()).collect();
        Ok(Page::new(items, total, next))
    })
    .await?;
    Ok(Json(page))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Artifact, &id);
    let artifact = super::blocking({
        let iri = iri.clone();
        move || dom::load_artifact(&ctx, &iri)
    })
    .await?;
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
    Ok(resource_response(&state, &headers, &iri, &artifact, sp, Repr::Json).await?)
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
    let (external_key, title) = (input.external_key.clone(), input.title.clone());
    super::blocking({
        let (state, iri, subject) = (state.clone(), iri.clone(), principal.subject.clone());
        move || {
            let quads = dom::artifact_quads(state.base(), &iri, &input, &subject, None);
            shacl::enforce_write(&state, &quads)?;
            let mut tx = GraphTx::new();
            tx.extend(quads);
            state.store.apply(tx).map_err(AppError::from)
        }
    })
    .await?;
    if let Some(k) = &external_key {
        let _ = state.ops.remember_artifact(k, &iri).await;
    }
    // The other way an artifact appears. A curator's direct registration has no run and often
    // no Instance behind the credential, so provenance filters on it cannot match — but a
    // subscription for "any SHACL report" must still fire, or the feature would have a hole
    // exactly where a backfilled historical artifact lands.
    crate::api::subscriptions::notify_advertised(
        &state,
        principal.instance_iri.as_deref(),
        None,
        std::slice::from_ref(&iri),
        crate::ops::subscriptions::ROLE_PRODUCED,
    )
    .await;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "artifact.create", Some(&iri), title.as_deref(), None)
        .await;
    let ctx = Ctx::new(&state).await?;
    let artifact = super::blocking({
        let iri = iri.clone();
        move || dom::load_artifact(&ctx, &iri)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(artifact)))
}

// ------------------------------------------------------- naming bytes

/// The path `require_read_access` has to treat as a read even though it arrives as a POST.
pub const IDENTIFY_PATH: &str = "/api/v1/artifacts/identify";

#[derive(Debug, Deserialize, Default)]
pub struct IdentifyIn {
    /// The same two fields a distribution's `checksum` carries, so a producer can post exactly
    /// what it is about to put in the record.
    #[serde(default)]
    pub algorithm: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    /// Not a parameter — a trap, and a deliberate one. A caller who sends the file expects the
    /// registry to hash it, and the refusal is the only place that expectation can be corrected
    /// before it becomes a dependency. See `refuse_bytes`.
    #[serde(default)]
    pub bytes: Option<serde_json::Value>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

fn refuse_bytes() -> AppError {
    AppError::new(StatusCode::UNPROCESSABLE_ENTITY, "bytes-not-accepted", "Send a digest, not the data").detail(
        "This registry never holds the bytes of an artifact, and this endpoint will not take \
             them either: streaming a file here to compute a digest the caller can compute \
             locally would put a network round trip, a size limit and this registry's \
             availability between a producer and an identifier that is a pure function of the \
             file. Hash it where the file already is — `sha256sum FILE` — and send the digest.",
    )
}

fn invalid_checksum(algorithm: &str, problem: &crate::domain::content::Problem) -> AppError {
    use crate::domain::content::Problem;
    let mut e = AppError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid-checksum", "Unusable checksum")
        .detail(problem.message(algorithm))
        .with("field", serde_json::json!("value"))
        .with("algorithms", serde_json::json!(crate::domain::content::deriving()));
    if let Problem::MalformedDigest { expected_hex_chars } = problem {
        e = e.with("expected_hex_characters", serde_json::json!(expected_hex_chars));
    }
    e
}

/// Everything a producer needs to stop calling this endpoint.
///
/// Carried in the response body on purpose. The identifier is a pure function of the digest, so
/// anyone who can hash a file can compute it offline — but only if they are told how, and a
/// producer who is never told will wire this endpoint into their pipeline and acquire an
/// availability dependency on a registry they did not need to talk to at all.
fn how_to_compute() -> serde_json::Value {
    serde_json::json!({
        "summary":
            "The identifier is a pure function of the algorithm and the digest: the digest in \
             base64url without padding, after `ni:///<algorithm>;`. Nothing about this registry \
             goes into it, so you can compute it offline and never call this endpoint again. \
             This endpoint is a convenience, not the source of truth.",
        "shell":
            "printf 'ni:///sha-256;%s\\n' \
             \"$(openssl dgst -binary -sha256 FILE | openssl base64 -A | tr '+/' '-_' | tr -d '=')\"",
        "shell_digest_only": "sha256sum FILE | cut -d' ' -f1",
        "python":
            "import hashlib, base64\n\
             d = hashlib.sha256(open('FILE','rb').read()).digest()\n\
             print('ni:///sha-256;' + base64.urlsafe_b64encode(d).decode().rstrip('='))",
        "javascript":
            "const { createHash } = require('node:crypto'), { readFileSync } = require('node:fs');\n\
             const d = createHash('sha256').update(readFileSync('FILE')).digest('base64url');\n\
             console.log(`ni:///sha-256;${d}`);",
        "algorithms": crate::domain::content::deriving(),
        "specification": "https://www.rfc-editor.org/rfc/rfc6920",
    })
}

/// `POST /api/v1/artifacts/identify` — the identifier this registry derives for a digest.
///
/// Stateless in the sense that matters: it writes nothing, reads nothing, needs no credential a
/// read does not need, and two calls with the same input give the same answer forever. It is
/// deliberately not a lookup — a digest nothing here describes gets an identifier all the same,
/// because the identifier exists whether or not anybody has advertised those bytes.
pub async fn identify(
    State(state): State<Arc<AppState>>,
    Json(input): Json<IdentifyIn>,
) -> AppResult<impl IntoResponse> {
    if input.bytes.is_some() || input.data.is_some() {
        return Err(refuse_bytes());
    }
    identified(&state, input.algorithm.as_deref(), input.value.as_deref())
}

/// The same function under `GET`, because a pure function of two short strings should be a URL
/// somebody can paste, bookmark and cache.
pub async fn identify_get(
    State(state): State<Arc<AppState>>,
    Query(q): Query<IdentifyIn>,
) -> AppResult<impl IntoResponse> {
    identified(&state, q.algorithm.as_deref(), q.value.as_deref())
}

fn identified(state: &AppState, algorithm: Option<&str>, value: Option<&str>) -> AppResult<axum::response::Response> {
    let algorithm = algorithm.map(str::trim).filter(|v| !v.is_empty()).unwrap_or("sha256");
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Err(AppError::bad_request(
            "send the digest as `value`, with the algorithm that produced it as `algorithm` \
             (default sha256)",
        ));
    };
    let id = crate::domain::content::identify(algorithm, value).map_err(|p| invalid_checksum(algorithm, &p))?;
    let encoded: String =
        percent_encoding::utf8_percent_encode(&id.iri, percent_encoding::NON_ALPHANUMERIC).to_string();
    Ok(Json(serde_json::json!({
        "content_identifier": id.iri,
        "algorithm": id.algorithm,
        "digest_hex": id.hex,
        "digest_base64url": id.base64url,
        // Both halves of what a caller does next: look for these bytes here, and look for them
        // everywhere this registry can reach.
        "find": format!("{}/api/v1/artifacts?content={}", state.base(), encoded),
        "how_to_compute": how_to_compute(),
    }))
    .into_response())
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
    let depth = q.depth.unwrap_or(1).clamp(1, 6);
    let direction = q.direction.unwrap_or_else(|| "both".into());
    if !["up", "down", "both"].contains(&direction.as_str()) {
        return Err(AppError::bad_request("direction must be up, down or both"));
    }
    let lineage = super::blocking({
        let iri = iri.clone();
        move || {
            if !ctx.state.store.exists(&iri).map_err(AppError::from)? {
                return Err(AppError::not_found(format!("no artifact at {iri}")));
            }
            dom::lineage(&ctx, &iri, depth, &direction)
        }
    })
    .await?;
    Ok(Json(lineage))
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
