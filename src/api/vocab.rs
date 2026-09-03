//! Vocabulary lookup for the pickers (handoff §5.7: "EDAM autocomplete plus a free-IRI escape
//! hatch").
//!
//! Nobody should have to paste `http://edamontology.org/data_2048` by hand to say "report".
//! The bundled vocabularies ship with the registry (`crate::bundles`), so this searches
//! locally: the picker keeps working on a laptop with no network, which is the same promise the
//! rest of the deployment makes.
//!
//! It searches the record store and the in-memory reference store together, so a bundled term,
//! one this registry minted or adopted, and one cached from a peer all appear side by side with
//! no special casing. Both are asked because `domain::vocabulary::held` asks both: the write
//! path accepts exactly the terms this returns, and a search that could not find one of them —
//! or offered one it refuses — would be the trap the paragraph below warns about.
//!
//! This is also the route out of a refused write: `crate::domain::vocabulary` only accepts terms
//! the registry holds, and those are exactly the terms this returns. The two must not diverge —
//! a restriction resting on a search that cannot find what it restricts you to is a trap — which
//! is why the classes the check reads are the same ones the filter here applies.
//!
//! `branch` keeps its name and its values. It used to be the literal a concept carried; it is
//! now a short public alias for a concept class, translated in `class_for_branch` below. The
//! frontend pickers, the MCP tools and `llms.txt` all pass it, and renaming a query parameter
//! that four surfaces agree on to describe an internal change would be a cost with no payer.

use super::Paging;
use crate::domain::vocabulary;
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
    /// Which kind of concept to offer: `topic` for the fields of science a Software is about,
    /// `data` for what an artifact can conform to, `keyword`, or `topic-retired` for the subject
    /// areas kept only so older records still render a label. Omit for all of them at once.
    /// Each value names a class in `BRANCHES`; an unrecognised one returns nothing.
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

pub async fn search(State(state): State<Arc<AppState>>, Query(q): Query<VocabQuery>) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let needle = q.q.clone().unwrap_or_default();
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    if needle.trim().len() < 2 {
        // One character matches most of EDAM; make the caller be a little specific.
        return Ok(Json(VocabResults { items: Vec::new(), total: 0 }));
    }

    let branch_filter = match q.branch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(b) => match class_for_branch(b) {
            Some(class) => format!("GRAPH ?cg {{ ?c a <{class}> }}"),
            // An unrecognised value asks for a kind of term that does not exist, and answering it
            // with everything would be worse than answering it with nothing.
            None => return Ok(Json(VocabResults { items: Vec::new(), total: 0 })),
        },
        None => String::new(),
    };
    let results = super::blocking(move || {
        // Match the label or any synonym; EDAM's altLabels are how people actually name things.
        let filter = super::text_filter(&needle, &["?label", "?alt"]);
        let sparql = format!(
            r#"{p}
SELECT DISTINCT ?c ?label ?def ?class ?broader WHERE {{
  GRAPH ?g {{
    ?c a skos:Concept .
    {{ ?c skos:prefLabel ?label }} UNION {{ ?c rdfs:label ?label }}
    OPTIONAL {{ ?c skos:altLabel ?alt }}
    OPTIONAL {{ ?c skos:definition ?def }}
    OPTIONAL {{ ?c tar:inBroader ?broader }}
    {branch_filter}
  }}
  OPTIONAL {{ GRAPH ?kg {{ ?c a ?class }} FILTER(STRSTARTS(STR(?class), "{tar}")) }}
  {filter}
}} LIMIT 400"#,
            p = ns::PREFIXES,
            tar = ns::TAR
        );

        // Both stores, record first. The record store holds the terms this registry minted,
        // adopted or cached from a peer; the reference store holds the bundles.
        // `domain::vocabulary::held` reads the same union, and it must: a search that could not
        // find a term the write path accepts — or that offered one it refuses — is the trap
        // this module's header warns about. Record first so that a locally adopted copy of a
        // bundled IRI shows the label somebody here actually gave it.
        let mut rows = ctx.state.store.select(&sparql).map_err(AppError::from)?;
        rows.rows.extend(ctx.state.reference.select(&sparql).map_err(AppError::from)?.rows);
        let mut items: Vec<VocabHit> = Vec::new();
        for row in rows.rows {
            let (Some(iri), Some(label)) = (row.iri("c"), row.str("label")) else { continue };
            if items.iter().any(|h| h.iri == iri) {
                continue;
            }
            let score = rank(&needle, &label);
            items.push(VocabHit {
                source: crate::domain::type_source(ctx.base(), &iri),
                branch: row.iri("class").as_deref().and_then(branch_for_class).map(str::to_string),
                // EuroSciVoc carries almost no definitions but nearly always a parent, and the
                // parent is what disambiguates: this vocabulary has ontology, odontology and
                // palaeontology, and only the broader term tells them apart at a glance.
                definition: row.str("def").or_else(|| row.str("broader").map(|b| format!("in {b}"))),
                label,
                iri,
                score,
            });
        }
        items.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.label.len().cmp(&b.label.len()))
        });
        let total = items.len();
        items.truncate(limit);
        Ok(VocabResults { items, total: total as i64 })
    })
    .await?;
    Ok(Json(results))
}

