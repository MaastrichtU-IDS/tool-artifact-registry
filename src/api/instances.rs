//! Instance endpoints (spec §7.4). An Instance is the deployment that acts, and — under the
//! workload-identity model — the thing an OIDC client id binds to.

use super::{count, page_iris, resource_response, Paging};
use crate::auth::Principal;
use crate::domain::{instance as dom, run as rundom, Ctx};
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
pub struct InstanceFilter {
    pub q: Option<String>,
    pub software: Option<String>,
    pub operator: Option<String>,
    pub status: Option<String>,
    pub release: Option<String>,
    pub registry: Option<String>,
    #[serde(flatten)]
    pub paging: Paging,
}

fn where_body(base: &str, f: &InstanceFilter) -> String {
    let mut w = format!(
        "GRAPH ?g {{ ?s a <{t}> ; rdfs:label ?label }}\n\
         FILTER NOT EXISTS {{ GRAPH ?tg {{ ?s tar:tombstoned true }} }}",
        t = dom::TYPE_SOFTWARE_AGENT
    );
    if let Some(q) = &f.q {
        w.push('\n');
        w.push_str(&super::text_filter(q, &["?label"]));
    }
    if let Some(sw) = f.software.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Software, sw);
        w.push_str(&format!("\nGRAPH ?g {{ ?s tar:instanceOf <{iri}> }}"));
    }
    if let Some(r) = f.release.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(base, Kind::Release, r);
        w.push_str(&format!("\nGRAPH ?g {{ ?s tar:runsRelease <{iri}> }}"));
    }
    if let Some(o) = f.operator.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!("\nGRAPH ?g {{ ?s dct:publisher <{o}> }}"));
    }
    if let Some(s) = f.status.as_deref().filter(|v| !v.is_empty()) {
        w.push_str(&format!("\nGRAPH ?g {{ ?s tar:health \"{}\" }}", super::escape_literal(s)));
    }
    match f.registry.as_deref() {
        Some("local") => w.push_str(&format!("\nFILTER(?g = <{}>)", ns::G_LOCAL)),
        Some(peer) if !peer.is_empty() => w.push_str(&format!("\nFILTER(?g = <{}>)", ns::peer_graph(peer))),
        _ => {}
    }
    w.push_str(&format!("\n{}", f.paging.cursor_filter("?s")));
    w
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(f): Query<InstanceFilter>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let body = where_body(state.base(), &f);
    let (iris, next) = page_iris(&state, &body, &f.paging)?;
    let total = count(&state, &body)?;
    let signals = dom::instance_signals(&ctx, None)?;
    let mut items = Vec::new();
    for iri in iris {
        let quads = state.store.describe(&iri).map_err(AppError::from)?;
        let p = Props::from_quads(&iri, &quads);
        let mut i = dom::instance_from_props(&ctx, &iri, &p);
        if let Some(s) = signals.get(&iri) {
            i.last_run_at = s.last_run_at.clone();
            i.runs_30d = s.runs_30d;
            i.failures_30d = s.failures_30d;
            i.artifact_count = s.artifacts;
        }
        items.push(i);
    }
    dom::decorate(&ctx, &mut items)?;
    Ok(Json(Page::new(items, total, next)))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    principal: Principal,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    let mut inst = dom::load_instance(&ctx, &iri)?;
    // Token count is operational state; only someone who could manage them needs it.
    if principal.is_curator() || principal.instance_iri.as_deref() == Some(iri.as_str()) {
        inst.token_count = state.ops.list_tokens(&iri).await.map(|t| t.iter().filter(|x| x.revoked_at.is_none()).count() as i64).unwrap_or(0);
    }
    let mut sp = Signposting::new(&iri).collection(&format!("{}/api/v1/instances", state.base()));
    if let Some(e) = &inst.endpoint_url {
        sp = sp.item(e, None);
    }
    if let Some(o) = &inst.operator {
        sp = sp.author(&o.iri);
    }
    Ok(resource_response(&state, &headers, &iri, &inst, sp, Repr::Json).await?)
}

