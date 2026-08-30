//! Subscription state and the matching rule.
//!
//! A subscription is a standing question asked by a deployment: *"tell me when an artifact
//! like this appears."* The capability model (spec D6) already answers "what **can** produce a
//! SHACL report?" before anything has run, and the run graph answers "where did this file come
//! from?" afterwards. A subscription is the third tense: "what **just** appeared that I care
//! about", answered at the moment it happens instead of by polling a list endpoint.
//!
//! Two things are deliberate here.
//!
//! **Matching is a pure function, not a query.** [`matches`] takes a [`Filter`] and a
//! [`Candidate`] and returns a bool, with no database and no graph. That is what makes the
//! semantics testable — every rule below has a unit test at the bottom of this file — and it
//! keeps the advertisement path free of a per-subscription SPARQL round trip.
//!
//! **A match writes a row, never a socket.** Advertisement must not block on the network
//! (spec §9.3), so the only thing the hot path does is insert into `subscription_deliveries`.
//! Everything with a timeout attached happens later, in the worker, or not at all — a
//! pull-only subscriber drains the very same rows through an HTTP GET it initiates itself.

use crate::ops::Ops;
use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;

// ------------------------------------------------------------------ the filter

/// What an Instance is interested in.
///
/// **Semantics: OR within a field, AND across fields.** An empty field is "don't care". So
/// `{conforms_to: [report, summary], availability: ["public"]}` reads as *"a report or a
/// summary, that I can actually retrieve"*, which is the shape of real interest — the fields
/// people combine are different axes, and the values within one axis are alternatives.
///
/// Every field earns its place; see the design note for the argument. In short:
/// `conforms_to` is the capability question in event form and is the reason the feature
/// exists; `availability` separates "I can act on this" from "I can only cite it", and since
/// `metadata-only` is the common case (spec §6.2) a subscription without it is mostly noise;
/// `software` and `instance` are the trust axis ("only from a deployment I believe"); and
/// `keywords` plus `q` carry the project and cohort axis that no ontology covers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    /// `dct:conformsTo` — the artifact type. Full IRIs (EDAM or a local `/type/…`).
    #[serde(default)]
    pub conforms_to: Vec<String>,
    /// Software IRIs. Matches when the producing deployment runs that software, anywhere.
    #[serde(default)]
    pub software: Vec<String>,
    /// Instance IRIs. Matches one named deployment.
    #[serde(default)]
    pub instance: Vec<String>,
    /// `dcat:keyword`, compared case-insensitively.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// `dct:license`, full SPDX IRIs.
    #[serde(default)]
    pub license: Vec<String>,
    /// `public` | `restricted` | `embargoed` | `metadata-only`, against the artifact's
    /// strongest distribution.
    #[serde(default)]
    pub availability: Vec<String>,
    /// Case-insensitive substring of title or description.
    #[serde(default)]
    pub q: Option<String>,
    /// `produced` and/or `consumed`. Empty means `produced` alone: "appears" normally means
    /// "was made", and a consume advertisement of a foreign artifact is a different event that
    /// a subscriber should have to opt into.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Do not notify a deployment about its own output. Default on: a tool that both produces
    /// and subscribes would otherwise wake itself up on every run, and the one thing it
    /// certainly already knows about is what it just made.
    #[serde(default = "yes")]
    pub exclude_own: bool,
}

fn yes() -> bool {
    true
}

/// Written out rather than derived, because `#[derive(Default)]` would make `exclude_own`
/// false and quietly disagree with the serde default above — so a filter that arrived as JSON
/// and a filter constructed in Rust would behave differently. They must not.
impl Default for Filter {
    fn default() -> Self {
        Self {
            conforms_to: Vec::new(),
            software: Vec::new(),
            instance: Vec::new(),
            keywords: Vec::new(),
            license: Vec::new(),
            availability: Vec::new(),
            q: None,
            roles: Vec::new(),
            exclude_own: yes(),
        }
    }
}

impl Filter {
    /// Roles this filter listens on, with the default applied.
    pub fn effective_roles(&self) -> Vec<&str> {
        if self.roles.is_empty() {
            return vec![ROLE_PRODUCED];
        }
        self.roles.iter().map(String::as_str).collect()
    }

    /// True when nothing is constrained at all. A catch-all subscription is legal — "tell me
    /// about everything" is a real thing to want on a small registry — but the UI says so out
    /// loud rather than letting someone create it by accident.
    pub fn is_catch_all(&self) -> bool {
        self.conforms_to.is_empty()
            && self.software.is_empty()
            && self.instance.is_empty()
            && self.keywords.is_empty()
            && self.license.is_empty()
            && self.availability.is_empty()
            && self.q.as_deref().map(str::trim).unwrap_or("").is_empty()
    }
}

pub const ROLE_PRODUCED: &str = "produced";
pub const ROLE_CONSUMED: &str = "consumed";
pub const AVAILABILITIES: [&str; 4] = ["public", "restricted", "embargoed", "metadata-only"];

/// Everything the matcher is allowed to see about one advertised artifact.
///
/// Built once per artifact from the graph the advertisement just wrote, then tested against
/// every subscription. Local reads only — no peer is contacted to decide a match.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub artifact_iri: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub conforms_to: Option<String>,
    pub license: Option<String>,
    pub keywords: Vec<String>,
    pub availability: String,
    /// The deployment that advertised it. `None` for a curator's direct registration.
    pub instance_iri: Option<String>,
    /// The software that deployment runs, resolved through `tar:instanceOf`.
    pub software_iri: Option<String>,
    pub role: String,
}

