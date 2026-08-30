//! Vocabulary constants and term helpers.
//!
//! The registry speaks DCAT 3, PROV-O, schema.org, SKOS, SPDX and DCTERMS, plus a small
//! `tar:` namespace for the four things none of them cover: capability declarations,
//! access descriptors, availability, and federation bookkeeping (spec §4, §6.1).

pub const TAR: &str = "https://w3id.org/tar/ns#";
pub const DCAT: &str = "http://www.w3.org/ns/dcat#";
pub const DCT: &str = "http://purl.org/dc/terms/";
pub const PROV: &str = "http://www.w3.org/ns/prov#";
pub const SCHEMA: &str = "https://schema.org/";
pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
pub const SPDX: &str = "http://spdx.org/rdf/terms#";
pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
pub const FOAF: &str = "http://xmlns.com/foaf/0.1/";

/// Named graphs (spec §5.4). Provenance of every triple is recoverable by construction.
pub const G_LOCAL: &str = "urn:tar:local";
pub const G_SHAPES: &str = "urn:tar:shapes";
pub const G_VOCAB: &str = "urn:tar:vocab";
pub const G_PEER_PREFIX: &str = "urn:tar:peer:";

pub fn peer_graph(id: &str) -> String {
    format!("{G_PEER_PREFIX}{id}")
}


/// Build a `NamedNode` from a namespace + local name.
pub fn t(ns: &str, local: &str) -> oxigraph::model::NamedNode {
    oxigraph::model::NamedNode::new_unchecked(format!("{ns}{local}"))
}

pub fn rdf_type() -> oxigraph::model::NamedNode {
    t(RDF, "type")
}

/// The `@context` served with every JSON-LD response and by `/api/v1/context`.
pub fn jsonld_context() -> serde_json::Value {
    serde_json::json!({
        "tar": TAR, "dcat": DCAT, "dct": DCT, "prov": PROV, "schema": SCHEMA,
        "rdfs": RDFS, "skos": SKOS, "spdx": SPDX, "xsd": XSD, "foaf": FOAF
    })
}

pub const PREFIXES: &str = concat!(
    "PREFIX tar: <https://w3id.org/tar/ns#>\n",
    "PREFIX dcat: <http://www.w3.org/ns/dcat#>\n",
    "PREFIX dct: <http://purl.org/dc/terms/>\n",
    "PREFIX prov: <http://www.w3.org/ns/prov#>\n",
    "PREFIX schema: <https://schema.org/>\n",
    "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n",
    "PREFIX skos: <http://www.w3.org/2004/02/skos/core#>\n",
    "PREFIX spdx: <http://spdx.org/rdf/terms#>\n",
    "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n",
);
