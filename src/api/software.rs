//! Software, Release and Capability endpoints (spec §7.2, §7.3).

use super::{count, page_iris, resource_response, Paging};
use crate::auth::Principal;
use crate::domain::{software as dom, Ctx};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::model::*;
use crate::negotiate::{Repr, Signposting};
use crate::ns;
use crate::rdf::Props;
use crate::shacl;
use crate::state::AppState;
use crate::store::GraphTx;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize, Default)]
pub struct SoftwareFilter {
    pub q: Option<String>,
    pub license: Option<String>,
    pub publisher: Option<String>,
    pub topic: Option<String>,
    pub keyword: Option<String>,
    pub kind: Option<String>,
    /// Matchmaking passthrough: software whose capability produces/consumes a type.
    pub produces: Option<String>,
    pub consumes: Option<String>,
    pub registry: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

fn where_body(f: &SoftwareFilter) -> String {
    let mut w = format!(
        "GRAPH ?g {{ ?s a <{t}> ; schema:name ?name . \
         OPTIONAL {{ ?s dct:abstract|tar:tagline ?tagline }} OPTIONAL {{ ?s schema:description ?desc }} }}\n\
         FILTER NOT EXISTS {{ GRAPH ?tg {{ ?s tar:tombstoned true }} }}",
        t = dom::TYPE_SOFTWARE
    );
    if let Some(q) = &f.q {
        w.push('\n');
        w.push_str(&super::text_filter(q, &["?name", "?tagline", "?desc"]));
    }
    for (value, pattern) in [
        (&f.license, "GRAPH ?g {{ ?s dct:license <{v}> }}"),
        (&f.publisher, "GRAPH ?g {{ ?s dct:publisher <{v}> }}"),
        (&f.topic, "GRAPH ?g {{ ?s dct:subject <{v}> }}"),
        (&f.produces, "GRAPH ?g {{ ?s tar:hasCapability/tar:produces <{v}> }}"),
        (&f.consumes, "GRAPH ?g {{ ?s tar:hasCapability/tar:consumes <{v}> }}"),
    ] {
        if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
            w.push('\n');
            w.push_str(&pattern.replace("{{", "{").replace("}}", "}").replace("{v}", v));
        }
    }
    if let Some(k) = f.keyword.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!("\nGRAPH ?g {{ ?s schema:keywords \"{}\" }}", super::escape_literal(k)));
    }
    if let Some(k) = f.kind.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!(
            "\nGRAPH ?g {{ ?s schema:applicationCategory|tar:kind \"{}\" }}",
            super::escape_literal(k)
        ));
    }
    match f.registry.as_deref() {
        Some("local") => w.push_str(&format!("\nFILTER(?g = <{}>)", ns::G_LOCAL)),
        Some(peer) if !peer.is_empty() => w.push_str(&format!("\nFILTER(?g = <{}>)", ns::peer_graph(peer))),
        _ => {}
    }
    w.push_str(&format!("\n{}", f.paging.cursor_filter("?s")));
    w
}

pub async fn list(State(state): State<Arc<AppState>>, Query(f): Query<SoftwareFilter>) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let body = where_body(&f);
    let (iris, next) = page_iris(&state, &body, &f.paging)?;
    let total = count(&state, &body)?;
    let counts = dom::software_counts(&ctx, None)?;
    let mut items = Vec::new();
    for iri in iris {
        let quads = state.store.describe(&iri).map_err(AppError::from)?;
        let p = Props::from_quads(&iri, &quads);
        let mut sw = dom::software_from_props(&ctx, &iri, &p);
        if let Some(c) = counts.get(&iri) {
            sw.instance_count = c.instances;
            sw.runs_30d = c.runs_30d;
        }
        items.push(sw);
    }
    let mut page = Page::new(items, total, next);
    page.facets = facets(&ctx)?;
    Ok(Json(page))
}

