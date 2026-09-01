//! Vocabulary constants and term helpers.
//!
//! The registry speaks DCAT 3, PROV-O, schema.org, SKOS, SPDX, DCTERMS, CodeMeta and ADMS,
//! plus a small `tar:` namespace for the things none of them cover: capability declarations,
//! access descriptors, and federation bookkeeping (spec §4, §6.1). Every `tar:` term is
//! audited against the standard vocabularies in `docs/specs/2026-08-30-vocabulary-audit.md`;
//! `shapes/vocab.ttl` records, per kept term, what was checked and why nothing standard fit.

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
/// CodeMeta terms (v3, w3id-hosted): `developmentStatus`, `maintainer`.
pub const CODEMETA: &str = "https://w3id.org/codemeta/terms/";
/// W3C ADMS: `adms:status` marks tombstoned records with the EU dataset-status vocabulary.
pub const ADMS: &str = "http://www.w3.org/ns/adms#";
/// EU Publications Office access-right authority table, the DCAT-AP value set for
/// `dct:accessRights`: PUBLIC, RESTRICTED, NON_PUBLIC, SENSITIVE, CONFIDENTIAL.
pub const EU_ACCESS_RIGHT: &str = "http://publications.europa.eu/resource/authority/access-right/";
/// EU dataset-status authority table; `WITHDRAWN` is the tombstone marker.
pub const EU_DATASET_STATUS: &str = "http://publications.europa.eu/resource/authority/dataset-status/";
/// VoID, the vocabulary for describing an RDF dataset. Used only for the bundle graphs'
/// provenance node in `<urn:tar:bundles>` — `void:Dataset`, `void:triples`.
pub const VOID: &str = "http://rdfs.org/ns/void#";

/// Named graphs (spec §5.4). Provenance of every triple is recoverable by construction.
///
/// Three families, and which family a graph is in decides who may write it:
///
/// * `urn:tar:local` — the records this registry is authoritative for. Written by the API.
/// * `urn:tar:peer:{id}` — one cached stub per peer, written by the resolver and by nothing
///   else. Nothing this registry enforces on its own records is applied here.
/// * `urn:tar:shapes`, `urn:tar:bundle:{name}`, `urn:tar:bundles` — reference data the binary
///   ships and owns outright (`crate::bundles`). Dropped and rewritten whenever the bundle's
///   content hash changes, so nothing but the bundle may ever be written into one.
pub const G_LOCAL: &str = "urn:tar:local";
pub const G_SHAPES: &str = "urn:tar:shapes";
/// One graph per bundled reference file; see `crate::bundles::BUNDLES` for the names.
pub const G_BUNDLE_PREFIX: &str = "urn:tar:bundle:";
/// Where each bundle graph's content hash and load timestamp live.
pub const G_BUNDLES: &str = "urn:tar:bundles";
/// The graph that held every bundle, the seeded types and every adopted term at once, before
/// they were split apart. A store written by an older build still has it; `seed::load_vocab`
/// rescues what the binary cannot regenerate into `urn:tar:local` and drops the rest.
pub const G_LEGACY_VOCAB: &str = "urn:tar:vocab";
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
        "rdfs": RDFS, "skos": SKOS, "spdx": SPDX, "xsd": XSD, "foaf": FOAF,
        "codemeta": CODEMETA, "adms": ADMS
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
