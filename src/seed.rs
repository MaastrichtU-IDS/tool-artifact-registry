//! Bootstrap content (spec §10.7) and the boot-time graph work.
//!
//! [`load_vocab`] runs at every start, from `main::boot` and from the test harness. It brings
//! the record store's copy of the bundled reference data up to date — see [`crate::bundles`]
//! for the split between that copy and the in-memory reference store — and then applies the
//! graph migrations, each of which is a correction to something an older build wrote.
//!
//! `tar seed --from ids-examples` registers the sibling repos with their declared
//! capabilities, so a fresh install is demonstrable immediately rather than being an empty
//! table with a "register software" button.

use crate::domain::{instance as instdom, software as swdom};
use crate::ids::{self, Kind};
use crate::model::*;
use crate::ns;
use crate::state::AppState;
use crate::store::GraphTx;
use anyhow::Result;
use std::sync::Arc;

pub fn load_vocab(state: &AppState) -> Result<usize> {
    // The bundled reference data, into the record store, only when its content hash says it
    // would differ. The in-memory reference store every hot read goes to is loaded in
    // `AppState::from_parts` and needs no guard, because memory starts empty.
    let loaded = crate::bundles::sync(state)?;
    // Graph migrations, in dependency order: the legacy vocabulary graph is emptied into
    // `<urn:tar:local>` first, so that a type rescued out of it is one of the concepts
    // `class_existing_types` then classes.
    let migrated = adopt_legacy_vocab_graph(state)?
        + class_existing_types(state)?
        + drop_branch_markers(state)?
        + unclaim_series_concepts(state)?;
    warn_about_unusable_stored_terms(state);
    Ok(loaded + migrated)
}

/// Empty `<urn:tar:vocab>` — the graph that used to hold four bundles, the seeded artifact
/// types and every adopted term at once — and drop it.
///
/// Everything the binary can regenerate is now in its own bundle graph and has just been
/// reloaded there, so those quads are duplicates and go. What cannot be regenerated is a
/// registry-local or adopted type that `tar seed` or an older `POST /api/v1/types` wrote into
/// the vocabulary graph, and losing one would break every record citing it: those move to
/// `<urn:tar:local>`, which is where this build writes them and where they should always have
/// been.
///
/// **Regenerable is decided by asking, not by guessing.** A subject that appears in any bundle
/// graph is one of the bundles' own and is dropped; anything else is rescued. That is a
/// question about the store as it is now, so it stays right as bundles are added or trimmed.
///
/// **A copy already in `<urn:tar:local>` wins.** `POST /api/v1/types` used to clear both graphs
/// and write to the local one, so a subject in both is a stale vocabulary-graph copy of a term
/// that has since been re-registered — the exact split that made one IRI carry two
/// `skos:prefLabel`s. Merging them would recreate it.
///
/// One cheap `ASK` guards the whole thing, so a store that never had the legacy graph pays for
/// one boolean and a registry that has migrated once never pays again.
fn adopt_legacy_vocab_graph(state: &AppState) -> Result<usize> {
    let legacy = ns::G_LEGACY_VOCAB;
    if !state.store.ask(&format!("ASK {{ GRAPH <{legacy}> {{ ?s ?p ?o }} }}"))? {
        return Ok(0);
    }
    let q = format!(
        r#"{p}
SELECT DISTINCT ?s WHERE {{
  GRAPH <{legacy}> {{ ?s ?p ?o }}
  FILTER(isIRI(?s))
  FILTER NOT EXISTS {{ GRAPH ?b {{ ?s ?bp ?bo }} FILTER(STRSTARTS(STR(?b), "{bundle}") || ?b = <{shapes}>) }}
  FILTER NOT EXISTS {{ GRAPH <{local}> {{ ?s ?lp ?lo }} }}
}}"#,
        p = ns::PREFIXES,
        bundle = ns::G_BUNDLE_PREFIX,
        shapes = ns::G_SHAPES,
        local = ns::G_LOCAL
    );
    let rescue: Vec<String> = state.store.select(&q)?.rows.iter().filter_map(|r| r.iri("s")).collect();

    let mut tx = GraphTx::new();
    if !rescue.is_empty() {
        let values = rescue.iter().map(|i| format!("<{i}>")).collect::<Vec<_>>().join(" ");
        let statements = format!(
            "SELECT ?s ?p ?o WHERE {{ VALUES ?s {{ {values} }} GRAPH <{legacy}> {{ ?s ?p ?o }} }}"
        );
        for row in &state.store.select(&statements)?.rows {
            let (Some(s), Some(p), Some(o)) = (row.term("s"), row.term("p"), row.term("o")) else {
                continue;
            };
            let (oxigraph::model::Term::NamedNode(s), oxigraph::model::Term::NamedNode(p)) = (s, p)
            else {
                continue;
            };
            tx.insert(oxigraph::model::Quad::new(
                s.clone(),
                p.clone(),
                o.clone(),
                oxigraph::model::GraphName::NamedNode(oxigraph::model::NamedNode::new_unchecked(ns::G_LOCAL)),
            ));
        }
    }
    let n = tx.insert.len();
    if n > 0 {
        state.store.apply(tx)?;
        tracing::info!(
            subjects = rescue.len(),
            quads = n,
            "moved terms this registry minted or adopted out of the retired vocabulary graph"
        );
    }
    state.store.drop_graph(legacy)?;
    Ok(n)
}

