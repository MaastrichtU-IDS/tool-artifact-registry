# Rate Limiting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per-class, per-client rate limits in the registry itself, keyed on the verified principal or the real client IP, answering `429` as problem+json.

**Architecture:** One new module, `src/ratelimit.rs`, holds the pure pieces (classification, CIDR and `X-Forwarded-For` handling, config parsing) and the runtime (`RateLimits`, a set of `governor` keyed limiters living on `AppState`). One axum middleware resolves the client (IP + authenticate once), stores a `ClientContext` in request extensions, and charges the request's class. The `Principal` extractor and the MCP transport reuse that context rather than authenticating again; MCP's in-process dispatch forwards it through a Tokio task-local.

**Tech Stack:** Rust, axum 0.8, tokio, `governor` 0.10 (new), existing `tests/` harness pattern (`tower::ServiceExt::oneshot` against `tar::app`).

**Spec:** `docs/specs/2026-09-23-rate-limiting.md` — read it first. Where this plan and the spec disagree, stop and ask.

## Global Constraints

- Default limits, exactly (anon per IP / authed per principal, `rate:burst` per minute): `read` 300:60 / 1200:200 · `write` 60:20 / 300:60 · `sparql` 30:10 / 120:30 · `federated` 10:5 / 60:10 · `mcp` 120:30 / 600:100 · `outbound` 10:5 / 60:10 · `auth_fail` 20:10 per IP.
- Exempt paths: `/healthz`, `/readyz`, `/metrics`, `/assets/*`. Exempt principals: anything `Principal::is_admin()` (root has `Role::Admin`).
- Env: `TAR_RATE_LIMIT_ENABLED` (default `true`), `TAR_TRUSTED_PROXIES` (CIDRs, default unset), `TAR_RATE_LIMIT_<READ|WRITE|SPARQL|FEDERATED|MCP|OUTBOUND>=<anon>/<authed>` each side `rate:burst` or `off`, `TAR_RATE_LIMIT_AUTH_FAIL=<rate:burst|off>`. A malformed value fails at boot and the error names the variable.
- IPv6 clients bucket per /64; IPv4-mapped IPv6 is treated as IPv4 everywhere.
- `429` body is `application/problem+json` via `AppError`, `type` `https://w3id.org/tar/problem/rate-limited`, with `Retry-After` (whole seconds, ≥1).
- `Config::for_test` disables the limiter; the 292 existing tests must pass unchanged.
- Only new dependency: `governor = "0.10"`. CIDR matching is hand-written.
- House style: `rustfmt.toml` is authoritative (`cargo fmt`); comments explain *why*, in full sentences, like the surrounding code. Commit messages: imperative, sentence-case subject describing the behaviour (see `git log`), body explaining why, ending with the attribution lines:
  ```
  Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_014kEAfhgTfX5ocg392QFViY
  ```

## Review Focus

- **A malformed `X-Forwarded-For` entry** (garbage, `ip:port`) from behind a trusted proxy must not let a caller escape limiting: the walk stops and charges the last hop that vouched (a proxy address). Pinned in Task 1.
- **A dual-stack listener reporting `::ffff:10.0.0.2`** must still match a `10.0.0.0/8` trusted-proxy entry and bucket as IPv4. Pinned in Task 1.
- **`federated=false`, or `federated=true` inside another parameter's value**, must stay `read`, not be charged as federated. Pinned in Task 1.
- **A zero or half-written override** (`0:0`, `30:10` without `/`) must fail boot naming the variable, not panic in `NonZeroU32` or silently keep the default. Pinned in Task 1.
- **An MCP tool call whose inner request is refused** must come back as a tool error (`isError: true`) inside a `200` JSON-RPC response, not a transport failure. Pinned in Task 4.

---

### Task 1: The pure pieces — classes, client IP, configuration

**Files:**
- Modify: `Cargo.toml` (add `governor`)
- Create: `src/ratelimit.rs`
- Modify: `src/lib.rs` (add `pub mod ratelimit;`)
- Modify: `src/config.rs` (field `rate_limit`, in `from_env` and `for_test`)
- Modify: `src/main.rs` (`tar config` prints limits)

**Interfaces:**
- Produces (all `pub` in `tar::ratelimit`):
  - `enum Class { Read, Write, Sparql, Federated, Mcp, Outbound }` with `const ALL: [Class; 6]`, `fn name(self) -> &'static str` (lowercase), private `fn index(self) -> usize`.
  - `fn classify(method: &Method, path: &str, query: Option<&str>) -> Option<Class>` — `None` = exempt.
  - `struct Limit { pub per_minute: NonZeroU32, pub burst: NonZeroU32 }`, `const fn Limit::new(u32, u32) -> Limit` (panics on zero; for literals only), `Display` = `"30/min, burst 10"`.
  - `struct ClassLimits { pub anon: Option<Limit>, pub authed: Option<Limit> }` (`Copy`, `PartialEq`, `Debug`).
  - `struct Cidr` with `fn parse(&str) -> anyhow::Result<Cidr>`, `fn contains(&self, IpAddr) -> bool`, `Display`.
  - `fn client_ip(peer: IpAddr, forwarded_for: Option<&str>, trusted: &[Cidr]) -> IpAddr`
  - `fn bucket(ip: IpAddr) -> IpAddr`
  - `struct RateLimitConfig { pub enabled: bool, pub trusted_proxies: Vec<Cidr>, pub auth_fail: Option<Limit>, classes: [ClassLimits; 6] }` with `Default` (the spec table), `fn disabled() -> Self`, `fn limits(&self, Class) -> ClassLimits`, `fn set(&mut self, Class, ClassLimits)`, `fn from_env() -> Result<Self>`, `fn from_lookup(impl Fn(&str) -> Option<String>) -> Result<Self>`.
  - `Config.rate_limit: RateLimitConfig`.

