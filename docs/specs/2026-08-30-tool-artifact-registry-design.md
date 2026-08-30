# Tool Artifact Registry — Design

| | |
|---|---|
| **Status** | Draft for review |
| **Date** | 2026-08-30 |
| **Owner** | Ensar Emir Erol — MaastrichtU-IDS |
| **Frontend handoff** | [`docs/design-handoff.md`](../design-handoff.md) |

---

## 1. Problem

MaastrichtU-IDS runs a growing set of tools — `shacl-manager`, `sulo-schema-builder`,
`rdf_tx`, `obda-lazy-cache-demo` — across several deployments (the `ids3` and `idsg2`
clusters, partner sites, laptops). Each consumes and produces data artifacts: RDF graphs,
SHACL shapes, validation reports, OWL ontologies, mapping files, masked replicas.

Today there is no shared record of:

- what tools exist, who is responsible for them, under what licence;
- what a tool *can* consume and produce;
- what a given deployment *actually did* produce, and when;
- where those artifacts live and how to get at them;
- how an artifact in one institution's estate relates to one in another's.

Nothing outlives the person who ran the job. This document specifies a service that fixes
that, and that any third party can deploy for their own estate and cross-link with ours.

### 1.1 Requirements

Verbatim from the stakeholder:

1. Store in a graph (or NoSQL/SQL DB) — choose and justify.
2. Tool information (name, link, repo, licence, responsible party, …).
3. Artifacts each tool generates and consumes.
4. Endpoint to advertise artifacts a tool generates (with access ways, FAIR data).
5. Endpoint to advertise artifacts a tool consumed (or, tool sent to be consumed).
6. Deployable by many people: anyone can run their own artifact registry
   (containerised, minimal ops, sane defaults, Helm/compose).
7. Deployments must be referenceable by each other: a tool or artifact in one registry can
   point at one in another registry (stable global identifiers, federation/cross-links,
   discovery of peer registries).

### 1.2 Non-goals for v1

- Not an artifact store. The registry never holds artifact bytes (§3, D1).
- Not a workflow engine. It records what ran; it does not schedule or execute.
- Not multi-tenant. One registry serves one estate; multiple estates federate (§9).
- Not an access broker. It describes how to request access; it does not grant it.

---

## 2. Prior art

Three mature ecosystems exist. None covers the intersection this project needs. Building is
justified — reinventing their formats is not.

### 2.1 Software / tool registries

