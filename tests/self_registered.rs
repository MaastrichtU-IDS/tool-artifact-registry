//! A self-registered deployment owns its own record.
//!
//! The rule is narrow and the reasoning is worth stating once: a deployment that registers
//! itself re-states its fields on every announcement, so an edit made by anyone else survives
//! only until the next one and then disappears with no error and no trace. The registry refuses
//! the edit rather than accepting one it knows will be undone.
//!
//! What that leaves a curator is deliberately not "nothing": withdrawing the record and revoking
//! the credential both still work, and both are the remedies that actually stop a deployment
//! doing whatever prompted the edit.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tar::config::Config;
use tar::ops::Ops;
use tar::state::AppState;
use tar::store::OxigraphStore;
use tower::ServiceExt;

const BASE: &str = "https://reg.test.example";
const ROOT: &str = "test-root-token-0123456789";

struct Harness {
    app: axum::Router,
}

async fn harness() -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    let store = Arc::new(OxigraphStore::memory().unwrap());
    let ops = Ops::open(":memory:").await.unwrap();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state.clone()) }
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

    /// A software, an auto-registration key for it, and a deployment that registered itself.
    async fn self_registered(&self) -> (String, String, Value) {
        let (status, sw) = self
            .req("POST", "/api/v1/software", Some(ROOT), Some(json!({"name": "a-service", "kinds": ["service"]})))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{sw}");
        let software_id = sw["id"].as_str().unwrap().to_string();

        let (status, minted) = self
            .req("POST", &format!("/api/v1/software/{software_id}/tokens"), Some(ROOT), Some(json!({})))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{minted}");
        let key = minted["token"].as_str().unwrap().to_string();

        let (status, inst) = self
            .req(
                "PUT",
                "/api/v1/instances/self",
                Some(&key),
                Some(json!({"label": "as it calls itself", "instance_key": "prod",
                            "endpoint_url": "https://svc.example.org"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{inst}");
        (software_id, key, inst)
    }
}

#[tokio::test]
async fn a_curator_cannot_edit_a_record_the_deployment_maintains() {
    let h = harness().await;
    let (_, _, inst) = h.self_registered().await;
    let id = inst["id"].as_str().unwrap();

    let (status, body) = h
        .req("PATCH", &format!("/api/v1/instances/{id}"), Some(ROOT), Some(json!({"label": "renamed by hand"})))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // The refusal has to say what to do instead, or it reads as a bug.
    let detail = body["detail"].as_str().unwrap();
    assert!(detail.contains("/api/v1/instances/self"), "{detail}");
    assert!(detail.contains("overwritten"), "it explains why, not just that: {detail}");

    // And nothing changed.
    let (_, after) = h.req("GET", &format!("/api/v1/instances/{id}"), None, None).await;
    assert_eq!(after["label"], "as it calls itself", "{after}");
}

#[tokio::test]
async fn the_deployment_itself_still_maintains_it() {
    let h = harness().await;
    let (_, key, inst) = h.self_registered().await;
    let id = inst["id"].as_str().unwrap().to_string();

    // Through its own route, which is the one that will not be silently undone.
    let (status, updated) = h
        .req(
            "PUT",
            "/api/v1/instances/self",
            Some(&key),
            Some(json!({"instance_key": "prod", "label": "as it now calls itself"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["id"], id, "the same record, not a second one");
    assert_eq!(updated["label"], "as it now calls itself");
    assert_eq!(updated["endpoint_url"], "https://svc.example.org", "the rest of it survives");
}

#[tokio::test]
async fn a_curator_can_still_withdraw_it_and_that_is_the_remedy() {
    // Refusing the edit must not leave a curator unable to act on a deployment misbehaving.
    let h = harness().await;
    let (_, _, inst) = h.self_registered().await;
    let id = inst["id"].as_str().unwrap();

    let (status, _) = h.req("DELETE", &format!("/api/v1/instances/{id}"), Some(ROOT), None).await;
    assert!(status.is_success(), "withdrawing is still a curator's to do: {status}");

    let (_, list) = h.req("GET", "/api/v1/instances", None, None).await;
    assert_eq!(list["total"], 0, "a withdrawn deployment leaves the list: {list}");
}

#[tokio::test]
async fn a_curator_created_record_stays_editable() {
    // The rule is about who maintains the record, not about deployments in general.
    let h = harness().await;
    let (status, sw) = h
        .req("POST", "/api/v1/software", Some(ROOT), Some(json!({"name": "a-service", "kinds": ["service"]})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");

    let (status, inst) = h
        .req(
            "POST",
            "/api/v1/instances",
            Some(ROOT),
            Some(json!({"label": "created by a person", "software": sw["id"]})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inst}");

    let (status, patched) = h
        .req(
            "PATCH",
            &format!("/api/v1/instances/{}", inst["id"].as_str().unwrap()),
            Some(ROOT),
            Some(json!({"label": "renamed by a person"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["label"], "renamed by a person");
}

// ---------------------------------------------------------------- vocabulary branches

#[tokio::test]
async fn the_retired_topic_branch_no_longer_names_a_vocabulary() {
    // `topic-edam` put a vocabulary's name in an API value, which is the one place this project
    // does not put one — the value would be wrong the moment the retired vocabulary is a
    // different one. Records and clients in the wild still send it, so it keeps working.
    let h = harness().await;

    let (status, now) = h.req("GET", "/api/v1/vocab/search?branch=topic-retired&q=ontology", None, None).await;
    assert_eq!(status, StatusCode::OK, "{now}");
    let (status, old) = h.req("GET", "/api/v1/vocab/search?branch=topic-edam&q=ontology", None, None).await;
    assert_eq!(status, StatusCode::OK, "{old}");
    assert_eq!(now["items"], old["items"], "the old spelling means the same thing");
    assert!(!now["items"].as_array().unwrap().is_empty(), "the fixture needs a retired topic: {now}");

    // But it is never handed back.
    for item in now["items"].as_array().unwrap() {
        assert_eq!(item["branch"], "topic-retired", "{item}");
    }

    // And a retired topic is still not what the current topic branch offers.
    let (_, current) = h.req("GET", "/api/v1/vocab/search?branch=topic&q=ontology", None, None).await;
    for item in current["items"].as_array().unwrap() {
        assert_ne!(item["branch"], "topic-retired", "{item}");
    }
}