- [ ] **Step 1: Add the dependency and an empty module**

In `Cargo.toml` `[dependencies]`, alphabetically after `futures`:

```toml
# Keyed GCRA rate limiters for `src/ratelimit.rs`. In-memory is enough: the registry runs as one
# replica, because Oxigraph is single-writer.
governor = "0.10"
```

In `src/lib.rs` add `pub mod ratelimit;` between `pub mod ops;` and `pub mod rdf;`.

Create `src/ratelimit.rs` with only the module doc for now:

```rust
//! Rate limiting (design note `docs/specs/2026-09-23-rate-limiting.md`, answering spec Q8).
//!
//! A request is sorted into a cost class and charged to whoever is calling: the verified
//! principal if it authenticated, the client's address otherwise. The limits live here rather
//! than only at an ingress because a bare `docker run` has none, and an ingress can tell neither
//! a curator from a stranger nor a list read from a SPARQL query.
```

- [ ] **Step 2: Write the failing unit tests**

Append to `src/ratelimit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
        assert_eq!(c.limits(Class::Read), ClassLimits { anon: Some(Limit::new(300, 60)), authed: Some(Limit::new(1200, 200)) });
        assert_eq!(c.limits(Class::Write), ClassLimits { anon: Some(Limit::new(60, 20)), authed: Some(Limit::new(300, 60)) });
        assert_eq!(c.limits(Class::Sparql), ClassLimits { anon: Some(Limit::new(30, 10)), authed: Some(Limit::new(120, 30)) });
        assert_eq!(c.limits(Class::Federated), ClassLimits { anon: Some(Limit::new(10, 5)), authed: Some(Limit::new(60, 10)) });
        assert_eq!(c.limits(Class::Mcp), ClassLimits { anon: Some(Limit::new(120, 30)), authed: Some(Limit::new(600, 100)) });
        assert_eq!(c.limits(Class::Outbound), ClassLimits { anon: Some(Limit::new(10, 5)), authed: Some(Limit::new(60, 10)) });
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
        ] {
            let err = RateLimitConfig::from_lookup(lookup(&[(k, v)])).unwrap_err();
            assert!(format!("{err:#}").contains(k), "{k}={v}: {err:#}");
        }
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --lib ratelimit`
Expected: compile errors — `classify`, `Class`, `Cidr`, `client_ip`, `bucket`, `RateLimitConfig`, `Limit`, `ClassLimits` not found.

- [ ] **Step 4: Implement the pure pieces**

Insert between the module doc and `#[cfg(test)]` in `src/ratelimit.rs`:

```rust
use anyhow::{bail, Context, Result};
use axum::http::Method;
use governor::Quota;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU32;

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
    pub const ALL: [Class; 6] = [Class::Read, Class::Write, Class::Sparql, Class::Federated, Class::Mcp, Class::Outbound];

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
    let is_read = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        || path == crate::api::artifacts::IDENTIFY_PATH;
    Some(if is_read { Class::Read } else { Class::Write })
}

/// A search that starts a fan-out (`federated=true`) or is one leg of somebody else's (carries
/// a `fed_id`). Matched on whole parameters, so `q=federated=true` is a plain search.
fn is_federated(query: Option<&str>) -> bool {
    query.unwrap_or("").split('&').any(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == "federated" && v == "true") || k == "fed_id"
    })
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
        let (rate, burst) = s.split_once(':').with_context(|| format!("{s:?}: expected rate:burst, e.g. 30:10, or off"))?;
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
            c.enabled = matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
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
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib ratelimit`
Expected: 11 tests PASS.

- [ ] **Step 6: Wire the config**

In `src/config.rs`, add to `pub struct Config` after `pub oidc: OidcConfig,`:

```rust
    /// Rate limits (`src/ratelimit.rs`). On by default, so a bare `docker run` is protected.
    pub rate_limit: crate::ratelimit::RateLimitConfig,
```

In `Config::from_env`'s `Ok(Self { … })`, after `oidc,`:

```rust
            rate_limit: crate::ratelimit::RateLimitConfig::from_env()?,
```

In `Config::for_test`, after the `oidc: OidcConfig { … },` field:

```rust
            // Off, so no existing test meets a limit by running fast. The middleware that
            // resolves the client still runs, so every suite exercises authenticate-once.
            rate_limit: crate::ratelimit::RateLimitConfig::disabled(),
```

In `src/main.rs`, inside `Command::Config => { … }`, after the last existing `println!`, add:

```rust
            let rl = &c.rate_limit;
            let side = |l: Option<tar::ratelimit::Limit>| l.map_or_else(|| "off".to_string(), |l| l.to_string());
            println!("rate_limit            {}", if rl.enabled { "on" } else { "off" });
            let proxies: Vec<String> = rl.trusted_proxies.iter().map(ToString::to_string).collect();
            println!("trusted_proxies       {}", if proxies.is_empty() { "(none)".into() } else { proxies.join(", ") });
            for class in tar::ratelimit::Class::ALL {
                let l = rl.limits(class);
                println!("rate_limit.{:<11}anon {} · authed {}", class.name(), side(l.anon), side(l.authed));
            }
            println!("rate_limit.auth_fail  {}", side(rl.auth_fail));
```

