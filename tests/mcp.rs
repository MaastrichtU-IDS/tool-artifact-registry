//! End-to-end tests for the hosted MCP server, against the real router.
//!
//! Two things are being checked here, and the second matters more than the first:
//!
//! 1. **Protocol conformance** to revision `2026-07-28` — the modern handshake, the mirrored
//!    header validation, version negotiation, the legacy `initialize` fallback, and the OAuth
//!    2.1 resource-server discovery an MCP client needs to authenticate on its own.
//! 2. **That the tools cannot do more than the credential could, and cannot invent
//!    vocabulary.** Those are the two ways a hosted, agent-driven write path damages a
//!    registry, and both are tested against the real SHACL shapes and the real EDAM index.

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

const BASE: &str = "https://reg.mcp.test";
const ROOT: &str = "test-root-token-0123456789";
const V: &str = "2026-07-28";

struct Harness {
    app: axum::Router,
    state: Arc<AppState>,
}

async fn harness() -> Harness {
    harness_with(true).await
}

/// A registry that does not serve anonymous reads. MCP follows that policy, so this is where
/// the OAuth challenge lives.
async fn closed_harness() -> Harness {
    harness_with(false).await
}

async fn harness_with(public_read: bool) -> Harness {
    let mut config = Config::for_test(BASE);
    config.public_read = public_read;
    config.root_token = Some(ROOT.into());
    config.oidc.issuer = Some("https://kc.test/realms/tar".into());
    config.oidc.client_id = Some("tar-ui".into());
    let store = Arc::new(OxigraphStore::memory().unwrap());
    let ops = Ops::open(":memory:").await.unwrap();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state.clone()), state }
}

impl Harness {
    async fn raw(&self, req: Request<Body>) -> (StatusCode, Value, axum::http::HeaderMap) {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).to_string()));
        (status, value, headers)
    }

    /// A well-formed `2026-07-28` request, with the mirrored headers a conforming client sends.
    async fn modern(&self, token: Option<&str>, id: Value, method: &str, params: Value) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", V)
            .header("mcp-method", method);
        if let Some(name) = params.get("name").and_then(Value::as_str) {
            b = b.header("mcp-name", name);
        }
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let (s, v, _) = self.raw(b.body(Body::from(body.to_string())).unwrap()).await;
        (s, v)
    }

    /// A `tools/call`, returning the `CallToolResult`.
    async fn call(&self, token: &str, name: &str, arguments: Value) -> Value {
        let (status, body) =
            self.modern(Some(token), json!(1), "tools/call", json!({ "name": name, "arguments": arguments })).await;
        assert_eq!(status, StatusCode::OK, "transport error for {name}: {body}");
        body["result"].clone()
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value, axum::http::HeaderMap) {
        self.raw(Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap()).await
    }

    /// Register a software and a deployment of it, and mint a token that *acts as* that
    /// deployment — the credential shape spec §8.3 requires for advertisement.
    async fn instance_token(&self, label: &str, scopes: Value) -> String {
        let sw = self.call(ROOT, "register_software", json!({ "name": label })).await;
        let sw_id = sw["structuredContent"]["id"].as_str().unwrap().to_string();
        let inst = self
            .call(ROOT, "register_instance", json!({ "label": format!("{label} prod"), "software": sw_id }))
            .await;
        let inst_id = inst["structuredContent"]["id"].as_str().unwrap().to_string();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/instances/{inst_id}/tokens"))
            .header("authorization", format!("Bearer {ROOT}"))
            .header("content-type", "application/json")
            .body(Body::from(json!({ "label": "ci", "scopes": scopes }).to_string()))
            .unwrap();
        let (status, tok, _) = self.raw(req).await;
        assert_eq!(status, StatusCode::CREATED, "{tok}");
        tok["token"].as_str().unwrap().to_string()
    }
}

