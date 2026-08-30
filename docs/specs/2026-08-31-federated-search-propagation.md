# Federated search propagation

| | |
|---|---|
| **Status** | Implemented |
| **Date** | 2026-08-31 |
| **Extends** | [design §7.7, §7.8, §9.6](2026-08-30-tool-artifact-registry-design.md) — D9 (cross-link plus lazy resolve, opt-in peers) is unchanged |
| **Code** | `src/api/search.rs`, `src/ops/federation.rs`, `migrations/0002_federation.sql` |

## 1. What changed

`?federated=true` used to fan out exactly one hop: registry A asked the peers in A's own peer
list and merged their answers. A record at D — which only B peers with — was unreachable from
A no matter how the query was phrased.

Federated search now **propagates**. A asks B; B asks its peers; and so on to a hop budget.
That immediately raises the only hard problem in the design: in a peer graph with any cycle
(A↔B, B↔C, C↔A — the shape a real federation acquires within a week) naive propagation is an
exponential storm that never terminates.

Nothing about the trust model moves. Peers are still opt-in and admin-added, peer data is
still a read-only stub, and an announcement still only produces a suggestion (§8.4).
Propagation changes what a *query* may traverse, not what a registry will *trust*.

## 2. The envelope

Everything travelling with a federated search rides on the existing
`GET /api/v1/search` as query parameters. There is no new endpoint and no new verb; a peer
running the previous version answers a propagated query correctly as a one-hop local search,
and its narrower response deserialises with the new fields defaulted.

| Parameter | Meaning | Trust |
|---|---|---|
| `fed_id` | The query's identity, minted by the origin and carried **unchanged** across every hop. Seeing one twice is how a cycle is detected. | Validated: 1–100 chars of `[A-Za-z0-9._:-]`, else `400`. Never rewritten. |
| `fed_hops` | Hops still spendable. Decremented at each hop; `0` means answer locally. | **Clamped** to the receiver's own `max_hops`. |
| `fed_budget_ms` | Milliseconds the caller will wait. | **Clamped** to the receiver's own per-peer timeout. |
| `fed_origin` | Base IRI where the query started. | Advisory. Used for tracing and to avoid asking the origin back. Never authorises anything. |
| `fed_path` | Comma-separated base IRIs already on this query's path, receiver appended before forwarding. | Parsed defensively: trimmed, de-duplicated, entry- and list-length capped. |

`q`, `type` and `limit` are forwarded unchanged so every registry filters identically.

A request is treated as a leg of a federated query when it is marked `federated=true` **or**
carries a `fed_id`. A peer that sends only `fed_id` is asking for a local answer under that
id, and it is still deduplicated — it is the same query.

## 3. Three independent brakes

No single mechanism is trusted to stop the storm, because each fails differently.

**1 — Query identity (`federated_queries` in SQLite).** Every registry claims the id with a
single `INSERT OR IGNORE` *before doing any work*. SQLite serialises writers, so of any number
of concurrent arrivals of the same id exactly one wins and every other one learns it lost.
This is the brake that catches the case the path check cannot: a **diamond**, where two
different routes reach the same registry at the same time. Rows carry a TTL
(`TAR_FEDERATED_SEARCH_ID_TTL`, default 10 min — comfortably longer than any query lives) and
are swept on the write path, with a hard 50 000-row cap behind that, so the table cannot grow
without bound whatever a flood does.

**2 — Hop budget.** `fed_hops` decrements per hop and stops the walk at zero. Each registry
takes `min(granted, its own max)`, so a peer handing out a budget bigger than the one it was
given achieves nothing. `max_hops` itself is clamped to 8 however it is configured: a 40-hop
federation is not a configuration, it is an outage.

**3 — Path check.** A registry never forwards to a peer already on `fed_path`, to the origin,
or to itself. This is a pure optimisation over brake 1 — the id check would refuse those
anyway — but it saves the round trip and, more usefully, it *reports* the cut edge instead of
silently dropping it.

The path is attacker-controlled, so it cannot be the primary defence: a peer that strips it
would restore the storm. That is exactly why brake 1 is enforced independently at every
registry and does not depend on any claim made by the request.

## 4. The already-handled answer

> The user's requirement, verbatim in shape: a registry that sees an id it has already
> handled **rejects the repeat, saying so explicitly** rather than silently returning empty.

