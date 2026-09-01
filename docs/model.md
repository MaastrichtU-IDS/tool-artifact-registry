# The model

Four layers, plus two things that hang off them. This chapter is the conceptual model; for the
RDF it becomes — the classes, the properties, and why the registry has a vocabulary of its own
at all — see [The `tar:` ontology](ontology.md).

```
Software                      abstract: the program, its repository, licence, responsible party
  └─ Release                  a versioned, runnable plan — a tag, an image digest, a download
       └─ Deployment          an installation of that release; the agent that actually acts
            └─ Run            one execution
                 ├─ used      → Artifact     a consume advertisement
                 └─ generated → Artifact     a produce advertisement
                                  └─ Distribution   how to get at it, or how to ask
```

## Why four and not two

The layer that people want to collapse is the deployment, and collapsing it is what breaks the
model. A run is performed by *something that exists somewhere* — it has an endpoint, an
operator, a jurisdiction and a credential. Abstract software has none of those. If runs
attached to software, then "which of our installations produced this?" would have no answer,
two organisations running the same program would be indistinguishable, and there would be
nothing for a credential to identify.

So the deployment is the acting agent, and one rule follows from it that the whole
authorisation model rests on: **a deployment may only advertise runs in which it is itself the
agent**, and which deployment that is comes from the credential, never from the request body.

The release layer earns its place more quietly. It is what makes "this deployment is three
versions behind" a fact the registry can state, and it is where a capability can change: a
tool that gained an output format in v3 declares that on the release, not on the software.

## Software

The abstract program. Not a copy of it, not a place to run it — the thing you would name in a
sentence.

Notable fields:

- `kinds` — a **set**, not one choice, from `service`, `library`, `cli`, `desktop`, `workflow`.
  One program is routinely several: a library with a CLI wrapper and a hosted service. There is
  a singular `kind` field too; it is the first of `kinds`, kept so older clients still read.
- `deployable` — whether it makes sense for this software to have an endpoint at all. A library
  or a desktop application is not deployable, and marking it so is what stops the registry
  demanding a URL that does not exist. A deployment of non-deployable software may not carry an
  endpoint; that is enforced on write.
- `maturity` — a [repostatus.org] development status. Set it only if the project declares one.
- `topics` — what the software is *about*. Controlled; see [Vocabulary](vocabulary/terms.md).
- `capability` — what it can produce and consume. See below.
- `sync` — a source repository the registry may keep named fields in step with. See
  [Registering software](api/software.md#keeping-a-record-in-step-with-its-repository).

## Release

A versioned, runnable plan. A version string, optionally a publication date, a container image
and its digest, an install command, a changelog, and any number of downloads.

A release may carry its own `capability`, which is how a capability that changed between
versions is recorded truthfully.

## Deployment

An installation. It has a `label`, the `software` it is a deployment of, optionally the
`release` it runs, an `endpoint_url` if it is reachable, an `operator`, an `availability` and a
`jurisdiction`.

It also carries whatever identifies it as a caller: an `oidc_client_id` and `oidc_issuer`, or
registry-minted API tokens, and `allowed_scopes` bounding what those credentials may do. See
[How a tool authenticates](api/authentication.md).

### Health

The registry probes deployment endpoints in the background and records `health` as `up`,
`down` or `unknown`, with `health_checked_at` and a `health_detail`. This is observed, never
written by a caller — a deployment asserting that it is up is a claim, and the interesting case
is exactly the one where it cannot answer.

A record may name a `health_endpoint`, a URL whose only job is to say the deployment is alive.
That is held to a **2xx**. Leave it out and the `endpoint_url` itself is probed, where anything
that answers at all counts as up — because a great many healthy services return `401` or `404`
at their root, and marking those down would be a false alarm about a working deployment.

A deployment with no endpoint is never probed and never reports "down" for it. Its liveness
signal is `last_seen_at`, stamped when it announces itself or advertises a run.

## Run

One execution, performed by one deployment. A `status` of `success`, `failed`, `running` or
`aborted`, a start and end time, optionally the `release` that ran and an `external_key`.

The `external_key` is how a run in some other system — a CI job id, a workflow attempt — is
named here, and it is what makes advertisement idempotent. Retrying the same CI step does not
duplicate lineage.

## Artifact

A data artifact: what it is, who made it, what produced it, and how to get at it. The registry
never holds the bytes.

The field that carries the most weight is `conforms_to` — the artifact's **type**. It is what
every capability query and every subscription filter matches on, exactly, so it is controlled:
it must be a term the registry holds. See [Artifact types and topics](vocabulary/terms.md).

Lineage lives on the artifact too, as `was_derived_from`, `was_revision_of` and `is_version_of`,
alongside the `was_generated_by` link the run advertisement creates. Any of those may point at
a foreign IRI at another registry.

### Distribution

How to get at an artifact, or how to ask. An artifact has zero or more.

A distribution carries an `access_url` or a `download_url`, a `media_type`, a `byte_size`, a
`checksum`, an `access_protocol` (`https`, `http`, `s3`, `sparql`, `oci`, `ipfs`, `file`), an
`auth_method` (`none`, `apikey`, `oauth2`, `basic`, `signed-url`), an `availability`, and an
`access_request_url` for the case where the answer is "apply".

The media type belongs here and not on the artifact, deliberately. A serialisation is not a
kind of thing: the same shapes graph re-serialised from Turtle to N-Triples is the same
artifact.

### Availability, and the honest absence

`availability` is one of:

| | |
|---|---|
| `public` | anyone can get it |
| `restricted` | access needs an agreement or an account |
| `embargoed` | it will become available later |
| `metadata-only` | the bytes are not obtainable from here at all |

`metadata-only` is the one that matters. It means the artifact is findable, described, and
provably not retrievable — and the registry makes that provable rather than inferable. There is
no `download_url` at all, the UI renders no download affordance, and the [Signposting] `Link`
headers omit `rel="item"`. A machine can therefore tell "no bytes here" from "bytes behind
auth" without parsing the body and guessing.

This is the concrete form of the commitment that FAIR is not open. A registry that quietly
omitted a URL would leave a client unable to distinguish a policy from an oversight.

## Capability

A declaration of what some software, release or deployment is *able* to do, expressed as sets
of artifact types it `produces` and `consumes`.

It is separate from the run graph on purpose, and both are first-class:

- the **capability** answers *"what could produce this kind of artifact?"* before anything has
  ever run;
- the **run graph** answers *"what did produce this one, and who used it?"* afterwards.

A registry with no runs in it is still useful for the first question, which is the question you
have when you are choosing a tool.

Declared and observed capability can disagree. The registry does not currently reconcile them;
see [Limitations](limitations.md).

## Artifact series

Artifacts that are successive versions of the same thing are linked with `is_version_of` into a
series, which is what lets a detail page show siblings rather than a wall of near-duplicates.

## Records that are withdrawn

Deleting a record tombstones it rather than erasing it: the IRI still resolves, and says that
it was withdrawn. An identifier that has been published and then returns `404` is a broken
promise, and something somewhere is still citing it. A withdrawn record leaves the lists it was
in, so it stops appearing in search and in listings, but a client holding its IRI still gets a
truthful answer.

## Peer records

A record whose `origin.kind` is not local came from a peer registry. It is a cached stub, held
in a named graph of that peer's own and never mixed into local data. It carries `cached_at` and
a `resolve_status`, and the UI marks it with an origin chip. See
[Federation](api/federation.md).

[repostatus.org]: https://www.repostatus.org/
[Signposting]: https://signposting.org/
