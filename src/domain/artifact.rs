//! Artifact and Distribution projections (spec §4.2, §6.1).
//!
//! Artifacts are immutable once advertised (D10): a correction mints a new IRI linked by
//! `prov:wasRevisionOf`, and both sit under one `dct:isVersionOf` series concept.

use super::{agent_quads, Ctx};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::model::*;
use crate::ns;
use crate::rdf::{Node, Props};
use oxigraph::model::Quad;

pub const TYPE_DATASET: &str = "http://www.w3.org/ns/dcat#Dataset";
pub const TYPE_ENTITY: &str = "http://www.w3.org/ns/prov#Entity";
pub const TYPE_DISTRIBUTION: &str = "http://www.w3.org/ns/dcat#Distribution";

pub const AVAILABILITIES: [&str; 4] = ["public", "restricted", "embargoed", "metadata-only"];
pub const PROTOCOLS: [&str; 6] = ["https", "s3", "sparql", "oci", "ipfs", "file"];
pub const AUTH_METHODS: [&str; 5] = ["none", "apikey", "oauth2", "basic", "signed-url"];

pub fn artifact_quads(base: &str, iri: &str, input: &ArtifactIn, actor: &str, run_iri: Option<&str>) -> Vec<Quad> {
    let mut quads = Vec::new();
    let mut n = Node::local(iri);
    n.a(TYPE_DATASET);
    n.a(TYPE_ENTITY);
    n.opt_text(ns::DCT, "title", &input.title);
    n.opt_text(ns::DCT, "description", &input.description);
    n.opt_link(ns::DCT, "conformsTo", &input.conforms_to);
    n.opt_link(ns::DCT, "license", &input.license);
    n.texts(ns::DCAT, "keyword", &input.keywords);
    n.datetime(ns::DCT, "issued", input.issued.as_deref().unwrap_or(&chrono::Utc::now().to_rfc3339()));
    n.links(ns::PROV, "wasDerivedFrom", &input.was_derived_from);
    n.opt_link(ns::PROV, "wasRevisionOf", &input.was_revision_of);
    n.opt_text(ns::TAR, "externalKey", &input.external_key);
    if let Some(r) = run_iri {
        n.link(ns::PROV, "wasGeneratedBy", r);
    }
    // Version-series concept IRI (D10): mint one per artifact unless the caller places this
    // artifact in an existing series.
    let series = input.is_version_of.clone().unwrap_or_else(|| ids::mint(base, Kind::ArtifactSeries));
    n.link(ns::DCT, "isVersionOf", &series);
    let mut s = Node::local(&series);
    s.a(&format!("{}Concept", ns::SKOS));
    s.opt_text(ns::SKOS, "prefLabel", &input.title);
    quads.extend(s.finish());
    crate::rdf::attribution(&mut n, actor);

    if let Some(p) = &input.publisher {
        let (piri, pq) = agent_quads(base, p);
        if let Some(pi) = piri {
            n.link(ns::DCT, "publisher", &pi);
        }
        quads.extend(pq);
    }
    for d in &input.distributions {
        let d_iri = ids::mint(base, Kind::Distribution);
        quads.extend(distribution_quads(&d_iri, d));
        n.link(ns::DCAT, "distribution", &d_iri);
    }
    quads.extend(n.finish());
    quads
}

pub fn distribution_quads(iri: &str, d: &DistributionIn) -> Vec<Quad> {
    let mut n = Node::local(iri);
    n.a(TYPE_DISTRIBUTION);
    n.opt_text(ns::DCT, "title", &d.title);
    n.opt_link(ns::DCAT, "accessURL", &d.access_url);
    // metadata-only carries no downloadURL at all (spec §6.2) — the absence *is* the model.
    let availability = d.availability.clone().unwrap_or_else(|| "public".into());
    if availability != "metadata-only" {
        n.opt_link(ns::DCAT, "downloadURL", &d.download_url);
    }
    n.opt_text(ns::DCAT, "mediaType", &d.media_type);
    n.opt_text(ns::DCT, "format", &d.media_type);
    n.opt_int(ns::DCAT, "byteSize", &d.byte_size);
    n.opt_link(ns::DCT, "conformsTo", &d.conforms_to);
    n.opt_link(ns::DCT, "license", &d.license);
    n.opt_link(ns::DCAT, "accessService", &d.access_service);
    n.opt_text(ns::TAR, "accessProtocol", &d.access_protocol);
    n.opt_text(ns::TAR, "authMethod", &d.auth_method);
    n.text(ns::TAR, "availability", &availability);
    n.opt_link(ns::TAR, "accessRequestURL", &d.access_request_url);
    if let Some(c) = &d.checksum {
        let mut cn = Node::blank(ns::G_LOCAL);
        cn.a(&format!("{}Checksum", ns::SPDX));
        cn.link(ns::SPDX, "algorithm", &format!("{}checksumAlgorithm_{}", ns::SPDX, c.algorithm));
        cn.text(ns::SPDX, "checksumValue", &c.value);
        n.child(ns::SPDX, "checksum", cn);
    }
    n.finish()
}

