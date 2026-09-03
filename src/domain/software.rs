//! Software, Release and Capability projections (spec §4.2).

use super::{agent_quads, Ctx};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::model::*;
use crate::ns;
use crate::rdf::{Node, Props};
use crate::store::GraphTx;
use oxigraph::model::Quad;
use std::collections::HashMap;

pub const TYPE_SOFTWARE: &str = "https://schema.org/SoftwareApplication";
/// Registry-internal discriminators. A Release is *also* a `schema:SoftwareApplication`
/// (spec §4.2), so without these neither a count query nor a SHACL `sh:targetClass` can tell
/// the two apart, and the Software shape would fire on every Release.
pub const TYPE_TAR_SOFTWARE: &str = "https://w3id.org/tar/ns#Software";
pub const TYPE_TAR_RELEASE: &str = "https://w3id.org/tar/ns#Release";
pub const TYPE_SOURCE: &str = "https://schema.org/SoftwareSourceCode";
pub const TYPE_RELEASE_PLAN: &str = "http://www.w3.org/ns/prov#Plan";
/// The repostatus.org concepts CodeMeta points `developmentStatus` at.
/// <https://www.repostatus.org/>
pub fn repostatus_iri(value: &str) -> Option<String> {
    const CONCEPTS: [&str; 8] =
        ["concept", "wip", "suspended", "abandoned", "active", "inactive", "unsupported", "moved"];
    let v = value.trim().to_ascii_lowercase();
    CONCEPTS.contains(&v.as_str()).then(|| format!("https://www.repostatus.org/#{v}"))
}

pub const TYPE_CAPABILITY: &str = "https://w3id.org/tar/ns#Capability";

// ------------------------------------------------------------------- writes

