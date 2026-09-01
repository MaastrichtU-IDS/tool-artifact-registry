# Artifact keywords

A type says what an artifact *is*, and is checked hard. A keyword is a label, and is checked
softly — but not not at all, because free text is where a catalogue quietly stops being
searchable.

One deployment writes `SHACL`, another `shacl`, a third `shacl shapes`, and a filter for any of
them finds a third of what is there. Worse, a subscription written against `SHACL` silently
misses everything advertised as `shacl`, and a subscription that delivers nothing looks exactly
like one with nothing to deliver.

## The list

The registry keeps a short list of its own, at `GET /api/v1/keywords`. Seven entries, each with
a label, a slug, a definition and a set of aliases:

| Label | Slug |
|---|---|
| Embeddings | `embeddings` |
| OWL | `owl` |
| RDF Graphs | `rdf-graphs` |
| SHACL | `shacl` |
| SHEX | `shex` |
| Mappings | `mappings` |
| SPARQL query | `sparql-query` |

It is deliberately short. A controlled list long enough to cover everything is a list nobody
reads, and the escape hatch below is what makes a short one workable.

## What happens to a keyword you send

A keyword matching the list — by label, slug or alias, ignoring case and punctuation — is
stored under its **preferred label**, and additionally linked with `dcat:theme` to a concept
that dereferences.

Anything else is kept verbatim as `dcat:keyword`.

That is DCAT's own division between a concept drawn from a scheme and a plain literal, so
nothing is invented and no existing record breaks. Free text stays allowed; it simply will not
match a keyword filter or a subscription written against the list.

```bash
# five spellings in, four keywords out
curl … -d '{"artifacts":[{"keywords":["shacl","SHACL Shapes","rml","pizza-ontology","RDF"]}]}'
# → ["SHACL", "Mappings", "pizza-ontology", "RDF Graphs"]
```

Two of the five collapsed onto one concept, one mapped to a differently-named one, one was
unrecognised and survived as written, and the case was normalised. That is the whole behaviour.

## Filtering

`?keyword=` on `/api/v1/artifacts` accepts the concept IRI, the slug, the label, or any alias —
so a client that only ever saw one spelling still finds the records stored under another.

Subscription filters take keywords too, matched case-insensitively; see
[Subscriptions](../api/subscriptions.md).

## Why keywords are not types

They overlap in what they describe, and it is worth being explicit about why both exist.

A **type** answers a machine's question — *can this software consume that artifact?* — so it has
to be exact, and the cost of getting it wrong is a query that silently under-returns. A
**keyword** answers a person's question — *what is this roughly about?* — so it has to be
forgiving, and the cost of getting it wrong is a slightly worse search result.

Holding keywords to the type rule would mean refusing a write because somebody described their
own data in their own words. Holding types to the keyword rule would mean matchmaking that
quietly does not work.
