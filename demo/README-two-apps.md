# Demo — two applications, coordinating through the registry

Two programs run on your machine. One builds an ontology and says so. The other has a standing
interest in ontologies, is told, fetches it, ingests it, and says what that produced. **Neither
one knows the other exists.** The registry is the only thing between them.

```bash
./demo/run-two-app-demo.sh          # start a private registry, run both apps, leave it up
./demo/run-two-app-demo.sh --stop   # stop the registry and both apps
./demo/run-two-app-demo.sh --clean  # stop, and delete this demo's data directory
```

It starts its **own** registry on a free port with its **own** data directory (`demo/.run/data`).
Port 8099 and `./data` are never touched. To run it against a registry that is already up:

```bash
TAR_URL=http://127.0.0.1:8099 TAR_ROOT_TOKEN=… ./demo/run-two-app-demo.sh
```

In that mode nothing is deleted; the demo only adds records. It will add two `Software` records
whose names end in `(simulated)`, five artifact types, two deployments and two credentials.

---

## The two programs

| | | |
|---|---|---|
| [`apps/sulo_app.py`](apps/sulo_app.py) | **sulo-app** — a stand-in for a `sulo-schema-builder` deployment | builds a schema model, renders it to OWL and to SHACL shapes, serves both, advertises them |
| [`apps/onto_app.py`](apps/onto_app.py) | **onto-app** — a stand-in for an `OntoExplorer` deployment | subscribes to OWL ontologies, pulls its deliveries, fetches, ingests, advertises what it derived |

Both are Python 3 with **only the standard library** — no dependency to install, because what is
being demonstrated is the HTTP contract, not a client library. Between them they use nine
endpoints and nothing else:

```
GET   /api/v1/whoami                                  what did my credential resolve to?
GET   /api/v1/instances/{id}                          my own record
PATCH /api/v1/instances/{id}                          …and where I am answering, once I know
GET   /api/v1/instances/{id}/subscriptions            do I already have one?
POST  /api/v1/instances/{id}/subscriptions            register a standing interest
GET   /api/v1/subscriptions/{sid}/deliveries          the pull path
POST  /api/v1/subscriptions/{sid}/deliveries/ack      advance the cursor
POST  /api/v1/advertise/produced                      I made this
POST  /api/v1/advertise/consumed                      I used this
```

Each holds its own credential — a token scoped to `advertise:produce` and `advertise:consume`,
written into `demo/.run/{sulo,onto}.json` by the orchestrator the way an operator would
provision a deployment. Neither app ever sees the root token or the other app's token, and
neither touches the database or the graph.

Each also binds a **free port at start-up** and then records that address on its own Instance
record (`PATCH /api/v1/instances/{id}`, which a deployment is allowed to do for itself). That is
how an endpoint nobody knew in advance ends up in the registry. It is an address for *fetching*
files, not one the registry could deliver notifications to — which is exactly why the subscriber
still pulls.

---

## What happens, step by step

```
  onto-app                       registry                        sulo-app
     │                              │                                │
     │ POST …/subscriptions ───────▶│                                │
     │   conforms_to: owl-ontology  │                                │
     │   availability: public|restr │                                │
     │   roles: produced            │                                │
     │   exclude_own: true          │                                │
     │                              │◀──── POST /advertise/produced ─┤  1 run
     │                              │      2 artifacts               │  2 artifacts
     │                              │                                │
     │                        match in memory,                       │
     │                        INSERT one delivery row  ← only the OWL one
     │                              │                                │
     │ GET …/deliveries ───────────▶│                                │
     │◀──── seq 1, the whole artifact record                         │
     │                                                               │
     │ GET  http://…/biobank.ttl ───────────────────────────────────▶│  the bytes
     │ verify sha256 against the advertisement                       │
     │ parse, count, close the subclass hierarchy                    │
     │                              │                                │
     │ POST /advertise/consumed ───▶│  prov:used                     │
     │ POST /advertise/produced ───▶│  3 artifacts, prov:wasDerivedFrom
     │ POST …/deliveries/ack ──────▶│  cursor := 1                   │
```

**The subscriber starts first.** A subscription only ever sees what arrives after it exists;
there is no backfill. The orchestrator waits for onto-app's subscription to be registered
before it starts sulo-app, and that ordering is part of the demo, not an accident of it.

**Two artifacts go out, one delivery comes back.** sulo-app advertises the ontology *and* a
SHACL shapes graph on the same run. onto-app's filter asks for `owl-ontology`, so the shapes
never reach it. That is the filter doing its job somewhere you can see it.

**The ontology is real.** Eighteen OWL classes with a four-deep hierarchy, five object
properties (one transitive), five datatype properties (all functional), two disjointness
axioms, an existential restriction and a defined class — rendered from the model sulo-app
holds, and parseable by any RDF toolchain. Every checksum, byte size and timestamp in the
registry is computed from the bytes actually served.

