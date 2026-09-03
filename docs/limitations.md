# Limitations

Where the prototype departs from its design, or stops short of it. Written down rather than
discovered.

## 1. Write validation sees the candidate record, not the whole graph

SHACL validation runs on the record being written, before it is committed. Constraints that
would need the rest of the store — `sh:class` on a referenced node, say — are therefore not
evaluated. That is the price of validating before committing rather than after.

Validation itself is real SHACL: `shapes/tar-shapes.ttl` is enforced by a SHACL engine, and
editing that file changes what the API accepts with no Rust change. Severity decides:
`sh:Violation` blocks, `sh:Warning` never does, and `TAR_SHACL_VALIDATE_WRITES=false` downgrades
violations to warnings.

Two rules that *do* need the rest of the graph live in Rust for exactly this reason, and report
through the same `sh:ValidationReport`: whether an artifact type is a term the registry holds,
and whether a deployment of non-deployable software may carry an endpoint. Neither honours
`TAR_SHACL_VALIDATE_WRITES` — a half-described record is a trade an operator can make, an
unlookuppable type is not.

The first of those is exactly `sh:class`, and it was measured rather than assumed. Injecting the
bundles' class assertions into the candidate data graph so the engine can see them takes
validation from **1.9 ms to 9.8 ms** per write, plus **2.8 ms** to read them back, against the
**52 µs** the targeted lookup costs — roughly 2 ms a write against 12.6 ms.

The time is the smaller objection. Three things a shape cannot do at all settled it:
`sh:message` is fixed text and could not name the offending IRI or the way out;
`TAR_SHACL_VALIDATE_WRITES=false` would switch the rule off; and the allowance for a type cached
from a peer is about which named graph a concept sits in, which the engine's single data graph
cannot express.

## 2. Distributions and capabilities are IRIs, not blank nodes

The design shows blank nodes. They are minted as IRIs instead, which makes them addressable and
citable. A record still reads as one document, because `describe` returns them with their parent.

## 3. Two denormalised links

`tar:instanceOf` and `tar:atInstance` sit alongside the authoritative
`prov:qualifiedAssociation`. Every list and count query would otherwise be a two-hop join
through a reified node.

## 4. Repository liveness metrics are not implemented

Repository *sync* is — a record can keep named fields in step with its source repository. What
is missing is the liveness signal the design asked for: stars, forks, last commit. The UI
degrades by omitting those cells rather than rendering zeros.

## 5. ~~Human sign-in has no token refresh~~ — closed

Sign-in itself is verified against a live identity provider: a browser was driven through
authorisation code + PKCE, and the `curator` and `admin` roles were confirmed to decide what a
signed-in person may do — `POST /api/v1/software` succeeds for a curator, `/api/v1/peers` is
`403` for a curator and `200` for an admin.

The gap was **refresh**: the access token was used until it expired with no silent renewal, so a
long editing session ended in a `401` and a re-sign-in with the form's contents lost. The
registry now renews twice over — from a timer set from the token's own `exp` claim, ahead of
expiry, and reactively when a request comes back `401` for the case the timer missed (a laptop
that slept through it). Concurrent requests that all expire together share one renewal rather
than each spending the refresh token, which matters once a provider rotates it: the first use
would otherwise invalidate it for the rest.

The refresh token itself stays exactly where the design note said it should not go: in memory
only, never `sessionStorage`, `localStorage`, or a cookie. A closed tab or a reload ends the
renewable session — the access token in `sessionStorage` keeps working until it expires, then
sign-in is needed again — which is the trade this file has always made, kept rather than
loosened to get renewal.

A registry API token has no refresh token to renew with, so nothing changes for it: a `401`
still means sign in again, exactly as before.

## 6. Federated search is not deduplicated

Fan-out is live, and results are not deduplicated across peers beyond the origin chip. Two peers
that both cache a stub of the same third-party record produce two rows.

## 7. Peer resolution caches more than it needs to

The resolver fetches a whole Turtle document into the peer graph rather than extracting a
minimal stub, so a verbose peer can cache considerably more than the type, title, publisher and
home registry the design called for.

## 8. Keyset pagination orders by IRI string

Which is time-ordered within one registry, because identifiers are UUIDv7. Across origins it
interleaves imperfectly.

## 9. No lineage graph visualisation

Deferred. `GET /api/v1/graph` and `GET /api/v1/artifacts/{id}/lineage` already return exactly
what a graph view would need.

## 10. A shapes or vocabulary change can strand an existing record

A write is judged on the whole record it asserts, and a `PATCH` carries the fields the caller did
not name — so a record citing a term the registry has since retired is refused on an edit to some
other field entirely.

The boot log names every such record and the term, once, rather than deleting a value nobody
asked it to delete. Replacing or clearing the named term fixes the record permanently. Nothing
shipped is affected; a long-lived store might be.

## 11. A type resolved from a peer is accepted but not offered

It is a term this registry holds, so citing it is fine. It carries none of this registry's own
classes, so the picker does not list it.

That is the conservative half-answer. Which registry owns a term, and whether adopting a peer's
implies agreeing with it, is a federation question this prototype does not settle. The picker's
adopt flow is how to make such a term first-class here.

## 12. Observed capability is not reconciled with declared capability

