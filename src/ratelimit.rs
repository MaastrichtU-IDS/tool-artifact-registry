//! Rate limiting (design note `docs/specs/2026-09-23-rate-limiting.md`, answering spec Q8).
//!
//! A request is sorted into a cost class and charged to whoever is calling: the verified
//! principal if it authenticated, the client's address otherwise. The limits live here rather
//! than only at an ingress because a bare `docker run` has none, and an ingress can tell neither
//! a curator from a stranger nor a list read from a SPARQL query.

use crate::auth::Principal;
use crate::error::AppError;
use crate::state::AppState;
use anyhow::{bail, Context, Result};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::Method;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use governor::clock::Clock;
use governor::middleware::StateInformationMiddleware;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::Quota;
use governor::RateLimiter;
use std::collections::HashMap;
use std::hash::Hash;
use std::net::SocketAddr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------------------ classes

/// What a request costs, which decides which bucket it is charged to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Read,
    Write,
    Sparql,
    Federated,
    Mcp,
    Outbound,
}

impl Class {
    pub const ALL: [Class; 6] =
        [Class::Read, Class::Write, Class::Sparql, Class::Federated, Class::Mcp, Class::Outbound];

    pub fn name(self) -> &'static str {
        match self {
            Class::Read => "read",
            Class::Write => "write",
            Class::Sparql => "sparql",
            Class::Federated => "federated",
            Class::Mcp => "mcp",
            Class::Outbound => "outbound",
        }
    }

    fn env_key(self) -> String {
        format!("TAR_RATE_LIMIT_{}", self.name().to_ascii_uppercase())
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// Sort a request into its class, or `None` for the paths that are never limited: probes and
/// scrapers must not be starved by the traffic they exist to observe, and the SPA's assets are
/// what a browser fetches before it can make a single API call.
pub fn classify(method: &Method, path: &str, query: Option<&str>) -> Option<Class> {
    if matches!(path, "/healthz" | "/readyz" | "/metrics") || path.starts_with("/assets/") {
        return None;
    }
    if path == "/sparql" {
        return Some(Class::Sparql);
    }
    if path == crate::mcp::ENDPOINT_PATH {
        return Some(Class::Mcp);
    }
    if path == "/api/v1/search" && is_federated(query) {
        return Some(Class::Federated);
    }
    if let Some(rest) = path.strip_prefix("/api/v1/software/") {
        if let Some((id, action)) = rest.split_once('/') {
            if !id.is_empty() && matches!(action, "api-doc" | "sync") {
                return Some(Class::Outbound);
            }
        }
    }
    // A POST that writes nothing is a read, as `require_read_access` already reasons.
    let is_read =
        matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) || path == crate::api::artifacts::IDENTIFY_PATH;
    Some(if is_read { Class::Read } else { Class::Write })
}

/// A search that starts a fan-out (`federated=true`) or is one leg of somebody else's (carries
/// a `fed_id`). Matched on whole parameters, so `q=federated=true` is a plain search, and
/// decoded first, as the search handler's extractor decodes them: otherwise `federated=%74rue`
/// would fan out while being charged as a plain read.
fn is_federated(query: Option<&str>) -> bool {
    url::form_urlencoded::parse(query.unwrap_or("").as_bytes())
        .any(|(k, v)| (k == "federated" && v == "true") || k == "fed_id")
}

// ------------------------------------------------------------------------------- limits

/// A sustained rate per minute, and how many requests may arrive at once above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub per_minute: NonZeroU32,
    pub burst: NonZeroU32,
}

impl Limit {
    /// For literals. A zero here is a programming error, so it panics; values from the
    /// environment go through [`Limit::parse`], which refuses zero with a message instead.
    pub const fn new(per_minute: u32, burst: u32) -> Self {
        match (NonZeroU32::new(per_minute), NonZeroU32::new(burst)) {
            (Some(per_minute), Some(burst)) => Self { per_minute, burst },
            _ => panic!("a rate limit needs a rate and a burst of at least 1"),
        }
    }

    pub(crate) fn quota(self) -> Quota {
        Quota::per_minute(self.per_minute).allow_burst(self.burst)
    }

