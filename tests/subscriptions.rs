//! Artifact subscriptions, end to end against the real router.
//!
//! The harness is the same as `tests/api.rs`: the real `axum::Router`, an in-memory graph and
//! an in-memory ops database, so nothing here is testing a mock of the thing it means to test.
//!
//! What these cover, in the order the design note argues them:
//!
//! * a filter matches, and the match is queued without the advertisement touching a socket;
//! * an artifact that does not match produces nothing;
//! * the pull path — the channel for a subscriber that cannot receive an inbound connection —
//!   returns the right artifacts from a cursor, and the cursor advances only on acknowledgement;
//! * a webhook to a host that does not resolve backs off instead of being retried in a loop;
//! * a deployment cannot see or touch another deployment's subscriptions.

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
const REPORT: &str = "http://edamontology.org/data_2048";
const GRAPH: &str = "http://edamontology.org/data_2600";

struct Harness {
    app: axum::Router,
    state: Arc<AppState>,
}

async fn harness() -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    let store = Arc::new(OxigraphStore::memory().unwrap());
    let ops = Ops::open(":memory:").await.unwrap();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state.clone()), state }
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
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).to_string()));
        (status, value)
    }

    async fn get(&self, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
        self.req("GET", uri, token, None).await
    }
    async fn post(&self, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.req("POST", uri, Some(token), Some(body)).await
    }

    async fn software(&self) -> String {
        let (status, sw) = self
            .post(
                "/api/v1/software",
                ROOT,
                json!({
                    "name": "shacl-manager",
                    "tagline": "SHACL shape management and validation",
                    "license": "https://spdx.org/licenses/Apache-2.0",
                    "kind": "service",
                    "capability": {"consumes": [GRAPH], "produces": [REPORT]}
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{sw}");
        sw["id"].as_str().unwrap().to_string()
    }

    /// A deployment plus a token that acts as it — the credential a subscription is managed
    /// with, exactly as for API tokens (spec §8.3).
    async fn deployment(&self, software_id: &str, label: &str) -> Deployment {
        let (status, inst) = self
            .post("/api/v1/instances", ROOT, json!({"label": label, "software": software_id}))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{inst}");
        let id = inst["id"].as_str().unwrap().to_string();
        let (status, tok) = self
            .post(
                &format!("/api/v1/instances/{id}/tokens"),
                ROOT,
                json!({"scopes": ["advertise:produce", "advertise:consume"], "label": label}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{tok}");
        Deployment { id, iri: inst["iri"].as_str().unwrap().to_string(), token: tok["token"].as_str().unwrap().to_string() }
    }

    async fn subscribe(&self, d: &Deployment, body: Value) -> Value {
        let (status, res) = self.post(&format!("/api/v1/instances/{}/subscriptions", d.id), &d.token, body).await;
        assert_eq!(status, StatusCode::CREATED, "{res}");
        res
    }

    /// Advertise one produced artifact of the given type.
    async fn advertise(&self, d: &Deployment, key: &str, title: &str, conforms_to: &str, availability: &str) -> String {
        let (status, res) = self
            .post(
                "/api/v1/advertise/produced",
                &d.token,
                json!({
                    "run": {"external_key": key, "status": "success",
                            "started_at": "2026-08-30T14:02:11Z", "ended_at": "2026-08-30T14:02:49Z"},
                    "artifacts": [{
                        "title": title,
                        "conforms_to": conforms_to,
                        "license": "https://spdx.org/licenses/CC-BY-4.0",
                        "keywords": ["shacl", "fhir"],
                        "distributions": [{
                            "download_url": "https://shacl.example/r.ttl",
                            "media_type": "text/turtle",
                            "access_protocol": "https",
                            "availability": availability
                        }]
                    }]
                }),
            )
            .await;
        // 201 the first time, 200 when the same CI step is retried — both are successes.
        assert!(status.is_success(), "{status} {res}");
        res["artifacts"][0].as_str().unwrap().to_string()
    }
}

struct Deployment {
    id: String,
    iri: String,
    token: String,
}

/// Everything queued for a subscription, from the beginning, without acknowledging it.
async fn all_deliveries(h: &Harness, d: &Deployment, sid: &str) -> Vec<Value> {
    let (status, page) = h.get(&format!("/api/v1/subscriptions/{sid}/deliveries?cursor=0&limit=100"), Some(&d.token)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["items"].as_array().cloned().unwrap_or_default()
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_matching_artifact_is_queued_for_the_subscriber() {
    let h = harness().await;
    let sw = h.software().await;
    let producer = h.deployment(&sw, "shacl.ids.unimaas.nl").await;
    let subscriber = h.deployment(&sw, "downstream.mumc.nl").await;

    let sub = h
        .subscribe(
            &subscriber,
            json!({"label": "SHACL reports I can fetch",
                   "filter": {"conforms_to": [REPORT], "availability": ["public"]}}),
        )
        .await;
    let sid = sub["subscription"]["id"].as_str().unwrap().to_string();
    // A pull subscription is the default, and it gets no secret because nothing is signed.
    assert_eq!(sub["subscription"]["delivery_mode"], "pull");
    assert!(sub["secret"].is_null());
    assert!(sub["subscription"]["pull_url"].as_str().unwrap().ends_with(&format!("/subscriptions/{sid}/deliveries")));

    let artifact = h.advertise(&producer, "ci/1", "Validation report — patients.ttl", REPORT, "public").await;

    let items = all_deliveries(&h, &subscriber, &sid).await;
    assert_eq!(items.len(), 1, "the match must be queued: {items:?}");
    assert_eq!(items[0]["artifact_iri"], artifact);
    assert_eq!(items[0]["role"], "produced");
    assert_eq!(items[0]["status"], "pending");
    // The notification carries the whole artifact record, so a subscriber does not have to
    // come back and fetch it.
    assert_eq!(items[0]["notification"]["artifact"]["title"], "Validation report — patients.ttl");
    assert_eq!(items[0]["notification"]["instance"], producer.iri);
    assert_eq!(items[0]["notification"]["type"], "artifact.advertised");

    // Re-advertising the same CI step must not notify twice, the same promise §7.5 makes about
    // lineage itself.
    h.advertise(&producer, "ci/1", "Validation report — patients.ttl", REPORT, "public").await;
    assert_eq!(all_deliveries(&h, &subscriber, &sid).await.len(), 1);
}

#[tokio::test]
async fn an_artifact_outside_the_filter_notifies_nobody() {
    let h = harness().await;
    let sw = h.software().await;
    let producer = h.deployment(&sw, "shacl.ids.unimaas.nl").await;
    let subscriber = h.deployment(&sw, "downstream.mumc.nl").await;

    // Wants reports; a graph is advertised.
    let by_type = h.subscribe(&subscriber, json!({"filter": {"conforms_to": [REPORT]}})).await;
    // Wants anything retrievable; a metadata-only artifact is advertised.
    let by_availability = h.subscribe(&subscriber, json!({"filter": {"availability": ["public"]}})).await;
    // Wants a keyword nothing carries.
    let by_keyword = h.subscribe(&subscriber, json!({"filter": {"keywords": ["omop"]}})).await;

    h.advertise(&producer, "ci/2", "An input graph", GRAPH, "metadata-only").await;

    for s in [&by_type, &by_availability, &by_keyword] {
        let sid = s["subscription"]["id"].as_str().unwrap();
        assert!(
            all_deliveries(&h, &subscriber, sid).await.is_empty(),
            "a non-matching artifact must not notify subscription {sid}"
        );
    }

    // And the producer is not told about its own output, which it already knows about.
    let own = h.subscribe(&producer, json!({"filter": {}})).await;
    let own_sid = own["subscription"]["id"].as_str().unwrap();
    h.advertise(&producer, "ci/3", "Another report", REPORT, "public").await;
    assert!(all_deliveries(&h, &producer, own_sid).await.is_empty());
}

#[tokio::test]
async fn the_pull_path_returns_the_right_artifacts_from_a_cursor() {
    let h = harness().await;
    let sw = h.software().await;
    let producer = h.deployment(&sw, "shacl.ids.unimaas.nl").await;
    // A deployment with no endpoint at all — a CLI or a laptop. It cannot receive an inbound
    // connection, so the pull path is the only channel it has.
    let subscriber = h.deployment(&sw, "laptop-eerol").await;

    let sub = h.subscribe(&subscriber, json!({"filter": {"conforms_to": [REPORT]}})).await;
    let sid = sub["subscription"]["id"].as_str().unwrap().to_string();

    let a1 = h.advertise(&producer, "ci/1", "report one", REPORT, "public").await;
    h.advertise(&producer, "ci/2", "not a report", GRAPH, "public").await;
    let a2 = h.advertise(&producer, "ci/3", "report two", REPORT, "restricted").await;
    let a3 = h.advertise(&producer, "ci/4", "report three", REPORT, "public").await;

    // First page: oldest first, so a subscriber processes in the order things happened.
    let (status, page) = h
        .get(&format!("/api/v1/subscriptions/{sid}/deliveries?limit=2"), Some(&subscriber.token))
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["artifact_iri"], a1);
    assert_eq!(items[1]["artifact_iri"], a2);
    assert_eq!(page["remaining"], 1);
    let cursor = page["next_cursor"].as_i64().unwrap();

    // Second page, from the cursor.
    let (_, page2) = h
        .get(&format!("/api/v1/subscriptions/{sid}/deliveries?cursor={cursor}"), Some(&subscriber.token))
        .await;
    let items2 = page2["items"].as_array().unwrap();
    assert_eq!(items2.len(), 1);
    assert_eq!(items2[0]["artifact_iri"], a3);
    assert_eq!(page2["remaining"], 0);

    // Nothing was acknowledged, so an omitted cursor still starts from the beginning: the
    // guarantee is at-least-once, and a crashed subscriber must not lose what it never handled.
    let (_, replay) = h.get(&format!("/api/v1/subscriptions/{sid}/deliveries"), Some(&subscriber.token)).await;
    assert_eq!(replay["items"].as_array().unwrap().len(), 3);

    // Acknowledging moves the resumption point, and only forwards.
    let (status, acked) = h
        .post(&format!("/api/v1/subscriptions/{sid}/deliveries/ack"), &subscriber.token, json!({"cursor": cursor}))
        .await;
    assert_eq!(status, StatusCode::OK, "{acked}");
    assert_eq!(acked["remaining"], 1);
    let (_, after) = h.get(&format!("/api/v1/subscriptions/{sid}/deliveries"), Some(&subscriber.token)).await;
    assert_eq!(after["items"].as_array().unwrap().len(), 1);
    assert_eq!(after["items"][0]["artifact_iri"], a3);

    let (_, rewound) = h
        .post(&format!("/api/v1/subscriptions/{sid}/deliveries/ack"), &subscriber.token, json!({"cursor": 0}))
        .await;
    assert_eq!(rewound["cursor"], cursor, "a stale acknowledgement must not replay what was handled");
}

#[tokio::test]
async fn a_failing_webhook_backs_off_rather_than_retrying_forever() {
    let h = harness().await;
    let sw = h.software().await;
    let producer = h.deployment(&sw, "shacl.ids.unimaas.nl").await;
    let subscriber = h.deployment(&sw, "downstream.mumc.nl").await;

    // `.invalid` is reserved by RFC 2606 and never resolves: a receiver whose host does not
    // exist, which is what a decommissioned deployment looks like from here.
    let sub = h
        .subscribe(
            &subscriber,
            json!({"label": "dead receiver",
                   "webhook_url": "https://receiver.tar-test.invalid/hook",
                   "filter": {"conforms_to": [REPORT]}}),
        )
        .await;
    let sid = sub["subscription"]["id"].as_str().unwrap().to_string();
    // A webhook is signed by default: the receiver has to be able to tell our POST from
    // anybody else's.
    assert_eq!(sub["subscription"]["delivery_mode"], "webhook");
    assert_eq!(sub["subscription"]["webhook_signed"], true);
    assert!(sub["secret"].as_str().unwrap().starts_with("whsec_"), "the signing secret is shown once");

    h.advertise(&producer, "ci/1", "report one", REPORT, "public").await;

    // The advertisement itself never waited on that host — the delivery is still queued.
    let items = all_deliveries(&h, &subscriber, &sid).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "pending");

    // Now run the worker by hand, as the background loop would.
    assert_eq!(tar::api::subscriptions::deliver_due(&h.state).await, 1);

    let items = all_deliveries(&h, &subscriber, &sid).await;
    assert_eq!(items[0]["status"], "failed");
    assert_eq!(items[0]["attempts"], 1);
    assert!(items[0]["last_error"].is_string(), "the reason must be visible to the owner, not only in a log");
    assert!(items[0]["next_attempt_at"].is_string(), "a retry must be scheduled, not immediate");

    // The point of the backoff: a second pass right away does nothing at all.
    assert_eq!(tar::api::subscriptions::deliver_due(&h.state).await, 0, "a failed delivery must not be retried at once");
    let (_, detail) = h.get(&format!("/api/v1/subscriptions/{sid}"), Some(&subscriber.token)).await;
    assert_eq!(detail["subscription"]["consecutive_failures"], 1);
    assert!(detail["subscription"]["last_error"].is_string());

    // Attempts are finite. Driving the ops layer directly is how we reach the end of the
    // schedule without waiting hours for the backoff.
    let policy = tar::ops::subscriptions::RetryPolicy::from_env();
    let did = items[0]["id"].as_str().unwrap();
    let mut died = None;
    let mut suspended = false;
    for _ in 0..policy.suspend_after {
        let o = tar::ops::subscriptions::mark_failed(&h.state.ops, did, &sid, "host does not resolve", None, &policy)
            .await
            .unwrap();
        if o.status == "dead" && died.is_none() {
            died = Some(o.clone());
        }
        suspended |= o.suspended;
    }
    let died = died.expect("a delivery must stop being retried eventually");
    assert_eq!(died.attempts, policy.max_attempts, "it dies exactly at the attempt limit");
    assert_eq!(died.retry_in_secs, None, "nothing is scheduled after it dies");
    assert!(suspended, "a consistently failing endpoint must be suspended, not retried forever");

    // And a consistently dead endpoint stops being contacted at all, rather than being
    // hammered forever — while the pull path keeps working, which is the right degradation.
    let (_, detail) = h.get(&format!("/api/v1/subscriptions/{sid}"), Some(&subscriber.token)).await;
    assert_eq!(detail["subscription"]["delivery_state"], "suspended");
    assert_eq!(tar::api::subscriptions::deliver_due(&h.state).await, 0);
    assert_eq!(all_deliveries(&h, &subscriber, &sid).await.len(), 1);

    // Fixing the receiver re-arms what died while it was down.
    let (status, resumed) = h
        .req("PATCH", &format!("/api/v1/subscriptions/{sid}"), Some(&subscriber.token), Some(json!({"resume": true})))
        .await;
    assert_eq!(status, StatusCode::OK, "{resumed}");
    assert_eq!(resumed["subscription"]["delivery_state"], "active");
    assert_eq!(resumed["subscription"]["consecutive_failures"], 0);
}

#[tokio::test]
async fn a_deployment_cannot_manage_another_deployments_subscriptions() {
    let h = harness().await;
    let sw = h.software().await;
    let mine = h.deployment(&sw, "mine.example.org").await;
    let theirs = h.deployment(&sw, "theirs.example.org").await;

    let sub = h.subscribe(&theirs, json!({"label": "theirs", "filter": {"conforms_to": [REPORT]}})).await;
    let sid = sub["subscription"]["id"].as_str().unwrap().to_string();

    // Listing another deployment's subscriptions.
    let (status, body) = h.get(&format!("/api/v1/instances/{}/subscriptions", theirs.id), Some(&mine.token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["type"], "https://w3id.org/tar/problem/forbidden");

    // Creating one on their behalf — the Instance comes from the credential, never the path.
    let (status, _) = h
        .post(&format!("/api/v1/instances/{}/subscriptions", theirs.id), &mine.token, json!({"filter": {}}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Reading, editing, acknowledging or deleting one by id.
    for (method, uri, body) in [
        ("GET", format!("/api/v1/subscriptions/{sid}"), None),
        ("PATCH", format!("/api/v1/subscriptions/{sid}"), Some(json!({"enabled": false}))),
        ("DELETE", format!("/api/v1/subscriptions/{sid}"), None),
        ("GET", format!("/api/v1/subscriptions/{sid}/deliveries"), None),
        ("POST", format!("/api/v1/subscriptions/{sid}/deliveries/ack"), Some(json!({"cursor": 1}))),
    ] {
        let (status, body) = h.req(method, &uri, Some(&mine.token), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri} must be refused: {body}");
    }

    // Anonymous is not a way around it either.
    let (status, _) = h.get(&format!("/api/v1/subscriptions/{sid}"), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The owner still can, and a curator or admin may act on anyone's behalf — the same rule
    // the token endpoints use.
    let (status, ok) = h.get(&format!("/api/v1/subscriptions/{sid}"), Some(&theirs.token)).await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    let (status, ok) = h.get(&format!("/api/v1/instances/{}/subscriptions", theirs.id), Some(ROOT)).await;
    assert_eq!(status, StatusCode::OK, "{ok}");
}

#[tokio::test]
async fn a_webhook_cannot_be_pointed_at_the_registrys_own_network() {
    let h = harness().await;
    let sw = h.software().await;
    let d = h.deployment(&sw, "mine.example.org").await;

    for bad in [
        "http://hooks.example.org/plain",              // unencrypted
        "https://user:pw@hooks.example.org/hook",      // credentials in the URL
        "https://127.0.0.1/hook",                      // loopback
        "https://169.254.169.254/latest/meta-data/",   // cloud metadata, the classic SSRF target
        "https://10.1.2.3/hook",                       // RFC1918
        "https://[::ffff:127.0.0.1]/hook",             // IPv4-mapped loopback
        "https://localhost/hook",
        "file:///etc/passwd",
    ] {
        let (status, body) = h
            .post(&format!("/api/v1/instances/{}/subscriptions", d.id), &d.token, json!({"webhook_url": bad}))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} must be refused, got {body}");
        assert_eq!(body["type"], "https://w3id.org/tar/problem/bad-request");
    }

    // A filter value that could never match is refused too, rather than silently never firing.
    let (status, _) = h
        .post(
            &format!("/api/v1/instances/{}/subscriptions", d.id),
            &d.token,
            json!({"filter": {"availability": ["publicc"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_curators_direct_registration_also_reaches_subscribers() {
    // The other door an artifact comes through (spec §7.5): a curator recording something that
    // predates the registry. A subscription for "any report" must not have a hole there.
    let h = harness().await;
    let sw = h.software().await;
    let subscriber = h.deployment(&sw, "downstream.mumc.nl").await;
    let sub = h.subscribe(&subscriber, json!({"filter": {"conforms_to": [REPORT]}})).await;
    let sid = sub["subscription"]["id"].as_str().unwrap().to_string();

    let (status, art) = h
        .post(
            "/api/v1/artifacts",
            ROOT,
            json!({"title": "A report from before the registry existed", "conforms_to": REPORT,
                   "distributions": [{"download_url": "https://old.example/r.ttl", "availability": "public"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{art}");

    let items = all_deliveries(&h, &subscriber, &sid).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["artifact_iri"], art["iri"]);
}
