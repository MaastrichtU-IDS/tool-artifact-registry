//! Instance projections (spec §4.2, D5). An Instance is a *deployment*: the agent that acts,
//! the thing runs are attributed to, and — with the workload-identity model — the thing an
//! OIDC client id is bound to.

use super::{agent_quads, Ctx};
use crate::error::{AppError, AppResult};
use crate::ids;
use crate::model::*;
use crate::ns;
use crate::rdf::{Node, Props};
use crate::store::GraphTx;
use oxigraph::model::Quad;
use std::collections::HashMap;

pub const TYPE_SOFTWARE_AGENT: &str = "http://www.w3.org/ns/prov#SoftwareAgent";
pub const TYPE_TAR_INSTANCE: &str = "https://w3id.org/tar/ns#Instance";
pub const TYPE_DATA_SERVICE: &str = "http://www.w3.org/ns/dcat#DataService";

pub fn instance_quads(base: &str, iri: &str, input: &InstanceIn, actor: &str, software: Option<&str>) -> Vec<Quad> {
    let mut quads = Vec::new();
    let mut n = Node::local(iri);
    n.a(TYPE_SOFTWARE_AGENT);
    n.a(TYPE_TAR_INSTANCE);
    // Only a deployment that serves an endpoint is a dcat:DataService (spec §4.2). An
    // Instance without one is a laptop or a batch job, and that is normal (handoff §5.3).
    if input.endpoint_url.as_deref().is_some_and(|s| !s.is_empty()) {
        n.a(TYPE_DATA_SERVICE);
    }
    n.text(ns::RDFS, "label", &input.label);
    n.opt_text(ns::DCT, "description", &input.description);
    n.opt_link(ns::TAR, "runsRelease", &input.release);
    if let Some(sw) = software.or(input.software.as_deref()) {
        // Denormalised: the Software is derivable through the Release, but an Instance may
        // exist before any Release does, and every list query filters on it.
        n.link(ns::TAR, "instanceOf", sw);
    }
    n.opt_link(ns::DCAT, "endpointURL", &input.endpoint_url);
    n.opt_link(ns::DCAT, "endpointDescription", &input.endpoint_description);
    // Where the registry probes. Declared separately from the endpoint because a deployment
    // that serves an API at `/` and health at `/healthz` should not have its liveness judged
    // by whatever `/` happens to return.
    n.opt_link(ns::TAR, "healthEndpoint", &input.health_endpoint);
    n.opt_text(ns::TAR, "selfRegisteredBy", &input.self_registered_by);
    n.opt_text(ns::TAR, "instanceKey", &input.instance_key);
    n.opt_text(ns::TAR, "availability", &input.availability);
    // Supplement (audit 2026-08-30): dct:accessRights with the EU access-right authority
    // table is what DCAT-AP harvesters read; tar:availability keeps the finer four-way enum.
    if let Some(ar) = input.availability.as_deref().and_then(super::artifact::access_right) {
        n.link(ns::DCT, "accessRights", &ar);
    }
    n.opt_text(ns::TAR, "jurisdiction", &input.jurisdiction);
    n.opt_text(ns::TAR, "oidcClientId", &input.oidc_client_id);
    n.opt_text(ns::TAR, "oidcIssuer", &input.oidc_issuer);
    n.texts(ns::TAR, "allowedScope", &input.allowed_scopes);
    // Audit 2026-08-30: dcat:inCatalog (DCAT 3) replaces tar:homeRegistry. Its scope note
    // requires the inverse dcat:resource on the catalog side, so both edges are written.
    n.link(ns::DCAT, "inCatalog", base);
    let mut cat = Node::local(base);
    cat.a(&format!("{}Catalog", ns::DCAT));
    cat.link(ns::DCAT, "resource", iri);
    quads.extend(cat.finish());
    crate::rdf::attribution(&mut n, actor);
    if let Some(op) = &input.operator {
        let (oiri, oq) = agent_quads(base, op);
        if let Some(oi) = oiri {
            n.link(ns::DCT, "publisher", &oi);
        }
        quads.extend(oq);
    }
    if let Some(cap) = &input.capability {
        let cap_iri = ids::mint(base, ids::Kind::Capability);
        quads.extend(super::software::capability_quads(&cap_iri, cap));
        n.link(ns::TAR, "hasCapability", &cap_iri);
    }
    quads.extend(n.finish());
    quads
}

