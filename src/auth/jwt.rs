//! OIDC / JWT verification — workload identity for tools, and sign-in for humans.
//!
//! ## Why this exists
//!
//! Spec D8 minted a per-Instance API token. That works, but it makes the registry a secret
//! store: every deployment gets a long-lived bearer string that somebody has to distribute,
//! rotate and revoke by hand. Since Keycloak already runs on `ids3`, the better primitive is
//! there already: give each deployment an **OIDC client** and let it fetch a short-lived JWT
//! with the `client_credentials` grant. We verify the signature against the issuer's JWKS,
//! read the client id out of the token, and look up the `Instance` that declared it.
//!
//! The authorisation rule does not change — it gets *stronger*, because the identity is now
//! asserted by an issuer we trust and expires in minutes rather than living in a CI secret
//! until someone remembers to rotate it.
//!
//! ## What a tool does
//!
//! ```text
//! curl -s -d grant_type=client_credentials -u "$CLIENT_ID:$CLIENT_SECRET" \
//!      https://keycloak.example.org/realms/ids/protocol/openid-connect/token | jq -r .access_token
//! # then
//! curl -H "Authorization: Bearer $TOKEN" -d @produced.json \
//!      https://reg.example.org/api/v1/advertise/produced
//! ```
//!
//! ## Beyond Keycloak
//!
//! The same code path accepts any OIDC issuer listed in `TAR_WORKLOAD_ISSUERS`, so a
//! Kubernetes projected ServiceAccount token (`kubernetes.default.svc`) or a GitHub Actions
//! OIDC token (`token.actions.githubusercontent.com`) authenticates a deployment with **no
//! stored secret at all**. The Instance record simply declares which client id / subject it
//! trusts via `tar:oidcClientId`.

use crate::error::{AppError, AppResult};
use crate::ns;
use crate::state::AppState;
use anyhow::Result;
use base64::Engine;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use super::{CredentialKind, Principal, Role, ALL_SCOPES};

const JWKS_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default)]
    crv: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

struct Cached {
    keys: Vec<Jwk>,
    at: Instant,
}

pub struct JwtVerifier {
    http: reqwest::Client,
    jwks: RwLock<HashMap<String, Cached>>,
    /// Statically supplied keys, keyed by `kid`. Used by tests, and by deployments that
    /// pin a key rather than fetching JWKS.
    static_keys: HashMap<String, (Algorithm, DecodingKey)>,
}

impl JwtVerifier {
    pub fn new(timeout: Duration) -> Self {
        Self {
            http: reqwest::Client::builder().timeout(timeout).build().unwrap_or_default(),
            jwks: RwLock::new(HashMap::new()),
            static_keys: HashMap::new(),
        }
    }

    /// Register a key directly instead of discovering it over the network.
    pub fn with_static_key(mut self, kid: &str, alg: Algorithm, key: DecodingKey) -> Self {
        self.static_keys.insert(kid.to_string(), (alg, key));
        self
    }

    async fn jwks_uri(&self, issuer: &str) -> Result<String> {
        let disco = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
        let doc: Value = self.http.get(&disco).send().await?.error_for_status()?.json().await?;
        doc.get("jwks_uri")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("{disco} has no jwks_uri"))
    }

    async fn keys_for(&self, issuer: &str, force: bool) -> Result<Vec<Jwk>> {
        if !force {
            let cache = self.jwks.read().await;
            if let Some(c) = cache.get(issuer) {
                if c.at.elapsed() < JWKS_TTL {
                    return Ok(c.keys.clone());
                }
            }
        }
        let uri = self.jwks_uri(issuer).await?;
        let set: JwkSet = self.http.get(&uri).send().await?.error_for_status()?.json().await?;
        let mut cache = self.jwks.write().await;
        cache.insert(issuer.to_string(), Cached { keys: set.keys.clone(), at: Instant::now() });
        Ok(set.keys)
    }
}

