# A local Keycloak for the registry

A one-container Keycloak that imports a realm, so the OIDC paths of the
[workload-identity addendum](../../docs/specs/2026-08-30-workload-identity-addendum.md) can be
exercised for real — human sign-in with authorisation code + PKCE, and a deployment
authenticating with `client_credentials`.

Everything here is **local test configuration**. The passwords are in the realm file on
purpose: that is what makes this reproducible instead of click-configured, and what makes the
demo runnable by someone who just cloned the repo. Nothing in this directory belongs anywhere
near a real deployment — it runs Keycloak in `start-dev` mode, over plain HTTP, with an
in-memory database that is wiped on every `down`.

```bash
docker compose -f deploy/keycloak/compose.yaml up -d
# ready when this answers:
curl -s http://127.0.0.1:8090/realms/tar/.well-known/openid-configuration | head -c 80
```

## What is in the realm

Realm **`tar`**, issuer `http://127.0.0.1:8090/realms/tar`.

| Realm role | Means |
|---|---|
| `reader` | Signed in; no write authority. |
| `curator` | May register and edit software, releases and instances. |
| `admin` | Everything a curator can, plus peers and tokens. |

| User | Password | Realm roles |
|---|---|---|
| `curator` | `curator-password` | `reader`, `curator` |
| `registryadmin` | `admin-password` | `reader`, `curator`, `admin` |
| `reader` | `reader-password` | `reader` |
| `nobody` | `nobody-password` | *(none of ours — a signed-in person with no authority)* |

Keycloak's own admin console is at <http://127.0.0.1:8090/admin>, `admin` / `admin`.

| Client | Kind | For |
|---|---|---|
| `tar-ui` | public, PKCE `S256` required | The browser. Redirect URIs cover `127.0.0.1`/`localhost` on 8099, 8098 and the Vite dev server on 5173. |
| `shacl-manager-demo` | confidential, service accounts on, secret `shacl-manager-demo-secret` | A deployment's workload identity — put this client id in `tar:oidcClientId` on an Instance. |

**Both clients carry an audience mapper.** Without one, Keycloak's access token has
`aud: ["account"]` and the registry rejects it, because `TAR_OIDC_AUDIENCE` defaults to
`TAR_BASE_IRI` and `TAR_OIDC_REQUIRE_AUDIENCE` defaults to `true` (addendum §4). The mappers
add `http://127.0.0.1:8099` and `http://127.0.0.1:8098` so the realm works whichever of the two
the registry is served on. **Point the registry at a different origin and you must add a
mapper for it** — or the sign-in will complete at Keycloak and then fail here with
`JWT rejected: InvalidAudience`. That is the single most common way to misconfigure this.

## Running the registry against it

```bash
cargo build --release
(cd frontend && npm install && npm run build)

export TAR_BASE_IRI=http://127.0.0.1:8099
export TAR_LISTEN=127.0.0.1:8099
export TAR_DATA_DIR=./data-kc
export TAR_STATIC_DIR=frontend/dist
export TAR_ROOT_TOKEN=$(openssl rand -hex 24)

export TAR_OIDC_ISSUER=http://127.0.0.1:8090/realms/tar
export TAR_OIDC_CLIENT_ID=tar-ui
export TAR_OIDC_AUDIENCE=http://127.0.0.1:8099   # = TAR_BASE_IRI; shown for clarity
export TAR_OIDC_REQUIRE_AUDIENCE=true
export TAR_OIDC_CLIENT_CLAIM=azp
export TAR_OIDC_ROLES_CLAIM=realm_access.roles

./target/release/tar seed --from ids-examples
./target/release/tar serve
```

Open <http://127.0.0.1:8099>, press **Sign in → Continue with single sign-on**, and log in as
`curator` / `curator-password`. The header should show `curator · curator`, and:

```bash
curl -s localhost:8099/api/v1/whoami -H "Authorization: Bearer $TOKEN"
# {"credential":"oidc-human","is_curator":true,"roles":["reader","curator"], …}
```

`TAR_OIDC_ISSUER` must be the string Keycloak puts in `iss`, byte for byte — the registry
compares it as text. `127.0.0.1` and `localhost` are *not* interchangeable here, and neither is
a trailing slash on the port.

## Checking it without a browser

The `tar-ui` client has direct access grants enabled so a script can get a human token:

```bash
TOKEN=$(curl -s -d grant_type=password -d client_id=tar-ui \
  -d username=curator -d password=curator-password -d scope='openid profile email' \
  http://127.0.0.1:8090/realms/tar/protocol/openid-connect/token | jq -r .access_token)

curl -s -H "Authorization: Bearer $TOKEN" localhost:8099/api/v1/whoami | jq
```

And the workload half, which is what a deployment actually does:

```bash
TOKEN=$(curl -s -u shacl-manager-demo:shacl-manager-demo-secret \
  -d grant_type=client_credentials \
  http://127.0.0.1:8090/realms/tar/protocol/openid-connect/token | jq -r .access_token)

# Until an Instance declares tar:oidcClientId "shacl-manager-demo", this authenticates
# but can advertise nothing — whoami names the client id to register.
curl -s -H "Authorization: Bearer $TOKEN" localhost:8099/api/v1/whoami | jq
```

## Teardown

```bash
docker compose -f deploy/keycloak/compose.yaml down     # -v is unnecessary: dev mode is in-memory
```