pub fn replace_instance(base: &str, iri: &str, input: &InstanceIn, actor: &str, software: Option<&str>) -> GraphTx {
    let mut tx = GraphTx::new();
    tx.replace_subject(iri, ns::G_LOCAL);
    tx.extend(instance_quads(base, iri, input, actor, software));
    tx
}

pub fn instance_from_props(ctx: &Ctx, iri: &str, p: &Props) -> Instance {
    Instance {
        iri: iri.to_string(),
        id: ids::local_id(ctx.base(), iri).map(|(_, i)| i).unwrap_or_else(|| ids::iri_tail(iri).to_string()),
        label: p.str(ns::RDFS, "label").unwrap_or_else(|| ids::iri_tail(iri).to_string()),
        description: p.str(ns::DCT, "description"),
        software: p.iri(ns::TAR, "instanceOf"),
        software_name: None,
        release: p.iri(ns::TAR, "runsRelease"),
        release_version: None,
        outdated: false,
        latest_version: None,
        endpoint_url: p.iri(ns::DCAT, "endpointURL"),
        endpoint_description: p.iri(ns::DCAT, "endpointDescription"),
        operator: ctx.opt_agent_ref(p.iri(ns::DCT, "publisher")),
        availability: p.str(ns::TAR, "availability"),
        jurisdiction: p.str(ns::TAR, "jurisdiction"),
        health: p.str(ns::TAR, "health").unwrap_or_else(|| "unknown".into()),
        health_checked_at: p.str(ns::TAR, "healthCheckedAt"),
        health_detail: p.str(ns::TAR, "healthDetail"),
        health_endpoint: p.iri(ns::TAR, "healthEndpoint"),
        self_registered_by: p.str(ns::TAR, "selfRegisteredBy"),
        instance_key: p.str(ns::TAR, "instanceKey"),
        last_seen_at: p.str(ns::TAR, "lastSeenAt"),
        home_registry: p.iri(ns::DCAT, "inCatalog").or_else(|| p.iri(ns::TAR, "homeRegistry")),
        capability: p.iri(ns::TAR, "hasCapability").and_then(|c| super::software::capability_from(ctx, &c, "instance")),
        last_run_at: None,
        runs_30d: 0,
        failures_30d: 0,
        artifact_count: 0,
        oidc_client_id: p.str(ns::TAR, "oidcClientId"),
        oidc_issuer: p.str(ns::TAR, "oidcIssuer"),
        allowed_scopes: p.strs(ns::TAR, "allowedScope"),
        token_count: 0,
        origin: ctx.origin(p.graph.as_deref()),
        tombstoned: p.bool(ns::TAR, "tombstoned").unwrap_or(false),
    }
}

#[derive(Default, Clone)]
pub struct InstanceSignals {
    pub last_run_at: Option<String>,
    pub runs_30d: i64,
    pub failures_30d: i64,
    pub artifacts: i64,
}

