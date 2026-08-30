//! Bootstrap content (spec §10.7) and the vocabulary preload (§5.4 `<urn:tar:vocab>`).
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

/// EDAM and local terms preloaded so that chips render labels without a network call.
/// The full EDAM ontology is not vendored: this is the working subset for IDS artifacts,
/// and any other IRI still works because `ArtifactType` is any IRI (D11).
pub const VOCAB_TTL: &str = include_str!("../shapes/vocab.ttl");
pub const SHAPES_TTL: &str = include_str!("../shapes/tar-shapes.ttl");

pub fn load_vocab(state: &AppState) -> Result<usize> {
    let a = state.store.load_turtle(VOCAB_TTL, ns::G_VOCAB, Some(&state.config.base_iri))?;
    let b = state.store.load_turtle(SHAPES_TTL, ns::G_SHAPES, Some(&state.config.base_iri))?;
    Ok(a + b)
}

const EDAM: &str = "http://edamontology.org/";

fn edam(id: &str) -> String {
    format!("{EDAM}{id}")
}

fn local_type(base: &str, slug: &str) -> String {
    format!("{base}/type/{slug}")
}

/// Local ArtifactTypes for the things EDAM does not name — SHACL shape graphs, OBDA mappings,
/// hash-chained patch logs (D11).
fn local_types(base: &str) -> Vec<(String, &'static str, &'static str, &'static str)> {
    vec![
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

fn type_quads(base: &str) -> Vec<oxigraph::model::Quad> {
    let mut out = Vec::new();
    for (iri, label, definition, media) in local_types(base) {
        let mut n = crate::rdf::Node::iri(&iri, ns::G_VOCAB);
        n.a(&format!("{}Concept", ns::SKOS));
        n.text(ns::SKOS, "prefLabel", label);
        n.text(ns::SKOS, "definition", definition);
        n.text(ns::TAR, "defaultMediaType", media);
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
            topics: vec![edam("topic_3071"), edam("topic_0089")],
            keywords: vec!["shacl", "validation", "rdf", "data quality"],
            consumes: vec![edam("data_2600"), local_type(base, "shacl-shapes-graph")],
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
            topics: vec![edam("topic_3071")],
            keywords: vec!["ontology", "sulo", "owl", "schema"],
            consumes: vec![local_type(base, "schema-model"), local_type(base, "sulo-ontology")],
            produces: vec![
                edam("data_2600"),
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
            topics: vec![edam("topic_3071"), edam("topic_0089")],
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
            topics: vec![edam("topic_0089")],
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
            license: Some("https://spdx.org/licenses/Apache-2.0".into()),
            kind: Some(s.kind.into()),
            maturity: Some("active".into()),
            edam_topics: s.topics.clone(),
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
            capability: None,
        };
        tx.extend(swdom::release_quads(&base, &rel_iri, &sw_iri, &release, actor));
        state.store.apply(tx)?;
        created_software.push(sw_iri.clone());

        for (label, endpoint, client_id) in &s.instances {
            let input = InstanceIn {
                label: (*label).into(),
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
                    conforms_to: Some(edam("data_2600")),
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
                            checksum: Some(ChecksumIn { algorithm: "sha256".into(), value: format!("9f2a{n}{i}c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2") }),
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
