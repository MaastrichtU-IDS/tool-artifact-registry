//! The tool catalogue: what an agent may ask the registry to do, and — just as importantly —
//! how the tool is described to the model.
//!
//! # The failure mode this catalogue is designed against
//!
//! A registry's records are only as good as the metadata in them, and a language model asked
//! to fill in a form will fill it in. A guessed `http://edamontology.org/topic_3170` or a
//! confident "MIT licence" for a repository that states none produces a record that *looks*
//! right and is wrong, which is strictly worse than an empty one: the UI renders an absent
//! licence honestly as "licence not stated", and there is no rendering for "invented".
//!
//! Three things push against that, in increasing order of how much they can be relied on:
//!
//! 1. **The descriptions below say so, in the tool text the model actually reads.** Every
//!    write tool repeats [`NO_INVENTION`], and every vocabulary-valued parameter repeats it
//!    again in its own `description`, because a model reads the parameter it is filling.
//! 2. **Vocabulary is a first-class tool, not a footnote.** [`vocab_search`] and
//!    [`list_enumerations`] exist so that looking a term up is cheaper than recalling one, and
//!    `register_artifact_type` is the honest escape hatch for "the vocabulary has no term for
//!    this" — the alternative to which is a plausible invented IRI.
//! 3. **The server checks.** Prose is a suggestion; `crate::mcp::call::guard_vocabulary`
//!    checks every vocabulary IRI before the write and *refuses* one from a vocabulary this
//!    registry bundles (EDAM, EuroSciVoc) or minted itself that it cannot resolve. That is the
//!    one measure that holds when the model has not read the description.
//!
//! # Why this set and not a mirror of the REST API
//!
//! The REST surface has around forty routes. Most of them are pagination variants, federation
//! plumbing, or operations that should not be reachable by an agent at all (minting tokens,
//! deleting records, adding peers, raw SPARQL). Seventeen tools cover everything an agent
//! sitting in a repository actually needs, and a smaller catalogue is a better one: each entry
//! costs context on every request, and near-duplicates make the model choose badly.

use crate::auth::{Principal, SCOPE_ADVERTISE_CONSUME, SCOPE_ADVERTISE_PRODUCE, SCOPE_REGISTER_INSTANCE, SCOPE_REGISTER_SOFTWARE};
use serde_json::{json, Value};

/// Repeated into every write tool's description. Clients are not obliged to show the
/// server-level `instructions` to the model, but they always show the tool description.
pub const NO_INVENTION: &str = "DO NOT INVENT VALUES. \
Ontology IRIs (topics, artifact types) MUST come from `vocab_search` or `register_artifact_type`; \
this server refuses an IRI from a vocabulary it bundles that it cannot resolve, so guessing fails loudly rather than quietly. \
Closed value sets MUST come from `list_enumerations`. \
Omit any field you cannot confirm from the repository, the package metadata or the user — \
the registry renders an absent field honestly (\"licence not stated\"), while a plausible wrong value is undetectable.";

/// Server-level `instructions`, offered in `server/discover` and `initialize`.
pub fn instructions() -> String {
    format!(
        "This is the Tool Artifact Registry: a FAIR catalogue of research software, its deployments, \
         the runs they perform and the data artifacts those runs produce and consume.\n\n\
         Start with `registry_info` — it reports what this registry holds and, from your own credential, \
         exactly what you are allowed to write. Then use `vocab_search` and `list_enumerations` before \
         any write: they are cheap, and they are the difference between a record that is right and one \
         that merely looks right.\n\n{NO_INVENTION}\n\n\
         Writes are validated against the registry's SHACL shapes. A rejected write comes back with the \
         offending field named — treat that as a correction loop: fix the named field and retry, or drop \
         the field if you cannot establish its true value."
    )
}