/// Give the types of a registry that predates the concept classes the class they should carry.
///
/// The bundled vocabularies and the keyword scheme are reloaded above on every start, so those
/// pick their classes up for free. A type this registry minted or adopted does not: it was
/// written once, by `api::types::create`, and nothing rewrites it.
///
/// Only `<urn:tar:local>` is considered. A peer's cached graph is left exactly as the peer
/// served it, for the same reason a peer's record never passes a write handler: this registry
/// is not authoritative for what a peer's terms are. A bundle graph is left alone because the
/// classes are in the bundle — and because a statement written into one that its file does not
/// contain would be undone by the next reload, or survive as the one line in it nothing can
/// reproduce. The types that used to sit in the retired vocabulary graph are in
/// `<urn:tar:local>` by the time this runs; `adopt_legacy_vocab_graph` puts them there.
///
/// **Which graph.** The concept's own, found per concept — not a graph this file picks. The
/// marker this replaces was backfilled into `<urn:tar:vocab>` while the concepts it was about
/// sat in `<urn:tar:local>`, and since every query that reads a concept and its kind asks for
/// both inside one `GRAPH` block, the result was types that were held and accepted on write and
/// that no picker would offer. Writing into `?g` is the fix, and it is the whole reason the kind
/// is a class: from here on the two triples are made in the same statement and cannot be split.
///
/// **Which concepts.** A `…/type/…` IRI is an artifact type by construction, and `<urn:tar:local>`
/// is where `api::types::create` puts a term adopted under its own foreign identifier — the one
/// path that writes a concept into that graph. Everything else there that is nevertheless shaped
/// like a record of some registry is skipped, because a version-series node used to be typed
/// `skos:Concept` too and classing those would put every artifact's title into the type picker.
///
/// Idempotent: a concept that already carries any of the concept classes is not selected.
fn class_existing_types(state: &AppState) -> Result<usize> {
    let classes = crate::domain::vocabulary::CONCEPT_CLASSES
        .map(|c| format!("<{c}>"))
        .join(", ");
    let q = format!(
        r#"{p}
SELECT DISTINCT ?c ?g WHERE {{
  GRAPH <{local}> {{ ?c a skos:Concept }}
  BIND(<{local}> AS ?g)
  FILTER NOT EXISTS {{ GRAPH ?cg {{ ?c a ?any . FILTER(?any IN ({classes})) }} }}
}}"#,
        p = ns::PREFIXES,
        local = ns::G_LOCAL
    );
    let rows = state.store.select(&q)?;
    let mut tx = GraphTx::new();
    let mut n = 0;
    for row in &rows.rows {
        let (Some(iri), Some(graph)) = (row.iri("c"), row.iri("g")) else { continue };
        if is_a_record_that_is_not_a_type(&iri) {
            continue;
        }
        let mut node = crate::rdf::Node::iri(&iri, &graph);
        node.a(crate::domain::vocabulary::CLASS_ARTIFACT_TYPE);
        tx.extend(node.finish());
        n += 1;
    }
    if n > 0 {
        state.store.apply(tx)?;
    }
    Ok(n)
}

