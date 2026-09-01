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
            auth: SparqlAuth::Basic {
                username: username.to_string(),
                password: password.to_string(),
            },
            timeout: Duration::from_secs(120),
        })
        .unwrap(),
    )
}
