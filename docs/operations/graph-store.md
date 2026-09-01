# Graph store

The registry keeps its records as quads. By default those quads live in an **embedded Oxigraph
store** under `TAR_DATA_DIR`, which is why one binary and one volume is a complete install.

An estate that already runs a triple store can point the registry at it instead:

| Variable | Default | |
|---|---|---|
| `TAR_SPARQL_ENDPOINT` | — | SPARQL 1.1 **Query** endpoint. **Setting it selects the external backend.** Unset, the registry uses embedded Oxigraph, exactly as before. |
| `TAR_SPARQL_UPDATE_ENDPOINT` | the query endpoint | SPARQL 1.1 **Update** endpoint. Many servers split the two — Fuseki serves `/ds/sparql` and `/ds/update`. |
| `TAR_SPARQL_BEARER_TOKEN` | — | `Authorization: Bearer …` on every request. |
| `TAR_SPARQL_USERNAME` / `TAR_SPARQL_PASSWORD` | — | HTTP basic auth. Both or neither. |
| `TAR_SPARQL_TIMEOUT` | `60s` | Per request. Generous next to the federation timeouts: this is the registry's own storage, and a boot-time load of the bundled vocabularies is one request. |

Configuring a bearer token *and* a username is an error rather than a silent preference — the
registry will not choose a credential for you. `tar config` prints which backend is in use and
which kind of credential, never the credential itself:

```console
$ tar config | head -4
base_iri              https://demo.example
data_dir              /var/lib/tar
graph store           external SPARQL endpoint — query http://fuseki:3030/tar/sparql / update http://fuseki:3030/tar/update (basic auth)
listen                0.0.0.0:8080
```

`TAR_DATA_DIR` still matters with an external endpoint: the operational database (tokens, peers,
audit, subscriptions, the federation cache) is SQLite either way, and only the *graph* moves.

## This is not `/sparql`

`/sparql` is the registry's own **read-only** query surface for analysts and peers, and it
refuses updates whichever backend is configured. The variables above are a private connection to
the registry's storage. They have nothing to do with each other.

## Fuseki, end to end

```bash
docker run --rm -d --name tar-fuseki -p 3030:3030 -e ADMIN_PASSWORD=admin stain/jena-fuseki
curl -u admin:admin -X POST 'http://localhost:3030/$/datasets?dbName=tar&dbType=tdb2'

export TAR_BASE_IRI=https://registry.example
export TAR_SPARQL_ENDPOINT=http://localhost:3030/tar/sparql
export TAR_SPARQL_UPDATE_ENDPOINT=http://localhost:3030/tar/update
export TAR_SPARQL_USERNAME=admin
export TAR_SPARQL_PASSWORD=admin
tar seed && tar serve
```

Nothing else changes. The named graphs are the same either way — see
[Named graphs](../identifiers.md#named-graphs) — so `tar dump` from an embedded registry restores
into an external one and vice versa, and the two answer identically for the same data.

## Reference data

About 12 000 of the registry's quads are not records at all: four bundled files under `shapes/`
and one table in the source, holding the SHACL shapes, the registry's own terms, two external
vocabularies and the artifact keyword scheme. They used to be pushed into the record store on
every single start, with a `DROP GRAPH` of the shapes each time. Against an external endpoint
that is 12 000 quads over HTTP at every restart, and the write-path question "is this a term the
registry holds" was one more round trip per record written.

They now live in two places:

* **An in-memory store, loaded at every start from the constants compiled into the binary.**
  It starts empty, so loading it every time is correct by construction — no staleness, no guard,
  no network. This is what the write path reads, which is why registering a record whose type
  comes from a bundled vocabulary asks the endpoint nothing about the vocabulary at all.
* **The record store, one graph per bundle, guarded by a content digest.** `/sparql` has to be
  able to join a record to the term it cites and a peer has to be able to fetch a definition, so
  the copy is real. `<urn:tar:bundles>` records each graph's digest, its size and when it was
  written; a boot that finds every digest unchanged issues one `SELECT` and writes nothing.

The digest covers the base IRI as well as the file, because the base decides how a relative IRI
resolves and where the keyword concepts are minted: a store served under a new `TAR_BASE_IRI`
reloads its reference data rather than serving the old registry's identifiers.

```console
$ curl -sG --data-urlencode 'query=PREFIX void: <http://rdfs.org/ns/void#>
    SELECT ?g ?n WHERE { GRAPH <urn:tar:bundles> { ?g void:triples ?n } }' \
    -H 'Accept: application/sparql-results+json' https://registry.example/sparql
```

A term the registry minted, adopted, or cached from a peer is a *record*, not reference data:
it lives in `urn:tar:local` or in that peer's graph, and the write-path check falls through to
the record store for exactly those. That fallback is one query — the same one the old code made
unconditionally — so the worst case is what every write used to cost and the common case is
nothing.

## What the two backends share, and what they do not

Every read the registry performs by subject — the record description, "does this exist", "which
graph is it in", "how many quads" — is **one SPARQL query**, written once in
`src/store/queries.rs` and run through the backend's `select`/`ask`. A backend implements
`select`, `ask`, `construct` and `apply` and inherits the rest, so the two cannot drift apart on
what a record *is* without one of them failing to run a standard query.

**Writes are one request.** A registry write is "replace what we said about this resource": some
subject deletions, some property deletions, some insertions. Against the embedded store that is
one Oxigraph transaction. Against an external endpoint it is a single SPARQL Update request —
`DELETE {…} WHERE {…} ; DELETE WHERE {…} ; INSERT DATA {…}` in one body — because a request is
processed as one unit by the servers this targets, whereas one HTTP call per operation can leave
a record with its old distribution deleted and its new one never inserted.

That last guarantee is the *server's*. SPARQL 1.1 does not require a request to be atomic, and
a server that processes operations independently gives atomicity only per operation. The
registry cannot detect the difference over HTTP and does not claim to.

**An unreachable endpoint is an error, never an empty result.** A query that returns nothing
because the server is down looks exactly like a registry with no records, so every failure names
the endpoint:

```console
$ TAR_SPARQL_ENDPOINT=http://127.0.0.1:3999/nothing/sparql tar seed
Error: SPARQL endpoint http://127.0.0.1:3999/nothing/sparql is unreachable

Caused by:
    0: error sending request for url (http://127.0.0.1:3999/nothing/sparql)
    1: client error (Connect)
    2: tcp connect error
    3: Connection refused (os error 111)
```

## Choosing

Embedded is the right default and stays the recommendation for a single registry: no second
process, no JVM, no network hop on the read path, and writes are a local transaction.

Reach for an external endpoint when the graph is already somewhere else — a Fuseki or GraphDB
your organisation runs and backs up, a store other tools query directly, or a dataset too large
to sit beside the API process. The cost is a round trip per store call and, today, a worker
thread blocked for its duration (see [Limitations](../limitations.md)).