    /// `rate:burst`, or `off` for no limit on that side.
    fn parse(s: &str) -> Result<Option<Self>> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("off") {
            return Ok(None);
        }
        let (rate, burst) =
            s.split_once(':').with_context(|| format!("{s:?}: expected rate:burst, e.g. 30:10, or off"))?;
        let rate: u32 = rate.trim().parse().with_context(|| format!("{rate:?} is not a whole number"))?;
        let burst: u32 = burst.trim().parse().with_context(|| format!("{burst:?} is not a whole number"))?;
        match (NonZeroU32::new(rate), NonZeroU32::new(burst)) {
            (Some(per_minute), Some(burst)) => Ok(Some(Self { per_minute, burst })),
            _ => bail!("{s:?}: rate and burst must be at least 1; use `off` to remove a limit"),
        }
    }
}

impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/min, burst {}", self.per_minute, self.burst)
    }
}

/// The two sides of one class. `None` on a side means that side is not limited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassLimits {
    pub anon: Option<Limit>,
    pub authed: Option<Limit>,
}

// ---------------------------------------------------------------------- client address

/// An address block, for `TAR_TRUSTED_PROXIES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    net: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// `10.0.0.0/8`, `fd00::/8`, or a bare address meaning exactly that address.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let net = canonical(addr.trim().parse().with_context(|| format!("{addr:?} is not an IP address"))?);
        let max = if net.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) => p
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .with_context(|| format!("{s:?}: prefix must be a number from 0 to {max}"))?,
        };
        Ok(Self { net: mask(net, prefix), prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        ip.is_ipv4() == self.net.is_ipv4() && mask(ip, self.prefix) == self.net
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.net, self.prefix)
    }
}

/// A dual-stack listener reports IPv4 peers as `::ffff:a.b.c.d`; they are IPv4 for every
/// purpose here, or a `10.0.0.0/8` proxy entry would never match its own proxy.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(a) => {
            let m = if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix)) };
            IpAddr::V4(Ipv4Addr::from(u32::from(a) & m))
        }
        IpAddr::V6(a) => {
            let m = if prefix == 0 { 0 } else { u128::MAX << (128 - u32::from(prefix)) };
            IpAddr::V6(Ipv6Addr::from(u128::from(a) & m))
        }
    }
}

/// Who is really calling. An untrusted peer is the client whatever `X-Forwarded-For` claims —
/// otherwise any caller picks its own bucket. Behind trusted proxies the header is walked from
/// the right, past the proxies' own addresses, to the first hop none of them is. An entry that
/// does not parse stops the walk at the last hop that vouched, which is a proxy: charging every
/// client behind it together is the safe failure, letting one escape its bucket is not.
pub fn client_ip(peer: IpAddr, forwarded_for: Option<&str>, trusted: &[Cidr]) -> IpAddr {
    let is_trusted = |ip: IpAddr| trusted.iter().any(|c| c.contains(ip));
    let peer = canonical(peer);
    if !is_trusted(peer) {
        return peer;
    }
    let mut client = peer;
    for entry in forwarded_for.unwrap_or("").rsplit(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Ok(ip) = entry.parse::<IpAddr>() else { return client };
        client = canonical(ip);
        if !is_trusted(client) {
            return client;
        }
    }
    client
}

/// The address a client is charged under. One IPv6 host usually controls a whole /64, and a
/// bucket per address would hand it 2^64 of them.
pub fn bucket(ip: IpAddr) -> IpAddr {
    match canonical(ip) {
        v6 @ IpAddr::V6(_) => mask(v6, 64),
        v4 => v4,
    }
}

// ------------------------------------------------------------------------ configuration

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub trusted_proxies: Vec<Cidr>,
    /// Failed authentications per client address.
    pub auth_fail: Option<Limit>,
    classes: [ClassLimits; 6],
}

impl Default for RateLimitConfig {
    /// The design note's table (§3). Generous enough that normal use never meets them.
    fn default() -> Self {
        let both = |anon: Limit, authed: Limit| ClassLimits { anon: Some(anon), authed: Some(authed) };
        Self {
            enabled: true,
            trusted_proxies: Vec::new(),
            auth_fail: Some(Limit::new(20, 10)),
            // Indexed by `Class::index`, in `Class::ALL` order.
            classes: [
                both(Limit::new(300, 60), Limit::new(1200, 200)),
                both(Limit::new(60, 20), Limit::new(300, 60)),
                both(Limit::new(30, 10), Limit::new(120, 30)),
                both(Limit::new(10, 5), Limit::new(60, 10)),
                both(Limit::new(120, 30), Limit::new(600, 100)),
                both(Limit::new(10, 5), Limit::new(60, 10)),
            ],
        }
    }
}

