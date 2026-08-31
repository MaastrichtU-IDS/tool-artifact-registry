# Tool Artifact Registry

An RDF-native, self-hostable, federatable registry of **tools**, the **deployments** that run
them, the **runs** they perform, and the **data artifacts** those runs consume and produce.

- Design: [`docs/specs/2026-08-30-tool-artifact-registry-design.md`](docs/specs/2026-08-30-tool-artifact-registry-design.md)
- Workload identity (Keycloak): [`docs/specs/2026-08-30-workload-identity-addendum.md`](docs/specs/2026-08-30-workload-identity-addendum.md)
- Frontend handoff: [`docs/design-handoff.md`](docs/design-handoff.md)

**Status: working prototype.** Every endpoint in §7 of the spec is implemented and covered by
tests; the gaps are named in [Known gaps](#known-gaps) rather than hidden.

---

## Quick start

```bash
cargo build --release
cd frontend && npm install && npm run build && cd ..

export TAR_BASE_IRI=http://127.0.0.1:8080
export TAR_ROOT_TOKEN=$(openssl rand -hex 24)
export TAR_DATA_DIR=./data

./target/release/tar seed --from ids-examples   # 4 tools, 5 deployments, runs, artifacts
./target/release/tar serve
```

Then open <http://127.0.0.1:8080>. Everything reads anonymously; sign in with the root token to
register or edit.

With Docker:

```bash
docker compose up --build
```

---

## What it does

**Four layers, because runs belong to a deployment and not to abstract software.**

```
Software        shacl-manager                    abstract; repo, licence, responsible party
  └─ Release      v2.1 (image sha256:ab12…)      a versioned, runnable plan
       └─ Instance  shacl.ids.unimaas.nl         a deployment; the agent that acts
            └─ Run    01J9F…                     one execution
                 ├─ used       → Artifact        consume advertisement
                 └─ generated  → Artifact        produce advertisement
                                    └─ Distribution   how to get at it, or how to ask
```

**Capability and lineage are both first-class.** A capability declaration answers *"what can
produce a SHACL report?"* before anything has ever run; the run graph answers *"where did this
file come from and who used it?"* afterwards.

```bash
curl 'localhost:8080/api/v1/capabilities?produces=http://edamontology.org/data_2048'
```

**Writes are validated by real SHACL.** `shapes/tar-shapes.ttl` is the rule set, enforced by
`shacl-rust` before anything is committed. A rejected write returns `422` with the engine's
`sh:ValidationReport` in Turtle, plus a `tar:jsonField` per result so a form can attach the
error to the input that caused it. Changing what the API accepts is an edit to a Turtle file.

**FAIR is not open.** `tar:availability = metadata-only` means the artifact is findable,
described, and provably not retrievable: no `downloadURL` exists at all, the UI renders no
download affordance, and the Signposting headers omit `rel="item"` so a machine can tell "no
bytes here" from "bytes behind auth" without parsing the body.

**Federation is a cross-link, not a harvest.** Any object position may hold a foreign IRI.
Advertising never blocks on the network: an unknown IRI is stored verbatim and a background
worker fetches a stub into that peer's own read-only graph.

**Every IRI dereferences.** The registry's identifiers and the UI's routes are the same URLs.

```bash
curl -H 'Accept: text/turtle'      localhost:8080/software/01a05…
curl -H 'Accept: application/json' localhost:8080/software/01a05…
curl                               localhost:8080/software/01a05….jsonld
```

---

## Advertising from a tool

Requirements 4 and 5. Both endpoints are idempotent on `(run, artifact, role)`, so a retried
CI step does not duplicate lineage.

```bash
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{
       "run": {"external_key": "gh-actions/12345/attempt-1", "status": "success",
               "started_at": "2026-08-30T14:02:11Z", "ended_at": "2026-08-30T14:02:49Z"},
       "artifacts": [{
         "title": "Validation report — patients.ttl vs fhir-shapes v3",
         "conforms_to": "http://edamontology.org/data_2048",
         "license": "https://spdx.org/licenses/CC-BY-4.0",
         "was_derived_from": ["https://reg.mumc.nl/artifact/01J7Z…"],
         "distributions": [{
           "download_url": "https://shacl.ids.unimaas.nl/reports/9f2a.ttl",
           "media_type": "text/turtle", "byte_size": 2118342,
           "checksum": {"algorithm": "sha256", "value": "9f2a…"},
           "access_protocol": "https", "auth_method": "apikey", "availability": "restricted",
           "access_request_url": "https://ids.unimaas.nl/data-access"
         }]
       }]
     }' \
     localhost:8080/api/v1/advertise/produced
```

Airflow, dbt and Spark can post their native events to `/api/v1/openlineage` instead; the
adapter maps what OpenLineage covers and keeps the whole event as `tar:openLineagePayload` so
nothing it does not model is lost.

---

## Registering a deployment

Two modes, because deployments arrive in two very different ways.

**Curated.** Someone who knows the estate creates the record: `POST /api/v1/instances`, or the
form in the UI. Right when deployments are few and long-lived. The record may declare a
`health_endpoint` — a URL whose only job is to say the deployment is alive, and which is held
to a **2xx**. Leave it out and the endpoint URL itself is probed, where anything that answers
counts as up, because a great many healthy services return `401` or `404` at their root and
marking those down would be a false alarm about a working deployment.

**Self-registering.** The application is handed one credential and every deployment of it
creates and maintains its own record:

```bash
# A curator issues the key once, for the software rather than for a deployment.
curl -X POST -H "Authorization: Bearer $CURATOR" \
     localhost:8080/api/v1/software/$SOFTWARE_ID/tokens

# Every deployment then announces itself, and repeats this whenever anything changes.
curl -X PUT -H "Authorization: Bearer $APP_KEY" -H 'content-type: application/json' \
     -d '{"label": "sulo on prod", "instance_key": "prod-cluster",
          "endpoint_url": "https://sulo.example.org",
          "health_endpoint": "https://sulo.example.org/healthz"}' \
     localhost:8080/api/v1/instances/self
```

The first call creates the record; every call after it updates the same one. `instance_key`
tells two deployments sharing a key apart — without it, one key would mean one deployment.
The credential decides which software this is a deployment *of*: naming a different one in the
body is a 403, not a hint.

With an identity provider there is no key to issue or leak at all — list the OIDC client ids in
the software's `registration_clients`, and a deployment presenting a token from its own issuer
registers itself the same way. `TAR_OIDC_AUTO_REGISTER_INSTANCES` is the looser third option:
it lets *any* accepted credential name its own software, which is convenient in a trusted
cluster and much weaker.

---

## Artifact keywords

The registry keeps a short list of its own — **Embeddings, OWL, RDF Graphs, SHACL, SHEX,
Mappings, SPARQL query** — at `GET /api/v1/keywords`.

Free text is where a catalogue quietly stops being searchable: one deployment writes `SHACL`,
another `shacl`, a third `shacl shapes`, and a filter for any of them finds a third of what is
there. Worse, a subscription written against `SHACL` silently misses everything advertised as
`shacl`, and a subscription that delivers nothing looks exactly like one with nothing to
deliver.

So a keyword matching the list — by label, slug or alias, ignoring case and punctuation — is
stored under its preferred label and additionally linked with `dcat:theme` to a concept that
dereferences. Anything else is kept verbatim as `dcat:keyword`. That is DCAT's own division
between a concept from a scheme and a literal, so nothing is invented and no existing record
breaks. `?keyword=` on `/api/v1/artifacts` accepts the IRI, the slug, the label or any alias.

```bash
# five spellings in, four keywords out
curl … -d '{"artifacts":[{"keywords":["shacl","SHACL Shapes","rml","pizza-ontology","RDF"]}]}'
# → ["SHACL", "Mappings", "pizza-ontology", "RDF Graphs"]
```

---

## For agents

Every record IRI serves Markdown. Append `.md`, or send `Accept: text/markdown`:

```bash
curl -H 'accept: text/markdown' localhost:8080/software/01a0…
curl localhost:8080/software/01a0….md          # the same bytes
curl localhost:8080/llms.txt                    # what this registry is, and everything in it
```

`/llms.txt` follows [llmstxt.org](https://llmstxt.org): what the registry is, how to read any
record without an RDF parser, the entry points, and a link to every record. It is public
whenever reads are, because a file whose whole purpose is to tell an unfamiliar client how to
read the registry is worth nothing behind a credential the client does not know it needs.

The markdown is a *representation*, not a second copy — same graph, same code path as `.ttl`,
so the prose cannot drift from the RDF. It states the things an agent otherwise gets wrong:
that `deployable: no` means there is no endpoint to call, that a peer's record is a cached stub,
that a withdrawn record still resolves, and that vocabulary terms must be looked up rather than
guessed.

Agents that would rather call tools than compose URLs can use the hosted MCP server at `/mcp`
(see `docs/mcp.md`). Nothing needs installing. The **Connect** tab in the UI shows the copy-paste
setup for Claude Code, Claude Desktop, the editors, an SDK client and plain `curl`, built from
the registry's own URL rather than a placeholder.

---

## How a tool authenticates

Three credentials, one rule: **an Instance may only advertise runs in which it is itself the
agent**, and the Instance always comes from the credential, never from the payload.

### OIDC workload identity — preferred

Give the deployment a client in the identity provider you already run, and tell the registry
which client that is:

```bash
curl -X PATCH -H "Authorization: Bearer $ADMIN" -H 'content-type: application/json' \
     -d '{"label":"shacl.ids.unimaas.nl","software":"01a05…",
          "oidc_client_id":"shacl-manager-ids3"}' \
     localhost:8080/api/v1/instances/01a05…
```

The tool then fetches its own short-lived token and uses it:

```bash
TOKEN=$(curl -s -u "$CLIENT_ID:$CLIENT_SECRET" -d grant_type=client_credentials \
  "$ISSUER/protocol/openid-connect/token" | jq -r .access_token)
curl -H "Authorization: Bearer $TOKEN" … /api/v1/advertise/produced
```

No secret for that deployment is ever stored in the registry. Rotation, expiry and revocation
belong to the identity provider.

The same path accepts **Kubernetes projected ServiceAccount tokens** and **GitHub Actions
OIDC** — list their issuers in `TAR_WORKLOAD_ISSUERS` and put the subject in
`oidc_client_id`. A CI job then advertises with no stored secret at all.

```
tar:oidcClientId  "repo:MaastrichtU-IDS/shacl-manager:ref:refs/heads/main"
tar:oidcClientId  "system:serviceaccount:shacl:shacl-manager"
```

`GET /api/v1/whoami` reports what a credential resolved to — the first thing to curl when a CI
job gets a 403.

### Registry API tokens — the fallback

A registry with no identity provider still has to work (requirement 6). Tokens are Argon2id
hashed, scoped, revocable, optionally expiring, and shown exactly once.

```bash
curl -X POST -H "Authorization: Bearer $ADMIN" -H 'content-type: application/json' \
     -d '{"scopes":["advertise:produce","advertise:consume"],"label":"ci","expires_in":"90d"}' \
     localhost:8080/api/v1/instances/01a05…/tokens
```

### Humans

OIDC authorisation code + PKCE against the same issuer; roles `reader` / `curator` / `admin`
come from the token. When no issuer is configured, the UI hides sign-in and falls back to
pasting a registry token.

Only `TAR_OIDC_ISSUER` may assert those roles. An issuer listed in `TAR_WORKLOAD_ISSUERS` is
trusted to say *which deployment* is calling and nothing else — a partner's Keycloak, a
Kubernetes API server and GitHub Actions can all mint a token containing a realm role called
`admin`, and honouring it would hand them this registry.

### Trying sign-in locally

A Keycloak with an importable realm — three roles, a PKCE public client, a service-account
client, and four users with known passwords — lives in
[`deploy/keycloak/`](deploy/keycloak/README.md).

```bash
docker compose -f deploy/keycloak/compose.yaml up -d

export TAR_BASE_IRI=http://127.0.0.1:8099
export TAR_LISTEN=127.0.0.1:8099
export TAR_STATIC_DIR=frontend/dist
export TAR_OIDC_ISSUER=http://127.0.0.1:8090/realms/tar
export TAR_OIDC_CLIENT_ID=tar-ui
./target/release/tar serve
```

Open <http://127.0.0.1:8099>, **Sign in → Continue with single sign-on**, and log in as
`curator` / `curator-password` (or `registryadmin` / `admin-password`). The credentials are
test values committed on purpose; the realm file is what makes the setup reproducible.

The one thing that must line up is the **audience**: `TAR_OIDC_AUDIENCE` defaults to
`TAR_BASE_IRI` and is required by default, so the Keycloak client needs an audience mapper
adding that exact string — otherwise the token carries `aud: ["account"]` and sign-in
completes at Keycloak and then fails here. The realm ships mappers for `127.0.0.1:8099` and
`127.0.0.1:8098`; serve on any other origin and you must add one.

---

## Layout

```
src/
  api/          HTTP surface: routes, dereference, SPARQL, SPA serving
  auth/         principals, scopes, and JWT/JWKS workload identity
  domain/       projections between the graph and the JSON API
  rdf/          property maps and quad builders
  store/        GraphStore trait + embedded Oxigraph implementation
  ops/          SQLite: tokens, peers, audit, federation cursors, idempotency
  shacl.rs      write validation and sh:ValidationReport generation
  negotiate.rs  content negotiation and FAIR Signposting
  seed.rs       `tar seed --from ids-examples`
shapes/         SHACL shapes and the preloaded vocabulary
frontend/       React 18 + Vite + TypeScript UI
tests/api.rs    end-to-end tests against the real router
```

---

## Tests

```bash
cargo test                      # 50 tests: unit + end-to-end against the real router
cd frontend && npm test         # 22 component, parsing and screen tests
```

The end-to-end suite covers advertisement idempotency, the `§8.3` authorisation rule under all
three credential types, OIDC verification failures (untrusted issuer, wrong audience, expired,
unbound client), `422` with a SHACL report, metadata-only handling including the absent
`rel="item"` link, content negotiation, keyset pagination, tombstones, the OpenLineage
adapter, peer announcement, and the read-only SPARQL endpoint.

---

## Configuration

| Variable | Default | What it does |
| --- | --- | --- |
| `TAR_PUBLIC_READ` | `true` | Serve reads without a credential. |
| `TAR_SPARQL_PUBLIC` | `true` | Serve SPARQL without a credential — independent of the above, so closing REST reads does not close the query endpoint. |
| `TAR_HEALTH_CHECK_ENABLED` | `true` | Probe deployments in the background. |
| `TAR_HEALTH_CHECK_INTERVAL` | `5m` | How often. |
| `TAR_HEALTH_ALLOW_PRIVATE` | `true` | Probe private addresses. |
| `TAR_OIDC_AUTO_REGISTER_INSTANCES` | `false` | Let any accepted credential register a deployment of software it names itself. |
| `TAR_APIDOC_ALLOW_PRIVATE` | `true` | Fetch API descriptions from private addresses. |


`TAR_BASE_IRI` is the only universally required setting. Everything else has a working default
— see spec §10.5 and addendum §4. `tar config` prints the effective configuration with secrets
redacted.

---

## Known gaps

Honest list of where the prototype departs from the spec, or stops short of it.

1. **Write validation is checked against the candidate record alone**, not the whole graph, so
   constraints that would need the rest of the store — `sh:class` on a referenced node, say —
   are not evaluated. That is the price of validating before committing rather than after.
   Validation itself is real SHACL: `shapes/tar-shapes.ttl` is enforced by the
   [`shacl-rust`](https://github.com/ensaremirerol/shacl-rust) engine, and editing that file
   changes what the API accepts with no Rust change. This also answers spec Q6: severity
   decides — `sh:Violation` blocks, `sh:Warning` never does, and
   `TAR_SHACL_VALIDATE_WRITES=false` downgrades violations to warnings.
2. **Distributions and capabilities are minted as IRIs, not blank nodes** (spec §4.5 shows blank
   nodes). They are addressable and citable that way. `GraphStore::describe` returns them with
   their parent, so a record still reads as one document.
3. **`tar:instanceOf` and `tar:atInstance` are denormalised** links, alongside the
   authoritative `prov:qualifiedAssociation`. Every list and count query would otherwise be a
   two-hop join through a reified node.
4. **Instance health is always `unknown`.** Nothing probes endpoints yet; the field, the chip
   and the filter exist.
5. **Repo liveness metrics are not implemented** (spec §10.5 `TAR_FORGE_TOKEN`). Per the
   handoff, the UI degrades by omitting those cells rather than rendering zeros.
6. **Human sign-in is verified against a live Keycloak** ([`deploy/keycloak/`](deploy/keycloak/)):
   a browser was driven through authorisation code + PKCE, and `curator` and `admin` realm
   roles were confirmed to decide what the signed-in person may do (`POST /api/v1/software`
   succeeds for a curator; `/api/v1/peers` is 403 for a curator and 200 for an admin). What is
   *not* covered: token **refresh**. The access token is used until it expires (30 minutes in
   the dev realm) and there is no silent renewal, so a long editing session ends in a 401 and
   a re-sign-in. The refresh token is deliberately not stored in the browser.
7. **Federated search fans out live** and is not deduplicated across peers beyond the origin
   chip.
8. **Peer resolution fetches a whole Turtle document** into the peer graph rather than
   extracting a minimal stub, so a verbose peer can cache more than the spec's "type, title,
   publisher, home registry".
9. **No lineage graph visualisation** — deferred to v2 by the handoff; `/api/v1/graph` and
   `/artifacts/{id}/lineage` already return exactly what a graph view would need.
10. **Keyset pagination orders by IRI string**, which is time-ordered within one registry
    because of UUIDv7, but interleaves imperfectly across origins.

## Answers to the handoff's open questions

1. **Dark mode in v1?** Yes — tokens in `frontend/src/styles.css`, cheap now and expensive to
   retrofit.
2. **Component library?** Hand-rolled, matching the sibling repos. ~10 KB of CSS, no
   dependency to track.
3. **Observed vs declared capability on the Software page?** Not built. The declared capability
   is shown; the observed one is one SPARQL query away and should be added only once there is
   enough run data for a disagreement to mean something.
4. **How much of an unresolved peer record to render?** The bare IRI, marked "not resolved
   yet", plus the origin chip. Never a skeleton: a skeleton promises content that may never
   arrive.
