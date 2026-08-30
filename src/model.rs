//! JSON API shapes (spec §7). These are the "flattened developer-facing shape" served for
//! `Accept: application/json`; the same records serialise as Turtle or JSON-LD straight from
//! the graph.

use serde::{Deserialize, Serialize};

fn is_false(b: &bool) -> bool {
    !*b
}

/// Local vs foreign, on every record and every list row (handoff §6.1, §7 "stale peer data").
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Origin {
    /// `local` | `peer`
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_base_iri: Option<String>,
    /// When the stub was last refreshed from its home registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve_status: Option<String>,
}

impl Origin {
    pub fn local() -> Self {
        Self { kind: "local".into(), ..Default::default() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentRef {
    pub iri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentIn {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `person` | `organization`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// ORCID or ROR IRI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

/// A typed reference with a label, so a chip can render without a second request
/// (handoff §6.1 `ArtifactTypeChip`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TypeRef {
    pub iri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    /// `edam` | `local` | `external`
    pub source: String,
}

// ------------------------------------------------------------------ software

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SoftwareIn {
    pub name: String,
    #[serde(default)]
    pub tagline: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub code_repository: Option<String>,
    #[serde(default)]
    pub documentation: Option<String>,
    /// Where to get it: a releases page, a download page, a package listing. For software that
    /// cannot be hosted this is the most important link on the record, since "how do I run it"
    /// has no endpoint to answer it.
    #[serde(default)]
    pub download_url: Option<String>,
    /// Logo or hero image. A pointer, never bytes: the registry stores no images (D1).
    #[serde(default)]
    pub image: Option<String>,
    /// Further screenshots, in display order.
    #[serde(default)]
    pub screenshots: Vec<String>,
    /// The full README, as Markdown. `description` is the short line that goes in a list;
    /// this is the long-form page, rendered with its images, the way a forge shows one.
    #[serde(default)]
    pub readme: Option<String>,
    /// Base URL that relative links and images in the README resolve against — normally the
    /// repository's raw content root. Without it, `![](docs/images/x.png)` resolves nowhere.
    #[serde(default)]
    pub readme_base_url: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    /// What the software *is*, as a set: `service`, `library`, `cli`, `desktop`, `workflow`.
    ///
    /// A set rather than one value, because one program is routinely several of these at once
    /// — a tool with a desktop build and a hosted deployment is both, and forcing a choice
    /// makes the record lie about one of them. `schema:applicationCategory`, which carries
    /// this, is free text and unrestricted in cardinality, so the set is the vocabulary's own
    /// shape; only the allowed values are ours.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Accepted for compatibility with single-valued callers; folded into `kinds`.
    #[serde(default, skip_serializing)]
    pub kind: Option<String>,
    #[serde(default)]
    pub maturity: Option<String>,
    /// Whether this software can be hosted at an endpoint at all.
    ///
    /// A desktop application or a CLI runs on someone's machine; it still has Instances —
    /// installations — because runs have to be attributable to something, but none of them can
    /// have an `endpoint_url`, and calling them "deployments" misdescribes them. Defaults to
    /// true; set it false and the registry refuses an endpoint on any instance of it.
    #[serde(default)]
    pub deployable: Option<bool>,
    #[serde(default)]
    pub edam_topics: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub publisher: Option<AgentIn>,
    #[serde(default)]
    pub contact: Option<AgentIn>,
    #[serde(default)]
    pub publications: Vec<String>,
    /// Optional capability declared inline at registration time.
    #[serde(default)]
    pub capability: Option<CapabilityIn>,
    /// Keep parts of this record in step with a source repository.
    #[serde(default)]
    pub sync: Option<SyncIn>,
}

/// Which fields a forge owns, and where they come from.
///
/// The point of naming the fields is that sync must never silently overwrite something a
/// person wrote. A field listed here is *managed*: sync overwrites it, and the UI says so. A
/// field not listed is the curator's, and sync leaves it alone even when the repository has an
/// obvious value for it. Anything else turns "connect the repo" into "lose my edits".
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SyncIn {
    /// Only `github` today. Named rather than assumed, because GitLab is the obvious next one
    /// and a boolean `github: true` would have to be replaced rather than extended.
    #[serde(default = "default_forge")]
    pub source: String,
    /// `owner/name`.
    pub repo: String,
    /// Any of: `tagline`, `description`, `readme`, `homepage`, `license`, `keywords`,
    /// `maturity`, `releases`, `image`.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default = "default_true_sync")]
    pub enabled: bool,
}

fn default_forge() -> String {
    "github".to_string()
}
fn default_true_sync() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncStatus {
    pub source: String,
    pub repo: String,
    pub fields: Vec<String>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<String>,
    /// `ok` | `error` | `never`
    pub last_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// What the last run actually changed, so a sync is auditable rather than magic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_changed: Vec<String>,
}

/// Fields a forge can supply. Anything outside this list is always the curator's.
pub const SYNCABLE_FIELDS: [&str; 9] = [
    "tagline", "description", "readme", "homepage", "license", "keywords", "maturity",
    "releases", "image",
];

impl SoftwareIn {
    /// The declared kinds, accepting either the set or the older single value, de-duplicated
    /// and order-preserving.
    pub fn resolved_kinds(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for k in self.kinds.iter().chain(self.kind.iter()) {
            let k = k.trim();
            if !k.is_empty() && !out.iter().any(|e| e == k) {
                out.push(k.to_string());
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Software {
    pub iri: String,
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tagline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub screenshots: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readme_base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub kinds: Vec<String>,
    /// The first kind, for callers and UI that want one word. Absent when none is declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maturity: Option<String>,
    /// False when the software cannot be hosted — see `SoftwareIn::deployable`.
    ///
    /// Independent of `kinds`: a program can ship a desktop build *and* be hosted, and it is
    /// then both `desktop` and `service` and deployable. What this flag governs is only
    /// whether an instance of it may carry an endpoint.
    pub deployable: bool,
    pub edam_topics: Vec<TypeRef>,
    pub keywords: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<AgentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<AgentRef>,
    pub publications: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_release: Option<Release>,
    pub instance_count: i64,
    pub release_count: i64,
    /// Runs across every Instance of this Software in the last 30 days (handoff §4.1).
    pub runs_30d: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tombstoned: bool,
}

// ------------------------------------------------------------------- release

/// One downloadable file of a release. A packaged application ships several — a Windows
/// installer, a macOS bundle, a Linux package — and `container_image` cannot express any of
/// them.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DownloadIn {
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
    /// Free text, as `schema:operatingSystem` is: "Windows", "macOS", "Linux".
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub byte_size: Option<i64>,
    /// `public` | `restricted` | `embargoed` | `metadata-only`. Defaults to public: a release
    /// asset you can name a URL for is normally one anyone can fetch.
    #[serde(default)]
    pub availability: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReleaseIn {
    pub version: String,
    #[serde(default)]
    pub date_published: Option<String>,
    #[serde(default)]
    pub container_image: Option<String>,
    #[serde(default)]
    pub image_digest: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
    #[serde(default)]
    pub install_command: Option<String>,
    #[serde(default)]
    pub downloads: Vec<DownloadIn>,
    #[serde(default)]
    pub capability: Option<CapabilityIn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Release {
    pub iri: String,
    pub id: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_published: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<DownloadIn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    pub origin: Origin,
}

// ---------------------------------------------------------------- capability

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapabilityIn {
    #[serde(default)]
    pub produces: Vec<String>,
    #[serde(default)]
    pub consumes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Capability {
    pub iri: String,
    pub produces: Vec<TypeRef>,
    pub consumes: Vec<TypeRef>,
    /// Which layer declared it: `software` | `release` | `instance`.
    pub declared_at: String,
}

// ------------------------------------------------------------------ instance

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstanceIn {
    pub label: String,
    #[serde(default)]
    pub software: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub endpoint_description: Option<String>,
    #[serde(default)]
    pub operator: Option<AgentIn>,
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default)]
    pub jurisdiction: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Workload identity: the OIDC client id (Keycloak), Kubernetes ServiceAccount subject,
    /// or GitHub Actions subject allowed to advertise as this deployment.
    #[serde(default)]
    pub oidc_client_id: Option<String>,
    #[serde(default)]
    pub oidc_issuer: Option<String>,
    /// Scopes granted to that workload identity when its token carries none of ours.
    #[serde(default)]
    pub allowed_scopes: Vec<String>,
    #[serde(default)]
    pub capability: Option<CapabilityIn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Instance {
    pub iri: String,
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_version: Option<String>,
    /// True when the Software has a newer Release than this Instance runs (handoff §5.2).
    #[serde(default, skip_serializing_if = "is_false")]
    pub outdated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator: Option<AgentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<String>,
    /// `up` | `down` | `unknown`
    pub health: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    pub runs_30d: i64,
    pub failures_30d: i64,
    pub artifact_count: i64,
    /// Credential binding, so the UI can say how this deployment authenticates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oidc_client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oidc_issuer: Option<String>,
    #[serde(default)]
    pub allowed_scopes: Vec<String>,
    pub token_count: i64,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tombstoned: bool,
}

// ------------------------------------------------------------------ artifact

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChecksumIn {
    pub algorithm: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DistributionIn {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub access_url: Option<String>,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub byte_size: Option<i64>,
    #[serde(default)]
    pub checksum: Option<ChecksumIn>,
    #[serde(default)]
    pub conforms_to: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub access_service: Option<String>,
    /// `https` | `s3` | `sparql` | `oci` | `ipfs` | `file`
    #[serde(default)]
    pub access_protocol: Option<String>,
    /// `none` | `apikey` | `oauth2` | `basic` | `signed-url`
    #[serde(default)]
    pub auth_method: Option<String>,
    /// `public` | `restricted` | `embargoed` | `metadata-only`
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default)]
    pub access_request_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Distribution {
    pub iri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<ChecksumIn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conforms_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<String>,
    pub availability: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_request_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArtifactIn {
    #[serde(default)]
    pub iri: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub conforms_to: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub issued: Option<String>,
    #[serde(default)]
    pub publisher: Option<AgentIn>,
    /// Who made this. Distinct from `publisher` (who released it) and from the credential the
    /// advertisement arrived on (which the registry records itself as `prov:wasAttributedTo`
    /// and no caller can forge). A person here can be an ORCID, and then the same researcher
    /// is the same node across every registry that federates with this one.
    #[serde(default)]
    pub creators: Vec<AgentIn>,
    #[serde(default)]
    pub contributors: Vec<AgentIn>,
    /// Who to ask about it, when that is not the publisher.
    #[serde(default)]
    pub contact: Option<AgentIn>,
    #[serde(default)]
    pub modified: Option<String>,
    /// The artifact's own version string, distinct from the version *series* it belongs to.
    #[serde(default)]
    pub version: Option<String>,
    /// A human-facing page about it, as opposed to a distribution that yields the bytes.
    #[serde(default)]
    pub landing_page: Option<String>,
    #[serde(default)]
    pub documentation: Option<String>,
    /// Where it came from, when that is not a registered artifact — a paper, a survey, a
    /// database export that predates this registry.
    #[serde(default)]
    pub source: Option<String>,
    /// BCP 47, e.g. `en`, `nl`.
    #[serde(default)]
    pub language: Vec<String>,
    /// Free text for now: a place name or a geographic IRI.
    #[serde(default)]
    pub spatial: Option<String>,
    #[serde(default)]
    pub temporal_start: Option<String>,
    #[serde(default)]
    pub temporal_end: Option<String>,
    #[serde(default)]
    pub was_derived_from: Vec<String>,
    #[serde(default)]
    pub was_revision_of: Option<String>,
    /// Version-series concept IRI (D10). Omit to start a new series.
    #[serde(default)]
    pub is_version_of: Option<String>,
    #[serde(default)]
    pub external_key: Option<String>,
    #[serde(default)]
    pub distributions: Vec<DistributionIn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Artifact {
    pub iri: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conforms_to: Option<TypeRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub keywords: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issued: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<AgentRef>,
    pub creators: Vec<AgentRef>,
    pub contributors: Vec<AgentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<AgentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing_page: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub language: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spatial: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temporal_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temporal_end: Option<String>,
    /// The credential the advertisement arrived on. Recorded by the registry, never supplied
    /// by the caller — this is what makes attribution trustworthy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributed_to: Option<String>,
    pub distributions: Vec<Distribution>,
    /// The strongest availability across distributions; `metadata-only` when there are none.
    pub availability: String,
    pub was_derived_from: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub was_revision_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_version_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub was_generated_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_by_run: Option<RunSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_key: Option<String>,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tombstoned: bool,
}

// ----------------------------------------------------------------------- run

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunIn {
    #[serde(default)]
    pub external_key: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
    /// `success` | `failed` | `running` | `aborted`
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunSummary {
    pub iri: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software_name: Option<String>,
    pub used_count: i64,
    pub generated_count: i64,
    pub origin: Origin,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Run {
    #[serde(flatten)]
    pub summary: RunSummary,
    pub used: Vec<ArtifactRef>,
    pub generated: Vec<ArtifactRef>,
    /// Everything an OpenLineage event carried that our model does not name (spec §7.6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openlineage_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArtifactRef {
    pub iri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conforms_to: Option<TypeRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    pub origin: Origin,
    /// True when the record is a foreign IRI we have not resolved yet (spec §9.3).
    #[serde(default, skip_serializing_if = "is_false")]
    pub unresolved: bool,
}

// ------------------------------------------------------------------ envelope

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
    /// Keyset cursor — pass back as `?cursor=` (handoff §5.1: not page numbers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facets: Vec<Facet>,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, total: i64, next_cursor: Option<String>) -> Self {
        Self { items, total, next_cursor, facets: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Facet {
    pub name: String,
    pub values: Vec<FacetValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacetValue {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub count: i64,
}

// ------------------------------------------------------------------- search

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub iri: String,
    pub entity_type: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    pub origin: Origin,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub total: i64,
    /// True when at least one peer failed or timed out (handoff §5.10).
    pub partial: bool,
    pub peers: Vec<PeerSearchStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSearchStatus {
    pub peer_id: String,
    pub base_iri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `ok` | `timeout` | `error`
    pub status: String,
    pub hits: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ------------------------------------------------------------- advertisement

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvertiseIn {
    #[serde(default)]
    pub run: RunIn,
    #[serde(default)]
    pub artifacts: Vec<ArtifactIn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvertiseOut {
    pub run: String,
    pub artifacts: Vec<String>,
    /// False when every artifact in the payload was already recorded for this run and role.
    pub created: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_for_resolution: Vec<String>,
}

// --------------------------------------------------------------- lineage

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageNode {
    pub iri: String,
    pub entity_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub origin: Origin,
    pub depth: i32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unresolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageEdge {
    pub from: String,
    pub to: String,
    /// `used` | `generated` | `derivedFrom` | `revisionOf`
    pub predicate: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lineage {
    pub root: String,
    pub nodes: Vec<LineageNode>,
    pub edges: Vec<LineageEdge>,
    pub truncated: bool,
}