pub fn software_quads(base: &str, iri: &str, input: &SoftwareIn, actor: &str, created: Option<String>) -> Vec<Quad> {
    let mut quads = Vec::new();
    let mut n = Node::local(iri);
    n.a(TYPE_SOFTWARE);
    n.a(TYPE_SOURCE);
    n.a(TYPE_TAR_SOFTWARE);
    n.text(ns::SCHEMA, "name", &input.name);
    // Audit 2026-08-30: the short one-liner is dct:abstract ("a summary of the resource"),
    // not an invented tar:tagline. schema:description keeps the long form.
    n.opt_text(ns::DCT, "abstract", &input.tagline);
    n.opt_text(ns::SCHEMA, "description", &input.description);
    n.opt_link(ns::SCHEMA, "url", &input.homepage);
    n.opt_link(ns::SCHEMA, "codeRepository", &input.code_repository);
    n.opt_link(ns::SCHEMA, "softwareHelp", &input.documentation);
    n.opt_link(ns::SCHEMA, "downloadUrl", &input.download_url);
    n.opt_link(ns::SCHEMA, "image", &input.image);
    n.links(ns::SCHEMA, "screenshot", &input.screenshots);
    n.opt_text(ns::TAR, "readme", &input.readme);
    n.opt_link(ns::TAR, "readmeBaseURL", &input.readme_base_url);
    // API descriptions as dcat:endpointDescription — DCAT's own definition is "a description of
    // the service endpoint, including its operations, parameters", with OpenAPI given as the
    // worked example. The document node carries dct:conformsTo naming which specification it
    // follows, so a consumer that has never heard of this registry still knows what it is
    // holding. Nothing here is OpenAPI-only: a SPARQL service description or an OLS4 route
    // listing is the same shape with a different conformsTo.
    for d in &input.api_docs {
        if d.url.trim().is_empty() {
            continue;
        }
        let format = d.normalised_format();
        let mut dn = Node::local(d.url.trim());
        dn.a(&format!("{}Standard", ns::DCT));
        dn.text(ns::TAR, "apiFormat", &format);
        if let Some(spec) = crate::model::api_format_iri(&format) {
            dn.link(ns::DCT, "conformsTo", spec);
        }
        dn.opt_text(ns::DCT, "title", &d.title);
        dn.opt_text(ns::DCT, "description", &d.description);
        n.child(ns::DCAT, "endpointDescription", dn);
    }
    n.opt_link(ns::DCT, "license", &input.license);
    // Audit 2026-08-30: schema:applicationCategory already carried the kind — the duplicate
    // tar:kind triple is gone. codemeta:developmentStatus is CodeMeta's term for exactly
    // this ("description of development status, e.g. active, inactive, suspended").
    n.texts(ns::SCHEMA, "applicationCategory", &input.resolved_kinds());
    // codemeta:developmentStatus is declared `"@type": "@id"` in CodeMeta's context: its range
    // is an IRI from repostatus.org, not a string. Emitting a literal there would be a term
    // used against its own definition, which is worse than not using it — a consumer would try
    // to dereference "active". So the literal stays on tar:maturity as the authoritative value,
    // and the standard term is added only when the value names a repostatus concept.
    n.opt_text(ns::TAR, "maturity", &input.maturity);
    // Only written when false: the default is "can be hosted", and a triple asserting the
    // default on every record is noise a federating peer has to read and discard.
    if input.deployable == Some(false) {
        n.boolean(ns::TAR, "deployable", false);
    }
    if let Some(iri) = input.maturity.as_deref().and_then(repostatus_iri) {
        n.link(ns::CODEMETA, "developmentStatus", &iri);
    }
    n.texts(ns::TAR, "registrationClient", &input.registration_clients);
    // Only meaningful alongside the client ids it scopes; a lone issuer names nobody.
    if !input.registration_clients.is_empty() {
        n.opt_text(ns::TAR, "registrationIssuer", &input.registration_issuer);
    }
    n.links(ns::DCT, "subject", &input.topics);
    n.texts(ns::SCHEMA, "keywords", &input.keywords);
    n.links(ns::DCT, "references", &input.publications);
    n.datetime(ns::DCT, "created", &created.unwrap_or_else(|| chrono::Utc::now().to_rfc3339()));
    crate::rdf::attribution(&mut n, actor);

    if let Some(p) = &input.publisher {
        let (piri, pq) = agent_quads(base, p);
        if let Some(pi) = piri {
            n.link(ns::DCT, "publisher", &pi);
        }
        quads.extend(pq);
    }
    if let Some(c) = &input.contact {
        let (ciri, cq) = agent_quads(base, c);
        if let Some(ci) = ciri {
            // Audit 2026-08-30: codemeta:maintainer — "individual responsible for maintaining
            // the software (usually includes an email contact address)" — is this exact role.
            n.link(ns::CODEMETA, "maintainer", &ci);
        }
        quads.extend(cq);
    }
    if let Some(sync) = &input.sync {
        let mut sn = Node::local(&format!("{iri}#sync"));
        sn.a(&format!("{}RepositorySync", ns::TAR));
        sn.text(ns::TAR, "syncSource", &sync.source);
        sn.text(ns::TAR, "syncRepo", &sync.repo);
        sn.texts(ns::TAR, "syncField", &sync.fields);
        sn.boolean(ns::TAR, "syncEnabled", sync.enabled);
        n.child(ns::TAR, "sync", sn);
    }
    if let Some(cap) = &input.capability {
        let cap_iri = ids::mint(base, Kind::Capability);
        quads.extend(capability_quads(&cap_iri, cap));
        n.link(ns::TAR, "hasCapability", &cap_iri);
    }
    quads.extend(n.finish());
    quads
}

pub fn capability_quads(iri: &str, cap: &CapabilityIn) -> Vec<Quad> {
    let mut n = Node::local(iri);
    n.a(TYPE_CAPABILITY);
    n.a(TYPE_RELEASE_PLAN);
    n.links(ns::TAR, "produces", &cap.produces);
    n.links(ns::TAR, "consumes", &cap.consumes);
    n.finish()
}

