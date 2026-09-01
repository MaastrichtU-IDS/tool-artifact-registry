//! Markdown renderings of registry records, and the site-level `llms.txt` (the convention at
//! <https://llmstxt.org>).
//!
//! An agent that fetches a registry IRI gets one of three unhelpful things: HTML that is an
//! empty SPA shell until JavaScript runs, Turtle that needs an RDF parser, or JSON whose field
//! names it has to guess the meaning of. Markdown is the fourth option, and it is the one that
//! costs the agent nothing: the whole record as prose, with every IRI written out so the next
//! fetch is obvious.
//!
//! This is a *representation*, not a second copy of the data. `/software/{id}.md` and
//! `Accept: text/markdown` reach the same renderer as `.ttl` reaches the serialiser, from the
//! same graph, so the prose cannot drift from the RDF.

use crate::model::*;

/// Header shared by every record page: what this is, and where the machine-readable forms are.
fn front_matter(out: &mut String, kind: &str, title: &str, iri: &str) {
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!("> {kind} in a Tool Artifact Registry. Canonical IRI: <{iri}>\n\n"));
    out.push_str(&format!(
        "Other representations of this same record: [Turtle]({iri}.ttl), [JSON-LD]({iri}.jsonld), [JSON]({iri}.json).\n\n",
    ));
}

fn field(out: &mut String, label: &str, value: Option<&str>) {
    if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
        out.push_str(&format!("- **{label}:** {v}\n"));
    }
}

fn link_field(out: &mut String, label: &str, value: Option<&str>) {
    if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
        out.push_str(&format!("- **{label}:** <{v}>\n"));
    }
}

fn list_field(out: &mut String, label: &str, values: &[String]) {
    if !values.is_empty() {
        out.push_str(&format!("- **{label}:** {}\n", values.join(", ")));
    }
}

fn types_field(out: &mut String, label: &str, values: &[TypeRef]) {
    if values.is_empty() {
        return;
    }
    let rendered: Vec<String> = values
        .iter()
        .map(|t| match &t.label {
            Some(l) => format!("{l} (`{}`)", t.iri),
            None => format!("`{}`", t.iri),
        })
        .collect();
    out.push_str(&format!("- **{label}:** {}\n", rendered.join("; ")));
}

fn agent_line(a: &AgentRef) -> String {
    let mut s = a.name.clone().unwrap_or_else(|| a.iri.clone());
    let mut extras = Vec::new();
    if let Some(id) = &a.identifier {
        extras.push(id.clone());
    }
    if let Some(e) = &a.email {
        extras.push(e.clone());
    }
    if let Some(h) = &a.homepage {
        extras.push(h.clone());
    }
    if !extras.is_empty() {
        s.push_str(&format!(" — {}", extras.join(", ")));
    }
    s
}

fn agents_field(out: &mut String, label: &str, agents: &[AgentRef]) {
    if agents.is_empty() {
        return;
    }
    let rendered: Vec<String> = agents.iter().map(agent_line).collect();
    out.push_str(&format!("- **{label}:** {}\n", rendered.join("; ")));
}

/// Where the record came from — stated on every page, because an agent reading a peer's cached
/// stub should know it is reading a cache and where the original lives.
fn origin_section(out: &mut String, origin: &Origin) {
    if origin.kind == "local" {
        return;
    }
    out.push_str("\n## Provenance of this record\n\n");
    out.push_str("This record is a cached stub held on behalf of another registry, not an original.\n\n");
    field(out, "Home registry", origin.peer_title.as_deref());
    link_field(out, "Home base IRI", origin.peer_base_iri.as_deref());
    field(out, "Cached at", origin.cached_at.as_deref());
    field(out, "Resolve status", origin.resolve_status.as_deref());
}

fn tombstone_note(out: &mut String, tombstoned: bool) {
    if tombstoned {
        out.push_str(
            "\n> **Withdrawn.** This record has been withdrawn by a curator. Its IRI still \
             resolves, because IRIs that once meant something must keep meaning it, but the \
             record is no longer listed and should not be treated as current.\n",
        );
    }
}

