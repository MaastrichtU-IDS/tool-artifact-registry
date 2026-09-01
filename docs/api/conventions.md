# Conventions

Everything in this section is under `/api/v1` on the registry's own origin. The registry
describes itself at `GET /.well-known/tar-registry`, which is the first thing a client should
read: it reports the API base, whether reads are public, which authentication methods are
configured, the SPARQL endpoint, the `llms.txt` location and the peers it federates with.

```bash
curl https://registry.example.org/.well-known/tar-registry
```

## Authentication

A credential is a bearer token, whatever kind it is:

```
Authorization: Bearer <token>
```

Reads are anonymous by default (`TAR_PUBLIC_READ`). Writes always need a credential. Which
credential to use, and how a deployment gets one, is [How a tool
authenticates](authentication.md).

`GET /api/v1/whoami` reports what a credential resolved to — the principal, its roles, its
scopes and the deployment it acts as, if any. It is the first thing to call when a job gets a
`403`.

### Roles and scopes

Two orthogonal things. **Roles** come from a person's identity provider token, or from the root
token; **scopes** are carried by registry-minted API tokens and bound the deployment
credentials.

| Role | May |
|---|---|
| `reader` | read |
| `curator` | register and edit software, deployments, releases and vocabulary terms |
| `admin` | everything, including peers and token administration |

| Scope | Permits |
|---|---|
| `advertise:produce` | advertise artifacts a run produced |
| `advertise:consume` | advertise artifacts a run consumed |
| `register:software` | register and update software |
| `register:instance` | register deployments |
| `read:private` | read records that are not publicly readable |
| `admin:*` | everything |

## Errors

Every error path returns [RFC 9457] `application/problem+json`:

```json
{
  "type": "https://w3id.org/tar/problem/forbidden",
  "title": "Forbidden",
  "status": 403,
  "detail": "credential lacks the advertise:produce scope (has: advertise:consume)"
}
```

| Status | `type` suffix | When |
|---|---|---|
| `400` | `bad-request` | Malformed request. |
| `401` | `unauthorized` | No credential, or one that did not verify. `WWW-Authenticate: Bearer` is set. |
| `403` | `forbidden` | A valid credential lacking the role or scope. Retrying with different arguments will not help. |
| `404` | `not-found` | No such record. |
| `409` | `conflict` | A uniqueness rule was violated. |
| `410` | `tombstoned` | The record was withdrawn. It still resolves and says so. |
| `422` | `shacl-validation-failed` | The write is well-formed JSON but not a record the registry accepts. |
| `502` | `upstream-failed` | A peer or repository the registry had to reach did not answer. |

### The `422`, specifically

A rejected write carries the SHACL engine's own report alongside the problem document:

```json
{
  "type": "https://w3id.org/tar/problem/shacl-validation-failed",
  "title": "Write rejected by SHACL validation",
  "status": 422,
  "detail": "kind: value must be one of service, library, cli, desktop, workflow",
  "report": "@prefix sh: … a sh:ValidationReport ; sh:result [ … ] .",
  "report_media_type": "text/turtle"
}
```

Each result in the report carries a `tar:jsonField` naming the JSON field that caused it, so a
form can attach the message to the input that produced it without parsing `sh:resultPath` back
into a field name.

The vocabulary rule reports through the *same* report, deliberately. A caller has one error
shape to handle, not two — see [Artifact types and topics](../vocabulary/terms.md#the-refusal).

## Listing, filtering and pagination

List endpoints return:

```json
{ "items": [ … ], "total": 42, "next_cursor": "https://registry.example.org/software/01a05…", "facets": [ … ] }
```

Pagination is **keyset**, not offset. Pass the `next_cursor` you were given back as `?cursor=`
to get the following page; a `null` cursor means the end. The cursor is a record IRI, and the
ordering is descending IRI string — which is newest-first within one registry, because ids are
UUIDv7 and sort by mint time. `?limit=` defaults to 25 and is clamped to 200.

`facets` accompanies the software listing with the value counts a filter UI needs, so a client
does not have to fetch the whole catalogue to know what is worth filtering on. Other listings
omit it.

## Idempotency

The advertisement endpoints are idempotent on `(run, artifact, role)`, keyed by the run's
`external_key`. A retried CI step does not duplicate lineage. See [Advertising runs and
artifacts](advertising.md).

## Request size

Bodies are capped by `TAR_MAX_PAYLOAD_BYTES`, default 2 MiB. Software records carry whole
READMEs, so this is worth raising if you are importing large ones.

## Audit

Every write is recorded. `GET /api/v1/audit` returns the log, for admins.

[RFC 9457]: https://www.rfc-editor.org/rfc/rfc9457
