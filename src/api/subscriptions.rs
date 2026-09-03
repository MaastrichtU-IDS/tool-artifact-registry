//! Artifact subscriptions — "tell me when an artifact like this appears", and then telling it.
//!
//! See `docs/specs/2026-08-31-artifact-subscriptions.md`. Three properties drive the shape of
//! this module:
//!
//! 1. **Advertisement never blocks on the network** (spec §9.3). [`notify_advertised`] is
//!    called on the advertise path and does nothing but read the local graph and insert rows
//!    into SQLite. Every socket is opened later, by [`deliver_due`], on a worker task.
//! 2. **A subscription must work for a tool behind a firewall.** The registry already models
//!    CLI and desktop tools with no endpoint at all, so a webhook cannot be the only channel.
//!    The webhook worker and the pull endpoint drain *the same queue*; a subscription with no
//!    `webhook_url` is a perfectly ordinary pull-only subscription, and that is the default.
//! 3. **A webhook makes this registry issue outbound HTTP to an address someone else chose.**
//!    That is a capability, and it is guarded: HTTPS only by default, no credentials in the
//!    URL, no redirects followed, no private/loopback/link-local/metadata targets, a bounded
//!    number of subscriptions per deployment, a bounded body read, and a signature so the
//!    receiver can tell our POST from anybody else's.

use crate::auth::Principal;
use crate::domain::{artifact as artdom, Ctx};
use crate::error::{AppError, AppResult};
use crate::ids::{self, Kind};
use crate::ns;
use crate::ops::subscriptions as subs;
use crate::ops::subscriptions::{Candidate, Filter, RetryPolicy};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use once_cell::sync::Lazy;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

// --------------------------------------------------------------------- settings

/// Delivery tunables. Read from the environment with the registry's `TAR_*` naming rather
/// than from `Config`, which is owned elsewhere while this lands; moving them is mechanical.
#[derive(Debug, Clone)]
pub struct WebhookSettings {
    pub enabled: bool,
    /// Allow `http://` targets. Off by default: an unencrypted webhook leaks the artifact
    /// metadata and the signature to anyone on the path.
    pub allow_http: bool,
    /// Allow loopback and RFC1918 targets. Off by default — this is the SSRF guard. Turn it
    /// on only for a registry and its subscribers inside one trusted network.
    pub allow_private_targets: bool,
    pub timeout: Duration,
    /// Deliveries attempted per worker tick, across all subscriptions.
    pub batch: i64,
    /// Bytes of a receiver's response body we are willing to read. We need the status code;
    /// the body is only ever quoted back in an error message.
    pub max_response_bytes: usize,
    pub tick: Duration,
}

impl Default for WebhookSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_http: false,
            allow_private_targets: false,
            timeout: Duration::from_secs(5),
            batch: 20,
            max_response_bytes: 2048,
            tick: Duration::from_secs(5),
        }
    }
}

impl WebhookSettings {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            enabled: env_bool("TAR_SUBSCRIPTION_WEBHOOKS", d.enabled),
            allow_http: env_bool("TAR_SUBSCRIPTION_ALLOW_HTTP", d.allow_http),
            allow_private_targets: env_bool("TAR_SUBSCRIPTION_ALLOW_PRIVATE_TARGETS", d.allow_private_targets),
            timeout: env_duration("TAR_SUBSCRIPTION_TIMEOUT", d.timeout).min(Duration::from_secs(30)),
            batch: env_i64("TAR_SUBSCRIPTION_BATCH", d.batch).clamp(1, 500),
            max_response_bytes: d.max_response_bytes,
            tick: env_duration("TAR_SUBSCRIPTION_TICK", d.tick).max(Duration::from_secs(1)),
        }
    }
}

fn env_raw(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
fn env_bool(key: &str, default: bool) -> bool {
    match env_raw(key).map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) => matches!(v.as_str(), "1" | "true" | "yes" | "on"),
        None => default,
    }
}
fn env_i64(key: &str, default: i64) -> i64 {
    env_raw(key).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}
fn env_duration(key: &str, default: Duration) -> Duration {
    env_raw(key).and_then(|v| crate::config::parse_duration(&v).ok()).unwrap_or(default)
}

/// A client of our own, because the shared one follows redirects. A redirect is exactly how a
/// checked target turns into an unchecked one: `https://evil.example/h` → `302` →
/// `http://169.254.169.254/latest/meta-data/`. Nothing is followed here.
static WEBHOOK_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| webhook_client_builder().build().unwrap_or_default());

fn webhook_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).user_agent(concat!(
        "tool-artifact-registry/",
        env!("CARGO_PKG_VERSION"),
        " (webhook)"
    ))
}

