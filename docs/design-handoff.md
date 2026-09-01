# Tool Artifact Registry — Frontend Handoff

| | |
|---|---|
| **Status** | Draft for review |
| **Date** | 2026-08-30 |
| **Spec** | [`docs/specs/2026-08-30-tool-artifact-registry-design.md`](specs/2026-08-30-tool-artifact-registry-design.md) |
| **Audience** | Frontend developer implementing the v1 UI |

Read §4 (data model), §6 (FAIR access), §7 (API) and §8 (auth) of the spec before starting.
This document adds only what the spec does not: screens, routes, components, states.

---

## 1. Stack

Match the sibling repos (`shacl-manager/frontend`, `sulo-schema-builder/frontend`):

- React 18 · Vite 5 · TypeScript 5
- `react-router-dom` v6
- Vitest + Testing Library + jsdom
- No UI framework mandated. No state-management library in v1 — router loaders plus local
  state are sufficient; add one only when a real cross-screen need appears.

Served either by the registry binary as static assets under `/` (default, keeps the
single-container promise of spec §10.2) or standalone via Vite in development with a proxy to
`/api`.

---

## 2. Design principles

1. **Lead with what is unique.** Any registry can show a name and a licence. Only this one can
   show *what a tool consumes and produces* and *what a deployment actually did*. Those
   blocks sit above the fold; description and metadata sit below.
2. **Software and Instance are different things and must never look alike.** Software is
   abstract and has no runs. Instance is concrete, has an endpoint, a health state, and runs.
   Confusing them is the single most likely user error — see the `kind` and origin chips (§6.1).
3. **Local vs foreign must always be visible.** A record cached from a peer registry is
   read-only and possibly stale. It never renders identically to a local record.
4. **Machine-readable is a first-class affordance.** Every detail page offers Turtle / JSON-LD
   download and shows the persistent IRI. This is a FAIR tool; hiding the RDF would be absurd.
5. **Anonymous read is the default.** Every read screen must render fully without a session.

---

## 3. Information architecture

Top-level tabs:

```
Software   Instances   Artifacts   Runs   Peers*        [search]   [sign in]
                                                 * admin/curator only
```

### Routes

| Route | Screen | Auth |
|---|---|---|
| `/` | redirect → `/software` | — |
| `/software` | Software list | anon |
| `/software/:id` | Software detail | anon |
| `/software/new`, `/software/:id/edit` | Software form | curator |
| `/instances` | Instance list | anon |
| `/instances/:id` | Instance detail | anon |
| `/instances/new`, `/instances/:id/edit` | Instance form | curator |
| `/instances/:id/tokens` | Token management | owner or admin |
| `/artifacts` | Artifact list | anon |
| `/artifacts/:id` | Artifact detail | anon |
| `/runs` | Run list | anon |
| `/runs/:id` | Run detail | anon |
| `/peers` | Peer admin | admin |
| `/search?q=` | Search results | anon |
| `/auth/callback` | OIDC callback | — |
| `*` | Not found / tombstone | anon |

IRI dereference: the backend content-negotiates. A browser `Accept: text/html` on
`{base}/software/{uuid}` serves the SPA, which routes to `/software/:id`. **Registry IRIs and
UI routes are the same URLs** — do not invent a separate `/ui` prefix, it would break the
"IRIs are dereferenceable in a browser" property.

---

## 4. Layout system

Two-column with a sticky right rail, applied to **both** Software and Instance detail pages so
the two read as one system.

- Main column `minmax(0, 1fr)`, rail `320px`, gutter `24px`, page max-width `1200px`.
- Rail is `position: sticky; top: 64px` above `1024px`; **below `1024px` the rail collapses
  and its sections flow to the bottom of the main column in rail order.**
- Header is full-width above both columns.
- Section order in the main column is fixed; rail order is fixed. Do not reorder responsively
  beyond the collapse described above.

### 4.1 Software detail

