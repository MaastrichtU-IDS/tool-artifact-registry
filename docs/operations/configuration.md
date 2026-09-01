# Configuration

Everything is an environment variable. **`TAR_BASE_IRI` is the only universally required one** —
the registry cannot mint dereferenceable identifiers without knowing what it is called, and
refuses to start without it. Everything else has a working default, so `docker run` with one
variable is a complete install.

`tar config` prints the effective configuration with secrets redacted.

## Core

| Variable | Default | |
|---|---|---|
| `TAR_BASE_IRI` | — | **Required.** The `http(s)` URL the registry is reachable at. Becomes part of every identifier it mints, permanently. |
| `TAR_LISTEN` | `0.0.0.0:8080` | |
| `TAR_DATA_DIR` | `./data` | The graph store and the SQLite database. `memory` for an ephemeral store. |
| `TAR_STATIC_DIR` | `frontend/dist` if it exists | The built UI. Unset and with no such directory, the registry serves the API only. |
| `TAR_TITLE` | `Tool Artifact Registry` | Shown in the UI and in `llms.txt`. |
| `TAR_OPERATOR` | — | Who runs this registry. Reported in `/.well-known/tar-registry`. |
| `TAR_ROOT_TOKEN` | — | Bootstrap admin credential. Refuses a placeholder or anything under 16 characters. |
| `TAR_MAX_PAYLOAD_BYTES` | `2MiB` | Accepts `KiB`, `MiB`, `MB` suffixes. Raise it if you import software records with large READMEs. |
| `TAR_LOG` | `info,tower_http=warn` | Tracing filter, `tracing-subscriber` `EnvFilter` syntax. |

## Graph store

| Variable | Default | |
|---|---|---|
| `TAR_SPARQL_ENDPOINT` | — | An external SPARQL 1.1 Query endpoint to use **instead of** the embedded store. Unset, the registry uses embedded Oxigraph under `TAR_DATA_DIR` — which is what every existing install does, and nothing about it changes. |
| `TAR_SPARQL_UPDATE_ENDPOINT` | the query endpoint | Many servers split query and update onto separate URLs. |
| `TAR_SPARQL_BEARER_TOKEN` | — | Bearer credential for the endpoint. |
| `TAR_SPARQL_USERNAME` / `TAR_SPARQL_PASSWORD` | — | HTTP basic credential. Both or neither, and not alongside a bearer token. |
| `TAR_SPARQL_TIMEOUT` | `60s` | Per request. |

`TAR_DATA_DIR` still holds the SQLite operational database either way; only the graph moves.
Details, and what atomicity means over HTTP, in [Graph store](graph-store.md).

## Read access

| Variable | Default | |
|---|---|---|
| `TAR_PUBLIC_READ` | `true` | Serve reads without a credential. |
| `TAR_SPARQL_PUBLIC` | `true` | Serve SPARQL without a credential. |

The two are **independent**, and that is deliberate: SPARQL is a public read surface in its own
right, and losing it whenever an operator closes REST reads would make the two settings one. An
operator who wants a genuinely private registry has to say so about the query endpoint too.

`/healthz`, `/readyz`, `/metrics`, `/api/v1/registry`, `/api/v1/context`, `/api/v1/whoami`, the
`/.well-known/` documents and the MCP handshake stay reachable either way — a probe or a
discovery document that needs a credential fails for the wrong reason.

## Validation

| Variable | Default | |
|---|---|---|
| `TAR_SHACL_VALIDATE_WRITES` | `true` | Off downgrades SHACL violations to warnings. |

It does **not** switch off the two rules that need the rest of the graph — that an artifact type
is a term the registry holds, and that a deployment of non-deployable software may not carry an
endpoint. A half-described record is a trade an operator can make; an unlookuppable type is not,
because it silently breaks matchmaking and subscriptions rather than one record.

## Identity

Covered in [Identity provider setup](identity-provider.md): `TAR_OIDC_ISSUER`,
`TAR_OIDC_CLIENT_ID`, `TAR_OIDC_CLIENT_SECRET`, `TAR_OIDC_AUDIENCE`,
`TAR_OIDC_REQUIRE_AUDIENCE`, `TAR_OIDC_ROLES_CLAIM`, `TAR_OIDC_CLIENT_CLAIM`,
`TAR_OIDC_SCOPE_CLAIM`, `TAR_WORKLOAD_ISSUERS`, `TAR_OIDC_AUTO_REGISTER_INSTANCES`.

`TAR_DEV_INSECURE_JWT` short-circuits token verification. Test only.

## Health probing

| Variable | Default | |
|---|---|---|
| `TAR_HEALTH_CHECK_ENABLED` | `true` | Probe deployment endpoints in the background. |
| `TAR_HEALTH_CHECK_INTERVAL` | `5m` | |
| `TAR_HEALTH_CHECK_TIMEOUT` | `5s` | |
| `TAR_HEALTH_CHECK_BATCH` | `20` | Deployments probed per pass. |
| `TAR_HEALTH_ALLOW_PRIVATE` | `true` | Probe private and loopback addresses. |

