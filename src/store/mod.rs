//! Graph storage behind a trait (spec §5.3, "structural requirement, not a v2 aspiration").
//!
//! Every graph access in the registry goes through [`GraphStore`]. The shipped implementation
//! is embedded Oxigraph; swapping in a remote SPARQL backend (Fuseki, QLever, GraphDB) for an
//! estate that outgrows embedded storage is then a config switch rather than a rewrite.

pub mod http;
pub mod oxi;
pub mod queries;

use anyhow::Result;
use oxigraph::model::{GraphName, NamedOrBlankNode, Quad, Term, Triple};

pub use http::HttpSparqlStore;
pub use oxi::OxigraphStore;
pub use queries::{is_owned_subresource, OWNED_SUBRESOURCE_PREDICATES};

/// Open the graph store the configuration asks for.
///
/// The owner's rule, verbatim: "if user doesn't provide a sparql endpoint, fall back to
/// oxigraph". The absence of `TAR_SPARQL_ENDPOINT` is the whole switch, so an install that
/// exists today changes nothing. Both backends satisfy the same trait and nothing above this
/// function knows which one it got.
pub fn open(
    backend: Option<&crate::config::SparqlBackend>,
    graph_path: &str,
) -> Result<std::sync::Arc<dyn GraphStore>> {
    Ok(match backend {
        Some(b) => {
            tracing::info!(endpoint = %b.describe(), "graph store: external SPARQL endpoint");
            std::sync::Arc::new(HttpSparqlStore::connect(b.clone())?)
        }
        None => {
            tracing::info!(path = %graph_path, "graph store: embedded Oxigraph");
            std::sync::Arc::new(OxigraphStore::open(graph_path)?)
        }
    })
}

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
    /// `(subject IRI, predicate IRI, graph IRI)` — removes just that property, so a value can
    /// be replaced rather than accumulated. A run advertised as `running` and later as
    /// `success` must end up with one status, not both.
    pub delete_properties: Vec<(String, String, String)>,
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
    /// Drop every value of one property so an insert in the same transaction replaces it.
    pub fn replace_property(&mut self, subject: &str, predicate: &str, graph: &str) -> &mut Self {
        self.delete_properties.push((subject.to_string(), predicate.to_string(), graph.to_string()));
        self
    }
    pub fn is_empty(&self) -> bool {
        self.insert.is_empty() && self.delete_subjects.is_empty() && self.delete_properties.is_empty()
    }
}

pub trait GraphStore: Send + Sync + 'static {
    fn select(&self, sparql: &str) -> Result<Bindings>;
    fn ask(&self, sparql: &str) -> Result<bool>;
    fn construct(&self, sparql: &str) -> Result<Vec<Triple>>;
    fn apply(&self, tx: GraphTx) -> Result<()>;
    fn drop_graph(&self, graph: &str) -> Result<()>;
    fn dump_nquads(&self, graph: Option<&str>) -> Result<String>;
    fn load_turtle(&self, data: &str, graph: &str, base: Option<&str>) -> Result<usize>;
    fn load_nquads(&self, data: &str) -> Result<usize>;

    // ---- Derived reads. -----------------------------------------------------------------
    //
    // These four were bespoke, written against Oxigraph's quad-pattern API, which put their
    // semantics inside one backend. They are now one SPARQL query each (`store::queries`) run
    // through `select`/`ask` above, so a backend implements three methods and gets the same
    // answers as every other. Nothing overrides them; a backend that did would be re-opening
    // exactly the divergence they exist to close.

    /// The concise bounded description of a subject: its own quads, the blank-node closure,
    /// and the sub-resources it owns outright — a Distribution, a Capability, a checksum, a
    /// qualified association. Those are minted as IRIs so the UI can link to them and a peer
    /// can cite them, but they are part of the record, not records of their own, so every
    /// implementation of this trait must return them together with their parent.
    ///
    /// Not SPARQL `DESCRIBE`, whose result is implementation-defined and would differ between
    /// the two backends on the first query. See `queries::describe` for the closure.
    fn describe(&self, subject: &str) -> Result<Vec<Quad>> {
        describe_via_sparql(self, subject)
    }
    /// Does any graph say anything about this subject?
    fn exists(&self, subject: &str) -> Result<bool> {
        self.ask(&queries::exists(subject)?)
    }
    /// Which named graph holds the authoritative statements about a subject — this is how
    /// `local` vs `peer: name` is decided for every record and list row (handoff §6.1).
    fn graph_of(&self, subject: &str) -> Result<Option<String>> {
        Ok(self.select(&queries::graph_of(subject)?)?.rows.first().and_then(|r| r.iri("g")))
    }
    fn count(&self) -> Result<usize> {
        Ok(self.select(&queries::count())?.rows.first().and_then(|r| r.i64("n")).unwrap_or(0) as usize)
    }
}

