//! Advertisement endpoints — requirements 4 and 5 (spec §7.5).
//!
//! Both are idempotent on `(run, artifact, role)`, so a retried CI step does not duplicate
//! lineage, and both accept foreign IRIs in artifact position, which is how cross-registry
//! lineage forms with no coordination. Neither ever blocks on the network: an unknown foreign
//! IRI is stored verbatim and queued for background resolution (spec §9.3).
//!
//! The Instance is taken from the presenting credential and never from the payload (§8.3) —
//! whether that credential is a registry token or a Keycloak/Kubernetes/GitHub OIDC token.

use crate::auth::{Principal, SCOPE_ADVERTISE_CONSUME, SCOPE_ADVERTISE_PRODUCE};
use crate::domain::{artifact as artdom, run as rundom, Ctx};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::model::*;
use crate::ns;
use crate::rdf::Node;
use crate::shacl;
use crate::state::AppState;
use crate::store::GraphTx;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use std::sync::Arc;

pub async fn produced(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(input): Json<AdvertiseIn>,
) -> AppResult<impl IntoResponse> {
    advertise(state, principal, input, Role::Produced).await
}

pub async fn consumed(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(input): Json<AdvertiseIn>,
) -> AppResult<impl IntoResponse> {
    advertise(state, principal, input, Role::Consumed).await
}

#[derive(Clone, Copy, PartialEq)]
pub enum Role {
    Produced,
    Consumed,
}

impl Role {
    fn as_str(&self) -> &'static str {
        match self {
            Role::Produced => "produced",
            Role::Consumed => "consumed",
        }
    }
    fn scope(&self) -> &'static str {
        match self {
            Role::Produced => SCOPE_ADVERTISE_PRODUCE,
            Role::Consumed => SCOPE_ADVERTISE_CONSUME,
        }
    }
}

async fn advertise(
    state: Arc<AppState>,
    principal: Principal,
    input: AdvertiseIn,
    role: Role,
) -> AppResult<impl IntoResponse> {
    principal.require_scope(role.scope())?;
    let instance_iri = principal.require_instance()?;
    if !state.store.exists(&instance_iri).map_err(AppError::from)? {
        return Err(AppError::forbidden(format!(
            "credential names Instance {instance_iri}, which this registry does not know"
        )));
    }
    // Validate up front, on throwaway candidate quads, rather than on the transaction just
    // before it commits: the loop below writes idempotency and resolution rows to SQLite as it
    // goes, and a rejection after that would leave a claim recorded for a run that never
    // existed.
    {
        let mut candidate =
            rundom::run_quads(&ids::mint(state.base(), Kind::Run), &input.run, &instance_iri, &principal.subject);
        for a in input.artifacts.iter().filter(|a| a.iri.is_none()) {
            candidate.extend(artdom::artifact_quads(
                state.base(),
                &ids::mint(state.base(), Kind::Artifact),
                a,
                &principal.subject,
                None,
            ));
        }
        shacl::enforce_write(&state, &candidate)?;
    }

    // Resolve or mint the Run. A second advertisement for the same CI attempt attaches to the
    // same Run rather than inventing a new one.
    let mut run_input = input.run.clone();
    if let Some(r) = run_input.release.clone() {
        run_input.release = Some(ids::iri_for(state.base(), Kind::Release, &r));
    }
    let existing_run = match run_input.external_key.as_deref().filter(|k| !k.is_empty()) {
        Some(key) => state.ops.run_for_key(key, &instance_iri).await.map_err(AppError::from)?,
        None => None,
    };
    let mut tx = GraphTx::new();
    let mut created_any = false;
    let run_iri = match existing_run {
        Some(iri) => {
            update_run_outcome(&mut tx, &iri, &run_input);
            iri
        }
        None => {
            let iri = ids::mint(state.base(), Kind::Run);
            created_any = true;
            tx.extend(rundom::run_quads(&iri, &run_input, &instance_iri, &principal.subject));
            if let Some(key) = run_input.external_key.as_deref().filter(|k| !k.is_empty()) {
                state.ops.remember_run(key, &instance_iri, &iri).await.map_err(AppError::from)?;
            }
            iri
        }
    };

    let mut artifact_iris = Vec::new();
    let mut queued = Vec::new();

    for a in &input.artifacts {
        let artifact_iri = match a.iri.as_deref().filter(|s| !s.is_empty()) {
            // A bare reference: local or foreign. Foreign IRIs are stored verbatim.
            Some(iri) => {
                if !ids::is_local(state.base(), iri) && !state.store.exists(iri).map_err(AppError::from)? {
                    state.ops.queue_resolve(iri, None).await.map_err(AppError::from)?;
                    queued.push(iri.to_string());
                }
                iri.to_string()
            }
            None => {
                // Idempotency by the producer's own key, so re-running a step that re-emits
                // the same dataset does not mint a second artifact. When the producer gives
                // no key, one is derived from the run key plus the artifact's description —
                // otherwise a retried CI step would mint a fresh IRI every attempt and the
                // idempotency promise of §7.5 would be empty.
                let key = a
                    .external_key
                    .clone()
                    .filter(|k| !k.is_empty())
                    .or_else(|| run_input.external_key.as_deref().map(|rk| derived_key(rk, a)));
                let known = match key.as_deref() {
                    Some(k) => state.ops.artifact_for_key(k).await.map_err(AppError::from)?,
                    None => None,
                };
                match known {
                    Some(iri) => iri,
                    None => {
                        let iri = ids::mint(state.base(), Kind::Artifact);
                        let generated_by = (role == Role::Produced).then_some(run_iri.as_str());
                        tx.extend(artdom::artifact_quads(state.base(), &iri, a, &principal.subject, generated_by));
                        for parent in &a.was_derived_from {
                            if !ids::is_local(state.base(), parent)
                                && !state.store.exists(parent).map_err(AppError::from)?
                            {
                                state.ops.queue_resolve(parent, None).await.map_err(AppError::from)?;
                                queued.push(parent.clone());
                            }
                        }
                        if let Some(k) = key.as_deref() {
                            state.ops.remember_artifact(k, &iri).await.map_err(AppError::from)?;
                        }
                        created_any = true;
                        iri
                    }
                }
            }
        };

        let fresh =
            state.ops.claim_advertisement(&run_iri, &artifact_iri, role.as_str()).await.map_err(AppError::from)?;
        if fresh {
            created_any = true;
            match role {
                // `prov:used` on the Run — the consume advertisement (requirement 5).
                Role::Consumed => {
                    let mut n = Node::local(&run_iri);
                    n.link(ns::PROV, "used", &artifact_iri);
                    tx.extend(n.finish());
                }
                // `prov:wasGeneratedBy` on the Artifact — the produce advertisement (req. 4).
                Role::Produced => {
                    let mut n = Node::local(&artifact_iri);
                    n.link(ns::PROV, "wasGeneratedBy", &run_iri);
                    tx.extend(n.finish());
                }
            }
        }
        artifact_iris.push(artifact_iri);
    }

    if !tx.is_empty() {
        state.store.apply(tx).map_err(AppError::from)?;
    }

    // Standing interest, answered at the moment it is satisfied. This runs *after* the commit
    // so a subscriber is never told about an artifact that failed to land, and it touches
    // nothing but the local graph and SQLite — a match writes a queue row, never a socket, so
    // a slow or dead subscriber cannot slow down the advertisement that triggered it (§9.3).
    crate::api::subscriptions::notify_advertised(
        &state,
        Some(&instance_iri),
        Some(&run_iri),
        &artifact_iris,
        role.as_str(),
    )
    .await;

    let _ = state
        .ops
        .audit(
            Some(&principal.subject),
            principal.actor_kind(),
            &format!("advertise.{}", role.as_str()),
            Some(&run_iri),
            Some(&format!("{} artifact(s)", artifact_iris.len())),
            None,
        )
        .await;

    let status = if created_any { StatusCode::CREATED } else { StatusCode::OK };
    Ok((
        status,
        Json(AdvertiseOut {
            run: run_iri,
            artifacts: artifact_iris,
            created: created_any,
            queued_for_resolution: queued,
        }),
    ))
}