(Check how `main.rs` refers to the library crate — if it uses `tar::`, keep that; match the existing `Config` import.)

- [ ] **Step 7: Verify everything still builds and passes**

Run: `cargo test 2>&1 | grep -E '^test result|FAILED|error'`
Expected: every `test result: ok`, none failed. Then `TAR_BASE_IRI=http://x TAR_RATE_LIMIT_SPARQL=0:0/1:1 cargo run -q -- config` — expected: exits non-zero with an error containing `TAR_RATE_LIMIT_SPARQL`.

- [ ] **Step 8: Format and commit**

```bash
cargo fmt
git add Cargo.toml Cargo.lock src/ratelimit.rs src/lib.rs src/config.rs src/main.rs
git commit -F - <<'EOF'
Classify requests by cost and find the real client behind a proxy

The pure half of rate limiting: which class a request is charged to, how
the client address is recovered from X-Forwarded-For without letting an
untrusted caller choose it, and the TAR_RATE_LIMIT_* configuration. A bad
value fails at boot and names the variable.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_014kEAfhgTfX5ocg392QFViY
EOF
```

---

### Task 2: The limiter — buckets, failed-credential blocking, metrics

**Files:**
- Modify: `src/ratelimit.rs`
- Modify: `src/state.rs` (field `rate_limits`)
- Modify: `src/error.rs` (`Clone`, `too_many_requests`)
- Modify: `src/api/registry.rs` (`metrics` appends limiter counters)

**Interfaces:**
- Consumes: Task 1's `Class`, `Limit`, `ClassLimits`, `RateLimitConfig`, `bucket`.
- Produces (in `tar::ratelimit`):
  - `enum Key { Ip(IpAddr), Principal(String) }` (`Clone, Hash, Eq`).
  - `enum Decision { Exempt, Allowed { limit: Limit, remaining: u32 }, Limited { limit: Limit, retry_after: Duration, detail: String } }`.
  - `struct RateLimits` with `fn new(RateLimitConfig) -> Self`, `fn config(&self) -> &RateLimitConfig`, `fn check(&self, Class, &Key, authenticated: bool) -> Decision`, `fn auth_blocked(&self, IpAddr) -> Option<Duration>`, `fn note_auth_failure(&self, IpAddr)`, `fn note_auth_fail_rejection(&self)`, `fn retain_recent(&self)`, `fn metrics(&self) -> String`.
  - `fn is_exempt(&Principal) -> bool`, `fn principal_key(&Principal) -> String`.
  - `AppState.rate_limits: RateLimits`.
  - `AppError: Clone`; `AppError::too_many_requests(detail) -> AppError` (status 429, kind `rate-limited`, title `Too many requests`).

- [ ] **Step 1: Write the failing unit tests**

Add inside `mod tests` in `src/ratelimit.rs` (and add `use crate::auth::{Principal, Role};` and `use std::time::Duration;` at the top of `mod tests`):

```rust
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
        let mut cfg = RateLimitConfig::default();
        cfg.auth_fail = Some(Limit::new(1, 2));
        let rl = RateLimits::new(cfg);
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib ratelimit`
Expected: compile errors — `RateLimits`, `Key`, `Decision`, `is_exempt`, `principal_key` not found.

- [ ] **Step 3: Make `AppError` cloneable and add the 429 constructor**

In `src/error.rs`, change `#[derive(Debug)]` on `pub struct AppError` to `#[derive(Debug, Clone)]` (a stored authentication result is handed to each extractor that asks, so it must clone). Add after `pub fn gone`:

```rust
    /// Over a rate limit (`src/ratelimit.rs`). The middleware adds `Retry-After`.
    pub fn too_many_requests(d: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "rate-limited", "Too many requests").detail(d)
    }
```

- [ ] **Step 4: Implement the runtime**

In `src/ratelimit.rs`, extend the imports:

```rust
use crate::auth::Principal;
use governor::clock::Clock;
use governor::middleware::StateInformationMiddleware;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::RateLimiter;
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
```

Add before `#[cfg(test)]`:

```rust
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
    Allowed { limit: Limit, remaining: u32 },
    Limited { limit: Limit, retry_after: Duration, detail: String },
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
```

- [ ] **Step 5: Put it on `AppState` and in `/metrics`**

In `src/state.rs`, add to `pub struct AppState` after `pub api_doc_cache: …,`:

```rust
    /// Rate limiters (`crate::ratelimit`). Here rather than on the router, which is rebuilt per
    /// MCP tool call and per test harness.
    pub rate_limits: crate::ratelimit::RateLimits,
```

In `from_parts`, first line of the body after `let timeout = …;`:

```rust
        let rate_limits = crate::ratelimit::RateLimits::new(config.rate_limit.clone());
```

and in the `Self { … }` literal add `rate_limits,` after `api_doc_cache: Default::default(),`.

In `src/api/registry.rs` `metrics`, before the final `Ok((…))`:

```rust
    out.push_str(&state.rate_limits.metrics());
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib ratelimit`
Expected: 15 tests PASS.
Run: `cargo test 2>&1 | grep -E '^test result|FAILED|error'`
Expected: all ok.

- [ ] **Step 7: Format and commit**

