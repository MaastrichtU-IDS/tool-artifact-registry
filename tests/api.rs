//! End-to-end tests against the real router, an in-memory graph and an in-memory ops db.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tar::auth::jwt::jsonwebtoken::{encode, Algorithm, DecodingKey, EncodingKey, Header};
use tar::config::Config;
use tar::ops::Ops;
use tar::state::AppState;
use tar::store::OxigraphStore;
use tower::ServiceExt;

const BASE: &str = "https://reg.test.example";
const ROOT: &str = "test-root-token-0123456789";
const HS_SECRET: &[u8] = b"a-test-signing-secret-not-used-in-production";
const ISSUER: &str = "https://keycloak.test.example/realms/ids";

struct Harness {
    app: axum::Router,
    state: Arc<AppState>,
}

async fn harness() -> Harness {
    harness_with_oidc(false).await
}

async fn harness_with_oidc(oidc: bool) -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    if oidc {
        config.oidc.issuer = Some(ISSUER.into());
        config.oidc.client_id = Some("tar-ui".into());
        config.oidc.audience = Some(BASE.into());
    }
    let store = Arc::new(OxigraphStore::memory().unwrap());
    let ops = Ops::open(":memory:").await.unwrap();
    let mut state = AppState::from_parts(config, store, ops);
    // Pin a signing key instead of fetching JWKS over the network.
    state.jwt = state.jwt.with_static_key("test-key", Algorithm::HS256, DecodingKey::from_secret(HS_SECRET));
    let state = Arc::new(state);
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state.clone()), state }
}

