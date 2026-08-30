//! Shared application state.

use crate::auth::jwt::JwtVerifier;
use crate::config::Config;
use crate::ops::Ops;
use crate::store::{GraphStore, OxigraphStore};
use std::sync::Arc;

pub struct AppState {
    pub config: Config,
    pub store: Arc<dyn GraphStore>,
    pub ops: Ops,
    pub jwt: JwtVerifier,
    pub http: reqwest::Client,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub version: &'static str,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Arc<Self>> {
        let graph_path = if config.data_dir == "memory" {
            "memory".to_string()
        } else {
            format!("{}/graph", config.data_dir.trim_end_matches('/'))
        };
        let ops_path = if config.data_dir == "memory" {
            ":memory:".to_string()
        } else {
            format!("{}/ops.db", config.data_dir.trim_end_matches('/'))
        };
        let store = Arc::new(OxigraphStore::open(&graph_path)?);
        let ops = Ops::open(&ops_path).await?;
        Ok(Arc::new(Self::from_parts(config, store, ops)))
    }

    pub fn from_parts(config: Config, store: Arc<dyn GraphStore>, ops: Ops) -> Self {
        let timeout = config.peer_resolve_timeout;
        Self {
            jwt: JwtVerifier::new(timeout),
            http: reqwest::Client::builder()
                .timeout(timeout)
                .user_agent(concat!("tool-artifact-registry/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default(),
            store,
            ops,
            config,
            started_at: chrono::Utc::now(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    pub fn base(&self) -> &str {
        &self.config.base_iri
    }
}
