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
/// The version-series concept an artifact belongs to (D10).
pub const TYPE_ARTIFACT_SERIES: &str = "https://w3id.org/tar/ns#ArtifactSeries";

pub const AVAILABILITIES: [&str; 4] = ["public", "restricted", "embargoed", "metadata-only"];
/// `http` is here deliberately. An intranet service or a local deployment really does serve
/// over plain HTTP, and a vocabulary that cannot name it forces the record to omit the field —
/// which loses exactly the fact worth knowing, that the transport is unencrypted.
pub const PROTOCOLS: [&str; 7] = ["https", "http", "s3", "sparql", "oci", "ipfs", "file"];
pub const AUTH_METHODS: [&str; 5] = ["none", "apikey", "oauth2", "basic", "signed-url"];

/// The EU access-right authority concept for a `tar:availability` value (audit 2026-08-30).
///
/// Written as `dct:accessRights` beside `tar:availability` — DCAT 3 admits the property on
/// both `dcat:Resource` and `dcat:Distribution`, and DCAT-AP binds its values to this table.
/// The mapping is deliberately lossy: the table has no embargo concept and nothing that
/// distinguishes "described but not retrievable", so `embargoed` and `metadata-only` both
/// coarsen to NON_PUBLIC and the tar literal keeps the finer distinction.
pub fn access_right(availability: &str) -> Option<String> {
    let concept = match availability {
        "public" => "PUBLIC",
        "restricted" => "RESTRICTED",
        "embargoed" | "metadata-only" => "NON_PUBLIC",
        _ => return None,
    };
    Some(format!("{}{}", ns::EU_ACCESS_RIGHT, concept))
}

/// The roles a qualified attribution can carry. Roles are the part PROV expects a domain to
/// define; the relation and the node type are its own.
pub const ROLE_PRODUCING_SYSTEM: &str = "https://w3id.org/tar/ns#producingSystem";
pub const ROLE_PRODUCING_USER: &str = "https://w3id.org/tar/ns#producingUser";

/// The agent behind one role's qualified attribution.
fn attributed_agent(ctx: &Ctx, p: &Props, role: &str) -> Option<AgentRef> {
    for key in p.node_keys(ns::PROV, "qualifiedAttribution") {
        let Some(a) = p.nested_for(&key) else { continue };
        if a.iri(ns::PROV, "hadRole").as_deref() == Some(role) {
            return ctx.opt_agent_ref(a.iri(ns::PROV, "agent"));
        }
    }
    None
}

