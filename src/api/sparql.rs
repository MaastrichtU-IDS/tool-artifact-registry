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
    /// Optional so that `GET /sparql` with no query is a page rather than a deserialisation
    /// error: the SPA's query screen lives at this URL, and the registry's IRIs and its UI
    /// routes are deliberately the same URLs (handoff §3).
    pub query: Option<String>,
    pub format: Option<String>,
}

pub async fn query_get(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    headers: HeaderMap,
    Query(p): Query<SparqlParams>,
) -> axum::response::Response {
    let Some(q) = p.query else {
        // A browser asked for the endpoint itself: serve the SPA, which routes to /sparql and
        // renders the query editor. Anything else forgot a required parameter and is told so.
        let wants_html = headers
            .get(axum::http::header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("text/html"));
        return if wants_html {
            crate::api::web::spa_response(&state).await
        } else {
            AppError::bad_request("the `query` parameter is required").into_response()
        };
    };
    match run(state, principal, headers, q, p.format).await {
        Ok(r) => r.into_response(),
        Err(e) => e.into_response(),
    }
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
    if !state.config.sparql_public {
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
    // Parse here so a malformed query is a 400 carrying the parser's own message (line and
    // column), rather than a 500 whose detail only echoes the query back. The store parses
    // again when it evaluates; queries are small and this runs once per request.
    if let Err(e) = oxigraph::sparql::SparqlEvaluator::new().parse_query(&q) {
        return Err(AppError::bad_request(format!("SPARQL syntax error: {e}")));
    }

    let wants_json = format.as_deref() == Some("json")
        || headers
            .get(axum::http::header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("json"));

    // ASK and CONSTRUCT are answered too — a peer registry uses CONSTRUCT for stubs.
    // The form is read after the prologue: `PREFIX … ASK { … }` is the normal way to write an
    // ASK, and matching on the raw first word sent every prefixed ASK and DESCRIBE down the
    // SELECT path, where the store answers "expected a SELECT query" with a 500.
    let form = query_form(&q);
    if form == "ASK" {
        let b = state.store.ask(&q).map_err(AppError::from)?;
        return Ok((
            [(axum::http::header::CONTENT_TYPE, "application/sparql-results+json")],
            serde_json::json!({"head": {}, "boolean": b}).to_string(),
        ));
    }
    if form == "CONSTRUCT" || form == "DESCRIBE" {
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

/// The query form — `SELECT`, `ASK`, `CONSTRUCT` or `DESCRIBE` — read past the SPARQL
/// prologue, since `BASE`/`PREFIX` declarations and comments legally precede it.
fn query_form(q: &str) -> &'static str {
    let mut rest = q;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix('#') {
            // A comment runs to the end of the line; an unterminated one ends the query.
            rest = after.split_once('\n').map(|(_, r)| r).unwrap_or("");
            continue;
        }
        let head: String = rest.chars().take(9).collect::<String>().to_uppercase();
        if head.starts_with("PREFIX") || head.starts_with("BASE") {
            // Both declarations end with an IRIREF, so skip past its closing '>'.
            match rest.split_once('>') {
                Some((_, after)) => rest = after,
                None => return "SELECT",
            }
            continue;
        }
        for form in ["SELECT", "ASK", "CONSTRUCT", "DESCRIBE"] {
            if head.starts_with(form) {
                return form;
            }
        }
        return "SELECT";
    }
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