fn facets(ctx: &Ctx) -> AppResult<Vec<Facet>> {
    let mut out = Vec::new();
    for (name, predicate) in
        [("license", "dct:license"), ("kind", "schema:applicationCategory|tar:kind"), ("topic", "dct:subject")]
    {
        let q = format!(
            "{p}\nSELECT ?v (COUNT(DISTINCT ?s) AS ?n) WHERE {{ GRAPH ?g {{ ?s a <{t}> ; {predicate} ?v }} }} GROUP BY ?v ORDER BY DESC(?n) LIMIT 25",
            p = ns::PREFIXES,
            t = dom::TYPE_SOFTWARE
        );
        let rows = ctx.state.store.select(&q).map_err(AppError::from)?;
        let raw: Vec<(String, i64)> =
            rows.rows.iter().filter_map(|r| Some((r.str("v")?, r.i64("n").unwrap_or(0)))).collect();
        // Resolve labels from the vocabulary graph, so the facet reads the same as the chip
        // beside it rather than falling back to the IRI's last segment.
        let labels = ctx.type_refs(&raw.iter().map(|(v, _)| v.clone()).collect::<Vec<_>>());
        let values: Vec<FacetValue> = raw
            .into_iter()
            .zip(labels)
            .map(|((value, count), t)| FacetValue {
                label: t.label.or_else(|| Some(ids::iri_tail(&value).to_string())),
                value,
                count,
            })
            .collect();
        if !values.is_empty() {
            out.push(Facet { name: name.to_string(), values });
        }
    }
    Ok(out)
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    let sw = dom::load_software(&ctx, &iri)?;
    let mut sp = Signposting::new(&iri).collection(&format!("{}/api/v1/software", state.base()));
    if let Some(l) = &sw.license {
        sp = sp.license(l);
    }
    if let Some(p) = &sw.publisher {
        sp = sp.author(&p.iri);
    }
    if let Some(r) = &sw.code_repository {
        sp = sp.item(r, None);
    }
    Ok(resource_response(&state, &headers, &iri, &sw, sp, Repr::Json).await?)
}

/// Refuse a registration binding that could never authorise anybody.
///
/// `registration_clients` without an issuer is only self-explanatory when the registry has one
/// issuer to read it against. Once several are accepted and none is primary, the record names a
/// client id at no particular provider, and `find_software_for_client` will decline it — so say
/// that here, where the curator is looking at the form, rather than as a 403 the first time the
/// workload calls with a message about a field it never sent.
fn check_registration_binding(state: &AppState, input: &SoftwareIn) -> AppResult<()> {
    if input.registration_clients.is_empty() || input.registration_issuer.is_some() {
        return Ok(());
    }
    if state.config.oidc.issuer_pin_required() {
        return Err(AppError::bad_request(format!(
            "registration_issuer is required: this registry accepts tokens from {}, and a client \
             id is only unique within an issuer. Name the one these registration clients belong to.",
            state.config.oidc.accepted_issuers().join(", ")
        )));
    }
    Ok(())
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(input): Json<SoftwareIn>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    check_registration_binding(&state, &input)?;
    if let Some(sync) = &input.sync {
        crate::domain::forge::check_fields(&sync.fields)?;
    }
    let iri = ids::mint(state.base(), Kind::Software);
    let quads = dom::software_quads(state.base(), &iri, &input, &principal.subject, None);
    shacl::enforce_write(&state, &quads)?;
    let mut tx = GraphTx::new();
    tx.extend(quads);
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "software.create", Some(&iri), Some(&input.name), None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok((StatusCode::CREATED, Json(dom::load_software(&ctx, &iri)?)))
}

/// PATCH merges: the body carries only what changes, and everything else stands.
///
/// It used to take a whole `SoftwareIn`, which made `{"api_docs": [...]}` fail with "missing
/// field `name`" — a replace wearing a PATCH's method. The UI never noticed because its form
/// always sends every field; anything else did.
pub async fn patch(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    if !ids::is_local(state.base(), &iri) {
        return Err(AppError::forbidden("this registry is not authoritative for that IRI (spec §9.7)"));
    }
    let existing = state.store.describe(&iri).map_err(AppError::from)?;
    if existing.is_empty() {
        return Err(AppError::not_found(format!("no software at {iri}")));
    }
    let ctx = Ctx::new(&state).await?;
    let current = software_in_from(&dom::load_software(&ctx, &iri)?);
    let merged = super::instances::merge_json(
        serde_json::to_value(current).map_err(|e| AppError::internal(e.to_string()))?,
        body,
    );
    let input: SoftwareIn = serde_json::from_value(merged)
        .map_err(|e| AppError::bad_request(format!("could not apply the change: {e}")))?;
    check_registration_binding(&state, &input)?;
    if let Some(sync) = &input.sync {
        crate::domain::forge::check_fields(&sync.fields)?;
    }
    let created = Props::from_quads(&iri, &existing).str(ns::DCT, "created");
    let tx = dom::replace_software(state.base(), &iri, &input, &principal.subject, created);
    shacl::enforce_write(&state, &tx.insert)?;
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "software.update", Some(&iri), None, None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok(Json(dom::load_software(&ctx, &iri)?))
}

