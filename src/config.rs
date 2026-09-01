//! Configuration (spec §10.5).
//!
//! `TAR_BASE_IRI` is the only universally mandatory setting — IRIs cannot be minted without
//! it. Everything else has a working default so that `docker run` with one variable is a
//! complete install (requirement 6).

use anyhow::{bail, Context, Result};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub base_iri: String,
    pub data_dir: String,
    pub listen: String,
    pub public_read: bool,
    /// SPARQL is a public read surface in its own right (spec §7.7): a standard query language
    /// is most of its value to analysts and peers, and losing it whenever an operator closes
    /// REST reads would make the two settings one. Defaults open, and stays open when
    /// `public_read` is false, so an operator who wants a genuinely private registry has to say
    /// so about the query endpoint too.
    pub sparql_public: bool,
    pub root_token: Option<String>,
    pub shacl_validate_writes: bool,
    pub max_payload_bytes: usize,
    pub static_dir: Option<String>,
    pub title: String,
    pub operator: Option<String>,
    pub peer_resolve_enabled: bool,
    pub peer_resolve_ttl: Duration,
    pub peer_resolve_timeout: Duration,
    pub federated_search_timeout: Duration,
    /// An external SPARQL 1.1 endpoint to use *instead of* the embedded store. Absent — the
    /// default, and what everybody running this today has — means embedded Oxigraph under
    /// `data_dir`, unchanged.
    pub sparql_backend: Option<SparqlBackend>,
    pub oidc: OidcConfig,
}

/// A remote graph store, reached over SPARQL 1.1 Query and Update.
///
/// This is a *backend* connection and has nothing to do with `/sparql`, the registry's own
/// read-only query surface. `/sparql` stays read-only whichever backend is configured.
#[derive(Clone, Debug)]
pub struct SparqlBackend {
    pub query_endpoint: String,
    /// Many servers split query and update onto separate URLs (Fuseki's `/ds/sparql` and
    /// `/ds/update`), so this is configured separately and defaults to the query endpoint for
    /// the servers that do not.
    pub update_endpoint: String,
    pub auth: SparqlAuth,
    pub timeout: Duration,
}

/// How the registry authenticates to its graph store. Never guessed: a credential is used
/// only when it is configured, and configuring both forms is an error rather than a silent
/// preference.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SparqlAuth {
    #[default]
    None,
    Bearer(String),
    Basic {
        username: String,
        password: String,
    },
}

impl SparqlBackend {
    fn from_env(endpoint: String) -> Result<Self> {
        let bearer = env("TAR_SPARQL_BEARER_TOKEN");
        let username = env("TAR_SPARQL_USERNAME");
        let password = env("TAR_SPARQL_PASSWORD");
        let auth = match (bearer, username, password) {
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => bail!(
                "TAR_SPARQL_BEARER_TOKEN and TAR_SPARQL_USERNAME/TAR_SPARQL_PASSWORD are both set \
                 — pick one; the registry will not choose a credential for you"
            ),
            (Some(t), None, None) => SparqlAuth::Bearer(t),
            (None, Some(u), Some(p)) => SparqlAuth::Basic { username: u, password: p },
            (None, Some(_), None) => {
                bail!("TAR_SPARQL_USERNAME is set without TAR_SPARQL_PASSWORD")
            }
            (None, None, Some(_)) => {
                bail!("TAR_SPARQL_PASSWORD is set without TAR_SPARQL_USERNAME")
            }
            (None, None, None) => SparqlAuth::None,
        };
        if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
            bail!("TAR_SPARQL_ENDPOINT must be an http(s) URL, got {endpoint:?}");
        }
        let update_endpoint = env("TAR_SPARQL_UPDATE_ENDPOINT").unwrap_or_else(|| endpoint.clone());
        Ok(Self {
            query_endpoint: endpoint,
            update_endpoint,
            auth,
            // Generous next to the peer-resolution timeouts: this is the registry's own
            // storage, and a boot-time bulk load of the bundled vocabularies is one request.
            timeout: env_duration("TAR_SPARQL_TIMEOUT", "60s")?,
        })
    }

    /// What `tar config` and the logs say. Never the credential itself.
    pub fn describe(&self) -> String {
        let auth = match &self.auth {
            SparqlAuth::None => "no credential",
            SparqlAuth::Bearer(_) => "bearer token",
            SparqlAuth::Basic { .. } => "basic auth",
        };
        if self.update_endpoint == self.query_endpoint {
            format!("{} ({auth})", self.query_endpoint)
        } else {
            format!("query {} / update {} ({auth})", self.query_endpoint, self.update_endpoint)
        }
    }
}

