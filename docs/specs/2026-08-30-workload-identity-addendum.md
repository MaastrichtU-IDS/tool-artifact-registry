# Workload Identity — Addendum to the Tool Artifact Registry design

| | |
|---|---|
| **Status** | Draft for review · implemented in the prototype |
| **Date** | 2026-08-30 |
| **Amends** | [`2026-08-30-tool-artifact-registry-design.md`](2026-08-30-tool-artifact-registry-design.md) — D8, §8, §9.1, §10.5 |
| **Question** | *"Instead of giving API keys to each tool, can we authenticate and authorise tools using Keycloak or something similar?"* |

---

## 1. Answer

Yes, and it is a better primitive than the per-Instance API token of D8.

The insight D8 already had is the right one: **the advertising party is a deployment, and the
credential should identify that deployment.** What D8 got wrong is *who mints the credential*.
Minting it here makes the registry a secret store: a long-lived bearer string that somebody
has to distribute to a cluster, rotate on a schedule nobody owns, and revoke by hand when a
person leaves. Keycloak already runs on `ids3` and already does all three.

So: **each Instance becomes an OIDC client.** The deployment authenticates to its own identity
provider with the `client_credentials` grant, gets a short-lived JWT, and presents that. The
registry verifies the signature against the issuer's JWKS and maps a claim in the token to the
`Instance` record that declared it.

The authorisation rule of §8.3 does not change. It gets *stronger*:

> An Instance may only advertise runs in which it is itself the agent.

The Instance is still taken from the credential and never from the request body. The
difference is that the identity is now asserted by an issuer we trust, expires in minutes, and
is revocable centrally rather than by an admin remembering which CI secret to rotate.

---

## 2. Decisions

| # | Decision | Rationale | Rejected |
|---|---|---|---|
| D12 | **An Instance may declare `tar:oidcClientId` (optionally narrowed by `tar:oidcIssuer`). A verified token whose client claim matches acts as that Instance.** | Reuses the identity provider the estate already runs. No secret for a deployment is ever stored in the registry. Rotation, expiry and revocation move to Keycloak, where they are already solved problems. | Registry-minted tokens as the only credential (D8): a secret store we did not want to be. mTLS client certs: a second PKI to operate. |
| D13 | **Registry API tokens remain, as the zero-dependency fallback.** | Requirement 6 says anyone can run their own registry with minimal ops. A registry that *requires* a Keycloak is not that. A single container with `TAR_ROOT_TOKEN` must stay a complete install. | OIDC-only (breaks req. 6); tokens-only (the status quo this addendum replaces). |
| D14 | **Trust a list of issuers, not one.** `TAR_OIDC_ISSUER` for the estate's own provider, `TAR_WORKLOAD_ISSUERS` for others. | The same verification path then accepts a Kubernetes projected ServiceAccount token and a GitHub Actions OIDC token, which lets a CI job advertise with **no stored secret at all**. That is strictly better than any token we could mint. | One issuer only: would have forced a registry token back into every GitHub Actions workflow. |
| D15 | **Scopes come from the token when it carries ours, and from the Instance record otherwise.** | Keycloak client scopes are fiddly to configure per client; an estate that has not done it still gets least privilege from `tar:allowedScope` on the record. A token that *does* carry scopes always wins, so a provider can tighten but never widen. | Trusting only token scopes (adoption cost); trusting only the record (ignores what the provider asserted). |
| D16 | **Human sign-in uses the same issuer, with roles.** `reader` / `curator` / `admin` come from `realm_access.roles` or `resource_access.{client}.roles`. | One identity provider for people and for workloads; the registry stores no passwords and no user table. | A local user table. |

---

## 3. How it works

### 3.1 A deployment advertises

```bash
# Once, in Keycloak: create client `shacl-manager-ids3`, service accounts enabled.
# Once, in the registry: set tar:oidcClientId on the Instance record.

TOKEN=$(curl -s -u "$CLIENT_ID:$CLIENT_SECRET" \
  -d grant_type=client_credentials \
  "$ISSUER/protocol/openid-connect/token" | jq -r .access_token)

curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     --data @produced.json https://reg.ids.unimaas.nl/api/v1/advertise/produced
```

### 3.2 What the registry does with it

1. Read `iss` from the token without verifying, and reject it unless the issuer is trusted.
2. Fetch that issuer's JWKS (cached one hour, refetched once on an unknown `kid`) and verify
   the signature, `exp`, `iss` and `aud`.
3. Read the client claim — `azp` by default, configurable, falling back to `client_id` then
   `sub`.