/// Apply a later advertisement's outcome to a run that already exists.
///
/// The status and end time are *replaced*, not added: a run advertised first as `running` and
/// then as `success` must end up with one of each, or every reader has to guess which of two
/// values in the graph is current.
pub fn update_run_outcome(tx: &mut GraphTx, run_iri: &str, run: &RunIn) {
    if run.ended_at.is_none() && run.status.is_none() {
        return;
    }
    let mut n = Node::local(run_iri);
    if run.ended_at.is_some() {
        tx.replace_property(run_iri, &format!("{}endedAtTime", ns::PROV), ns::G_LOCAL);
        n.opt_datetime(ns::PROV, "endedAtTime", &run.ended_at);
    }
    if let Some(status) = run.status.as_deref() {
        tx.replace_property(run_iri, &format!("{}status", ns::TAR), ns::G_LOCAL);
        n.text(ns::TAR, "status", status);
        // The schema:actionStatus supplement must move with it, or a reader sees two states.
        tx.replace_property(run_iri, &format!("{}actionStatus", ns::SCHEMA), ns::G_LOCAL);
        if let Some(st) = rundom::action_status(status) {
            n.link(ns::SCHEMA, "actionStatus", &st);
        }
    }
    tx.extend(n.finish());
}

/// A stable identity for an artifact that carries none of its own: the run it belongs to plus
/// what the producer said about it. Two identical advertisements of the same step collapse;
/// two genuinely different artifacts in one run do not.
fn derived_key(run_key: &str, a: &ArtifactIn) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(run_key.as_bytes());
    h.update(a.title.as_deref().unwrap_or_default().as_bytes());
    h.update(a.conforms_to.as_deref().unwrap_or_default().as_bytes());
    for d in &a.distributions {
        h.update(d.download_url.as_deref().unwrap_or_default().as_bytes());
        h.update(d.access_url.as_deref().unwrap_or_default().as_bytes());
    }
    format!("urn:tar:derived:{}", hex::encode(&h.finalize()[..16]))
}

/// Shared by the OpenLineage adapter: resolve the run for a payload, minting when needed.
pub async fn ensure_run(
    state: &Arc<AppState>,
    principal: &Principal,
    instance_iri: &str,
    run: &RunIn,
    tx: &mut GraphTx,
) -> AppResult<String> {
    if let Some(key) = run.external_key.as_deref().filter(|k| !k.is_empty()) {
        if let Some(iri) = state.ops.run_for_key(key, instance_iri).await.map_err(AppError::from)? {
            update_run_outcome(tx, &iri, run);
            return Ok(iri);
        }
    }
    let iri = ids::mint(state.base(), Kind::Run);
    tx.extend(rundom::run_quads(&iri, run, instance_iri, &principal.subject));
    if let Some(key) = run.external_key.as_deref().filter(|k| !k.is_empty()) {
        state.ops.remember_run(key, instance_iri, &iri).await.map_err(AppError::from)?;
    }
    Ok(iri)
}

/// Load a run for the response of the adapter.
pub async fn run_summary(state: &Arc<AppState>, iri: &str) -> AppResult<RunSummary> {
    let ctx = Ctx::new(state).await?;
    rundom::load_run_summary(&ctx, iri)
}
