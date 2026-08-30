//! Write validation and SHACL validation reports (spec §5.3, §7.9).
//!
//! ## What this is, and is not
//!
//! The shapes that describe the model ship in `shapes/tar-shapes.ttl` and are loaded into
//! `<urn:tar:shapes>`, so they are queryable, downloadable and dogfoodable by
//! `shacl-manager`. What runs on the write path in this prototype is a hand-written subset of
//! the same constraints — cardinality, node kind, and the enumerations of §6.1 — because
//! there is no mature SHACL engine in Rust yet. The *contract* is the spec's: a rejected
//! write returns `422` with a `sh:ValidationReport` in Turtle, which is what tooling and the
//! form-error mapping in the UI depend on. Swapping the subset for a full engine (or a call
//! into `shacl-manager`) changes nothing above this module.
//!
//! Spec Q6 asks whether validation is blocking or advisory. Here: violations block,
//! warnings never do, and `TAR_SHACL_VALIDATE_WRITES=false` downgrades violations to
//! warnings for an estate that would rather have a half-described artifact than none.

use crate::domain::artifact::{AUTH_METHODS, AVAILABILITIES, PROTOCOLS};
use crate::domain::run::STATUSES;
use crate::model::*;
use crate::ns;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Violation,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub focus: String,
    /// `sh:resultPath` — the UI maps this back to a form field (handoff §5.7).
    pub path: String,
    pub constraint: &'static str,
    pub message: String,
    /// Dotted JSON pointer into the request body, so the form does not have to guess.
    pub field: String,
}

impl Finding {
    fn violation(focus: &str, path: &str, field: &str, constraint: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Violation,
            focus: focus.into(),
            path: path.into(),
            constraint,
            message: message.into(),
            field: field.into(),
        }
    }
    fn warning(focus: &str, path: &str, field: &str, constraint: &'static str, message: impl Into<String>) -> Self {
        Self { severity: Severity::Warning, ..Self::violation(focus, path, field, constraint, message) }
    }
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
        let v: Vec<String> = self.violations().map(|f| format!("{}: {}", f.field, f.message)).collect();
        if v.is_empty() {
            "no violations".into()
        } else {
            v.join("; ")
        }
    }

    /// Serialise as a `sh:ValidationReport` in Turtle — the same shape `shacl-manager` emits.
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
            s.push_str(&format!("        sh:resultPath <{}> ;\n", f.path));
            s.push_str(&format!("        sh:sourceConstraintComponent sh:{} ;\n", f.constraint));
            s.push_str(&format!("        tar:jsonField \"{}\" ;\n", escape(&f.field)));
            s.push_str(&format!("        sh:resultMessage \"{}\"\n    ] ", escape(&f.message)));
        }
        s.push_str(".\n");
        s
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn is_iri(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://") || s.starts_with("urn:")
}

fn in_set(value: &str, allowed: &[&str]) -> bool {
    allowed.contains(&value)
}

// ------------------------------------------------------------------ shapes

pub fn validate_software(focus: &str, input: &SoftwareIn) -> Report {
    let mut r = Report::default();
    if input.name.trim().is_empty() {
        r.findings.push(Finding::violation(
            focus,
            &format!("{}name", ns::SCHEMA),
            "name",
            "MinCountConstraintComponent",
            "Software needs a name",
        ));
    }
    for (value, path, field) in [
        (&input.homepage, "url", "homepage"),
        (&input.code_repository, "codeRepository", "code_repository"),
        (&input.documentation, "softwareHelp", "documentation"),
    ] {
        if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
            if !is_iri(v) {
                r.findings.push(Finding::violation(
                    focus,
                    &format!("{}{path}", ns::SCHEMA),
                    field,
                    "NodeKindConstraintComponent",
                    format!("{field} must be an absolute IRI, got {v:?}"),
                ));
            }
        }
    }
    if let Some(l) = input.license.as_deref().filter(|v| !v.is_empty()) {
        if !is_iri(l) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}license", ns::DCT),
                "license",
                "NodeKindConstraintComponent",
                "license must be an SPDX IRI such as https://spdx.org/licenses/Apache-2.0",
            ));
        }
    } else {
        r.findings.push(Finding::warning(
            focus,
            &format!("{}license", ns::DCT),
            "license",
            "MinCountConstraintComponent",
            "no licence declared — FAIR R1.1 asks for one",
        ));
    }
    if let Some(k) = input.kind.as_deref().filter(|v| !v.is_empty()) {
        if !in_set(k, &["service", "library", "cli", "workflow"]) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}kind", ns::TAR),
                "kind",
                "InConstraintComponent",
                format!("kind must be one of service, library, cli, workflow — got {k:?}"),
            ));
        }
    }
    for (i, t) in input.edam_topics.iter().enumerate() {
        if !is_iri(t) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}subject", ns::DCT),
                &format!("edam_topics[{i}]"),
                "NodeKindConstraintComponent",
                format!("topic must be an IRI, got {t:?}"),
            ));
        }
    }
    if let Some(c) = &input.capability {
        r.findings.extend(validate_capability(focus, c).findings);
    }
    r
}

