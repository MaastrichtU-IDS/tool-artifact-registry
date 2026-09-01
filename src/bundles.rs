//! Reference data: the five bundles that ship inside the binary, and the two places they go.
//!
//! Four files under `shapes/` and one table in `src/domain/keywords.rs` are *reference* data —
//! the registry's own terms, a working subset of two external vocabularies, the SHACL shapes,
//! and the artifact keyword scheme. They are not records. Nobody writes them through the API,
//! nothing federates them, and the binary can reproduce every quad of them from its own
//! contents. Roughly 12 000 quads in total.
//!
//! They were loaded into the record store on every boot, unconditionally, with a `DROP GRAPH`
//! of the shapes each time. On the embedded store that is waste; against
//! `TAR_SPARQL_ENDPOINT` it is 12 000 quads over HTTP at every restart. Worse, the write-path
//! check "is this a term the registry holds" (`crate::domain::vocabulary::held`) asked the
//! record store, which put an HTTP round trip inside every single write.
//!
//! So the bundles now go to two places, for two different reasons.
//!
//! # The reference store — an in-memory Oxigraph, always
//!
//! [`reference_store`] is where every hot reference read goes, above all the write-path
//! vocabulary check. It is in memory, so it starts empty and loading it on every boot is
//! correct by construction: no hash guard, no staleness, no drop-and-reload, and never a
//! network call. It is populated before the first request, from the compiled-in constants
//! alone.
//!
//! It is read-only after construction and depends on nothing but those constants and the base
//! IRI (which decides where the keyword concepts live and how a relative IRI in a bundle
//! resolves), so one is built per base IRI per process and shared. That matters for the test
//! suite, which builds a hundred registries in one process and would otherwise parse 550 kB of
//! Turtle a hundred times.
//!
//! # The record store — the same bundles again, guarded by a content hash
//!
//! The record store keeps its own copy, because `/sparql` has to be able to join a record to
//! the vocabulary term it cites, and a federating peer has to be able to fetch the definition
//! of a type we handed it. That copy is written **only when it would differ**: each bundle's
//! graph carries a content digest and a load timestamp in `<urn:tar:bundles>`, and a boot that
//! finds every digest unchanged issues one SELECT and no writes at all.
//!
//! # One graph per bundle
//!
//! Each bundle gets its own named graph and is the only writer of it, structurally rather than
//! by convention — a graph that is dropped and reloaded from a file must contain only what the
//! binary can regenerate, and the previous layout put four bundles, the seeded artifact types
//! and every adopted term into `<urn:tar:vocab>` together. Re-registering a seeded type then
//! had to reach into the vocabulary graph to clear a stale copy, which is the shape of bug this
//! layout removes rather than documents.
//!
//! **The names are provenance.** `urn:tar:bundle:edam` says "these quads came from the edam
//! bundle", which is what a graph name is for and what `tar dump` has to be able to round-trip.
//! It deliberately does *not* say what kind of term is inside: two bundles hold artifact types
//! and two hold subject areas, so a name like `urn:tar:types` would be false the day a second
//! bundle carries one. No API field, response value or UI label names a vocabulary; a graph IRI
//! records where a statement came from, and that is a different question.
//!
//! `<urn:tar:shapes>` keeps the name it has always had: it was already exactly one bundle in
//! one graph with one writer, and renaming it would break every saved query and the graph list
//! on the query page for nothing.

