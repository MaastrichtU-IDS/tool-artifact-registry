//! OpenLineage adapter (spec §7.6, D7).
//!
//! OpenLineage covers the run-event skeleton and nothing else we need — no licence, no
//! checksum, no DCAT distributions, no access protocol, no resolvable IRIs, no capabilities.
//! So it is an ingest adapter, not the canonical wire format. Everything the mapping does not
//! name is preserved verbatim as `tar:openLineagePayload` on the Run, so nothing is lost.

use crate::auth::{Principal, SCOPE_ADVERTISE_PRODUCE};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::model::*;
use crate::ns;
use crate::rdf::Node;
use crate::state::AppState;
use crate::store::GraphTx;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::Value;
use std::sync::Arc;

pub async fn ingest(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(event): Json<Value>,
) -> AppResult<impl IntoResponse> {
    principal.require_scope(SCOPE_ADVERTISE_PRODUCE)?;
    let instance_iri = principal.require_instance()?;
    if !state.store.exists(&instance_iri).map_err(AppError::from)? {
        return Err(AppError::forbidden(format!("credential names unknown Instance {instance_iri}")));
    }

    let event_type = event.get("eventType").and_then(|v| v.as_str()).unwrap_or("OTHER").to_uppercase();
    let event_time = event.get("eventTime").and_then(|v| v.as_str()).map(str::to_string);
    let run_id = event.get("run").and_then(|r| r.get("runId")).and_then(|v| v.as_str());
    let job_name = event.get("job").and_then(|j| j.get("name")).and_then(|v| v.as_str());
    let job_namespace = event.get("job").and_then(|j| j.get("namespace")).and_then(|v| v.as_str());

    let status = match event_type.as_str() {
        "COMPLETE" => "success",
        "FAIL" | "ABORT" => "failed",
        "START" | "RUNNING" => "running",
        _ => "running",
    };
    let terminal = matches!(event_type.as_str(), "COMPLETE" | "FAIL" | "ABORT");

    let run_in = RunIn {
        external_key: run_id.map(str::to_string),
        label: job_name.map(str::to_string),
        started_at: (!terminal).then(|| event_time.clone()).flatten(),
        ended_at: terminal.then(|| event_time.clone()).flatten(),
        status: Some(status.to_string()),
        release: None,
    };

    let mut tx = GraphTx::new();
    let run_iri = super::advertise::ensure_run(&state, &principal, &instance_iri, &run_in, &mut tx).await?;

    // Keep the whole event. `job.namespace` is recorded as a label only — authorisation comes
    // from the credential, never the payload (spec §8.3).
    {
        let mut n = Node::local(&run_iri);
        n.text(ns::TAR, "openLineagePayload", &event.to_string());
        if let Some(ns_) = job_namespace {
            n.text(ns::TAR, "claimedNamespace", ns_);
        }
        if run_in.started_at.is_none() && !terminal {
            n.datetime(ns::PROV, "startedAtTime", event_time.as_deref().unwrap_or(""));
        }
        tx.extend(n.finish());
    }

    let mut produced = Vec::new();
    let mut consumed = Vec::new();

    for (key, role) in [("outputs", "produced"), ("inputs", "consumed")] {
        let Some(items) = event.get(key).and_then(|v| v.as_array()) else { continue };
        for ds in items {
            let artifact_iri = map_dataset(&state, &principal, ds, &run_iri, role, &mut tx).await?;
            if state.ops.claim_advertisement(&run_iri, &artifact_iri, role).await.map_err(AppError::from)? {
                let mut n = if role == "consumed" { Node::local(&run_iri) } else { Node::local(&artifact_iri) };
                if role == "consumed" {
                    n.link(ns::PROV, "used", &artifact_iri);
                } else {
                    n.link(ns::PROV, "wasGeneratedBy", &run_iri);
                }
                tx.extend(n.finish());
            }
            if role == "produced" {
                produced.push(artifact_iri);
            } else {
                consumed.push(artifact_iri);
            }
        }
    }

    state.store.apply(tx).map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(
            Some(&principal.subject),
            principal.actor_kind(),
            "openlineage.ingest",
            Some(&run_iri),
            Some(&event_type),
            None,
        )
        .await;

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "run": run_iri,
            "produced": produced,
            "consumed": consumed,
            "mapped_status": status,
        })),
    ))
}