/// The whole matching rule, in one place, with no I/O.
///
/// `owner_instance` is the subscribing deployment, needed only for `exclude_own`.
pub fn matches(filter: &Filter, owner_instance: &str, c: &Candidate) -> bool {
    if !filter.effective_roles().iter().any(|r| *r == c.role) {
        return false;
    }
    if filter.exclude_own && c.instance_iri.as_deref() == Some(owner_instance) {
        return false;
    }
    if !any_of(&filter.conforms_to, c.conforms_to.as_deref()) {
        return false;
    }
    if !any_of(&filter.software, c.software_iri.as_deref()) {
        return false;
    }
    if !any_of(&filter.instance, c.instance_iri.as_deref()) {
        return false;
    }
    if !any_of(&filter.license, c.license.as_deref()) {
        return false;
    }
    if !any_of(&filter.availability, Some(c.availability.as_str())) {
        return false;
    }
    if !filter.keywords.is_empty() {
        let hit = filter
            .keywords
            .iter()
            .any(|want| c.keywords.iter().any(|have| have.trim().eq_ignore_ascii_case(want.trim())));
        if !hit {
            return false;
        }
    }
    if let Some(q) = filter.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let needle = q.to_lowercase();
        let hay = format!(
            "{} {}",
            c.title.as_deref().unwrap_or_default(),
            c.description.as_deref().unwrap_or_default()
        )
        .to_lowercase();
        if !hay.contains(&needle) {
            return false;
        }
    }
    true
}

/// An empty constraint accepts anything; a non-empty one needs a value that is in the list.
/// An artifact with no licence at all therefore does **not** match `license: [...]`, which is
/// the honest reading: we cannot claim it is CC-BY when nobody said so.
fn any_of(allowed: &[String], value: Option<&str>) -> bool {
    if allowed.is_empty() {
        return true;
    }
    match value {
        Some(v) => allowed.iter().any(|a| a == v),
        None => false,
    }
}

// ------------------------------------------------------------------- records

#[derive(Debug, Clone, Serialize)]
pub struct Subscription {
    pub id: String,
    pub instance_iri: String,
    pub label: Option<String>,
    pub filter: Filter,
    pub webhook_url: Option<String>,
    /// Whether a signing secret exists. The secret itself is never listed back.
    pub webhook_signed: bool,
    pub enabled: bool,
    pub delivery_state: String,
    pub consecutive_failures: i64,
    pub cursor_seq: i64,
    pub created_at: String,
    pub created_by: Option<String>,
    pub updated_at: Option<String>,
    pub last_match_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
    pub last_polled_at: Option<String>,
    /// Rolled up from the delivery queue so the owner sees the health without a second call.
    #[serde(default)]
    pub pending_count: i64,
    #[serde(default)]
    pub failed_count: i64,
    #[serde(default)]
    pub dead_count: i64,
    /// Matches the subscriber has not acknowledged. Meaningful for the pull path.
    #[serde(default)]
    pub unacked_count: i64,
}

impl Subscription {
    /// A subscription the worker should attempt over HTTP.
    pub fn is_pushable(&self) -> bool {
        self.enabled && self.delivery_state == "active" && self.webhook_url.is_some()
    }
    pub fn delivery_mode(&self) -> &'static str {
        if self.webhook_url.is_some() {
            "webhook"
        } else {
            "pull"
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    pub seq: i64,
    pub id: String,
    pub subscription_id: String,
    pub artifact_iri: String,
    pub run_iri: Option<String>,
    pub role: String,
    pub matched_at: String,
    pub status: String,
    pub attempts: i64,
    pub last_attempt_at: Option<String>,
    pub next_attempt_at: Option<String>,
    pub last_error: Option<String>,
    pub last_status: Option<i64>,
    pub delivered_at: Option<String>,
    /// The frozen notification body.
    pub notification: serde_json::Value,
}

/// A delivery the worker has picked up, joined with what it needs to send it.
#[derive(Debug, Clone)]
pub struct DueDelivery {
    pub seq: i64,
    pub id: String,
    pub subscription_id: String,
    pub webhook_url: String,
    pub webhook_secret: Option<String>,
    pub payload: String,
    pub attempts: i64,
}

// -------------------------------------------------------------- delivery policy

/// Retry policy. Tuned so a receiver that is merely restarting is not given up on, and a
/// receiver that is gone stops costing anything within a day.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Attempts on one delivery before it is `dead` and never retried.
    pub max_attempts: i64,
    /// Consecutive failed attempts on a subscription before its webhook is suspended.
    pub suspend_after: i64,
    pub base_backoff_secs: i64,
    pub max_backoff_secs: i64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 8, suspend_after: 12, base_backoff_secs: 30, max_backoff_secs: 6 * 3600 }
    }
}