// -------------------------------------------------------------------- software

pub fn software(s: &Software, releases: &[Release], instances: &[Instance]) -> String {
    let mut out = String::new();
    front_matter(&mut out, "Software", &s.name, &s.iri);
    tombstone_note(&mut out, s.tombstoned);

    if let Some(t) = &s.tagline {
        out.push_str(&format!("{t}\n\n"));
    }

    out.push_str("## Summary\n\n");
    field(&mut out, "Name", Some(&s.name));
    if !s.kinds.is_empty() {
        out.push_str(&format!("- **Kind:** {}\n", s.kinds.join(", ")));
    }
    field(&mut out, "Maturity", s.maturity.as_deref());
    field(&mut out, "License", s.license.as_deref());
    // Stated in both directions, because "can this be deployed" is the question an agent most
    // often gets wrong about a CLI or a desktop application.
    out.push_str(&format!(
        "- **Deployable:** {}\n",
        if s.deployable {
            "yes — it can be hosted, and its deployments may carry an endpoint"
        } else {
            "no — this software cannot be hosted; it runs on a user's own machine, so no \
             deployment of it has a callable endpoint"
        }
    ));
    types_field(&mut out, "Research topics", &s.topics);
    list_field(&mut out, "Keywords", &s.keywords);
    if let Some(p) = &s.publisher {
        out.push_str(&format!("- **Publisher:** {}\n", agent_line(p)));
    }
    if let Some(c) = &s.contact {
        out.push_str(&format!("- **Contact:** {}\n", agent_line(c)));
    }

    out.push_str("\n## Where to get it\n\n");
    link_field(&mut out, "Homepage", s.homepage.as_deref());
    link_field(&mut out, "Source code", s.code_repository.as_deref());
    link_field(&mut out, "Documentation", s.documentation.as_deref());
    link_field(&mut out, "Download", s.download_url.as_deref());
    for p in &s.publications {
        out.push_str(&format!("- **Publication:** <{p}>\n"));
    }

    if let Some(cap) = &s.capability {
        out.push_str("\n## Capability\n\n");
        out.push_str(&format!("Declared at the *{}* layer.\n\n", cap.declared_at));
        types_field(&mut out, "Consumes", &cap.consumes);
        types_field(&mut out, "Produces", &cap.produces);
    }

    if !s.api_docs.is_empty() {
        out.push_str("\n## API\n\n");
        out.push_str(
            "Machine-readable descriptions of this software's API. Fetch the document itself to \
             learn the operations; the registry does not restate them.\n\n",
        );
        for d in &s.api_docs {
            let kind = api_format_label(&d.format);
            let title = d.title.clone().unwrap_or_else(|| kind.to_string());
            // Only name the format when the title does not already say it.
            let suffix = if title.eq_ignore_ascii_case(kind) { String::new() } else { format!(" ({kind})") };
            out.push_str(&format!("- **{title}**{suffix}: <{}>\n", d.url));
            if let Some(desc) = &d.description {
                out.push_str(&format!("  - {desc}\n"));
            }
        }
    }

    out.push_str("\n## Activity\n\n");
    out.push_str(&format!("- **Deployments:** {}\n", s.instance_count));
    out.push_str(&format!("- **Releases:** {}\n", s.release_count));
    out.push_str(&format!("- **Runs in the last 30 days:** {}\n", s.runs_30d));
    field(&mut out, "Record created", s.created.as_deref());
    field(&mut out, "Record last modified", s.modified.as_deref());

    if let Some(sync) = &s.sync {
        out.push_str("\n## Synchronisation\n\n");
        out.push_str(&format!(
            "- **Source:** {} `{}` ({})\n",
            sync.source,
            sync.repo,
            if sync.enabled { "enabled" } else { "disabled" }
        ));
        list_field(&mut out, "Fields owned by the source", &sync.fields);
        field(&mut out, "Last synced", sync.last_synced_at.as_deref());
        field(&mut out, "Last status", Some(&sync.last_status));
        field(&mut out, "Last error", sync.last_error.as_deref());
    }

    if !releases.is_empty() {
        out.push_str("\n## Releases\n\n");
        for r in releases {
            let when = r.date_published.as_deref().unwrap_or("date unknown");
            out.push_str(&format!("- **{}** ({when}) — <{}>\n", r.version, r.iri));
            if let Some(img) = &r.container_image {
                out.push_str(&format!("  - Container image: `{img}`\n"));
            }
            if let Some(cmd) = &r.install_command {
                out.push_str(&format!("  - Install: `{cmd}`\n"));
            }
            for d in &r.downloads {
                let what = [d.label.as_deref(), d.platform.as_deref()]
                    .iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("/");
                let what = if what.is_empty() { "download".to_string() } else { what };
                out.push_str(&format!("  - Download ({what}): <{}>\n", d.url));
            }
        }
    }

    if !instances.is_empty() {
        out.push_str("\n## Deployments\n\n");
        for i in instances {
            out.push_str(&format!("- **{}** — <{}>\n", i.label, i.iri));
            if let Some(e) = &i.endpoint_url {
                out.push_str(&format!("  - Endpoint: <{e}> (health: {})\n", i.health));
            }
        }
    } else if s.deployable {
        out.push_str("\n## Deployments\n\nNone registered.\n");
    }

    if let Some(d) = &s.description {
        out.push_str("\n## Description\n\n");
        out.push_str(d.trim());
        out.push('\n');
    }

    if let Some(readme) = &s.readme {
        out.push_str("\n## README\n\n");
        if let Some(base) = &s.readme_base_url {
            out.push_str(&format!(
                "*Relative links and images below resolve against <{base}>.*\n\n"
            ));
        }
        out.push_str(readme.trim());
        out.push('\n');
    }

    origin_section(&mut out, &s.origin);
    out
}

