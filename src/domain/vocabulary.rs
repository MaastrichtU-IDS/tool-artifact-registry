//! What a write may put in a vocabulary-valued field.
//!
//! `dct:conformsTo` on an artifact and `tar:produces` / `tar:consumes` on a capability used to
//! accept any IRI at all (spec D11). That is right for federation and wrong for everything
//! else: asked to describe the same SHACL report, three callers reach for `…/type/shacl-report`,
//! `…/type/shacl-validation-report` and a remembered EDAM number within a day of each other, and
//! a catalogue whose types are near-synonyms cannot be filtered. `?conforms_to=` then finds a
//! third of what is there, and a subscription written against one spelling silently never fires
//! — which looks exactly like a subscription with nothing to deliver.
//!
//! So a write this registry is authoritative for may only name a term the registry actually
//! holds: a concept in one of its bundled vocabularies, one it minted itself, or one it has
//! cached from a peer. Nothing fits? `POST /api/v1/types` mints a real one, with a label and a
//! definition, in a single call — the honest alternative to a fabricated IRI, and cheaper than
//! guessing.
//!
//! **Why this is not a SHACL shape.** `sh:property [ sh:path dct:conformsTo ; sh:class
//! tar:ArtifactType ]` is what SHACL is for, and it was tried before this was written.
//! `Shapes::validate_quads` hands the engine the candidate record alone, so the class
//! assertions would have to be read out of the store and injected into that data graph on every
//! write. Measured on the shipped bundles: validation goes from
//! 1.9 ms to 9.8 ms, and reading the assertions back costs a further 2.8 ms against the 52 µs
//! the targeted lookup below costs — call it 2 ms a write against 12.6 ms, six and a half times.
//!
//! The time is not what settled it. Three things a shape cannot do at all did:
//!
//! * `sh:message` is fixed text. The refusal has to name the offending IRI and then search,
//!   adopt and mint in that order, because a refusal that does not say how to succeed is a dead
//!   end; a `ClassConstraintComponent` result would say "Value is not an instance of class
//!   tar:ArtifactType" and leave the caller to work the rest out.
//! * `TAR_SHACL_VALIDATE_WRITES=false` downgrades every shape violation to a warning. A
//!   half-described artifact is a trade an operator may make; a type nobody can look up is not,
//!   and moving this rule into the shapes would put it behind that switch.
//! * The peer allowance below is about which *graph* a concept lives in, and the validation
//!   data graph has no graphs — everything the engine sees is one default graph. Expressing it
//!   in SHACL would mean asserting `tar:ArtifactType` on a peer's term, which is a claim this
//!   registry is not authoritative for.
//!
//! So the rule stays here, keyed on class membership, and is reported through the same
//! `sh:ValidationReport` with the same `tar:jsonField` — an edit form highlights the offending
//! input exactly as it does for a shape violation and no caller learns a second error shape.
//!
//! **Why federation is unaffected.** A peer's record is loaded by `api::peers::fetch_stub`
//! straight into `<urn:tar:peer:…>` and never passes through a write handler, so a peer citing a
//! type minted at that peer keeps rendering. Better: once that type has been resolved into the
//! peer graph it becomes a term *this* registry holds, and may then be named locally too. The
//! restriction is on what this registry mints, not on what it is told.
//!
//! The check asks the graph, never the search index: whether a caller found a term by typing its
//! name, by pasting an IRI, or by whatever `/api/v1/vocab/search` grows into, the question here
//! is the same one — does the registry hold it, and is it the right kind of thing.
//!
//! **What "the right kind" is.** A class on the concept (`shapes/vocab.ttl`), asserted in the
//! same statement as `a skos:Concept`. It was a `tar:conceptBranch` string beside the concept
//! until the two were written by different code paths and ended up in different named graphs —
//! see the classes' own comments for that failure. A class cannot come apart from the concept
//! that way, because it *is* the statement that makes the concept.

use crate::error::AppResult;
use crate::ns;
use crate::shacl::{Finding, Report, Severity};
use crate::state::AppState;
use oxigraph::model::Quad;
use std::collections::{HashMap, HashSet};

