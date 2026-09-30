//! Embedded Oxigraph implementation of [`GraphStore`] (spec D3, §5).

use anyhow::{anyhow, bail, Context, Result};
use once_cell::sync::Lazy;
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{GraphNameRef, NamedNode, NamedOrBlankNode, NamedOrBlankNodeRef, Quad, Term, Triple};
use oxigraph::sparql::{CancellationToken, QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use std::collections::{BinaryHeap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use super::queries::is_owned_subresource;
use super::{Bindings, GraphStore, GraphTx, Row};

pub struct OxigraphStore {
    store: Store,
    /// How long one query may run (`TAR_SPARQL_TIMEOUT`). `/sparql` runs queries a caller
    /// wrote, and without this a cross join pinned a CPU until it finished, however long.
    timeout: Duration,
}

/// Rows one query may return. A result set is built whole before it is answered, so this is
/// what keeps one query from holding the process's memory; a caller who needs more pages.
pub const MAX_ROWS: usize = 100_000;

/// The one thread that cancels queries at their deadline. A thread per query would cost more
/// than most queries; this one sleeps until the nearest deadline and cancels what has passed it.
static DEADLINES: Lazy<std::sync::mpsc::Sender<Deadline>> = Lazy::new(|| {
    let (tx, rx) = std::sync::mpsc::channel::<Deadline>();
    std::thread::Builder::new()
        .name("sparql-deadlines".into())
        .spawn(move || {
            let mut pending: BinaryHeap<std::cmp::Reverse<Deadline>> = BinaryHeap::new();
            loop {
                let now = Instant::now();
                while pending.peek().is_some_and(|d| d.0.at <= now) {
                    if let Some(d) = pending.pop() {
                        d.0.token.cancel();
                    }
                }
                let next = match pending.peek() {
                    Some(d) => rx.recv_timeout(d.0.at.saturating_duration_since(now)),
                    None => rx.recv().map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
                };
                match next {
                    Ok(d) => pending.push(std::cmp::Reverse(d)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("the SPARQL deadline thread must start");
    tx
});

struct Deadline {
    at: Instant,
    seq: u64,
    token: CancellationToken,
}
impl PartialEq for Deadline {
    fn eq(&self, o: &Self) -> bool {
        (self.at, self.seq) == (o.at, o.seq)
    }
}
impl Eq for Deadline {}
impl PartialOrd for Deadline {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Deadline {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (self.at, self.seq).cmp(&(o.at, o.seq))
    }
}

/// A token that the deadline thread will cancel after `timeout`.
fn deadline(timeout: Duration) -> CancellationToken {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let token = CancellationToken::new();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = DEADLINES.send(Deadline { at: Instant::now() + timeout, seq, token: token.clone() });
    token
}

/// Say which limit a cancelled query met, rather than oxigraph's bare "cancelled".
fn explain(e: impl std::fmt::Display, timeout: Duration) -> anyhow::Error {
    let text = e.to_string();
    if text.to_ascii_lowercase().contains("cancel") {
        anyhow!("the query ran longer than TAR_SPARQL_TIMEOUT ({}s) and was stopped", timeout.as_secs())
    } else {
        anyhow!(text)
    }
}

impl OxigraphStore {
    /// `path == "memory"` opens a non-persistent store — used by tests and `tar seed --dry-run`.
    pub fn open(path: &str) -> Result<Self> {
        Self::open_with_timeout(path, Duration::from_secs(60))
    }

    pub fn open_with_timeout(path: &str, timeout: Duration) -> Result<Self> {
        let store = if path == "memory" {
            Store::new()?
        } else {
            let p = Path::new(path);
            std::fs::create_dir_all(p).with_context(|| format!("creating {path}"))?;
            Store::open(p).with_context(|| format!("opening graph store at {path}"))?
        };
        Ok(Self { store, timeout })
    }

    pub fn memory() -> Result<Self> {
        Ok(Self { store: Store::new()?, timeout: Duration::from_secs(60) })
    }

    /// The same store with a different query deadline; for tests.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn inner(&self) -> &Store {
        &self.store
    }

    fn subject_ref(iri: &str) -> Result<NamedNode> {
        NamedNode::new(iri).map_err(|e| anyhow!("bad IRI {iri}: {e}"))
    }
}

impl GraphStore for OxigraphStore {
    fn select(&self, sparql: &str) -> Result<Bindings> {
        let results = SparqlEvaluator::new()
            .with_cancellation_token(deadline(self.timeout))
            .parse_query(sparql)
            .with_context(|| format!("parsing SPARQL: {sparql}"))?
            .on_store(&self.store)
            .execute()
            .map_err(|e| explain(e, self.timeout))?;
        match results {
            QueryResults::Solutions(iter) => {
                let vars: Vec<String> = iter.variables().iter().map(|v| v.as_str().to_string()).collect();
                let mut rows = Vec::new();
                for sol in iter {
                    let sol = sol.map_err(|e| explain(e, self.timeout))?;
                    if rows.len() == MAX_ROWS {
                        bail!("the query returns more than {MAX_ROWS} rows; add a LIMIT and page through it");
                    }
                    let mut map = std::collections::HashMap::new();
                    for v in &vars {
                        if let Some(t) = sol.get(v.as_str()) {
                            map.insert(v.clone(), t.clone());
                        }
                    }
                    rows.push(Row(map));
                }
                Ok(Bindings { vars, rows })
            }
            _ => Err(anyhow!("expected a SELECT query")),
        }
    }

    fn ask(&self, sparql: &str) -> Result<bool> {
        let run = SparqlEvaluator::new().with_cancellation_token(deadline(self.timeout)).parse_query(sparql)?;
        match run.on_store(&self.store).execute().map_err(|e| explain(e, self.timeout))? {
            QueryResults::Boolean(b) => Ok(b),
            _ => Err(anyhow!("expected an ASK query")),
        }
    }

    fn construct(&self, sparql: &str) -> Result<Vec<Triple>> {
        let run = SparqlEvaluator::new().with_cancellation_token(deadline(self.timeout)).parse_query(sparql)?;
        match run.on_store(&self.store).execute().map_err(|e| explain(e, self.timeout))? {
            QueryResults::Graph(iter) => {
                let mut out = Vec::new();
                for t in iter {
                    out.push(t.map_err(|e| explain(e, self.timeout))?);
                    if out.len() > MAX_ROWS {
                        bail!("the query returns more than {MAX_ROWS} triples; narrow it");
                    }
                }
                Ok(out)
            }
            _ => Err(anyhow!("expected a CONSTRUCT or DESCRIBE query")),
        }
    }

    fn apply(&self, tx: GraphTx) -> Result<()> {
        let mut t = self.store.start_transaction()?;
        for graph in &tx.clear_graphs {
            let g = Self::subject_ref(graph)?;
            let doomed: Vec<Quad> = t
                .quads_for_pattern(None, None, None, Some(GraphNameRef::NamedNode(g.as_ref())))
                .collect::<Result<Vec<_>, _>>()?;
            for q in doomed {
                t.remove(q.as_ref());
            }
        }
        for (subject, graph) in &tx.delete_subjects {
            let s = Self::subject_ref(subject)?;
            let g = Self::subject_ref(graph)?;
            // Collect first: the iterator borrows the transaction.
            let mut frontier: Vec<NamedOrBlankNode> = vec![NamedOrBlankNode::NamedNode(s)];
            let mut seen: HashSet<String> = HashSet::new();
            let mut doomed: Vec<Quad> = Vec::new();
            while let Some(node) = frontier.pop() {
                let key = node.to_string();
                if !seen.insert(key) {
                    continue;
                }
                let quads: Vec<Quad> = t
                    .quads_for_pattern(
                        Some(NamedOrBlankNodeRef::from(&node)),
                        None,
                        None,
                        Some(GraphNameRef::NamedNode(g.as_ref())),
                    )
                    .collect::<Result<Vec<_>, _>>()?;
                for q in quads {
                    // A record's own sub-resources die with it, or replacing a record would
                    // orphan its distributions and checksums in the graph.
                    match &q.object {
                        Term::BlankNode(b) => frontier.push(NamedOrBlankNode::BlankNode(b.clone())),
                        Term::NamedNode(n) if is_owned_subresource(q.predicate.as_str()) => {
                            frontier.push(NamedOrBlankNode::NamedNode(n.clone()))
                        }
                        _ => {}
                    }
                    doomed.push(q);
                }
            }
            for q in doomed {
                t.remove(q.as_ref());
            }
        }
        for (subject, predicate, graph) in &tx.delete_properties {
            let s = Self::subject_ref(subject)?;
            let pred = Self::subject_ref(predicate)?;
            let g = Self::subject_ref(graph)?;
            let doomed: Vec<Quad> = t
                .quads_for_pattern(
                    Some(NamedOrBlankNodeRef::from(s.as_ref())),
                    Some(pred.as_ref()),
                    None,
                    Some(GraphNameRef::NamedNode(g.as_ref())),
                )
                .collect::<Result<Vec<_>, _>>()?;
            for q in doomed {
                t.remove(q.as_ref());
            }
        }
        for q in &tx.insert {
            t.insert(q.as_ref());
        }
        t.commit()?;
        Ok(())
    }

    fn drop_graph(&self, graph: &str) -> Result<()> {
        let g = Self::subject_ref(graph)?;
        self.store.remove_named_graph(g.as_ref())?;
        Ok(())
    }

    fn dump_nquads(&self, graph: Option<&str>) -> Result<String> {
        let mut buf = Vec::new();
        match graph {
            Some(g) => {
                let g = Self::subject_ref(g)?;
                self.store.dump_graph_to_writer(
                    GraphNameRef::NamedNode(g.as_ref()),
                    RdfSerializer::from_format(RdfFormat::NTriples),
                    &mut buf,
                )?;
            }
            None => {
                self.store.dump_to_writer(RdfSerializer::from_format(RdfFormat::NQuads), &mut buf)?;
            }
        }
        Ok(String::from_utf8(buf)?)
    }

    fn load_turtle(&self, data: &str, graph: &str, base: Option<&str>) -> Result<usize> {
        let g = Self::subject_ref(graph)?;
        let mut parser = RdfParser::from_format(RdfFormat::Turtle).without_named_graphs().with_default_graph(g);
        if let Some(b) = base {
            parser = parser.with_base_iri(b).map_err(|e| anyhow!("bad base IRI: {e}"))?;
        }
        let before = self.store.len()?;
        self.store.load_from_slice(parser, data.as_bytes())?;
        Ok(self.store.len()?.saturating_sub(before))
    }

    fn load_nquads(&self, data: &str) -> Result<usize> {
        let before = self.store.len()?;
        self.store.load_from_slice(RdfFormat::NQuads, data.as_bytes())?;
        Ok(self.store.len()?.saturating_sub(before))
    }
}