impl RateLimitConfig {
    pub fn disabled() -> Self {
        Self { enabled: false, ..Self::default() }
    }

    pub fn limits(&self, class: Class) -> ClassLimits {
        self.classes[class.index()]
    }

    pub fn set(&mut self, class: Class, limits: ClassLimits) {
        self.classes[class.index()] = limits;
    }

    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
    }

    /// Reads through `get` rather than the process environment, so the parsing is testable
    /// without tests racing each other over `std::env`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let mut c = Self::default();
        if let Some(v) = get("TAR_RATE_LIMIT_ENABLED") {
            // Strict, unlike other switches: a typo here would silently remove every limit.
            c.enabled = match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                other => bail!("TAR_RATE_LIMIT_ENABLED: {other:?} is not true or false"),
            };
        }
        if let Some(v) = get("TAR_TRUSTED_PROXIES") {
            c.trusted_proxies = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(Cidr::parse)
                .collect::<Result<_>>()
                .context("TAR_TRUSTED_PROXIES")?;
        }
        for class in Class::ALL {
            let key = class.env_key();
            let Some(v) = get(&key) else { continue };
            let (anon, authed) =
                v.split_once('/').with_context(|| format!("{key}: expected <anon>/<authed>, e.g. 30:10/120:30"))?;
            c.set(
                class,
                ClassLimits {
                    anon: Limit::parse(anon).with_context(|| format!("{key}, anonymous side"))?,
                    authed: Limit::parse(authed).with_context(|| format!("{key}, authenticated side"))?,
                },
            );
        }
        if let Some(v) = get("TAR_RATE_LIMIT_AUTH_FAIL") {
            c.auth_fail = Limit::parse(&v).context("TAR_RATE_LIMIT_AUTH_FAIL")?;
        }
        Ok(c)
    }
}

// ------------------------------------------------------------------------------ runtime

/// Who a request is charged to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Ip(IpAddr),
    Principal(String),
}

/// What the limiter decided about one request.
#[derive(Debug)]
pub enum Decision {
    /// No limit applies to this class and side.
    Exempt,
    Allowed {
        limit: Limit,
        remaining: u32,
    },
    Limited {
        limit: Limit,
        retry_after: Duration,
        detail: String,
    },
}

/// `StateInformationMiddleware` so an accepted request learns how much burst is left, for the
/// `RateLimit` header.
type Limiter = RateLimiter<Key, DefaultKeyedStateStore<Key>, governor::clock::DefaultClock, StateInformationMiddleware>;

fn limiter(limit: Option<Limit>) -> Option<Limiter> {
    limit.map(|l| RateLimiter::keyed(l.quota()).with_middleware::<StateInformationMiddleware>())
}

/// The live limiters. On `AppState`, not on the router: the MCP server builds a router per tool
/// call and the tests build one per harness, and limiters owned by a router would reset each time.
pub struct RateLimits {
    anon: Vec<Option<Limiter>>,
    authed: Vec<Option<Limiter>>,
    auth_fail: Option<Limiter>,
    /// Addresses refused authentication until the instant given. governor cannot report "this
    /// key is empty" without spending from it, so the refusal is recorded when it happens.
    auth_blocked: Mutex<HashMap<IpAddr, Instant>>,
    rejections: [AtomicU64; 6],
    auth_fail_rejections: AtomicU64,
    config: RateLimitConfig,
}