pub fn api_format_label(format: &str) -> &str {
    match format {
        "openapi" => "OpenAPI",
        "asyncapi" => "AsyncAPI",
        "graphql" => "GraphQL schema",
        "sparql-service-description" => "SPARQL service description",
        "ols4" => "OLS4-compatible API",
        "postman" => "Postman collection",
        _ => "API description",
    }
}

// -------------------------------------------------------------------- instance

pub fn instance(i: &Instance) -> String {
    let mut out = String::new();
    front_matter(&mut out, "Deployment (Instance)", &i.label, &i.iri);
    tombstone_note(&mut out, i.tombstoned);

    out.push_str("A running deployment of a piece of software, operated by someone in particular.\n\n");
    out.push_str("## Summary\n\n");
    field(&mut out, "Label", Some(&i.label));
    field(&mut out, "Description", i.description.as_deref());
    if let (Some(name), Some(iri)) = (&i.software_name, &i.software) {
        out.push_str(&format!("- **Software:** {name} — <{iri}>\n"));
    }
    if let Some(v) = &i.release_version {
        out.push_str(&format!(
            "- **Running version:** {v}{}\n",
            if i.outdated {
                match &i.latest_version {
                    Some(l) => format!(" (outdated — latest is {l})"),
                    None => " (outdated)".to_string(),
                }
            } else {
                String::new()
            }
        ));
    }
    if let Some(op) = &i.operator {
        out.push_str(&format!("- **Operator:** {}\n", agent_line(op)));
    }
    field(&mut out, "Availability", i.availability.as_deref());
    field(&mut out, "Jurisdiction", i.jurisdiction.as_deref());

    out.push_str("\n## Reaching it\n\n");
    match &i.endpoint_url {
        Some(e) => out.push_str(&format!("- **Endpoint:** <{e}>\n")),
        None => out.push_str(
            "- **Endpoint:** none. This deployment has no callable endpoint — it is a local \
             install, a CLI, or a desktop application, and is reached by running it yourself.\n",
        ),
    }
    link_field(&mut out, "Endpoint description", i.endpoint_description.as_deref());
    out.push_str(&format!("- **Health:** {}\n", i.health));
    field(&mut out, "Health detail", i.health_detail.as_deref());
    field(&mut out, "Health last checked", i.health_checked_at.as_deref());
    field(&mut out, "Last seen", i.last_seen_at.as_deref());
    field(&mut out, "Authenticates as (OIDC client)", i.oidc_client_id.as_deref());
    field(&mut out, "Trusted issuer", i.oidc_issuer.as_deref());
    field(&mut out, "Self-registered by", i.self_registered_by.as_deref());
    field(&mut out, "Self-registered via issuer", i.self_registered_issuer.as_deref());
    list_field(&mut out, "Allowed scopes", &i.allowed_scopes);

    if let Some(cap) = &i.capability {
        out.push_str("\n## Capability\n\n");
        out.push_str(&format!("Declared at the *{}* layer.\n\n", cap.declared_at));
        types_field(&mut out, "Consumes", &cap.consumes);
        types_field(&mut out, "Produces", &cap.produces);
    }

    out.push_str("\n## Activity\n\n");
    out.push_str(&format!("- **Runs in the last 30 days:** {}\n", i.runs_30d));
    out.push_str(&format!("- **Failures in the last 30 days:** {}\n", i.failures_30d));
    out.push_str(&format!("- **Artifacts advertised:** {}\n", i.artifact_count));
    field(&mut out, "Last run", i.last_run_at.as_deref());

    origin_section(&mut out, &i.origin);
    out
}