/// A client that will connect to `pinned` and nowhere else, closing the gap between checking a
/// name and using it.
///
/// `resolve_to_addrs` replaces the resolver for this one host, so the addresses
/// [`resolve_public_targets`] approved are the addresses used — the name is never looked up
/// again, and a record that changes in between cannot redirect the connection. TLS still
/// verifies the certificate against the hostname.
///
/// Built per delivery rather than shared, because the pinning is per host and per resolution:
/// a client cached across deliveries would be caching a DNS answer, which is the thing being
/// avoided. Deliveries are a background trickle, so a connection pool that lives for one POST
/// is the right trade for not having to trust a second lookup.
///
/// With no addresses to pin — which is what `allow_private_targets` produces, since an
/// operator who has turned the guard off has said the target set is theirs to choose — the
/// shared client is used unchanged.
fn pinned_client(url: &url::Url, pinned: &[std::net::SocketAddr]) -> Result<reqwest::Client, String> {
    if pinned.is_empty() {
        return Ok(WEBHOOK_CLIENT.clone());
    }
    let host = url.host_str().ok_or_else(|| "webhook URL has no host".to_string())?;
    // An IP literal resolves to itself; there is no name to rebind and nothing to override.
    if url.host().is_some_and(|h| matches!(h, url::Host::Ipv4(_) | url::Host::Ipv6(_))) {
        return Ok(WEBHOOK_CLIENT.clone());
    }
    webhook_client_builder()
        .resolve_to_addrs(host.trim_matches(['[', ']']), pinned)
        .build()
        .map_err(|e| format!("could not build a pinned HTTP client for {host}: {e}"))
}

// ------------------------------------------------------------------ authorisation

/// Identical to the rule for tokens (`api::tokens::may_manage`): a subscription belongs to one
/// Instance, and is managed by that Instance's own credential, a curator, or an admin. The
/// Instance comes from the credential, never from the body — §8.3 applied to reads.
fn may_manage(principal: &Principal, instance_iri: &str) -> AppResult<()> {
    if principal.is_admin() || principal.is_curator() {
        return Ok(());
    }
    if principal.instance_iri.as_deref() == Some(instance_iri) {
        return Ok(());
    }
    // Deliberately the same message and status whether the subscription exists or not: a
    // 404-vs-403 split here would let one deployment enumerate another's subscription ids.
    Err(AppError::forbidden(
        "only the owning deployment, a curator or an admin may manage this deployment's subscriptions",
    ))
}

async fn load_owned(state: &Arc<AppState>, principal: &Principal, sid: &str) -> AppResult<subs::Subscription> {
    let Some(sub) = subs::get(&state.ops, sid).await.map_err(AppError::from)? else {
        return Err(AppError::not_found("no such subscription"));
    };
    may_manage(principal, &sub.instance_iri)?;
    Ok(sub)
}

// --------------------------------------------------------------------- requests