pub fn distribution_from(p: &Props, key: &str) -> Option<Distribution> {
    let d = p.nested_for(key)?;
    let checksum = d.node_keys(ns::SPDX, "checksum").first().and_then(|k| {
        let c = p.nested_for(k)?;
        Some(ChecksumIn {
            algorithm: c
                .iri(ns::SPDX, "algorithm")
                .map(|a| ids::iri_tail(&a).trim_start_matches("checksumAlgorithm_").to_string())
                .unwrap_or_else(|| "sha256".into()),
            value: c.str(ns::SPDX, "checksumValue").unwrap_or_default(),
        })
    });
    Some(Distribution {
        iri: key.to_string(),
        title: d.str(ns::DCT, "title"),
        access_url: d.iri(ns::DCAT, "accessURL"),
        download_url: d.iri(ns::DCAT, "downloadURL"),
        media_type: d.str(ns::DCAT, "mediaType"),
        byte_size: d.i64(ns::DCAT, "byteSize"),
        checksum,
        conforms_to: d.iri(ns::DCT, "conformsTo"),
        license: d.iri(ns::DCT, "license"),
        access_service: d.iri(ns::DCAT, "accessService"),
        access_protocol: d.str(ns::TAR, "accessProtocol"),
        auth_method: d.str(ns::TAR, "authMethod"),
        availability: d.str(ns::TAR, "availability").unwrap_or_else(|| "public".into()),
        access_request_url: d.iri(ns::TAR, "accessRequestURL"),
    })
}

/// The artifact-level availability shown as a badge: the most open of its distributions.
/// An artifact with no distribution is `metadata-only` — findable, described, not retrievable.
pub fn overall_availability(dists: &[Distribution]) -> String {
    let rank = |a: &str| match a {
        "public" => 0,
        "restricted" => 1,
        "embargoed" => 2,
        _ => 3,
    };
    dists
        .iter()
        .map(|d| d.availability.clone())
        .min_by_key(|a| rank(a))
        .unwrap_or_else(|| "metadata-only".into())
}

pub fn artifact_from_props(ctx: &Ctx, iri: &str, p: &Props) -> Artifact {
    let distributions: Vec<Distribution> = p
        .node_keys(ns::DCAT, "distribution")
        .iter()
        .filter_map(|k| distribution_from(p, k))
        .collect();
    Artifact {
        iri: iri.to_string(),
        id: ids::local_id(ctx.base(), iri).map(|(_, i)| i).unwrap_or_else(|| ids::iri_tail(iri).to_string()),
        title: p.str(ns::DCT, "title"),
        description: p.str(ns::DCT, "description"),
        conforms_to: p.iri(ns::DCT, "conformsTo").map(|t| ctx.type_ref(&t)),
        license: p.iri(ns::DCT, "license"),
        keywords: p.strs(ns::DCAT, "keyword"),
        issued: p.str(ns::DCT, "issued"),
        publisher: ctx.opt_agent_ref(p.iri(ns::DCT, "publisher")),
        availability: overall_availability(&distributions),
        distributions,
        was_derived_from: p.iris(ns::PROV, "wasDerivedFrom"),
        was_revision_of: p.iri(ns::PROV, "wasRevisionOf"),
        is_version_of: p.iri(ns::DCT, "isVersionOf"),
        was_generated_by: p.iri(ns::PROV, "wasGeneratedBy"),
        generated_by_run: None,
        external_key: p.str(ns::TAR, "externalKey"),
        origin: ctx.origin(p.graph.as_deref()),
        tombstoned: p.bool(ns::TAR, "tombstoned").unwrap_or(false),
    }
}

pub fn load_artifact(ctx: &Ctx, iri: &str) -> AppResult<Artifact> {
    let quads = ctx.state.store.describe(iri).map_err(AppError::from)?;
    if quads.is_empty() {
        return Err(AppError::not_found(format!("no artifact at {iri}")));
    }
    let p = Props::from_quads(iri, &quads);
    let mut a = artifact_from_props(ctx, iri, &p);
    if let Some(run) = a.was_generated_by.clone() {
        a.generated_by_run = super::run::load_run_summary(ctx, &run).ok();
    }
    Ok(a)
}

