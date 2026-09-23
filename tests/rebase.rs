//! Moving a registry to a new base IRI (design note `docs/specs/2026-09-23-base-iri-rebase.md`).
//!
//! Records, a run and a deployment's token are created under an old base; the same stores are
//! then opened under a new base, as a restarted registry would open them, and rebased.

mod common;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tar::config::Config;
use tar::ops::Ops;
use tar::state::AppState;
use tar::store::GraphStore;
use tower::ServiceExt;

const OLD: &str = "https://old.example.org";
const NEW: &str = "https://reg.example.org";
const ROOT: &str = "test-root-token-rebase-0123";

struct Registry {
    app: axum::Router,
    state: Arc<AppState>,
}

fn open(base: &str, previous: &[&str], store: Arc<dyn GraphStore>, ops: Ops) -> Registry {
    let mut config = Config::for_test(base);
    config.root_token = Some(ROOT.into());
    config.previous_base_iris = previous.iter().map(|s| s.to_string()).collect();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();
    Registry { app: tar::app(state.clone()), state }
}

impl Registry {
    async fn send(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
        host: Option<&str>,
    ) -> (StatusCode, Value, HeaderMap) {
        let mut b = Request::builder().method(method).uri(uri).header("accept", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        if let Some(h) = host {
            b = b.header("host", h);
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

    async fn post(&self, uri: &str, token: &str, body: Value) -> Value {
        let (s, v, _) = self.send("POST", uri, Some(token), Some(body), None).await;
        assert!(s.is_success(), "POST {uri}: {s} {v}");
        v
    }

    fn local_graph(&self) -> String {
        self.state.store.dump_nquads(Some(tar::ns::G_LOCAL)).unwrap()
    }
}

#[tokio::test]
async fn a_rebased_registry_answers_under_the_new_base_and_for_the_old_one() {
    let store = common::test_store().await;
    let ops = Ops::open(":memory:").await.unwrap();

    // Life under the old base: a software record, a deployment of it with a token, and an
    // artifact that deployment advertised.
    let old = open(OLD, &[], store.clone(), ops.clone());
    let sw = old.post("/api/v1/software", ROOT, json!({"name": "mover", "kind": "service"})).await;
    let software_id = sw["id"].as_str().unwrap().to_string();
    let inst = old
        .post(
            "/api/v1/instances",
            ROOT,
            json!({"label": "mover-prod", "software": software_id, "endpoint_url": "https://mover.example.org",
                   "allowed_scopes": ["advertise:produce", "advertise:consume"]}),
        )
        .await;
    let instance_id = inst["id"].as_str().unwrap().to_string();
    let tok = old
        .post(
            &format!("/api/v1/instances/{instance_id}/tokens"),
            ROOT,
            json!({"scopes": ["advertise:produce", "advertise:consume"]}),
        )
        .await;
    let token = tok["token"].as_str().unwrap().to_string();
    let produced_body = json!({"run": {"external_key": "ci/1", "status": "success"},
                               "artifacts": [{"title": "report", "conforms_to": "http://edamontology.org/data_2048",
                                              "distributions": [{"download_url": "https://mover.example.org/r.ttl"}]}]});
    let produced = old.post("/api/v1/advertise/produced", &token, produced_body.clone()).await;
    let old_artifact = produced["artifacts"][0].as_str().unwrap().to_string();
    assert!(old_artifact.starts_with(&format!("{OLD}/artifact/")), "{old_artifact}");

    // The registry restarts under the new base. Before the rebase it can tell something is off.
    let new = open(NEW, &[OLD], store.clone(), ops.clone());
    let bundles_before = new.state.store.dump_nquads(None).unwrap();
    assert_eq!(tar::rebase::stray_base(&new.state).unwrap().as_deref(), Some(OLD));

    // A dry run counts and writes nothing.
    let dry = tar::rebase::run(&new.state, OLD, true).await.unwrap();
    assert!(dry.statements > 0, "{dry:?}");
    assert!(dry.rows.iter().any(|(c, _)| c == "api_tokens.instance_iri"), "{dry:?}");
    assert!(new.local_graph().contains(OLD), "a dry run must not write");
    assert_eq!(new.state.store.dump_nquads(None).unwrap(), bundles_before, "nothing at all");

    let done = tar::rebase::run(&new.state, OLD, false).await.unwrap();
    assert_eq!(done.statements, dry.statements);
    assert_eq!(done.rows, dry.rows);
    assert!(done.rows.iter().any(|(c, _)| c == "advertise_idem.idem_key"), "{done:?}");

    let g = new.local_graph();
    let left: Vec<&str> = g.lines().filter(|l| l.contains(OLD)).collect();
    assert!(left.is_empty(), "every IRI under the old base is renamed: {left:#?}");
    assert_eq!(tar::rebase::stray_base(&new.state).unwrap(), None);

    // A second run finds nothing to do.
    assert!(tar::rebase::run(&new.state, OLD, false).await.unwrap().is_empty());

    // Records answer under the new base, by the same ids.
    let (s, sw, _) = new.send("GET", &format!("/api/v1/software/{software_id}"), None, None, None).await;
    assert_eq!(s, StatusCode::OK, "{sw}");
    assert_eq!(sw["iri"], format!("{NEW}/software/{software_id}"), "{sw}");
    let artifact_id = old_artifact.rsplit('/').next().unwrap();
    let (s, a, _) = new.send("GET", &format!("/api/v1/artifacts/{artifact_id}"), None, None, None).await;
    assert_eq!(s, StatusCode::OK, "{a}");

    // The deployment's token, minted before the move, still maps to its Instance.
    let (s, me, _) = new.send("GET", "/api/v1/whoami", Some(&token), None, None).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    assert_eq!(me["instance"], format!("{NEW}/instance/{instance_id}"), "{me}");

    // A retry of the pre-move advertisement is still recognised as one.
    let (s, retried, _) =
        new.send("POST", "/api/v1/advertise/produced", Some(&token), Some(produced_body.clone()), None).await;
    assert_eq!(s, StatusCode::OK, "{retried}");
    assert_eq!(retried["created"], false, "{retried}");

    // A pipeline still citing the old IRI is translated, not left dangling.
    let consumed = new
        .post(
            "/api/v1/advertise/consumed",
            &token,
            json!({"run": {"external_key": "ci/2"}, "artifacts": [{"iri": old_artifact}]}),
        )
        .await;
    assert!(consumed["queued_for_resolution"].as_array().is_none_or(Vec::is_empty), "{consumed}");
    assert!(!new.local_graph().contains(OLD), "nothing new is stored under the old base: {consumed}");

    // A request that reaches the old host is sent to the new one.
    let (s, _, headers) =
        new.send("GET", &format!("/software/{software_id}"), None, None, Some("old.example.org")).await;
    assert_eq!(s, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(headers["location"], format!("{NEW}/software/{software_id}"));

    // And the move is discoverable.
    let (_, wk, _) = new.send("GET", "/.well-known/tar-registry", None, None, None).await;
    assert_eq!(wk["previous_base_iris"], json!([OLD]), "{wk}");
}