/// What authority a tool needs, mirroring the check the REST handler behind it performs.
///
/// This is used for two things: filtering `tools/list` to what the caller can actually do —
/// which the spec explicitly permits, since "the set MAY vary by the authorization presented
/// on the request" — and producing an actionable message when a call is refused. It is *not*
/// the enforcement point. Enforcement is the REST handler's own `require_*` call, reached
/// because every tool executes as an internal HTTP request carrying the caller's own token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Any authenticated principal.
    Read,
    /// `Principal::require_curator` — the curator role, or the `register:software` scope.
    Curator,
    /// Curator, or the named scope (`instances::create`'s rule).
    CuratorOrScope(&'static str),
    /// A credential that acts *as an Instance* and carries the named scope (spec §8.3).
    InstanceScope(&'static str),
}

impl Gate {
    pub fn allows(&self, p: &Principal) -> bool {
        match self {
            Gate::Read => !p.is_anonymous(),
            Gate::Curator => p.is_curator() || p.has_scope(SCOPE_REGISTER_SOFTWARE),
            Gate::CuratorOrScope(s) => p.is_curator() || p.has_scope(s),
            Gate::InstanceScope(s) => p.instance_iri.is_some() && p.has_scope(s),
        }
    }

    /// What to tell the model when it calls a tool it is not allowed to call.
    pub fn refusal(&self, p: &Principal) -> String {
        let held = if p.scopes.is_empty() {
            "no scopes".to_string()
        } else {
            p.scopes.iter().cloned().collect::<Vec<_>>().join(" ")
        };
        match self {
            Gate::Read => "this tool needs an authenticated credential".to_string(),
            Gate::Curator => format!(
                "this tool needs the curator role or the {SCOPE_REGISTER_SOFTWARE} scope; your credential has {held}. \
                 Ask a registry curator to make the change, or to grant the scope."
            ),
            Gate::CuratorOrScope(s) => format!(
                "this tool needs the curator role or the {s} scope; your credential has {held}."
            ),
            Gate::InstanceScope(s) => {
                if p.instance_iri.is_none() {
                    format!(
                        "this tool advertises on behalf of a deployment, so it needs a credential that acts as an \
                         Instance (an instance API token, or an OIDC workload token whose client id an Instance \
                         declares). Your credential does not map to an Instance, and the registry never takes the \
                         Instance from the request body. It also needs the {s} scope."
                    )
                } else {
                    format!("this tool needs the {s} scope; your credential has {held}.")
                }
            }
        }
    }
}

pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: String,
    pub schema: Value,
    /// Whether the tool changes registry state — the switch `TAR_MCP_READ_ONLY` acts on.
    pub write: bool,
    pub gate: Gate,
    /// `annotations.readOnlyHint` / `destructiveHint` / `idempotentHint`, for clients that
    /// use them to decide whether to ask the user.
    pub idempotent: bool,
}

impl Tool {
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "inputSchema": self.schema,
            "annotations": {
                "title": self.title,
                "readOnlyHint": !self.write,
                // Nothing in this catalogue deletes or overwrites irrecoverably: the registry
                // keeps an audit log and tombstones rather than erasing, and no delete tool is
                // exposed at all.
                "destructiveHint": false,
                "idempotentHint": self.idempotent,
                "openWorldHint": true,
            }
        })
    }
}

// ------------------------------------------------------------- schema helpers

fn obj(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn s(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn s_enum(description: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "description": description, "enum": values })
}

fn arr_s(description: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": description })
}

fn int(description: &str) -> Value {
    json!({ "type": "integer", "description": description })
}

fn boolean(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

/// The IRI-valued parameter description, written once so it reads identically everywhere.
fn vocab_param(what: &str) -> Value {
    s(&format!(
        "{what} Each entry MUST be an IRI returned by `vocab_search` (or minted by \
         `register_artifact_type`). Do not assemble an ontology IRI from an identifier you \
         remember: the server checks every one against the vocabularies it bundles and refuses \
         the write if it is not a real term. Omit this field entirely if you have not looked one up."
    ))
}

fn vocab_array(what: &str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": format!(
            "{what} Every entry MUST be an IRI returned by `vocab_search` (or minted by \
             `register_artifact_type`). Never construct an ontology IRI from memory — the server \
             checks each one and refuses terms its bundled vocabularies do not contain. Omit rather than guess."
        )
    })
}

// -------------------------------------------------------- reusable sub-schemas

fn agent_schema(what: &str) -> Value {
    json!({
        "type": "object",
        "description": format!("{what} Give `iri` when you have a persistent identifier (an ORCID for a person, a ROR for an organisation) — that is what makes the same party the same node across federated registries. Otherwise give `name`. Omit the whole object if the repository does not say."),
        "properties": {
            "iri": s("ORCID, ROR or other persistent identifier IRI."),
            "name": s("Display name, when no identifier is known."),
            "kind": s_enum("person or organization.", &["person", "organization"]),
            "email": s("Contact email, only if the repository publishes one."),
            "homepage": s("URL."),
        },
        "additionalProperties": false,
    })
}

fn distribution_schema() -> Value {
    json!({
        "type": "object",
        "description": "One concrete way to get at the artifact. An artifact with no distribution is metadata about something nobody can fetch, which is sometimes the honest answer (set availability to metadata-only).",
        "properties": {
            "title": s("Human label for this distribution."),
            "access_url": s("Landing page or API endpoint for the data."),
            "download_url": s("Direct URL to the bytes."),
            "media_type": s("IANA media type, e.g. text/turtle, application/json."),
            "byte_size": int("Size in bytes, if known exactly. Do not estimate."),
            "checksum": json!({
                "type": "object",
                "description": "Only if you have actually computed or been given the digest.",
                "properties": { "algorithm": s("e.g. sha256"), "value": s("Hex digest.") },
                "required": ["algorithm", "value"],
                "additionalProperties": false,
            }),
            "conforms_to": vocab_param("Schema or shapes the bytes conform to."),
            "license": s("SPDX licence IRI, e.g. https://spdx.org/licenses/CC-BY-4.0. Only if stated."),
            "access_protocol": s_enum("Transport used to reach it.", &["https", "http", "s3", "sparql", "oci", "ipfs", "file"]),
            "auth_method": s_enum("How a caller authenticates to fetch it.", &["none", "apikey", "oauth2", "basic", "signed-url"]),
            "availability": s_enum("Who can get the bytes.", &["public", "restricted", "embargoed", "metadata-only"]),
            "access_request_url": s("Where to apply for access, when availability is restricted or embargoed."),
        },
        "additionalProperties": false,
    })
}