#[derive(Debug, Deserialize, Default)]
pub struct CreateSubscription {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub filter: Filter,
    /// Omit for a pull-only subscription — the right answer for anything without an inbound
    /// endpoint, which is most CLI and batch deployments.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Bring your own signing secret, or omit and one is generated and shown once.
    #[serde(default)]
    pub webhook_secret: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PatchSubscription {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub filter: Option<Filter>,
    /// An empty string clears the webhook, turning the subscription pull-only.
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub rotate_secret: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Un-suspend a webhook that was switched off after repeated failure, and re-arm the
    /// deliveries that died while it was down.
    #[serde(default)]
    pub resume: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct DeliveryQuery {
    /// Sequence number to read after. Omitted, the subscription's own acknowledged cursor is
    /// used, so a subscriber that keeps no state of its own still makes progress.
    pub cursor: Option<String>,
    pub limit: Option<String>,
    /// `true` acknowledges everything returned in the same round trip. Convenient for a
    /// subscriber that processes synchronously; leave it off for at-least-once with an
    /// explicit ack after the work is done.
    pub ack: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AckIn {
    pub cursor: i64,
}

#[derive(Debug, Serialize)]
pub struct SubscriptionOut {
    #[serde(flatten)]
    pub subscription: subs::Subscription,
    pub delivery_mode: &'static str,
    /// How this subscriber fetches matches when it cannot receive an inbound connection.
    pub pull_url: String,
}

fn out(state: &AppState, s: subs::Subscription) -> SubscriptionOut {
    SubscriptionOut {
        pull_url: format!("{}/api/v1/subscriptions/{}/deliveries", state.base(), s.id),
        delivery_mode: s.delivery_mode(),
        subscription: s,
    }
}

// --------------------------------------------------------------------- handlers

pub async fn list(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    may_manage(&principal, &iri)?;
    let items = subs::list_for_instance(&state.ops, &iri).await.map_err(AppError::from)?;
    let total = items.len();
    let items: Vec<SubscriptionOut> = items.into_iter().map(|s| out(&state, s)).collect();
    Ok(Json(json!({ "items": items, "total": total, "max_per_instance": subs::MAX_PER_INSTANCE })))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
    Json(input): Json<CreateSubscription>,
) -> AppResult<impl IntoResponse> {
    let iri = ids::iri_for(state.base(), Kind::Instance, &id);
    may_manage(&principal, &iri)?;
    let exists = super::blocking({
        let (state, iri) = (state.clone(), iri.clone());
        move || state.store.exists(&iri).map_err(AppError::from)
    })
    .await?;
    if !exists {
        return Err(AppError::not_found(format!("no instance at {iri}")));
    }
    if subs::count_for_instance(&state.ops, &iri).await.map_err(AppError::from)? >= subs::MAX_PER_INSTANCE {
        return Err(AppError::conflict(format!(
            "this deployment already has the maximum of {} subscriptions; delete one first",
            subs::MAX_PER_INSTANCE
        )));
    }
    let filter = normalise_filter(state.base(), input.filter)?;
    let settings = WebhookSettings::from_env();
    let webhook_url = match input.webhook_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => Some(validate_webhook_url(u, &settings)?),
        None => None,
    };
    // A secret only means anything if there is somewhere to send it.
    let secret =
        webhook_url.as_ref().map(|_| match input.webhook_secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => new_secret(),
        });

    let sub = subs::create(
        &state.ops,
        &subs::NewSubscription {
            instance_iri: iri.clone(),
            label: input.label.clone(),
            filter,
            webhook_url,
            webhook_secret: secret.clone(),
            created_by: Some(principal.subject.clone()),
        },
    )
    .await
    .map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(
            Some(&principal.subject),
            principal.actor_kind(),
            "subscription.create",
            Some(&iri),
            input.label.as_deref().or(Some(sub.id.as_str())),
            None,
        )
        .await;
    // Shown exactly once, like a token: the registry keeps it because HMAC needs the key
    // itself, but it is never handed back a second time.
    Ok((StatusCode::CREATED, Json(json!({ "subscription": out(&state, sub), "secret": secret, "shown_once": true }))))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(sid): Path<String>,
) -> AppResult<impl IntoResponse> {
    let sub = load_owned(&state, &principal, &sid).await?;
    let recent = subs::recent_deliveries(&state.ops, &sub.id, 20).await.map_err(AppError::from)?;
    Ok(Json(json!({ "subscription": out(&state, sub), "recent_deliveries": recent })))
}

pub async fn patch(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(sid): Path<String>,
    Json(input): Json<PatchSubscription>,
) -> AppResult<impl IntoResponse> {
    let sub = load_owned(&state, &principal, &sid).await?;
    let settings = WebhookSettings::from_env();
    let mut p = subs::Patch { resume: input.resume.unwrap_or(false), ..Default::default() };
    if let Some(l) = input.label {
        p.label = Some(Some(l));
    }
    if let Some(f) = input.filter {
        p.filter = Some(normalise_filter(state.base(), f)?);
    }
    let mut has_webhook = sub.webhook_url.is_some();
    if let Some(u) = input.webhook_url.as_deref().map(str::trim) {
        if u.is_empty() {
            p.webhook_url = Some(None);
            p.webhook_secret = Some(None);
            has_webhook = false;
        } else {
            p.webhook_url = Some(Some(validate_webhook_url(u, &settings)?));
            has_webhook = true;
        }
    }
    // Changing where we POST invalidates nothing about the old secret except trust, so a new
    // endpoint gets a new secret unless one already exists and the caller kept the URL.
    let mut rotated = None;
    if input.rotate_secret.unwrap_or(false) || (has_webhook && !sub.webhook_signed && p.webhook_secret.is_none()) {
        if !has_webhook {
            return Err(AppError::bad_request("there is no webhook to sign; set webhook_url first"));
        }
        let s = new_secret();
        p.webhook_secret = Some(Some(s.clone()));
        rotated = Some(s);
    }
    if let Some(e) = input.enabled {
        p.enabled = Some(e);
    }
    subs::update(&state.ops, &sub.id, &p).await.map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(
            Some(&principal.subject),
            principal.actor_kind(),
            "subscription.update",
            Some(&sub.instance_iri),
            Some(&sub.id),
            None,
        )
        .await;
    let updated = subs::get(&state.ops, &sub.id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("no such subscription"))?;
    Ok(Json(json!({ "subscription": out(&state, updated), "secret": rotated, "shown_once": rotated.is_some() })))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(sid): Path<String>,
) -> AppResult<impl IntoResponse> {
    let sub = load_owned(&state, &principal, &sid).await?;
    subs::delete(&state.ops, &sub.id).await.map_err(AppError::from)?;
    let _ = state
        .ops
        .audit(
            Some(&principal.subject),
            principal.actor_kind(),
            "subscription.delete",
            Some(&sub.instance_iri),
            Some(&sub.id),
            None,
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// The pull path. This is what makes a subscription work for a tool that cannot receive an
/// inbound connection — a CLI, a laptop, a batch job inside a hospital network. It reads the
/// same queue the webhook worker drains, so the two channels never disagree about what
/// matched.
pub async fn deliveries(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(sid): Path<String>,
    Query(q): Query<DeliveryQuery>,
) -> AppResult<impl IntoResponse> {
    let sub = load_owned(&state, &principal, &sid).await?;
    let limit = q.limit.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(25).clamp(1, 200);
    let cursor = match q.cursor.as_deref().filter(|c| !c.is_empty()) {
        Some(c) => c.parse::<i64>().map_err(|_| AppError::bad_request("cursor must be an integer sequence number"))?,
        None => sub.cursor_seq,
    };
    let items = subs::deliveries_since(&state.ops, &sub.id, cursor, limit).await.map_err(AppError::from)?;
    let next_cursor = items.last().map(|d| d.seq).unwrap_or(cursor);
    let latest = subs::max_seq(&state.ops, &sub.id).await.map_err(AppError::from)?;
    let _ = subs::touch_polled(&state.ops, &sub.id).await;
    let acked = if matches!(q.ack.as_deref(), Some("true" | "1")) {
        Some(subs::ack(&state.ops, &sub.id, next_cursor).await.map_err(AppError::from)?)
    } else {
        None
    };
    Ok(Json(json!({
        "items": items,
        "cursor": cursor,
        "next_cursor": next_cursor,
        // Matches after this page. A pulling subscriber uses it to decide whether to keep
        // going without a second round trip.
        "remaining": (latest - next_cursor).max(0),
        "acknowledged": acked,
    })))
}

/// Acknowledge up to a sequence number. Separate from the read so a subscriber can process
/// first and acknowledge after — at-least-once, which is the only honest guarantee here.
pub async fn ack(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(sid): Path<String>,
    Json(input): Json<AckIn>,
) -> AppResult<impl IntoResponse> {
    let sub = load_owned(&state, &principal, &sid).await?;
    let cursor = subs::ack(&state.ops, &sub.id, input.cursor).await.map_err(AppError::from)?;
    let latest = subs::max_seq(&state.ops, &sub.id).await.map_err(AppError::from)?;
    Ok(Json(json!({ "cursor": cursor, "remaining": (latest - cursor).max(0) })))
}

// ------------------------------------------------------------------- validation

/// Registry-minted kinds may be given as a bare id or a full IRI, because that is what the
/// rest of the API accepts in a path. External vocabularies (EDAM types, SPDX licences) must
/// be full IRIs — there is nothing to expand a bare token against, and guessing would produce
/// a filter that silently never matches.
fn normalise_filter(base: &str, mut f: Filter) -> AppResult<Filter> {
    f.software = f.software.iter().map(|s| ids::iri_for(base, Kind::Software, s)).collect();
    f.instance = f.instance.iter().map(|s| ids::iri_for(base, Kind::Instance, s)).collect();
    for t in &f.conforms_to {
        require_iri("conforms_to", t)?;
    }
    for l in &f.license {
        require_iri("license", l)?;
    }
    for a in &f.availability {
        if !subs::AVAILABILITIES.contains(&a.as_str()) {
            return Err(AppError::bad_request(format!(
                "unknown availability {a:?}; known: {}",
                subs::AVAILABILITIES.join(", ")
            )));
        }
    }
    for r in &f.roles {
        if r != subs::ROLE_PRODUCED && r != subs::ROLE_CONSUMED {
            return Err(AppError::bad_request(format!("unknown role {r:?}; known: produced, consumed")));
        }
    }
    f.keywords.retain(|k| !k.trim().is_empty());
    f.conforms_to.retain(|k| !k.trim().is_empty());
    f.license.retain(|k| !k.trim().is_empty());
    Ok(f)
}

fn require_iri(field: &str, value: &str) -> AppResult<()> {
    if value.starts_with("http://") || value.starts_with("https://") || value.starts_with("urn:") {
        return Ok(());
    }
    Err(AppError::bad_request(format!(
        "{field} must be a full IRI (a vocabulary term, a registry-local type, or an SPDX licence), got {value:?}"
    )))
}

/// The static half of the SSRF guard, applied when a subscription is created or edited.
///
/// It deliberately does **not** resolve DNS: a receiver that has not been deployed yet must
/// still be registrable, and a name that resolves now can resolve elsewhere later anyway. The
/// resolution check happens again for every attempt, in [`resolve_public_targets`], whose
/// answer the delivery is then pinned to.
pub fn validate_webhook_url(raw: &str, settings: &WebhookSettings) -> AppResult<String> {
    let url = url::Url::parse(raw).map_err(|e| AppError::bad_request(format!("webhook_url is not a URL: {e}")))?;
    match url.scheme() {
        "https" => {}
        "http" if settings.allow_http => {}
        "http" => {
            return Err(AppError::bad_request(
                "webhook_url must be https: an unencrypted webhook leaks the artifact metadata and the \
                 signature to anyone on the path (set TAR_SUBSCRIPTION_ALLOW_HTTP=true for a trusted network)",
            ))
        }
        s => return Err(AppError::bad_request(format!("webhook_url scheme {s:?} is not supported; use https"))),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::bad_request(
            "webhook_url must not carry credentials; sign-in is the receiver's job, and the URL is shown in the UI",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(AppError::bad_request("webhook_url has no host"));
    };
    if !settings.allow_private_targets {
        if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            if !is_public_ip(&ip) {
                return Err(AppError::bad_request(format!(
                    "webhook_url points at {ip}, which is not a public address; the registry will not be used to \
                     reach into a private network"
                )));
            }
        }
        let lower = host.to_ascii_lowercase();
        if lower == "localhost"
            || lower.ends_with(".localhost")
            || lower.ends_with(".local")
            || lower.ends_with(".internal")
        {
            return Err(AppError::bad_request(format!(
                "webhook_url host {host:?} is a private name; the registry only delivers to public addresses"
            )));
        }
    }
    Ok(url.to_string())
}

/// Anything that is not routable on the public internet, refused. The list is the usual
/// SSRF target set: loopback, RFC1918, link-local (which is where cloud metadata lives at
/// `169.254.169.254`), carrier-grade NAT, unspecified, multicast, and the IPv6 equivalents.
fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                // 100.64.0.0/10 carrier-grade NAT
                || (o[0] == 100 && (64..128).contains(&o[1]))
                // 192.0.0.0/24 IETF protocol assignments
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                // 198.18.0.0/15 benchmarking
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                // 240.0.0.0/4 reserved
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7 unique local
                || (seg[0] & 0xfe00) == 0xfc00
                // fe80::/10 link local
                || (seg[0] & 0xffc0) == 0xfe80
                // IPv4-mapped: judge the embedded address, or ::ffff:127.0.0.1 walks straight in
                || v6.to_ipv4_mapped().map(|m| !is_public_ip(&IpAddr::V4(m))).unwrap_or(false))
        }
    }
}