/// Soft delete (spec §7.2): the IRI keeps resolving and the UI renders a tombstone banner.
pub async fn soft_delete(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    tombstone(&state, &iri, &principal).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn tombstone(state: &AppState, iri: &str, principal: &Principal) -> AppResult<()> {
    let existing = state.store.describe(iri).map_err(AppError::from)?;
    if existing.is_empty() {
        return Err(AppError::not_found(format!("nothing at {iri}")));
    }
    let mut n = crate::rdf::Node::local(iri);
    n.boolean(ns::TAR, "tombstoned", true);
    n.datetime(ns::TAR, "tombstonedAt", &chrono::Utc::now().to_rfc3339());
    // Supplement (audit 2026-08-30): the standard reading of a tombstone. adms:status has an
    // open domain, and WITHDRAWN comes from the EU dataset-status authority table.
    n.link(ns::ADMS, "status", &format!("{}WITHDRAWN", ns::EU_DATASET_STATUS));
    n.link(ns::PROV, "wasAttributedTo", &principal.subject);
    let mut tx = GraphTx::new();
    tx.extend(n.finish());
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state.ops.audit(Some(&principal.subject), principal.actor_kind(), "tombstone", Some(iri), None, None).await;
    Ok(())
}

pub async fn list_releases(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    let releases = dom::list_releases(&ctx, &iri)?;
    let total = releases.len() as i64;
    Ok(Json(Page::new(releases, total, None)))
}

pub async fn create_release(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(input): Json<ReleaseIn>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let software_iri = ids::iri_for(state.base(), Kind::Software, &id);
    if !state.store.exists(&software_iri).map_err(AppError::from)? {
        return Err(AppError::not_found(format!("no software at {software_iri}")));
    }
    let iri = ids::mint(state.base(), Kind::Release);
    let quads = dom::release_quads(state.base(), &iri, &software_iri, &input, &principal.subject);
    shacl::enforce_write(&state, &quads)?;
    let mut tx = GraphTx::new();
    tx.extend(quads);
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(
            Some(&principal.subject),
            principal.actor_kind(),
            "release.create",
            Some(&iri),
            Some(&input.version),
            None,
        )
        .await;
    let ctx = Ctx::new(&state).await?;
    let quads = state.store.describe(&iri).map_err(AppError::from)?;
    let p = Props::from_quads(&iri, &quads);
    Ok((StatusCode::CREATED, Json(dom::release_from_props(&ctx, &iri, &p))))
}

/// Pull the managed fields from the source repository (`POST /api/v1/software/{id}/sync`).
///
/// Only fields the record named as managed are touched. Everything else is the curator's, and
/// stays that way — see `domain::forge` for why that constraint is the whole point.
pub async fn sync(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    let quads = state.store.describe(&iri).map_err(AppError::from)?;
    if quads.is_empty() {
        return Err(AppError::not_found(format!("no software at {iri}")));
    }
    let ctx = Ctx::new(&state).await?;
    let existing = dom::load_software(&ctx, &iri)?;
    let Some(cfg) = existing.sync.clone() else {
        return Err(AppError::bad_request("this record is not connected to a repository — set `sync` on it first"));
    };
    if !cfg.enabled {
        return Err(AppError::bad_request("sync is disabled for this record"));
    }

    // The signed-in curator's own GitHub token, when Keycloak brokered one, so the registry
    // reads exactly what that person can read. Falls back to the registry-wide token.
    // A curator's own brokered GitHub token would go here; until the Keycloak identity-provider
    // wiring lands this is the registry-wide token, which is the weaker of the two options.
    let token = crate::domain::forge::token_for(None);
    let mut input = software_in_from(&existing);
    let outcome = crate::domain::forge::sync_into(&state.http, &cfg.repo, &cfg.fields, token.as_deref(), &mut input)
        .await
        .map_err(|e| AppError::new(StatusCode::BAD_GATEWAY, "forge-unreachable", "Repository sync failed").detail(e));

    let mut status = cfg.clone();
    status.last_synced_at = Some(chrono::Utc::now().to_rfc3339());
    let outcome = match outcome {
        Ok(o) => {
            status.last_status = "ok".into();
            status.last_error = None;
            status.last_changed = o.changed.clone();
            o
        }
        Err(e) => {
            // Record the failure on the record rather than only returning it, so a sync that
            // has been quietly broken for a month is visible on the page.
            status.last_status = "error".into();
            status.last_error = e.detail.clone();
            status.last_changed = Vec::new();
            write_sync_status(&state, &iri, &status)?;
            return Err(e);
        }
    };

    input.sync = Some(crate::model::SyncIn {
        source: cfg.source.clone(),
        repo: cfg.repo.clone(),
        fields: cfg.fields.clone(),
        enabled: cfg.enabled,
    });
    let created = Props::from_quads(&iri, &quads).str(ns::DCT, "created");
    let tx = dom::replace_software(state.base(), &iri, &input, &principal.subject, created);
    shacl::enforce_write(&state, &tx.insert)?;
    state.store.apply(tx).map_err(AppError::from)?;
    write_sync_status(&state, &iri, &status)?;

    // Releases are separate records, so they are added rather than replaced: a version already
    // registered is left alone, which keeps any local edits to it.
    let mut added = Vec::new();
    if !outcome.releases.is_empty() {
        let known: Vec<String> = dom::list_releases(&ctx, &iri)?.into_iter().map(|r| r.version).collect();
        for r in &outcome.releases {
            if known.iter().any(|k| k == &r.version) {
                continue;
            }
            let rel_iri = ids::mint(state.base(), Kind::Release);
            let quads = dom::release_quads(state.base(), &rel_iri, &iri, r, &principal.subject);
            if shacl::enforce_write(&state, &quads).is_err() {
                continue;
            }
            let mut tx = GraphTx::new();
            tx.extend(quads);
            state.store.apply(tx).map_err(AppError::from)?;
            added.push(r.version.clone());
        }
    }

    if !added.is_empty() {
        status.last_changed.push("releases".into());
        write_sync_status(&state, &iri, &status)?;
    }
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "software.sync", Some(&iri), Some(&cfg.repo), None)
        .await;
    let ctx = Ctx::new(&state).await?;
    let mut changed = outcome.changed.clone();
    if !added.is_empty() {
        changed.push("releases".into());
    }
    Ok(Json(serde_json::json!({
        "software": dom::load_software(&ctx, &iri)?,
        "changed": changed,
        "releases_added": added,
        "skipped": outcome.skipped,
    })))
}