```
┌──────────────────────────────┬─────────────────┐
│ shacl-manager                │ METADATA        │
│ SHACL shape mgmt+validation  │ Apache-2.0      │
│ [Repo] [Docs]                │ kind: service   │
├──────────────────────────────┤ EDAM topics:    │
│ 5 instances │ 143 runs/30d   │  ◆ Data quality │
├──────────────────────────────┤  ◆ Semantic web │
│ USE IT                       │ Rust 94% Py 4%  │
│ $ docker pull ghcr.io/…  ⧉   ├─────────────────┤
├──────────────────────────────┤ CITE            │
│ CONSUMES      │ PRODUCES     │ …/software/01J  │
│ ◆ RDF graph   │ ◆ SHACL rpt  │ [BibTeX] [RIS]  │
│ ◆ SHACL shape │ ◆ Summary    ├─────────────────┤
├──────────────────────────────┤ RELEASES        │
│ INSTANCES                    │ v2.1  2mo       │
│ shacl.ids.unimaas.nl v2.1 ●  │ v2.0  7mo       │
│ shacl.mumc.nl        v2.0 ●  ├─────────────────┤
│ laptop-eerol         v2.1 —  │ PEOPLE          │
├──────────────────────────────┤ ▣ E. Erol ORCID │
│ ## Description               │ ▣ MaastrichtU   │
│ Multi-tenant platform for …  ├─────────────────┤
│                              │ PUBLICATIONS    │
│ ## Publications              │ FEDERATION      │
│ …                            │ FAIR ⬇ ttl jsonld│
└──────────────────────────────┴─────────────────┘
```

**Software pages have no run list.** Runs belong to Instances (spec D5). The signal bar shows
a roll-up (`143 runs/30d` across all instances) that links to `/runs?software=:id`.

### 4.2 Instance detail

Same shell. Main column: header (label, health dot, `runs ▸ software v2.1`, operator,
`[Open endpoint] [OpenAPI]`), signal bar (last run, runs/30d, failures, artifacts), **Runs**
table, **Artifacts produced here** table, narrow-capability note if it differs from the
Software declaration. Rail: endpoint details, `tar:availability`, jurisdiction, operator,
release currency warning, home registry, FAIR downloads.

---

## 5. Screens

Each entry gives the API calls (spec §7) and the states to build.

### 5.1 Software list — `/software`

`GET /api/v1/software?q=&license=&publisher=&edam_topic=&keyword=&kind=&cursor=`

Card or row per software: name, tagline, licence chip, `kind` chip, instance count, EDAM topic
chips, origin chip. Left facet panel mirrors the query params. Keyset pagination
(`cursor`) — not page numbers.

States: loading skeleton · empty ("No software registered yet" + *Register software* CTA for
curators, plus a `tar seed --from ids-examples` hint for admins) · filtered-empty (distinct
copy + *Clear filters*) · error.

### 5.2 Software detail — `/software/:id`

`GET /api/v1/software/{id}` · `GET /api/v1/software/{id}/releases` ·
`GET /api/v1/instances?software={id}`

Blocks in the order shown in §4.1. Notes:

- **Use it** renders only if a container image or install command exists on the latest Release.
- **Consumes / Produces** — two columns of `ArtifactTypeChip`. Each chip links to
  `/artifacts?conforms_to={typeIRI}`. Under Produces, a link *"N tools consume this"* →
  `/software?consumes={typeIRI}` (backed by `GET /api/v1/capabilities`). If no capability is
  declared, show an inline empty state with a *Declare capability* action for curators — do
  not hide the block; its absence is information.
- **Instances** — label, release, health dot, operator, last run. A row whose release is older
  than the latest Release gets a muted "outdated" marker.
- **Cite** (rail) — persistent IRI with copy, `[BibTeX] [RIS]`, version selector when
  releases exist.
- **FAIR** (rail) — `⬇ Turtle` `⬇ JSON-LD` `⬇ biotoolsSchema`, hitting the same IRI with an
  `Accept` header or the `/export/biotools` endpoint.

### 5.3 Instance list — `/instances`

`GET /api/v1/instances?software=&operator=&status=&release=&registry=`

Row: label, software + version, health dot, operator, endpoint (or "no endpoint — CLI/batch"),
last run, origin chip. Facets: software, operator, status, home registry.

An Instance **without** `dcat:endpointURL` is normal (a laptop or batch run) and must not
render as broken.

### 5.4 Instance detail — `/instances/:id`

`GET /api/v1/instances/{id}` · `/runs` · `/artifacts`

Run table columns: run id (short, copyable), started (relative + absolute on hover), status,
`N in → M out`, external key, agent. Row expands to a run summary; clicking opens
`/runs/:id`.

### 5.5 Artifact detail — `/artifacts/:id`