```json
{
  "query": "shacl-manager",
  "hits": [], "total": 0, "partial": false, "peers": [],
  "already_handled": true,
  "federation": {
    "query_id": "cycle-1",
    "registry": "https://reg.b.example",
    "origin": "https://reg.a.example",
    "first_seen_at": "2026-08-31T09:14:02.113Z",
    "reason": "https://reg.b.example already handled federated query cycle-1 at 2026-08-31T09:14:02.113Z; its results were returned on the path that arrived first. This is the first repeat, which means the peer graph contains a cycle."
  }
}
```

Design notes on that shape:

- **HTTP 200, not 4xx.** In a mesh with any cycle a repeat is the *expected*, correct
  outcome of a well-formed query, not a client error. Returning `409` would poison every
  caller's error rate and make a healthy federation look broken. The flag is the contract; the
  status code is not.
- **`hits: []` is explained, not implied.** Zero hits here does not mean "nothing matched" —
  it means "you already have these, on the path that reached me first" — and `reason` says
  so in words a human reading a peer report can act on.
- **`partial` stays `false`.** Coverage is complete: another path covered this subtree. A
  refused repeat is a healthy answer and must not be rendered as a failure.
- The caller records the peer as `status: "already_handled"` with the peer's own `reason` as
  a `note`, so the cut edge is visible in the topology report rather than vanishing.

## 5. Reporting the topology honestly

A hit that travelled two hops is not the same evidence as one from a peer the operator chose
to trust, so the response keeps them apart. `model::SearchResults`, `SearchHit` and
`PeerSearchStatus` are unchanged (owned elsewhere); `ops::federation` defines a **superset**
with the same field names, so every existing consumer keeps working.

Per hit (`#[serde(flatten)]` over `SearchHit`, so the old JSON shape is byte-for-byte intact):

| Field | Meaning |
|---|---|
| `reach` | `local` \| `direct` (a peer we configured) \| `indirect` (relayed to us) |
| `hops` | Registry-to-registry hops crossed. `0` = local, `1` = a peer of ours |
| `via` | The **directly configured** peer the hit entered through |

`origin` keeps pointing at the *home* registry, not the relay: when B relays D's record, A
shows `origin.peer_base_iri = D` and `via = B`. The relayed record's `origin.peer_id` is
dropped — it is a row id in B's peer table and means nothing in A's.

Per peer, `PeerSearchStatus` gains `reach`, `hops`, `via`, `note`, and two statuses beyond
`ok`/`timeout`/`error`:

- `already_handled` — the peer refused a repeat of this query id (a cut cycle edge).
- `skipped` — we did not ask: already on the path, hop budget exhausted, or fan-out capped.

Peers reported by a peer are ingested and re-expressed from our point of view, so the caller
sees the whole subtree that answered, including registries it has never configured.

Per response, `federation` carries `query_id`, `origin`, `registry`, `max_hops`,
`hops_granted`, `hops_forwarded`, `path`, and `budget_exhausted` — the last being the honest
admission that peers existed which the budget did not reach. A bounded answer says it is
bounded rather than passing for a complete sweep.

## 6. Abuse cases

| Case | What stops it |
|---|---|
| **Cycle / storm** | Three independent brakes (§3). Each registry handles a given query id exactly once. |
| **Slow peer** | One `tokio::time::timeout` wraps send *and* body read. Previously only `send()` was covered, so a peer that answered headers instantly and then dribbled a body was bounded only by the client-wide timeout. |
| **Unbounded total time** | Time is a budget, not a per-hop cost. The caller grants `fed_budget_ms`; the callee clamps it to its own and spends `budget − hop_margin` (600 ms) on its own fan-out. A walk of any depth finishes inside the origin's per-peer timeout instead of multiplying by depth. Peers are asked concurrently, so the total is bounded by one timeout, not by their number. |
| **Enormous result set** | `Content-Length` is checked when offered, and the stream is cut at 2 MiB regardless (a peer can lie about, or omit, the length). Hits from one peer are capped at 100, relayed peer statuses at 32, our own merged response at 500. |
| **Inflated hop budget** | Every budget is intersected with local policy, never adopted. `max_hops` clamped to 8. |
| **Inflated time budget** | Same: `min(own, granted)`, and `granted` itself capped at 600 s before the `min`. |
| **Fan-out amplification** | Peers per query capped at 12 (`TAR_FEDERATED_SEARCH_MAX_PEERS`); the excess is reported as `skipped`, not hidden. |
| **Seen-id table growth** | TTL sweep plus a hard row cap, both on the write path. No background task to own or to fail silently. |
| **Injection / log spam via `fed_id`** | The id is validated, never sanitised — rewriting it would break the sender's own deduplication — and every SQL parameter is bound. |
| **Duplicate records from two routes** | Merged on `(iri, entity_type)`, keeping the copy with the fewest hops: the most direct evidence wins. |