/// Persist just the sync bookkeeping, without rewriting the record.
fn write_sync_status(state: &AppState, iri: &str, status: &crate::model::SyncStatus) -> AppResult<()> {
    let node = format!("{iri}#sync");
    let mut tx = GraphTx::new();
    for pred in ["syncedAt", "syncStatus", "syncError", "syncChanged"] {
        tx.replace_property(&node, &format!("{}{pred}", ns::TAR), ns::G_LOCAL);
    }
    let mut n = crate::rdf::Node::local(&node);
    n.opt_datetime(ns::TAR, "syncedAt", &status.last_synced_at);
    n.text(ns::TAR, "syncStatus", &status.last_status);
    n.opt_text(ns::TAR, "syncError", &status.last_error);
    n.texts(ns::TAR, "syncChanged", &status.last_changed);
    tx.extend(n.finish());
    state.store.apply(tx).map_err(AppError::from)
}

/// Round-trip a stored record back into the input shape, so a sync edits rather than replaces.
fn software_in_from(s: &crate::model::Software) -> SoftwareIn {
    SoftwareIn {
        name: s.name.clone(),
        tagline: s.tagline.clone(),
        description: s.description.clone(),
        homepage: s.homepage.clone(),
        code_repository: s.code_repository.clone(),
        documentation: s.documentation.clone(),
        download_url: s.download_url.clone(),
        image: s.image.clone(),
        screenshots: s.screenshots.clone(),
        readme: s.readme.clone(),
        readme_base_url: s.readme_base_url.clone(),
        api_docs: s.api_docs.clone(),
        registration_clients: s.registration_clients.clone(),
        registration_issuer: s.registration_issuer.clone(),
        license: s.license.clone(),
        kinds: s.kinds.clone(),
        kind: None,
        maturity: s.maturity.clone(),
        deployable: Some(s.deployable),
        topics: s.topics.iter().map(|t| t.iri.clone()).collect(),
        keywords: s.keywords.clone(),
        publisher: s.publisher.as_ref().map(agent_in),
        contact: s.contact.as_ref().map(agent_in),
        publications: s.publications.clone(),
        capability: s.capability.as_ref().map(|c| crate::model::CapabilityIn {
            produces: c.produces.iter().map(|t| t.iri.clone()).collect(),
            consumes: c.consumes.iter().map(|t| t.iri.clone()).collect(),
        }),
        sync: None,
    }
}

fn agent_in(a: &crate::model::AgentRef) -> crate::model::AgentIn {
    crate::model::AgentIn {
        iri: Some(a.iri.clone()),
        name: a.name.clone(),
        kind: a.kind.clone(),
        identifier: a.identifier.clone(),
        email: a.email.clone(),
        homepage: a.homepage.clone(),
        version: a.version.clone(),
    }
}

