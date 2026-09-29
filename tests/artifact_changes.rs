//! Correcting and withdrawing an advertised artifact: who may, what may change, and what a
//! withdrawal leaves behind.

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
    async fn req(&self, method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(uri);
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

    /// A deployment of one software, with a token that may advertise as it.
    async fn deployment(&self, software: &str, label: &str) -> String {
        let (status, inst) = self
            .req("POST", "/api/v1/instances", Some(ROOT), Some(json!({"label": label, "software": software})))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{inst}");
        let id = inst["id"].as_str().unwrap();
        let (status, tok) = self
            .req(
                "POST",
                &format!("/api/v1/instances/{id}/tokens"),
                Some(ROOT),
                Some(json!({"scopes": ["advertise:produce", "advertise:consume"]})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{tok}");
        tok["token"].as_str().unwrap().to_string()
    }
}

async fn setup() -> (Harness, String, String, String) {
    let h = harness().await;
    let (status, sw) =
        h.req("POST", "/api/v1/software", Some(ROOT), Some(json!({"name": "producer", "kinds": ["service"]}))).await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let sw = sw["id"].as_str().unwrap().to_string();
    let mine = h.deployment(&sw, "mine").await;
    let theirs = h.deployment(&sw, "theirs").await;
    let (status, out) = h
        .req(
            "POST",
            "/api/v1/advertise/produced",
            Some(&mine),
            Some(json!({"run": {"external_key": "ci/1", "status": "success"},
                         "artifacts": [{"external_key": "ci/1/report", "title": "Reprot with a typo",
                                        "keywords": ["validation"],
                                        "distributions": [{"download_url": "https://files.example.org/old.ttl",
                                                           "media_type": "text/turtle"}]}]})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    let id = out["artifacts"][0].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    (h, mine, theirs, id)
}

#[tokio::test]
async fn the_deployment_that_advertised_it_can_correct_it() {
    let (h, mine, theirs, id) = setup().await;
    let uri = format!("/api/v1/artifacts/{id}");

    let (status, body) = h.req("PATCH", &uri, Some(&theirs), Some(json!({"title": "not yours"}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "another deployment may not: {body}");
    let (status, body) = h.req("PATCH", &uri, None, Some(json!({"title": "anonymous"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    let (status, a) = h
        .req(
            "PATCH",
            &uri,
            Some(&mine),
            Some(json!({"title": "Report", "license": "https://spdx.org/licenses/CC-BY-4.0",
                         "distributions": [{"download_url": "https://files.example.org/new.ttl",
                                            "media_type": "text/turtle", "availability": "restricted",
                                            "access_request_url": "https://files.example.org/access"}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{a}");
    assert_eq!(a["title"], "Report");
    assert_eq!(a["license"], "https://spdx.org/licenses/CC-BY-4.0");
    assert!(a["modified"].is_string(), "{a}");
    let dists = a["distributions"].as_array().unwrap();
    assert_eq!(dists.len(), 1, "the distributions were replaced, not added to: {a}");
    assert_eq!(dists[0]["download_url"], "https://files.example.org/new.ttl");
    assert_eq!(a["availability"], "restricted");
    // Untouched fields stay as they were, and so does the history.
    assert_eq!(a["keywords"], json!(["validation"]), "{a}");
    assert!(a["was_generated_by"].is_string(), "{a}");

    // A curator may correct anyone's.
    let (status, a) = h.req("PATCH", &uri, Some(ROOT), Some(json!({"description": "Checked by a curator."}))).await;
    assert_eq!(status, StatusCode::OK, "{a}");
    assert_eq!(a["title"], "Report");
}

#[tokio::test]
async fn its_history_is_not_editable_and_its_fields_are_still_validated() {
    let (h, mine, _, id) = setup().await;
    let uri = format!("/api/v1/artifacts/{id}");
    for field in [
        json!({"conforms_to": "https://reg.test.example/type/rdf-graph"}),
        json!({"was_derived_from": ["https://elsewhere.example/artifact/1"]}),
    ] {
        let (status, body) = h.req("PATCH", &uri, Some(&mine), Some(field.clone())).await;
        assert!(status.is_client_error(), "{field} must be refused, not ignored: {status} {body}");
    }
    let (status, body) = h.req("PATCH", &uri, Some(&mine), Some(json!({"license": "CC-BY-4.0"}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "a licence must be an IRI: {body}");
}

#[tokio::test]
async fn a_withdrawn_artifact_still_resolves_but_is_no_longer_offered() {
    let (h, mine, theirs, id) = setup().await;
    let uri = format!("/api/v1/artifacts/{id}");
    let (status, _) = h.req("DELETE", &uri, Some(&theirs), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = h.req("DELETE", &uri, Some(&mine), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Its IRI still answers, marked withdrawn, so lineage that points at it does not dangle.
    let (status, a) = h.req("GET", &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "{a}");
    assert_eq!(a["tombstoned"], true, "{a}");
    assert!(a["attributed_to"].as_str().is_some_and(|s| !s.contains("root")), "withdrawing is not advertising: {a}");

    let (_, list) = h.req("GET", "/api/v1/artifacts", None, None).await;
    assert!(!list["items"].as_array().unwrap().iter().any(|i| i["id"] == id.as_str()), "{list}");
    let (_, found) = h.req("GET", "/api/v1/search?q=typo", None, None).await;
    assert!(found["hits"].as_array().unwrap().is_empty(), "search does not offer it: {found}");

    // Withdrawing again is not an error; changing it is.
    let (status, _) = h.req("DELETE", &uri, Some(&mine), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = h.req("PATCH", &uri, Some(&mine), Some(json!({"title": "back again"}))).await;
    assert_eq!(status, StatusCode::GONE, "a withdrawn artifact is not revived by an edit: {body}");
}
