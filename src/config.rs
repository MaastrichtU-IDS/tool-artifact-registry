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
    pub oidc: OidcConfig,
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