/// The dynamic half of the guard, applied before every attempt. A name that was fine at
/// registration can be repointed at `169.254.169.254` afterwards; this is what catches that.
///
/// **Returns the addresses it approved, and the delivery connects to those.** It used to
/// return only "yes", and the name was then resolved a second time by the HTTP client — so a
/// short-TTL record could answer the check with a public address and the connection with a
/// private one, and the window between the two lookups was the whole vulnerability. There is
/// no second lookup now: `post_webhook` pins these addresses onto the client it uses, so the
/// bytes go to an address this function returned or the delivery does not happen.
///
/// The hostname is still what TLS verifies against — pinning replaces DNS, not the identity
/// check, so a certificate for the receiver's name is still required.
async fn resolve_public_targets(
    url: &url::Url,
    settings: &WebhookSettings,
) -> Result<Vec<std::net::SocketAddr>, String> {
    if settings.allow_private_targets {
        return Ok(Vec::new());
    }
    let host = url.host_str().ok_or_else(|| "webhook URL has no host".to_string())?;
    // A URL that already names an address has nothing to rebind: it is checked, and pinning it
    // to itself would only add a resolver override that can never fire.
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.trim_matches(['[', ']']), port))
        .await
        .map_err(|e| format!("could not resolve {host}: {e}"))?
        .collect();
    for a in &addrs {
        if !is_public_ip(&a.ip()) {
            return Err(format!("{host} resolves to {}, which is not a public address; refusing to connect", a.ip()));
        }
    }
    if addrs.is_empty() {
        return Err(format!("{host} resolved to no addresses"));
    }
    Ok(addrs)
}

