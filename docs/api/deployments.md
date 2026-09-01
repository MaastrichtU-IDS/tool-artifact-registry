# Registering a deployment

A deployment record says that some release of some software is installed somewhere, and it is
the thing a credential identifies. Deployments arrive in two very different ways, and either
way may be driven by either kind of credential — so there are four combinations, and the UI
prints the exact commands for all four on a software's **Create deployment** page
(`/software/{id}/deploy`), built from that registry's own base IRI rather than a placeholder.

## Curated

Somebody who knows the estate creates the record. Right when deployments are few and
long-lived.

```bash
curl -X POST -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{
       "label": "validator, production",
       "software": "01a05…",
       "release": "01a06…",
       "endpoint_url": "https://validator.example.org",
       "health_endpoint": "https://validator.example.org/healthz",
       "operator": {"name": "Platform team", "kind": "organization"},
       "availability": "restricted",
       "jurisdiction": "NL",
       "oidc_client_id": "validator-prod",
       "allowed_scopes": ["advertise:produce", "advertise:consume"]
     }' \
     https://registry.example.org/api/v1/instances
```

`PATCH /api/v1/instances/{id}` edits it; `DELETE` tombstones it.

Needs the `curator` role or the `register:instance` scope.

### `health_endpoint`

A URL whose only job is to say the deployment is alive, held to a **2xx**. Leave it out and the
`endpoint_url` itself is probed, where anything that answers counts as up — because a great
many healthy services answer `401` or `404` at their root, and marking those down would be a
false alarm about a working deployment. Give a real health endpoint if you have one; the check
is only as good as what it is pointed at.

### Software that cannot be deployed

If the software is marked `deployable: false` — a library, a desktop application — a deployment
of it may not carry an endpoint, and one that does is refused. The record is still useful: it
says the thing is installed here, and it can still advertise runs.

## Self-registering

The application is handed one credential and **every deployment of it creates and maintains its
own record**. Right when deployments are many, short-lived, or created by something other than
a person.

A curator issues the key once, for the *software* rather than for a deployment:

```bash
curl -X POST -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{"label":"self-registration","scopes":["register:instance","advertise:produce"]}' \
     https://registry.example.org/api/v1/software/$SOFTWARE_ID/tokens
```

Every deployment then announces itself, and repeats this whenever anything changes:

```bash
curl -X PUT -H "Authorization: Bearer $APP_KEY" -H 'content-type: application/json' \
     -d '{"label": "validator on prod", "instance_key": "prod-cluster",
          "endpoint_url": "https://validator.example.org",
          "health_endpoint": "https://validator.example.org/healthz",
          "version": "2.1.0"}' \
     https://registry.example.org/api/v1/instances/self
```

The first call creates the record; every call after it updates that same one. It is a `PUT`
because it is meant to be run unconditionally at startup — announcing is not something a
deployment should have to remember whether it has already done.

### `instance_key`

What tells two deployments sharing one credential apart. Without it, one key would mean one
deployment, and a second replica would overwrite the first one's record instead of making its
own. Anything stable per deployment works: a cluster name, a hostname, a pod's stable
identifier.

### The credential decides which software

Not the payload. A credential bound to one application cannot register a deployment of another
by naming it in the body — that is a `403`. `GET /api/v1/whoami` reports the binding as
`may_register_deployments_of`.

## The four combinations

| | Curated | Self-registering |
|---|---|---|
| **Registry API token** | An admin creates the record and mints a per-deployment token at `POST /api/v1/instances/{id}/tokens`. | A curator mints a per-software token at `POST /api/v1/software/{id}/tokens`; every deployment `PUT`s to `/instances/self`. |
| **Identity provider** | An admin creates the record with `oidc_client_id` set to the deployment's client. | The software lists the client ids in `registration_clients`; a deployment presenting a token from its own issuer registers itself. |

With an identity provider in either row there is no key to issue, store or leak at all, which
is the argument for it.

### `registration_clients`

A field on the *software* record listing the OIDC client ids allowed to self-register
deployments of it:

```bash
curl -X PATCH -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{"registration_clients":["validator-prod","validator-staging"]}' \
     https://registry.example.org/api/v1/software/01a05…
```

A credential authorised this way is shared by every deployment of the application, so the
registry deliberately does *not* write that client id onto the deployment record — doing so
would make the next replica authenticate as this one. The credential is remembered as
`self_registered_by`, which is what finds the right record on the next announcement.

### `registration_issuer`

A client id is only unique *within* an identity provider: `validator-prod` at your Keycloak and
`validator-prod` at a partner's are two different principals that spell their name the same
way, and registering a client under a given name is free at every issuer. So on a registry that
accepts more than one issuer — `TAR_OIDC_ISSUER` plus anything in `TAR_WORKLOAD_ISSUERS`, such
as a Kubernetes API server or GitHub Actions — the client id alone does not say who may
register.

`registration_issuer` names the provider the ids in `registration_clients` belong to:

```bash
curl -X PATCH -H "Authorization: Bearer $CURATOR" -H 'content-type: application/json' \
     -d '{"registration_issuer":"https://keycloak.example.org/realms/ids"}' \
     https://registry.example.org/api/v1/software/01a05…
```

Leave it unset and the registry reads the clients against its **primary** issuer, or against
the sole workload issuer when that is the only one configured. Those are the cases with one
obvious answer. When several issuers are accepted and none is primary there is no honest
default — picking one would hand the weakest accepted issuer the authority meant for the
strongest — so the registry refuses to create the record until the issuer is named, and says so
at `POST`/`PATCH` rather than as a `403` the first time the workload calls.

The same rule governs the other two bindings: an Instance's `oidc_issuer` beside its
`oidc_client_id`, and `self_registered_issuer`, which the registry records itself when a
deployment self-registers so that later announcements from a same-named client at a different
issuer do not land on the record.

### The looser third option

`TAR_OIDC_AUTO_REGISTER_INSTANCES` lets **any** accepted credential name its own software and
register a deployment of it. Convenient in a trusted cluster, and much weaker: it is the
operator saying that every credential this registry accepts may add records. Off by default.

When it is off and an unrecognised workload announces itself, the `403` names all three ways
out rather than just refusing — create the deployment with its `oidc_client_id`, issue a
software token, or add the client to `registration_clients`.

## Reading deployments

```
GET  /api/v1/instances                    list, with ?software= ?health= ?availability=
GET  /api/v1/instances/{id}               one record
GET  /api/v1/instances/{id}/runs          its runs
GET  /api/v1/instances/{id}/artifacts     the artifacts its runs touched
PUT  /api/v1/instances/{id}/capability    declare what it can produce and consume
```
