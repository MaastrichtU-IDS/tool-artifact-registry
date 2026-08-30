//! Run projections (spec §4.3).
//!
//! `prov:qualifiedAssociation` binds *who acted* (the Instance) to *what plan they followed*
//! (the Release) in one reified node. `prov:wasAssociatedWith` — PROV-O's own unqualified
//! form of exactly that association — sits alongside it so that list and count queries stay
//! one-hop. (It replaced the invented `tar:atInstance` in the 2026-08-30 vocabulary audit;
//! readers still fall back to the old term for pre-audit graphs.)

use super::Ctx;
use crate::error::{AppError, AppResult};
use crate::ids;
use crate::model::*;
use crate::ns;
use crate::rdf::{Node, Props};
use oxigraph::model::Quad;

pub const TYPE_ACTIVITY: &str = "http://www.w3.org/ns/prov#Activity";
pub const TYPE_SCHEMA_ACTION: &str = "https://schema.org/Action";
pub const STATUSES: [&str; 4] = ["success", "failed", "running", "aborted"];

/// The schema.org ActionStatusType IRI for a run status (audit 2026-08-30). Written as a
/// supplement beside `tar:status`: the standard enumeration has no distinct value for
/// "aborted" (it folds into FailedActionStatus), so the tar literal stays authoritative.
pub fn action_status(status: &str) -> Option<String> {
    let local = match status {
        "success" => "CompletedActionStatus",
        "failed" | "aborted" => "FailedActionStatus",
        "running" => "ActiveActionStatus",
        _ => return None,
    };
    Some(format!("{}{}", ns::SCHEMA, local))
}

pub fn run_quads(iri: &str, input: &RunIn, instance_iri: &str, actor: &str) -> Vec<Quad> {
    let mut n = Node::local(iri);
    n.a(TYPE_ACTIVITY);
    // Also a schema:Action, so the schema:actionStatus supplement sits on its intended type.
    n.a(TYPE_SCHEMA_ACTION);
    n.opt_text(ns::RDFS, "label", &input.label);
    n.datetime(ns::PROV, "startedAtTime", input.started_at.as_deref().unwrap_or(&chrono::Utc::now().to_rfc3339()));
    n.opt_datetime(ns::PROV, "endedAtTime", &input.ended_at);
    let status = input.status.as_deref().unwrap_or("success");
    n.text(ns::TAR, "status", status);
    if let Some(st) = action_status(status) {
        n.link(ns::SCHEMA, "actionStatus", &st);
    }
    // Audit 2026-08-30: dct:identifier ("an unambiguous reference to the resource within a
    // given context") carries the caller's own run key; tar:externalKey is retired.
    n.opt_text(ns::DCT, "identifier", &input.external_key);
    // The unqualified form of the association below — PROV-O's own one-hop shortcut.
    n.link(ns::PROV, "wasAssociatedWith", instance_iri);
    n.opt_link(ns::TAR, "usedRelease", &input.release);

    let mut assoc = Node::blank(ns::G_LOCAL);
    assoc.a(&format!("{}Association", ns::PROV));
    assoc.link(ns::PROV, "agent", instance_iri);
    if let Some(r) = &input.release {
        assoc.link(ns::PROV, "hadPlan", r);
    }
    n.child(ns::PROV, "qualifiedAssociation", assoc);
    crate::rdf::attribution(&mut n, actor);
    n.finish()
}

pub fn run_summary_from_props(ctx: &Ctx, iri: &str, p: &Props) -> RunSummary {
    let started = p.str(ns::PROV, "startedAtTime");
    let ended = p.str(ns::PROV, "endedAtTime");
    let duration = match (&started, &ended) {
        (Some(s), Some(e)) => {
            match (chrono::DateTime::parse_from_rfc3339(s), chrono::DateTime::parse_from_rfc3339(e)) {
                (Ok(s), Ok(e)) => Some((e - s).num_seconds()),
                _ => None,
            }
        }
        _ => None,
    };
    RunSummary {
        iri: iri.to_string(),
        id: ids::local_id(ctx.base(), iri).map(|(_, i)| i).unwrap_or_else(|| ids::iri_tail(iri).to_string()),
        label: p.str(ns::RDFS, "label"),
        status: p.str(ns::TAR, "status").unwrap_or_else(|| "unknown".into()),
        started_at: started,
        ended_at: ended,
        duration_seconds: duration,
        external_key: p.str(ns::DCT, "identifier").or_else(|| p.str(ns::TAR, "externalKey")),
        instance: p.iri(ns::PROV, "wasAssociatedWith").or_else(|| p.iri(ns::TAR, "atInstance")),
        instance_label: None,
        release: p.iri(ns::TAR, "usedRelease"),
        release_version: None,
        software: None,
        software_name: None,
        used_count: p.iris(ns::PROV, "used").len() as i64,
        generated_count: 0,
        origin: ctx.origin(p.graph.as_deref()),
    }
}