fn new_secret() -> String {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    format!("whsec_{}", hex::encode(b))
}

// --------------------------------------------------------------------- matching

/// Called on the advertisement path. Reads the local graph, evaluates every enabled
/// subscription, and writes a queue row per match. **No network, no HTTP client, no peer.**
///
/// Errors are logged and swallowed: a subscription problem must never fail an advertisement
/// that has already been committed to the graph.
pub async fn notify_advertised(
    state: &Arc<AppState>,
    instance_iri: Option<&str>,
    run_iri: Option<&str>,
    artifact_iris: &[String],
    role: &str,
) {
    if artifact_iris.is_empty() {
        return;
    }
    let all = match subs::active_subscriptions(&state.ops).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "could not read subscriptions; advertisement is unaffected");
            return;
        }
    };
    // The common case on a registry where nobody has subscribed: one indexed read, then out.
    if all.is_empty() {
        return;
    }
    let ctx = match Ctx::new(state).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = ?e.detail, "could not build read context for subscription matching");
            return;
        }
    };
    let (software_iri, loaded_all) = match super::blocking({
        let (ctx, instance_iri, artifact_iris) = (ctx, instance_iri.map(str::to_string), artifact_iris.to_vec());
        move || {
            let software_iri = instance_iri.as_deref().and_then(|i| software_of_instance(&ctx.state, i));
            // The candidate is read back from the graph rather than from the request body, so
            // the matcher sees exactly what a reader of the artifact would see — including, for
            // a reference to an artifact registered earlier, fields this advertisement never
            // mentioned. A foreign IRI that has not been resolved yet simply has few fields,
            // and a filter on a field it lacks correctly does not match.
            let loaded: Vec<Option<crate::model::Artifact>> =
                artifact_iris.iter().map(|iri| artdom::load_artifact(&ctx, iri).ok()).collect();
            Ok((software_iri, loaded))
        }
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e.detail, "could not read artifacts for subscription matching");
            return;
        }
    };

    for (artifact_iri, loaded) in artifact_iris.iter().zip(loaded_all) {
        let candidate = Candidate {
            artifact_iri: artifact_iri.clone(),
            title: loaded.as_ref().and_then(|a| a.title.clone()),
            description: loaded.as_ref().and_then(|a| a.description.clone()),
            conforms_to: loaded.as_ref().and_then(|a| a.conforms_to.as_ref().map(|t| t.iri.clone())),
            license: loaded.as_ref().and_then(|a| a.license.clone()),
            keywords: loaded.as_ref().map(|a| a.keywords.clone()).unwrap_or_default(),
            availability: loaded.as_ref().map(|a| a.availability.clone()).unwrap_or_else(|| "metadata-only".into()),
            instance_iri: instance_iri.map(str::to_string),
            software_iri: software_iri.clone(),
            role: role.to_string(),
        };

        for sub in &all {
            if !subs::matches(&sub.filter, &sub.instance_iri, &candidate) {
                continue;
            }
            let payload = json!({
                "type": "artifact.advertised",
                "subscription": sub.id,
                "registry": state.base(),
                "role": role,
                "run": run_iri,
                "instance": instance_iri,
                "software": software_iri,
                "artifact_iri": artifact_iri,
                // The whole artifact record, which is exactly what an anonymous
                // `GET /api/v1/artifacts/{id}` returns. A webhook never carries anything the
                // receiver could not already have read.
                "artifact": loaded,
            });
            match subs::enqueue(&state.ops, &sub.id, artifact_iri, run_iri, role, &payload).await {
                Ok(Some(seq)) => {
                    tracing::debug!(subscription = %sub.id, seq, artifact = %artifact_iri, "subscription matched")
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(subscription = %sub.id, error = %e, "could not queue a subscription delivery"),
            }
        }
    }
}

