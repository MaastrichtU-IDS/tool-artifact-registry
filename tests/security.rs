//! Regressions for the pre-deploy security audit (`docs/superpowers/tasks/08-pre-deploy-audit.md`).

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tar::config::Config;
use tar::ops::Ops;
use tar::state::AppState;
use tower::ServiceExt;

mod common;

const BASE: &str = "https://reg.test.example";
const ROOT: &str = "test-root-token-0123456789";

struct Harness {
    app: axum::Router,
    state: Arc<AppState>,
}

async fn harness_with(tune: impl FnOnce(&mut Config)) -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    tune(&mut config);
    let store = common::test_store().await;
    let ops = Ops::open(":memory:").await.unwrap();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state.clone()), state }
}

async fn harness() -> Harness {
    harness_with(|_| {}).await
}

impl Harness {
    async fn send(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value, HeaderMap) {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let req = match body {
            Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into())),
            headers,
        )
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let (s, v, _) = self.send("GET", uri, None, None).await;
        (s, v)
    }
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// §1.1: a filter value closed the angle brackets and appended its own query. With a condition
/// in the appended part, `total` answered true or false about anything in the store — an
/// anonymous read oracle that worked with `/sparql` switched off.
#[tokio::test]
async fn a_filter_value_cannot_change_the_query() {
    let h = harness_with(|c| c.sparql_public = false).await;
    let (status, _, _) = h
        .send(
            "POST",
            "/api/v1/software",
            Some(ROOT),
            Some(json!({"name": "target", "kinds": ["service"], "license": "https://spdx.org/licenses/MIT"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // Always-true and always-false versions of the same probe. Before the fix these returned
    // 1 and 0; now both name an IRI that does not exist, so both return 0.
    let probe = |cond: &str| format!("https://spdx.org/licenses/MIT> }} FILTER({cond}) {{ ?s ?p <https://x");
    for cond in ["true", "false"] {
        let (status, body) = h.get(&format!("/api/v1/software?license={}", enc(&probe(cond)))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 0, "condition {cond} must not leak: {body}");
    }

    // A real IRI still filters.
    let (_, body) = h.get(&format!("/api/v1/software?license={}", enc("https://spdx.org/licenses/MIT"))).await;
    assert_eq!(body["total"], 1, "{body}");

    // Every IRI-valued filter on every list: never a 500 echoing a broken query.
    let junk = enc("x> } UNION { ?s ?p ?o } #");
    for path in [
        "/api/v1/software?license=",
        "/api/v1/software?publisher=",
        "/api/v1/software?topic=",
        "/api/v1/software?produces=",
        "/api/v1/software?consumes=",
        "/api/v1/software?registry=",
        "/api/v1/artifacts?conforms_to=",
        "/api/v1/artifacts?license=",
        "/api/v1/artifacts?instance=",
        "/api/v1/artifacts?software=",
        "/api/v1/artifacts?run=",
        "/api/v1/artifacts?registry=",
        "/api/v1/instances?software=",
        "/api/v1/instances?operator=",
        "/api/v1/instances?release=",
        "/api/v1/instances?registry=",
        "/api/v1/runs?instance=",
        "/api/v1/runs?software=",
        "/api/v1/capabilities?produces=",
        "/api/v1/capabilities?consumes=",
        "/api/v1/vocab/resolve?iris=",
    ] {
        let (status, body) = h.get(&format!("{path}{junk}")).await;
        assert!(status.is_success() || status == StatusCode::BAD_REQUEST, "{path}: {status} {body}");
        assert!(!body.to_string().contains("SELECT"), "{path} must not echo a query: {body}");
    }
    // Ids in a path position are the same hole by another door.
    for path in [
        "/api/v1/artifacts/x%3E%20%7D%20%23/lineage?direction=down",
        "/api/v1/instances/x%3E%20%7D/runs",
        "/api/v1/instances/x%3E%20%7D/artifacts",
    ] {
        let (status, body) = h.get(path).await;
        assert!(status != StatusCode::INTERNAL_SERVER_ERROR, "{path}: {body}");
    }
    let _ = &h.state;
}