impl RateLimits {
    pub fn new(config: RateLimitConfig) -> Self {
        let anon = Class::ALL.iter().map(|c| limiter(config.limits(*c).anon)).collect();
        let authed = Class::ALL.iter().map(|c| limiter(config.limits(*c).authed)).collect();
        Self {
            anon,
            authed,
            auth_fail: limiter(config.auth_fail),
            auth_blocked: Mutex::new(HashMap::new()),
            rejections: Default::default(),
            auth_fail_rejections: AtomicU64::new(0),
            config,
        }
    }

    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }

    pub fn check(&self, class: Class, key: &Key, authenticated: bool) -> Decision {
        let (limiters, limit, who) = if authenticated {
            (&self.authed, self.config.limits(class).authed, "authenticated clients")
        } else {
            (&self.anon, self.config.limits(class).anon, "anonymous clients")
        };
        let (Some(limiter), Some(limit)) = (&limiters[class.index()], limit) else { return Decision::Exempt };
        match limiter.check_key(key) {
            Ok(snapshot) => Decision::Allowed { limit, remaining: snapshot.remaining_burst_capacity() },
            Err(not_until) => {
                self.rejections[class.index()].fetch_add(1, Ordering::Relaxed);
                Decision::Limited {
                    limit,
                    retry_after: not_until.wait_time_from(limiter.clock().now()),
                    detail: format!("{} limit for {who}: {limit}", class.name()),
                }
            }
        }
    }

    /// How much longer this address is refused authentication, if it is.
    pub fn auth_blocked(&self, ip: IpAddr) -> Option<Duration> {
        let until = *self.auth_blocked.lock().unwrap().get(&bucket(ip))?;
        until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())
    }

    /// Spend one failed attempt for this address; past its burst, block it until the next
    /// attempt would be allowed.
    pub fn note_auth_failure(&self, ip: IpAddr) {
        let Some(limiter) = &self.auth_fail else { return };
        let ip = bucket(ip);
        if let Err(not_until) = limiter.check_key(&Key::Ip(ip)) {
            let wait = not_until.wait_time_from(limiter.clock().now());
            self.auth_blocked.lock().unwrap().insert(ip, Instant::now() + wait);
        }
    }

    pub fn note_auth_fail_rejection(&self) {
        self.auth_fail_rejections.fetch_add(1, Ordering::Relaxed);
    }

    /// Forget keys that have fully replenished, so the maps stay bounded under address churn.
    pub fn retain_recent(&self) {
        for l in self.anon.iter().chain(&self.authed).chain(std::iter::once(&self.auth_fail)).flatten() {
            l.retain_recent();
        }
        let now = Instant::now();
        self.auth_blocked.lock().unwrap().retain(|_, until| *until > now);
    }

    /// Prometheus text, appended to `/metrics`.
    pub fn metrics(&self) -> String {
        let mut out = String::from(
            "# HELP tar_ratelimit_rejections_total Requests refused with 429, by class\n\
             # TYPE tar_ratelimit_rejections_total counter\n",
        );
        for c in Class::ALL {
            let n = self.rejections[c.index()].load(Ordering::Relaxed);
            out.push_str(&format!("tar_ratelimit_rejections_total{{class=\"{}\"}} {n}\n", c.name()));
        }
        let n = self.auth_fail_rejections.load(Ordering::Relaxed);
        out.push_str(&format!("tar_ratelimit_rejections_total{{class=\"auth_fail\"}} {n}\n"));
        out
    }
}

/// Admins, root among them, are never limited: an operator must not be locked out of their own
/// registry during the incident the limits exist for.
pub fn is_exempt(p: &Principal) -> bool {
    p.is_admin()
}

/// The same subject at two issuers is two callers.
pub fn principal_key(p: &Principal) -> String {
    format!("{}|{}", p.issuer.as_deref().unwrap_or(""), p.subject)
}

// --------------------------------------------------------------------------- middleware

/// Who is calling, resolved once per request and stored in its extensions. The `Principal`
/// extractor returns `principal` rather than authenticating again, so a route's own `401` —
/// the MCP endpoint's `WWW-Authenticate` challenge among them — is exactly what it was.
#[derive(Debug, Clone)]
pub struct ClientContext {
    pub ip: IpAddr,
    pub principal: Result<Principal, AppError>,
}

tokio::task_local! {
    /// The outer request's client while the MCP server dispatches a tool call through the
    /// router in-process. `mcp::call::rest` copies it into each inner request, so a SPARQL query
    /// made by a tool is charged as `sparql` to the caller, not to nobody. A task-local rather
    /// than a parameter because `rest` has twenty callers and none of them should have to care.
    pub static FORWARDED: ClientContext;
}

const RATELIMIT_POLICY: HeaderName = HeaderName::from_static("ratelimit-policy");
const RATELIMIT: HeaderName = HeaderName::from_static("ratelimit");

