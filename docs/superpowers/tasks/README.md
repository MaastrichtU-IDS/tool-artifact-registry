# Task backlog

What is left, as of 2026-09-23, after rate limiting (spec Q8) and the base-IRI rebase (spec Q9)
were built on branch `base-iri-rebase`. Each file is a self-contained brief for an agent picking
the task up cold: context, where the code is, what done looks like, and what is still undecided.

**Before starting any task:** branch from whatever carries the latest work. At the time of
writing that is `base-iri-rebase`, which contains `rate-limiting`; neither is merged to `main`.

**Decisions.** The user answered every open question for 03, 04, 05 and 07 on 2026-09-24. Each brief now ends with a *Decisions* section; the spec for each still has to be written before any code.

**Two kinds of task.** *Bounded* tasks change code that already exists and can go straight to
TDD. *Needs design* tasks have an open question the user must answer first — run the
brainstorming flow, get the design approved, and write a spec in `docs/specs/` before any code.

| # | Task | Kind | Source |
|---|---|---|---|
| 01 | [`tar dump --graph` restores into the wrong graph](01-dump-graph-name.md) | Bounded · **done** on `bounded-tasks` | limitations #18 |
| 02 | [Clear the clippy warnings](02-clippy-cleanup.md) | Bounded · **done** on `bounded-tasks` | CI advisory step |
| 03 | [Peer stub retention, and caching only a stub](03-peer-stub-retention.md) | Needs design · **done** on `peer-stubs` | spec Q3, limitations #7 |
| 04 | [Repository liveness metrics](04-repo-liveness-metrics.md) | Needs design · **decided** | limitations #4, handoff §9 |
| 05 | [Deduplicate federated search results](05-federated-dedup.md) | Needs design · **done** on `federated-dedup` | limitations #6 |
| 06 | [A `subscribe:*` scope](06-subscribe-scope.md) | Bounded · **done** on `bounded-tasks` | limitations #14 |
| 07 | [Helm chart and ServiceMonitor, or drop the promise](07-helm-chart.md) | Needs decision · **decided** | spec §10.3 |

Suggested order: 01 and 02 (small, independent), then 03, 06, 05, 04, 07.

## Deferred on purpose — not briefed

Recorded so nobody mistakes them for forgotten. Each has a stated reason in `docs/limitations.md`.

- Lineage graph visualisation (limitations #9) — v2; the API already serves the data.
- Observed vs declared capability (limitations #12) — waits for enough run data to mean something.
- Offering peer-resolved types in the picker (limitations #11) — a federation-ownership question.
- DOIs (Q1), signed advertisements (Q2), multi-tenancy (Q4), capability as a SHACL shape (Q5).
- Q7: licence and repository home are assumed (Apache-2.0, MaastrichtU-IDS) — a question for the
  user, not an agent.
