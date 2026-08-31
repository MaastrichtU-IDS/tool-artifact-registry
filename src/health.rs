//! Liveness probing for deployments that serve an endpoint.
//!
//! Until now `tar:health` was read but never written, so every deployment rendered "unknown"
//! forever (README known gap 4). This fills it in the only way that is honest: the registry
//! asks, rather than believing what a deployment says about itself. A deployment asserting it
//! is up is a claim; the interesting case is precisely the one where it cannot answer.
//!
//! A deployment with no endpoint — a CLI, a desktop install — is never probed and never
//! reports "down" for it. Its liveness signal is `tar:lastSeenAt`, stamped when it announces
//! itself or advertises a run.

use crate::domain::Ctx;
use crate::ns;
use crate::state::AppState;
use crate::store::GraphTx;
use std::sync::Arc;
use std::time::Duration;

pub struct Settings {
    pub enabled: bool,
    pub interval: Duration,
    pub timeout: Duration,
    /// Whether to probe endpoints on private or loopback addresses.
    ///
    /// Default **on**, unlike the webhook guard. The two look alike and are not: a webhook URL
    /// is chosen by whoever registers the subscription and points anywhere, so refusing private
    /// targets is what stops the registry being used to reach inside a network. An endpoint is
    /// the address of a deployment in this estate, and for an internal registry it is *normally*
    /// private — refusing those would mean the feature never works where it is most wanted.
    pub allow_private: bool,
    pub batch: usize,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

impl Settings {
    pub fn from_env() -> Self {
        let dur = |k: &str, d: &str| {
            crate::config::parse_duration(&env(k).unwrap_or_else(|| d.into())).unwrap_or_else(|_| {
                crate::config::parse_duration(d).expect("built-in default parses")
            })
        };
        Self {
            enabled: env("TAR_HEALTH_CHECK_ENABLED").map(|v| v == "1" || v == "true").unwrap_or(true),
            interval: dur("TAR_HEALTH_CHECK_INTERVAL", "5m"),
            timeout: dur("TAR_HEALTH_CHECK_TIMEOUT", "5s"),
            allow_private: env("TAR_HEALTH_ALLOW_PRIVATE").map(|v| v == "1" || v == "true").unwrap_or(true),
            batch: env("TAR_HEALTH_CHECK_BATCH").and_then(|v| v.parse().ok()).unwrap_or(20),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Verdict {
    pub health: &'static str,
    pub detail: String,
}

/// Which URL the probe went to, because the same status means different things at the two.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProbeTarget {
    /// `tar:healthEndpoint`: a URL whose only job is to answer this question.
    Declared,
    /// No health endpoint declared, so the endpoint URL itself was probed.
    EndpointRoot,
}

/// Turn an HTTP outcome into a verdict.
///
/// A declared health endpoint must answer **2xx**. It exists for exactly one purpose, so
/// anything else — a 404 saying the route moved, a 503 saying the service is shedding load —
/// is a failure of the thing being asked about.
///
/// A probe that fell back to the endpoint root is judged more loosely, and deliberately so.
/// A great many healthy services answer `401` or `403` at `/` because they require
/// authentication, and `404` because nothing is mounted at the root; calling those deployments
/// "down" would fill the registry with false alarms about services that are working perfectly.
/// At the root the question is only "is something listening and speaking HTTP", so 5xx is down
/// and everything else is up, with the status recorded either way.
pub fn verdict_for(status: Option<u16>, error: Option<&str>, target: ProbeTarget) -> Verdict {
    match (status, error) {
        (Some(s), _) if (200..300).contains(&s) => Verdict { health: "up", detail: format!("responded {s}") },
        (Some(s), _) if target == ProbeTarget::Declared => Verdict {
            health: "down",
            detail: format!("health endpoint responded {s}, and a health endpoint must answer 2xx"),
        },
        (Some(s), _) if s >= 500 => Verdict { health: "down", detail: format!("responded {s}") },
        (Some(s), _) => Verdict {
            health: "up",
            detail: format!("responded {s} at the endpoint root — listening, but declare a health endpoint to be sure"),
        },
        (None, Some(e)) => Verdict { health: "down", detail: e.to_string() },
        (None, None) => Verdict { health: "unknown", detail: "not probed".into() },
    }
}

pub fn is_private_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") || host.ends_with(".internal") {
        return true;
    }
    match host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback() || v6.is_unspecified(),
        Err(_) => false,
    }
}

