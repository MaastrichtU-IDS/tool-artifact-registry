//! Projection between the graph and the JSON API.
//!
//! Reads gather a subject's quads once and project fields off a property map; writes build a
//! quad list and hand it to the store as one transaction. Keeping this in one place is what
//! lets the same record serialise as Turtle, JSON-LD, developer JSON or an HTML page without
//! three different code paths (spec §4.4).

pub mod props;
pub mod build;

pub use build::*;
pub use props::Props;

use crate::ns;
use oxigraph::model::{GraphName, NamedNode, Quad, Term};

/// A `Quad` in the local authoritative graph (spec §5.4).
pub fn local_quad(s: &str, p: NamedNode, o: impl Into<Term>) -> Quad {
    Quad::new(NamedNode::new_unchecked(s), p, o, GraphName::NamedNode(NamedNode::new_unchecked(ns::G_LOCAL)))
}

pub fn quad_in(graph: &str, s: impl Into<oxigraph::model::NamedOrBlankNode>, p: NamedNode, o: impl Into<Term>) -> Quad {
    Quad::new(s, p, o, GraphName::NamedNode(NamedNode::new_unchecked(graph)))
}
