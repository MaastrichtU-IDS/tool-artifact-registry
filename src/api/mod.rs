//! HTTP surface (spec §7). Base path `/api/v1`, plus the dereferenceable IRI routes and the
//! SPA.

pub mod advertise;
pub mod apidocs;
pub mod artifacts;
pub mod deref;
pub mod instances;
pub mod llms;
pub mod openlineage;
pub mod peers;
pub mod registry;
pub mod runs;
pub mod search;
pub mod software;
pub mod sparql;
pub mod subscriptions;
pub mod tokens;
pub mod types;
pub mod vocab;
pub mod web;

use crate::error::{AppError, AppResult};
use crate::negotiate::{negotiate, serialize, Negotiated, Repr, Signposting};
use crate::ns;
use crate::state::AppState;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::Router;
use serde::Deserialize;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

pub fn router(state: Arc<AppState>) -> Router {
    let limit = state.config.max_payload_bytes;
    let api = Router::new()
        // discovery
        .route("/registry", get(registry::registry))
        .route("/context", get(registry::context))
        .route("/audit", get(registry::audit))
        // software
        .route("/software", get(software::list).post(software::create))
        .route("/software/{id}", get(software::get).patch(software::patch).delete(software::soft_delete))
        .route("/software/{id}/releases", get(software::list_releases).post(software::create_release))
        .route("/software/{id}/releases/{release_id}", delete(software::delete_release))
        .route("/software/{id}/capability", put(software::put_capability))
        .route("/software/{id}/sync", post(software::sync))
        // The record's API description, fetched server-side: browsers cannot fetch most of
        // them directly, because almost no `openapi.json` is served with CORS headers.
        .route("/software/{id}/api-doc", get(apidocs::fetch))
        // Auto-registration credentials: a key bound to the software, not to one deployment.
        .route("/software/{id}/tokens", get(tokens::list_for_software).post(tokens::create_for_software))
        .route("/software/{id}/tokens/{token_id}", delete(tokens::revoke_for_software))
        .route("/software/{id}/export/biotools", get(software::export_biotools))
        // capability matchmaking
        .route("/capabilities", get(search::capabilities))
        // local ArtifactTypes (D11)
        .route("/types", get(types::list).post(types::create))
        .route("/vocab/search", get(vocab::search))
        // The registry's own artifact keyword list, whole — short enough that a picker wants
        // all of it before anyone types.
        .route("/keywords", get(vocab::keywords))
        .route("/vocab/resolve", get(vocab::resolve))
        .route("/types/{id}", get(types::get))
        // instances
        .route("/instances", get(instances::list).post(instances::create))
        // A running service records what it is. Must precede /instances/{id}.
        .route("/instances/self", put(instances::announce_self))
        .route("/instances/{id}", get(instances::get).patch(instances::patch).delete(instances::soft_delete))
        .route("/instances/{id}/capability", put(instances::put_capability))
        .route("/instances/{id}/runs", get(instances::runs))
        .route("/instances/{id}/artifacts", get(instances::artifacts))
        .route("/instances/{id}/tokens", get(tokens::list).post(tokens::create))
        .route("/instances/{id}/tokens/{token_id}", delete(tokens::revoke))
        // subscriptions: standing interest in artifacts, owned by an Instance like its tokens
        .route("/instances/{id}/subscriptions", get(subscriptions::list).post(subscriptions::create))
        .route(
            "/subscriptions/{sid}",
            get(subscriptions::get).patch(subscriptions::patch).delete(subscriptions::delete),
        )
        // the pull path, for a subscriber that cannot receive an inbound connection
        .route("/subscriptions/{sid}/deliveries", get(subscriptions::deliveries))
        .route("/subscriptions/{sid}/deliveries/ack", post(subscriptions::ack))
        // artifacts and runs
        .route("/artifacts", get(artifacts::list).post(artifacts::create))
        .route("/artifacts/{id}", get(artifacts::get))
        .route("/artifacts/{id}/lineage", get(artifacts::lineage))
        .route("/runs", get(runs::list))
        .route("/runs/{id}", get(runs::get))
        // advertisement (requirements 4 and 5)
        .route("/advertise/produced", post(advertise::produced))
        .route("/advertise/consumed", post(advertise::consumed))
        .route("/openlineage", post(openlineage::ingest))
        // query
        .route("/search", get(search::search))
        .route("/graph", get(search::graph))
        // federation
        .route("/peers", get(peers::list).post(peers::add))
        .route("/peers/suggested", get(peers::suggested))
        .route("/peers/announce", post(peers::announce))
        .route("/peers/{id}", delete(peers::remove))
        .route("/resolve", get(peers::resolve))
        // whoami — how a tool checks which Instance its credential maps to
        .route("/whoami", get(registry::whoami));

    Router::new()
        .nest("/api/v1", api)
        // The hosted MCP server: one route on this same process, behind these same
        // credentials, plus the RFC 9728 metadata a client discovers the authorization
        // server from. See `crate::mcp`.
        .merge(crate::mcp::routes())
        .route("/.well-known/tar-registry", get(registry::well_known))
        .route("/healthz", get(registry::healthz))
        .route("/readyz", get(registry::readyz))
        .route("/metrics", get(registry::metrics))
        .route("/sparql", post(sparql::query).get(sparql::query_get))
        // The agent's front door: what this registry is and how to read any record in it,
        // in the format an LLM reads without a parser (https://llmstxt.org).
        .route("/llms.txt", get(llms::llms_txt))
        .route("/admin/dump", get(registry::dump))
        // Registry IRIs and UI routes are the same URLs (handoff §3).
        .route("/software/{id}", get(deref::deref_software))
        .route("/release/{id}", get(deref::deref_release))
        .route("/instance/{id}", get(deref::deref_instance))
        .route("/instances/{id}", get(deref::deref_instance))
        .route("/artifact/{id}", get(deref::deref_artifact))
        .route("/artifacts/{id}", get(deref::deref_artifact))
        .route("/artifact-series/{id}", get(deref::deref_series))
        .route("/run/{id}", get(deref::deref_run))
        .route("/runs/{id}", get(deref::deref_run))
        .route("/type/{id}", get(deref::deref_type))
        .route("/capability/{id}", get(deref::deref_generic))
        // The registry's own keyword concepts and the scheme they belong to. Without these a
        // `dcat:theme` on every artifact would point at a URL that resolves to nothing, which
        // is the FAIR failure this registry exists to avoid.
        .route("/keyword/{id}", get(deref::deref_generic))
        .route("/scheme/{id}", get(deref::deref_generic))
        .route("/distribution/{id}", get(deref::deref_generic))
        .route("/agent/{id}", get(deref::deref_generic))
        .fallback(web::spa)
        // `TAR_PUBLIC_READ=false` closes anonymous reads. Enforced here rather than in each
        // handler: there are more than twenty read routes, and a setting that is enforced in
        // nineteen of them is not a setting.
        .layer(axum::middleware::from_fn_with_state(state.clone(), require_read_access))
        .layer(DefaultBodyLimit::max(limit))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Paths that stay open on a registry with anonymous reads closed.
///
/// Discovery and liveness: a client has to be able to learn *that* it needs a credential, and
/// what to authenticate against, without already holding one. `/sparql` is here because it
/// carries its own `TAR_SPARQL_PUBLIC` switch and enforces it itself.
fn is_always_public(path: &str) -> bool {
    const OPEN: [&str; 8] = [
        "/healthz",
        "/readyz",
        "/metrics",
        "/sparql",
        "/api/v1/registry",
        "/api/v1/context",
        "/api/v1/whoami",
        "/.well-known/",
    ];
    OPEN.iter().any(|p| path == *p || path.starts_with(p) && p.ends_with('/'))
        // The OAuth metadata an MCP client fetches before it has a token, and the handshake
        // that tells it so.
        || path.starts_with("/.well-known/")
        || path == crate::mcp::ENDPOINT_PATH
}

async fn require_read_access(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if state.config.public_read {
        return next.run(req).await;
    }
    // Writes carry their own authorisation, and every handler already enforces it. This gate is
    // only about anonymous *reading*.
    let is_read = matches!(req.method(), &axum::http::Method::GET | &axum::http::Method::HEAD);
    if !is_read || is_always_public(req.uri().path()) {
        return next.run(req).await;
    }
    // Presence, not validity. Whether a credential is *good* is the handler's question, and
    // answering it here would replace each route's own challenge — the MCP endpoint's
    // `WWW-Authenticate: Bearer resource_metadata=...`, for one — with a bare 401 that tells a
    // client nothing about how to authenticate.
    let anonymous = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.trim().is_empty());
    if anonymous {
        return AppError::unauthorized(
            "this registry does not serve anonymous reads; present a credential",
        )
        .into_response();
    }
    next.run(req).await
}

