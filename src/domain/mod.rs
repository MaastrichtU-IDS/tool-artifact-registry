//! Read projections and write builders between the graph and the API model.

pub mod artifact;
pub mod content;
pub mod forge;
pub mod instance;
pub mod keywords;
pub mod run;
pub mod software;
pub mod vocabulary;

use crate::error::AppResult;
use crate::model::{AgentIn, AgentRef, Origin, TypeRef};
use crate::ns;
use crate::ops::PeerRecord;
use crate::rdf::{Node, Props};
use crate::state::AppState;
use oxigraph::model::Quad;
use std::collections::HashMap;

/// Per-request context: the peer table, loaded once, so every record and list row can carry
/// a truthful origin chip without a lookup per row.
pub struct Ctx<'a> {
    pub state: &'a AppState,
    /// graph IRI -> peer
    peers: HashMap<String, PeerRecord>,
}

impl<'a> Ctx<'a> {
    pub async fn new(state: &'a AppState) -> AppResult<Ctx<'a>> {
        let peers = state.ops.list_peers(None).await.unwrap_or_default();
        let map = peers.into_iter().map(|p| (ns::peer_graph(&p.id), p)).collect();
        Ok(Ctx { state, peers: map })
    }

    pub fn base(&self) -> &str {
        &self.state.config.base_iri
    }

    /// Origin chip data. A record cached from a peer never renders identically to a local one
    /// (handoff §2.3).
    pub fn origin(&self, graph: Option<&str>) -> Origin {
        match graph {
            Some(g) if g == ns::G_LOCAL => Origin::local(),
            Some(g) if g.starts_with(ns::G_PEER_PREFIX) => match self.peers.get(g) {
                Some(p) => Origin {
                    kind: "peer".into(),
                    peer_id: Some(p.id.clone()),
                    peer_title: p.title.clone(),
                    peer_base_iri: Some(p.base_iri.clone()),
                    cached_at: p.last_seen_at.clone(),
                    resolve_status: Some(p.resolve_status.clone()),
                },
                None => Origin {
                    kind: "peer".into(),
                    peer_id: Some(g.trim_start_matches(ns::G_PEER_PREFIX).to_string()),
                    ..Default::default()
                },
            },
            _ => Origin::local(),
        }
    }

    /// Origin decided from the IRI alone — used for cross-links we have not resolved.
    pub fn origin_of_iri(&self, iri: &str) -> Origin {
        if crate::ids::is_local(self.base(), iri) {
            return Origin::local();
        }
        for p in self.peers.values() {
            if iri.starts_with(&p.base_iri) {
                return Origin {
                    kind: "peer".into(),
                    peer_id: Some(p.id.clone()),
                    peer_title: p.title.clone(),
                    peer_base_iri: Some(p.base_iri.clone()),
                    cached_at: p.last_seen_at.clone(),
                    resolve_status: Some(p.resolve_status.clone()),
                };
            }
        }
        Origin { kind: "peer".into(), ..Default::default() }
    }

    /// Resolve a batch of type IRIs to labelled chips in one query. Unlabelled IRIs fall back
    /// to their last path segment (handoff §6.1).
    pub fn type_refs(&self, iris: &[String]) -> Vec<TypeRef> {
        if iris.is_empty() {
            return Vec::new();
        }
        let mut labels: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        let values = iris.iter().filter(|i| i.starts_with("http")).map(|i| format!("<{i}>")).collect::<Vec<_>>().join(" ");
        if !values.is_empty() {
            let q = format!(
                r#"{p}
SELECT ?t ?label ?def WHERE {{
  VALUES ?t {{ {values} }}
  GRAPH ?g {{
    OPTIONAL {{ ?t skos:prefLabel ?label }}
    OPTIONAL {{ ?t rdfs:label ?label }}
    OPTIONAL {{ ?t skos:definition ?def }}
  }}
}}"#,
                p = ns::PREFIXES
            );
            // The record store first, then the bundles in memory. Both are asked because a chip
            // may name a bundled term, a term minted here, or one cached from a peer, and only
            // the record store holds the last two; `or_insert` then leaves the record store's
            // label standing where a term is in both.
            for store in [self.state.store.as_ref(), self.state.reference.as_ref()] {
                let Ok(res) = store.select(&q) else { continue };
                for row in res.rows {
                    if let Some(t) = row.iri("t") {
                        let e = labels.entry(t).or_insert((None, None));
                        if e.0.is_none() {
                            e.0 = row.str("label");
                        }
                        if e.1.is_none() {
                            e.1 = row.str("def");
                        }
                    }
                }
            }
        }
        iris.iter()
            .map(|iri| {
                let (label, definition) = labels.get(iri).cloned().unwrap_or((None, None));
                TypeRef {
                    label: Some(label.unwrap_or_else(|| crate::ids::iri_tail(iri).to_string())),
                    definition,
                    source: type_source(self.base(), iri),
                    iri: iri.clone(),
                }
            })
            .collect()
    }

    pub fn type_ref(&self, iri: &str) -> TypeRef {
        self.type_refs(&[iri.to_string()]).pop().unwrap_or_default()
    }

    /// Resolve an Agent IRI to a renderable reference.
    pub fn agent_ref(&self, iri: &str) -> AgentRef {
        let props = self
            .state
            .store
            .describe(iri)
            .ok()
            .map(|q| Props::from_quads(iri, &q))
            .unwrap_or_default();
        let kind = if props.has_type(&format!("{}Person", ns::SCHEMA)) {
            Some("person".to_string())
        } else if props.has_type(&format!("{}Organization", ns::SCHEMA)) {
            Some("organization".to_string())
        } else if props.has_type(&format!("{}SoftwareAgent", ns::PROV)) {
            Some("software".to_string())
        } else {
            None
        };
        AgentRef {
            name: props.str(ns::SCHEMA, "name").or_else(|| props.str(ns::RDFS, "label")),
            kind,
            identifier: props.iri(ns::SCHEMA, "identifier").or_else(|| props.str(ns::SCHEMA, "identifier")),
            email: props.str(ns::SCHEMA, "email"),
            homepage: props.iri(ns::SCHEMA, "url"),
            version: props.str(ns::SCHEMA, "softwareVersion"),
            iri: iri.to_string(),
        }
    }

    pub fn opt_agent_ref(&self, iri: Option<String>) -> Option<AgentRef> {
        iri.map(|i| self.agent_ref(&i))
    }

    /// Labels for a batch of subjects (`schema:name`, `dct:title` or `rdfs:label`).
    pub fn labels(&self, iris: &[String]) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let values = iris.iter().filter(|i| i.starts_with("http")).map(|i| format!("<{i}>")).collect::<Vec<_>>().join(" ");
        if values.is_empty() {
            return out;
        }
        let q = format!(
            r#"{p}
SELECT ?s ?l WHERE {{
  VALUES ?s {{ {values} }}
  GRAPH ?g {{ {{ ?s schema:name ?l }} UNION {{ ?s dct:title ?l }} UNION {{ ?s rdfs:label ?l }} UNION {{ ?s schema:softwareVersion ?l }} }}
}}"#,
            p = ns::PREFIXES
        );
        if let Ok(res) = self.state.store.select(&q) {
            for row in res.rows {
                if let (Some(s), Some(l)) = (row.iri("s"), row.str("l")) {
                    out.entry(s).or_insert(l);
                }
            }
        }
        out
    }
}