/// Workload and human identity via OIDC (spec §8, workload-identity addendum).
///
/// The registry never holds a client secret for a workload: a tool authenticates to its own
/// identity provider (Keycloak, a Kubernetes API server, GitHub Actions) and presents the
/// resulting JWT here. We verify the signature against the issuer's JWKS and map a claim to
/// the `Instance` that is allowed to advertise.
#[derive(Clone, Debug, Default)]
pub struct OidcConfig {
    /// Primary issuer — used for both browser sign-in and workload tokens.
    pub issuer: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// Additional issuers accepted for *workload* tokens only (k8s, GitHub Actions, a
    /// partner's Keycloak). Never used for browser sign-in.
    pub workload_issuers: Vec<String>,
    /// Expected `aud`. Defaults to the registry base IRI.
    pub audience: Option<String>,
    pub require_audience: bool,
    /// Claim carrying the OIDC client id of the calling workload. Keycloak: `azp`.
    pub client_claim: String,
    /// Claim carrying human roles. Keycloak realm roles: `realm_access.roles`.
    pub roles_claim: String,
    /// Claim carrying granted scopes (space separated string or array).
    pub scope_claim: String,
    /// Map a verified workload token to an Instance even when no Instance declares that
    /// client id yet. Off by default: an unknown workload should be registered explicitly.
    pub auto_register_instances: bool,
    /// Accept unsigned/short-circuit verification. Test-only; refuses to be set in release
    /// deployments unless `TAR_DEV_INSECURE_JWT=1` is explicit.
    pub dev_insecure: bool,
}

impl OidcConfig {
    pub fn enabled(&self) -> bool {
        self.issuer.is_some() || !self.workload_issuers.is_empty()
    }
    /// Every issuer whose tokens may authenticate a workload.
    pub fn accepted_issuers(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(i) = &self.issuer {
            v.push(i.trim_end_matches('/').to_string());
        }
        for i in &self.workload_issuers {
            let i = i.trim_end_matches('/').to_string();
            if !v.contains(&i) {
                v.push(i);
            }
        }
        v
    }
    pub fn human_signin_enabled(&self) -> bool {
        self.issuer.is_some() && self.client_id.is_some()
    }

    /// The issuer a credential binding is read against when the record does not name one.
    ///
    /// A client id is only unique *within* an issuer: "ontoexplorer-prod" at Keycloak and
    /// "ontoexplorer-prod" at a partner's identity provider are two different principals that
    /// happen to spell their name the same way. A record that names a client id but no issuer
    /// therefore does not identify anybody on its own, and the registry has to supply the
    /// missing half from somewhere.
    ///
    /// It supplies the *primary* issuer, or the sole workload issuer when that is the only one
    /// configured — the unambiguous cases. When several issuers are accepted and the record
    /// pins none, there is no honest answer: any of them could have minted that client id, and
    /// picking one would be a guess that silently grants a credential from the weakest accepted
    /// issuer (a CI runner, a shared cluster) the authority intended for the strongest. That
    /// case returns `None`, and the binding is refused until a curator pins the issuer.
    pub fn default_binding_issuer(&self) -> Option<String> {
        if let Some(i) = &self.issuer {
            return Some(i.trim_end_matches('/').to_string());
        }
        match self.workload_issuers.as_slice() {
            [only] => Some(only.trim_end_matches('/').to_string()),
            _ => None,
        }
    }