/// Reject an endpoint on an instance of software that cannot be hosted.
///
/// This cannot live in `shapes/tar-shapes.ttl`: the rule spans two records, and a write is
/// validated against the candidate record alone (README "Known gaps" 1). So it is checked here,
/// where the Software is already being looked up anyway.
fn check_deployable(state: &AppState, software: Option<&str>, input: &InstanceIn) -> AppResult<()> {
    let (Some(sw), Some(endpoint)) = (software, input.endpoint_url.as_deref().filter(|e| !e.is_empty()))
    else {
        return Ok(());
    };
    let quads = state.store.describe(sw).map_err(AppError::from)?;
    let props = Props::from_quads(sw, &quads);
    if props.bool(ns::TAR, "deployable") == Some(false) {
        let name = props.str(ns::SCHEMA, "name").unwrap_or_else(|| sw.to_string());
        return Err(AppError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "not-deployable",
            "Endpoint on software that cannot be hosted",
        )
        .detail(format!(
            "{name} is marked as not deployable — it runs on a machine rather than being hosted — \
             so this instance is an installation and cannot have an endpoint. Remove \
             endpoint_url ({endpoint}), or clear `deployable: false` on the software if that is wrong."
        ))
        .with("field", serde_json::json!("endpoint_url")));
    }
    Ok(())
}

fn resolve_software_for(state: &AppState, input: &InstanceIn) -> AppResult<Option<String>> {
    if let Some(sw) = input.software.as_deref().filter(|v| !v.is_empty()) {
        return Ok(Some(ids::iri_for(state.base(), Kind::Software, sw)));
    }
    // Derive from the Release when only that was given.
    if let Some(r) = input.release.as_deref().filter(|v| !v.is_empty()) {
        let iri = ids::iri_for(state.base(), Kind::Release, r);
        let quads = state.store.describe(&iri).map_err(AppError::from)?;
        return Ok(Props::from_quads(&iri, &quads).iri(ns::DCT, "isVersionOf"));
    }
    Ok(None)
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(mut input): Json<InstanceIn>,
) -> AppResult<impl IntoResponse> {
    if !principal.is_curator() {
        principal.require_scope(crate::auth::SCOPE_REGISTER_INSTANCE)?;
    }
    // These two are the registry's record of which credential owns a self-registered
    // deployment. A caller that could set them could claim another deployment's record on its
    // next announcement, so they are dropped from anything that arrives over the wire.
    input.self_registered_by = None;
    input.instance_key = None;
    // Accept a bare id or a full IRI in `software` / `release`.
    if let Some(sw) = input.software.clone() {
        input.software = Some(ids::iri_for(state.base(), Kind::Software, &sw));
    }
    // A credential bound to one application decides which software its deployments belong to,
    // here exactly as at `PUT /instances/self`. Without this the binding was enforced at one
    // route and not the other: an auto-registration key carries `register:instance` and no
    // roles, so it passed the scope check above and then took the software from the *body* —
    // letting a key issued for one application register deployments of any other.
    if let Some(bound) = principal.software_iri.as_deref() {
        match input.software.as_deref() {
            Some(named) if named != bound => {
                return Err(AppError::forbidden(format!(
                    "this credential registers deployments of {bound}, not of {named}"
                )))
            }
            // Not naming one is fine: the credential already says which.
            _ => input.software = Some(bound.to_string()),
        }
    }
    if let Some(r) = input.release.clone() {
        input.release = Some(ids::iri_for(state.base(), Kind::Release, &r));
    }
    let iri = ids::mint(state.base(), Kind::Instance);
    let software = resolve_software_for(&state, &input)?;
    check_deployable(&state, software.as_deref(), &input)?;
    let quads = dom::instance_quads(state.base(), &iri, &input, &principal.subject, software.as_deref());
    shacl::enforce_write(&state, &quads)?;
    let mut tx = GraphTx::new();
    tx.extend(quads);
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "instance.create", Some(&iri), Some(&input.label), None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok((StatusCode::CREATED, Json(dom::load_instance(&ctx, &iri)?)))
}

