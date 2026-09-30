# 08 — Fix the pre-deploy audit findings

**Kind:** Bounded (a few need a decision, marked) · **Source:** audit of 2026-09-28, before
deploying the registry on the IDS platform as a private platform app

Five audits ran in parallel: repo hygiene and deploy readiness, security, docs versus code,
build/release/first run, and a first-user walkthrough. Every finding below was checked against
the code; the ones marked **reproduced** were confirmed by running the server. Parked on
2026-09-29 to be picked up later.

**Already fixed since the audit** (PRs #9 and #10): self-registered deployments advertising via
`instance_key`, `version` linking a release, release-id validation (`ids::reference`, reuse it
below), the backwards run-time SHACL warning, bare licence names, artifact PATCH/DELETE, and
search showing tombstoned records.

Suggested split: one PR for §1, one for §2, one for §3, then bump to 0.2.0 and tag.

## Decisions (user, 2026-09-30)

- **Visibility: keep public.** Repo, image and docs site stay public; no pull secret or
  private-repo changes to the release workflow.
- **Token minting: tighten both ways.** A minted token's scopes must be within the deployment's
  `allowed_scopes` when it has any, and a non-admin caller cannot mint scopes it does not hold.
  Existing tokens keep working.
- **`/resolve`: peers only, and signed in.** On-demand resolve needs a credential and only
  fetches IRIs under an active peer's base; the background resolver skips non-peer IRIs, which
  stay plain cross-links. All fetches go through the guarded client.
- **Private addresses: split.** Records may list internal endpoints. The health probe keeps
  probing them (it only reports up/down); the API-doc viewer does not proxy private addresses
  unless `TAR_APIDOC_ALLOW_PRIVATE=true`. Neither follows redirects, and both always refuse
  link-local addresses (cloud metadata).

- **Tombstones: 410 with the record.** A withdrawn record answers `410 Gone` and still carries
  its body, marked tombstoned, so the page and the UI's banner keep working.
- **`/metrics`: admin token.** Prometheus sends an admin bearer token; the counts are cached for
  30 seconds so a scrape does not walk the graph every time.

## 0. Decision for the user first

- **Visibility.** The repo, the GHCR package and the Pages site are all **public** today; the
  user said it need not be public. Three separate switches (repo settings, package settings,
  Pages). Going private means the server needs an image pull secret (not in the manifests or
  docs), and `release.yml`'s arm64 runners and provenance attestation may need a paid plan.

## 1. Security — must fix before deploying

**Done** on branch `security-fixes` (2026-09-30), one commit per item, regressions in
`tests/security.rs`. Two notes: a SPARQL query that sorts before returning stops at oxigraph's
next cancellation check, which can be seconds past `TAR_SPARQL_TIMEOUT`; and the JWKS refetch
floor has no automated test (no network seam).

1. **SPARQL injection in IRI filters** (reproduced). `license`, `publisher`, `topic`, `produces`,
   `consumes` (`src/api/software.rs:50-58`), `conforms_to`, `license` (`artifacts.rs`),
   `operator` (`instances.rs`), and every `instance=`/`software=`/`run=` filter via
   `ids::iri_for` are spliced into `<{v}>`. An anonymous boolean oracle reads the whole store
   even with `/sparql` off. Fix: `ids::reference` / `queries::iri` on every user IRI; 400 on
   failure. Same for `/vocab/resolve` (`domain/mod.rs:101,182`).
2. **`/api/v1/resolve` is anonymous SSRF** (reproduced): fetches any URL, follows redirects,
   writes the result into the public catalogue, no body cap (1.1 GB from one request).
   Fix: require auth or restrict to active peers' bases; guarded client; body cap.
3. **One guarded HTTP client for all outbound fetches.** `state.http` follows redirects and has
   no address guard; the webhook guard (`is_public_ip`, `subscriptions.rs:563-640`) is reused
   nowhere else. Covers resolve, API-doc proxy, health probe, peer add, advertise-queued IRIs.
   Also default `TAR_APIDOC_ALLOW_PRIVATE` and `TAR_HEALTH_ALLOW_PRIVATE` to false.
4. **No SPARQL timeout on the embedded store** (reproduced): `store/oxi.rs:45-85`; a few anonymous
   queries pin CPUs. Deadline check in the iteration loop, plus a row cap.
5. **Stored XSS.** The API-doc proxy serves upstream HTML/SVG from the registry's origin
   (`apidocs.rs`): always `text/plain` + `nosniff` + `CSP: sandbox`. Link fields accept
   `javascript:`/`data:` (shapes only check `sh:nodeKind sh:IRI`): an `sh:pattern` for http(s)
   on link fields, like `tar:LicenceShape`, plus a `safeHref()` in the frontend. Add security
   headers (CSP, frame-ancestors, nosniff, Referrer-Policy) in one layer.
6. **Scope widening.**
   - A deployment credential can mint a token with scopes it does not hold, and `allowed_scopes`
     bounds nothing for API tokens (contradicts `docs/model.md`). Minted scopes ⊆ allowed_scopes,
     and ⊆ the caller's own scopes for non-admins. **Changes existing behaviour — confirm.**
   - `register:software` passes `require_curator()` (`auth/mod.rs:184`), so it can edit and
     tombstone every record. Give it its own guard.
   - JWT scopes (including `admin:*`) are honoured from any accepted issuer (`auth/jwt.rs:310-320`);
     roles are already home-issuer only. Take scopes from the home issuer only, never `admin:*`
     from a JWT, and intersect with the binding's `allowed_scopes`.
7. **Smaller:** anonymous `POST /peers/announce` stores unbounded rows (cap length and count,
   validate URL); `SERVICE` queries reach an external triplestore (reject in the algebra); 500
   responses echo the generated SPARQL (`error.rs`: generic 5xx body plus a log id); JWKS
   refetch has no negative cache.

## 2. Correctness and operations — must fix before deploying

8. **Tombstones:** GET of a tombstoned software/instance is 200, not the documented 410; PATCH
   revives a tombstoned software/instance (reproduced). A deleted deployment's tokens still
   advertise (reproduced): refuse a principal whose instance is tombstoned, or revoke on delete.
9. **Panics on user input** (reproduced): `expires_in` overflow at `config.rs:235` (`checked_mul`)
   and `ops/mod.rs:137` (`checked_add_signed`). Add `CatchPanicLayer`.
10. **Edit forms wipe records:** `SoftwareForm.tsx:46-69` and `InstanceForm.tsx:33-55` render
    blank when loading fails, and saving PATCHes nulls.
11. **Ack past the end hides deliveries** (`subscriptions.rs:462`): clamp to `max_seq` or 400.
12. **Container runs as root** under docker/compose; the documented non-root command fails on a
    fresh volume. `distroless/cc-debian12:nonroot`, an empty `/data` owned by 65532, `USER`;
    `user`, `read_only`, `cap_drop` in `compose.yaml`.
13. **`:0.1.0` is 52 commits old** and reports the same version as current builds. Bump to
    0.2.0 everywhere the version is pinned and tag `v0.2.0` — last, after the fixes.
14. **Backups:** no complete online backup (`/admin/dump` omits SQLite: tokens, peers, audit,
    subscriptions). Add a `VACUUM INTO` export, document a schedule. `backup.md` wrongly says
    SQLite holds only hashed secrets — webhook secrets are plaintext.
15. **`/metrics`** is reachable through the ingress, rate-limit exempt, and counts the whole
    graph per request: block at the ingress or serve on an internal port.

## 3. Should fix

- Non-problem+json errors: axum `Json`/`Query` rejections (400/415/413/422 text) and 405 with an
  empty body; wrap the extractors, add a method-not-allowed fallback.
- Lineage returns duplicated and reversed edges (`domain/artifact.rs:373+`); walk breadth-first,
  dedupe on (from, to, predicate). Default depth 2.
- `/types` cannot be paged past page 1; search `limit` is per kind and `total` is the count
  returned; list pages take 1.8–2.5 s on seed data (one `load_artifact` per row).
- Subscriptions: missing id is 404 and another's is 403 (enumeration split); filters skip the
  vocabulary check; unknown JSON fields are ignored everywhere (`deny_unknown_fields`).
- Config: `TAR_SUBSCRIPTION_BACKOFF_*` and `…MAX_PEER_BYTES` ignore documented units;
  `TAR_APIDOC_ALLOW_PRIVATE` only takes `1`/`true`; `tar seed --with-runs=false` fails.
- Deploy defaults: Fuseki password `admin` in `compose.yaml`; demo compose defaults a public
  root token (and the placeholder check misses `change-me`); `TAR_TRUSTED_PROXIES` belongs in
  the "what you must change" list; TLS/reverse-proxy section; Kubernetes checklist (TLS secret,
  ingress class, storage class, pull secret); `compose.yaml` both builds and pins `:0.1.0`;
  CI never builds the Dockerfile; `latest` would move on a pre-release tag.
- UI: TermPicker re-request loop on failure; destructive modals without try/catch; lists with
  no error state; `/release/…`, `/type/…`, `/artifact-series/…` land on "not found".
- Docs (~25): instance filter params, MCP tool count (18, `identify_content` undocumented), the
  federated timeouts are `min(TIMEOUT, TOTAL_TIMEOUT)`, missing routes and error types, three
  specs still "Draft for review", "EDAM topics" labels in `design-handoff.md` (vocabulary rule).
- `npm audit`: `react-router` moderate in the shipped bundle.

## 4. Decide, then maybe build

- Runs cannot be edited or withdrawn (artifacts now can).
- No delete in the UI for software, deployments or releases.
- Logs are text only (`TAR_LOG_FORMAT=json`).
- Per-owner permissions: `curator` can edit every record, not only a provider's own.
- A LICENSE file (Cargo.toml says Apache-2.0; spec Q7 unconfirmed).

## Held up

No real secrets in the repo or its history; token hashing and root-token comparison; CORS with
bearer auth; admin endpoints; MCP running tools as the caller; hardened Kubernetes manifests;
all doc links; the image builds and runs.