    /// Whether a record that names a credential must also pin its issuer to be usable.
    ///
    /// True exactly when [`default_binding_issuer`](Self::default_binding_issuer) cannot answer
    /// — which is what the write path warns about, so the refusal arrives when the record is
    /// created rather than as a 403 the first time the workload calls.
    pub fn issuer_pin_required(&self) -> bool {
        self.enabled() && self.default_binding_issuer().is_none()
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    match env(key) {
        Some(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        None => default,
    }
}

/// Accepts `30s`, `5m`, `24h`, `7d`, or a bare number of seconds.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1u64),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86400),
        _ => (s, 1),
    };
    let n: u64 = num.trim().parse().with_context(|| format!("bad duration {s:?}"))?;
    Ok(Duration::from_secs(n * mult))
}

fn env_duration(key: &str, default: &str) -> Result<Duration> {
    parse_duration(&env(key).unwrap_or_else(|| default.to_string()))
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let base_iri = env("TAR_BASE_IRI").context(
            "TAR_BASE_IRI is required — the registry cannot mint dereferenceable IRIs without it",
        )?;
        let base_iri = base_iri.trim_end_matches('/').to_string();
        if !(base_iri.starts_with("http://") || base_iri.starts_with("https://")) {
            bail!("TAR_BASE_IRI must be an http(s) URL, got {base_iri:?}");
        }

        let root_token = env("TAR_ROOT_TOKEN");
        if let Some(t) = &root_token {
            // §8.2: refuses to start with a default or empty value.
            const REFUSED: [&str; 6] = ["changeme", "change-me", "root", "admin", "secret", "tar"];
            if REFUSED.contains(&t.to_ascii_lowercase().as_str()) || t.len() < 16 {
                bail!("TAR_ROOT_TOKEN is a default or too short (need >= 16 chars, not a placeholder)");
            }
        }

        let oidc = OidcConfig {
            issuer: env("TAR_OIDC_ISSUER").map(|s| s.trim_end_matches('/').to_string()),
            client_id: env("TAR_OIDC_CLIENT_ID"),
            client_secret: env("TAR_OIDC_CLIENT_SECRET"),
            workload_issuers: env("TAR_WORKLOAD_ISSUERS")
                .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
                .unwrap_or_default(),
            audience: env("TAR_OIDC_AUDIENCE").or_else(|| Some(base_iri.clone())),
            require_audience: env_bool("TAR_OIDC_REQUIRE_AUDIENCE", true),
            client_claim: env("TAR_OIDC_CLIENT_CLAIM").unwrap_or_else(|| "azp".into()),
            roles_claim: env("TAR_OIDC_ROLES_CLAIM").unwrap_or_else(|| "realm_access.roles".into()),
            scope_claim: env("TAR_OIDC_SCOPE_CLAIM").unwrap_or_else(|| "scope".into()),
            auto_register_instances: env_bool("TAR_OIDC_AUTO_REGISTER_INSTANCES", false),
            dev_insecure: env_bool("TAR_DEV_INSECURE_JWT", false),
        };

        Ok(Self {
            base_iri,
            data_dir: env("TAR_DATA_DIR").unwrap_or_else(|| "./data".into()),
            listen: env("TAR_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()),
            public_read: env_bool("TAR_PUBLIC_READ", true),
            sparql_public: env_bool("TAR_SPARQL_PUBLIC", true),
            root_token,
            shacl_validate_writes: env_bool("TAR_SHACL_VALIDATE_WRITES", true),
            max_payload_bytes: env("TAR_MAX_PAYLOAD_BYTES")
                .and_then(|v| parse_bytes(&v))
                .unwrap_or(2 * 1024 * 1024),
            static_dir: env("TAR_STATIC_DIR").or_else(|| {
                let d = "frontend/dist";
                std::path::Path::new(d).is_dir().then(|| d.to_string())
            }),
            title: env("TAR_TITLE").unwrap_or_else(|| "Tool Artifact Registry".into()),
            operator: env("TAR_OPERATOR"),
            peer_resolve_enabled: env_bool("TAR_PEER_RESOLVE_ENABLED", true),
            peer_resolve_ttl: env_duration("TAR_PEER_RESOLVE_TTL", "24h")?,
            peer_resolve_timeout: env_duration("TAR_PEER_RESOLVE_TIMEOUT", "5s")?,
            federated_search_timeout: env_duration("TAR_FEDERATED_SEARCH_TIMEOUT", "3s")?,
            // "if user doesn't provide a sparql endpoint, fall back to oxigraph" — so the
            // absence of one variable is the whole switch, and an existing install changes
            // nothing.
            sparql_backend: env("TAR_SPARQL_ENDPOINT").map(SparqlBackend::from_env).transpose()?,
            oidc,
        })
    }

    /// A config for tests and for `tar seed` against a temporary store.
    pub fn for_test(base_iri: &str) -> Self {
        Self {
            base_iri: base_iri.trim_end_matches('/').to_string(),
            data_dir: "memory".into(),
            listen: "127.0.0.1:0".into(),
            public_read: true,
            sparql_public: true,
            root_token: Some("test-root-token-0123456789".into()),
            shacl_validate_writes: true,
            max_payload_bytes: 2 * 1024 * 1024,
            static_dir: None,
            title: "Test Registry".into(),
            operator: None,
            peer_resolve_enabled: false,
            peer_resolve_ttl: Duration::from_secs(86400),
            peer_resolve_timeout: Duration::from_secs(5),
            federated_search_timeout: Duration::from_secs(3),
            sparql_backend: None,
            oidc: OidcConfig { client_claim: "azp".into(), roles_claim: "realm_access.roles".into(), scope_claim: "scope".into(), require_audience: true, ..Default::default() },
        }
    }
}