// -------------------------------------------------------------------- artifact

pub fn artifact(a: &Artifact) -> String {
    let mut out = String::new();
    let title = a.title.clone().unwrap_or_else(|| "Untitled artifact".into());
    front_matter(&mut out, "Artifact", &title, &a.iri);

    out.push_str("A dataset produced or consumed by a run of some software.\n\n");
    out.push_str("## Summary\n\n");
    field(&mut out, "Title", a.title.as_deref());
    field(&mut out, "Description", a.description.as_deref());
    if let Some(t) = &a.conforms_to {
        out.push_str(&format!(
            "- **Artifact type:** {} (`{}`)\n",
            t.label.clone().unwrap_or_else(|| t.iri.clone()),
            t.iri
        ));
    }
    field(&mut out, "Version", a.version.as_deref());
    field(&mut out, "License", a.license.as_deref());
    list_field(&mut out, "Keywords", &a.keywords);
    list_field(&mut out, "Language", &a.language);
    field(&mut out, "Issued", a.issued.as_deref());
    field(&mut out, "Modified", a.modified.as_deref());
    field(&mut out, "Spatial coverage", a.spatial.as_deref());
    if a.temporal_start.is_some() || a.temporal_end.is_some() {
        out.push_str(&format!(
            "- **Temporal coverage:** {} to {}\n",
            a.temporal_start.as_deref().unwrap_or("unspecified"),
            a.temporal_end.as_deref().unwrap_or("unspecified")
        ));
    }
    out.push_str(&format!("- **Availability:** {}\n", a.availability));

    out.push_str("\n## Who made it\n\n");
    agents_field(&mut out, "Creators", &a.creators);
    agents_field(&mut out, "Contributors", &a.contributors);
    if let Some(p) = &a.publisher {
        out.push_str(&format!("- **Publisher:** {}\n", agent_line(p)));
    }
    if let Some(c) = &a.contact {
        out.push_str(&format!("- **Contact:** {}\n", agent_line(c)));
    }
    if let Some(at) = &a.attributed_to {
        out.push_str(&format!(
            "- **Advertised by:** <{at}> — recorded by the registry from the credential the \
             advertisement arrived on, not supplied by the caller.\n"
        ));
    }

    out.push_str("\n## Where to get it\n\n");
    link_field(&mut out, "Landing page", a.landing_page.as_deref());
    link_field(&mut out, "Documentation", a.documentation.as_deref());
    link_field(&mut out, "Source", a.source.as_deref());
    if a.distributions.is_empty() {
        out.push_str(
            "No distributions: this is a metadata-only record. The registry knows the artifact \
             exists but holds no way to fetch its bytes.\n",
        );
    }
    for d in &a.distributions {
        let t = d.title.clone().unwrap_or_else(|| "Distribution".into());
        out.push_str(&format!("\n### {t}\n\n"));
        link_field(&mut out, "Download URL", d.download_url.as_deref());
        link_field(&mut out, "Access URL", d.access_url.as_deref());
        field(&mut out, "Media type", d.media_type.as_deref());
        if let Some(b) = d.byte_size {
            out.push_str(&format!("- **Size:** {b} bytes\n"));
        }
        if let Some(c) = &d.checksum {
            out.push_str(&format!("- **Checksum:** {} `{}`\n", c.algorithm, c.value));
        }
        if let Some(cid) = &d.content_identifier {
            out.push_str(&format!(
                "- **Content identifier:** `{cid}` — derived from the checksum above, not minted \
                 here. Any registry given the same digest derives the same string, so this is how \
                 to tell that another registry's record describes these same bytes.\n"
            ));
        }
        field(&mut out, "Access protocol", d.access_protocol.as_deref());
        field(&mut out, "Authentication", d.auth_method.as_deref());
        field(&mut out, "Availability", Some(&d.availability));
        link_field(&mut out, "Request access at", d.access_request_url.as_deref());
    }

    if !a.was_derived_from.is_empty() || a.was_revision_of.is_some() {
        out.push_str("\n## Lineage\n\n");
        for d in &a.was_derived_from {
            out.push_str(&format!("- **Derived from:** <{d}>\n"));
        }
        link_field(&mut out, "Revision of", a.was_revision_of.as_deref());
        out.push_str(&format!(
            "\nFull lineage graph: <{}/lineage>\n",
            a.iri.replace("/artifact/", "/api/v1/artifacts/")
        ));
    }

    origin_section(&mut out, &a.origin);
    out
}

