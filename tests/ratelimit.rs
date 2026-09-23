//! Rate limiting against the real router (design note `docs/specs/2026-09-23-rate-limiting.md`).
//!
//! Each request carries a `ConnectInfo`, as `axum::serve` gives a real one, so the tests choose
//! the caller's address.

mod common;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tar::config::Config;
use tar::ops::Ops;
use tar::ratelimit::{Cidr, Class, ClassLimits, Limit, RateLimitConfig};
use tar::AppState;
use tower::ServiceExt;

const BASE: &str = "http://reg.test";
const ROOT: &str = "test-root-token-ratelimit-0123";
const V: &str = "2026-07-28";
const ASK: &str = "/sparql?query=ASK%20%7B%7D";
const TOO_MANY: StatusCode = StatusCode::TOO_MANY_REQUESTS;

struct Harness {
    app: axum::Router,
}

async fn harness(tune: impl FnOnce(&mut RateLimitConfig)) -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    let mut rl = RateLimitConfig::default();
    tune(&mut rl);
    config.rate_limit = rl;
    let store = common::test_store().await;
    let ops = Ops::open(":memory:").await.unwrap();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state) }
}

fn only(anon: Limit) -> ClassLimits {
    ClassLimits { anon: Some(anon), authed: None }
}

impl Harness {
    async fn send(
        &self,
        from: &str,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value, HeaderMap) {
        let mut b = Request::builder().method(method).uri(uri);
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept")) {
            b = b.header("accept", "application/json");
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let mut req = match body {
            Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(from.parse().unwrap(), 40000)));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, value, headers)
    }

    async fn get(&self, from: &str, uri: &str) -> StatusCode {
        self.send(from, "GET", uri, None, None, &[]).await.0
    }

    /// A registry token that is not an admin's: minted for a software record by root.
    async fn software_token(&self) -> String {
        let (s, sw, _) = self
            .send(
                "192.0.2.1",
                "POST",
                "/api/v1/software",
                Some(ROOT),
                Some(json!({"name": "rl-tool", "kinds": ["service"]})),
                &[],
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{sw}");
        let uri = format!("/api/v1/software/{}/tokens", sw["id"].as_str().unwrap());
        let (s, minted, _) = self.send("192.0.2.1", "POST", &uri, Some(ROOT), Some(json!({})), &[]).await;
        assert_eq!(s, StatusCode::CREATED, "{minted}");
        minted["token"].as_str().unwrap().to_string()
    }
}

#[tokio::test]
async fn an_anonymous_sparql_burst_is_refused_with_a_retry_after_and_other_addresses_are_not() {
    let h = harness(|rl| rl.set(Class::Sparql, only(Limit::new(1, 2)))).await;
    for _ in 0..2 {
        let (s, body, headers) = h.send("198.51.100.7", "GET", ASK, None, None, &[]).await;
        assert_ne!(s, TOO_MANY, "{body}");
        assert!(headers.contains_key("ratelimit-policy"), "{headers:?}");
        assert!(headers.contains_key("ratelimit"), "{headers:?}");
    }
    let (s, body, headers) = h.send("198.51.100.7", "GET", ASK, None, None, &[]).await;
    assert_eq!(s, TOO_MANY, "{body}");
    assert_eq!(headers["content-type"], "application/problem+json");
    let retry: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((50..=60).contains(&retry), "{retry}");
    assert_eq!(body["type"], "https://w3id.org/tar/problem/rate-limited");
    assert!(body["detail"].as_str().unwrap().contains("sparql limit for anonymous clients"), "{body}");

    assert_ne!(h.get("198.51.100.8", ASK).await, TOO_MANY, "another address has its own bucket");
    assert_eq!(h.get("198.51.100.7", "/api/v1/software").await, StatusCode::OK, "another class is untouched");

    let (_, metrics, _) = h.send("198.51.100.7", "GET", "/metrics", None, None, &[]).await;
    assert!(metrics.as_str().unwrap().contains("tar_ratelimit_rejections_total{class=\"sparql\"} 1"), "{metrics}");
}

#[tokio::test]
async fn an_authenticated_caller_is_charged_to_itself_not_to_its_address() {
    let h =
        harness(|rl| rl.set(Class::Read, ClassLimits { anon: Some(Limit::new(1, 1)), authed: Some(Limit::new(1, 3)) }))
            .await;
    let token = h.software_token().await;
    let from = "198.51.100.7";
    assert_eq!(h.get(from, "/api/v1/software").await, StatusCode::OK);
    assert_eq!(h.get(from, "/api/v1/software").await, TOO_MANY, "the address has spent its one");
    for i in 0..3 {
        let (s, body, _) = h.send(from, "GET", "/api/v1/software", Some(&token), None, &[]).await;
        assert_eq!(s, StatusCode::OK, "request {i} with the credential: {body}");
    }
    // The credential's bucket follows it to another address.
    let (s, _, _) = h.send("203.0.113.5", "GET", "/api/v1/software", Some(&token), None, &[]).await;
    assert_eq!(s, TOO_MANY);
}