pub async fn middleware(State(state): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    let limits = &state.rate_limits;
    let ctx = match req.extensions().get::<ClientContext>() {
        // Already resolved: an MCP tool call dispatched in-process carries its caller's.
        Some(ctx) => ctx.clone(),
        None => {
            let peer = req
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| c.0.ip());
            let forwarded = forwarded_for(req.headers());
            let ip = client_ip(peer, forwarded.as_deref(), &limits.config().trusted_proxies);
            let principal = match crate::auth::bearer(req.headers()) {
                None => Ok(Principal::anonymous()),
                Some(raw) => {
                    if limits.config().enabled {
                        if let (Some(wait), Some(limit)) = (limits.auth_blocked(ip), limits.config().auth_fail) {
                            limits.note_auth_fail_rejection();
                            let detail = format!(
                                "authentication refused for this address: too many failed credentials \
                                 (auth_fail limit: {limit})"
                            );
                            let mut resp = too_many(wait, detail);
                            set_ratelimit_headers(resp.headers_mut(), "auth_fail", limit, 0);
                            return resp;
                        }
                    }
                    let result = crate::auth::authenticate(&state, &raw).await;
                    // Only a refused credential counts, not the ops store being down.
                    if limits.config().enabled && matches!(&result, Err(e) if e.status == StatusCode::UNAUTHORIZED) {
                        limits.note_auth_failure(ip);
                    }
                    result
                }
            };
            let ctx = ClientContext { ip, principal };
            req.extensions_mut().insert(ctx.clone());
            ctx
        }
    };

    if !limits.config().enabled {
        return next.run(req).await;
    }
    let Some(class) = classify(req.method(), req.uri().path(), req.uri().query()) else {
        return next.run(req).await;
    };
    let (key, authenticated) = match &ctx.principal {
        Ok(p) if is_exempt(p) => return next.run(req).await,
        Ok(p) if !p.is_anonymous() => (Key::Principal(principal_key(p)), true),
        // Anonymous, or a credential that failed: charged to the address, so a stream of random
        // bearer strings does not buy a fresh bucket each.
        _ => (Key::Ip(bucket(ctx.ip)), false),
    };
    match limits.check(class, &key, authenticated) {
        Decision::Exempt => next.run(req).await,
        Decision::Limited { limit, retry_after, detail } => {
            let mut resp = too_many(retry_after, detail);
            set_ratelimit_headers(resp.headers_mut(), class.name(), limit, 0);
            resp
        }
        Decision::Allowed { limit, remaining } => {
            let mut resp = next.run(req).await;
            set_ratelimit_headers(resp.headers_mut(), class.name(), limit, remaining);
            resp
        }
    }
}

/// Every `X-Forwarded-For` header, joined: a proxy may append a second header rather than
/// extending the first.
fn forwarded_for(headers: &HeaderMap) -> Option<String> {
    let all: Vec<&str> = headers.get_all("x-forwarded-for").iter().filter_map(|v| v.to_str().ok()).collect();
    (!all.is_empty()).then(|| all.join(","))
}

fn too_many(retry_after: Duration, detail: String) -> Response {
    let secs = retry_after.as_secs().saturating_add(u64::from(retry_after.subsec_nanos() > 0)).max(1);
    let mut resp = AppError::too_many_requests(detail).with("retry_after", serde_json::json!(secs)).into_response();
    resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(secs));
    resp
}