/// Whether an IRI is shaped like a registry record of some kind other than a type.
///
/// Read off the path, not off this registry's own base. A store served under a different
/// `TAR_BASE_IRI` than the one that wrote it — a restored dump, a copy brought up on another
/// port — still holds records whose IRIs say what they are, and a guard that asked "is this one
/// of ours" would wave every one of them through as an adopted term. That is not hypothetical:
/// it is what the first version of this did, and a `…/artifact-series/…` node from a store on
/// another port came back as an artifact type.
fn is_a_record_that_is_not_a_type(iri: &str) -> bool {
    let mut segments = iri.rsplit('/');
    let (Some(id), Some(kind)) = (segments.next(), segments.next()) else { return false };
    !id.is_empty() && matches!(ids::Kind::from_segment(kind), Some(k) if k != ids::Kind::Type)
}

/// Retire the literal the classes replace.
///
/// Left in place it is a second answer to "what kind of concept is this", written by nothing and
/// read by nothing, waiting for somebody to trust it. Removing it is the migration's other half
/// and costs one pass; a second start finds none.
///
/// Peer graphs are left alone. A peer still running an older build may serve the marker in its
/// stub, and a cached copy of what a peer said is not ours to edit — the same reason a peer's
/// record never passes a write handler.
/// Say plainly, once at boot, which stored records a curator can no longer edit.
///
/// The vocabulary rule judges the whole record a write asserts, and a PATCH carries the fields
/// the caller did not name — so a record written by an older build against a term this registry
/// has since retired fails on a field nobody touched, with a message about that field rather
/// than about the upgrade. Nothing in the shipped data is affected, but a store that predates
/// the rule can be, and finding out by having an unrelated edit refused is a poor way to learn
/// it. The data is left exactly as it is: a stale subject is the operator's to correct or keep,
/// and quietly deleting a term from someone's records to make an edit succeed would be a worse
/// trade than telling them.
fn warn_about_unusable_stored_terms(state: &AppState) {
    let q = format!(
        r#"{p}
SELECT ?s ?t WHERE {{
  GRAPH ?g {{ ?s dct:subject ?t }}
  FILTER(!STRSTARTS(STR(?g), "{peer}"))
}} LIMIT 200"#,
        p = ns::PREFIXES,
        peer = ns::G_PEER_PREFIX
    );
    let Ok(rows) = state.store.select(&q) else { return };
    let pairs: Vec<(String, String)> =
        rows.rows.iter().filter_map(|r| Some((r.iri("s")?, r.iri("t")?))).collect();
    let terms: Vec<&str> = pairs.iter().map(|(_, t)| t.as_str()).collect();
    let Some(held) = crate::domain::vocabulary::held(state, &terms) else { return };

    let mut stuck: Vec<&(String, String)> = pairs
        .iter()
        .filter(|(_, t)| {
            held.get(t).is_none_or(|h| !h.usable_as(crate::domain::vocabulary::Slot::Topic))
        })
        .collect();
    stuck.sort();
    stuck.dedup();
    if stuck.is_empty() {
        return;
    }
    tracing::warn!(
        count = stuck.len(),
        "records hold a topic this registry no longer classifies software by; editing one will \
         be refused until the topic is replaced or removed, even on an unrelated field"
    );
    for (record, term) in stuck.iter().take(10) {
        tracing::warn!(record = %record, term = %term, "stale topic");
    }
}

/// Stop version-series nodes claiming to be vocabulary concepts.
///
/// A series is the idea of "this artifact, any version", and an early build typed it
/// `skos:Concept` to say so — which put every artifact's *title* into the artifact-type picker.
/// The minting was corrected long before the type rule existed, but records written in between
/// still carry the triple: harmless now that the rule refuses them, and still a statement the
/// graph makes that is not true. Only the `skos:Concept` claim goes; `tar:ArtifactSeries` and
/// the series' own label stay, because the series is real.
fn unclaim_series_concepts(state: &AppState) -> Result<usize> {
    let q = format!(
        r#"{p}
SELECT DISTINCT ?s ?g WHERE {{
  GRAPH ?g {{ ?s a skos:Concept ; a <{series}> }}
  FILTER(!STRSTARTS(STR(?g), "{peer}") && !STRSTARTS(STR(?g), "{bundle}") && ?g != <{shapes}>)
}}"#,
        p = ns::PREFIXES,
        series = crate::domain::artifact::TYPE_ARTIFACT_SERIES,
        peer = ns::G_PEER_PREFIX,
        bundle = ns::G_BUNDLE_PREFIX,
        shapes = ns::G_SHAPES
    );
    let rows = state.store.select(&q)?;
    let mut tx = GraphTx::new();
    let mut n = 0;
    for row in &rows.rows {
        let (Some(iri), Some(graph)) = (row.iri("s"), row.iri("g")) else { continue };
        // There is no delete-one-quad; clearing the property and re-asserting the type we want
        // is the same thing in one transaction, and a series has no other type to lose.
        tx.replace_property(&iri, &format!("{}type", ns::RDF), &graph);
        let mut node = crate::rdf::Node::iri(&iri, &graph);
        node.a(crate::domain::artifact::TYPE_ARTIFACT_SERIES);
        tx.extend(node.finish());
        n += 1;
    }
    if n > 0 {
        state.store.apply(tx)?;
    }
    Ok(n)
}

