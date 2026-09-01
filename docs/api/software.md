# Registering software

A software record describes the abstract program. Registering one needs the `curator` role or
the `register:software` scope.

```bash
curl -X POST -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{
       "name": "example-validator",
       "tagline": "Validates RDF graphs against shapes.",
       "description": "…",
       "homepage": "https://example.org/validator",
       "code_repository": "https://github.com/your-org/example-validator",
       "documentation": "https://example.org/validator/docs",
       "license": "https://spdx.org/licenses/Apache-2.0",
       "kinds": ["service", "cli"],
       "maturity": "active",
       "deployable": true,
       "topics": ["https://registry.example.org/… (from vocab search)"],
       "keywords": ["SHACL"],
       "publisher": {"name": "Your Organisation", "kind": "organization",
                     "homepage": "https://example.org"},
       "contact": {"name": "A Maintainer", "kind": "person",
                   "email": "maintainer@example.org"}
     }' \
     https://registry.example.org/api/v1/software
```

`PATCH /api/v1/software/{id}` edits it — sending only the fields you mean to change. `DELETE`
tombstones it.

## The fields worth thinking about

**`kinds`** is a set, from `service`, `library`, `cli`, `desktop`, `workflow`. One program is
routinely several of these, and forcing a single choice makes the catalogue lie about the
common case. There is a singular `kind` too, which is the first of `kinds`; it exists for older
clients and you should send `kinds`.

**`deployable`** says whether it makes sense for this software to have an endpoint at all. Set
it `false` for a library or a desktop application, and the registry stops asking for a URL that
does not exist — and refuses a deployment of it that carries one.

**`license`** is an SPDX IRI, not a string. `https://spdx.org/licenses/MIT`, not `MIT`. An
absent licence is rendered honestly as "licence not stated", which is strictly better than a
plausible guess; leave it out if the project does not state one.

**`topics`** are controlled. Look them up with `GET /api/v1/vocab/search?branch=topic&q=…` and
use the IRI verbatim; a topic the registry cannot resolve is a `422`. See
[Artifact types and topics](../vocabulary/terms.md).

**`readme`** carries the project's README so the detail page can render it. It is often the
largest thing in the record — if you are importing many, `TAR_MAX_PAYLOAD_BYTES` is the setting
that will bite first. `readme_base_url` is what relative image and link paths in it resolve
against.

**`api_docs`** is a list of `{url, format, title, description}`, where format is one of
`openapi`, `asyncapi`, `graphql`, `sparql-service-description`, `ols4`, `postman`, `other`. The
registry can fetch and render one at `GET /api/v1/software/{id}/api-doc`.

**`registration_clients`** lists the OIDC client ids allowed to self-register deployments of
this software. See [Registering a deployment](deployments.md#registration_clients).

## Releases

A release is a versioned, runnable plan.

```bash
curl -X POST -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{
       "version": "2.1.0",
       "date_published": "2026-08-30",
       "container_image": "ghcr.io/your-org/example-validator:2.1.0",
       "image_digest": "sha256:ab12…",
       "install_command": "pipx install example-validator==2.1.0",
       "changelog": "https://example.org/validator/changelog#2.1.0",
       "downloads": [{"url": "https://example.org/…/validator-2.1.0-linux-x86_64.tar.gz",
                      "platform": "linux-x86_64", "byte_size": 8412233,
                      "availability": "public"}]
     }' \
     https://registry.example.org/api/v1/software/$SOFTWARE_ID/releases
```

```
GET    /api/v1/software/{id}/releases
POST   /api/v1/software/{id}/releases
DELETE /api/v1/software/{id}/releases/{release_id}
```

A release may carry its own `capability`, which is how a capability that changed between
versions is recorded truthfully rather than by overwriting the software's.

## Declaring a capability

What the software is *able* to produce and consume, as artifact type IRIs:

```bash
curl -X PUT -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{"produces": ["https://registry.example.org/type/shacl-validation-report"],
          "consumes": ["https://registry.example.org/type/rdf-graph",
                       "https://registry.example.org/type/shacl-shapes-graph"]}' \
     https://registry.example.org/api/v1/software/$SOFTWARE_ID/capability
```

`PUT /api/v1/instances/{id}/capability` does the same for one deployment, when a particular
installation can do more or less than the software in general.

This is what makes matchmaking work on a registry with no runs in it. See [Searching and
matchmaking](search.md).

Every IRI here is held to the vocabulary rule.

## Keeping a record in step with its repository

A software record may name a source repository the registry will re-read.

```bash
curl -X PATCH -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{"sync": {"source": "github", "repo": "your-org/example-validator",
                   "fields": ["tagline", "description", "readme", "license", "maturity"],
                   "enabled": true}}' \
     https://registry.example.org/api/v1/software/01a05…

curl -X POST -H "Authorization: Bearer $CURATOR" \
     https://registry.example.org/api/v1/software/01a05…/sync
```

**Sync overwrites only the fields the record named as managed.** Everything else belongs to
whoever curated it and is left alone, even when the repository has an obvious value for it.

That constraint is the whole design. A sync that helpfully refreshed everything would silently
discard the sentence a curator wrote because it was better than the repository's one-liner, and
the loss would be invisible until somebody noticed the page had got worse. Naming the managed
fields makes the trade explicit at the moment somebody opts in.

The record reports what the last run changed, in `sync.last_changed`, alongside
`last_synced_at`, `last_status` and `last_error` — so a sync that has been quietly broken for a
month is visible on the page rather than inferred from staleness.

Which fields may be managed is a fixed set; `list_enumerations` on the [MCP server](../mcp.md)
reports it, as does the UI.

### Credentials for a private repository

A public repository needs none. A private one needs a token, and there are two ways to get one,
in order of preference:

1. **the signed-in curator's own forge token, brokered by the identity provider** — then the
   registry reads exactly what that person can read, and nothing more;
2. **`TAR_FORGE_TOKEN`**, a registry-wide token. Simpler, and it means every curator can pull
   anything that token can see.

## Reading and filtering

```
GET /api/v1/software    ?q= ?kind= ?topic= ?keyword= ?license= ?publisher= ?produces= ?consumes= ?registry=
GET /api/v1/software/{id}
GET /api/v1/software/{id}/api-doc
GET /api/v1/software/{id}/export/biotools
```

The listing returns `facets` alongside the items — value counts for licence, kind and topic —
so a filter UI does not have to fetch the catalogue to know what is worth offering.

`export/biotools` projects a record into the bio.tools interchange shape, for estates that
already publish there.