/// `tar:instanceOf`, denormalised alongside the authoritative `prov:qualifiedAssociation`
/// (README deviation 3) — which is what makes "any deployment of this software, anywhere" a
/// single lookup rather than a two-hop join per advertisement.
fn software_of_instance(state: &Arc<AppState>, instance_iri: &str) -> Option<String> {
    let q = format!(
        "{p}\nSELECT ?sw WHERE {{ GRAPH ?g {{ <{instance_iri}> tar:instanceOf ?sw }} }} LIMIT 1",
        p = ns::PREFIXES
    );
    state.store.select(&q).ok()?.rows.first()?.iri("sw")
}

// --------------------------------------------------------------------- delivery

/// Attempt every due webhook delivery. Returns how many were attempted.
///
/// Separated from the loop so a test can drive it deterministically instead of sleeping.
pub async fn deliver_due(state: &Arc<AppState>) -> usize {
    let settings = WebhookSettings::from_env();
    if !settings.enabled {
        return 0;
    }
    let policy = RetryPolicy::from_env();
    let due = match subs::due_deliveries(&state.ops, settings.batch).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "could not read the subscription delivery queue");
            return 0;
        }
    };
    let mut attempted = 0;
    for d in due {
        attempted += 1;
        match post_webhook(&d, &settings).await {
            Ok(code) => {
                if let Err(e) = subs::mark_delivered(&state.ops, &d.id, &d.subscription_id, code, d.attempts + 1).await
                {
                    tracing::warn!(error = %e, "could not record a successful delivery");
                }
            }
            Err(err) => {
                let outcome =
                    subs::mark_failed(&state.ops, &d.id, &d.subscription_id, &err.message, err.status, &policy).await;
                match outcome {
                    Ok(o) => {
                        if o.suspended {
                            tracing::warn!(
                                subscription = %d.subscription_id,
                                url = %d.webhook_url,
                                "webhook suspended after repeated failure; the owner must resume it. \
                                 Matches keep queueing and stay readable through the pull endpoint."
                            );
                        } else {
                            tracing::info!(
                                subscription = %d.subscription_id,
                                attempts = o.attempts,
                                retry_in = ?o.retry_in_secs,
                                error = %err.message,
                                "webhook delivery failed; backing off"
                            );
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "could not record a failed delivery"),
                }
            }
        }
    }
    attempted
}

