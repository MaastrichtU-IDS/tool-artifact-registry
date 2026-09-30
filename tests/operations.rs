//! Regressions for the correctness half of the pre-deploy audit (task 08, §2).

use axum::body::Body;
use axum::http::{Request, StatusCode};
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
}

async fn harness() -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    let store = common::test_store().await;
    let ops = Ops::open(":memory:").await.unwrap();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state) }
}

impl Harness {
    async fn send(&self, method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(uri).header("accept", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let req = match body {
            Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into())))
    }

    async fn created(&self, uri: &str, body: Value) -> Value {
        let (status, v) = self.send("POST", uri, Some(ROOT), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{uri}: {v}");
        v
    }
}

/// §2.8: withdrawn records answered 200 while the docs promised 410, and a PATCH brought them
/// back to life.
#[tokio::test]
async fn a_withdrawn_record_answers_gone_and_stays_withdrawn() {
    let h = harness().await;
    let sw = h.created("/api/v1/software", json!({"name": "retired", "kinds": ["service"]})).await;
    let inst = h.created("/api/v1/instances", json!({"label": "old deployment", "software": sw["id"]})).await;
    for (kind, id) in [("software", &sw["id"]), ("instances", &inst["id"])] {
        let uri = format!("/api/v1/{kind}/{}", id.as_str().unwrap());
        let (status, _) = h.send("DELETE", &uri, Some(ROOT), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{uri}");

        let (status, body) = h.send("GET", &uri, None, None).await;
        assert_eq!(status, StatusCode::GONE, "{uri}: {body}");
        assert_eq!(body["tombstoned"], true, "the record still says what it was: {body}");

        let (status, body) =
            h.send("PATCH", &uri, Some(ROOT), Some(json!({"label": "zombie", "tagline": "zombie"}))).await;
        assert_eq!(status, StatusCode::GONE, "{uri}: an edit must not revive it: {body}");
        let (status, _) = h.send("GET", &uri, None, None).await;
        assert_eq!(status, StatusCode::GONE, "{uri} is still withdrawn");
    }
    // The IRI itself too, in a representation other than JSON.
    let iri = sw["iri"].as_str().unwrap().trim_start_matches(BASE).to_string();
    let req = Request::builder().uri(&iri).header("accept", "text/turtle").body(Body::empty()).unwrap();
    assert_eq!(h.app.clone().oneshot(req).await.unwrap().status(), StatusCode::GONE);
}

/// §2.8: a withdrawn deployment's token kept advertising runs as it.
#[tokio::test]
async fn a_withdrawn_deployments_token_stops_working() {
    let h = harness().await;
    let sw = h.created("/api/v1/software", json!({"name": "svc", "kinds": ["service"]})).await;
    let inst = h.created("/api/v1/instances", json!({"label": "d", "software": sw["id"]})).await;
    let id = inst["id"].as_str().unwrap();
    let tok = h.created(&format!("/api/v1/instances/{id}/tokens"), json!({})).await;
    let token = tok["token"].as_str().unwrap();
    let advertise = json!({"run": {"external_key": "ci/1", "status": "success"}, "artifacts": [{"title": "out"}]});

    let (status, body) = h.send("POST", "/api/v1/advertise/produced", Some(token), Some(advertise.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "before: {body}");
    h.send("DELETE", &format!("/api/v1/instances/{id}"), Some(ROOT), None).await;
    let (status, body) = h.send("POST", "/api/v1/advertise/produced", Some(token), Some(advertise)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "after: {body}");
    assert!(body["detail"].as_str().unwrap().contains("withdrawn"), "{body}");
}

/// §2.9: an absurd expiry panicked the handler and dropped the connection.
#[tokio::test]
async fn an_absurd_token_expiry_is_refused_not_a_crash() {
    let h = harness().await;
    let sw = h.created("/api/v1/software", json!({"name": "svc", "kinds": ["service"]})).await;
    let inst = h.created("/api/v1/instances", json!({"label": "d", "software": sw["id"]})).await;
    let uri = format!("/api/v1/instances/{}/tokens", inst["id"].as_str().unwrap());
    for expiry in ["1000000000000000s", "18446744073709551615d", "50000d"] {
        let (status, body) = h.send("POST", &uri, Some(ROOT), Some(json!({"expires_in": expiry}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{expiry}: {body}");
    }
    let (status, _) = h.send("POST", &uri, Some(ROOT), Some(json!({"expires_in": "90d"}))).await;
    assert_eq!(status, StatusCode::CREATED);
}

/// §2.11: acknowledging a cursor past the end silently hid every later delivery.
#[tokio::test]
async fn a_cursor_past_the_end_is_refused() {
    let h = harness().await;
    let sw = h.created("/api/v1/software", json!({"name": "svc", "kinds": ["service"]})).await;
    let inst = h.created("/api/v1/instances", json!({"label": "d", "software": sw["id"]})).await;
    let sub = h
        .created(&format!("/api/v1/instances/{}/subscriptions", inst["id"].as_str().unwrap()), json!({"filter": {}}))
        .await;
    let uri = format!("/api/v1/subscriptions/{}/deliveries/ack", sub["subscription"]["id"].as_str().unwrap());
    let (status, body) = h.send("POST", &uri, Some(ROOT), Some(json!({"cursor": 999_999}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = h.send("POST", &uri, Some(ROOT), Some(json!({"cursor": 0}))).await;
    assert_eq!(status, StatusCode::OK);
}

/// §2.15: /metrics walked the whole store for anyone who asked.
#[tokio::test]
async fn metrics_are_for_the_admin_and_counted_at_most_every_30s() {
    let h = harness().await;
    let (status, _) = h.send("GET", "/metrics", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (_, before) = h.send("GET", "/metrics", Some(ROOT), None).await;
    h.created("/api/v1/software", json!({"name": "new", "kinds": ["service"]})).await;
    let (status, after) = h.send("GET", "/metrics", Some(ROOT), None).await;
    assert_eq!(status, StatusCode::OK);
    let triples = |v: &Value| v.as_str().unwrap().lines().find(|l| l.starts_with("tar_triples ")).unwrap().to_string();
    assert_eq!(triples(&before), triples(&after), "the second scrape reused the count");
}

/// §2.14: there was no online backup of the operational database.
#[tokio::test]
async fn the_operational_database_can_be_backed_up_while_running() {
    // On disk, as in production: an in-memory SQLite database "copies" into memory.
    let dir = std::env::temp_dir().join(format!("tar-ops-backup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    config.data_dir = dir.to_string_lossy().into_owned();
    let ops = Ops::open(&dir.join("ops.db").to_string_lossy()).await.unwrap();
    let state = Arc::new(AppState::from_parts(config, common::test_store().await, ops));
    let h = Harness { app: tar::app(state) };

    let (status, _) = h.send("GET", "/admin/ops-backup", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let req = Request::builder()
        .uri("/admin/ops-backup")
        .header("authorization", format!("Bearer {ROOT}"))
        .body(Body::empty())
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
    assert!(bytes.starts_with(b"SQLite format 3\0"), "{} bytes", bytes.len());
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(left.iter().all(|n| !n.to_string_lossy().contains("backup")), "the copy is removed: {left:?}");
    std::fs::remove_dir_all(&dir).ok();
}
