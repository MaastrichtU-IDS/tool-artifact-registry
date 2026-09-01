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
use tower::ServiceExt;

mod common;

const BASE: &str = "https://reg.test.example";
const ROOT: &str = "test-root-token-0123456789";

struct Harness {
    app: axum::Router,
    state: Arc<AppState>,
}

async fn harness() -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    let store = common::test_store().await;
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

// ------------------------------------------------- who produced an artifact, with no run

#[tokio::test]
async fn an_artifact_with_no_run_can_still_say_what_made_it_and_for_whom() {
    // Normally the run answers this — artifact → run → deployment → software — and answers it
    // better, because the registry built that chain from the credential. An artifact registered
    // by hand has no run, so the chain has no first link and the question had no answer at all.
    let h = harness().await;
    let (status, art) = h
        .req(
            "POST",
            "/api/v1/artifacts",
            Some(ROOT),
            Some(json!({
                "title": "An export from something that will never advertise a run",
                "produced_by": {
                    "name": "batch-exporter",
                    "kind": "software",
                    "version": "3.2.1",
                    "homepage": "https://example.org/batch-exporter"
                },
                "produced_by_user": {
                    "name": "A Researcher",
                    "kind": "person",
                    "identifier": "https://orcid.org/0000-0002-1825-0097",
                    "email": "researcher@example.org"
                }
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{art}");
    let id = art["id"].as_str().unwrap().to_string();

    let (_, back) = h.req("GET", &format!("/api/v1/artifacts/{id}"), None, None).await;
    assert_eq!(back["produced_by"]["name"], "batch-exporter", "{back}");
    assert_eq!(back["produced_by"]["kind"], "software");
    assert_eq!(back["produced_by"]["version"], "3.2.1", "a system is only reproducible with one");
    assert_eq!(back["produced_by_user"]["name"], "A Researcher");
    assert_eq!(back["produced_by_user"]["email"], "researcher@example.org");
    // An ORCID becomes the agent's own identity, so the same researcher is one node everywhere.
    assert_eq!(back["produced_by_user"]["iri"], "https://orcid.org/0000-0002-1825-0097");

    // The registry's own attribution is untouched and still single — that is the whole reason
    // these went on qualified attributions instead of `prov:wasAttributedTo`.
    assert_eq!(back["attributed_to"], "urn:tar:root", "{back}");

    // The system acted for the person, and PROV says so. Asserted between the *same* two nodes
    // the attributions point at: building an agent twice mints two IRIs, and the delegation
    // silently landed on an orphan nobody referenced.
    let system = back["produced_by"]["iri"].as_str().unwrap();
    let user = back["produced_by_user"]["iri"].as_str().unwrap();
    let rows = h
        .state
        .store
        .select(&format!(
            "PREFIX prov: <http://www.w3.org/ns/prov#>
             SELECT ?o WHERE {{ GRAPH ?g {{ <{system}> prov:actedOnBehalfOf ?o }} }}"
        ))
        .unwrap();
    let behalf: Vec<String> = rows.rows.iter().filter_map(|r| r.iri("o")).collect();
    assert_eq!(behalf, vec![user.to_string()], "the delegation must join the two agents");

    // And no orphan agent was minted along the way.
    let agents = h
        .state
        .store
        .select(
            "PREFIX prov: <http://www.w3.org/ns/prov#>
             PREFIX schema: <https://schema.org/>
             SELECT ?a WHERE { GRAPH ?g { ?a a prov:SoftwareAgent ; schema:name \"batch-exporter\" } }",
        )
        .unwrap();
    assert_eq!(agents.rows.len(), 1, "one system named once, not two");
}

#[tokio::test]
async fn a_claim_about_who_produced_something_cannot_displace_the_evidence() {
    // A caller may say anything about who produced an artifact. What it must never be able to
    // do is overwrite what the registry knows: which credential presented the record.
    let h = harness().await;
    let (status, sw) = h
        .req("POST", "/api/v1/software", Some(ROOT), Some(json!({"name": "a-service", "kinds": ["service"]})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let (status, inst) = h
        .req("POST", "/api/v1/instances", Some(ROOT), Some(json!({"label": "d", "software": sw["id"]})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{inst}");
    let (status, minted) = h
        .req("POST", &format!("/api/v1/instances/{}/tokens", inst["id"].as_str().unwrap()), Some(ROOT), Some(json!({})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    let token = minted["token"].as_str().unwrap().to_string();

    let (status, out) = h
        .req(
            "POST",
            "/api/v1/advertise/produced",
            Some(&token),
            Some(json!({
                "run": {"status": "success", "external_key": "x/1"},
                "artifacts": [{
                    "title": "Claims a different producer",
                    "produced_by": {"name": "somebody else entirely", "kind": "software"}
                }]
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
    let iri = out["artifacts"][0].as_str().unwrap();
    let id = iri.rsplit('/').next().unwrap();

    let (_, back) = h.req("GET", &format!("/api/v1/artifacts/{id}"), None, None).await;
    // The claim is recorded...
    assert_eq!(back["produced_by"]["name"], "somebody else entirely", "{back}");
    // ...and the evidence still says which deployment actually presented it.
    assert_eq!(back["attributed_to"], inst["iri"], "{back}");
}

// ------------------------------------------------------- one modification date, not two

#[tokio::test]
async fn a_supplied_modification_date_replaces_the_stamp_rather_than_joining_it() {
    // `dct:modified` says when the resource changed, so a producer that knows beats the clock.
    // Both used to be written, leaving two values on one record and a reader taking whichever
    // came back first — a tie nothing in the graph can break.
    let h = harness().await;
    let (status, art) = h
        .req(
            "POST",
            "/api/v1/artifacts",
            Some(ROOT),
            Some(json!({"title": "Knows its own date", "modified": "2024-03-01T09:00:00Z"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{art}");
    let iri = art["iri"].as_str().unwrap().to_string();

    let rows = h
        .state
        .store
        .select(&format!(
            "PREFIX dct: <http://purl.org/dc/terms/>
             SELECT ?m WHERE {{ GRAPH ?g {{ <{iri}> dct:modified ?m }} }}"
        ))
        .unwrap();
    let dates: Vec<String> = rows.rows.iter().filter_map(|r| r.str("m")).collect();
    assert_eq!(dates.len(), 1, "exactly one modification date: {dates:?}");
    assert!(dates[0].starts_with("2024-03-01"), "and it is the one the caller gave: {dates:?}");

    // Saying nothing still gets a stamp — the registry did modify the record.
    let (status, other) = h
        .req("POST", "/api/v1/artifacts", Some(ROOT), Some(json!({"title": "Says nothing"})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{other}");
    assert!(other["modified"].as_str().is_some_and(|m| m.starts_with("20")), "{other}");
}

#[tokio::test]
async fn an_artifacts_contact_uses_the_standard_term_and_still_reads_the_retired_one() {
    // The ontology marked `tar:contact` deprecated in favour of `codemeta:maintainer` while the
    // artifact path went on writing it — the registry contradicting its own published model.
    let h = harness().await;
    let (status, art) = h
        .req(
            "POST",
            "/api/v1/artifacts",
            Some(ROOT),
            Some(json!({"title": "Has someone to ask",
                        "contact": {"name": "A Maintainer", "kind": "person"}})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{art}");
    assert_eq!(art["contact"]["name"], "A Maintainer");
    let iri = art["iri"].as_str().unwrap().to_string();

    // Asked separately: two OPTIONALs inside one `GRAPH ?g` leave `?g` unbound and match
    // nothing, which reads as "the triple is absent" whether it is or not.
    let count = |predicate: &str| {
        h.state
            .store
            .select(&format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH ?g {{ <{iri}> <{predicate}> ?o }} }}"
            ))
            .unwrap()
            .rows
            .first()
            .and_then(|r| r.i64("n"))
            .unwrap_or(0)
    };
    assert_eq!(count("https://w3id.org/codemeta/terms/maintainer"), 1, "the standard term is written");
    assert_eq!(count("https://w3id.org/tar/ns#contact"), 0, "and the retired one is not");

    // A record written before the change still resolves, which is why the read keeps a fallback.
    let legacy = format!("{BASE}/artifact/01a05e00-0000-7000-8000-000000000000");
    let agent = format!("{BASE}/agent/01a05e00-0000-7000-8000-000000000001");
    let mut tx = tar::store::GraphTx::new();
    let mut n = tar::rdf::Node::iri(&legacy, tar::ns::G_LOCAL);
    n.a("http://www.w3.org/ns/dcat#Dataset");
    n.link(tar::ns::TAR, "contact", &agent);
    tx.extend(n.finish());
    let mut a = tar::rdf::Node::iri(&agent, tar::ns::G_LOCAL);
    a.a("https://schema.org/Person");
    a.text(tar::ns::SCHEMA, "name", "Written Before The Change");
    tx.extend(a.finish());
    h.state.store.apply(tx).unwrap();

    let (status, old) = h
        .req("GET", &format!("/api/v1/artifacts/{}", legacy.rsplit('/').next().unwrap()), None, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{old}");
    assert_eq!(old["contact"]["name"], "Written Before The Change", "{old}");
}

// ---------------------------------------------------------------- issuer pinning
//
// A client id is only unique *within* an issuer. "ontoexplorer-prod" at the estate's Keycloak
// and "ontoexplorer-prod" at a partner's identity provider are two different principals that
// spell their name the same way, and naming a client is free at every issuer.
//
// The registry used to match a credential to a record by client id alone whenever the record
// did not pin an issuer, which made every unpinned record a wildcard across every issuer the
// deployment trusts. On the Software side it could not do anything else: the lookup asked for
// `tar:oidcIssuer`, but no field ever wrote it, so the check was dead code that always passed.

const HS_SECRET: &[u8] = b"a-test-signing-secret-not-used-in-production";
const ISSUER: &str = "https://keycloak.test.example/realms/ids";
const PARTNER_ISSUER: &str = "https://partner.example/realms/theirs";
const OTHER_ISSUER: &str = "https://ci.example/oidc";

fn jwt(claims: Value) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
    header.kid = Some("test-key".into());
    jsonwebtoken::encode(&header, &claims, &jsonwebtoken::EncodingKey::from_secret(HS_SECRET)).unwrap()
}

fn exp() -> i64 {
    (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp()
}

/// Trusts the estate's own issuer plus a partner's, for workloads only — the
/// `TAR_OIDC_ISSUER` + `TAR_WORKLOAD_ISSUERS` split.
async fn harness_two_issuers() -> Harness {
    harness_with_issuers(Some(ISSUER), vec![PARTNER_ISSUER.into()]).await
}

async fn harness_with_issuers(primary: Option<&str>, workload: Vec<String>) -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    config.oidc.issuer = primary.map(str::to_string);
    config.oidc.client_id = Some("tar-ui".into());
    config.oidc.audience = Some(BASE.into());
    config.oidc.workload_issuers = workload;
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

/// A token for `client` from `issuer`, as a `client_credentials` grant looks: no `sid`, no
/// realm roles, so it is classified as a workload rather than a person.
fn workload_token(issuer: &str, client: &str) -> String {
    jwt(json!({"iss": issuer, "aud": BASE, "exp": exp(), "sub": format!("svc-{client}"), "azp": client}))
}

/// The defect, stated as a test: the partner's issuer must not be able to spend a client id
/// that the estate's own Keycloak was meant to own.
#[tokio::test]
async fn a_registration_client_is_not_spendable_from_another_issuer() {
    let h = harness_two_issuers().await;
    let (status, sw) = h
        .req(
            "POST",
            "/api/v1/software",
            Some(ROOT),
            Some(json!({"name": "ontoexplorer", "kinds": ["service"],
                        "registration_clients": ["ontoexplorer-prod"],
                        "registration_issuer": ISSUER})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");

    // The partner mints a client of the same name and announces a deployment.
    let theirs = workload_token(PARTNER_ISSUER, "ontoexplorer-prod");
    let (status, body) = h
        .req("PUT", "/api/v1/instances/self", Some(&theirs), Some(json!({"instance_key": "theirs"})))
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a client id from an issuer the software did not name must not register deployments of it: {body}"
    );

    // The same client id from the issuer the software *did* name works.
    let ours = workload_token(ISSUER, "ontoexplorer-prod");
    let (status, body) =
        h.req("PUT", "/api/v1/instances/self", Some(&ours), Some(json!({"instance_key": "ours"}))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["software_name"], "ontoexplorer", "{body}");
}

/// With several issuers accepted and none pinned, the binding is refused rather than guessed.
/// Guessing would hand the weakest accepted issuer the authority meant for the strongest.
#[tokio::test]
async fn an_unpinned_registration_client_does_not_match_a_secondary_issuer() {
    let h = harness_two_issuers().await;
    // Written straight to the store, because the API now refuses to create this record at all
    // — the point here is that an existing one is not honoured either.
    let (status, sw) = h
        .req(
            "POST",
            "/api/v1/software",
            Some(ROOT),
            Some(json!({"name": "legacy", "kinds": ["service"],
                        "registration_clients": ["legacy-prod"], "registration_issuer": ISSUER})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    let iri = sw["iri"].as_str().unwrap().to_string();
    let mut tx = tar::store::GraphTx::new();
    tx.replace_property(&iri, &format!("{}registrationIssuer", tar::ns::TAR), tar::ns::G_LOCAL);
    h.state.store.apply(tx).unwrap();

    // The estate's own issuer is the default, so it still resolves.
    let ours = workload_token(ISSUER, "legacy-prod");
    let (status, body) =
        h.req("PUT", "/api/v1/instances/self", Some(&ours), Some(json!({"instance_key": "a"}))).await;
    assert_eq!(status, StatusCode::CREATED, "an unpinned record still means the primary issuer: {body}");

    // The partner's does not.
    let theirs = workload_token(PARTNER_ISSUER, "legacy-prod");
    let (status, body) =
        h.req("PUT", "/api/v1/instances/self", Some(&theirs), Some(json!({"instance_key": "b"}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// One issuer and no primary: unambiguous, so no pin is needed and none is demanded.
#[tokio::test]
async fn a_single_workload_issuer_needs_no_pin() {
    let h = harness_with_issuers(None, vec![PARTNER_ISSUER.into()]).await;
    let (status, sw) = h
        .req(
            "POST",
            "/api/v1/software",
            Some(ROOT),
            Some(json!({"name": "solo", "kinds": ["service"], "registration_clients": ["solo-prod"]})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "a lone issuer is not ambiguous: {sw}");

    let t = workload_token(PARTNER_ISSUER, "solo-prod");
    let (status, body) =
        h.req("PUT", "/api/v1/instances/self", Some(&t), Some(json!({"instance_key": "a"}))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// The refusal arrives where the curator can act on it — at the form, not as a 403 the first
/// time the workload calls with a message about a field it never sent.
#[tokio::test]
async fn a_registration_client_without_an_issuer_is_refused_where_it_is_ambiguous() {
    // Two workload issuers and no primary: nothing makes one of them the obvious reading.
    let h = harness_with_issuers(None, vec![PARTNER_ISSUER.into(), OTHER_ISSUER.into()]).await;
    let (status, body) = h
        .req(
            "POST",
            "/api/v1/software",
            Some(ROOT),
            Some(json!({"name": "unpinned", "kinds": ["service"],
                        "registration_clients": ["unpinned-prod"]})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let detail = body.to_string();
    assert!(detail.contains("registration_issuer"), "the message must name the field: {detail}");
    assert!(detail.contains(PARTNER_ISSUER), "and the issuers it has to choose between: {detail}");

    // Naming no clients at all is not affected: there is nothing to disambiguate.
    let (status, body) = h
        .req("POST", "/api/v1/software", Some(ROOT), Some(json!({"name": "no-clients", "kinds": ["service"]})))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// The issuer survives a round trip and a PATCH that does not mention it.
#[tokio::test]
async fn the_registration_issuer_survives_a_partial_update() {
    let h = harness_two_issuers().await;
    let (status, sw) = h
        .req(
            "POST",
            "/api/v1/software",
            Some(ROOT),
            Some(json!({"name": "patched", "kinds": ["service"],
                        "registration_clients": ["patched-prod"], "registration_issuer": ISSUER})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sw}");
    assert_eq!(sw["registration_issuer"], ISSUER, "{sw}");
    let id = sw["id"].as_str().unwrap().to_string();

    let (status, after) =
        h.req("PATCH", &format!("/api/v1/software/{id}"), Some(ROOT), Some(json!({"tagline": "now with a tagline"}))).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["registration_issuer"], ISSUER, "a PATCH that never mentioned it must not drop it: {after}");
    assert_eq!(after["registration_clients"][0], "patched-prod", "{after}");
}