fn artifact_schema(with_iri: bool) -> Value {
    let mut props = json!({
        "title": s("What this artifact is, in a few words."),
        "description": s("Longer description. Omit if you would be paraphrasing nothing."),
        "conforms_to": vocab_param("The artifact's type — what kind of data this is."),
        "license": s("SPDX licence IRI. Only when the artifact actually states one."),
        "keywords": arr_s("Free-text keywords. These are not ontology terms and need no lookup."),
        "issued": s("ISO 8601 date or datetime the artifact was issued."),
        "publisher": agent_schema("Who published it."),
        "creators": json!({ "type": "array", "items": agent_schema("Who made it."), "description": "Creators, distinct from the publisher." }),
        "was_derived_from": arr_s("IRIs of artifacts this one was derived from. Foreign IRIs from other registries are welcome — that is how cross-registry lineage forms."),
        "distributions": json!({ "type": "array", "items": distribution_schema(), "description": "Concrete ways to get the bytes." }),
    });
    if with_iri {
        props.as_object_mut().unwrap().insert(
            "iri".into(),
            json!(s("IRI of an artifact that already exists (here or in another registry). Give this alone to reference it; give the descriptive fields instead to register a new one.")),
        );
    }
    json!({ "type": "object", "properties": props, "additionalProperties": false })
}

fn run_schema() -> Value {
    json!({
        "type": "object",
        "description": "The execution this advertisement is about. Advertisements are idempotent on (external_key, artifact, role), so a retried CI step does not duplicate lineage — always pass a stable external_key.",
        "properties": {
            "external_key": s("A stable key for this execution from the system that ran it, e.g. \"gh-actions/12345/attempt-1\". Pass the same value on the produced and consumed calls for one run."),
            "started_at": s("ISO 8601 datetime."),
            "ended_at": s("ISO 8601 datetime."),
            "status": s_enum("Outcome.", &["success", "failed", "running", "aborted"]),
            "release": s("IRI of the Release that was executed, if known."),
            "label": s("Human label for the run."),
        },
        "additionalProperties": false,
    })
}

fn capability_schema() -> Value {
    json!({
        "produces": vocab_array("Artifact types this software can emit."),
        "consumes": vocab_array("Artifact types this software can take as input."),
    })
}

// ------------------------------------------------------------- the catalogue

