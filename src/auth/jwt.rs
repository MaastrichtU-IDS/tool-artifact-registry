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
    let issuer = unverified_issuer(token)
        .ok_or_else(|| AppError::unauthorized("token is not a readable JWT"))?;
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
            if let Some(jwk) = keys
                .iter()
                .find(|k| k.kid.as_deref() == Some(kid.as_str()) || (kid.is_empty() && keys.len() == 1))
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
        (Some(aud), true) => validation.set_audience(&[aud.as_str()]),
        _ => validation.validate_aud = false,
    }
    let data = decode::<Value>(token, &key, &validation)
        .map_err(|e| AppError::unauthorized(format!("JWT rejected: {e}")))?;
    let claims = data.claims;

    principal_from_claims(state, &issuer, &claims).await
}

/// Map a *verified* claim set onto a registry principal.
pub async fn principal_from_claims(
    state: &Arc<AppState>,
    issuer: &str,
    claims: &Value,
) -> AppResult<Principal> {
    let cfg = &state.config.oidc;
    let sub = claims.get("sub").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = claims
        .get("preferred_username")
        .or_else(|| claims.get("name"))
        .or_else(|| claims.get("email"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // Candidate workload identities, most specific first.
    let mut candidates: Vec<String> = Vec::new();
    for path in [cfg.client_claim.as_str(), "azp", "client_id", "clientId"] {
        if let Some(v) = claim_path(claims, path).and_then(|v| v.as_str()) {
            if !v.is_empty() && !candidates.iter().any(|c| c == v) {
                candidates.push(v.to_string());
            }
        }
    }
    if !sub.is_empty() {
        candidates.push(sub.clone());
    }

    // Roles: Keycloak realm roles, plus client roles for our own client id.
    let mut roles: BTreeSet<Role> = BTreeSet::new();
    let mut role_strings: Vec<String> = Vec::new();
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
    let bound = find_instance_for_client(state, issuer, &candidates).await?;

    if let Some(binding) = bound {
        // A workload: the deployment itself is the principal (spec §8.3).
        let scopes = if !token_scopes.is_empty() {
            token_scopes
        } else if !binding.allowed_scopes.is_empty() {
            binding.allowed_scopes.iter().cloned().collect()
        } else {
            [super::SCOPE_ADVERTISE_PRODUCE, super::SCOPE_ADVERTISE_CONSUME]
                .iter()
                .map(|s| s.to_string())
                .collect()
        };
        return Ok(Principal {
            credential: CredentialKind::OidcWorkload,
            instance_iri: Some(binding.instance_iri),
            subject: candidates.first().cloned().unwrap_or(sub),
            display_name: binding.label.or(name),
            scopes,
            roles,
            issuer: Some(issuer.to_string()),
        });
    }

    // No Instance claims this client id. If the token carries human roles, it is a person.
    if !roles.is_empty() {
        return Ok(Principal {
            credential: CredentialKind::OidcHuman,
            instance_iri: None,
            subject: if sub.is_empty() { candidates.first().cloned().unwrap_or_default() } else { sub },
            display_name: name,
            scopes: token_scopes,
            roles,
            issuer: Some(issuer.to_string()),
        });
    }

    // Verified, but nothing here is bound to it. Authenticated with no authority — the error
    // when it tries to write names exactly what an admin has to do about it.
    Ok(Principal {
        credential: CredentialKind::OidcWorkload,
        instance_iri: None,
        subject: if sub.is_empty() { candidates.first().cloned().unwrap_or_default() } else { sub },
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
}

/// Find the Instance that declared one of these OIDC client ids (`tar:oidcClientId`).
/// When the Instance also declares `tar:oidcIssuer`, the issuer must match — a client id is
/// only unique within an issuer.
pub async fn find_instance_for_client(
    state: &Arc<AppState>,
    issuer: &str,
    candidates: &[String],
) -> AppResult<Option<InstanceBinding>> {
    if candidates.is_empty() {
        return Ok(None);
    }
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
        if let Some(declared) = row.str("iss") {
            if declared.trim_end_matches('/') != issuer {
                continue;
            }
        }
        let Some(iri) = row.iri("i") else { continue };
        return Ok(Some(InstanceBinding {
            instance_iri: iri,
            label: row.str("label"),
            allowed_scopes: row
                .str("scopes")
                .map(|s| s.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
        }));
    }
    Ok(None)
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
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"iss":"https://kc/realms/ids/"}"#);
        let token = format!("aaa.{payload}.bbb");
        assert_eq!(unverified_issuer(&token).as_deref(), Some("https://kc/realms/ids"));
    }
}
pub use jsonwebtoken;
