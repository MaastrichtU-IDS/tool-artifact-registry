# Troubleshooting

Start every investigation with the same call, using the credential that failed:

```bash
curl -H "Authorization: Bearer $TOKEN" https://registry.example.org/api/v1/whoami
```

It says what the registry made of the credential: which kind it is, which deployment it acts
as (`instance`), and which roles and scopes it carries. Most problems below are a mismatch
between what you expected there and what it shows.

Every error comes back as `application/problem+json`, and its `detail` names the problem and
usually the fix.

## Refused: what the message means

| Status and message | What it means | Fix |
|---|---|---|
| `401` "this endpoint needs a credential" | No `Authorization` header was sent. | Send `Authorization: Bearer <token>`. |
| `401` "unrecognised credential format" | The header holds something that is neither a registry token (`tar_…`) nor a JWT. | Check the variable holding the token is set, and not the client secret by mistake. |
| `401` "issuer … is not trusted by this registry" | The token comes from an identity provider the registry was not configured to accept. | Use the realm the registry trusts (the message lists it), or ask a registry admin. |
| `401` "JWT rejected: InvalidAudience" | The token's `aud` does not include the registry's address. | Ask your identity-provider admin to add an **audience mapper** for the registry's address to your client. |
| `401` "JWT rejected: ExpiredSignature" | The access token has expired. They last minutes. | Fetch a fresh one per job. The Python client renews them itself. |
| `403` "curator role required" | Registering or editing software or deployments needs the `curator` role. | Ask for the role, or ask a registry admin to do it ([Get access](register.md#1-get-access)). |
| `403` "this credential does not act as an Instance" | Advertising was attempted with a credential that is not bound to a deployment, and has not registered one: a client whose id no deployment declares, or a shared key that has not announced yet. | Advertise with the deployment's own credential, or announce first. For a client, check `oidc_client_id` on the deployment matches the client id exactly. |
| `403` "this credential registered N deployments; say which one ran this with "instance_key"" | A shared registration credential is advertising, and it registered several deployments. | Send `"instance_key"` in the advertisement: the same key the deployment announced with. |
| `403` "credential lacks the advertise:produce scope (has: none)" | Almost always a **person's** sign-in, or an admin token, used to advertise. Only the deployment can advertise its runs. | Advertise with the deployment's own credential. |
| `403` "credential lacks the advertise:produce scope (has: …)" | The deployment's credential exists but may not do this. | Mint a token with that scope, or add it to the deployment's `allowed_scopes`. |
| `422` "… is not an artifact type this registry knows" | `conforms_to`, `consumes`, `produces` or a topic is not a term the registry holds. | Look the term up and use its `iri` ([types](register.md#types-and-topics-are-looked-up-not-typed)). |
| `422` "Write rejected by SHACL validation", other fields | A field has the wrong shape: a licence that is not an SPDX IRI, a timestamp that is not RFC 3339, an unknown `availability`. | The `detail` lists each field and what is wrong with it. |
| `429` "… limit for …" | Too many requests from your credential or address. | Wait the `Retry-After` seconds. The examples do this themselves. |

A `422` report may also carry **warnings**. They do not stop the write. One of them, "a run
should not end before it starts", currently appears on every run that has an `ended_at`, even a
correct one. It is a known bug in the rules and safe to ignore.

## It succeeded, but not how I expected

**Every retry created a new run.** The payload has no `run.external_key`, or its value changes
between retries (a timestamp in it, say). Use an id your system already assigns to the job.

**A retry created a second artifact.** The artifacts have no `external_key`, so the registry
derived one, and something in the payload changed. Give each artifact a stable `external_key`.

**I fixed a typo and re-advertised, but the old title is still there.** A re-advertisement with
the same keys is recognised as a retry, and the first version is kept. Correct it with
`PATCH /api/v1/artifacts/{id}` instead ([Correct or withdraw an artifact](advertise.md#correct-or-withdraw-an-artifact)).

**My deployment shows "down" while the service is fine.** The registry probes `health_endpoint`,
or `endpoint_url` when there is none, from where the registry runs. Check the URL answers from
there, and that a health endpoint answers `2xx`.

**My subscription never fires.** Check the filter names the artifact **type**, as a full IRI
from vocabulary search. A keyword filter only matches that exact spelling. Remember
`exclude_own` hides your own deployment's output, and that `roles` defaults to `produced`.

**The webhook stopped.** After repeated failed deliveries the registry suspends a webhook.
Nothing is lost: pull the queue (`GET …/deliveries`), and resume with
`PATCH /api/v1/subscriptions/{id}` `{"resume": true}` once your receiver is back.

## Current limits worth knowing

- An artifact's **type and lineage cannot be changed** after it is advertised: they are evidence
  the registry recorded. Withdraw it and advertise again under a new key.
- **Runs cannot be edited or withdrawn.**
- The **`curator` role is registry-wide**. It is not limited to your own records.

The full list is in [Limitations](../limitations.md).

## Asking for help

Include the request (method, path and body, **without** the token), the full problem document
you got back, and the output of `whoami` for that credential. That is almost always enough to
see what happened.
