//! Federated-search propagation: hop budget, query identity, loop prevention.
//!
//! Spec §9.6 and `docs/specs/2026-08-31-federated-search-propagation.md`.
//!
//! A federated search at registry A used to stop at A's own peer list. It now *propagates*:
//! A asks B, B asks its own peers, and so on. In a mesh with any cycle (A↔B, B↔C, C↔A) that
//! is an infinite storm unless three independent brakes are applied, which is what this
//! module provides:
//!
//! 1. **Query identity.** The origin mints a `query_id` that travels with every leg. Every
//!    registry claims the id in SQLite before doing any work; a second arrival is refused
//!    with an explicit `already_handled` answer (never a silent empty result). The seen-id
//!    table has a TTL and a hard row cap so it cannot grow without bound.
//! 2. **A hop budget.** `fed_hops` decrements at each hop and stops the walk at zero. A
//!    receiving registry clamps whatever it was granted to *its own* configured maximum, so
//!    a malicious peer cannot hand out a budget bigger than the one it was given.
//! 3. **A time budget.** The caller grants a millisecond budget; the callee clamps it to its
//!    own and spends strictly less than it on its own fan-out, so the whole walk — however
//!    deep — completes inside the origin's per-peer timeout.
//!
//! None of the settings live in `Config`: that file is owned elsewhere while this lands, so
//! they are read from the environment here with the same `TAR_*` naming and the same
//! duration grammar as `crate::config`. Moving them into `Config` is a mechanical change.

use crate::config::parse_duration;
use crate::model::{Origin, SearchHit};
use crate::ops::Ops;
use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::time::Duration;

// --------------------------------------------------------------------- settings

/// Tunables for propagation. Read per request (a handful of `getenv` calls, next to a
/// network fan-out) so tests and operators can change them without a restart-only config.
#[derive(Clone, Debug)]
pub struct FedSettings {
    /// Hops a query may travel from the origin. 0 disables propagation entirely (the old
    /// one-hop-only behaviour is `max_hops = 1`).
    pub max_hops: u32,
    /// Wall-clock ceiling for the whole fan-out at one registry.
    pub total_timeout: Duration,
    /// Milliseconds held back from a granted budget to cover the round trip, so a callee
    /// always answers *before* its caller gives up on it.
    pub hop_margin: Duration,
    /// Peers contacted in one fan-out. A registry with 400 peers must not open 400 sockets.
    pub max_peers: usize,
    /// Bytes read from one peer's response before it is treated as abusive.
    pub max_peer_bytes: usize,
    /// Hits accepted from one peer's response.
    pub max_peer_hits: usize,
    /// Peer-status rows accepted from one peer's response (the topology it reports).
    pub max_peer_statuses: usize,
    /// Hits in our own response, after merging and sorting.
    pub max_total_hits: usize,
    /// How long a query id stays claimed. Must comfortably exceed one query's lifetime.
    pub id_ttl: Duration,
    /// Hard cap on rows in `federated_queries`, enforced on every claim.
    pub max_seen_rows: i64,
}