/// Every deployment worth probing: local, not withdrawn, and serving something.
fn probe_targets(state: &AppState, limit: usize) -> Vec<(String, String, ProbeTarget)> {
    let q = format!(
        r#"{p}
SELECT ?i ?url ?which WHERE {{
  GRAPH <{g}> {{
    ?i a <{t}> .
    OPTIONAL {{ ?i tar:healthEndpoint ?probe }}
    OPTIONAL {{ ?i dcat:endpointURL ?endpoint }}
    BIND(COALESCE(?probe, ?endpoint) AS ?url)
    BIND(IF(BOUND(?probe), "declared", "root") AS ?which)
  }}
  FILTER(BOUND(?url))
  FILTER NOT EXISTS {{ GRAPH ?tg {{ ?i tar:tombstoned true }} }}
}} LIMIT {limit}"#,
        p = ns::PREFIXES,
        g = ns::G_LOCAL,
        t = crate::domain::instance::TYPE_SOFTWARE_AGENT,
    );
    state
        .store
        .select(&q)
        .map(|b| {
            b.rows
                .iter()
                .filter_map(|r| {
                    let which = if r.str("which").as_deref() == Some("declared") {
                        ProbeTarget::Declared
                    } else {
                        ProbeTarget::EndpointRoot
                    };
                    Some((r.iri("i")?, r.iri("url")?, which))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn record(state: &AppState, iri: &str, v: &Verdict) -> anyhow::Result<()> {
    let mut tx = GraphTx::new();
    for pred in ["health", "healthCheckedAt", "healthDetail"] {
        tx.replace_property(iri, &format!("{}{pred}", ns::TAR), ns::G_LOCAL);
    }
    let mut n = crate::rdf::Node::local(iri);
    n.text(ns::TAR, "health", v.health);
    n.datetime(ns::TAR, "healthCheckedAt", &chrono::Utc::now().to_rfc3339());
    n.text(ns::TAR, "healthDetail", &v.detail);
    tx.extend(n.finish());
    state.store.apply(tx)
}

async fn probe(state: &Arc<AppState>, url: &str, target: ProbeTarget, s: &Settings) -> Verdict {
    if !s.allow_private {
        if let Some(host) = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)) {
            if is_private_host(&host) {
                return Verdict { health: "unknown", detail: format!("{host} is not a public address and probing private targets is off") };
            }
        }
    }
    // GET, not HEAD. Health endpoints are routinely written as a GET handler and nothing else,
    // and a framework that answers 405 to the HEAD would have every one of them marked down now
    // that a declared health endpoint is required to return 2xx. The body is never read — the
    // response is dropped after the status line — so this still pulls no arbitrary bytes.
    let result = tokio::time::timeout(s.timeout, state.http.get(url).send()).await;
    match result {
        Ok(Ok(r)) => verdict_for(Some(r.status().as_u16()), None, target),
        Ok(Err(e)) if e.is_connect() => verdict_for(None, Some("could not connect"), target),
        Ok(Err(e)) if e.is_timeout() => verdict_for(None, Some("did not answer in time"), target),
        Ok(Err(_)) => verdict_for(None, Some("request failed"), target),
        Err(_) => verdict_for(None, Some("did not answer in time"), target),
    }
}

/// The background loop, spawned beside the peer resolver and the subscription worker.
pub async fn check_loop(state: Arc<AppState>) {
    let s = Settings::from_env();
    if !s.enabled {
        tracing::info!("health checks disabled");
        return;
    }
    tracing::info!(interval = ?s.interval, "probing deployment endpoints");
    let mut ticker = tokio::time::interval(s.interval);
    loop {
        ticker.tick().await;
        let targets = probe_targets(&state, s.batch);
        for (iri, url, which) in targets {
            let v = probe(&state, &url, which, &s).await;
            if let Err(e) = record(&state, &iri, &v) {
                tracing::warn!(instance = %iri, error = %e, "could not record health");
            } else {
                tracing::debug!(instance = %iri, health = v.health, detail = %v.detail, "probed");
            }
        }
        // Touched here so a caller can see the loop is alive even when nothing is probeable.
        let _ = Ctx::new(&state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ProbeTarget::{Declared, EndpointRoot};

    #[test]
    fn a_declared_health_endpoint_must_answer_2xx() {
        for s in [200, 204] {
            assert_eq!(verdict_for(Some(s), None, Declared).health, "up", "{s}");
        }
        // The whole point of declaring one is that its status is meaningful. A 404 there says
        // the route moved and nobody updated the record; a 503 says the service is refusing
        // work. Both are the deployment failing to be healthy.
        for s in [301, 401, 403, 404, 418, 500, 503] {
            assert_eq!(verdict_for(Some(s), None, Declared).health, "down", "{s}");
        }
    }

    #[test]
    fn at_the_endpoint_root_anything_that_answers_is_up() {
        // No health endpoint was declared, so the root was probed. Plenty of healthy services
        // answer 401 or 404 there; marking them down would be a false alarm about a working
        // deployment, which is worse than a soft "up".
        for s in [200, 204, 301, 401, 403, 404, 418] {
            assert_eq!(verdict_for(Some(s), None, EndpointRoot).health, "up", "{s}");
        }
        for s in [500, 502, 503] {
            assert_eq!(verdict_for(Some(s), None, EndpointRoot).health, "down", "{s}");
        }
    }

    #[test]
    fn a_deployment_that_does_not_answer_is_down_either_way() {
        for t in [Declared, EndpointRoot] {
            assert_eq!(verdict_for(None, Some("could not connect"), t).health, "down");
            assert_eq!(
                verdict_for(None, Some("did not answer in time"), t).detail,
                "did not answer in time"
            );
        }
    }

    #[test]
    fn never_probed_is_unknown_not_down() {
        // "We have not asked" and "we asked and it did not answer" are different facts, and a
        // registry that reported the first as the second would be lying about every new record.
        assert_eq!(verdict_for(None, None, EndpointRoot).health, "unknown");
    }

    #[test]
    fn private_hosts_are_recognised() {
        for h in ["localhost", "127.0.0.1", "10.0.0.5", "192.168.1.1", "169.254.169.254", "::1", "svc.internal"] {
            assert!(is_private_host(h), "{h}");
        }
        for h in ["shacl.ids.unimaas.nl", "example.org", "8.8.8.8"] {
            assert!(!is_private_host(h), "{h}");
        }
    }
}