```bash
cargo fmt
git add src/ratelimit.rs src/state.rs src/error.rs src/api/registry.rs
git commit -F - <<'EOF'
Keep per-client buckets, and block an address guessing credentials

The limiter itself: one governor keyed limiter per class and side, on
AppState so a router rebuilt per MCP call or per test does not reset it.
Failed authentications spend a per-address bucket; once it is empty the
address is refused authentication until the next attempt would be
allowed. Rejections are counted in /metrics.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_014kEAfhgTfX5ocg392QFViY
EOF
```

---

### Task 3: The middleware — authenticate once, charge, answer 429

**Files:**
- Modify: `src/ratelimit.rs` (`ClientContext`, `middleware`, `retain_loop`, response helpers)
- Modify: `src/auth/mod.rs` (`bearer` takes `&HeaderMap` and is `pub`; extractor reuses `ClientContext`)
- Modify: `src/api/mod.rs` (layer)
- Modify: `src/main.rs` (`ConnectInfo`, `retain_loop`)
- Create: `tests/ratelimit.rs`

**Interfaces:**
- Consumes: Task 1 `classify`, `client_ip`, `bucket`, `Cidr`; Task 2 `RateLimits`, `Key`, `Decision`, `is_exempt`, `principal_key`, `AppError::too_many_requests`.
- Produces:
  - `pub struct ClientContext { pub ip: IpAddr, pub principal: Result<Principal, AppError> }` (`Clone`).
  - `pub async fn middleware(State<Arc<AppState>>, Request, Next) -> Response`.
  - `pub async fn retain_loop(state: Arc<AppState>)`.
  - `pub fn crate::auth::bearer(headers: &HeaderMap) -> Option<String>`.
  - Task 4 relies on: the middleware using an existing `ClientContext` extension as-is instead of resolving one.

- [ ] **Step 1: Write the failing end-to-end tests**

Create `tests/ratelimit.rs`:

```rust
//! Rate limiting against the real router (design note `docs/specs/2026-09-23-rate-limiting.md`).
//!
//! Each request carries a `ConnectInfo`, as `axum::serve` gives a real one, so the tests choose
//! the caller's address.

mod common;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tar::config::Config;
use tar::ops::Ops;
use tar::ratelimit::{Cidr, Class, ClassLimits, Limit, RateLimitConfig};
use tar::AppState;
use tower::ServiceExt;

const BASE: &str = "http://reg.test";
const ROOT: &str = "test-root-token-ratelimit-0123";
const V: &str = "2026-07-28";
const ASK: &str = "/sparql?query=ASK%20%7B%7D";
const TOO_MANY: StatusCode = StatusCode::TOO_MANY_REQUESTS;

struct Harness {
    app: axum::Router,
}

async fn harness(tune: impl FnOnce(&mut RateLimitConfig)) -> Harness {
    let mut config = Config::for_test(BASE);
    config.root_token = Some(ROOT.into());
    let mut rl = RateLimitConfig::default();
    tune(&mut rl);
    config.rate_limit = rl;
    let store = common::test_store().await;
    let ops = Ops::open(":memory:").await.unwrap();
    let state = Arc::new(AppState::from_parts(config, store, ops));
    tar::seed::load_vocab(&state).unwrap();
    Harness { app: tar::app(state) }
}

fn only(anon: Limit) -> ClassLimits {
    ClassLimits { anon: Some(anon), authed: None }
}

impl Harness {
    async fn send(
        &self,
        from: &str,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value, HeaderMap) {
        let mut b = Request::builder().method(method).uri(uri);
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept")) {
            b = b.header("accept", "application/json");
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        let mut req = match body {
            Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(from.parse().unwrap(), 40000)));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, value, headers)
    }

    async fn get(&self, from: &str, uri: &str) -> StatusCode {
        self.send(from, "GET", uri, None, None, &[]).await.0
    }

    /// A registry token that is not an admin's: minted for a software record by root.
    async fn software_token(&self) -> String {
        let (s, sw, _) = self
            .send("192.0.2.1", "POST", "/api/v1/software", Some(ROOT), Some(json!({"name": "rl-tool", "kinds": ["service"]})), &[])
            .await;
        assert_eq!(s, StatusCode::CREATED, "{sw}");
        let uri = format!("/api/v1/software/{}/tokens", sw["id"].as_str().unwrap());
        let (s, minted, _) = self.send("192.0.2.1", "POST", &uri, Some(ROOT), Some(json!({})), &[]).await;
        assert_eq!(s, StatusCode::CREATED, "{minted}");
        minted["token"].as_str().unwrap().to_string()
    }
}

#[tokio::test]
async fn an_anonymous_sparql_burst_is_refused_with_a_retry_after_and_other_addresses_are_not() {
    let h = harness(|rl| rl.set(Class::Sparql, only(Limit::new(1, 2)))).await;
    for _ in 0..2 {
        let (s, body, headers) = h.send("198.51.100.7", "GET", ASK, None, None, &[]).await;
        assert_ne!(s, TOO_MANY, "{body}");
        assert!(headers.contains_key("ratelimit-policy"), "{headers:?}");
        assert!(headers.contains_key("ratelimit"), "{headers:?}");
    }
    let (s, body, headers) = h.send("198.51.100.7", "GET", ASK, None, None, &[]).await;
    assert_eq!(s, TOO_MANY, "{body}");
    assert_eq!(headers["content-type"], "application/problem+json");
    let retry: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((50..=60).contains(&retry), "{retry}");
    assert_eq!(body["type"], "https://w3id.org/tar/problem/rate-limited");
    assert!(body["detail"].as_str().unwrap().contains("sparql limit for anonymous clients"), "{body}");

    assert_ne!(h.get("198.51.100.8", ASK).await, TOO_MANY, "another address has its own bucket");
    assert_eq!(h.get("198.51.100.7", "/api/v1/software").await, StatusCode::OK, "another class is untouched");

    let (_, metrics, _) = h.send("198.51.100.7", "GET", "/metrics", None, None, &[]).await;
    assert!(metrics.as_str().unwrap().contains("tar_ratelimit_rejections_total{class=\"sparql\"} 1"), "{metrics}");
}

#[tokio::test]
async fn an_authenticated_caller_is_charged_to_itself_not_to_its_address() {
    let h = harness(|rl| {
        rl.set(Class::Read, ClassLimits { anon: Some(Limit::new(1, 1)), authed: Some(Limit::new(1, 3)) })
    })
    .await;
    let token = h.software_token().await;
    let from = "198.51.100.7";
    assert_eq!(h.get(from, "/api/v1/software").await, StatusCode::OK);
    assert_eq!(h.get(from, "/api/v1/software").await, TOO_MANY, "the address has spent its one");
    for i in 0..3 {
        let (s, body, _) = h.send(from, "GET", "/api/v1/software", Some(&token), None, &[]).await;
        assert_eq!(s, StatusCode::OK, "request {i} with the credential: {body}");
    }
    // The credential's bucket follows it to another address.
    let (s, _, _) = h.send("203.0.113.5", "GET", "/api/v1/software", Some(&token), None, &[]).await;
    assert_eq!(s, TOO_MANY);
}

#[tokio::test]
async fn the_root_token_is_never_limited() {
    let h = harness(|rl| rl.set(Class::Sparql, ClassLimits { anon: Some(Limit::new(1, 1)), authed: Some(Limit::new(1, 1)) }))
        .await;
    for i in 0..5 {
        let (s, body, _) = h.send("198.51.100.7", "GET", ASK, Some(ROOT), None, &[]).await;
        assert_ne!(s, TOO_MANY, "request {i}: {body}");
    }
}

#[tokio::test]
async fn random_credentials_from_one_address_are_refused_before_they_are_checked() {
    let h = harness(|rl| rl.auth_fail = Some(Limit::new(1, 2))).await;
    let from = "198.51.100.7";
    // Two failures fit the burst; the third is refused by the bucket, which blocks the address.
    for i in 0..3 {
        let (s, body, _) = h.send(from, "GET", "/api/v1/whoami", Some(&format!("tar_guess{i}_x")), None, &[]).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "attempt {i}: {body}");
    }
    let (s, body, headers) = h.send(from, "GET", "/api/v1/whoami", Some("tar_guess9_x"), None, &[]).await;
    assert_eq!(s, TOO_MANY, "{body}");
    assert!(headers.contains_key("retry-after"));
    assert!(body["detail"].as_str().unwrap().contains("authentication"), "{body}");
    // Reading without a credential is still fine from there, and other addresses may still try.
    assert_eq!(h.get(from, "/api/v1/software").await, StatusCode::OK);
    let (s, _, _) = h.send("198.51.100.8", "GET", "/api/v1/whoami", Some("tar_guess_x"), None, &[]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn probes_are_never_limited() {
    let h = harness(|rl| rl.set(Class::Read, only(Limit::new(1, 1)))).await;
    let from = "198.51.100.7";
    assert_eq!(h.get(from, "/api/v1/software").await, StatusCode::OK);
    assert_eq!(h.get(from, "/api/v1/software").await, TOO_MANY);
    for path in ["/healthz", "/readyz", "/metrics"] {
        for _ in 0..3 {
            assert_ne!(h.get(from, path).await, TOO_MANY, "{path}");
        }
    }
}

#[tokio::test]
async fn behind_a_trusted_proxy_the_forwarded_client_is_charged_and_a_forged_header_is_not() {
    let h = harness(|rl| {
        rl.set(Class::Read, only(Limit::new(1, 1)));
        rl.trusted_proxies = vec![Cidr::parse("10.0.0.0/8").unwrap()];
    })
    .await;
    let via = |client: &'static str| [("x-forwarded-for", client)];
    let read = |from: &'static str, xff: &'static str| {
        let h = &h;
        async move { h.send(from, "GET", "/api/v1/software", None, None, &via(xff)).await.0 }
    };
    assert_eq!(read("10.0.0.2", "198.51.100.7").await, StatusCode::OK);
    assert_eq!(read("10.0.0.2", "198.51.100.7").await, TOO_MANY);
    assert_eq!(read("10.0.0.2", "198.51.100.8").await, StatusCode::OK, "a different client behind the same proxy");
    // An untrusted caller forging the header is charged to its own address.
    assert_eq!(read("203.0.113.5", "198.51.100.9").await, StatusCode::OK);
    assert_eq!(read("203.0.113.5", "198.51.100.10").await, TOO_MANY);
}
```