pub fn artifact_quads(base: &str, iri: &str, input: &ArtifactIn, actor: &str, run_iri: Option<&str>) -> Vec<Quad> {
    let mut quads = Vec::new();
    let mut n = Node::local(iri);
    n.a(TYPE_DATASET);
    n.a(TYPE_ENTITY);
    n.opt_text(ns::DCT, "title", &input.title);
    n.opt_text(ns::DCT, "description", &input.description);
    n.opt_link(ns::DCT, "conformsTo", &input.conforms_to);
    n.opt_link(ns::DCT, "license", &input.license);
    // Normalised here rather than in each caller, so every write path — the REST create, both
    // advertise endpoints, the OpenLineage adapter and the seed — gets the same keywords.
    // `dcat:keyword` keeps the literal a person reads; `dcat:theme` carries the concept a
    // filter and a subscription can match on exactly.
    let (keywords, themes) = super::keywords::normalise(base, &input.keywords);
    n.texts(ns::DCAT, "keyword", &keywords);
    n.links(ns::DCAT, "theme", &themes);
    n.datetime(ns::DCT, "issued", input.issued.as_deref().unwrap_or(&chrono::Utc::now().to_rfc3339()));
    n.links(ns::PROV, "wasDerivedFrom", &input.was_derived_from);
    n.opt_link(ns::PROV, "wasRevisionOf", &input.was_revision_of);
    // Audit 2026-08-30: dct:identifier replaces tar:externalKey for the producer's own key.
    n.opt_text(ns::DCT, "identifier", &input.external_key);
    // Who produced it, when the run chain cannot say.
    //
    // `prov:wasAttributedTo` would be the obvious predicate and is the wrong one to reuse: the
    // registry already writes it with the credential that presented the record, and reads it
    // back as a single value. Adding caller-supplied agents there would leave the one
    // attribution nobody can forge indistinguishable from the ones anybody can.
    //
    // PROV's answer to several attributions is `prov:qualifiedAttribution` — an Attribution node
    // carrying the agent and the role it played. So the trustworthy statement keeps the plain
    // predicate, the claims get qualified ones, and a reader can tell which is which by shape
    // rather than by convention.
    // Each agent is built exactly once and its IRI reused. `agent_quads` *mints* an IRI for an
    // agent that carries no identifier of its own, so calling it twice for the same input
    // produces two different nodes — which is how the delegation below first came to be written
    // onto an orphan agent that nothing referenced and no query could reach.
    let mut produced: Vec<(&str, String)> = Vec::new();
    for (agent, role) in [
        (&input.produced_by, ROLE_PRODUCING_SYSTEM),
        (&input.produced_by_user, ROLE_PRODUCING_USER),
    ] {
        let Some(a) = agent else { continue };
        let (Some(agent_iri), agent_body) = agent_quads(base, a) else { continue };
        quads.extend(agent_body);
        // The role's own local name, so the node reads `…artifact/01a…#producingSystem`. Split
        // on the fragment as well as the path: these role IRIs end in `…/ns#producingSystem`,
        // and splitting on `/` alone left the namespace segment glued to the front.
        let local = role.rsplit(['#', '/']).next().unwrap_or(role);
        let mut attribution = Node::local(&format!("{iri}#{local}"));
        attribution.a(&format!("{}Attribution", ns::PROV));
        attribution.link(ns::PROV, "agent", &agent_iri);
        attribution.link(ns::PROV, "hadRole", role);
        n.child(ns::PROV, "qualifiedAttribution", attribution);
        produced.push((role, agent_iri));
    }
    // A system acting for a person is a delegation, and PROV has the word for it. Written only
    // when both are given, because that is the only case in which the relation is asserted.
    let find = |role: &str| produced.iter().find(|(r, _)| *r == role).map(|(_, i)| i.clone());
    if let (Some(system), Some(user)) = (find(ROLE_PRODUCING_SYSTEM), find(ROLE_PRODUCING_USER)) {
        let mut d = Node::local(&system);
        d.link(ns::PROV, "actedOnBehalfOf", &user);
        quads.extend(d.finish());
    }
    if let Some(r) = run_iri {
        n.link(ns::PROV, "wasGeneratedBy", r);
    }
    // Version-series concept IRI (D10): mint one per artifact unless the caller places this
    // artifact in an existing series.
    let series = input.is_version_of.clone().unwrap_or_else(|| ids::mint(base, Kind::ArtifactSeries));
    n.link(ns::DCT, "isVersionOf", &series);
    let mut s = Node::local(&series);
    // Deliberately NOT skos:Concept. It is a concept in the Zenodo sense — the idea of "this
    // artifact, any version" — but typing it that way put every artifact's *title* into the
    // artifact-type picker, where it is not a type and must never be offered as one.
    s.a(TYPE_ARTIFACT_SERIES);
    s.opt_text(ns::SKOS, "prefLabel", &input.title);
    quads.extend(s.finish());
    crate::rdf::attribution_at(&mut n, actor, input.modified.as_deref());

    if let Some(p) = &input.publisher {
        let (piri, pq) = agent_quads(base, p);
        if let Some(pi) = piri {
            n.link(ns::DCT, "publisher", &pi);
        }
        quads.extend(pq);
    }
    for (agents, predicate) in [(&input.creators, "creator"), (&input.contributors, "contributor")] {
        for a in agents {
            let (iri, aq) = agent_quads(base, a);
            if let Some(i) = iri {
                n.link(ns::DCT, predicate, &i);
            }
            quads.extend(aq);
        }
    }
    if let Some(c) = &input.contact {
        let (iri, aq) = agent_quads(base, c);
        if let Some(i) = iri {
            // The same term the Software path uses. `tar:contact` said this until the
            // vocabulary audit found `codemeta:maintainer` already meant it, and the ontology
            // has marked it deprecated ever since while this line kept writing it — a
            // contradiction between what the registry says it does and what it does.
            n.link(ns::CODEMETA, "maintainer", &i);
        }
        quads.extend(aq);
    }
    n.opt_text(ns::DCAT, "version", &input.version);
    n.opt_link(ns::DCAT, "landingPage", &input.landing_page);
    n.opt_link(ns::FOAF, "page", &input.documentation);
    n.opt_link(ns::DCT, "source", &input.source);
    n.texts(ns::DCT, "language", &input.language);
    n.opt_text(ns::DCT, "spatial", &input.spatial);
    n.opt_datetime(ns::TAR, "temporalStart", &input.temporal_start);
    n.opt_datetime(ns::TAR, "temporalEnd", &input.temporal_end);
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
    // Supplement: the coarse standard reading of availability, for DCAT-AP harvesters.
    if let Some(ar) = access_right(&availability) {
        n.link(ns::DCT, "accessRights", &ar);
    }
    n.opt_link(ns::TAR, "accessRequestURL", &d.access_request_url);
    let mut extra = Vec::new();
    if let Some(c) = &d.checksum {
        let mut cn = Node::blank(ns::G_LOCAL);
        cn.a(&format!("{}Checksum", ns::SPDX));
        cn.link(ns::SPDX, "algorithm", &format!("{}checksumAlgorithm_{}", ns::SPDX, c.algorithm));
        cn.text(ns::SPDX, "checksumValue", &c.value);
        n.child(ns::SPDX, "checksum", cn);
        // The digest again, as a name instead of a literal. The checksum node already holds the
        // fact; what it does not give anybody is something to join on — matching two registries'
        // records through `spdx:checksum/spdx:checksumValue` means agreeing on the spelling of
        // the algorithm and the case of the hex first. See `super::content` for the form and
        // why `prov:specializationOf` carries it.
        if let Ok(id) = super::content::identify(&c.algorithm, &c.value) {
            n.link(ns::PROV, "specializationOf", &id.iri);
            // PROV defines specialization between two entities, so the bit-string is typed as
            // one. Without this the record asserts a PROV relation with nothing on the far end,
            // and a reasoner or a peer merging our Turtle has to guess what it was given.
            let mut cid = Node::local(&id.iri);
            cid.a(TYPE_ENTITY);
            extra.extend(cid.finish());
        }
    }
    let mut quads = n.finish();
    quads.extend(extra);
    quads
}

