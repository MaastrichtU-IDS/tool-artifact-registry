//! Write validation (spec §5.3, §7.9).
//!
//! The shapes in `shapes/tar-shapes.ttl` are enforced, not decorative: every write is turned
//! into candidate triples, validated against those shapes by the [`shacl-rust`] engine, and
//! committed only if it conforms. A rejected write returns `422` with a `sh:ValidationReport`
//! in Turtle — the same report format `shacl-manager` emits, so tooling is shared.
//!
//! Because the shapes file *is* the rule set, changing what the API accepts is an edit to that
//! file; no Rust changes with it.
//!
//! Spec Q6 asks whether validation is blocking or advisory. Answer: severity decides.
//! `sh:Violation` blocks, `sh:Warning` never does, and `TAR_SHACL_VALIDATE_WRITES=false`
//! downgrades violations to warnings for an estate that would rather have a half-described
//! artifact than none.
//!
//! [`shacl-rust`]: https://github.com/ensaremirerol/shacl-rust

use crate::ns;
use anyhow::{Context, Result};
use oxigraph::model::{Graph, Quad, Term, TermRef, Triple};
use shacl_rust::validation::dataset::ValidationDataset;
use shacl_rust::{parse_shapes, validate, vocab::sh, Path, PathElement};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Violation,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub focus: String,
    /// `sh:resultPath`, when the constraint has one.
    pub path: String,
    pub constraint: String,
    pub message: String,
    /// Dotted path into the request body, so the form does not have to guess which input the
    /// report is about (handoff §5.7).
    pub field: String,
    /// The offending value, when the engine reports one.
    pub value: Option<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn conforms(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Violation)
    }
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }
    pub fn violations(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.severity == Severity::Violation)
    }
    pub fn summary(&self) -> String {
        let v: Vec<String> = self
            .violations()
            .map(|f| if f.field.is_empty() { f.message.clone() } else { format!("{}: {}", f.field, f.message) })
            .collect();
        if v.is_empty() {
            "no violations".into()
        } else {
            v.join("; ")
        }
    }

    /// Serialise as a `sh:ValidationReport` in Turtle.
    ///
    /// Written out here rather than handed straight from the engine so that each result also
    /// carries `tar:jsonField` — the hint the UI uses to attach an error to an input. It is an
    /// additional triple on a standard report, so a plain SHACL consumer ignores it.
    pub fn to_turtle(&self) -> String {
        let mut s = String::from(
            "@prefix sh:     <http://www.w3.org/ns/shacl#> .\n\
             @prefix tar:    <https://w3id.org/tar/ns#> .\n\
             @prefix dcat:   <http://www.w3.org/ns/dcat#> .\n\
             @prefix dct:    <http://purl.org/dc/terms/> .\n\
             @prefix schema: <https://schema.org/> .\n\
             @prefix rdfs:   <http://www.w3.org/2000/01/rdf-schema#> .\n\
             @prefix prov:   <http://www.w3.org/ns/prov#> .\n\n\
             [] a sh:ValidationReport ;\n",
        );
        s.push_str(&format!("    sh:conforms {} ", self.conforms()));
        for f in &self.findings {
            s.push_str(";\n    sh:result [\n        a sh:ValidationResult ;\n");
            s.push_str(&format!(
                "        sh:resultSeverity sh:{} ;\n",
                match f.severity {
                    Severity::Violation => "Violation",
                    Severity::Warning => "Warning",
                }
            ));
            if f.focus.starts_with("http") {
                s.push_str(&format!("        sh:focusNode <{}> ;\n", f.focus));
            } else {
                s.push_str(&format!("        sh:focusNode \"{}\" ;\n", escape(&f.focus)));
            }
            if !f.path.is_empty() {
                s.push_str(&format!("        sh:resultPath <{}> ;\n", f.path));
            }
            if !f.constraint.is_empty() {
                s.push_str(&format!("        sh:sourceConstraintComponent sh:{} ;\n", f.constraint));
            }
            if let Some(v) = &f.value {
                s.push_str(&format!("        sh:value \"{}\" ;\n", escape(v)));
            }
            if !f.field.is_empty() {
                s.push_str(&format!("        tar:jsonField \"{}\" ;\n", escape(&f.field)));
            }
            s.push_str(&format!("        sh:resultMessage \"{}\"\n    ] ", escape(&f.message)));
        }
        s.push_str(".\n");
        s
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// The parsed shape set, held in application state so the Turtle is read once at boot.
///
/// The triples are kept rather than the parsed `Shape`s because a `Shape` borrows the graph of
/// the dataset it was parsed against, and each validation gets a fresh dataset holding that
/// write's candidate data. Rebuilding a ~150-triple graph per write is not worth optimising.
pub struct Shapes {
    triples: Vec<Triple>,
}

impl Shapes {
    pub fn parse(turtle: &str) -> Result<Self> {
        let graph = shacl_rust::rdf::read_graph_from_string(turtle, "ttl")
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("parsing shapes/tar-shapes.ttl")?;
        let triples: Vec<Triple> = graph.iter().map(|t| t.into_owned()).collect();
        anyhow::ensure!(!triples.is_empty(), "the shapes graph is empty");
        Ok(Self { triples })
    }

    fn graph(&self) -> Graph {
        self.triples.iter().cloned().collect()
    }

    /// Validate one write's candidate quads.
    ///
    /// The data graph is the candidate record alone, not the whole store: a write is checked
    /// for being well-formed in itself. Constraints that would need the rest of the graph —
    /// `sh:class` on a referenced node, say — are therefore not evaluated here, which is the
    /// price of validating before committing rather than after.
    pub fn validate_quads<'q>(&self, quads: impl IntoIterator<Item = &'q Quad>) -> Report {
        let data: Vec<Triple> =
            quads.into_iter().map(|q| Triple::new(q.subject.clone(), q.predicate.clone(), q.object.clone())).collect();
        self.validate_triples(data)
    }

    pub fn validate_triples(&self, data: Vec<Triple>) -> Report {
        let field_hints = field_hints(&data);
        let data_graph: Graph = data.into_iter().collect();
        let dataset = match ValidationDataset::from_graphs(data_graph, self.graph()) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(error = %e, "could not build the validation dataset");
                return Report::default();
            }
        };
        let shapes = match parse_shapes(dataset.shapes_graph()) {
            Ok(s) => s,
            Err(e) => {
                // A broken shapes file must not silently wave writes through.
                tracing::error!(error = %e, "shapes failed to parse; rejecting the write");
                return Report {
                    findings: vec![Finding {
                        severity: Severity::Violation,
                        focus: "urn:tar:shapes".into(),
                        path: String::new(),
                        constraint: "ShapesGraphError".into(),
                        message: format!("the registry's SHACL shapes could not be parsed: {e}"),
                        field: String::new(),
                        value: None,
                    }],
                };
            }
        };

        let report = validate(&dataset, &shapes);
        let mut findings = Vec::new();
        for r in report.get_results() {
            let path = r.result_path().and_then(first_predicate).unwrap_or_default();
            let focus = r.focus_node().to_string();
            let focus_iri = focus.trim_start_matches('<').trim_end_matches('>').to_string();
            findings.push(Finding {
                severity: if r.severity() == sh::VIOLATION { Severity::Violation } else { Severity::Warning },
                field: field_for(&path, &focus_iri, &field_hints),
                path,
                constraint: r
                    .source_constraint_component()
                    .map(|c| c.as_str().rsplit(['#', '/']).next().unwrap_or_default().to_string())
                    .unwrap_or_default(),
                // The engine emits a generic message plus whatever sh:message the shape gave;
                // the shape's wording is the one worth showing.
                message: best_message(r.messages()),
                value: r.value().map(|v| match v {
                    TermRef::Literal(l) => l.value().to_string(),
                    other => other.to_string(),
                }),
                focus: focus_iri,
            });
        }
        findings.sort_by(|a, b| a.field.cmp(&b.field).then_with(|| a.message.cmp(&b.message)));
        Report { findings }
    }
}