/// Run the closure query, growing the chain until nothing new is at the bottom of it.
///
/// The first attempt covers more levels than any record the registry writes; escalation is
/// there so that a nested node somebody adds later comes back whole instead of being silently
/// clipped, which is the failure this whole arrangement exists to prevent. Only the last
/// attempt's rows are kept: blank node labels are consistent within one result set and not
/// between two, so mixing attempts would produce a record whose checksum node had two names.
fn describe_via_sparql<S: GraphStore + ?Sized>(store: &S, subject: &str) -> Result<Vec<Quad>> {
    let mut depth = queries::DEFAULT_DEPTH;
    loop {
        let bindings = store.select(&queries::describe(subject, depth)?)?;
        let mut quads: Vec<Quad> = Vec::with_capacity(bindings.rows.len());
        let mut seen: std::collections::HashSet<Quad> = std::collections::HashSet::new();
        let mut levels: Vec<(usize, String)> = Vec::with_capacity(bindings.rows.len());
        for row in &bindings.rows {
            let (Some(g), Some(s), Some(p), Some(o)) = (row.term("g"), row.term("s"), row.term("p"), row.term("o"))
            else {
                continue;
            };
            let (Term::NamedNode(g), Term::NamedNode(p)) = (g, p) else { continue };
            let subj: NamedOrBlankNode = match s {
                Term::NamedNode(n) => n.clone().into(),
                Term::BlankNode(b) => b.clone().into(),
                // A literal cannot be a subject; nothing to fetch under it.
                Term::Literal(_) => continue,
            };
            levels.push((row.i64("d").unwrap_or(0) as usize, crate::rdf::props::subject_key(&subj)));
            let quad = Quad::new(subj, p.clone(), o.clone(), GraphName::NamedNode(g.clone()));
            // One node can be reached at more than one level; the record must not say
            // anything twice or a multi-valued property gains a duplicate.
            if seen.insert(quad.clone()) {
                quads.push(quad);
            }
        }
        if !queries::truncated(&levels, depth) {
            return Ok(quads);
        }
        if depth >= queries::MAX_DEPTH {
            // Loud, because the alternative is the silent clipping this guards against.
            tracing::error!(
                %subject,
                depth,
                "sub-resource closure is deeper than {} levels — the description is incomplete; \
                 this is a cycle or a nesting nobody intended",
                queries::MAX_DEPTH
            );
            return Ok(quads);
        }
        depth = (depth * 2).min(queries::MAX_DEPTH);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ns;
    use oxigraph::model::{BlankNode, Literal, NamedNode};

    fn n(iri: &str) -> NamedNode {
        NamedNode::new_unchecked(iri)
    }
    fn q(s: impl Into<NamedOrBlankNode>, p: &str, o: impl Into<Term>, g: &str) -> Quad {
        Quad::new(s, n(p), o, GraphName::NamedNode(n(g)))
    }
    fn store(quads: Vec<Quad>) -> OxigraphStore {
        let s = OxigraphStore::memory().unwrap();
        let mut tx = GraphTx::new();
        tx.extend(quads);
        s.apply(tx).unwrap();
        s
    }

    const A: &str = "https://reg.example/artifact/1";
    const D: &str = "https://reg.example/distribution/1";
    const AGENT: &str = "https://reg.example/agent/1";
    const PEER: &str = "urn:tar:peer:other";

    /// The closure, spelled out. This is the test that fails if a predicate falls out of
    /// `OWNED_SUBRESOURCE_PREDICATES`, if `dct:publisher` ever falls into it, or if the graph
    /// each quad came from stops being carried.
    #[test]
    fn describe_returns_the_owned_closure_and_nothing_else() {
        let checksum = BlankNode::default();
        let inner = BlankNode::default();
        let s = store(vec![
            q(n(A), "http://www.w3.org/ns/dcat#distribution", n(D), ns::G_LOCAL),
            q(n(A), "http://purl.org/dc/terms/title", Literal::new_simple_literal("A report"), ns::G_LOCAL),
            // Deliberately not owned: an Agent is its own record, shared between many.
            q(n(A), "http://purl.org/dc/terms/publisher", n(AGENT), ns::G_LOCAL),
            q(n(AGENT), "https://schema.org/name", Literal::new_simple_literal("Someone"), ns::G_LOCAL),
            // The sub-resource's own statements, deliberately in a different graph: the walk
            // this replaced crossed graphs, so this one must too.
            q(n(D), "http://spdx.org/rdf/terms#checksum", checksum.clone(), PEER),
            q(n(D), "http://www.w3.org/ns/dcat#mediaType", Literal::new_simple_literal("text/turtle"), PEER),
            // Blank nodes are followed whatever the predicate, transitively.
            q(
                checksum.clone(),
                "http://spdx.org/rdf/terms#checksumValue",
                Literal::new_simple_literal("deadbeef"),
                PEER,
            ),
            q(checksum.clone(), "https://w3id.org/tar/ns#note", inner.clone(), PEER),
            q(inner.clone(), "http://www.w3.org/2000/01/rdf-schema#label", Literal::new_simple_literal("deep"), PEER),
        ]);

        let got = s.describe(A).unwrap();
        let says = |subject: &str, predicate: &str| {
            got.iter().any(|x| x.subject.to_string().contains(subject) && x.predicate.as_str() == predicate)
        };
        assert!(says(A, "http://purl.org/dc/terms/title"));
        assert!(says(D, "http://spdx.org/rdf/terms#checksum"), "an owned sub-resource comes with its parent");
        assert!(says(D, "http://www.w3.org/ns/dcat#mediaType"));
        assert!(says(checksum.as_str(), "http://spdx.org/rdf/terms#checksumValue"), "blank nodes are followed");
        assert!(says(inner.as_str(), "http://www.w3.org/2000/01/rdf-schema#label"), "and followed transitively");
        assert!(
            !says(AGENT, "https://schema.org/name"),
            "dct:publisher is not ownership — the agent's own record must stay out"
        );
        // Nine quads in, eight of them reachable; the agent's name is the one left behind.
        assert_eq!(got.len(), 8, "{got:#?}");
        // And each quad still says which graph it came from — this is how origin is decided.
        for x in &got {
            let expected = if x.subject.to_string().contains("artifact/1") { ns::G_LOCAL } else { PEER };
            assert_eq!(x.graph_name.to_string(), format!("<{expected}>"), "{x}");
        }
    }

    /// Nothing comes back twice. A node reachable at two depths would otherwise give a
    /// multi-valued property a duplicate value.
    #[test]
    fn a_node_reachable_by_two_routes_is_described_once() {
        let s = store(vec![
            q(n(A), "http://www.w3.org/ns/dcat#distribution", n(D), ns::G_LOCAL),
            q(n(D), "https://w3id.org/tar/ns#hasCapability", n(D), ns::G_LOCAL),
            q(n(D), "http://www.w3.org/ns/dcat#mediaType", Literal::new_simple_literal("text/turtle"), ns::G_LOCAL),
        ]);
        let got = s.describe(A).unwrap();
        assert_eq!(got.len(), 3, "{got:#?}");
    }

    /// Deeper than the first attempt covers. The query escalates rather than clipping — the
    /// failure this whole arrangement exists to prevent is a nested node quietly going missing.
    #[test]
    fn a_closure_deeper_than_the_first_attempt_still_comes_back_whole() {
        let chain: Vec<BlankNode> = (0..queries::DEFAULT_DEPTH * 2).map(|_| BlankNode::default()).collect();
        let mut quads = vec![q(n(A), "http://www.w3.org/ns/prov#qualifiedAssociation", chain[0].clone(), ns::G_LOCAL)];
        for pair in chain.windows(2) {
            quads.push(q(pair[0].clone(), "https://w3id.org/tar/ns#note", pair[1].clone(), ns::G_LOCAL));
        }
        let last = chain.last().unwrap().clone();
        quads.push(q(
            last,
            "http://www.w3.org/2000/01/rdf-schema#label",
            Literal::new_simple_literal("bottom"),
            ns::G_LOCAL,
        ));
        let n_quads = quads.len();
        let s = store(quads);
        let got = s.describe(A).unwrap();
        assert_eq!(got.len(), n_quads, "the bottom of the chain must not be clipped: {got:#?}");
    }

    /// A subject held locally *and* cached from a peer reports as local. The walk this
    /// replaced returned whichever graph the storage index happened to yield first, which is
    /// not something a second backend can reproduce.
    #[test]
    fn graph_of_prefers_what_this_registry_holds_itself() {
        let s = store(vec![
            q(n(A), "http://purl.org/dc/terms/title", Literal::new_simple_literal("ours"), ns::G_LOCAL),
            q(n(A), "http://purl.org/dc/terms/title", Literal::new_simple_literal("theirs"), PEER),
        ]);
        assert_eq!(s.graph_of(A).unwrap().as_deref(), Some(ns::G_LOCAL));

        let only_peer =
            store(vec![q(n(A), "http://purl.org/dc/terms/title", Literal::new_simple_literal("theirs"), PEER)]);
        assert_eq!(only_peer.graph_of(A).unwrap().as_deref(), Some(PEER));
        assert_eq!(only_peer.graph_of("https://reg.example/artifact/nope").unwrap(), None);
    }

    #[test]
    fn exists_and_count_answer_over_every_named_graph() {
        let s = store(vec![
            q(n(A), "http://purl.org/dc/terms/title", Literal::new_simple_literal("ours"), ns::G_LOCAL),
            q(n(D), "http://purl.org/dc/terms/title", Literal::new_simple_literal("theirs"), PEER),
        ]);
        assert!(s.exists(A).unwrap());
        assert!(s.exists(D).unwrap());
        // Object position is not existence: `exists` asks what the store *says about* a subject.
        assert!(!s.exists(AGENT).unwrap());
        assert_eq!(s.count().unwrap(), 2);
    }

    /// `replace_subject` takes the sub-resources with it, or replacing a record would orphan
    /// its distributions and checksums. Same closure, narrowed to one graph.
    #[test]
    fn replacing_a_subject_removes_what_it_owns_and_leaves_what_it_does_not() {
        let checksum = BlankNode::default();
        let s = store(vec![
            q(n(A), "http://www.w3.org/ns/dcat#distribution", n(D), ns::G_LOCAL),
            q(n(A), "http://purl.org/dc/terms/publisher", n(AGENT), ns::G_LOCAL),
            q(n(AGENT), "https://schema.org/name", Literal::new_simple_literal("Someone"), ns::G_LOCAL),
            q(n(D), "http://spdx.org/rdf/terms#checksum", checksum.clone(), ns::G_LOCAL),
            q(
                checksum,
                "http://spdx.org/rdf/terms#checksumValue",
                Literal::new_simple_literal("deadbeef"),
                ns::G_LOCAL,
            ),
        ]);
        let mut tx = GraphTx::new();
        tx.replace_subject(A, ns::G_LOCAL);
        s.apply(tx).unwrap();
        assert!(!s.exists(A).unwrap());
        assert!(!s.exists(D).unwrap());
        assert!(s.exists(AGENT).unwrap(), "the agent is its own record and survives");
        assert_eq!(s.count().unwrap(), 1);
    }
}