**The ingest is real too, and honest about its limits.** onto-app parses the Turtle with a
small subset reader, counts terms, verifies the advertised sha256 against what it received,
and computes the **transitive closure of the asserted named-superclass edges** — 12 subclass
axioms that the file does not state. That is genuine entailment, and it is also the cheapest
kind there is. The restriction and the defined class in the ontology are exactly what an
ELK-style reasoner would use, and they are ignored here. Both the code and the artifact's own
description say so, rather than calling closure "reasoning".

---

## What the subscription actually guarantees

This is the part worth reading carefully, because a subscriber built on a wrong belief about
it fails in ways that only show up under load or after a crash.

### At-least-once. Never exactly-once.

A delivery is a row in SQLite. Reading it changes nothing. It stops being handed to you when
**you** say you are finished with it, and not before. So:

- crash after doing the work and before acknowledging → you get it again;
- read a page and never come back → you get the same page again;
- acknowledge before doing the work → you have silently dropped an ontology, and nothing
  anywhere will tell you.

onto-app therefore acknowledges *after* the artifacts are on disk and in the registry, and
keys its own idempotency on the **artifact IRI** in `ingested.json`. The demo makes this
visible on purpose: after finishing, it re-reads from `cursor=0`, the same delivery comes back,
and the second pass is a no-op instead of a second set of derived artifacts.

Note what at-least-once does *not* mean here. A delivery row is unique per
`(subscription, artifact, role)`, so re-advertising the same artifact does not queue a second
notification. The duplicate you must survive is the *same row, re-read* — not a second row.

### The acknowledged cursor

The cursor is the delivery row's `seq`: `INTEGER PRIMARY KEY AUTOINCREMENT`, so a deleted row
can never let a later row reuse a number a client has already passed.

- `GET …/deliveries` **with no cursor** resumes from the subscription's own acknowledged
  position. A subscriber that keeps no state of its own still makes progress — onto-app relies
  on this and never sends a cursor in its normal loop.
- `POST …/deliveries/ack {"cursor": n}` is **monotonic**. A stale acknowledgement never rewinds
  and never replays work already done. Try it: acknowledging `0` after acknowledging `1` leaves
  the cursor at `1`.
- `remaining` in the response says how much is left after this page, so a subscriber decides
  whether to keep going without a second round trip.
- `?ack=true` acknowledges everything in the same round trip. Convenient, and wrong for
  anything that does real work with what it read — it turns at-least-once into at-most-once.

### Ordering: one integer, and not much else

The pull path returns rows in ascending `seq` — the order the registry *noticed* the matches.
That is worth exactly what it says and no more:

- it is not causal order of anything in the world;
- the webhook channel makes no ordering promise at all, because a failed delivery backs off
  while the ones behind it go through;
- the acknowledged position is a **single integer**, so there is no way to acknowledge a
  delivery out of order. A subscriber that fans deliveries out to workers must acknowledge only
  the contiguous prefix it has actually finished, or it will lose the gaps.

### Why a pull path exists at all

onto-app has no inbound address. It binds a random loopback port for the files it serves, it is
behind whatever your machine is behind, and there is no URL the registry could POST to. That is
not a limitation of the demo — it is the normal case. The registry already models CLI, desktop
and batch software (`deployable = false`, an Instance with no `dcat:endpointURL`), and a
webhook-only design would exclude exactly those deployments.

So a subscription with no `webhook_url` is an ordinary pull subscription, both channels drain
the same rows, and they can never disagree about what matched. The registry would also refuse a
webhook pointing at `127.0.0.1` anyway — that is the SSRF guard, and it is right to.

---

## Modelling decisions, and why

**Three derived artifacts, not three distributions of one.** The entailed axioms, the term
index and the ingest metrics are not other ways of obtaining the ontology; they did not exist
before the run. Each is its own artifact with `prov:wasDerivedFrom` pointing back. The test is
the one the other demo states: *would a byte-for-byte comparison against the source succeed?*
If yes it is a distribution, if no it is a derived artifact.

**The term index is `metadata-only`.** It is a set of rows in a database. There is no file, and
there never will be, so its distribution carries **no `downloadURL` at all** — not an empty
one, not a broken one — and names an `accessRequestURL` instead. You can see the model doing
its job in the HTTP headers:

```
$ curl -sD- -o/dev/null -H 'Accept: text/turtle' …/artifact/<the ontology>   | grep item
link: <http://…/biobank.ttl>; rel="item"; type="text/turtle"

$ curl -sD- -o/dev/null -H 'Accept: text/turtle' …/artifact/<the term index> | grep item
                                          # nothing. There are no bytes, and it says so.
```

The `Accept` header is load-bearing: the Signposting headers come with the machine-readable
representations (`application/json`, `text/turtle`, `application/ld+json`) and **not** with the
HTML one, so a bare `curl -I` on the same URL shows none of them. See the next section.

**Both sides of the run are advertised.** onto-app posts to `/advertise/consumed` before it
posts to `/advertise/produced`, on the same run key. Without the consume side the three derived
artifacts would appear from nowhere; with it, one `prov:Activity` carries `prov:used` and
`prov:generated` and the lineage reads in both directions. `GET /runs` shows the ingest run as
*used 1, generated 3*.

