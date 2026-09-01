# Demo — four IDS tools, one pizza

> **There are four demos in this directory.** This one loads a worked scenario as *data*.
> [`README-two-apps.md`](README-two-apps.md) runs two actual programs that coordinate through
> the registry — one advertises an OWL ontology, the other has a subscription that matches it,
> is notified, ingests it, and advertises what it derived. Start there if what you want to see
> is a tool reacting to another tool.
>
> [`run-workload-identity-demo.sh`](run-workload-identity-demo.sh) is the third, and the first
> where a deployment is not handed a registry API token: it exchanges a credential for its
> *own* identity provider and the registry works out which deployment that is.
>
> ```bash
> docker compose -f deploy/keycloak/compose.yaml up -d
> TAR_ROOT_TOKEN=… ./demo/run-workload-identity-demo.sh
> ```
>
> It ends by showing a *curator's* token being refused the same call — because advertising is
> something a deployment does about itself, and being trusted is not the same as being the
> thing that ran.
>
> [`run-two-credentials-demo.sh`](run-two-credentials-demo.sh) is the fourth, and the one to
> read if the question you actually have is *"how should my tool get in here, and what will
> that cost me?"* Two applications, already in the catalogue, acquire a deployment record by
> opposite routes and then trade artifacts through the registry:
>
> ```bash
> docker compose -f deploy/keycloak/compose.yaml up -d
> ./demo/run-two-credentials-demo.sh
> ```
>
> | | how the record was made | what the application holds | what rotation costs |
> |---|---|---|---|
> | `graph-publisher` | a curator, by hand, at `POST /api/v1/instances` | a registry API token, minted for it | a registry job, per deployment |
> | `shacl-manager` | the deployment itself, at `PUT /api/v1/instances/self` | a Keycloak client secret, and nothing from the registry | a Keycloak job; the registry holds nothing |
>
> Neither route is the better one. Tokens need no identity provider and are the only option
> when there isn't one; client credentials need Keycloak, and in return the registry never
> stores a secret for the deployment at all. What makes the second possible is one line on the
> *software* record — `registration_clients`, naming an OIDC client that may register
> deployments **of that software** — which is why the demo ends with that same credential being
> refused a deployment of the other application. It starts its own registry with its own data
> directory, deletes that directory on each run and says so, and it never reconfigures the
> Keycloak it reads tokens from.
>
> **Two records below are known to be modelled wrong and are left unfixed.** RDFCraft is
> registered as a deployable service producing RDF, and shacl-rust's own CI is bound as a
> deployment that validates third-party ontologies. Both were flagged by the repository owner;
> correcting them means deciding what they should say instead, which is not a demo author's
> call. Do not copy either as a pattern.

A worked scenario across four real tools, loaded entirely through the public HTTP API.

```bash
./demo/run-demo.sh              # start the stack, then load the story
./demo/run-demo.sh --no-stack   # load into a registry you are already running
./demo/run-demo.sh --down       # tear it all down
```

Then open <http://localhost:8080>.