impl Harness {
    async fn req(&self, method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value, axum::http::HeaderMap) {
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
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).to_string()));
        (status, value, headers)
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let (s, v, _) = self.req("GET", uri, None, None).await;
        (s, v)
    }

    async fn post(&self, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let (s, v, _) = self.req("POST", uri, Some(token), Some(body)).await;
        (s, v)
    }

    /// Register software + instance and mint a scoped token for the instance.
    async fn fixture(&self) -> Fixture {
        let (status, sw) = self
            .post(
                "/api/v1/software",
                ROOT,
                json!({
                    "name": "shacl-manager",
                    "tagline": "SHACL shape management and validation",
                    "code_repository": "https://github.com/MaastrichtU-IDS/shacl-manager",
                    "license": "https://spdx.org/licenses/Apache-2.0",
                    "kind": "service",
                    "capability": {
                        "consumes": ["http://edamontology.org/data_2600"],
                        "produces": ["http://edamontology.org/data_2048"]
                    }
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{sw}");
        let software_id = sw["id"].as_str().unwrap().to_string();

        let (status, inst) = self
            .post(
                "/api/v1/instances",
                ROOT,
                json!({
                    "label": "shacl.ids.unimaas.nl",
                    "software": software_id,
                    "endpoint_url": "https://shacl.ids.unimaas.nl",
                    "oidc_client_id": "shacl-manager-ids3",
                    "allowed_scopes": ["advertise:produce", "advertise:consume"]
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{inst}");
        let instance_id = inst["id"].as_str().unwrap().to_string();

        let (status, tok) = self
            .post(
                &format!("/api/v1/instances/{instance_id}/tokens"),
                ROOT,
                json!({"scopes": ["advertise:produce", "advertise:consume"], "label": "ci"}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{tok}");
        Fixture {
            software_id,
            instance_iri: inst["iri"].as_str().unwrap().to_string(),
            instance_id,
            token: tok["token"].as_str().unwrap().to_string(),
        }
    }
}

struct Fixture {
    software_id: String,
    instance_id: String,
    instance_iri: String,
    token: String,
}

fn jwt(claims: Value) -> String {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("test-key".into());
    encode(&header, &claims, &EncodingKey::from_secret(HS_SECRET)).unwrap()
}

fn exp() -> i64 {
    (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp()
}

// ------------------------------------------------------------------- reads

#[tokio::test]
async fn anonymous_read_is_the_default() {
    let h = harness().await;
    let f = h.fixture().await;

    let (status, list) = h.get("/api/v1/software").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["total"], 1);

    let (status, sw) = h.get(&format!("/api/v1/software/{}", f.software_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sw["name"], "shacl-manager");
    assert_eq!(sw["origin"]["kind"], "local");
    assert_eq!(sw["instance_count"], 1);
}

#[tokio::test]
async fn writes_require_a_credential_and_say_so_in_problem_json() {
    let h = harness().await;
    let (status, body, headers) = h
        .req("POST", "/api/v1/software", None, Some(json!({"name": "x"})))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers.get("content-type").unwrap(), "application/problem+json");
    assert_eq!(body["status"], 401);
    assert!(body["type"].as_str().unwrap().contains("unauthorized"));
    assert!(body["detail"].as_str().unwrap().contains("credential"));
}

#[tokio::test]
async fn iris_dereference_as_turtle_json_ld_and_json_with_signposting() {
    let h = harness().await;
    let f = h.fixture().await;
    let path = format!("/software/{}", f.software_id);

    for (accept, expected_ct) in [
        ("text/turtle", "text/turtle; charset=utf-8"),
        ("application/ld+json", "application/ld+json"),
        ("application/json", "application/json"),
    ] {
        let req = Request::builder().uri(&path).header("accept", accept).body(Body::empty()).unwrap();
        let resp = h.app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{accept}");
        assert_eq!(resp.headers().get("content-type").unwrap(), expected_ct);
        let link = resp.headers().get("link").expect("signposting").to_str().unwrap().to_string();
        assert!(link.contains("rel=\"cite-as\""), "{link}");
        assert!(link.contains("rel=\"describedby\""), "{link}");
        assert!(link.contains("https://spdx.org/licenses/Apache-2.0>; rel=\"license\""), "{link}");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);
        if accept == "text/turtle" {
            assert!(text.contains("schema:name \"shacl-manager\""), "{text}");
            // Prefix declarations must stay absolute or no other parser reads them the same.
            assert!(text.contains("@prefix schema: <https://schema.org/>"), "{text}");
        }
    }

    // A browser gets the SPA at the same URL.
    let req = Request::builder().uri(&path).header("accept", "text/html,*/*;q=0.8").body(Body::empty()).unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.headers().get("content-type").unwrap(), "text/html; charset=utf-8");
}

// ------------------------------------------------------------ advertisement

#[tokio::test]
async fn advertising_produced_records_lineage_and_is_idempotent() {
    let h = harness().await;
    let f = h.fixture().await;
    let payload = json!({
        "run": {
            "external_key": "gh-actions/12345/attempt-1",
            "started_at": "2026-08-30T14:02:11Z",
            "ended_at": "2026-08-30T14:02:49Z",
            "status": "success"
        },
        "artifacts": [{
            "title": "Validation report — patients.ttl vs fhir-shapes v3",
            "conforms_to": "http://edamontology.org/data_2048",
            "license": "https://spdx.org/licenses/CC-BY-4.0",
            "keywords": ["shacl", "validation"],
            "was_derived_from": ["https://reg.mumc.nl/artifact/01J7Z"],
            "distributions": [{
                "access_url": "https://shacl.ids.unimaas.nl/reports/9f2a",
                "download_url": "https://shacl.ids.unimaas.nl/reports/9f2a.ttl",
                "media_type": "text/turtle",
                "byte_size": 2118342,
                "checksum": {"algorithm": "sha256", "value": "9f2acafe"},
                "access_protocol": "https",
                "auth_method": "apikey",
                "availability": "restricted",
                "access_request_url": "https://ids.unimaas.nl/data-access"
            }]
        }]
    });

    let (status, first) = h.post("/api/v1/advertise/produced", &f.token, payload.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["created"], true);
    let run = first["run"].as_str().unwrap().to_string();
    let artifact = first["artifacts"][0].as_str().unwrap().to_string();
    // The unresolved foreign parent is queued, never fetched inline.
    assert_eq!(first["queued_for_resolution"][0], "https://reg.mumc.nl/artifact/01J7Z");

    // A retried CI step must not duplicate lineage (spec §7.5).
    let (status, second) = h.post("/api/v1/advertise/produced", &f.token, payload).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["created"], false);
    assert_eq!(second["run"], run, "same external key must attach to the same run");

    let artifact_id = artifact.rsplit('/').next().unwrap();
    let (status, a) = h.get(&format!("/api/v1/artifacts/{artifact_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(a["was_generated_by"], run);
    assert_eq!(a["availability"], "restricted");
    assert_eq!(a["distributions"][0]["checksum"]["algorithm"], "sha256");
    assert_eq!(a["generated_by_run"]["instance"], f.instance_iri);
    assert_eq!(a["generated_by_run"]["duration_seconds"], 38);

    // One run, one artifact each way round.
    let run_id = run.rsplit('/').next().unwrap();
    let (_, r) = h.get(&format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(r["generated"].as_array().unwrap().len(), 1);
    assert_eq!(r["status"], "success");
    assert_eq!(r["software_name"], "shacl-manager");
}

#[tokio::test]
async fn a_later_advertisement_replaces_a_runs_outcome_rather_than_adding_to_it() {
    let h = harness().await;
    let f = h.fixture().await;

    // A CI job advertises its inputs while still running…
    let (status, started) = h
        .post(
            "/api/v1/advertise/consumed",
            &f.token,
            json!({"run": {"external_key": "ci/42", "status": "running",
                           "started_at": "2026-08-30T10:31:00Z"},
                   "artifacts": [{"iri": "https://reg.mumc.nl/artifact/01J7Z"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{started}");
    let run_id = started["run"].as_str().unwrap().rsplit('/').next().unwrap().to_string();

    let (_, run) = h.get(&format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(run["status"], "running");

    // …and reports the outcome on the same run key when it finishes.
    h.post(
        "/api/v1/advertise/produced",
        &f.token,
        json!({"run": {"external_key": "ci/42", "status": "success",
                       "ended_at": "2026-08-30T10:31:04Z"},
               "artifacts": []}),
    )
    .await;

    let (_, run) = h.get(&format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(run["status"], "success", "the outcome must replace the earlier state");
    assert_eq!(run["ended_at"], "2026-08-30T10:31:04Z");
    assert_eq!(run["duration_seconds"], 4);

    // The graph must hold exactly one status, or every reader has to guess which is current.
    let quads = h
        .state
        .store
        .describe(started["run"].as_str().unwrap())
        .expect("run quads");
    let n = quads
        .iter()
        .filter(|q| q.predicate.as_str() == "https://w3id.org/tar/ns#status")
        .count();
    assert_eq!(n, 1, "expected one tar:status triple, found {n}");
}

#[tokio::test]
async fn consuming_accepts_a_bare_foreign_iri_and_never_blocks_on_the_network() {
    let h = harness().await;
    let f = h.fixture().await;
    let (status, out) = h
        .post(
            "/api/v1/advertise/consumed",
            &f.token,
            json!({
                "run": {"external_key": "gh-actions/999/attempt-1"},
                "artifacts": [
                    {"iri": "https://reg.mumc.nl/artifact/01J7Z"},
                    {"title": "local input graph", "conforms_to": "http://edamontology.org/data_2600",
                     "distributions": [{"download_url": "s3://ids-bucket/in.ttl", "access_protocol": "s3"}]}
                ]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    assert_eq!(out["artifacts"].as_array().unwrap().len(), 2);
    assert_eq!(out["queued_for_resolution"][0], "https://reg.mumc.nl/artifact/01J7Z");

    let run_id = out["run"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let (_, r) = h.get(&format!("/api/v1/runs/{run_id}")).await;
    let used = r["used"].as_array().unwrap();
    assert_eq!(used.len(), 2);
    let foreign = used.iter().find(|u| u["iri"] == "https://reg.mumc.nl/artifact/01J7Z").unwrap();
    assert_eq!(foreign["unresolved"], true, "an unresolved cross-link must be visibly unresolved");
    assert_eq!(foreign["origin"]["kind"], "peer");
}

#[tokio::test]
async fn an_instance_cannot_advertise_as_another_deployment() {
    let h = harness().await;
    let f = h.fixture().await;

    // A token with no instance behind it (root) is not a deployment.
    let (status, body) = h
        .post("/api/v1/advertise/produced", ROOT, json!({"run": {}, "artifacts": []}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["detail"].as_str().unwrap().contains("Instance"));

    // The payload cannot name a different instance: there is nowhere to put one.
    let (status, _) = h
        .post(
            "/api/v1/advertise/produced",
            &f.token,
            json!({"run": {"external_key": "k"}, "instance": "https://reg.test.example/instance/someone-else", "artifacts": []}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, runs) = h.get(&format!("/api/v1/instances/{}/runs", f.instance_id)).await;
    assert_eq!(runs["total"], 1, "the run belongs to the credential's instance");
}

#[tokio::test]
async fn a_token_without_the_scope_is_refused() {
    let h = harness().await;
    let f = h.fixture().await;
    let (status, tok) = h
        .post(
            &format!("/api/v1/instances/{}/tokens", f.instance_id),
            ROOT,
            json!({"scopes": ["advertise:consume"], "label": "consume-only"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let consume_only = tok["token"].as_str().unwrap();

    let (status, body) = h.post("/api/v1/advertise/produced", consume_only, json!({"run": {}, "artifacts": []})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["detail"].as_str().unwrap().contains("advertise:produce"));
}

#[tokio::test]
async fn a_revoked_token_stops_working() {
    let h = harness().await;
    let f = h.fixture().await;
    let (_, list) = {
        let (s, v, _) = h.req("GET", &format!("/api/v1/instances/{}/tokens", f.instance_id), Some(ROOT), None).await;
        (s, v)
    };
    let token_id = list["items"][0]["id"].as_str().unwrap().to_string();
    let (status, _, _) = h
        .req("DELETE", &format!("/api/v1/instances/{}/tokens/{token_id}", f.instance_id), Some(ROOT), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = h.post("/api/v1/advertise/produced", &f.token, json!({"run": {}, "artifacts": []})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// --------------------------------------------------- OIDC workload identity

#[tokio::test]
async fn a_keycloak_service_account_token_authenticates_as_its_instance() {
    let h = harness_with_oidc(true).await;
    let f = h.fixture().await;

    // What a Keycloak client_credentials token looks like.
    let token = jwt(json!({
        "iss": ISSUER,
        "aud": BASE,
        "exp": exp(),
        "sub": "service-account-shacl-manager-ids3",
        "azp": "shacl-manager-ids3",
        "scope": "advertise:produce openid"
    }));

    let (status, who, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{who}");
    assert_eq!(who["credential"], "oidc-workload");
    assert_eq!(who["instance"], f.instance_iri, "azp must map to the Instance that declared it");
    assert_eq!(who["issuer"], ISSUER);

    let (status, out) = h
        .post(
            "/api/v1/advertise/produced",
            &token,
            json!({"run": {"external_key": "kc-run-1"}, "artifacts": [{
                "title": "report from a Keycloak-authenticated deployment",
                "conforms_to": "http://edamontology.org/data_2048",
                "distributions": [{"access_url": "https://shacl.ids.unimaas.nl/r/1", "availability": "public"}]
            }]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");

    // The run is attributed to the deployment, not to whoever held the token.
    let (_, runs) = h.get(&format!("/api/v1/instances/{}/runs", f.instance_id)).await;
    assert_eq!(runs["total"], 1);
}

#[tokio::test]
async fn oidc_scopes_are_honoured_over_the_instance_default() {
    let h = harness_with_oidc(true).await;
    let _f = h.fixture().await;
    let token = jwt(json!({
        "iss": ISSUER, "aud": BASE, "exp": exp(),
        "sub": "service-account-shacl-manager-ids3", "azp": "shacl-manager-ids3",
        "scope": "advertise:consume"
    }));
    let (status, body) = h.post("/api/v1/advertise/produced", &token, json!({"run": {}, "artifacts": []})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["detail"].as_str().unwrap().contains("advertise:produce"));
}

#[tokio::test]
async fn a_token_from_an_untrusted_issuer_is_rejected() {
    let h = harness_with_oidc(true).await;
    h.fixture().await;
    let token = jwt(json!({
        "iss": "https://evil.example/realms/ids", "aud": BASE, "exp": exp(), "azp": "shacl-manager-ids3"
    }));
    let (status, body, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body["detail"].as_str().unwrap().contains("not trusted"), "{body}");
}

#[tokio::test]
async fn a_token_for_the_wrong_audience_is_rejected() {
    let h = harness_with_oidc(true).await;
    h.fixture().await;
    let token = jwt(json!({
        "iss": ISSUER, "aud": "https://some-other-service", "exp": exp(), "azp": "shacl-manager-ids3"
    }));
    let (status, _, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_expired_token_is_rejected() {
    let h = harness_with_oidc(true).await;
    h.fixture().await;
    let token = jwt(json!({
        "iss": ISSUER, "aud": BASE, "azp": "shacl-manager-ids3",
        "exp": (chrono::Utc::now() - chrono::Duration::hours(2)).timestamp()
    }));
    let (status, _, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_verified_token_bound_to_no_instance_can_authenticate_but_not_advertise() {
    let h = harness_with_oidc(true).await;
    h.fixture().await;
    let token = jwt(json!({
        "iss": ISSUER, "aud": BASE, "exp": exp(), "azp": "some-unregistered-client",
        "scope": "advertise:produce"
    }));
    let (status, who, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(who["instance"].is_null());

    let (status, body) = h.post("/api/v1/advertise/produced", &token, json!({"run": {}, "artifacts": []})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["detail"].as_str().unwrap().contains("does not act as an Instance"), "{body}");
}

#[tokio::test]
async fn a_human_with_the_curator_role_may_register_software() {
    let h = harness_with_oidc(true).await;
    let token = jwt(json!({
        "iss": ISSUER, "aud": BASE, "exp": exp(), "sub": "u-123",
        "preferred_username": "eerol",
        "realm_access": {"roles": ["curator", "offline_access"]}
    }));
    let (status, who, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{who}");
    assert_eq!(who["credential"], "oidc-human");
    assert_eq!(who["is_curator"], true);
    assert_eq!(who["is_admin"], false);

    let (status, _) = h.post("/api/v1/software", &token, json!({"name": "rdf_tx", "kind": "library"})).await;
    assert_eq!(status, StatusCode::CREATED);

    // …but not administer peers.
    let (status, _) = h.post("/api/v1/peers", &token, json!({"base_url": "https://reg.mumc.nl"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn oidc_tokens_are_refused_when_no_issuer_is_configured() {
    let h = harness().await; // OIDC off
    let token = jwt(json!({"iss": ISSUER, "aud": BASE, "exp": exp(), "azp": "x"}));
    let (status, body, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body["detail"].as_str().unwrap().contains("no OIDC issuer is configured"), "{body}");
}

// ------------------------------------------------------------ validation

#[tokio::test]
async fn a_rejected_write_returns_422_with_a_shacl_report() {
    let h = harness().await;
    let (status, body) = h
        .post(
            "/api/v1/software",
            ROOT,
            json!({"name": "", "kind": "teapot", "code_repository": "not-an-iri"}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let report = body["report"].as_str().expect("turtle report");
    assert!(report.contains("a sh:ValidationReport"), "{report}");
    assert!(report.contains("sh:conforms false"), "{report}");
    assert!(report.contains("sh:resultPath <https://schema.org/name>"), "{report}");
    // The UI maps a report entry back to a form field (handoff §5.7).
    assert!(report.contains("tar:jsonField \"kind\""), "{report}");
    assert!(report.contains("tar:jsonField \"code_repository\""), "{report}");
}

#[tokio::test]
async fn metadata_only_artifacts_carry_no_download_affordance_and_no_item_link() {
    let h = harness().await;
    let f = h.fixture().await;
    let (status, out) = h
        .post(
            "/api/v1/advertise/produced",
            &f.token,
            json!({"run": {"external_key": "mo-1"}, "artifacts": [{
                "title": "masked replica of the MUMC cohort",
                "conforms_to": "http://edamontology.org/data_2600",
                "distributions": [{
                    "availability": "metadata-only",
                    "media_type": "text/turtle",
                    "access_request_url": "https://ids.unimaas.nl/data-access"
                }]
            }]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    let id = out["artifacts"][0].as_str().unwrap().rsplit('/').next().unwrap().to_string();

    let (status, a) = h.get(&format!("/api/v1/artifacts/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(a["availability"], "metadata-only");
    assert!(a["distributions"][0]["download_url"].is_null(), "no downloadURL at all (spec §6.2)");
    assert_eq!(a["distributions"][0]["access_request_url"], "https://ids.unimaas.nl/data-access");

    // Signposting omits rel="item" so a client can tell "no bytes" from "bytes behind auth".
    let req = Request::builder().uri(format!("/artifact/{id}")).header("accept", "text/turtle").body(Body::empty()).unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    let link = resp.headers().get("link").unwrap().to_str().unwrap().to_string();
    assert!(!link.contains("rel=\"item\""), "{link}");
    assert!(link.contains("rel=\"describedby\""), "{link}");
}

// ------------------------------------------------------ capability & search

#[tokio::test]
async fn capability_matchmaking_answers_before_anything_has_run() {
    let h = harness().await;
    h.fixture().await;
    let (status, out) = h.get("/api/v1/capabilities?produces=http://edamontology.org/data_2048").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["total"], 1);
    assert_eq!(out["items"][0]["name"], "shacl-manager");
    assert_eq!(out["items"][0]["entity_type"], "software");

    let (_, none) = h.get("/api/v1/capabilities?produces=http://edamontology.org/data_0006").await;
    assert_eq!(none["total"], 0);

    let (status, _) = h.get("/api/v1/capabilities").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "one of produces= or consumes= is required");
}

#[tokio::test]
async fn search_groups_by_entity_type_and_is_local_only_by_default() {
    let h = harness().await;
    let f = h.fixture().await;
    h.post(
        "/api/v1/advertise/produced",
        &f.token,
        json!({"run": {"external_key": "s-1"}, "artifacts": [{
            "title": "shacl validation report",
            "conforms_to": "http://edamontology.org/data_2048",
            "distributions": [{"access_url": "https://x.example/1", "availability": "public"}]
        }]}),
    )
    .await;

    let (status, r) = h.get("/api/v1/search?q=shacl").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(r["partial"], false);
    let types: Vec<&str> = r["hits"].as_array().unwrap().iter().map(|h| h["entity_type"].as_str().unwrap()).collect();
    assert!(types.contains(&"software"), "{types:?}");
    assert!(types.contains(&"artifact"), "{types:?}");
    assert!(types.contains(&"instance"), "{types:?}");
    for hit in r["hits"].as_array().unwrap() {
        assert_eq!(hit["origin"]["kind"], "local");
    }

    let (_, only) = h.get("/api/v1/search?q=shacl&type=software").await;
    assert_eq!(only["hits"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn lineage_walks_both_directions_and_marks_unresolved_cross_links() {
    let h = harness().await;
    let f = h.fixture().await;
    let (_, first) = h
        .post(
            "/api/v1/advertise/produced",
            &f.token,
            json!({"run": {"external_key": "l-1"}, "artifacts": [{
                "title": "intermediate graph",
                "conforms_to": "http://edamontology.org/data_2600",
                "was_derived_from": ["https://reg.mumc.nl/artifact/01J7Z"],
                "distributions": [{"access_url": "https://x.example/a", "availability": "public"}]
            }]}),
        )
        .await;
    let parent = first["artifacts"][0].as_str().unwrap().to_string();
    let (_, second) = h
        .post(
            "/api/v1/advertise/produced",
            &f.token,
            json!({"run": {"external_key": "l-2"}, "artifacts": [{
                "title": "final report",
                "conforms_to": "http://edamontology.org/data_2048",
                "was_derived_from": [parent],
                "distributions": [{"access_url": "https://x.example/b", "availability": "public"}]
            }]}),
        )
        .await;
    let child_id = second["artifacts"][0].as_str().unwrap().rsplit('/').next().unwrap().to_string();

    let (status, lin) = h.get(&format!("/api/v1/artifacts/{child_id}/lineage?depth=3&direction=up")).await;
    assert_eq!(status, StatusCode::OK, "{lin}");
    let nodes = lin["nodes"].as_array().unwrap();
    assert!(nodes.iter().any(|n| n["entity_type"] == "run"), "the generating run is part of lineage");
    let foreign = nodes.iter().find(|n| n["iri"] == "https://reg.mumc.nl/artifact/01J7Z");
    assert!(foreign.is_some(), "a cross-registry ancestor is an ordinary node");
    assert_eq!(foreign.unwrap()["unresolved"], true);
}

// -------------------------------------------------------------- lifecycle

#[tokio::test]
async fn a_withdrawn_release_leaves_the_list_but_keeps_resolving() {
    let h = harness().await;
    let f = h.fixture().await;
    let (_, keep) = h
        .post(&format!("/api/v1/software/{}/releases", f.software_id), ROOT, json!({"version": "2.1.0"}))
        .await;
    let (_, drop_me) = h
        .post(&format!("/api/v1/software/{}/releases", f.software_id), ROOT, json!({"version": "2.1.0"}))
        .await;
    let (_, list) = h.get(&format!("/api/v1/software/{}/releases", f.software_id)).await;
    assert_eq!(list["total"], 2, "a duplicate version is allowed in; withdrawing is how you fix it");

    let (status, _, _) = h
        .req(
            "DELETE",
            &format!("/api/v1/software/{}/releases/{}", f.software_id, drop_me["id"].as_str().unwrap()),
            Some(ROOT),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, list) = h.get(&format!("/api/v1/software/{}/releases", f.software_id)).await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], keep["id"]);

    // Withdrawn, not erased: an Instance may cite the IRI, so it still dereferences.
    let req = Request::builder()
        .uri(format!("/release/{}", drop_me["id"].as_str().unwrap()))
        .header("accept", "text/turtle")
        .body(Body::empty())
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // And a release belonging to another software cannot be withdrawn through this path.
    let (_, other) = h.post("/api/v1/software", ROOT, json!({"name": "other", "kind": "cli"})).await;
    let (status, _, _) = h
        .req(
            "DELETE",
            &format!("/api/v1/software/{}/releases/{}", other["id"].as_str().unwrap(), keep["id"].as_str().unwrap()),
            Some(ROOT),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_tombstoned_record_still_resolves() {
    let h = harness().await;
    let f = h.fixture().await;
    let (status, _, _) = h.req("DELETE", &format!("/api/v1/software/{}", f.software_id), Some(ROOT), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, sw) = h.get(&format!("/api/v1/software/{}", f.software_id)).await;
    assert_eq!(status, StatusCode::OK, "a soft-deleted IRI must keep resolving, not 404");
    assert_eq!(sw["tombstoned"], true);

    // …but it is withdrawn, so it must not still be offered in listings or counted in totals.
    // Resolving and being listed are different promises, and only the first survives a delete.
    let (_, list) = h.get("/api/v1/software").await;
    assert_eq!(list["total"], 0, "a withdrawn record must leave the list");
    let (_, reg) = h.get("/api/v1/registry").await;
    assert_eq!(reg["counts"]["software"], 0);
}

#[tokio::test]
async fn a_withdrawn_instance_stops_counting_against_its_software() {
    let h = harness().await;
    let f = h.fixture().await;
    let (_, before) = h.get(&format!("/api/v1/software/{}", f.software_id)).await;
    assert_eq!(before["instance_count"], 1);

    let (status, _, _) = h
        .req("DELETE", &format!("/api/v1/instances/{}", f.instance_id), Some(ROOT), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, after) = h.get(&format!("/api/v1/software/{}", f.software_id)).await;
    assert_eq!(after["instance_count"], 0, "a withdrawn deployment must not still be counted");
    let (_, list) = h.get("/api/v1/instances").await;
    assert_eq!(list["total"], 0);

    // Still resolves, because something may cite it.
    let (status, inst) = h.get(&format!("/api/v1/instances/{}", f.instance_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inst["tombstoned"], true);
}

#[tokio::test]
async fn editing_software_preserves_its_creation_date() {
    let h = harness().await;
    let f = h.fixture().await;
    let (_, before) = h.get(&format!("/api/v1/software/{}", f.software_id)).await;
    let created = before["created"].as_str().unwrap().to_string();

    let (status, after, _) = h
        .req(
            "PATCH",
            &format!("/api/v1/software/{}", f.software_id),
            Some(ROOT),
            Some(json!({"name": "shacl-manager", "tagline": "renamed", "license": "https://spdx.org/licenses/MIT"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["tagline"], "renamed");
    assert_eq!(after["license"], "https://spdx.org/licenses/MIT");
    assert_eq!(after["created"], created);
}

// ------------------------------------------------------------ openlineage

#[tokio::test]
async fn an_openlineage_event_becomes_a_run_with_the_payload_preserved() {
    let h = harness().await;
    let f = h.fixture().await;
    let event = json!({
        "eventType": "COMPLETE",
        "eventTime": "2026-08-30T14:02:49Z",
        "run": {"runId": "3f2b1c4d-0000-4000-8000-000000000001"},
        "job": {"namespace": "airflow://ids3", "name": "validate_cohort"},
        "inputs": [{"namespace": "s3://ids-bucket", "name": "cohort/in.ttl",
                    "facets": {"dataSource": {"uri": "s3://ids-bucket/cohort/in.ttl"}}}],
        "outputs": [{"namespace": "https://shacl.ids.unimaas.nl", "name": "reports/9f2a",
                     "facets": {"storage": {"fileFormat": "text/turtle"}}}],
        "producer": "https://github.com/OpenLineage/OpenLineage"
    });
    let (status, out) = h.post("/api/v1/openlineage", &f.token, event.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{out}");
    assert_eq!(out["mapped_status"], "success");
    assert_eq!(out["produced"].as_array().unwrap().len(), 1);
    assert_eq!(out["consumed"].as_array().unwrap().len(), 1);

    let run_id = out["run"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let (_, run) = h.get(&format!("/api/v1/runs/{run_id}")).await;
    assert_eq!(run["status"], "success");
    assert_eq!(run["external_key"], "3f2b1c4d-0000-4000-8000-000000000001");
    assert_eq!(run["instance"], f.instance_iri, "the instance comes from the token, not job.namespace");
    // Nothing the mapping does not name is lost (spec §7.6).
    assert_eq!(run["openlineage_payload"]["producer"], "https://github.com/OpenLineage/OpenLineage");

    // A second event for the same runId updates that run rather than making a new one.
    let (_, again) = h.post("/api/v1/openlineage", &f.token, event).await;
    assert_eq!(again["run"], out["run"]);
    let (_, runs) = h.get(&format!("/api/v1/instances/{}/runs", f.instance_id)).await;
    assert_eq!(runs["total"], 1);
}

// -------------------------------------------------------------- federation

#[tokio::test]
async fn peer_administration_is_admin_only_and_announcements_only_suggest() {
    let h = harness().await;
    let f = h.fixture().await;

    let (status, _, _) = h.req("GET", "/api/v1/peers", Some(&f.token), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "an instance token is not an admin");

    // An inbound announcement is deliberately unauthenticated: it grants nothing.
    let (status, out) = {
        let (s, v, _) = h.req("POST", "/api/v1/peers/announce", None, Some(json!({"base_url": "https://reg.mumc.nl", "title": "MUMC"}))).await;
        (s, v)
    };
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["state"], "suggested");

    let (status, active, _) = h.req("GET", "/api/v1/peers", Some(ROOT), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(active["total"], 0, "an announcement must never auto-add a peer (spec §8.4)");

    let (_, suggested, _) = h.req("GET", "/api/v1/peers/suggested", Some(ROOT), None).await;
    assert_eq!(suggested["total"], 1);
    assert_eq!(suggested["items"][0]["base_iri"], "https://reg.mumc.nl");
}

#[tokio::test]
async fn registry_counts_do_not_confuse_a_release_with_software() {
    let h = harness().await;
    let f = h.fixture().await;
    h.post(&format!("/api/v1/software/{}/releases", f.software_id), ROOT, json!({"version": "2.1.0"})).await;

    let (status, reg) = h.get("/api/v1/registry").await;
    assert_eq!(status, StatusCode::OK);
    // A Release is also a schema:SoftwareApplication; counting by type alone would say 2.
    assert_eq!(reg["counts"]["software"], 1);
    assert_eq!(reg["counts"]["releases"], 1);
    assert_eq!(reg["counts"]["instances"], 1);
}

#[tokio::test]
async fn the_well_known_document_describes_how_to_authenticate() {
    let h = harness_with_oidc(true).await;
    let (status, doc) = h.get("/.well-known/tar-registry").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["base_iri"], BASE);
    assert_eq!(doc["auth"]["anonymous_read"], true);
    assert_eq!(doc["auth"]["oidc"]["enabled"], true);
    assert_eq!(doc["auth"]["oidc"]["issuer"], ISSUER);
    assert_eq!(doc["auth"]["oidc"]["client_claim"], "azp");
    assert_eq!(doc["auth"]["oidc"]["human_signin"], true);

    // With no issuer configured the UI must hide sign-in entirely (handoff §7).
    let h = harness().await;
    let (_, doc) = h.get("/.well-known/tar-registry").await;
    assert_eq!(doc["auth"]["oidc"]["enabled"], false);
    assert_eq!(doc["auth"]["oidc"]["human_signin"], false);
}

// ------------------------------------------------------------------ sparql

#[tokio::test]
async fn sparql_is_read_only_and_answers_select_ask_and_construct() {
    let h = harness().await;
    h.fixture().await;

    let q = "PREFIX schema: <https://schema.org/> SELECT ?name WHERE { GRAPH ?g { ?s schema:name ?name } }";
    let req = Request::builder().method("POST").uri("/sparql").body(Body::from(q.to_string())).unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let names: Vec<&str> = body["results"]["bindings"].as_array().unwrap().iter().map(|b| b["name"]["value"].as_str().unwrap()).collect();
    assert!(names.contains(&"shacl-manager"), "{names:?}");

    let req = Request::builder()
        .method("POST")
        .uri("/sparql")
        .body(Body::from("DELETE WHERE { ?s ?p ?o }".to_string()))
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "the SPARQL endpoint must refuse writes");

    // The query form is read past the prologue: every realistic ASK and DESCRIBE begins with
    // PREFIX declarations, and matching on the raw first word sent those down the SELECT path.
    let q = "PREFIX schema: <https://schema.org/> ASK { GRAPH ?g { ?s schema:name \"shacl-manager\" } }";
    let req = Request::builder().method("POST").uri("/sparql").body(Body::from(q.to_string())).unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "a prefixed ASK is still an ASK");
    let body: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["boolean"], true);

    let q = "PREFIX schema: <https://schema.org/>\n# a comment before the form\nCONSTRUCT { ?s schema:name ?n } WHERE { GRAPH ?g { ?s schema:name ?n } }";
    let req = Request::builder().method("POST").uri("/sparql").body(Body::from(q.to_string())).unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[axum::http::header::CONTENT_TYPE], "text/turtle; charset=utf-8");

    // A syntax error is the client's fault and says where it is, rather than a 500 whose
    // detail only echoes the query back.
    let req = Request::builder()
        .method("POST")
        .uri("/sparql")
        .body(Body::from("SELECT ?s WHERE { ?s ?p".to_string()))
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(
        body["detail"].as_str().unwrap().starts_with("SPARQL syntax error:"),
        "{body}"
    );
}

#[tokio::test]
async fn biotools_export_carries_the_declared_capability_as_edam_typed_io() {
    let h = harness().await;
    let f = h.fixture().await;
    let (status, doc) = h.get(&format!("/api/v1/software/{}/export/biotools", f.software_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["name"], "shacl-manager");
    assert_eq!(doc["toolType"][0], "Web service");
    assert_eq!(doc["license"], "Apache-2.0");
    assert_eq!(doc["function"][0]["input"][0]["data"]["uri"], "http://edamontology.org/data_2600");
    assert_eq!(doc["function"][0]["output"][0]["data"]["uri"], "http://edamontology.org/data_2048");
}

#[tokio::test]
async fn instance_signals_and_the_outdated_release_marker() {
    let h = harness().await;
    let f = h.fixture().await;

    let (status, rel) = h
        .post(&format!("/api/v1/software/{}/releases", f.software_id), ROOT, json!({"version": "2.0.0", "date_published": "2026-01-01T00:00:00Z"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{rel}");
    let old_release = rel["id"].as_str().unwrap().to_string();
    h.post(&format!("/api/v1/software/{}/releases", f.software_id), ROOT, json!({"version": "2.1.0", "date_published": "2026-06-01T00:00:00Z"})).await;

    let (status, inst, _) = h
        .req(
            "PATCH",
            &format!("/api/v1/instances/{}", f.instance_id),
            Some(ROOT),
            Some(json!({"label": "shacl.ids.unimaas.nl", "software": f.software_id, "release": old_release,
                        "endpoint_url": "https://shacl.ids.unimaas.nl"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{inst}");
    assert_eq!(inst["release_version"], "2.0.0");
    assert_eq!(inst["latest_version"], "2.1.0");
    assert_eq!(inst["outdated"], true, "an instance behind the latest release is marked");

    h.post(
        "/api/v1/advertise/produced",
        &f.token,
        json!({"run": {"external_key": "sig-1", "status": "failed", "started_at": chrono::Utc::now().to_rfc3339()}, "artifacts": []}),
    )
    .await;
    let (_, inst) = h.get(&format!("/api/v1/instances/{}", f.instance_id)).await;
    assert_eq!(inst["runs_30d"], 1);
    assert_eq!(inst["failures_30d"], 1);
}

#[tokio::test]
async fn an_instance_without_an_endpoint_is_normal_not_broken() {
    let h = harness().await;
    let f = h.fixture().await;
    let (status, inst) = h
        .post("/api/v1/instances", ROOT, json!({"label": "laptop-eerol", "software": f.software_id}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inst}");
    assert!(inst["endpoint_url"].is_null());
    assert_eq!(inst["label"], "laptop-eerol");

    // It is a prov:SoftwareAgent but not a dcat:DataService.
    let (_, ttl, _) = {
        let req = Request::builder()
            .uri(format!("/instance/{}", inst["id"].as_str().unwrap()))
            .header("accept", "text/turtle")
            .body(Body::empty())
            .unwrap();
        let resp = h.app.clone().oneshot(req).await.unwrap();
        let s = resp.status();
        let headers = resp.headers().clone();
        let b = resp.into_body().collect().await.unwrap().to_bytes();
        (s, String::from_utf8_lossy(&b).to_string(), headers)
    };
    assert!(ttl.contains("prov:SoftwareAgent"), "{ttl}");
    assert!(!ttl.contains("dcat:DataService"), "{ttl}");
}

#[tokio::test]
async fn keyset_pagination_walks_the_whole_list_without_repeats() {
    let h = harness().await;
    for i in 0..7 {
        let (status, _) = h.post("/api/v1/software", ROOT, json!({"name": format!("tool-{i}"), "kind": "cli"})).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let uri = match &cursor {
            Some(c) => format!("/api/v1/software?limit=3&cursor={}", urlencoding(c)),
            None => "/api/v1/software?limit=3".to_string(),
        };
        let (status, page) = h.get(&uri).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        for item in page["items"].as_array().unwrap() {
            seen.push(item["iri"].as_str().unwrap().to_string());
        }
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(seen.len(), 7, "every record exactly once: {seen:?}");
    let unique: std::collections::HashSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), 7);
}

fn urlencoding(s: &str) -> String {
    s.replace(':', "%3A").replace('/', "%2F")
}

#[tokio::test]
async fn the_audit_log_records_who_wrote_what() {
    let h = harness().await;
    let f = h.fixture().await;
    h.post("/api/v1/advertise/produced", &f.token, json!({"run": {"external_key": "a-1"}, "artifacts": []})).await;

    let (status, log, _) = h.req("GET", "/api/v1/audit", Some(ROOT), None).await;
    assert_eq!(status, StatusCode::OK);
    let actions: Vec<&str> = log["items"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    assert!(actions.contains(&"software.create"), "{actions:?}");
    assert!(actions.contains(&"advertise.produced"), "{actions:?}");
    assert!(actions.contains(&"token.mint"), "{actions:?}");

    let advert = log["items"].as_array().unwrap().iter().find(|e| e["action"] == "advertise.produced").unwrap();
    assert_eq!(advert["actor"], f.instance_iri, "the audit actor is the deployment");
    assert_eq!(advert["actor_kind"], "instance-token");
}

// -------------------------------------------- human sign-in (Keycloak, verified end to end)
//
// These cover what driving a real Keycloak through the browser flow turned up: how a person
// with no registry role is classified, and what a *workload-only* issuer is allowed to assert.

const PARTNER_ISSUER: &str = "https://partner.example/realms/theirs";

/// Trusts `ISSUER` for people and workloads, and `PARTNER_ISSUER` for workloads only —
/// exactly the `TAR_OIDC_ISSUER` + `TAR_WORKLOAD_ISSUERS` split of addendum §4.
async fn harness_with_workload_issuer() -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    config.oidc.issuer = Some(ISSUER.into());
    config.oidc.client_id = Some("tar-ui".into());
    config.oidc.audience = Some(BASE.into());
    config.oidc.workload_issuers = vec![PARTNER_ISSUER.into()];
    let store = Arc::new(OxigraphStore::memory().unwrap());
    let ops = Ops::open(":memory:").await.unwrap();
    let mut state = AppState::from_parts(config, store, ops);
    state.jwt = state.jwt.with_static_key("test-key", Algorithm::HS256, DecodingKey::from_secret(HS_SECRET));
    let state = Arc::new(state);
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state.clone()), state }
}

#[tokio::test]
async fn a_signed_in_person_with_no_role_is_a_person_not_a_workload() {
    let h = harness_with_oidc(true).await;
    // What Keycloak issues to the browser client for a user who holds none of our roles:
    // `azp` is the UI client and `sid` marks an interactive session.
    let token = jwt(json!({
        "iss": ISSUER, "aud": BASE, "exp": exp(), "sub": "u-nobody",
        "azp": "tar-ui", "sid": "0b1d…", "preferred_username": "nobody",
        "realm_access": {"roles": ["default-roles-tar", "offline_access"]}
    }));
    let (status, who, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{who}");
    assert_eq!(who["credential"], "oidc-human", "a person without a role is still a person");
    assert_eq!(who["display_name"], "nobody");
    assert_eq!(who["is_curator"], false);
    assert!(who["instance"].is_null());

    // Authenticated, but with no authority: writing needs a role.
    let (status, body) = h.post("/api/v1/software", &token, json!({"name": "x", "kind": "cli"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn a_workload_only_issuer_cannot_assert_human_roles() {
    let h = harness_with_workload_issuer().await;
    // A partner's Keycloak can put any realm role it likes in its own tokens. It is trusted
    // to say *who a workload is*, never to say who is an admin here (addendum §4).
    let token = jwt(json!({
        "iss": PARTNER_ISSUER, "aud": BASE, "exp": exp(), "sub": "them-1",
        "azp": "their-client", "sid": "s-1", "preferred_username": "mallory",
        "realm_access": {"roles": ["admin", "curator"]}
    }));
    let (status, who, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{who}");
    assert_eq!(who["roles"].as_array().unwrap().len(), 0, "{who}");
    assert_eq!(who["is_admin"], false);
    assert_eq!(who["credential"], "oidc-workload");

    let (status, _) = h.post("/api/v1/peers", &token, json!({"base_url": "https://reg.example"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The same claims from the estate's own issuer *are* honoured.
    let mine = jwt(json!({
        "iss": ISSUER, "aud": BASE, "exp": exp(), "sub": "u-1", "azp": "tar-ui", "sid": "s-2",
        "realm_access": {"roles": ["admin"]}
    }));
    let (status, who, _) = h.req("GET", "/api/v1/whoami", Some(&mine), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(who["is_admin"], true, "{who}");
    assert_eq!(who["credential"], "oidc-human");
}

#[tokio::test]
async fn the_browser_sign_in_client_is_never_treated_as_a_deployment() {
    let h = harness_with_oidc(true).await;
    let f = h.fixture().await;
    // An Instance record that declares the UI's own client id must not turn every person who
    // signs in into that deployment.
    let (status, body) = h
        .post(
            "/api/v1/instances",
            ROOT,
            json!({
                "label": "a deployment that wrongly claims the UI client id",
                "software": f.software_id,
                "endpoint_url": "https://confused.example",
                "oidc_client_id": "tar-ui"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let token = jwt(json!({
        "iss": ISSUER, "aud": BASE, "exp": exp(), "sub": "u-2", "azp": "tar-ui", "sid": "s-3",
        "realm_access": {"roles": ["curator"]}
    }));
    let (status, who, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{who}");
    assert_eq!(who["credential"], "oidc-human");
    assert!(who["instance"].is_null(), "a person is not a deployment: {who}");
}

// ------------------------------------------- federated search propagation
//
// A federated search no longer stops at this registry's own peers: it propagates, carrying a
// query id and a hop budget. In a mesh with a cycle that is an infinite storm unless the
// same query is refused the second time it arrives, so these tests exercise the refusal, the
// budget, and a real cycle over real sockets.

/// One registry serving on a real loopback port, plus an in-process handle to the same router
/// so a test can start a query at it without needing an HTTP client of its own.
struct FedNode {
    base: String,
    h: Harness,
}

/// Bind first, then build the config: a registry's base IRI has to be the URL its peers will
/// actually reach it on, and the port is only known after binding.
async fn spawn_registry(title: &str, software_name: &str) -> FedNode {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());

    let mut config = Config::for_test(&base);
    config.root_token = Some(ROOT.into());
    config.title = title.to_string();
    let store = Arc::new(OxigraphStore::memory().unwrap());
    let ops = Ops::open(":memory:").await.unwrap();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();

    let app = tar::app(state.clone());
    let served = app.clone();
    tokio::spawn(async move {
        axum::serve(listener, served).await.unwrap();
    });

    let node = FedNode { base, h: Harness { app, state } };
    let (status, body) = node
        .h
        .post(
            "/api/v1/software",
            ROOT,
            json!({
                "name": software_name,
                "tagline": "SHACL shape management and validation",
                "code_repository": "https://github.com/MaastrichtU-IDS/shacl-manager",
                "license": "https://spdx.org/licenses/Apache-2.0",
                "kind": "service"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    node
}

/// Trust `other` as a peer of `node`, over the real well-known handshake.
async fn peer_with(node: &FedNode, other: &FedNode) {
    let (status, body) = node.h.post("/api/v1/peers", ROOT, json!({"base_url": other.base, "announce": false})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

fn titles(results: &Value) -> Vec<String> {
    results["hits"].as_array().unwrap().iter().map(|h| h["title"].as_str().unwrap_or_default().to_string()).collect()
}

fn hit<'a>(results: &'a Value, title: &str) -> &'a Value {
    results["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["title"] == title)
        .unwrap_or_else(|| panic!("no hit titled {title} in {results}"))
}

#[tokio::test]
async fn a_repeated_federated_query_id_is_refused_as_already_handled() {
    let h = harness().await;
    h.fixture().await;

    let (status, first) = h.get("/api/v1/search?q=shacl&federated=true&fed_id=q-repeat-1").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert!(first["total"].as_i64().unwrap() > 0, "the first arrival is served normally: {first}");
    assert!(!first["already_handled"].as_bool().unwrap_or(false));
    assert_eq!(first["federation"]["query_id"], "q-repeat-1");

    // The same id again is a loop. It is refused *explicitly* — the caller is told which id
    // was refused and why zero hits is the right answer — not silently answered with an
    // empty result set that looks like "nothing matched".
    let (status, again) = h.get("/api/v1/search?q=shacl&federated=true&fed_id=q-repeat-1").await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["already_handled"], true, "{again}");
    assert_eq!(again["total"], 0);
    assert!(again["hits"].as_array().unwrap().is_empty());
    assert_eq!(again["federation"]["query_id"], "q-repeat-1");
    assert!(again["federation"]["first_seen_at"].is_string(), "{again}");
    let reason = again["federation"]["reason"].as_str().unwrap();
    assert!(reason.contains("already handled") && reason.contains("q-repeat-1"), "{reason}");

    // The refusal is per query id, not a circuit breaker on the registry.
    let (_, other) = h.get("/api/v1/search?q=shacl&federated=true&fed_id=q-repeat-2").await;
    assert!(!other["already_handled"].as_bool().unwrap_or(false));
    assert!(other["total"].as_i64().unwrap() > 0);

    // A query id is an identifier, not free text: a malformed one is refused rather than
    // rewritten, because rewriting it would silently break the sender's own deduplication.
    let (status, _) = h.get("/api/v1/search?q=shacl&federated=true&fed_id=%27%20OR%201%3D1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A plain local search claims nothing and reports no federation envelope.
    let (_, local) = h.get("/api/v1/search?q=shacl").await;
    assert!(local["federation"].is_null(), "{local}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hop_budget_stops_propagation() {
    // A knows B; B knows C; A has never heard of C.
    let a = spawn_registry("A", "shacl-manager-alpha").await;
    let b = spawn_registry("B", "shacl-manager-bravo").await;
    let c = spawn_registry("C", "shacl-manager-charlie").await;
    peer_with(&a, &b).await;
    peer_with(&b, &c).await;

    // Two hops: A -> B -> C. C's record reaches A even though A does not peer with C.
    let (status, deep) = a.h.get("/api/v1/search?q=shacl-manager&federated=true&fed_hops=2").await;
    assert_eq!(status, StatusCode::OK, "{deep}");
    let found = titles(&deep);
    assert!(found.contains(&"shacl-manager-charlie".to_string()), "propagation should reach C: {found:?}");

    // And the report says *how* each result arrived. A hit relayed by B is not the same
    // evidence as one from B itself, and the response keeps them apart.
    let from_b = hit(&deep, "shacl-manager-bravo");
    assert_eq!(from_b["reach"], "direct");
    assert_eq!(from_b["hops"], 1);
    assert_eq!(from_b["via"], b.base);
    let from_c = hit(&deep, "shacl-manager-charlie");
    assert_eq!(from_c["reach"], "indirect");
    assert_eq!(from_c["hops"], 2);
    assert_eq!(from_c["via"], b.base, "C was reached through B");
    assert_eq!(from_c["origin"]["peer_base_iri"], c.base, "the home registry stays C, not B");
    assert_eq!(hit(&deep, "shacl-manager-alpha")["reach"], "local");

    // C also shows up in the topology report as a peer we never configured.
    let c_status = deep["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["base_iri"] == c.base)
        .unwrap_or_else(|| panic!("C should appear in the peer report: {deep}"));
    assert_eq!(c_status["reach"], "indirect");
    assert_eq!(c_status["via"], b.base);

    // One hop: the budget runs out at B, so C is never asked.
    let (status, shallow) = a.h.get("/api/v1/search?q=shacl-manager&federated=true&fed_hops=1").await;
    assert_eq!(status, StatusCode::OK, "{shallow}");
    let found = titles(&shallow);
    assert!(found.contains(&"shacl-manager-bravo".to_string()), "{found:?}");
    assert!(!found.contains(&"shacl-manager-charlie".to_string()), "the hop budget must stop at B: {found:?}");
    assert_eq!(shallow["federation"]["hops_granted"], 1);
    assert_eq!(shallow["federation"]["hops_forwarded"], 0);

    // Zero hops is a local search that still claims its id.
    let (_, none) = a.h.get("/api/v1/search?q=shacl-manager&federated=true&fed_hops=0").await;
    assert_eq!(titles(&none), vec!["shacl-manager-alpha".to_string()]);
    assert_eq!(none["federation"]["budget_exhausted"], true, "{none}");

    // A budget bigger than ours is clamped, so a peer cannot grant us more than it was given.
    let (_, greedy) = a.h.get("/api/v1/search?q=shacl-manager&federated=true&fed_hops=9999").await;
    let granted = greedy["federation"]["hops_granted"].as_u64().unwrap();
    let max = greedy["federation"]["max_hops"].as_u64().unwrap();
    assert!(granted <= max && max <= 8, "granted {granted}, max {max}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cycle_in_the_peer_graph_terminates() {
    // The nastiest shape: every registry peers with both others, so every query has two
    // routes to every node and one of them must be cut.
    let a = spawn_registry("A", "shacl-manager-alpha").await;
    let b = spawn_registry("B", "shacl-manager-bravo").await;
    let c = spawn_registry("C", "shacl-manager-charlie").await;
    for (x, y) in [(&a, &b), (&a, &c), (&b, &a), (&b, &c), (&c, &a), (&c, &b)] {
        peer_with(x, y).await;
    }

    // If propagation did not terminate this would hang rather than fail; the timeout turns a
    // storm into a test failure.
    let (status, r) = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        a.h.get("/api/v1/search?q=shacl-manager&federated=true&fed_id=cycle-1&fed_hops=3"),
    )
    .await
    .expect("a federated search over a cyclic peer graph must terminate");
    assert_eq!(status, StatusCode::OK, "{r}");

    // Every registry answered, and each record appears exactly once however many routes
    // reached it.
    let mut found = titles(&r);
    found.sort();
    assert_eq!(
        found,
        vec!["shacl-manager-alpha".to_string(), "shacl-manager-bravo".to_string(), "shacl-manager-charlie".to_string()],
        "each registry's record exactly once: {r}"
    );

    // The proof that termination came from loop prevention and not from luck: every registry
    // recorded this query id exactly once, and at least one refused a repeat of it.
    let mut repeats = 0;
    for (name, node) in [("A", &a), ("B", &b), ("C", &c)] {
        let seen = tar::ops::federation::seen_query(&node.h.state.ops, "cycle-1")
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{name} should have claimed the query id"));
        repeats += seen.repeat_count;
    }
    assert!(repeats >= 1, "a triangle gives some registry two routes to the same query; one must be refused");

    // …and that refusal is visible in the topology report rather than looking like a failure.
    let refused: Vec<&Value> =
        r["peers"].as_array().unwrap().iter().filter(|p| p["status"] == "already_handled" || p["status"] == "skipped").collect();
    assert!(!refused.is_empty(), "the cut edges must be reported: {r}");
    assert_eq!(r["partial"], false, "a refused repeat is a healthy answer, not a partial one: {r}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_cannot_flood_us_with_results() {
    let a = spawn_registry("A", "shacl-manager-alpha").await;

    // A peer that answers with five thousand hits, and one that answers with megabytes.
    let flood = spawn_hostile_peer(5_000, 20).await;
    let giant = spawn_hostile_peer(5_000, 800).await;
    let (status, body) = a.h.post("/api/v1/peers", ROOT, json!({"base_url": flood, "announce": false})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = a.h.post("/api/v1/peers", ROOT, json!({"base_url": giant, "announce": false})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, r) = a.h.get("/api/v1/search?q=shacl&federated=true").await;
    assert_eq!(status, StatusCode::OK, "{r}");

    let from_flood = r["hits"].as_array().unwrap().iter().filter(|h| h["via"] == flood).count();
    assert!(from_flood > 0 && from_flood <= 100, "a peer's hits are capped, got {from_flood}");
    assert!(r["hits"].as_array().unwrap().len() <= 200, "our own response stays bounded: {}", r["total"]);

    // The oversized body is refused outright rather than buffered.
    let giant_status = r["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["base_iri"] == giant)
        .unwrap_or_else(|| panic!("{r}"));
    assert_eq!(giant_status["status"], "error", "{giant_status}");
    assert!(giant_status["error"].as_str().unwrap().contains("cap"), "{giant_status}");
    assert_eq!(r["partial"], true, "a peer we could not read makes the answer partial");
}

/// A stub that speaks just enough of the protocol to be added as a peer, and then answers
/// every search with `hits` results whose titles are `title_len` characters long.
async fn spawn_hostile_peer(hits: usize, title_len: usize) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let wk = base.clone();
    let app = axum::Router::new()
        .route(
            "/.well-known/tar-registry",
            axum::routing::get(move || {
                let wk = wk.clone();
                async move { axum::Json(json!({"base_iri": wk, "title": "hostile", "peers": []})) }
            }),
        )
        .route(
            "/api/v1/search",
            axum::routing::get(move || async move {
                let filler = "shacl".repeat(title_len / 5 + 1);
                let hits: Vec<Value> = (0..hits)
                    .map(|i| {
                        json!({
                            "iri": format!("http://hostile.example/software/{i}"),
                            "entity_type": "software",
                            "title": format!("shacl-{i}-{filler}"),
                            "origin": {"kind": "local"},
                            "score": 0.9
                        })
                    })
                    .collect();
                axum::Json(json!({"query": "shacl", "hits": hits, "total": hits.len(), "partial": false, "peers": []}))
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    base
}


// ---------------------------------------------------------------- forge sync

#[tokio::test]
async fn sync_only_touches_the_fields_the_record_put_under_its_control() {
    let h = harness().await;
    let (status, sw) = h
        .post(
            "/api/v1/software",
            ROOT,
            json!({
                "name": "shacl-rust",
                "tagline": "a tagline a person wrote",
                "description": "a paragraph a person wrote",
                "sync": {"source": "github", "repo": "ensaremirerol/shacl-rust",
                         "fields": ["tagline", "license"]}
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let id = sw["id"].as_str().unwrap().to_string();
    assert_eq!(sw["sync"]["repo"], "ensaremirerol/shacl-rust");
    assert_eq!(sw["sync"]["last_status"], "never");

    // `description` is not managed, so no sync may ever overwrite it. That is the whole
    // contract: connecting a repository must not silently discard curated prose.
    let (_, after) = h.get(&format!("/api/v1/software/{id}")).await;
    assert_eq!(after["description"], "a paragraph a person wrote");
}

#[tokio::test]
async fn a_field_no_forge_can_supply_is_refused_when_it_is_configured() {
    let h = harness().await;
    // Better to fail here, where someone is looking, than at 3am in a scheduled sync.
    let (status, body) = h
        .post(
            "/api/v1/software",
            ROOT,
            json!({"name": "x", "sync": {"repo": "o/n", "fields": ["capability", "name"]}}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let detail = body["detail"].as_str().unwrap();
    assert!(detail.contains("capability"), "{detail}");
    assert!(detail.contains("syncable fields are"), "{detail}");
}

#[tokio::test]
async fn syncing_a_record_with_no_repository_says_so() {
    let h = harness().await;
    let (_, sw) = h.post("/api/v1/software", ROOT, json!({"name": "unconnected"})).await;
    let (status, body) = h
        .post(&format!("/api/v1/software/{}/sync", sw["id"].as_str().unwrap()), ROOT, json!({}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["detail"].as_str().unwrap().contains("not connected to a repository"), "{body}");
}


#[tokio::test]
async fn a_token_with_no_audience_at_all_is_rejected_when_an_audience_is_required() {
    let h = harness_with_oidc(true).await;
    h.fixture().await;
    // `aud` is how a token says which service it is for. A token minted by a trusted issuer
    // for some *other* service, carrying no audience, must not be usable here — otherwise
    // "require audience" only means "check it if they bothered to send one", and any token
    // from that issuer works against every service that trusts it.
    let token = jwt(json!({
        "iss": ISSUER, "exp": exp(), "sub": "u-1", "azp": "shacl-manager-ids3",
        "scope": "advertise:produce"
    }));
    let (status, body, _) = h.req("GET", "/api/v1/whoami", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}


#[tokio::test]
async fn the_html_representation_carries_signposting_too() {
    let h = harness().await;
    let f = h.fixture().await;
    // Spec §6.3 says "every artifact and software GET emits Signposting Link headers". HTML is
    // the representation a person shares and a machine then follows, so it is the one that can
    // least afford to omit them.
    let req = Request::builder()
        .uri(format!("/software/{}", f.software_id))
        .header("accept", "text/html,application/xhtml+xml,*/*;q=0.8")
        .body(Body::empty())
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-type").unwrap(), "text/html; charset=utf-8");
    let link = resp.headers().get("link").expect("Signposting on the HTML page").to_str().unwrap().to_string();
    assert!(link.contains("rel=\"cite-as\""), "{link}");
    assert!(link.contains("rel=\"describedby\""), "{link}");
    assert!(link.contains("https://spdx.org/licenses/Apache-2.0>; rel=\"license\""), "{link}");

    // A page for something that does not exist still renders (the SPA owns 404s), and simply
    // has nothing to point at.
    let req = Request::builder()
        .uri("/software/01a00000-0000-7000-8000-000000000000")
        .header("accept", "text/html")
        .body(Body::empty())
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("link").is_none());
}

#[tokio::test]
async fn plain_http_is_a_nameable_access_protocol() {
    let h = harness().await;
    let f = h.fixture().await;
    // An intranet service really does serve over http. Refusing the value would force the
    // record to omit the field and lose the fact that the transport is unencrypted.
    let (status, out) = h
        .post(
            "/api/v1/advertise/produced",
            &f.token,
            json!({"run": {"external_key": "http-1"}, "artifacts": [{
                "title": "a graph on an intranet host",
                "conforms_to": "http://edamontology.org/data_2600",
                "distributions": [{
                    "download_url": "http://intranet.example.internal/g.ttl",
                    "access_protocol": "http",
                    "availability": "restricted",
                    "access_request_url": "https://example.org/access"
                }]
            }]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
}


#[tokio::test]
async fn an_artifact_title_never_becomes_a_selectable_artifact_type() {
    let h = harness().await;
    let f = h.fixture().await;
    h.post(
        "/api/v1/advertise/produced",
        &f.token,
        json!({"run": {"external_key": "t-1"}, "artifacts": [{
            "title": "Cohort extract, March",
            "conforms_to": "http://edamontology.org/data_2048",
            "distributions": [{"access_url": "https://x.example/1", "availability": "public"}]
        }]}),
    )
    .await;

    // Every artifact gets a version-series node (D10). It is a "concept" in the Zenodo sense,
    // but it is not a *type*, and offering an artifact's own title as a type to classify the
    // next artifact with would be nonsense that compounds with every advertisement.
    let (_, types) = h.get("/api/v1/types?limit=200").await;
    let labels: Vec<&str> = types["items"].as_array().unwrap().iter().map(|t| t["label"].as_str().unwrap()).collect();
    assert!(!labels.contains(&"Cohort extract, March"), "{labels:?}");

    let (_, hits) = h.get("/api/v1/vocab/search?q=Cohort").await;
    assert_eq!(hits["items"].as_array().unwrap().len(), 0, "nor in the picker: {hits}");

    // The type it actually conforms to is listed, because something declares itself as it.
    assert!(labels.iter().any(|l| *l == "Report"), "the EDAM type in use should be listed: {labels:?}");
}

#[tokio::test]
async fn the_type_list_is_types_in_use_not_the_whole_bundled_vocabulary() {
    let h = harness().await;
    h.fixture().await;
    // EDAM ships bundled for the pickers — over a thousand concepts. Answering "which types
    // does this registry use?" with all of them is not an answer; /vocab/search is for
    // searching the vocabulary.
    let (status, types) = h.get("/api/v1/types?limit=500").await;
    assert_eq!(status, StatusCode::OK);
    assert!(types["total"].as_i64().unwrap() < 50, "expected types in use, got {}", types["total"]);

    // The picker still sees the whole bundle.
    let (_, hits) = h.get("/api/v1/vocab/search?q=sequence%20alignment&branch=data&limit=5").await;
    assert!(!hits["items"].as_array().unwrap().is_empty(), "vocabulary search should still reach EDAM");
}


// ------------------------------------------------------- self-advertisement

#[tokio::test]
async fn a_deployment_records_its_own_endpoint_without_losing_the_rest() {
    let h = harness().await;
    let f = h.fixture().await;
    // PATCH means merge. It used to replace, so a deployment recording an endpoint silently
    // dropped its operator, jurisdiction, scopes and OIDC binding — everything it did not
    // happen to resend.
    let (status, patched, _) = h
        .req(
            "PATCH",
            &format!("/api/v1/instances/{}", f.instance_id),
            Some(&f.token),
            Some(json!({"endpoint_url": "https://shacl.ids.unimaas.nl"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["endpoint_url"], "https://shacl.ids.unimaas.nl");
    assert_eq!(patched["label"], "shacl.ids.unimaas.nl", "the label survived");
    assert_eq!(patched["oidc_client_id"], "shacl-manager-ids3", "the binding survived");
    assert_eq!(patched["allowed_scopes"].as_array().unwrap().len(), 2, "the scopes survived");

    // An explicit null is the way to clear a field, and the only way to tell "leave it" from
    // "erase it" once absent means leave it.
    let (_, cleared, _) = h
        .req(
            "PATCH",
            &format!("/api/v1/instances/{}", f.instance_id),
            Some(&f.token),
            Some(json!({"endpoint_url": null})),
        )
        .await;
    assert!(cleared["endpoint_url"].is_null());
    assert_eq!(cleared["oidc_client_id"], "shacl-manager-ids3");
}

#[tokio::test]
async fn a_service_announces_itself_and_is_stamped_as_seen() {
    let h = harness().await;
    let f = h.fixture().await;
    let (status, announced, _) = h
        .req(
            "PUT",
            "/api/v1/instances/self",
            Some(&f.token),
            Some(json!({"endpoint_url": "https://shacl.ids.unimaas.nl", "jurisdiction": "NL"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{announced}");
    assert_eq!(announced["endpoint_url"], "https://shacl.ids.unimaas.nl");
    assert_eq!(announced["jurisdiction"], "NL");
    assert!(announced["last_seen_at"].is_string(), "an announcement is a liveness signal");
    // It updated its own record rather than creating a second one.
    let (_, list) = h.get("/api/v1/instances").await;
    assert_eq!(list["total"], 1);
}

#[tokio::test]
async fn an_unbound_workload_cannot_conjure_a_deployment_by_default() {
    let h = harness_with_oidc(true).await;
    h.fixture().await;
    // A verified token from a trusted issuer, bound to no Instance. Registering it silently
    // would mean the registry gains records for anything holding a trusted token.
    let token = jwt(json!({
        "iss": ISSUER, "aud": BASE, "exp": exp(), "sub": "service-account-newcomer",
        "azp": "newcomer", "scope": "advertise:produce"
    }));
    let (status, body, _) = h
        .req("PUT", "/api/v1/instances/self", Some(&token), Some(json!({"software": "whatever"})))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let detail = body["detail"].as_str().unwrap();
    assert!(detail.contains("newcomer"), "it must name the client id an admin has to register: {detail}");
    assert!(detail.contains("TAR_OIDC_AUTO_REGISTER_INSTANCES"), "{detail}");
}

#[tokio::test]
async fn a_person_cannot_announce_a_deployment_as_if_they_were_one() {
    let h = harness().await;
    h.fixture().await;
    // The root credential is an administrator, not a running service. Which Instance a caller
    // *is* comes from the credential, so a principal that is not one has nothing to announce.
    let (status, body, _) = h
        .req("PUT", "/api/v1/instances/self", Some(ROOT), Some(json!({"endpoint_url": "https://x.example"})))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["detail"].as_str().unwrap().contains("only a deployment may announce itself"));
}

// ------------------------------------------- markdown representations and llms.txt

/// Fetch a URI asking for markdown, returning the body as text.
async fn markdown(h: &Harness, uri: &str) -> (StatusCode, String, axum::http::HeaderMap) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("accept", "text/markdown")
        .body(Body::empty())
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string(), headers)
}

#[tokio::test]
async fn an_agent_reads_a_software_record_as_markdown_without_a_parser() {
    let h = harness().await;
    let f = h.fixture().await;

    // Both routes to the same representation: the `.md` extension and the Accept header.
    let (status, body, headers) = markdown(&h, &format!("/software/{}", f.software_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "text/markdown; charset=utf-8"
    );
    let (_, by_extension, _) = markdown(&h, &format!("/software/{}.md", f.software_id)).await;
    assert_eq!(body, by_extension, "`.md` and Accept must render the same thing");

    assert!(body.starts_with("# shacl-manager"), "{body}");
    // The canonical IRI is written out, so the agent's next fetch is obvious.
    assert!(body.contains(&format!("{BASE}/software/{}", f.software_id)), "{body}");
    // And the other representations are named rather than assumed.
    assert!(body.contains(".ttl"), "{body}");
    assert!(body.contains("SHACL shape management and validation"), "{body}");
    assert!(body.contains("Apache-2.0"), "{body}");
    // The deployment is listed, so the agent can reach the running thing.
    assert!(body.contains("shacl.ids.unimaas.nl"), "{body}");
}

#[tokio::test]
async fn markdown_says_plainly_when_software_cannot_be_deployed() {
    let h = harness().await;
    let (status, sw) = h
        .post(
            "/api/v1/software",
            ROOT,
            json!({"name": "RDFCraft", "kinds": ["desktop"], "deployable": false}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let (_, body, _) = markdown(&h, &format!("/software/{}.md", sw["id"].as_str().unwrap())).await;
    // An agent that skims this must not come away thinking there is an endpoint to call.
    assert!(body.contains("**Deployable:** no"), "{body}");
    assert!(body.contains("cannot be hosted"), "{body}");
}

#[tokio::test]
async fn every_record_kind_renders_as_markdown() {
    let h = harness().await;
    let f = h.fixture().await;
    let (status, art) = h
        .post(
            "/api/v1/advertise/produced",
            &f.token,
            json!({
                "run": {"status": "success"},
                "artifacts": [{
                    "title": "Pizza shapes",
                    "conforms_to": "http://edamontology.org/data_2048",
                    "creators": [{"name": "A Person", "kind": "person"}],
                    "distributions": [{"download_url": "https://example.org/pizza.ttl", "media_type": "text/turtle"}]
                }]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{art}");

    let artifact_iri = art["artifacts"][0].as_str().unwrap();
    let artifact_id = artifact_iri.rsplit('/').next().unwrap();
    let (status, body, _) = markdown(&h, &format!("/artifact/{artifact_id}.md")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("# Pizza shapes"), "{body}");
    assert!(body.contains("A Person"), "{body}");
    assert!(body.contains("https://example.org/pizza.ttl"), "{body}");

    let run_id = art["run"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let (status, body, _) = markdown(&h, &format!("/run/{run_id}.md")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("## Produced"), "{body}");
    assert!(body.contains("Pizza shapes"), "{body}");

    let (status, body, _) = markdown(&h, &format!("/instance/{}.md", f.instance_id)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("shacl.ids.unimaas.nl"), "{body}");
}

#[tokio::test]
async fn llms_txt_is_a_map_of_the_whole_registry_and_needs_no_credential() {
    let h = harness().await;
    let f = h.fixture().await;

    let (status, body, headers) = markdown(&h, "/llms.txt").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "text/markdown; charset=utf-8"
    );
    // The llmstxt.org shape: an H1, then a blockquote summary.
    assert!(body.starts_with("# "), "{body}");
    assert!(body.contains("\n> "), "{body}");
    // It tells an unfamiliar client how to read a record...
    assert!(body.contains(".md"), "{body}");
    assert!(body.contains(".ttl"), "{body}");
    // ...names the entry points...
    assert!(body.contains("/api/v1/search"), "{body}");
    assert!(body.contains("/sparql"), "{body}");
    assert!(body.contains("/api/v1/vocab/search"), "{body}");
    // ...and lists what is actually here.
    assert!(body.contains("shacl-manager"), "{body}");
    assert!(body.contains(&f.software_id), "{body}");
    // The warnings that stop an agent inventing things.
    assert!(body.contains("deployable"), "{body}");
    assert!(body.contains("withdrawn"), "{body}");
}

#[tokio::test]
async fn a_withdrawn_record_still_resolves_and_says_so() {
    let h = harness().await;
    let (status, sw) = h.post("/api/v1/software", ROOT, json!({"name": "gone-tool"})).await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let id = sw["id"].as_str().unwrap().to_string();
    let (status, _, _) = h.req("DELETE", &format!("/api/v1/software/{id}"), Some(ROOT), None).await;
    assert!(status.is_success(), "{status}");

    let (status, body, _) = markdown(&h, &format!("/software/{id}.md")).await;
    assert_eq!(status, StatusCode::OK, "an IRI that once meant something keeps meaning it");
    assert!(body.contains("Withdrawn"), "{body}");

    // ...and it is not in the index a fresh agent reads.
    let (_, index, _) = markdown(&h, "/llms.txt").await;
    assert!(!index.contains("gone-tool"), "{index}");
}

// ------------------------------------------------------------------- API docs

#[tokio::test]
async fn api_descriptions_round_trip_as_dcat_endpoint_descriptions() {
    let h = harness().await;
    let (status, sw) = h
        .post(
            "/api/v1/software",
            ROOT,
            json!({
                "name": "ontoexplorer",
                "kinds": ["service"],
                "api_docs": [
                    {"url": "https://onto.example.org/openapi.json", "format": "openapi", "title": "REST API"},
                    {"url": "https://onto.example.org/sparql", "format": "sparql-service-description"},
                    // No format given: it is guessed from the URL rather than lost.
                    {"url": "https://onto.example.org/v2/swagger.json"}
                ]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let id = sw["id"].as_str().unwrap().to_string();

    let (status, back) = h.get(&format!("/api/v1/software/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    let docs = back["api_docs"].as_array().unwrap();
    assert_eq!(docs.len(), 3, "{back}");
    let by_url: std::collections::HashMap<&str, &Value> =
        docs.iter().map(|d| (d["url"].as_str().unwrap(), d)).collect();
    assert_eq!(by_url["https://onto.example.org/openapi.json"]["format"], "openapi");
    assert_eq!(by_url["https://onto.example.org/openapi.json"]["title"], "REST API");
    assert_eq!(
        by_url["https://onto.example.org/sparql"]["format"],
        "sparql-service-description",
        "not everything is OpenAPI"
    );
    assert_eq!(by_url["https://onto.example.org/v2/swagger.json"]["format"], "openapi");

    // The RDF uses DCAT's own term, not an invention of ours, and says which spec it follows.
    let req = Request::builder()
        .method("GET")
        .uri(format!("/software/{id}.ttl"))
        .body(Body::empty())
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    let ttl = String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).to_string();
    assert!(ttl.contains("endpointDescription"), "{ttl}");
    assert!(ttl.contains("spec.openapis.org"), "{ttl}");

    // And an agent reading the markdown is told where to fetch them.
    let (_, md, _) = markdown(&h, &format!("/software/{id}.md")).await;
    assert!(md.contains("## API"), "{md}");
    assert!(md.contains("https://onto.example.org/openapi.json"), "{md}");
    assert!(md.contains("SPARQL service description"), "{md}");
}

#[tokio::test]
async fn the_api_doc_proxy_only_fetches_what_the_record_itself_declares() {
    let h = harness().await;
    let (status, sw) = h
        .post(
            "/api/v1/software",
            ROOT,
            json!({"name": "svc", "api_docs": [{"url": "https://onto.example.org/openapi.json"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let id = sw["id"].as_str().unwrap().to_string();

    // An index the record does not have is a 404, not a fetch.
    let (status, body) = h.get(&format!("/api/v1/software/{id}/api-doc?n=7")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // There is no URL parameter to pass at all: the endpoint indexes the record's own list, so
    // it cannot be pointed at an arbitrary host.
    let (status, _) = h.get(&format!("/api/v1/software/{id}/api-doc?url=http://169.254.169.254/latest/meta-data")).await;
    assert_ne!(status, StatusCode::OK, "an unreachable declared doc must not 200");
}

// -------------------------------------------- registration mode 2: auto-register

#[tokio::test]
async fn an_application_key_registers_a_deployment_and_then_keeps_it_updated() {
    let h = harness().await;
    let (status, sw) = h
        .post("/api/v1/software", ROOT, json!({"name": "sulo-schema-builder", "kinds": ["service"]}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let software_id = sw["id"].as_str().unwrap().to_string();

    // A curator issues one key for the application itself, not for a deployment.
    let (status, minted) = h
        .post(&format!("/api/v1/software/{software_id}/tokens"), ROOT, json!({"label": "cluster deploys"}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    let key = minted["token"].as_str().unwrap().to_string();
    assert_eq!(minted["record"]["software_iri"], format!("{BASE}/software/{software_id}"));
    assert!(minted["record"]["scopes"].as_array().unwrap().iter().any(|s| s == "register:instance"), "{minted}");

    // The deployment registers itself. It never had to be created by hand.
    let (status, first, _) = h
        .req(
            "PUT",
            "/api/v1/instances/self",
            Some(&key),
            Some(json!({
                "label": "sulo on ids-cluster",
                "instance_key": "ids-cluster",
                "endpoint_url": "https://sulo.ids.example",
                "health_endpoint": "https://sulo.ids.example/healthz",
                "availability": "restricted"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let iri = first["iri"].as_str().unwrap().to_string();
    assert_eq!(first["software"], format!("{BASE}/software/{software_id}"));
    assert_eq!(first["health_endpoint"], "https://sulo.ids.example/healthz");

    // Announcing again updates that record rather than creating a second one.
    let (status, second, _) = h
        .req(
            "PUT",
            "/api/v1/instances/self",
            Some(&key),
            Some(json!({"instance_key": "ids-cluster", "endpoint_url": "https://sulo2.ids.example"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["iri"], iri, "the same deployment must not register twice");
    assert_eq!(second["endpoint_url"], "https://sulo2.ids.example");
    // And the rest of what it said the first time survives.
    assert_eq!(second["label"], "sulo on ids-cluster", "{second}");
    assert_eq!(second["health_endpoint"], "https://sulo.ids.example/healthz", "{second}");

    // A second deployment of the same application, under the same key, is a second record.
    let (status, other, _) = h
        .req(
            "PUT",
            "/api/v1/instances/self",
            Some(&key),
            Some(json!({"label": "sulo on dev", "instance_key": "dev-cluster"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    assert_ne!(other["iri"], iri);

    let (_, list) = h.get(&format!("/api/v1/instances?software={software_id}")).await;
    assert_eq!(list["total"], 2, "{list}");
}

#[tokio::test]
async fn an_application_key_cannot_register_a_deployment_of_a_different_application() {
    let h = harness().await;
    let (_, mine) = h.post("/api/v1/software", ROOT, json!({"name": "mine"})).await;
    let (_, theirs) = h.post("/api/v1/software", ROOT, json!({"name": "theirs"})).await;
    let mine_id = mine["id"].as_str().unwrap().to_string();
    let theirs_id = theirs["id"].as_str().unwrap().to_string();

    let (_, minted) = h.post(&format!("/api/v1/software/{mine_id}/tokens"), ROOT, json!({})).await;
    let key = minted["token"].as_str().unwrap().to_string();

    let (status, body, _) = h
        .req(
            "PUT",
            "/api/v1/instances/self",
            Some(&key),
            Some(json!({"label": "impostor", "software": theirs_id})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Nothing was created.
    let (_, list) = h.get(&format!("/api/v1/instances?software={theirs_id}")).await;
    assert_eq!(list["total"], 0, "{list}");
}

#[tokio::test]
async fn only_a_curator_may_issue_an_application_key() {
    let h = harness().await;
    let f = h.fixture().await;
    // The instance's own advertise token is a perfectly good credential, and still must not be
    // able to mint a standing permission to add records.
    let (status, body) = h
        .post(&format!("/api/v1/software/{}/tokens", f.software_id), &f.token, json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn a_curator_created_deployment_declares_where_to_probe_it() {
    let h = harness().await;
    let (_, sw) = h.post("/api/v1/software", ROOT, json!({"name": "svc", "kinds": ["service"]})).await;
    let software_id = sw["id"].as_str().unwrap().to_string();

    // Mode 1 unchanged: a curator creates the record, and may now say where health lives.
    let (status, inst) = h
        .post(
            "/api/v1/instances",
            ROOT,
            json!({
                "label": "svc prod",
                "software": software_id,
                "endpoint_url": "https://svc.example",
                "health_endpoint": "https://svc.example/healthz"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inst}");
    assert_eq!(inst["health_endpoint"], "https://svc.example/healthz");
    // Never probed yet is "unknown", which is a different fact from "down".
    assert_eq!(inst["health"], "unknown", "{inst}");

    // A caller cannot claim ownership of a record it did not self-register.
    let (status, patched, _) = h
        .req(
            "PATCH",
            &format!("/api/v1/instances/{}", inst["id"].as_str().unwrap()),
            Some(ROOT),
            Some(json!({"self_registered_by": "urn:tar:token:someone-else", "instance_key": "hijack"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert!(patched["self_registered_by"].is_null(), "{patched}");
    assert!(patched["instance_key"].is_null(), "{patched}");
}

#[tokio::test]
async fn sparql_stays_public_even_when_rest_reads_are_closed() {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    // The operator closed anonymous REST reads but left the query endpoint alone.
    config.public_read = false;
    let store = Arc::new(OxigraphStore::memory().unwrap());
    let ops = Ops::open(":memory:").await.unwrap();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();
    let h = Harness { app: tar::app(state.clone()), state };

    let (status, _) = h.get("/api/v1/software").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "REST reads are closed");

    let (status, body) = h.get("/sparql?query=ASK%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D").await;
    assert_eq!(status, StatusCode::OK, "SPARQL is public in its own right: {body}");
}

#[tokio::test]
async fn patching_software_changes_only_what_the_body_names() {
    let h = harness().await;
    let (status, sw) = h
        .post(
            "/api/v1/software",
            ROOT,
            json!({
                "name": "ontoexplorer",
                "tagline": "FAIR ontology repository",
                "kinds": ["service"],
                "license": "https://spdx.org/licenses/Apache-2.0",
                "keywords": ["ontology", "fair"]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let id = sw["id"].as_str().unwrap().to_string();

    // A PATCH naming one field must not require, or discard, all the others.
    let (status, patched, _) = h
        .req(
            "PATCH",
            &format!("/api/v1/software/{id}"),
            Some(ROOT),
            Some(json!({"api_docs": [{"url": "https://onto.example.org/openapi.json"}]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["api_docs"][0]["url"], "https://onto.example.org/openapi.json");
    assert_eq!(patched["name"], "ontoexplorer");
    assert_eq!(patched["tagline"], "FAIR ontology repository");
    assert_eq!(patched["license"], "https://spdx.org/licenses/Apache-2.0");
    assert_eq!(patched["keywords"].as_array().unwrap().len(), 2, "{patched}");
    assert_eq!(patched["kinds"][0], "service");

    // Clearing is still possible, and `null` is how you say it. Without this, a merging PATCH
    // would make emptying a field impossible — which is how the edit form clears one.
    let (status, cleared, _) = h
        .req(
            "PATCH",
            &format!("/api/v1/software/{id}"),
            Some(ROOT),
            Some(json!({"tagline": null, "license": null})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(cleared["tagline"].is_null(), "{cleared}");
    assert!(cleared["license"].is_null(), "{cleared}");
    assert_eq!(cleared["name"], "ontoexplorer", "clearing one field clears only that field");
    assert_eq!(cleared["api_docs"][0]["url"], "https://onto.example.org/openapi.json");
}
