//! Authentication and authorisation (spec §8, extended by the workload-identity addendum).
//!
//! Three credential types reach this module, and all three end up as one [`Principal`]:
//!
//! 1. **OIDC workload token** (recommended). A tool authenticates to its *own* identity
//!    provider — Keycloak `client_credentials`, a Kubernetes projected ServiceAccount token,
//!    GitHub Actions OIDC — and presents the resulting JWT. We verify it against the issuer's
//!    JWKS and map a claim (`azp` by default) to the `Instance` that declared that client id.
//!    The registry stores no secret for that tool and never has to rotate one.
//! 2. **OIDC human token** — same verification, but the principal is a person with roles.
//! 3. **Local `tar_…` token** — an Argon2id-hashed opaque token minted per Instance. This is
//!    the zero-dependency fallback that keeps requirement 6 ("anyone can run their own")
//!    true when no identity provider exists.
//!
//! Whatever the credential, the rule of spec §8.3 is enforced identically: an Instance may
//! only advertise runs in which it is itself the agent, and the Instance is taken from the
//! credential, never from the request body.

pub mod jwt;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Scopes (spec §8.2).
pub const SCOPE_ADVERTISE_PRODUCE: &str = "advertise:produce";
pub const SCOPE_ADVERTISE_CONSUME: &str = "advertise:consume";
pub const SCOPE_REGISTER_SOFTWARE: &str = "register:software";
pub const SCOPE_REGISTER_INSTANCE: &str = "register:instance";
pub const SCOPE_READ_PRIVATE: &str = "read:private";
pub const SCOPE_ADMIN: &str = "admin:*";

pub const ALL_SCOPES: [&str; 6] = [
    SCOPE_ADVERTISE_PRODUCE,
    SCOPE_ADVERTISE_CONSUME,
    SCOPE_REGISTER_SOFTWARE,
    SCOPE_REGISTER_INSTANCE,
    SCOPE_READ_PRIVATE,
    SCOPE_ADMIN,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialKind {
    None,
    /// Opaque `tar_…` token, Argon2id-hashed in SQLite.
    LocalToken,
    /// JWT from a trusted OIDC issuer, bound to an Instance by client id.
    OidcWorkload,
    /// JWT from the human issuer, carrying roles.
    OidcHuman,
    /// `TAR_ROOT_TOKEN` — bootstrap only.
    RootToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Reader,
    Curator,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "reader" | "tar-reader" | "tar_reader" => Some(Role::Reader),
            "curator" | "tar-curator" | "tar_curator" => Some(Role::Curator),
            "admin" | "tar-admin" | "tar_admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Principal {
    pub credential: CredentialKind,
    /// The Instance IRI this credential acts as, when it is a deployment.
    pub instance_iri: Option<String>,
    /// Stable subject identifier for the audit log.
    pub subject: String,
    pub display_name: Option<String>,
    pub scopes: BTreeSet<String>,
    pub roles: BTreeSet<Role>,
    /// Issuer, for OIDC principals — worth showing in the UI and the audit log.
    pub issuer: Option<String>,
}

impl Principal {
    pub fn anonymous() -> Self {
        Self {
            credential: CredentialKind::None,
            instance_iri: None,
            subject: "anonymous".into(),
            display_name: None,
            scopes: BTreeSet::new(),
            roles: BTreeSet::new(),
            issuer: None,
        }
    }

    pub fn is_anonymous(&self) -> bool {
        self.credential == CredentialKind::None
    }

    pub fn is_admin(&self) -> bool {
        self.roles.contains(&Role::Admin) || self.scopes.contains(SCOPE_ADMIN)
    }

    pub fn is_curator(&self) -> bool {
        self.is_admin() || self.roles.contains(&Role::Curator)
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.is_admin() || self.scopes.contains(scope)
    }

    pub fn actor_kind(&self) -> &'static str {
        match self.credential {
            CredentialKind::None => "anonymous",
            CredentialKind::LocalToken => "instance-token",
            CredentialKind::OidcWorkload => "oidc-workload",
            CredentialKind::OidcHuman => "oidc-human",
            CredentialKind::RootToken => "root-token",
        }
    }

    // ------------------------------------------------------------- guards

    pub fn require_authenticated(&self) -> AppResult<()> {
        if self.is_anonymous() {
            return Err(AppError::unauthorized(
                "this endpoint needs a credential: an OIDC token from a trusted issuer, or a registry API token",
            ));
        }
        Ok(())
    }

    pub fn require_scope(&self, scope: &str) -> AppResult<()> {
        self.require_authenticated()?;
        if self.has_scope(scope) {
            return Ok(());
        }
        Err(AppError::forbidden(format!(
            "credential lacks the {scope} scope (has: {})",
            if self.scopes.is_empty() { "none".to_string() } else { self.scopes.iter().cloned().collect::<Vec<_>>().join(" ") }
        )))
    }

    pub fn require_curator(&self) -> AppResult<()> {
        self.require_authenticated()?;
        if self.is_curator() || self.has_scope(SCOPE_REGISTER_SOFTWARE) {
            return Ok(());
        }
        Err(AppError::forbidden("curator role required"))
    }

    pub fn require_admin(&self) -> AppResult<()> {
        self.require_authenticated()?;
        if self.is_admin() {
            return Ok(());
        }
        Err(AppError::forbidden("admin role required"))
    }

    /// Spec §8.3. The Instance is the credential's, full stop.
    pub fn require_instance(&self) -> AppResult<String> {
        self.require_authenticated()?;
        self.instance_iri.clone().ok_or_else(|| {
            AppError::forbidden(
                "this credential does not act as an Instance; advertisement must be authenticated \
                 as the deployment that performed the run (spec §8.3)",
            )
        })
    }
}