pub fn validate_capability(focus: &str, c: &CapabilityIn) -> Report {
    let mut r = Report::default();
    for (list, path, field) in [(&c.produces, "produces", "capability.produces"), (&c.consumes, "consumes", "capability.consumes")] {
        for (i, t) in list.iter().enumerate() {
            if !is_iri(t) {
                r.findings.push(Finding::violation(
                    focus,
                    &format!("{}{path}", ns::TAR),
                    &format!("{field}[{i}]"),
                    "NodeKindConstraintComponent",
                    format!("an ArtifactType must be an IRI (EDAM recommended), got {t:?}"),
                ));
            }
        }
    }
    r
}

pub fn validate_instance(focus: &str, input: &InstanceIn) -> Report {
    let mut r = Report::default();
    if input.label.trim().is_empty() {
        r.findings.push(Finding::violation(
            focus,
            &format!("{}label", ns::RDFS),
            "label",
            "MinCountConstraintComponent",
            "Instance needs a label",
        ));
    }
    if let Some(e) = input.endpoint_url.as_deref().filter(|v| !v.is_empty()) {
        if !is_iri(e) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}endpointURL", ns::DCAT),
                "endpoint_url",
                "NodeKindConstraintComponent",
                "endpoint_url must be an absolute IRI",
            ));
        }
    }
    if input.software.is_none() && input.release.is_none() {
        r.findings.push(Finding::violation(
            focus,
            &format!("{}instanceOf", ns::TAR),
            "software",
            "MinCountConstraintComponent",
            "an Instance must name the Software it deploys, or the Release it runs",
        ));
    }
    if let Some(a) = input.availability.as_deref().filter(|v| !v.is_empty()) {
        if !in_set(a, &AVAILABILITIES) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}availability", ns::TAR),
                "availability",
                "InConstraintComponent",
                format!("availability must be one of {}", AVAILABILITIES.join(", ")),
            ));
        }
    }
    for (i, s) in input.allowed_scopes.iter().enumerate() {
        if !crate::auth::ALL_SCOPES.contains(&s.as_str()) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}allowedScope", ns::TAR),
                &format!("allowed_scopes[{i}]"),
                "InConstraintComponent",
                format!("unknown scope {s:?}; known scopes are {}", crate::auth::ALL_SCOPES.join(", ")),
            ));
        }
    }
    if input.oidc_issuer.is_some() && input.oidc_client_id.is_none() {
        r.findings.push(Finding::violation(
            focus,
            &format!("{}oidcClientId", ns::TAR),
            "oidc_client_id",
            "MinCountConstraintComponent",
            "an OIDC issuer without a client id binds nothing — give the client id this deployment authenticates with",
        ));
    }
    if let Some(c) = &input.capability {
        r.findings.extend(validate_capability(focus, c).findings);
    }
    r
}

