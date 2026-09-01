# Artifact types and topics

Two fields are controlled, and for the same reason.

- An artifact's **type** — `conforms_to`, and `produces` / `consumes` on a capability — says
  what the artifact *is*.
- A software's **topics** say what it is *about*.

Both must be terms the registry actually holds. A write naming an IRI it cannot resolve is
refused before anything is written.

## Why this is not free text

A keyword is a label, and a wrong one costs a little. A type is what every capability query and
every subscription filter matches on, **exactly**, so the same slippage costs far more.

Asked to describe one validation report, one caller writes `…/type/shacl-report`, another
`…/type/shacl-validation-report`, and a third assembles a plausible-looking ontology number
from memory. All three records look right. None of them match each other. `?conforms_to=` then
answers a third of what is in the catalogue, and a subscription written against one spelling
never fires — which is indistinguishable from a subscription with nothing to deliver.

That last one is the real damage. A search that under-returns is at least visibly a search. A
subscription that silently never fires looks exactly like a quiet week.

## Where the terms come from

Two sources, and for an RDF-heavy estate the second carries most of the weight.

### Bundled vocabularies

The registry ships two, generated at build time and committed so that a checkout builds with no
network:

| File | Source | Holds |
|---|---|---|
| `shapes/edam.ttl` | EDAM | 949 data-branch terms as artifact types, plus 260 topic-branch terms kept only as legacy (see below). |
| `shapes/euroscivoc.ttl` | EuroSciVoc | 1,064 research topics. |

EDAM's data branch was chosen because it is a real, maintained, dereferenceable data-type
vocabulary with synonyms and definitions, it is what a widely used tool registry types software
inputs and outputs with, and it costs nothing to carry. EuroSciVoc covers what software is
*about* rather than what data *is*, which is the other half.

`build.rs` checks upstream at most once a day, rewrites the file only when the content actually
differs, and leaves the committed bundle alone with a warning if the network or the parse
fails. `TAR_UPDATE_EDAM=1` forces a check; `TAR_EDAM_OFFLINE=1` skips it entirely and fails if
there is no committed bundle to fall back on.

Which vocabularies these are is a property of *this build*, not of the API. The API never
returns a vocabulary's name in a field — `source` is `bundled`, `local` or `external`, which is
the distinction a caller actually needs — because several vocabularies are in play and more will
follow, and a field value naming one would be wrong the moment another arrives.

### Terms the registry holds itself

`POST /api/v1/types`. This is not a fallback path for awkward cases.

Searched for what an RDF tooling estate actually emits, a bundled life-science data vocabulary
of 949 terms yields seventeen containing the word "ontology" — almost all of them *identifiers*
of ontology concepts — and not one term for a shapes graph, a validation report, a schema, a
mapping, an update, a hash-chained patch log or a masked replica. Those are exactly the things
such an estate produces all day. Sixteen of the types the bundled example content registers are
therefore the registry's own, and the bundled data branch is there for the artifacts that
genuinely are life-science data.

This is the expected shape of the thing. A general registry will have local terms.

## Adopting versus minting

Registering a term does two different jobs, and choosing the wrong one recreates exactly the
problem the rule exists to prevent.

| | when | what it records |
|---|---|---|
| **Adopt** — send `iri` | the term already has an identifier somewhere else | that identifier, and the scheme it came from |
| **Mint** — omit `iri` | nothing anywhere names this thing | an identifier of this registry's own |

Adoption matters more than it looks. If every registry invents a local alias for a term that
already has a public IRI, then federation is comparing near-synonyms again, one level up, and
the problem has simply moved. Two registries that adopt the same term end up agreeing on one
identifier without ever having to coordinate.

```bash
# adopt a term that already has a name elsewhere
curl -X POST -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{"label":"Software suite",
          "iri":"http://purl.obolibrary.org/obo/SWO_0000001",
          "scheme":"http://www.ebi.ac.uk/swo"}' \
     https://registry.example.org/api/v1/types

# mint one for a thing nothing else names
curl -X POST -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{"label":"Hash-chained patch log","slug":"patch-log",
          "definition":"An append-only log of RDF patches, each linked to its predecessor by hash."}' \
     https://registry.example.org/api/v1/types
```

Accepted fields: `label` (required), `iri`, `scheme`, `slug`, `definition`,
`default_media_type`, `aliases`.

`POST /api/v1/types` needs the `curator` role. That is the point: it is the one place new types
enter the registry, so it is where the judgement belongs.

## Searching before you write

```bash
curl 'https://registry.example.org/api/v1/vocab/search?q=validation+report&branch=data&limit=20'
```

| Parameter | |
|---|---|
| `q` | Plain words. Fewer than two characters returns nothing — one character matches most of a large vocabulary. |
| `branch` | Restrict the kind of term: `data` for artifact types, `topic` for research topics, `keyword` for the keyword list. Omit to search everything, including locally minted types. A fourth value exists for the legacy topics described below; it is there for completeness and there is no reason for a caller to use it. An unrecognised value returns no hits rather than an error. |
| `limit` | Default 20, clamped 1–100. |

