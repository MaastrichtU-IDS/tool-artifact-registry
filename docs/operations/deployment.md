# Deployment

What you are deploying is one statically linked binary that also serves the built UI, plus one
directory of state. There is no application server, no separate database process, and no
background daemon to install. `docker run` with one environment variable is a complete install;
everything below is about making that survivable.

Three shapes, in increasing order of ceremony:

| | For |
|---|---|
| [A single container](#a-single-container) | One host, one registry. The quickest real deployment. |
| [Compose](#compose) | The same, plus the pieces it can use — an identity provider, an external graph store. |
| [Kubernetes](#kubernetes) | A cluster you already run. |

All three are the same image with the same settings; only the thing that supervises it changes.

## Decide `TAR_BASE_IRI` before anything else

It is the only universally required setting, and the only one you cannot change later.

Every identifier this registry mints is built from it. A record registered while
`TAR_BASE_IRI` is `https://registry.example.org` is called
`https://registry.example.org/software/01a05d4c-…` — permanently, in the graph, in every
response, in every peer that has cross-linked to it, and in every file anyone has exported.
Change the base IRI afterwards and those identifiers do not move: they stay in the store,
pointing at a host that no longer answers for them. Nothing rewrites them. `tar dump` will show
you how many you invalidated, and that is all the help there is.

So, before first boot:

- It must be the URL people and machines actually reach the registry at — **the public one**,
  the one on the certificate, the one in the ingress rule. Not the pod IP, not the service name,
  not `localhost` because that is what you tested with.
- The scheme must be the scheme they use. `https://` in the base IRI with a plain-HTTP ingress
  mints identifiers that do not dereference.
- No trailing slash (one is trimmed), and no port unless the port is genuinely part of the URL.
- It is also the default audience a signed-in person's token must carry. See [Identity provider
  setup](identity-provider.md), where that catches everybody.

The registry refuses to start without it, and refuses anything that is not an `http(s)` URL.

If you must change it, treat it as a migration and not a config edit: dump, decide what the old
identifiers should do — a redirect from the old host is the only thing that keeps them working —
and restore into a registry that has never been anything else.

## A single container

```bash
docker volume create tar-data
export TAR_ROOT_TOKEN=$(openssl rand -hex 24)

docker run -d --name tar \
  -p 8080:8080 \
  -e TAR_BASE_IRI=https://registry.example.org \
  -e TAR_ROOT_TOKEN="$TAR_ROOT_TOKEN" \
  -v tar-data:/data \
  --restart unless-stopped \
  ghcr.io/maastrichtu-ids/tool-artifact-registry:0.1.0
```

That is the whole install. The image already sets `TAR_DATA_DIR=/data`,
`TAR_LISTEN=0.0.0.0:8080` and `TAR_STATIC_DIR=/ui`, declares `/data` as a volume, and carries a
`HEALTHCHECK` that runs `tar healthcheck`.

Optionally load the worked example so the first page is not empty — with the server **stopped**,
because both stores are single-writer:

```console
$ docker run --rm -e TAR_BASE_IRI=https://registry.example.org -v tar-data:/data \
    ghcr.io/maastrichtu-ids/tool-artifact-registry:0.1.0 seed
{
  "artifacts": 19,
  "instances": 5,
  "runs": 12,
  "software": 4,
  "types": 16
}
```

### What persists, and what does not

Everything that matters is under `/data`, and nothing that matters is anywhere else:

| Under `/data` | |
|---|---|
| the graph store | Every record — software, releases, deployments, runs, artifacts, minted vocabulary terms, cached peer stubs. |
| a SQLite database | Hashed API tokens, peers, the audit log, federation cursors, idempotency keys, subscriptions and their delivery queues. |

Back up the volume and you have backed up both. Lose it and you have lost every issued token and
the audit log even if the graph lives in an external store — only the *graph* moves when you set
`TAR_SPARQL_ENDPOINT`, never the operational database.

The SHACL shapes and the bundled vocabularies are compiled into the binary and reloaded into the
store on every start, so they need no volume and a restored dump does not have to carry them.

### Running it unprivileged

The binary writes only under `TAR_DATA_DIR`. It runs with a read-only root filesystem, as a
non-root user, with every capability dropped:

```bash
docker run -d --name tar \
  --read-only --user 65532:65532 --cap-drop ALL --security-opt no-new-privileges \
  -p 8080:8080 \
  -e TAR_BASE_IRI=https://registry.example.org \
  -e TAR_ROOT_TOKEN="$TAR_ROOT_TOKEN" \
  -v tar-data:/data \
  ghcr.io/maastrichtu-ids/tool-artifact-registry:0.1.0
```

The volume has to be writable by that user — `chown 65532:65532` on a bind mount, or an
`fsGroup` on Kubernetes, which the manifests set. This is verified, not assumed: seeding,
serving, and a `POST /api/v1/software` all succeed under those restrictions.

## Compose

`compose.yaml` at the root of the repository is the same single container, supervised:

```bash
export TAR_BASE_IRI=https://registry.example.org
export TAR_ROOT_TOKEN=$(openssl rand -hex 24)
docker compose up -d
```

`TAR_ROOT_TOKEN` has no default and compose refuses to start without one, which is deliberate:
a bootstrap credential every install shares is not a credential.

### An external graph store

`compose.yaml` carries a Fuseki under a profile, off unless you ask for it:

```bash
docker compose --profile external-store up -d
```

and the `TAR_SPARQL_*` settings to point the registry at it, commented out beside it. The volume
is still required — see above. [Graph store](graph-store.md) covers what the two backends share
and where they differ.

### With an identity provider

`compose.identity.yaml` brings up the registry **and** a Keycloak with the realm already
imported, in one command:

```bash
docker compose -f compose.identity.yaml up -d --wait
```

Then <http://127.0.0.1:8099>, sign in as `curator` / `curator-password`.

This exists because doing it by hand took two commands and then an audience that had to be set
by hand — and getting that wrong produces a sign-in that appears to succeed at the identity
provider and then fails at the registry, which is a miserable thing to debug from either end.
Here the registry is served on `http://127.0.0.1:8099`, one of the two origins the bundled
realm's audience mappers already name, so there is nothing to set:

A token minted by that Keycloak, with nothing configured by hand, comes out carrying
`aud: ["http://127.0.0.1:8099", "http://127.0.0.1:8098", "account"]` — and the registry accepts
it:

```console
$ curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8099/api/v1/whoami
{"authenticated":true,"credential":"oidc-human","display_name":"curator",
 "is_curator":true,"roles":["reader","curator"], …}
```

Two things to know before changing it:

- **`TAR_PORT` may be 8099 or 8098, and nothing else.** Those are the origins
  `deploy/keycloak/realm-tar.json` carries audience mappers and redirect URIs for. Any other
  value needs a mapper added to that file.
- **The registry uses the host network.** The registry compares a token's `iss` to
  `TAR_OIDC_ISSUER` byte for byte and fetches the signing keys from that same URL, so the issuer
  string has to be true for the browser *and* for the registry. On a bridge network those are
  two different strings. Sharing the host's network namespace makes one of them work for both.

It is a development stack and says so: Keycloak runs in development mode, over plain HTTP, with
an in-memory database wiped on `down`, and passwords committed to the repository on purpose. A
real deployment points `TAR_OIDC_ISSUER` at a real identity provider and adds the audience
mapper there — see [Identity provider setup](identity-provider.md).

## Kubernetes

`deploy/kubernetes/` holds plain manifests assembled with kustomize, which `kubectl` has built
in:

```bash
kubectl kustomize deploy/kubernetes          # see exactly what will be applied
kubectl apply -k deploy/kubernetes
```

Manifests rather than a chart. There is one workload, one service and one volume here; a chart
would buy templating this does not need, at the cost of a values file standing between the
reader and the object that actually gets created. The one thing a chart would genuinely unify —
keeping the ingress hostname and `TAR_BASE_IRI` in step — is done with a kustomize
`replacement`, so the ingress host and the certificate's host are *derived* from
`TAR_BASE_IRI` rather than repeated next to it.

### What you edit

One line, in `kustomization.yaml`:

```yaml
configMapGenerator:
  - name: tar-config
    literals:
      - TAR_BASE_IRI=https://registry.example.org
```

The ingress `host` and the TLS `hosts` entry follow from it automatically. Change it and check:

```console
$ kubectl kustomize deploy/kubernetes | grep -E 'TAR_BASE_IRI|host:'
  TAR_BASE_IRI: https://registry.example.org
  - host: registry.example.org
```

### What you create out of band

Never in a manifest, never in git:

```bash
kubectl -n tar create secret generic tar-secrets \
  --from-literal=TAR_ROOT_TOKEN="$(openssl rand -hex 24)"
```

Add the graph-store credential to the same secret if you use an external endpoint —
`TAR_SPARQL_USERNAME` and `TAR_SPARQL_PASSWORD`, or `TAR_SPARQL_BEARER_TOKEN`. The Deployment
takes them with `envFrom.secretRef`, so nothing enumerates them in a file that gets committed.

### The volume

`TAR_DATA_DIR` is a `ReadWriteOnce` PersistentVolumeClaim mounted at `/data`. Both stores under
it are single-writer, which decides two other things in the manifest:

- **`replicas: 1`.** The embedded graph store takes an exclusive lock on its directory; a second
  pod does not share it, it fails to start.
- **`strategy: Recreate`.** A rolling update would start the new pod while the old one still
  held the lock, and it would crash-loop until the rollout gave up. Recreate trades a few
  seconds of downtime for an upgrade that works.

### Probes

Both endpoints exist and answer different questions, so they are used for different things:

| | |
|---|---|
| `GET /healthz` | Static `{"status":"ok"}`. **Liveness.** A liveness probe that also checks a dependency restarts a healthy process because something else broke. |
| `GET /readyz` | Counts the graph and runs `SELECT 1` against SQLite. **Readiness**, and the startup probe. A pod whose external SPARQL endpoint is unreachable stops taking traffic without being killed. |
| `GET /metrics` | Prometheus text: total triples, records by kind, peers configured and failing. |

All three are reachable regardless of `TAR_PUBLIC_READ` — a probe that needs a credential fails
for the wrong reason.

The startup probe allows up to 150 seconds. Boot itself is fast (about 0.3 s to listening on a
warm volume), but a first boot on cold storage loads the shapes and the bundled vocabularies
into an empty store, and a startup probe that is too tight turns a slow first start into a crash
loop.

### The ingress

`TAR_MAX_PAYLOAD_BYTES` defaults to 2 MiB and ingress-nginx caps request bodies at 1 MiB, so the
manifest sets `nginx.ingress.kubernetes.io/proxy-body-size: "2m"`. Without it a large software
record is rejected by the proxy, with the proxy's error rather than the registry's. Keep the two
in step if you raise either.

## The published image

`ghcr.io/maastrichtu-ids/tool-artifact-registry`, built and pushed by
`.github/workflows/release.yml` on every push to the default branch and every `v*` tag.

| Tag | |
|---|---|
| `0.1.0`, `0.1` | From a `v0.1.0` tag. What a deployment should pin. |
| `latest` | The most recent release tag. Not the default branch. |
| `main`, `main-<sha>` | The tip of the default branch, for when you deliberately want it. |

`linux/amd64` and `linux/arm64`, each built on a runner of that architecture and joined into one
manifest list, so `docker pull` gets the right one with no `--platform`. Each release carries a
signed build provenance attestation:

```bash
gh attestation verify oci://ghcr.io/maastrichtu-ids/tool-artifact-registry:0.1.0 \
  --owner MaastrichtU-IDS
```

**Packages on GHCR are created private, and that is a setting on the package rather than on the
repository.** A public repository does not make its images public. Until someone changes it in
the package's own settings, `docker pull` from a machine that is not logged in fails with an
authentication error that looks like the image does not exist:

```bash
echo "$GITHUB_TOKEN" | docker login ghcr.io -u <username> --password-stdin
```

## Configuration that matters in production

[Configuration](configuration.md) is the complete list. These are the ones a deployment gets
wrong:

| | |
|---|---|
| `TAR_BASE_IRI` | Required, permanent. See the top of this page. |
| `TAR_ROOT_TOKEN` | The bootstrap admin. In a Secret. Refused if it is a recognisable placeholder or shorter than 16 characters. Issue real tokens and stop using it. |
| `TAR_PUBLIC_READ` | `true`. Anonymous reads. |
| `TAR_SPARQL_PUBLIC` | `true`. Anonymous SPARQL. **Independent of the above on purpose** — a private registry has to say so about both, because SPARQL is a read surface in its own right and closing REST reads should not silently close it. |
| `TAR_OIDC_ISSUER`, `TAR_OIDC_CLIENT_ID` | Browser sign-in. The client needs an audience mapper for `TAR_BASE_IRI`, which is the mistake everyone makes once. |
| `TAR_WORKLOAD_ISSUERS` | Extra issuers accepted for *workload* tokens only — a Kubernetes API server, a CI provider. They are trusted to say which deployment is calling and nothing else; only `TAR_OIDC_ISSUER` may assert roles. Getting that backwards hands the registry to anyone who can open a pull request. |
| `TAR_SPARQL_ENDPOINT` | An external graph store instead of the embedded one. Setting it is the whole switch. |
| `TAR_OPERATOR` | Who runs this. Reported in `/.well-known/tar-registry`. |

`tar config` prints the effective configuration with secrets redacted, reading the environment
exactly as `serve` does — so it answers "why is this registry behaving like that" without
starting it:

```console
$ docker run --rm -e TAR_BASE_IRI=… -e TAR_ROOT_TOKEN=… -v tar-data:/data \
    ghcr.io/maastrichtu-ids/tool-artifact-registry:0.1.0 config
base_iri              https://registry.example.org
data_dir              /data
graph store           embedded oxigraph at /data/graph
listen                0.0.0.0:8080
public_read           true
sparql_public         true
shacl_validate_writes true
root_token            set
static_dir            /ui
oidc issuer           (unset)
```

### An external graph store

`TAR_SPARQL_ENDPOINT` points the registry at any SPARQL 1.1 endpoint instead of the embedded
store; its absence is the whole switch, so an existing install changes nothing.
[Graph store](graph-store.md) is the detail. For a deployment, three things matter:

- **The volume is still required.** Only the graph moves. Tokens, peers, the audit log,
  subscriptions and the federation cache stay in SQLite under `TAR_DATA_DIR`.
- **Credentials belong in a Secret.** `TAR_SPARQL_USERNAME`/`TAR_SPARQL_PASSWORD` or
  `TAR_SPARQL_BEARER_TOKEN`. Setting both forms is an error rather than a silent preference.
- **Readiness follows it.** `/readyz` touches the store, so an unreachable endpoint takes the
  pod out of the load balancer rather than serving empty results. That is the point: a query
  that returns nothing because the server is down looks exactly like a registry with no records.

## Upgrades

Pull the new tag and restart. There is no migration step to run by hand:

- The SQLite schema migrations run on every start.
- The SHACL shapes and the bundled vocabularies are reloaded from the binary into the store on
  every start. That is idempotent, and it is also how a graph migration is applied.

Two things to know:

- **Take a backup first.** See below. Migrations are applied to the volume in place.
- **A shapes change can strand a record.** A write is judged on the whole record it asserts, so
  a record citing a vocabulary term the registry has since retired is refused on an edit to some
  entirely different field. The boot log names every such record and the term, once, rather than
  deleting a value nobody asked it to delete. Read the boot log after an upgrade.

On Kubernetes the `Recreate` strategy means an upgrade is a short outage rather than an overlap;
that is a consequence of the single-writer store, not a choice about availability.

## Backup and restore

[Backup and restore](backup.md) is the reference. What a deployment needs:

**The whole of it is `TAR_DATA_DIR`.** Snapshot the volume with the process stopped and you have
everything, including the operational database that a graph dump does not contain.

**To back up a running registry, use the HTTP endpoint, not the CLI.** `tar dump` boots its own
handle on the store, and the store is single-writer, so it fails against a live server:

```console
$ docker exec tar /tar dump
Error: opening graph store at /data/graph

Caused by:
    IO error: While lock file: /data/graph/LOCK: Resource temporarily unavailable
```

`GET /admin/dump` serves the same N-Quads over HTTP, for admins, from the running process:

```bash
curl -H "Authorization: Bearer $TAR_ROOT_TOKEN" \
  https://registry.example.org/admin/dump > registry.nq
```

Restoring is the reverse, into a stopped registry:

```console
$ docker run --rm -e TAR_BASE_IRI=… -v tar-data:/data -v "$PWD":/backup:ro \
    ghcr.io/maastrichtu-ids/tool-artifact-registry:0.1.0 restore --nquads /backup/registry.nq
loaded 1456 quads
```

The count is the quads that were *new*: the shapes and vocabularies are already in the store,
loaded from the binary at boot, so a restore does not re-add them. Verified round trip — a
12,145-triple registry dumped and restored into an empty volume comes back at 12,145 triples
with the same record counts.

### The trap: `--graph` is not a backup

`tar dump` with no argument writes **N-Quads**, and the named graph is part of the meaning —
which graph a statement is in is what distinguishes this registry's records from a peer's cached
stub.

`tar dump --graph <g>` and `/admin/dump?graph=<g>` write **N-Triples**, because that is what the
single-graph consumers want. Restoring *that* file puts its triples in the default graph, where
nothing looks for them:

```console
$ head -1 registry.nq        # four terms, the graph last
<https://w3id.org/tar/ns#ReachableShape> <http://www.w3.org/ns/shacl#not> _:b0 <urn:tar:shapes> .

$ head -1 local.nt           # three terms, no graph
<https://registry.example.org/artifact/01a05d4c-…> <http://www.w3.org/ns/prov#wasAttributedTo> <urn:tar:seed> .
```

Use the whole-store dump for backups. See [Limitations §18](../limitations.md).

## Where the documentation lives

The site is published from the default branch to
<https://maastrichtu-ids.github.io/tool-artifact-registry/>.