impl RetryPolicy {
    /// Read from the environment with the same `TAR_*` grammar as the rest of the registry.
    /// These do not live in `Config` because that file is owned elsewhere while this lands;
    /// moving them there is mechanical.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            max_attempts: env_i64("TAR_SUBSCRIPTION_MAX_ATTEMPTS", d.max_attempts).clamp(1, 20),
            suspend_after: env_i64("TAR_SUBSCRIPTION_SUSPEND_AFTER", d.suspend_after).clamp(1, 500),
            base_backoff_secs: env_i64("TAR_SUBSCRIPTION_BACKOFF_BASE", d.base_backoff_secs).clamp(1, 3600),
            max_backoff_secs: env_i64("TAR_SUBSCRIPTION_BACKOFF_MAX", d.max_backoff_secs).clamp(1, 7 * 86400),
        }
    }

    /// Exponential, capped. `attempts` is the count *including* the one that just failed, so
    /// the first retry waits `base`, the second `2 × base`, and so on.
    pub fn backoff_secs(&self, attempts: i64) -> i64 {
        let shift = (attempts.max(1) - 1).min(20) as u32;
        self.base_backoff_secs.saturating_mul(1i64 << shift.min(40)).min(self.max_backoff_secs)
    }
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(default)
}

// ------------------------------------------------------------------ store ops

/// Cap on subscriptions per Instance. A webhook makes the registry issue outbound HTTP to an
/// address someone else chose; an unbounded number of them makes it a free traffic amplifier.
pub const MAX_PER_INSTANCE: i64 = 32;

#[derive(Debug, Clone)]
pub struct NewSubscription {
    pub instance_iri: String,
    pub label: Option<String>,
    pub filter: Filter,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<String>,
    pub created_by: Option<String>,
}

pub async fn create(ops: &Ops, new: &NewSubscription) -> Result<Subscription> {
    let id = uuid::Uuid::now_v7().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO subscriptions (id, instance_iri, label, filter, webhook_url, webhook_secret, created_at, created_by, updated_at)
         VALUES (?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&new.instance_iri)
    .bind(&new.label)
    .bind(serde_json::to_string(&new.filter)?)
    .bind(&new.webhook_url)
    .bind(&new.webhook_secret)
    .bind(&now)
    .bind(&new.created_by)
    .bind(&now)
    .execute(ops.pool())
    .await?;
    get(ops, &id).await?.ok_or_else(|| anyhow::anyhow!("subscription vanished immediately after insert"))
}

pub async fn get(ops: &Ops, id: &str) -> Result<Option<Subscription>> {
    let row = sqlx::query("SELECT * FROM subscriptions WHERE id = ?").bind(id).fetch_optional(ops.pool()).await?;
    let Some(row) = row else { return Ok(None) };
    let mut s = from_row(&row)?;
    attach_counts(ops, &mut s).await?;
    Ok(Some(s))
}

pub async fn list_for_instance(ops: &Ops, instance_iri: &str) -> Result<Vec<Subscription>> {
    let rows = sqlx::query("SELECT * FROM subscriptions WHERE instance_iri = ? ORDER BY created_at DESC")
        .bind(instance_iri)
        .fetch_all(ops.pool())
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let mut s = from_row(r)?;
        attach_counts(ops, &mut s).await?;
        out.push(s);
    }
    Ok(out)
}

pub async fn count_for_instance(ops: &Ops, instance_iri: &str) -> Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) AS n FROM subscriptions WHERE instance_iri = ?")
        .bind(instance_iri)
        .fetch_one(ops.pool())
        .await?;
    Ok(row.try_get::<i64, _>("n").unwrap_or(0))
}

/// Every subscription that could match, cheaply. Matching itself is [`matches`]; this is only
/// the "do not even consider a disabled one" pre-filter.
///
/// A registry with thousands of subscriptions would want an index on `conforms_to` here rather
/// than a full scan per advertisement. At the scale this registry targets — tens of
/// deployments — a scan of a few dozen rows next to a graph write is not the bottleneck, and
/// an index that disagreed with [`matches`] would be a correctness bug waiting to happen.
pub async fn active_subscriptions(ops: &Ops) -> Result<Vec<Subscription>> {
    let rows = sqlx::query("SELECT * FROM subscriptions WHERE enabled = 1").fetch_all(ops.pool()).await?;
    rows.iter().map(from_row).collect()
}

#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub label: Option<Option<String>>,
    pub filter: Option<Filter>,
    pub webhook_url: Option<Option<String>>,
    pub webhook_secret: Option<Option<String>>,
    pub enabled: Option<bool>,
    /// Clear the failure counters and un-suspend.
    pub resume: bool,
}