pub fn release_quads(base: &str, iri: &str, software_iri: &str, input: &ReleaseIn, actor: &str) -> Vec<Quad> {
    let mut quads = Vec::new();
    let mut n = Node::local(iri);
    n.a(TYPE_SOFTWARE);
    n.a(TYPE_RELEASE_PLAN);
    n.a(TYPE_TAR_RELEASE);
    n.text(ns::SCHEMA, "softwareVersion", &input.version);
    n.opt_datetime(ns::SCHEMA, "datePublished", &input.date_published);
    n.opt_text(ns::TAR, "containerImage", &input.container_image);
    n.opt_text(ns::TAR, "imageDigest", &input.image_digest);
    n.opt_link(ns::SCHEMA, "releaseNotes", &input.changelog);
    n.opt_text(ns::TAR, "installCommand", &input.install_command);
    // Release assets are dcat:Distributions: a distribution is "a way of obtaining a thing",
    // which is exactly what a per-platform installer is. Reusing the term also means
    // GraphStore::describe already returns them with their release, and DCAT-aware readers
    // understand them without knowing anything about tar:.
    for d in &input.downloads {
        let mut a = Node::local(&ids::mint(base, Kind::Distribution));
        a.a(super::artifact::TYPE_DISTRIBUTION);
        a.link(ns::DCAT, "downloadURL", &d.url);
        a.opt_text(ns::DCT, "title", &d.label);
        a.opt_text(ns::SCHEMA, "operatingSystem", &d.platform);
        a.opt_int(ns::DCAT, "byteSize", &d.byte_size);
        let availability = d.availability.clone().unwrap_or_else(|| "public".into());
        a.text(ns::TAR, "availability", &availability);
        if let Some(rights) = super::artifact::access_right(&availability) {
            a.link(ns::DCT, "accessRights", &rights);
        }
        n.child(ns::DCAT, "distribution", a);
    }
    n.link(ns::DCT, "isVersionOf", software_iri);
    crate::rdf::attribution(&mut n, actor);
    if let Some(cap) = &input.capability {
        let cap_iri = ids::mint(base, Kind::Capability);
        quads.extend(capability_quads(&cap_iri, cap));
        n.link(ns::TAR, "hasCapability", &cap_iri);
    }
    quads.extend(n.finish());
    quads
}

// -------------------------------------------------------------------- reads

pub fn capability_from(ctx: &Ctx, cap_iri: &str, declared_at: &str) -> Option<Capability> {
    let quads = ctx.state.store.describe(cap_iri).ok()?;
    if quads.is_empty() {
        return None;
    }
    let p = Props::from_quads(cap_iri, &quads);
    Some(Capability {
        iri: cap_iri.to_string(),
        produces: ctx.type_refs(&p.iris(ns::TAR, "produces")),
        consumes: ctx.type_refs(&p.iris(ns::TAR, "consumes")),
        declared_at: declared_at.to_string(),
    })
}

