# 01 — `tar dump --graph` restores into the wrong graph

**Kind:** Bounded · **Source:** `docs/limitations.md` #18

## The defect

`tar dump` (no argument) writes N-Quads and `tar restore` puts every quad back where it was.
`tar dump --graph <g>` writes **N-Triples** — the graph name is gone — so `tar restore` loads those
triples into the default graph, where nothing reads them. An operator who backs up one graph
this way gets a restore that silently does nothing useful.

## Where

- `src/main.rs` — `Command::Dump { graph }` and `Command::Restore { nquads }`.
- `src/store/{oxi,http}.rs` — `dump_nquads(graph: Option<&str>)` on both backends; with a graph
  it emits triples.
- `/admin/dump?graph=` (`src/api/registry.rs::dump`) and peer stub exchange **want** N-Triples.
  Do not change what they return.

## Done looks like

- `tar dump --graph <g>` output restores into `<g>` with plain `tar restore`, on both backends.
  Simplest route: the CLI writes N-Quads for a single graph (the store method, or a CLI-side
  conversion) while the HTTP endpoint keeps N-Triples. Choose whichever keeps the backends'
  behaviour identical; say which in the commit.
- A test that dumps one named graph, restores into an empty store, and finds the triples in that
  graph and not the default one. Runs against both backends (`tests/common::test_store`).
- `docs/limitations.md` #18 struck through and marked closed, in the style of #5, #13 and #15;
  `docs/operations/deployment.md` "The trap: `--graph` is not a backup" rewritten to match.
