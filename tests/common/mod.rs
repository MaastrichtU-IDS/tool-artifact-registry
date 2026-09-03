//! The graph store the integration suite runs against.
//!
//! `cargo test` runs every test on the embedded Oxigraph store, exactly as before — that is
//! the regression test for expressing the bespoke reads as SPARQL.
//!
//! Set `TAR_TEST_SPARQL_ENDPOINT` to a Fuseki base URL and the *same* tests run against a real
//! SPARQL 1.1 server instead, each harness getting its own freshly created dataset:
//!
//! ```text
//! docker run --rm -d --name tar-fuseki -p 127.0.0.1:3131:3030 -e ADMIN_PASSWORD=admin \
//!   stain/jena-fuseki:latest
//! TAR_TEST_SPARQL_ENDPOINT=http://127.0.0.1:3131 cargo test --test api
//! ```
//!
//! Equivalence between the two backends is then enforced by the suite rather than argued for
//! in a report. `TAR_TEST_SPARQL_AUTH` overrides the admin credential (default `admin:admin`),
//! which is also what exercises the basic-auth path.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tar::config::{SparqlAuth, SparqlBackend};
use tar::store::{GraphStore, HttpSparqlStore, OxigraphStore};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub async fn test_store() -> Arc<dyn GraphStore> {
    let Ok(base) = std::env::var("TAR_TEST_SPARQL_ENDPOINT") else {
        return Arc::new(OxigraphStore::memory().unwrap());
    };
    let base = base.trim_end_matches('/').to_string();
    let credential = std::env::var("TAR_TEST_SPARQL_AUTH").unwrap_or_else(|_| "admin:admin".into());
    let (username, password) = credential.split_once(':').expect("TAR_TEST_SPARQL_AUTH is user:password");

    // One dataset per harness: tests share a process and run in parallel, so anything less is
    // one test seeing another's records.
    let name = format!("t{}x{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let created = reqwest::Client::new()
        .post(format!("{base}/$/datasets"))
        .basic_auth(username, Some(password))
        .form(&[("dbName", name.as_str()), ("dbType", "mem")])
        .send()
        .await
        .unwrap_or_else(|e| panic!("creating the test dataset at {base}: {e}"));
    assert!(created.status().is_success(), "creating dataset {name}: {}", created.status());

    Arc::new(
        HttpSparqlStore::connect(SparqlBackend {
            query_endpoint: format!("{base}/{name}/sparql"),
            update_endpoint: format!("{base}/{name}/update"),
            auth: SparqlAuth::Basic { username: username.to_string(), password: password.to_string() },
            timeout: Duration::from_secs(120),
        })
        .unwrap(),
    )
}

/// A [`GraphStore`] that records what was asked of it and passes it straight through.
///
/// "The bundles are not reloaded on an unchanged boot" cannot be shown by comparing the store
/// before and after: the end state is identical either way, which is exactly why the waste went
/// unnoticed. The only honest evidence is the calls, so this counts them.
///
/// It wraps whichever backend the suite is running against, so the same assertions hold over
/// HTTP to Fuseki, where a write is a request and a query is a round trip.
pub struct CountingStore {
    inner: Arc<dyn GraphStore>,
    calls: std::sync::Mutex<Calls>,
}

#[derive(Debug, Default, Clone)]
pub struct Calls {
    /// Every SELECT, verbatim — so a test can assert what was *not* asked.
    pub selects: Vec<String>,
    pub asks: Vec<String>,
    /// Transactions applied, and the quads they carried.
    pub applies: usize,
    pub inserted: usize,
    /// Bulk loads, and the graphs dropped.
    pub loads: usize,
    pub drops: Vec<String>,
}

impl Calls {
    /// Anything that changes the store.
    pub fn writes(&self) -> usize {
        self.applies + self.loads + self.drops.len()
    }
    /// Whether any query mentions the given text — used to show that a record write asks the
    /// record store nothing about the vocabulary.
    pub fn queried(&self, needle: &str) -> bool {
        self.selects.iter().chain(self.asks.iter()).any(|q| q.contains(needle))
    }
}

impl CountingStore {
    pub fn wrap(inner: Arc<dyn GraphStore>) -> Arc<Self> {
        Arc::new(Self { inner, calls: std::sync::Mutex::new(Calls::default()) })
    }
    pub fn calls(&self) -> Calls {
        self.calls.lock().unwrap().clone()
    }
    pub fn reset(&self) {
        *self.calls.lock().unwrap() = Calls::default();
    }
    fn note(&self, f: impl FnOnce(&mut Calls)) {
        f(&mut self.calls.lock().unwrap());
    }
}

impl GraphStore for CountingStore {
    fn select(&self, sparql: &str) -> anyhow::Result<tar::store::Bindings> {
        self.note(|c| c.selects.push(sparql.to_string()));
        self.inner.select(sparql)
    }
    fn ask(&self, sparql: &str) -> anyhow::Result<bool> {
        self.note(|c| c.asks.push(sparql.to_string()));
        self.inner.ask(sparql)
    }
    fn construct(&self, sparql: &str) -> anyhow::Result<Vec<oxigraph::model::Triple>> {
        self.note(|c| c.selects.push(sparql.to_string()));
        self.inner.construct(sparql)
    }
    fn apply(&self, tx: tar::store::GraphTx) -> anyhow::Result<()> {
        self.note(|c| {
            c.applies += 1;
            c.inserted += tx.insert.len();
        });
        self.inner.apply(tx)
    }
    fn drop_graph(&self, graph: &str) -> anyhow::Result<()> {
        self.note(|c| c.drops.push(graph.to_string()));
        self.inner.drop_graph(graph)
    }
    fn dump_nquads(&self, graph: Option<&str>) -> anyhow::Result<String> {
        self.inner.dump_nquads(graph)
    }
    fn load_turtle(&self, data: &str, graph: &str, base: Option<&str>) -> anyhow::Result<usize> {
        self.note(|c| c.loads += 1);
        self.inner.load_turtle(data, graph, base)
    }
    fn load_nquads(&self, data: &str) -> anyhow::Result<usize> {
        self.note(|c| c.loads += 1);
        self.inner.load_nquads(data)
    }
}