pub fn software_from_props(ctx: &Ctx, iri: &str, p: &Props) -> Software {
    let id = ids::local_id(ctx.base(), iri).map(|(_, i)| i).unwrap_or_else(|| ids::iri_tail(iri).to_string());
    let capability = p.iri(ns::TAR, "hasCapability").and_then(|c| capability_from(ctx, &c, "software"));
    let mut kinds = p.strs(ns::SCHEMA, "applicationCategory");
    if kinds.is_empty() {
        kinds = p.strs(ns::TAR, "kind");
    }
    kinds.sort();
    Software {
        iri: iri.to_string(),
        id,
        name: p.str(ns::SCHEMA, "name").unwrap_or_else(|| ids::iri_tail(iri).to_string()),
        // Standard term first; the tar: fallbacks keep graphs written before the 2026-08-30
        // vocabulary audit readable at zero cost.
        tagline: p.str(ns::DCT, "abstract").or_else(|| p.str(ns::TAR, "tagline")),
        description: p.str(ns::SCHEMA, "description"),
        homepage: p.iri(ns::SCHEMA, "url"),
        code_repository: p.iri(ns::SCHEMA, "codeRepository"),
        documentation: p.iri(ns::SCHEMA, "softwareHelp"),
        download_url: p.iri(ns::SCHEMA, "downloadUrl"),
        image: p.iri(ns::SCHEMA, "image"),
        screenshots: p.iris(ns::SCHEMA, "screenshot"),
        readme: p.str(ns::TAR, "readme"),
        readme_base_url: p.iri(ns::TAR, "readmeBaseURL"),
        api_docs: p
            .node_keys(ns::DCAT, "endpointDescription")
            .iter()
            .filter_map(|k| {
                let dp = p.nested_for(k)?;
                Some(ApiDoc {
                    url: k.trim_start_matches('<').trim_end_matches('>').to_string(),
                    // The literal is authoritative; conformsTo is the fallback for records a
                    // peer wrote without it.
                    format: dp
                        .str(ns::TAR, "apiFormat")
                        .or_else(|| {
                            dp.iri(ns::DCT, "conformsTo")
                                .and_then(|i| crate::model::api_format_from_iri(&i).map(str::to_string))
                        })
                        .unwrap_or_else(|| "other".into()),
                    title: dp.str(ns::DCT, "title"),
                    description: dp.str(ns::DCT, "description"),
                })
            })
            .collect(),
        license: p.iri(ns::DCT, "license"),
        kind: kinds.first().cloned(),
        kinds,
        maturity: p.str(ns::TAR, "maturity").or_else(|| {
            // Recover the literal from the IRI for records written by an older build.
            p.iri(ns::CODEMETA, "developmentStatus").and_then(|i| i.rsplit('#').next().map(str::to_string))
        }),
        deployable: p.bool(ns::TAR, "deployable").unwrap_or(true),
        registration_clients: p.strs(ns::TAR, "registrationClient"),
        registration_issuer: p.str(ns::TAR, "registrationIssuer"),
        topics: ctx.type_refs(&p.iris(ns::DCT, "subject")),
        keywords: p.strs(ns::SCHEMA, "keywords"),
        publisher: ctx.opt_agent_ref(p.iri(ns::DCT, "publisher")),
        contact: ctx.opt_agent_ref(p.iri(ns::CODEMETA, "maintainer").or_else(|| p.iri(ns::TAR, "contact"))),
        publications: p.iris(ns::DCT, "references"),
        capability,
        sync: p.node_keys(ns::TAR, "sync").first().and_then(|k| {
            let sp = p.nested_for(k)?;
            Some(SyncStatus {
                source: sp.str(ns::TAR, "syncSource").unwrap_or_else(|| "github".into()),
                repo: sp.str(ns::TAR, "syncRepo")?,
                fields: sp.strs(ns::TAR, "syncField"),
                enabled: sp.bool(ns::TAR, "syncEnabled").unwrap_or(true),
                last_synced_at: sp.str(ns::TAR, "syncedAt"),
                last_status: sp.str(ns::TAR, "syncStatus").unwrap_or_else(|| "never".into()),
                last_error: sp.str(ns::TAR, "syncError"),
                last_changed: sp.strs(ns::TAR, "syncChanged"),
            })
        }),
        latest_release: None,
        instance_count: 0,
        release_count: 0,
        runs_30d: 0,
        created: p.str(ns::DCT, "created"),
        modified: p.str(ns::DCT, "modified"),
        origin: ctx.origin(p.graph.as_deref()),
        tombstoned: p.bool(ns::TAR, "tombstoned").unwrap_or(false),
    }
}

pub fn load_software(ctx: &Ctx, iri: &str) -> AppResult<Software> {
    let quads = ctx.state.store.describe(iri).map_err(AppError::from)?;
    if quads.is_empty() {
        return Err(AppError::not_found(format!("no software at {iri}")));
    }
    let p = Props::from_quads(iri, &quads);
    if !p.has_type(TYPE_SOFTWARE) && !p.has_type(TYPE_SOURCE) {
        return Err(AppError::not_found(format!("{iri} is not a Software record")));
    }
    let mut sw = software_from_props(ctx, iri, &p);
    let releases = list_releases(ctx, iri)?;
    sw.release_count = releases.len() as i64;
    sw.latest_release = releases.into_iter().next();
    let counts = software_counts(ctx, Some(iri))?;
    if let Some(c) = counts.get(iri) {
        sw.instance_count = c.instances;
        sw.runs_30d = c.runs_30d;
    }
    Ok(sw)
}

pub fn release_from_props(ctx: &Ctx, iri: &str, p: &Props) -> Release {
    Release {
        iri: iri.to_string(),
        id: ids::local_id(ctx.base(), iri).map(|(_, i)| i).unwrap_or_default(),
        version: p.str(ns::SCHEMA, "softwareVersion").unwrap_or_default(),
        date_published: p.str(ns::SCHEMA, "datePublished"),
        container_image: p.str(ns::TAR, "containerImage"),
        image_digest: p.str(ns::TAR, "imageDigest"),
        // schema:releaseNotes ranges over Text as well as URL, and the write path stores a
        // non-IRI value as a literal. Reading only IRIs meant prose release notes were kept in
        // the graph and never shown — present but invisible, which is the worst of both.
        changelog: p.iri(ns::SCHEMA, "releaseNotes").or_else(|| p.str(ns::SCHEMA, "releaseNotes")),
        install_command: p.str(ns::TAR, "installCommand"),
        downloads: p
            .node_keys(ns::DCAT, "distribution")
            .iter()
            .filter_map(|k| {
                let d = p.nested_for(k)?;
                Some(DownloadIn {
                    url: d.iri(ns::DCAT, "downloadURL")?,
                    label: d.str(ns::DCT, "title"),
                    platform: d.str(ns::SCHEMA, "operatingSystem"),
                    byte_size: d.i64(ns::DCAT, "byteSize"),
                    availability: d.str(ns::TAR, "availability"),
                })
            })
            .collect(),
        software: p.iri(ns::DCT, "isVersionOf"),
        software_name: None,
        capability: p.iri(ns::TAR, "hasCapability").and_then(|c| capability_from(ctx, &c, "release")),
        origin: ctx.origin(p.graph.as_deref()),
    }
}

