//! Tool Artifact Registry — an RDF-native, self-hostable, federatable registry of tools,
//! deployments, runs and data artifacts.
//!
//! See `docs/specs/2026-08-30-tool-artifact-registry-design.md` for the design this
//! implements, and `README.md` for what the prototype does and does not yet cover.

pub mod api;
pub mod auth;
pub mod config;
pub mod domain;
pub mod error;
pub mod ids;
pub mod model;
pub mod negotiate;
pub mod ns;
pub mod ops;
pub mod rdf;
pub mod seed;
pub mod shacl;
pub mod state;
pub mod store;

pub use config::Config;
pub use state::AppState;

/// Build the router with a ready state — the entry point for both `main` and the tests.
pub fn app(state: std::sync::Arc<AppState>) -> axum::Router {
    api::router(state)
}