| | |
|---|---|
| [`shacl-rust`](https://github.com/ensaremirerol/shacl-rust) | SHACL validator in Rust. Also the engine this registry validates its own writes with. |
| [`sulo-schema-builder`](https://github.com/MaastrichtU-IDS/sulo-schema-builder) | Design a schema, emit OWL, SHACL, Turtle and a Mermaid diagram aligned to SULO. |
| `ontoexplorer` | FAIR ontology repository — ingest by IRI, URL or upload; reason with ELK; semantic search. *(private repository)* |
| [`RDFCraft`](https://github.com/MaastrichtU-IDS/RDFCraft) | Map CSV/JSON to RDF through a GUI. |

## The story

```
        pizza.owl (upstream, public URL)
                    │  ingested by URL
                    ▼
        ┌───────────────────────────┐
        │  Pizza ontology           │  ← one artifact, two distributions
        │  https upstream + s3 copy │    (same sha256)
        └─────────────┬─────────────┘
                      │ prov:wasDerivedFrom
     ┌────────────┬───┴────────┬──────────────┐
     ▼            ▼            ▼              ▼
  inferred    embedding      VoID          DCAT record
  hierarchy   index          statistics    (SPARQL)
              (metadata-only)

  sulo-schema-builder ──▶ SHACL shapes ──┐
                                         ├──▶ shacl-rust ──▶ validation report
  Pizza ontology ────────────────────────┘

  menu.csv ──▶ RDFCraft ──▶ mapped RDF ──▶ ingested by OntoExplorer
```

Seven runs across four deployments, one of them failed. Every input a tool consumed and every
output it produced was advertised by the deployment itself, authenticated as itself.

---

## How an ingested or uploaded resource becomes an artifact

This is the part worth reading. The same question — *"a tool took in a file, what do I record?"* —
has three different right answers, and picking the wrong one either loses provenance or invents it.

### 1. Ingested from a URL, bytes unchanged → **one artifact, two distributions**

OntoExplorer fetched `pizza.owl` from a public URL and kept a copy in its own object store. The
bytes did not change. So this is **one** `dcat:Dataset` with **two** `dcat:Distribution` — the
upstream URL and the `s3://` object — because a distribution is *a way of obtaining a dataset*,
and there are now two ways of obtaining the same one.

```jsonc
{
  "title": "Pizza ontology (pizza.owl)",
  "conforms_to": ".../type/owl-ontology",
  "distributions": [
    { "title": "Upstream source",
      "download_url": "https://raw.githubusercontent.com/owlcs/pizza-ontology/master/pizza.owl",
      "access_protocol": "https", "auth_method": "none", "availability": "public",
      "checksum": { "algorithm": "sha256", "value": "0de2cd4d…" } },

    { "title": "OntoExplorer object store",
      "download_url": "s3://ontoexplorer-raw/ontologies/pizza/pizza.owl",
      "access_protocol": "s3",    "auth_method": "apikey", "availability": "restricted",
      "access_request_url": "https://onto.ids.unimaas.nl/access",
      "checksum": { "algorithm": "sha256", "value": "0de2cd4d…" } }
  ]
}
```

**The shared checksum is what licenses this modelling.** It is the evidence that the two
distributions are interchangeable. Without it you are asserting sameness you cannot back up, and
you should mint two artifacts instead.

Note what each distribution says on its own: the upstream copy is `public` with `auth: none`; the
object-store copy is `restricted` behind an API key and names where to request access. Same bytes,
different terms of access — which is exactly why they are distributions and not one flattened blob.

### 2. Ingestion produced something new → **a separate artifact, linked by `prov:wasDerivedFrom`**

The reasoner output, the embedding index, the VoID statistics and the DCAT record are not other
ways of getting the pizza ontology. They are new things that did not exist before the run. Each is
its own artifact pointing back:

```jsonc
{ "title": "Pizza ontology — inferred class hierarchy (ELK, OWL-EL)",
  "was_derived_from": ["https://reg.example.org/artifact/01a05…"] }
```

The test is simple: **would a byte-for-byte comparison against the source succeed?** If yes, it is a
distribution. If no, it is a derived artifact.

### 3. Uploaded from a laptop → **the object store is the only distribution**

Someone uploaded `restaurant-menu.ttl` through a web form. There is no upstream URL, so there is
nothing to record one. The object store is not a *copy* here — it is the original.

```jsonc
{ "title": "Restaurant menu vocabulary (uploaded)",
  "external_key": "ontoexplorer/upload/2026-08-30-emenu",
  "distributions": [
    { "download_url": "s3://ontoexplorer-raw/uploads/2026-08-30/restaurant-menu.ttl",
      "access_protocol": "s3", "availability": "restricted" }
  ] }
```

The run that created it carries who uploaded it and under what upload id. That is the whole
provenance that honestly exists, and the record should not imply more.

### 4. When the bytes are not retrievable at all → `metadata-only`

The embedding index lives in a pgvector table. There is no file to fetch and there never will be,
so it carries **no `downloadURL` at all** — not an empty one, not a broken one. The UI renders no
download control, and the Signposting headers omit `rel="item"`, so a machine can tell *"no bytes
here"* from *"bytes behind auth"* without parsing the body.

### 5. Not everything is an artifact

The tool logos and README files this demo publishes to its object store are **assets**, not
artifacts. They are pictures of software, with no provenance worth tracing and no consumer
downstream. They live on the `Software` record as `image` / `screenshots` — plain URLs the browser
loads. If it has no lineage and nothing derives from it, it does not belong in the artifact graph.

---

## The two object stores, and why they are not the same thing

| | holds | modelled as |
|---|---|---|
| the demo's asset bucket (`compose.demo.yaml`) | tool logos, screenshots, README files | plain URLs on a `Software` record |
| OntoExplorer's own MinIO | raw ontology files it ingested | `dcat:Distribution` with `access_protocol: s3`, on artifacts with full provenance |

The registry stores bytes in neither (spec D1). It records *where they are* and *how to get at
them*. That is the whole point: the artifacts stay in the systems that own them, and the registry
stays a catalogue rather than becoming a storage tier with quota problems.

---

## Things to try once it is loaded

```bash
# What can produce a SHACL shapes graph? What consumes one?
curl 'localhost:8080/api/v1/capabilities?produces=http://localhost:8080/type/shacl-shapes-graph'
curl 'localhost:8080/api/v1/capabilities?consumes=http://localhost:8080/type/shacl-shapes-graph'

# Everything derived from the pizza ontology, three hops out
curl 'localhost:8080/api/v1/artifacts/<id>/lineage?depth=3&direction=down'

# The same record as RDF, from the same URL the UI uses
curl -H 'Accept: text/turtle' localhost:8080/artifact/<id>

# Which artifacts are described but not retrievable
curl 'localhost:8080/api/v1/artifacts?availability=metadata-only'

# Every artifact that still has no licence — a FAIR gap, in one query
curl -s localhost:8080/sparql --data 'PREFIX dcat: <http://www.w3.org/ns/dcat#>
  PREFIX dct: <http://purl.org/dc/terms/>
  SELECT ?a WHERE { GRAPH ?g { ?a a dcat:Dataset . FILTER NOT EXISTS { ?a dct:license ?l } } }'
```

## Notes

- `ontoexplorer` is a private repository. Its record carries no repository link rather than a URL
  that would 404 for anyone else, and its README is fetched through `gh` when the person running
  the demo has access. The demo skips it otherwise and says so.
- `sulo-schema-builder` and `ontoexplorer` declare no licence. The registry records that as a
  `sh:Warning` and the UI says "licence not stated" — it neither blocks the write nor invents a
  licence. That is a real FAIR gap in the real repositories, left visible on purpose.
- Each deployment is bound to an OIDC client id, which is how it would authenticate in production.
  The demo also mints a registry token for each, because no identity provider runs here — see
  [`../docs/specs/2026-08-30-workload-identity-addendum.md`](../docs/specs/2026-08-30-workload-identity-addendum.md).
- Checksums, file sizes and the ontology itself are fetched live, so the provenance in the demo is
  true rather than decorative. Everything else — endpoints, S3 URIs, run timings — is invented, and
  the endpoints do not resolve.