use crate::domain::keywords;
use crate::ns;
use crate::rdf::Node;
use crate::state::AppState;
use crate::store::{GraphStore, GraphTx, OxigraphStore};
use anyhow::{Context, Result};
use oxigraph::model::Quad;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The registry's own terms, plus a working subset of EDAM, preloaded so that chips render
/// labels without a network call. Since a write may only name a term the registry holds
/// (`crate::domain::vocabulary`), what is loaded here is also part of what is accepted.
pub const VOCAB_TTL: &str = include_str!("../shapes/vocab.ttl");
/// The shape set enforced on every write (spec §5.3). Also loaded into the store so it is
/// queryable and downloadable by any SHACL processor — `state.shapes` is parsed from this same
/// constant, so the stored copy is for `/sparql` and for a peer, never for validation.
pub const SHAPES_TTL: &str = include_str!("../shapes/tar-shapes.ttl");
/// EDAM's topic and data branches, bundled so the pickers work offline. See `shapes/edam.ttl`.
pub const EDAM_TTL: &str = include_str!("../shapes/edam.ttl");
/// EuroSciVoc, the EU Science Vocabulary — the topics a Software record is classified by.
/// See `shapes/euroscivoc.ttl` for why it is this and not EDAM's topic branch.
pub const EUROSCIVOC_TTL: &str = include_str!("../shapes/euroscivoc.ttl");

/// Bumped when the *shape* of what a bundle writes changes without the source changing — a new
/// class on every keyword, say. It is part of every digest, so one edit here reloads every
/// bundle in every store on its next boot, which is the only way a stored copy written by an
/// older build gets corrected.
const LAYOUT_VERSION: &str = "2";

/// Where a bundle's quads come from.
enum Body {
    /// A Turtle file compiled into the binary, parsed against the registry's base IRI.
    Turtle(&'static str),
    /// The artifact keyword scheme, built from `crate::domain::keywords` — the authoritative
    /// list is the Rust table, and this is what puts it in the graph so that the pickers, the
    /// vocabulary search and a federating peer all see it as an ordinary set of concepts.
    Keywords,
}

pub struct Bundle {
    /// Short name, used in the graph IRI and in log lines.
    pub name: &'static str,
    /// Where it comes from, for a human reading `<urn:tar:bundles>`.
    pub source: &'static str,
    /// The named graph this bundle owns outright.
    pub graph: &'static str,
    body: Body,
}

/// Every bundle, in load order. Adding one here is the whole of adding a bundle: it is loaded
/// into both stores, hash-guarded, dumped, restored and described without another edit.
pub const BUNDLES: &[Bundle] = &[
    Bundle {
        name: "vocab",
        source: "shapes/vocab.ttl",
        graph: "urn:tar:bundle:vocab",
        body: Body::Turtle(VOCAB_TTL),
    },
    Bundle {
        name: "shapes",
        source: "shapes/tar-shapes.ttl",
        // The one bundle graph not under `urn:tar:bundle:` — it was already one bundle in one
        // graph under this name, and the name is on the query page and in saved queries.
        graph: ns::G_SHAPES,
        body: Body::Turtle(SHAPES_TTL),
    },
    Bundle {
        name: "edam",
        source: "shapes/edam.ttl",
        graph: "urn:tar:bundle:edam",
        body: Body::Turtle(EDAM_TTL),
    },
    Bundle {
        name: "euroscivoc",
        source: "shapes/euroscivoc.ttl",
        graph: "urn:tar:bundle:euroscivoc",
        body: Body::Turtle(EUROSCIVOC_TTL),
    },
    Bundle {
        name: "keywords",
        source: "src/domain/keywords.rs",
        graph: "urn:tar:bundle:keywords",
        body: Body::Keywords,
    },
];

/// Whether a graph is one the binary owns and may drop and rewrite.
///
/// Every migration that rewrites statements in place asks this first. A migration that touched
/// a bundle graph would either be undone by the next reload or, worse, survive as the one
/// statement in it that the file does not contain.
pub fn is_bundle_graph(graph: &str) -> bool {
    graph == ns::G_BUNDLES || BUNDLES.iter().any(|b| b.graph == graph)
}

impl Bundle {
    /// The content digest: what this bundle would write, hashed.
    ///
    /// The base IRI is in it because the base decides where the keyword concepts live and how a
    /// relative IRI inside a Turtle bundle resolves — the same file loaded under two bases is
    /// two different sets of quads, and a digest that ignored that would leave a re-based store
    /// holding the old registry's IRIs forever.
    fn digest(&self, base: &str) -> Result<String> {
        let mut h = Sha256::new();
        h.update(LAYOUT_VERSION.as_bytes());
        h.update(b"\0");
        h.update(self.name.as_bytes());
        h.update(b"\0");
        h.update(self.graph.as_bytes());
        h.update(b"\0");
        h.update(base.as_bytes());
        h.update(b"\0");
        match &self.body {
            Body::Turtle(text) => h.update(text.as_bytes()),
            // No file to hash, so hash what it produces. Sorted, because quad order is not
            // something the table promises and a reordering is not a change.
            Body::Keywords => {
                let mut lines: Vec<String> =
                    keyword_quads(base).iter().map(|q| q.to_string()).collect();
                lines.sort();
                h.update(lines.join("\n").as_bytes());
            }
        }
        Ok(hex::encode(h.finalize()))
    }