pub fn catalogue() -> Vec<Tool> {
    vec![
        // ---------------------------------------------------------- orientation
        Tool {
            name: "registry_info",
            title: "About this registry, and what you may do in it",
            description:
                "Call this first. Reports what this registry is (title, base IRI, how many software, \
                 instances, artifacts and runs it holds, which peers it federates with) and — from the \
                 credential on your request — who you are authenticated as, which Instance you act as if \
                 any, and which scopes and roles you hold. Use it to find out what you can write before \
                 you try to write it, rather than discovering it from a 403."
                    .into(),
            schema: obj(json!({}), &[]),
            write: false,
            gate: Gate::Read,
            idempotent: true,
        },
        // ---------------------------------------------------------- vocabulary
        Tool {
            name: "vocab_search",
            title: "Search the controlled vocabulary",
            description:
                "Search the controlled vocabulary by label, synonym or definition, over the bundled \
                 ontology plus every artifact type this registry has minted locally. Returns IRIs with \
                 their labels, definitions, source (edam | local | external) and a match score.\n\n\
                 THIS IS THE ONLY LEGITIMATE SOURCE OF AN ONTOLOGY IRI. Call it before every write that \
                 takes a topic or an artifact type, and use the IRI it returns verbatim. If nothing \
                 matches, the answer is either to omit the field or to mint a local type with \
                 `register_artifact_type` — never to write an IRI you assembled yourself.\n\n\
                 Search the words a person would use (\"sequence alignment\", \"validation report\", \
                 \"proteomics\") rather than an identifier. Try two or three phrasings before concluding \
                 there is no term."
                    .into(),
            schema: obj(
                json!({
                    "q": s("What to look for, in plain words. At least two characters."),
                    "branch": s_enum(
                        "Restrict the branch: `topic` is what a piece of software is *about* (use for software topics); `data` is what an artifact *is* (use for artifact types). Omit to search everything, including this registry's locally minted types.",
                        &["topic", "data"],
                    ),
                    "limit": int("Maximum hits, 1–100. Default 20."),
                }),
                &["q"],
            ),
            write: false,
            gate: Gate::Read,
            idempotent: true,
        },
        Tool {
            name: "vocab_resolve",
            title: "Check that vocabulary IRIs are real, and get their labels",
            description:
                "Resolve IRIs to their labels and definitions. Use it to verify an IRI you were given by \
                 a human or found in a repository file before writing it into a record: an entry that \
                 comes back without a label is one this registry cannot resolve, which for an \
                 term from a vocabulary this registry bundles or minted means it is not real, and the write will be refused."
                    .into(),
            schema: obj(json!({ "iris": arr_s("IRIs to resolve, up to 100.") }), &["iris"]),
            write: false,
            gate: Gate::Read,
            idempotent: true,
        },
        Tool {
            name: "list_enumerations",
            title: "The registry's closed value sets",
            description:
                "Every field in this registry whose value is drawn from a fixed list — software kinds, \
                 maturity, artifact and release availability, distribution access protocols and auth \
                 methods, run statuses, credential scopes, lineage directions, syncable fields. Returns \
                 each set with its allowed values and what they mean.\n\n\
                 Read this instead of recalling the values. The registry validates against these sets \
                 with SHACL and rejects anything outside them, and the sets are small enough that one \
                 call replaces all guessing."
                    .into(),
            schema: obj(json!({}), &[]),
            write: false,
            gate: Gate::Read,
            idempotent: true,
        },
        Tool {
            name: "register_artifact_type",
            title: "Mint a local artifact type when the vocabulary has no term",
            description: format!(
                "Create a registry-local `skos:Concept` to serve as an artifact type, for the case where \
                 `vocab_search` genuinely finds nothing: a SHACL shapes graph, an RML mapping, a \
                 hash-chained patch log — real artifact kinds no bundled vocabulary names. The IRI it returns is \
                 then a legitimate value for `conforms_to`, `produces` and `consumes`.\n\n\
                 This exists so that \"the vocabulary has no word for this\" has an honest answer that is not a \
                 fabricated ontology IRI. Search first; mint only when the search really came back empty, and \
                 re-registering the same `slug` updates the label rather than creating a duplicate, so a \
                 type IRI stays a stable name.\n\n{NO_INVENTION}"
            ),
            schema: obj(
                json!({
                    "label": s("Short name for the type, as a person would say it."),
                    "definition": s("What an artifact of this type is. Optional but strongly worth writing."),
                    "default_media_type": s("The IANA media type these artifacts usually have, if there is one."),
                    "slug": s("Readable last path segment, e.g. `shacl-shapes-graph`. Recommended: type IRIs get quoted by hand in documentation and mapping files."),
                }),
                &["label"],
            ),
            write: true,
            gate: Gate::Curator,
            idempotent: true,
        },
        // ---------------------------------------------------------------- read
        Tool {
            name: "search_registry",
            title: "Search across software, instances, artifacts and runs",
            description:
                "Free-text search over every record type at once, returning typed hits with their IRIs. \
                 Use it to find out whether something is already registered before you register it \
                 again — duplicate software records are the most common way a registry rots. Set \
                 `federated` to also ask this registry's peers, which is slower and returns records \
                 this registry is not authoritative for."
                    .into(),
            schema: obj(
                json!({
                    "q": s("Search terms."),
                    "type": s_enum("Restrict to one record type.", &["software", "instance", "artifact", "run"]),
                    "federated": boolean("Also query peer registries. Default false."),
                    "limit": int("Maximum hits. Default 25."),
                }),
                &["q"],
            ),
            write: false,
            gate: Gate::Read,
            idempotent: true,
        },
        Tool {
            name: "list_records",
            title: "List and filter records of one kind",
            description:
                "Paginated, filtered listing of one record kind. Use the filters rather than fetching \
                 everything: `software` filters by licence, publisher, topic, keyword, kind and by \
                 what its capability produces or consumes; `instance` by software, release, operator and \
                 health; `artifact` by type, licence, availability, and the instance, software or run \
                 involved; `run` by instance, software and status; `release` lists the releases of one \
                 software and requires `software`.\n\n\
                 Paginate with `cursor`, which each response returns as `next_cursor` when more remain."
                    .into(),
            schema: obj(
                json!({
                    "kind": s_enum("Which records to list.", &["software", "instance", "artifact", "run", "release", "type"]),
                    "q": s("Free-text filter on names and descriptions."),
                    "software": s("Software id or IRI. Required when kind is `release`."),
                    "instance": s("Instance id or IRI."),
                    "release": s("Release id or IRI."),
                    "run": s("Run id or IRI."),
                    "conforms_to": s("Artifact type IRI, for kind=artifact. Must come from `vocab_search`."),
                    "edam_topic": s("Topic IRI, for kind=software. Must come from `vocab_search` (branch=topic)."),
                    "produces": s("Artifact type IRI the software's capability emits."),
                    "consumes": s("Artifact type IRI the software's capability accepts."),
                    "license": s("Licence IRI."),
                    "publisher": s("Publisher IRI."),
                    "keyword": s("Exact keyword."),
                    "kind_filter": s("For kind=software: one of the software kinds from `list_enumerations`."),
                    "availability": s("For kind=artifact: public | restricted | embargoed | metadata-only."),
                    "status": s("For kind=run: success | failed | running | aborted. For kind=instance: health up | down | unknown."),
                    "registry": s("`local` for records this registry minted, or a peer base IRI for one peer's."),
                    "cursor": s("Opaque cursor from a previous response's `next_cursor`."),
                    "limit": int("Page size, 1–200. Default 25."),
                }),
                &["kind"],
            ),
            write: false,
            gate: Gate::Read,
            idempotent: true,
        },
        Tool {
            name: "get_record",
            title: "Read one record in full",
            description:
                "Fetch a single record by id or by full IRI: a software record with its releases, \
                 capability, sync status and instance counts; an instance with its endpoint, health and \
                 credential binding; an artifact with its distributions and provenance; a run with its \
                 inputs and outputs; an artifact type with its definition.\n\n\
                 Read the existing record before updating it. `update_software` replaces the fields you \
                 send, so writing an update without first reading what is there is how curated prose \
                 gets destroyed."
                    .into(),
            schema: obj(
                json!({
                    "kind": s_enum("What kind of record.", &["software", "instance", "artifact", "run", "type"]),
                    "id": s("The record's local id (the last path segment of its IRI) or its full IRI."),
                }),
                &["kind", "id"],
            ),
            write: false,
            gate: Gate::Read,
            idempotent: true,
        },
        Tool {
            name: "find_capable_software",
            title: "Which software can produce or consume this artifact type",
            description:
                "Capability matchmaking: given an artifact type IRI, find the software and deployments \
                 that declare they can produce it, consume it, or both. This answers \"what could process \
                 the output of X?\" before any run has ever happened, which is what makes a declared \
                 capability worth writing down. Give at least one of `produces` or `consumes`."
                    .into(),
            schema: obj(
                json!({
                    "produces": vocab_param("Find things that can emit this artifact type."),
                    "consumes": vocab_param("Find things that can accept this artifact type."),
                }),
                &[],
            ),
            write: false,
            gate: Gate::Read,
            idempotent: true,
        },
        Tool {
            name: "get_artifact_lineage",
            title: "Trace what an artifact came from and fed into",
            description:
                "Walk the provenance graph around one artifact: upstream to what it was derived from, \
                 downstream to what was derived from it, or both, to a bounded depth. Lineage crosses \
                 registry boundaries — a node may be a stub for an artifact another registry is \
                 authoritative for."
                    .into(),
            schema: obj(
                json!({
                    "id": s("Artifact id or full IRI."),
                    "depth": int("How many hops, 1–6. Default 1."),
                    "direction": s_enum("Which way to walk.", &["up", "down", "both"]),
                }),
                &["id"],
            ),
            write: false,
            gate: Gate::Read,
            idempotent: true,
        },
        // --------------------------------------------------------------- write
        Tool {
            name: "register_software",
            title: "Register a piece of software",
            description: format!(
                "Create a Software record — the catalogue entry for a program, distinct from any \
                 deployment of it. Search first with `search_registry`: registering something that is \
                 already there is worse than not registering it.\n\n\
                 What to fill in from a repository you can read: `name`, `code_repository`, `homepage`, \
                 `description` and `readme` from the README, `license` from the LICENSE file or package \
                 metadata (the SPDX IRI, e.g. https://spdx.org/licenses/Apache-2.0), `kinds` from what \
                 the project actually ships, `keywords` from its own stated keywords. `edam_topics` \
                 requires `vocab_search` — always.\n\n\
                 What NOT to fill in: a licence the repository does not state; a maturity you inferred \
                 from commit dates; a tagline you composed when the project has one; topics that \
                 sound right. Leave them out. Someone will fill them in later, and an empty field says \
                 so while a wrong one does not.\n\n{NO_INVENTION}"
            ),
            schema: obj(
                json!({
                    "name": s("The software's own name, spelled as the project spells it."),
                    "tagline": s("The project's own one-line description, quoted not composed. Omit if it has none."),
                    "description": s("Longer description, from the repository."),
                    "homepage": s("Project homepage URL."),
                    "code_repository": s("Source repository URL."),
                    "documentation": s("Documentation URL."),
                    "download_url": s("Releases, downloads or package page. For software that cannot be hosted this is the most important link on the record."),
                    "readme": s("The full README as Markdown, verbatim."),
                    "readme_base_url": s("Raw content root the README's relative links and images resolve against, e.g. https://raw.githubusercontent.com/owner/repo/main/."),
                    "image": s("Logo or hero image URL. The registry stores no bytes, only the pointer."),
                    "license": s("SPDX licence IRI, e.g. https://spdx.org/licenses/MIT. ONLY when the repository actually states a licence — omit it otherwise, never guess from convention."),
                    "kinds": json!({ "type": "array", "items": { "type": "string", "enum": ["service", "library", "cli", "desktop", "workflow"] }, "description": "What the software is. A set, because one program is routinely several of these at once." }),
                    "maturity": s_enum("repostatus.org development status, only if the project declares one.", &["concept", "wip", "active", "inactive", "unsupported", "suspended", "abandoned", "moved"]),
                    "deployable": boolean("False when the software cannot be hosted at an endpoint at all (a desktop app, a CLI). Defaults to true; set it false and the registry refuses an endpoint on any instance of it."),
                    "edam_topics": vocab_array("Topics this software is about, from `vocab_search` with branch=topic. Named `edam_topics` for API compatibility; the topic branch is EuroSciVoc, not EDAM."),
                    "keywords": arr_s("The project's own keywords. Free text, no lookup needed."),
                    "publisher": agent_schema("The organisation or person that publishes it."),
                    "contact": agent_schema("Who to ask about it."),
                    "publications": arr_s("DOIs or URLs of papers describing it."),
                    "capability": json!({ "type": "object", "properties": capability_schema(), "additionalProperties": false, "description": "Optionally declare produces/consumes inline instead of a second call to `declare_capability`." }),
                }),
                &["name"],
            ),
            write: true,
            gate: Gate::Curator,
            idempotent: false,
        },
        Tool {
            name: "update_software",
            title: "Update a software record",
            description: format!(
                "Replace fields on an existing Software record. READ THE RECORD FIRST with `get_record`: \
                 this is a replace, so a field you omit is cleared, and sending a partial body over a \
                 curated record destroys what a person wrote.\n\n\
                 The safe pattern is: `get_record` → change the specific fields you have evidence for → \
                 send the whole object back.\n\n{NO_INVENTION}"
            ),
            schema: {
                let mut base = obj(
                    json!({
                        "id": s("Software id or full IRI."),
                        "name": s("Name. Required — send the existing one if you are not changing it."),
                        "tagline": s("One-line description."),
                        "description": s("Longer description."),
                        "homepage": s("Homepage URL."),
                        "code_repository": s("Source repository URL."),
                        "documentation": s("Documentation URL."),
                        "download_url": s("Downloads or package page."),
                        "readme": s("Full README as Markdown."),
                        "readme_base_url": s("Raw content root for the README's relative links."),
                        "image": s("Logo or hero image URL."),
                        "license": s("SPDX licence IRI, only when stated."),
                        "kinds": json!({ "type": "array", "items": { "type": "string", "enum": ["service", "library", "cli", "desktop", "workflow"] }, "description": "What the software is." }),
                        "maturity": s_enum("repostatus.org status.", &["concept", "wip", "active", "inactive", "unsupported", "suspended", "abandoned", "moved"]),
                        "deployable": boolean("Whether it can be hosted at an endpoint."),
                        "edam_topics": vocab_array("Topics, from `vocab_search` with branch=topic."),
                        "keywords": arr_s("Keywords."),
                        "publisher": agent_schema("Publisher."),
                        "contact": agent_schema("Contact."),
                        "publications": arr_s("DOIs or URLs."),
                    }),
                    &["id", "name"],
                );
                base.as_object_mut().unwrap().insert("additionalProperties".into(), json!(false));
                base
            },
            write: true,
            gate: Gate::Curator,
            idempotent: true,
        },
        Tool {
            name: "add_release",
            title: "Record a release of a software",
            description: format!(
                "Add a versioned Release to a Software record: the version string, when it was published, \
                 the container image and digest if it ships one, the install command, and the downloadable \
                 files. Take these from the forge's release page, the tag, or the package registry — a \
                 version number is a fact, not an estimate, and a digest you did not read is worthless.\n\n\
                 A release may narrow the software's capability if this version produces or consumes \
                 something different.\n\n{NO_INVENTION}"
            ),
            schema: obj(
                json!({
                    "software": s("Software id or full IRI this release belongs to."),
                    "version": s("Version string exactly as the project publishes it, e.g. `1.4.2` or `v1.4.2`."),
                    "date_published": s("ISO 8601 date the release was published."),
                    "container_image": s("Full image reference, e.g. ghcr.io/owner/tool:1.4.2."),
                    "image_digest": s("The image digest, e.g. sha256:… — only if you have actually read it."),
                    "changelog": s("Release notes, verbatim from the release."),
                    "install_command": s("A one-line install command, e.g. `pip install tool==1.4.2`."),
                    "downloads": json!({
                        "type": "array",
                        "description": "Downloadable files this release ships. A packaged application has several — a Windows installer, a macOS bundle, a Linux package.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "url": s("Direct download URL."),
                                "label": s("What this file is."),
                                "platform": s("Free text, as the project says it: \"Windows\", \"macOS\", \"Linux\"."),
                                "byte_size": int("Exact size in bytes, if known."),
                                "availability": s_enum("Who can download it.", &["public", "restricted", "embargoed", "metadata-only"]),
                            },
                            "required": ["url"],
                            "additionalProperties": false,
                        }
                    }),
                    "capability": json!({ "type": "object", "properties": capability_schema(), "additionalProperties": false, "description": "Capability specific to this version, if it differs from the software's." }),
                }),
                &["software", "version"],
            ),
            write: true,
            gate: Gate::Curator,
            idempotent: false,
        },
        Tool {
            name: "declare_capability",
            title: "Declare what a software or deployment produces and consumes",
            description: format!(
                "Say which artifact types a Software (or one Instance of it) can emit and accept. This is \
                 what `find_capable_software` answers from, and it is answerable before any run has \
                 happened — which is the whole point of declaring it separately from lineage.\n\n\
                 Declaring on an Instance narrows what the Software declared: a deployment that only \
                 accepts one of the formats its software supports should say so.\n\n\
                 This replaces the whole declaration, so send the complete produces and consumes sets, \
                 not just additions.\n\n{NO_INVENTION}"
            ),
            schema: obj(
                json!({
                    "target": s_enum("Whether the declaration is on the software as a whole or on one deployment.", &["software", "instance"]),
                    "id": s("Software or Instance id, or full IRI."),
                    "produces": vocab_array("Artifact types it emits."),
                    "consumes": vocab_array("Artifact types it accepts."),
                }),
                &["target", "id"],
            ),
            write: true,
            gate: Gate::Curator,
            idempotent: true,
        },
        Tool {
            name: "register_instance",
            title: "Register a deployment or installation",
            description: format!(
                "Create an Instance: one running deployment of a Software, or one installation of \
                 software that cannot be hosted. An Instance is what runs are attributed to, so \
                 desktop applications and CLIs have them too — they simply have no `endpoint_url`.\n\n\
                 `oidc_client_id` binds a workload identity to this deployment: the tool authenticates to \
                 its own identity provider and presents that token, and the registry maps the client id \
                 back to this Instance. The registry then never stores a secret for it. Set it only to a \
                 client id you were actually given.\n\n{NO_INVENTION}"
            ),
            schema: obj(
                json!({
                    "label": s("What this deployment is called, e.g. \"UM production\"."),
                    "software": s("Software id or IRI this deploys."),
                    "release": s("Release id or IRI this deployment runs, if known."),
                    "endpoint_url": s("Where the deployment is reachable. Omit for an installation of non-hostable software; the registry refuses one if the software is marked not deployable."),
                    "endpoint_description": s("OpenAPI or other API description URL."),
                    "description": s("What this deployment is for."),
                    "operator": agent_schema("The organisation running it."),
                    "availability": s_enum("Who may use it.", &["public", "restricted", "embargoed", "metadata-only"]),
                    "jurisdiction": s("Where it runs, when that matters for data governance."),
                    "oidc_client_id": s("OIDC client id, Kubernetes ServiceAccount subject or GitHub Actions subject allowed to advertise as this deployment."),
                    "oidc_issuer": s("Issuer of that workload token, if not the registry's default."),
                    "allowed_scopes": json!({ "type": "array", "items": { "type": "string", "enum": crate::auth::ALL_SCOPES }, "description": "Scopes granted to that workload identity when its own token carries none of ours." }),
                    "capability": json!({ "type": "object", "properties": capability_schema(), "additionalProperties": false, "description": "Narrow the software's capability for this deployment." }),
                }),
                &["label"],
            ),
            write: true,
            gate: Gate::CuratorOrScope(SCOPE_REGISTER_INSTANCE),
            idempotent: false,
        },
        Tool {
            name: "advertise_produced",
            title: "Advertise artifacts a run produced",
            description: format!(
                "Report that the deployment your credential acts as performed a run and produced these \
                 artifacts. This is the primary way lineage enters the registry, and it is meant to be \
                 called by the tool that did the work, from its own CI or runtime.\n\n\
                 The Instance is taken from your credential and never from the request — a deployment can \
                 only advertise runs in which it is itself the agent. Advertisement is idempotent on \
                 (external_key, artifact, role), so pass a stable `run.external_key` and a retried CI \
                 step will not duplicate the lineage.\n\n\
                 Report what the run actually emitted: real URLs, real digests you computed, real sizes. \
                 An artifact nobody can fetch is honestly described with `availability: metadata-only` \
                 rather than an invented download URL.\n\n{NO_INVENTION}"
            ),
            schema: obj(
                json!({
                    "run": run_schema(),
                    "artifacts": json!({ "type": "array", "items": artifact_schema(false), "description": "The artifacts this run produced." }),
                }),
                &["run", "artifacts"],
            ),
            write: true,
            gate: Gate::InstanceScope(SCOPE_ADVERTISE_PRODUCE),
            idempotent: true,
        },
        Tool {
            name: "advertise_consumed",
            title: "Advertise artifacts a run consumed",
            description: format!(
                "Report the inputs of a run your deployment performed. Each entry is either a bare `iri` \
                 referencing an artifact that already exists — including one another registry is \
                 authoritative for, which is how cross-registry lineage forms with no coordination — or a \
                 full description that registers the input as it records it.\n\n\
                 Pass the same `run.external_key` you used for `advertise_produced` and both halves \
                 attach to one run.\n\n{NO_INVENTION}"
            ),
            schema: obj(
                json!({
                    "run": run_schema(),
                    "artifacts": json!({ "type": "array", "items": artifact_schema(true), "description": "The inputs. Reference an existing artifact by `iri`, or describe a new one." }),
                }),
                &["run", "artifacts"],
            ),
            write: true,
            gate: Gate::InstanceScope(SCOPE_ADVERTISE_CONSUME),
            idempotent: true,
        },
    ]
}