/// The classes a concept can carry, declared in `shapes/vocab.ttl` as subclasses of
/// `skos:Concept`. Every site that creates a concept asserts one of these beside
/// `a skos:Concept`; nothing else in the registry decides what kind of term something is.
pub const CLASS_ARTIFACT_TYPE: &str = "https://w3id.org/tar/ns#ArtifactType";
pub const CLASS_RESEARCH_TOPIC: &str = "https://w3id.org/tar/ns#ResearchTopic";
pub const CLASS_ARTIFACT_KEYWORD: &str = "https://w3id.org/tar/ns#ArtifactKeyword";
pub const CLASS_LEGACY_TOPIC: &str = "https://w3id.org/tar/ns#LegacyTopic";

/// The four in one place, so a query that has to enumerate them cannot fall behind the list.
pub const CONCEPT_CLASSES: [&str; 4] =
    [CLASS_ARTIFACT_TYPE, CLASS_RESEARCH_TOPIC, CLASS_ARTIFACT_KEYWORD, CLASS_LEGACY_TOPIC];

/// Which kind of term a field expects.
///
/// The distinction is the half a plain existence check misses: a term can be perfectly real and
/// still be the wrong kind of thing for the field it was put in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    /// What a piece of software is *about*.
    Topic,
    /// What an artifact *is*, or what a capability produces and consumes.
    Type,
}

impl Slot {
    /// The `branch` a caller passes to `/api/v1/vocab/search` to be offered these terms. Used in
    /// the refusal, so the recovery step is a query the caller can run verbatim.
    pub fn branch(self) -> &'static str {
        match self {
            Slot::Topic => "topic",
            Slot::Type => "data",
        }
    }

    /// The class a concept must carry to be usable here.
    pub fn class(self) -> &'static str {
        match self {
            Slot::Topic => CLASS_RESEARCH_TOPIC,
            Slot::Type => CLASS_ARTIFACT_TYPE,
        }
    }
}

/// Why a term was refused. The diagnosis, not the advice: the advice differs by caller — a REST
/// client is pointed at routes and an agent at tools — but the verdict must not, or the two
/// surfaces drift into two rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Not a concept this registry holds in any graph.
    Unknown,
    /// A real term, but not one `vocab_search` would return for this slot. Carries the classes it
    /// does carry, which is empty for a concept that predates them.
    WrongClass(Vec<String>),
}

impl Verdict {
    /// What is wrong, in one sentence, naming no vocabulary: several are in play here and more
    /// will follow, so a message that named one would be wrong the moment another is added.
    pub fn describe(&self, iri: &str, slot: Slot) -> String {
        match (self, slot) {
            (Verdict::Unknown, Slot::Type) => format!(
                "{iri} is not an artifact type this registry knows"
            ),
            (Verdict::Unknown, Slot::Topic) => format!("{iri} is not a topic this registry knows"),
            // The one the failure that motivated this rule actually produced: a term that
            // resolves, on a record it has nothing to do with.
            (Verdict::WrongClass(_), Slot::Type) => format!(
                "{iri} is a term this registry holds, but it names a subject area rather than a \
                 kind of data, so it cannot be what an artifact is"
            ),
            (Verdict::WrongClass(_), Slot::Topic) => format!(
                "{iri} is a term this registry holds, but not one it classifies software by — it \
                 is kept so that records already citing it still render a label"
            ),
        }
    }

    /// The whole message a REST caller sees: what is wrong, then the ways out, as routes they can
    /// paste. A refusal that does not say how to succeed is a dead end.
    ///
    /// The order is the order of preference, and it matters. Reusing a term the registry already
    /// holds is best; adopting one that is already identified somewhere else keeps two registries
    /// agreeing on one IRI; minting is last, because a new identifier for a thing that already
    /// had one is the duplication this rule exists to prevent, one level up.
    pub fn message(&self, iri: &str, slot: Slot) -> String {
        let search = format!(
            "search for one with GET /api/v1/vocab/search?branch={}&q=… and use the `iri` it returns",
            slot.branch()
        );
        match slot {
            Slot::Type => format!(
                "{}. First {search}. If the term is defined somewhere this registry does not carry \
                 and you have its IRI, adopt it with POST /api/v1/types, sending that `iri`. Mint a \
                 new one with POST /api/v1/types, without an `iri`, only when nothing anywhere \
                 names this thing.",
                self.describe(iri, slot)
            ),
            Slot::Topic => format!(
                "{}. Either {search}, or leave the field out.",
                self.describe(iri, slot)
            ),
        }
    }
}