fn first_predicate(path: &Path<'_>) -> Option<String> {
    path.get_elements().first().and_then(|e| match e {
        PathElement::Iri(n) | PathElement::Inverse(n) => Some(n.as_str().to_string()),
        _ => None,
    })
}

/// The engine emits the constraint's own description first and the shape's `sh:message` after
/// it — see `build_validation_result`: "Include all constraint-specific messages, then
/// shape-level messages." The shape's wording is the one written for a person, so take the
/// last; with only one message there is no shape-level wording and the built-in is all we have.
fn best_message(messages: &[String]) -> String {
    messages.last().cloned().unwrap_or_else(|| "constraint violated".into())
}

/// Focus IRI -> field prefix, derived from the candidate data itself so that a violation on a
/// nested Distribution reports `distributions[1].auth_method` rather than a bare predicate.
fn field_hints(data: &[Triple]) -> HashMap<String, String> {
    let mut hints = HashMap::new();
    let distribution_pred = format!("{}distribution", ns::DCAT);
    let mut by_parent: HashMap<String, Vec<String>> = HashMap::new();
    for t in data {
        if t.predicate.as_str() == distribution_pred {
            if let Term::NamedNode(n) = &t.object {
                by_parent.entry(t.subject.to_string()).or_default().push(n.as_str().to_string());
            }
        }
    }
    for dists in by_parent.values() {
        for (i, d) in dists.iter().enumerate() {
            hints.insert(d.clone(), format!("distributions[{i}]"));
        }
    }
    hints
}