The software page shows what was declared. What a deployment has actually produced is one SPARQL
query away, and the two can disagree. Surfacing the disagreement should wait until there is
enough run data for it to mean something.

## 13. ~~A residual DNS-rebinding race on webhook targets~~ — closed

A subscription's webhook URL is checked against private and loopback addresses at registration
*and* re-resolved at send time. The send-time check used to return only a verdict, leaving the
HTTP client to resolve the name a second time — so a record with a short TTL could answer the
check with a public address and the connection with `169.254.169.254`.

The check now returns the addresses it approved and the delivery is pinned to them, so there is
no second lookup to win. TLS still verifies the certificate against the hostname: pinning
replaces DNS, not identity.

## 14. Subscriptions have no scope of their own

Managing a subscription reuses the rule that governs token management — admin, curator, or the
credential of the owning deployment. There is no `subscribe:*` scope, so a credential cannot be
issued that may subscribe and nothing else.

## 15. ~~The external SPARQL backend blocks a worker thread per store call~~ — closed for the request path

`GraphStore` is a synchronous trait — it predates the second backend, and every read in the
registry is a plain function call because the store used to be in-process. Against
`TAR_SPARQL_ENDPOINT`, calling one of its methods directly from an async handler occupied that
handler's Tokio worker thread for the whole HTTP round trip: on a multi-threaded runtime with
more concurrent requests than worker threads, one slow remote query delayed every other request
the same worker was about to advance.

Making the trait itself `async fn` — the fix this entry used to name — turned out to be far
larger than it reads: not 119 call sites, but recolouring most of `src/domain`, most of
`src/api`, and `src/auth/jwt`, converting every synchronous store-layer unit test to
`#[tokio::test]`, and rewriting iterator patterns that call domain functions into `stream`
combinators. The change actually made is the one Tokio's own documentation recommends for
exactly this shape of problem: every request-path call site — all ~90 API handlers across 18
files, and the three OIDC credential-binding lookups in `auth::jwt` that run on every
bearer-token request before any handler is reached — passes its synchronous, store-touching
work to [`error::blocking`], which runs it on Tokio's dedicated blocking thread pool and gives
the worker thread back to the scheduler for the round trip's duration. `Ctx` moved from
borrowing `&AppState` to owning an `Arc<AppState>` clone so it can cross that boundary. Nothing
in `src/domain` or `src/store` changed: the same synchronous functions run, just on a different
thread.

Two call sites were deliberately left as they were, both flagged in code comments: the
per-artifact and per-`was_derived_from`-parent existence checks inside `advertise()`'s main
loop, and the equivalent one in the OpenLineage adapter's `map_dataset`. Both interleave a
synchronous `exists()` check with `state.ops.*` calls that are already async, inside a loop that
decides its own control flow per iteration — cleanly separating the two would mean restructuring
the loop into two passes, which is a larger and more error-prone change than the check's actual
cost justifies: it is a single fast existence lookup, not a scan.

Also unwrapped, and deliberately: `seed.rs`, `bundles.rs::sync`, and the peer-resolver and
health-check background loops. Those run once at boot or pace themselves on their own timer,
never competing with a flood of concurrent user requests for the same worker pool, which is the
specific failure this fix closes.

Embedded Oxigraph, the default, is unaffected either way: those calls never leave the process,
so moving them to a different thread pool costs a small fixed dispatch overhead and buys
nothing. The behavioural difference only shows up against a remote endpoint under real
concurrent load.

## 16. Atomicity on an external endpoint is the server's promise, not ours

A registry write is built into a single SPARQL Update request so that its deletions and
insertions are one unit. Fuseki, GraphDB, Virtuoso and Oxigraph's own server execute a request in
one transaction, which is what makes this equivalent to the embedded backend's transaction.

SPARQL 1.1 does not *require* that. Against a server that processes the operations of a request
independently, a write is atomic only per operation, and the registry has no way to tell over
HTTP which kind of server it is talking to. It does not attempt to detect it and does not claim
the guarantee it cannot verify.

## 17. Two rough edges in the external backend

Neither affects the embedded default.

**A subject deletion follows the ownership closure four levels deep**, where the embedded store's
walk is unbounded. A record's sub-resources — its distributions, its checksums — are removed with
it so that replacing a record does not orphan them, and doing that inside one atomic update
request means a fixed-depth pattern rather than a walk that can look at what it found. The
deepest nesting the registry writes is two levels, so this is headroom; a sub-resource nested
deeper than four would be orphaned against an external endpoint and removed against the embedded
one.

**`/admin/dump` and `tar dump` materialise the whole graph in memory** on the external backend,
because there is no streaming path from a `SELECT` result to the response body. Embedded
Oxigraph streams. For a catalogue-sized registry this is fine and for a very large external
dataset it is not; back that up with the store's own tools instead.

## 18. `tar dump --graph` loses the graph name, on either backend

`tar dump` with no argument writes N-Quads and restores faithfully. `tar dump --graph <g>` writes
N-Triples — which is what `/admin/dump?graph=` and peer stub exchange want — so restoring *that*
file with `tar restore` puts its triples in the default graph, where nothing looks for them.
Pre-existing behaviour, unchanged by the second backend, and worth knowing before using a
single-graph dump as a backup.