    /// Load this bundle into a store, replacing whatever was in its graph.
    ///
    /// `load_turtle` rather than a `GraphTx`: an external endpoint chunks a bulk load into
    /// requests it can actually accept, where `apply` deliberately sends one body.
    fn load_into(&self, store: &dyn GraphStore, base: &str) -> Result<usize> {
        store.drop_graph(self.graph).with_context(|| format!("dropping {}", self.graph))?;
        match &self.body {
            Body::Turtle(text) => store
                .load_turtle(text, self.graph, Some(base))
                .with_context(|| format!("loading {}", self.source)),
            Body::Keywords => {
                let quads = keyword_quads(base);
                let n = quads.len();
                let mut tx = GraphTx::new();
                tx.extend(quads);
                store.apply(tx).context("loading the artifact keyword scheme")?;
                Ok(n)
            }
        }
    }

    /// The provenance node for this bundle's graph: what it holds, how big it is, what it
    /// hashes to and when this registry last wrote it.
    ///
    /// **Which vocabulary, and what was rejected.** The subject is the graph IRI and the
    /// statements are about an RDF graph, which is precisely what VoID describes — so
    /// `void:Dataset` and `void:triples`. `dcat:Dataset` was rejected: these are the registry's
    /// internal graphs, not entries in its catalogue, and typing them `dcat:Dataset` would put
    /// five of them into every DCAT crawl of this endpoint. `sd:NamedGraph` (SPARQL 1.1 Service
    /// Description) was rejected too: it describes what an *endpoint* offers, and reaching the
    /// graph through `sd:name`/`sd:graph` indirection buys nothing here.
    ///
    /// The timestamp is `dct:modified` — when this copy of the graph last changed, which is
    /// exactly what it means. `dct:issued`/`dct:created` would be claims about the bundle's own
    /// history, which this registry does not know; `prov:generatedAtTime` says the same thing
    /// as `dct:modified` and would pull PROV into a node that is pure administrative metadata.
    ///
    /// The digest is `spdx:checksum` on a node typed `spdx:Checksum`, the same three triples
    /// the registry already writes for an artifact distribution
    /// (`crate::domain::artifact::distribution_quads`). Reusing them means no new vocabulary at
    /// all and one spelling of "digest" in the store, which is the whole reason `tar:` terms
    /// have to be argued for one at a time.
    fn description(&self, digest: &str, triples: usize) -> Vec<Quad> {
        let mut n = Node::iri(self.graph, ns::G_BUNDLES);
        n.a(&format!("{}Dataset", ns::VOID));
        n.text(ns::DCT, "title", self.source);
        n.int(ns::VOID, "triples", triples as i64);
        n.datetime(ns::DCT, "modified", &chrono::Utc::now().to_rfc3339());
        let mut c = Node::blank(ns::G_BUNDLES);
        c.a(&format!("{}Checksum", ns::SPDX));
        c.link(ns::SPDX, "algorithm", &format!("{}checksumAlgorithm_sha256", ns::SPDX));
        c.text(ns::SPDX, "checksumValue", digest);
        n.child(ns::SPDX, "checksum", c);
        n.finish()
    }
}

/// The artifact keyword scheme as quads, in its own bundle graph.
pub fn keyword_quads(base: &str) -> Vec<Quad> {
    let scheme = keywords::scheme_iri(base);
    let graph = bundle("keywords").graph;
    let mut out = Vec::new();
    let mut sn = Node::iri(&scheme, graph);
    sn.a(&format!("{}ConceptScheme", ns::SKOS));
    sn.text(ns::SKOS, "prefLabel", "Artifact keywords");
    sn.text(
        ns::SKOS,
        "definition",
        "The keywords this registry recognises on artifacts. A keyword outside the list is kept as free text.",
    );
    out.extend(sn.finish());
    for k in keywords::KEYWORDS {
        let iri = keywords::iri(base, k.slug);
        let mut n = Node::iri(&iri, graph);
        n.a(&format!("{}Concept", ns::SKOS));
        n.text(ns::SKOS, "prefLabel", k.label);
        n.text(ns::SKOS, "definition", k.definition);
        for a in k.aliases {
            n.text(ns::SKOS, "altLabel", a);
        }
        n.link(ns::SKOS, "inScheme", &scheme);
        // The same kind of class the type and topic vocabularies carry, so `vocab_search` scopes
        // to keywords knowing nothing about this scheme in particular — and so that a keyword,
        // which is a label rather than a type, is refused where a type is expected.
        n.a(crate::domain::vocabulary::CLASS_ARTIFACT_KEYWORD);
        out.extend(n.finish());
    }
    out
}

fn bundle(name: &str) -> &'static Bundle {
    BUNDLES.iter().find(|b| b.name == name).expect("every bundle named here is in BUNDLES")
}