pub fn validate_artifact(focus: &str, input: &ArtifactIn) -> Report {
    let mut r = Report::default();
    if input.title.as_deref().unwrap_or("").trim().is_empty() {
        r.findings.push(Finding::violation(
            focus,
            &format!("{}title", ns::DCT),
            "title",
            "MinCountConstraintComponent",
            "an Artifact needs a title",
        ));
    }
    if input.conforms_to.as_deref().unwrap_or("").is_empty() {
        r.findings.push(Finding::warning(
            focus,
            &format!("{}conformsTo", ns::DCT),
            "conforms_to",
            "MinCountConstraintComponent",
            "no ArtifactType declared — the artifact will not appear in capability matchmaking",
        ));
    } else if !is_iri(input.conforms_to.as_deref().unwrap_or("")) {
        r.findings.push(Finding::violation(
            focus,
            &format!("{}conformsTo", ns::DCT),
            "conforms_to",
            "NodeKindConstraintComponent",
            "conforms_to must be an IRI (EDAM recommended)",
        ));
    }
    for (i, d) in input.was_derived_from.iter().enumerate() {
        if !is_iri(d) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}wasDerivedFrom", ns::PROV),
                &format!("was_derived_from[{i}]"),
                "NodeKindConstraintComponent",
                "lineage links must be IRIs — a foreign registry's IRI is fine and is the point",
            ));
        }
    }
    if input.distributions.is_empty() {
        r.findings.push(Finding::warning(
            focus,
            &format!("{}distribution", ns::DCAT),
            "distributions",
            "MinCountConstraintComponent",
            "no distribution: this artifact is metadata-only, which is valid but must be deliberate (spec §6.2)",
        ));
    }
    for (i, d) in input.distributions.iter().enumerate() {
        r.findings.extend(validate_distribution(focus, d, i).findings);
    }
    r
}

fn validate_distribution(focus: &str, d: &DistributionIn, i: usize) -> Report {
    let mut r = Report::default();
    let f = |name: &str| format!("distributions[{i}].{name}");
    let availability = d.availability.as_deref().unwrap_or("public");
    if !in_set(availability, &AVAILABILITIES) {
        r.findings.push(Finding::violation(
            focus,
            &format!("{}availability", ns::TAR),
            &f("availability"),
            "InConstraintComponent",
            format!("availability must be one of {}", AVAILABILITIES.join(", ")),
        ));
    }
    if let Some(p) = d.access_protocol.as_deref().filter(|v| !v.is_empty()) {
        if !in_set(p, &PROTOCOLS) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}accessProtocol", ns::TAR),
                &f("access_protocol"),
                "InConstraintComponent",
                format!("access_protocol must be one of {}", PROTOCOLS.join(", ")),
            ));
        }
    }
    if let Some(m) = d.auth_method.as_deref().filter(|v| !v.is_empty()) {
        if !in_set(m, &AUTH_METHODS) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}authMethod", ns::TAR),
                &f("auth_method"),
                "InConstraintComponent",
                format!("auth_method must be one of {}", AUTH_METHODS.join(", ")),
            ));
        }
    }
    if availability == "metadata-only" {
        if d.download_url.as_deref().is_some_and(|v| !v.is_empty()) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}downloadURL", ns::DCAT),
                &f("download_url"),
                "MaxCountConstraintComponent",
                "a metadata-only distribution carries no downloadURL (spec §6.2)",
            ));
        }
        if d.access_request_url.as_deref().unwrap_or("").is_empty() {
            r.findings.push(Finding::warning(
                focus,
                &format!("{}accessRequestURL", ns::TAR),
                &f("access_request_url"),
                "MinCountConstraintComponent",
                "metadata-only without an access_request_url leaves no way to ask for the data",
            ));
        }
    } else if d.access_url.as_deref().unwrap_or("").is_empty() && d.download_url.as_deref().unwrap_or("").is_empty() {
        r.findings.push(Finding::violation(
            focus,
            &format!("{}accessURL", ns::DCAT),
            &f("access_url"),
            "MinCountConstraintComponent",
            "a distribution needs an access_url or a download_url unless it is metadata-only",
        ));
    }
    if availability != "public" && d.access_request_url.as_deref().unwrap_or("").is_empty() {
        r.findings.push(Finding::warning(
            focus,
            &format!("{}accessRequestURL", ns::TAR),
            &f("access_request_url"),
            "MinCountConstraintComponent",
            "restricted or embargoed data should say where access is requested (FAIR A1.2)",
        ));
    }
    if let Some(c) = &d.checksum {
        if c.value.trim().is_empty() {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}checksumValue", ns::SPDX),
                &f("checksum.value"),
                "MinCountConstraintComponent",
                "a checksum needs a value",
            ));
        }
    }
    r
}