`GET /api/v1/artifacts/{id}` · `GET /api/v1/artifacts/{id}/lineage?depth=1`

- Header: title, type chip, licence chip, availability badge, origin chip.
- **Distributions** — one card each: access/download URLs, media type, size, checksum with
  copy, `conformsTo` link, protocol and auth-method chips.
  **If `availability` is `metadata-only`, render no download affordance at all** — show the
  availability badge and an *Request access* button bound to `tar:accessRequestURL`. Never a
  disabled download button; that miscommunicates.
- **Provenance** — "Generated by run X at instance Y running software Z v2.1", plus
  `wasDerivedFrom` inputs and downstream consumers (from the depth-1 lineage call), as lists.
- **Versions** — other artifacts in the same `dct:isVersionOf` series, current one marked.
- Rail: persistent IRI, cite, FAIR downloads, publisher, dates.

### 5.6 Run detail — `/runs/:id`

Timeline header (started, ended, duration, status), instance + release + operator,
`Consumed` and `Produced` artifact lists side by side, external key, raw payload viewer
(collapsed) when `tar:openLineagePayload` is present.

### 5.7 Software / Instance forms

`POST|PATCH /api/v1/software` · `POST|PATCH /api/v1/instances`

Sectioned form mirroring the model: identity → links → licence & party → EDAM topics &
keywords → capability (produces/consumes type pickers with EDAM autocomplete plus a
free-IRI escape hatch, per spec D11).

The API validates writes with SHACL and returns `422` with a Turtle validation report
(spec §7.9). **Map report entries back to the offending fields** — parse `sh:resultPath` and
`sh:resultMessage` and render inline field errors, with the full report available behind a
*Show validation report* disclosure. Do not dump Turtle at the user as the primary error.

### 5.8 Token management — `/instances/:id/tokens`

`POST /api/v1/instances/{id}/tokens`

Scope checkboxes (`advertise:produce`, `advertise:consume`, …), optional expiry. **The token
value is shown exactly once**, in a modal, with a copy button and an explicit "this will not
be shown again" warning; the list thereafter shows prefix, scopes, created, last used, expiry,
and a revoke action. Revocation asks for confirmation and names the instance.

### 5.9 Peer admin — `/peers`

`GET|POST|DELETE /api/v1/peers` · `GET /api/v1/peers/suggested`

Peer table: title, base IRI, last seen, resolve status, cached-record count, actions.
Add-peer flow: paste base URL → the UI calls the peer's `/.well-known/tar-registry` preview →
show what will be trusted (title, operator, base IRI) → confirm.

Suggested peers (peers-of-peers) are a separate, visually secondary list with *Review* and
*Dismiss*; they are never added automatically (spec §8.4, §9.5). Removing a peer is
destructive — it drops the cached graph — so confirm with the record count named.

### 5.10 Search — `/search?q=`

`GET /api/v1/search?q=&type=&federated=`

Results grouped by entity type. A **Search peer registries** toggle sets `federated=true`;
when the response carries `partial: true`, render a persistent banner listing which peers
timed out. Federated results are visually distinct via the origin chip and are never
interleaved silently with local ones.

---

## 6. Shared components

### 6.1 Chips and badges

| Component | Purpose | Rules |
|---|---|---|
| `OriginChip` | `local` vs `peer: <name>` | On every record header and every list row that can contain foreign data. Peer chips link to the peer's record at its home registry. Foreign records render all edit affordances as absent, not disabled. |
| `KindChip` | `service` / `library` / `cli` / `workflow` | Distinguishes Software kinds; helps keep Software vs Instance distinct. |
| `ArtifactTypeChip` | EDAM or local type | Shows label, tooltips the definition, links to filtered artifacts. Falls back to the IRI's last segment if the label is unresolved. |
| `AvailabilityBadge` | `public` / `restricted` / `embargoed` / `metadata-only` | Drives whether download affordances render at all (§5.5). |
| `LicenseChip` | SPDX | Links to the SPDX IRI. Renders "unlicensed" distinctly from absent. |
| `HealthDot` | instance up/down/unknown | Never colour-only — pair with text or shape. |
| `RunStatus` | success / failed / running / aborted | Same rule. |

### 6.2 Other