impl Default for FedSettings {
    fn default() -> Self {
        Self {
            max_hops: 3,
            total_timeout: Duration::from_secs(10),
            hop_margin: Duration::from_millis(600),
            max_peers: 12,
            max_peer_bytes: 2 * 1024 * 1024,
            max_peer_hits: 100,
            max_peer_statuses: 32,
            max_total_hits: 500,
            id_ttl: Duration::from_secs(600),
            max_seen_rows: 50_000,
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_usize(key: &str, default: usize, max: usize) -> usize {
    env(key).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(default).min(max)
}

impl FedSettings {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            // Clamped: a 40-hop budget is not a configuration, it is an outage.
            max_hops: env("TAR_FEDERATED_SEARCH_MAX_HOPS")
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(d.max_hops)
                .min(8),
            total_timeout: env("TAR_FEDERATED_SEARCH_TOTAL_TIMEOUT")
                .and_then(|v| parse_duration(&v).ok())
                .unwrap_or(d.total_timeout)
                .min(Duration::from_secs(60)),
            hop_margin: env("TAR_FEDERATED_SEARCH_HOP_MARGIN")
                .and_then(|v| parse_duration(&v).ok())
                .unwrap_or(d.hop_margin),
            max_peers: env_usize("TAR_FEDERATED_SEARCH_MAX_PEERS", d.max_peers, 64),
            max_peer_bytes: env_usize("TAR_FEDERATED_SEARCH_MAX_PEER_BYTES", d.max_peer_bytes, 32 * 1024 * 1024),
            max_peer_hits: env_usize("TAR_FEDERATED_SEARCH_MAX_PEER_HITS", d.max_peer_hits, 1000),
            max_peer_statuses: env_usize("TAR_FEDERATED_SEARCH_MAX_PEER_STATUSES", d.max_peer_statuses, 256),
            max_total_hits: env_usize("TAR_FEDERATED_SEARCH_MAX_TOTAL_HITS", d.max_total_hits, 5000),
            id_ttl: env("TAR_FEDERATED_SEARCH_ID_TTL").and_then(|v| parse_duration(&v).ok()).unwrap_or(d.id_ttl),
            max_seen_rows: d.max_seen_rows,
        }
    }
}

// ------------------------------------------------------------------- query ids

/// Mint a query id. UUIDv7 so the seen-id table is swept in insertion order.
pub fn new_query_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Accept a query id from the wire, or reject it.
///
/// The id is attacker-controlled: it arrives on an unauthenticated `GET` and is stored,
/// echoed back and logged. Anything but a short, boring token is refused outright rather
/// than sanitised into something that no longer matches what the sender deduplicates on.
pub fn valid_query_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

#[derive(Debug, Clone)]
pub enum Claim {
    /// This registry had not seen the id; it owns this leg of the query.
    Fresh,
    /// Seen before. The caller must refuse to handle it again.
    AlreadyHandled { first_seen_at: String, repeat_count: i64 },
}

impl Claim {
    pub fn is_fresh(&self) -> bool {
        matches!(self, Claim::Fresh)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SeenQuery {
    pub query_id: String,
    pub first_seen_at: String,
    pub origin: Option<String>,
    pub received_from: Option<String>,
    pub repeat_count: i64,
}

/// Claim a query id for this registry, atomically.
///
/// `INSERT OR IGNORE` is the whole loop-prevention primitive: exactly one of any number of
/// concurrent arrivals of the same id inserts a row, and every other arrival learns it lost.
/// SQLite serialises writers, so two legs of the same query racing in from two peers cannot
/// both come back `Fresh`.
pub async fn claim_query(
    ops: &Ops,
    query_id: &str,
    origin: Option<&str>,
    received_from: Option<&str>,
    ttl: Duration,
    max_rows: i64,
) -> Result<Claim> {
    let now = Utc::now();
    let ttl = ChronoDuration::from_std(ttl).unwrap_or_else(|_| ChronoDuration::minutes(10));
    let r = sqlx::query(
        "INSERT OR IGNORE INTO federated_queries (query_id, first_seen_at, expires_at, origin, received_from, repeat_count)
         VALUES (?,?,?,?,?,0)",
    )
    .bind(query_id)
    .bind(now.to_rfc3339())
    .bind((now + ttl).to_rfc3339())
    .bind(origin)
    .bind(received_from)
    .execute(ops.pool())
    .await?;

    if r.rows_affected() > 0 {
        // Sweep on the write path: no background task to own, and the table only grows on
        // this path anyway. Expiry first, then a hard row cap in case a flood outruns the TTL.
        let _ = sqlx::query("DELETE FROM federated_queries WHERE expires_at < ?")
            .bind(now.to_rfc3339())
            .execute(ops.pool())
            .await;
        let _ = sqlx::query(
            "DELETE FROM federated_queries WHERE query_id IN (
                 SELECT query_id FROM federated_queries ORDER BY first_seen_at DESC LIMIT -1 OFFSET ?
             )",
        )
        .bind(max_rows)
        .execute(ops.pool())
        .await;
        return Ok(Claim::Fresh);
    }

    sqlx::query("UPDATE federated_queries SET repeat_count = repeat_count + 1 WHERE query_id = ?")
        .bind(query_id)
        .execute(ops.pool())
        .await?;
    let row = sqlx::query("SELECT first_seen_at, repeat_count FROM federated_queries WHERE query_id = ?")
        .bind(query_id)
        .fetch_optional(ops.pool())
        .await?;
    match row {
        Some(r) => Ok(Claim::AlreadyHandled {
            first_seen_at: r.try_get("first_seen_at").unwrap_or_default(),
            repeat_count: r.try_get("repeat_count").unwrap_or(1),
        }),
        // The row expired between the failed insert and this read. Treat the leg as fresh:
        // a missed dedup costs one duplicate answer, a wrong refusal costs the results.
        None => Ok(Claim::Fresh),
    }
}

/// What this registry recorded for a query id. Used by the propagation tests to prove that
/// no registry handled the same query twice.
pub async fn seen_query(ops: &Ops, query_id: &str) -> Result<Option<SeenQuery>> {
    let row = sqlx::query("SELECT * FROM federated_queries WHERE query_id = ?")
        .bind(query_id)
        .fetch_optional(ops.pool())
        .await?;
    Ok(row.map(|r| SeenQuery {
        query_id: r.try_get("query_id").unwrap_or_default(),
        first_seen_at: r.try_get("first_seen_at").unwrap_or_default(),
        origin: r.try_get("origin").ok().flatten(),
        received_from: r.try_get("received_from").ok().flatten(),
        repeat_count: r.try_get("repeat_count").unwrap_or(0),
    }))
}

// ----------------------------------------------------------------- wire shapes

/// How a result or a peer status reached the registry that is reporting it.
///
/// This is the distinction the origin chip in the UI depends on: a hit from a peer the
/// operator chose to trust is not the same evidence as a hit relayed by that peer from
/// somewhere the operator has never heard of.
pub mod reach {
    /// Answered from this registry's own graph.
    pub const LOCAL: &str = "local";
    /// From a peer in this registry's own peer list.
    pub const DIRECT: &str = "direct";
    /// Relayed to us by a peer, from a registry we do not peer with ourselves.
    pub const INDIRECT: &str = "indirect";
}

/// `SearchHit` plus provenance about the path it travelled.
///
/// `#[serde(flatten)]` keeps the JSON shape of `SearchHit` byte-for-byte, so an older peer's
/// response still deserialises (the added fields default) and an older client still reads
/// ours. `model::SearchHit` is not edited — it is owned elsewhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSearchHit {
    #[serde(flatten)]
    pub hit: SearchHit,
    /// `local` | `direct` | `indirect`.
    #[serde(default = "default_local_reach")]
    pub reach: String,
    /// Registry-to-registry hops this hit crossed to reach this response. 0 = local.
    #[serde(default)]
    pub hops: u32,
    /// The *directly configured* peer this hit entered through. `None` for local hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

fn default_local_reach() -> String {
    reach::LOCAL.to_string()
}

impl FedSearchHit {
    pub fn local(hit: SearchHit) -> Self {
        Self { hit, reach: reach::LOCAL.into(), hops: 0, via: None }
    }