pub async fn update(ops: &Ops, id: &str, p: &Patch) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    if let Some(label) = &p.label {
        sqlx::query("UPDATE subscriptions SET label = ? WHERE id = ?").bind(label).bind(id).execute(ops.pool()).await?;
    }
    if let Some(f) = &p.filter {
        sqlx::query("UPDATE subscriptions SET filter = ? WHERE id = ?")
            .bind(serde_json::to_string(f)?)
            .bind(id)
            .execute(ops.pool())
            .await?;
    }
    if let Some(url) = &p.webhook_url {
        sqlx::query("UPDATE subscriptions SET webhook_url = ? WHERE id = ?").bind(url).bind(id).execute(ops.pool()).await?;
    }
    if let Some(secret) = &p.webhook_secret {
        sqlx::query("UPDATE subscriptions SET webhook_secret = ? WHERE id = ?").bind(secret).bind(id).execute(ops.pool()).await?;
    }
    if let Some(e) = p.enabled {
        sqlx::query("UPDATE subscriptions SET enabled = ? WHERE id = ?")
            .bind(if e { 1 } else { 0 })
            .bind(id)
            .execute(ops.pool())
            .await?;
    }
    if p.resume {
        // Resuming re-arms the queue too: deliveries that died while the endpoint was down are
        // handed one more chance, which is the whole point of fixing the endpoint.
        sqlx::query(
            "UPDATE subscriptions SET delivery_state = 'active', consecutive_failures = 0, last_error = NULL, last_error_at = NULL WHERE id = ?",
        )
        .bind(id)
        .execute(ops.pool())
        .await?;
        sqlx::query(
            "UPDATE subscription_deliveries SET status = 'pending', attempts = 0, next_attempt_at = ?, last_error = NULL
             WHERE subscription_id = ? AND status IN ('dead','failed')",
        )
        .bind(&now)
        .bind(id)
        .execute(ops.pool())
        .await?;
    }
    sqlx::query("UPDATE subscriptions SET updated_at = ? WHERE id = ?").bind(&now).bind(id).execute(ops.pool()).await?;
    Ok(())
}

pub async fn delete(ops: &Ops, id: &str) -> Result<bool> {
    sqlx::query("DELETE FROM subscription_deliveries WHERE subscription_id = ?").bind(id).execute(ops.pool()).await?;
    let r = sqlx::query("DELETE FROM subscriptions WHERE id = ?").bind(id).execute(ops.pool()).await?;
    Ok(r.rows_affected() > 0)
}

/// The webhook signing secret. Read only by the delivery worker.
pub async fn webhook_secret(ops: &Ops, id: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT webhook_secret FROM subscriptions WHERE id = ?")
        .bind(id)
        .fetch_optional(ops.pool())
        .await?;
    Ok(row.and_then(|r| r.try_get::<Option<String>, _>("webhook_secret").ok().flatten()))
}

// -------------------------------------------------------------- the delivery queue

/// Record a match. Returns the new sequence number, or `None` when this artifact was already
/// queued for this subscription in this role — the same idempotency promise advertisement
/// itself makes (spec §7.5), so a retried CI step does not notify twice.
///
/// This is the only thing the advertisement path does, and it is a single SQLite insert.
pub async fn enqueue(
    ops: &Ops,
    subscription_id: &str,
    artifact_iri: &str,
    run_iri: Option<&str>,
    role: &str,
    payload: &serde_json::Value,
) -> Result<Option<i64>> {
    let id = uuid::Uuid::now_v7().to_string();
    let now = Utc::now().to_rfc3339();
    let mut body = payload.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.insert("delivery".into(), serde_json::json!(id));
        obj.insert("matched_at".into(), serde_json::json!(now));
    }
    let r = sqlx::query(
        "INSERT OR IGNORE INTO subscription_deliveries
           (id, subscription_id, artifact_iri, run_iri, role, payload, matched_at, status, next_attempt_at)
         VALUES (?,?,?,?,?,?,?, 'pending', ?)",
    )
    .bind(&id)
    .bind(subscription_id)
    .bind(artifact_iri)
    .bind(run_iri)
    .bind(role)
    .bind(serde_json::to_string(&body)?)
    .bind(&now)
    .bind(&now)
    .execute(ops.pool())
    .await?;
    if r.rows_affected() == 0 {
        return Ok(None);
    }
    sqlx::query("UPDATE subscriptions SET last_match_at = ? WHERE id = ?").bind(&now).bind(subscription_id).execute(ops.pool()).await?;
    let row = sqlx::query("SELECT seq FROM subscription_deliveries WHERE id = ?").bind(&id).fetch_one(ops.pool()).await?;
    Ok(Some(row.try_get::<i64, _>("seq")?))
}

/// Deliveries the worker should attempt now: due, not exhausted, and belonging to a
/// subscription that is enabled, unsuspended and has somewhere to POST to.
pub async fn due_deliveries(ops: &Ops, limit: i64) -> Result<Vec<DueDelivery>> {
    let rows = sqlx::query(
        "SELECT d.seq, d.id, d.subscription_id, d.payload, d.attempts, s.webhook_url, s.webhook_secret
           FROM subscription_deliveries d
           JOIN subscriptions s ON s.id = d.subscription_id
          WHERE d.status IN ('pending','failed')
            AND (d.next_attempt_at IS NULL OR d.next_attempt_at <= ?)
            AND s.enabled = 1
            AND s.delivery_state = 'active'
            AND s.webhook_url IS NOT NULL
          ORDER BY d.next_attempt_at, d.seq
          LIMIT ?",
    )
    .bind(Utc::now().to_rfc3339())
    .bind(limit)
    .fetch_all(ops.pool())
    .await?;
    rows.iter()
        .map(|r| {
            Ok(DueDelivery {
                seq: r.try_get("seq")?,
                id: r.try_get("id")?,
                subscription_id: r.try_get("subscription_id")?,
                webhook_url: r.try_get::<Option<String>, _>("webhook_url")?.unwrap_or_default(),
                webhook_secret: r.try_get("webhook_secret")?,
                payload: r.try_get("payload")?,
                attempts: r.try_get("attempts")?,
            })
        })
        .collect()
}