/// Round-trip a stored Instance back into the input shape, so a PATCH can merge onto it.
pub fn instance_in_from(i: &Instance) -> InstanceIn {
    InstanceIn {
        label: i.label.clone(),
        // Carried through, or a PATCH would drop the triples that let a self-registered
        // deployment find its own record on the next announcement.
        self_registered_by: i.self_registered_by.clone(),
        instance_key: i.instance_key.clone(),
        software: i.software.clone(),
        release: i.release.clone(),
        endpoint_url: i.endpoint_url.clone(),
        endpoint_description: i.endpoint_description.clone(),
        operator: i.operator.as_ref().map(|a| crate::model::AgentIn {
            iri: Some(a.iri.clone()),
            name: a.name.clone(),
            kind: a.kind.clone(),
            identifier: a.identifier.clone(),
            email: a.email.clone(),
            homepage: a.homepage.clone(),
            version: a.version.clone(),
        }),
        availability: i.availability.clone(),
        jurisdiction: i.jurisdiction.clone(),
        description: i.description.clone(),
        oidc_client_id: i.oidc_client_id.clone(),
        oidc_issuer: i.oidc_issuer.clone(),
        allowed_scopes: i.allowed_scopes.clone(),
        health_endpoint: i.health_endpoint.clone(),
        capability: i.capability.as_ref().map(|c| CapabilityIn {
            produces: c.produces.iter().map(|t| t.iri.clone()).collect(),
            consumes: c.consumes.iter().map(|t| t.iri.clone()).collect(),
        }),
    }
}

/// Overlay the JSON a caller sent onto the JSON of the record as it stands.
///
/// PATCH means merge to almost everyone, and this used to replace: a deployment recording its
/// own endpoint with `{label, endpoint_url}` silently dropped its operator, jurisdiction,
/// scopes and OIDC binding. Merging at the JSON level rather than on the typed struct is what
/// makes "absent" and "explicitly null" different things — absent keeps the stored value, and
/// `null` clears it, which is the only way to erase a field through a merging PATCH.
pub fn merge_json(base: serde_json::Value, patch: serde_json::Value) -> serde_json::Value {
    match (base, patch) {
        (serde_json::Value::Object(mut b), serde_json::Value::Object(p)) => {
            for (k, v) in p {
                match v {
                    // Arrays and scalars replace wholesale; only objects recurse. Merging
                    // arrays element-wise would make it impossible to remove one.
                    serde_json::Value::Object(_) => {
                        let existing = b.remove(&k).unwrap_or(serde_json::Value::Null);
                        b.insert(k, merge_json(existing, v));
                    }
                    _ => {
                        b.insert(k, v);
                    }
                }
            }
            serde_json::Value::Object(b)
        }
        (_, patch) => patch,
    }
}

pub async fn patch(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    // A deployment may maintain its own record; anyone else needs curator.
    if principal.instance_iri.as_deref() != Some(iri.as_str()) {
        principal.require_curator()?;
    }
    if !ids::is_local(state.base(), &iri) {
        return Err(AppError::forbidden("this registry is not authoritative for that IRI (spec §9.7)"));
    }
    if !state.store.exists(&iri).map_err(AppError::from)? {
        return Err(AppError::not_found(format!("no instance at {iri}")));
    }
    let ctx = Ctx::new(&state).await?;
    let current = dom::load_instance(&ctx, &iri)?;

    // A self-registered deployment owns its own record, and nobody else may edit it here.
    //
    // Not a policy so much as an admission of what is already true: the deployment re-states
    // these fields on every announcement, so a curator's edit survives only until the next one
    // and then vanishes with no error and no trace. Offering an edit that will be silently
    // undone is worse than refusing it. The credential that registered the record is the one
    // that may change it, through `PUT /api/v1/instances/self`.
    //
    // A curator is not powerless — the remedies are the ones that actually work: withdraw the
    // record, or revoke what lets the deployment speak (the key, or the client id in the
    // software's `registration_clients`). Editing the description of a deployment that is
    // misbehaving was never the fix.
    //
    // Below the API there is no such rule: someone with the store itself can write anything.
    // That is the break-glass path, deliberately out of band and deliberately not an endpoint —
    // the SPARQL surface is read-only, so nothing here can be talked into doing it.
    if let Some(owner) = current.self_registered_by.as_deref() {
        if principal.instance_iri.as_deref() != Some(iri.as_str()) {
            return Err(AppError::forbidden(format!(
                "this deployment maintains its own record — it was registered by {owner}, and \
                 re-states these fields every time it announces itself, so an edit made here \
                 would be overwritten without warning. The deployment changes them at \
                 PUT /api/v1/instances/self. To stop it, withdraw the record or revoke the \
                 credential it registered with."
            )));
        }
    }

    let merged = merge_json(
        serde_json::to_value(instance_in_from(&current)).map_err(|e| AppError::internal(e.to_string()))?,
        body,
    );
    let mut input: InstanceIn = serde_json::from_value(merged)
        .map_err(|e| AppError::bad_request(format!("could not apply the change: {e}")))?;
    // Ownership of a self-registered record is the registry's to state, not the caller's to
    // edit: whatever the body said, the stored values stand.
    input.self_registered_by = current.self_registered_by.clone();
    input.instance_key = current.instance_key.clone();
    if let Some(sw) = input.software.clone() {
        input.software = Some(ids::iri_for(state.base(), Kind::Software, &sw));
    }
    if let Some(r) = input.release.clone() {
        input.release = Some(ids::iri_for(state.base(), Kind::Release, &r));
    }
    let software = resolve_software_for(&state, &input)?;
    check_deployable(&state, software.as_deref(), &input)?;
    let tx = dom::replace_instance(state.base(), &iri, &input, &principal.subject, software.as_deref());
    shacl::enforce_write(&state, &tx.insert)?;
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "instance.update", Some(&iri), None, None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok(Json(dom::load_instance(&ctx, &iri)?))
}

