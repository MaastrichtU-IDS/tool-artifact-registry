# `tar:` vocabulary audit

| | |
|---|---|
| **Status** | Applied |
| **Date** | 2026-08-30 |
| **Scope** | Every term in `https://w3id.org/tar/ns#`, audited against DCAT 3 / DCAT-AP, DCTERMS, PROV-O, schema.org + Bioschemas, ADMS, VoID, DQV, ODRL, FOAF, DOAP, CodeMeta, biotoolsSchema, SKOS, RO-Crate and the EU authority tables |

Every invented term is a federation cost: a peer registry, a DCAT-AP harvester or a generic
SPARQL client understands the standard term and not ours. This audit checked each `tar:` term
against the vocabularies above — fetching the actual definitions, domains and ranges rather
than working from memory — and replaced, supplemented, or kept-with-evidence accordingly.

**Ground rules applied:**

- Where a standard term has the same meaning *and* a compatible domain/range, it replaces the
  `tar:` term outright. The old term is marked `owl:deprecated true` in `shapes/vocab.ttl`
  and is still **read** as a fallback (both in projections and via SPARQL property-path
  alternation, e.g. `prov:wasAssociatedWith|tar:atInstance`), so graphs written before the
  audit stay queryable. It is never written again.
- Where a standard term is coarser than ours but is what harvesters actually read, **both**
  are written: the `tar:` literal stays authoritative for the registry's own logic, the
  standard triple is the interoperable supplement.
- Where nothing standard fits, the term is kept and its `rdfs:comment` in `shapes/vocab.ttl`
  now states specifically what was checked and why it did not fit.
- The JSON API in `src/model.rs` is unchanged throughout; this is the RDF underneath.

## Replaced terms