    /// Re-attribute a hit that arrived in a peer's response, from that peer's point of view
    /// to ours: one more hop, entered through `direct_peer`.
    pub fn relayed(mut self, direct_peer: &crate::ops::PeerRecord) -> Self {
        let was_local_to_peer = self.hops == 0;
        self.hops = self.hops.saturating_add(1);
        self.reach = if self.hops == 1 { reach::DIRECT.into() } else { reach::INDIRECT.into() };
        self.via = Some(direct_peer.base_iri.clone());
        if was_local_to_peer {
            // The peer minted this IRI, so it is the home registry.
            self.hit.origin = Origin {
                kind: "peer".into(),
                peer_id: Some(direct_peer.id.clone()),
                peer_title: direct_peer.title.clone(),
                peer_base_iri: Some(direct_peer.base_iri.clone()),
                cached_at: None,
                resolve_status: Some("live".into()),
            };
        } else {
            // Already attributed to its home registry by the peer that relayed it. Keep that
            // attribution, but drop the peer id: it is an identifier in *their* peer table
            // and means nothing in ours.
            self.hit.origin.peer_id = None;
            if self.hit.origin.kind == "local" {
                self.hit.origin.kind = "peer".into();
            }
        }
        self
    }
}

/// `PeerSearchStatus` plus how the peer was reached. Same field names as the model type, so
/// the existing contract is a subset; `model::PeerSearchStatus` is not edited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedPeerStatus {
    #[serde(default)]
    pub peer_id: String,
    pub base_iri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `ok` | `timeout` | `error` | `already_handled` | `skipped`.
    pub status: String,
    #[serde(default)]
    pub hits: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `direct` | `indirect`.
    #[serde(default = "default_direct_reach")]
    pub reach: String,
    /// Hops from the registry answering this request. 1 = a peer of ours.
    #[serde(default = "one")]
    pub hops: u32,
    /// The directly configured peer that told us about this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Human-readable colour for a non-error outcome (`already_handled`, `skipped`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn default_direct_reach() -> String {
    reach::DIRECT.to_string()
}

fn one() -> u32 {
    1
}

impl FedPeerStatus {
    pub fn direct(peer: &crate::ops::PeerRecord, status: &str) -> Self {
        Self {
            peer_id: peer.id.clone(),
            base_iri: peer.base_iri.clone(),
            title: peer.title.clone(),
            status: status.into(),
            hits: 0,
            error: None,
            reach: reach::DIRECT.into(),
            hops: 1,
            via: None,
        note: None,
        }
    }

