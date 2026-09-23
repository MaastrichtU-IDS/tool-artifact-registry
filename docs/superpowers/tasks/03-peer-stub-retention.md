# 03 — Peer stub retention, and caching only a stub

**Kind:** Needs design · **Source:** spec Q3, `docs/limitations.md` #7

Two problems in the same code, best designed together.

## The problems

1. **Retention (Q3).** Records cited from a peer registry are fetched into that peer's named
   graph `<urn:tar:peer:{id}>` and refreshed on `TAR_PEER_RESOLVE_TTL` (24h) forever. Nothing
   decides what happens when a peer has been unreachable for months: its stubs stay, look current,
   and are never marked stale beyond the "cached N ago" chip.
2. **Over-caching (#7).** The resolver loads the peer's **whole Turtle document** into the peer
   graph, where the design asked for a minimal stub: type, title, publisher, home registry. A
   verbose peer can put far more into our store than we will ever show.

## Where

- `src/api/peers.rs` — `resolver_loop` (~l.318), the fetch + `load_turtle` into
  `ns::peer_graph(&peer_id)` (~l.289–299), peer removal dropping the graph (~l.221).
- `src/ops/` — the peers table (`resolve_status`, last seen) in SQLite.
- `docs/specs/2026-08-30-tool-artifact-registry-design.md` §8.4, §9 — the federation model.
- Frontend: stale-peer rendering rules in `docs/design-handoff.md` §7 ("cached from *peer* · N ago").

## Questions the user must answer first

- After how long unreachable does a stub change state, and to what: **dropped**, **tombstoned**
  (IRI still resolves, marked gone), or **kept but flagged stale**? Q3 names 90 days as an example.
  Does a local record citing that stub affect the choice (dropping it leaves a dangling link)?
- Per peer, or per stub (a peer that is up but no longer serves one record)?
- Stub contents: exactly which predicates are kept? Is the list fixed, or shaped by what the
  UI and search actually read (check `src/domain` and `src/api/search.rs`)?
- Existing over-full peer graphs: trimmed by a boot migration (see `src/seed.rs` migrations) or on
  the next refresh?

## Done looks like

A spec in `docs/specs/`, approved, then an implementation with tests for: the state transition
at the threshold, a recovered peer coming back, a stub keeping only the agreed predicates, and
peer search / dereference still working on trimmed stubs. Q3 marked answered; #7 closed.
