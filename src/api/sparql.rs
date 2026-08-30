//! Read-only SPARQL 1.1 (spec §7.7, D3). A first-class surface: it gives analysts and peer
//! registries a standard federated query language without us designing one.

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct SparqlParams {
    pub query: String,
    pub format: Option<String>,
}

pub async fn query_get(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    headers: HeaderMap,
    Query(p): Query<SparqlParams>,
) -> AppResult<impl IntoResponse> {
    run(state, principal, headers, p.query, p.format).await
}

pub async fn query(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    headers: HeaderMap,
    body: String,
) -> AppResult<impl IntoResponse> {
    // Accept both `application/sparql-query` and a form-encoded `query=`.
    let q = match body.strip_prefix("query=") {
        Some(rest) => percent_encoding::percent_decode_str(&rest.replace('+', " ")).decode_utf8_lossy().to_string(),
        None => body,
    };
    run(state, principal, headers, q, None).await
}

async fn run(
    state: Arc<AppState>,
    principal: Principal,
    headers: HeaderMap,
    q: String,
    format: Option<String>,
) -> AppResult<impl IntoResponse> {
    if !state.config.public_read {
        principal.require_authenticated()?;
    }
    // Read-only: updates are rejected outright rather than silently ignored.
    let upper = q.to_uppercase();
    for forbidden in ["INSERT", "DELETE", "DROP ", "CLEAR ", "LOAD ", "CREATE ", "COPY ", "MOVE ", "ADD "] {
        if upper.contains(forbidden) {
            return Err(AppError::forbidden(
                "this endpoint is read-only; writes go through the REST API so they are validated and audited",
            ));
        }
    }
    let wants_json = format.as_deref() == Some("json")
        || headers
            .get(axum::http::header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("json"));

    // ASK and CONSTRUCT are answered too — a peer registry uses CONSTRUCT for stubs.
    if upper.trim_start().starts_with("ASK") {
        let b = state.store.ask(&q).map_err(AppError::from)?;
        return Ok((
            [(axum::http::header::CONTENT_TYPE, "application/sparql-results+json")],
            serde_json::json!({"head": {}, "boolean": b}).to_string(),
        ));
    }
    if upper.contains("CONSTRUCT") || upper.trim_start().starts_with("DESCRIBE") {
        let triples = state.store.construct(&q).map_err(AppError::from)?;
        let quads: Vec<oxigraph::model::Quad> = triples
            .into_iter()
            .map(|t| oxigraph::model::Quad::new(t.subject, t.predicate, t.object, oxigraph::model::GraphName::DefaultGraph))
            .collect();
        let body = crate::negotiate::serialize(&quads, crate::negotiate::Repr::Turtle, state.base())?;
        return Ok(([(axum::http::header::CONTENT_TYPE, "text/turtle; charset=utf-8")], body));
    }

    let bindings = state.store.select(&q).map_err(AppError::from)?;
    let rows: Vec<serde_json::Value> = bindings
        .rows
        .iter()
        .map(|r| {
            let mut o = serde_json::Map::new();
            for v in &bindings.vars {
                if let Some(term) = r.term(v) {
                    o.insert(v.clone(), term_json(term));
                }
            }
            serde_json::Value::Object(o)
        })
        .collect();
    let doc = serde_json::json!({
        "head": {"vars": bindings.vars},
        "results": {"bindings": rows}
    });
    let ct = if wants_json { "application/json" } else { "application/sparql-results+json" };
    Ok(([(axum::http::header::CONTENT_TYPE, ct)], doc.to_string()))
}

fn term_json(t: &oxigraph::model::Term) -> serde_json::Value {
    use oxigraph::model::Term;
    match t {
        Term::NamedNode(n) => serde_json::json!({"type": "uri", "value": n.as_str()}),
        Term::BlankNode(b) => serde_json::json!({"type": "bnode", "value": b.as_str()}),
        Term::Literal(l) => {
            let mut o = serde_json::json!({"type": "literal", "value": l.value()});
            if let Some(lang) = l.language() {
                o["xml:lang"] = serde_json::json!(lang);
            } else if l.datatype().as_str() != "http://www.w3.org/2001/XMLSchema#string" {
                o["datatype"] = serde_json::json!(l.datatype().as_str());
            }
            o
        }
    }
}