/// `draft-ietf-httpapi-ratelimit-headers`: the policy (quota per 60-second window) and what is
/// left of it, with `t` the seconds until the burst is fully replenished.
fn set_ratelimit_headers(headers: &mut HeaderMap, name: &str, limit: Limit, remaining: u32) {
    let spent = u64::from(limit.burst.get().saturating_sub(remaining));
    let reset = (spent * 60).div_ceil(u64::from(limit.per_minute.get()));
    if let Ok(v) = HeaderValue::from_str(&format!("\"{name}\";q={};w=60", limit.per_minute)) {
        headers.insert(RATELIMIT_POLICY, v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("\"{name}\";r={remaining};t={reset}")) {
        headers.insert(RATELIMIT, v);
    }
}

/// Sweep replenished keys once a minute (see [`RateLimits::retain_recent`]).
pub async fn retain_loop(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        state.rate_limits.retain_recent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Principal, Role};
    use std::time::Duration;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn cidrs(list: &[&str]) -> Vec<Cidr> {
        list.iter().map(|s| Cidr::parse(s).unwrap()).collect()
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn requests_fall_into_the_class_their_cost_says() {
        let c = |m: Method, p: &str, q: Option<&str>| classify(&m, p, q);
        assert_eq!(c(Method::GET, "/sparql", Some("query=ASK%7B%7D")), Some(Class::Sparql));
        assert_eq!(c(Method::POST, "/sparql", None), Some(Class::Sparql));
        assert_eq!(c(Method::POST, "/mcp", None), Some(Class::Mcp));
        assert_eq!(c(Method::GET, "/api/v1/search", Some("q=x&federated=true")), Some(Class::Federated));
        assert_eq!(c(Method::GET, "/api/v1/search", Some("q=x&fed_id=abc&fed_hops=1")), Some(Class::Federated));
        assert_eq!(c(Method::GET, "/api/v1/software/01a/api-doc", None), Some(Class::Outbound));
        assert_eq!(c(Method::POST, "/api/v1/software/01a/sync", None), Some(Class::Outbound));
        assert_eq!(c(Method::GET, "/api/v1/software/01a/releases", None), Some(Class::Read));
        assert_eq!(c(Method::POST, "/api/v1/software", None), Some(Class::Write));
        assert_eq!(c(Method::DELETE, "/api/v1/peers/x", None), Some(Class::Write));
        assert_eq!(c(Method::POST, "/api/v1/artifacts/identify", None), Some(Class::Read));
        assert_eq!(c(Method::GET, "/software/01a", None), Some(Class::Read));
        for exempt in ["/healthz", "/readyz", "/metrics", "/assets/index-abc.js"] {
            assert_eq!(c(Method::GET, exempt, None), None, "{exempt}");
        }
    }

    /// Review focus: only the parameter itself, set to `true`, makes a search federated.
    #[test]
    fn a_search_is_federated_only_when_it_says_so() {
        let c = |q: &str| classify(&Method::GET, "/api/v1/search", Some(q));
        assert_eq!(c("q=x&federated=false"), Some(Class::Read));
        assert_eq!(c("q=federated=true"), Some(Class::Read));
        assert_eq!(c("q=x"), Some(Class::Read));
        // Decoded, as the handler decodes them.
        assert_eq!(c("q=x&federated=%74rue"), Some(Class::Federated));
        assert_eq!(c("q=x&fed%5Fid=abc"), Some(Class::Federated));
    }

    #[test]
    fn an_untrusted_peer_is_the_client_whatever_it_forwards() {
        assert_eq!(client_ip(ip("203.0.113.9"), Some("1.1.1.1"), &cidrs(&["10.0.0.0/8"])), ip("203.0.113.9"));
        assert_eq!(client_ip(ip("203.0.113.9"), Some("1.1.1.1"), &[]), ip("203.0.113.9"));
    }

    #[test]
    fn a_trusted_chain_is_walked_from_the_right() {
        let t = cidrs(&["10.0.0.0/8"]);
        // The spoofed leftmost entry is never reached: the first untrusted hop from the right wins.
        assert_eq!(client_ip(ip("10.0.0.2"), Some("6.6.6.6, 198.51.100.7, 10.0.0.5"), &t), ip("198.51.100.7"));
        assert_eq!(client_ip(ip("10.0.0.2"), Some("10.1.1.1, 10.0.0.5"), &t), ip("10.1.1.1"));
        assert_eq!(client_ip(ip("10.0.0.2"), None, &t), ip("10.0.0.2"));
        assert_eq!(client_ip(ip("10.0.0.2"), Some("198.51.100.7,,10.0.0.5"), &t), ip("198.51.100.7"));
    }

    /// Review focus: garbage stops the walk at the last hop that vouched — a proxy — so a caller
    /// cannot escape its bucket by forwarding something unparsable.
    #[test]
    fn a_malformed_entry_stops_the_walk_at_the_last_trusted_hop() {
        let t = cidrs(&["10.0.0.0/8"]);
        assert_eq!(client_ip(ip("10.0.0.2"), Some("198.51.100.7, garbage"), &t), ip("10.0.0.2"));
        assert_eq!(client_ip(ip("10.0.0.2"), Some("198.51.100.7, 1.2.3.4:80, 10.0.0.5"), &t), ip("10.0.0.5"));
    }

    /// Review focus: a dual-stack listener reports IPv4 peers as `::ffff:a.b.c.d`.
    #[test]
    fn an_ipv4_mapped_peer_is_matched_and_bucketed_as_ipv4() {
        let t = cidrs(&["10.0.0.0/8"]);
        assert_eq!(client_ip(ip("::ffff:10.0.0.2"), Some("198.51.100.7"), &t), ip("198.51.100.7"));
        assert_eq!(bucket(ip("::ffff:198.51.100.7")), ip("198.51.100.7"));
    }

    #[test]
    fn ipv6_clients_share_a_bucket_per_64() {
        assert_eq!(bucket(ip("2001:db8:1:2:aaaa::1")), bucket(ip("2001:db8:1:2:ffff::9")));
        assert_ne!(bucket(ip("2001:db8:1:2::1")), bucket(ip("2001:db8:1:3::1")));
        assert_eq!(bucket(ip("198.51.100.7")), ip("198.51.100.7"));
    }

    #[test]
    fn cidrs_parse_match_and_refuse() {
        assert!(Cidr::parse("10.0.0.0/8").unwrap().contains(ip("10.255.0.1")));
        assert!(!Cidr::parse("10.0.0.0/8").unwrap().contains(ip("11.0.0.1")));
        assert!(Cidr::parse("192.168.1.7").unwrap().contains(ip("192.168.1.7")));
        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));
        assert!(Cidr::parse("fd00::/8").unwrap().contains(ip("fd12::1")));
        assert!(!Cidr::parse("fd00::/8").unwrap().contains(ip("10.0.0.1")));
        assert_eq!(Cidr::parse("10.1.2.3/8").unwrap().to_string(), "10.0.0.0/8");
        for bad in ["10.0.0.0/33", "fd00::/129", "not-an-ip", "10.0.0.0/x"] {
            assert!(Cidr::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn defaults_match_the_design_note() {
        let c = RateLimitConfig::from_lookup(lookup(&[])).unwrap();
        assert!(c.enabled);
        assert!(c.trusted_proxies.is_empty());
        assert_eq!(
            c.limits(Class::Read),
            ClassLimits { anon: Some(Limit::new(300, 60)), authed: Some(Limit::new(1200, 200)) }
        );
        assert_eq!(
            c.limits(Class::Write),
            ClassLimits { anon: Some(Limit::new(60, 20)), authed: Some(Limit::new(300, 60)) }
        );
        assert_eq!(
            c.limits(Class::Sparql),
            ClassLimits { anon: Some(Limit::new(30, 10)), authed: Some(Limit::new(120, 30)) }
        );
        assert_eq!(
            c.limits(Class::Federated),
            ClassLimits { anon: Some(Limit::new(10, 5)), authed: Some(Limit::new(60, 10)) }
        );
        assert_eq!(
            c.limits(Class::Mcp),
            ClassLimits { anon: Some(Limit::new(120, 30)), authed: Some(Limit::new(600, 100)) }
        );
        assert_eq!(
            c.limits(Class::Outbound),
            ClassLimits { anon: Some(Limit::new(10, 5)), authed: Some(Limit::new(60, 10)) }
        );
        assert_eq!(c.auth_fail, Some(Limit::new(20, 10)));
        assert_eq!(Limit::new(30, 10).to_string(), "30/min, burst 10");
    }

    #[test]
    fn overrides_are_read_per_class_and_side() {
        let c = RateLimitConfig::from_lookup(lookup(&[
            ("TAR_RATE_LIMIT_ENABLED", "false"),
            ("TAR_RATE_LIMIT_SPARQL", "5:2/off"),
            ("TAR_TRUSTED_PROXIES", "10.0.0.0/8, fd00::/8"),
            ("TAR_RATE_LIMIT_AUTH_FAIL", "off"),
        ]))
        .unwrap();
        assert!(!c.enabled);
        assert_eq!(c.limits(Class::Sparql), ClassLimits { anon: Some(Limit::new(5, 2)), authed: None });
        assert_eq!(c.limits(Class::Read), RateLimitConfig::default().limits(Class::Read));
        assert_eq!(c.trusted_proxies.len(), 2);
        assert_eq!(c.auth_fail, None);
    }

    /// Review focus: a bad value fails boot and says which variable, rather than panicking in
    /// `NonZeroU32` or quietly keeping the default.
    #[test]
    fn a_malformed_override_fails_and_names_the_variable() {
        for (k, v) in [
            ("TAR_RATE_LIMIT_SPARQL", "30:10"),
            ("TAR_RATE_LIMIT_SPARQL", "0:0/1:1"),
            ("TAR_RATE_LIMIT_READ", "ten:1/1:1"),
            ("TAR_RATE_LIMIT_MCP", "1:1/1"),
            ("TAR_TRUSTED_PROXIES", "10.0.0.0/40"),
            ("TAR_RATE_LIMIT_AUTH_FAIL", "5"),
            ("TAR_RATE_LIMIT_ENABLED", "flase"),
        ] {
            let err = RateLimitConfig::from_lookup(lookup(&[(k, v)])).unwrap_err();
            assert!(format!("{err:#}").contains(k), "{k}={v}: {err:#}");
        }
    }

    fn with(class: Class, anon: Option<Limit>, authed: Option<Limit>) -> RateLimits {
        let mut cfg = RateLimitConfig::default();
        cfg.set(class, ClassLimits { anon, authed });
        RateLimits::new(cfg)
    }

    #[test]
    fn a_bucket_refuses_past_its_burst_and_says_when_to_retry() {
        let rl = with(Class::Sparql, Some(Limit::new(1, 2)), None);
        let a = Key::Ip(ip("198.51.100.7"));
        assert!(matches!(rl.check(Class::Sparql, &a, false), Decision::Allowed { remaining: 1, .. }));
        assert!(matches!(rl.check(Class::Sparql, &a, false), Decision::Allowed { remaining: 0, .. }));
        match rl.check(Class::Sparql, &a, false) {
            Decision::Limited { retry_after, detail, .. } => {
                assert!(retry_after > Duration::from_secs(50), "{retry_after:?}");
                assert_eq!(detail, "sparql limit for anonymous clients: 1/min, burst 2");
            }
            _ => panic!("the third request passed a burst of two"),
        }
        // Another address, and another class for the same address, are untouched.
        assert!(matches!(rl.check(Class::Sparql, &Key::Ip(ip("198.51.100.8")), false), Decision::Allowed { .. }));
        assert!(matches!(rl.check(Class::Read, &a, false), Decision::Allowed { .. }));
        // A side set to `off` is not limited at all.
        assert!(matches!(rl.check(Class::Sparql, &Key::Principal("p".into()), true), Decision::Exempt));
        assert!(rl.metrics().contains("tar_ratelimit_rejections_total{class=\"sparql\"} 1\n"), "{}", rl.metrics());
        assert!(rl.metrics().contains("tar_ratelimit_rejections_total{class=\"read\"} 0\n"));
    }

    #[test]
    fn repeated_failed_credentials_block_an_address_for_a_while() {
        let rl = RateLimits::new(RateLimitConfig { auth_fail: Some(Limit::new(1, 2)), ..Default::default() });
        let bad = ip("198.51.100.7");
        rl.note_auth_failure(bad);
        rl.note_auth_failure(bad);
        assert_eq!(rl.auth_blocked(bad), None, "two failures are within the burst");
        rl.note_auth_failure(bad);
        assert!(rl.auth_blocked(bad).is_some_and(|d| d > Duration::from_secs(50)), "{:?}", rl.auth_blocked(bad));
        assert_eq!(rl.auth_blocked(ip("198.51.100.8")), None);
        // Expired blocks are forgotten by the sweep; a live one survives it.
        rl.retain_recent();
        assert!(rl.auth_blocked(bad).is_some());
    }

    #[test]
    fn admins_are_exempt_and_nobody_else_is() {
        let mut admin = Principal::anonymous();
        admin.roles.insert(Role::Admin);
        let mut curator = Principal::anonymous();
        curator.roles.insert(Role::Curator);
        assert!(is_exempt(&admin));
        assert!(!is_exempt(&curator));
        assert!(!is_exempt(&Principal::anonymous()));
    }

    #[test]
    fn a_principal_is_keyed_by_issuer_and_subject() {
        let mut a = Principal::anonymous();
        a.subject = "alice".into();
        a.issuer = Some("https://kc.example/realms/a".into());
        let mut b = a.clone();
        b.issuer = Some("https://kc.example/realms/b".into());
        assert_ne!(principal_key(&a), principal_key(&b), "the same subject at two issuers is two callers");
    }
}