/// Releases newest first. `datePublished` when present, otherwise the UUIDv7 ordering.
pub fn list_releases(ctx: &Ctx, software_iri: &str) -> AppResult<Vec<Release>> {
    let q = format!(
        r#"{p}
SELECT ?r WHERE {{
  GRAPH ?g {{ ?r dct:isVersionOf <{software_iri}> ; schema:softwareVersion ?v }}
  FILTER NOT EXISTS {{ GRAPH ?g2 {{ ?r tar:tombstoned true }} }}
}}"#,
        p = ns::PREFIXES
    );
    let rows = ctx.state.store.select(&q).map_err(AppError::from)?;
    let mut out = Vec::new();
    for row in rows.rows {
        let Some(iri) = row.iri("r") else { continue };
        let quads = ctx.state.store.describe(&iri).map_err(AppError::from)?;
        let p = Props::from_quads(&iri, &quads);
        out.push(release_from_props(ctx, &iri, &p));
    }
    out.sort_by(|a, b| {
        b.date_published
            .as_deref()
            .unwrap_or("")
            .cmp(a.date_published.as_deref().unwrap_or(""))
            .then_with(|| b.iri.cmp(&a.iri))
    });
    Ok(out)
}

#[derive(Default, Clone, Copy)]
pub struct SoftwareCounts {
    pub instances: i64,
    pub runs_30d: i64,
}

/// Instance and 30-day run counts per Software, in two aggregate queries rather than N.
pub fn software_counts(ctx: &Ctx, only: Option<&str>) -> AppResult<HashMap<String, SoftwareCounts>> {
    let filter = only.map(|s| format!("FILTER(?sw = <{s}>)")).unwrap_or_default();
    let mut out: HashMap<String, SoftwareCounts> = HashMap::new();
    let q = format!(
        r#"{p}
SELECT ?sw (COUNT(DISTINCT ?i) AS ?n) WHERE {{
  GRAPH ?g {{ ?i tar:instanceOf ?sw }} {filter}
  FILTER NOT EXISTS {{ GRAPH ?tg {{ ?i tar:tombstoned true }} }}
}} GROUP BY ?sw"#,
        p = ns::PREFIXES
    );
    for row in ctx.state.store.select(&q).map_err(AppError::from)?.rows {
        if let (Some(sw), Some(n)) = (row.iri("sw"), row.i64("n")) {
            out.entry(sw).or_default().instances = n;
        }
    }
    let since = super::thirty_days_ago();
    let q = format!(
        r#"{p}
SELECT ?sw (COUNT(DISTINCT ?run) AS ?n) WHERE {{
  GRAPH ?g {{
    ?run a prov:Activity ; prov:wasAssociatedWith|tar:atInstance ?i ; prov:startedAtTime ?t .
    ?i tar:instanceOf ?sw .
  }}
  FILTER(?t >= "{since}"^^xsd:dateTime)
  {filter}
}} GROUP BY ?sw"#,
        p = ns::PREFIXES
    );
    for row in ctx.state.store.select(&q).map_err(AppError::from)?.rows {
        if let (Some(sw), Some(n)) = (row.iri("sw"), row.i64("n")) {
            out.entry(sw).or_default().runs_30d = n;
        }
    }
    Ok(out)
}

/// Replace a Software record in place, preserving its creation date and capability link
/// unless the patch supplies new ones.
pub fn replace_software(base: &str, iri: &str, input: &SoftwareIn, actor: &str, created: Option<String>) -> GraphTx {
    let mut tx = GraphTx::new();
    tx.replace_subject(iri, ns::G_LOCAL);
    tx.extend(software_quads(base, iri, input, actor, created));
    tx
}