/// Map a constraint's `sh:resultPath` onto the JSON field the caller sent.
fn field_for(path: &str, focus: &str, hints: &HashMap<String, String>) -> String {
    let local = |ns_: &str, name: &str| format!("{ns_}{name}");
    let base = [
        (local(ns::SCHEMA, "name"), "name"),
        (local(ns::DCT, "abstract"), "tagline"),
        (local(ns::SCHEMA, "description"), "description"),
        (local(ns::SCHEMA, "url"), "homepage"),
        (local(ns::SCHEMA, "codeRepository"), "code_repository"),
        (local(ns::SCHEMA, "softwareHelp"), "documentation"),
        (local(ns::SCHEMA, "softwareVersion"), "version"),
        (local(ns::DCT, "license"), "license"),
        (local(ns::SCHEMA, "applicationCategory"), "kind"),
        (local(ns::CODEMETA, "developmentStatus"), "maturity"),
        (local(ns::DCT, "subject"), "topics"),
        (local(ns::SCHEMA, "keywords"), "keywords"),
        (local(ns::RDFS, "label"), "label"),
        (local(ns::TAR, "instanceOf"), "software"),
        (local(ns::TAR, "runsRelease"), "release"),
        (local(ns::DCAT, "endpointURL"), "endpoint_url"),
        (local(ns::DCAT, "endpointDescription"), "endpoint_description"),
        (local(ns::TAR, "jurisdiction"), "jurisdiction"),
        (local(ns::TAR, "registrationClient"), "registration_clients"),
        (local(ns::TAR, "registrationIssuer"), "registration_issuer"),
        (local(ns::TAR, "selfRegisteredIssuer"), "self_registered_issuer"),
        (local(ns::TAR, "oidcClientId"), "oidc_client_id"),
        (local(ns::TAR, "oidcIssuer"), "oidc_issuer"),
        (local(ns::TAR, "allowedScope"), "allowed_scopes"),
        (local(ns::TAR, "produces"), "capability.produces"),
        (local(ns::TAR, "consumes"), "capability.consumes"),
        (local(ns::DCT, "title"), "title"),
        (local(ns::DCT, "conformsTo"), "conforms_to"),
        (local(ns::DCAT, "keyword"), "keywords"),
        (local(ns::PROV, "wasDerivedFrom"), "was_derived_from"),
        (local(ns::DCT, "isVersionOf"), "is_version_of"),
        (local(ns::DCAT, "distribution"), "distributions"),
        (local(ns::DCAT, "accessURL"), "access_url"),
        (local(ns::DCAT, "downloadURL"), "download_url"),
        (local(ns::DCAT, "byteSize"), "byte_size"),
        (local(ns::DCAT, "mediaType"), "media_type"),
        (local(ns::TAR, "accessProtocol"), "access_protocol"),
        (local(ns::TAR, "authMethod"), "auth_method"),
        (local(ns::TAR, "availability"), "availability"),
        (local(ns::DCT, "accessRights"), "availability"),
        (local(ns::TAR, "accessRequestURL"), "access_request_url"),
        (local(ns::PROV, "startedAtTime"), "run.started_at"),
        (local(ns::PROV, "endedAtTime"), "run.ended_at"),
        (local(ns::TAR, "status"), "run.status"),
        (local(ns::SCHEMA, "actionStatus"), "run.status"),
        (local(ns::PROV, "wasAssociatedWith"), "run.instance"),
    ]
    .into_iter()
    .find(|(iri, _)| iri == path)
    .map(|(_, field)| field.to_string());

    match (hints.get(focus), base) {
        // A violation on a nested distribution is reported against the input that carried it.
        (Some(prefix), Some(field)) => format!("{prefix}.{field}"),
        (Some(prefix), None) => prefix.clone(),
        (None, Some(field)) => field,
        (None, None) => String::new(),
    }
}