**Idempotent run keys.** Both advertisements use `ontoexplorer/ingest/<artifact-id>`, so a
replayed delivery attaches to the same Run instead of inventing a second one.

**The ontology has `creators`; the derived artifacts do not.** A person wrote the schema, so
the ontology carries an ORCID. Nobody authored the transitive closure — a program derived it,
and that is already recorded by the registry itself: the Run, the Instance, and
`prov:wasAttributedTo` taken from the credential rather than from the payload. Filling in
`creators` there would be decoration.

**`spatial` and `temporal_start`/`temporal_end` are absent.** They describe the coverage of
*data*. An ontology has neither a place nor a period, and a field filled with plausible noise
is worse than a field left out — an absent field is a fact about the artifact.

**The ORCID is Josiah Carberry's.** `0000-0002-1825-0097` is ORCID's own public example record,
a deliberately fictitious researcher. Attaching a real person's ORCID to a demo artifact would
be inventing a fact about a real human being; a made-up number would resolve to nothing.

**The two `Software` records are simulations and say so.** Their names end in `(simulated)` and
their descriptions open by stating that they are not the real deployed tools and do not talk to
them. No claim is made here about the real `sulo-schema-builder` or `OntoExplorer`.

---

## Sharp edges hit while building this

**`tar:accessProtocol` has no `http`.** Its vocabulary is `https | s3 | sparql | oci | ipfs |
file` (spec §6.1, enforced by `shapes/tar-shapes.ttl`). Both apps serve their files over plain
HTTP on loopback, which is neither `https` nor `file`. Writing `https` would be a lie in a field
a client might act on, so the demo **omits the field** on those distributions and records the
gap here. Every intranet deployment serving over plain HTTP has the same problem. The fix is one
value in one Turtle file.

**Signposting is absent from the HTML representation.** Spec §6.3 says *"every artifact and
software `GET` emits Signposting `Link` headers"*. In practice they are emitted for
`application/json`, `text/turtle` and `application/ld+json`, and not for `text/html` — so
`curl -I <artifact URL>`, which is the first thing anyone tries, returns none of them. That is
the one representation Signposting was designed around: the whole point of the convention is
that a machine landing on the human-facing page can find the bytes, the licence and the author
without parsing it. This is a registry-side observation, not a demo workaround; the demo just
sends an explicit `Accept`.

**`PATCH /api/v1/instances/{id}` is a replace, not a merge.** It rebuilds the record from the
body, so a deployment recording its own endpoint with `{"label": …, "endpoint_url": …}` would
silently drop its operator, availability, jurisdiction, allowed scopes and OIDC client binding.
Both apps therefore do a read-modify-write ([`tarclient.announce_endpoint`](apps/tarclient.py)),
and the comment there says why. A tool author who does not read the handler source has no way to
know — `PATCH` means merge to almost everyone.

**A subscription has no backfill.** It sees what arrives after it exists, and there is no way to
say "and everything from the last hour that would have matched". The only catch-up is
`GET /api/v1/artifacts?…`, whose query parameters are not the subscription filter model, so a
subscriber that was down has to reimplement its own filter against a different vocabulary. This
demo sidesteps it by starting the subscriber first; a real deployment restarting after an outage
cannot.

**A deployment cannot register the artifact type it produces.** `POST /api/v1/types` requires a
curator, so the type IRIs are provisioned into each app's config by the orchestrator. That is
almost certainly the right call — a vocabulary anyone can extend at will stops being one — but it
means "deploy a new tool" is a two-party operation, and it is worth stating rather than
discovering.

Everything else the demo needed, the API had.

---

## Files

```
demo/
  run-two-app-demo.sh      the orchestrator: registry, records, credentials, both apps, the story
  apps/tarclient.py        a JSON client, a file server, sha256, endpoint self-registration
  apps/sulo_app.py         the producer
  apps/onto_app.py         the subscriber
  .run/                    created at run time: data dir, logs, pids, per-app config, output
```

`demo/.run/` is git-ignored and safe to delete. It holds the demo registry's data directory,
each app's credential, and the files the apps generated and served.

---

## Relationship to the older demo

[`run-demo.sh`](run-demo.sh) is a different scenario — four tools and one pizza ontology, loaded
as data rather than produced by running programs. It is **kept**, and its script is unchanged;
the only edit is a note added to the top of [`README.md`](README.md).

That note is there because two pieces of its modelling were flagged as wrong by the repository
owner and are **not** fixed: it registers RDFCraft as a deployable service producing RDF, and it
binds shacl-rust's own CI as a deployment that validates third-party ontologies. Correcting them
means deciding what those records should say instead, which is the owner's call and not a demo
author's. Nothing here reuses those records or repeats those choices. The two demos share only
the artifact types `owl-ontology` and `shacl-shapes-graph`, which are re-registered by slug and
are therefore the same concept rather than a duplicate of it.
