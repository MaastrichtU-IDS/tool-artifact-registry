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

// ------------------------------------------------------------- outbound fetches

/// A local server that counts every request by path, so a test can prove what the registry
/// did and did not fetch.
async fn spawn_counting_server(extra: axum::Router<()>) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (wk, log) = (base.clone(), hits.clone());
    let app = axum::Router::new()
        .route(
            "/.well-known/tar-registry",
            axum::routing::get(move || {
                let wk = wk.clone();
                async move { axum::Json(json!({"base_iri": wk, "title": "peer", "peers": []})) }
            }),
        )
        .merge(extra)
        .layer(axum::middleware::from_fn(move |req: axum::extract::Request, next: axum::middleware::Next| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(req.uri().path().to_string());
                next.run(req).await
            }
        }));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, hits)
}

/// §1.2: `/resolve` made the registry fetch any URL for anyone, and cache what came back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_needs_a_credential_and_fetches_only_a_peers_records() {
    let h = harness().await;
    let (elsewhere, hits) = spawn_counting_server(axum::Router::new()).await;
    let target = format!("{elsewhere}/internal/secret");
    let uri = format!("/api/v1/resolve?iri={}&refresh=true", enc(&target));

    let (status, body, _) = h.send("GET", &uri, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, body, _) = h.send("GET", &uri, Some(ROOT), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "not a peer's IRI: {body}");
    assert!(hits.lock().unwrap().is_empty(), "nothing was fetched: {:?}", hits.lock().unwrap());
}

/// §1.3: a peer answering with a redirect is not followed there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_redirect_from_a_peer_is_not_followed() {
    let h = harness().await;
    let routes = axum::Router::new()
        .route("/artifact/1", axum::routing::get(|| async { axum::response::Redirect::to("/secret") }))
        .route("/secret", axum::routing::get(|| async { "<https://x/secret> <https://x/p> \"leaked\" ." }));
    let (peer, hits) = spawn_counting_server(routes).await;
    let (status, body, _) =
        h.send("POST", "/api/v1/peers", Some(ROOT), Some(json!({"base_url": peer, "announce": false}))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let uri = format!("/api/v1/resolve?iri={}&refresh=true", enc(&format!("{peer}/artifact/1")));
    let (status, body, _) = h.send("GET", &uri, Some(ROOT), None).await;
    assert!(!status.is_success(), "a 302 is not a record: {status} {body}");
    assert!(!hits.lock().unwrap().iter().any(|p| p == "/secret"), "{:?}", hits.lock().unwrap());
    assert!(!h.state.store.dump_nquads(None).unwrap().contains("leaked"));
}