fn parse_bytes(v: &str) -> Option<usize> {
    let v = v.trim();
    let (n, m) = if let Some(x) = v.strip_suffix("MiB") {
        (x, 1024 * 1024)
    } else if let Some(x) = v.strip_suffix("KiB") {
        (x, 1024)
    } else if let Some(x) = v.strip_suffix("MB") {
        (x, 1_000_000)
    } else {
        (v, 1)
    };
    n.trim().parse::<usize>().ok().map(|n| n * m)
}

#[cfg(test)]
mod binding_issuer_tests {
    use super::*;

    fn cfg(primary: Option<&str>, workload: &[&str]) -> OidcConfig {
        OidcConfig {
            issuer: primary.map(str::to_string),
            workload_issuers: workload.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// The two readings with one obvious answer, and the one without.
    #[test]
    fn an_unpinned_binding_resolves_only_when_one_issuer_is_the_obvious_reading() {
        let primary = "https://kc.example/realms/ids";
        let ci = "https://token.actions.githubusercontent.com";

        // A primary issuer is what an unpinned binding means, whatever else is accepted.
        assert_eq!(cfg(Some(primary), &[]).default_binding_issuer().as_deref(), Some(primary));
        assert_eq!(cfg(Some(primary), &[ci]).default_binding_issuer().as_deref(), Some(primary));

        // No primary, one workload issuer: still unambiguous.
        assert_eq!(cfg(None, &[ci]).default_binding_issuer().as_deref(), Some(ci));

        // No primary and several: no honest answer, so none is given.
        assert_eq!(cfg(None, &[ci, "https://partner.example/realms/theirs"]).default_binding_issuer(), None);

        // A trailing slash is not a different issuer.
        assert_eq!(
            cfg(Some(&format!("{primary}/")), &[]).default_binding_issuer().as_deref(),
            Some(primary)
        );
    }

    #[test]
    fn a_pin_is_demanded_exactly_where_the_default_cannot_answer() {
        let ci = "https://token.actions.githubusercontent.com";
        assert!(!cfg(Some("https://kc.example/realms/ids"), &[ci]).issuer_pin_required());
        assert!(!cfg(None, &[ci]).issuer_pin_required());
        assert!(cfg(None, &[ci, "https://partner.example/realms/theirs"]).issuer_pin_required());
        // OIDC off entirely: there is no binding to pin.
        assert!(!cfg(None, &[]).issuer_pin_required());
    }
}
