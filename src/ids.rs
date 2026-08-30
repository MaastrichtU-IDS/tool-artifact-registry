//! Identifier minting and parsing (spec D2, §4.4).
//!
//! `{base_iri}/{kind}/{uuidv7}` — no central coordination, no cross-peer collisions, and
//! lexicographically time-ordered, which is what makes keyset pagination and "newest first"
//! free.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Software,
    Release,
    Instance,
    Artifact,
    ArtifactSeries,
    Run,
    Type,
    /// Not in spec §4.4: distributions and capabilities are minted as IRIs rather than blank
    /// nodes so the UI can link to them and a peer can cite them. See README "deviations".
    Distribution,
    Capability,
    Agent,
    Peer,
}

impl Kind {
    pub fn segment(&self) -> &'static str {
        match self {
            Kind::Software => "software",
            Kind::Release => "release",
            Kind::Instance => "instance",
            Kind::Artifact => "artifact",
            Kind::ArtifactSeries => "artifact-series",
            Kind::Run => "run",
            Kind::Type => "type",
            Kind::Distribution => "distribution",
            Kind::Capability => "capability",
            Kind::Agent => "agent",
            Kind::Peer => "peer",
        }
    }

    pub fn from_segment(s: &str) -> Option<Kind> {
        Some(match s {
            "software" => Kind::Software,
            "release" => Kind::Release,
            // The UI route is /instances/:id while the IRI segment is /instance/:id; accept
            // both so a pasted UI URL resolves.
            "instance" | "instances" => Kind::Instance,
            "artifact" | "artifacts" => Kind::Artifact,
            "artifact-series" => Kind::ArtifactSeries,
            "run" | "runs" => Kind::Run,
            "type" | "types" => Kind::Type,
            "distribution" => Kind::Distribution,
            "capability" => Kind::Capability,
            "agent" => Kind::Agent,
            "peer" | "peers" => Kind::Peer,
            _ => return None,
        })
    }

    /// The `entity_type` used by search results and the JSON API.
    pub fn api_name(&self) -> &'static str {
        self.segment()
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.segment())
    }
}

/// Mints `{base}/{kind}/{uuidv7}`.
pub fn mint(base_iri: &str, kind: Kind) -> String {
    format!("{}/{}/{}", base_iri.trim_end_matches('/'), kind.segment(), new_id())
}

pub fn new_id() -> String {
    Uuid::now_v7().to_string()
}

/// Rebuild an IRI from a local id. Used by every route that takes `{id}` in the path.
pub fn iri_for(base_iri: &str, kind: Kind, id: &str) -> String {
    if id.starts_with("http://") || id.starts_with("https://") {
        // Allow a full IRI in a path position, which is how a foreign record is addressed.
        return id.to_string();
    }
    format!("{}/{}/{}", base_iri.trim_end_matches('/'), kind.segment(), id)
}

/// The local id of an IRI minted by this registry, if it is one.
pub fn local_id(base_iri: &str, iri: &str) -> Option<(Kind, String)> {
    let rest = iri.strip_prefix(base_iri.trim_end_matches('/'))?.strip_prefix('/')?;
    let (seg, id) = rest.split_once('/')?;
    let kind = Kind::from_segment(seg)?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    Some((kind, id.to_string()))
}

/// True when this registry minted the IRI, i.e. it is authoritative for it (spec §9.7).
pub fn is_local(base_iri: &str, iri: &str) -> bool {
    local_id(base_iri, iri).is_some()
}

/// Last path segment or fragment — the display fallback for an unresolved type IRI
/// (handoff §6.1, `ArtifactTypeChip`).
pub fn iri_tail(iri: &str) -> &str {
    iri.rsplit(['#', '/']).find(|s| !s.is_empty()).unwrap_or(iri)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_local_iris() {
        let base = "https://reg.example.org";
        let iri = mint(base, Kind::Software);
        let (kind, id) = local_id(base, &iri).expect("local");
        assert_eq!(kind, Kind::Software);
        assert_eq!(iri_for(base, Kind::Software, &id), iri);
    }

    #[test]
    fn foreign_iris_are_not_local() {
        let base = "https://reg.example.org";
        assert!(!is_local(base, "https://reg.mumc.nl/artifact/01J7Z"));
        assert!(!is_local(base, "http://edamontology.org/data_2048"));
    }

    #[test]
    fn uuidv7_ids_sort_by_creation_time() {
        let a = new_id();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let b = new_id();
        assert!(a < b, "uuidv7 must sort lexicographically by time: {a} !< {b}");
    }

    #[test]
    fn iri_tail_handles_edam_and_slugs() {
        assert_eq!(iri_tail("http://edamontology.org/data_2048"), "data_2048");
        assert_eq!(iri_tail("https://w3id.org/tar/ns#Capability"), "Capability");
    }
}