fn drop_branch_markers(state: &AppState) -> Result<usize> {
    let predicate = format!("{}conceptBranch", ns::TAR);
    let q = format!(
        r#"{p}
SELECT DISTINCT ?c ?g WHERE {{
  GRAPH ?g {{ ?c <{predicate}> ?b }}
  FILTER(!STRSTARTS(STR(?g), "{peer}") && !STRSTARTS(STR(?g), "{bundle}") && ?g != <{shapes}>)
}}"#,
        p = ns::PREFIXES,
        peer = ns::G_PEER_PREFIX,
        bundle = ns::G_BUNDLE_PREFIX,
        shapes = ns::G_SHAPES
    );
    let rows = state.store.select(&q)?;
    let mut tx = GraphTx::new();
    let mut n = 0;
    for row in &rows.rows {
        let (Some(iri), Some(graph)) = (row.iri("c"), row.iri("g")) else { continue };
        tx.replace_property(&iri, &predicate, &graph);
        n += 1;
    }
    if n > 0 {
        state.store.apply(tx)?;
    }
    Ok(n)
}

/// The seeded software topics, by the identifiers the topic vocabulary uses.
///
/// These were EDAM topic IRIs, left behind when software classification moved to EuroSciVoc.
/// They rendered labels, so nothing looked broken — but they are terms the topic picker does not
/// offer and a write now refuses, which would have made the shipped records the one set of
/// records nobody could reproduce.
const ESV: &str = "http://data.europa.eu/8mn/euroscivoc/";
const T_SEMANTIC_WEB: &str = "981a4eb6-f63a-4360-953d-efe0ec861672";
const T_ONTOLOGY: &str = "123e5118-1586-4a45-b4da-34583bd74940";
const T_DATABASES: &str = "1f6c74df-a512-462e-99aa-8dcbaa98972a";
const T_SOFTWARE: &str = "aafff649-e02a-496e-b436-284ce76044c4";

fn topic(id: &str) -> String {
    format!("{ESV}{id}")
}

fn local_type(base: &str, slug: &str) -> String {
    format!("{base}/type/{slug}")
}

/// Local ArtifactTypes for the things EDAM does not name — SHACL shape graphs, OBDA mappings,
/// hash-chained patch logs (D11).
fn local_types(base: &str) -> Vec<(String, &'static str, &'static str, &'static str)> {
    vec![
        (local_type(base, "rdf-graph"), "RDF graph", "An RDF graph in any serialisation. EDAM has no general term for this, so the registry defines one (D11).", "text/turtle"),
        (local_type(base, "shacl-shapes-graph"), "SHACL shapes graph", "An RDF graph of SHACL shapes used to validate other graphs.", "text/turtle"),
        (local_type(base, "shacl-validation-report"), "SHACL validation report", "An RDF validation report produced by a SHACL processor.", "text/turtle"),
        (local_type(base, "conformance-summary"), "Conformance summary", "A human-readable summary of a validation run.", "application/json"),
        (local_type(base, "schema-model"), "Schema model", "A structural description of a dataset schema.", "application/json"),
        (local_type(base, "sulo-ontology"), "SULO ontology", "The SULO upper ontology, or a module of it.", "text/turtle"),
        (local_type(base, "owl-ontology"), "OWL ontology", "An OWL ontology serialised as RDF.", "text/turtle"),
        (local_type(base, "mermaid-uml"), "Mermaid UML diagram", "A class diagram in Mermaid syntax.", "text/vnd.mermaid"),
        (local_type(base, "sparql-update"), "SPARQL update", "A SPARQL 1.1 Update request.", "application/sparql-update"),
        (local_type(base, "rdf-quads"), "RDF quads", "A named-graph RDF dataset.", "application/n-quads"),
        (local_type(base, "patch-log"), "Hash-chained patch log", "An append-only log of RDF patches, each linked to its predecessor by hash.", "application/n-quads"),
        (local_type(base, "masked-replica"), "Masked RDF replica", "A privacy-masked copy of an RDF dataset.", "application/n-quads"),
        (local_type(base, "relational-source"), "Relational source", "A relational database exposed for virtualisation.", "application/sql"),
        (local_type(base, "r2rml-mapping"), "R2RML/RML mapping", "A declarative mapping from relational or heterogeneous data to RDF.", "text/turtle"),
        (local_type(base, "materialised-view"), "Materialised RDF view", "RDF materialised from a virtual mapping.", "application/n-quads"),
        (local_type(base, "mapping-coverage-report"), "Mapping coverage report", "Which parts of a source a mapping covers.", "application/json"),
    ]
}