/// Everything a write must satisfy before it is committed: the shapes, and the two rules the
/// shapes cannot express — which vocabulary terms exist, and whether a digest is well formed.
///
/// All three are merged into one report rather than checked one after the other, so a write that
/// is wrong in more than one way comes back naming every field instead of sending the caller
/// round the loop once per mistake.
///
/// `TAR_SHACL_VALIDATE_WRITES=false` downgrades shape violations to warnings, as it always has —
/// an estate that would rather have a half-described artifact than none. It deliberately reaches
/// neither of the other two. An unknown type is not a half-described artifact, it is a new tag
/// minted by accident; a digest that cannot be a digest is not a half-described artifact either,
/// it is a false claim about bytes that costs the record the one identity another registry could
/// have recognised it by. Those are the two things the switch must not turn off.
pub fn enforce_write(state: &crate::state::AppState, quads: &[Quad]) -> Result<Report, crate::error::AppError> {
    let mut report = state.shapes.validate_quads(quads);
    if !state.config.shacl_validate_writes {
        for f in &mut report.findings {
            f.severity = Severity::Warning;
        }
    }
    report.findings.extend(crate::domain::vocabulary::findings(state, quads));
    // The other rule a shape cannot state: whether a digest is a digest at all. Unlike the
    // vocabulary rule it needs nothing from the store — it is a property of the write alone —
    // but it lands in the same report for the same reason, so a caller who got both an
    // unknown type and a mistyped digest is told both at once.
    report.findings.extend(crate::domain::content::findings(quads));
    report.findings.sort_by(|a, b| a.field.cmp(&b.field).then_with(|| a.message.cmp(&b.message)));
    enforce(report, true)
}

