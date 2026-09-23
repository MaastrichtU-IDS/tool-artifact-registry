# Peer Stubs: Staleness and What a Stub Holds — Design Note

| | |
|---|---|
| **Status** | Implemented |
| **Date** | 2026-09-24 |
| **Spec** | [`2026-08-30-tool-artifact-registry-design.md`](2026-08-30-tool-artifact-registry-design.md) — answers Q3; closes limitations #7 |
| **Code** | `src/api/peers.rs`, `src/ops/mod.rs`, `src/domain/mod.rs`, `src/model.rs`, `src/ops/federation.rs`, `frontend/src/components/chips.tsx`, `frontend/src/routes/Peers.tsx` |

---

## 1. Why

When a record here cites a record at a peer registry, the resolver fetches it and caches a
**stub** in that peer's graph, `<urn:tar:peer:{id}>`. Two questions were open:

1. **Q3.** What happens to those stubs when the peer has been unreachable for months? Nothing
   decided it, and the UI's "cached N ago" chip was the only hint.
2. **Limitations #7.** The resolver loaded the peer's **whole** Turtle document, where the design
   asked for a minimal stub. A peer that serves a rich page puts far more into our store than we
   ever show.

Reading the code for this turned up two defects that both answers depend on:

- **Stubs were never refreshed.** `mark_resolved` schedules the next refresh at
  `TAR_PEER_RESOLVE_TTL`, but `due_resolves` skipped every entry with status `resolved`. A stub
  was fetched once and then never again.
- **Peer liveness was never recorded.** `peers.last_seen_at` and `resolve_status` were written
  when a peer was added and never again. The chip's "cached N ago" was really "added N ago".

---

## 2. Decisions

The user answered each question on 2026-09-24.

| Question | Answer |
|---|---|
| A long-unreachable peer's stubs are… | **kept and flagged stale**, never dropped or tombstoned, so local records citing them keep resolving |
| Threshold | **90 days, per peer**, from the peer's last successful contact |
| A stub keeps… | **what the registry reads** (§4) |
| Existing over-full peer graphs | **trimmed on their next refresh**, with no boot migration |

---

## 3. Staleness

- Every stub fetch records its outcome on the owning peer. On success it sets `last_seen_at` to
  now, `resolve_status` to `ok` and clears `last_error`. On failure it sets `resolve_status` to
  `error` and `last_error` to the reason. `last_seen_at` then really means the last successful
  contact.
- A peer is **stale** when its last successful contact, or its `added_at` if it has never been
  reached, is more than **90 days** ago. The threshold is a constant, not a setting: nobody asked
  to tune it, and a setting is easy to add later.
- The record `origin` gains `stale: true` on every record from a stale peer, and the peer list
  carries the same flag. The origin chip says "stale" in words, beside the cache age, rather
  than by colour alone.
- Nothing is deleted. A peer that comes back refreshes its stubs on the next resolver pass, and
  the flag clears with the first successful contact.
- Due refreshes now include resolved entries whose `next_attempt_at` has passed, so a stub is
  refreshed every `TAR_PEER_RESOLVE_TTL` as the design always said.

---

## 4. What a stub holds

"What the registry reads" was measured, not guessed. The domain layer reads about 135
predicates through its helpers, and search, chips and dereference name about 130 more terms
in SPARQL text. Together that covers ~160 predicates across 11 namespaces, which is the
registry's whole record model. That is expected, because a cached peer record is shown as a full
record page. A hand-kept list of 160 predicates would drift the first time someone adds a
field, so the boundary is drawn in two places instead:

1. **Subjects.** Only the requested IRI and the sub-resources it owns are kept: its blank-node
   closure and the named sub-resources `store::is_owned_subresource` recognises (distributions,
   checksums). This is the same closure `describe` returns. Other records that come along in the
   peer's document — its catalog, related runs, neighbouring artifacts — are dropped. They are
   resolved on their own if something here cites them.
2. **Predicates.** Only predicates in the namespaces the model reads are kept:
   `rdf`, `rdfs`, `dcterms`, `dcat`, `prov`, `schema.org`, `skos`, `spdx`, `codemeta`, `foaf`,
   `tar`, and `owl:sameAs`. A statement in any other vocabulary is something no screen or query
   here reads.

**This departs from the brief's "exact predicate list".** A per-predicate list would be the whole
model, restated. The two rules above remove what was actually over-cached: other records, and
other vocabularies.

The stub is written as one `GraphTx` (replace the subject, insert the trimmed quads), where
before it was a delete followed by a separate load. The embedded store applies that as one
transaction. On an external endpoint it is one SPARQL Update request, and it is atomic only if
the endpoint runs a request as one (limitations §16).

Ownership is followed no deeper than the external backend's delete follows it
(`queries::DEFAULT_DEPTH`, four levels), so on either backend a refresh removes everything the
previous fetch wrote.

**Trimming what the old resolver left.** The old resolver loaded the whole document, so a peer
graph can hold other records the document described. On each refresh, every other named subject
in the fresh document is removed from the peer graph, unless it is a stub the resolver tracks in
its own right, which its own refresh keeps. Every stub is tracked once it has been fetched, so
one resolved through `/resolve` directly is not mistaken for a leftover. A leftover the peer has
since removed from the document is not seen, and stays.

---

## 5. Tests

- **Unit** (`src/api/peers.rs`): a document containing the record, a distribution with a
  checksum, a second record and a foreign-vocabulary statement is trimmed to the record and its
  distribution, with the foreign statement and the second record gone.
- **Unit** (`src/ops/mod.rs`): the 90-day threshold, based on the last contact and falling back
  to `added_at`; a resolved entry whose TTL has run out is due again, and one that has not is not
  (the old query returned neither); a contact is recorded, and a failure sets the error without
  moving `last_seen_at`.
- **End-to-end** (`tests/api.rs`, `a_refresh_trims_a_legacy_peer_graph_and_counts_the_record_once`):
  a peer graph holding a whole legacy document keeps only the record after a refresh, and two
  refreshes leave the cached count where it was.
- **Frontend** (`chips.test.tsx`): a stale origin says so in words.

---

## 6. Not done, deliberately

- **Per-stub staleness.** One clock per peer, as decided. A peer that is up but has stopped
  serving one record keeps failing that record's refresh with a visible `last_error` in the
  resolve queue, but the record is not flagged on its own.
- **A setting for the threshold.** Add `TAR_PEER_STALE_AFTER` when someone needs a different
  number.
- **Trimming stubs of a peer that never comes back.** They stay full until a refresh succeeds, as
  decided. A peer that never returns stays full and stale.
