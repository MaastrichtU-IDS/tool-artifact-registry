# Identity provider setup

The registry can run with no identity provider at all — registry API tokens and a root token are
enough to administer it. Configuring one buys two things: people sign in instead of pasting
tokens, and deployments authenticate with credentials the registry never stores.

## Configuration

```bash
export TAR_OIDC_ISSUER=https://sso.example.org/realms/main
export TAR_OIDC_CLIENT_ID=tar-ui
```

That is the minimum for browser sign-in. Everything else has a working default:

| | Default | |
|---|---|---|
| `TAR_OIDC_ISSUER` | — | The human issuer. The **only** issuer allowed to assert roles. |
| `TAR_OIDC_CLIENT_ID` | — | The public client the browser uses. |
| `TAR_OIDC_CLIENT_SECRET` | — | Only if your client is confidential. |
| `TAR_OIDC_AUDIENCE` | `TAR_BASE_IRI` | The `aud` a token must carry. |
| `TAR_OIDC_REQUIRE_AUDIENCE` | `true` | Whether `aud` is required rather than merely checked when present. |
| `TAR_OIDC_ROLES_CLAIM` | `realm_access.roles` | Where human roles are read from. |
| `TAR_OIDC_CLIENT_CLAIM` | `azp` | Where a workload's client id is read from. |
| `TAR_OIDC_SCOPE_CLAIM` | `scope` | Where granted scopes are read from. |
| `TAR_WORKLOAD_ISSUERS` | — | Comma-separated. Accepted for **workload** tokens only. |
| `TAR_OIDC_AUTO_REGISTER_INSTANCES` | `false` | Let any accepted credential register a deployment of software it names itself. |

## The audience mapper, which is what catches everyone

`TAR_OIDC_AUDIENCE` defaults to `TAR_BASE_IRI`, and `TAR_OIDC_REQUIRE_AUDIENCE` defaults to
`true`. So **the client in your identity provider needs an audience mapper adding that exact
string.**

Without one, a typical provider issues an access token with an audience of its own account
service, sign-in completes at the provider, the browser comes back, and the registry rejects the
token. The symptom is a successful login followed immediately by a failure, which is a
confusing thing to debug from either end.

The audience is the base IRI, not the origin, not the sign-in redirect URL. If you serve the
registry on a second origin, that origin needs its own mapper.

Requiring the audience rather than merely checking it when present is deliberate. A token with
no audience is a token minted for nobody in particular, and accepting one means accepting any
token that issuer ever signs, for any application.

## Roles

Three, read from the roles claim:

| Role | Means |
|---|---|
| `reader` | Signed in; no write authority. |
| `curator` | Register and edit software, releases, deployments and vocabulary terms. |
| `admin` | Everything a curator can, plus peers and token administration. |

A signed-in person with none of them is exactly that: signed in, and able to read what anonymous
readers can read.

## Workload issuers

`TAR_WORKLOAD_ISSUERS` lists additional issuers accepted for **workload** tokens: a Kubernetes
API server, a CI provider's OIDC issuer, a partner's identity provider.

They are trusted to say *which deployment is calling* and nothing else. Only `TAR_OIDC_ISSUER`
may assert roles.

This is the single most important line in the auth configuration. A Kubernetes API server and a
CI provider can each mint a token containing a realm role called `admin`; if the registry
honoured that, adding a CI issuer would hand the registry to anyone who can open a pull request
against any repository on that platform.

See [How a tool authenticates](../api/authentication.md).

## A local provider to try it against

`deploy/keycloak/` holds a one-container Keycloak with an importable realm — three roles, a
PKCE public client, a service-account client for the workload path, and users with known
passwords — so both flows can be exercised for real rather than described.

```bash
docker compose -f deploy/keycloak/compose.yaml up -d
```

`deploy/keycloak/README.md` has the realm's users, clients, ports and redirect URIs, and the
exact environment to serve the registry with. The credentials in it are test values committed on
purpose: that is what makes the setup reproducible rather than click-configured. Nothing in that
directory belongs anywhere near a real deployment — it runs in development mode, over plain
HTTP, with a database that is wiped on every `down`.

Its clients already carry the audience mappers for the origins it documents. Serve on any other
origin and you must add one.

## Repository sync credentials

Keeping a software record in step with a private repository needs a forge token. If your
identity provider can broker one for the signed-in person, the registry reads exactly what that
person can read. Otherwise `TAR_FORGE_TOKEN` is a registry-wide fallback, which means every
curator can pull anything that token can see. See [Registering
software](../api/software.md#credentials-for-a-private-repository).

## What is not covered

**Token refresh.** The access token is used until it expires and there is no silent renewal, so
a long editing session ends in a `401` and a re-sign-in. The refresh token is deliberately not
stored in the browser. See [Limitations](../limitations.md).