pub async fn soft_delete(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    principal.require_curator()?;
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    super::software::tombstone(&state, &iri, &principal).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Narrow the capability inherited from the Software (spec §7.3).
pub async fn put_capability(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(input): Json<CapabilityIn>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    if principal.instance_iri.as_deref() != Some(iri.as_str()) {
        principal.require_curator()?;
    }
    super::software::put_capability_on(&state, &principal, &iri, &input, "instance").await
}

/// `PUT /api/v1/instances/self` — a running service records what it is.
///
/// The deployment describes itself and nothing else. Which Instance it *is* comes from the
/// presenting credential, never from the body: a Kubernetes pod presenting its projected
/// ServiceAccount token, or a service with Keycloak client credentials, is already identified
/// by the time it gets here. That is the same rule that governs advertisement (§8.3), and it is
/// what stops one deployment rewriting another's record.
///
/// First announcement creates the Instance, but only when the operator has opted in with
/// `TAR_OIDC_AUTO_REGISTER_INSTANCES=1`. Off by default, because a registry that silently gains
/// a record for anything holding a trusted token has no idea what is in it. Later announcements
/// update the record and always work, since the credential is bound to it by then.
pub async fn announce_self(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(input): Json<SelfAnnounceIn>,
) -> AppResult<impl IntoResponse> {
    principal.require_authenticated()?;
    let client_id = match principal.credential {
        crate::auth::CredentialKind::OidcWorkload | crate::auth::CredentialKind::LocalToken => {
            principal.subject.clone()
        }
        _ => {
            return Err(AppError::forbidden(
                "only a deployment may announce itself; this is a person's or an administrator's \
                 credential, so use POST /api/v1/instances",
            ))
        }
    };

    let now = chrono::Utc::now().to_rfc3339();
    // A software-bound credential has no instance of its own, so the deployment is identified
    // by the credential plus the name it calls itself. Look for the record it made last time
    // before deciding this is a first announcement.
    let self_key = input.instance_key.clone().unwrap_or_else(|| principal.subject.clone());
    // Which record this announcement is about.
    //
    // For a credential that *is* one deployment, that deployment — it has no key to give and
    // none to honour. For a shared registration credential the `instance_key` decides, and it
    // must be consulted *first*: the credential is also bound to whichever deployment it
    // registered, and taking that binding would make the second deployment to announce merge
    // into the first and relabel it, which is what happened.
    let shared = principal.software_iri.is_some();
    let existing = if shared {
        find_self_registered(&state, &principal.subject, &self_key)
            .or_else(|| principal.instance_iri.clone().filter(|_| input.instance_key.is_none()))
    } else {
        principal
            .instance_iri
            .clone()
            .or_else(|| find_self_registered(&state, &principal.subject, &self_key))
    };

    if let Some(iri) = existing {
        // Known deployment: merge what it said onto what we hold.
        let ctx = Ctx::new(&state).await?;
        let current = dom::load_instance(&ctx, &iri)?;
        let mut merged = instance_in_from(&current);
        apply_announcement(&mut merged, &input, state.base());
        shacl::enforce_write(
            &state,
            &dom::instance_quads(state.base(), &iri, &merged, &principal.subject, merged.software.as_deref()),
        )?;
        let software = resolve_software_for(&state, &merged)?;
        check_deployable(&state, software.as_deref(), &merged)?;
        let mut tx = dom::replace_instance(state.base(), &iri, &merged, &principal.subject, software.as_deref());
        stamp_seen(&mut tx, &iri, &now);
        state.store.apply(tx).map_err(AppError::from)?;
        let _ = state
            .ops
            .audit(Some(&principal.subject), principal.actor_kind(), "instance.announce", Some(&iri), None, None)
            .await;
        let ctx = Ctx::new(&state).await?;
        return Ok((StatusCode::OK, Json(dom::load_instance(&ctx, &iri)?)));
    }

    // Which software may this credential register a deployment of?
    //
    // Two modes, and the difference is where the authority comes from:
    //
    // 1. **Bound to the software.** The credential itself names it — an auto-registration key
    //    minted at `POST /api/v1/software/{id}/tokens`, or an OIDC client the software lists in
    //    `registration_clients`. The caller does not get to choose; the credential decides, so
    //    a key for one application cannot register deployments of another.
    // 2. **Open auto-registration.** `TAR_OIDC_AUTO_REGISTER_INSTANCES` lets any authenticated
    //    workload name its own software. Convenient in a trusted cluster, and much weaker: it
    //    is the operator saying every credential this registry accepts may add records.
    let bound_software = principal.software_iri.clone();
    let software_iri = match (&bound_software, &input.software) {
        (Some(bound), Some(claimed)) => {
            let claimed_iri = ids::iri_for(state.base(), Kind::Software, claimed);
            if &claimed_iri != bound {
                return Err(AppError::forbidden(format!(
                    "this credential registers deployments of {bound}, not of {claimed_iri}"
                )));
            }
            bound.clone()
        }
        (Some(bound), None) => bound.clone(),
        (None, claimed) => {
            if !state.config.oidc.auto_register_instances {
                return Err(AppError::forbidden(format!(
                    "no deployment is bound to {client_id}, and no software authorises it to \
                     register one. Either an administrator creates the deployment with POST \
                     /api/v1/instances setting oidc_client_id to {client_id}, or a curator issues \
                     an auto-registration key with POST /api/v1/software/{{id}}/tokens (or lists \
                     {client_id} in that software's registration_clients), or the operator \
                     enables TAR_OIDC_AUTO_REGISTER_INSTANCES."
                )));
            }
            let Some(claimed) = claimed.clone() else {
                return Err(AppError::bad_request(
                    "a first announcement must say which software this is a deployment of",
                ));
            };
            ids::iri_for(state.base(), Kind::Software, &claimed)
        }
    };
    let mut fresh = InstanceIn {
        label: input.label.clone().unwrap_or_else(|| client_id.clone()),
        software: Some(software_iri.clone()),
        // Bind the record to the credential that announced it, so the next announcement from
        // the same workload finds this record instead of making another.
        // Only bind the client id when the credential *is* this deployment. A software-scoped
        // credential is shared by every deployment of the application, and writing it here
        // would make the next one authenticate as this one.
        //
        // The issuer goes with it, both or neither. Writing the issuer alone produced a record
        // saying "authenticated somewhere, by nobody in particular", which the shapes reject
        // outright — so an OIDC client authorised through `registration_clients` could not
        // register itself at all: it got a 422 about a field it had never sent. The credential
        // is remembered as `self_registered_by` regardless, which is what finds this record on
        // the next announcement.
        oidc_client_id: bound_software.is_none().then(|| client_id.clone()),
        oidc_issuer: bound_software.is_none().then(|| principal.issuer.clone()).flatten(),
        allowed_scopes: vec!["advertise:produce".into(), "advertise:consume".into()],
        ..Default::default()
    };
    apply_announcement(&mut fresh, &input, state.base());
    // `apply_announcement` copies `software` from the payload; put the authorised value back,
    // so a credential bound to one application cannot register a deployment of another by
    // naming it twice.
    fresh.software = Some(software_iri);
    fresh.self_registered_by = Some(principal.subject.clone());
    fresh.instance_key = Some(self_key.clone());
    let iri = ids::mint(state.base(), Kind::Instance);
    let resolved = resolve_software_for(&state, &fresh)?;
    check_deployable(&state, resolved.as_deref(), &fresh)?;
    let quads = dom::instance_quads(state.base(), &iri, &fresh, &principal.subject, resolved.as_deref());
    shacl::enforce_write(&state, &quads)?;
    let mut tx = GraphTx::new();
    tx.extend(quads);
    stamp_seen(&mut tx, &iri, &now);
    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(Some(&principal.subject), principal.actor_kind(), "instance.self-register", Some(&iri), Some(&client_id), None)
        .await;
    let ctx = Ctx::new(&state).await?;
    Ok((StatusCode::CREATED, Json(dom::load_instance(&ctx, &iri)?)))
}

/// Copy the fields a deployment is allowed to say about itself. Everything absent is left as
/// it stands, so announcing an endpoint does not erase the jurisdiction a curator set.
fn apply_announcement(target: &mut InstanceIn, input: &SelfAnnounceIn, base: &str) {
    if let Some(v) = &input.label {
        target.label = v.clone();
    }
    if let Some(v) = &input.software {
        target.software = Some(ids::iri_for(base, Kind::Software, v));
    }
    if let Some(v) = &input.release {
        target.release = Some(ids::iri_for(base, Kind::Release, v));
    }
    for (from, to) in [
        (&input.endpoint_url, &mut target.endpoint_url),
        (&input.endpoint_description, &mut target.endpoint_description),
        (&input.health_endpoint, &mut target.health_endpoint),
        (&input.availability, &mut target.availability),
        (&input.jurisdiction, &mut target.jurisdiction),
        (&input.description, &mut target.description),
    ] {
        if from.is_some() {
            *to = from.clone();
        }
    }
    if input.capability.is_some() {
        target.capability = input.capability.clone();
    }
}

/// Record that we heard from this deployment. For one with no endpoint — a CLI, a desktop
/// install — this is the only liveness signal that exists.
fn stamp_seen(tx: &mut GraphTx, iri: &str, now: &str) {
    tx.replace_property(iri, &format!("{}lastSeenAt", ns::TAR), ns::G_LOCAL);
    let mut n = crate::rdf::Node::local(iri);
    n.datetime(ns::TAR, "lastSeenAt", now);
    tx.extend(n.finish());
}

pub async fn runs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(paging): Query<Paging>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    let body = format!(
        "GRAPH ?g {{ ?s a <{t}> ; prov:wasAssociatedWith|tar:atInstance <{iri}> }}\n{}",
        paging.cursor_filter("?s"),
        t = rundom::TYPE_ACTIVITY
    );
    let (iris, next) = page_iris(&state, &body, &paging)?;
    let total = count(&state, &body)?;
    let mut items = Vec::new();
    for r in iris {
        if let Ok(s) = rundom::load_run_summary(&ctx, &r) {
            items.push(s);
        }
    }
    Ok(Json(Page::new(items, total, next)))
}