/// Axum extractor. Never fails — an absent or unusable credential yields the anonymous
/// principal, and each handler decides what it needs. Read stays anonymous by default.
impl FromRequestParts<Arc<AppState>> for Principal {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let Some(raw) = bearer(parts) else { return Ok(Principal::anonymous()) };
        authenticate(state, &raw).await
    }
}

fn bearer(parts: &Parts) -> Option<String> {
    let h = parts.headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, value) = h.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| value.trim().to_string())
}

pub async fn authenticate(state: &Arc<AppState>, raw: &str) -> AppResult<Principal> {
    // Bootstrap admin (spec §8.2). Constant-time-ish comparison on equal lengths.
    if let Some(root) = &state.config.root_token {
        if raw.len() == root.len() && constant_time_eq(raw.as_bytes(), root.as_bytes()) {
            return Ok(Principal {
                credential: CredentialKind::RootToken,
                instance_iri: None,
                subject: "urn:tar:root".into(),
                display_name: Some("bootstrap admin".into()),
                scopes: ALL_SCOPES.iter().map(|s| s.to_string()).collect(),
                roles: [Role::Admin].into_iter().collect(),
                issuer: None,
            });
        }
    }

    if raw.starts_with("tar_") {
        let Some(rec) = state.ops.verify_token(raw).await.map_err(AppError::from)? else {
            return Err(AppError::unauthorized("token is unknown, revoked or expired"));
        };
        return Ok(Principal {
            credential: CredentialKind::LocalToken,
            instance_iri: rec.instance_iri.clone(),
            subject: rec.instance_iri.clone().unwrap_or_else(|| format!("urn:tar:token:{}", rec.id)),
            display_name: rec.label.clone(),
            scopes: rec.scopes.iter().cloned().collect(),
            roles: BTreeSet::new(),
            issuer: None,
        });
    }

    // Anything else with three dot-separated segments is treated as a JWT.
    if raw.matches('.').count() == 2 {
        return jwt::authenticate_jwt(state, raw).await;
    }

    Err(AppError::unauthorized("unrecognised credential format"))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
