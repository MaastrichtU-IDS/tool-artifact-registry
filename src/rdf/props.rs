//! A property map over a subject's quads, with typed getters.

use crate::ns;
use oxigraph::model::{Quad, Term};
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct Props {
    pub subject: String,
    /// Predicate IRI -> objects, insertion-ordered per predicate.
    pub map: HashMap<String, Vec<Term>>,
    /// Property maps of blank nodes and sub-resources reachable from this subject.
    pub nested: HashMap<String, Props>,
    /// The named graph the statements came from — `local` vs `peer:…` (handoff §6.1).
    pub graph: Option<String>,
}

impl Props {
    /// Build a property map for `subject` out of a describe result, indexing nested nodes
    /// (blank nodes and minted sub-resources) so a Distribution can be read off its parent.
    pub fn from_quads(subject: &str, quads: &[Quad]) -> Props {
        let mut root = Props { subject: subject.to_string(), ..Default::default() };
        let mut by_subject: HashMap<String, Props> = HashMap::new();
        for q in quads {
            let s = subject_key(&q.subject);
            let entry = by_subject.entry(s.clone()).or_insert_with(|| Props { subject: s, ..Default::default() });
            entry.map.entry(q.predicate.as_str().to_string()).or_default().push(q.object.clone());
            if entry.graph.is_none() {
                entry.graph = match &q.graph_name {
                    oxigraph::model::GraphName::NamedNode(n) => Some(n.as_str().to_string()),
                    _ => None,
                };
            }
        }
        if let Some(p) = by_subject.get(subject) {
            root.map = p.map.clone();
            root.graph = p.graph.clone();
        }
        root.nested = by_subject;
        root
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn nested_for(&self, key: &str) -> Option<&Props> {
        self.nested.get(key)
    }

    pub fn terms(&self, ns_: &str, local: &str) -> &[Term] {
        self.map.get(&format!("{ns_}{local}")).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn one(&self, ns_: &str, local: &str) -> Option<&Term> {
        self.terms(ns_, local).first()
    }

    /// Lexical value of a literal, or the IRI of a named node.
    pub fn str(&self, ns_: &str, local: &str) -> Option<String> {
        self.one(ns_, local).map(term_value)
    }

    pub fn strs(&self, ns_: &str, local: &str) -> Vec<String> {
        self.terms(ns_, local).iter().map(term_value).collect()
    }

    pub fn iri(&self, ns_: &str, local: &str) -> Option<String> {
        match self.one(ns_, local)? {
            Term::NamedNode(n) => Some(n.as_str().to_string()),
            _ => None,
        }
    }

    pub fn iris(&self, ns_: &str, local: &str) -> Vec<String> {
        self.terms(ns_, local)
            .iter()
            .filter_map(|t| match t {
                Term::NamedNode(n) => Some(n.as_str().to_string()),
                _ => None,
            })
            .collect()
    }

    /// Objects that are named nodes *or* blank nodes — the addressing key into `nested`.
    pub fn node_keys(&self, ns_: &str, local: &str) -> Vec<String> {
        self.terms(ns_, local).iter().map(term_key).collect()
    }

    pub fn i64(&self, ns_: &str, local: &str) -> Option<i64> {
        self.str(ns_, local)?.parse().ok()
    }

    pub fn bool(&self, ns_: &str, local: &str) -> Option<bool> {
        match self.str(ns_, local)?.as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        }
    }

    pub fn types(&self) -> Vec<String> {
        self.iris(ns::RDF, "type")
    }

    pub fn has_type(&self, iri: &str) -> bool {
        self.types().iter().any(|t| t == iri)
    }
}

pub fn term_value(t: &Term) -> String {
    match t {
        Term::Literal(l) => l.value().to_string(),
        Term::NamedNode(n) => n.as_str().to_string(),
        Term::BlankNode(b) => format!("_:{}", b.as_str()),
    }
}

pub fn term_key(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => n.as_str().to_string(),
        Term::BlankNode(b) => format!("_:{}", b.as_str()),
        other => other.to_string(),
    }
}

pub fn subject_key(s: &oxigraph::model::NamedOrBlankNode) -> String {
    match s {
        oxigraph::model::NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
        oxigraph::model::NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
    }
}