pub async fn artifacts(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(paging): Query<Paging>,
) -> AppResult<impl IntoResponse> {
    let ctx = Ctx::new(&state).await?;
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    let body = format!(
        "GRAPH ?g {{ ?run prov:wasAssociatedWith|tar:atInstance <{iri}> . ?s prov:wasGeneratedBy ?run }}\n{}",
        paging.cursor_filter("?s")
    );
    let (iris, next) = page_iris(&state, &body, &paging)?;
    let total = count(&state, &body)?;
    let items: Vec<Artifact> = iris
        .iter()
        .filter_map(|a| crate::domain::artifact::load_artifact(&ctx, a).ok())
        .collect();
    Ok(Json(Page::new(items, total, next)))
}

/// The deployment this credential registered previously, if any.
///
/// Keyed on the credential's subject *and* the name the deployment gave, so one auto-
/// registration key can maintain several deployments (one per cluster, say) while each
/// announcement still lands on the right record.
fn find_self_registered(state: &AppState, subject: &str, key: &str) -> Option<String> {
    let q = format!(
        r#"{p}
SELECT ?i WHERE {{
  GRAPH <{g}> {{
    ?i tar:selfRegisteredBy {subject} ; tar:instanceKey {key} .
  }}
  FILTER NOT EXISTS {{ GRAPH ?tg {{ ?i tar:tombstoned true }} }}
}} LIMIT 1"#,
        p = ns::PREFIXES,
        g = ns::G_LOCAL,
        subject = format!("\"{}\"", super::escape_literal(subject)),
        key = format!("\"{}\"", super::escape_literal(key)),
    );
    state.store.select(&q).ok()?.rows.first()?.iri("i")
}