fn text_of(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

fn is_error(result: &Value) -> bool {
    result["isError"].as_bool().unwrap_or(false)
}

// ============================================================ protocol conformance

#[tokio::test]
async fn discover_advertises_the_pinned_revision_and_needs_no_credential() {
    let h = harness().await;
    let (status, body) = h.modern(None, json!("d1"), "server/discover", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let r = &body["result"];
    assert_eq!(r["resultType"], "complete");
    assert_eq!(r["supportedVersions"][0], V, "the pinned revision must come first");
    assert!(r["capabilities"]["tools"].is_object());
    assert_eq!(r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "tool-artifact-registry");
    // The handshake is open, but it leaks nothing about the registry's contents.
    assert!(!body.to_string().contains("software\":"));
}

#[tokio::test]
async fn an_unknown_protocol_version_gets_the_supported_list_rather_than_a_bare_error() {
    let h = harness().await;
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "1999-01-01")
        .header("mcp-method", "tools/list")
        .body(Body::from(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string()))
        .unwrap();
    let (status, body, _) = h.raw(req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32022);
    assert_eq!(body["error"]["data"]["supported"][0], V);
    assert_eq!(body["error"]["data"]["requested"], "1999-01-01");
}

#[tokio::test]
async fn a_header_that_disagrees_with_the_body_is_refused() {
    let h = harness().await;
    // The security property: a gateway routing on `Mcp-Name` must not be able to disagree with
    // what the server executes.
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {ROOT}"))
        .header("mcp-protocol-version", V)
        .header("mcp-method", "tools/call")
        .header("mcp-name", "vocab_search")
        .body(Body::from(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": "register_software", "arguments": { "name": "x" } } })
            .to_string(),
        ))
        .unwrap();
    let (status, body, _) = h.raw(req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32020, "HeaderMismatch");
}

#[tokio::test]
async fn a_missing_mcp_method_header_is_refused_on_a_modern_request() {
    let h = harness().await;
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", V)
        .body(Body::from(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string()))
        .unwrap();
    let (status, body, _) = h.raw(req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32020);
}

#[tokio::test]
async fn a_notification_is_accepted_with_no_body() {
    let h = harness().await;
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string()))
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(resp.into_body().collect().await.unwrap().to_bytes().is_empty());
}

#[tokio::test]
async fn an_unimplemented_method_is_a_404_with_a_jsonrpc_error() {
    let h = harness().await;
    let (status, body) = h.modern(Some(ROOT), json!(9), "resources/list", json!({})).await;
    // The distinction the spec asks for: a modern server's 404 carries a JSON-RPC error, so a
    // dual-era client knows not to fall back to the deprecated HTTP+SSE transport.
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], -32601);
}

#[tokio::test]
async fn the_removed_get_stream_and_delete_teardown_answer_405() {
    let h = harness().await;
    for method in ["GET", "DELETE"] {
        let resp = h
            .app
            .clone()
            .oneshot(Request::builder().method(method).uri("/mcp").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "{method} /mcp");
    }
}

#[tokio::test]
async fn a_legacy_client_that_sends_initialize_is_served_too() {
    let h = harness().await;
    // Exactly what a 2025-11-25 client sends: no version header, an `initialize` handshake.
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize",
                    "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                                "clientInfo": { "name": "legacy", "version": "1" } } })
            .to_string(),
        ))
        .unwrap();
    let (status, body, _) = h.raw(req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
    assert!(body["result"]["capabilities"]["tools"].is_object());

    // …and its subsequent calls work, with no session id anywhere.
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2025-11-25")
        .header("authorization", format!("Bearer {ROOT}"))
        .body(Body::from(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string()))
        .unwrap();
    let (status, body, headers) = h.raw(req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body["result"]["tools"].as_array().unwrap().is_empty());
    // Legacy results must not carry the modern-only fields.
    assert!(body["result"].get("resultType").is_none());
    assert!(headers.get("mcp-session-id").is_none(), "this revision has no sessions");
}

// =========================================================== authorization discovery