pub async fn mark_delivered(ops: &Ops, delivery_id: &str, subscription_id: &str, status_code: u16, attempts: i64) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE subscription_deliveries
            SET status='delivered', attempts=?, last_attempt_at=?, delivered_at=?, last_status=?, last_error=NULL, next_attempt_at=NULL
          WHERE id=?",
    )
    .bind(attempts)
    .bind(&now)
    .bind(&now)
    .bind(status_code as i64)
    .bind(delivery_id)
    .execute(ops.pool())
    .await?;
    sqlx::query(
        "UPDATE subscriptions SET consecutive_failures = 0, last_success_at = ?, last_error = NULL, last_error_at = NULL WHERE id = ?",
    )
    .bind(&now)
    .bind(subscription_id)
    .execute(ops.pool())
    .await?;
    Ok(())
}

/// The outcome of a failed attempt, so the caller can tell the operator what happened.
#[derive(Debug, Clone, PartialEq)]
pub struct FailureOutcome {
    pub attempts: i64,
    /// `failed` (will retry) or `dead` (exhausted).
    pub status: String,
    pub retry_in_secs: Option<i64>,
    /// The subscription's webhook was suspended by this failure.
    pub suspended: bool,
}

/// Record a failed attempt: back the delivery off, or bury it; and if the endpoint has been
/// failing consistently, stop attempting it at all until the owner intervenes.
pub async fn mark_failed(
    ops: &Ops,
    delivery_id: &str,
    subscription_id: &str,
    error: &str,
    status_code: Option<u16>,
    policy: &RetryPolicy,
) -> Result<FailureOutcome> {
    let now = Utc::now();
    let now_s = now.to_rfc3339();
    let row = sqlx::query("SELECT attempts FROM subscription_deliveries WHERE id = ?")
        .bind(delivery_id)
        .fetch_optional(ops.pool())
        .await?;
    let attempts: i64 = row.and_then(|r| r.try_get("attempts").ok()).unwrap_or(0) + 1;
    let exhausted = attempts >= policy.max_attempts;
    let retry_in = (!exhausted).then(|| policy.backoff_secs(attempts));
    let next = retry_in.map(|s| (now + ChronoDuration::seconds(s)).to_rfc3339());
    // A truncated error: this is shown in the UI, not a log sink.
    let error = error.chars().take(400).collect::<String>();
    sqlx::query(
        "UPDATE subscription_deliveries
            SET status=?, attempts=?, last_attempt_at=?, next_attempt_at=?, last_error=?, last_status=?
          WHERE id=?",
    )
    .bind(if exhausted { "dead" } else { "failed" })
    .bind(attempts)
    .bind(&now_s)
    .bind(&next)
    .bind(&error)
    .bind(status_code.map(|c| c as i64))
    .bind(delivery_id)
    .execute(ops.pool())
    .await?;
    sqlx::query(
        "UPDATE subscriptions SET consecutive_failures = consecutive_failures + 1, last_error = ?, last_error_at = ? WHERE id = ?",
    )
    .bind(&error)
    .bind(&now_s)
    .bind(subscription_id)
    .execute(ops.pool())
    .await?;
    let failures: i64 = sqlx::query("SELECT consecutive_failures FROM subscriptions WHERE id = ?")
        .bind(subscription_id)
        .fetch_optional(ops.pool())
        .await?
        .and_then(|r| r.try_get("consecutive_failures").ok())
        .unwrap_or(0);
    let mut suspended = false;
    if failures >= policy.suspend_after {
        let r = sqlx::query("UPDATE subscriptions SET delivery_state = 'suspended' WHERE id = ? AND delivery_state != 'suspended'")
            .bind(subscription_id)
            .execute(ops.pool())
            .await?;
        suspended = r.rows_affected() > 0;
    }
    Ok(FailureOutcome {
        attempts,
        status: if exhausted { "dead".into() } else { "failed".into() },
        retry_in_secs: retry_in,
        suspended,
    })
}

/// The pull path: matches after `cursor`, oldest first, so a subscriber behind a firewall
/// drains exactly the same queue a webhook subscriber is pushed.
pub async fn deliveries_since(ops: &Ops, subscription_id: &str, cursor: i64, limit: i64) -> Result<Vec<Delivery>> {
    let rows = sqlx::query(
        "SELECT * FROM subscription_deliveries WHERE subscription_id = ? AND seq > ? ORDER BY seq ASC LIMIT ?",
    )
    .bind(subscription_id)
    .bind(cursor)
    .bind(limit)
    .fetch_all(ops.pool())
    .await?;
    rows.iter().map(delivery_from_row).collect()
}

/// Recent deliveries newest-first, for the management screen. Failures have to be visible to
/// whoever owns the subscription, not only in a server log.
pub async fn recent_deliveries(ops: &Ops, subscription_id: &str, limit: i64) -> Result<Vec<Delivery>> {
    let rows = sqlx::query("SELECT * FROM subscription_deliveries WHERE subscription_id = ? ORDER BY seq DESC LIMIT ?")
        .bind(subscription_id)
        .bind(limit)
        .fetch_all(ops.pool())
        .await?;
    rows.iter().map(delivery_from_row).collect()
}

pub async fn max_seq(ops: &Ops, subscription_id: &str) -> Result<i64> {
    let row = sqlx::query("SELECT COALESCE(MAX(seq), 0) AS m FROM subscription_deliveries WHERE subscription_id = ?")
        .bind(subscription_id)
        .fetch_one(ops.pool())
        .await?;
    Ok(row.try_get::<i64, _>("m").unwrap_or(0))
}

