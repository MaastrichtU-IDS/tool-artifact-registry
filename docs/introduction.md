# Introduction

The Tool Artifact Registry is a self-hostable catalogue of four things and the links between
them: the **software** an organisation has, the **deployments** that actually run it, the
**runs** those deployments perform, and the **data artifacts** those runs consume and produce.

It exists because those four are usually kept in four places that do not agree. A software
catalogue knows what exists but not where it runs. A monitoring system knows what is running
but not what it is for. A data catalogue knows a file exists but not which program wrote it. A
CI system knows a job ran but tells nobody. Each is right about its own slice and none of them
can answer a question that crosses two — *what in this estate can produce a validation report,
where is it deployed, and what did it last produce?*

Two capabilities follow from putting them in one graph, and they are the point:

**Matchmaking, before anything has run.** A deployment can declare what it is *able* to
produce and consume. That answers "what could produce this kind of artifact?" on an empty
registry, which is the question you have when you are looking for a tool rather than looking
for a file.

**Lineage, after it has.** A run links the artifacts it used to the artifacts it generated, so
"where did this file come from and who else used it?" is a graph walk rather than an
archaeology project.

## What it is not

It is not a data lake, an artifact store or a package registry. It holds **descriptions and
pointers**, never the bytes. An artifact record says what a thing is, who made it, what
produced it and how to get at it — or, honestly, that you cannot get at it from here.

It is not a monitoring system. It probes deployment endpoints for liveness because a catalogue
full of dead links is worse than useless, but one probe every few minutes is a freshness
signal, not an SLA.

It is not an authorization system for the data it describes. Access to an artifact is decided
by whoever serves the artifact.

## Design commitments

These shape everything else, so they are worth stating up front.

**RDF-native, not RDF-flavoured.** Records are quads in an [Oxigraph] store, described with
standard vocabularies — DCAT for datasets and distributions, PROV for lineage, SKOS for
concepts, schema.org and CodeMeta for software. There is a JSON API because JSON is what
clients want, but it is a projection of the graph, not the source of truth. A read-only SPARQL
endpoint is a first-class surface, not a debugging aid.

**Writes are validated by real SHACL.** `shapes/tar-shapes.ttl` is the rule set and a SHACL
engine enforces it before anything is committed. What the API accepts is changed by editing a
Turtle file, not by editing Rust. A rejected write returns `422` with the engine's own
`sh:ValidationReport`.

**FAIR is not open.** Findable and described does not imply retrievable, and the registry
refuses to blur the two. An artifact may be recorded as provably not obtainable here, and that
state is machine-detectable rather than something a client infers from a missing field. See
[Availability](model.md#availability-and-the-honest-absence).

**Federation is a cross-link, not a harvest.** Any object position in the graph may hold a
foreign IRI. A registry does not copy its peers' catalogues; it points at them and caches a
stub, in a named graph of that peer's own, never mixed with its own records.

**Every identifier dereferences.** A record's IRI is also its web page, its Turtle, its
JSON-LD and its Markdown. There is no separate "web view" URL to keep in step.

**Vocabulary is checked, not suggested.** An artifact type must be a term the registry
actually holds. A write naming one it cannot resolve is refused before anything is written,
with a message saying how to search for the right term, adopt an existing one, or mint a new
one. This exists because free-text classification degrades silently — see
[Artifact types and topics](vocabulary/terms.md).

## Status

A working prototype. Every endpoint in the [design record](specs/README.md) is implemented and
covered by tests. Where it departs from the design, or stops short of it, that is written down
in [Limitations](limitations.md) rather than left for you to discover.

## Where to go next

- [The model](model.md) — the four layers and why runs belong to a deployment.
- [Getting started](getting-started.md) — a registry running locally in a few commands.
- [Conventions](api/conventions.md) — then the API chapter for whatever you are trying to do.
- [Agent-facing surfaces](agents/surfaces.md) — if the client is a language model.

[Oxigraph]: https://github.com/oxigraph/oxigraph