    /// A status a peer reported about *its* peers, re-expressed from our point of view.
    pub fn relayed(mut self, direct_peer: &crate::ops::PeerRecord) -> Self {
        self.hops = self.hops.saturating_add(1);
        self.reach = if self.hops == 1 { reach::DIRECT.into() } else { reach::INDIRECT.into() };
        self.via = Some(direct_peer.base_iri.clone());
        // Their peer id is meaningless here, and the UI keys rows on it.
        self.peer_id = format!("{}|{}", direct_peer.id, self.base_iri);
        self
    }
}

/// What a federated search reports about the walk itself.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FederationTrace {
    /// The id that travelled with this query.
    pub query_id: String,
    /// Base IRI of the registry that minted it, as claimed by the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The registry that produced this response.
    pub registry: String,
    /// Our configured ceiling, after clamping.
    pub max_hops: u32,
    /// Hops we were still allowed to spend when this request arrived.
    pub hops_granted: u32,
    /// Hops we passed on to our peers.
    pub hops_forwarded: u32,
    /// True when peers existed that we did not ask because the budget ran out. The answer is
    /// bounded, not complete — and says so rather than pretending it swept the network.
    #[serde(default)]
    pub budget_exhausted: bool,
    /// Registries already on the path when this request arrived, us appended.
    #[serde(default)]
    pub path: Vec<String>,
    /// Set on a refusal. See `already_handled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The federated search response. A superset of `model::SearchResults` — same field names,
/// same types — so every existing consumer keeps working and only propagation-aware ones
/// read the extra members.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FedSearchResults {
    pub query: String,
    pub hits: Vec<FedSearchHit>,
    pub total: i64,
    /// At least one peer, anywhere on the walk, failed or timed out.
    pub partial: bool,
    pub peers: Vec<FedPeerStatus>,
    /// **This registry refused to handle the query a second time.** Set when a query id we
    /// have already served comes back to us — which is what a cycle looks like from the
    /// inside. It is never a silent empty result: `hits` is empty *because* the caller
    /// already has these results on the path that reached us first, and `federation.reason`
    /// says so in words.
    #[serde(default, skip_serializing_if = "is_false")]
    pub already_handled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub federation: Option<FederationTrace>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl FedSearchResults {
    pub fn empty(query: &str) -> Self {
        Self { query: query.into(), hits: vec![], total: 0, partial: false, peers: vec![], already_handled: false, federation: None }
    }
}

// ------------------------------------------------------------ path bookkeeping

/// The `fed_path` header value: registries already on this query's path.
///
/// Parsing is defensive — it arrives from a peer. Entries are trimmed, empties dropped,
/// duplicates dropped, each entry length-capped, and the list capped, so a peer cannot make
/// us build a megabyte URL for the next hop.
pub fn parse_path(raw: Option<&str>, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.unwrap_or_default().split(',') {
        let p = part.trim().trim_end_matches('/');
        if p.is_empty() || p.len() > 200 || out.iter().any(|e| e == p) {
            continue;
        }
        out.push(p.to_string());
        if out.len() >= cap {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_query_id_can_only_be_claimed_once() {
        let ops = Ops::open(":memory:").await.unwrap();
        let ttl = Duration::from_secs(600);
        let first = claim_query(&ops, "q-1", Some("https://a.example"), None, ttl, 100).await.unwrap();
        assert!(first.is_fresh());

        for expected_repeats in 1..=3 {
            match claim_query(&ops, "q-1", Some("https://a.example"), Some("https://b.example"), ttl, 100).await.unwrap() {
                Claim::AlreadyHandled { repeat_count, .. } => assert_eq!(repeat_count, expected_repeats),
                Claim::Fresh => panic!("a repeated query id must never be claimed twice"),
            }
        }
        // A different id is unaffected.
        assert!(claim_query(&ops, "q-2", None, None, ttl, 100).await.unwrap().is_fresh());
        let seen = seen_query(&ops, "q-1").await.unwrap().expect("recorded");
        assert_eq!(seen.repeat_count, 3);
    }

    #[tokio::test]
    async fn expired_ids_are_swept_and_the_table_is_capped() {
        let ops = Ops::open(":memory:").await.unwrap();
        // A zero TTL expires immediately, so the next claim sweeps it and the id is free.
        assert!(claim_query(&ops, "q-old", None, None, Duration::from_secs(0), 100).await.unwrap().is_fresh());
        assert!(claim_query(&ops, "q-new", None, None, Duration::from_secs(600), 100).await.unwrap().is_fresh());
        assert!(seen_query(&ops, "q-old").await.unwrap().is_none(), "an expired id must be swept");

        // With a cap of 2 rows, a third claim evicts the oldest.
        let ops = Ops::open(":memory:").await.unwrap();
        for i in 0..5 {
            claim_query(&ops, &format!("q{i}"), None, None, Duration::from_secs(600), 2).await.unwrap();
        }
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM federated_queries").fetch_one(ops.pool()).await.unwrap();
        assert!(n <= 2, "seen-id table must stay capped, got {n}");
    }

    #[test]
    fn query_ids_from_the_wire_are_validated_not_sanitised() {
        assert!(valid_query_id(&new_query_id()));
        assert!(valid_query_id("cycle-test-1"));
        assert!(!valid_query_id(""));
        assert!(!valid_query_id("a".repeat(101).as_str()));
        assert!(!valid_query_id("q'; DROP TABLE peers;--"));
        assert!(!valid_query_id("q\u{202e}spoof"));
    }

    #[test]
    fn a_path_from_a_peer_cannot_grow_without_bound() {
        let raw = (0..500).map(|i| format!("https://r{i}.example")).collect::<Vec<_>>().join(",");
        assert_eq!(parse_path(Some(&raw), 6).len(), 6);
        assert_eq!(parse_path(Some("https://a.example/, ,https://a.example"), 6), vec!["https://a.example"]);
        assert!(parse_path(None, 6).is_empty());
    }

    #[test]
    fn the_hop_budget_is_clamped_however_it_is_configured() {
        std::env::set_var("TAR_FEDERATED_SEARCH_MAX_HOPS", "9999");
        assert_eq!(FedSettings::from_env().max_hops, 8);
        std::env::remove_var("TAR_FEDERATED_SEARCH_MAX_HOPS");
    }
}
