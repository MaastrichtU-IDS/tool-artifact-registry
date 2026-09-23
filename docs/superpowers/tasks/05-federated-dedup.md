# 05 — Deduplicate federated search results

**Kind:** Needs design (small) · **Source:** `docs/limitations.md` #6

## The problem

Federated search fans out to peers and merges what comes back, labelled with an origin chip.
When two peers both hold a cached stub of the **same third-party record**, the user sees two rows
for one record.

## Where

- `src/api/search.rs` — the fan-out and merge.
- `docs/specs/2026-08-31-federated-search-propagation.md` — the propagation design; read its
  reasoning on origins before changing the merge.
- `frontend/src/routes/Search*` — how rows and origin chips render.

## Questions the user must answer first

- Key: the record's IRI is the obvious one. Is that enough, given stubs are copies of a record
  at its home registry?
- When rows collide, which wins: the record's home registry if present, else the freshest copy?
  Is "also cached by: A, B" shown, or dropped?
- Does dedup happen at each hop (smaller responses) or only at the origin (simpler, no
  information lost along the way)?

## Done looks like

A short design note (a section appended to the propagation spec is fine), then: merge by the
agreed key, a test with two fake peers returning the same IRI, the UI showing one row, #6 closed.