// ---------------------------------------------------------------------------- reference store

/// One reference store per base IRI, for the life of the process.
///
/// Shared because it is immutable after construction and is a pure function of the compiled-in
/// bundles and the base IRI. A registry has one base; the test suite has a handful and builds a
/// hundred registries, which is the case this exists for.
static REFERENCE: OnceLock<Mutex<HashMap<String, Arc<dyn GraphStore>>>> = OnceLock::new();

/// The in-memory store holding every bundle, ready before the first request.
///
/// Panics if a bundle will not parse, for the reason `Shapes::parse` does: a registry that
/// cannot load its own reference data would refuse every write that names a term, and finding
/// that out one request at a time is worse than not starting.
pub fn reference_store(base: &str) -> Arc<dyn GraphStore> {
    let cache = REFERENCE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = guard.get(base) {
        return s.clone();
    }
    let store = Arc::new(OxigraphStore::memory().expect("an in-memory store must open"));
    for b in BUNDLES {
        b.load_into(store.as_ref(), base)
            .unwrap_or_else(|e| panic!("the bundled reference data must load: {b} — {e:#}"));
    }
    let store: Arc<dyn GraphStore> = store;
    guard.insert(base.to_string(), store.clone());
    store
}

impl std::fmt::Display for Bundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.name, self.source)
    }
}

// ---------------------------------------------------------------------------- record store

/// Bring the record store's copy of the bundles up to date, and write nothing if it already is.
///
/// One SELECT, always. Then, per bundle whose digest differs, a `DROP GRAPH`, a bulk load and
/// one small transaction carrying the new provenance node. A boot against an unchanged store
/// issues the SELECT and stops — which is the whole point on a remote backend, where the old
/// behaviour was 12 000 quads over HTTP every time.
pub fn sync(state: &AppState) -> Result<usize> {
    let base = &state.config.base_iri;
    let stored = stored_digests(state.store.as_ref())?;
    let mut written = 0;
    for b in BUNDLES {
        let digest = b.digest(base)?;
        if stored.get(b.graph) == Some(&digest) {
            continue;
        }
        let n = b.load_into(state.store.as_ref(), base)?;
        let mut tx = GraphTx::new();
        // The provenance node is replaced with its graph, not accumulated: the checksum hangs
        // off a blank node, and a blank node is a fresh identifier on every parse.
        tx.replace_subject(b.graph, ns::G_BUNDLES);
        tx.extend(b.description(&digest, n));
        state.store.apply(tx)?;
        tracing::info!(bundle = %b.name, graph = %b.graph, quads = n, "reference bundle loaded");
        written += n;
    }
    if written == 0 {
        tracing::debug!("reference bundles unchanged; nothing written to the record store");
    }
    Ok(written)
}