#[derive(Debug, Serialize)]
pub struct VocabResults {
    pub items: Vec<VocabHit>,
    pub total: i64,
}

/// The public `branch` tokens, and the concept class each one names.
///
/// One table, read in both directions, so the value a caller filters by and the value they get
/// back in `branch` cannot drift apart. `topic-retired` is the kind kept only so a record that
/// already cites one of those terms still renders a label: it is searchable on request and is
/// never what `branch=topic` returns.
///
/// It used to be spelled `topic-edam`, which put a vocabulary's name in an API value — the one
/// place this project does not put one, because the value would then be wrong the moment the
/// retired vocabulary is a different one. The old spelling is still accepted, since records and
/// clients in the wild use it, but it is never returned.
const BRANCHES: [(&str, &str); 4] = [
    ("data", vocabulary::CLASS_ARTIFACT_TYPE),
    ("topic", vocabulary::CLASS_RESEARCH_TOPIC),
    ("keyword", vocabulary::CLASS_ARTIFACT_KEYWORD),
    ("topic-retired", vocabulary::CLASS_LEGACY_TOPIC),
];

/// Spellings that are still understood but never produced.
const BRANCH_ALIASES: [(&str, &str); 1] = [("topic-edam", "topic-retired")];

fn class_for_branch(branch: &str) -> Option<&'static str> {
    let branch = BRANCH_ALIASES.iter().find(|(old, _)| *old == branch).map(|(_, now)| *now).unwrap_or(branch);
    BRANCHES.iter().find(|(b, _)| *b == branch).map(|(_, c)| *c)
}

fn branch_for_class(class: &str) -> Option<&'static str> {
    BRANCHES.iter().find(|(_, c)| *c == class).map(|(b, _)| *b)
}

/// How well a candidate matches, in one place.
///
/// The matching above is lexical: a case-insensitive contains over the preferred label and every
/// synonym, which is what makes a search for "shapes" find a term labelled "SHACL shapes graph"
/// and a search for "table" find one whose only tabular word is an `altLabel`. The ordering is
/// what a caller actually sees, and it is deliberately the only thing that decides it — a second
/// strategy (a semantic ranker over the same candidates, say) is a second arm here and needs no
/// change to the query, the response shape or the `score` field the caller already reads.
///
/// A synonym-only hit ranks below every label hit rather than being scored on its own: SPARQL
/// hands back one row per synonym and the projection collapses them, so *which* synonym matched
/// is not knowable here. That is a real limit of doing it this way, not a preference.
fn rank(needle: &str, label: &str) -> f32 {
    let (needle, label) = (needle.to_lowercase(), label.to_lowercase());
    if label == needle {
        1.0
    } else if label.starts_with(&needle) {
        0.8
    } else if label.contains(&needle) {
        0.6
    } else {
        0.3
    }
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
    let iris: Vec<String> =
        q.iris.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).take(100).collect();
    let refs = super::blocking(move || Ok(ctx.type_refs(&iris))).await?;
    Ok(Json(refs))
}

/// Unused today, but keeps the paging import honest if the endpoint grows a cursor.
pub fn _paging(_p: &Paging) {}

/// `GET /api/v1/keywords` — the registry's own artifact keyword list, whole.
///
/// A list endpoint rather than only a search one, because the list is short and a picker wants
/// to show it before anyone has typed anything. Free-text keywords remain allowed; this is what
/// the registry recognises and normalises, not a closed set of what may be said.
pub async fn keywords(State(state): State<Arc<AppState>>) -> AppResult<impl IntoResponse> {
    use crate::domain::keywords as kw;
    let base = state.base();
    let items: Vec<serde_json::Value> = kw::KEYWORDS
        .iter()
        .map(|k| {
            serde_json::json!({
                "iri": kw::iri(base, k.slug),
                "slug": k.slug,
                "label": k.label,
                "definition": k.definition,
                "aliases": k.aliases,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "scheme": kw::scheme_iri(base),
        "items": items,
        "total": items.len(),
        "note": "A keyword matching one of these — by label, slug or alias, ignoring case and \
                 punctuation — is stored under its label and linked with dcat:theme. Anything \
                 else is kept verbatim as free text.",
    })))
}