fn generated_iris(ctx: &Ctx, run_iri: &str) -> Vec<String> {
    let q = format!(
        r#"{p}
SELECT ?a WHERE {{ GRAPH ?g {{ ?a prov:wasGeneratedBy <{run_iri}> }} }}"#,
        p = ns::PREFIXES
    );
    ctx.state
        .store
        .select(&q)
        .map(|b| b.rows.iter().filter_map(|r| r.iri("a")).collect())
        .unwrap_or_default()
}

pub fn load_run_summary(ctx: &Ctx, iri: &str) -> AppResult<RunSummary> {
    let quads = ctx.state.store.describe(iri).map_err(AppError::from)?;
    if quads.is_empty() {
        return Err(AppError::not_found(format!("no run at {iri}")));
    }
    let p = Props::from_quads(iri, &quads);
    let mut s = run_summary_from_props(ctx, iri, &p);
    s.generated_count = generated_iris(ctx, iri).len() as i64;
    let mut one = [s];
    decorate(ctx, &mut one)?;
    let [s] = one;
    Ok(s)
}

pub fn load_run(ctx: &Ctx, iri: &str) -> AppResult<Run> {
    let quads = ctx.state.store.describe(iri).map_err(AppError::from)?;
    if quads.is_empty() {
        return Err(AppError::not_found(format!("no run at {iri}")));
    }
    let p = Props::from_quads(iri, &quads);
    let mut summary = run_summary_from_props(ctx, iri, &p);
    let used: Vec<ArtifactRef> = p.iris(ns::PROV, "used").iter().map(|a| super::artifact::artifact_ref(ctx, a)).collect();
    let generated: Vec<ArtifactRef> =
        generated_iris(ctx, iri).iter().map(|a| super::artifact::artifact_ref(ctx, a)).collect();
    summary.used_count = used.len() as i64;
    summary.generated_count = generated.len() as i64;
    let mut one = [summary];
    decorate(ctx, &mut one)?;
    let [summary] = one;
    Ok(Run {
        summary,
        used,
        generated,
        openlineage_payload: p.str(ns::TAR, "openLineagePayload").and_then(|s| serde_json::from_str(&s).ok()),
    })
}

/// Fill instance/release/software labels for a batch of runs.
pub fn decorate(ctx: &Ctx, runs: &mut [RunSummary]) -> AppResult<()> {
    let mut wanted: Vec<String> = Vec::new();
    for r in runs.iter() {
        wanted.extend(r.instance.clone());
        wanted.extend(r.release.clone());
    }
    let labels = ctx.labels(&wanted);
    // instance -> software, in one query.
    let q = format!(
        r#"{p}
SELECT ?i ?sw WHERE {{ GRAPH ?g {{ ?i tar:instanceOf ?sw }} }}"#,
        p = ns::PREFIXES
    );
    let mut inst_sw = std::collections::HashMap::new();
    for row in ctx.state.store.select(&q).map_err(AppError::from)?.rows {
        if let (Some(i), Some(sw)) = (row.iri("i"), row.iri("sw")) {
            inst_sw.insert(i, sw);
        }
    }
    let sw_labels = ctx.labels(&inst_sw.values().cloned().collect::<Vec<_>>());
    for r in runs.iter_mut() {
        r.instance_label = r.instance.as_ref().and_then(|i| labels.get(i).cloned());
        r.release_version = r.release.as_ref().and_then(|x| labels.get(x).cloned());
        r.software = r.instance.as_ref().and_then(|i| inst_sw.get(i).cloned());
        r.software_name = r.software.as_ref().and_then(|s| sw_labels.get(s).cloned());
    }
    Ok(())
}