/// The digest recorded for each bundle graph, from `<urn:tar:bundles>`.
///
/// A store that has never seen this layout answers with nothing, which loads every bundle once
/// and records a digest for each — that is the upgrade, and it needs no version flag.
fn stored_digests(store: &dyn GraphStore) -> Result<HashMap<String, String>> {
    let q = format!(
        "{p}\nSELECT ?g ?v WHERE {{ GRAPH <{bundles}> {{ ?g <{spdx}checksum> ?c . ?c <{spdx}checksumValue> ?v }} }}",
        p = ns::PREFIXES,
        bundles = ns::G_BUNDLES,
        spdx = ns::SPDX
    );
    let rows = store.select(&q).context("reading the recorded bundle digests")?;
    Ok(rows.rows.iter().filter_map(|r| Some((r.iri("g")?, r.str("v")?))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundle_has_its_own_graph_and_nothing_else_writes_there() {
        let mut graphs: Vec<&str> = BUNDLES.iter().map(|b| b.graph).collect();
        graphs.sort();
        let n = graphs.len();
        graphs.dedup();
        assert_eq!(graphs.len(), n, "two bundles share a graph, so neither owns it");
        for b in BUNDLES {
            assert!(is_bundle_graph(b.graph), "{b}");
            // Not the record graph, and not a peer's: those have their own writers.
            assert_ne!(b.graph, ns::G_LOCAL);
            assert!(!b.graph.starts_with(ns::G_PEER_PREFIX));
        }
        assert!(is_bundle_graph(ns::G_BUNDLES));
        assert!(!is_bundle_graph(ns::G_LOCAL));
        assert!(!is_bundle_graph(ns::G_LEGACY_VOCAB), "the legacy graph is migrated, not owned");
    }

    /// The guard is only worth having if it actually distinguishes. Same base, same bytes, same
    /// digest; a different base is a different set of quads and must reload.
    #[test]
    fn a_digest_changes_with_the_base_iri_and_not_otherwise() {
        for b in BUNDLES {
            let a = b.digest("https://reg.one.example").unwrap();
            assert_eq!(a, b.digest("https://reg.one.example").unwrap(), "{b}");
            assert_ne!(a, b.digest("https://reg.two.example").unwrap(), "{b}");
        }
    }

    #[test]
    fn the_reference_store_holds_every_bundle_and_is_shared_per_base() {
        let base = "https://reg.reference-test.example";
        let s = reference_store(base);
        for b in BUNDLES {
            let n = s
                .select(&format!("SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{}> {{ ?s ?p ?o }} }}", b.graph))
                .unwrap()
                .rows
                .first()
                .and_then(|r| r.i64("n"))
                .unwrap();
            assert!(n > 0, "{b} is empty in the reference store");
        }
        assert!(Arc::ptr_eq(&s, &reference_store(base)), "one store per base, not one per caller");
    }

    /// The keyword scheme is a bundle like any other, and its concepts must carry the class the
    /// pickers filter on or the keyword picker offers nothing.
    #[test]
    fn the_keyword_scheme_is_a_bundle_with_classed_concepts() {
        let base = "https://reg.keywords-test.example";
        let s = reference_store(base);
        let hits = s
            .select(&format!(
                "SELECT ?c WHERE {{ GRAPH <{g}> {{ ?c a <{class}> }} }}",
                g = bundle("keywords").graph,
                class = crate::domain::vocabulary::CLASS_ARTIFACT_KEYWORD
            ))
            .unwrap();
        assert_eq!(hits.rows.len(), keywords::KEYWORDS.len(), "every keyword, classed");
    }
}