// ------------------------------------------------------------------------- run

pub fn run(r: &Run) -> String {
    let s = &r.summary;
    let mut out = String::new();
    let title = s.label.clone().unwrap_or_else(|| format!("Run {}", s.id));
    front_matter(&mut out, "Run", &title, &s.iri);

    out.push_str("One execution of a piece of software, and the artifacts it touched.\n\n");
    out.push_str("## Summary\n\n");
    out.push_str(&format!("- **Status:** {}\n", s.status));
    field(&mut out, "Started", s.started_at.as_deref());
    field(&mut out, "Ended", s.ended_at.as_deref());
    if let Some(d) = s.duration_seconds {
        out.push_str(&format!("- **Duration:** {d} seconds\n"));
    }
    if let (Some(n), Some(iri)) = (&s.software_name, &s.software) {
        out.push_str(&format!("- **Software:** {n} — <{iri}>\n"));
    }
    if let (Some(n), Some(iri)) = (&s.instance_label, &s.instance) {
        out.push_str(&format!("- **Deployment:** {n} — <{iri}>\n"));
    }
    field(&mut out, "Version run", s.release_version.as_deref());
    field(&mut out, "External key", s.external_key.as_deref());

    for (heading, items) in [("Consumed", &r.used), ("Produced", &r.generated)] {
        out.push_str(&format!("\n## {heading}\n\n"));
        if items.is_empty() {
            out.push_str("Nothing recorded.\n");
        }
        for a in items {
            let t = a.title.clone().unwrap_or_else(|| a.iri.clone());
            let ty = a
                .conforms_to
                .as_ref()
                .map(|t| format!(" — {}", t.label.clone().unwrap_or_else(|| t.iri.clone())))
                .unwrap_or_default();
            let note = if a.unresolved {
                " *(a record held by another registry, not yet resolved)*"
            } else {
                ""
            };
            out.push_str(&format!("- **{t}**{ty} — <{}>{note}\n", a.iri));
        }
    }

    origin_section(&mut out, &s.origin);
    out
}

// --------------------------------------------------------------------- release

