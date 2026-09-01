# Searching and matchmaking

Three different questions, three different endpoints. It is worth knowing which one you have.

| Question | Endpoint |
|---|---|
| *I know roughly what it is called.* | `/api/v1/search` |
| *What can produce or consume this kind of artifact?* | `/api/v1/capabilities` |
| *Where did this artifact come from, and who used it?* | `/api/v1/artifacts/{id}/lineage`, `/api/v1/graph` |

## Free-text search

```bash
curl 'https://registry.example.org/api/v1/search?q=validation&type=software&limit=30'
```

| Parameter | |
|---|---|
| `q` | Required in practice — an empty `q` returns nothing rather than everything. |
| `type` | `software`, `instance`, `artifact` or `run`. Omit for all four. |
| `limit` | Default 30, clamped 1–100. |
| `federated` | `true` to ask this registry's peers too. See [Federation](federation.md). |

It matches the fields a person would search on: a software's name, tagline and abstract; a
deployment's label and description; an artifact's title and description; a run's label and
identifier.

There are also `fed_*` parameters in the query string. Those belong to the propagation envelope
registries use when they ask each other, not to you.

## Matchmaking

This is the question a catalogue exists to answer, and it works on a registry with no runs in
it.

```bash
curl 'https://registry.example.org/api/v1/capabilities?produces=https://registry.example.org/type/shacl-validation-report'
```

`produces`, `consumes`, or both; at least one is required, and each is an artifact type IRI.
The answer is the software, releases and deployments that have *declared* they can do it.

Declared capability is a claim, and the registry says so rather than dressing it up. It is the
claim you need when you are choosing a tool, before there is any run history to go on.

The results are capped at 200 and there is no pagination on this endpoint. If matchmaking
returns more than 200 candidates the question was probably too broad.

For the corresponding filter on the software listing, `/api/v1/software?produces=…` takes the
same IRIs.

## Lineage

```bash
curl 'https://registry.example.org/api/v1/artifacts/01a05…/lineage?direction=both&depth=3'
```

`direction` is `up` (what this came from), `down` (what came from this) or `both`, default
`both`. `depth` is clamped 1–6, default 1.

`GET /api/v1/graph?iri=…&depth=` returns the same subgraph centred on any node, with `depth`
clamped 1–4. Both return exactly what a graph view would need; there is no graph visualisation
in the UI yet.

## SPARQL, when the question does not fit

The three endpoints above cover the questions worth having a route for. Anything else — *which
deployments in this jurisdiction have produced nothing in six months?* — is a SPARQL query, and
the endpoint is public by default:

```bash
curl -G --data-urlencode 'query=…' \
     -H 'Accept: application/sparql-results+json' \
     https://registry.example.org/sparql
```

See [Identifiers and representations](../identifiers.md#sparql).