Private addresses are allowed **by default here and refused by default for webhooks**, which
looks inconsistent and is not. A deployment endpoint is an address in your own estate, and for an
internal registry it is normally private — refusing those would mean the feature never worked
where it is most wanted. A webhook URL is chosen by whoever registers a subscription and points
anywhere.

## Federation

| Variable | Default | |
|---|---|---|
| `TAR_PEER_RESOLVE_ENABLED` | `true` | Fetch stubs for foreign IRIs in the background. |
| `TAR_PEER_RESOLVE_TTL` | `24h` | How long a cached stub stays fresh. |
| `TAR_PEER_RESOLVE_TIMEOUT` | `5s` | |
| `TAR_FEDERATED_SEARCH_TIMEOUT` | `3s` | Per peer. |
| `TAR_FEDERATED_SEARCH_TOTAL_TIMEOUT` | `10s` | Whole fan-out. Max 60s. |
| `TAR_FEDERATED_SEARCH_MAX_HOPS` | `3` | Max 8. |
| `TAR_FEDERATED_SEARCH_HOP_MARGIN` | `600ms` | Held back before forwarding, so a hop can still answer. |
| `TAR_FEDERATED_SEARCH_MAX_PEERS` | `12` | Max 64. |
| `TAR_FEDERATED_SEARCH_MAX_PEER_HITS` | `100` | Max 1000. |
| `TAR_FEDERATED_SEARCH_MAX_PEER_BYTES` | `2 MiB` | Max 32 MiB. |
| `TAR_FEDERATED_SEARCH_MAX_PEER_STATUSES` | `32` | Max 256. |
| `TAR_FEDERATED_SEARCH_MAX_TOTAL_HITS` | `500` | Max 5000. |
| `TAR_FEDERATED_SEARCH_ID_TTL` | `10m` | How long a query id is remembered for loop detection. |

See [Federation](../api/federation.md).

## Subscriptions

| Variable | Default | |
|---|---|---|
| `TAR_SUBSCRIPTION_WEBHOOKS` | `true` | Run the delivery worker at all. |
| `TAR_SUBSCRIPTION_TICK` | `5s` | Worker poll interval. |
| `TAR_SUBSCRIPTION_BATCH` | `20` | Deliveries attempted per tick. Max 500. |
| `TAR_SUBSCRIPTION_TIMEOUT` | `5s` | Per attempt. Capped at 30s. |
| `TAR_SUBSCRIPTION_MAX_ATTEMPTS` | `8` | Before one delivery is dead. Max 20. |
| `TAR_SUBSCRIPTION_SUSPEND_AFTER` | `12` | Consecutive failures before the webhook is suspended. |
| `TAR_SUBSCRIPTION_BACKOFF_BASE` | `30s` | |
| `TAR_SUBSCRIPTION_BACKOFF_MAX` | `6h` | |
| `TAR_SUBSCRIPTION_ALLOW_HTTP` | `false` | Otherwise HTTPS only. |
| `TAR_SUBSCRIPTION_ALLOW_PRIVATE_TARGETS` | `false` | Otherwise private, loopback and link-local targets are refused. |

See [Subscriptions](../api/subscriptions.md).

## MCP

| Variable | Default | |
|---|---|---|
| `TAR_MCP_ENABLED` | `true` | Serve `/mcp` at all. |
| `TAR_MCP_READ_ONLY` | `false` | Hide and refuse every write tool. |
| `TAR_MCP_ALLOWED_ORIGINS` | base IRI only | Comma-separated extra `Origin` values. |
| `TAR_MCP_SCOPES` | — | `scopes_supported` in the metadata document. Set only if your authorization server actually knows these scope names. |

See [The hosted MCP server](../mcp.md).

## Repository sync and API documents

| Variable | Default | |
|---|---|---|
| `TAR_FORGE_TOKEN` | — | Registry-wide forge token for reading private repositories. |
| `TAR_APIDOC_ALLOW_PRIVATE` | `true` | Fetch API description documents from private addresses. |

## Build-time

These affect `build.rs`, not the running server.

| Variable | |
|---|---|
| `TAR_UPDATE_EDAM` | Force an upstream vocabulary check rather than waiting for the daily one. |
| `TAR_EDAM_OFFLINE` | Skip the check entirely. Fails the build if there is no committed bundle. |

## Durations and sizes

Durations accept `30s`, `5m`, `24h`, `7d`, or a bare number of seconds. Sizes accept `KiB`,
`MiB`, `MB`, or bare bytes. Booleans accept `1`, `true`, `yes` or `on` — except the
`TAR_HEALTH_*` switches, which currently recognise only `1` and `true`. Prefer `1` or `true`
everywhere and the difference never comes up.