pub fn release(r: &Release) -> String {
    let mut out = String::new();
    let title = match &r.software_name {
        Some(n) => format!("{n} {}", r.version),
        None => format!("Release {}", r.version),
    };
    front_matter(&mut out, "Release", &title, &r.iri);

    out.push_str("## Summary\n\n");
    field(&mut out, "Version", Some(&r.version));
    field(&mut out, "Published", r.date_published.as_deref());
    if let (Some(n), Some(iri)) = (&r.software_name, &r.software) {
        out.push_str(&format!("- **Software:** {n} — <{iri}>\n"));
    }
    field(&mut out, "Container image", r.container_image.as_deref());
    field(&mut out, "Image digest", r.image_digest.as_deref());
    field(&mut out, "Install command", r.install_command.as_deref());

    if !r.downloads.is_empty() {
        out.push_str("\n## Downloads\n\n");
        for d in &r.downloads {
            let what = [d.label.as_deref(), d.platform.as_deref()]
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join("/");
            let what = if what.is_empty() { "any platform".to_string() } else { what };
            out.push_str(&format!("- **{what}:** <{}>\n", d.url));
        }
    }

    if let Some(cap) = &r.capability {
        out.push_str("\n## Capability\n\n");
        types_field(&mut out, "Consumes", &cap.consumes);
        types_field(&mut out, "Produces", &cap.produces);
    }

    if let Some(c) = &r.changelog {
        out.push_str("\n## Changelog\n\n");
        out.push_str(c.trim());
        out.push('\n');
    }

    origin_section(&mut out, &r.origin);
    out
}

// ------------------------------------------------------------------ site index

/// One entry in the site index.
pub struct IndexEntry {
    pub title: String,
    pub iri: String,
    pub note: Option<String>,
}

/// The site-level `llms.txt` (<https://llmstxt.org>): a short statement of what this registry
/// is, how to read any record without a parser, and a link to every record it holds.
///
/// The convention's own shape is an H1, a blockquote summary, free prose, then `##` sections of
/// links. We follow it exactly, because the value of a convention is that a client written
/// against someone else's site works against ours.
pub struct SiteIndex<'a> {
    pub title: &'a str,
    pub base: &'a str,
    pub operator: Option<&'a str>,
    pub software: Vec<IndexEntry>,
    pub instances: Vec<IndexEntry>,
    pub artifacts: Vec<IndexEntry>,
    pub runs: Vec<IndexEntry>,
    /// Totals, so a truncated section can say what it left out rather than quietly lying.
    pub totals: (i64, i64, i64, i64),
    pub sparql_public: bool,
    pub mcp_enabled: bool,
}

fn index_section(
    out: &mut String,
    heading: &str,
    entries: &[IndexEntry],
    total: i64,
    base: &str,
    plural: &str,
    // Whether this section is a recent window rather than the whole set. The wording has to
    // say which, or a reader takes a truncated list for the complete one.
    recent: bool,
) {
    out.push_str(&format!("\n## {heading}\n\n"));
    if entries.is_empty() {
        out.push_str("None registered.\n");
        return;
    }
    for e in entries {
        let note = e.note.as_deref().map(|n| format!(": {n}")).unwrap_or_default();
        out.push_str(&format!("- [{}]({}.md){note}\n", e.title.replace(']', ")"), e.iri));
    }
    if total > entries.len() as i64 {
        let what = if recent { "The most recent" } else { "" };
        out.push_str(&format!(
            "\n{what}{}{} of {total}. The rest are at <{base}/api/v1/{plural}>, which pages with `?cursor=`, \
             newest first.\n",
            if recent { " " } else { "" },
            entries.len()
        ));
    }
}