/// Run and artifact signals per Instance, aggregated (handoff §4.2 signal bar).
pub fn instance_signals(ctx: &Ctx, only: Option<&str>) -> AppResult<HashMap<String, InstanceSignals>> {
    let filter = only.map(|s| format!("FILTER(?i = <{s}>)")).unwrap_or_default();
    let mut out: HashMap<String, InstanceSignals> = HashMap::new();
    let since = super::thirty_days_ago();

    let q = format!(
        r#"{p}
SELECT ?i (MAX(?t) AS ?last) (COUNT(DISTINCT ?run) AS ?n) WHERE {{
  GRAPH ?g {{ ?run a prov:Activity ; prov:wasAssociatedWith|tar:atInstance ?i . OPTIONAL {{ ?run prov:startedAtTime ?t }} }}
  {filter}
}} GROUP BY ?i"#,
        p = ns::PREFIXES
    );
    for row in ctx.state.store.select(&q).map_err(AppError::from)?.rows {
        if let Some(i) = row.iri("i") {
            let e = out.entry(i).or_default();
            e.last_run_at = row.str("last");
        }
    }
    let q = format!(
        r#"{p}
SELECT ?i (COUNT(DISTINCT ?run) AS ?n) (SUM(?failed) AS ?f) WHERE {{
  GRAPH ?g {{ ?run a prov:Activity ; prov:wasAssociatedWith|tar:atInstance ?i ; prov:startedAtTime ?t . OPTIONAL {{ ?run tar:status ?st }} }}
  FILTER(?t >= "{since}"^^xsd:dateTime)
  BIND(IF(?st = "failed" || ?st = "aborted", 1, 0) AS ?failed)
  {filter}
}} GROUP BY ?i"#,
        p = ns::PREFIXES
    );
    for row in ctx.state.store.select(&q).map_err(AppError::from)?.rows {
        if let Some(i) = row.iri("i") {
            let e = out.entry(i).or_default();
            e.runs_30d = row.i64("n").unwrap_or(0);
            e.failures_30d = row.i64("f").unwrap_or(0);
        }
    }
    let q = format!(
        r#"{p}
SELECT ?i (COUNT(DISTINCT ?a) AS ?n) WHERE {{
  GRAPH ?g {{ ?run prov:wasAssociatedWith|tar:atInstance ?i . ?a prov:wasGeneratedBy ?run }}
  {filter}
}} GROUP BY ?i"#,
        p = ns::PREFIXES
    );
    for row in ctx.state.store.select(&q).map_err(AppError::from)?.rows {
        if let Some(i) = row.iri("i") {
            out.entry(i).or_default().artifacts = row.i64("n").unwrap_or(0);
        }
    }
    Ok(out)
}

/// Fill in software/release labels and the "outdated release" marker (handoff §5.2).
pub fn decorate(ctx: &Ctx, items: &mut [Instance]) -> AppResult<()> {
    let mut wanted: Vec<String> = Vec::new();
    for i in items.iter() {
        wanted.extend(i.software.clone());
        wanted.extend(i.release.clone());
    }
    let labels = ctx.labels(&wanted);
    // Latest release version per software, for the outdated marker.
    let mut latest: HashMap<String, (String, String)> = HashMap::new();
    let q = format!(
        r#"{p}
SELECT ?sw ?r ?v ?d WHERE {{ GRAPH ?g {{ ?r dct:isVersionOf ?sw ; schema:softwareVersion ?v . OPTIONAL {{ ?r schema:datePublished ?d }} }} }}"#,
        p = ns::PREFIXES
    );
    for row in ctx.state.store.select(&q).map_err(AppError::from)?.rows {
        let (Some(sw), Some(r), Some(v)) = (row.iri("sw"), row.iri("r"), row.str("v")) else { continue };
        let key = row.str("d").unwrap_or_else(|| r.clone());
        latest
            .entry(sw)
            .and_modify(|cur| {
                if key > cur.0 {
                    *cur = (key.clone(), v.clone());
                }
            })
            .or_insert((key, v));
    }
    for i in items.iter_mut() {
        i.software_name = i.software.as_ref().and_then(|s| labels.get(s).cloned());
        i.release_version = i.release.as_ref().and_then(|r| labels.get(r).cloned());
        if let (Some(sw), Some(v)) = (&i.software, &i.release_version) {
            if let Some((_, latest_v)) = latest.get(sw) {
                i.latest_version = Some(latest_v.clone());
                i.outdated = latest_v != v;
            }
        }
    }
    Ok(())
}

pub fn load_instance(ctx: &Ctx, iri: &str) -> AppResult<Instance> {
    let quads = ctx.state.store.describe(iri).map_err(AppError::from)?;
    if quads.is_empty() {
        return Err(AppError::not_found(format!("no instance at {iri}")));
    }
    let p = Props::from_quads(iri, &quads);
    if !p.has_type(TYPE_SOFTWARE_AGENT) {
        return Err(AppError::not_found(format!("{iri} is not an Instance record")));
    }
    let mut inst = instance_from_props(ctx, iri, &p);
    let signals = instance_signals(ctx, Some(iri))?;
    if let Some(s) = signals.get(iri) {
        inst.last_run_at = s.last_run_at.clone();
        inst.runs_30d = s.runs_30d;
        inst.failures_30d = s.failures_30d;
        inst.artifact_count = s.artifacts;
    }
    let mut one = [inst];
    decorate(ctx, &mut one)?;
    let [inst] = one;
    Ok(inst)
}
