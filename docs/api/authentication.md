# How a tool authenticates

Three credential types, one rule:

> **A deployment may only advertise runs in which it is itself the agent**, and which
> deployment that is comes from the credential, never from the payload.

Naming a different deployment in a request body is a `403`, not a hint. This is the whole
reason the deployment layer exists — see [The model](../model.md#why-four-and-not-two).

`GET /api/v1/whoami` tells you what a credential actually resolved to, and it is the first
thing to call when something returns `403`:

```json
{
  "authenticated": true,
  "credential": "oidc-workload",
  "subject": "validator-prod",
  "instance": "https://registry.example.org/instance/01a05…",
  "may_register_deployments_of": null,
  "issuer": "https://sso.example.org/realms/main",
  "scopes": ["advertise:produce", "advertise:consume"],
  "roles": [],
  "is_curator": false,
  "is_admin": false
}
```

## 1. OIDC workload identity — prefer this

Give the deployment a client in the identity provider you already run, and tell the registry
which client that is.

```bash
curl -X PATCH -H "Authorization: Bearer $ADMIN" -H 'content-type: application/json' \
     -d '{"oidc_client_id":"validator-prod"}' \
     https://registry.example.org/api/v1/instances/01a05…
```

The tool then fetches its own short-lived token and presents it:

```bash
TOKEN=$(curl -s -u "$CLIENT_ID:$CLIENT_SECRET" -d grant_type=client_credentials \
  "$ISSUER/protocol/openid-connect/token" | jq -r .access_token)

curl -H "Authorization: Bearer $TOKEN" … https://registry.example.org/api/v1/advertise/produced
```

**No secret for that deployment is ever stored in the registry.** Rotation, expiry and
revocation belong to the identity provider, which is where an organisation already manages
them. That is the whole argument for preferring this.

The registry verifies the signature against the issuer's JWKS, checks the audience, and maps
the claim named by `TAR_OIDC_CLIENT_CLAIM` (default `azp`) to a deployment that declares it.

### The same path takes Kubernetes and CI tokens

A projected Kubernetes ServiceAccount token and a GitHub Actions OIDC token are ordinary OIDC
tokens from other issuers. List those issuers in `TAR_WORKLOAD_ISSUERS` and put the subject in
`oidc_client_id`:

```
tar:oidcClientId  "repo:your-org/your-tool:ref:refs/heads/main"
tar:oidcClientId  "system:serviceaccount:tools:validator"
```

A CI job then advertises what it produced with no stored secret at all.

### Workload issuers assert identity, never authority

Only `TAR_OIDC_ISSUER` may assert the `reader` / `curator` / `admin` roles. An issuer listed in
`TAR_WORKLOAD_ISSUERS` is trusted to say *which deployment is calling* and nothing else.

This distinction is load-bearing rather than fussy: a partner's identity provider, a Kubernetes
API server and a CI provider can all mint a token containing a realm role called `admin`, and
honouring it would hand them the registry.

## 2. Registry API tokens — the fallback

A registry with no identity provider still has to work. Tokens are Argon2id-hashed, scoped,
revocable, optionally expiring, and shown exactly once.

```bash
curl -X POST -H "Authorization: Bearer $ADMIN" -H 'content-type: application/json' \
     -d '{"scopes":["advertise:produce","advertise:consume"],"label":"ci","expires_in":"90d"}' \
     https://registry.example.org/api/v1/instances/01a05…/tokens
```

The token is minted **for one deployment**, so the identity rule holds the same way it does for
a workload token: the deployment comes from the credential.

There is a second flavour, minted for a *software* rather than a deployment, which is what
self-registration uses — see [Registering a deployment](deployments.md#self-registering).

`GET` the same path to list a deployment's tokens; `DELETE
/api/v1/instances/{id}/tokens/{token_id}` revokes one.

### When to use which

| | |
|---|---|
| You run an identity provider, and the tool can reach it | Workload identity. |
| The tool runs in Kubernetes or in CI | Workload identity, using that platform's issuer. |
| There is no identity provider, or the tool cannot reach one | A registry API token. |
| Many short-lived deployments of one program | A software token and self-registration. |

A registry token is not a worse credential in kind — it is a credential whose lifecycle you
have to manage yourself, in the registry, rather than in the system that already does that job.

## 3. People

Browser sign-in is OIDC authorisation code + PKCE against `TAR_OIDC_ISSUER`. Roles come from
the token, read from `TAR_OIDC_ROLES_CLAIM` (default `realm_access.roles`).

When no issuer is configured the UI hides sign-in altogether and falls back to pasting a
registry token, so a registry with no identity provider is still administrable.

Setting up a provider, including the audience mapper that catches everybody out, is
[Identity provider setup](../operations/identity-provider.md).

### Token refresh

The access token is renewed silently, twice over: from a timer set from its own `exp` claim,
ahead of expiry, and reactively when a request comes back `401` — the backstop for a timer that
did not fire on time, such as a laptop that slept through it. Several requests that expire
together share one renewal, which matters once a provider rotates the refresh token: without
it, the first renewal would invalidate the token the others are about to present.

**The refresh token is held in memory and nowhere else** — not `sessionStorage`, not
`localStorage`, not a cookie. It is the long-lived half of the credential, so a closed tab or a
reload drops it with nothing left behind for the next person on a shared machine to find. The
access token in `sessionStorage` keeps working until it expires; after that, sign-in is needed
again. A pasted registry API token has no refresh token, so nothing changes for it.

## The bootstrap token

`TAR_ROOT_TOKEN` is an admin credential for getting a fresh registry to the point where real
credentials exist. The registry refuses to start if it is a recognisable placeholder or shorter
than 16 characters, because a bootstrap credential that everybody has the same value for is not
a credential.

Use it to seed the catalogue and mint the first real credentials, then configure an identity
provider and stop using it.
