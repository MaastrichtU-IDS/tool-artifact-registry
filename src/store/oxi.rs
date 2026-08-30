//! Embedded Oxigraph implementation of [`GraphStore`] (spec D3, §5).

use anyhow::{anyhow, Context, Result};
use oxigraph::io::{RdfFormat, RdfParser, RdfSerializer};
use oxigraph::model::{
    GraphName, GraphNameRef, NamedNode, NamedNodeRef, Quad, NamedOrBlankNode, NamedOrBlankNodeRef, Term, Triple,
};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use std::collections::HashSet;
use std::path::Path;

use super::{Bindings, GraphStore, GraphTx, Row};

pub struct OxigraphStore {
    store: Store,
}

impl OxigraphStore {
    /// `path == "memory"` opens a non-persistent store — used by tests and `tar seed --dry-run`.
    pub fn open(path: &str) -> Result<Self> {
        let store = if path == "memory" {
            Store::new()?
        } else {
            let p = Path::new(path);
            std::fs::create_dir_all(p).with_context(|| format!("creating {path}"))?;
            Store::open(p).with_context(|| format!("opening graph store at {path}"))?
        };
        Ok(Self { store })
    }

    pub fn memory() -> Result<Self> {
        Ok(Self { store: Store::new()? })
    }

    pub fn inner(&self) -> &Store {
        &self.store
    }

    fn subject_ref(iri: &str) -> Result<NamedNode> {
        NamedNode::new(iri).map_err(|e| anyhow!("bad IRI {iri}: {e}"))
    }
}

/// Predicates whose objects are sub-resources of the subject rather than records in their own
/// right (see `GraphStore::describe`). `dct:publisher` is deliberately absent: an Agent is its
/// own record, shared between many.
const OWNED_SUBRESOURCE_PREDICATES: [&str; 4] = [
    "http://www.w3.org/ns/dcat#distribution",
    "https://w3id.org/tar/ns#hasCapability",
    "http://spdx.org/rdf/terms#checksum",
    "http://www.w3.org/ns/prov#qualifiedAssociation",
];

fn is_owned_subresource(predicate: &str) -> bool {
    OWNED_SUBRESOURCE_PREDICATES.contains(&predicate)
}

impl GraphStore for OxigraphStore {
    fn select(&self, sparql: &str) -> Result<Bindings> {
        let results = SparqlEvaluator::new()
            .parse_query(sparql)
            .with_context(|| format!("parsing SPARQL: {sparql}"))?
            .on_store(&self.store)
            .execute()?;
        match results {
            QueryResults::Solutions(iter) => {
                let vars: Vec<String> =
                    iter.variables().iter().map(|v| v.as_str().to_string()).collect();
                let mut rows = Vec::new();
                for sol in iter {
                    let sol = sol?;
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
        match SparqlEvaluator::new().parse_query(sparql)?.on_store(&self.store).execute()? {
            QueryResults::Boolean(b) => Ok(b),
            _ => Err(anyhow!("expected an ASK query")),
        }
    }

    fn construct(&self, sparql: &str) -> Result<Vec<Triple>> {
        match SparqlEvaluator::new().parse_query(sparql)?.on_store(&self.store).execute()? {
            QueryResults::Graph(iter) => Ok(iter.collect::<Result<Vec<_>, _>>()?),
            _ => Err(anyhow!("expected a CONSTRUCT or DESCRIBE query")),
        }
    }

    fn apply(&self, tx: GraphTx) -> Result<()> {
        let mut t = self.store.start_transaction()?;
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

    fn describe(&self, subject: &str) -> Result<Vec<Quad>> {
        let s = Self::subject_ref(subject)?;
        let mut out: Vec<Quad> = Vec::new();
        let mut frontier: Vec<NamedOrBlankNode> = vec![NamedOrBlankNode::NamedNode(s)];
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(node) = frontier.pop() {
            if !seen.insert(node.to_string()) {
                continue;
            }
            for q in self.store.quads_for_pattern(Some(NamedOrBlankNodeRef::from(&node)), None, None, None) {
                let q = q?;
                match &q.object {
                    Term::BlankNode(b) => frontier.push(NamedOrBlankNode::BlankNode(b.clone())),
                    Term::NamedNode(n) if is_owned_subresource(q.predicate.as_str()) => {
                        frontier.push(NamedOrBlankNode::NamedNode(n.clone()))
                    }
                    _ => {}
                }
                out.push(q);
            }
        }
        Ok(out)
    }

    fn exists(&self, subject: &str) -> Result<bool> {
        let s = Self::subject_ref(subject)?;
        Ok(self
            .store
            .quads_for_pattern(Some(NamedNodeRef::from(s.as_ref()).into()), None, None, None)
            .next()
            .transpose()?
            .is_some())
    }

    fn graph_of(&self, subject: &str) -> Result<Option<String>> {
        let s = Self::subject_ref(subject)?;
        for q in
            self.store.quads_for_pattern(Some(NamedNodeRef::from(s.as_ref()).into()), None, None, None)
        {
            let q = q?;
            return Ok(match q.graph_name {
                GraphName::NamedNode(n) => Some(n.as_str().to_string()),
                _ => None,
            });
        }
        Ok(None)
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
                self.store
                    .dump_to_writer(RdfSerializer::from_format(RdfFormat::NQuads), &mut buf)?;
            }
        }
        Ok(String::from_utf8(buf)?)
    }

    fn load_turtle(&self, data: &str, graph: &str, base: Option<&str>) -> Result<usize> {
        let g = Self::subject_ref(graph)?;
        let mut parser = RdfParser::from_format(RdfFormat::Turtle)
            .without_named_graphs()
            .with_default_graph(g);
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

    fn count(&self) -> Result<usize> {
        Ok(self.store.len()?)
    }
}