(`V` is used by Task 4's test; `#[allow(dead_code)]` is not needed once that test exists. If clippy flags it before then, leave it — Task 4 lands in the same branch.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test ratelimit`
Expected: compiles; tests FAIL — no `429` is ever returned (e.g. `an_anonymous_sparql_burst…` fails at `assert_eq!(s, TOO_MANY)`).

- [ ] **Step 3: Make `bearer` reusable and let the extractor reuse the stored result**

In `src/auth/mod.rs`, replace:

```rust
fn bearer(parts: &Parts) -> Option<String> {
    let h = parts.headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
```

with:

```rust
/// The bearer credential in an `Authorization` header, if there is one.
pub fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    let h = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
```

and replace the body of `from_request_parts` with:

```rust
        // The rate-limit middleware has already authenticated this request once, to know whom
        // to charge; doing it again here would double the cost of every credential check.
        if let Some(ctx) = parts.extensions.get::<crate::ratelimit::ClientContext>() {
            return ctx.principal.clone();
        }
        let Some(raw) = bearer(&parts.headers) else { return Ok(Principal::anonymous()) };
        authenticate(state, &raw).await
```

- [ ] **Step 4: Implement the middleware**

In `src/ratelimit.rs`, extend the imports:

```rust
use crate::error::AppError;
use crate::state::AppState;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;
use std::sync::Arc;
```

Add before `#[cfg(test)]`:

```rust
// --------------------------------------------------------------------------- middleware

/// Who is calling, resolved once per request and stored in its extensions. The `Principal`
/// extractor returns `principal` rather than authenticating again, so a route's own `401` —
/// the MCP endpoint's `WWW-Authenticate` challenge among them — is exactly what it was.
#[derive(Debug, Clone)]
pub struct ClientContext {
    pub ip: IpAddr,
    pub principal: Result<Principal, AppError>,
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
                        if let Some(wait) = limits.auth_blocked(ip) {
                            limits.note_auth_fail_rejection();
                            let detail = "authentication refused for this address: too many failed credentials";
                            return too_many(wait, detail.into());
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
            set_ratelimit_headers(resp.headers_mut(), class, limit, 0);
            resp
        }
        Decision::Allowed { limit, remaining } => {
            let mut resp = next.run(req).await;
            set_ratelimit_headers(resp.headers_mut(), class, limit, remaining);
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
fn set_ratelimit_headers(headers: &mut HeaderMap, class: Class, limit: Limit, remaining: u32) {
    let spent = u64::from(limit.burst.get().saturating_sub(remaining));
    let reset = (spent * 60).div_ceil(u64::from(limit.per_minute.get()));
    let name = class.name();
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
```

- [ ] **Step 5: Mount it, and give the server the peer address**

In `src/api/mod.rs` `router()`, directly after the `.layer(axum::middleware::from_fn_with_state(state.clone(), require_read_access))` line, add:

```rust
        // Outside `require_read_access`, so a closed registry's refusals are rate-limited too.
        .layer(axum::middleware::from_fn_with_state(state.clone(), crate::ratelimit::middleware))
```

In `src/main.rs`:
- next to the other background `tokio::spawn`s, add:

```rust
    if state.config.rate_limit.enabled {
        tokio::spawn(tar::ratelimit::retain_loop(state.clone()));
    }
```

- change the serve line to hand the socket address to the router:

```rust
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
```

- [ ] **Step 6: Run the new tests to verify they pass**

Run: `cargo test --test ratelimit`
Expected: 6 tests PASS.

- [ ] **Step 7: Run the whole suite**

Run: `cargo test 2>&1 | grep -E '^test result|FAILED|panicked'`
Expected: every result ok. The existing suites run through `resolve_client` now, so a failure here means the extractor's reuse changed an authentication outcome — fix that, do not loosen the test.

- [ ] **Step 8: Run it for real once**

```bash
TAR_BASE_IRI=http://127.0.0.1:18080 TAR_LISTEN=127.0.0.1:18080 TAR_DATA_DIR=memory \
  TAR_ROOT_TOKEN=$(openssl rand -hex 24) TAR_RATE_LIMIT_SPARQL=2:2/off cargo run -q -- serve &
sleep 3
for i in 1 2 3; do curl -s -o /dev/null -w '%{http_code} ' 'http://127.0.0.1:18080/sparql?query=ASK%7B%7D'; done; echo
curl -si 'http://127.0.0.1:18080/sparql?query=ASK%7B%7D' | grep -iE '^(HTTP|retry-after|ratelimit)'
kill %1
```

Expected: `200 200 429`, then a `429` with `Retry-After` and `RateLimit` headers. (If `TAR_DATA_DIR=memory` is not accepted by `serve`, use a temp dir.)

- [ ] **Step 9: Format and commit**

```bash
cargo fmt
git add src/ratelimit.rs src/auth/mod.rs src/api/mod.rs src/main.rs tests/ratelimit.rs
git commit -F - <<'EOF'
Refuse a client past its limit with 429, keyed on who it really is

A middleware resolves the client once per request - the socket address,
or the forwarded one behind a trusted proxy, and the principal from a
single authenticate() - and charges the request's class to that
principal, or to the address when anonymous or when the credential
failed. The Principal extractor reuses the stored result, so each
route's own 401 is unchanged. Admins are never limited; probes never are.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_014kEAfhgTfX5ocg392QFViY
EOF
```

---

### Task 4: MCP — charge a tool call's inner requests to its caller

**Files:**
- Modify: `src/ratelimit.rs` (task-local `FORWARDED`)
- Modify: `src/mcp/transport.rs` (reuse `ClientContext`; scope the tool call)
- Modify: `src/mcp/call.rs` (`rest()` attaches the forwarded context)
- Modify: `tests/ratelimit.rs`

**Interfaces:**
- Consumes: Task 3 `ClientContext`, and the middleware's "existing extension is used as-is" behaviour.
- Produces: `pub static tar::ratelimit::FORWARDED: tokio::task::LocalKey<ClientContext>`.

- [ ] **Step 1: Write the failing test**

Append to `tests/ratelimit.rs`:

```rust
async fn mcp_call(h: &Harness, from: &str, name: &str, arguments: Value) -> (StatusCode, Value) {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } });
    let headers = [
        ("accept", "application/json, text/event-stream"),
        ("mcp-protocol-version", V),
        ("mcp-method", "tools/call"),
        ("mcp-name", name),
    ];
    let (s, v, _) = h.send(from, "POST", "/mcp", None, Some(body), &headers).await;
    (s, v)
}

/// Review focus: a refused inner request is a tool error inside a `200`, not a transport failure.
#[tokio::test]
async fn a_tool_call_spends_the_bucket_of_the_request_it_makes() {
    let h = harness(|rl| rl.set(Class::Read, only(Limit::new(1, 2)))).await;
    let from = "198.51.100.7";
    for i in 0..2 {
        let (s, body) = mcp_call(&h, from, "list_records", json!({ "kind": "software" })).await;
        assert_eq!(s, StatusCode::OK, "call {i}: {body}");
        assert_eq!(body["result"]["isError"], false, "call {i}: {body}");
    }
    // The two inner reads were charged to this address, so a direct read is refused...
    assert_eq!(h.get(from, "/api/v1/software").await, TOO_MANY);
    // ...and a third tool call's read comes back as a tool error, not a transport one.
    let (s, body) = mcp_call(&h, from, "list_records", json!({ "kind": "software" })).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["isError"], true, "{body}");
    // Another address is unaffected by any of it.
    assert_eq!(h.get("198.51.100.8", "/api/v1/software").await, StatusCode::OK);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test ratelimit a_tool_call_spends`
Expected: FAIL at the direct-read assertion (`left: 200, right: 429`) — inner requests have no `ConnectInfo`, so today they are all charged to `0.0.0.0`.

- [ ] **Step 3: Add the task-local**

In `src/ratelimit.rs`, after the `ClientContext` definition:

```rust
tokio::task_local! {
    /// The outer request's client while the MCP server dispatches a tool call through the
    /// router in-process. `mcp::call::rest` copies it into each inner request, so a SPARQL query
    /// made by a tool is charged as `sparql` to the caller, not to nobody. A task-local rather
    /// than a parameter because `rest` has twenty callers and none of them should have to care.
    pub static FORWARDED: ClientContext;
}
```

- [ ] **Step 4: Forward it from `rest()`**

In `src/mcp/call.rs` `rest()`, replace:

```rust
    let Ok(req) = req else {
        return (StatusCode::INTERNAL_SERVER_ERROR, json!({ "detail": "could not build the internal request" }));
    };
```

with:

```rust
    let Ok(mut req) = req else {
        return (StatusCode::INTERNAL_SERVER_ERROR, json!({ "detail": "could not build the internal request" }));
    };
    // Charge this request to the tool call's caller (see `ratelimit::FORWARDED`). Extensions
    // cannot be set over HTTP, so nobody outside the process can claim someone else's context.
    if let Ok(ctx) = crate::ratelimit::FORWARDED.try_with(Clone::clone) {
        req.extensions_mut().insert(ctx);
    }
```

- [ ] **Step 5: Reuse the context in the transport, and scope the call**

In `src/mcp/transport.rs`:

Change the handler signature to also take the context:

```rust
pub async fn endpoint(
    State(state): State<Arc<AppState>>,
    ctx: Option<axum::Extension<crate::ratelimit::ClientContext>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = ctx.map(|axum::Extension(c)| c);
```

Replace `Some(t) => match crate::auth::authenticate(&state, t).await {` with:

```rust
                Some(t) => match authenticate_once(&state, ctx.as_ref(), t).await {
```

and add this function at the bottom of the file:

```rust
/// The middleware has already authenticated this request to know whom to charge; reuse its
/// answer. Without it (a router built without the layer), authenticate here as before.
async fn authenticate_once(
    state: &Arc<AppState>,
    ctx: Option<&crate::ratelimit::ClientContext>,
    token: &str,
) -> crate::error::AppResult<Principal> {
    match ctx {
        Some(c) => c.principal.clone(),
        None => crate::auth::authenticate(state, token).await,
    }
}
```

Replace the tool-call line:

```rust
            let outcome = super::call::call(&state, &principal, raw_auth.as_deref(), name, &args, cfg.read_only).await;
```

with:

```rust
            let run = super::call::call(&state, &principal, raw_auth.as_deref(), name, &args, cfg.read_only);
            let outcome = match ctx.clone() {
                Some(c) => crate::ratelimit::FORWARDED.scope(c, run).await,
                None => run.await,
            };
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --test ratelimit && cargo test --test mcp`
Expected: 7 and all MCP tests PASS. If `isError` is `false` on the third call, read how `list_records` maps a non-2xx inner status to an `Outcome` in `src/mcp/call.rs` — a 429 must reach the caller as an error with the problem's `detail`; fix the mapping there if it swallows it.

- [ ] **Step 7: Whole suite, clippy, format, commit**

Run: `cargo test 2>&1 | grep -E '^test result|FAILED'` — all ok.
Run: `cargo clippy --all-targets 2>&1 | grep -A5 ratelimit` — no new warnings in files this plan touched.

```bash
cargo fmt
git add src/ratelimit.rs src/mcp/transport.rs src/mcp/call.rs tests/ratelimit.rs
git commit -F - <<'EOF'
Charge a tool call's inner requests to the client that made it

The MCP server dispatches each tool through the router in-process, where
there is no socket address, so every inner request was charged to
0.0.0.0. The caller's context now travels with the call through a
task-local, the transport reuses the middleware's authentication, and a
SPARQL query made by a tool costs sparql to its caller.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_014kEAfhgTfX5ocg392QFViY
EOF
```

---

### Task 5: Documentation

**Files:**
- Modify: `docs/operations/configuration.md` (new `## Rate limits` section after `## Read access`)
- Modify: `docs/operations/deployment.md` (`## Configuration that matters in production`, and `### The ingress`)
- Modify: `docs/api/conventions.md` (new `## Rate limits` section after `## Request size`)
- Modify: `deploy/kubernetes/deployment.yaml` (commented `TAR_TRUSTED_PROXIES`)
- Modify: `docs/specs/2026-08-30-tool-artifact-registry-design.md` (Q8 row)
- Modify: `docs/specs/2026-09-23-rate-limiting.md` (Status)
- Modify: `README.md` (Layout: `ratelimit.rs`)

- [ ] **Step 1: Operator docs**

`docs/operations/configuration.md`, new section after `## Read access` (match the file's table style):

```markdown
## Rate limits

On by default. A request is charged by cost class to its verified principal, or to its client
address when it is anonymous or its credential failed. Admins are never limited; `/healthz`,
`/readyz`, `/metrics` and `/assets/*` never are. The design is in
[Rate limiting](../specs/2026-09-23-rate-limiting.md).

| Variable | Default | |
|---|---|---|
| `TAR_RATE_LIMIT_ENABLED` | `true` | `false` removes every limit. |
| `TAR_TRUSTED_PROXIES` | — | Comma-separated CIDRs. **Set this behind any reverse proxy or ingress**, or every request is charged to the proxy's address. |
| `TAR_RATE_LIMIT_READ` | `300:60/1200:200` | `<anonymous>/<authenticated>`, each `rate:burst` per minute, or `off`. |
| `TAR_RATE_LIMIT_WRITE` | `60:20/300:60` | |
| `TAR_RATE_LIMIT_SPARQL` | `30:10/120:30` | `/sparql`. |
| `TAR_RATE_LIMIT_FEDERATED` | `10:5/60:10` | A federated search, or a peer's relayed leg of one. |
| `TAR_RATE_LIMIT_MCP` | `120:30/600:100` | The `/mcp` request itself; the requests a tool makes are charged to their own classes. |
| `TAR_RATE_LIMIT_OUTBOUND` | `10:5/60:10` | Fetching a record's API document, and repository sync. |
| `TAR_RATE_LIMIT_AUTH_FAIL` | `20:10` | Failed credentials per address; past it, the address is refused authentication until the next attempt would be allowed. |

A malformed value stops the registry at boot and names the variable. `tar config` prints the
effective limits. Refusals are counted in `/metrics` as `tar_ratelimit_rejections_total{class}`.
```

`docs/operations/deployment.md`: in `### The ingress`, add a paragraph saying the ingress controller's pod network must be listed in `TAR_TRUSTED_PROXIES`, or every client shares the proxy's bucket and the first busy minute locks out everyone; in `## Configuration that matters in production`, add `TAR_TRUSTED_PROXIES` with the same one-line reason.

`deploy/kubernetes/deployment.yaml`: in the container `env:` list, add a commented entry in the same style as its neighbours:

```yaml
            # The ingress controller's pod network, so rate limits see the real client rather
            # than the proxy. Find it with `kubectl get pods -n <ingress-ns> -o wide`.
            # - name: TAR_TRUSTED_PROXIES
            #   value: "10.42.0.0/16"
```

- [ ] **Step 2: Client docs**

`docs/api/conventions.md`, new section after `## Request size`:

```markdown
## Rate limits

A client over its limit gets `429 Too Many Requests` as `application/problem+json`, with
`Retry-After` in seconds and a `detail` naming the limit that applied. Wait that long and retry;
retrying sooner is refused again.

Every limited response, accepted or not, carries `RateLimit-Policy` (the quota per 60-second
window) and `RateLimit` (`r`, what is left; `t`, seconds until it is fully replenished), in the
form of the IETF `draft-ietf-httpapi-ratelimit-headers`, so a client can pace itself instead of
finding the limit by hitting it.

Authenticated requests are charged to the credential, not the address, and have higher limits;
a batch job should authenticate even for reads. Repeatedly presenting a bad credential gets an
address refused authentication for a while — check the credential rather than retrying it.
```

- [ ] **Step 3: Close the loop in the design record**

In `docs/specs/2026-08-30-tool-artifact-registry-design.md`, replace the Q8 row's notes with:
`**Answered: in-registry limits by cost class**, keyed on the verified principal or the real client address — see [Rate limiting](2026-09-23-rate-limiting.md).`

In `docs/specs/2026-09-23-rate-limiting.md`, change `| **Status** | Approved, not yet implemented |` to `| **Status** | Implemented |`.

In `README.md`'s Layout block, add under `health.rs`:

```
  ratelimit.rs  rate limits by cost class, and finding the real client behind a proxy
```

- [ ] **Step 4: Build the docs and commit**

Run: `mdbook build docs 2>&1 | tail -3` (skip if mdbook is not installed; say so).
Expected: no broken-link errors.

```bash
git add docs README.md deploy/kubernetes/deployment.yaml
git commit -F - <<'EOF'
Document rate limits for operators and for clients

Operators need to know TAR_TRUSTED_PROXIES exists before the first busy
minute behind an ingress; clients need to know what a 429 and the
RateLimit headers mean. Spec Q8 is marked answered.

Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_014kEAfhgTfX5ocg392QFViY
EOF
```