/// Map one OpenLineage dataset onto an Artifact (spec §7.6 mapping table).
async fn map_dataset(
    state: &Arc<AppState>,
    principal: &Principal,
    ds: &Value,
    _run_iri: &str,
    role: &str,
    tx: &mut GraphTx,
) -> AppResult<String> {
    let namespace = ds.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let name = ds.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let facets = ds.get("facets").cloned().unwrap_or(Value::Null);

    // If the producer put one of our IRIs in the symlinks facet, match it rather than mint.
    if let Some(links) = facets.get("symlinks").and_then(|s| s.get("identifiers")).and_then(|v| v.as_array()) {
        for l in links {
            if let Some(n) = l.get("name").and_then(|v| v.as_str()) {
                if n.starts_with("http")
                    && (ids::is_local(state.base(), n) || state.store.exists(n).map_err(AppError::from)?)
                {
                    return Ok(n.to_string());
                }
            }
        }
    }

    let external_key = format!("{namespace}#{name}");
    if let Some(iri) = state.ops.artifact_for_key(&external_key).await.map_err(AppError::from)? {
        return Ok(iri);
    }

    let media_type =
        facets.get("storage").and_then(|s| s.get("fileFormat")).and_then(|v| v.as_str()).map(str::to_string);
    let access_url = facets
        .get("dataSource")
        .and_then(|s| s.get("uri"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| namespace.starts_with("http").then(|| format!("{namespace}/{name}")));

    // A producer that knows about us can send the full access descriptor in a custom facet.
    let fair = facets.get("fairAccess");
    let distribution = match fair {
        Some(f) => serde_json::from_value::<DistributionIn>(f.clone()).unwrap_or_default(),
        None => DistributionIn {
            access_url,
            media_type,
            access_protocol: Some(protocol_for(namespace)),
            availability: Some("restricted".into()),
            ..Default::default()
        },
    };
    let has_access = distribution.access_url.is_some() || distribution.download_url.is_some();

    let input = ArtifactIn {
        title: Some(if name.is_empty() { external_key.clone() } else { name.to_string() }),
        description: Some(format!("Ingested from an OpenLineage {role} dataset in namespace {namespace}")),
        conforms_to: facets.get("schema").and_then(|_| Some("https://w3id.org/tar/ns#TabularDataset".to_string())),
        external_key: Some(external_key.clone()),
        distributions: if has_access { vec![distribution] } else { Vec::new() },
        ..Default::default()
    };

    let iri = ids::mint(state.base(), Kind::Artifact);
    let quads = crate::domain::artifact::artifact_quads(state.base(), &iri, &input, &principal.subject, None);
    // The vocabulary rule, but not the shapes: this adapter is deliberately lenient about
    // everything OpenLineage does not model, and holding a foreign event to the full shape set
    // would refuse ingest over fields its producer has never heard of. The type is the
    // exception, and the only thing here a caller cannot set — it comes from the mapping table
    // above, so this catches the *adapter* naming a type nobody can look up, which is precisely
    // how an undeclared IRI reached this path in the first place.
    crate::domain::vocabulary::enforce(state, &quads)?;
    tx.extend(quads);
    state.ops.remember_artifact(&external_key, &iri).await.map_err(AppError::from)?;
    Ok(iri)
}

fn protocol_for(namespace: &str) -> String {
    let ns_ = namespace.to_ascii_lowercase();
    if ns_.starts_with("s3") {
        "s3".into()
    } else if ns_.starts_with("file") {
        "file".into()
    } else if ns_.contains("sparql") {
        "sparql".into()
    } else {
        "https".into()
    }
}