/// §1.3: the API-description viewer returns what it fetched to anonymous readers, so it does
/// not fetch private addresses unless the operator opts in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_api_doc_viewer_does_not_fetch_a_private_address_by_default() {
    let h = harness().await;
    let (internal, hits) = spawn_counting_server(axum::Router::new()).await;
    let (status, sw, _) = h
        .send(
            "POST",
            "/api/v1/software",
            Some(ROOT),
            Some(json!({"name": "svc", "kinds": ["service"],
                          "api_docs": [{"url": format!("{internal}/openapi.json"), "format": "openapi"}]})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let (status, body) = h.get(&format!("/api/v1/software/{}/api-doc", sw["id"].as_str().unwrap())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["detail"].as_str().unwrap().contains("TAR_APIDOC_ALLOW_PRIVATE"), "{body}");
    assert!(hits.lock().unwrap().is_empty(), "{:?}", hits.lock().unwrap());
}

/// §1.5: nothing bounded what a script that got into the page could do.
#[tokio::test]
async fn pages_carry_a_content_security_policy_and_every_response_the_basics() {
    let h = harness_with(|c| c.oidc.issuer = Some("https://sso.example.org/realms/main".into())).await;
    let (status, _, headers) = h.send("GET", "/software", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let csp = headers["content-security-policy"].to_str().unwrap();
    for part in [
        "script-src 'self'",
        "frame-ancestors 'none'",
        "object-src 'none'",
        "connect-src 'self' https://sso.example.org",
    ] {
        assert!(csp.contains(part), "{part} missing from {csp}");
    }
    assert_eq!(headers["x-frame-options"], "DENY");

    let (_, _, headers) = h.send("GET", "/api/v1/software", None, None).await;
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["referrer-policy"], "strict-origin-when-cross-origin");
    assert!(!headers.contains_key("content-security-policy"), "JSON needs no page policy");
}

// ---------------------------------------------------------------------- scopes

async fn deployment(h: &Harness, allowed: &[&str]) -> String {
    let (_, sw, _) =
        h.send("POST", "/api/v1/software", Some(ROOT), Some(json!({"name": "svc", "kinds": ["service"]}))).await;
    let (status, inst, _) = h
        .send(
            "POST",
            "/api/v1/instances",
            Some(ROOT),
            Some(json!({"label": "d", "software": sw["id"], "allowed_scopes": allowed})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inst}");
    inst["id"].as_str().unwrap().to_string()
}

async fn mint(h: &Harness, as_: &str, instance: &str, scopes: &[&str]) -> (StatusCode, Value) {
    let (s, v, _) = h
        .send("POST", &format!("/api/v1/instances/{instance}/tokens"), Some(as_), Some(json!({"scopes": scopes})))
        .await;
    (s, v)
}

/// §1.6: a deployment's credential could mint itself wider scopes, and `allowed_scopes`
/// bounded nothing for API tokens.
#[tokio::test]
async fn a_minted_token_stays_within_the_deployment_and_the_minter() {
    let h = harness().await;
    let id = deployment(&h, &["advertise:produce", "advertise:consume"]).await;

    // The deployment's ceiling holds even for the root token.
    let (status, body) = mint(&h, ROOT, &id, &["register:software"]).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = mint(&h, ROOT, &id, &["advertise:produce"]).await;
    assert_eq!(status, StatusCode::CREATED);
    // subscribe:artifacts grants less than any of the deployment's credentials already has.
    let (status, body) = mint(&h, ROOT, &id, &["subscribe:artifacts"]).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // A deployment token cannot mint what it does not hold, even inside the ceiling.
    let (_, narrow) = mint(&h, ROOT, &id, &["advertise:produce"]).await;
    let narrow = narrow["token"].as_str().unwrap();
    let (status, body) = mint(&h, narrow, &id, &["advertise:consume"]).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = mint(&h, narrow, &id, &["advertise:produce"]).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// §1.6: `register:software` passed the curator check, so a token for one project's CI could
/// edit or withdraw every deployment.
#[tokio::test]
async fn register_software_maintains_software_and_nothing_more() {
    let h = harness().await;
    let other = deployment(&h, &[]).await;
    // A software-scoped credential, as an admin would issue to a project's CI.
    let (_, sw, _) =
        h.send("POST", "/api/v1/software", Some(ROOT), Some(json!({"name": "ci-owner", "kinds": ["service"]}))).await;
    let ci_host = deployment(&h, &["register:software"]).await;
    let (status, tok) = mint(&h, ROOT, &ci_host, &["register:software"]).await;
    assert_eq!(status, StatusCode::CREATED, "{tok}");
    let ci = tok["token"].as_str().unwrap();

    let (status, body, _) =
        h.send("POST", "/api/v1/software", Some(ci), Some(json!({"name": "from-ci", "kinds": ["cli"]}))).await;
    assert_eq!(status, StatusCode::CREATED, "it still registers software: {body}");
    let sw_id = sw["id"].as_str().unwrap();
    let (status, body, _) =
        h.send("PATCH", &format!("/api/v1/software/{sw_id}"), Some(ci), Some(json!({"tagline": "edited"}))).await;
    assert_eq!(status, StatusCode::OK, "and maintains it: {body}");

    for (method, uri, body) in [
        ("PATCH", format!("/api/v1/instances/{other}"), Some(json!({"label": "hijacked"}))),
        ("DELETE", format!("/api/v1/instances/{other}"), None),
        ("DELETE", format!("/api/v1/software/{sw_id}"), None),
    ] {
        let (status, resp, _) = h.send(method, &uri, Some(ci), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {resp}");
    }
}

const HS_SECRET: &[u8] = b"a-test-signing-secret-not-used-in-production";
const HOME: &str = "https://keycloak.test.example/realms/ids";
const PARTNER: &str = "https://partner.example/realms/theirs";

fn jwt(claims: Value) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
    header.kid = Some("test-key".into());
    jsonwebtoken::encode(&header, &claims, &jsonwebtoken::EncodingKey::from_secret(HS_SECRET)).unwrap()
}

async fn oidc_harness() -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    config.oidc.issuer = Some(HOME.into());
    config.oidc.client_id = Some("tar-ui".into());
    config.oidc.audience = Some(BASE.into());
    config.oidc.workload_issuers = vec![PARTNER.into()];
    let store = common::test_store().await;
    let ops = Ops::open(":memory:").await.unwrap();
    let mut state = AppState::from_parts(config, store, ops);
    state.jwt = state.jwt.with_static_key(
        "test-key",
        jsonwebtoken::Algorithm::HS256,
        jsonwebtoken::DecodingKey::from_secret(HS_SECRET),
    );
    let state = Arc::new(state);
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state.clone()), state }
}

/// §1.6: scopes in a token were honoured from any accepted issuer, `admin:*` included.
#[tokio::test]
async fn a_token_cannot_make_itself_an_administrator() {
    let h = oidc_harness().await;
    let exp = chrono::Utc::now().timestamp() + 600;
    for issuer in [HOME, PARTNER] {
        let t = jwt(
            json!({"iss": issuer, "aud": BASE, "exp": exp, "sub": "svc", "azp": "some-client", "scope": "admin:*"}),
        );
        let (status, me, _) = h.send("GET", "/api/v1/whoami", Some(&t), None).await;
        assert_eq!(status, StatusCode::OK, "{me}");
        assert_eq!(me["is_admin"], false, "admin:* from {issuer}: {me}");
        let (status, _, _) = h.send("GET", "/admin/dump", Some(&t), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{issuer}");
    }

    // A bound deployment's token is capped by its allowed scopes.
    let id = deployment(&h, &["advertise:produce"]).await;
    let (status, body, _) = h
        .send("PATCH", &format!("/api/v1/instances/{id}"), Some(ROOT), Some(json!({"oidc_client_id": "bound-client"})))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let t = jwt(json!({"iss": HOME, "aud": BASE, "exp": exp, "sub": "svc", "azp": "bound-client",
                       "scope": "advertise:produce advertise:consume register:software"}));
    let (_, me, _) = h.send("GET", "/api/v1/whoami", Some(&t), None).await;
    assert_eq!(me["scopes"], json!(["advertise:produce"]), "{me}");
}