// --------------------------------------------------------------- pagination

pub const DEFAULT_LIMIT: usize = 25;
pub const MAX_LIMIT: usize = 200;

#[derive(Debug, Deserialize, Default)]
pub struct Paging {
    /// Keyset cursor: the IRI of the last item on the previous page (handoff §5.1).
    pub cursor: Option<String>,
    /// Kept as a string: `#[serde(flatten)]` pushes every query parameter through a
    /// string-typed map, so a `usize` here would reject `?limit=25`.
    pub limit: Option<String>,
}

impl Paging {
    pub fn limit(&self) -> usize {
        self.limit
            .as_deref()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT)
    }
    /// Keyset filter. UUIDv7 IRIs sort by mint time, so descending IRI order is
    /// newest-first without a sort column.
    pub fn cursor_filter(&self, var: &str) -> String {
        match &self.cursor {
            Some(c) if !c.is_empty() => format!("FILTER(STR({var}) < \"{}\")", escape_literal(c)),
            _ => String::new(),
        }
    }
}

pub fn escape_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ")
}

/// A SPARQL regex-safe, case-insensitive contains filter over several variables.
pub fn text_filter(q: &str, vars: &[&str]) -> String {
    if q.trim().is_empty() {
        return String::new();
    }
    let needle = escape_literal(&regex_escape(q));
    let clauses: Vec<String> = vars.iter().map(|v| format!("REGEX(STR({v}), \"{needle}\", \"i\")")).collect();
    format!("FILTER({})", clauses.join(" || "))
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if "\\.+*?()|[]{}^$".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Count matching subjects for a where-clause body.
pub fn count(state: &AppState, where_body: &str) -> AppResult<i64> {
    let q = format!("{p}\nSELECT (COUNT(DISTINCT ?s) AS ?n) WHERE {{ {where_body} }}", p = ns::PREFIXES);
    let rows = state.store.select(&q).map_err(AppError::from)?;
    Ok(rows.rows.first().and_then(|r| r.i64("n")).unwrap_or(0))
}

/// Select one page of subject IRIs, newest first.
pub fn page_iris(state: &AppState, where_body: &str, paging: &Paging) -> AppResult<(Vec<String>, Option<String>)> {
    let limit = paging.limit();
    let q = format!(
        "{p}\nSELECT DISTINCT ?s WHERE {{ {where_body} }} ORDER BY DESC(STR(?s)) LIMIT {}",
        limit + 1,
        p = ns::PREFIXES
    );
    let rows = state.store.select(&q).map_err(AppError::from)?;
    let mut iris: Vec<String> = rows.rows.iter().filter_map(|r| r.iri("s")).collect();
    // One extra row was fetched only to learn whether another page exists. The cursor is the
    // last row we actually return, because `cursor_filter` excludes it on the next call.
    let has_more = iris.len() > limit;
    iris.truncate(limit);
    let next = has_more.then(|| iris.last().cloned()).flatten();
    Ok((iris, next))
}

/// Respond to a resource GET honouring `Accept`, with Signposting attached (spec §6.3).
pub async fn resource_response(
    state: &AppState,
    headers: &HeaderMap,
    iri: &str,
    json: &impl serde::Serialize,
    signposting: Signposting,
    default_repr: Repr,
) -> AppResult<Negotiated> {
    let repr = negotiate(headers, default_repr);
    match repr {
        Repr::Json => Ok(Negotiated {
            repr,
            body: serde_json::to_string(json).map_err(|e| AppError::internal(e.to_string()))?,
            signposting: Some(signposting),
            status: axum::http::StatusCode::OK,
        }),
        // An agent that asks the JSON API for markdown gets the same rendering the IRI
        // serves, rather than a 406 or, worse, JSON under a markdown content type.
        Repr::Markdown => {
            let quads = state.store.describe(iri).map_err(AppError::from)?;
            let kind = crate::ids::local_id(&state.config.base_iri, iri)
                .map(|(k, _)| k)
                .ok_or_else(|| AppError::not_found(format!("{iri} is not a record of this registry")))?;
            let ctx = crate::domain::Ctx::new(state).await?;
            Ok(Negotiated {
                repr,
                body: llms::render_record(state, &ctx, kind, iri, &quads)?,
                signposting: Some(signposting),
                status: axum::http::StatusCode::OK,
            })
        }
        Repr::Turtle | Repr::JsonLd | Repr::NQuads => {
            let quads = state.store.describe(iri).map_err(AppError::from)?;
            Ok(Negotiated {
                repr,
                body: serialize(&quads, repr, &state.config.base_iri)?,
                signposting: Some(signposting),
                status: axum::http::StatusCode::OK,
            })
        }
        Repr::Html => Err(AppError::unsupported_media("HTML is served by the SPA route")),
    }
}