| System | What it does well | Why it is not enough |
|---|---|---|
| [bio.tools](https://bio.tools/) + [biotoolsSchema](https://github.com/bio-tools/biotoolsSchema) | 20k+ tools; 50+ curated attributes; EDAM-typed inputs/outputs per function; the reference vocabulary for describing a computational tool | Class-level I/O only. No artifact *instances*, no runs, no deployments, no federation of independent installs. |
| [Research Software Directory (RSD)](https://research-software-directory.org/documentation/rsd-instance/) | Self-hostable (Docker images per release), already run by NLeSC, Utrecht, Leiden, Amsterdam UMC; excellent citation and impact UX | No artifact model **at all**. Cannot express "this deployment produced this file". Instances are not modelled; federation is not a feature. |
| WorkflowHub, Software Heritage, OpenAIRE | Archival, citation, DOIs | Same gap: describe the software, not what it emitted. |

### 2.2 Data lineage platforms

| System | What it does well | Why it is not enough |
|---|---|---|
| [OpenLineage](https://openlineage.io/getting-started/) (Linux Foundation) | The standard event format for "job run X consumed dataset A, produced dataset B"; native emitters in Airflow, Spark, dbt, Flink, Dagster | Not RDF. No licence, no checksum, no DCAT distributions, no access protocol or auth descriptor, no dereferenceable identifiers, no capability declarations, no federation. |
| Marquez (OL reference impl.), DataHub, OpenMetadata, Apache Atlas, Egeria, Spline | Self-hostable lineage capture and visualisation | Assume a single enterprise data platform. No cross-instance federation, no persistent global identifiers, no tool-registry semantics (licence, repo, responsible party), no FAIR story. |

### 2.3 Catalogue federation

CKAN + DCAT-AP harvesting, OAI-PMH, and [Signposting](https://signposting.org) solve
federated catalogue discovery and machine-actionable FAIR navigation. None of them model
tools, deployments, or runs.

### 2.4 Conclusion

The gap is the **intersection**: an RDF-native, self-hostable, federatable registry in which
tool identity, declared capability, deployment instances, and instance-level produce/consume
lineage live in one graph under dereferenceable global IRIs. No existing system ships that.

**What we adopt rather than reinvent:** biotoolsSchema, DCAT 3, PROV-O, EDAM, SPDX,
FAIR Signposting, and OpenLineage (via a translating adapter, §7.6).

### 2.5 Why not just deploy RSD?

RSD is the closest existing system to requirement 6 and is institutionally adjacent to us.
It is rejected because requirements 3, 4, 5 and 7 — the core of this project — are entirely
absent from its data model, and retrofitting an artifact/run/instance graph onto a
PostgREST-over-Postgres schema designed for software cards is a larger job than building on a
triplestore that already speaks our vocabularies.

We keep the door open: `GET /api/v1/software/{id}/export/biotools` emits a
biotoolsSchema-conformant record, so our tool descriptions can populate a bio.tools or RSD
instance without a runtime dependency in either direction (§7.2).

---

## 3. Decisions

| # | Decision | Rationale | Rejected |
|---|---|---|---|
| D1 | **Metadata and pointers only.** The registry never stores artifact bytes. | Keeps ops minimal (req. 6), makes federation cheap, avoids storage scaling and quota/GC. Artifacts stay in the systems that own them. | Optional blob store; content-addressed pinning cache. Both add a storage dependency for marginal benefit at our scale. |
| D2 | **Identifiers: `{base_iri}/{kind}/{uuidv7}`**, HTTPS, dereferenceable, content-negotiated. | No central coordination, no collisions across peers, time-ordered keys, resolvable by any client. Human slugs exist as non-authoritative `schema:identifier`. | w3id.org (needs central coordination per namespace); readable slugs (rename breaks links, cross-peer collisions); DOI (cost, latency, not per-artifact viable). See §12 Q1 for a DOI overlay. |
| D3 | **Store: embedded Oxigraph (RDF) + SQLite (operational state).** | §5. | Fuseki/QLever (second container, JVM or index build); Postgres+JSONB (impedance mismatch with DCAT/PROV); Neo4j (non-standard serialisation, weak FAIR alignment). |
| D4 | **Stack: Rust — axum + oxigraph + sqlx.** | Single static binary, ~30 MB image, no runtime deps → best possible answer to "anyone can run their own" (req. 6). Matches `rdf_tx`. Oxigraph is a Rust crate, so the triplestore embeds rather than being a service. | Python/FastAPI (matches more sibling repos, but needs an external triplestore process). |
| D5 | **Four-layer model: Software → Release → Instance → Run.** | Runs belong to a *deployment*, not to abstract software. Two sites running `shacl-manager` are two Instances of one Software; that is the join key across registries. | Collapsing Instance into Software (cannot attribute runs or endpoints); collapsing Release into Software (no image digest provenance). |
| D6 | **Capability (class-level) and Run (instance-level) are both first-class.** | Capability answers "what can produce a SHACL report?" — discovery works before anything has run. Run answers "where did this come from, who used it?" — lineage and audit. | Runs only (cold-start discovery problem); capability only (loses the whole provenance story). |
| D7 | **Native JSON-LD/DCAT+PROV is the canonical API; OpenLineage is a translating adapter endpoint.** | OL covers the run-event skeleton and essentially nothing else we need — no licence, no distributions, no checksum, no access protocol, no capabilities, no resolvable IRIs (see §7.6 gap table). Canonical-native keeps semantics clean; the adapter still buys free Airflow/dbt/Spark integration. | OL as the sole wire format (our FAIR fields would live in a non-standard facet and `Capability` would have no home). |
| D8 | **Auth: API tokens per Instance + OIDC for humans.** Anonymous read by default. | The advertising party *is* a deployment; a token that identifies an Instance is exactly the authorisation and attribution primitive the advertise endpoints need. Keycloak already runs on `ids3`. | Open write (unusable as a record of authority); signed advertisements (deferred, §12 Q2). |
| D9 | **Federation: cross-link + lazy resolve, opt-in peer list.** | Advertisement never blocks on network. No global consensus, no harvest storage, no staleness reconciliation. Peers are added deliberately — the trust boundary stays manual. | Harvest/mirror (storage + conflict rules); live fan-out only (availability hostage to slowest peer). |
| D10 | **Artifacts are immutable once advertised.** Corrections and new versions mint a new IRI linked by `prov:wasRevisionOf` / `dct:isVersionOf` to a version-series concept IRI. | A lineage edge that can silently change meaning is worthless. Mirrors Zenodo's concept-DOI/version-DOI split. | Mutable artifact records with in-place edits. |
| D11 | **`ArtifactType` is any IRI.** EDAM is preloaded and is the recommended default. | Life-science typing must not be a hard dependency for non-bio artifacts (SHACL shapes, OBDA mappings). | EDAM-only (excludes our own artifact kinds). |

---

## 4. Data model

### 4.1 Layers

```
Software        shacl-manager                       abstract; repo, licence, party
  └─ Release      v2.1  (image sha256:ab12…)        a versioned, runnable plan
       └─ Instance  shacl.ids.unimaas.nl            a deployment; agent that acts
            └─ Run    01J9F…  2026-08-30T14:02Z     one execution
                 ├─ used       → Artifact           (consume advertisement)
                 └─ generated  → Artifact           (produce advertisement)
                                    └─ Distribution  access descriptors
```

### 4.2 Entities

| Entity | RDF type(s) | Key fields |
|---|---|---|
| `Registry` | `dcat:Catalog` | base IRI, title, operator, software version, SPARQL URL, peers |
| `Agent` | `schema:Person`, `schema:Organization`, `prov:Agent` | name, ORCID / ROR, email, homepage |
| `Software` | `schema:SoftwareApplication`, `schema:SoftwareSourceCode` | name, description, homepage, `codeRepository`, `dct:license` (SPDX IRI), `applicationCategory`, EDAM topics, keywords, maturity, `dct:publisher`, contact `Agent` |
| `Release` | `schema:SoftwareApplication` (versioned), `prov:Plan` | `schema:softwareVersion`, `schema:datePublished`, container image ref + digest, changelog URL, `dct:isVersionOf` → Software |
| `Instance` | `prov:SoftwareAgent`; **also** `dcat:DataService` when it serves an endpoint | label, `tar:runsRelease` → Release, operator `Agent`, `dcat:endpointURL`, `dcat:endpointDescription` (OpenAPI/service description), `tar:availability`, jurisdiction, health, home registry |
| `ArtifactType` | `skos:Concept` (any IRI; EDAM preloaded) | label, definition, default media type |
| `Capability` | `prov:Plan`, `tar:Capability` | `tar:produces` → ArtifactType[], `tar:consumes` → ArtifactType[], declared at Software or Release; an Instance may narrow it |
| `Artifact` | `dcat:Dataset`, `prov:Entity` | title, description, `dct:conformsTo` → ArtifactType, keywords, `dct:license`, `dct:issued`, `prov:wasDerivedFrom`, `prov:wasRevisionOf`, `dct:isVersionOf` → version-series IRI |
| `Distribution` | `dcat:Distribution` | see §6.1 |
| `Run` | `prov:Activity` | `prov:startedAtTime`, `prov:endedAtTime`, `tar:status`, `prov:qualifiedAssociation`, `prov:used`, external run key |
| `Peer` | `dcat:Catalog` (foreign) | base IRI, title, last seen, resolve status |
| `ApiToken` | *(SQLite only, never in RDF)* | hash, Instance, scopes, expiry, created-by |

### 4.3 Relations

```turtle
@prefix tar:  <https://w3id.org/tar/ns#> .

<Release>   dct:isVersionOf        <Software> .
<Software>  tar:hasCapability      <Capability> .
<Capability> tar:produces          <ArtifactType> ;
             tar:consumes          <ArtifactType> .

<Instance>  a                      prov:SoftwareAgent ;
            tar:runsRelease        <Release> ;
            dct:publisher          <Agent> ;
            dcat:endpointURL       <https://shacl.ids.unimaas.nl> .

<Run>       a                      prov:Activity ;
            prov:qualifiedAssociation [ prov:agent   <Instance> ;
                                        prov:hadPlan <Release> ] ;
            prov:used              <Artifact_in> .          # consumed
<Artifact_out> prov:wasGeneratedBy <Run> .                  # produced

<Artifact>  dcat:distribution      <Distribution> ;
            dct:conformsTo         <ArtifactType> ;
            prov:wasDerivedFrom    <https://peer.example.org/artifact/01J7…> .
```

The `prov:qualifiedAssociation` form is deliberate: it binds *who acted* (the Instance) and
*what plan they followed* (the Release) in one reified node, which is exactly PROV's
intended use and lets a run be attributed even when the Release is unknown.

**Federation touches the model in exactly one place: any object position may hold a foreign
IRI.** There is no "remote artifact" type. A cross-registry lineage edge is an ordinary triple.

### 4.4 Identifiers

```
{TAR_BASE_IRI}/software/{uuidv7}
{TAR_BASE_IRI}/release/{uuidv7}
{TAR_BASE_IRI}/instance/{uuidv7}
{TAR_BASE_IRI}/artifact/{uuidv7}
{TAR_BASE_IRI}/artifact-series/{uuidv7}     # version concept IRI (D10)
{TAR_BASE_IRI}/run/{uuidv7}
{TAR_BASE_IRI}/type/{uuidv7}                # local ArtifactTypes only; EDAM keeps its own IRIs
```

Every IRI dereferences with content negotiation: `text/turtle`, `application/ld+json`,
`application/json` (a flattened developer-facing shape), `text/html` (the UI page).

### 4.5 Worked example

```turtle
<https://reg.ids.unimaas.nl/software/01J8A…>
    a schema:SoftwareApplication ;
    schema:name "shacl-manager" ;
    schema:codeRepository <https://github.com/MaastrichtU-IDS/shacl-manager> ;
    dct:license <https://spdx.org/licenses/Apache-2.0> ;
    dct:publisher <https://ror.org/02jz4aj89> ;
    tar:hasCapability [
        tar:consumes <http://edamontology.org/data_2600> ,
                     <https://reg.ids.unimaas.nl/type/01J8B…> ;   # SHACL shapes graph
        tar:produces <http://edamontology.org/data_2048> ] .

<https://reg.ids.unimaas.nl/instance/01J8C…>
    a prov:SoftwareAgent, dcat:DataService ;
    rdfs:label "shacl.ids.unimaas.nl" ;
    tar:runsRelease <https://reg.ids.unimaas.nl/release/01J8D…> ;
    dcat:endpointURL <https://shacl.ids.unimaas.nl> .

<https://reg.ids.unimaas.nl/run/01J9F…>
    a prov:Activity ;
    prov:startedAtTime "2026-08-30T14:02:11Z"^^xsd:dateTime ;
    tar:status "success" ;
    prov:qualifiedAssociation [ prov:agent   <https://reg.ids.unimaas.nl/instance/01J8C…> ;
                                prov:hadPlan <https://reg.ids.unimaas.nl/release/01J8D…> ] ;
    prov:used <https://reg.mumc.nl/artifact/01J7Z…> .            # foreign input

<https://reg.ids.unimaas.nl/artifact/01J9G…>
    a dcat:Dataset ;
    dct:title "Validation report — patients.ttl vs fhir-shapes v3" ;
    dct:conformsTo <http://edamontology.org/data_2048> ;
    dct:license <https://spdx.org/licenses/CC-BY-4.0> ;
    prov:wasGeneratedBy <https://reg.ids.unimaas.nl/run/01J9F…> ;
    dcat:distribution [
        a dcat:Distribution ;
        dcat:accessURL <https://shacl.ids.unimaas.nl/reports/9f2a> ;
        dcat:mediaType "text/turtle" ;
        dcat:byteSize 2118342 ;
        spdx:checksum [ spdx:algorithm spdx:checksumAlgorithm_sha256 ;
                        spdx:checksumValue "9f2a…" ] ;
        tar:accessProtocol "https" ;
        tar:authMethod     "apikey" ;
        tar:availability   "restricted" ] .
```

---

## 5. Store choice

### 5.1 Decision

**Embedded [Oxigraph](https://github.com/oxigraph/oxigraph) for the RDF domain graph, plus
SQLite (via `sqlx`) for operational state.** Both embed in the single Rust binary. The default
deployment is one container and one volume.

| Data | Store | Why |
|---|---|---|
| Software, Releases, Instances, Capabilities, Artifacts, Distributions, Runs, peer stubs | Oxigraph | Native RDF. Zero impedance mismatch with DCAT/PROV-O/biotoolsSchema. SPARQL 1.1 becomes a public API for free. Open-world extension without migrations. |
| API tokens, OIDC sessions, peer sync cursors and backoff, audit log, resolve cache TTLs, background job state, rate-limit counters | SQLite | Relational, transactional, frequently mutated, and must never be exposed via the public SPARQL endpoint. Secrets do not belong in a queryable graph. |

### 5.2 Justification against the requirements

- **Req. 1 (graph):** the domain genuinely is a graph — lineage traversal, cross-registry
  cross-links, typed relations. Modelling it as RDF also *is* the interoperability story;
  a relational schema would need a serialisation layer to reach the same place.
- **Req. 6 (minimal ops):** no external database process. No JVM (Fuseki), no index build
  step (QLever), no separate Postgres. `docker run -v tar-data:/data …` is a complete install.
- **Req. 7 (federation):** SPARQL `SERVICE` gives federated query to power users at no
  implementation cost, and peer graphs isolate foreign data cleanly (§5.4).

### 5.3 Costs, honestly

| Cost | Mitigation |
|---|---|
| Oxigraph is single-writer; no clustering | Catalogue-scale workload. Writes are advertisements, not a data plane. Deployment is 1 replica by design (§10). |
| No relational constraints on the graph | SHACL shapes ship with the registry and validate every write before commit (`TAR_SHACL_VALIDATE_WRITES`, default on). Dogfoods `shacl-manager`'s shapes. |
| SPARQL pagination is awkward | The REST API owns pagination via SQLite-backed keyset cursors; SPARQL is for ad-hoc analytical use, not the UI. |
| Ceiling if an estate outgrows embedded storage | All graph access sits behind a `GraphStore` trait. A `RemoteSparqlStore` implementation (Fuseki / QLever / GraphDB) is a config switch, not a rewrite. This is a v1 structural requirement, not a v2 aspiration. |

### 5.4 Named graphs

```
<urn:tar:local>        authoritative — triples this registry minted
<urn:tar:peer:{id}>    cached foreign stubs, read-only
<urn:tar:shapes>       SHACL shapes used for write validation
<urn:tar:vocab>        preloaded EDAM / SPDX / DCAT terms
```

Provenance of every triple is recoverable by construction; foreign data can never be mistaken
for local authority; evicting a peer is `DROP GRAPH <urn:tar:peer:{id}>`.

---

## 6. FAIR metadata and access descriptors

### 6.1 `dcat:Distribution` fields

| Field | Vocabulary | Notes |
|---|---|---|
| `dcat:accessURL` | DCAT | Landing page or service entry point |
| `dcat:downloadURL` | DCAT | Direct bytes, when they exist |
| `dcat:mediaType` / `dct:format` | DCAT / DCTERMS | IANA media type |
| `dct:conformsTo` | DCTERMS | SHACL shape or profile IRI the bytes conform to |
| `dcat:byteSize` | DCAT | |
| `spdx:checksum` | SPDX | algorithm + value; `sha256` recommended |
| `dct:license`, `dct:rights`, `odrl:hasPolicy` | DCTERMS / ODRL | |
| `dcat:accessService` | DCAT | → a `dcat:DataService` (SPARQL endpoint, S3 bucket, OGC API) |
| `tar:accessProtocol` | ours | `https` \| `s3` \| `sparql` \| `oci` \| `ipfs` \| `file` |
| `tar:authMethod` | ours | `none` \| `apikey` \| `oauth2` \| `basic` \| `signed-url` |
| `tar:availability` | ours | `public` \| `restricted` \| `embargoed` \| `metadata-only` |
| `tar:accessRequestURL` | ours | Where to request access when not `public` |

### 6.2 `metadata-only` is the common case, not an edge case

For IDS's health-data work, most artifacts must be **findable and described but not
retrievable**. `tar:availability = metadata-only` means: the registry advertises that the
artifact exists, its type, its shape, its provenance chain, its responsible party, and
`tar:accessRequestURL` — and carries no `downloadURL` at all. FAIR is not open, and the model
says so structurally rather than by convention.

### 6.3 Signposting

Every artifact and software `GET` emits [Signposting](https://signposting.org) `Link` headers:

```http
Link: <https://reg.ids.unimaas.nl/artifact/01J9G…>          ; rel="cite-as"
Link: <https://reg.ids.unimaas.nl/artifact/01J9G….ttl>      ; rel="describedby"; type="text/turtle"
Link: <https://reg.ids.unimaas.nl/artifact/01J9G….jsonld>   ; rel="describedby"; type="application/ld+json"
Link: <http://edamontology.org/data_2048>                    ; rel="type"
Link: <https://shacl.ids.unimaas.nl/reports/9f2a>            ; rel="item"; type="text/turtle"
Link: <https://spdx.org/licenses/CC-BY-4.0>                  ; rel="license"
Link: <https://orcid.org/0000-…>                             ; rel="author"
Link: <https://reg.ids.unimaas.nl/api/v1/registry>           ; rel="collection"
```

`metadata-only` artifacts omit `rel="item"` and add `rel="describedby"` only — a client can
tell the difference between "no bytes here" and "bytes behind auth" without parsing the body.

### 6.4 FAIR principle coverage

| Principle | Mechanism |
|---|---|
| F1 unique persistent ID | UUIDv7 HTTPS IRIs (D2); registry-of-mint is authoritative (§9.7) |
| F2 rich metadata | DCAT + PROV-O + biotoolsSchema + EDAM |
| F3 metadata includes ID of data | `dcat:distribution` → `accessURL`/`downloadURL` |
| F4 registered/indexed | The registry itself; `/api/v1/search`; peer federation |
| A1 retrievable by ID over open protocol | HTTPS content negotiation on every IRI |
| A1.2 auth where necessary | `tar:authMethod`, `tar:accessRequestURL` |
| A2 metadata persists when data does not | Metadata is independent of the bytes; `metadata-only` and tombstoned distributions remain |
| I1 formal knowledge representation | RDF / Turtle / JSON-LD |
| I2 FAIR vocabularies | EDAM, SPDX, DCAT, PROV-O, SKOS, ODRL |
| I3 qualified references | `prov:wasDerivedFrom`, `prov:used`, `prov:qualifiedAssociation` |
| R1.1 clear licence | `dct:license` on Software, Artifact and Distribution |
| R1.2 provenance | The Run graph — this is the project's core contribution |
| R1.3 community standards | biotoolsSchema export; DCAT-AP-compatible catalogue |

---

## 7. API surface

Base path `/api/v1`. Every `GET` on a resource honours `Accept` for `text/turtle`,
`application/ld+json`, `application/json`, `text/html`, and emits Signposting headers.

### 7.1 Identity and discovery

```
GET  /.well-known/tar-registry     registry self-description (JSON-LD)
GET  /api/v1/registry              dcat:Catalog record
GET  /healthz  /readyz  /metrics   liveness, readiness, Prometheus
```

### 7.2 Software and releases

```
POST   /api/v1/software                        register
GET    /api/v1/software                        list; q, license, publisher, edam_topic, keyword, kind
GET    /api/v1/software/{id}
PATCH  /api/v1/software/{id}
DELETE /api/v1/software/{id}                   soft delete (tombstone; IRI keeps resolving)
POST   /api/v1/software/{id}/releases
GET    /api/v1/software/{id}/releases
GET    /api/v1/software/{id}/export/biotools   biotoolsSchema JSON (§2.5)
```

### 7.3 Capabilities

```
PUT  /api/v1/software/{id}/capability          declare produces[] / consumes[]
PUT  /api/v1/instances/{id}/capability         narrow the inherited declaration
GET  /api/v1/capabilities?produces={typeIRI}&consumes={typeIRI}
```

`GET /api/v1/capabilities` is the matchmaking endpoint: *"what can consume what
shacl-manager emits?"* It answers before any run exists, which is why D6 keeps it separate
from lineage.

### 7.4 Instances

```
POST   /api/v1/instances                       register a deployment
GET    /api/v1/instances                       list; software, operator, status, release, registry
GET    /api/v1/instances/{id}
PATCH  /api/v1/instances/{id}
POST   /api/v1/instances/{id}/tokens           mint a scoped API token
GET    /api/v1/instances/{id}/runs
GET    /api/v1/instances/{id}/artifacts
```

### 7.5 Artifacts, runs, advertisement

```
POST /api/v1/artifacts                         register an artifact + distributions
GET  /api/v1/artifacts/{id}
GET  /api/v1/artifacts/{id}/lineage?depth=&direction=up|down|both
GET  /api/v1/runs/{id}

POST /api/v1/advertise/produced                requirement 4
POST /api/v1/advertise/consumed                requirement 5
```

Both advertisement endpoints are **idempotent on `(run_key, artifact_iri, role)`** — a retried
CI step does not duplicate lineage. Both accept foreign IRIs in artifact position; that is how
cross-registry lineage forms with no coordination.

`POST /api/v1/advertise/produced`:

```json
{
  "run": {
    "external_key": "gh-actions/12345/attempt-1",
    "started_at": "2026-08-30T14:02:11Z",
    "ended_at":   "2026-08-30T14:02:49Z",
    "status": "success",
    "release": "https://reg.ids.unimaas.nl/release/01J8D…"
  },
  "artifacts": [{
    "title": "Validation report — patients.ttl vs fhir-shapes v3",
    "conforms_to": "http://edamontology.org/data_2048",
    "license": "https://spdx.org/licenses/CC-BY-4.0",
    "keywords": ["shacl", "validation", "fhir"],
    "was_derived_from": ["https://reg.mumc.nl/artifact/01J7Z…"],
    "distributions": [{
      "access_url":   "https://shacl.ids.unimaas.nl/reports/9f2a",
      "download_url": "https://shacl.ids.unimaas.nl/reports/9f2a.ttl",
      "media_type": "text/turtle",
      "byte_size": 2118342,
      "checksum": { "algorithm": "sha256", "value": "9f2a…" },
      "conforms_to": "https://reg.ids.unimaas.nl/shapes/validation-report",
      "access_protocol": "https",
      "auth_method": "apikey",
      "availability": "restricted",
      "access_request_url": "https://ids.unimaas.nl/data-access"
    }]
  }]
}
```

Response `201`:

```json
{
  "run": "https://reg.ids.unimaas.nl/run/01J9F…",
  "artifacts": ["https://reg.ids.unimaas.nl/artifact/01J9G…"],
  "created": true
}
```

`POST /api/v1/advertise/consumed` takes the same `run` block and an `artifacts` array whose
entries are **either** a bare reference or a full inline registration:

```json
{
  "run": { "external_key": "gh-actions/12345/attempt-1" },
  "artifacts": [
    { "iri": "https://reg.mumc.nl/artifact/01J7Z…" },
    { "title": "local input graph", "conforms_to": "http://edamontology.org/data_2600",
      "distributions": [{ "download_url": "s3://ids-bucket/in.ttl", "access_protocol": "s3" }] }
  ]
}
```

An unknown foreign IRI is stored verbatim and queued for background resolution. **The
advertisement never blocks on the network** (§9.4).

The Instance is derived from the presenting token, never from the payload — see §8.3.

### 7.6 OpenLineage adapter

```
POST /api/v1/openlineage                       accepts an OpenLineage RunEvent
```

Rationale for the adapter-not-canonical shape (D7). OL coverage of our required fields:

| Requirement | OpenLineage |
|---|---|
| Run id, timestamps, state, job | covered — `RunEvent`, `job`, `nominalTime` |
| Repository link | covered — `sourceCodeLocation` job facet |
| Responsible party | covered — `ownership` facet |
| Storage format | partial — `storage` facet, `fileFormat` only |
| Alternate identifiers | partial — `symlinks` facet |
| Licence (SPDX) | **absent** |
| DCAT distribution set (accessURL vs downloadURL, mediaType, conformsTo, multiple distributions) | **absent** |
| Checksum | **absent** |
| Access protocol + auth method | **absent** |
| Dereferenceable global IRIs | **absent** — OL identifies datasets by `(namespace, name)` strings by design |
| Capability declarations | **absent** — OL is run-events only |
| Tool registry metadata (homepage, keywords, EDAM, citation, funding) | **absent** |
| Federation / peers / cross-links | **absent** |

Mapping performed on ingest:

| OpenLineage | Tool Artifact Registry |
|---|---|
| `run.runId` | `Run` `tar:externalKey`; a local UUIDv7 IRI is minted |
| `eventTime` + `eventType` | `prov:startedAtTime` / `prov:endedAtTime`; `COMPLETE`→success, `FAIL`/`ABORT`→failed |
| `job.namespace` | `Instance` — resolved from the presenting token; payload value recorded as a label only |
| `job.name` | run label; matched against `Release` when recognisable |
| `job.facets.sourceCodeLocation` | `Software` `schema:codeRepository`; creates a stub Software if unknown |
| `job.facets.ownership` | `dct:publisher` |
| `inputs[]` | `prov:used` |
| `outputs[]` | `prov:wasGeneratedBy` |
| `dataset.namespace` + `.name` | `Artifact` `tar:externalKey` (identity key for idempotency) |
| `dataset.facets.symlinks` | if a `tar:` IRI appears here, the artifact is matched to it instead of minted |
| `dataset.facets.storage.fileFormat` | `dcat:mediaType`, best effort |
| `dataset.facets.dataSource.uri` | `dcat:accessURL` |
| `dataset.facets.fairAccess` *(custom)* | full `Distribution` per §6.1, when the producer supplies it |
| everything else | preserved verbatim as a JSON literal on the Run (`tar:openLineagePayload`) so nothing is lost |

### 7.7 Query

```
GET  /api/v1/search?q=&type=&federated=false   cross-entity, faceted
GET  /api/v1/graph?iri=&depth=                 subgraph for UI rendering
POST /sparql                                   read-only SPARQL 1.1 (Oxigraph)
```

`POST /sparql` is a first-class surface, not a bonus: it gives analysts and peer registries a
standard federated query language without us designing one.

### 7.8 Federation

```
GET    /api/v1/peers
POST   /api/v1/peers                           admin: add by base URL, validated via well-known
DELETE /api/v1/peers/{id}                      DROP GRAPH <urn:tar:peer:{id}>
POST   /api/v1/peers/announce                  inbound mutual-discovery announcement
GET    /api/v1/peers/suggested                 peers-of-peers, for admin review
GET    /api/v1/resolve?iri=                    resolve a foreign IRI, cache a stub, return it
```

### 7.9 Errors

RFC 9457 `application/problem+json` throughout. SHACL write-validation failures return `422`
with the validation report embedded as `text/turtle` in a `report` member — the same report
format `shacl-manager` emits, so tooling is shared.

---

## 8. Auth model

### 8.1 Principals

| Principal | Credential | Typical use |
|---|---|---|
| Instance | Bearer API token, scoped | CI job or running service advertising produce/consume |
| Human | OIDC (Keycloak on `ids3`); roles `reader` / `curator` / `admin` | UI: register software, mint tokens, manage peers |
| Anonymous | none | Read, unless `TAR_PUBLIC_READ=false` |

### 8.2 Scopes

`advertise:produce`, `advertise:consume`, `register:software`, `register:instance`,
`read:private`, `admin:*`. Tokens are minted per Instance, stored as Argon2id hashes in
SQLite, shown once, revocable, optionally expiring.

Bootstrap: `TAR_ROOT_TOKEN` on first boot creates the initial admin; the registry refuses to
start with a default or empty value.

### 8.3 Core authorisation rule

> **An Instance may only advertise runs in which it is itself the agent.**

The Instance is taken from the presenting token and never from the request body. No principal
can forge another deployment's lineage. Any `job.namespace` or instance field in a payload is
retained as a label for debugging and ignored for authorisation.

Software records are editable by their creator or by a `curator`; ownership transfer is an
admin action. Every write records `prov:wasAttributedTo` in the graph and an append-only row
in the SQLite audit log (who, when, what, from where).

### 8.4 Federation trust

Peer data is **always** a read-only stub in `<urn:tar:peer:{id}>` and is never merged into
`<urn:tar:local>`. A peer cannot create, modify, or delete local records. Inbound
`/api/v1/peers/announce` only produces a *suggestion* for admin review — never an
auto-added peer.

---

## 9. Federation model

1. **Self-description.** `/.well-known/tar-registry` returns JSON-LD: base IRI, title,
   operator, software version, public-read flag, SPARQL URL, capabilities, and the peer list.
2. **Adding a peer.** An admin `POST`s a base URL. The registry fetches the well-known
   document, validates it, checks the advertised base IRI matches, and stores the peer.
   Optionally it `POST`s `/api/v1/peers/announce` back for mutual discovery.
3. **Cross-linking needs no resolution.** An unknown foreign IRI in any object position is
   stored verbatim. Advertisement latency is never coupled to peer availability.
4. **Lazy resolution.** A background worker dereferences unknown foreign IRIs with
   `Accept: text/turtle`, writes a minimal stub (type, title, publisher, home registry) into
   the peer graph, and caches with a TTL (`TAR_PEER_RESOLVE_TTL`, default 24 h). Failures back
   off exponentially and are visible in the peer admin UI.
5. **Peers of peers.** The resolver reads a peer's advertised peer list and surfaces them at
   `/api/v1/peers/suggested`. They are never auto-added — the trust boundary stays manual (D9).
6. **Federated search.** `?federated=true` fans out to peers' `/api/v1/search` with
   `TAR_FEDERATED_SEARCH_TIMEOUT` (default 3 s), returning partial results with a
   `partial: true` flag and a per-peer status list. Power users use SPARQL `SERVICE`.
7. **Conflict rule.** The registry that minted an IRI is authoritative for it. Stubs never
   overwrite local triples. If two registries describe the same Software, the Software IRIs
   differ and are linked by `owl:sameAs` / `schema:sameAs` — set deliberately by a curator,
   never inferred.

---

## 10. Deployment

### 10.1 Artifact

One statically linked Rust binary in a distroless image (~30 MB). Volumes: `/data` (Oxigraph)
and `/data/ops.db` (SQLite).

### 10.2 Compose

```yaml
services:
  registry:
    image: ghcr.io/maastrichtu-ids/tool-artifact-registry:0.1.0
    environment:
      TAR_BASE_IRI: https://reg.example.org
      TAR_ROOT_TOKEN: ${TAR_ROOT_TOKEN:?set me}
    ports: ["8080:8080"]
    volumes: ["tar-data:/data"]
    healthcheck:
      test: ["CMD", "/tar", "healthcheck"]
volumes: { tar-data: }
```

One service. That is the whole minimal install (req. 6).

### 10.3 Helm

Chart at `deploy/helm/tool-artifact-registry`: Deployment (**1 replica — Oxigraph is
single-writer**, `strategy: Recreate`), RWO PVC, Service, Ingress, ConfigMap, Secret,
ServiceMonitor. Probes on `/healthz` and `/readyz`.

### 10.4 ids3

Kustomize overlay at `services/ids3/projects/tool-artifact-registry/` following the existing
`_base` project pattern; an ArgoCD Application; Vault + VSO for `TAR_ROOT_TOKEN` and the OIDC
client secret; egress through `egress-proxy` for peer resolution and forge polling; Harbor for
the image; Traefik for ingress and TLS.

### 10.5 Configuration

| Variable | Default | Notes |
|---|---|---|
| `TAR_BASE_IRI` | *(required)* | The only mandatory setting — IRIs cannot be minted without it. Changing it after data exists requires a documented rebase migration. |
| `TAR_ROOT_TOKEN` | *(required on first boot)* | Refuses empty/default |
| `TAR_DATA_DIR` | `/data` | |
| `TAR_LISTEN` | `0.0.0.0:8080` | |
| `TAR_PUBLIC_READ` | `true` | |
| `TAR_OIDC_ISSUER` / `_CLIENT_ID` / `_CLIENT_SECRET` | unset | OIDC disabled when unset |
| `TAR_SHACL_VALIDATE_WRITES` | `true` | |
| `TAR_PEER_RESOLVE_ENABLED` | `true` | |
| `TAR_PEER_RESOLVE_TTL` | `24h` | |
| `TAR_PEER_RESOLVE_TIMEOUT` | `5s` | |
| `TAR_FEDERATED_SEARCH_TIMEOUT` | `3s` | |
| `TAR_FORGE_TOKEN` | unset | GitHub/GitLab token for repo liveness metrics |
| `TAR_FORGE_POLL_INTERVAL` | `24h` | |
| `TAR_MAX_PAYLOAD_BYTES` | `2MiB` | |

Everything but `TAR_BASE_IRI` and `TAR_ROOT_TOKEN` has a working default (req. 6, sane defaults).

### 10.6 Operations

- **Backup:** `GET /admin/dump` streams N-Quads (all graphs, or `?graph=`); SQLite via `.backup`.
- **Restore:** `tar restore --nquads dump.nq --ops ops.db`.
- **Upgrade:** graph migrations are additive SPARQL Updates shipped per release and applied
  idempotently on boot; SQLite via `sqlx migrate`.
- **Scaling:** read-heavy growth is answered by the `GraphStore` trait and a remote-SPARQL
  backend (§5.3), not by replicas.

### 10.7 Seed data

`tar seed --from ids-examples` registers the sibling repos as Software with declared
capabilities, so a fresh install is demonstrable immediately:

| Software | Consumes | Produces |
|---|---|---|
| `shacl-manager` | RDF graph, SHACL shapes graph | SHACL ValidationReport, conformance summary |
| `sulo-schema-builder` | schema model, SULO ontology | RDF/Turtle, OWL+SULO, SHACL shapes, Mermaid UML |
| `rdf_tx` | SPARQL update, RDF quads | hash-chained patch log, masked RDF replica |
| `obda-lazy-cache-demo` | relational source, R2RML/RML mapping | materialised RDF view, mapping coverage report |

---

## 11. Frontend

v1 covers browse/search, registration and editing, and peer administration. Layout is
two-column with a sticky right rail; `Instances` is a top-level tab alongside `Software`,
`Artifacts`, `Runs` and `Peers`. Lineage *graph* visualisation is deferred to v2 — v1 renders
the same data as tables.

Full screen inventory, routes, component list, API contracts per screen, and states:
[`docs/design-handoff.md`](../design-handoff.md).

---

## 12. Open questions

| # | Question | Notes |
|---|---|---|
| Q1 | Do we mint DOIs for artifacts or software releases? | Requires DataCite membership and cost. Would sit as an overlay on the UUIDv7 IRIs (D2), not a replacement. Blocks nothing in v1. |
| Q2 | When do we add cryptographically signed advertisements? | Rejected for v1 (D8) on key-distribution cost. Becomes important the moment a peer registry we do not operate can influence our lineage view. |
| Q3 | Retention and GC for stale peer stubs. | Currently TTL-refreshed forever. Do stubs for a peer that has been unreachable for 90 days get dropped, tombstoned, or kept? |
| Q4 | Multi-tenancy inside one registry. | Out of scope for v1 (§1.2). Confirm no IDS use case needs it before that ossifies — retrofitting tenancy onto named graphs is expensive. |
| Q5 | Should `Capability` eventually be a SHACL shape rather than an `ArtifactType` chip? | Far more precise matchmaking ("consumes graphs conforming to *this* shape"). Natural v2, and `shacl-manager` already has the machinery. |
| Q6 | Is SHACL write-validation blocking or advisory? | Spec assumes blocking (`422`). Advisory may be better for adoption — a half-described artifact is more useful than a rejected one. |
| Q7 | Project licence and repository home. | Assumed Apache-2.0 under `MaastrichtU-IDS`, matching siblings. Confirm. |
| Q8 | Rate limiting and abuse controls for a public instance. | Not designed yet. Needed before any registry is exposed to the open internet with `TAR_PUBLIC_READ=true`. |
| Q9 | `TAR_BASE_IRI` change after data exists. | A rebase migration is named in §10.5 but not specified. Needed before the first production deployment moves domain. |

---

## 13. Out of scope for v1

Artifact byte storage; workflow execution; access granting/brokering; multi-tenancy;
lineage graph visualisation; harvest-based federation; DOI minting; signed advertisements;
horizontal write scaling.