It matches labels, synonyms and definitions, and returns each hit's `iri`, `label`,
`definition`, `source` and a score. Use the `iri` verbatim.

`GET /api/v1/vocab/resolve?iris=…` takes a comma-separated list (up to 100) and returns labels
for IRIs you were handed by a person or found in a repository file — the cheap way to check
something before writing it into a record.

`GET /api/v1/types` lists what the registry holds; `GET /api/v1/types/{id}` is one term, and
like every other record it dereferences.

## A term, and the right kind of term

Existing is not enough, and this was found rather than anticipated.

Pointed at a registry and told to classify a piece of software, a coding agent produced an IRI
that *did* exist — it was a term for a specific laboratory technique, in a completely unrelated
branch — and a plain existence check waved it onto a record that had nothing to do with it.

So every concept the registry holds carries a class saying which kind of term it is, declared as
a subclass of `skos:Concept`:

| Kind | Accepted in |
|---|---|
| an artifact type | `conforms_to`, `capability.produces`, `capability.consumes` |
| a research topic | `topics` |
| an artifact keyword | the keyword list; never where a type or topic is expected |
| a legacy topic | nothing |

A legacy topic is a subject area kept only so that a record already citing one still renders a
label. It is never offered by a picker and never accepted on a write.

The rule is therefore not *"does this term exist"* but *"could a search have returned this term
for this field"*, which rejects a real term of the wrong kind as firmly as an invented one.

### The class is the statement

The class is asserted in the same statement that makes the concept, and that is the point.

It was a separate literal beside the concept until the two were written by different code paths
and landed in different named graphs — the concept in one, a backfilled marker in another —
after which every query asking for both inside one `GRAPH` block found neither, and the
registry's own types were held, accepted on write, and offered by no picker.

A marker that has to be kept next to something drifts away from it. A marker that *is* the
statement cannot.

## The refusal

A type or topic IRI the registry cannot resolve is a `422` before anything is written, on every
path that can name one: `POST /api/v1/artifacts`, both `/api/v1/advertise/*`,
`/api/v1/openlineage`, both capability routes, and software, deployment and release writes —
including repository sync. The same rule runs behind the MCP tools, so an agent and a `curl` get
the same verdict.

```
422 https://example.org/type/invented is not an artifact type this registry knows. First
    search for one with GET /api/v1/vocab/search?branch=data&q=… and use the `iri` it returns.
    If the term is defined somewhere this registry does not carry and you have its IRI, adopt
    it with POST /api/v1/types, sending that `iri`. Mint a new one with POST /api/v1/types,
    without an `iri`, only when nothing anywhere names this thing.
```

Reuse, then adopt, then mint — in that order, and a test asserts the message keeps that order,
because the order is the advice.

It travels as a `sh:ValidationReport` carrying `tar:jsonField "conforms_to"`, the same shape a
shape violation uses, so an edit form highlights the offending input without learning a second
error format.

The message names no vocabulary. Several are in play and more will follow, so a refusal that
named one would be wrong the moment another arrived.

In the UI the term picker closes the loop itself: type a name nothing matches and it offers to
register it, paste an IRI and it offers to adopt it. Either way what leaves the picker is an IRI
the registry will accept.

## What was considered and not used

| Candidate | Why not |
|---|---|
| A serialisation-format vocabulary | A format is not a kind of thing. It would type a shapes graph, a validation report and an ontology identically as Turtle, and would change an artifact's type when the same bytes are re-serialised. The distribution already carries `media_type`, which is where serialisation belongs. |
| The DCMI Type Vocabulary | Twelve terms. Every artifact here is `Dataset`, so it discriminates nothing — and DCAT already types these records as `dcat:Dataset` anyway. |
| IANA media types | Already in use, at the distribution level, and again not a type: one media type covers a shapes graph, a report and an ontology alike. A type concept may carry a `default_media_type`, which is the honest relationship between the two. |
| Bundling a new vocabulary for RDF artifacts | There is no maintained public one that names a shapes graph, a mapping or a schema as data types. Inventing one and bundling it would be minting registry-local terms with extra steps and no upstream. |

## Federation is untouched by this rule

A peer's record legitimately cites a type minted at that peer, and it keeps doing so. Peer data
is loaded straight into that peer's own named graph by the resolver and never passes through a
write handler, so nothing this registry is not authoritative for is ever held to this rule.

Once a foreign type *has* been resolved into a peer graph it becomes a term this registry holds,
and a local record may then cite it too.

It is accepted, but not offered: it carries none of this registry's own classes, so a picker
does not list it. That is a deliberate half-answer — which registry owns a term, and whether
adopting a peer's implies agreeing with it, is a federation question this prototype does not
settle. The picker's adopt flow is how to make such a term first-class here.
