//! Serving a software record's API description to the browser (`GET
//! /api/v1/software/{id}/api-doc?n=`).
//!
//! The document lives at somebody else's URL, and a browser cannot fetch it: almost no
//! `openapi.json` is served with permissive CORS headers, so a client-side fetch fails on most
//! records and the failure is invisible. The registry fetches it instead.
//!
//! **This is deliberately not a general proxy.** The caller passes an *index* into the docs the
//! record itself declares, never a URL — otherwise this endpoint is an SSRF gadget that will
//! fetch anything the registry's network can reach, on behalf of anyone who can spell a
//! software id. Everything else here follows from that: `http(s)` only, private hosts refused
//! unless the operator opts in, a size cap, a timeout, and no request headers forwarded.

use crate::domain::{software as swdom, Ctx};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Documents above this are refused rather than streamed: the UI renders an operation list,
/// and a 10 MB specification is a link to follow, not something to hold in a browser tab.
const MAX_BYTES: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);
/// Long enough that reloading a software page is free, short enough that a specification
/// published this morning is visible this afternoon.
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Deserialize, Default)]
pub struct DocQuery {
    /// Index into the record's `api_docs`. Defaults to the first.
    pub n: Option<usize>,
    /// Skip the cache. Costs a fetch, so it is opt-in.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Clone)]
pub struct CachedDoc {
    pub body: String,
    pub content_type: String,
    pub fetched: Instant,
}

pub type DocCache = std::sync::Mutex<std::collections::HashMap<String, CachedDoc>>;

fn allow_private() -> bool {
    std::env::var("TAR_APIDOC_ALLOW_PRIVATE").map(|v| v == "1" || v == "true").unwrap_or(true)
}

pub async fn fetch(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<DocQuery>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    let ctx = Ctx::new(&state).await?;
    let sw = super::blocking({
        let iri = iri.clone();
        move || swdom::load_software(&ctx, &iri)
    })
    .await?;
    let n = q.n.unwrap_or(0);
    let doc = sw
        .api_docs
        .get(n)
        .ok_or_else(|| AppError::not_found(format!("{} declares no API description at index {n}", sw.name)))?;

    let url = doc.url.trim().to_string();
    let parsed = url::Url::parse(&url).map_err(|e| AppError::bad_request(format!("{url} is not a URL: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::bad_request(format!(
            "an API description must be served over http or https; {url} is {}",
            parsed.scheme()
        )));
    }
    if !allow_private() && parsed.host_str().is_some_and(crate::health::is_private_host) {
        return Err(AppError::forbidden(format!("{url} is on a private address and TAR_APIDOC_ALLOW_PRIVATE is off")));
    }

    if !q.refresh {
        if let Some(hit) = state.api_doc_cache.lock().ok().and_then(|c| c.get(&url).cloned()) {
            if hit.fetched.elapsed() < CACHE_TTL {
                return Ok(cached_response(hit, true));
            }
        }
    }

    let resp = state
        .http
        .get(&url)
        // Ask for the machine-readable forms first. A server that content-negotiates its API
        // description will otherwise hand back the human documentation page.
        .header(
            axum::http::header::ACCEPT,
            "application/json, application/yaml, text/yaml, application/ld+json, text/turtle, */*;q=0.5",
        )
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| AppError::bad_gateway(format!("could not fetch {url}: {e}")))?;

    if !resp.status().is_success() {
        return Err(AppError::bad_gateway(format!(
            "{url} answered {} — the record points at a document that is not there",
            resp.status()
        )));
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_BYTES {
            return Err(AppError::bad_gateway(format!(
                "{url} is {len} bytes, over the {MAX_BYTES}-byte limit; follow the link instead"
            )));
        }
    }
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let bytes = resp.bytes().await.map_err(|e| AppError::bad_gateway(format!("could not read {url}: {e}")))?;
    if bytes.len() > MAX_BYTES {
        return Err(AppError::bad_gateway(format!(
            "{url} is over the {MAX_BYTES}-byte limit; follow the link instead"
        )));
    }
    let body = String::from_utf8_lossy(&bytes).to_string();

    let entry = CachedDoc { body, content_type, fetched: Instant::now() };
    if let Ok(mut c) = state.api_doc_cache.lock() {
        c.insert(url.clone(), entry.clone());
    }
    Ok(cached_response(entry, false))
}

fn cached_response(d: CachedDoc, from_cache: bool) -> impl IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, d.content_type.clone()),
            (
                axum::http::HeaderName::from_static("x-tar-cache"),
                if from_cache { "hit".to_string() } else { "miss".to_string() },
            ),
        ],
        d.body,
    )
}
