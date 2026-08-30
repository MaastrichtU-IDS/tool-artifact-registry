//! Graph storage behind a trait (spec §5.3, "structural requirement, not a v2 aspiration").
//!
//! Every graph access in the registry goes through [`GraphStore`]. The shipped implementation
//! is embedded Oxigraph; swapping in a remote SPARQL backend (Fuseki, QLever, GraphDB) for an
//! estate that outgrows embedded storage is then a config switch rather than a rewrite.

pub mod oxi;

use anyhow::Result;
use oxigraph::model::{Quad, Term, Triple};

pub use oxi::OxigraphStore;

/// A SELECT result set, materialised so that a remote implementation can satisfy the same
/// contract without leaking a streaming type into the API layer.
#[derive(Debug, Default)]
pub struct Bindings {
    pub vars: Vec<String>,
    pub rows: Vec<Row>,
}

#[derive(Debug, Default, Clone)]
pub struct Row(pub std::collections::HashMap<String, Term>);

impl Row {
    pub fn term(&self, var: &str) -> Option<&Term> {
        self.0.get(var)
    }
    /// The lexical form of a literal, or the IRI of a named node.
    pub fn str(&self, var: &str) -> Option<String> {
        match self.0.get(var)? {
            Term::Literal(l) => Some(l.value().to_string()),
            Term::NamedNode(n) => Some(n.as_str().to_string()),
            Term::BlankNode(b) => Some(format!("_:{}", b.as_str())),
        }
    }
    pub fn iri(&self, var: &str) -> Option<String> {
        match self.0.get(var)? {
            Term::NamedNode(n) => Some(n.as_str().to_string()),
            _ => None,
        }
    }
    pub fn i64(&self, var: &str) -> Option<i64> {
        self.str(var)?.parse().ok()
    }
}

/// An atomic unit of graph change. Deletions are expressed per subject-in-graph because
/// every mutation the registry performs is "replace what we said about this resource".
#[derive(Debug, Default)]
pub struct GraphTx {
    pub insert: Vec<Quad>,
    /// `(subject IRI, graph IRI)` — all quads with this subject in this graph are removed,
    /// following blank-node closure.
    pub delete_subjects: Vec<(String, String)>,
}

impl GraphTx {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, q: Quad) -> &mut Self {
        self.insert.push(q);
        self
    }
    pub fn extend(&mut self, qs: impl IntoIterator<Item = Quad>) -> &mut Self {
        self.insert.extend(qs);
        self
    }
    pub fn replace_subject(&mut self, subject: &str, graph: &str) -> &mut Self {
        self.delete_subjects.push((subject.to_string(), graph.to_string()));
        self
    }
    pub fn is_empty(&self) -> bool {
        self.insert.is_empty() && self.delete_subjects.is_empty()
    }
}

pub trait GraphStore: Send + Sync + 'static {
    fn select(&self, sparql: &str) -> Result<Bindings>;
    fn ask(&self, sparql: &str) -> Result<bool>;
    fn construct(&self, sparql: &str) -> Result<Vec<Triple>>;
    fn apply(&self, tx: GraphTx) -> Result<()>;
    /// The concise bounded description of a subject: its own quads, the blank-node closure,
    /// and the sub-resources it owns outright — a Distribution, a Capability, a checksum, a
    /// qualified association. Those are minted as IRIs so the UI can link to them and a peer
    /// can cite them, but they are part of the record, not records of their own, so every
    /// implementation of this trait must return them together with their parent.
    fn describe(&self, subject: &str) -> Result<Vec<Quad>>;
    /// Does any graph say anything about this subject?
    fn exists(&self, subject: &str) -> Result<bool>;
    /// Which named graph holds the authoritative statements about a subject — this is how
    /// `local` vs `peer: name` is decided for every record and list row (handoff §6.1).
    fn graph_of(&self, subject: &str) -> Result<Option<String>>;
    fn drop_graph(&self, graph: &str) -> Result<()>;
    fn dump_nquads(&self, graph: Option<&str>) -> Result<String>;
    fn load_turtle(&self, data: &str, graph: &str, base: Option<&str>) -> Result<usize>;
    fn load_nquads(&self, data: &str) -> Result<usize>;
    fn count(&self) -> Result<usize>;
}
