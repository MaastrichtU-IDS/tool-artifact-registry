//! Quad construction helpers. Every write goes through one of these so that datatypes and
//! graph placement stay consistent.

use crate::ns;
use oxigraph::model::{BlankNode, GraphName, Literal, NamedNode, Quad, NamedOrBlankNode, Term};

pub struct Node {
    pub subject: NamedOrBlankNode,
    graph: GraphName,
    pub quads: Vec<Quad>,
}

impl Node {
    pub fn iri(iri: &str, graph: &str) -> Self {
        Self {
            subject: NamedOrBlankNode::NamedNode(NamedNode::new_unchecked(iri)),
            graph: GraphName::NamedNode(NamedNode::new_unchecked(graph)),
            quads: Vec::new(),
        }
    }

    pub fn local(iri: &str) -> Self {
        Self::iri(iri, ns::G_LOCAL)
    }

    pub fn blank(graph: &str) -> Self {
        Self {
            subject: NamedOrBlankNode::BlankNode(BlankNode::default()),
            graph: GraphName::NamedNode(NamedNode::new_unchecked(graph)),
            quads: Vec::new(),
        }
    }

    pub fn term(&self) -> Term {
        match &self.subject {
            NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
            NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
        }
    }

    fn push(&mut self, p: NamedNode, o: Term) -> &mut Self {
        self.quads.push(Quad::new(self.subject.clone(), p, o, self.graph.clone()));
        self
    }

    /// `rdf:type`.
    pub fn a(&mut self, type_iri: &str) -> &mut Self {
        self.push(ns::rdf_type(), Term::NamedNode(NamedNode::new_unchecked(type_iri)))
    }

    pub fn link(&mut self, ns_: &str, local: &str, target: &str) -> &mut Self {
        if target.is_empty() {
            return self;
        }
        match NamedNode::new(target) {
            Ok(n) => self.push(ns::t(ns_, local), Term::NamedNode(n)),
            // A non-IRI value in an IRI position is kept as a literal rather than dropped;
            // SHACL validation is what rejects it, not silent data loss.
            Err(_) => self.push(ns::t(ns_, local), Term::Literal(Literal::new_simple_literal(target))),
        }
    }

    pub fn opt_link(&mut self, ns_: &str, local: &str, target: &Option<String>) -> &mut Self {
        match target {
            Some(t) if !t.is_empty() => self.link(ns_, local, t),
            _ => self,
        }
    }

    pub fn links(&mut self, ns_: &str, local: &str, targets: &[String]) -> &mut Self {
        for t in targets {
            self.link(ns_, local, t);
        }
        self
    }

    pub fn text(&mut self, ns_: &str, local: &str, value: &str) -> &mut Self {
        if value.is_empty() {
            return self;
        }
        self.push(ns::t(ns_, local), Term::Literal(Literal::new_simple_literal(value)))
    }

    pub fn opt_text(&mut self, ns_: &str, local: &str, value: &Option<String>) -> &mut Self {
        match value {
            Some(v) if !v.is_empty() => self.text(ns_, local, v),
            _ => self,
        }
    }

    pub fn texts(&mut self, ns_: &str, local: &str, values: &[String]) -> &mut Self {
        for v in values {
            self.text(ns_, local, v);
        }
        self
    }

    pub fn int(&mut self, ns_: &str, local: &str, value: i64) -> &mut Self {
        self.push(
            ns::t(ns_, local),
            Term::Literal(Literal::new_typed_literal(value.to_string(), NamedNode::new_unchecked(format!("{}integer", ns::XSD)))),
        )
    }

    pub fn opt_int(&mut self, ns_: &str, local: &str, value: &Option<i64>) -> &mut Self {
        match value {
            Some(v) => self.int(ns_, local, *v),
            None => self,
        }
    }

    pub fn boolean(&mut self, ns_: &str, local: &str, value: bool) -> &mut Self {
        self.push(
            ns::t(ns_, local),
            Term::Literal(Literal::new_typed_literal(value.to_string(), NamedNode::new_unchecked(format!("{}boolean", ns::XSD)))),
        )
    }

    /// An `xsd:dateTime` literal. Input is expected RFC 3339; anything else is stored as a
    /// plain literal so that bad input surfaces in validation rather than vanishing.
    pub fn datetime(&mut self, ns_: &str, local: &str, value: &str) -> &mut Self {
        if value.is_empty() {
            return self;
        }
        match chrono::DateTime::parse_from_rfc3339(value) {
            Ok(dt) => self.push(
                ns::t(ns_, local),
                Term::Literal(Literal::new_typed_literal(
                    dt.to_utc().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    NamedNode::new_unchecked(format!("{}dateTime", ns::XSD)),
                )),
            ),
            Err(_) => self.text(ns_, local, value),
        }
    }

    pub fn opt_datetime(&mut self, ns_: &str, local: &str, value: &Option<String>) -> &mut Self {
        match value {
            Some(v) if !v.is_empty() => self.datetime(ns_, local, v),
            _ => self,
        }
    }

    /// Attach a nested node (blank or minted) and return its quads together with ours.
    pub fn child(&mut self, ns_: &str, local: &str, child: Node) -> &mut Self {
        let t = child.term();
        self.push(ns::t(ns_, local), t);
        self.quads.extend(child.quads);
        self
    }

    pub fn finish(self) -> Vec<Quad> {
        self.quads
    }
}

/// `prov:wasAttributedTo` plus a minted timestamp — recorded on every write (spec §8.3).
pub fn attribution(node: &mut Node, actor: &str) {
    attribution_at(node, actor, None)
}

/// Attribution where the caller has its own idea of when the thing was last modified.
///
/// `dct:modified` means when the *resource* changed, so a producer that knows that date is a
/// better source than the clock. Both used to be written — the caller's value and the stamp —
/// leaving two `dct:modified` triples on one record and a reader taking whichever came back
/// first. That is not a tie the reader can break, because nothing in the graph says which is
/// which; the write path is the only place that knows, so it decides here.
pub fn attribution_at(node: &mut Node, actor: &str, modified: Option<&str>) {
    node.link(ns::PROV, "wasAttributedTo", actor);
    node.datetime(
        ns::DCT,
        "modified",
        modified.unwrap_or(&chrono::Utc::now().to_rfc3339()),
    );
}
