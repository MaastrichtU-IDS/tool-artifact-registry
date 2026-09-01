//! Shared application state.

use crate::auth::jwt::JwtVerifier;
use crate::config::Config;
use crate::ops::Ops;
use crate::shacl::Shapes;
use crate::store::GraphStore;
use std::sync::Arc;

pub struct AppState {
    pub config: Config,
    /// The records: `urn:tar:local`, the peer caches, and a copy of the bundled reference data
    /// so `/sparql` can join a record to the term it cites. Embedded Oxigraph, or the external
    /// SPARQL endpoint `TAR_SPARQL_ENDPOINT` names.
    pub store: Arc<dyn GraphStore>,
    /// The bundled reference data, in memory, always (`crate::bundles`). Every hot reference
    /// read goes here first — above all `domain::vocabulary::held`, which the SHACL write path
    /// calls on every write and which used to be an HTTP round trip per write on a remote
    /// backend. It is read-only, it holds no record and no peer statement, and a term it does
    /// not have is looked for in `store` instead.
    pub reference: Arc<dyn GraphStore>,
    pub ops: Ops,
    pub jwt: JwtVerifier,
    /// The shape set every write is validated against (spec §5.3).
    pub shapes: Shapes,
    pub http: reqwest::Client,
    /// Fetched API descriptions, by URL. In memory and per process: it is a cache of somebody
    /// else's document, so losing it on restart costs one fetch.
    pub api_doc_cache: crate::api::apidocs::DocCache,
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
        let store = crate::store::open(config.sparql_backend.as_ref(), &graph_path)?;
        let ops = Ops::open(&ops_path).await?;
        Ok(Arc::new(Self::from_parts(config, store, ops)))
    }

    pub fn from_parts(config: Config, store: Arc<dyn GraphStore>, ops: Ops) -> Self {
        let timeout = config.peer_resolve_timeout;
        Self {
            jwt: JwtVerifier::new(timeout),
            // A registry that cannot parse its own shapes would accept anything, so this
            // fails loudly at construction rather than quietly at the first write.
            shapes: Shapes::parse(crate::bundles::SHAPES_TTL).expect("the shipped SHACL shapes must parse"),
            // Built here rather than at the first request, for the same reason: a registry that
            // cannot load its own reference data would refuse every write that names a term.
            reference: crate::bundles::reference_store(&config.base_iri),
            http: reqwest::Client::builder()
                .timeout(timeout)
                .user_agent(concat!("tool-artifact-registry/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default(),
            store,
            ops,
            config,
            api_doc_cache: Default::default(),
            started_at: chrono::Utc::now(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    pub fn base(&self) -> &str {
        &self.config.base_iri
    }
}