/// The seeded artifact types, as records.
///
/// They go into `<urn:tar:local>`, which is where `api::types::create` writes a minted or
/// adopted type and where `GET /api/v1/types` looks. They used to go into the vocabulary graph
/// beside the bundles, which meant re-registering one left the seeded definition standing beside
/// the new one — the same IRI with two `skos:prefLabel`s, the picker showing whichever it read
/// first, and a curator getting a 200 for a rename that never appeared. A bundle graph is
/// dropped and reloaded from a file, so nothing that a file cannot reproduce may live in one.
fn type_quads(base: &str) -> Vec<oxigraph::model::Quad> {
    let mut out = Vec::new();
    for (iri, label, definition, media) in local_types(base) {
        let mut n = crate::rdf::Node::local(&iri);
        n.a(&format!("{}Concept", ns::SKOS));
        n.text(ns::SKOS, "prefLabel", label);
        n.text(ns::SKOS, "definition", definition);
        n.text(ns::TAR, "defaultMediaType", media);
        // Same class the bundled vocabularies carry, or the type picker — which filters on it —
        // would offer everything except the registry's own types, which are most of what this
        // estate actually produces.
        n.a(crate::domain::vocabulary::CLASS_ARTIFACT_TYPE);
        out.extend(n.finish());
    }
    out
}