/// The background worker. Spawned from `serve()` next to the peer resolver, and never on a
/// request path.
pub async fn delivery_loop(state: Arc<AppState>) {
    let tick = WebhookSettings::from_env().tick;
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        deliver_due(&state).await;
    }
}

struct DeliveryError {
    message: String,
    status: Option<u16>,
}

async fn post_webhook(d: &subs::DueDelivery, settings: &WebhookSettings) -> Result<u16, DeliveryError> {
    let url = url::Url::parse(&d.webhook_url)
        .map_err(|e| DeliveryError { message: format!("webhook URL is no longer valid: {e}"), status: None })?;
    let pinned =
        resolve_public_targets(&url, settings).await.map_err(|m| DeliveryError { message: m, status: None })?;
    let client = pinned_client(&url, &pinned).map_err(|m| DeliveryError { message: m, status: None })?;

    let timestamp = chrono::Utc::now().timestamp().to_string();
    let mut req = client
        .post(url)
        .timeout(settings.timeout)
        .header("content-type", "application/json")
        .header("x-tar-delivery", &d.id)
        .header("x-tar-subscription", &d.subscription_id)
        .header("x-tar-timestamp", &timestamp)
        .header("x-tar-attempt", (d.attempts + 1).to_string());
    if let Some(secret) = &d.webhook_secret {
        // Signed over `timestamp.body`, so a captured POST cannot be replayed later against a
        // receiver that checks the age of the timestamp.
        let sig = hmac_sha256(secret.as_bytes(), format!("{timestamp}.{}", d.payload).as_bytes());
        req = req.header("x-tar-signature", format!("sha256={}", hex::encode(sig)));
    }

    let resp = req
        .body(d.payload.clone())
        .send()
        .await
        .map_err(|e| DeliveryError { message: describe_send_error(&e), status: None })?;
    let status = resp.status();
    if status.is_success() {
        return Ok(status.as_u16());
    }
    // A bounded peek at the body, purely so the owner sees *why* their receiver said no.
    let body = resp.text().await.unwrap_or_default();
    let excerpt: String = body.chars().take(settings.max_response_bytes.min(400)).collect();
    Err(DeliveryError {
        message: if excerpt.trim().is_empty() {
            format!("receiver answered {status}")
        } else {
            format!("receiver answered {status}: {}", excerpt.replace('\n', " "))
        },
        status: Some(status.as_u16()),
    })
}

/// `reqwest`'s Display is a chain of "error sending request"; the owner needs to know whether
/// their host does not exist, refused, or was simply too slow.
fn describe_send_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "receiver did not answer within the delivery timeout".into();
    }
    if e.is_connect() {
        return format!("could not connect to the receiver: {e}");
    }
    if e.is_redirect() {
        return "receiver redirected; the registry does not follow webhook redirects".into();
    }
    format!("delivery failed: {e}")
}

