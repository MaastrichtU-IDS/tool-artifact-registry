# Rate Limiting — Design Note

| | |
|---|---|
| **Status** | Approved, not yet implemented |
| **Date** | 2026-09-23 |
| **Spec** | [`2026-08-30-tool-artifact-registry-design.md`](2026-08-30-tool-artifact-registry-design.md) — answers Q8 |
| **Code** | `src/ratelimit.rs`, `src/auth/mod.rs`, `src/api/mod.rs`, `src/mcp/call.rs`, `src/main.rs`, `tests/ratelimit.rs` |

---

## 1. Why

Q8 said it plainly: rate limiting was not designed, and it was needed before any registry is
exposed to the open internet with `TAR_PUBLIC_READ=true`. That is the default.

The registry has four surfaces where one request costs far more than one request:

- **`/sparql`** runs a query the caller wrote. `TAR_SPARQL_TIMEOUT` bounds one query, not a
  thousand of them.
- **Federated search** fans out. One `federated=true` request becomes up to
  `TAR_FEDERATED_SEARCH_MAX_PEERS` outbound requests, each of which may fan out again.
- **The hosted MCP server** turns one JSON-RPC call into one or more internal API calls.
- **Outbound fetches** — `/software/{id}/api-doc` and `/software/{id}/sync` — make the registry
  contact a third party on the caller's behalf.

Everything else is cheap per request and expensive only in volume: scraping, and guessing
credentials.

Limiting belongs **in the registry**, not only at the ingress. A self-hoster running the
published image with a bare `docker run` has no ingress, and an ingress cannot tell an anonymous
caller from a curator, or a `GET /api/v1/software` from a `GET /sparql`. An ingress limit can
still sit in front; the two compose.

---

## 2. Classes

A request is classified by method, path and query into exactly one class:

| Class | Matches |
|---|---|
| `sparql` | `/sparql` (GET and POST) |
| `federated` | `/api/v1/search` with `federated=true` or a `fed_id` (a relayed leg) |
| `mcp` | the hosted MCP route |
| `outbound` | `/api/v1/software/{id}/api-doc`, `/api/v1/software/{id}/sync` |
| `write` | any other non-`GET`/`HEAD` request, except `POST /api/v1/artifacts/identify`, which is a read |
| `read` | everything else |

**Exempt**, always: `/healthz`, `/readyz`, `/metrics` (probes and scrapers must not be starved by
the traffic they exist to observe), and static SPA assets served by the fallback.

A further bucket, `auth_fail`, is not a class a request is sorted into. It counts failed
authentications per client IP (§4).

---

## 3. Keys and default limits

A request is charged to its **verified principal** if it authenticated, and to its **client IP**
otherwise — including when it presented a credential that failed. Keying on the raw
`Authorization` header would hand an attacker a fresh bucket per random string.

Limits are GCRA (a smoothed token bucket): a sustained rate plus a burst.

| Class | Anonymous, per IP | Authenticated, per principal |
|---|---|---|
| `read` | 300/min, burst 60 | 1200/min, burst 200 |
| `write` | 60/min, burst 20 — a write needs a credential, so this only bounds refused attempts | 300/min, burst 60 |
| `sparql` | 30/min, burst 10 | 120/min, burst 30 |
| `federated` | 10/min, burst 5 | 60/min, burst 10 |
| `mcp` | 120/min, burst 30 | 600/min, burst 100 |
| `outbound` | 10/min, burst 5 | 60/min, burst 10 |
| `auth_fail` | 20/min, burst 10, per IP | — |

`write` for authenticated callers is generous because CI pipelines advertise in bursts.

**Root and admin principals are exempt.** An operator must never be locked out of their own
registry during the incident the limits exist for.

**Peers.** A peer relaying a federated leg is keyed by its IP under `federated`, like any other
caller. The hop budget and the repeated-query refusal in the federated-search design already
bound loops; the limit bounds volume.

IPv6 clients are bucketed per **/64**: one host usually controls a whole /64, and per-address
buckets would give it 2⁶⁴ of them.

---

## 4. Finding the client, and who it is

### 4.1 Client IP

The server is started with `into_make_service_with_connect_info::<SocketAddr>()`, so the socket
peer is known. Today it is not known at all.

`TAR_TRUSTED_PROXIES` is a comma-separated list of CIDRs, unset by default.

- Peer **not** in the list: the peer is the client. `X-Forwarded-For` is ignored — otherwise any
  caller picks its own bucket.
- Peer in the list: walk `X-Forwarded-For` from the right, skipping addresses that are
  themselves trusted. The first untrusted address is the client. If every entry is trusted, the
  leftmost is.

CIDR matching is written by hand over `std::net::IpAddr` (about thirty lines) rather than taken
as a dependency.

### 4.2 Authenticate once

The `Principal` extractor currently authenticates inside each handler, so middleware does not
know who is calling. A new middleware, `resolve_client`, runs before the limiter:

1. Determine the client IP (§4.1).
2. If a bearer credential is present **and** the IP's `auth_fail` bucket has capacity, call
   `authenticate()` once. If the bucket is exhausted, answer `429` without calling it.
3. A failed `authenticate()` spends one `auth_fail` token.
4. Store `ClientContext { ip, principal: Result<Principal, AppError> }` in the request's
   extensions.

Nothing is **rejected** here for a bad credential. The `Principal` extractor reads the stored
result and returns it exactly as it would have computed it, so each route keeps its own `401`
body and challenge — the MCP route's `WWW-Authenticate: Bearer resource_metadata=…` among them.
If no `ClientContext` is present (a router built without the layer, as some tests do), the
extractor authenticates as it does today.

### 4.3 MCP's internal dispatch

MCP tools dispatch through `api::router(...).oneshot(req)` in-process (`src/mcp/call.rs`). Those
inner requests pass through the same middleware and have no socket address.

`call.rs` copies the outer request's `ClientContext` into each inner request's extensions. The
middleware then uses it rather than resolving again, and charges the inner request under **its
own class** to the same key — a SPARQL query issued through MCP costs `sparql`, not merely
`mcp`. The outer `/mcp` request is charged under `mcp`. Extensions cannot be set over HTTP, so
the copy cannot be forged by a caller.

---

## 5. Responses

A request over its limit gets `429 Too Many Requests` as `application/problem+json`, through a
new `AppError::too_many_requests`, with:

- `Retry-After` in whole seconds, from the limiter's earliest-possible time;
- a `detail` naming the class and the limit that applied, e.g. *"sparql limit for anonymous
  clients: 30/min, burst 10"*.

Every limited response — accepted or refused — carries `RateLimit-Policy` and `RateLimit` headers
in the IETF `draft-ietf-httpapi-ratelimit-headers` form, so a well-behaved client can pace itself
instead of discovering the limit by hitting it.

`/metrics` gains `tar_ratelimit_rejections_total{class="…"}`.

---

## 6. State

In memory, one `governor::DefaultKeyedRateLimiter` per (class, anonymous/authenticated). The
registry runs as one replica — Oxigraph is single-writer — so there is no shared store to
coordinate through, and a restart resetting the counters is harmless.

A background task calls `retain_recent()` on every limiter each 60 seconds, so the key maps stay
bounded under IP churn.

---

## 7. Configuration

| Variable | Default | Notes |
|---|---|---|
| `TAR_RATE_LIMIT_ENABLED` | `true` | `false` removes the limiter entirely. `resolve_client` still runs. |
| `TAR_TRUSTED_PROXIES` | unset | Comma-separated CIDRs. Required behind Traefik, nginx or an ingress, or every request is keyed to the proxy. |
| `TAR_RATE_LIMIT_<CLASS>` | table in §3 | `<anon>/<authed>`, each `rate:burst` per minute. E.g. `TAR_RATE_LIMIT_SPARQL=30:10/120:30`. A side may be `off`. Classes: `READ`, `WRITE`, `SPARQL`, `FEDERATED`, `MCP`, `OUTBOUND`; `AUTH_FAIL` takes one side only. |

A malformed value fails at boot, like every other setting. `tar config` prints the effective
limits.

---

## 8. Tests

**Unit** (`src/ratelimit.rs`): the classification table; the `X-Forwarded-For` walk — untrusted
peer with a spoofed header, trusted chain, all-trusted chain, IPv6 /64 bucketing; configuration
parsing, including rejection of malformed values.

**End-to-end** (`tests/ratelimit.rs`, against the real router):

- an anonymous SPARQL burst reaches `429` with `Retry-After`, while a second IP still succeeds;
- an authenticated principal has its own bucket, separate from its IP's;
- the root token is never limited;
- random bearer strings from one IP reach `429` via `auth_fail`, and `authenticate()` is not
  called once it is exhausted;
- `/healthz` is never limited;
- a SPARQL query issued through MCP spends the `sparql` bucket.

`Config::for_test` disables the limiter, so the existing suites' assertions do not change — but
`resolve_client` still runs in every one of them, so the authenticate-once path is exercised by
all 292 existing tests, not only by the new ones.

---

## 9. Consequences

- **A bearer token on a read is now always verified.** Before, a handler that never asked for a
  `Principal` never authenticated. A registry token's verification is an Argon2 check, so a
  signed-in UI browsing lists now pays it on requests that used to skip it. That is the price
  of keying on who is calling; `auth_fail` bounds what a stranger can make it cost.
- **State lives on `AppState`, not the router.** The MCP server rebuilds the router per tool call
  and the tests build one per harness; limiters held by the router would reset each time.

## 10. Not done, deliberately

- **Concurrency caps** on expensive queries. A rate bounds volume over time, not how many run at
  once; `TAR_SPARQL_TIMEOUT` bounds each. Worth adding if a real deployment shows a burst of
  slow queries hurting.
- **Shared state across replicas.** There is one replica.
- **Per-token configurable limits.** One authenticated tier is enough until someone needs two.