pub fn validate_run(focus: &str, input: &RunIn) -> Report {
    let mut r = Report::default();
    if let Some(s) = input.status.as_deref().filter(|v| !v.is_empty()) {
        if !in_set(s, &STATUSES) {
            r.findings.push(Finding::violation(
                focus,
                &format!("{}status", ns::TAR),
                "run.status",
                "InConstraintComponent",
                format!("status must be one of {}", STATUSES.join(", ")),
            ));
        }
    }
    for (v, field, path) in [
        (&input.started_at, "run.started_at", "startedAtTime"),
        (&input.ended_at, "run.ended_at", "endedAtTime"),
    ] {
        if let Some(t) = v.as_deref().filter(|x| !x.is_empty()) {
            if chrono::DateTime::parse_from_rfc3339(t).is_err() {
                r.findings.push(Finding::violation(
                    focus,
                    &format!("{}{path}", ns::PROV),
                    field,
                    "DatatypeConstraintComponent",
                    format!("{field} must be an RFC 3339 timestamp, got {t:?}"),
                ));
            }
        }
    }
    if let (Some(s), Some(e)) = (&input.started_at, &input.ended_at) {
        if let (Ok(s), Ok(e)) = (chrono::DateTime::parse_from_rfc3339(s), chrono::DateTime::parse_from_rfc3339(e)) {
            if e < s {
                r.findings.push(Finding::violation(
                    focus,
                    &format!("{}endedAtTime", ns::PROV),
                    "run.ended_at",
                    "LessThanConstraintComponent",
                    "a run cannot end before it starts",
                ));
            }
        }
    }
    r
}

/// Turn a report into the `422` of spec §7.9, or pass the write through.
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

    #[test]
    fn a_nameless_software_is_rejected() {
        let r = validate_software("urn:new", &SoftwareIn::default());
        assert!(!r.conforms());
        assert!(r.to_turtle().contains("sh:ValidationReport"));
        assert!(r.to_turtle().contains("sh:conforms false"));
        assert!(r.to_turtle().contains("MinCountConstraintComponent"));
    }

    #[test]
    fn a_missing_licence_warns_but_does_not_block() {
        let input = SoftwareIn { name: "shacl-manager".into(), ..Default::default() };
        let r = validate_software("urn:new", &input);
        assert!(r.conforms(), "missing licence must not block a write");
        assert!(r.findings.iter().any(|f| f.severity == Severity::Warning));
    }

    #[test]
    fn metadata_only_must_not_carry_a_download_url() {
        let input = ArtifactIn {
            title: Some("masked replica".into()),
            conforms_to: Some("http://edamontology.org/data_2600".into()),
            distributions: vec![DistributionIn {
                availability: Some("metadata-only".into()),
                download_url: Some("https://example.org/data.ttl".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = validate_artifact("urn:new", &input);
        assert!(!r.conforms());
        assert!(r.violations().any(|f| f.field == "distributions[0].download_url"));
    }

    #[test]
    fn enum_values_from_the_spec_are_enforced() {
        let input = ArtifactIn {
            title: Some("x".into()),
            conforms_to: Some("http://edamontology.org/data_2048".into()),
            distributions: vec![DistributionIn {
                access_url: Some("https://example.org".into()),
                access_protocol: Some("carrier-pigeon".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let r = validate_artifact("urn:new", &input);
        assert!(r.violations().any(|f| f.field == "distributions[0].access_protocol"));
    }

    #[test]
    fn advisory_mode_lets_a_bad_write_through() {
        let r = validate_software("urn:new", &SoftwareIn::default());
        assert!(enforce(r, false).is_ok());
        let r = validate_software("urn:new", &SoftwareIn::default());
        assert!(enforce(r, true).is_err());
    }
}