/// Advance the acknowledged cursor. Monotonic: an out-of-order ack never rewinds it, so a
/// retried poll cannot make the registry replay what the subscriber already handled.
pub async fn ack(ops: &Ops, subscription_id: &str, cursor: i64) -> Result<i64> {
    sqlx::query("UPDATE subscriptions SET cursor_seq = MAX(cursor_seq, ?), updated_at = ? WHERE id = ?")
        .bind(cursor)
        .bind(Utc::now().to_rfc3339())
        .bind(subscription_id)
        .execute(ops.pool())
        .await?;
    let row = sqlx::query("SELECT cursor_seq FROM subscriptions WHERE id = ?")
        .bind(subscription_id)
        .fetch_one(ops.pool())
        .await?;
    Ok(row.try_get::<i64, _>("cursor_seq").unwrap_or(cursor))
}

pub async fn touch_polled(ops: &Ops, subscription_id: &str) -> Result<()> {
    sqlx::query("UPDATE subscriptions SET last_polled_at = ? WHERE id = ?")
        .bind(Utc::now().to_rfc3339())
        .bind(subscription_id)
        .execute(ops.pool())
        .await?;
    Ok(())
}

// ------------------------------------------------------------------ row mapping

fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Subscription> {
    let filter: String = row.try_get("filter").unwrap_or_else(|_| "{}".into());
    let secret: Option<String> = row.try_get("webhook_secret").unwrap_or(None);
    Ok(Subscription {
        id: row.try_get("id")?,
        instance_iri: row.try_get("instance_iri")?,
        label: row.try_get("label")?,
        // A filter that no longer parses must not take the whole list down; an empty filter is
        // a catch-all, which is loud rather than silent.
        filter: serde_json::from_str(&filter).unwrap_or_default(),
        webhook_url: row.try_get("webhook_url")?,
        webhook_signed: secret.is_some(),
        enabled: row.try_get::<i64, _>("enabled").unwrap_or(1) != 0,
        delivery_state: row.try_get("delivery_state")?,
        consecutive_failures: row.try_get("consecutive_failures").unwrap_or(0),
        cursor_seq: row.try_get("cursor_seq").unwrap_or(0),
        created_at: row.try_get("created_at")?,
        created_by: row.try_get("created_by")?,
        updated_at: row.try_get("updated_at")?,
        last_match_at: row.try_get("last_match_at")?,
        last_success_at: row.try_get("last_success_at")?,
        last_error: row.try_get("last_error")?,
        last_error_at: row.try_get("last_error_at")?,
        last_polled_at: row.try_get("last_polled_at")?,
        pending_count: 0,
        failed_count: 0,
        dead_count: 0,
        unacked_count: 0,
    })
}

async fn attach_counts(ops: &Ops, s: &mut Subscription) -> Result<()> {
    let row = sqlx::query(
        "SELECT
           SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END) AS pending,
           SUM(CASE WHEN status = 'failed'  THEN 1 ELSE 0 END) AS failed,
           SUM(CASE WHEN status = 'dead'    THEN 1 ELSE 0 END) AS dead,
           SUM(CASE WHEN seq > ?            THEN 1 ELSE 0 END) AS unacked
         FROM subscription_deliveries WHERE subscription_id = ?",
    )
    .bind(s.cursor_seq)
    .bind(&s.id)
    .fetch_one(ops.pool())
    .await?;
    s.pending_count = row.try_get::<Option<i64>, _>("pending").unwrap_or(None).unwrap_or(0);
    s.failed_count = row.try_get::<Option<i64>, _>("failed").unwrap_or(None).unwrap_or(0);
    s.dead_count = row.try_get::<Option<i64>, _>("dead").unwrap_or(None).unwrap_or(0);
    s.unacked_count = row.try_get::<Option<i64>, _>("unacked").unwrap_or(None).unwrap_or(0);
    Ok(())
}

fn delivery_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Delivery> {
    let payload: String = row.try_get("payload").unwrap_or_else(|_| "{}".into());
    Ok(Delivery {
        seq: row.try_get("seq")?,
        id: row.try_get("id")?,
        subscription_id: row.try_get("subscription_id")?,
        artifact_iri: row.try_get("artifact_iri")?,
        run_iri: row.try_get("run_iri")?,
        role: row.try_get("role")?,
        matched_at: row.try_get("matched_at")?,
        status: row.try_get("status")?,
        attempts: row.try_get("attempts").unwrap_or(0),
        last_attempt_at: row.try_get("last_attempt_at")?,
        next_attempt_at: row.try_get("next_attempt_at")?,
        last_error: row.try_get("last_error")?,
        last_status: row.try_get("last_status")?,
        delivered_at: row.try_get("delivered_at")?,
        notification: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
    })
}