/// Withdraw a release (spec §7.2's soft-delete rule, applied to releases).
///
/// A Release IRI can be cited — an Instance says which release it runs — so this tombstones
/// rather than erases, and the IRI keeps resolving. Withdrawn releases drop out of the list so
/// a mistaken or superseded one stops being offered.
pub async fn delete_release(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((id, release_id)): Path<(String, String)>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let software_iri = ids::iri_for(state.base(), Kind::Software, &id);
    let iri = ids::iri_for(state.base(), Kind::Release, &release_id);
    let quads = state.store.describe(&iri).map_err(AppError::from)?;
    if quads.is_empty() {
        return Err(AppError::not_found(format!("no release at {iri}")));
    }
    if Props::from_quads(&iri, &quads).iri(ns::DCT, "isVersionOf").as_deref() != Some(software_iri.as_str()) {
        return Err(AppError::not_found("that release does not belong to this software"));
    }
    tombstone(&state, &iri, &principal).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Declare produces[]/consumes[] at the Software layer (spec §7.3).
pub async fn put_capability(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(input): Json<CapabilityIn>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    put_capability_on(&state, &principal, &iri, &input, "software").await
}

pub async fn put_capability_on(
    state: &Arc<AppState>,
    principal: &Principal,
    subject: &str,
    input: &CapabilityIn,
    layer: &str,
) -> AppResult<axum::response::Response> {
    let existing = state.store.describe(subject).map_err(AppError::from)?;
    if existing.is_empty() {
        return Err(AppError::not_found(format!("nothing at {subject}")));
    }
    let props = Props::from_quads(subject, &existing);
    let cap_iri = props.iri(ns::TAR, "hasCapability").unwrap_or_else(|| ids::mint(state.base(), Kind::Capability));
    let cap_quads = dom::capability_quads(&cap_iri, input);
    shacl::enforce_write(&state, &cap_quads)?;
    let mut tx = GraphTx::new();
    tx.replace_subject(&cap_iri, ns::G_LOCAL);
    tx.extend(cap_quads);
    let mut n = crate::rdf::Node::local(subject);
    n.link(ns::TAR, "hasCapability", &cap_iri);
    tx.extend(n.finish());
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "capability.declare", Some(subject), Some(layer), None)
        .await;
    let ctx = Ctx::new(state).await?;
    let cap = dom::capability_from(&ctx, &cap_iri, layer);
    Ok(Json(cap).into_response())
}

/// biotoolsSchema export (spec §2.5, §7.2) — our descriptions can populate bio.tools or an
/// RSD instance with no runtime dependency in either direction.
pub async fn export_biotools(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Software, &id);
    let sw = dom::load_software(&ctx, &iri)?;

    let function = sw.capability.as_ref().map(|c| {
        serde_json::json!([{
            "operation": [],
            "input": c.consumes.iter().map(|t| serde_json::json!({"data": {"uri": t.iri, "term": t.label}})).collect::<Vec<_>>(),
            "output": c.produces.iter().map(|t| serde_json::json!({"data": {"uri": t.iri, "term": t.label}})).collect::<Vec<_>>(),
        }])
    });
    let doc = serde_json::json!({
        "name": sw.name,
        "biotoolsID": sw.id,
        "description": sw.description.clone().or_else(|| sw.tagline.clone()),
        "homepage": sw.homepage,
        "toolType": sw.kind.as_deref().map(|k| vec![match k {
            "service" => "Web service",
            "cli" => "Command-line tool",
            "library" => "Library",
            "workflow" => "Workflow",
            _ => "Command-line tool",
        }]).unwrap_or_default(),
        "topic": sw.topics.iter().map(|t| serde_json::json!({"uri": t.iri, "term": t.label})).collect::<Vec<_>>(),
        "function": function,
        "license": sw.license.as_deref().map(ids::iri_tail),
        "version": sw.latest_release.as_ref().map(|r| vec![r.version.clone()]).unwrap_or_default(),
        "documentation": sw.documentation.map(|d| vec![serde_json::json!({"url": d, "type": ["General"]})]).unwrap_or_default(),
        "download": sw.latest_release.as_ref().and_then(|r| r.container_image.clone())
            .map(|img| vec![serde_json::json!({"url": img, "type": "Container file"})]).unwrap_or_default(),
        "link": sw.code_repository.map(|r| vec![serde_json::json!({"url": r, "type": ["Repository"]})]).unwrap_or_default(),
        "credit": sw.publisher.map(|p| vec![serde_json::json!({"name": p.name, "typeEntity": "Institute", "url": p.homepage, "orcidid": p.identifier})]).unwrap_or_default(),
        "publication": sw.publications.iter().map(|p| serde_json::json!({"doi": p})).collect::<Vec<_>>(),
    });
    Ok(Json(doc))
}
