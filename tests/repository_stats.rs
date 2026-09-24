//! Repository liveness (design note `docs/specs/2026-09-24-repository-liveness.md`).
//!
//! GitHub is never contacted: `poll_once` takes the fetch as a closure, and these tests pass
//! one that answers from a table and records what it was asked.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tar::config::Config;
use tar::domain::forge::{poll_once, RepoStats};
use tar::ops::Ops;
use tar::state::AppState;
use tower::ServiceExt;

const ROOT: &str = "test-root-token-liveness-0123";

struct Harness {
    app: axum::Router,
    state: Arc<AppState>,
}

async fn harness() -> Harness {
    let mut config = Config::for_test("https://reg.test.example");
    config.root_token = Some(ROOT.into());
    let state =
        Arc::new(AppState::from_parts(config, common::test_store().await, Ops::open(":memory:").await.unwrap()));
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state.clone()), state }
}

impl Harness {
    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let b = Request::builder().method(method).uri(uri).header("authorization", format!("Bearer {ROOT}"));
        let req = match body {
            Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn software(&self, name: &str, repo: &str) -> String {
        let (s, v) = self.send("POST", "/api/v1/software", Some(json!({"name": name, "code_repository": repo}))).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    async fn get(&self, id: &str) -> Value {
        let (s, v) = self.send("GET", &format!("/api/v1/software/{id}"), None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    /// One poll pass with every record due and no pacing, against a stand-in forge that
    /// answers `result` for any repository. Returns the repositories it was asked about.
    async fn poll(&self, result: Result<RepoStats, String>) -> Vec<String> {
        let asked = Arc::new(Mutex::new(Vec::new()));
        poll_once(&self.state, Duration::ZERO, Duration::ZERO, |repo| {
            asked.lock().unwrap().push(repo);
            let r = result.clone();
            async move { r }
        })
        .await;
        let mut v = asked.lock().unwrap().clone();
        v.sort();
        v
    }
}

fn stats(stars: i64, forks: i64) -> RepoStats {
    RepoStats { stars: Some(stars), forks: Some(forks), pushed_at: Some("2026-09-20T10:00:00Z".into()) }
}

#[tokio::test]
async fn a_poll_stores_stats_for_a_github_record_and_skips_one_hosted_elsewhere() {
    let h = harness().await;
    let gh = h.software("on-github", "https://github.com/MaastrichtU-IDS/shacl-manager.git").await;
    let elsewhere = h.software("on-gitlab", "https://gitlab.com/someone/tool").await;

    // Before any poll there is nothing to show, and nothing is invented.
    assert!(h.get(&gh).await.get("repository_stats").is_none());

    assert_eq!(h.poll(Ok(stats(42, 7))).await, vec!["MaastrichtU-IDS/shacl-manager"]);

    let s = &h.get(&gh).await["repository_stats"];
    assert_eq!(s["stars"], 42);
    assert_eq!(s["forks"], 7);
    assert_eq!(s["last_commit_at"], "2026-09-20T10:00:00Z");
    assert!(s["fetched_at"].is_string(), "{s}");
    assert!(s.get("last_error").is_none(), "{s}");

    // The GitLab record was never asked about, so it has no stats rather than zeros.
    assert!(h.get(&elsewhere).await.get("repository_stats").is_none());
    assert!(h
        .state
        .ops
        .repository_stats(&format!("https://reg.test.example/software/{elsewhere}"))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_forge_error_is_recorded_and_the_old_numbers_are_kept() {
    let h = harness().await;
    let id = h.software("flaky", "https://github.com/a/flaky").await;
    h.poll(Ok(stats(10, 2))).await;
    let before = h.get(&id).await["repository_stats"].clone();

    h.poll(Err("GitHub returned 502".into())).await;

    let after = &h.get(&id).await["repository_stats"];
    assert_eq!(after["stars"], 10);
    assert_eq!(after["forks"], 2);
    assert_eq!(after["fetched_at"], before["fetched_at"], "a failure is not a fetch");
    assert_eq!(after["last_error"], "GitHub returned 502");

    // The next success clears the error.
    h.poll(Ok(stats(11, 2))).await;
    let s = &h.get(&id).await["repository_stats"];
    assert_eq!(s["stars"], 11);
    assert!(s.get("last_error").is_none(), "{s}");
}

#[tokio::test]
async fn a_record_that_moved_repository_does_not_show_the_old_ones_numbers() {
    let h = harness().await;
    let id = h.software("moved", "https://github.com/a/old").await;
    h.poll(Ok(stats(99, 9))).await;

    let (s, v) = h
        .send("PATCH", &format!("/api/v1/software/{id}"), Some(json!({"code_repository": "https://github.com/a/new"})))
        .await;
    assert!(s.is_success(), "{s} {v}");
    // Not polled yet: the stored numbers belong to a/old, so none are shown.
    assert!(h.get(&id).await.get("repository_stats").is_none());

    // And a failed poll of the new repository does not resurrect them either.
    assert_eq!(h.poll(Err("GitHub returned 502".into())).await, vec!["a/new"]);
    assert!(h.get(&id).await.get("repository_stats").is_none());
}

#[tokio::test]
async fn a_record_polled_within_the_interval_is_not_polled_again() {
    let h = harness().await;
    h.software("fresh", "https://github.com/a/fresh").await;
    h.poll(Ok(stats(1, 1))).await;
    let asked = Arc::new(Mutex::new(0));
    let n = poll_once(&h.state, Duration::from_secs(3600), Duration::ZERO, |_| {
        *asked.lock().unwrap() += 1;
        async { Ok(stats(1, 1)) }
    })
    .await;
    assert_eq!((n, *asked.lock().unwrap()), (0, 0), "a restart must not re-spend the rate budget");
}