/// One vocabulary-valued object found in a candidate write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub iri: String,
    pub slot: Slot,
    /// The request field, so the report can point the edit form at the input that carried it.
    pub field: &'static str,
    /// The predicate, for `sh:resultPath`.
    pub path: String,
    /// The node the term was written on, for `sh:focusNode`.
    pub focus: String,
}

/// Every vocabulary term a candidate write names.
///
/// `dct:conformsTo` is deliberately only collected from artifact nodes. The same predicate also
/// names the specification an API description follows (`software.rs`, on a `dct:Standard` node)
/// and the schema a distribution conforms to; neither is an artifact type, and holding an
/// OpenAPI specification IRI to the artifact-type vocabulary would refuse a correct record.
pub fn terms_in(quads: &[Quad]) -> Vec<Term> {
    let rdf_type = ns::rdf_type();
    let conforms_to = format!("{}conformsTo", ns::DCT);
    let named = |t: &oxigraph::model::Term| match t {
        oxigraph::model::Term::NamedNode(n) => Some(n.as_str().to_string()),
        _ => None,
    };
    let artifacts: HashSet<String> = quads
        .iter()
        .filter(|q| {
            q.predicate == rdf_type
                && named(&q.object).as_deref() == Some(super::artifact::TYPE_DATASET)
        })
        .map(|q| q.subject.to_string())
        .collect();

    let mut out = Vec::new();
    for q in quads {
        let predicate = q.predicate.as_str();
        let (Some((slot, field)), Some(object)) = (slot_of(predicate), named(&q.object)) else {
            continue;
        };
        if predicate == conforms_to && !artifacts.contains(&q.subject.to_string()) {
            continue;
        }
        out.push(Term {
            iri: object,
            slot,
            field,
            path: predicate.to_string(),
            focus: q.subject.to_string().trim_matches(['<', '>']).to_string(),
        });
    }
    // Deduplicated per field rather than per IRI: one bad term named in both halves of a
    // capability is two inputs to correct, and a form that highlighted only one of them would
    // send the curator round the loop twice.
    out.sort_by(|a, b| a.iri.cmp(&b.iri).then(a.slot.cmp(&b.slot)).then(a.field.cmp(b.field)));
    out.dedup_by(|a, b| a.iri == b.iri && a.slot == b.slot && a.field == b.field);
    out
}

/// The predicate a vocabulary term appears behind, the slot it expects, and the request field a
/// violation belongs to — the same field names `shacl::field_for` produces, so whichever check
/// refuses a write, the form highlights the same input.
fn slot_of(predicate: &str) -> Option<(Slot, &'static str)> {
    if let Some(local) = predicate.strip_prefix(ns::DCT) {
        return match local {
            "conformsTo" => Some((Slot::Type, "conforms_to")),
            "subject" => Some((Slot::Topic, "topics")),
            _ => None,
        };
    }
    if let Some(local) = predicate.strip_prefix(ns::TAR) {
        return match local {
            "produces" => Some((Slot::Type, "capability.produces")),
            "consumes" => Some((Slot::Type, "capability.consumes")),
            _ => None,
        };
    }
    None
}

/// Judge a set of terms against the registry's own vocabulary graphs.
///
/// A term with no verdict conformed. A failure to read the store yields no verdicts at all
/// rather than refusing everything: an unreachable index is not evidence that an IRI is wrong,
/// and a write blocked for a reason the caller cannot act on is worse than one let through.
pub fn verdicts(state: &AppState, terms: &[Term]) -> Vec<(usize, Verdict)> {
    if terms.is_empty() {
        return Vec::new();
    }
    let iris: Vec<&str> = terms.iter().map(|t| t.iri.as_str()).collect();
    let Some(known) = held(state, &iris) else { return Vec::new() };

    let mut out = Vec::new();
    for (i, term) in terms.iter().enumerate() {
        let Some(held) = known.get(&term.iri) else {
            out.push((i, Verdict::Unknown));
            continue;
        };
        if !held.usable_as(term.slot) {
            out.push((i, Verdict::WrongClass(held.classes.clone())));
        }
    }
    out
}