pub fn site_index(ix: &SiteIndex<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", ix.title));
    out.push_str(
        "> A Tool Artifact Registry: the software available in this estate, where each piece is \
         deployed, and the data artifacts those deployments produce and consume.\n\n",
    );
    if let Some(op) = ix.operator {
        out.push_str(&format!("Operated by {op}.\n\n"));
    }

    out.push_str("## How to read this registry\n\n");
    out.push_str(&format!(
        "Every record has a permanent IRI under `{}` that is also its web page. Append an \
         extension to any of them, or send an `Accept` header, to choose a representation:\n\n",
        ix.base
    ));
    out.push_str(
        "- `.md` — this format. The whole record as prose, with every related IRI written out. \
           Equivalent to `Accept: text/markdown`.\n\
         - `.ttl` — Turtle. The record as RDF, which is what it natively is.\n\
         - `.jsonld` — JSON-LD.\n\
         - `.json` — a flat developer JSON shape.\n\
         - no extension — the HTML page, which is a JavaScript application. Prefer `.md`.\n\n",
    );
    out.push_str(
        "Every response also carries FAIR Signposting `Link` headers, so the alternates above \
         can be discovered from any single response rather than assumed.\n\n",
    );

    out.push_str("## Entry points\n\n");
    out.push_str(&format!("- [Registry description]({}/api/v1/registry): what this registry is, its counts, and its capabilities.\n", ix.base));
    out.push_str(&format!("- [Search]({}/api/v1/search?q=): free-text search across every record type. Add `&federated=true` to ask this registry's peers too.\n", ix.base));
    out.push_str(&format!("- [Software]({}/api/v1/software): the catalogue. Filter by `?kind=`, `?topic=`, `?keyword=`, `?license=`, `?produces=`, `?consumes=`.\n", ix.base));
    out.push_str(&format!("- [Deployments]({}/api/v1/instances): where the software actually runs, with health.\n", ix.base));
    out.push_str(&format!("- [Artifacts]({}/api/v1/artifacts): the data. Filter by `?conforms_to=`, `?availability=`, `?keyword=` and `?content=`.\n", ix.base));
    out.push_str(&format!("- [Name a set of bytes]({}/api/v1/artifacts/identify?algorithm=sha256&value=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855): turn a digest into the content identifier this registry derives from it. `GET` with `?algorithm=&value=`, or `POST` `{{\"algorithm\":\"sha256\",\"value\":\"…\"}}`. It writes nothing and needs no more credential than a read; the section on naming bytes, at the end of this file, has how to compute the same string without calling it at all.\n", ix.base));
    out.push_str(&format!("- [Runs]({}/api/v1/runs): executions, each linking the artifacts it used and generated.\n", ix.base));
    out.push_str(&format!("- [Capability matchmaking]({}/api/v1/capabilities?produces=): which software can produce or consume a given type of artifact.\n", ix.base));
    out.push_str(&format!("- [Vocabulary search]({}/api/v1/vocab/search?q=&branch=topic): look up a controlled term before citing it. `branch=topic` for research topics, `branch=data` for artifact types. Never guess a term IRI — a write naming one this registry cannot resolve is refused, and the refusal says how to recover.\n", ix.base));
    out.push_str(&format!("- [Artifact types]({}/api/v1/types): what an artifact may say it is. When the search has nothing, POST here — with an `iri` to adopt a term that already has one elsewhere, without to have this registry name it.\n", ix.base));
    out.push_str(&format!("- [Artifact keywords]({}/api/v1/keywords): the short list of keywords this registry recognises on artifacts. Use these spellings; anything else is kept as free text and will not match a keyword filter or a subscription written against the list.\n", ix.base));
    if ix.sparql_public {
        out.push_str(&format!("- [SPARQL]({}/sparql?query=): read-only SPARQL 1.1 over everything above, open without credentials. `POST` a query as `application/sparql-query`, or `GET` with `?query=`. Ask for `Accept: application/sparql-results+json`.\n", ix.base));
    }
    if ix.mcp_enabled {
        out.push_str(&format!("- [MCP]({}/mcp): a hosted Model Context Protocol server over this same registry, for agents that would rather call tools than compose URLs.\n", ix.base));
    }

    index_section(&mut out, "Software", &ix.software, ix.totals.0, ix.base, "software", false);
    index_section(&mut out, "Deployments", &ix.instances, ix.totals.1, ix.base, "instances", false);
    // Named for what they are: a window on what this registry is currently doing, not a
    // catalogue. A busy pipeline makes more of these in a day than the catalogue holds.
    index_section(&mut out, "Recent artifacts", &ix.artifacts, ix.totals.2, ix.base, "artifacts", true);
    index_section(&mut out, "Recent runs", &ix.runs, ix.totals.3, ix.base, "runs", true);

    out.push_str("\n## Things worth knowing before you write anything down\n\n");
    out.push_str(
        "- **`deployable: no` means there is no endpoint to call.** A CLI or a desktop \
           application is registered here with releases and downloads, and no deployment of it \
           will ever answer a request. Do not invent one.\n\
         - **Records from peer registries are cached stubs.** Each says so, with the home \
           registry named. Treat the home registry as authoritative.\n\
         - **A withdrawn record still resolves.** Its IRI keeps working and the page says it \
           was withdrawn; it is not current.\n\
         - **Vocabulary terms must be looked up, not guessed.** A plausible-looking term IRI \
           that belongs to the wrong branch is rejected on write.\n\
         - **Artifact keywords are normalised against the registry's list.** Write `shacl` or \
           `SHACL Shapes` and it is stored as `SHACL`; write something not on the list and it \
           is kept verbatim, which is fine but will not match a keyword filter.\n\
         - **A hash the registry cannot verify is an assertion, not a proof.** The registry never \
           holds the bytes. What it does record, and what no caller can set, is which credential \
           asserted the digest. Two records may claim the same content identifier; the registry \
           shows both and merges neither.\n",
    );

    out.push_str("\n## Naming a set of bytes\n\n");
    out.push_str(
        "Every record here has an identifier this registry minted, and no other registry can \
         guess it. A file also has an identifier that *nobody* mints: it is computed from the \
         file's digest, so two registries handed the same file arrive at the same string with no \
         coordination. That is what makes it possible to notice that a peer's record and a local \
         one describe the same data.\n\n\
         The form is [RFC 6920](https://www.rfc-editor.org/rfc/rfc6920): `ni:///<algorithm>;<digest \
         in base64url, unpadded>`, for example \
         `ni:///sha-256;47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU`. Compute it yourself — it is \
         a pure function of the digest and calling a registry to learn it is a dependency you do \
         not need:\n\n",
    );
    out.push_str(
        "```sh\n\
         # the identifier, in one line\n\
         printf 'ni:///sha-256;%s\\n' \\\n\
         \x20 \"$(openssl dgst -binary -sha256 FILE | openssl base64 -A | tr '+/' '-_' | tr -d '=')\"\n\
         \n\
         # just the digest, if that is all you need for the record\n\
         sha256sum FILE | cut -d' ' -f1\n\
         ```\n\n",
    );
    out.push_str(
        "```python\n\
         import hashlib, base64\n\
         d = hashlib.sha256(open('FILE','rb').read()).digest()\n\
         print('ni:///sha-256;' + base64.urlsafe_b64encode(d).decode().rstrip('='))\n\
         ```\n\n",
    );
    out.push_str(
        "```javascript\n\
         const { createHash } = require('node:crypto'), { readFileSync } = require('node:fs');\n\
         const d = createHash('sha256').update(readFileSync('FILE')).digest('base64url');\n\
         console.log(`ni:///sha-256;${d}`);\n\
         ```\n\n",
    );
    out.push_str(&format!(
        "Send the digest as a distribution's `checksum` when you advertise, and the registry \
         derives the identifier and records it alongside. To find every record of those bytes, \
         here and in the peer registries this one caches, ask \
         `{}/api/v1/artifacts?content=` with either the identifier or the bare digest. Records are \
         never merged: two descriptions of one file come back as two records, each saying where it \
         came from.\n\n\
         Not every artifact has bytes. `availability: metadata-only` means the registry knows the \
         artifact exists and holds no way to fetch it; those records carry no digest and no \
         content identifier, and that is correct rather than incomplete.\n",
        ix.base
    ));
    out
}
