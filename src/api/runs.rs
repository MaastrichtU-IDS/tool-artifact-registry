//! Run endpoints (spec §7.5).

use super::{count, page_iris, resource_response, Paging};
use crate::domain::{run as dom, Ctx};
use crate::error::AppResult;
use crate::ids::{self, Kind};
use crate::model::Page;
use crate::negotiate::{Repr, Signposting};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize, Default)]
pub struct RunFilter {
    pub instance: Option<String>,
    pub software: Option<String>,
    pub status: Option<String>,
    pub q: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

pub async fn list(State(state): State<Arc<AppState>>, Query(f): Query<RunFilter>) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let mut body = format!(
        "GRAPH ?g {{ ?s a <{t}> . OPTIONAL {{ ?s rdfs:label ?label }} OPTIONAL {{ ?s tar:externalKey ?key }} }}",
        t = dom::TYPE_ACTIVITY
    );
    if let Some(q) = &f.q {
        body.push('\n');
        body.push_str(&super::text_filter(q, &["?label", "?key"]));
    }
    if let Some(i) = f.instance.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(state.base(), Kind::Instance, i);
        body.push_str(&format!("\nGRAPH ?g {{ ?s tar:atInstance <{iri}> }}"));
    }
    if let Some(sw) = f.software.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(state.base(), Kind::Software, sw);
        body.push_str(&format!("\nGRAPH ?g {{ ?s tar:atInstance ?i . ?i tar:instanceOf <{iri}> }}"));
    }
    if let Some(s) = f.status.as_deref().filter(|v| !v.is_empty()) {
        body.push_str(&format!("\nGRAPH ?g {{ ?s tar:status \"{}\" }}", super::escape_literal(s)));
    }
    body.push_str(&format!("\n{}", f.paging.cursor_filter("?s")));

    let (iris, next) = page_iris(&state, &body, &f.paging)?;
    let total = count(&state, &body)?;
    let mut items = Vec::new();
    for iri in iris {
        if let Ok(s) = dom::load_run_summary(&ctx, &iri) {
            items.push(s);
        }
    }
    Ok(Json(Page::new(items, total, next)))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Run, &id);
    let run = dom::load_run(&ctx, &iri)?;
    let sp = Signposting::new(&iri).collection(&format!("{}/api/v1/runs", state.base()));
    Ok(resource_response(&state, &headers, &iri, &run, sp, Repr::Json)?)
}