/// HMAC-SHA256, RFC 2104. Hand-rolled on top of `sha2` rather than adding a dependency for
/// thirty lines; the RFC 4231 vectors are asserted below.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(msg).finalize();
    let outer = Sha256::new().chain_update(opad).chain_update(inner).finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deny_private() -> WebhookSettings {
        WebhookSettings { allow_private_targets: false, allow_http: false, ..Default::default() }
    }

    #[test]
    fn hmac_matches_the_rfc_4231_vectors() {
        // Case 2.
        assert_eq!(
            hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Case 1.
        assert_eq!(
            hex::encode(hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // Case 3 exercises the >block-size key path.
        assert_eq!(
            hex::encode(hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn the_registry_will_not_be_pointed_at_a_private_network() {
        let s = deny_private();
        for bad in [
            "https://127.0.0.1/hook",
            "https://10.0.0.5/hook",
            "https://192.168.1.10/hook",
            "https://172.16.4.4/hook",
            // The one that matters most: the cloud metadata endpoint.
            "https://169.254.169.254/latest/meta-data/",
            "https://[::1]/hook",
            "https://[fd00::1]/hook",
            // An IPv4-mapped IPv6 literal must not be a way around the IPv4 rules.
            "https://[::ffff:127.0.0.1]/hook",
            "https://localhost/hook",
            "https://receiver.local/hook",
            "https://receiver.internal/hook",
        ] {
            assert!(validate_webhook_url(bad, &s).is_err(), "{bad} must be refused");
        }
        assert!(validate_webhook_url("https://hooks.example.org/tar", &s).is_ok());
    }

    /// The rebinding fix, demonstrated rather than asserted.
    ///
    /// `.invalid` is guaranteed by RFC 2606 never to resolve, so a request to a host under it
    /// can only arrive if the pinned address replaced DNS entirely. The listener receiving the
    /// bytes is the proof: there is no lookup left between the check and the connection for a
    /// changed record to win.
    #[tokio::test]
    async fn a_delivery_connects_to_the_address_that_was_checked_and_not_to_dns() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 128];
            let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await.unwrap_or(0);
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let url = url::Url::parse(&format!("http://receiver.test.invalid:{}/hook", addr.port())).unwrap();

        // The counterfactual, so this test proves the pin rather than assuming it: the shared
        // client has no override and cannot resolve the name at all.
        let unpinned = pinned_client(&url, &[]).expect("the shared client");
        let refused = unpinned.post(url.clone()).body("{}").timeout(Duration::from_secs(5)).send().await;
        assert!(refused.is_err(), "without the pin the name must not resolve; the pin is what connects");

        let client = pinned_client(&url, &[addr]).expect("a pinned client");
        let _ = client.post(url).body("{}").timeout(Duration::from_secs(5)).send().await;

        let request = tokio::time::timeout(Duration::from_secs(5), accepted).await.expect("connected").unwrap();
        assert!(request.starts_with("POST /hook"), "the pinned address received the delivery: {request:?}");
        assert!(
            request.contains("receiver.test.invalid"),
            "and still addressed the receiver by name, which is what TLS would verify: {request:?}"
        );
    }

    /// The check itself still refuses a name that resolves somewhere private — pinning is what
    /// makes the answer binding, not a replacement for asking the question.
    #[tokio::test]
    async fn a_name_resolving_into_the_private_range_is_refused_before_anything_is_pinned() {
        let s = deny_private();
        let local = url::Url::parse("https://localhost/hook").unwrap();
        let err = resolve_public_targets(&local, &s).await.expect_err("localhost is not public");
        assert!(err.contains("not a public address"), "{err}");

        // With the guard off there is nothing to pin, and the shared client is used.
        let lax = WebhookSettings { allow_private_targets: true, ..Default::default() };
        assert_eq!(resolve_public_targets(&local, &lax).await.unwrap(), Vec::new());
    }

    #[test]
    fn plaintext_and_credentialed_webhooks_are_refused() {
        let s = deny_private();
        assert!(validate_webhook_url("http://hooks.example.org/tar", &s).is_err());
        assert!(validate_webhook_url("https://user:pw@hooks.example.org/tar", &s).is_err());
        assert!(validate_webhook_url("file:///etc/passwd", &s).is_err());
        assert!(validate_webhook_url("gopher://hooks.example.org/", &s).is_err());
        // An operator inside one trusted network can opt back in, explicitly.
        let lax = WebhookSettings { allow_http: true, allow_private_targets: true, ..Default::default() };
        assert!(validate_webhook_url("http://127.0.0.1:9000/hook", &lax).is_ok());
    }

    #[test]
    fn filters_reject_values_that_could_never_match() {
        let base = "https://reg.test";
        assert!(normalise_filter(base, Filter { availability: vec!["public".into()], ..Default::default() }).is_ok());
        // A typo here would otherwise be a subscription that silently never fires.
        assert!(normalise_filter(base, Filter { availability: vec!["publicc".into()], ..Default::default() }).is_err());
        assert!(normalise_filter(base, Filter { roles: vec!["produced".into()], ..Default::default() }).is_ok());
        assert!(normalise_filter(base, Filter { roles: vec!["generated".into()], ..Default::default() }).is_err());
        assert!(normalise_filter(base, Filter { conforms_to: vec!["data_2048".into()], ..Default::default() }).is_err());
    }

    #[test]
    fn registry_minted_kinds_accept_a_bare_id_the_way_every_path_does() {
        let base = "https://reg.test";
        let f = normalise_filter(
            base,
            Filter { software: vec!["01a".into()], instance: vec!["01b".into()], ..Default::default() },
        )
        .unwrap();
        assert_eq!(f.software, vec!["https://reg.test/software/01a"]);
        assert_eq!(f.instance, vec!["https://reg.test/instance/01b"]);
    }
}