fn decoding_key(jwk: &Jwk) -> Result<(Algorithm, DecodingKey)> {
    match jwk.kty.as_str() {
        "RSA" => {
            let (n, e) = (jwk.n.as_deref().unwrap_or_default(), jwk.e.as_deref().unwrap_or_default());
            let alg = match jwk.alg.as_deref() {
                Some("RS384") => Algorithm::RS384,
                Some("RS512") => Algorithm::RS512,
                Some("PS256") => Algorithm::PS256,
                _ => Algorithm::RS256,
            };
            Ok((alg, DecodingKey::from_rsa_components(n, e)?))
        }
        "EC" => {
            let (x, y) = (jwk.x.as_deref().unwrap_or_default(), jwk.y.as_deref().unwrap_or_default());
            let alg = match jwk.crv.as_deref() {
                Some("P-384") => Algorithm::ES384,
                _ => Algorithm::ES256,
            };
            Ok((alg, DecodingKey::from_ec_components(x, y)?))
        }
        other => Err(anyhow::anyhow!("unsupported JWK key type {other}")),
    }
}

/// Read `iss` without verifying — we need it to pick the right JWKS.
fn unverified_issuer(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("iss")?.as_str().map(|s| s.trim_end_matches('/').to_string())
}

/// Pull a dotted claim path such as `realm_access.roles` out of the claim set.
fn claim_path<'a>(claims: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = claims;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

fn string_list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => s.split_whitespace().map(str::to_string).collect(),
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    }
}

pub async fn authenticate_jwt(state: &Arc<AppState>, token: &str) -> AppResult<Principal> {
    let cfg = &state.config.oidc;
    if !cfg.enabled() {
        return Err(AppError::unauthorized(
            "this registry accepts registry API tokens only — no OIDC issuer is configured \
             (set TAR_OIDC_ISSUER / TAR_WORKLOAD_ISSUERS to accept Keycloak or Kubernetes tokens)",
        ));
    }
    let issuer = unverified_issuer(token).ok_or_else(|| AppError::unauthorized("token is not a readable JWT"))?;
    let accepted = cfg.accepted_issuers();
    if !accepted.iter().any(|i| i == &issuer) {
        return Err(AppError::unauthorized(format!(
            "issuer {issuer} is not trusted by this registry (trusted: {})",
            accepted.join(", ")
        )));
    }

    let header = decode_header(token).map_err(|e| AppError::unauthorized(format!("bad JWT header: {e}")))?;
    let kid = header.kid.clone().unwrap_or_default();

    let verifier = &state.jwt;
    let (alg, key) = if let Some((alg, key)) = verifier.static_keys.get(&kid) {
        (*alg, key.clone())
    } else {
        let mut found = None;
        for force in [false, true] {
            let keys = verifier
                .keys_for(&issuer, force)
                .await
                .map_err(|e| AppError::unauthorized(format!("cannot fetch JWKS for {issuer}: {e}")))?;
            if let Some(jwk) =
                keys.iter().find(|k| k.kid.as_deref() == Some(kid.as_str()) || (kid.is_empty() && keys.len() == 1))
            {
                found = Some(decoding_key(jwk).map_err(|e| AppError::unauthorized(e.to_string()))?);
                break;
            }
        }
        found.ok_or_else(|| AppError::unauthorized(format!("no JWKS key matches kid {kid:?}")))?
    };

    let mut validation = Validation::new(alg);
    validation.set_issuer(&[issuer.as_str()]);
    match (&cfg.audience, cfg.require_audience) {
        (Some(aud), true) => {
            validation.set_audience(&[aud.as_str()]);
            // `set_audience` alone checks the claim *only if the token carries one*: the
            // library's own note is "Validation only happens if `aud` claim is present".
            // A token minted by a trusted issuer for a different service, with no audience,
            // would otherwise be accepted here — which is exactly the cross-service reuse the
            // claim exists to prevent. Requiring the claim is what makes the setting's name
            // true.
            validation.set_required_spec_claims(&["exp", "aud"]);
        }
        // No expected audience configured, or the operator turned the check off. Say nothing
        // about `aud` rather than half-checking it.
        _ => validation.validate_aud = false,
    }
    let data =
        decode::<Value>(token, &key, &validation).map_err(|e| AppError::unauthorized(format!("JWT rejected: {e}")))?;
    let claims = data.claims;

    principal_from_claims(state, &issuer, &claims).await
}