/// Turn a report into the `422` of spec §7.9, or let the write through.
pub fn enforce(report: Report, blocking: bool) -> Result<Report, crate::error::AppError> {
    if blocking && !report.conforms() {
        let summary = report.summary();
        return Err(crate::error::AppError::validation(report.to_turtle(), summary));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{artifact as artdom, instance as instdom, run as rundom, software as swdom};
    use crate::model::*;

    fn shapes() -> Shapes {
        Shapes::parse(crate::bundles::SHAPES_TTL).expect("the shipped shapes must parse")
    }

    const BASE: &str = "https://reg.test.example";

    fn software(input: SoftwareIn) -> Report {
        let quads = swdom::software_quads(BASE, &format!("{BASE}/software/x"), &input, "urn:test", None);
        shapes().validate_quads(&quads)
    }

    fn artifact(input: ArtifactIn) -> Report {
        let quads = artdom::artifact_quads(BASE, &format!("{BASE}/artifact/x"), &input, "urn:test", None);
        shapes().validate_quads(&quads)
    }

    #[test]
    fn the_shipped_shapes_parse() {
        let s = shapes();
        assert!(!s.triples.is_empty());
        // A conforming record produces no violations at all.
        let r = software(SoftwareIn {
            name: "shacl-manager".into(),
            license: Some("https://spdx.org/licenses/Apache-2.0".into()),
            kind: Some("service".into()),
            ..Default::default()
        });
        assert!(r.conforms(), "{:?}", r.findings);
        assert!(r.is_empty(), "a complete record should not even warn: {:?}", r.findings);
    }

    #[test]
    fn a_nameless_software_is_rejected() {
        let r = software(SoftwareIn::default());
        assert!(!r.conforms());
        assert!(r.violations().any(|f| f.field == "name"), "{:?}", r.findings);
        let ttl = r.to_turtle();
        assert!(ttl.contains("a sh:ValidationReport"));
        assert!(ttl.contains("sh:conforms false"));
        assert!(ttl.contains("sh:resultPath <https://schema.org/name>"));
        assert!(ttl.contains("MinCountConstraintComponent"));
    }

    #[test]
    fn a_program_can_be_several_kinds_at_once() {
        // A tool with a desktop build and a hosted deployment is both; forcing one would make
        // the record wrong about the other.
        let r = software(SoftwareIn {
            name: "rdfcraft".into(),
            kinds: vec!["desktop".into(), "service".into()],
            license: Some("https://spdx.org/licenses/MIT".into()),
            ..Default::default()
        });
        assert!(r.conforms(), "{:?}", r.findings);

        // Each value is still checked against the list.
        let r = software(SoftwareIn {
            name: "rdfcraft".into(),
            kinds: vec!["desktop".into(), "teapot".into()],
            ..Default::default()
        });
        let f = r.violations().find(|f| f.field == "kind").expect("kind violation");
        assert_eq!(f.value.as_deref(), Some("teapot"));
    }

    #[test]
    fn the_older_single_kind_field_still_works() {
        let input = SoftwareIn { name: "x".into(), kind: Some("cli".into()), ..Default::default() };
        assert_eq!(input.resolved_kinds(), vec!["cli"]);
        // And it does not duplicate when both are given.
        let input =
            SoftwareIn { name: "x".into(), kinds: vec!["cli".into()], kind: Some("cli".into()), ..Default::default() };
        assert_eq!(input.resolved_kinds(), vec!["cli"]);
    }

    #[test]
    fn a_shapes_own_message_is_what_the_caller_sees() {
        // Not the engine's "Value does not have node kind: IRI".
        let r =
            software(SoftwareIn { name: "x".into(), code_repository: Some("not-an-iri".into()), ..Default::default() });
        let f = r.violations().find(|f| f.field == "code_repository").expect("repo violation");
        assert_eq!(f.message, "code_repository must be an absolute IRI", "{:?}", r.findings);
    }

    #[test]
    fn a_missing_licence_warns_but_does_not_block() {
        let r = software(SoftwareIn { name: "shacl-manager".into(), ..Default::default() });
        assert!(r.conforms(), "missing licence must not block a write: {:?}", r.findings);
        let warning = r.findings.iter().find(|f| f.field == "license").expect("a warning about the licence");
        assert_eq!(warning.severity, Severity::Warning);
        assert!(warning.message.contains("FAIR R1.1"), "the shape's own wording should survive: {warning:?}");
    }

    #[test]
    fn enum_values_from_the_spec_are_enforced() {
        let r = software(SoftwareIn { name: "x".into(), kind: Some("teapot".into()), ..Default::default() });
        let f = r.violations().find(|f| f.field == "kind").expect("kind violation");
        assert_eq!(f.constraint, "InConstraintComponent");
        assert_eq!(f.value.as_deref(), Some("teapot"));
        assert!(f.message.contains("service, library, cli, desktop, workflow"));
    }

    #[test]
    fn metadata_only_must_not_carry_a_download_url() {
        let r = artifact(ArtifactIn {
            title: Some("masked replica".into()),
            conforms_to: Some("http://edamontology.org/data_2600".into()),
            distributions: vec![DistributionIn {
                availability: Some("metadata-only".into()),
                download_url: Some("https://example.org/data.ttl".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        // The write path strips a downloadURL from a metadata-only distribution, so the shape
        // has nothing to fire on — the model makes the illegal state unrepresentable.
        assert!(r.conforms(), "{:?}", r.findings);

        // Fed the illegal shape directly, the constraint does fire.
        let mut dist = crate::rdf::Node::local(&format!("{BASE}/distribution/x"));
        dist.a(artdom::TYPE_DISTRIBUTION);
        dist.text(ns::TAR, "availability", "metadata-only");
        dist.link(ns::DCAT, "downloadURL", "https://example.org/leaked.ttl");
        let r = shapes().validate_quads(&dist.finish());
        assert!(!r.conforms(), "a metadata-only distribution with bytes must be rejected");
        assert!(r.violations().any(|f| f.message.contains("carries no downloadURL")), "{:?}", r.findings);
    }

    #[test]
    fn a_distribution_must_say_where_the_bytes_are() {
        let r = artifact(ArtifactIn {
            title: Some("report".into()),
            conforms_to: Some("http://edamontology.org/data_2048".into()),
            distributions: vec![DistributionIn { availability: Some("public".into()), ..Default::default() }],
            ..Default::default()
        });
        assert!(!r.conforms(), "{:?}", r.findings);
        assert!(r.violations().any(|f| f.message.contains("access_url or a download_url")), "{:?}", r.findings);
    }

    #[test]
    fn a_violation_on_a_distribution_names_which_one() {
        let r = artifact(ArtifactIn {
            title: Some("report".into()),
            conforms_to: Some("http://edamontology.org/data_2048".into()),
            distributions: vec![
                DistributionIn { access_url: Some("https://a.example".into()), ..Default::default() },
                DistributionIn {
                    access_url: Some("https://b.example".into()),
                    access_protocol: Some("carrier-pigeon".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let f = r.violations().find(|f| f.field.ends_with("access_protocol")).expect("protocol violation");
        assert_eq!(f.field, "distributions[1].access_protocol", "the report must name the offending input");
    }

    #[test]
    fn an_instance_must_name_its_software() {
        let quads = instdom::instance_quads(
            BASE,
            &format!("{BASE}/instance/x"),
            &InstanceIn { label: "laptop".into(), ..Default::default() },
            "urn:test",
            None,
        );
        let r = shapes().validate_quads(&quads);
        assert!(r.violations().any(|f| f.field == "software"), "{:?}", r.findings);
    }

    #[test]
    fn an_unknown_scope_is_rejected() {
        let quads = instdom::instance_quads(
            BASE,
            &format!("{BASE}/instance/x"),
            &InstanceIn {
                label: "shacl.ids".into(),
                software: Some(format!("{BASE}/software/1")),
                allowed_scopes: vec!["advertise:produce".into(), "take:over:the:world".into()],
                ..Default::default()
            },
            "urn:test",
            None,
        );
        let r = shapes().validate_quads(&quads);
        let f = r.violations().find(|f| f.field == "allowed_scopes").expect("scope violation");
        assert_eq!(f.value.as_deref(), Some("take:over:the:world"));
    }

    #[test]
    fn a_run_must_be_attributed_and_carry_a_known_status() {
        let quads = rundom::run_quads(
            &format!("{BASE}/run/x"),
            &RunIn { status: Some("exploded".into()), ..Default::default() },
            &format!("{BASE}/instance/1"),
            "urn:test",
        );
        let r = shapes().validate_quads(&quads);
        let f = r.violations().find(|f| f.field == "run.status").expect("status violation");
        assert!(f.message.contains("success, failed, running, aborted"));
    }

    #[test]
    fn advisory_mode_lets_a_bad_write_through() {
        assert!(enforce(software(SoftwareIn::default()), false).is_ok());
        assert!(enforce(software(SoftwareIn::default()), true).is_err());
    }
}
