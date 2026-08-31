//! `ArtifactType` registration (spec D11, §4.4) — the escape hatch from the vocabulary rule.
//!
//! A write may only name a type the registry holds (`crate::domain::vocabulary`). This is how a
//! type gets held, and it does two different jobs that must not be confused:
//!
//! * **Adopt.** The thing already has a perfectly good IRI in some vocabulary this registry does
//!   not bundle. Send that IRI and it is recorded here — label, definition, scheme — under *its
//!   own identifier*. Minting a local alias for it instead would be the near-duplicate problem
//!   moved up a level: two registries adopting the same external term have to end up agreeing on
//!   one IRI, or federation is comparing synonyms again.
//! * **Mint.** The thing has no IRI anywhere — a SHACL shapes graph, an RML mapping, a
//!   hash-chained patch log. Then a registry-local IRI is the honest identifier and `ids` mints
//!   one.
//!
//! Which happened is readable from the IRI itself and needs no flag: a type under this
//! registry's base was minted here, anything else was adopted. That an adopted term sits in
//! `<urn:tar:local>` with `prov:wasAttributedTo` is the record that somebody here *chose* it —
//! as opposed to a term cached from a peer, which lands in that peer's own graph and asserts
//! nothing about what this registry accepts.

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
    /// Adopt a term that already has an identifier somewhere else, under that identifier.
    /// Absent, a new one is minted here.
    #[serde(default)]
    pub iri: Option<String>,
    /// The vocabulary an adopted term belongs to, so a curator can see later where it came from
    /// and a peer can tell two registries adopted the same thing.
    #[serde(default)]
    pub scheme: Option<String>,
    /// The other names people call it. Search matches these, and an adopted term usually
    /// arrives with a list of them.
    #[serde(default)]
    pub aliases: Vec<String>,
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
    /// Where the term comes from relative to this registry, derived from the IRI
    /// (`crate::domain::type_source`). Never the name of a vocabulary.
    pub source: String,
    /// False when the IRI is one this registry minted, true when it was taken from elsewhere.
    pub adopted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// Check an IRI a caller wants to adopt, and hand it back unchanged.
///
/// Three things can be wrong with it, and each is a different mistake:
/// a relative or malformed string is not an identifier at all; an IRI under this registry's base
/// that is not a type IRI would file some other record as a type; and an IRI the registry
/// already holds as a topic is a real term being adopted into the wrong role, which is the exact
/// failure the vocabulary rule exists to catch — allowing it here would open a back door into it.
fn adoptable(state: &AppState, given: &str) -> AppResult<String> {
    if !(given.starts_with("http://") || given.starts_with("https://")) {
        return Err(AppError::bad_request(format!(
            "`iri` must be an absolute http(s) IRI, and {given} is not one. Leave it out to mint a \
             type identified by this registry instead."
        )));
    }
    let local_prefix = format!("{}/type/", state.base());
    if crate::ids::is_local(state.base(), given) && !given.starts_with(&local_prefix) {
        return Err(AppError::bad_request(format!(
            "{given} is a record of this registry, not a type. Leave `iri` out to mint one."
        )));
    }
    // A term already held as something else — a subject area, a keyword — is a real term being
    // adopted into the wrong role. A term cached from a peer is the one exception: a peer's own
    // type carries none of this registry's classes and adopting it is exactly the intended move.
    if let Some(held) = crate::domain::vocabulary::holding(state, given) {
        if !held.usable_as(crate::domain::vocabulary::Slot::Type) {
            return Err(AppError::conflict(format!(
                "{given} is already a term here, and it is not one an artifact can be. Adopting it \
                 as a type would leave the same IRI meaning two things."
            )));
        }
    }
    Ok(given.to_string())
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
    let iri = match input.iri.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(given) => adoptable(&state, given)?,
        None => match input.slug.as_deref().map(slugify).filter(|s| !s.is_empty()) {
            Some(slug) => format!("{}/type/{slug}", state.base()),
            None => ids::mint(state.base(), Kind::Type),
        },
    };
    // Re-registering the same slug updates the label rather than minting a duplicate concept:
    // a type IRI is a name, and names are meant to be stable.
    let mut tx = GraphTx::new();
    tx.replace_subject(&iri, ns::G_LOCAL);
    let mut n = Node::local(&iri);
    n.a(TYPE_CONCEPT);
    // The class, in the same node and therefore the same graph as `a skos:Concept`, on both the
    // minting and the adopting path. This used to be a `tar:conceptBranch` literal, and because
    // it was a separate triple a later backfill was able to write it into a different graph from
    // the concept these lines create — after which the type was held, accepted on write, and
    // offered by no picker.
    n.a(crate::domain::vocabulary::CLASS_ARTIFACT_TYPE);
    n.text(ns::SKOS, "prefLabel", &input.label);
    n.opt_text(ns::SKOS, "definition", &input.definition);
    n.texts(ns::SKOS, "altLabel", &input.aliases);
    n.opt_link(ns::SKOS, "inScheme", &input.scheme);
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
    // Types this registry *knows as types*: the ones it minted or adopted, plus any IRI
    // something actually declares itself as or claims to produce or consume.
    //
    // Deliberately not "every skos:Concept in the store". The bundled vocabularies run to
    // thousands of concepts and live in the same graph; listing them here would answer "which
    // types does this registry use?" with "all of them", which is not an answer. Searching the
    // whole vocabulary is what /api/v1/vocab/search is for.
    //
    // The last two arms ask for the class rather than for skos:Concept, which is also what keeps
    // a version-series node out: those were typed as concepts once, and every artifact's title
    // turned up in the type list.
    let minted_prefix = format!("{}/type/", ctx.base());
    let q = format!(
        r#"{p}
SELECT DISTINCT ?s WHERE {{
  GRAPH ?g {{
    {{ ?a dct:conformsTo ?s }} UNION {{ ?c tar:produces ?s }} UNION {{ ?c tar:consumes ?s }}
    UNION {{ ?s a <{artifact_type}> . FILTER(STRSTARTS(STR(?s), "{minted_prefix}")) }}
    UNION {{ ?s a <{artifact_type}> . FILTER(?g = <{local}>) }}
  }}
}} ORDER BY STR(?s)"#,
        p = ns::PREFIXES,
        local = ns::G_LOCAL,
        artifact_type = crate::domain::vocabulary::CLASS_ARTIFACT_TYPE
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
        source: crate::domain::type_source(ctx.base(), iri),
        // Read off the IRI rather than a stored flag: the identifier already says who named the
        // thing, and a flag beside it could only ever disagree with it.
        adopted: !crate::ids::is_local(ctx.base(), iri),
        scheme: p.iri(ns::SKOS, "inScheme"),
        aliases: p.strs(ns::SKOS, "altLabel"),
        iri: iri.to_string(),
    })
}