| `tar:` term | Now written as | Evidence | Notes |
|---|---|---|---|
| `tar:atInstance` (Run → Instance) | `prov:wasAssociatedWith` | [PROV-O §wasAssociatedWith](https://www.w3.org/TR/prov-o/#wasAssociatedWith) — domain `prov:Activity`, range `prov:Agent` | This is PROV-O's own unqualified form of the `prov:qualifiedAssociation` the registry already writes; the Instance is a `prov:SoftwareAgent`. The old vocab comment even admitted it ("the authoritative form is prov:qualifiedAssociation"). Exact match. |
| `tar:externalKey` (Run, Artifact) | `dct:identifier` | [DCMI Terms §identifier](https://www.dublincore.org/specifications/dublin-core/dcmi-terms/#http://purl.org/dc/terms/identifier) — "an unambiguous reference to the resource within a given context" | A CI system's run key (`gh-actions/12345/attempt-1`) is exactly an identifier-in-a-context. `adms:identifier` was considered and rejected: it requires an `adms:Identifier` node with agency/notation, which overstates a plain opaque idempotency token. |
| `tar:homeRegistry` (Instance → Registry) | `dcat:inCatalog` **plus** the catalog-side `dcat:resource` edge | [DCAT 3 vocabulary Turtle](https://www.w3.org/ns/dcat.ttl): `dcat:inCatalog owl:inverseOf dcat:resource`, new in DCAT 3, with the scope note "MAY be used only in addition to its inverse" — hence both directions are written | Registration of an Instance now also asserts `<registry> a dcat:Catalog ; dcat:resource <instance>`, so the catalog membership is visible to DCAT clients from either end. |
| `tar:tagline` (Software) | `dct:abstract` | [DCMI Terms §abstract](https://www.dublincore.org/specifications/dublin-core/dcmi-terms/#http://purl.org/dc/terms/abstract) — "a summary of the resource" | The long-form text stays on `schema:description`. `schema:slogan` was rejected: its `domainIncludes` is Organization/Brand/Place/Product, not CreativeWork. |
| `tar:kind` (Software; service\|library\|cli\|workflow) | `schema:applicationCategory` (alone) | [schema.org/applicationCategory](https://schema.org/applicationCategory) — domainIncludes SoftwareApplication | The registry already wrote **both** predicates with the identical value; the `tar:` triple was pure duplication and is simply dropped. The SHACL `sh:in` constraint moved to the schema term. |
| `tar:maturity` (Software) | `codemeta:developmentStatus` (`https://w3id.org/codemeta/terms/developmentStatus`) | [CodeMeta terms](https://codemeta.github.io/terms/): "Description of development status, e.g. active, inactive, suspended. See repostatus.org"; namespace resolves via w3id | Values remain free-text literals (the UI treats them so); [repostatus.org](https://www.repostatus.org) IRIs are the recommended values going forward. `adms:status` rejected: workflow status of an asset, not software lifecycle. |
| `tar:contact` (Software → Agent) | `codemeta:maintainer` (`https://w3id.org/codemeta/terms/maintainer`) | [CodeMeta terms](https://codemeta.github.io/terms/): "Individual responsible for maintaining the software (usually includes an email contact address)" | `schema:maintainer` is still in schema.org's *pending* area; CodeMeta v3 deliberately mints its own IRI for that reason and matches the intended "responsible contact" semantics. `dcat:contactPoint` rejected: its range is `vcard:Kind`, ours are `schema:Person`/`schema:Organization`. |

## Kept, with a standard supplement written beside it

| `tar:` term | Supplement written | Evidence | Why both |
|---|---|---|---|
| `tar:availability` (Distribution, Instance; public\|restricted\|embargoed\|metadata-only) | `dct:accessRights` with the [EU Access Rights authority table](http://publications.europa.eu/resource/authority/access-right): public→`PUBLIC`, restricted→`RESTRICTED`, embargoed→`NON_PUBLIC`, metadata-only→`NON_PUBLIC` | [DCAT 3 §6.8.6](https://www.w3.org/TR/vocab-dcat-3/#Property:distribution_access_rights) defines `dct:accessRights` on `dcat:Distribution` as well as on `dcat:Resource`; the table's members were fetched and verified (PUBLIC, RESTRICTED, NON_PUBLIC, SENSITIVE, CONFIDENTIAL, NORMAL, OP_DATPRO) | The EU table has **no embargo concept** and nothing that distinguishes "described but not retrievable" from merely non-public — and the registry's SHACL rules (`metadata-only` ⇒ no `downloadURL`) key on exactly those distinctions. So the four-way literal stays authoritative and the standard triple is the deliberately lossy DCAT-AP reading. Note `dcatap:availability` is a *different* concept (planned persistence of a distribution) and was not confused with this. |
| `tar:status` (Run; success\|failed\|running\|aborted) | `schema:actionStatus` with an [ActionStatusType](https://schema.org/ActionStatusType) member (success→`CompletedActionStatus`, failed/aborted→`FailedActionStatus`, running→`ActiveActionStatus`), and the Run additionally typed `schema:Action` so the property sits on its intended domain | [schema.org/actionStatus](https://schema.org/actionStatus) — domain Action, range ActionStatusType; the four enumeration members were verified | The enumeration has no distinct member for *aborted* (it folds into failed) — the mapping is non-injective, so replacing outright would lose information the UI and failure metrics use. PROV models run state only via `prov:endedAtTime` presence, which loses failure semantics entirely. |
| `tar:tombstoned` / `tar:tombstonedAt` | `adms:status` → [`dataset-status/WITHDRAWN`](http://publications.europa.eu/resource/authority/dataset-status) | [W3C ADMS](https://www.w3.org/ns/adms) `adms:status` ("status of the Asset in the context of a particular workflow process", domain `rdfs:Resource`); the EU dataset-status members were fetched and verified (COMPLETED, DEPRECATED, DEVELOP, DISCONT, OP_DATPRO, WITHDRAWN) | The boolean stays as the cheap query key. `prov:invalidatedAtTime` was rejected as the sole standard form: its domain is `prov:Entity`, and tombstones also apply to Software and Instances (agents). |

## Kept as-is (nothing standard fits)

Each of these now carries an `rdfs:comment` in `shapes/vocab.ttl` recording the search, so a
reader does not have to redo it.

| `tar:` term | What was checked, in short |
|---|---|
| `tar:Software`, `tar:Release`, `tar:Instance` (marker classes) | A Release is *also* a `schema:SoftwareApplication`, so SHACL `sh:targetClass` and count queries need discriminators; no vocabulary separates abstract software from a versioned release as classes. |
| `tar:Capability`, `tar:hasCapability`, `tar:produces`, `tar:consumes` | [Bioschemas ComputationalTool](https://bioschemas.org/profiles/ComputationalTool) `input`/`output` ([bioschemas.org/properties/input](https://bioschemas.org/properties/input), verified to resolve) expect `FormalParameter` nodes attached to the tool itself — a different shape with an indirection this model does not need, given the object here already *is* the EDAM/`skos:Concept` type IRI. biotoolsSchema's function/input/output is JSON/XML, served by `/export/biotools` (spec §7.2), not an RDF vocabulary. wf4ever `wfdesc:hasInput/hasOutput` expect workflow parameter nodes. The Capability node stays, doubling as the `prov:Plan`. |
| `tar:runsRelease` | schema.org (nothing), DOAP (`doap:release` is Project→Version, wrong direction/subject), PROV (`prov:hadPlan` exists only on a qualified Association), DCAT (`dcat:version` is a literal). "Deployment X currently runs release Y" has no standard property. |
| `tar:instanceOf` | `dct:isVersionOf` would claim the Instance is a *version* of the Software; `prov:specializationOf` claims identity; nothing in schema.org/DOAP/DCAT expresses "deployment of". Denormalised on purpose (an Instance may predate any Release). |
| `tar:usedRelease` | PROV deliberately has no unqualified activity→plan property; `prov:used` is unusable because this registry's readers treat `prov:used` as "consumed artifact" (lineage traversal, counts). The authoritative `prov:qualifiedAssociation`/`prov:hadPlan` is always written beside it. |
| `tar:accessProtocol` | DCAT 3 conveys protocol only implicitly (accessURL scheme, `dcat:accessService`); no protocol property in DCAT/DCAT-AP/VoID/schema.org. |
| `tar:authMethod` | DCAT 3: nothing. ODRL: permissions/duties, not authentication mechanics. `schema:conditionsOfAccess`: free text for humans. Machine-readable auth otherwise lives only inside OpenAPI documents behind `dcat:endpointDescription`. This is the FAIR A1.2 hint. |
| `tar:accessRequestURL` | Nearest are `dcat:landingPage` (dataset-level, generic navigation) and `odrl:hasPolicy` (states the policy, not where to apply). DCAT-AP 3 has no access-request property; HealthDCAT-AP is still drafting in this space — revisit when it stabilises. |
| `tar:defaultMediaType` | `dcat:mediaType` is defined on `dcat:Distribution`; putting it on a `skos:Concept` (ArtifactType) would misstate its domain. Scheme-level UI metadata. |
| `tar:readme`, `tar:readmeBaseURL` | CodeMeta's `readme` is a **URL to** a README file, verified at [codemeta.github.io/terms](https://codemeta.github.io/terms/); ours is the inline Markdown content, stored so the UI renders without a network call. |
| `tar:containerImage`, `tar:imageDigest`, `tar:installCommand` | CodeMeta (`buildInstructions` is a URL to docs), schema.org (`downloadUrl` — an OCI ref is not a URL), DOAP: no terms for OCI references, image digests, or install one-liners. |
| `tar:jurisdiction` | `dct:spatial` asserts spatial *coverage* of the data, not the law an operator answers to; DPV's `dpv:hasJurisdiction` expects a Location resource and drags in a policy framework for one field. |
| `tar:health` | Volatile, registry-observed liveness. DQV measures dataset quality, not service uptime; no catalogue vocabulary models it. |
| `tar:openLineagePayload`, `tar:claimedNamespace` | OpenLineage is JSON, not RDF; the payload is preserved verbatim by design (spec §7.6). The claimed namespace is deliberately *not* `dct:identifier` — it is an unverified claim, recorded as a label only (spec §8.3). |
| `tar:oidcClientId`, `tar:oidcIssuer`, `tar:allowedScope` | Workload-identity binding (addendum D12–D15). No RDF vocabulary models identity-provider client binding or token scopes. |
| `tar:jsonField` | An extra triple on a standard `sh:ValidationResult` mapping it to the JSON input field; plain SHACL consumers ignore it. |

## What a harvester or generic SPARQL client can now understand

Before the audit, the following facts about our records were expressed **only** in the
invented namespace and were invisible to anything that had not read our vocabulary. Now:

- **Who performed a run.** `?run prov:wasAssociatedWith ?agent` — the single most common
  PROV query pattern — now works against this registry, from any PROV-aware client,
  alongside the qualified form. Previously only `tar:atInstance` held the one-hop edge.
- **Whether data is accessible.** A DCAT-AP harvester reading
  `?distribution dct:accessRights ?r` gets EU authority-table IRIs — the exact value set
  DCAT-AP mandates — for every distribution and data-serving instance. Open-data portals can
  correctly file our `metadata-only` health-data artifacts as NON_PUBLIC instead of
  displaying them as undescribed.
- **Whether a run succeeded.** `?run schema:actionStatus schema:FailedActionStatus`, with
  runs typed `schema:Action` — legible to schema.org consumers with no profile knowledge.
- **Which catalog a deployment belongs to.** `dcat:inCatalog` / `dcat:resource` in both
  directions; a harvester walking the catalog now discovers instances as DCAT resources.
- **Software cards.** Short summary (`dct:abstract`), category
  (`schema:applicationCategory` without a proprietary duplicate), development status
  (`codemeta:developmentStatus`) and maintainer (`codemeta:maintainer`) are all in
  vocabularies that CodeMeta-based tooling (e.g. software heritage/scholarly-infrastructure
  crosswalks) already consumes.
- **External identifiers.** Run and artifact keys are `dct:identifier`, so deduplication and
  cross-referencing against other systems no longer require our namespace.
- **Deletion.** A tombstoned record is `adms:status` WITHDRAWN — a standard, queryable
  lifecycle statement rather than a proprietary boolean alone.

What still requires our vocabulary: capability matchmaking (`tar:produces`/`tar:consumes`),
the deployment structure (`tar:instanceOf`/`tar:runsRelease`), the fine-grained access
descriptor (`tar:accessProtocol`/`tar:authMethod`/`tar:accessRequestURL`/`tar:availability`'s
embargoed/metadata-only distinction), and operational/security bookkeeping. Each of those is
documented in `shapes/vocab.ttl` with the reason no standard term fits.

## Genuine modelling issues surfaced by the audit (not vocabulary problems)

1. **`availability` conflates two axes.** public/restricted is an access level;
   embargoed/metadata-only is an existence/temporal statement. This is why no single standard
   value set maps onto it. An embargo also has no end date in the model — if embargoes
   matter, `dct:available` (date the resource becomes available) is the standard carrier and
   would let embargoed→PUBLIC-from-date be said properly.
2. **A "metadata-only distribution" is a slightly odd object** — a `dcat:Distribution` that
   distributes nothing. The artifact-level absence of any distribution already encodes
   metadata-only (and `overall_availability` treats it so); the explicit metadata-only
   distribution row exists mainly to carry `tar:accessRequestURL`. Consider allowing
   `access_request_url` at artifact level and dropping the empty distribution.
3. **Capability cannot express constraints**, only type chips — already spec Q5. When that
   remodel happens, the Bioschemas `FormalParameter` shape is the natural target and would
   make `tar:produces`/`tar:consumes` replaceable after all.
4. **Catalog membership is asserted for Instances but not for Artifacts/Software.** A
   DCAT-AP harvester walking `dcat:resource` from the catalog finds deployments but not the
   datasets themselves; adding `dcat:dataset` edges on artifact registration would complete
   the DCAT catalog picture at one triple per artifact.


## Addendum: `kind` gained a `desktop` value

Registering RDFCraft — a Nuitka-packaged executable that opens a local `pywebview` window —
showed the `kind` list had no honest value for it. `service` was wrong (it is not hosted),
`library` was wrong (it is an application, not something you import), `workflow` was wrong, and
`cli` asserted a command-line interface it does not have. `desktop` was added.

Worth recording why this bit: `schema:applicationCategory`, which now carries this value, is
**free text** in schema.org. The closed `sh:in` list is ours, not the vocabulary's. Closing an
open term buys validation and costs expressiveness, and every value we failed to anticipate
becomes a write the registry rejects for no good reason. `capability` deliberately went the
other way with a free-IRI escape hatch (D11); `kind` has none, and that asymmetry is a
deliberate trade rather than an oversight — but it is one to revisit if a third case appears.


## Addendum: EuroSciVoc replaces EDAM for software topics

EDAM stays for artifact types and the biotoolsSchema export. It no longer classifies the
software, because it could not: asked to describe a SHACL validator, an ontology browser, a
schema builder and a CSV-to-RDF mapper, it returned *the same two topics* — "Ontology and
terminology" and "Data management" — for all four. Four agents classified independently and each
reached the same pair, because those are the only EDAM topics that fit a semantic-web estate.
A facet where every value has the same count as every record is not classifying anything.

EDAM is "an ontology of concepts prevalent within **bioinformatics and computational biology**".
These tools are not that. The mismatch showed up in the data branch too: `data_2600`, used here
for "an RDF graph", actually means *Pathway or network*.

The Software Ontology (SWO) was considered and rejected: it imports GO wholesale (its roots are
`molecular_function` and `biological_process`, and searching it for "validation" returns
`valid_for_go_annotation_extension`), and it has **no** term for SPARQL, SHACL or the semantic
web. Its genuine strength — 145 licence classes — is ground already covered by SPDX and CodeMeta.

[EuroSciVoc](http://data.europa.eu/8mn/euroscivoc/), the EU Science Vocabulary, has `semantic
web`, `ontology` (under *knowledge engineering*), `databases`, `software` and `software
development`. It is what DCAT-AP uses for `dct:subject`, so a harvester understands these records
without knowing anything about us — the same argument that put `dct:accessRights` on EU authority
IRIs. 1064 concepts, generated by `build.rs` from the Publications Office SPARQL endpoint.

The result, on the same four records:

| | before (EDAM) | after (EuroSciVoc) |
|---|---|---|
| shacl-rust | Ontology and terminology · Data management | ontology · software |
| sulo-schema-builder | Ontology and terminology · Data management | ontology · knowledge engineering |
| OntoExplorer | Ontology and terminology · Data management | ontology · semantic web · databases |
| RDFCraft | Ontology and terminology · Data management | semantic web · databases · software |

The facet went from two values covering everything to five that separate: filtering on
`semantic web` returns RDFCraft and OntoExplorer; on `software`, RDFCraft and shacl-rust.

EDAM's topic branch is still bundled, branched `topic-edam` so it is not offered in the picker
but any record still citing an EDAM topic — ours or a federated peer's — keeps rendering a label.
The JSON field is still named `edam_topics`; renaming it would break every existing caller for a
cosmetic gain, and it has always accepted any IRI (D11).
