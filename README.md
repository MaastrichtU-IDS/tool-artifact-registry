# Tool Artifact Registry

An RDF-native, self-hostable, federatable registry of **software**, the **deployments** that run
it, the **runs** they perform, and the **data artifacts** those runs consume and produce.

**📖 [Documentation](docs/)** — the model, the API, authentication, the vocabulary rules,
self-hosting. It reads on GitHub as it is, and builds into a site with `mdbook serve docs`;
pushing to the default branch publishes it to GitHub Pages.

---

## What it is for

Those four things are usually kept in four systems that do not agree. A software catalogue knows
what exists but not where it runs; a monitoring system knows what is running but not what it is
for; a data catalogue knows a file exists but not which program wrote it. None of them can answer
a question that crosses two.

Putting them in one graph buys two things:

- **Matchmaking, before anything has run.** A deployment declares what it is *able* to produce
  and consume, so *"what here could produce this kind of artifact?"* is answerable on an empty
  registry — which is the question you have when you are looking for a tool rather than a file.
- **Lineage, after it has.** A run links what it used to what it generated, so *"where did this
  come from and who else used it?"* is a graph walk.

It holds descriptions and pointers, never bytes.

## Quick start

```bash
cargo build --release
cd frontend && npm install && npm run build && cd ..

export TAR_BASE_IRI=http://127.0.0.1:8080
export TAR_ROOT_TOKEN=$(openssl rand -hex 24)
export TAR_DATA_DIR=./data

./target/release/tar seed      # example content, so a fresh install is not an empty page
./target/release/tar serve
```

Open <http://127.0.0.1:8080>. Everything reads anonymously; sign in with the root token to
register or edit.

With Docker:

```bash
export TAR_ROOT_TOKEN=$(openssl rand -hex 24)
docker compose up --build
```

`TAR_BASE_IRI` is the only universally required setting — the registry cannot mint
dereferenceable identifiers without knowing what it is called. Everything else has a working
default. `tar config` prints the effective configuration with secrets redacted.

See [Getting started](docs/getting-started.md) for the rest.

## Why it is built this way

Five commitments explain most of the design. Each is argued properly in the documentation.

**Writes are validated by real SHACL.** `shapes/tar-shapes.ttl` is the rule set, enforced before
anything is committed. Changing what the API accepts is an edit to a Turtle file, not to Rust. A
rejected write returns `422` with the engine's own `sh:ValidationReport`, plus a `tar:jsonField`
per result so a form can attach the error to the input that caused it.

**FAIR is not open.** An artifact can be recorded as findable, described, and provably not
retrievable: no download URL exists at all, the UI renders no download affordance, and the
Signposting headers omit `rel="item"` — so a machine can tell "no bytes here" from "bytes behind
auth" without parsing the body and guessing.

**Vocabulary is checked, not suggested.** An artifact type must be a term the registry actually
holds. Free-text classification degrades silently: three callers spell the same thing three ways,
a filter finds a third of what is there, and a subscription written against one spelling never
fires — which is indistinguishable from a subscription with nothing to deliver. A write naming an
unknown term is refused before anything is written, and the refusal says how to search for the
right one, adopt an existing one, or mint a new one.

**Federation is a cross-link, not a harvest.** Any object position may hold a foreign IRI.
Advertising never blocks on the network: an unknown IRI is stored verbatim and a background
worker fetches a stub into that peer's own named graph, never mixed with local records.

**Every identifier dereferences.** A record's IRI is also its web page, its Turtle, its JSON-LD
and its Markdown — the same graph through one code path, so the prose cannot drift from the RDF.

```bash
curl -H 'Accept: text/turtle'      localhost:8080/software/01a05…
curl -H 'Accept: application/json' localhost:8080/software/01a05…
curl                               localhost:8080/software/01a05….md
```

## Status

**A working prototype.** Every endpoint in the design is implemented and covered by tests. Where
it departs from the design, or stops short of it, that is written down in
[Limitations](docs/limitations.md) rather than left to be discovered.

## Layout

```
src/
  api/          HTTP surface: routes, dereference, SPARQL, SPA serving
  auth/         principals, roles, scopes, and JWT/JWKS workload identity
  domain/       projections between the graph and the JSON API
  mcp/          the hosted Model Context Protocol server
  rdf/          property maps and quad builders
  store/        GraphStore trait + embedded Oxigraph implementation
  ops/          SQLite: tokens, peers, audit, federation, subscriptions, idempotency
  health.rs     background liveness probing of deployment endpoints
  shacl.rs      write validation and sh:ValidationReport generation
  negotiate.rs  content negotiation and FAIR Signposting
  llms.rs       the llms.txt index
  seed.rs       example content for a fresh install
shapes/         SHACL shapes and the bundled vocabularies
frontend/       React 18 + Vite + TypeScript UI
docs/           the documentation site (mdBook)
deploy/         a local identity provider with an importable realm
tests/          end-to-end tests against the real router
```

## Tests

```bash
cargo test                      # unit, end-to-end, MCP and subscription suites
cd frontend && npm test         # component, parsing and screen tests
```

## Documentation

Everything else lives in [`docs/`](docs/), which builds into a site with
[mdBook](https://rust-lang.github.io/mdBook/):

```bash
cargo install mdbook --locked
mdbook serve docs --open
```

| | |
|---|---|
| [The model](docs/model.md) | Software → releases → deployments → runs → artifacts, and capabilities. |
| [Getting started](docs/getting-started.md) | Running it, seeding it, first requests. |
| [API](docs/api/conventions.md) | Organised by task: registering, advertising, searching, subscribing, federating. |
| [How a tool authenticates](docs/api/authentication.md) | The three credential types and when each is right. |
| [Vocabulary](docs/vocabulary/terms.md) | What types and topics must be, and how to search, adopt or mint one. |
| [For agents](docs/agents/surfaces.md) | `llms.txt`, Markdown representations, and the hosted [MCP server](docs/mcp.md). |
| [Operating a registry](docs/operations/configuration.md) | Configuration, identity provider, backup. |
| [Limitations](docs/limitations.md) | The honest list. |
| [Design record](docs/specs/README.md) | What was decided, and what else was considered. |

## Licence

Apache-2.0.
