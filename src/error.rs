//! RFC 9457 `application/problem+json` for every error path (spec §7.9).

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct AppError {
    pub status: StatusCode,
    /// RFC 9457 `type` — a URI reference identifying the problem class.
    pub kind: &'static str,
    pub title: String,
    pub detail: Option<String>,
    /// SHACL validation report, Turtle, for 422 (spec §7.9).
    pub report: Option<String>,
    /// Extra members merged into the problem document.
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(status: StatusCode, kind: &'static str, title: impl Into<String>) -> Self {
        Self { status, kind, title: title.into(), detail: None, report: None, extra: Default::default() }
    }
    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
    pub fn with(mut self, key: &str, value: serde_json::Value) -> Self {
        self.extra.insert(key.to_string(), value);
        self
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not-found", "Not found").detail(what)
    }
    pub fn bad_request(d: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad-request", "Bad request").detail(d)
    }
    pub fn unauthorized(d: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "Authentication required").detail(d)
    }
    pub fn forbidden(d: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", "Forbidden").detail(d)
    }
    pub fn conflict(d: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", "Conflict").detail(d)
    }
    pub fn gone(d: impl Into<String>) -> Self {
        Self::new(StatusCode::GONE, "tombstoned", "Record is tombstoned").detail(d)
    }
    /// Over a rate limit (`src/ratelimit.rs`). The middleware adds `Retry-After`.
    pub fn too_many_requests(d: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "rate-limited", "Too many requests").detail(d)
    }
    /// An upstream the registry depends on failed. Distinct from `internal` because the fault
    /// is somebody else's server and the caller can usually see that from the message.
    pub fn bad_gateway(d: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "upstream-failed", "Upstream request failed").detail(d)
    }
    pub fn internal(d: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "Internal error").detail(d)
    }
    pub fn unsupported_media(d: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_ACCEPTABLE, "not-acceptable", "Not acceptable").detail(d)
    }
    /// SHACL write validation failure — the report travels with the problem document so the
    /// UI can map `sh:resultPath` back to form fields (handoff §5.7).
    pub fn validation(report_ttl: String, summary: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            kind: "shacl-validation-failed",
            title: "Write rejected by SHACL validation".into(),
            detail: Some(summary.into()),
            report: Some(report_ttl),
            extra: Default::default(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "type": format!("https://w3id.org/tar/problem/{}", self.kind),
            "title": self.title,
            "status": self.status.as_u16(),
        });
        let obj = body.as_object_mut().expect("object");
        // A 500's detail is for the operator, not the caller: it has carried generated SPARQL,
        // store errors and SQL, which tell an attacker how to shape the next request. The
        // caller gets a reference; the log has the rest under the same reference.
        let detail = if self.status == StatusCode::INTERNAL_SERVER_ERROR {
            let reference = uuid::Uuid::now_v7().to_string();
            tracing::error!(%reference, detail = self.detail.as_deref().unwrap_or(""), "internal error");
            Some(format!("something went wrong on the server; quote reference {reference} to its operator"))
        } else {
            self.detail
        };
        if let Some(d) = detail {
            obj.insert("detail".into(), json!(d));
        }
        if let Some(r) = self.report {
            obj.insert("report".into(), json!(r));
            obj.insert("report_media_type".into(), json!("text/turtle"));
        }
        for (k, v) in self.extra {
            obj.insert(k, v);
        }
        let mut resp = (self.status, axum::Json(body)).into_response();
        resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/problem+json"));
        if self.status == StatusCode::UNAUTHORIZED {
            resp.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        resp
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        let text = e.to_string();
        // The store's two limits are about the query, not a fault: say so to whoever sent it.
        if text.contains("TAR_SPARQL_TIMEOUT") {
            return AppError::new(StatusCode::SERVICE_UNAVAILABLE, "query-timeout", "Query took too long").detail(text);
        }
        if text.contains("add a LIMIT") || text.contains("narrow it") {
            return AppError::new(StatusCode::UNPROCESSABLE_ENTITY, "result-too-large", "Result too large")
                .detail(text);
        }
        AppError::internal(text)
    }
}

impl From<oxigraph::store::StorageError> for AppError {
    fn from(e: oxigraph::store::StorageError) -> Self {
        AppError::internal(format!("graph store: {e}"))
    }
}

impl From<oxigraph::sparql::QueryEvaluationError> for AppError {
    fn from(e: oxigraph::sparql::QueryEvaluationError) -> Self {
        AppError::internal(format!("sparql: {e}"))
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::internal(format!("ops store: {e}"))
    }
}

/// Run synchronous, store-touching work on Tokio's blocking thread pool instead of the async
/// worker thread that is polling this task.
///
/// `GraphStore` is a synchronous trait (spec §5.3: "structural requirement, not a v2
/// aspiration"). Against the embedded default — everybody running this today — a call never
/// leaves the process and this makes no measurable difference. Against `TAR_SPARQL_ENDPOINT` a
/// call is an HTTP round trip, and running it directly inside an async task occupies the
/// worker thread that is polling that task for the whole of it: one slow remote query then
/// delays every other request that worker was about to advance, on a multi-threaded runtime
/// with more requests than workers. `spawn_blocking` moves the call onto Tokio's dedicated
/// blocking pool — sized for exactly this bimodal "usually instant, sometimes I/O" shape — and
/// gives the worker back to the scheduler for the duration.
///
/// Lives here rather than in `api`, which is where it started: `auth::jwt` needs the identical
/// treatment on the authentication path, and that path runs before any handler in `api` is
/// reached at all — every bearer-token request resolves an `Instance` or `Software` binding
/// with a synchronous store read, on every single call, not only the ones a specific handler
/// happens to make. `api::blocking` re-exports this so existing call sites there are unchanged.
///
/// Not a fix for every call site uniformly: one that only ever touches the store is one call to
/// this; one that interleaves a store write with genuine async I/O of its own (an audit log
/// entry, a repository sync fetch) wraps each synchronous segment separately and leaves the
/// `.await`s between them where they are.
///
/// A panic inside `f` surfaces as [`AppError::internal`] here rather than unwinding the pool
/// thread that ran it — the panic message becomes the error detail rather than being lost.
pub async fn blocking<T, F>(f: F) -> AppResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> AppResult<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => Err(AppError::internal(format!("a store operation panicked: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    /// §1.7: a 500 told the caller the generated query, the store error or the SQL.
    #[tokio::test]
    async fn a_500_gives_a_reference_not_the_internals() {
        let resp = AppError::internal("SELECT ?secret WHERE { GRAPH <urn:tar:local> { ?s ?p ?o } }").into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("SELECT") && text.contains("reference"), "{text}");
    }

    #[test]
    fn the_stores_limits_are_answered_as_what_they_are() {
        let timeout =
            AppError::from(anyhow::anyhow!("the query ran longer than TAR_SPARQL_TIMEOUT (60s) and was stopped"));
        assert_eq!(timeout.status, StatusCode::SERVICE_UNAVAILABLE);
        let big =
            AppError::from(anyhow::anyhow!("the query returns more than 100000 rows; add a LIMIT and page through it"));
        assert_eq!(big.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(AppError::from(anyhow::anyhow!("disk on fire")).status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