struct SeedSoftware {
    name: &'static str,
    tagline: &'static str,
    description: &'static str,
    repo: &'static str,
    kind: &'static str,
    topics: Vec<String>,
    keywords: Vec<&'static str>,
    consumes: Vec<String>,
    produces: Vec<String>,
    version: &'static str,
    image: &'static str,
    instances: Vec<(&'static str, &'static str, Option<&'static str>)>,
}

fn ids_examples(base: &str) -> Vec<SeedSoftware> {
    vec![
        SeedSoftware {
            name: "shacl-manager",
            tagline: "SHACL shape management and validation",
            description: "Multi-tenant platform for managing SHACL shape graphs and validating RDF against them. Emits validation reports and conformance summaries.",
            repo: "https://github.com/MaastrichtU-IDS/shacl-manager",
            kind: "service",
            topics: vec![topic(T_SEMANTIC_WEB), topic(T_DATABASES)],
            keywords: vec!["shacl", "validation", "rdf", "data quality"],
            consumes: vec![local_type(base, "rdf-graph"), local_type(base, "shacl-shapes-graph")],
            produces: vec![local_type(base, "shacl-validation-report"), local_type(base, "conformance-summary")],
            version: "2.1.0",
            image: "ghcr.io/maastrichtu-ids/shacl-manager:2.1.0",
            instances: vec![
                ("shacl.ids.unimaas.nl", "https://shacl.ids.unimaas.nl", Some("shacl-manager-ids3")),
                ("laptop-eerol", "", None),
            ],
        },
        SeedSoftware {
            name: "sulo-schema-builder",
            tagline: "Build SULO-aligned schemas from data models",
            description: "Turns a schema model into SULO-aligned RDF, OWL ontologies, SHACL shapes and Mermaid class diagrams.",
            repo: "https://github.com/MaastrichtU-IDS/sulo-schema-builder",
            kind: "service",
            topics: vec![topic(T_ONTOLOGY), topic(T_SEMANTIC_WEB)],
            keywords: vec!["ontology", "sulo", "owl", "schema"],
            consumes: vec![local_type(base, "schema-model"), local_type(base, "sulo-ontology")],
            produces: vec![
                local_type(base, "rdf-graph"),
                local_type(base, "owl-ontology"),
                local_type(base, "shacl-shapes-graph"),
                local_type(base, "mermaid-uml"),
            ],
            version: "0.9.2",
            image: "ghcr.io/maastrichtu-ids/sulo-schema-builder:0.9.2",
            instances: vec![("sulo.ids.unimaas.nl", "https://sulo.ids.unimaas.nl", Some("sulo-schema-builder-ids3"))],
        },
        SeedSoftware {
            name: "rdf_tx",
            tagline: "Transactional, hash-chained RDF patching",
            description: "Applies SPARQL updates transactionally, keeps a hash-chained patch log, and produces privacy-masked replicas.",
            repo: "https://github.com/MaastrichtU-IDS/rdf_tx",
            kind: "library",
            topics: vec![topic(T_SEMANTIC_WEB), topic(T_SOFTWARE)],
            keywords: vec!["rdf", "provenance", "masking", "rust"],
            consumes: vec![local_type(base, "sparql-update"), local_type(base, "rdf-quads")],
            produces: vec![local_type(base, "patch-log"), local_type(base, "masked-replica")],
            version: "0.4.0",
            image: "ghcr.io/maastrichtu-ids/rdf_tx:0.4.0",
            instances: vec![("rdf-tx-idsg2", "", Some("rdf-tx-idsg2"))],
        },
        SeedSoftware {
            name: "obda-lazy-cache-demo",
            tagline: "Lazy-caching OBDA over relational sources",
            description: "Demonstrates ontology-based data access with a lazy materialisation cache over relational sources and R2RML mappings.",
            repo: "https://github.com/MaastrichtU-IDS/obda-lazy-cache-demo",
            kind: "cli",
            topics: vec![topic(T_DATABASES), topic(T_SEMANTIC_WEB)],
            keywords: vec!["obda", "r2rml", "virtualisation"],
            consumes: vec![local_type(base, "relational-source"), local_type(base, "r2rml-mapping")],
            produces: vec![local_type(base, "materialised-view"), local_type(base, "mapping-coverage-report")],
            version: "0.2.0",
            image: "ghcr.io/maastrichtu-ids/obda-lazy-cache-demo:0.2.0",
            instances: vec![("obda-demo-ids3", "https://obda-demo.ids.unimaas.nl", None)],
        },
    ]
}

/// Register the IDS example estate. Returns a short report of what was created.
pub async fn seed_ids_examples(state: &Arc<AppState>, with_runs: bool) -> Result<serde_json::Value> {
    let base = state.config.base_iri.clone();
    let actor = "urn:tar:seed";
    let mut tx = GraphTx::new();
    tx.extend(type_quads(&base));
    state.store.apply(tx)?;

    let publisher = AgentIn {
        iri: None,
        name: Some("Maastricht University — Institute of Data Science".into()),
        kind: Some("organization".into()),
        identifier: Some("https://ror.org/02jz4aj89".into()),
        email: None,
        homepage: Some("https://www.maastrichtuniversity.nl/ids".into()),
        version: None,
    };

    let mut created_software = Vec::new();
    let mut created_instances = Vec::new();

    for s in ids_examples(&base) {
        let input = SoftwareIn {
            name: s.name.into(),
            tagline: Some(s.tagline.into()),
            description: Some(s.description.into()),
            homepage: Some(s.repo.into()),
            code_repository: Some(s.repo.into()),
            documentation: Some(format!("{}#readme", s.repo)),
            image: None,
            screenshots: Vec::new(),
            api_docs: Vec::new(),
            registration_clients: Vec::new(),
            registration_issuer: None,
            readme: None,
            readme_base_url: None,
            deployable: None,
            sync: None,
            download_url: Some(format!("{}/releases", s.repo)),
            license: Some("https://spdx.org/licenses/Apache-2.0".into()),
            kinds: vec![s.kind.into()],
            kind: None,
            maturity: Some("active".into()),
            topics: s.topics.clone(),
            keywords: s.keywords.iter().map(|k| k.to_string()).collect(),
            publisher: Some(publisher.clone()),
            contact: Some(AgentIn {
                name: Some("Ensar Emir Erol".into()),
                kind: Some("person".into()),
                ..Default::default()
            }),
            publications: vec![],
            capability: Some(CapabilityIn { produces: s.produces.clone(), consumes: s.consumes.clone() }),
        };
        let sw_iri = ids::mint(&base, Kind::Software);
        let mut tx = GraphTx::new();
        tx.extend(swdom::software_quads(&base, &sw_iri, &input, actor, None));

        let rel_iri = ids::mint(&base, Kind::Release);
        let release = ReleaseIn {
            version: s.version.into(),
            date_published: Some((chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339()),
            container_image: Some(s.image.into()),
            image_digest: None,
            changelog: Some(format!("{}/releases", s.repo)),
            install_command: Some(format!("docker pull {}", s.image)),
            downloads: Vec::new(),
            capability: None,
        };
        tx.extend(swdom::release_quads(&base, &rel_iri, &sw_iri, &release, actor));
        state.store.apply(tx)?;
        created_software.push(sw_iri.clone());

        for (label, endpoint, client_id) in &s.instances {
            let input = InstanceIn {
                label: (*label).into(),
                self_registered_by: None,
                self_registered_issuer: None,
                instance_key: None,
                software: Some(sw_iri.clone()),
                release: Some(rel_iri.clone()),
                endpoint_url: (!endpoint.is_empty()).then(|| (*endpoint).to_string()),
                endpoint_description: (!endpoint.is_empty()).then(|| format!("{endpoint}/openapi.json")),
                operator: Some(publisher.clone()),
                availability: Some("restricted".into()),
                jurisdiction: Some("NL".into()),
                description: None,
                // Workload identity: this is what a Keycloak client id looks like on a record.
                oidc_client_id: client_id.map(|c| c.to_string()),
                oidc_issuer: None,
                allowed_scopes: vec!["advertise:produce".into(), "advertise:consume".into()],
                health_endpoint: None,
                capability: None,
            };
            let inst_iri = ids::mint(&base, Kind::Instance);
            let mut tx = GraphTx::new();
            tx.extend(instdom::instance_quads(&base, &inst_iri, &input, actor, Some(&sw_iri)));
            state.store.apply(tx)?;
            created_instances.push(inst_iri);
        }
    }

    let mut runs = 0;
    let mut artifacts = 0;
    if with_runs {
        let (r, a) = seed_runs(state, &created_instances, &base, actor)?;
        runs = r;
        artifacts = a;
    }

    Ok(serde_json::json!({
        "software": created_software.len(),
        "instances": created_instances.len(),
        "runs": runs,
        "artifacts": artifacts,
        "types": local_types(&base).len(),
    }))
}

/// A handful of runs and artifacts so lineage, signals and the artifact pages have something
/// to show — including one cross-registry input, which is the point of federation.
fn seed_runs(state: &Arc<AppState>, instances: &[String], base: &str, actor: &str) -> Result<(usize, usize)> {
    let mut runs = 0;
    let mut artifacts = 0;
    let foreign_input = "https://reg.mumc.nl/artifact/01J7ZQK8W0RXAB3M2C7YQ1V4TD";

    for (n, inst) in instances.iter().take(4).enumerate() {
        for i in 0..3 {
            let started = chrono::Utc::now() - chrono::Duration::days((i * 4 + n) as i64) - chrono::Duration::hours(2);
            let ended = started + chrono::Duration::seconds(38 + (i as i64) * 11);
            let run_iri = ids::mint(base, Kind::Run);
            let status = if n == 2 && i == 1 { "failed" } else { "success" };
            let run = RunIn {
                external_key: Some(format!("gh-actions/{}{}/attempt-1", 12345 + n * 7, i)),
                label: Some(format!("validate-batch-{}", i + 1)),
                started_at: Some(started.to_rfc3339()),
                ended_at: Some(ended.to_rfc3339()),
                status: Some(status.into()),
                release: None,
            };
            let mut tx = GraphTx::new();
            tx.extend(crate::domain::run::run_quads(&run_iri, &run, inst, actor));

            // One consumed artifact, cross-registry on the first run of each instance.
            let input_iri = if i == 0 {
                let mut n2 = crate::rdf::Node::local(&run_iri);
                n2.link(ns::PROV, "used", foreign_input);
                tx.extend(n2.finish());
                foreign_input.to_string()
            } else {
                let a = ids::mint(base, Kind::Artifact);
                let input = ArtifactIn {
                    title: Some(format!("input graph {}-{}", n + 1, i + 1)),
                    conforms_to: Some(local_type(base, "rdf-graph")),
                    license: Some("https://spdx.org/licenses/CC-BY-4.0".into()),
                    keywords: vec!["rdf".into(), "input".into()],
                    issued: Some(started.to_rfc3339()),
                    distributions: vec![DistributionIn {
                        access_url: Some(format!("https://data.ids.unimaas.nl/graphs/{}-{}", n + 1, i + 1)),
                        media_type: Some("text/turtle".into()),
                        access_protocol: Some("https".into()),
                        auth_method: Some("apikey".into()),
                        availability: Some("restricted".into()),
                        access_request_url: Some("https://ids.unimaas.nl/data-access".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                };
                tx.extend(crate::domain::artifact::artifact_quads(base, &a, &input, actor, None));
                let mut n2 = crate::rdf::Node::local(&run_iri);
                n2.link(ns::PROV, "used", &a);
                tx.extend(n2.finish());
                artifacts += 1;
                a
            };

            if status == "success" {
                let out_iri = ids::mint(base, Kind::Artifact);
                // Every third output is metadata-only — the common case for health data
                // (spec §6.2), and the case the UI must render without a download button.
                let metadata_only = i == 2;
                let out = ArtifactIn {
                    title: Some(format!("Validation report — batch {} at deployment {}", i + 1, n + 1)),
                    description: Some("SHACL validation report produced by a scheduled run.".into()),
                    conforms_to: Some(format!("{base}/type/shacl-validation-report")),
                    license: Some("https://spdx.org/licenses/CC-BY-4.0".into()),
                    keywords: vec!["shacl".into(), "validation".into()],
                    issued: Some(ended.to_rfc3339()),
                    was_derived_from: vec![input_iri.clone()],
                    distributions: if metadata_only {
                        vec![DistributionIn {
                            availability: Some("metadata-only".into()),
                            access_request_url: Some("https://ids.unimaas.nl/data-access".into()),
                            media_type: Some("text/turtle".into()),
                            ..Default::default()
                        }]
                    } else {
                        vec![DistributionIn {
                            access_url: Some(format!("https://shacl.ids.unimaas.nl/reports/{}{}", n, i)),
                            download_url: Some(format!("https://shacl.ids.unimaas.nl/reports/{}{}.ttl", n, i)),
                            media_type: Some("text/turtle".into()),
                            byte_size: Some(2_118_342 + (i as i64) * 1024),
                            // A real digest of a made-up thing, rather than a made-up digest.
                            // The seed invents these files, so no digest here is the digest of
                            // anything you can fetch — but it has to be a well-formed sha-256 or
                            // the registry declines to derive a content identifier from it and
                            // the seeded catalogue cannot demonstrate the one feature that
                            // needs several registries to show. Hashing the download URL also
                            // makes two seeded registries agree, which is the point being made.
                            checksum: Some(ChecksumIn {
                                algorithm: "sha256".into(),
                                value: {
                                    use sha2::{Digest, Sha256};
                                    hex::encode(Sha256::digest(format!("https://shacl.ids.unimaas.nl/reports/{n}{i}.ttl").as_bytes()))
                                },
                            }),
                            access_protocol: Some("https".into()),
                            auth_method: Some("apikey".into()),
                            availability: Some("restricted".into()),
                            access_request_url: Some("https://ids.unimaas.nl/data-access".into()),
                            ..Default::default()
                        }]
                    },
                    ..Default::default()
                };
                tx.extend(crate::domain::artifact::artifact_quads(base, &out_iri, &out, actor, Some(&run_iri)));
                artifacts += 1;
            }
            state.store.apply(tx)?;
            runs += 1;
        }
    }
    Ok((runs, artifacts))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard the migration uses to tell an adopted term from a record that happens to have
    /// been typed as a concept. It reads the path, so it holds for a store brought up under a
    /// different base than the one that wrote it — which is exactly the case that caught it out.
    #[test]
    fn a_record_of_another_kind_is_never_taken_for_a_type() {
        for record in [
            "http://127.0.0.1:8099/artifact-series/01a054c0-b751-7032-81a1-535cb3bc8653",
            "https://reg.example/software/01a05400-0000-7000-8000-000000000001",
            "https://reg.example/artifact/01a05400-0000-7000-8000-000000000001",
        ] {
            assert!(is_a_record_that_is_not_a_type(record), "{record}");
        }
        for term in [
            "http://127.0.0.1:8099/type/shacl-validation-report",
            "https://reg.example/type/01a05400-0000-7000-8000-000000000001",
            // Adopted under an identifier that was never this registry's to shape.
            "http://purl.obolibrary.org/obo/SWO_0000001",
            "http://edamontology.org/data_2048",
        ] {
            assert!(!is_a_record_that_is_not_a_type(term), "{term}");
        }
    }
}