Two residual risks, stated rather than hidden:

- **Censorship via `fed_path` / `fed_origin`.** A malicious peer can name a registry in the
  path to stop us asking it. It could equally just not forward the query, so this grants no
  new power; it is bounded to one query and visible as a `skipped` row.
- **Amplification of an anonymous read.** One unauthenticated `GET` still causes up to
  `max_peers` outbound requests, and the mesh sees at most one handled query per registry per
  id. There is no rate limiter yet. An operator who is exposed sets
  `TAR_FEDERATED_SEARCH_MAX_HOPS=1` (the previous one-hop behaviour) or turns off
  `TAR_PUBLIC_READ`.

## 7. Configuration

All defaults are working defaults; a `docker run` with only `TAR_BASE_IRI` still propagates
safely.

| Variable | Default | Meaning |
|---|---|---|
| `TAR_FEDERATED_SEARCH_MAX_HOPS` | `3` | Hops from the origin. `1` restores one-hop-only. Clamped to 8. |
| `TAR_FEDERATED_SEARCH_TOTAL_TIMEOUT` | `10s` | Ceiling on one registry's whole fan-out. Clamped to 60 s. |
| `TAR_FEDERATED_SEARCH_HOP_MARGIN` | `600ms` | Held back from a granted budget so a callee answers before its caller gives up. |
| `TAR_FEDERATED_SEARCH_MAX_PEERS` | `12` | Peers contacted per query. |
| `TAR_FEDERATED_SEARCH_MAX_PEER_BYTES` | `2MiB` | Bytes read from one peer. |
| `TAR_FEDERATED_SEARCH_MAX_PEER_HITS` | `100` | Hits accepted from one peer. |
| `TAR_FEDERATED_SEARCH_MAX_PEER_STATUSES` | `32` | Peer-status rows accepted from one peer. |
| `TAR_FEDERATED_SEARCH_MAX_TOTAL_HITS` | `500` | Hits in our own merged response. |
| `TAR_FEDERATED_SEARCH_ID_TTL` | `10m` | How long a query id stays claimed. |

`TAR_FEDERATED_SEARCH_TIMEOUT` (existing, default `3s`) remains the per-peer timeout.

These are read in `ops::federation::FedSettings::from_env` rather than `Config`, because
`src/config.rs` was owned by another change while this landed. Folding them into `Config`
is mechanical and should happen next; the names and the duration grammar already match.

## 8. Tests

`tests/api.rs` stands up real registries on loopback ports and peers them over the real
well-known handshake, so the cycle is a genuine one across sockets rather than a mocked call.

- `a_repeated_federated_query_id_is_refused_as_already_handled` — the refusal, its wording,
  that it is per-id and not a circuit breaker, and that a malformed id is `400`.
- `the_hop_budget_stops_propagation` — A→B→C: at two hops C's record reaches A and is
  labelled `indirect`, `hops: 2`, `via: B`, with `origin` still pointing at C; at one hop C is
  never asked; a 9999-hop request is clamped.
- `a_cycle_in_the_peer_graph_terminates` — the full triangle, where every registry has two
  routes to every other. It asserts each registry claimed the id **exactly once**, that at
  least one *refused a repeat* (so termination came from loop prevention, not from the hop
  budget running out), that each record appears exactly once, and that a cut edge does not
  make the answer `partial`.
- `a_peer_cannot_flood_us_with_results` — a hostile stub peer returning 5 000 hits is
  truncated; one returning megabytes is refused unread.
- `src/ops/federation.rs` unit tests cover the claim primitive, TTL sweep and row cap, id
  validation, path parsing, and the hop clamp.