/// The tools this principal may actually call, in a deterministic order.
///
/// `2026-07-28/server/tools` allows the set to vary by the authorization on the request, and
/// it should: a model shown a tool it cannot use will call it, read a refusal, and try again.
pub fn visible(principal: &Principal, read_only: bool) -> Vec<Tool> {
    catalogue()
        .into_iter()
        .filter(|t| !(read_only && t.write))
        .filter(|t| t.gate.allows(principal))
        .collect()
}

pub fn find(name: &str) -> Option<Tool> {
    catalogue().into_iter().find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{CredentialKind, Role};
    use std::collections::BTreeSet;

    fn principal(scopes: &[&str], roles: &[Role], instance: bool) -> Principal {
        Principal {
            credential: CredentialKind::LocalToken,
            instance_iri: instance.then(|| "https://reg.test/instance/1".to_string()),
            subject: "test".into(),
            display_name: None,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            roles: roles.iter().copied().collect::<BTreeSet<_>>(),
            issuer: None,
        }
    }

    #[test]
    fn every_tool_has_a_valid_object_schema_and_a_substantial_description() {
        for t in catalogue() {
            assert_eq!(t.schema["type"], "object", "{} schema must be an object", t.name);
            assert!(t.schema.get("properties").is_some(), "{} needs properties", t.name);
            assert!(
                t.description.len() > 120,
                "{} description is too thin to be a contract with the model",
                t.name
            );
            assert!(t.name.len() <= 128 && t.name.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)));
        }
    }

    #[test]
    fn tool_names_are_unique() {
        let mut seen = BTreeSet::new();
        for t in catalogue() {
            assert!(seen.insert(t.name.to_string()), "duplicate tool {}", t.name);
        }
    }

    #[test]
    fn every_write_tool_repeats_the_no_invention_contract() {
        for t in catalogue().into_iter().filter(|t| t.write) {
            assert!(t.description.contains("DO NOT INVENT VALUES"), "{} must carry the contract", t.name);
        }
    }

    #[test]
    fn anonymous_sees_nothing() {
        assert!(visible(&Principal::anonymous(), false).is_empty());
    }

    #[test]
    fn a_reader_sees_reads_but_no_writes() {
        let p = principal(&[], &[Role::Reader], false);
        let names: Vec<_> = visible(&p, false).into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"vocab_search"));
        assert!(names.contains(&"registry_info"));
        assert!(!names.contains(&"register_software"));
        assert!(!names.contains(&"advertise_produced"));
    }

    #[test]
    fn a_curator_sees_the_curation_tools_but_not_advertisement() {
        let p = principal(&[], &[Role::Curator], false);
        let names: Vec<_> = visible(&p, false).into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"register_software"));
        assert!(names.contains(&"register_artifact_type"));
        // Advertisement needs a credential that *is* a deployment, which a person's is not.
        assert!(!names.contains(&"advertise_produced"));
    }

    #[test]
    fn an_instance_token_sees_only_what_its_scopes_allow() {
        let p = principal(&[SCOPE_ADVERTISE_PRODUCE], &[], true);
        let names: Vec<_> = visible(&p, false).into_iter().map(|t| t.name).collect();
        assert!(names.contains(&"advertise_produced"));
        assert!(!names.contains(&"advertise_consumed"));
        assert!(!names.contains(&"register_software"));
    }

    #[test]
    fn read_only_mode_hides_every_write_tool() {
        let p = principal(&[], &[Role::Admin], true);
        let all = visible(&p, false);
        let ro = visible(&p, true);
        assert!(all.iter().any(|t| t.write));
        assert!(ro.iter().all(|t| !t.write));
        assert!(ro.iter().any(|t| t.name == "vocab_search"));
    }

    #[test]
    fn the_advertise_refusal_explains_the_instance_rule() {
        let p = principal(&[SCOPE_ADVERTISE_PRODUCE], &[], false);
        let msg = Gate::InstanceScope(SCOPE_ADVERTISE_PRODUCE).refusal(&p);
        assert!(msg.contains("Instance"), "{msg}");
    }
}