/// The content identifier on a distribution, if it has one.
///
/// Read back rather than recomputed, so a record loaded from a peer's graph reports the name that
/// peer actually wrote — which is the only way to notice if two implementations ever disagree.
pub fn content_identifier(d: &Props) -> Option<String> {
    d.iris(ns::PROV, "specializationOf").into_iter().find(|i| i.starts_with("ni:"))
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
        content_identifier: content_identifier(d),
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
        creators: p.iris(ns::DCT, "creator").iter().map(|i| ctx.agent_ref(i)).collect(),
        contributors: p.iris(ns::DCT, "contributor").iter().map(|i| ctx.agent_ref(i)).collect(),
        // Standard term first, with the retired one as a fallback so records written before
        // this change keep resolving. Same pattern as every other term the audit replaced.
        contact: ctx
            .opt_agent_ref(p.iri(ns::CODEMETA, "maintainer").or_else(|| p.iri(ns::TAR, "contact"))),
        modified: p.str(ns::DCT, "modified"),
        version: p.str(ns::DCAT, "version"),
        landing_page: p.iri(ns::DCAT, "landingPage"),
        documentation: p.iri(ns::FOAF, "page"),
        source: p.iri(ns::DCT, "source"),
        language: p.strs(ns::DCT, "language"),
        spatial: p.str(ns::DCT, "spatial"),
        temporal_start: p.str(ns::TAR, "temporalStart"),
        temporal_end: p.str(ns::TAR, "temporalEnd"),
        attributed_to: p.iri(ns::PROV, "wasAttributedTo"),
        produced_by: attributed_agent(ctx, p, ROLE_PRODUCING_SYSTEM),
        produced_by_user: attributed_agent(ctx, p, ROLE_PRODUCING_USER),
        availability: overall_availability(&distributions),
        // Projected from the distributions rather than stored on the artifact. The digest is a
        // fact about bytes and an artifact may have several sets of them (the same graph as
        // Turtle and as N-Triples), so an artifact-level triple would have to pick one
        // arbitrarily or claim the artifact is several things at once. This field is the list a
        // reader wants; the graph keeps the honest one-per-distribution relation.
        content_identifiers: distributions.iter().filter_map(|d| d.content_identifier.clone()).collect(),
        distributions,
        was_derived_from: p.iris(ns::PROV, "wasDerivedFrom"),
        was_revision_of: p.iri(ns::PROV, "wasRevisionOf"),
        is_version_of: p.iri(ns::DCT, "isVersionOf"),
        was_generated_by: p.iri(ns::PROV, "wasGeneratedBy"),
        generated_by_run: None,
        external_key: p.str(ns::DCT, "identifier").or_else(|| p.str(ns::TAR, "externalKey")),
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
