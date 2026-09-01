# Identifiers and representations

Every record has one permanent IRI, minted under the registry's `TAR_BASE_IRI`, and that IRI is
also its web page. There is no separate "web view" URL to keep in step with the identifier,
because a pair of URLs for one thing is a pair that eventually disagrees.

```
https://registry.example.org/software/01a05…
https://registry.example.org/release/01a05…
https://registry.example.org/instance/01a05…
https://registry.example.org/run/01a05…
https://registry.example.org/artifact/01a05…
https://registry.example.org/artifact-series/01a05…
https://registry.example.org/type/shacl-validation-report
https://registry.example.org/keyword/rdf-graphs
```

Ids are UUIDv7, so they sort by creation time. Minted vocabulary terms get a slug instead of a
UUID, because a term's identifier is read by people.

## Representations

The same IRI serves six representations. Ask by `Accept` header, or append an extension — they
return the same bytes.

| Extension | `Accept` | What it is |
|---|---|---|
| `.ttl` | `text/turtle` | Turtle. What the record natively is. |
| `.jsonld` | `application/ld+json` | JSON-LD. |
| `.nq` | `application/n-quads` | N-Quads, named graph included. |
| `.json` | `application/json` | A flat developer JSON shape — the same body the REST API returns. |
| `.md` | `text/markdown` | The record as prose. See [Agent-facing surfaces](agents/surfaces.md). |
| `.html` | `text/html` | The web application. The default for a browser. |

```bash
curl -H 'Accept: text/turtle'      https://registry.example.org/software/01a05…
curl -H 'Accept: application/json' https://registry.example.org/software/01a05…
curl                               https://registry.example.org/software/01a05….jsonld
```

The Markdown is a *representation*, not a second copy: same graph, same code path as the
Turtle. The prose cannot drift from the RDF because there is nothing for it to drift from.

Anything the registry does not route falls through to the web application, so an unknown path
returns the app shell rather than a `404` — which is worth knowing if you are writing a client
that treats HTML as an error.

## Signposting

Every record response carries [FAIR Signposting] `Link` headers, so a client can discover the
alternates from any single response instead of assuming the extension convention:

```
Link: <…/artifact/01a05…>; rel="cite-as",
      <…/artifact/01a05….ttl>; rel="describedby"; type="text/turtle",
      <…/artifact/01a05….jsonld>; rel="describedby"; type="application/ld+json",
      <…/artifact/01a05….md>; rel="alternate"; type="text/markdown",
      <http://…/type/…>; rel="type",
      <https://…/report.ttl>; rel="item"; type="text/turtle",
      <https://spdx.org/licenses/CC-BY-4.0>; rel="license"
```

`rel="item"` is emitted only for bytes that actually exist. A `metadata-only` artifact omits it
entirely, which is how a machine distinguishes "there are no bytes here" from "there are bytes
and you need a credential" — see [Availability](model.md#availability-and-the-honest-absence).

## Named graphs

The store is quads, not triples, and which graph a statement is in carries meaning:

| Graph | Holds |
|---|---|
| `<urn:tar:local>` | Records this registry is authoritative for. |
| `<urn:tar:vocab>` | The bundled vocabularies and the terms this registry has minted. |
| `<urn:tar:shapes>` | The SHACL shapes that validate writes. |
| `<urn:tar:peer:{id}>` | A cached stub fetched from one peer. One graph per peer. |

Peer data is loaded straight into its own graph by the resolver and never passes through a
write handler, which is why rules this registry enforces on its own records are not, and must
not be, applied to a peer's.

## SPARQL

A read-only SPARQL 1.1 endpoint over all of the above is at `/sparql`, and it is a public read
surface in its own right rather than a debugging aid — a standard query language is most of the
registry's value to an analyst.

```bash
curl -G --data-urlencode 'query=SELECT ?s WHERE { GRAPH <urn:tar:local> { ?s a <https://w3id.org/tar/ns#Instance> } } LIMIT 10' \
     -H 'Accept: application/sparql-results+json' \
     https://registry.example.org/sparql
```

`POST` a query as `application/sparql-query`, or `GET` with `?query=`. It is governed by
`TAR_SPARQL_PUBLIC`, which is independent of `TAR_PUBLIC_READ`: closing REST reads does not
close the query endpoint, so an operator who wants a genuinely private registry has to say so
about both.

Updates are not accepted. Writes go through the API, where SHACL validation, the vocabulary
rule and the audit log live.

[FAIR Signposting]: https://signposting.org/
