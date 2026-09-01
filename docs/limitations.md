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

## 5. Human sign-in has no token refresh

Sign-in itself is verified against a live identity provider: a browser was driven through
authorisation code + PKCE, and the `curator` and `admin` roles were confirmed to decide what a
signed-in person may do — `POST /api/v1/software` succeeds for a curator, `/api/v1/peers` is
`403` for a curator and `200` for an admin.

What is not covered is **refresh**. The access token is used until it expires and there is no
silent renewal, so a long editing session ends in a `401` and a re-sign-in. The refresh token is
deliberately not stored in the browser.

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

## 13. A residual DNS-rebinding race on webhook targets

A subscription's webhook URL is checked against private and loopback addresses at registration
*and* re-resolved at send time. A name that resolves differently between the two checks is a
window the current code documents rather than closes.

## 14. Subscriptions have no scope of their own

Managing a subscription reuses the rule that governs token management — admin, curator, or the
credential of the owning deployment. There is no `subscribe:*` scope, so a credential cannot be
issued that may subscribe and nothing else.