/// How the registry holds one concept: the classes it carries, and whether it came from a peer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Held {
    /// The `tar:` concept classes asserted on it, in any graph.
    pub classes: Vec<String>,
    /// True when its `a skos:Concept` triple is in a peer's own read-only graph.
    pub from_peer: bool,
}

impl Held {
    pub fn is(&self, class: &str) -> bool {
        self.classes.iter().any(|c| c == class)
    }

    /// Whether a write may name this concept in that slot.
    ///
    /// A term this registry cached from a peer carries none of our classes, and a peer is
    /// authoritative for its own types. Accepting it is what lets a local record cite a type
    /// another registry defined, once we have actually resolved it. That allowance is keyed on
    /// the graph rather than on the absence of a class, which is the distinction the branch
    /// string could not draw: an untyped concept in *our* graphs is a term nobody classified —
    /// a subject area kept only for its label, or a leftover from a write path that has since
    /// stopped typing things as concepts — and those must not inherit a peer's benefit of the
    /// doubt.
    pub fn usable_as(&self, slot: Slot) -> bool {
        self.is(slot.class()) || (slot == Slot::Type && self.from_peer)
    }
}

/// Which of these IRIs the registry holds as a concept, and as what. `None` when the store could
/// not be read at all, which is not the same answer as "holds none of them".
///
/// `a skos:Concept` rather than "has any triple at all": every record in the store is a subject
/// of something, and a rule that accepted any known IRI would wave an artifact's own IRI into
/// its type field.
///
/// **Two stores, in that order, and why.** This runs on every write —
/// `shacl::enforce_write` calls it through [`findings`] — and against `TAR_SPARQL_ENDPOINT` a
/// question asked of the record store is an HTTP round trip. So it asks the in-memory reference
/// store first, which holds every bundled term and answers in microseconds without a socket.
/// That is the overwhelming majority of lookups: the bundles carry some 2 300 concepts and a
/// record normally cites one of them.
///
/// The record store is asked only for what is left over, and it has to be asked, because two
/// kinds of term live only there and both must keep validating:
///
/// * a type this registry minted or adopted, written to `<urn:tar:local>` by
///   `api::types::create`;
/// * a type cached from a peer, in that peer's own `<urn:tar:peer:…>` graph — and the peer
///   allowance in [`Held::usable_as`] is keyed on exactly which graph the concept sits in, so
///   it cannot be answered anywhere but there.
///
/// The worst case is a write that names only minted or peer types, or only terms nobody holds:
/// one in-memory query that finds nothing, then the same single record-store query the old
/// code made. Never two round trips, never more than before.
///
/// **A reference answer is not overridden.** A bundled term found in the reference store is not
/// looked up again in the record store, so a locally adopted copy of a bundled IRI cannot
/// change what kind of thing it is. It cannot anyway: `api::types::adoptable` refuses to adopt
/// an IRI this registry already holds as something other than a type.
pub fn held(state: &AppState, iris: &[&str]) -> Option<HashMap<String, Held>> {
    if iris.is_empty() {
        return Some(HashMap::new());
    }
    let mut out = held_in(state.reference.as_ref(), iris)?;
    let missing: Vec<&str> = iris.iter().copied().filter(|i| !out.contains_key(*i)).collect();
    if !missing.is_empty() {
        out.extend(held_in(state.store.as_ref(), &missing)?);
    }
    Some(out)
}

