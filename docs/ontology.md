# The `tar:` ontology

[The model](model.md) explains what the registry describes. This chapter is the formal side of
it: the classes and properties the registry's own namespace declares, what each one may be said
about, and — for every one of them — what was checked in the standard vocabularies before it was
added.

The registry speaks DCAT 3, DCTERMS, PROV-O, schema.org, SKOS, SPDX, CodeMeta, ADMS and VoID
wherever they say what it means. `tar:` exists for what none of them says.

> **The namespace IRI does not resolve.** `https://w3id.org/tar/ns#` is a name, not a location:
> it needs a w3id.org registration that has not been made, so following it gets you nothing
> today. Until it is made, the document below is the ontology, and every running registry serves
> it:
>
> ```bash
> curl -G --data-urlencode \
>   'query=CONSTRUCT { ?s ?p ?o } WHERE { GRAPH <urn:tar:bundle:vocab> { ?s ?p ?o } }' \
>   -H 'Accept: text/turtle' https://registry.example/sparql
> ```

## Where it lives

`shapes/vocab.ttl`, compiled into the binary and loaded into the named graph
`urn:tar:bundle:vocab` at every start. One file, not two, on purpose: the `rdfs:comment` on each
term is the reason that term exists rather than a standard one, and an ontology whose
justifications are kept somewhere else is an ontology whose justifications nobody reads. Since
that file is one bundle in one graph ([Named graphs](identifiers.md#named-graphs)), the
`CONSTRUCT` above returns the ontology whole and nothing else — which is what a w3id redirect
would eventually point at.

The SHACL shapes are a separate file and a separate graph (`shapes/tar-shapes.ttl`,
`urn:tar:shapes`). They also mint IRIs in this namespace — `tar:SoftwareShape`,
`tar:ArtifactShape` and so on — which are shape identifiers rather than vocabulary terms and are
not declared here.

## What the axioms are for, and what they are not

The registry validates writes with **SHACL**, and SHACL ignores `rdfs:domain` and `rdfs:range`
entirely. Nothing in this file rejects anything.

What the axioms do is let a consumer running an RDFS reasoner **infer** types. That inverts the
usual instinct: an over-tight domain does not tighten this registry, it silently invents triples
in somebody else's store. So a domain is declared only where every subject the registry writes
genuinely is of that class, and left off where a property is used on more than one kind of
record — `rdfs:domain` over two classes means the subject is inferred to be both at once, which
would be false, and minting a union class to make the axiom well-formed would be tidiness
asserted as fact. `tar:availability` (distributions and deployments), `tar:hasCapability`
(software, releases and deployments) and `tar:tombstoned` (every kind of record) therefore have
ranges but no domains.

Ranges given as `xsd:dateTime` describe what the write builders emit and what the shapes
enforce. A caller can defeat that with `TAR_SHACL_VALIDATE_WRITES=false` and a malformed date,
which the builder stores as a plain literal rather than dropping; a reasoner reading such a
graph is reading data the registry itself reports as invalid.

## Classes

| Class | Also asserted on the same node | Declared hierarchy |
|---|---|---|
| `tar:Software` | `schema:SoftwareApplication`, `schema:SoftwareSourceCode` | `rdfs:subClassOf schema:SoftwareApplication` |
| `tar:Release` | `schema:SoftwareApplication`, `prov:Plan` | `rdfs:subClassOf schema:SoftwareApplication, prov:Plan` |
| `tar:Instance` | `prov:SoftwareAgent`; `dcat:DataService` only when it has an endpoint | `rdfs:subClassOf prov:SoftwareAgent` |
| `tar:Capability` | `prov:Plan` | `rdfs:subClassOf prov:Plan` |
| `tar:ArtifactSeries` | — | none |
| `tar:RepositorySync` | — | none |
| `tar:ArtifactType` | `skos:Concept` | `rdfs:subClassOf skos:Concept` |
| `tar:ResearchTopic` | `skos:Concept` | `rdfs:subClassOf skos:Concept` |
| `tar:ArtifactKeyword` | `skos:Concept` | `rdfs:subClassOf skos:Concept` |
| `tar:LegacyTopic` | `skos:Concept` | `rdfs:subClassOf skos:Concept` |

`tar:producingSystem` and `tar:producingUser` are not classes but individuals: instances of
`prov:Role`, used as the object of `prov:hadRole`.

Two things are worth reading off that table.

**Nothing is declared `owl:equivalentClass` with anything.** A reasoner acts on equivalence, and
a wrong equivalence spreads: it would make every statement about one IRI a statement about the
other. `rdfs:subClassOf` is the strongest claim that is actually true here, and for two of the
classes even that is too strong.

**The multi-typing is deliberate, and subsumption explains it rather than excusing it.** A
Release *is* a `schema:SoftwareApplication`, and so is the Software it is a release of — which
is exactly why `tar:Software` and `tar:Release` exist: no standard vocabulary distinguishes an
abstract program from one of its versioned, runnable plans as classes, and a SHACL shape
targeting one would fire on both. The subclass axioms say the schema.org typing is honest; the
`tar:` classes say which of the two you are looking at.

`tar:Instance` is `rdfs:subClassOf prov:SoftwareAgent` and deliberately **not** a subclass of
`dcat:DataService`: the code asserts `dcat:DataService` only when a deployment has an endpoint,
and a library installed on a laptop is a deployment with no endpoint at all. Making it a
subclass would infer a data service where there is none.

`tar:ArtifactSeries` and `tar:RepositorySync` are the two classes the code has always asserted
and the ontology never declared. Both are given no superclass, for stated reasons:

* A series was typed `skos:Concept` once, which put every artifact's *title* into the
  artifact-type picker and had to be migrated back out. It is not a `dcat:Dataset` either — it
  has no distributions, no licence and no bytes, which is the whole reason it is a separate
  node.
* A `tar:RepositorySync` is registry configuration rather than a described resource. Typing it
  `prov:Plan` was considered and rejected: that would claim the registry executes it as the plan
  of some activity, and there is no `prov:Activity` in the graph that it is the plan of.

## Properties

Domain and range as declared. "Beside" names a standard term the registry writes **as well**, on
the same subject, in the same write — not a `rdfs:subPropertyOf`, because in each case the
standard term is lossy in a way the comment records, and a subproperty axiom would assert an
entailment that is not there.

### Capability

| Property | Domain | Range | Beside |
|---|---|---|---|
| `tar:hasCapability` | — (Software, Release *or* Instance) | `tar:Capability` | — |
| `tar:produces` | `tar:Capability` | `skos:Concept` | — |
| `tar:consumes` | `tar:Capability` | `skos:Concept` | — |

The range of `produces`/`consumes` is `skos:Concept` and **not** `tar:ArtifactType`, even though
that is what the write path demands of a local record. A type cached from a peer carries none of
this registry's classes — a peer is authoritative for its own types — and is nonetheless a legal
value here. A range of `tar:ArtifactType` would infer, about somebody else's term, a
classification this registry has explicitly declined to make.

### Structure

| Property | Domain | Range | Beside |
|---|---|---|---|
| `tar:runsRelease` | `tar:Instance` | `tar:Release` | — |
| `tar:instanceOf` | `tar:Instance` | `tar:Software` | — |
| `tar:usedRelease` | `prov:Activity` | `tar:Release` | `prov:qualifiedAssociation` / `prov:hadPlan`, always written too and authoritative |
| `tar:sync` | `tar:Software` | `tar:RepositorySync` | — |

### Access descriptors

| Property | Domain | Range | Beside |
|---|---|---|---|
| `tar:availability` | — (Distribution *or* Instance) | `xsd:string` | `dct:accessRights` → EU access-right authority table |
| `tar:accessProtocol` | `dcat:Distribution` | `xsd:string` | — |
| `tar:authMethod` | `dcat:Distribution` | `xsd:string` | — |
| `tar:accessRequestURL` | `dcat:Distribution` | — | — |
| `tar:defaultMediaType` | `skos:Concept` | `xsd:string` | — |

`tar:availability` takes `public`, `restricted`, `embargoed` or `metadata-only`, and it stays
authoritative because its standard reading is lossy: the EU table has no embargo concept and
cannot distinguish "described but not retrievable" from merely non-public, so `embargoed` and
`metadata-only` both coarsen to `NON_PUBLIC`. The SHACL rules that stop a metadata-only
distribution carrying a download URL key on the four-way distinction.

### Run bookkeeping

| Property | Domain | Range | Beside |
|---|---|---|---|
| `tar:status` | `prov:Activity` | `xsd:string` | `schema:actionStatus` |
| `tar:openLineagePayload` | `prov:Activity` | `xsd:string` | — |
| `tar:claimedNamespace` | `prov:Activity` | `xsd:string` | — |

`tar:status` takes `success`, `failed`, `running` or `aborted`. schema.org's `ActionStatusType`
has no member for an aborted action and folds it into failure, so the literal stays
authoritative and `schema:actionStatus` is the interoperable supplement.

### Lifecycle

| Property | Domain | Range | Beside |
|---|---|---|---|
| `tar:tombstoned` | — (any record) | `xsd:boolean` | `adms:status` → `…/dataset-status/WITHDRAWN` |
| `tar:tombstonedAt` | — (any record) | `xsd:dateTime` | as above |
| `tar:health` | `tar:Instance` | `xsd:string` | — |
| `tar:healthCheckedAt` | `tar:Instance` | `xsd:dateTime` | — |
| `tar:healthDetail` | `tar:Instance` | `xsd:string` | — |
| `tar:lastSeenAt` | `tar:Instance` | `xsd:dateTime` | — |

`prov:invalidatedAtTime` was rejected as the sole form of a tombstone because its domain is
`prov:Entity`, and a tombstone also applies to a deployment (an agent) and to software.

### Software, release and deployment description

| Property | Domain | Range |
|---|---|---|
| `tar:deployable` | `tar:Software` | `xsd:boolean` |
| `tar:readme` | `tar:Software` | `xsd:string` |
| `tar:readmeBaseURL` | `tar:Software` | — |
| `tar:registrationClient` | `tar:Software` | `xsd:string` |
| `tar:containerImage` | `tar:Release` | `xsd:string` |
| `tar:imageDigest` | `tar:Release` | `xsd:string` |
| `tar:installCommand` | `tar:Release` | `xsd:string` |
| `tar:healthEndpoint` | `tar:Instance` | — |
| `tar:jurisdiction` | `tar:Instance` | `xsd:string` |
| `tar:selfRegisteredBy` | `tar:Instance` | `xsd:string` |
| `tar:instanceKey` | `tar:Instance` | `xsd:string` |
| `tar:oidcClientId` | `tar:Instance` | `xsd:string` |
| `tar:oidcIssuer` | `tar:Instance` | `xsd:string` |
| `tar:allowedScope` | `tar:Instance` | `xsd:string` |
| `tar:apiFormat` | `dct:Standard` | `xsd:string` |
| `tar:temporalStart` | `dcat:Dataset` | `xsd:dateTime` |
| `tar:temporalEnd` | `dcat:Dataset` | `xsd:dateTime` |

### Repository sync

| Property | Domain | Range |
|---|---|---|
| `tar:syncSource` | `tar:RepositorySync` | `xsd:string` |
| `tar:syncRepo` | `tar:RepositorySync` | `xsd:string` |
| `tar:syncField` | `tar:RepositorySync` | `xsd:string` |
| `tar:syncEnabled` | `tar:RepositorySync` | `xsd:boolean` |
| `tar:syncedAt` | `tar:RepositorySync` | `xsd:dateTime` |
| `tar:syncStatus` | `tar:RepositorySync` | `xsd:string` |
| `tar:syncError` | `tar:RepositorySync` | `xsd:string` |
| `tar:syncChanged` | `tar:RepositorySync` | `xsd:string` |

### Vocabulary navigation and validation reporting

| Property | Domain | Range |
|---|---|---|
| `tar:inBroader` | `skos:Concept` | `xsd:string` |
| `tar:jsonField` | `sh:ValidationResult` | `xsd:string` |

`tar:inBroader` carries a parent concept's **label**, not the parent concept, and is deliberately
not `skos:broader`. It is a rendering shortcut for the pickers, which show the parent to tell
near-synonyms apart — one bundled vocabulary contains ontology, odontology and palaeontology,
and only the parent separates them at a glance. A consumer that wants the relation should follow
`skos:broader` in the bundle.

## Which standard vocabulary does what

| Vocabulary | Used for |
|---|---|
| DCAT 3 | artifacts as `dcat:Dataset`, `dcat:Distribution` with `accessURL`/`downloadURL`/`mediaType`/`byteSize`, deployments as `dcat:DataService` with `dcat:endpointURL`, the registry as a `dcat:Catalog` via `dcat:inCatalog`, `dcat:keyword` and `dcat:theme` |
| DCTERMS | `dct:title`, `dct:description`, `dct:abstract`, `dct:license`, `dct:conformsTo`, `dct:subject`, `dct:identifier`, `dct:issued`/`dct:modified`/`dct:created`, `dct:isVersionOf`, `dct:publisher`/`creator`/`contributor`, `dct:accessRights`, `dct:Standard` |
| PROV-O | runs as `prov:Activity` with `startedAtTime`/`endedAtTime`, deployments as `prov:SoftwareAgent`, `prov:used` and `prov:wasGeneratedBy` for lineage, `prov:wasDerivedFrom`/`wasRevisionOf`, `prov:qualifiedAssociation`/`hadPlan`, `prov:qualifiedAttribution`/`hadRole`, `prov:actedOnBehalfOf`, `prov:specializationOf` for content identity, `prov:wasAttributedTo` for the writing credential |
| schema.org | `schema:SoftwareApplication`/`SoftwareSourceCode`, `schema:name`, `codeRepository`, `softwareVersion`, `applicationCategory`, `actionStatus`, and people and organisations as `schema:Person`/`Organization` |
| SKOS | every vocabulary term: `skos:Concept`, `prefLabel`, `altLabel`, `definition`, `inScheme`, `skos:ConceptScheme` |
| SPDX | `spdx:checksum` → `spdx:Checksum` with `spdx:algorithm` and `spdx:checksumValue`; licence IRIs under `spdx.org/licenses/` |
| CodeMeta | `codemeta:developmentStatus` (repostatus.org values), `codemeta:maintainer` |
| ADMS | `adms:status` on a tombstoned record |
| VoID | `void:Dataset` and `void:triples` on the bundle graphs themselves |
| FOAF | `foaf:page` for an artifact's documentation |
| EU authority tables | the value sets for `dct:accessRights` and for the withdrawn dataset status |

## Why each `tar:` term exists

Grouped by the reason, and every reason is the term's own `rdfs:comment` in `shapes/vocab.ttl`
rather than a fresh argument. That file is the authority; this is a reading of it.

**No vocabulary distinguishes these classes, and SHACL has to target them.** `tar:Software`,
`tar:Release`, `tar:Instance`, `tar:Capability`. A Release is also a
`schema:SoftwareApplication`, so a shape targeting software would fire on both;
`prov:SoftwareAgent` alone cannot target a deployment without also firing on every other agent.

**No vocabulary attaches a reusable I/O declaration to a tool.** `tar:hasCapability`,
`tar:produces`, `tar:consumes`. Bioschemas' `input`/`output` expect `FormalParameter` nodes on
the tool itself rather than a reusable declaration object; `wfdesc` expects workflow `Parameter`
nodes; biotoolsSchema's `function` is JSON and XML, not RDF. The type concept a harvester wants
is already the object IRI here, and the indirection would only hide it. The `/export/biotools`
endpoint is where that mapping belongs.

**No vocabulary says which release a deployment runs.** `tar:runsRelease`, `tar:instanceOf`,
`tar:usedRelease`. `doap:release` runs Project → Version, the wrong direction and the wrong
subject; `dcat:version` is a literal; `dct:isVersionOf` would claim a deployment is a version of
its software, and `prov:specializationOf` that it is the same entity. PROV deliberately has no
unqualified activity-to-plan property, and `prov:used` cannot be reused because every reader
here treats `prov:used` as "consumed artifact".

**DCAT conveys access mechanics only implicitly.** `tar:availability`, `tar:accessProtocol`,
`tar:authMethod`, `tar:accessRequestURL`. DCAT 3 leaves the protocol to be guessed from the URL
scheme; ODRL expresses policy rather than authentication mechanics; `schema:conditionsOfAccess`
is free text for humans; DCAT-AP 3 has no access-request property and HealthDCAT-AP is still
drafting one. `dcat:landingPage` is dataset-level navigation rather than an access-request flow.

**The interoperable term exists but loses a distinction the registry enforces.** `tar:status`,
`tar:tombstoned`, `tar:tombstonedAt`, `tar:syncStatus` — each written beside, or in place of, a
standard term that folds a state the registry treats separately.

**Registry operations nobody else models.** `tar:health`, `tar:healthEndpoint`,
`tar:healthCheckedAt`, `tar:healthDetail`, `tar:lastSeenAt`, `tar:deployable`,
`tar:jurisdiction`, `tar:readme`, `tar:readmeBaseURL`, `tar:containerImage`, `tar:imageDigest`,
`tar:installCommand`, `tar:apiFormat`, `tar:defaultMediaType`, `tar:temporalStart`,
`tar:temporalEnd`, `tar:sync` and the `tar:sync*` family, `tar:openLineagePayload`,
`tar:claimedNamespace`, `tar:inBroader`, `tar:jsonField`, and the workload-identity terms
`tar:oidcClientId`, `tar:oidcIssuer`, `tar:allowedScope`, `tar:registrationClient`,
`tar:selfRegisteredBy`, `tar:instanceKey`. Each comment names what was checked: `dct:spatial`
asserts spatial coverage of data rather than the law an operator answers to; `codemeta:readme`
is a URL pointing at a README rather than its text; `codemeta:buildInstructions` is a link to
build documentation rather than an executable one-liner; `dcat:mediaType` is defined on a
distribution and would misstate its domain on a concept; `dct:temporal` needs a
`dct:PeriodOfTime` node for what two optional timestamps say flat.

**Two roles, because `prov:wasAttributedTo` is spoken for.** `tar:producingSystem` and
`tar:producingUser` are `prov:Role` individuals carried on a `prov:Attribution`. The unqualified
`prov:wasAttributedTo` is written by the registry itself from the presenting credential and read
back as a single value; letting a caller supply an agent there would make the one attribution
nobody can forge indistinguishable from the ones anybody can.

**Concept classes, so that "is it a term" and "is it the right kind of term" are one question.**
`tar:ArtifactType`, `tar:ResearchTopic`, `tar:ArtifactKeyword`, `tar:LegacyTopic`. Asked to
classify a tool, a coding agent produced EDAM's "RNA-Seq" — a real term, on a record that had
nothing to do with RNA-Seq. The kind used to be a `tar:conceptBranch` literal beside the
concept, and it came apart from the concept the first time the two were written by different
code paths: the concepts went into `urn:tar:local` and a later backfill put their markers
somewhere else, after which every query asking for both inside one `GRAPH` block found neither.
A class asserted in the same statement as `a skos:Concept` cannot come apart that way, because
it *is* that statement. `rdfs:subClassOf skos:Concept` keeps every one of them a concept to a
reader that has never heard of `tar:`.

## Retired terms

Eight `tar:` terms carry `owl:deprecated true`. They are read as fallbacks for graphs written
before the 2026-08-30 audit, and none of them is given a domain or a range — a term that is
being removed should not be teaching a reasoner to infer anything.

| Retired | Replaced by |
|---|---|
| `tar:atInstance` | `prov:wasAssociatedWith` |
| `tar:externalKey` | `dct:identifier` |
| `tar:homeRegistry` | `dcat:inCatalog` |
| `tar:tagline` | `dct:abstract` |
| `tar:kind` | `schema:applicationCategory` |
| `tar:maturity` | `codemeta:developmentStatus` |
| `tar:contact` | `codemeta:maintainer` |
| `tar:conceptBranch` | the concept classes |

**One of these is not fully retired, and the ontology says so rather than pretending.**
`tar:contact` was replaced by `codemeta:maintainer` on Software, which is what the write path
emits — but the artifact write path still emits `tar:contact` for an artifact's `contact` field,
and nothing was put in its place. That is an inconsistency in the registry rather than a
considered distinction between two kinds of contact, and it is recorded in a `skos:note` on the
term. No domain is declared for it, because it now means one thing on records written before the
audit and another on artifacts written today.

`tar:conceptBranch` is declared only so that a store written before the concept classes has a
name to look the leftover up by; the first boot of this version removes it.

## A worked example

One artifact, exactly as a registry serves it — `GET /artifact/{id}` with
`Accept: text/turtle`, or the `.ttl` extension the Signposting `describedby` link points at.
The identifiers below are shortened for width; nothing else is edited.

```turtle
@prefix dcat: <http://www.w3.org/ns/dcat#> .
@prefix dct:  <http://purl.org/dc/terms/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix spdx: <http://spdx.org/rdf/terms#> .
@prefix tar:  <https://w3id.org/tar/ns#> .
@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .

<https://registry.example/artifact/01a05d84-…> a dcat:Dataset , prov:Entity ;
        # DCAT because it is a thing in a catalogue; PROV because it is a thing in a lineage.
        # Both, on the same node, deliberately: neither vocabulary alone answers both questions.
    dct:title       "Validation report — batch 2" ;
    dct:description "SHACL validation report produced by a scheduled run." ;
    dct:license     <https://spdx.org/licenses/CC-BY-4.0> ;
    dct:issued      "2026-09-01T15:08:50Z"^^xsd:dateTime ;

    dct:conformsTo  <http://edamontology.org/data_2048> ;
        # What the artifact *is*. The value must be a concept the registry holds and carry
        # tar:ArtifactType, or the write is refused with the search, adopt and mint routes.

    dcat:keyword    "SHACL" , "validation" , "batch-17" ;
    dcat:theme      <https://registry.example/keyword/shacl> ;
        # One input field, two predicates. DCAT's own division: dcat:keyword is a literal, and
        # dcat:theme ranges over a concept from a scheme. "SHACL" matched the registry's keyword
        # list and gained a theme; "batch-17" did not and stays free text rather than being
        # dropped.

    dct:isVersionOf <https://registry.example/artifact-series/01a05d84-…> ;
    dcat:distribution <https://registry.example/distribution/01a05d84-…> ;

    prov:wasAttributedTo <urn:tar:root> ;
    dct:modified         "2026-09-01T15:08:50Z"^^xsd:dateTime ;
        # Written by the registry from the presenting credential, on every write, by the same
        # code for every record type. This is the attribution nobody can forge, which is why a
        # caller-supplied agent never goes here.

    prov:qualifiedAttribution
        <https://registry.example/artifact/01a05d84-…#producingSystem> ,
        <https://registry.example/artifact/01a05d84-…#producingUser> .

# Who produced it, and in what capacity — a qualified attribution because there is more than one
# answer and they mean different things. The node IRIs are the artifact's own IRI plus the role,
# so they are stable across rewrites rather than blank nodes that change identity on every parse.
<https://registry.example/artifact/01a05d84-…#producingSystem> a prov:Attribution ;
    prov:agent   <https://registry.example/agent/01a05d84-…> ;
    prov:hadRole tar:producingSystem .

<https://registry.example/artifact/01a05d84-…#producingUser> a prov:Attribution ;
    prov:agent   <https://orcid.org/0000-0002-1825-0097> ;
    prov:hadRole tar:producingUser .
        # An ORCID is a better subject than a minted one, so the registry uses it as-is rather
        # than inventing a local agent IRI that would federate with nothing.

<https://registry.example/distribution/01a05d84-…> a dcat:Distribution ;
    dcat:accessURL   <https://shacl.example.org/reports/21> ;
    dcat:downloadURL <https://shacl.example.org/reports/21.ttl> ;
    dcat:mediaType   "text/turtle" ;
    dct:format       "text/turtle" ;
    dcat:byteSize    2119366 ;

    tar:availability  "restricted" ;
    dct:accessRights  <http://publications.europa.eu/resource/authority/access-right/RESTRICTED> ;
        # The four-way literal is authoritative — the SHACL rules that stop a metadata-only
        # distribution carrying a download URL key on it — and the EU authority table is the
        # DCAT-AP reading written beside it. Lossy on purpose: the table has no embargo concept.
    tar:accessProtocol   "https" ;
    tar:authMethod       "apikey" ;
    tar:accessRequestURL <https://example.org/data-access> ;
        # Three things DCAT conveys only implicitly, so that a client can choose a retrievable
        # distribution without guessing from the URL scheme, and can find out how to ask when it
        # cannot.

    spdx:checksum [ a spdx:Checksum ;
                    spdx:algorithm     spdx:checksumAlgorithm_sha256 ;
                    spdx:checksumValue "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08" ] ;

    prov:specializationOf <ni:///sha-256;n4bQgYhMfWWaL-qgxVrQFaO_TxsrC4Is0V1sFbDwCgg> .
        # The same digest again, as a name instead of a literal. Two registries handed the same
        # digest derive the same RFC 6920 IRI with no coordination, which is how they discover
        # they hold the same bytes; the checksum literal above joins with nothing. It hangs off
        # the distribution and not the artifact because bytes are what a distribution has — one
        # artifact may have several distributions with different digests.
```

Two nodes the record refers to are records of their own and come back from their own IRIs, not
with the artifact:

```turtle
<https://registry.example/artifact-series/01a05d84-…> a tar:ArtifactSeries ;
    skos:prefLabel "Validation report — batch 2" .
        # "This report, any version". No distributions, no licence, no bytes — which is why it
        # is a node of its own rather than a second dcat:Dataset.

<https://registry.example/agent/01a05d84-…>
    a schema:SoftwareApplication , prov:SoftwareAgent , prov:Agent ;
    schema:name            "shacl-manager" ;
    schema:softwareVersion "2.1.0" ;
    prov:actedOnBehalfOf   <https://orcid.org/0000-0002-1825-0097> .
        # Written only when both roles are filled: the system acted for the person. PROV says
        # delegation on the agents, not on the attributions.
```

Two more choices are worth naming, because a reader meeting them for the first time would
reasonably expect something else. Neither appears above, since both are on Software:

* **`dct:abstract` for the one-line summary.** The short tagline is `dct:abstract`, "a summary
  of the resource", while the long form stays `schema:description`. `schema:slogan` was rejected
  because its `domainIncludes` is Organization, Brand and Product rather than a creative work.
* **`dcat:endpointDescription` for API documentation.** The object is a node whose IRI *is* the
  document's own URL, typed `dct:Standard` and carrying `tar:apiFormat` plus a `dct:conformsTo`
  pointing at the specification the document follows. DCAT's own definition — "a description of
  the service's operations and how to invoke them" — is what an OpenAPI document is.