// ----------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "https://reg.test/instance/owner";
    const OTHER: &str = "https://reg.test/instance/other";
    const REPORT: &str = "http://edamontology.org/data_2048";
    const GRAPH: &str = "http://edamontology.org/data_2600";

    fn candidate() -> Candidate {
        Candidate {
            artifact_iri: "https://reg.test/artifact/1".into(),
            title: Some("Validation report — patients.ttl".into()),
            description: Some("SHACL report for the MUMC cohort".into()),
            conforms_to: Some(REPORT.into()),
            license: Some("https://spdx.org/licenses/CC-BY-4.0".into()),
            keywords: vec!["shacl".into(), "FHIR".into()],
            availability: "restricted".into(),
            instance_iri: Some(OTHER.into()),
            software_iri: Some("https://reg.test/software/shacl".into()),
            role: ROLE_PRODUCED.into(),
        }
    }

    #[test]
    fn the_rust_default_and_the_json_default_are_the_same_filter() {
        // `{}` off the wire and `Filter::default()` in Rust must mean the identical thing, or
        // a subscription would behave differently depending on which door it came through.
        assert_eq!(Filter::default(), serde_json::from_str::<Filter>("{}").unwrap());
        assert!(Filter::default().exclude_own);
    }

    #[test]
    fn an_empty_filter_matches_any_produced_artifact() {
        assert!(matches(&Filter::default(), OWNER, &candidate()));
    }

    #[test]
    fn type_filter_is_or_within_and_and_across() {
        let f = Filter { conforms_to: vec![GRAPH.into(), REPORT.into()], ..Default::default() };
        assert!(matches(&f, OWNER, &candidate()), "one of two listed types must match");

        let f = Filter { conforms_to: vec![GRAPH.into()], ..Default::default() };
        assert!(!matches(&f, OWNER, &candidate()));

        // Across fields it is AND: the right type but the wrong availability is not a match.
        let f = Filter {
            conforms_to: vec![REPORT.into()],
            availability: vec!["public".into()],
            ..Default::default()
        };
        assert!(!matches(&f, OWNER, &candidate()));
    }

    #[test]
    fn an_artifact_missing_the_field_does_not_match_a_constraint_on_it() {
        // We cannot claim an unlicensed artifact is CC-BY.
        let f = Filter { license: vec!["https://spdx.org/licenses/CC-BY-4.0".into()], ..Default::default() };
        let c = Candidate { license: None, ..candidate() };
        assert!(!matches(&f, OWNER, &c));
        assert!(matches(&f, OWNER, &candidate()));
    }

    #[test]
    fn keywords_are_case_insensitive_and_any_of() {
        let f = Filter { keywords: vec!["fhir".into()], ..Default::default() };
        assert!(matches(&f, OWNER, &candidate()));
        let f = Filter { keywords: vec!["omop".into()], ..Default::default() };
        assert!(!matches(&f, OWNER, &candidate()));
    }

    #[test]
    fn free_text_searches_title_and_description() {
        let f = Filter { q: Some("mumc cohort".into()), ..Default::default() };
        assert!(matches(&f, OWNER, &candidate()));
        let f = Filter { q: Some("patients.ttl".into()), ..Default::default() };
        assert!(matches(&f, OWNER, &candidate()));
        let f = Filter { q: Some("genomics".into()), ..Default::default() };
        assert!(!matches(&f, OWNER, &candidate()));
    }

    #[test]
    fn provenance_filters_match_the_producing_deployment_and_its_software() {
        let f = Filter { instance: vec![OTHER.into()], ..Default::default() };
        assert!(matches(&f, OWNER, &candidate()));
        let f = Filter { instance: vec!["https://reg.test/instance/nope".into()], ..Default::default() };
        assert!(!matches(&f, OWNER, &candidate()));
        let f = Filter { software: vec!["https://reg.test/software/shacl".into()], ..Default::default() };
        assert!(matches(&f, OWNER, &candidate()));
    }

    #[test]
    fn a_deployment_is_not_told_about_its_own_output_by_default() {
        let mine = Candidate { instance_iri: Some(OWNER.into()), ..candidate() };
        assert!(!matches(&Filter::default(), OWNER, &mine));
        let f = Filter { exclude_own: false, ..Default::default() };
        assert!(matches(&f, OWNER, &mine), "opting in must be possible");
    }

    #[test]
    fn consume_advertisements_need_an_explicit_opt_in() {
        let consumed = Candidate { role: ROLE_CONSUMED.into(), ..candidate() };
        assert!(!matches(&Filter::default(), OWNER, &consumed));
        let f = Filter { roles: vec![ROLE_CONSUMED.into()], ..Default::default() };
        assert!(matches(&f, OWNER, &consumed));
        assert!(!matches(&f, OWNER, &candidate()), "opting into consumed must not silently keep produced");
    }

    #[test]
    fn catch_all_is_recognised_so_the_ui_can_say_so() {
        assert!(Filter::default().is_catch_all());
        assert!(!Filter { conforms_to: vec![REPORT.into()], ..Default::default() }.is_catch_all());
        assert!(Filter { q: Some("   ".into()), ..Default::default() }.is_catch_all());
    }

    #[test]
    fn backoff_grows_and_then_stops_growing() {
        let p = RetryPolicy { max_attempts: 8, suspend_after: 12, base_backoff_secs: 30, max_backoff_secs: 3600 };
        assert_eq!(p.backoff_secs(1), 30);
        assert_eq!(p.backoff_secs(2), 60);
        assert_eq!(p.backoff_secs(3), 120);
        assert_eq!(p.backoff_secs(20), 3600, "must be capped, not unbounded");
        // Monotone: no attempt is ever retried sooner than the one before it.
        for n in 1..12 {
            assert!(p.backoff_secs(n) <= p.backoff_secs(n + 1));
        }
    }

    #[tokio::test]
    async fn a_match_is_queued_once_per_artifact_and_role() {
        let ops = Ops::open(":memory:").await.unwrap();
        let sub = create(
            &ops,
            &NewSubscription {
                instance_iri: OWNER.into(),
                label: Some("reports".into()),
                filter: Filter::default(),
                webhook_url: None,
                webhook_secret: None,
                created_by: None,
            },
        )
        .await
        .unwrap();
        let payload = serde_json::json!({"type": "artifact.advertised"});
        let first = enqueue(&ops, &sub.id, "https://reg.test/artifact/1", Some("run/1"), ROLE_PRODUCED, &payload).await.unwrap();
        assert!(first.is_some());
        let again = enqueue(&ops, &sub.id, "https://reg.test/artifact/1", Some("run/1"), ROLE_PRODUCED, &payload).await.unwrap();
        assert!(again.is_none(), "a retried advertisement must not notify twice");
        // The same artifact in the other role is a different event.
        let consumed = enqueue(&ops, &sub.id, "https://reg.test/artifact/1", Some("run/2"), ROLE_CONSUMED, &payload).await.unwrap();
        assert!(consumed.is_some());

        let items = deliveries_since(&ops, &sub.id, 0, 10).await.unwrap();
        assert_eq!(items.len(), 2);
        assert!(items[0].seq < items[1].seq, "the cursor must be monotonic");
    }

    #[tokio::test]
    async fn a_failing_webhook_backs_off_then_dies_then_suspends_the_subscription() {
        let ops = Ops::open(":memory:").await.unwrap();
        let policy = RetryPolicy { max_attempts: 3, suspend_after: 3, base_backoff_secs: 30, max_backoff_secs: 3600 };
        let sub = create(
            &ops,
            &NewSubscription {
                instance_iri: OWNER.into(),
                label: None,
                filter: Filter::default(),
                webhook_url: Some("https://receiver.example/hook".into()),
                webhook_secret: Some("s3cret".into()),
                created_by: None,
            },
        )
        .await
        .unwrap();
        enqueue(&ops, &sub.id, "https://reg.test/artifact/1", None, ROLE_PRODUCED, &serde_json::json!({}))
            .await
            .unwrap()
            .unwrap();
        let due = due_deliveries(&ops, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        let did = due[0].id.clone();

        let a = mark_failed(&ops, &did, &sub.id, "connection refused", None, &policy).await.unwrap();
        assert_eq!(a.status, "failed");
        assert_eq!(a.retry_in_secs, Some(30));
        // Backed off: nothing is due again immediately.
        assert!(due_deliveries(&ops, 10).await.unwrap().is_empty(), "a failed delivery must not be retried at once");

        let b = mark_failed(&ops, &did, &sub.id, "connection refused", None, &policy).await.unwrap();
        assert_eq!(b.retry_in_secs, Some(60));
        let c = mark_failed(&ops, &did, &sub.id, "connection refused", None, &policy).await.unwrap();
        assert_eq!(c.status, "dead", "attempts are finite");
        assert_eq!(c.retry_in_secs, None);
        assert!(c.suspended, "a consistently dead endpoint stops being attempted at all");

        let after = get(&ops, &sub.id).await.unwrap().unwrap();
        assert_eq!(after.delivery_state, "suspended");
        assert!(!after.is_pushable());
        assert_eq!(after.dead_count, 1);
        // But the pull path still works while suspended: the subscriber can come and get it.
        assert_eq!(deliveries_since(&ops, &sub.id, 0, 10).await.unwrap().len(), 1);

        // Fixing the endpoint re-arms the queue rather than losing what was missed.
        update(&ops, &sub.id, &Patch { resume: true, ..Default::default() }).await.unwrap();
        let resumed = get(&ops, &sub.id).await.unwrap().unwrap();
        assert_eq!(resumed.delivery_state, "active");
        assert_eq!(resumed.consecutive_failures, 0);
        assert_eq!(due_deliveries(&ops, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_acknowledged_cursor_only_moves_forward() {
        let ops = Ops::open(":memory:").await.unwrap();
        let sub = create(
            &ops,
            &NewSubscription {
                instance_iri: OWNER.into(),
                label: None,
                filter: Filter::default(),
                webhook_url: None,
                webhook_secret: None,
                created_by: None,
            },
        )
        .await
        .unwrap();
        for i in 0..3 {
            enqueue(&ops, &sub.id, &format!("https://reg.test/artifact/{i}"), None, ROLE_PRODUCED, &serde_json::json!({}))
                .await
                .unwrap();
        }
        let all = deliveries_since(&ops, &sub.id, 0, 10).await.unwrap();
        assert_eq!(all.len(), 3);
        let mid = all[1].seq;
        assert_eq!(ack(&ops, &sub.id, mid).await.unwrap(), mid);
        assert_eq!(ack(&ops, &sub.id, 0).await.unwrap(), mid, "a stale ack must not rewind the cursor");
        assert_eq!(deliveries_since(&ops, &sub.id, mid, 10).await.unwrap().len(), 1);
        assert_eq!(get(&ops, &sub.id).await.unwrap().unwrap().unacked_count, 1);
    }
}