4. Find the `Instance` whose `tar:oidcClientId` matches, and whose `tar:oidcIssuer` matches
   the issuer when it declares one. A client id is only unique within an issuer.
5. Grant the scopes in the token that this registry understands; if none, the Instance's
   `tar:allowedScope`; if none of those, `advertise:produce` and `advertise:consume`.
6. Every write records `prov:wasAttributedTo` the Instance, and an audit row naming the
   credential kind and issuer.

A verified token bound to no Instance authenticates but can advertise nothing, and the `403`
says exactly which client id to register. `GET /api/v1/whoami` is the first thing to curl when
a CI job gets a 403: it reports the credential kind, the resolved Instance, the issuer and the
effective scopes.

### 3.3 Beyond Keycloak — no stored secret at all

Because trust is a list of issuers, two more identity sources work through the same path:

| Source | Issuer | What to put in `tar:oidcClientId` |
|---|---|---|
| Kubernetes projected ServiceAccount token | the cluster's OIDC issuer | `system:serviceaccount:shacl:shacl-manager` |
| GitHub Actions OIDC | `https://token.actions.githubusercontent.com` | `repo:MaastrichtU-IDS/shacl-manager:ref:refs/heads/main` |

A workflow then advertises with a token GitHub mints for that job. There is no secret in the
repository, nothing to rotate, and the credential cannot be replayed from anywhere else.

---

## 4. Configuration

| Variable | Default | Notes |
|---|---|---|
| `TAR_OIDC_ISSUER` | unset | The estate's provider. Enables human sign-in and workload tokens. OIDC is off entirely when unset. |
| `TAR_OIDC_CLIENT_ID` / `_CLIENT_SECRET` | unset | For browser sign-in. The UI uses authorisation code + PKCE, so the secret is only needed for confidential-client setups. |
| `TAR_WORKLOAD_ISSUERS` | unset | Comma-separated. Accepted for workload tokens only, never for browser sign-in. |
| `TAR_OIDC_AUDIENCE` | `TAR_BASE_IRI` | Expected `aud`. |
| `TAR_OIDC_REQUIRE_AUDIENCE` | `true` | Requires the `aud` claim **and** checks it. A token without one is rejected, not waved through: otherwise any token from a trusted issuer, minted for any other service, would work here. Turn off only for a provider that cannot set `aud` — it weakens replay protection. |
| `TAR_OIDC_CLIENT_CLAIM` | `azp` | Which claim carries the workload's identity. |
| `TAR_OIDC_ROLES_CLAIM` | `realm_access.roles` | Dotted path. |
| `TAR_OIDC_SCOPE_CLAIM` | `scope` | Space-delimited string or array. |
| `TAR_OIDC_AUTO_REGISTER_INSTANCES` | `false` | Deliberately off: an unknown workload should be registered by a person. |

`/.well-known/tar-registry` reports all of this, so a peer or a tool can discover how to
authenticate without being told out of band, and the UI hides sign-in entirely when no issuer
is configured.

---

## 5. What this does not solve

- **Federation trust is untouched.** A peer registry's tokens are not accepted here, and ours
  are not accepted there. Peer data stays a read-only stub (§8.4). Cross-registry *write*
  trust would need signed advertisements — still spec Q2, still deferred.
- **It is authentication, not authorisation of content.** A deployment with a valid token can
  still advertise a nonsensical artifact. SHACL write validation, not the credential, is what
  keeps records well-formed.
- **JWKS availability becomes a dependency of writes.** Verification needs the issuer
  reachable at least once per hour. Reads are unaffected, registry API tokens are unaffected,
  and a cached JWKS covers a short outage — but an estate whose Keycloak is down cannot
  advertise with OIDC in the meantime. That is the price of central revocation, and it is the
  right trade; the fallback token path exists for the case where it is not.
- **Token replay within its lifetime** is possible if a token leaks, exactly as for any bearer
  credential. Short lifetimes and a correct `aud` bound the window. DPoP or mTLS-bound tokens
  would close it and are a natural v2 if the threat model demands it.

---

## 6. Changes to the main spec

- **D8** is superseded by D12–D16: *"Auth: OIDC workload identity per Instance, with registry
  API tokens as the zero-dependency fallback; OIDC for humans. Anonymous read by default."*
- **§4.2** gains `tar:oidcClientId`, `tar:oidcIssuer` and `tar:allowedScope` on `Instance`.
- **§8.1** gains a principal row: an Instance authenticated by an OIDC workload token.
- **§9.1** the self-description gains an `auth` block.
- **§10.5** gains the variables in §4 above.
- **§12 Q2** (signed advertisements) is unchanged and still open — this addendum secures the
  hop *into* the registry, not the claim itself.