/// Map a *verified* claim set onto a registry principal.
pub async fn principal_from_claims(state: &Arc<AppState>, issuer: &str, claims: &Value) -> AppResult<Principal> {
    let cfg = &state.config.oidc;
    let sub = claims.get("sub").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = claims
        .get("preferred_username")
        .or_else(|| claims.get("name"))
        .or_else(|| claims.get("email"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Only the estate's own provider (`TAR_OIDC_ISSUER`) may assert who is a curator or an
    // admin here. `TAR_WORKLOAD_ISSUERS` is, in the words of addendum §4, "accepted for
    // workload tokens only, never for browser sign-in" — a partner's Keycloak, a Kubernetes
    // API server or GitHub Actions can all put a realm role called `admin` in a token, and
    // honouring it would hand them this registry.
    let home_issuer = cfg.issuer.as_deref().map(|i| i.trim_end_matches('/')) == Some(issuer);

    // Candidate workload identities, most specific first.
    let mut candidates: Vec<String> = Vec::new();
    for path in [cfg.client_claim.as_str(), "azp", "client_id", "clientId"] {
        if let Some(v) = claim_path(claims, path).and_then(|v| v.as_str()) {
            if !v.is_empty() && !candidates.iter().any(|c| c == v) {
                candidates.push(v.to_string());
            }
        }
    }
    // The browser sign-in client is a person's client, never a deployment's. Keeping it out
    // of the candidate list stops an Instance record that (mistakenly or maliciously)
    // declares `tar:oidcClientId "tar-ui"` from turning every signed-in person into that
    // deployment.
    let ui_client = cfg.client_id.as_deref().filter(|_| home_issuer);
    let is_ui_client = ui_client.is_some_and(|c| candidates.first().is_some_and(|f| f == c));
    if is_ui_client {
        candidates.clear();
    } else if !sub.is_empty() {
        candidates.push(sub.clone());
    }

    // Roles: Keycloak realm roles, plus client roles for our own client id.
    let mut roles: BTreeSet<Role> = BTreeSet::new();
    let mut role_strings: Vec<String> = Vec::new();
    if home_issuer {
        if let Some(v) = claim_path(claims, &cfg.roles_claim) {
            role_strings.extend(string_list(v));
        }
        if let Some(client) = &cfg.client_id {
            if let Some(v) = claim_path(claims, &format!("resource_access.{client}.roles")) {
                role_strings.extend(string_list(v));
            }
        }
        if let Some(v) = claims.get("roles") {
            role_strings.extend(string_list(v));
        }
    }
    for r in &role_strings {
        if let Some(role) = Role::parse(r) {
            roles.insert(role);
        }
    }

    // Scopes the token itself carries, filtered to scopes this registry understands.
    let mut token_scopes: BTreeSet<String> = BTreeSet::new();
    for path in [cfg.scope_claim.as_str(), "scope", "scp"] {
        if let Some(v) = claim_path(claims, path) {
            for s in string_list(v) {
                if ALL_SCOPES.contains(&s.as_str()) {
                    token_scopes.insert(s);
                }
            }
        }
    }

    // Bind to an Instance, if one declares any of the candidate client ids.
    let mut bound = find_instance_for_client(state, issuer, &candidates).await?;

    // Failing that, the deployment this credential registered for itself.
    //
    // A credential authorised through a software's `registration_clients` is shared by every
    // deployment of that application, so the record deliberately does not name it — writing the
    // client id there would make the next deployment authenticate as this one. But then nothing
    // bound the credential to what it had just created: it could register a deployment and was
    // then refused the advertise call it registered in order to make, with a message about a
    // missing scope that no amount of configuration would have supplied.
    //
    // Resolved only when it is unambiguous. One credential may register several deployments,
    // told apart by the `instance_key` each announcement carries; an advertisement carries no
    // such field, so with more than one there is no honest way to say which deployment ran, and
    // guessing would attribute a run to the wrong one. That case stays unbound and the refusal
    // says why.
    if bound.is_none() {
        bound = find_self_registered_instance(state, issuer, &candidates).await?;
    }

    if let Some(binding) = bound {
        // A workload: the deployment itself is the principal (spec §8.3).
        let scopes = if !token_scopes.is_empty() {
            token_scopes
        } else if !binding.allowed_scopes.is_empty() {
            binding.allowed_scopes.iter().cloned().collect()
        } else {
            [super::SCOPE_ADVERTISE_PRODUCE, super::SCOPE_ADVERTISE_CONSUME].iter().map(|s| s.to_string()).collect()
        };
        return Ok(Principal {
            credential: CredentialKind::OidcWorkload,
            instance_iri: Some(binding.instance_iri),
            software_iri: binding.registers_software,
            subject: candidates.first().cloned().unwrap_or(sub),
            display_name: binding.label.or(name),
            scopes,
            roles,
            issuer: Some(issuer.to_string()),
        });
    }

    // No Instance claims this client id, so this is a person or an unregistered workload.
    //
    // Roles alone are not the test. A signed-in user who holds none of `reader`/`curator`/
    // `admin` is still a person, and calling them `oidc-workload` in `whoami` and in the
    // audit log is simply wrong — it also sends them looking for an Instance to register
    // when what they actually need is a role. Two signals say "person": the token was issued
    // to the browser sign-in client, or it carries a user session id (`sid`), which Keycloak
    // puts on interactive logins and never on a `client_credentials` token.
    let is_person = !roles.is_empty() || (home_issuer && (is_ui_client || claims.get("sid").is_some()));

    // Nothing is bound to this client at the deployment level. Before concluding it has no
    // authority, ask whether some *software* names it as a registration client — the
    // auto-registration mode, where one credential belongs to the application and each of its
    // deployments registers itself.
    let software_iri = if is_person { None } else { find_software_for_client(state, issuer, &candidates).await? };

    // A person is their `sub`. An unbound workload is its *client id*: that is the string an
    // admin has to copy into `tar:oidcClientId` to make it work, and addendum §3.2 promises
    // `whoami` is the first thing you curl when a CI job gets a 403. Reporting Keycloak's
    // service-account UUID there sent people looking for the wrong value.
    let subject = if is_person {
        if sub.is_empty() {
            candidates.first().cloned().unwrap_or_default()
        } else {
            sub
        }
    } else {
        candidates.first().cloned().filter(|c| !c.is_empty()).unwrap_or(sub)
    };

    Ok(Principal {
        credential: if is_person {
            CredentialKind::OidcHuman
        } else {
            // Verified, but nothing here is bound to it. Authenticated with no authority —
            // the error when it tries to write names what an admin has to do about it.
            CredentialKind::OidcWorkload
        },
        instance_iri: None,
        software_iri,
        subject,
        display_name: name,
        scopes: token_scopes,
        roles,
        issuer: Some(issuer.to_string()),
    })
}

pub struct InstanceBinding {
    pub instance_iri: String,
    pub label: Option<String>,
    pub allowed_scopes: Vec<String>,
    /// Set only when the binding was *inferred* from a deployment this credential registered,
    /// rather than declared by a deployment naming this client id. A shared registration
    /// credential, in other words.
    pub registers_software: Option<String>,
}

/// Does a record that pinned `declared` (or pinned nothing) accept a token from `issuer`?
///
/// **A pin is required, not merely honoured when present.** The old rule — match the issuer
/// when the record names one, accept any accepted issuer when it does not — meant an unpinned
/// record was a wildcard across every issuer the registry trusts. On a registry that accepts a
/// partner's Keycloak, a Kubernetes API server or GitHub Actions alongside its own, whoever
/// controls the weakest of those could mint a token whose client id spells the same string as
/// the intended one and inherit its authority. Nothing had to be compromised for that: naming a
/// client is free at every issuer.
///
/// So an unpinned record now falls back to [`OidcConfig::default_binding_issuer`], which
/// answers only when the answer is unambiguous. On the single-issuer registries this is the
/// same behaviour as before; on a multi-issuer one it refuses until a curator says which issuer
/// was meant.
fn issuer_admits(cfg: &crate::config::OidcConfig, declared: Option<&str>, issuer: &str) -> bool {
    match declared {
        Some(d) => d.trim_end_matches('/') == issuer,
        None => cfg.default_binding_issuer().as_deref() == Some(issuer),
    }
}

/// Find the Instance that declared one of these OIDC client ids (`tar:oidcClientId`).
/// The issuer must match what the Instance pinned in `tar:oidcIssuer`, or — when it pinned
/// nothing — the registry's default binding issuer. See [`issuer_admits`].
pub async fn find_instance_for_client(
    state: &Arc<AppState>,
    issuer: &str,
    candidates: &[String],
) -> AppResult<Option<InstanceBinding>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let (state, issuer, candidates) = (state.clone(), issuer.to_string(), candidates.to_vec());
    crate::error::blocking(move || {
        let values = candidates
            .iter()
            .map(|c| format!("\"{}\"", c.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let q = format!(
            r#"{prefixes}
SELECT ?i ?label ?iss (GROUP_CONCAT(DISTINCT ?scope; separator=" ") AS ?scopes) WHERE {{
  GRAPH <{g}> {{
    VALUES ?cid {{ {values} }}
    ?i tar:oidcClientId ?cid .
    OPTIONAL {{ ?i rdfs:label ?label }}
    OPTIONAL {{ ?i tar:oidcIssuer ?iss }}
    OPTIONAL {{ ?i tar:allowedScope ?scope }}
  }}
}} GROUP BY ?i ?label ?iss"#,
            prefixes = ns::PREFIXES,
            g = ns::G_LOCAL,
        );
        let rows = state.store.select(&q).map_err(AppError::from)?;
        for row in rows.rows {
            if !issuer_admits(&state.config.oidc, row.str("iss").as_deref(), &issuer) {
                continue;
            }
            let Some(iri) = row.iri("i") else { continue };
            return Ok(Some(InstanceBinding {
                instance_iri: iri,
                label: row.str("label"),
                allowed_scopes: row
                    .str("scopes")
                    .map(|s| s.split_whitespace().map(str::to_string).collect())
                    .unwrap_or_default(),
                // Declared by the deployment itself, so not shared.
                registers_software: None,
            }));
        }
        Ok(None)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_dotted_claim_paths() {
        let v = serde_json::json!({"realm_access": {"roles": ["curator", "offline_access"]}});
        assert_eq!(string_list(claim_path(&v, "realm_access.roles").unwrap()), vec!["curator", "offline_access"]);
        assert!(claim_path(&v, "resource_access.tar.roles").is_none());
    }

    #[test]
    fn scope_claim_accepts_string_or_array() {
        assert_eq!(string_list(&serde_json::json!("a b c")), vec!["a", "b", "c"]);
        assert_eq!(string_list(&serde_json::json!(["a", "b"])), vec!["a", "b"]);
    }

    #[test]
    fn reads_issuer_without_verifying() {
        // header.payload.signature, payload = {"iss":"https://kc/realms/ids"}
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"iss":"https://kc/realms/ids/"}"#);
        let token = format!("aaa.{payload}.bbb");
        assert_eq!(unverified_issuer(&token).as_deref(), Some("https://kc/realms/ids"));
    }
}
pub use jsonwebtoken;

/// Find the Software that names one of these OIDC client ids as a registration client
/// (`tar:registrationClient`) — the auto-registration mode.
///
/// Deliberately a *different* predicate from `tar:oidcClientId` on an Instance. Reusing that
/// one would make a single triple mean "this credential *is* this deployment" in one place and
/// "this credential may create deployments" in another, and the difference between being a
/// thing and being allowed to create it is the whole of the authorisation question here.
///
/// The issuer those client ids belong to is `tar:registrationIssuer`, for the same reason:
/// `tar:oidcIssuer` is declared with `rdfs:domain tar:Instance`, so putting it on a Software
/// would assert that the Software is a deployment. Two properties, two domains, one rule —
/// see [`issuer_admits`], which both go through.
pub async fn find_software_for_client(
    state: &Arc<AppState>,
    issuer: &str,
    candidates: &[String],
) -> AppResult<Option<String>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let (state, issuer, candidates) = (state.clone(), issuer.to_string(), candidates.to_vec());
    crate::error::blocking(move || {
        let values = candidates
            .iter()
            .map(|c| format!("\"{}\"", c.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let q = format!(
            r#"{prefixes}
SELECT ?s ?iss WHERE {{
  GRAPH <{g}> {{
    VALUES ?cid {{ {values} }}
    ?s tar:registrationClient ?cid .
    OPTIONAL {{ ?s tar:registrationIssuer ?iss }}
  }}
}}"#,
            prefixes = ns::PREFIXES,
            g = ns::G_LOCAL,
        );
        let rows = state.store.select(&q).map_err(AppError::from)?;
        for row in rows.rows {
            if !issuer_admits(&state.config.oidc, row.str("iss").as_deref(), &issuer) {
                continue;
            }
            if let Some(iri) = row.iri("s") {
                return Ok(Some(iri));
            }
        }
        Ok(None)
    })
    .await
}

/// The deployment a credential registered for itself, when there is exactly one.
///
/// See the call site for why ambiguity is left unresolved rather than guessed at.
pub async fn find_self_registered_instance(
    state: &Arc<AppState>,
    issuer: &str,
    candidates: &[String],
) -> AppResult<Option<InstanceBinding>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let (state, issuer, candidates) = (state.clone(), issuer.to_string(), candidates.to_vec());
    crate::error::blocking(move || {
        let values = candidates
            .iter()
            .map(|c| format!("\"{}\"", c.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let q = format!(
            r#"{prefixes}
SELECT ?i ?label ?sw ?iss (GROUP_CONCAT(DISTINCT ?scope; separator=" ") AS ?scopes) WHERE {{
  GRAPH <{g}> {{
    VALUES ?sub {{ {values} }}
    ?i tar:selfRegisteredBy ?sub .
    OPTIONAL {{ ?i rdfs:label ?label }}
    OPTIONAL {{ ?i tar:instanceOf ?sw }}
    OPTIONAL {{ ?i tar:selfRegisteredIssuer ?iss }}
    OPTIONAL {{ ?i tar:allowedScope ?scope }}
  }}
  FILTER NOT EXISTS {{ GRAPH ?tg {{ ?i tar:tombstoned true }} }}
}} GROUP BY ?i ?label ?sw ?iss"#,
            prefixes = ns::PREFIXES,
            g = ns::G_LOCAL,
        );
        let rows = state.store.select(&q).map_err(AppError::from)?;
        // Only deployments this credential registered *at the issuer it is presenting now*.
        // The subject here is a client id, and this path was the way past a pinned software:
        // once any deployment existed under the name, a token from any other accepted issuer
        // that spelled its client the same way bound to it and inherited its scopes.
        let rows: Vec<_> =
            rows.rows.iter().filter(|r| issuer_admits(&state.config.oidc, r.str("iss").as_deref(), &issuer)).collect();
        // More than one is the ambiguous case: bind to nothing rather than to an arbitrary one.
        if rows.len() != 1 {
            return Ok(None);
        }
        let row = rows[0];
        let Some(iri) = row.iri("i") else { return Ok(None) };
        Ok(Some(InstanceBinding {
            instance_iri: iri,
            label: row.str("label"),
            allowed_scopes: row
                .str("scopes")
                .map(|s| s.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
            // The software this credential demonstrably registers deployments of. Carried so
            // that `announce_self` can tell this apart from a credential that *is* one
            // deployment: a shared one must keep honouring `instance_key`, or the second
            // deployment to announce silently overwrites the first.
            registers_software: row.iri("sw"),
        }))
    })
    .await
}
