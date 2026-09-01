# Federation

Two registries federate by pointing at each other, not by copying each other.

That choice runs through everything here. A harvest gives you a second copy of somebody else's
catalogue that is wrong as soon as they change it, and it makes you responsible for data you did
not curate. A cross-link gives you their identifier, a cached stub so the page renders, and a
clear statement of whose record it is.

## Peers

```
GET    /api/v1/peers              list                       admin
POST   /api/v1/peers              add one                    admin
GET    /api/v1/peers/suggested    the review queue           admin
DELETE /api/v1/peers/{id}         remove one                 admin
POST   /api/v1/peers/announce     inbound, unauthenticated
GET    /api/v1/resolve            dereference a foreign IRI
```

Adding one:

```bash
curl -X POST -H "Authorization: Bearer $ADMIN" -H 'content-type: application/json' \
     -d '{"base_url": "https://peer-registry.example.net", "preview": false, "announce": true}' \
     https://registry.example.org/api/v1/peers
```

The registry fetches the peer's `/.well-known/tar-registry`, and **refuses if the base IRI the
peer advertises does not match the URL you gave**. A registry that calls itself something else
is either misconfigured or not the registry you think you are pointing at, and cross-linking to
it would mint links that do not resolve.

`preview: true` does the fetch and the validation and stores nothing — worth doing first.
`announce: true`, the default, calls the peer's own `/api/v1/peers/announce` so the relationship
can become mutual.

### Suggestions are not peers

Adding a peer also records everyone *they* federate with as **suggested**, and the inbound
`/api/v1/peers/announce` records its caller the same way. Neither ever becomes an active peer on
its own.

`announce` is deliberately unauthenticated, because it grants nothing: it puts a URL in a queue
for an administrator to look at. Trust is not transitive, and an endpoint that made it transitive
would be an endpoint that lets a stranger join your federation by asking.

## Foreign IRIs in your own records

Any object position may hold a foreign IRI — `was_derived_from` pointing at an artifact at
another registry, most commonly.

**Advertising never blocks on the network.** An unknown IRI is stored verbatim, the write
succeeds, and a background worker fetches a stub afterwards. A peer being slow or down must
never be able to fail somebody else's CI job.

The stub lands in a named graph of that peer's own, `<urn:tar:peer:{id}>`, and is never merged
into local data. That separation is what lets the registry apply its own rules to its own
records and not to a peer's: peer data does not pass through a write handler at all, so the
[vocabulary rule](../vocabulary/terms.md#federation-is-untouched-by-this-rule) never sees it.

A resolved stub is cached for `TAR_PEER_RESOLVE_TTL`, default 24 hours. `GET
/api/v1/resolve?iri=…` dereferences one on demand, and `&refresh=true` forces a re-fetch. The
background resolver ticks every 30 seconds and backs off on failure.

An unresolved record renders as the bare IRI marked "not resolved yet", with its origin chip —
never as a skeleton, because a skeleton promises content that may never arrive.

## Federated search

`GET /api/v1/search?q=…&federated=true` fans out to peers live. Nothing is pre-fetched, so the
results are as current as the peers are.

Only search fans out. Capability matchmaking and the graph endpoint are local-only.

Live fan-out across a graph of registries is a loop waiting to happen, so there are three
independent brakes and a time budget:

1. **Query identity.** Each query carries an id; a registry that has already handled it answers
   `already_handled: true` with a `200`. Not an empty result and not an error — a peer must be
   able to tell "I have already answered this" from "I found nothing".
2. **A hop budget**, decremented at each hop and always reduced to the minimum of what was
   granted and this registry's own maximum. A peer cannot spend more of your budget than you
   have.
3. **A path check.** The query carries the registries it has visited, and a registry never asks
   a peer already on the path, nor the origin, nor itself.

The time budget is passed down, clamped to this registry's own total timeout, with a margin held
back before forwarding so that a hop still has time to answer after its own children do.

| | Default |
|---|---|
| `TAR_FEDERATED_SEARCH_MAX_HOPS` | 3 (max 8) |
| `TAR_FEDERATED_SEARCH_TIMEOUT` | 3s — per peer |
| `TAR_FEDERATED_SEARCH_TOTAL_TIMEOUT` | 10s (max 60s) |
| `TAR_FEDERATED_SEARCH_HOP_MARGIN` | 600ms |
| `TAR_FEDERATED_SEARCH_MAX_PEERS` | 12 (max 64) |
| `TAR_FEDERATED_SEARCH_MAX_PEER_HITS` | 100 (max 1000) |
| `TAR_FEDERATED_SEARCH_MAX_PEER_BYTES` | 2 MiB (max 32 MiB) |
| `TAR_FEDERATED_SEARCH_MAX_PEER_STATUSES` | 32 (max 256) |
| `TAR_FEDERATED_SEARCH_MAX_TOTAL_HITS` | 500 (max 5000) |
| `TAR_FEDERATED_SEARCH_ID_TTL` | 10m |

Every response says which peers answered, which timed out and which failed. A federated search
where half the federation was down should not look like a federated search that found half as
much.

Results are not deduplicated across peers beyond the origin chip, and the keyset ordering
interleaves imperfectly across origins. Both are in [Limitations](../limitations.md).

## What federation does not give you

- **Not authorisation.** A peer's records are as public as that peer makes them. Federating does
  not grant you access to anything.
- **Not agreement on vocabulary.** Two registries that each minted their own term for the same
  thing still have two terms. [Adopting](../vocabulary/terms.md#adopting-versus-minting) is the
  mechanism that fixes that, and it is a decision a curator makes, not something federation does
  for you.
- **Not availability.** If a peer is down, its records are unresolved. The registry says so
  rather than hiding it.
