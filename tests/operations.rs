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

/// §3: extractor rejections and 405 came out as plain text or nothing.
#[tokio::test]
async fn every_error_is_a_problem_document() {
    let h = harness().await;
    let send = |method: &str, uri: &str, ctype: Option<&str>, body: &'static str| {
        let mut b = Request::builder().method(method).uri(uri).header("authorization", format!("Bearer {ROOT}"));
        if let Some(c) = ctype {
            b = b.header("content-type", c);
        }
        h.app.clone().oneshot(b.body(Body::from(body)).unwrap())
    };
    for (resp, want) in [
        (send("POST", "/api/v1/software", Some("application/json"), "{not json").await.unwrap(), 400),
        (send("POST", "/api/v1/software", Some("text/csv"), "a,b").await.unwrap(), 415),
        (send("POST", "/api/v1/software", Some("application/json"), r#"{"name": 5}"#).await.unwrap(), 422),
        (send("DELETE", "/api/v1/registry", None, "").await.unwrap(), 405),
    ] {
        assert_eq!(resp.status().as_u16(), want);
        let allow = resp.headers().get("allow").cloned();
        let ctype = resp.headers()["content-type"].to_str().unwrap().to_string();
        assert_eq!(ctype, "application/problem+json", "{want}");
        let body: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(body["status"], want, "{body}");
        if want == 405 {
            assert!(allow.is_some(), "405 keeps its Allow header");
        } else {
            assert!(body["detail"].as_str().is_some_and(|d| !d.is_empty()), "{body}");
        }
    }
}

/// §3: /types had no next page, and search returned `limit` hits per kind, unranked.
#[tokio::test]
async fn types_page_and_search_honours_its_limit() {
    let h = harness().await;
    for label in ["Paged type A", "Paged type B", "Paged type C"] {
        h.created("/api/v1/types", json!({"label": label})).await;
    }
    let mut seen = Vec::new();
    let mut uri = "/api/v1/types?limit=2".to_string();
    loop {
        let (status, page) = h.send("GET", &uri, None, None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        seen.extend(page["items"].as_array().unwrap().iter().map(|t| t["iri"].as_str().unwrap().to_string()));
        match page["next_cursor"].as_str() {
            Some(c) => uri = format!("/api/v1/types?limit=2&cursor={}", urlencoding(c)),
            None => break,
        }
    }
    let total = h.send("GET", "/api/v1/types", None, None).await.1["total"].as_u64().unwrap() as usize;
    assert_eq!(seen.len(), total, "every type, once: {seen:?}");
    assert!(total >= 3);

    for name in ["zeta", "zeta two", "zeta three"] {
        let sw = h.created("/api/v1/software", json!({"name": name, "kinds": ["service"]})).await;
        h.created("/api/v1/instances", json!({"label": format!("{name} deployment"), "software": sw["id"]})).await;
    }
    let (_, found) = h.send("GET", "/api/v1/search?q=zeta&limit=2", None, None).await;
    let hits = found["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2, "{found}");
    assert_eq!(hits[0]["title"], "zeta", "the exact match ranks first: {found}");
}

fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// §3: a browser following a release, type or series IRI landed on the UI's "not found".
#[tokio::test]
async fn a_browser_is_sent_to_the_page_that_shows_a_release_or_a_type() {
    let h = harness().await;
    let sw = h.created("/api/v1/software", json!({"name": "has releases", "kinds": ["service"]})).await;
    let sid = sw["id"].as_str().unwrap();
    let rel = h.created(&format!("/api/v1/software/{sid}/releases"), json!({"version": "1.0.0"})).await;
    let ty = h.created("/api/v1/types", json!({"label": "Browsed type"})).await;
    let html = |path: String| {
        let req = Request::builder().uri(path).header("accept", "text/html").body(Body::empty()).unwrap();
        h.app.clone().oneshot(req)
    };
    let resp = html(format!("/release/{}", rel["id"].as_str().unwrap())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(resp.headers()["location"], format!("/software/{sid}").as_str());

    let path = ty["iri"].as_str().unwrap().trim_start_matches(BASE).to_string();
    let resp = html(path).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(resp.headers()["location"].to_str().unwrap().starts_with("/artifacts?conforms_to=https%3A"));
}