/// A light reference for run pages and lineage lists, including foreign IRIs we have not
/// resolved yet (spec §9.3 — a cross-registry edge is an ordinary triple).
pub fn artifact_ref(ctx: &Ctx, iri: &str) -> ArtifactRef {
    let quads = ctx.state.store.describe(iri).unwrap_or_default();
    if quads.is_empty() {
        return ArtifactRef {
            iri: iri.to_string(),
            title: None,
            conforms_to: None,
            availability: None,
            origin: ctx.origin_of_iri(iri),
            unresolved: true,
        };
    }
    let p = Props::from_quads(iri, &quads);
    let dists: Vec<Distribution> = p.node_keys(ns::DCAT, "distribution").iter().filter_map(|k| distribution_from(&p, k)).collect();
    ArtifactRef {
        iri: iri.to_string(),
        title: p.str(ns::DCT, "title").or_else(|| p.str(ns::SKOS, "prefLabel")),
        conforms_to: p.iri(ns::DCT, "conformsTo").map(|t| ctx.type_ref(&t)),
        availability: Some(overall_availability(&dists)),
        origin: ctx.origin(p.graph.as_deref()),
        unresolved: false,
    }
}

/// Depth-limited lineage in both directions (spec §7.5). v1 renders this as tables; the graph
/// visualisation deferred to v2 consumes the same payload.
pub fn lineage(ctx: &Ctx, root: &str, depth: i32, direction: &str) -> AppResult<Lineage> {
    let mut nodes: Vec<LineageNode> = Vec::new();
    let mut edges: Vec<LineageEdge> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut frontier = vec![(root.to_string(), 0i32)];
    let mut truncated = false;
    let up = direction == "up" || direction == "both";
    let down = direction == "down" || direction == "both";

    while let Some((iri, d)) = frontier.pop() {
        if !seen.insert(iri.clone()) {
            continue;
        }
        let quads = ctx.state.store.describe(&iri).map_err(AppError::from)?;
        let p = Props::from_quads(&iri, &quads);
        let entity_type = if p.has_type(TYPE_DATASET) {
            "artifact"
        } else if p.has_type("http://www.w3.org/ns/prov#Activity") {
            "run"
        } else {
            "unknown"
        };
        nodes.push(LineageNode {
            iri: iri.clone(),
            entity_type: entity_type.to_string(),
            title: p.str(ns::DCT, "title").or_else(|| p.str(ns::RDFS, "label")),
            origin: if quads.is_empty() { ctx.origin_of_iri(&iri) } else { ctx.origin(p.graph.as_deref()) },
            depth: d,
            unresolved: quads.is_empty(),
        });
        if d >= depth {
            if !quads.is_empty() {
                truncated = true;
            }
            continue;
        }
        if up {
            for parent in p.iris(ns::PROV, "wasDerivedFrom") {
                edges.push(LineageEdge { from: iri.clone(), to: parent.clone(), predicate: "derivedFrom".into() });
                frontier.push((parent, d + 1));
            }
            if let Some(rev) = p.iri(ns::PROV, "wasRevisionOf") {
                edges.push(LineageEdge { from: iri.clone(), to: rev.clone(), predicate: "revisionOf".into() });
                frontier.push((rev, d + 1));
            }
            if let Some(run) = p.iri(ns::PROV, "wasGeneratedBy") {
                edges.push(LineageEdge { from: run.clone(), to: iri.clone(), predicate: "generated".into() });
                frontier.push((run, d + 1));
            }
            for used in p.iris(ns::PROV, "used") {
                edges.push(LineageEdge { from: iri.clone(), to: used.clone(), predicate: "used".into() });
                frontier.push((used, d + 1));
            }
        }
        if down {
            let q = format!(
                r#"{p}
SELECT ?child ?pred WHERE {{
  GRAPH ?g {{
    {{ ?child prov:wasDerivedFrom <{iri}> BIND("derivedFrom" AS ?pred) }}
    UNION {{ ?child prov:used <{iri}> BIND("used" AS ?pred) }}
    UNION {{ ?child prov:wasGeneratedBy <{iri}> BIND("generated" AS ?pred) }}
    UNION {{ ?child prov:wasRevisionOf <{iri}> BIND("revisionOf" AS ?pred) }}
  }}
}}"#,
                p = ns::PREFIXES
            );
            for row in ctx.state.store.select(&q).map_err(AppError::from)?.rows {
                let (Some(child), Some(pred)) = (row.iri("child"), row.str("pred")) else { continue };
                edges.push(LineageEdge { from: child.clone(), to: iri.clone(), predicate: pred });
                frontier.push((child, d + 1));
            }
        }
    }
    Ok(Lineage { root: root.to_string(), nodes, edges, truncated })
}