#[tokio::test]
async fn the_root_token_is_never_limited() {
    let h = harness(|rl| {
        rl.set(Class::Sparql, ClassLimits { anon: Some(Limit::new(1, 1)), authed: Some(Limit::new(1, 1)) })
    })
    .await;
    for i in 0..5 {
        let (s, body, _) = h.send("198.51.100.7", "GET", ASK, Some(ROOT), None, &[]).await;
        assert_ne!(s, TOO_MANY, "request {i}: {body}");
    }
}

#[tokio::test]
async fn random_credentials_from_one_address_are_refused_before_they_are_checked() {
    let h = harness(|rl| rl.auth_fail = Some(Limit::new(1, 2))).await;
    let from = "198.51.100.7";
    // Two failures fit the burst; the third is refused by the bucket, which blocks the address.
    for i in 0..3 {
        let (s, body, _) = h.send(from, "GET", "/api/v1/whoami", Some(&format!("tar_guess{i}_x")), None, &[]).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "attempt {i}: {body}");
    }
    let (s, body, headers) = h.send(from, "GET", "/api/v1/whoami", Some("tar_guess9_x"), None, &[]).await;
    assert_eq!(s, TOO_MANY, "{body}");
    assert!(headers.contains_key("retry-after"));
    assert!(body["detail"].as_str().unwrap().contains("authentication"), "{body}");
    // Reading without a credential is still fine from there, and other addresses may still try.
    assert_eq!(h.get(from, "/api/v1/software").await, StatusCode::OK);
    let (s, _, _) = h.send("198.51.100.8", "GET", "/api/v1/whoami", Some("tar_guess_x"), None, &[]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn probes_are_never_limited() {
    let h = harness(|rl| rl.set(Class::Read, only(Limit::new(1, 1)))).await;
    let from = "198.51.100.7";
    assert_eq!(h.get(from, "/api/v1/software").await, StatusCode::OK);
    assert_eq!(h.get(from, "/api/v1/software").await, TOO_MANY);
    for path in ["/healthz", "/readyz", "/metrics"] {
        for _ in 0..3 {
            assert_ne!(h.get(from, path).await, TOO_MANY, "{path}");
        }
    }
}

#[tokio::test]
async fn behind_a_trusted_proxy_the_forwarded_client_is_charged_and_a_forged_header_is_not() {
    let h = harness(|rl| {
        rl.set(Class::Read, only(Limit::new(1, 1)));
        rl.trusted_proxies = vec![Cidr::parse("10.0.0.0/8").unwrap()];
    })
    .await;
    let via = |client: &'static str| [("x-forwarded-for", client)];
    let read = |from: &'static str, xff: &'static str| {
        let h = &h;
        async move { h.send(from, "GET", "/api/v1/software", None, None, &via(xff)).await.0 }
    };
    assert_eq!(read("10.0.0.2", "198.51.100.7").await, StatusCode::OK);
    assert_eq!(read("10.0.0.2", "198.51.100.7").await, TOO_MANY);
    assert_eq!(read("10.0.0.2", "198.51.100.8").await, StatusCode::OK, "a different client behind the same proxy");
    // An untrusted caller forging the header is charged to its own address.
    assert_eq!(read("203.0.113.5", "198.51.100.9").await, StatusCode::OK);
    assert_eq!(read("203.0.113.5", "198.51.100.10").await, TOO_MANY);
}

async fn mcp_call(h: &Harness, from: &str, name: &str, arguments: Value) -> (StatusCode, Value) {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
    let headers = [
        ("accept", "application/json, text/event-stream"),
        ("mcp-protocol-version", V),
        ("mcp-method", "tools/call"),
        ("mcp-name", name),
    ];
    let (s, v, _) = h.send(from, "POST", "/mcp", None, Some(body), &headers).await;
    (s, v)
}

/// Review focus: a refused inner request is a tool error inside a `200`, not a transport failure.
#[tokio::test]
async fn a_tool_call_spends_the_bucket_of_the_request_it_makes() {
    let h = harness(|rl| rl.set(Class::Read, only(Limit::new(1, 2)))).await;
    let from = "198.51.100.7";
    for i in 0..2 {
        let (s, body) = mcp_call(&h, from, "list_records", json!({ "kind": "software" })).await;
        assert_eq!(s, StatusCode::OK, "call {i}: {body}");
        assert_eq!(body["result"]["isError"], false, "call {i}: {body}");
    }
    // The two inner reads were charged to this address, so a direct read is refused...
    assert_eq!(h.get(from, "/api/v1/software").await, TOO_MANY);
    // ...and a third tool call's read comes back as a tool error, not a transport one.
    let (s, body) = mcp_call(&h, from, "list_records", json!({ "kind": "software" })).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["isError"], true, "{body}");
    // Another address is unaffected by any of it.
    assert_eq!(h.get("198.51.100.8", "/api/v1/software").await, StatusCode::OK);
}
