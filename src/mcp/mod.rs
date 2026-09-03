//! A **hosted** Model Context Protocol server, mounted on the registry's own axum router.
//!
//! Protocol revision implemented: **`2026-07-28`** (Streamable HTTP), with legacy fallback for
//! `2025-11-25`, `2025-06-18` and `2025-03-26`. Worked from
//! <https://modelcontextprotocol.io/specification/2026-07-28/> — in particular
//! `basic/transports/streamable-http`, `basic/versioning`, `basic/authorization` and
//! `basic/authorization/authorization-server-discovery`.
//!
//! # Why it is mounted rather than shipped as a binary
//!
//! There is already a web server with authentication, SHACL validation and a vocabulary index.
//! Asking a user to `cargo install tar` to reach a registry they can already reach over HTTPS
//! adds an install step, a version skew and a second copy of the authorisation rules. So this
//! is one route on the same process, at the same origin, behind the same credentials:
//! `POST {base}/mcp`.
//!
//! # How a tool call is executed
//!
//! [`call`] does not reimplement any registry operation. It builds an ordinary HTTP request,
//! carrying the caller's own `Authorization` header verbatim, and dispatches it through
//! [`crate::api::router`] in-process. The REST handler then runs its own `require_curator()` /
//! `require_scope()` check, its own SHACL validation and its own audit write. That makes
//! "a tool call can never do more than that credential could do through the REST API" a
//! structural property rather than a convention: there is no second code path to keep in step.
//!
//! # Configuration (all optional, read from the environment)
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `TAR_MCP_ENABLED` | `true` | Serve the MCP endpoint at all. |
//! | `TAR_MCP_READ_ONLY` | `false` | Hide and refuse every write tool. |
//! | `TAR_MCP_ALLOWED_ORIGINS` | base IRI | Comma-separated extra `Origin` values to accept. |

pub mod call;
pub mod oauth;
pub mod rpc;
pub mod tools;
pub mod transport;

use crate::state::AppState;
use axum::routing::get;
use axum::Router;
use std::sync::Arc;

/// The MCP endpoint path, relative to the registry base IRI.
pub const ENDPOINT_PATH: &str = "/mcp";

/// Server identity reported in `server/discover` and `initialize`.
pub const SERVER_NAME: &str = "tool-artifact-registry";

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => {
            matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
        }
        _ => default,
    }
}

/// Runtime switches. Read per request rather than cached, so a test can flip them.
#[derive(Debug, Clone, Copy)]
pub struct McpConfig {
    pub enabled: bool,
    pub read_only: bool,
}

impl McpConfig {
    pub fn from_env() -> Self {
        Self { enabled: env_bool("TAR_MCP_ENABLED", true), read_only: env_bool("TAR_MCP_READ_ONLY", false) }
    }
}

/// The canonical resource identifier of this MCP server, as RFC 8707 §2 defines it and
/// RFC 9728 echoes it. Clients put this in the `resource` parameter of the OAuth request, and
/// the registry's `aud` check is against the base IRI, which this must therefore agree with.
pub fn resource_uri(state: &AppState) -> String {
    format!("{}{}", state.base(), ENDPOINT_PATH)
}

/// Routes to merge into the registry router.
///
/// Three of them: the MCP endpoint itself, and the two RFC 9728 well-known locations a client
/// is required to probe — the path-inserted one for `/mcp` and the one at the root.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            ENDPOINT_PATH,
            axum::routing::post(transport::endpoint)
                // 2026-07-28 removed the standalone GET stream and the DELETE session
                // teardown; the spec asks a modern-only endpoint to answer both with 405.
                .get(transport::method_not_allowed)
                .delete(transport::method_not_allowed),
        )
        .route("/.well-known/oauth-protected-resource", get(oauth::protected_resource_metadata))
        .route("/.well-known/oauth-protected-resource/mcp", get(oauth::protected_resource_metadata))
}