/// Where a term comes from, *relative to this registry* — not which vocabulary it belongs to.
///
/// It used to answer `"edam"` / `"euroscivoc"`, which the picker rendered straight onto the
/// screen: a vocabulary's name in an API field and a UI label, which is precisely what this
/// project does not do, because several are in play and more will follow. The distinction
/// callers actually need is whether the registry ships the term, minted it, or is taking
/// somebody else's word for it. A UI that wants to *show* which vocabulary derives it from the
/// IRI — see `vocabularyOf` in `frontend/src/components/chips.tsx` — where it stays truthful as
/// vocabularies are added and needs no change here.
pub fn type_source(base: &str, iri: &str) -> String {
    if iri.starts_with("http://edamontology.org/")
        || iri.starts_with("http://data.europa.eu/8mn/euroscivoc/")
    {
        "bundled".into()
    } else if crate::ids::is_local(base, iri) {
        "local".into()
    } else {
        "external".into()
    }
}

/// Write an inline agent, minting an IRI when the caller supplied fields rather than an IRI.
/// Returns `(iri, quads)`.
pub fn agent_quads(base: &str, a: &AgentIn) -> (Option<String>, Vec<Quad>) {
    if let Some(iri) = a.iri.as_ref().filter(|s| !s.is_empty()) {
        // A bare IRI (ORCID, ROR) is used as-is: it is already a global identifier.
        if a.name.is_none() && a.email.is_none() && a.homepage.is_none() {
            return (Some(iri.clone()), Vec::new());
        }
        let mut n = Node::local(iri);
        agent_body(&mut n, a);
        return (Some(iri.clone()), n.finish());
    }
    // An identifier IRI (ORCID/ROR) is a better subject than a minted one — it federates.
    let iri = match a.identifier.as_ref().filter(|s| s.starts_with("http")) {
        Some(id) => id.clone(),
        None => {
            if a.name.is_none() {
                return (None, Vec::new());
            }
            crate::ids::mint(base, crate::ids::Kind::Agent)
        }
    };
    let mut n = Node::local(&iri);
    agent_body(&mut n, a);
    (Some(iri), n.finish())
}

fn agent_body(n: &mut Node, a: &AgentIn) {
    match a.kind.as_deref() {
        Some("person") => {
            n.a(&format!("{}Person", ns::SCHEMA));
        }
        Some("organization") => {
            n.a(&format!("{}Organization", ns::SCHEMA));
        }
        // A piece of software acting on its own account — a pipeline step, a service, a model.
        // PROV distinguishes this from a person because the questions differ: you ask a person
        // why, and a system what version.
        Some("software") => {
            n.a(&format!("{}SoftwareAgent", ns::PROV));
            n.a(&format!("{}SoftwareApplication", ns::SCHEMA));
        }
        _ => {
            n.a(&format!("{}Agent", ns::PROV));
        }
    };
    n.a(&format!("{}Agent", ns::PROV));
    n.opt_text(ns::SCHEMA, "name", &a.name);
    n.opt_link(ns::SCHEMA, "identifier", &a.identifier);
    n.opt_text(ns::SCHEMA, "email", &a.email);
    n.opt_link(ns::SCHEMA, "url", &a.homepage);
    n.opt_text(ns::SCHEMA, "softwareVersion", &a.version);
}

/// `xsd:dateTime` for "30 days ago", used by the runs/30d signals.
pub fn thirty_days_ago() -> String {
    (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