/// The lookup itself, against one store.
///
/// The class is matched in its own `GRAPH` block rather than inside the one that finds the
/// concept. Every site that creates a concept now writes both into the same graph, so the two
/// forms return the same rows — but requiring co-location is exactly what made the branch marker
/// fail, and there is no reason to write the requirement back in.
fn held_in(store: &dyn crate::store::GraphStore, iris: &[&str]) -> Option<HashMap<String, Held>> {
    let values = iris.iter().map(|i| format!("<{i}>")).collect::<Vec<_>>().join(" ");
    let q = format!(
        "{p}\nSELECT DISTINCT ?t ?g ?class WHERE {{\n  VALUES ?t {{ {values} }}\n\
         \x20 GRAPH ?g {{ ?t a skos:Concept }}\n\
         \x20 OPTIONAL {{ GRAPH ?cg {{ ?t a ?class }} FILTER(STRSTARTS(STR(?class), \"{tar}\")) }}\n}}",
        p = ns::PREFIXES,
        tar = ns::TAR
    );
    let rows = store.select(&q).ok()?;
    let mut out: HashMap<String, Held> = HashMap::new();
    let mut only_peer: HashMap<String, bool> = HashMap::new();
    for row in rows.rows {
        let Some(t) = row.iri("t") else { continue };
        let e = out.entry(t.clone()).or_default();
        if let Some(class) = row.iri("class") {
            if CONCEPT_CLASSES.contains(&class.as_str()) && !e.classes.contains(&class) {
                e.classes.push(class);
            }
        }
        // A concept this registry declares *and* a peer happens to cache is ours: the peer graph
        // is the fallback for terms we have no opinion about, not an override of one we do.
        let peer = row.iri("g").is_some_and(|g| g.starts_with(ns::G_PEER_PREFIX));
        let only = only_peer.entry(t).or_insert(true);
        *only &= peer;
    }
    for (iri, h) in out.iter_mut() {
        h.classes.sort();
        h.from_peer = only_peer.get(iri).copied().unwrap_or(false);
    }
    Some(out)
}

/// How the registry already holds one IRI, or `None` if it does not hold it as a concept at all.
pub fn holding(state: &AppState, iri: &str) -> Option<Held> {
    let mut known = held(state, &[iri])?;
    known.remove(iri)
}

/// The vocabulary rule as validation findings, ready to merge into a `sh:ValidationReport`.
pub fn findings(state: &AppState, quads: &[Quad]) -> Vec<Finding> {
    let terms = terms_in(quads);
    verdicts(state, &terms)
        .into_iter()
        .map(|(i, verdict)| {
            let term = &terms[i];
            Finding {
                severity: Severity::Violation,
                focus: term.focus.clone(),
                path: term.path.clone(),
                // A real component rather than an invented one: a plain SHACL consumer has to be
                // able to read this report, and "the value is not one of the permitted ones" is
                // exactly what sh:in reports. The permitted set is simply too large to inline.
                constraint: "InConstraintComponent".into(),
                message: verdict.message(&term.iri, term.slot),
                field: term.field.to_string(),
                value: Some(term.iri.clone()),
            }
        })
        .collect()
}