`CopyField` (IRI, checksum, token, install command — copy button with confirmation),
`CommandBlock` (`$ docker pull …` with copy), `SignalBar` (label/value pairs, degrades to
`—` for unknowns), `CiteBlock` (IRI + BibTeX/RIS + version selector), `FairDownloads`
(Turtle / JSON-LD / biotoolsSchema), `FacetPanel`, `KeysetPager`, `RelativeTime`
(relative text, absolute in `title`, `<time datetime>`), `EmptyState`, `ErrorState`,
`ProblemJsonError` (renders RFC 9457 `title`/`detail`/`instance`).

---

## 7. Cross-cutting states

- **Loading:** skeletons that match final layout. No spinners on full pages.
- **Empty vs filtered-empty:** always distinct copy; filtered-empty offers *Clear filters*.
- **Unknown values:** `—`, never `null`, `undefined`, or a blank cell.
- **Errors:** RFC 9457 rendered via `ProblemJsonError`; retry where the action is idempotent.
- **Stale peer data:** any record from `<urn:tar:peer:*>` shows "cached from *peer* · *N* ago"
  next to the origin chip.
- **Tombstoned records:** soft-deleted IRIs still resolve (spec §7.2). Render the record with a
  clear tombstone banner and no actions — do not 404.
- **Auth-gated actions:** hidden for anonymous users, not shown-and-disabled. Sign-in is a
  header affordance, and OIDC is optional server-side — **if `/.well-known/tar-registry`
  reports no OIDC issuer, hide sign-in entirely.**

---

## 8. Accessibility

- Keyboard-navigable throughout; visible focus rings; skip-to-content link.
- Status and health never conveyed by colour alone (§6.1).
- Tables use real `<table>` semantics with `<th scope>`; copy buttons have accessible names
  ("Copy persistent IRI", not "Copy").
- Modals trap focus and restore it on close; the token modal must be dismissible only by an
  explicit action, since the value is unrecoverable.
- Chip rows are lists; icon-only buttons carry `aria-label`.
- Target WCAG 2.2 AA contrast in both light and dark themes.

---

## 9. v1 scope

**In:** browse and search (Software, Instances, Artifacts, Runs); Software and Instance detail
pages; registration and editing forms; token management; peer administration; federated search
toggle; cite blocks; copy-paste install snippets; repo liveness metrics; publications.

**Deferred to v2:** lineage *graph* visualisation (v1 renders the same data as tables — the
data model and API already support the graph, so this is purely a UI addition);
capability-as-SHACL-shape pickers (spec Q5); multi-tenant UI; artifact upload.

**Note on liveness metrics:** of the four borrowed sections, repo liveness (commits, stars,
forks, last-commit age) is the only one with backend cost — a forge poller, a token, and a
rate-limit/cache story (spec §10.5). If it slips, the frontend must degrade to hiding those
signal-bar cells rather than rendering zeros.

**Charts:** v1 uses numeric signal cells only. If sparklines or activity charts are added
later, follow the `dataviz` guidance rather than inventing a palette.

---

## 10. Open questions for the frontend

1. Dark mode in v1, or light only? (Affects token setup and the whole component palette —
   cheap now, expensive to retrofit.)
2. Do we adopt a component library or hand-roll? Siblings hand-roll; this UI has more surface
   than they do.
3. Should the Software page show an aggregated Produces/Consumes derived from *observed runs*
   alongside the *declared* capability, and how do we render a disagreement between them?
4. How much of a peer's record do we render before resolution completes — a skeleton, or the
   bare IRI with a *Resolve now* action?

---

## 11. The answers those questions got

Recorded here rather than in the README, so the questions and their answers stay together.

1. **Dark mode in v1?** Yes — tokens in `frontend/src/styles.css`. Cheap now, expensive to
   retrofit, exactly as the question said.
2. **Component library?** Hand-rolled, matching the sibling repositories. About 10 KB of CSS
   and no dependency to track. The extra surface did not turn out to change the answer.
3. **Observed vs declared capability on the Software page?** Not built. The declared capability
   is shown; the observed one is one SPARQL query away. It should be added once there is enough
   run data for a disagreement between the two to mean something — showing a disagreement drawn
   from three runs would be noise presented as a finding.
4. **How much of an unresolved peer record to render?** The bare IRI, marked "not resolved yet",
   plus the origin chip. Never a skeleton: a skeleton promises content that may never arrive.

The liveness-metrics note above went the way it feared — repository *sync* is implemented,
repository *liveness metrics* are not, and the UI omits those cells. See
[Limitations](limitations.md).
