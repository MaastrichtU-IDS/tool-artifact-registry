//! Fetching a URL that a record, rather than the operator, chose.
//!
//! One set of rules for every such fetch — webhook deliveries, the peer-stub resolver, the
//! API-description viewer, the health probe — so none of them can be pointed at the inside of
//! the network by writing a URL into a record:
//!
//! - **No redirects.** A public URL that answers 302 to `http://169.254.169.254/` would
//!   otherwise walk straight past a check made on the first address.
//! - **Checked addresses, pinned.** The name is resolved once, every address is checked, and
//!   the connection goes to exactly those addresses, so a short-TTL record cannot answer the
//!   check with a public address and the connection with a private one.
//! - **Link-local is always refused** — `169.254.0.0/16` and `fe80::/10`, where cloud metadata
//!   lives — even where private addresses are allowed on purpose.
//! - **Bodies are capped while reading**, not after: a response is never held whole before its
//!   size is known.

use once_cell::sync::Lazy;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Anything that is not routable on the public internet: loopback, RFC1918, link-local,
/// carrier-grade NAT, unspecified, multicast, reserved, and the IPv6 equivalents.
pub fn is_public_ip(ip: &IpAddr) -> bool {
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

/// Link-local, where cloud metadata services answer. Refused even when private addresses are
/// allowed: nothing a record points at legitimately lives there.
pub fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => {
            (v6.segments()[0] & 0xffc0) == 0xfe80 || v6.to_ipv4_mapped().is_some_and(|m| m.is_link_local())
        }
    }
}

/// Resolve `url`'s host and check every address. Returns the addresses to connect to.
pub async fn resolve_targets(url: &url::Url, allow_private: bool) -> Result<Vec<SocketAddr>, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{url} is not an http(s) URL"));
    }
    let host = url.host_str().ok_or_else(|| format!("{url} has no host"))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.trim_matches(['[', ']']), port))
        .await
        .map_err(|e| format!("could not resolve {host}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("{host} resolved to no addresses"));
    }
    for a in &addrs {
        let ip = a.ip();
        if is_link_local(&ip) || (!allow_private && !is_public_ip(&ip)) {
            return Err(format!("{host} resolves to {ip}, which is not a public address; refusing to connect"));
        }
    }
    Ok(addrs)
}

fn builder(timeout: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .user_agent(concat!("tool-artifact-registry/", env!("CARGO_PKG_VERSION")))
}

static PLAIN: Lazy<reqwest::Client> = Lazy::new(|| builder(Duration::from_secs(30)).build().unwrap_or_default());

/// A client that follows no redirects and connects to `addrs` and nowhere else. The hostname
/// is still what TLS verifies against: pinning replaces DNS, not the identity check.
pub fn pinned_client(url: &url::Url, addrs: &[SocketAddr], timeout: Duration) -> Result<reqwest::Client, String> {
    let host = url.host_str().ok_or_else(|| format!("{url} has no host"))?;
    // An IP literal resolves to itself; there is no name to rebind.
    if addrs.is_empty() || url.host().is_some_and(|h| matches!(h, url::Host::Ipv4(_) | url::Host::Ipv6(_))) {
        return builder(timeout).build().map_err(|e| e.to_string());
    }
    builder(timeout)
        .resolve_to_addrs(host.trim_matches(['[', ']']), addrs)
        .build()
        .map_err(|e| format!("could not build a pinned HTTP client for {host}: {e}"))
}

/// A client for one URL, with its addresses checked and pinned.
pub async fn client_for(url: &url::Url, allow_private: bool, timeout: Duration) -> Result<reqwest::Client, String> {
    let addrs = resolve_targets(url, allow_private).await?;
    pinned_client(url, &addrs, timeout)
}

/// A client that follows no redirects, for the few callers that check addresses themselves.
pub fn plain_client() -> reqwest::Client {
    PLAIN.clone()
}

/// Read a response body, stopping as soon as it passes `cap` bytes.
pub async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    if let Some(len) = resp.content_length() {
        if len > cap as u64 {
            return Err(format!("the response is {len} bytes, over the {cap}-byte limit"));
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > cap {
            return Err(format!("the response exceeds the {cap}-byte limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn metadata_addresses_are_link_local_in_every_spelling() {
        for a in ["169.254.169.254", "fe80::1", "::ffff:169.254.169.254"] {
            assert!(is_link_local(&ip(a)), "{a}");
            assert!(!is_public_ip(&ip(a)), "{a}");
        }
        for a in ["10.0.0.1", "8.8.8.8", "::1"] {
            assert!(!is_link_local(&ip(a)), "{a}");
        }
    }

    #[tokio::test]
    async fn private_targets_are_refused_unless_allowed_and_link_local_always() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(resolve_targets(&u("http://127.0.0.1:9/x"), false).await.is_err());
        assert!(resolve_targets(&u("http://127.0.0.1:9/x"), true).await.is_ok());
        assert!(resolve_targets(&u("http://169.254.169.254/latest"), true).await.is_err());
        assert!(resolve_targets(&u("http://[::ffff:169.254.169.254]/"), true).await.is_err());
        assert!(resolve_targets(&u("file:///etc/passwd"), true).await.is_err());
    }
}