#[tokio::test]
async fn an_unauthenticated_call_gets_a_401_naming_the_metadata_document() {
    let h = closed_harness().await;
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", V)
        .header("mcp-method", "tools/list")
        .body(Body::from(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string()))
        .unwrap();
    let (status, _body, headers) = h.raw(req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let challenge = headers.get("www-authenticate").unwrap().to_str().unwrap();
    assert!(challenge.starts_with("Bearer "), "{challenge}");
    assert!(
        challenge.contains(&format!("resource_metadata=\"{BASE}/.well-known/oauth-protected-resource\"")),
        "{challenge}"
    );
}

#[tokio::test]
async fn a_closed_registry_tells_an_unauthenticated_caller_nothing() {
    let h = closed_harness().await;
    for method in ["tools/list", "tools/call"] {
        let (status, body) = h.modern(None, json!(1), method, json!({ "name": "search_registry" })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method}");
        // No tool names, no counts, no record IRIs.
        assert!(body.get("result").is_none());
        assert!(!body.to_string().contains("vocab_search"));
    }
}

#[tokio::test]
async fn a_public_registry_lets_an_anonymous_agent_read_without_an_oauth_dance() {
    // Anonymous callers can already list software over REST and query it over SPARQL on this
    // registry. Refusing them the equivalent tools bought no secrecy and forced every client
    // into a sign-in it did not need — which is exactly where a misconfigured identity provider
    // turns "read the catalogue" into "cannot connect at all".
    let h = harness().await;

    let (status, body) = h.modern(None, json!(1), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: Vec<&str> =
        body["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"search_registry"), "{names:?}");
    assert!(names.contains(&"vocab_search"), "{names:?}");

    // But only what anonymous may do: the list is still filtered by authority.
    assert!(!names.contains(&"register_software"), "a write tool must not be offered: {names:?}");
    assert!(!names.contains(&"advertise_produced"), "{names:?}");

    // And a read tool actually works.
    let (status, body) = h
        .modern(None, json!(2), "tools/call", json!({ "name": "registry_info", "arguments": {} }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["result"]["isError"], serde_json::Value::Bool(true), "{body}");

    // Naming a write tool anyway is refused, not quietly executed.
    let (status, body) = h
        .modern(
            None,
            json!(3),
            "tools/call",
            json!({ "name": "register_software", "arguments": { "name": "sneaky" } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["isError"], serde_json::Value::Bool(true), "{body}");
}

#[tokio::test]
async fn a_bad_token_gets_the_same_challenge_rather_than_a_bare_rejection() {
    // A credential that was offered and rejected is a 401 whether reads are public or not:
    // the caller meant to authenticate and needs to know it failed.
    let h = harness().await;
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("content-type", "application/json")
        .header("authorization", "Bearer tar_not-a-real-token")
        .header("mcp-protocol-version", V)
        .header("mcp-method", "tools/list")
        .body(Body::from(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string()))
        .unwrap();
    let (status, _, headers) = h.raw(req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers.get("www-authenticate").unwrap().to_str().unwrap().contains("resource_metadata="));
}

#[tokio::test]
async fn protected_resource_metadata_is_served_at_both_well_known_locations() {
    let h = harness().await;

    let (status, doc, _) = h.get("/.well-known/oauth-protected-resource").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["resource"], BASE, "the root document names the registry itself");
    assert_eq!(doc["authorization_servers"][0], "https://kc.test/realms/tar");
    assert_eq!(doc["bearer_methods_supported"][0], "header");

    let (status, doc, _) = h.get("/.well-known/oauth-protected-resource/mcp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["resource"], format!("{BASE}/mcp"), "the path-inserted document names the endpoint");
    assert_eq!(doc["authorization_servers"][0], "https://kc.test/realms/tar");
}

// ================================================================= the tool surface

#[tokio::test]
async fn tools_are_listed_with_the_caching_metadata_this_revision_requires() {
    let h = harness().await;
    let (status, body) = h.modern(Some(ROOT), json!(1), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let r = &body["result"];
    assert_eq!(r["resultType"], "complete");
    assert!(r["ttlMs"].as_u64().unwrap() > 0);
    // Private: the set is filtered by the caller's authority, so a shared cache must not serve
    // one credential's list to another.
    assert_eq!(r["cacheScope"], "private");

    let names: Vec<&str> = r["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "registry_info", "vocab_search", "vocab_resolve", "list_enumerations", "register_artifact_type",
        "search_registry", "list_records", "get_record", "find_capable_software", "get_artifact_lineage",
        "register_software", "update_software", "add_release", "declare_capability", "register_instance",
    ] {
        assert!(names.contains(&expected), "{expected} missing from {names:?}");
    }
    // The root token is an admin and passes every scope check, but it is not a *deployment*, so
    // the two advertisement tools are correctly withheld from it — spec §8.3 takes the Instance
    // from the credential. `advertisement_writes_lineage_and_is_idempotent` shows them appearing
    // for a credential that does act as one.
    assert!(!names.contains(&"advertise_produced"), "{names:?}");
    assert!(!names.contains(&"advertise_consumed"), "{names:?}");
    // Every tool must carry a schema a client can validate against.
    for t in r["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert!(t["annotations"]["readOnlyHint"].is_boolean());
    }
}

#[tokio::test]
async fn no_tool_exposes_credential_minting_deletion_peering_or_raw_sparql() {
    let h = harness().await;
    let (_, body) = h.modern(Some(ROOT), json!(1), "tools/list", json!({})).await;
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    // These are person-operations, or too sharp for an agent with a network-reachable endpoint.
    // The check is on the whole catalogue rather than on one name, so adding one later trips it.
    for forbidden in ["token", "delete", "peer", "sparql", "revoke", "tombstone", "subscription", "openlineage"] {
        assert!(
            !names.iter().any(|n| n.contains(forbidden)),
            "the catalogue must not expose `{forbidden}`: {names:?}"
        );
    }
}

#[tokio::test]
async fn registry_info_tells_the_agent_what_it_may_do() {
    let h = harness().await;
    let r = h.call(ROOT, "registry_info", json!({})).await;
    assert!(!is_error(&r));
    assert!(r["structuredContent"]["available_tools"].as_array().unwrap().len() >= 15);
    assert!(text_of(&r).contains("Tools you may call"));
}

// ================================================== vocabulary: the anti-invention path

#[tokio::test]
async fn vocab_search_finds_real_terms_in_both_branches() {
    let h = harness().await;
    // `data` is EDAM: what an artifact conforms to.
    let r = h.call(ROOT, "vocab_search", json!({ "q": "sequence", "branch": "data" })).await;
    assert!(!is_error(&r), "{}", text_of(&r));
    let items = r["structuredContent"]["items"].as_array().unwrap();
    assert!(!items.is_empty(), "bundled EDAM should match 'sequence' in the data branch");
    assert!(items.iter().any(|i| i["iri"].as_str().unwrap().starts_with("http://edamontology.org/")));

    // `topic` is EuroSciVoc: what a piece of software is about.
    let r = h.call(ROOT, "vocab_search", json!({ "q": "software", "branch": "topic" })).await;
    let items = r["structuredContent"]["items"].as_array().unwrap();
    assert!(!items.is_empty(), "bundled EuroSciVoc should match 'software' in the topic branch");
    assert!(items.iter().all(|i| i["iri"].as_str().unwrap().starts_with("http")));
}

#[tokio::test]
async fn a_search_with_no_hits_says_omit_or_mint_rather_than_guess() {
    let h = harness().await;
    let r = h.call(ROOT, "vocab_search", json!({ "q": "zzqqxx-not-a-real-concept" })).await;
    let text = text_of(&r);
    assert!(text.contains("register_artifact_type"), "{text}");
    assert!(text.contains("Do not write an ontology IRI that did not come from this tool"), "{text}");
}

#[tokio::test]
async fn an_invented_edam_iri_is_refused_before_anything_is_written() {
    let h = harness().await;
    // The exact failure mode: a well-formed, entirely plausible, non-existent EDAM term.
    let r = h
        .call(
            ROOT,
            "register_software",
            json!({ "name": "invented-topics-tool", "topics": ["http://edamontology.org/topic_9999999"] }),
        )
        .await;
    assert!(is_error(&r), "an invented EDAM IRI must not be written: {}", text_of(&r));
    let text = text_of(&r);
    assert!(text.contains("topic_9999999"), "{text}");
    assert!(text.contains("vocab_search"), "the refusal must say how to recover: {text}");
    assert!(text.contains("Do not adjust the identifier and try again"), "{text}");

    // …and nothing was created.
    let (_, list, _) = h.get("/api/v1/software?q=invented-topics-tool").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn an_invented_artifact_type_is_refused_on_advertisement_too() {
    let h = harness().await;
    let token = h.instance_token("advertiser", json!(["advertise:produce"])).await;
    let r = h
        .call(
            &token,
            "advertise_produced",
            json!({
                "run": { "external_key": "ci/1" },
                "artifacts": [{ "title": "report", "conforms_to": "http://edamontology.org/data_8888888" }]
            }),
        )
        .await;
    assert!(is_error(&r), "{}", text_of(&r));
    assert!(text_of(&r).contains("data_8888888"), "{}", text_of(&r));

    // Nothing was written: no run, no artifact.
    let (_, runs, _) = h.get("/api/v1/runs").await;
    assert_eq!(runs["items"].as_array().unwrap().len(), 0);
}

/// Found by pointing a real coding agent at this server and telling it to guess: it produced
/// `edamontology.org/topic_3170`, which *exists* — it is EDAM's "RNA-Seq" — and an existence
/// check alone waved it onto a record with nothing to do with RNA-Seq. That branch is bundled
/// only for label resolution and `vocab_search` never returns it, so the rule has to be "could
/// `vocab_search` have returned this for this field", not "does this term exist".
#[tokio::test]
async fn a_real_term_in_the_wrong_branch_is_refused_as_firmly_as_an_invented_one() {
    let h = harness().await;

    // Verify the premise: the term is genuinely in the registry's vocabulary…
    let r = h.call(ROOT, "vocab_resolve", json!({ "iris": ["http://edamontology.org/topic_3170"] })).await;
    assert!(text_of(&r).contains("All resolved."), "{}", text_of(&r));
    // …and `vocab_search` with branch=topic will never return it.
    let r = h.call(ROOT, "vocab_search", json!({ "q": "RNA-Seq", "branch": "topic" })).await;
    let hits = r["structuredContent"]["items"].as_array().unwrap();
    assert!(
        !hits.iter().any(|i| i["iri"] == "http://edamontology.org/topic_3170"),
        "premise broken: the topic picker now offers EDAM topics"
    );

    // So writing it as a software topic must be refused.
    let r = h
        .call(
            ROOT,
            "register_software",
            json!({ "name": "wrong-branch-tool", "topics": ["http://edamontology.org/topic_3170"] }),
        )
        .await;
    assert!(is_error(&r), "a real term in the wrong branch must not be written: {}", text_of(&r));
    let text = text_of(&r);
    assert!(text.contains("topic_3170"), "the refusal must name the term it refused: {text}");
    assert!(
        text.contains("not one it classifies software by"),
        "the refusal must say what is wrong with the term, not merely that it is wrong: {text}"
    );
    assert!(text.contains("branch=topic"), "the refusal must say how to find a real one: {text}");
    // Several vocabularies are in play and more will follow, so no message may name one.
    assert!(
        !text.contains("EDAM") && !text.contains("EuroSciVoc"),
        "a user-facing message must not name a vocabulary: {text}"
    );

    let (_, list, _) = h.get("/api/v1/software?q=wrong-branch-tool").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_topic_used_as_an_artifact_type_is_refused_too() {
    let h = harness().await;
    let topics = h.call(ROOT, "vocab_search", json!({ "q": "software", "branch": "topic" })).await;
    let topic = topics["structuredContent"]["items"][0]["iri"].as_str().unwrap().to_string();

    // A real EuroSciVoc topic is a real term — but it is not a thing an artifact can be.
    let r = h
        .call(ROOT, "register_software", json!({ "name": "swapped", "capability": { "produces": [topic] } }))
        .await;
    assert!(is_error(&r), "{}", text_of(&r));
    assert!(
        text_of(&r).contains("cannot be what an artifact is"),
        "the refusal must say why a topic is not a type: {}",
        text_of(&r)
    );
    assert!(text_of(&r).contains("branch=data"), "{}", text_of(&r));
}

#[tokio::test]
async fn a_data_type_is_accepted_where_an_artifact_type_belongs() {
    let h = harness().await;
    let hits = h.call(ROOT, "vocab_search", json!({ "q": "sequence", "branch": "data" })).await;
    let data_type = hits["structuredContent"]["items"][0]["iri"].as_str().unwrap().to_string();
    let r = h
        .call(ROOT, "register_software", json!({ "name": "right-branch", "capability": { "produces": [data_type] } }))
        .await;
    assert!(!is_error(&r), "{}", text_of(&r));
}

/// The escape hatch, in both of its shapes. A minted type names something nothing else names; an
/// adopted one keeps the identifier the term already had, because two registries inventing two
/// IRIs for one concept is the duplication this whole rule exists to prevent, one level up.
#[tokio::test]
async fn a_type_is_usable_once_it_is_minted_or_adopted_and_not_before() {
    let h = harness().await;
    let r = h
        .call(
            ROOT,
            "register_artifact_type",
            json!({ "label": "Patch log", "slug": "patch-log", "definition": "A hash-chained patch log." }),
        )
        .await;
    assert!(!is_error(&r), "{}", text_of(&r));
    let minted = r["structuredContent"]["iri"].as_str().unwrap().to_string();
    assert!(minted.ends_with("/type/patch-log"), "{minted}");
    assert_eq!(r["structuredContent"]["adopted"], false, "this registry named it, so it is not adopted");

    let r = h.call(ROOT, "vocab_resolve", json!({ "iris": [minted.clone()] })).await;
    assert!(text_of(&r).contains("All resolved."), "{}", text_of(&r));

    // An IRI nobody here has heard of is refused — the whole point. Nothing is written.
    let foreign = "http://purl.obolibrary.org/obo/SWO_0000001";
    let r = h
        .call(
            ROOT,
            "register_software",
            json!({ "name": "foreign-type-user", "capability": { "produces": [foreign] } }),
        )
        .await;
    assert!(is_error(&r), "an unknown type IRI must not be written: {}", text_of(&r));
    assert!(text_of(&r).contains(foreign), "{}", text_of(&r));
    let (_, list, _) = h.get("/api/v1/software?q=foreign-type-user").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);

    // Adopt it, under its own identifier rather than an alias minted here…
    let r = h
        .call(
            ROOT,
            "register_artifact_type",
            json!({ "label": "Software suite", "iri": foreign, "scheme": "http://www.ebi.ac.uk/swo" }),
        )
        .await;
    assert!(!is_error(&r), "{}", text_of(&r));
    assert_eq!(
        r["structuredContent"]["iri"], foreign,
        "adoption must keep the term's own IRI, or two registries adopting it would disagree"
    );
    assert_eq!(r["structuredContent"]["adopted"], true);

    // …and the same write now goes through.
    let r = h
        .call(
            ROOT,
            "register_software",
            json!({ "name": "foreign-type-user", "capability": { "produces": [foreign] } }),
        )
        .await;
    assert!(!is_error(&r), "{}", text_of(&r));
}

#[tokio::test]
async fn the_enumerations_match_the_shapes_the_registry_validates_against() {
    let h = harness().await;
    let r = h.call(ROOT, "list_enumerations", json!({})).await;
    let e = &r["structuredContent"];
    assert!(e["software_kinds"]["values"]["workflow"].is_string());
    assert_eq!(e["run_status"]["values"], json!(["success", "failed", "running", "aborted"]));
    assert_eq!(
        e["availability"]["values"]["metadata-only"].as_str().unwrap().contains("not obtainable"),
        true
    );
    assert!(e["scopes"]["values"]["advertise:produce"].is_string());
    // These are the values SHACL will actually accept — proven by using one and by the
    // rejection test below.
    assert!(e["access_protocol"]["values"].as_array().unwrap().iter().any(|v| v == "s3"));
}

// ================================================== writes, and the correction loop

#[tokio::test]
async fn a_full_curation_flow_works_end_to_end() {
    let h = harness().await;

    // 1. Look the topic up rather than recalling it.
    let hits = h.call(ROOT, "vocab_search", json!({ "q": "software", "branch": "topic" })).await;
    let topic = hits["structuredContent"]["items"][0]["iri"]
        .as_str()
        .expect("the topic branch must return at least one term")
        .to_string();

    // 2. Register, with only fields a repository would actually state.
    let r = h
        .call(
            ROOT,
            "register_software",
            json!({
                "name": "peak-caller",
                "tagline": "Calls peaks in mass spectra.",
                "code_repository": "https://github.com/example/peak-caller",
                "license": "https://spdx.org/licenses/Apache-2.0",
                "kinds": ["cli", "library"],
                "topics": [topic],
            }),
        )
        .await;
    assert!(!is_error(&r), "{}", text_of(&r));
    let iri = r["structuredContent"]["iri"].as_str().unwrap().to_string();
    let id = iri.rsplit('/').next().unwrap().to_string();

    // 3. Read it back through the MCP surface.
    let r = h.call(ROOT, "get_record", json!({ "kind": "software", "id": iri })).await;
    assert_eq!(r["structuredContent"]["name"], "peak-caller");
    assert_eq!(r["structuredContent"]["kinds"], json!(["cli", "library"]));

    // 4. A release.
    let r = h.call(ROOT, "add_release", json!({ "software": id, "version": "1.2.0" })).await;
    assert!(!is_error(&r), "{}", text_of(&r));

    // 5. A capability, with a type minted because EDAM has none.
    let t = h.call(ROOT, "register_artifact_type", json!({ "label": "Peak list", "slug": "peak-list" })).await;
    let type_iri = t["structuredContent"]["iri"].as_str().unwrap().to_string();
    let r = h
        .call(ROOT, "declare_capability", json!({ "target": "software", "id": id, "produces": [type_iri.clone()] }))
        .await;
    assert!(!is_error(&r), "{}", text_of(&r));

    // 6. Matchmaking finds it.
    let r = h.call(ROOT, "find_capable_software", json!({ "produces": type_iri })).await;
    assert!(!is_error(&r), "{}", text_of(&r));
    assert!(text_of(&r).contains("match"), "{}", text_of(&r));

    // 7. And a deployment.
    let r = h
        .call(
            ROOT,
            "register_instance",
            json!({ "label": "peak-caller at UM", "software": id, "endpoint_url": "https://peaks.um.example" }),
        )
        .await;
    assert!(!is_error(&r), "{}", text_of(&r));
}

#[tokio::test]
async fn a_shacl_rejection_comes_back_as_an_actionable_correction() {
    let h = harness().await;
    let r = h
        .call(ROOT, "register_software", json!({ "name": "bad-kind-tool", "kinds": ["banana"] }))
        .await;
    assert!(is_error(&r));
    let text = text_of(&r);
    // The registry names the offending JSON field via `tar:jsonField`; that has to survive
    // into the message, because it is what makes the retry a correction rather than a re-guess.
    assert!(text.contains("kind"), "the offending field must be named: {text}");
    assert!(text.contains("list_enumerations"), "{text}");
    assert!(text.contains("remove it from the request"), "{text}");

    // The correction succeeds.
    let r = h.call(ROOT, "register_software", json!({ "name": "bad-kind-tool", "kinds": ["cli"] })).await;
    assert!(!is_error(&r), "{}", text_of(&r));
}

#[tokio::test]
async fn advertisement_writes_lineage_and_is_idempotent() {
    let h = harness().await;
    // A deployment, and a token that acts as it — the credential shape the advertise tools need.
    let token = h.instance_token("shacl-manager", json!(["advertise:produce", "advertise:consume"])).await;

    // That token sees only what it may do.
    let (_, body) = h.modern(Some(&token), json!(1), "tools/list", json!({})).await;
    let names: Vec<&str> =
        body["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"advertise_produced"));
    assert!(names.contains(&"advertise_consumed"));
    assert!(!names.contains(&"register_software"), "an instance token is not a curator: {names:?}");

    let advert = json!({
        "run": { "external_key": "gh-actions/12345/attempt-1", "status": "success" },
        "artifacts": [{
            "title": "Validation report",
            "distributions": [{ "download_url": "https://x.example/r.ttl", "media_type": "text/turtle", "access_protocol": "https" }]
        }]
    });
    let r = h.call(&token, "advertise_produced", advert.clone()).await;
    assert!(!is_error(&r), "{}", text_of(&r));
    let run = r["structuredContent"]["run"].as_str().unwrap().to_string();

    // Idempotent on the run key: the retried CI step attaches to the same run.
    let r2 = h.call(&token, "advertise_produced", advert).await;
    assert!(!is_error(&r2), "{}", text_of(&r2));
    assert_eq!(r2["structuredContent"]["run"].as_str().unwrap(), run);

    // Lineage is readable back through the tools.
    let artifact = r["structuredContent"]["artifacts"][0].as_str().unwrap().to_string();
    let l = h.call(&token, "get_artifact_lineage", json!({ "id": artifact, "direction": "up" })).await;
    assert!(!is_error(&l), "{}", text_of(&l));
}

// ============================================ authorization parity with the REST API

#[tokio::test]
async fn a_tool_can_do_no_more_than_the_same_credential_could_over_rest() {
    let h = harness().await;
    // A token with only `advertise:produce` — no curation authority of any kind.
    let token = h.instance_token("gated", json!(["advertise:produce"])).await;

    // Over REST: refused.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/software")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({ "name": "sneaky" }).to_string()))
        .unwrap();
    let (rest_status, _, _) = h.raw(req).await;
    assert_eq!(rest_status, StatusCode::FORBIDDEN);

    // Over MCP: refused too, with a reason rather than a stack trace, and nothing written.
    let r = h.call(&token, "register_software", json!({ "name": "sneaky" })).await;
    assert!(is_error(&r));
    assert!(text_of(&r).contains("curator"), "{}", text_of(&r));

    let (_, list, _) = h.get("/api/v1/software?q=sneaky").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0, "nothing may be written by a refused call");

    // Not even `advertise:consume`, which it does not hold.
    let r = h
        .call(&token, "advertise_consumed", json!({ "run": { "external_key": "k" }, "artifacts": [] }))
        .await;
    assert!(is_error(&r));
    assert!(text_of(&r).contains("advertise:consume"), "{}", text_of(&r));
}

#[tokio::test]
async fn a_credential_that_is_not_a_deployment_cannot_advertise() {
    let h = harness().await;
    // The root token is an admin, so it passes every scope check — but it does not *act as* an
    // Instance, and spec §8.3 says the Instance comes from the credential, never the body.
    let r = h
        .call(ROOT, "advertise_produced", json!({ "run": { "external_key": "k" }, "artifacts": [] }))
        .await;
    assert!(is_error(&r), "{}", text_of(&r));
    assert!(text_of(&r).contains("Instance"), "{}", text_of(&r));
}

#[tokio::test]
async fn an_unknown_tool_name_is_a_tool_error_not_a_protocol_error() {
    let h = harness().await;
    // A model that hallucinates a tool name should be able to recover from the answer.
    let r = h.call(ROOT, "delete_everything", json!({})).await;
    assert!(is_error(&r));
    assert!(text_of(&r).contains("tools/list"), "{}", text_of(&r));
}

#[tokio::test]
async fn the_registry_state_is_untouched_by_read_tools() {
    let h = harness().await;
    let before = h.state.store.count().unwrap();
    for (name, args) in [
        ("registry_info", json!({})),
        ("search_registry", json!({ "q": "anything" })),
        ("list_records", json!({ "kind": "software" })),
        ("list_enumerations", json!({})),
        ("vocab_search", json!({ "q": "sequence" })),
    ] {
        let r = h.call(ROOT, name, args).await;
        assert!(!is_error(&r), "{name}: {}", text_of(&r));
    }
    assert_eq!(h.state.store.count().unwrap(), before);
}

#[tokio::test]
async fn a_search_that_finds_something_says_so_and_names_it() {
    // This counted the wrong field and answered "0 hit(s)" on every successful search. A model
    // reads that first line as "nothing here" and goes off to register a duplicate of the
    // record it had just found.
    let h = harness().await;
    let created = h
        .call(ROOT, "register_software", json!({ "name": "shacl-manager", "tagline": "SHACL shape management" }))
        .await;
    assert_ne!(created["isError"], json!(true), "{created}");

    let r = h.call(ROOT, "search_registry", json!({ "q": "shacl" })).await;
    let text = text_of(&r);
    assert!(text.starts_with("1 hit(s)."), "{text}");
    // The title is in the summary, so acting on the result costs no second call.
    assert!(text.contains("shacl-manager"), "{text}");
    assert!(text.contains("/software/"), "the IRI is named too: {text}");

    // And a genuine miss still says so, without inviting invention.
    let empty = h.call(ROOT, "search_registry", json!({ "q": "zzqqxx-nothing-like-this" })).await;
    let text = text_of(&empty);
    assert!(text.starts_with("0 hit(s)."), "{text}");
    assert!(text.contains("has to be registered"), "{text}");
}

#[tokio::test]
async fn a_listing_summarises_rather_than_returning_whole_records() {
    // Four software records with READMEs came to 112 KB and overran a client's tool-output
    // limit, so browsing a four-record catalogue failed outright. A listing is for choosing;
    // `get_record` is for reading.
    let h = harness().await;
    let readme = "# Big\n".repeat(4000);
    let r = h
        .call(
            ROOT,
            "register_software",
            json!({
                "name": "verbose-tool",
                "tagline": "A short line",
                "readme": readme,
                "kinds": ["service"],
            }),
        )
        .await;
    assert_ne!(r["isError"], json!(true), "{r}");

    let listing = h.call(ROOT, "list_records", json!({ "kind": "software" })).await;
    let body = serde_json::to_string(&listing).unwrap();
    assert!(!body.contains("# Big"), "the README must not travel in a listing");
    assert!(body.len() < 8_000, "a listing of one record should be small, was {}", body.len());

    let item = &listing["structuredContent"]["items"][0];
    // Still enough to choose one and then fetch it.
    assert_eq!(item["name"], "verbose-tool");
    assert_eq!(item["tagline"], "A short line");
    assert!(item["iri"].as_str().unwrap().contains("/software/"));
    assert!(item.get("readme").is_none(), "{item}");

    // And the whole record is one call away.
    let full = h.call(ROOT, "get_record", json!({ "kind": "software", "id": item["id"] })).await;
    assert!(serde_json::to_string(&full).unwrap().contains("# Big"), "get_record returns it all");
}

#[tokio::test]
async fn a_listing_clips_a_long_description_instead_of_dropping_it() {
    let h = harness().await;
    let long = "x".repeat(900);
    let r = h
        .call(ROOT, "register_software", json!({ "name": "wordy", "description": long }))
        .await;
    assert_ne!(r["isError"], json!(true), "{r}");
    let listing = h.call(ROOT, "list_records", json!({ "kind": "software" })).await;
    let tagline = listing["structuredContent"]["items"][0]["tagline"].as_str().unwrap();
    assert!(tagline.ends_with('…'), "{tagline}");
    assert!(tagline.chars().count() < 250, "{}", tagline.chars().count());
}