/// Enforce the vocabulary rule alone, for a write path that does not run the shapes.
pub fn enforce(state: &AppState, quads: &[Quad]) -> AppResult<()> {
    let report = Report { findings: findings(state, quads) };
    crate::shacl::enforce(report, true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    const BASE: &str = "https://reg.test.example";

    fn artifact(conforms_to: &str) -> Vec<Quad> {
        super::super::artifact::artifact_quads(
            BASE,
            &format!("{BASE}/artifact/x"),
            &ArtifactIn { title: Some("x".into()), conforms_to: Some(conforms_to.into()), ..Default::default() },
            "urn:test",
            None,
        )
    }

    #[test]
    fn an_artifacts_type_is_a_vocabulary_term_and_the_field_is_named() {
        let terms = terms_in(&artifact("https://example.org/type/report"));
        assert_eq!(terms.len(), 1, "{terms:?}");
        assert_eq!(terms[0].iri, "https://example.org/type/report");
        assert_eq!(terms[0].slot, Slot::Type);
        assert_eq!(terms[0].field, "conforms_to", "the report has to name the input the form must highlight");
    }

    /// `dct:conformsTo` is overloaded: an API description uses it to say which specification it
    /// follows. Holding an OpenAPI specification IRI to the artifact-type vocabulary would refuse
    /// a record that is entirely correct.
    #[test]
    fn a_specification_an_api_description_follows_is_not_an_artifact_type() {
        let quads = super::super::software::software_quads(
            BASE,
            &format!("{BASE}/software/x"),
            &SoftwareIn {
                name: "x".into(),
                api_docs: vec![ApiDoc {
                    url: "https://x.example/openapi.json".into(),
                    format: "openapi".into(),
                    title: None,
                    description: None,
                }],
                ..Default::default()
            },
            "urn:test",
            None,
        );
        assert!(
            terms_in(&quads).is_empty(),
            "only an artifact's own conformsTo is a type: {:?}",
            terms_in(&quads)
        );
    }

    #[test]
    fn a_capabilitys_two_sides_report_against_different_inputs() {
        let quads = super::super::software::capability_quads(
            &format!("{BASE}/capability/x"),
            &CapabilityIn {
                produces: vec!["https://example.org/type/a".into()],
                consumes: vec!["https://example.org/type/b".into()],
            },
        );
        let terms = terms_in(&quads);
        let fields: Vec<&str> = terms.iter().map(|t| t.field).collect();
        assert!(fields.contains(&"capability.produces"), "{terms:?}");
        assert!(fields.contains(&"capability.consumes"), "{terms:?}");
        assert!(terms.iter().all(|t| t.slot == Slot::Type), "{terms:?}");
    }

    /// The half a plain existence check misses, now that the kind is a class: both of these are
    /// real concepts the registry holds, and each is wrong in the other's field.
    #[test]
    fn a_concept_is_usable_only_in_the_slot_its_class_names() {
        let a_type = Held { classes: vec![CLASS_ARTIFACT_TYPE.into()], from_peer: false };
        let a_topic = Held { classes: vec![CLASS_RESEARCH_TOPIC.into()], from_peer: false };
        assert!(a_type.usable_as(Slot::Type));
        assert!(!a_type.usable_as(Slot::Topic));
        assert!(a_topic.usable_as(Slot::Topic));
        assert!(!a_topic.usable_as(Slot::Type));
    }

    /// A peer is authoritative for the types it mints, so one cached from a peer is usable here
    /// without carrying any class of ours. A concept in *our* graphs with no class is not: that
    /// is a subject area kept only for its label, or a leftover from a write path that has since
    /// stopped typing things as concepts, and neither is something an artifact can be.
    #[test]
    fn only_a_peers_untyped_concept_gets_the_benefit_of_the_doubt() {
        assert!(Held { classes: vec![], from_peer: true }.usable_as(Slot::Type));
        assert!(!Held { classes: vec![], from_peer: false }.usable_as(Slot::Type));
        assert!(!Held { classes: vec![], from_peer: true }.usable_as(Slot::Topic));
        assert!(
            !Held { classes: vec![CLASS_LEGACY_TOPIC.into()], from_peer: false }.usable_as(Slot::Topic),
            "a topic kept only so old records render a label is never offered again"
        );
        assert!(
            !Held { classes: vec![CLASS_ARTIFACT_KEYWORD.into()], from_peer: false }.usable_as(Slot::Type),
            "a keyword is a label, not a type"
        );
    }

    #[test]
    fn a_refusal_names_the_term_and_all_three_ways_out_in_order() {
        let m = Verdict::Unknown.message("https://example.org/type/invented", Slot::Type);
        assert!(m.contains("https://example.org/type/invented"), "{m}");
        let search = m.find("/api/v1/vocab/search").expect("the refusal must say how to search");
        let adopt = m.find("adopt it").expect("the refusal must say how to adopt an existing term");
        let mint = m.find("Mint a new one").expect("the refusal must say how to mint");
        assert!(search < adopt && adopt < mint, "reuse, then adopt, then mint — in that order: {m}");
    }

    /// Several vocabularies are in play here and more will follow, so naming one in a message a
    /// caller reads would be wrong the moment another is added.
    #[test]
    fn no_refusal_names_a_vocabulary() {
        for slot in [Slot::Topic, Slot::Type] {
            for verdict in [Verdict::Unknown, Verdict::WrongClass(vec![CLASS_LEGACY_TOPIC.into()])] {
                let m = verdict.message("https://example.org/x", slot);
                for name in ["EDAM", "EuroSciVoc", "edamontology", "euroscivoc"] {
                    assert!(!m.contains(name), "{m:?} names {name}");
                }
            }
        }
    }
}
