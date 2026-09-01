# Backup and restore

## What state there is

Two stores under `TAR_DATA_DIR`:

| | Holds |
|---|---|
| The graph store | Every record: software, releases, deployments, runs, artifacts, vocabulary, cached peer stubs. |
| A SQLite database | Hashed API tokens, peers, the audit log, federation cursors, idempotency keys, subscriptions and their delivery queues. |

They are separate on purpose. The graph is the catalogue and is meant to be dumped, diffed and
reloaded; the SQLite side is operational bookkeeping that is mostly reconstructible and contains
the only secrets — hashed, but still.

Backing up the directory backs up both. Do it with the process stopped.

## Dump and restore the graph

```bash
tar dump > registry.nq                    # every graph, as N-Quads
tar dump --graph urn:tar:local > local.nq # just this registry's own records
tar restore --nquads registry.nq
```

N-Quads rather than Turtle because the named graph is part of the meaning — which graph a
statement is in is what distinguishes this registry's records from a peer's cached stub, and a
format that dropped it would silently merge the two. See [Named
graphs](../identifiers.md#named-graphs).

A dump is also the honest way to migrate to a new base IRI, in that it shows you exactly how
many identifiers you are about to invalidate. The registry will not rewrite them for you.

`GET /admin/dump` serves the same thing over HTTP, for admins.

## Shapes and vocabulary reload on boot

The SHACL shapes and the bundled vocabularies are reloaded from disk on every start. That is
idempotent, and it is also how a graph migration is applied: change the shapes file, restart,
and the new rules are in force for subsequent writes.

It also means a restored dump does not need to carry them.

### Records a shapes change can strand

A write is judged on the whole record it asserts, and a `PATCH` carries the fields the caller did
not name. So a record citing a vocabulary term the registry has since retired is refused on an
edit to some entirely different field.

The boot log names every such record and the term, once, rather than deleting a value nobody
asked it to delete. Replacing or clearing the named term fixes the record permanently. See
[Limitations](../limitations.md).

## Health and monitoring

```
GET /healthz     liveness
GET /readyz      readiness
GET /metrics     Prometheus text format
```

`/metrics` reports total triples, record counts by kind, and how many peers are configured and
how many are failing to resolve. All three are public regardless of `TAR_PUBLIC_READ`, because a
liveness probe that needs a credential is a liveness probe that fails for the wrong reason.

The container image's own healthcheck runs `tar healthcheck`.

## Auditing

Every write is recorded with the principal, the kind of actor, the operation and the record.
`GET /api/v1/audit` returns it, for admins.

It is in SQLite rather than in the graph, deliberately: the audit log is about the registry, not
part of the catalogue, and putting it in the graph would mean it federated.

## Checking the configuration

```bash
tar config
```

Prints the effective configuration with secrets redacted — the base IRI, data directory, listen
address, read and validation switches, whether a root token is set, the static directory, the
OIDC issuer and workload issuers, and the peer resolution settings. It reads the environment
exactly as `serve` does, so it answers "why is this registry behaving like that" without
starting it.
