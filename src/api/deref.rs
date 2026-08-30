//! IRI dereference (spec §4.4). Registry IRIs and UI routes are the same URLs: a browser gets
//! the SPA, a machine gets Turtle or JSON-LD, and everyone gets Signposting headers.

use crate::domain::{artifact as artdom, instance as instdom, run as rundom, software as swdom, Ctx};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::negotiate::{negotiate, serialize, Negotiated, Repr, Signposting};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// Strip a `.ttl` / `.jsonld` / `.json` suffix, which pins the representation without an
/// `Accept` header — this is what the Signposting `describedby` links point at.
fn split_extension(id: &str) -> (String, Option<Repr>) {
    if let Some((stem, ext)) = id.rsplit_once('.') {
        if let Some(repr) = Repr::from_extension(ext) {
            return (stem.to_string(), Some(repr));
        }
    }
    (id.to_string(), None)
}

async fn deref(
    state: Arc<AppState>,
    headers: HeaderMap,
    kind: Kind,
    id: String,
) -> AppResult<Response> {
    let (id, pinned) = split_extension(&id);
    let iri = ids::iri_for(state.base(), kind, &id);
    let repr = pinned.unwrap_or_else(|| negotiate(&headers, Repr::Html));

    let quads = state.store.describe(&iri).map_err(AppError::from)?;
    if quads.is_empty() && repr != Repr::Html {
        return Err(AppError::not_found(format!("nothing at {iri}")));
    }
    let ctx = Ctx::new(&state).await?;
    let sp = signposting_for(&ctx, kind, &iri, &state);

    if repr == Repr::Html {
        // The SPA renders it; the route is the same URL (handoff §3). The Link headers ride
        // along, because this is the representation a person shares and a machine then
        // follows — omitting them here is omitting them where they matter most.
        let mut resp = super::web::spa_response(&state).await;
        if !quads.is_empty() {
            if let Some(v) = sp.header_value() {
                resp.headers_mut().insert(axum::http::header::LINK, v);
            }
        }
        return Ok(resp);
    }

    if repr == Repr::Json {
        let body = match kind {
            Kind::Software => serde_json::to_string(&swdom::load_software(&ctx, &iri)?),
            Kind::Instance => serde_json::to_string(&instdom::load_instance(&ctx, &iri)?),
            Kind::Artifact => serde_json::to_string(&artdom::load_artifact(&ctx, &iri)?),
            Kind::Run => serde_json::to_string(&rundom::load_run(&ctx, &iri)?),
            _ => Ok(serialize(&quads, Repr::Turtle, state.base())?),
        }
        .map_err(|e| AppError::internal(e.to_string()))?;
        return Ok(Negotiated { repr, body, signposting: Some(sp), status: axum::http::StatusCode::OK }.into_response());
    }

    Ok(Negotiated {
        repr,
        body: serialize(&quads, repr, state.base())?,
        signposting: Some(sp),
        status: axum::http::StatusCode::OK,
    }
    .into_response())
}

fn signposting_for(ctx: &Ctx, kind: Kind, iri: &str, state: &AppState) -> Signposting {
    let mut sp = Signposting::new(iri).collection(&format!("{}/api/v1/registry", state.base()));
    match kind {
        Kind::Artifact => {
            if let Ok(a) = artdom::load_artifact(ctx, iri) {
                if let Some(t) = &a.conforms_to {
                    sp = sp.type_(&t.iri);
                }
                if let Some(l) = &a.license {
                    sp = sp.license(l);
                }
                if let Some(p) = &a.publisher {
                    sp = sp.author(&p.iri);
                }
                for d in &a.distributions {
                    if d.availability != "metadata-only" {
                        if let Some(u) = d.download_url.as_deref().or(d.access_url.as_deref()) {
                            sp = sp.item(u, d.media_type.as_deref());
                        }
                    }
                }
            }
        }
        Kind::Software => {
            if let Ok(s) = swdom::load_software(ctx, iri) {
                if let Some(l) = &s.license {
                    sp = sp.license(l);
                }
                if let Some(p) = &s.publisher {
                    sp = sp.author(&p.iri);
                }
                if let Some(r) = &s.code_repository {
                    sp = sp.item(r, None);
                }
            }
        }
        _ => {}
    }
    sp
}

pub async fn deref_software(State(s): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<String>) -> AppResult<Response> {
    deref(s, h, Kind::Software, id).await
}
pub async fn deref_release(State(s): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<String>) -> AppResult<Response> {
    deref(s, h, Kind::Release, id).await
}
pub async fn deref_instance(State(s): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<String>) -> AppResult<Response> {
    deref(s, h, Kind::Instance, id).await
}
pub async fn deref_artifact(State(s): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<String>) -> AppResult<Response> {
    deref(s, h, Kind::Artifact, id).await
}
pub async fn deref_series(State(s): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<String>) -> AppResult<Response> {
    deref(s, h, Kind::ArtifactSeries, id).await
}
pub async fn deref_run(State(s): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<String>) -> AppResult<Response> {
    deref(s, h, Kind::Run, id).await
}
pub async fn deref_type(State(s): State<Arc<AppState>>, h: HeaderMap, Path(id): Path<String>) -> AppResult<Response> {
    deref(s, h, Kind::Type, id).await
}
/// Capability, Distribution and Agent IRIs resolve as RDF only — they have no UI page of
/// their own, so a browser is sent to the record that owns them by the SPA's not-found route.
pub async fn deref_generic(
    State(state): State<Arc<AppState>>,
    h: HeaderMap,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
) -> AppResult<Response> {
    let iri = format!("{}{}", state.base(), uri.path());
    let repr = negotiate(&h, Repr::Turtle);
    if repr == Repr::Html {
        return Ok(super::web::spa_response(&state).await);
    }
    let quads = state.store.describe(&iri).map_err(AppError::from)?;
    if quads.is_empty() {
        return Err(AppError::not_found(format!("nothing at {iri}")));
    }
    Ok(Negotiated {
        repr,
        body: serialize(&quads, if repr == Repr::Json { Repr::Turtle } else { repr }, state.base())?,
        signposting: Some(Signposting::new(&iri)),
        status: axum::http::StatusCode::OK,
    }
    .into_response())
}
