# 04 — Repository liveness metrics

**Kind:** Needs design · **Source:** `docs/limitations.md` #4, `docs/design-handoff.md` §9

The last unmet item of the v1 UI scope.

## What exists

Repository **sync** works: a software record can keep named fields in step with its GitHub
repository (`src/domain/forge.rs`, `POST /api/v1/software/{id}/sync`, `TAR_FORGE_TOKEN`).
What is missing is the **liveness signal** the design asked for — stars, forks, last-commit age —
in the software page's signal bar. Today the UI omits those cells rather than showing zeros,
which the handoff requires to keep doing whenever the data is unknown.

## Where

- `src/domain/forge.rs` — the GitHub client and `token_for` (brokered vs configured token).
- `docs/specs/2026-08-30-tool-artifact-registry-design.md` §10.5 — `TAR_FORGE_TOKEN`,
  `TAR_FORGE_POLL_INTERVAL` (24h); the poller named there was never built.
- `frontend/src/routes/SoftwareDetail.tsx` — the signal bar.
- `src/health.rs` — the existing background-probe loop, the natural model for a poller.

## Questions the user must answer first

- Stored in the graph (RDF, so SPARQL and federation see it — which vocabulary?) or in SQLite as
  operational data only? This decides whether peers see our numbers.
- GitHub only, or GitLab too (the design says both)?
- Poll all records on an interval, or fetch on page view with a cache? Rate limits: an
  unauthenticated GitHub client gets 60 requests/hour.
- Egress: in production this goes through the egress proxy (ids3). Same path as sync?

## Done looks like

Approved spec, then: a poller or cache with tests against a stubbed forge (no network in tests —
CI must stay offline), the signal-bar cells rendering real numbers and still omitted when unknown
(frontend test), `TAR_FORGE_*` documented in `docs/operations/configuration.md`, #4 closed.

## Decisions (user, 2026-09-24)

- Stored in **SQLite only**, as operational data. Not in the graph, so peers and SPARQL don't
  see the numbers.
- **GitHub only**, matching repository sync. Records hosted elsewhere keep the cells omitted.
- Fetched by a **background poller** on `TAR_FORGE_POLL_INTERVAL` (24h), spread out to stay
  within GitHub's rate limit.
- It uses **the same client, token and egress path as repository sync**: `TAR_FORGE_TOKEN` when
  set, anonymous otherwise (with a slower poll).
